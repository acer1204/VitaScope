//! 找回搬家、改名的影片的書籤：開了一個沒有書籤的本機檔案時，在背景找「同樣內容」的檔案（先比大小，再比
//! OpenSubtitles 的 moviehash），找到就把書籤搬過來（原檔不在了）或複製一份（原檔還在）。
//!
//! - 讀使用者的檔案（大小、特徵碼、原檔還在不在）都在這個模組的背景執行緒 `vitascope-bookmark-lookup` 做：
//!   網路磁碟斷線時一次查詢可能卡好幾十秒，不能卡住畫面，也不能卡住書籤的寫入執行緒（關閉程式時只等它兩秒）。
//! - 這裡只回報找到什麼；改書籤由介面執行緒在記憶體裡做，再交給寫入執行緒存檔（只有一個寫入的地方）。
//! - 大小不同就不讀特徵碼（目標、候選的都不讀）：只看檔案資訊，不打開檔案。
//! - 小於 128 KiB 的檔案沒有特徵碼（`filehash`），不找；真的影片不會這麼小。

use crate::filehash::{self, FileHash};
use crate::instance::Wake;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

/// 可能是同一個檔案的項目（介面執行緒從記憶體裡的書籤取的快照）
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Candidate {
    pub path: String,
    /// 新增書籤時記下的大小
    pub size: u64,
    /// 記下的特徵碼；舊的項目（之前的版本加的）沒有
    pub hash: Option<String>,
}

/// 找到的同樣內容的檔案
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Same {
    /// 開著的檔案（書籤要放到這裡）
    pub key: String,
    /// 書籤原本所在的檔案
    pub from: String,
    /// 原檔不在了：搬過來（原本的項目拿掉）；false = 複製一份
    pub moved: bool,
    pub size: u64,
    pub hash: String,
}

/// 查到的檔案資料：路徑、大小、特徵碼（小檔案沒有）
pub(super) type Identified = (String, u64, Option<String>);

/// 背景工作
pub(super) enum Job {
    /// 查這個檔案的大小和特徵碼（新增書籤後、開了有書籤但還沒記特徵碼的檔案）：記下來，搬家之後才找得回來
    Identify(String),
    /// 開了沒有書籤的本機檔案，第一步：只查大小（介面再挑出一樣大的項目，不用每次複製全部的書籤）
    Size {
        key: String,
        /// 第幾次找；比 [`Stats::latest`] 舊的（又開了別的檔案）就不找了
        generation: u64,
    },
    /// 第二步：在一樣大的 `candidates` 裡找同樣內容的（依最近改過的排前面）
    Lookup {
        key: String,
        /// 第幾次找；比 [`Stats::latest`] 舊的（又開了別的檔案）就不找了
        generation: u64,
        candidates: Vec<Candidate>,
    },
}

/// 背景工作的結果
#[derive(Debug, Default)]
pub(super) struct Done {
    /// 這一次找做完了（找到、沒找到、不用找都算）
    pub lookup: bool,
    /// [`Job::Size`] 的結果：代號、第幾次找、大小（128 KiB 以上才有；介面接著挑候選）
    pub sized: Option<(String, u64, u64)>,
    /// 查到的檔案資料（要記到書籤檔裡的）
    pub identified: Vec<Identified>,
    pub same: Option<Same>,
    /// `same` 是第幾次找的結果（又開了別的檔案就不套用）
    pub generation: u64,
}

/// 背景執行緒和介面共用的計數
#[derive(Debug, Default)]
pub(super) struct Stats {
    /// 讀了幾次檔案的特徵碼（測試用：大小不同時不能讀）
    pub hash_reads: AtomicU64,
    /// 處理完幾次「找同樣內容的檔案」（介面套用了結果才算；測試等它）
    pub finished: AtomicU64,
    /// 最新的一次找的編號
    pub latest: AtomicU64,
}

/// 查檔案的方法（測試換成假的）
pub(super) struct Probe<'a> {
    /// 檔案大小；不是檔案、讀不到就 None
    pub size: &'a dyn Fn(&Path) -> Option<u64>,
    /// 特徵碼；小檔案、讀不到就 None
    pub hash: &'a dyn Fn(&Path) -> Option<FileHash>,
    /// 確定不在了（所在的磁碟還在、檔案沒了）
    pub gone: &'a dyn Fn(&Path) -> bool,
    /// 兩個路徑是同一個檔案（捷徑、硬連結、macOS 不分大小寫、`Z:\` 與 `\\nas\分享`）
    pub same_file: &'a dyn Fn(&Path, &Path) -> bool,
}

/// 在 `candidates` 裡找跟 `key` 同樣內容的檔案。回傳找到的，加上順便查到的舊項目的特徵碼（之後不用再讀）
pub(super) fn find_same(key: &str, candidates: &[Candidate], probe: &Probe) -> (Option<Same>, Vec<Identified>) {
    let mut identified = Vec::new();
    let target = Path::new(key);
    let Some(size) = (probe.size)(target) else {
        return (None, identified);
    };
    if size < filehash::MIN_SIZE {
        return (None, identified);
    }
    // 先只比大小（只看檔案資訊）：沒有一樣大的就不讀特徵碼
    let same_size: Vec<&Candidate> = candidates.iter().filter(|c| c.size == size).collect();
    if same_size.is_empty() {
        return (None, identified);
    }
    let Some(hash) = (probe.hash)(target).filter(|h| h.size == size) else {
        return (None, identified);
    };
    let hex = hash.hex();
    let mut matches = Vec::new();
    for c in same_size {
        let theirs = match &c.hash {
            Some(h) => Some(h.clone()),
            // 之前的版本加的書籤沒有記特徵碼：原檔還在才讀得到（讀到的記下來）
            None => {
                let h = (probe.hash)(Path::new(&c.path));
                if let Some(h) = h {
                    identified.push((c.path.clone(), h.size, Some(h.hex())));
                }
                h.filter(|h| h.size == size).map(|h| h.hex())
            }
        };
        // 用別的寫法開了同一個檔案：不是另一份，複製過來的書籤之後會跟原本的各改各的，不如不動
        if theirs.is_some_and(|h| h.eq_ignore_ascii_case(&hex)) && !(probe.same_file)(target, Path::new(&c.path)) {
            matches.push(c);
        }
    }
    // 原檔不在了的優先（搬家）；都還在就從最近改過的那個複製
    let gone = matches.iter().find(|c| (probe.gone)(Path::new(&c.path)));
    let (from, moved) = match (gone, matches.first()) {
        (Some(c), _) => (c, true),
        (None, Some(c)) => (c, false),
        (None, None) => return (None, identified),
    };
    let same = Same {
        key: key.to_owned(),
        from: from.path.clone(),
        moved,
        size,
        hash: hex,
    };
    (Some(same), identified)
}

/// 檔案在哪一種磁碟上（檔案不見時，先確定那個磁碟還接著，才算「不在了」）
#[derive(Debug, PartialEq)]
pub(super) enum Volume {
    /// 這個根目錄在就是接著：Windows 的 `Q:\`、`\\伺服器\分享\`；Linux GNOME 掛的網路分享
    /// `/run/user/<uid>/gvfs/<分享>`（斷線時那個資料夾就沒了）
    Root(PathBuf),
    /// 外接、網路磁碟常掛在這個資料夾底下（macOS 的 `/Volumes`；Linux 的 `/media`、`/run/media`、`/mnt`、`/net`）。
    /// 拔掉之後掛載點的資料夾可能還在（空的），所以要在這之下找到一個真的掛載點才算接著
    MountsUnder(PathBuf),
}

/// 檔案所在的磁碟（[`Volume`]）；其他位置（例如家目錄）是 None：當作一直接著
pub(super) fn volume_of(path: &Path) -> Option<Volume> {
    let comps: Vec<Component> = path.components().collect();
    if let Some(Component::Prefix(_)) = comps.first() {
        return Some(Volume::Root(comps.iter().take(2).collect()));
    }
    if comps.first() != Some(&Component::RootDir) {
        return None;
    }
    let names: Vec<&std::ffi::OsStr> = comps
        .iter()
        .skip(1)
        .map_while(|c| match c {
            Component::Normal(n) => Some(*n),
            _ => None,
        })
        .collect();
    let under = |n: usize| Path::new("/").join(names[..n].iter().collect::<PathBuf>());
    match names.as_slice() {
        // 最後一段是檔案本身，不是分享的資料夾
        [r, u, _, g, _, _, ..] if *r == "run" && *u == "user" && *g == "gvfs" => Some(Volume::Root(under(5))),
        [r, m, ..] if *r == "run" && *m == "media" => Some(Volume::MountsUnder(under(2))),
        [v, ..] if *v == "Volumes" || *v == "media" || *v == "mnt" || *v == "net" => {
            Some(Volume::MountsUnder(under(1)))
        }
        _ => None,
    }
}

/// `path` 的上層資料夾裡，在 `top` 之下（不含 `top`）有沒有一個掛載點（跟它的上一層在不同的裝置上）
#[cfg(unix)]
pub(super) fn mounted_below(path: &Path, top: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let device = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .map(|m| m.dev())
    };
    path.ancestors()
        .skip(1)
        .take_while(|a| *a != top && a.starts_with(top))
        .any(|a| match (device(a), a.parent().and_then(device)) {
            (Some(here), Some(up)) => here != up,
            _ => false,
        })
}

/// 不是 Unix（不會有這種路徑）：最上層的資料夾在就算接著
#[cfg(not(unix))]
pub(super) fn mounted_below(_path: &Path, top: &Path) -> bool {
    matches!(top.try_exists(), Ok(true))
}

/// 原檔確定不在了：它所在的磁碟接著，但檔案沒了。磁碟、分享資料夾連不上（拔掉的外接硬碟、斷線的 NAS）、
/// 查詢出錯時都當作還在（複製，不搬走：接回來時原檔的書籤還在）
pub(super) fn original_gone(path: &Path) -> bool {
    if !matches!(path.try_exists(), Ok(false)) {
        return false;
    }
    match volume_of(path) {
        None => true,
        Some(Volume::Root(root)) => matches!(root.try_exists(), Ok(true)),
        Some(Volume::MountsUnder(top)) => mounted_below(path, &top),
    }
}

/// 兩個路徑是不是同一個檔案（都要存在）：Unix 比裝置與 inode，Windows 比磁碟序號與檔案編號
pub(super) fn same_file(a: &Path, b: &Path) -> bool {
    file_id(a).is_some_and(|id| file_id(b) == Some(id))
}

#[cfg(unix)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(windows)]
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: 全部是整數欄位的 C 結構，零是合法的值；handle 在 `file` 活著時有效
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return None;
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some((u64::from(info.dwVolumeSerialNumber), index))
}

#[cfg(not(any(unix, windows)))]
fn file_id(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// 背景執行緒：一次做一件事，做完送回結果、叫醒介面
pub(super) fn run(jobs: &Receiver<Job>, done: &Sender<Done>, stats: &Stats, wake: &Mutex<Option<Wake>>) {
    let size = |p: &Path| {
        std::fs::metadata(p)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|m| m.len())
    };
    let hash = |p: &Path| {
        stats.hash_reads.fetch_add(1, Ordering::Relaxed);
        filehash::moviehash(p).ok().flatten()
    };
    let probe = Probe {
        size: &size,
        hash: &hash,
        gone: &original_gone,
        same_file: &same_file,
    };
    while let Ok(job) = jobs.recv() {
        let result = match job {
            Job::Identify(key) => {
                let path = Path::new(&key);
                let identified = size(path).map(|s| {
                    let h = (s >= filehash::MIN_SIZE)
                        .then(|| hash(path))
                        .flatten()
                        .filter(|h| h.size == s);
                    (key.clone(), s, h.map(|h| h.hex()))
                });
                Done {
                    identified: identified.into_iter().collect(),
                    ..Done::default()
                }
            }
            // 又開了別的檔案：這個不找了（網路磁碟卡住時，排隊的舊工作不用一個一個做完）
            Job::Size { generation, .. } | Job::Lookup { generation, .. }
                if generation < stats.latest.load(Ordering::Relaxed) =>
            {
                Done {
                    lookup: true,
                    ..Done::default()
                }
            }
            Job::Size { key, generation } => match size(Path::new(&key)) {
                Some(s) if s >= filehash::MIN_SIZE => Done {
                    sized: Some((key, generation, s)),
                    ..Done::default()
                },
                // 不是檔案、太小：不找
                _ => Done {
                    lookup: true,
                    ..Done::default()
                },
            },
            Job::Lookup {
                key,
                generation,
                candidates,
            } => {
                let (same, identified) = find_same(&key, &candidates, &probe);
                Done {
                    lookup: true,
                    identified,
                    same,
                    generation,
                    sized: None,
                }
            }
        };
        if done.send(result).is_err() {
            return;
        }
        if let Ok(slot) = wake.lock()
            && let Some(wake) = slot.as_ref()
        {
            wake();
        }
    }
}

/// 背景執行緒的那一頭
pub(super) struct Worker {
    pub tx: Sender<Job>,
    pub rx: Receiver<Done>,
}

impl Worker {
    pub fn spawn(stats: Arc<Stats>, wake: Arc<Mutex<Option<Wake>>>) -> Option<Self> {
        let (tx, jobs) = std::sync::mpsc::channel();
        let (done, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("vitascope-bookmark-lookup".into())
            .spawn(move || run(&jobs, &done, &stats, &wake))
            .map_err(|e| eprintln!("[vitascope] 找不回搬家的檔案的書籤（開不了背景執行緒）：{e}"))
            .ok()?;
        Some(Self { tx, rx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    const BIG: u64 = 700_000_000;

    /// 假的檔案系統：路徑 → (大小, 特徵碼)；`gone` 裡的是確定不在了的。記下讀了哪些檔案的特徵碼
    struct FakeFs {
        files: HashMap<&'static str, (u64, u64)>,
        gone: Vec<&'static str>,
        /// 跟開的檔案是同一個檔案的別的寫法
        aliases: Vec<&'static str>,
        hashed: RefCell<Vec<String>>,
    }

    impl FakeFs {
        fn new(files: &[(&'static str, u64, u64)], gone: &[&'static str]) -> Self {
            Self {
                files: files.iter().map(|&(p, s, h)| (p, (s, h))).collect(),
                gone: gone.to_vec(),
                aliases: Vec::new(),
                hashed: RefCell::default(),
            }
        }

        fn find(&self, key: &str, candidates: &[Candidate]) -> (Option<Same>, Vec<Identified>) {
            let size = |p: &Path| self.files.get(p.to_str().unwrap()).map(|f| f.0);
            let hash = |p: &Path| {
                self.hashed.borrow_mut().push(p.to_string_lossy().into_owned());
                self.files
                    .get(p.to_str().unwrap())
                    .filter(|f| f.0 >= filehash::MIN_SIZE)
                    .map(|&(size, hash)| FileHash { size, hash })
            };
            let gone = |p: &Path| self.gone.contains(&p.to_str().unwrap());
            let same_file = |_: &Path, b: &Path| self.aliases.contains(&b.to_str().unwrap());
            find_same(
                key,
                candidates,
                &Probe {
                    size: &size,
                    hash: &hash,
                    gone: &gone,
                    same_file: &same_file,
                },
            )
        }
    }

    fn cand(path: &str, size: u64, hash: Option<u64>) -> Candidate {
        Candidate {
            path: path.into(),
            size,
            hash: hash.map(|h| FileHash { size, hash: h }.hex()),
        }
    }

    #[test]
    fn size_mismatch_never_reads_a_hash() {
        let fs = FakeFs::new(&[("new.mkv", BIG, 1), ("old.mkv", BIG + 1, 1)], &[]);
        let (same, identified) = fs.find("new.mkv", &[cand("old.mkv", BIG + 1, None), cand("x.mkv", 5, Some(1))]);
        assert_eq!(same, None);
        assert!(identified.is_empty());
        assert!(
            fs.hashed.borrow().is_empty(),
            "大小不同就不讀特徵碼：{:?}",
            fs.hashed.borrow()
        );
    }

    #[test]
    fn small_and_missing_targets_read_nothing() {
        let fs = FakeFs::new(&[("small.mkv", 1000, 1), ("old.mkv", 1000, 1)], &[]);
        assert_eq!(fs.find("small.mkv", &[cand("old.mkv", 1000, Some(1))]).0, None);
        assert_eq!(fs.find("missing.mkv", &[cand("old.mkv", 1000, Some(1))]).0, None);
        assert!(fs.hashed.borrow().is_empty());
    }

    #[test]
    fn copy_when_the_original_is_still_there_move_when_it_is_gone() {
        let fs = FakeFs::new(&[("new.mkv", BIG, 7)], &["gone.mkv"]);
        // 原檔還在：複製
        let (same, _) = fs.find("new.mkv", &[cand("old.mkv", BIG, Some(7))]);
        let same = same.unwrap();
        assert_eq!((same.from.as_str(), same.moved), ("old.mkv", false));
        assert_eq!(
            (same.size, same.hash.as_str()),
            (BIG, FileHash { size: BIG, hash: 7 }.hex().as_str())
        );
        assert_eq!(*fs.hashed.borrow(), ["new.mkv"], "記下的特徵碼不用再讀");
        // 不在了的優先（搬家）
        let (same, _) = fs.find(
            "new.mkv",
            &[cand("old.mkv", BIG, Some(7)), cand("gone.mkv", BIG, Some(7))],
        );
        let same = same.unwrap();
        assert_eq!((same.from.as_str(), same.moved), ("gone.mkv", true));
        // 一樣大、內容不同：不算
        assert_eq!(fs.find("new.mkv", &[cand("other.mkv", BIG, Some(8))]).0, None);
        // 用別的寫法開了同一個檔案（硬連結、捷徑、大小寫不同）：不複製
        let mut alias = FakeFs::new(&[("new.mkv", BIG, 7)], &["gone.mkv"]);
        alias.aliases = vec!["NEW.mkv"];
        assert_eq!(alias.find("new.mkv", &[cand("NEW.mkv", BIG, Some(7))]).0, None);
        let (same, _) = alias.find(
            "new.mkv",
            &[cand("NEW.mkv", BIG, Some(7)), cand("old.mkv", BIG, Some(7))],
        );
        assert_eq!(same.unwrap().from, "old.mkv", "另一份照樣複製");
        // 都還在：從排在前面的（最近改過的）複製
        let (same, _) = fs.find("new.mkv", &[cand("b.mkv", BIG, Some(7)), cand("a.mkv", BIG, Some(7))]);
        assert_eq!(same.unwrap().from, "b.mkv");
    }

    #[test]
    fn old_entries_without_a_hash_are_read_when_they_still_exist() {
        let fs = FakeFs::new(&[("new.mkv", BIG, 7), ("old.mkv", BIG, 7), ("diff.mkv", BIG, 9)], &[]);
        let (same, identified) = fs.find(
            "new.mkv",
            &[
                cand("diff.mkv", BIG, None),
                cand("old.mkv", BIG, None),
                cand("vanished.mkv", BIG, None),
            ],
        );
        assert_eq!(same.unwrap().from, "old.mkv");
        let hex = |h| FileHash { size: BIG, hash: h }.hex();
        assert_eq!(
            identified,
            [
                ("diff.mkv".to_owned(), BIG, Some(hex(9))),
                ("old.mkv".to_owned(), BIG, Some(hex(7)))
            ],
            "讀到的記下來；不在了的讀不到"
        );
    }

    #[test]
    fn volumes() {
        let root = |p: &str| Some(Volume::Root(PathBuf::from(p)));
        let mounts = |p: &str| Some(Volume::MountsUnder(PathBuf::from(p)));
        if cfg!(windows) {
            assert_eq!(volume_of(Path::new(r"Q:\動畫\第1集.mkv")), root(r"Q:\"));
            assert_eq!(volume_of(Path::new(r"\\nas\share\a\b.mkv")), root(r"\\nas\share\"));
        }
        assert_eq!(volume_of(Path::new("/Volumes/外接/a.mkv")), mounts("/Volumes"));
        assert_eq!(volume_of(Path::new("/media/me/USB/a/b.mkv")), mounts("/media"));
        // usbmount、沒有每個使用者一個資料夾的 udisks：/media/<標籤>/檔案
        assert_eq!(volume_of(Path::new("/media/USB/a.mkv")), mounts("/media"));
        assert_eq!(volume_of(Path::new("/run/media/me/USB/b.mkv")), mounts("/run/media"));
        assert_eq!(volume_of(Path::new("/mnt/nas/影片/a.mkv")), mounts("/mnt"));
        assert_eq!(volume_of(Path::new("/net/nas/share/a.mkv")), mounts("/net"));
        assert_eq!(
            volume_of(Path::new("/run/user/1000/gvfs/smb-share:server=nas,share=v/a/b.mkv")),
            root("/run/user/1000/gvfs/smb-share:server=nas,share=v")
        );
        assert_eq!(
            volume_of(Path::new("/run/user/1000/gvfs/b.mkv")),
            None,
            "檔案本身不是分享的資料夾"
        );
        assert_eq!(volume_of(Path::new("/home/me/a.mkv")), None);
        assert_eq!(volume_of(Path::new("a.mkv")), None);
    }

    /// 掛載點：跟上一層在不同的裝置上。拔掉之後留下的空資料夾不算
    #[cfg(unix)]
    #[test]
    fn mount_points_below_the_mount_folder() {
        use std::os::unix::fs::MetadataExt;
        let dev = |p: &str| std::fs::metadata(p).unwrap().dev();
        // /dev 在 Linux（包括容器）、macOS 都是另外掛的
        if dev("/dev") != dev("/") {
            assert!(mounted_below(Path::new("/dev/vitascope-missing/a.mkv"), Path::new("/")));
        }
        assert!(!mounted_below(Path::new("/vitascope-missing/a.mkv"), Path::new("/")));
        let dir = std::env::temp_dir().join(format!("vitascope-mount-{}", std::process::id()));
        let empty = dir.join("usb0");
        std::fs::create_dir_all(&empty).unwrap();
        // 空的掛載點資料夾（磁碟拔掉了）：跟上一層同一個裝置
        assert!(!mounted_below(&empty.join("a.mkv"), &dir));
        assert!(!mounted_below(&empty.join("sub").join("a.mkv"), &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_file_sees_hard_links() {
        let dir = std::env::temp_dir().join(format!("vitascope-same-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a.mkv"), dir.join("b.mkv"), dir.join("c.mkv"));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&c, b"x").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        assert!(same_file(&a, &b), "硬連結是同一個檔案");
        assert!(!same_file(&a, &c), "內容一樣的另一個檔案");
        assert!(!same_file(&a, &dir.join("沒有.mkv")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gone_needs_a_reachable_volume() {
        let dir = std::env::temp_dir().join(format!("vitascope-recover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mkv");
        std::fs::write(&file, b"x").unwrap();
        assert!(!original_gone(&file), "還在");
        std::fs::remove_file(&file).unwrap();
        assert!(original_gone(&file), "所在的磁碟還在、檔案沒了");
        let _ = std::fs::remove_dir_all(&dir);
        if cfg!(windows) {
            // 沒有這個磁碟機（連不上的磁碟）：當作還在。從 Z 往前找，不碰 Q:（主人的媒體）
            let missing = ('D'..='Z')
                .rev()
                .filter(|d| *d != 'Q')
                .map(|d| format!("{d}:\\"))
                .find(|root| !Path::new(root).exists());
            if let Some(root) = missing {
                assert!(!original_gone(&Path::new(&root).join("影片.mkv")), "{root}");
            }
        } else {
            assert!(!original_gone(Path::new("/media/nobody-vitascope/USB/a.mkv")));
            assert!(!original_gone(Path::new("/media/vitascope-usb/a.mkv")));
            assert!(!original_gone(Path::new("/mnt/vitascope-nas/a.mkv")));
            assert!(!original_gone(Path::new("/run/user/0/gvfs/vitascope-share/a.mkv")));
            assert!(!original_gone(Path::new("/Volumes/vitascope-not-mounted/a.mkv")));
        }
    }
}
