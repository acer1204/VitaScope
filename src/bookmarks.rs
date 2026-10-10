//! 書籤：每個檔案自己的標記（時間 + 名稱），存在 `bookmarks.json`（跟 history.json 同一個資料夾）。
//!
//! - 書籤是使用者的資料，不像續播位置有數量上限，不會被擠掉；所以跟設定、播放紀錄分開存。
//! - 介面執行緒只改記憶體裡的資料，再把修改（[`Change`]）交給背景的寫入執行緒 `vitascope-bookmarks`：
//!   存檔（讀回磁碟上的版本、套用修改、寫暫存檔、改名）不會卡住畫面，檔案很大、設定資料夾很慢時也一樣。
//! - 只有這一個執行緒寫檔，暫存檔名用行程編號 + 計數器：同一個程式裡有兩份（測試）也不會寫到同一個暫存檔。
//! - 每次寫入前都先讀回磁碟上的版本：同時開著好幾個播放器時，不會蓋掉別的視窗加的書籤。
//!   讀回、寫入這一段先鎖住 `bookmarks.json.lock`：兩個視窗同時存檔時一個等另一個，後寫的不會蓋掉先寫的。
//!   寫完把那個檔案在磁碟上的書籤送回來（[`Bookmarks::poll`]），別的視窗加的也看得到。
//! - 檔案的代號（`key`）：本機檔案是完整路徑（Windows 不分大小寫，跟續播一樣），網址一字不差地比對。

use crate::instance::Wake;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一個檔案最多幾個書籤
pub const MAX_MARKS: usize = 1000;
/// 書籤名稱最多幾個字
pub const MAX_NAME: usize = 200;
/// 這麼近（秒）就算同一個位置，不重複加
pub const SAME_SPOT: f64 = 0.5;
/// 「下一個書籤」：比目前的時間晚這麼多（秒）才算後面的（剛跳到的那一個不算）
pub const NEXT_AFTER: f64 = 0.5;
/// 「上一個書籤」：比目前的時間早這麼多（秒）才算前面的。比「下一個」寬：播放中剛跳到的書籤已經過了一點，
/// 連按時才會一個一個往前，不會一直跳回同一個（跟章節一樣）
pub const PREV_BEFORE: f64 = 1.5;

/// 一個書籤
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    /// 隨機的編號（同一個檔案裡不重複）。不用遞增的編號：刪掉最後一個再新增會拿到同一個號碼，
    /// 別的視窗對舊的那個改名、刪除時會改到新的
    pub id: u64,
    /// 秒
    pub time: f64,
    /// 名稱；空的 = 清單上只顯示時間
    #[serde(default)]
    pub name: String,
    /// 新增的時間（Unix 秒）
    #[serde(default)]
    pub added: u64,
}

/// 一個檔案的書籤
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileMarks {
    /// 本機檔案的完整路徑，或網址
    pub path: String,
    /// 檔案大小（本機檔案，新增書籤時在背景查；之後找回搬家、改名的檔案用）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 檔案內容的雜湊（找回搬家、改名的檔案用；之後的版本才填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// 依時間排序
    #[serde(default)]
    pub marks: Vec<Mark>,
}

impl FileMarks {
    fn new(key: &str) -> Self {
        Self {
            path: key.to_owned(),
            size: None,
            hash: None,
            marks: Vec::new(),
        }
    }

    /// 讀檔後整理：時間不合理的拿掉、依時間排序、名稱太長的截掉、重複的編號換一個。
    /// 換的編號由檔案代號、時間、原本的編號算出來（不是隨機的）：介面和寫入執行緒各自讀檔，
    /// 同一個書籤要拿到同一個編號，改名、刪除才找得到它。回傳有沒有換編號（要寫回去）
    fn sanitized(mut self) -> (Self, bool) {
        self.marks.retain(|m| m.time.is_finite() && m.time >= 0.0);
        self.marks.sort_by(|a, b| a.time.total_cmp(&b.time));
        let seed = norm(&self.path)
            .bytes()
            .fold(0, |h, b| crate::picture::splitmix64(h ^ u64::from(b)));
        let mut reid = false;
        for i in 0..self.marks.len() {
            self.marks[i].name = clean_name(&self.marks[i].name);
            let id = self.marks[i].id;
            if id == 0 || self.marks[..i].iter().any(|m| m.id == id) {
                self.marks[i].id = derived_id(seed ^ self.marks[i].time.to_bits() ^ id.rotate_left(17), &self.marks);
                reid = true;
            }
        }
        (self, reid)
    }
}

/// 新增書籤失敗的原因
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AddError {
    /// 這個位置（[`SAME_SPOT`] 秒內）已經有書籤：那個書籤的時間
    Duplicate(f64),
    /// 這個檔案已經有 [`MAX_MARKS`] 個書籤
    Full,
}

/// 一次修改（介面執行緒套用在記憶體裡，寫入執行緒套用在磁碟上的版本）
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Add {
        key: String,
        mark: Mark,
    },
    Rename {
        key: String,
        id: u64,
        name: String,
    },
    Remove {
        key: String,
        id: u64,
    },
    /// 刪掉這個檔案的所有書籤
    Clear {
        key: String,
    },
}

impl Change {
    fn key(&self) -> &str {
        match self {
            Change::Add { key, .. }
            | Change::Rename { key, .. }
            | Change::Remove { key, .. }
            | Change::Clear { key } => key,
        }
    }
}

/// 比對用的代號：Windows 的本機路徑不分大小寫（跟 `playlist::same_file` 一樣）；網址一字不差
fn norm(key: &str) -> String {
    if cfg!(windows) && !key.contains("://") {
        key.to_lowercase()
    } else {
        key.to_owned()
    }
}

/// 名稱：去掉前後空白、換行換成空白、最多 [`MAX_NAME`] 個字
fn clean_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_NAME)
        .collect()
}

/// 新的編號：時間 ^ 計數器 ^ 行程編號打散，取 53 位元（JSON 的數字用 double 讀也不會失真），不是 0 也不跟 `taken` 重複
fn new_id(taken: &[Mark]) -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = crate::picture::splitmix64(nanos ^ count.rotate_left(32) ^ u64::from(std::process::id())) >> 11;
        if id != 0 && !taken.iter().any(|m| m.id == id) {
            return id;
        }
    }
}

/// 由 `seed` 算出的編號（同樣的 `seed`、`taken` 一定得到同一個）：53 位元、不是 0、不跟 `taken` 重複
fn derived_id(mut seed: u64, taken: &[Mark]) -> u64 {
    loop {
        seed = crate::picture::splitmix64(seed);
        let id = seed >> 11;
        if id != 0 && !taken.iter().any(|m| m.id == id) {
            return id;
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 依時間插入；同一個位置已經有、或已經滿了就不加
fn insert_mark(marks: &mut Vec<Mark>, mark: Mark) -> Result<(), AddError> {
    if let Some(m) = marks
        .iter()
        .find(|m| m.id == mark.id || (m.time - mark.time).abs() < SAME_SPOT)
    {
        return Err(AddError::Duplicate(m.time));
    }
    if marks.len() >= MAX_MARKS {
        return Err(AddError::Full);
    }
    let at = marks.partition_point(|m| m.time <= mark.time);
    marks.insert(at, mark);
    Ok(())
}

/// 「下一個書籤」：第一個比 `now` 晚 [`NEXT_AFTER`] 秒以上的（`marks` 依時間排序）
pub fn next_after(marks: &[Mark], now: f64) -> Option<usize> {
    marks.iter().position(|m| m.time > now + NEXT_AFTER)
}

/// 「上一個書籤」：最後一個比 `now` 早 [`PREV_BEFORE`] 秒以上的
pub fn prev_before(marks: &[Mark], now: f64) -> Option<usize> {
    marks.iter().rposition(|m| m.time < now - PREV_BEFORE)
}

/// 目前的位置在哪個書籤上（最後一個不晚於 `now` + [`NEXT_AFTER`] 的；選單標出來用）
pub fn current_at(marks: &[Mark], now: f64) -> Option<usize> {
    marks.iter().rposition(|m| m.time <= now + NEXT_AFTER)
}

/// 把修改套用到磁碟上的版本（寫入執行緒）：改到的檔案移到最前面（最近改的在前面），
/// 書籤都刪光的檔案整個拿掉。`size_of` 查本機檔案的大小（新增書籤時填）。回傳有沒有改到
fn apply(files: &mut Vec<FileMarks>, change: &Change, size_of: &dyn Fn(&str) -> Option<u64>) -> bool {
    let key = norm(change.key());
    let pos = files.iter().position(|f| norm(&f.path) == key);
    let changed = match (change, pos) {
        (Change::Add { key, mark }, pos) => {
            let i = pos.unwrap_or_else(|| {
                files.insert(0, FileMarks::new(key));
                0
            });
            let entry = &mut files[i];
            let added = insert_mark(&mut entry.marks, mark.clone()).is_ok();
            if added && entry.size.is_none() && !key.contains("://") {
                entry.size = size_of(key);
            }
            if entry.marks.is_empty() {
                files.remove(i);
            }
            added
        }
        (Change::Rename { id, name, .. }, Some(i)) => {
            let name = clean_name(name);
            match files[i].marks.iter_mut().find(|m| m.id == *id) {
                Some(m) if m.name != name => {
                    m.name = name;
                    true
                }
                _ => false,
            }
        }
        (Change::Remove { id, .. }, Some(i)) => {
            let before = files[i].marks.len();
            files[i].marks.retain(|m| m.id != *id);
            let removed = files[i].marks.len() != before;
            if files[i].marks.is_empty() {
                files.remove(i);
            }
            removed
        }
        (Change::Clear { .. }, Some(i)) => {
            files.remove(i);
            true
        }
        (_, None) => false,
    };
    // 最近改的檔案放在最前面
    if changed
        && let Some(i) = files.iter().position(|f| norm(&f.path) == key)
        && i > 0
    {
        let entry = files.remove(i);
        files.insert(0, entry);
    }
    changed
}

/// 磁碟上的檔案內容。讀不懂的項目（新版加的欄位格式不同、手動改壞的一項）原封不動留著，寫回去時放在最後
#[derive(Debug, Clone, Default)]
struct Disk {
    files: Vec<FileMarks>,
    kept: Vec<serde_json::Value>,
    /// 讀檔時換了重複的編號：下一次存檔一定寫回去（換好的編號留在檔案裡）
    repaired: bool,
}

impl Disk {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            files: Vec<serde_json::Value>,
        }
        let raw: Raw = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let mut disk = Disk::default();
        for v in raw.files {
            match FileMarks::deserialize(&v) {
                Ok(f) => {
                    let (f, reid) = f.sanitized();
                    disk.repaired |= reid;
                    disk.files.push(f);
                }
                Err(e) => {
                    eprintln!("[vitascope] 書籤檔裡有一項讀不懂，原樣保留：{e}");
                    disk.kept.push(v);
                }
            }
        }
        Ok(disk)
    }

    fn to_json(&self) -> serde_json::Result<String> {
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Entry<'a> {
            Marks(&'a FileMarks),
            Kept(&'a serde_json::Value),
        }
        #[derive(Serialize)]
        struct Out<'a> {
            files: Vec<Entry<'a>>,
        }
        let files = self
            .files
            .iter()
            .map(Entry::Marks)
            .chain(self.kept.iter().map(Entry::Kept))
            .collect();
        serde_json::to_string_pretty(&Out { files })
    }

    fn find(&self, key: &str) -> Option<&FileMarks> {
        let key = norm(key);
        self.files.iter().find(|f| norm(&f.path) == key)
    }
}

/// 讀檔：不存在是 `Ok(None)`；格式錯誤（包括不是 UTF-8）就改名成 .bad 留著（不要被下一次存檔蓋掉），回傳錯誤
fn read_disk(path: &Path) -> Result<Option<Disk>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    match Disk::parse(&bytes) {
        Ok(d) => Ok(Some(d)),
        Err(why) => {
            eprintln!("[vitascope] 書籤檔讀不了，改名成 .bad 留著：{why}");
            let _ = std::fs::rename(path, path.with_extension("json.bad"));
            Err(why)
        }
    }
}

/// 讀回、寫入期間鎖住 `<檔名>.lock`（別的視窗的寫入執行緒等這邊寫完）；最多等兩秒，
/// 鎖不到（例如檔案系統不支援）就不鎖照樣寫：書籤只是少了同時存檔的保護，不會存不了
fn lock_store(path: &Path) -> Option<std::fs::File> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))
        .ok()?;
    for _ in 0..40 {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(50)),
            Err(std::fs::TryLockError::Error(_)) => return None,
        }
    }
    None
}

/// 寫檔：先寫暫存檔（行程編號 + 計數器，同一個程式裡的兩份也不會撞名）再改名，中途當掉不會留下寫一半的檔案
fn write_disk(path: &Path, disk: &Disk) -> std::io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}-{n}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(disk.to_json().map_err(std::io::Error::other)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Windows 上另一個播放器剛好在讀這個檔案時，改名會暫時失敗；稍等再試
        let mut result = std::fs::rename(&tmp, path);
        for _ in 0..3 {
            match &result {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    std::thread::sleep(Duration::from_millis(50));
                    result = std::fs::rename(&tmp, path);
                }
                _ => break,
            }
        }
        result
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// 送給寫入執行緒的訊息。`u64` 是送出的順序（回覆時帶回來，介面才知道是不是最新的）
enum Msg {
    Change(u64, Change),
    /// 讀回磁碟上這個檔案的書籤（開檔時：別的視窗加的也看得到）
    Refresh(u64, String),
    /// 之前送的都寫完了就回覆（關閉程式時）
    Flush(Sender<()>),
}

/// 寫入執行緒的回覆
struct Reply {
    /// 處理到哪一個訊息
    seq: u64,
    /// 改到、查到的檔案在磁碟上的書籤（`norm` 過的代號；None = 沒有書籤了）
    synced: Vec<(String, Option<FileMarks>)>,
    /// 存檔失敗的原因：只在這一批有使用者的修改時才回報。開檔時的讀回順便重試之前沒存到的，
    /// 失敗也不提示（使用者這次沒有動書籤，不要蓋掉開檔的提示）
    error: Option<std::io::Error>,
}

/// 寫入執行緒的那一頭
struct Writer {
    tx: Sender<Msg>,
    rx: Receiver<Reply>,
    /// 送出的最後一個訊息的順序
    sent: u64,
    /// 還有訊息沒處理完時收到的磁碟版本：全部處理完才換上去（不然會短暫蓋掉之後才加的書籤）
    pending: HashMap<String, Option<FileMarks>>,
    wake: Arc<Mutex<Option<Wake>>>,
}

/// 所有檔案的書籤
#[derive(Default)]
pub struct Bookmarks {
    /// `norm` 過的代號 → 書籤
    files: HashMap<String, FileMarks>,
    /// None = 只放在記憶體（自動測試、`--shot`）
    writer: Option<Writer>,
    /// 只放在記憶體、不存檔的檔案（`norm` 過的代號）：網址含登入資訊、token 的（[`Self::keep_in_memory`]）
    in_memory: HashSet<String>,
}

impl Bookmarks {
    /// 讀取書籤（設定資料夾裡的 bookmarks.json），開始寫入執行緒
    pub fn load() -> Self {
        match crate::settings::config_dir() {
            Some(dir) => Self::load_from(dir.join("bookmarks.json")),
            None => Self::default(),
        }
    }

    /// 讀取指定位置的書籤；檔案不存在就從空的開始，壞了就改名成 bookmarks.json.bad 留著
    pub fn load_from(path: PathBuf) -> Self {
        let disk = match read_disk(&path) {
            Ok(d) => d.unwrap_or_default(),
            Err(why) => {
                eprintln!("[vitascope] 書籤從空的開始：{why}");
                Disk::default()
            }
        };
        let mut files = HashMap::new();
        for f in &disk.files {
            // 同一個檔案出現兩次（手動改過）：用前面的（最近改的）
            files.entry(norm(&f.path)).or_insert_with(|| f.clone());
        }
        let (tx, worker_rx) = mpsc::channel();
        let (worker_tx, rx) = mpsc::channel();
        let wake: Arc<Mutex<Option<Wake>>> = Arc::default();
        let worker_wake = wake.clone();
        let spawned = std::thread::Builder::new()
            .name("vitascope-bookmarks".into())
            .spawn(move || run_writer(&path, disk, &worker_rx, &worker_tx, &worker_wake));
        let writer = match spawned {
            Ok(_) => Some(Writer {
                tx,
                rx,
                sent: 0,
                pending: HashMap::new(),
                wake,
            }),
            Err(e) => {
                eprintln!("[vitascope] 書籤無法存檔（開不了背景執行緒）：{e}");
                None
            }
        };
        Self {
            files,
            writer,
            in_memory: HashSet::new(),
        }
    }

    /// 寫入執行緒有回覆時叫醒介面（存檔失敗要提示）
    pub fn set_wake(&mut self, wake: Wake) {
        if let Some(w) = &self.writer
            && let Ok(mut slot) = w.wake.lock()
        {
            *slot = Some(wake);
        }
    }

    /// 這個檔案的書籤只放在記憶體，不寫進 bookmarks.json（關閉影戲就沒有了）：網址含帳號密碼、token 之類的，
    /// 跟最近開啟、續播一樣不記到磁碟。之後的新增、改名、刪除都不送去存檔，也不從磁碟讀回（讀回會蓋掉記憶體裡的）。
    /// 第一次標記時回傳 true
    pub fn keep_in_memory(&mut self, key: &str) -> bool {
        self.in_memory.insert(norm(key))
    }

    /// 這個檔案的書籤只放在記憶體（[`Self::keep_in_memory`]）
    pub fn is_in_memory(&self, key: &str) -> bool {
        self.in_memory.contains(&norm(key))
    }

    /// 這個檔案的書籤（依時間排序）
    pub fn marks(&self, key: &str) -> &[Mark] {
        self.files.get(&norm(key)).map_or(&[][..], |f| f.marks.as_slice())
    }

    /// 這個檔案的資料（介面測試、之後找回搬家的檔案用）
    pub fn file(&self, key: &str) -> Option<&FileMarks> {
        self.files.get(&norm(key))
    }

    /// 有書籤的檔案數
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 在 `time` 秒新增書籤（名稱空白）。只改記憶體、交給寫入執行緒存檔，不等它寫完
    pub fn add(&mut self, key: &str, time: f64) -> Result<Mark, AddError> {
        let norm_key = norm(key);
        let existing = self.files.get(&norm_key).map_or(&[][..], |f| f.marks.as_slice());
        let mark = Mark {
            id: new_id(existing),
            time: time.max(0.0),
            name: String::new(),
            added: now_secs(),
        };
        let entry = self
            .files
            .entry(norm_key.clone())
            .or_insert_with(|| FileMarks::new(key));
        if let Err(e) = insert_mark(&mut entry.marks, mark.clone()) {
            if entry.marks.is_empty() {
                self.files.remove(&norm_key);
            }
            return Err(e);
        }
        self.send(Change::Add {
            key: key.to_owned(),
            mark: mark.clone(),
        });
        Ok(mark)
    }

    /// 改名（空的 = 只顯示時間）；回傳有沒有這個書籤
    pub fn rename(&mut self, key: &str, id: u64, name: &str) -> bool {
        let name = clean_name(name);
        let Some(m) = self
            .files
            .get_mut(&norm(key))
            .and_then(|f| f.marks.iter_mut().find(|m| m.id == id))
        else {
            return false;
        };
        if m.name != name {
            m.name = name.clone();
            self.send(Change::Rename {
                key: key.to_owned(),
                id,
                name,
            });
        }
        true
    }

    /// 刪掉一個書籤；回傳有沒有這個書籤
    pub fn remove(&mut self, key: &str, id: u64) -> bool {
        let norm_key = norm(key);
        let Some(f) = self.files.get_mut(&norm_key) else {
            return false;
        };
        let before = f.marks.len();
        f.marks.retain(|m| m.id != id);
        if f.marks.len() == before {
            return false;
        }
        if f.marks.is_empty() {
            self.files.remove(&norm_key);
        }
        self.send(Change::Remove {
            key: key.to_owned(),
            id,
        });
        true
    }

    /// 刪掉這個檔案的所有書籤；回傳刪了幾個
    pub fn clear(&mut self, key: &str) -> usize {
        let n = self.files.remove(&norm(key)).map_or(0, |f| f.marks.len());
        if n > 0 {
            self.send(Change::Clear { key: key.to_owned() });
        }
        n
    }

    /// 背景讀回磁碟上這個檔案的書籤（開檔時：同時開著的別的視窗加的書籤也看得到），結果在 [`Self::poll`] 換上
    pub fn refresh(&mut self, key: &str) {
        if self.is_in_memory(key) {
            return;
        }
        if let Some(w) = &mut self.writer {
            w.sent += 1;
            let _ = w.tx.send(Msg::Refresh(w.sent, key.to_owned()));
        }
    }

    fn send(&mut self, change: Change) {
        if self.is_in_memory(change.key()) {
            return;
        }
        if let Some(w) = &mut self.writer {
            w.sent += 1;
            let _ = w.tx.send(Msg::Change(w.sent, change));
        }
    }

    /// 收寫入執行緒的回覆（每一幀）：送出的都處理完了，就換成磁碟上的版本（包括別的視窗加的）。
    /// 回傳存檔失敗的原因（記憶體裡照樣保留這次的修改）
    pub fn poll(&mut self) -> Vec<std::io::Error> {
        let Some(w) = &mut self.writer else {
            return Vec::new();
        };
        let mut errors = Vec::new();
        while let Ok(reply) = w.rx.try_recv() {
            errors.extend(reply.error);
            w.pending.extend(reply.synced);
            if reply.seq == w.sent {
                for (key, f) in w.pending.drain() {
                    // 只放在記憶體的：標記之前送出的讀回，不能蓋掉記憶體裡的書籤
                    if self.in_memory.contains(&key) {
                        continue;
                    }
                    match f {
                        Some(f) => self.files.insert(key, f),
                        None => self.files.remove(&key),
                    };
                }
            }
        }
        errors
    }

    /// 等之前的修改都寫進檔案（最多等 `timeout`；關閉程式時）。只放在記憶體的是 true
    pub fn flush(&self, timeout: Duration) -> bool {
        let Some(w) = &self.writer else { return true };
        let (ack, done) = mpsc::channel();
        w.tx.send(Msg::Flush(ack)).is_ok() && done.recv_timeout(timeout).is_ok()
    }
}

/// 寫入執行緒：一次收完排隊中的訊息，讀回磁碟上的版本、全部套用、寫一次，再回覆。
/// 存檔失敗的修改記著，之後每次讀回都再套用一次、下次存檔一起寫（介面上的書籤才不會在下一次讀回時消失）
fn run_writer(path: &Path, mut cache: Disk, rx: &Receiver<Msg>, tx: &Sender<Reply>, wake: &Mutex<Option<Wake>>) {
    let mut unsaved: Vec<Change> = Vec::new();
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        batch.extend(rx.try_iter());
        // 新增書籤的本機檔案先查好大小（網路磁碟可能很慢，不要在鎖住書籤檔的時候查）
        let sizes: HashMap<String, u64> = batch
            .iter()
            .filter_map(|m| match m {
                Msg::Change(_, Change::Add { key, .. }) if !key.contains("://") => {
                    let meta = std::fs::metadata(key).ok().filter(|m| m.is_file())?;
                    Some((key.clone(), meta.len()))
                }
                _ => None,
            })
            .collect();
        let size_of = |key: &str| sizes.get(key).copied();
        let needs_disk = batch.iter().any(|m| !matches!(m, Msg::Flush(_)));
        let user_change = batch.iter().any(|m| matches!(m, Msg::Change(..)));
        let mut error = None;
        let mut repaired = false;
        // 鎖到寫完（這一輪結束時放開）
        let _lock = needs_disk.then(|| lock_store(path));
        if needs_disk {
            // 讀不到（暫時鎖住、壞掉改名成 .bad 了）就用這裡記得的版本，不會把書籤清空
            match read_disk(path) {
                Ok(Some(disk)) => {
                    repaired = disk.repaired;
                    cache = disk;
                }
                Ok(None) => {}
                Err(why) => eprintln!("[vitascope] 書籤檔讀不了，用記得的版本：{why}"),
            }
            for change in &unsaved {
                apply(&mut cache.files, change, &|_| None);
            }
        }
        let (mut seq, mut touched, mut acks) = (None, Vec::new(), Vec::new());
        let mut changed = needs_disk && (!unsaved.is_empty() || repaired);
        for msg in batch {
            match msg {
                Msg::Change(s, change) => {
                    seq = Some(s);
                    changed |= apply(&mut cache.files, &change, &size_of);
                    touched.push(norm(change.key()));
                    unsaved.push(change);
                }
                Msg::Refresh(s, key) => {
                    seq = Some(s);
                    touched.push(norm(&key));
                }
                Msg::Flush(ack) => acks.push(ack),
            }
        }
        match changed.then(|| write_disk(path, &cache)) {
            Some(Err(e)) => {
                eprintln!("[vitascope] 無法儲存書籤：{e}");
                error = user_change.then_some(e);
            }
            // 寫好了；或是沒改到什麼（重複的、已經刪掉的）：之前的修改也都在磁碟上了
            _ if needs_disk => unsaved.clear(),
            _ => {}
        }
        if let Some(seq) = seq {
            touched.sort();
            touched.dedup();
            let synced = touched
                .into_iter()
                .map(|k| {
                    let f = cache.find(&k).cloned();
                    (k, f)
                })
                .collect();
            if tx.send(Reply { seq, synced, error }).is_err() {
                return;
            }
            if let Ok(slot) = wake.lock()
                && let Some(wake) = slot.as_ref()
            {
                wake();
            }
        }
        for ack in acks {
            let _ = ack.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(10);

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vitascope-bookmarks-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("bookmarks.json")
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// 等寫入執行緒寫完，收回覆
    fn sync(b: &mut Bookmarks) -> Vec<std::io::Error> {
        assert!(b.flush(WAIT), "寫入執行緒沒有回應");
        b.poll()
    }

    fn times(marks: &[Mark]) -> Vec<f64> {
        marks.iter().map(|m| m.time).collect()
    }

    fn mark_at(time: f64) -> Mark {
        Mark {
            id: new_id(&[]),
            time,
            name: String::new(),
            added: 0,
        }
    }

    #[test]
    fn add_sorts_dedupes_and_caps() {
        let mut b = Bookmarks::default();
        let key = "movie.mkv";
        for t in [30.0, 10.0, 20.0] {
            b.add(key, t).unwrap();
        }
        assert_eq!(times(b.marks(key)), [10.0, 20.0, 30.0]);
        // 0.5 秒內是同一個位置：不重複加，回報已經有的那一個
        assert_eq!(b.add(key, 20.4), Err(AddError::Duplicate(20.0)));
        assert_eq!(b.add(key, 19.6), Err(AddError::Duplicate(20.0)));
        assert!(b.add(key, 20.5).is_ok(), "0.5 秒以上就是另一個書籤");
        assert_eq!(b.marks(key).len(), 4);
        // 負的時間當成 0
        assert_eq!(b.add(key, -3.0).unwrap().time, 0.0);
        // 編號不重複、不是 0；新增時間有填
        let ids: Vec<u64> = b.marks(key).iter().map(|m| m.id).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(*id != 0 && *id < (1 << 53) && !ids[..i].contains(id), "{ids:?}");
        }
        assert!(b.marks(key).iter().all(|m| m.added > 1_700_000_000));
        // 最多 1000 個
        let mut b = Bookmarks::default();
        for i in 0..MAX_MARKS {
            b.add(key, i as f64).unwrap();
        }
        assert_eq!(b.add(key, 5000.0), Err(AddError::Full));
        assert_eq!(b.marks(key).len(), MAX_MARKS);
        // 失敗時不會留下空的項目
        let mut b = Bookmarks::default();
        b.add("a.mkv", 1.0).unwrap();
        assert!(b.add("a.mkv", 1.2).is_err());
        assert_eq!(b.file_count(), 1);
        assert!(b.marks("other.mkv").is_empty());
        assert_eq!(b.file_count(), 1, "查詢不會建立項目");
    }

    #[test]
    fn rename_remove_and_clear_by_id() {
        let mut b = Bookmarks::default();
        let key = "movie.mkv";
        let a = b.add(key, 10.0).unwrap();
        let c = b.add(key, 30.0).unwrap();
        assert!(b.rename(key, a.id, "  OP 結束\n "));
        assert_eq!(b.marks(key)[0].name, "OP 結束", "去掉前後空白");
        let long: String = "很長".repeat(300);
        assert!(b.rename(key, c.id, &long));
        assert_eq!(b.marks(key)[1].name.chars().count(), MAX_NAME);
        assert!(b.rename(key, c.id, "第\n二"));
        assert_eq!(b.marks(key)[1].name, "第 二", "換行換成空白");
        assert!(!b.rename(key, 12345, "沒有這個"));
        assert!(!b.rename("other.mkv", a.id, "沒有這個檔案"));
        assert!(b.remove(key, a.id));
        assert!(!b.remove(key, a.id), "已經刪掉了");
        assert_eq!(times(b.marks(key)), [30.0]);
        assert!(b.remove(key, c.id));
        assert_eq!(b.file_count(), 0, "刪光的檔案整個拿掉");
        for t in [1.0, 2.0, 3.0] {
            b.add(key, t).unwrap();
        }
        assert_eq!(b.clear(key), 3);
        assert_eq!(b.clear(key), 0);
        assert!(b.marks(key).is_empty());
    }

    #[test]
    fn step_tolerances() {
        let marks: Vec<Mark> = [10.0, 20.0, 30.0].into_iter().map(mark_at).collect();
        // 下一個：比現在晚 0.5 秒以上（剛跳到的 20 秒不算）
        assert_eq!(next_after(&marks, 0.0), Some(0));
        assert_eq!(next_after(&marks, 20.0), Some(2));
        assert_eq!(next_after(&marks, 19.6), Some(2));
        assert_eq!(next_after(&marks, 19.4), Some(1));
        assert_eq!(next_after(&marks, 29.6), None);
        // 上一個：比現在早 1.5 秒以上（播放中剛跳到的那一個不算，連按時一個一個往前）
        assert_eq!(prev_before(&marks, 21.0), Some(0));
        assert_eq!(prev_before(&marks, 21.6), Some(1));
        assert_eq!(prev_before(&marks, 30.0), Some(1));
        assert_eq!(prev_before(&marks, 11.4), None);
        assert_eq!(prev_before(&marks, 100.0), Some(2));
        assert_eq!(prev_before(&[], 100.0), None);
        // 選單標出目前在哪一個
        assert_eq!(current_at(&marks, 5.0), None);
        assert_eq!(current_at(&marks, 9.6), Some(0));
        assert_eq!(current_at(&marks, 25.0), Some(1));
    }

    #[test]
    fn saves_in_the_background_and_loads_back() {
        let path = temp_file("roundtrip");
        let mut b = Bookmarks::load_from(path.clone());
        let a = b.add("movie.mkv", 312.48).unwrap();
        b.add("movie.mkv", 1290.0).unwrap();
        b.rename("movie.mkv", a.id, "OP 結束");
        b.add("https://example.com/v.m3u8", 5.0).unwrap();
        assert!(sync(&mut b).is_empty());
        let again = Bookmarks::load_from(path.clone());
        assert_eq!(again.marks("movie.mkv"), b.marks("movie.mkv"));
        assert_eq!(again.marks("movie.mkv")[0].name, "OP 結束");
        assert_eq!(again.marks("https://example.com/v.m3u8").len(), 1);
        // 最近改的檔案在前面；暫存檔都改名掉了
        let text = std::fs::read_to_string(&path).unwrap();
        let disk: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(disk["files"][0]["path"], "https://example.com/v.m3u8");
        assert_eq!(disk["files"][1]["marks"][0]["name"], "OP 結束");
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != "bookmarks.json.lock")
            .collect();
        assert_eq!(leftovers.len(), 1, "暫存檔要改名掉：{leftovers:?}");
        // 刪光了：檔案裡也拿掉
        let mut b = again;
        assert_eq!(b.clear("movie.mkv"), 2);
        sync(&mut b);
        let disk = Bookmarks::load_from(path.clone());
        assert_eq!(disk.file_count(), 1);
        cleanup(&path);
    }

    #[test]
    fn marks_kept_in_memory_are_never_written_or_replaced() {
        // 網址含 token：書籤只放在記憶體（新增、改名、刪除都不送去存檔，讀回也不會蓋掉）
        let path = temp_file("in-memory");
        let secret = "https://iptv.example/live/user/pass/1.ts";
        let mut b = Bookmarks::load_from(path.clone());
        // 開檔時送出的讀回（標記之前）：回來的「磁碟上沒有」不能把記憶體裡的書籤拿掉
        b.refresh(secret);
        assert!(b.keep_in_memory(secret), "第一次標記");
        assert!(!b.keep_in_memory(secret), "已經標記過");
        assert!(b.is_in_memory(secret));
        let m = b.add(secret, 12.0).unwrap();
        assert!(b.rename(secret, m.id, "進球"));
        b.add(secret, 30.0).unwrap();
        // 一般的檔案照常存
        b.add("movie.mkv", 5.0).unwrap();
        assert!(sync(&mut b).is_empty());
        assert_eq!(times(b.marks(secret)), [12.0, 30.0], "讀回之後還在記憶體裡");
        assert_eq!(b.marks(secret)[0].name, "進球");
        b.refresh(secret);
        sync(&mut b);
        assert_eq!(b.marks(secret).len(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("iptv.example"), "含 token 的網址寫進了檔案：{text}");
        assert!(text.contains("movie.mkv"));
        let again = Bookmarks::load_from(path.clone());
        assert!(again.marks(secret).is_empty());
        assert_eq!(again.marks("movie.mkv").len(), 1);
        // 刪除也只在記憶體
        assert!(b.remove(secret, m.id));
        assert_eq!(b.clear(secret), 1);
        sync(&mut b);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("iptv.example"));
        cleanup(&path);
    }

    #[test]
    fn local_files_get_their_size_urls_do_not() {
        let path = temp_file("size");
        let video = path.parent().unwrap().join("影片.mkv");
        std::fs::write(&video, vec![7u8; 4096]).unwrap();
        let key = video.to_string_lossy().into_owned();
        let mut b = Bookmarks::load_from(path.clone());
        b.add(&key, 1.0).unwrap();
        b.add("https://example.com/a.mp4", 1.0).unwrap();
        assert!(b.file(&key).unwrap().size.is_none(), "介面執行緒不查檔案大小");
        sync(&mut b);
        assert_eq!(b.file(&key).unwrap().size, Some(4096), "寫入執行緒查到的送回來");
        assert_eq!(b.file("https://example.com/a.mp4").unwrap().size, None);
        cleanup(&path);
    }

    #[test]
    fn two_stores_on_the_same_file_keep_both() {
        let path = temp_file("two-windows");
        let mut a = Bookmarks::load_from(path.clone());
        let mut b = Bookmarks::load_from(path.clone());
        a.add("movie.mkv", 10.0).unwrap();
        sync(&mut a);
        b.add("movie.mkv", 20.0).unwrap();
        b.add("other.mkv", 5.0).unwrap();
        sync(&mut b);
        // b 寫的時候讀回了 a 加的
        assert_eq!(times(b.marks("movie.mkv")), [10.0, 20.0]);
        // a 開檔時讀回磁碟上的版本：看得到 b 加的
        assert_eq!(times(a.marks("movie.mkv")), [10.0]);
        a.refresh("movie.mkv");
        sync(&mut a);
        assert_eq!(times(a.marks("movie.mkv")), [10.0, 20.0]);
        // 同時在差不多的位置加：磁碟上只留一個
        a.add("movie.mkv", 30.0).unwrap();
        b.add("movie.mkv", 30.2).unwrap();
        sync(&mut a);
        sync(&mut b);
        // 先寫的那個留下來（兩個寫入執行緒誰先誰後不一定）
        let disk = Bookmarks::load_from(path.clone());
        let on_disk = times(disk.marks("movie.mkv"));
        assert!(
            on_disk == [10.0, 20.0, 30.0] || on_disk == [10.0, 20.0, 30.2],
            "{on_disk:?}"
        );
        assert_eq!(disk.marks("other.mkv").len(), 1);
        a.refresh("movie.mkv");
        sync(&mut a);
        assert_eq!(a.marks("movie.mkv"), disk.marks("movie.mkv"), "換成磁碟上的版本");
        assert_eq!(b.marks("movie.mkv"), disk.marks("movie.mkv"));
        cleanup(&path);
    }

    #[test]
    fn replies_wait_for_later_changes() {
        // 寫入執行緒還沒處理到的修改：先收到的回覆不能把它蓋掉
        let mut b = Bookmarks::default();
        let (tx, rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        b.writer = Some(Writer {
            tx,
            rx: reply_rx,
            sent: 0,
            pending: HashMap::new(),
            wake: Arc::default(),
        });
        b.add("movie.mkv", 10.0).unwrap();
        b.add("movie.mkv", 20.0).unwrap();
        let first: Vec<Msg> = rx.try_iter().collect();
        assert_eq!(first.len(), 2);
        let mut disk = Vec::new();
        let Msg::Change(1, change) = &first[0] else {
            panic!("第一個是新增")
        };
        apply(&mut disk, change, &|_| None);
        let one = disk[0].clone();
        reply_tx
            .send(Reply {
                seq: 1,
                synced: vec![(norm("movie.mkv"), Some(one))],
                error: None,
            })
            .unwrap();
        assert!(b.poll().is_empty());
        assert_eq!(times(b.marks("movie.mkv")), [10.0, 20.0], "第二個還沒寫，不換");
        let Msg::Change(2, change) = &first[1] else {
            panic!("第二個也是新增")
        };
        apply(&mut disk, change, &|_| Some(99));
        reply_tx
            .send(Reply {
                seq: 2,
                synced: vec![(norm("movie.mkv"), Some(disk[0].clone()))],
                error: Some(std::io::Error::other("磁碟滿了")),
            })
            .unwrap();
        let errors = b.poll();
        assert_eq!(errors.len(), 1);
        assert_eq!(times(b.marks("movie.mkv")), [10.0, 20.0]);
    }

    #[test]
    fn corrupt_and_non_utf8_files_are_kept_aside() {
        for (name, bytes) in [
            ("corrupt", "{ 壞掉的".as_bytes().to_vec()),
            ("not-utf8", vec![0xff, 0xfe, 0x00, 0x7b]),
            ("wrong-shape", b"[1, 2]".to_vec()),
        ] {
            let path = temp_file(name);
            std::fs::write(&path, &bytes).unwrap();
            let mut b = Bookmarks::load_from(path.clone());
            assert_eq!(b.file_count(), 0);
            assert!(path.with_extension("json.bad").exists(), "{name}：壞掉的檔案改名留著");
            assert_eq!(std::fs::read(path.with_extension("json.bad")).unwrap(), bytes);
            b.add("a.mkv", 1.0).unwrap();
            sync(&mut b);
            assert_eq!(Bookmarks::load_from(path.clone()).marks("a.mkv").len(), 1);
            cleanup(&path);
        }
    }

    #[test]
    fn failed_writes_keep_the_marks_and_retry() {
        let path = temp_file("read-only");
        let dir = path.parent().unwrap().to_path_buf();
        // 存檔的資料夾是一個檔案：建不了資料夾、寫不進去
        let blocked = dir.join("sub");
        std::fs::write(&blocked, "不是資料夾").unwrap();
        let store = blocked.join("bookmarks.json");
        let mut b = Bookmarks::load_from(store.clone());
        b.add("a.mkv", 1.0).unwrap();
        let errors = sync(&mut b);
        assert_eq!(errors.len(), 1, "存不了要回報");
        assert_eq!(times(b.marks("a.mkv")), [1.0], "記憶體裡照樣保留");
        // 開檔時讀回磁碟（還是存不了）：書籤不會消失；這次使用者沒有動書籤，重試失敗不再回報
        b.refresh("a.mkv");
        assert!(sync(&mut b).is_empty(), "只是讀回：不提示");
        assert_eq!(times(b.marks("a.mkv")), [1.0]);
        // 再加一個（還是存不了）：使用者的修改，回報
        b.add("a.mkv", 5.0).unwrap();
        assert_eq!(sync(&mut b).len(), 1, "使用者的修改存不了要回報");
        assert!(b.remove("a.mkv", b.marks("a.mkv")[1].id));
        sync(&mut b);
        // 可以存了：下一次修改連同之前沒存的一起寫
        std::fs::remove_file(&blocked).unwrap();
        b.add("a.mkv", 2.0).unwrap();
        assert!(sync(&mut b).is_empty());
        assert_eq!(times(Bookmarks::load_from(store).marks("a.mkv")), [1.0, 2.0]);
        cleanup(&path);
    }

    /// 讀得到、寫不進去（Windows：檔案唯讀；其他：資料夾唯讀）
    fn set_blocked(path: &Path, blocked: bool) {
        let target = if cfg!(windows) { path } else { path.parent().unwrap() };
        let mut perms = std::fs::metadata(target).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(if blocked { 0o555 } else { 0o755 });
        }
        #[cfg(not(unix))]
        perms.set_readonly(blocked);
        std::fs::set_permissions(target, perms).unwrap();
    }

    #[test]
    fn unsaved_marks_survive_reading_the_file_back() {
        let path = temp_file("write-fails");
        let mut b = Bookmarks::load_from(path.clone());
        b.add("a.mkv", 1.0).unwrap();
        assert!(sync(&mut b).is_empty());
        set_blocked(&path, true);
        b.add("a.mkv", 2.0).unwrap();
        let errors = sync(&mut b);
        if errors.is_empty() {
            // 以 root 執行（例如 Linux 的容器）時唯讀擋不住：這台機器測不了
            eprintln!("寫得進唯讀的位置，跳過");
            set_blocked(&path, false);
            cleanup(&path);
            return;
        }
        assert_eq!(times(b.marks("a.mkv")), [1.0, 2.0]);
        // 讀回磁碟上的版本（只有 1 秒那個）：沒存到的照樣在
        b.refresh("a.mkv");
        sync(&mut b);
        assert_eq!(times(b.marks("a.mkv")), [1.0, 2.0]);
        set_blocked(&path, false);
        b.add("a.mkv", 3.0).unwrap();
        assert!(sync(&mut b).is_empty());
        assert_eq!(
            times(Bookmarks::load_from(path.clone()).marks("a.mkv")),
            [1.0, 2.0, 3.0]
        );
        cleanup(&path);
    }

    #[test]
    fn file_corrupted_while_running_keeps_the_marks() {
        let path = temp_file("corrupted-later");
        let mut b = Bookmarks::load_from(path.clone());
        b.add("a.mkv", 1.0).unwrap();
        sync(&mut b);
        std::fs::write(&path, "壞掉").unwrap();
        b.add("a.mkv", 2.0).unwrap();
        sync(&mut b);
        assert!(path.with_extension("json.bad").exists());
        assert_eq!(times(Bookmarks::load_from(path.clone()).marks("a.mkv")), [1.0, 2.0]);
        cleanup(&path);
    }

    #[test]
    fn unreadable_entries_are_kept_and_bad_values_cleaned() {
        let path = temp_file("lenient");
        std::fs::write(
            &path,
            r#"{ "files": [
                { "path": "good.mkv", "size": 10, "marks": [
                    { "id": 7, "time": 20.0, "name": "B" },
                    { "id": 7, "time": 10.0 },
                    { "id": 9, "time": -1.0 }
                ] },
                { "path": "future.mkv", "marks": "新版的格式" },
                { "path": 42 }
            ] }"#,
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut b = Bookmarks::load_from(path.clone());
        let good = b.marks("good.mkv").to_vec();
        assert_eq!(times(&good), [10.0, 20.0], "排序、拿掉負的時間");
        assert_ne!(good[0].id, good[1].id, "重複的編號換掉");
        assert_eq!(b.file_count(), 1);
        // 每次讀都換成同一個編號（介面和寫入執行緒各自讀檔，要對得上）
        assert_eq!(Bookmarks::load_from(path.clone()).marks("good.mkv"), good);
        b.add("new.mkv", 1.0).unwrap();
        sync(&mut b);
        let disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let files = disk["files"].as_array().unwrap();
        assert_eq!(files.len(), 4, "讀不懂的兩項原樣寫回去：{files:?}");
        assert_eq!(files[0]["path"], "new.mkv");
        assert_eq!(files[1]["marks"][1]["id"], good[1].id, "換好的編號寫回去");
        assert_eq!(files[2]["marks"], "新版的格式");
        assert_eq!(files[3]["path"], 42);
        assert!(!path.with_extension("json.bad").exists());
        // 換過編號的書籤照樣刪得掉、改得了名
        assert!(b.rename("good.mkv", good[1].id, "改名"));
        sync(&mut b);
        assert_eq!(b.marks("good.mkv")[1].name, "改名");
        assert!(b.remove("good.mkv", good[1].id));
        sync(&mut b);
        assert_eq!(times(b.marks("good.mkv")), [10.0], "刪掉的不會被讀回來");
        assert_eq!(times(Bookmarks::load_from(path.clone()).marks("good.mkv")), [10.0]);
        // 只是開檔讀回（沒有修改）也把換好的編號寫回去
        std::fs::write(&path, &original).unwrap();
        let mut c = Bookmarks::load_from(path.clone());
        c.refresh("good.mkv");
        assert!(sync(&mut c).is_empty());
        let disk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disk["files"][0]["marks"][1]["id"], good[1].id);
        assert_eq!(c.marks("good.mkv"), good);
        cleanup(&path);
    }

    #[test]
    fn urls_are_compared_exactly_local_paths_follow_the_platform() {
        let mut b = Bookmarks::default();
        b.add("https://example.com/Video.mp4", 1.0).unwrap();
        assert!(b.marks("https://example.com/video.mp4").is_empty(), "網址分大小寫");
        assert!(b.marks("https://example.com/Video.mp4?x=1").is_empty());
        assert_eq!(b.marks("https://example.com/Video.mp4").len(), 1);
        b.add(r"Q:\動畫\第1集.MKV", 1.0).unwrap();
        if cfg!(windows) {
            assert_eq!(b.marks(r"q:\動畫\第1集.mkv").len(), 1);
            assert!(b.add(r"q:\動畫\第1集.mkv", 1.2).is_err(), "同一個檔案");
        } else {
            assert!(b.marks(r"q:\動畫\第1集.mkv").is_empty());
        }
        // 原本的寫法保留在檔案裡
        assert_eq!(b.file(r"Q:\動畫\第1集.MKV").unwrap().path, r"Q:\動畫\第1集.MKV");
    }

    #[test]
    fn in_memory_store_never_touches_disk() {
        let mut b = Bookmarks::default();
        b.add("a.mkv", 1.0).unwrap();
        b.refresh("a.mkv");
        assert!(b.poll().is_empty());
        assert!(b.flush(Duration::ZERO));
        assert_eq!(b.marks("a.mkv").len(), 1);
    }

    #[test]
    fn apply_moves_the_file_to_the_front() {
        let mut files = Vec::new();
        let add = |key: &str, t: f64| Change::Add {
            key: key.into(),
            mark: mark_at(t),
        };
        assert!(apply(&mut files, &add("a.mkv", 1.0), &|_| Some(1)));
        assert!(apply(&mut files, &add("b.mkv", 1.0), &|_| None));
        assert!(apply(&mut files, &add("a.mkv", 5.0), &|_| Some(2)));
        let order: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(order, ["a.mkv", "b.mkv"]);
        assert_eq!(files[0].size, Some(1), "大小只查一次");
        // 沒改到：順序不變、回報沒改
        assert!(!apply(&mut files, &add("b.mkv", 1.1), &|_| None));
        assert!(!apply(
            &mut files,
            &Change::Remove {
                key: "c.mkv".into(),
                id: 1
            },
            &|_| None
        ));
        let id = files[1].marks[0].id;
        assert!(apply(
            &mut files,
            &Change::Rename {
                key: "b.mkv".into(),
                id,
                name: "名稱".into()
            },
            &|_| None
        ));
        assert_eq!(files[0].path, "b.mkv");
        assert!(!apply(
            &mut files,
            &Change::Rename {
                key: "b.mkv".into(),
                id,
                name: "名稱".into()
            },
            &|_| None
        ));
        assert!(apply(
            &mut files,
            &Change::Remove {
                key: "b.mkv".into(),
                id
            },
            &|_| None
        ));
        assert_eq!(files.len(), 1, "刪光的檔案拿掉");
        assert!(apply(&mut files, &Change::Clear { key: "a.mkv".into() }, &|_| None));
        assert!(files.is_empty());
    }

    /// 介面執行緒的成本：5,000 個檔案、每個 3 個書籤時，新增一個書籤（交給寫入執行緒）不到 2 毫秒
    #[test]
    fn adding_costs_the_ui_thread_under_2_ms() {
        let path = temp_file("cost");
        // 代號放在測試自己的暫存資料夾：寫入執行緒會查本機檔案的大小
        let dir = path.parent().unwrap().to_path_buf();
        let files: Vec<FileMarks> = (0..5000)
            .map(|i| FileMarks {
                path: dir.join(format!("第{i}集.mkv")).to_string_lossy().into_owned(),
                size: Some(734_003_200),
                hash: None,
                marks: (0..3)
                    .map(|j| Mark {
                        id: new_id(&[]),
                        time: 100.0 * f64::from(j),
                        name: "OP 結束".into(),
                        added: 1_760_000_000,
                    })
                    .collect(),
            })
            .collect();
        let disk = Disk {
            files,
            kept: Vec::new(),
            repaired: false,
        };
        std::fs::write(&path, disk.to_json().unwrap()).unwrap();
        let mut b = Bookmarks::load_from(path.clone());
        assert_eq!(b.file_count(), 5000);
        let mut costs = Vec::new();
        for i in 0..7 {
            let key = dir.join(format!("第{}集.mkv", i * 700)).to_string_lossy().into_owned();
            let start = Instant::now();
            b.add(&key, 1000.0 + f64::from(i)).unwrap();
            costs.push(start.elapsed());
        }
        costs.sort();
        let median = costs[costs.len() / 2];
        assert!(
            median < Duration::from_millis(2),
            "新增書籤花了 {median:?}（{costs:?}）"
        );
        assert!(sync(&mut b).is_empty());
        assert_eq!(
            Bookmarks::load_from(path.clone())
                .marks(&dir.join("第700集.mkv").to_string_lossy())
                .len(),
            4
        );
        cleanup(&path);
    }
}
