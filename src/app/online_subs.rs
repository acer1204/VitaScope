//! 線上搜尋字幕的視窗（字幕選單「線上搜尋字幕…」、快捷鍵）、OpenSubtitles 的登入對話框、「設定 → 字幕」的「線上字幕」區塊。
//! 核心（搜尋、下載、存檔、登入）在 `crate::online_subs`，這裡只管介面。
//!
//! - 打開視窗就用整理過的檔名搜尋一次（「設定 → 字幕」可以關掉）。只在使用者動作時連網；
//!   自動測試的環境（`Launch.online` 的預設）是離線的，沒有金鑰。
//! - **結果綁在搜尋的那部影片上**（路徑 + 檔案特徵碼，R-A2）：下載存在**那部**影片旁邊，只有那部影片還在播時才載入。
//!   視窗開著時換了影片：自動搜尋開著就用新的檔名再搜尋，關著就把舊的結果灰掉、寫「影片已經換了，請重新搜尋」。
//! - 關掉視窗（✕、Esc）會取消正在進行的下載：換成正式名稱之前停下來、刪掉暫存檔（R-B10）。
//! - 下載次數省著用（R-B11）：同一個檔案這次已經存過、還在就直接用；拿過的下載連結 3 小時內先用它，
//!   失效（410）時下一次先重試同一個連結，再失效才花一次次數要新的（`online_subs::Downloads`）。
//! - 檔名的語言標記照內容判斷（R-B12，核心的 `content_tag`）。存到影片旁邊或快取資料夾照 Q6 = (a)。
//! - OpenSubtitles 下載要登入（R-A1）：伺服器說要登入、登入過期、未登入時次數用完，打開登入對話框；
//!   登入成功後接著下載剛才那一個。已經取消（關掉視窗）的下載不跳出登入對話框；迷你播放器、子母畫面裡
//!   登入對話框跟這個視窗一樣先不畫，回到一般視窗才出現。登入對話框開著時 Esc 只關對話框（R-C1：`egui::Modal`
//!   自己處理 Esc，`modal_open` 讓按鍵不再往後傳）。登入狀態只在按「登出」時清掉（R-C2），關閉影戲時不登出。
//! - 結果是上一部影片的（灰掉）時不能換頁：上一部的搜尋文字配上這部影片，結果會被當成這部的。
//! - 伺服器還沒說剩幾次之前不顯示數字（R-C14）。
//! - 迷你播放器、子母畫面裡不畫（狀態保留，回到一般視窗時照原樣出現）；在那裡打開會回到一般視窗。
//! - 規定的出處說明：OpenSubtitles 的免費金鑰要在搜尋、選字幕的地方寫「Subtitles provided by OpenSubtitles.com」
//!   （照它的寫法，連結到網站）；射手網（偽）要寫「字幕服務由 assrt.net 提供」。

use super::VitascopeApp;
use crate::keymap::Command;
use crate::online_subs::{
    self as core, Account, DownloadJob, Downloads, Job, JobResult, LANGUAGE_CHOICES, LoginInfo, NetError, Provider,
    ResultId, SearchJob, SearchPage, SubResult, VideoRef, assrt,
};
use crate::{tf, tr};
use eframe::egui::{self, Id, pos2, vec2};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};

/// OpenSubtitles 規定的出處說明（免費金鑰：照它的寫法，連結到網站）
pub(super) const OPENSUBTITLES_CREDIT: &str = "Subtitles provided by ";
pub(super) const OPENSUBTITLES_URL: &str = "https://opensubtitles.com";
/// 註冊 OpenSubtitles 帳號的網頁
const SIGN_UP_URL: &str = "https://www.opensubtitles.com/users/sign_up";
/// 射手網（偽）的網站、取得 Token 的說明
const ASSRT_URL: &str = "https://assrt.net";
const ASSRT_DOC_URL: &str = "https://assrt.net/api/doc";

/// 結果的一列：（搜尋結果的第幾個, 射手網（偽）展開後的第幾個檔案）
type RowKey = (usize, Option<usize>);

/// 正在做的背景工作
struct Pending {
    what: Busy,
    rx: Receiver<JobResult>,
    /// 下載的取消（關掉視窗時設成 true）
    cancel: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    Search,
    /// 射手網（偽）：展開一筆字幕
    Detail(u64),
    Download,
    Login,
    Logout,
}

/// 視窗下面那一行的狀態
#[derive(Debug, Clone, Default)]
enum Status {
    #[default]
    Idle,
    /// 搜尋完：這一頁幾個、其中幾個跟這個檔案完全相符、總共幾個（OpenSubtitles 才有）
    Found {
        n: usize,
        matched: usize,
        total: Option<u64>,
    },
    /// 下載好了（存在哪裡、有沒有載入）
    Saved {
        name: String,
        loaded: bool,
    },
    Failed(NetError),
}

/// 登入對話框
#[derive(Default)]
struct LoginForm {
    user: String,
    password: String,
    /// 為什麼打開（伺服器說要登入、過期、次數用完）
    reason: Option<NetError>,
    /// 上一次登入失敗的原因
    error: Option<NetError>,
}

/// 線上搜尋字幕的視窗、登入、這次下載過的
pub(super) struct OnlineSubsUi {
    /// 視窗開著
    pub(super) open: bool,
    /// 搜尋文字
    query: String,
    /// 視窗目前是給哪部影片的（打開時、換了影片時的；搜尋過就有特徵碼）
    video: Option<VideoRef>,
    /// 最近一次搜尋的結果（綁在 `page.video` 上）
    page: Option<SearchPage>,
    /// 換了影片：結果還是上一部的（灰掉、不能下載）
    stale: bool,
    selected: Option<RowKey>,
    /// 射手網（偽）展開的字幕：字幕的編號 → 裡面的檔案（或失敗的原因）
    details: HashMap<u64, Result<Vec<SubResult>, NetError>>,
    /// 搜尋、展開、下載（同時只有一個）
    job: Option<Pending>,
    /// 換了影片時正在下載：下載完再搜尋
    search_queued: bool,
    /// 登入、登出（跟搜尋、下載分開：設定頁也能登入）
    account_job: Option<Pending>,
    status: Status,
    login: Option<LoginForm>,
    /// 登入成功後接著下載的（伺服器說要登入的那一個）
    after_login: Option<DownloadJob>,
    /// 這次下載過的（同一個檔案不再花一次下載次數）
    downloads: Downloads,
    /// OpenSubtitles 的帳號（登入狀態、剩下的次數；背景工作也用）
    account: Arc<Account>,
    /// 這次登入時伺服器說的帳號資料（每天能下載幾個）
    login_info: Option<LoginInfo>,
    /// 已經在背景讀過登入狀態的檔案
    restore_started: bool,
    /// 「設定 → 字幕」的「其他語言」正在打的文字（離開欄位才寫回設定）
    other_draft: Option<String>,
    /// 視窗的語言選單按了「其他…」：設定頁畫出來時把焦點放到「其他語言」的欄位
    focus_other: bool,
}

impl OnlineSubsUi {
    /// `session_file`：OpenSubtitles 登入狀態的檔案（`OnlineEnv::session_file`；自動測試沒有）
    pub(super) fn new(session_file: Option<std::path::PathBuf>) -> Self {
        OnlineSubsUi {
            open: false,
            query: String::new(),
            video: None,
            page: None,
            stale: false,
            selected: None,
            details: HashMap::new(),
            job: None,
            search_queued: false,
            account_job: None,
            status: Status::Idle,
            login: None,
            after_login: None,
            downloads: Downloads::default(),
            account: Account::new(session_file),
            login_info: None,
            restore_started: false,
            other_draft: None,
            focus_other: false,
        }
    }

    /// 關閉影戲：正在下載的停下來（換成正式名稱之前；暫存檔由背景執行緒刪掉）
    pub(super) fn abandon(&mut self) {
        self.cancel_download();
    }

    fn cancel_download(&mut self) {
        if let Some(Pending {
            what: Busy::Download,
            cancel: Some(c),
            ..
        }) = &self.job
        {
            c.store(true, Ordering::SeqCst);
        }
        self.after_login = None;
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }
}

/// 一列的語言：OpenSubtitles 的語言碼換成名稱；射手網（偽）用網站寫的說明
fn lang_name(r: &SubResult) -> String {
    if let Some((_, zh, en)) = LANGUAGE_CHOICES
        .iter()
        .find(|(c, ..)| c.eq_ignore_ascii_case(&r.lang_code))
    {
        return tr!(*zh, *en).to_owned();
    }
    if !r.lang_code.is_empty() {
        return r.lang_code.clone();
    }
    use crate::subs::SubLang;
    match r.lang {
        SubLang::Traditional => tr!("繁體中文", "Chinese (Traditional)"),
        SubLang::Simplified => tr!("簡體中文", "Chinese (Simplified)"),
        SubLang::Chinese => tr!("中文", "Chinese"),
        SubLang::English => tr!("英文", "English"),
        SubLang::Japanese => tr!("日文", "Japanese"),
        SubLang::Unknown => "",
    }
    .to_owned()
}

/// 一列的標記：信任、聽障、自動翻譯、機器翻譯、只有壓縮檔
fn tags(r: &SubResult) -> String {
    let mut t: Vec<&str> = Vec::new();
    if r.trusted {
        t.push(tr!("信任", "Trusted"));
    }
    if r.hearing_impaired {
        t.push(tr!("聽障", "SDH"));
    }
    // OpenSubtitles 的 ai_translated：介面寫「自動翻譯」（§1.8）
    if r.auto_translated {
        t.push(tr!("自動翻譯", "Auto-translated"));
    }
    if r.machine_translated {
        t.push(tr!("機器翻譯", "Machine-translated"));
    }
    if r.archive_only {
        t.push(tr!("只有壓縮檔", "Archive only"));
    }
    t.join(tr!("、", ", "))
}

/// 滑鼠停在名稱上的說明：片名、第幾片、上傳者、日期、字幕組
fn detail(r: &SubResult) -> String {
    let mut lines = Vec::new();
    if !r.name.is_empty() {
        lines.push(r.name.clone());
    }
    if !r.feature.is_empty() {
        lines.push(r.feature.clone());
    }
    if let Some(cd) = r.cd {
        lines.push(tf!("第 {cd} 片", "CD {cd}"));
    }
    if !r.release_site.is_empty() {
        let site = &r.release_site;
        lines.push(tf!("字幕組：{site}", "Group: {site}"));
    }
    if !r.uploader.is_empty() {
        let who = &r.uploader;
        lines.push(tf!("上傳者：{who}", "Uploaded by {who}"));
    }
    if !r.upload_date.is_empty() {
        lines.push(r.upload_date.chars().take(10).collect());
    }
    lines.join("\n")
}

/// 下載數（OpenSubtitles）
fn downloads(r: &SubResult) -> String {
    r.downloads.map(|d| d.to_string()).unwrap_or_default()
}

/// 評分（射手網（偽）的 vote_score；0 = 沒有人評）
fn rating(r: &SubResult) -> String {
    r.rating
        .filter(|x| *x > 0.0)
        .map(|x| format!("{x:.1}"))
        .unwrap_or_default()
}

/// 表格的欄（兩個來源給的資料不同：OpenSubtitles 有「相符」、下載數；射手網（偽）有字幕組、評分）
fn columns(provider: Provider) -> [&'static str; 7] {
    match provider {
        Provider::Opensubtitles => [
            tr!("相符", "Match"),
            tr!("語言", "Language"),
            tr!("名稱 / 版本", "Name / release"),
            tr!("格式", "Format"),
            tr!("下載數", "Downloads"),
            tr!("標記", "Flags"),
            "",
        ],
        Provider::Assrt => [
            tr!("語言", "Language"),
            tr!("名稱 / 版本", "Name / release"),
            tr!("格式", "Format"),
            tr!("字幕組", "Group"),
            tr!("評分", "Rating"),
            tr!("標記", "Flags"),
            "",
        ],
    }
}

/// 按了什麼（畫完再做：畫的時候借著 `self`）
enum Click {
    Select(RowKey),
    Download(RowKey),
    Expand(u64),
    Collapse(u64),
    Page(u32),
    Search,
    SignIn,
    SignOut,
}

impl VitascopeApp {
    /// 測試用：線上搜尋字幕的視窗開著
    #[doc(hidden)]
    pub fn online_subs_open(&self) -> bool {
        self.online_subs.open
    }

    /// 測試用：線上搜尋字幕正在做背景工作（搜尋、展開、下載、登入、登出）
    #[doc(hidden)]
    pub fn online_subs_busy(&self) -> bool {
        self.online_subs.job.is_some() || self.online_subs.account_job.is_some()
    }

    /// 測試用：登入對話框開著
    #[doc(hidden)]
    pub fn online_login_open(&self) -> bool {
        self.online_subs.login.is_some()
    }

    /// 測試用：搜尋文字
    #[doc(hidden)]
    pub fn online_subs_query(&self) -> &str {
        &self.online_subs.query
    }

    /// 測試用：OpenSubtitles 的帳號（登入狀態、伺服器說的剩下次數）
    #[doc(hidden)]
    pub fn online_account(&self) -> &Account {
        &self.online_subs.account
    }

    /// 打開線上搜尋字幕的視窗（字幕選單、快捷鍵）。同一部影片再打開時照原樣（結果、選取都在）；
    /// 換了影片（或第一次）時用新的檔名，自動搜尋開著就馬上搜尋
    pub(super) fn show_online_subs(&mut self) {
        let st = &self.player.state;
        let Some(path) = st.path.clone().filter(|_| st.loaded) else {
            self.osd(tr!(
                "先開啟影片才能搜尋字幕",
                "Open a video first to search for subtitles"
            ));
            return;
        };
        let ui = &mut self.online_subs;
        ui.open = true;
        self.restore_online_account();
        let ui = &self.online_subs;
        let same = ui.video.as_ref().is_some_and(|v| v.path == path);
        if same && !ui.stale && (ui.page.is_some() || ui.busy()) {
            return;
        }
        self.online_new_video(path);
    }

    /// 讀 OpenSubtitles 的登入狀態（快取資料夾裡的檔案；在背景讀，讀完叫醒介面）
    fn restore_online_account(&mut self) {
        if std::mem::replace(&mut self.online_subs.restore_started, true) {
            return;
        }
        let account = self.online_subs.account.clone();
        let ctx = self.egui_ctx.clone();
        let _ = std::thread::Builder::new()
            .name("vitascope-online-restore".into())
            .spawn(move || {
                account.restore();
                ctx.request_repaint();
            });
    }

    /// 視窗改成給 `path` 這部影片：搜尋文字用它的檔名（網址用標題），舊的結果清掉；自動搜尋開著就搜尋
    fn online_new_video(&mut self, path: String) {
        let query = self.online_query_for(&path);
        let ui = &mut self.online_subs;
        ui.video = Some(VideoRef::new(path));
        ui.query = query;
        ui.page = None;
        ui.stale = false;
        ui.selected = None;
        ui.details.clear();
        ui.status = Status::Idle;
        if self.settings.online_subs.auto_search && self.online_usable().is_ok() {
            self.start_online_search(1);
        }
    }

    /// 搜尋文字：本機檔案的檔名、網址的標題（沒有標題時用網址的最後一段），整理過
    fn online_query_for(&self, path: &str) -> String {
        let name = if super::is_url(path) {
            let st = &self.player.state;
            self.titles
                .get(path)
                .cloned()
                .or_else(|| st.title.clone())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| {
                    url::Url::parse(path)
                        .ok()
                        .and_then(|u| u.path_segments()?.rev().find(|s| !s.is_empty()).map(str::to_owned))
                        .unwrap_or_default()
                })
        } else {
            std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        core::clean_query(&name)
    }

    /// 選的來源現在能不能用：OpenSubtitles 要有金鑰（自己編譯的版本沒有）、射手網（偽）要有格式對的 Token
    fn online_usable(&self) -> Result<(), NetError> {
        match self.settings.online_subs.provider {
            Provider::Opensubtitles => self.online.os_key.as_ref().map(|_| ()).ok_or(NetError::NoKey),
            Provider::Assrt => assrt::check_token(&self.settings.online_subs.assrt_token).map(|_| ()),
        }
    }

    /// 在背景做線上字幕的工作，做完時叫醒介面
    fn spawn_online(&self, job: Job) -> Receiver<JobResult> {
        let ctx = self.egui_ctx.clone();
        core::spawn(job, &self.online, &self.online_subs.account, move || {
            ctx.request_repaint()
        })
    }

    /// 搜尋第 `page` 頁（舊的搜尋還在跑就不等它：結果到了也不用）
    fn start_online_search(&mut self, page: u32) {
        if let Err(e) = self.online_usable() {
            self.online_subs.status = Status::Failed(e);
            return;
        }
        let ui = &self.online_subs;
        // 正在下載：下載完再搜尋（下載綁在自己的影片上，不受影響）
        if matches!(&ui.job, Some(p) if p.what != Busy::Search) {
            self.online_subs.search_queued = true;
            return;
        }
        let Some(video) = ui.video.clone() else { return };
        let settings = &self.settings.online_subs;
        let mut job = SearchJob::new(
            settings.provider,
            video,
            ui.query.trim(),
            settings,
            self.settings.language,
        );
        job.page = page.max(1);
        let rx = self.spawn_online(Job::Search(job));
        let ui = &mut self.online_subs;
        ui.search_queued = false;
        ui.job = Some(Pending {
            what: Busy::Search,
            rx,
            cancel: None,
        });
        ui.status = Status::Idle;
    }

    /// 展開射手網（偽）的一筆字幕
    fn start_online_detail(&mut self, sub_id: u64) {
        let token = self.settings.online_subs.assrt_token.clone();
        let rx = self.spawn_online(Job::AssrtDetail { sub_id, token });
        self.online_subs.job = Some(Pending {
            what: Busy::Detail(sub_id),
            rx,
            cancel: None,
        });
        self.online_subs.status = Status::Idle;
    }

    /// 這一列的結果
    fn online_row(&self, key: RowKey) -> Option<&SubResult> {
        let ui = &self.online_subs;
        let entry = ui.page.as_ref()?.results.get(key.0)?;
        match (key.1, &entry.id) {
            (None, _) => Some(entry),
            (Some(j), ResultId::Assrt { sub_id, .. }) => ui.details.get(sub_id)?.as_ref().ok()?.get(j),
            (Some(_), _) => None,
        }
    }

    /// 下載這一列（存在搜尋的那部影片旁邊）
    fn start_online_download(&mut self, key: RowKey) {
        let ui = &self.online_subs;
        if ui.stale || ui.busy() {
            return;
        }
        let (Some(row), Some(page)) = (self.online_row(key), ui.page.as_ref()) else {
            return;
        };
        let Some(job) = DownloadJob::for_result(row, page.video.clone(), self.settings.online_subs.save_next_to_video)
        else {
            return;
        };
        self.online_subs.selected = Some(key);
        self.run_online_download(job);
    }

    fn run_online_download(&mut self, mut job: DownloadJob) {
        self.online_subs.downloads.prepare(&mut job);
        let cancel = job.cancel.clone();
        let rx = self.spawn_online(Job::Download(job));
        let ui = &mut self.online_subs;
        ui.job = Some(Pending {
            what: Busy::Download,
            rx,
            cancel: Some(cancel),
        });
        ui.status = Status::Idle;
    }

    /// 關掉視窗（✕、Esc）：正在下載的取消
    pub(super) fn close_online_subs(&mut self) {
        self.online_subs.open = false;
        self.online_subs.cancel_download();
    }

    /// 打開登入對話框（`reason` = 為什麼要登入）
    fn open_online_login(&mut self, reason: Option<NetError>) {
        let user = self.settings.online_subs.opensubtitles_user.clone();
        self.online_subs.login = Some(LoginForm {
            user,
            reason,
            ..Default::default()
        });
    }

    fn start_online_logout(&mut self) {
        if self.online_subs.account_job.is_some() {
            return;
        }
        let rx = self.spawn_online(Job::Logout);
        self.online_subs.account_job = Some(Pending {
            what: Busy::Logout,
            rx,
            cancel: None,
        });
    }

    /// 每一幀：換了影片時跟著換、收背景工作的結果（視窗關著也收：下載的連結、次數要記下）
    pub(super) fn online_subs_tick(&mut self) {
        self.online_follow_video();
        self.poll_online_job();
        self.poll_online_account_job();
    }

    /// 視窗開著時換了影片（自動接下一個、開了別的檔案）：自動搜尋開著就重新搜尋，不然把舊的結果灰掉
    fn online_follow_video(&mut self) {
        if !self.online_subs.open {
            return;
        }
        let st = &self.player.state;
        let Some(path) = st.path.clone().filter(|_| st.loaded) else {
            return;
        };
        let ui = &self.online_subs;
        if ui.video.as_ref().is_none_or(|v| v.path == path) {
            return;
        }
        if self.settings.online_subs.auto_search && self.online_usable().is_ok() {
            self.online_new_video(path);
        } else {
            let query = self.online_query_for(&path);
            let ui = &mut self.online_subs;
            ui.video = Some(VideoRef::new(path));
            ui.query = query;
            ui.stale = ui.page.is_some();
            ui.selected = None;
            if matches!(ui.status, Status::Found { .. }) {
                ui.status = Status::Idle;
            }
        }
    }

    fn poll_online_job(&mut self) {
        let Some(p) = &self.online_subs.job else { return };
        let result = match p.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.online_subs.job = None;
                return;
            }
        };
        let what = p.what;
        // 關掉視窗（取消）之後才回來的結果：不再打開登入對話框
        let cancelled = p.cancel.as_ref().is_some_and(|c| c.load(Ordering::SeqCst));
        self.online_subs.job = None;
        match result {
            JobResult::Search { video, result, .. } => self.on_online_search(video, result),
            JobResult::Detail { sub_id, result } => {
                let ui = &mut self.online_subs;
                if let Err(e) = &result {
                    ui.status = Status::Failed(e.clone());
                }
                ui.details.insert(sub_id, result);
            }
            r @ JobResult::Download { .. } => self.on_online_download(r, cancelled),
            JobResult::Login(_) | JobResult::Logout => {}
        }
        debug_assert!(what != Busy::Login && what != Busy::Logout);
        if self.online_subs.search_queued && self.online_subs.job.is_none() {
            self.start_online_search(1);
        }
    }

    fn on_online_search(&mut self, video: VideoRef, result: Result<SearchPage, NetError>) {
        let ui = &mut self.online_subs;
        let current = ui.video.as_ref().is_some_and(|v| v.path == video.path);
        // 特徵碼算過了：同一部影片再搜尋時不用再讀檔案
        if current && let Some(v) = ui.video.as_mut() {
            v.hash = v.hash.or(video.hash);
        }
        match result {
            Ok(page) => {
                ui.status = Status::Found {
                    n: page.results.len(),
                    matched: page.matched(),
                    total: page.total,
                };
                ui.page = Some(page);
                ui.details.clear();
                ui.selected = None;
                // 搜尋時影片已經換了（自動搜尋關著）：結果是上一部的
                ui.stale = !current;
            }
            Err(e) => {
                // 視窗關掉之後才回來的：只記下原因，不跳出登入對話框
                let login = e.offers_login() && ui.open;
                ui.status = Status::Failed(e.clone());
                if login {
                    self.open_online_login(Some(e));
                }
            }
        }
    }

    /// `cancelled`：使用者關掉了視窗（下載的取消設了）
    fn on_online_download(&mut self, result: JobResult, cancelled: bool) {
        self.online_subs.downloads.record(&result);
        let JobResult::Download {
            video, item, result, ..
        } = result
        else {
            return;
        };
        match result {
            Ok(saved) => {
                let name = saved
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // 只載入到搜尋的那部影片（還在播的話）
                let st = &self.player.state;
                let playing = st.loaded && st.path.as_deref() == Some(video.path.as_str());
                let mut loaded = false;
                if playing {
                    match self.player.add_subtitle(&saved.path.to_string_lossy()) {
                        Ok(()) => {
                            loaded = true;
                            self.osd(if saved.fell_back {
                                tr!(
                                    "已載入線上字幕（影片資料夾無法寫入，存在暫存資料夾）",
                                    "Loaded online subtitle (couldn't write to the video's folder; kept in the cache folder)"
                                )
                                .to_owned()
                            } else {
                                tf!("已載入線上字幕：{name}", "Loaded online subtitle: {name}")
                            });
                        }
                        Err(e) => self.osd(tf!("無法載入字幕：{e}", "Cannot load the subtitle: {e}")),
                    }
                } else {
                    let path = saved.path.display().to_string();
                    self.osd(tf!(
                        "字幕已下載，但影片已經換了，沒有載入：{path}",
                        "Subtitle downloaded, but another video is playing now, so it wasn't loaded: {path}"
                    ));
                }
                self.online_subs.status = Status::Saved { name, loaded };
            }
            // 使用者取消了（關掉視窗）：什麼都不說。取消時請求已經送出、伺服器回了錯誤（例如要登入）也一樣：
            // 不為了已經取消的下載跳出登入對話框
            Err(NetError::Cancelled) => self.online_subs.status = Status::Idle,
            Err(_) if cancelled || !self.online_subs.open => self.online_subs.status = Status::Idle,
            Err(e) => {
                if e.offers_login() {
                    // 登入後接著下載同一個
                    self.online_subs.after_login = Some(DownloadJob {
                        video,
                        item,
                        next_to_video: self.settings.online_subs.save_next_to_video,
                        cancel: Arc::new(AtomicBool::new(false)),
                        known_link: None,
                        saved_before: None,
                    });
                    self.open_online_login(Some(e.clone()));
                }
                self.online_subs.status = Status::Failed(e);
            }
        }
    }

    fn poll_online_account_job(&mut self) {
        let Some(p) = &self.online_subs.account_job else {
            return;
        };
        let result = match p.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.online_subs.account_job = None;
                return;
            }
        };
        self.online_subs.account_job = None;
        match result {
            JobResult::Login(Ok(info)) => {
                // 只記帳號名稱（下次登入時先填好）；密碼一律不存
                if self.settings.online_subs.opensubtitles_user != info.user {
                    self.settings.online_subs.opensubtitles_user = info.user.clone();
                    self.save_settings();
                }
                let ui = &mut self.online_subs;
                ui.login = None;
                ui.login_info = Some(info);
                if matches!(&ui.status, Status::Failed(e) if e.offers_login()) {
                    ui.status = Status::Idle;
                }
                // 剛才要登入才能下載的那一個：接著下載（視窗還開著、沒有別的工作時）
                if let Some(job) = ui.after_login.take()
                    && ui.open
                    && ui.job.is_none()
                {
                    self.run_online_download(job);
                }
            }
            JobResult::Login(Err(e)) => {
                if let Some(form) = &mut self.online_subs.login {
                    form.error = Some(e);
                    form.password.clear();
                }
            }
            JobResult::Logout => {
                let ui = &mut self.online_subs;
                ui.login_info = None;
                ui.after_login = None;
            }
            _ => {}
        }
    }

    /// 帳號那一行：已登入 / 未登入、剩下的次數（伺服器說了才有數字）、登入 / 登出
    fn online_account_row(&mut self, ui: &mut egui::Ui, clicks: &mut Vec<Click>) {
        let busy = self.online_subs.account_job.is_some();
        let account = &self.online_subs.account;
        match account.user() {
            Some(user) => {
                let r = ui.label(tf!("已登入：{user}", "Signed in: {user}"));
                // 帳號每天能下載幾個（登入時伺服器說的；剩幾個要等下載後才知道）
                if let Some(n) = self.online_subs.login_info.as_ref().and_then(|i| i.allowed_downloads) {
                    r.on_hover_text(tf!("這個帳號每天能下載 {n} 個", "This account can download {n} a day"));
                }
                if ui
                    .add_enabled(!busy, egui::Button::new(tr!("登出", "Sign out")))
                    .clicked()
                {
                    clicks.push(Click::SignOut);
                }
            }
            None => {
                ui.label(tr!("未登入", "Not signed in"));
                if ui
                    .add_enabled(!busy, egui::Button::new(tr!("登入…", "Sign in…")))
                    .clicked()
                {
                    clicks.push(Click::SignIn);
                }
            }
        }
        if let Some(q) = account.quota() {
            let n = q.remaining.max(0);
            ui.weak(tf!("今天還能下載 {n} 個", "{n} downloads left today"));
        }
    }

    /// 線上搜尋字幕的視窗
    pub(super) fn online_subs_window(&mut self, ctx: &egui::Context) {
        if !self.online_subs.open {
            return;
        }
        let mut open = true;
        let mut clicks = Vec::new();
        let screen = ctx.content_rect();
        egui::Window::new(tr!("線上搜尋字幕", "Search subtitles online"))
            .id(Id::new("online_subs_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            // 不跟設定視窗（60, 40）疊在一起
            .default_pos(pos2(110.0, 70.0))
            .default_size(vec2(680.0, 440.0).min((screen.size() - vec2(40.0, 90.0)).max(vec2(320.0, 200.0))))
            .show(ctx, |ui| self.online_subs_contents(ui, &mut clicks));
        for click in clicks {
            self.online_click(click);
        }
        if !open {
            self.close_online_subs();
        }
    }

    fn online_subs_contents(&mut self, ui: &mut egui::Ui, clicks: &mut Vec<Click>) {
        let problem = crate::theme::Palette::of(ui.visuals()).problem;
        let provider = self.settings.online_subs.provider;
        let busy = self.online_subs.busy();
        // 來源、語言、帳號
        ui.horizontal_wrapped(|ui| {
            ui.label(tr!("來源", "Source"));
            let mut chosen = provider;
            egui::ComboBox::from_id_salt("online_provider")
                .selected_text(provider.label())
                .show_ui(ui, |ui| {
                    for p in Provider::ALL {
                        ui.selectable_value(&mut chosen, p, p.label());
                    }
                });
            if chosen != provider {
                self.settings.online_subs.provider = chosen;
                self.save_settings();
                let ui_state = &mut self.online_subs;
                ui_state.page = None;
                ui_state.details.clear();
                ui_state.selected = None;
                ui_state.status = Status::Idle;
                // 舊來源的搜尋還在跑：不等它（結果不用）；下載照樣做完
                if matches!(&ui_state.job, Some(p) if p.what == Busy::Search) {
                    ui_state.job = None;
                }
                if self.online_subs.job.is_none() && self.online_usable().is_ok() && self.online_subs.video.is_some() {
                    clicks.push(Click::Search);
                }
            }
            if chosen == Provider::Opensubtitles {
                ui.separator();
                if self.language_menu(ui) {
                    self.save_settings();
                }
                ui.separator();
                self.online_account_row(ui, clicks);
            }
        });
        let usable = self.online_usable();
        let can_search = usable.is_ok() && !busy && self.online_subs.video.is_some();
        // 搜尋
        ui.horizontal(|ui| {
            let label = ui.label(tr!("搜尋", "Search"));
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut self.online_subs.query)
                        .id(Id::new("online_subs_query"))
                        .desired_width((ui.available_width() - 90.0).max(120.0)),
                )
                .labelled_by(label.id);
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let button = ui.add_enabled(can_search, egui::Button::new(tr!("搜尋", "Search")));
            if (button.clicked() || (enter && can_search)) && can_search {
                clicks.push(Click::Search);
            }
        });
        if let Err(e) = &usable {
            ui.colored_label(problem, e.message());
        }
        ui.separator();
        // 結果
        let footer = 96.0;
        // 名稱很長時可以左右捲（表格的格子不換行）
        egui::ScrollArea::both()
            .id_salt("online_results")
            .max_height((ui.available_height() - footer).max(80.0))
            .auto_shrink([false, false])
            .show(ui, |ui| self.online_results(ui, clicks));
        // 頁數（結果是上一部影片的：不能換頁，要重新搜尋）
        if let Some(page) = &self.online_subs.page
            && page.total_pages > 1
        {
            let (p, n) = (page.page, page.total_pages);
            let busy = busy || self.online_subs.stale;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!busy && p > 1, egui::Button::new(tr!("上一頁", "Previous")))
                    .clicked()
                {
                    clicks.push(Click::Page(p - 1));
                }
                ui.label(tf!("第 {p} / {n} 頁", "Page {p} of {n}"));
                if ui
                    .add_enabled(!busy && p < n, egui::Button::new(tr!("下一頁", "Next")))
                    .clicked()
                {
                    clicks.push(Click::Page(p + 1));
                }
            });
        }
        ui.separator();
        self.online_status_line(ui, problem);
        // 出處、會送出什麼
        ui.horizontal_wrapped(|ui| match provider {
            Provider::Opensubtitles => {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(OPENSUBTITLES_CREDIT);
                ui.hyperlink_to("OpenSubtitles.com", OPENSUBTITLES_URL);
            }
            Provider::Assrt => {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.label(tr!("字幕服務由 ", "Subtitle service provided by "));
                ui.hyperlink_to("assrt.net", ASSRT_URL);
                ui.label(tr!(" 提供", ""));
            }
        });
        ui.weak(match provider {
            Provider::Opensubtitles => tr!(
                "搜尋時會把檔名和檔案特徵碼送到 OpenSubtitles.com",
                "Searching sends the file name and a file fingerprint to OpenSubtitles.com"
            ),
            Provider::Assrt => tr!(
                "搜尋時會把檔名送到射手網（偽）",
                "Searching sends the file name to assrt.net"
            ),
        });
    }

    /// 語言的選單（勾選）：存在設定裡（跟「設定 → 字幕」同一份）。回傳是否改了
    fn language_menu(&mut self, ui: &mut egui::Ui) -> bool {
        let lang = self.settings.language;
        let current = self.settings.online_subs.effective_languages(lang);
        let names: Vec<String> = current
            .iter()
            .map(|c| {
                LANGUAGE_CHOICES
                    .iter()
                    .find(|(code, ..)| code == c)
                    .map_or_else(|| c.clone(), |(_, zh, en)| tr!(*zh, *en).to_owned())
            })
            .collect();
        let mut changed = false;
        ui.label(tr!("語言", "Languages"));
        ui.menu_button(names.join(tr!("、", ", ")), |ui| {
            changed = self.language_checkboxes(ui);
            ui.separator();
            // 其他語言（OpenSubtitles 的語言碼）：到「設定 → 字幕」的欄位打
            if ui.button(tr!("其他…", "Other…")).clicked() {
                self.settings_page = super::settings_window::Page::Subtitles;
                self.settings_open = true;
                self.online_subs.focus_other = true;
                // 設定視窗之前開過、被這個視窗蓋住時也拉到最上面
                ui.ctx()
                    .move_to_top(egui::LayerId::new(egui::Order::Middle, Id::new("settings_window")));
                ui.close();
            }
        });
        changed
    }

    /// 語言的勾選框（視窗的選單、設定頁）。第一次勾時把「依介面語言」的預設寫成明確的清單
    fn language_checkboxes(&mut self, ui: &mut egui::Ui) -> bool {
        let lang = self.settings.language;
        let mut list = self.settings.online_subs.effective_languages(lang);
        let mut changed = false;
        for (code, zh, en) in LANGUAGE_CHOICES {
            let mut on = list.iter().any(|c| c == code);
            // 只剩這一種：不能取消（空的清單 = 依介面語言，取消最後一個反而變成好幾種）
            let last = on && list.len() == 1;
            let r = ui
                .add_enabled(!last, egui::Checkbox::new(&mut on, tr!(*zh, *en)))
                .on_disabled_hover_text(tr!("至少要選一種語言", "Choose at least one language"));
            if r.changed() {
                if on {
                    list.push((*code).to_owned());
                } else {
                    list.retain(|c| c != code);
                }
                changed = true;
            }
        }
        if changed {
            self.settings.online_subs.languages = list;
        }
        changed
    }

    /// 結果的表格：一列一個字幕檔（射手網（偽）的一筆字幕展開後下面列出裡面的檔案）
    fn online_results(&mut self, ui: &mut egui::Ui, clicks: &mut Vec<Click>) {
        let state = &self.online_subs;
        let Some(page) = &state.page else {
            return;
        };
        if page.results.is_empty() {
            return;
        }
        let stale = state.stale;
        let busy = state.busy();
        let selected = state.selected;
        let assrt = page.provider == Provider::Assrt;
        let mut rows: Vec<(RowKey, &SubResult, bool)> = Vec::new();
        for (i, r) in page.results.iter().enumerate() {
            rows.push(((i, None), r, false));
            if let ResultId::Assrt { sub_id, file: None } = &r.id
                && let Some(Ok(files)) = state.details.get(sub_id)
            {
                rows.extend(files.iter().enumerate().map(|(j, f)| ((i, Some(j)), f, true)));
            }
        }
        ui.add_enabled_ui(!stale, |ui| {
            egui::Grid::new("online_results_grid")
                .num_columns(7)
                .striped(true)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for h in columns(page.provider) {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for (key, r, nested) in rows {
                        // 射手網（偽）沒有檔案特徵碼：沒有「相符」這一欄
                        if !assrt {
                            if r.hash_match {
                                ui.label("✔").on_hover_text(tr!(
                                    "跟這個檔案完全相符（檔案特徵碼一樣）",
                                    "Made for this exact file (same file fingerprint)"
                                ));
                            } else {
                                ui.label("");
                            }
                        }
                        ui.label(lang_name(r));
                        // 選取、雙擊下載：整列的感應放在名稱這一格（Grid 沒有整列的感應，R-C13）
                        let name = if nested {
                            format!("　{}", r.name)
                        } else {
                            r.name.clone()
                        };
                        let cell = ui.selectable_label(selected == Some(key), name);
                        let hover = detail(r);
                        let cell = if hover.is_empty() {
                            cell
                        } else {
                            cell.on_hover_text(hover)
                        };
                        if cell.clicked() {
                            clicks.push(Click::Select(key));
                        }
                        if cell.double_clicked() && r.downloadable() && !busy {
                            clicks.push(Click::Download(key));
                        }
                        ui.label(&r.format);
                        if assrt {
                            ui.label(&r.release_site);
                            ui.label(rating(r));
                        } else {
                            ui.label(downloads(r));
                        }
                        ui.label(tags(r));
                        match &r.id {
                            _ if r.downloadable() => {
                                if ui
                                    .add_enabled(!busy, egui::Button::new(tr!("下載並載入", "Download & load")))
                                    .clicked()
                                {
                                    clicks.push(Click::Download(key));
                                }
                            }
                            ResultId::Assrt { file: None, .. } if r.archive_only => match &r.page_url {
                                Some(url) => {
                                    ui.hyperlink_to(tr!("開啟網頁", "Open web page"), url);
                                }
                                None => {
                                    ui.label("");
                                }
                            },
                            ResultId::Assrt { sub_id, file: None } => match state.details.get(sub_id) {
                                Some(Ok(_)) => {
                                    if ui.button(tr!("收起", "Collapse")).clicked() {
                                        clicks.push(Click::Collapse(*sub_id));
                                    }
                                }
                                // 只有壓縮檔：請使用者到網頁下載
                                Some(Err(NetError::Archive { page })) => match page.as_ref().or(r.page_url.as_ref()) {
                                    Some(url) => {
                                        ui.hyperlink_to(tr!("開啟網頁", "Open web page"), url);
                                    }
                                    None => {
                                        ui.label("");
                                    }
                                },
                                _ => {
                                    if ui
                                        .add_enabled(!busy, egui::Button::new(tr!("展開", "Expand")))
                                        .clicked()
                                    {
                                        clicks.push(Click::Expand(*sub_id));
                                    }
                                }
                            },
                            _ => {
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                });
        });
    }

    /// 狀態那一行：搜尋中、下載中、找到幾個、失敗的原因、影片已經換了
    fn online_status_line(&self, ui: &mut egui::Ui, problem: egui::Color32) {
        let state = &self.online_subs;
        if let Some(p) = &state.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(match p.what {
                    Busy::Search | Busy::Detail(_) => tr!("搜尋中…", "Searching…"),
                    _ => tr!("下載中…", "Downloading…"),
                });
            });
            return;
        }
        if state.stale {
            ui.colored_label(
                problem,
                tr!("影片已經換了，請重新搜尋", "The video has changed; please search again"),
            );
            return;
        }
        match &state.status {
            Status::Idle => {
                ui.label("");
            }
            Status::Found { n: 0, .. } => {
                ui.label(tr!(
                    "找不到字幕。試試改短一點的名稱，或換一種語言",
                    "No subtitles found. Try a shorter name or another language"
                ));
            }
            Status::Found { n, matched, total } => {
                let n = total.map_or(*n as u64, |t| t.max(*n as u64));
                if *matched > 0 {
                    ui.label(tf!(
                        "找到 {n} 個字幕，其中 {matched} 個跟這個檔案完全相符",
                        "{n} subtitles found, {matched} made for this exact file"
                    ));
                } else {
                    ui.label(tf!("找到 {n} 個字幕", "{n} subtitles found"));
                }
            }
            Status::Saved { name, loaded } => {
                if *loaded {
                    ui.label(tf!("已載入：{name}", "Loaded: {name}"));
                } else {
                    ui.label(tf!("已下載：{name}", "Downloaded: {name}"));
                }
            }
            Status::Failed(e) => {
                ui.colored_label(problem, e.message());
            }
        }
    }

    fn online_click(&mut self, click: Click) {
        match click {
            Click::Select(key) => self.online_subs.selected = Some(key),
            Click::Download(key) => self.start_online_download(key),
            Click::Expand(sub_id) => {
                if !self.online_subs.busy() {
                    self.start_online_detail(sub_id);
                }
            }
            Click::Collapse(sub_id) => {
                self.online_subs.details.remove(&sub_id);
            }
            Click::Page(p) => {
                // 換頁用上一次搜尋的文字（不是輸入框裡改到一半的）。結果是別部影片的就不換頁（R-A2：
                // 上一部影片的搜尋文字配上這部影片，結果會被當成這部的）
                let ui = &mut self.online_subs;
                let Some(page) = &ui.page else { return };
                if ui.stale || ui.video.as_ref().is_none_or(|v| v.path != page.video.path) {
                    return;
                }
                ui.query = page.query.clone();
                self.start_online_search(p);
            }
            Click::Search => self.start_online_search(1),
            Click::SignIn => self.open_online_login(None),
            Click::SignOut => self.start_online_logout(),
        }
    }

    /// OpenSubtitles 的登入對話框（視窗、設定頁都能打開；Esc、點外面 = 取消）
    pub(super) fn online_login_modal(&mut self, ctx: &egui::Context) {
        let Some(form) = &mut self.online_subs.login else {
            return;
        };
        let busy = self.online_subs.account_job.is_some();
        let mut submit = false;
        let mut cancel = false;
        let modal = egui::Modal::new(Id::new("online_login")).show(ctx, |ui| {
            ui.set_max_width(420.0);
            let problem = crate::theme::Palette::of(ui.visuals()).problem;
            ui.heading(tr!("登入 OpenSubtitles", "Sign in to OpenSubtitles"));
            if let Some(reason) = &form.reason {
                ui.colored_label(problem, reason.message());
            }
            ui.add_space(6.0);
            egui::Grid::new("online_login_grid")
                .num_columns(2)
                .spacing([10.0, 6.0])
                .show(ui, |ui| {
                    let label = ui.label(tr!("帳號", "Username"));
                    ui.add(
                        egui::TextEdit::singleline(&mut form.user)
                            .id(Id::new("online_login_user"))
                            .desired_width(240.0),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label(tr!("密碼", "Password"));
                    let r = ui
                        .add(
                            egui::TextEdit::singleline(&mut form.password)
                                .id(Id::new("online_login_password"))
                                .password(true)
                                .desired_width(240.0),
                        )
                        .labelled_by(label.id);
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.end_row();
                });
            if let Some(e) = &form.error {
                ui.colored_label(problem, e.message());
            }
            ui.add_space(4.0);
            ui.weak(tr!(
                "密碼不會存檔。登入狀態（不是密碼）保留 24 小時，按「登出」就清掉。",
                "Your password isn't saved. You stay signed in for 24 hours (the password is not kept); \
                 Sign out clears it."
            ));
            ui.hyperlink_to(tr!("註冊帳號（免費）", "Create a free account"), SIGN_UP_URL);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !busy && !form.user.trim().is_empty() && !form.password.is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new(tr!("登入", "Sign in")))
                    .clicked()
                {
                    submit = true;
                }
                if ui.button(tr!("取消", "Cancel")).clicked() {
                    cancel = true;
                }
                if busy {
                    ui.spinner();
                }
            });
        });
        // 對話框開著：下一幀的按鍵都交給它（Esc 只關對話框，不關後面的視窗、不離開全螢幕）
        self.modal_open = true;
        if cancel || modal.should_close() {
            let ui = &mut self.online_subs;
            ui.login = None;
            ui.after_login = None;
            return;
        }
        let Some(form) = &mut self.online_subs.login else {
            return;
        };
        if submit && !busy && !form.user.trim().is_empty() && !form.password.is_empty() {
            form.error = None;
            // 密碼只送這一次：交給背景工作後就從對話框清掉
            let job = Job::Login {
                user: form.user.trim().to_owned(),
                password: std::mem::take(&mut form.password),
            };
            let rx = self.spawn_online(job);
            self.online_subs.account_job = Some(Pending {
                what: Busy::Login,
                rx,
                cancel: None,
            });
        }
    }

    /// 「設定 → 字幕」的「線上字幕」區塊；回傳是否改了設定（要存檔，R-C13）
    pub(super) fn online_settings_section(&mut self, ui: &mut egui::Ui) -> bool {
        self.restore_online_account();
        let mut changed = false;
        let problem = crate::theme::Palette::of(ui.visuals()).problem;
        let search = self.keymap.labeled(
            tr!("線上搜尋字幕…", "Search subtitles online…"),
            Command::SearchSubtitles,
        );
        ui.strong(tr!("線上字幕", "Online subtitles"));
        ui.weak(tf!(
            "字幕選單的「{search}」：依檔名與檔案特徵碼在 OpenSubtitles.com、射手網（偽）搜尋，下載後馬上載入。",
            "“{search}” in the Subtitles menu searches OpenSubtitles.com or assrt.net by file name and \
             fingerprint, and loads the download right away."
        ));
        ui.add_space(4.0);
        ui.label(tr!("搜尋語言", "Languages"));
        ui.horizontal_wrapped(|ui| changed |= self.language_checkboxes(ui));
        // 其他語言：OpenSubtitles 的語言碼，以逗號分隔（離開欄位才寫回）
        let lang = self.settings.language;
        let others = || {
            self.settings
                .online_subs
                .effective_languages(lang)
                .into_iter()
                .filter(|c| !LANGUAGE_CHOICES.iter().any(|(code, ..)| code == c))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut draft = self.online_subs.other_draft.take().unwrap_or_else(others);
        ui.horizontal(|ui| {
            let label = ui.label(tr!(
                "其他語言（OpenSubtitles 的語言碼，以逗號分隔）",
                "Other languages (OpenSubtitles codes, comma separated)"
            ));
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut draft)
                        .id(Id::new("online_other_languages"))
                        .desired_width(140.0),
                )
                .labelled_by(label.id);
            if std::mem::take(&mut self.online_subs.focus_other) {
                r.request_focus();
                r.scroll_to_me(Some(egui::Align::Center));
            }
            if r.lost_focus() {
                let s = &mut self.settings.online_subs;
                let mut list: Vec<String> = s
                    .effective_languages(lang)
                    .into_iter()
                    .filter(|c| LANGUAGE_CHOICES.iter().any(|(code, ..)| code == c))
                    .collect();
                list.extend(draft.split([',', '，', '、', ' ']).map(str::to_owned));
                let cleaned = core::OnlineSubSettings {
                    languages: list,
                    ..Default::default()
                }
                .sanitized()
                .languages;
                // 全部清掉：不改（空的清單 = 依介面語言，不是「沒有語言」）
                if !cleaned.is_empty() && cleaned != s.effective_languages(lang) {
                    s.languages = cleaned;
                    changed = true;
                }
            } else if r.has_focus() {
                self.online_subs.other_draft = Some(draft.clone());
            }
        });
        let s = &mut self.settings.online_subs;
        changed |= ui
            .checkbox(
                &mut s.save_next_to_video,
                tr!(
                    "下載的字幕存到影片旁邊（下次開啟會自動載入）",
                    "Save downloaded subtitles next to the video (loaded automatically next time)"
                ),
            )
            .on_hover_text(tr!(
                "存成「影片名.zh-TW.srt」，不會覆蓋已經有的檔案；影片的資料夾寫不進去、或是網路影片時放在暫存資料夾。",
                "Saved as “video.zh-TW.srt”, never overwriting a file; when the video's folder can't be written, \
                 or for network videos, it goes to the cache folder."
            ))
            .changed();
        changed |= ui
            .checkbox(
                &mut s.auto_search,
                tr!(
                    "打開視窗、換了影片時自動搜尋",
                    "Search automatically when the window opens or the video changes"
                ),
            )
            .changed();
        changed |= ui
            .checkbox(&mut s.hearing_impaired, tr!("包含聽障字幕", "Include SDH subtitles"))
            .changed();
        changed |= ui
            .checkbox(
                &mut s.machine_translated,
                tr!("包含機器翻譯的字幕", "Include machine-translated subtitles"),
            )
            .changed();
        ui.add_space(6.0);
        // OpenSubtitles 帳號
        ui.horizontal_wrapped(|ui| {
            ui.label(tr!(
                "OpenSubtitles 帳號（下載時要登入）：",
                "OpenSubtitles account (needed to download): "
            ));
            if self.online.os_key.is_some() {
                let mut clicks = Vec::new();
                self.online_account_row(ui, &mut clicks);
                for c in clicks {
                    self.online_click(c);
                }
            } else {
                ui.colored_label(
                    problem,
                    tr!(
                        "這個版本沒有 OpenSubtitles 的 API 金鑰（自行編譯的版本）",
                        "This build has no OpenSubtitles API key (self-built)"
                    ),
                );
            }
        });
        // 射手網（偽）的 Token：照輸入的樣子存，用的時候才檢查（R-C7）
        ui.horizontal(|ui| {
            let label = ui.label(tr!("射手網（偽）Token", "assrt.net token"));
            changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut self.settings.online_subs.assrt_token)
                        .id(Id::new("online_assrt_token"))
                        .password(true)
                        .desired_width(240.0),
                )
                .labelled_by(label.id)
                .changed();
            ui.hyperlink_to(tr!("怎麼取得…", "How to get one…"), ASSRT_DOC_URL);
        });
        let token = &self.settings.online_subs.assrt_token;
        if !token.trim().is_empty()
            && let Err(e) = assrt::check_token(token)
        {
            ui.colored_label(
                problem,
                tf!(
                    "{}（應該是 32 個英文字母和數字）",
                    "{} (it should be 32 letters and digits)",
                    e.message()
                ),
            );
        }
        ui.weak(tr!(
            "只在使用「線上搜尋字幕」時連到 OpenSubtitles.com、射手網（偽），送出檔名與檔案特徵碼。",
            "VitaScope connects to OpenSubtitles.com or assrt.net only when you use “Search subtitles online”, \
             sending the file name and a file fingerprint."
        ));
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online_subs::{Provider, ResultId};

    fn row(code: &str) -> SubResult {
        let mut r = SubResult {
            lang_code: code.into(),
            ..sample()
        };
        r.name = "Movie.2019".into();
        r
    }

    fn sample() -> SubResult {
        let page = core::opensubtitles::parse_search(
            br#"{"data":[{"attributes":{"language":"en","release":"x","files":[{"file_id":1}]}}]}"#,
            &[],
        )
        .unwrap();
        page.results.into_iter().next().unwrap()
    }

    #[test]
    fn row_texts() {
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(lang_name(&row("zh-tw")), "繁體中文");
        assert_eq!(lang_name(&row("pt-br")), "pt-br");
        let mut r = row("ze");
        assert_eq!(lang_name(&r), "中英雙語");
        r.trusted = true;
        r.auto_translated = true;
        r.machine_translated = true;
        r.hearing_impaired = true;
        assert_eq!(tags(&r), "信任、聽障、自動翻譯、機器翻譯");
        // 介面上不能出現兩個英文字母的那個縮寫（§1.8）
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(tags(&r), "Trusted, SDH, Auto-translated, Machine-translated");
        assert_eq!(lang_name(&row("zh-cn")), "Chinese (Simplified)");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        r.downloads = Some(1203);
        assert_eq!(downloads(&r), "1203");
        r.downloads = None;
        assert_eq!(downloads(&r), "");
        r.rating = Some(7.5);
        assert_eq!(rating(&r), "7.5");
        r.rating = Some(0.0);
        assert_eq!(rating(&r), "", "沒有人評");
        assert_eq!(columns(Provider::Opensubtitles)[4], "下載數");
        assert_eq!(columns(Provider::Assrt)[3..5], ["字幕組", "評分"]);
        assert!(matches!(r.id, ResultId::Os { .. }) && r.provider == Provider::Opensubtitles);
    }
}
