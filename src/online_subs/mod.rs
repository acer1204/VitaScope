//! 線上搜尋字幕的核心（沒有介面；視窗在 `app/online_subs.rs`）：設定、搜尋、下載、存檔、登入。
//!
//! 來源：
//! - **OpenSubtitles.com**（[`opensubtitles`]）：影戲自己的 API 金鑰（建置時從 CI 的 secret 放進正式版，
//!   OpenSubtitles 禁止讓使用者自己填金鑰）。**搜尋不用登入；下載可能要登入**（免費金鑰每個 IP 每天 5 個免登入，
//!   之後要用使用者自己的帳號；伺服器說要登入時回傳 [`NetError::NeedLogin`]，介面打開登入視窗：R-A1）。
//!   登入憑證（JWT，24 小時）存在快取資料夾的 `online/opensubtitles-session.json`，**從不存密碼**（Q3 = a）。
//! - **射手網（偽）assrt.net**（[`assrt`]）：使用者自己的 API Token（Q7 = a）。
//!
//! 規則：
//! - 只在使用者動作時連網（打開視窗、搜尋、下載、登入）。自動測試的 [`OnlineEnv`] 預設離線（[`http::OfflineHttp`]）。
//! - 背景執行緒只回傳 [`NetError`]，給使用者看的文字由介面執行緒呼叫 [`NetError::message`] 產生（介面語言是每個執行緒自己的）。
//! - 下載的字幕：檢查內容（[`sniff`]）、依內容決定語言標記（[`content_tag`]，R-B12），存到影片旁邊
//!   `影片名.zh-TW.srt`（Q6 = a），寫不進去時放快取資料夾；**絕不覆蓋**已經有的檔案（內容一樣就直接用那個檔案）。
//!   先寫暫存檔，換成正式名稱之前看取消（R-B10）。
//! - 送出去的只有整理過的檔名（[`clean_query`]）與檔案特徵碼（[`crate::filehash`]）。金鑰、登入憑證、Token 只放在標頭，
//!   不放進網址、不印出來。

pub mod assrt;
pub mod http;
pub mod opensubtitles;

use crate::filehash::FileHash;
use crate::subs::SubLang;
use http::Http;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ───────────────────────────── 設定 ─────────────────────────────

/// 字幕來源
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    #[default]
    Opensubtitles,
    Assrt,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Opensubtitles, Provider::Assrt];

    pub fn site(self) -> Site {
        match self {
            Provider::Opensubtitles => Site::OpenSubtitles,
            Provider::Assrt => Site::Assrt,
        }
    }

    /// 選單上的名稱
    pub fn label(self) -> &'static str {
        match self {
            Provider::Opensubtitles => "OpenSubtitles.com",
            Provider::Assrt => crate::tr!("射手網（偽）assrt.net", "assrt.net"),
        }
    }
}

/// 搜尋語言的選項：（OpenSubtitles 的語言碼, 中文, English）
pub const LANGUAGE_CHOICES: &[(&str, &str, &str)] = &[
    ("zh-tw", "繁體中文", "Chinese (Traditional)"),
    ("zh-cn", "簡體中文", "Chinese (Simplified)"),
    ("ze", "中英雙語", "Chinese bilingual"),
    ("en", "英文", "English"),
    ("ja", "日文", "Japanese"),
    ("ko", "韓文", "Korean"),
];

/// 最多記幾個搜尋語言
pub const MAX_LANGUAGES: usize = 12;

/// 線上字幕（設定頁「字幕」、線上搜尋字幕視窗）
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnlineSubSettings {
    /// 上次用的來源
    pub provider: Provider,
    /// 搜尋語言（OpenSubtitles 的語言碼，依偏好排序）；空的 = 依介面語言（[`Self::effective_languages`]）
    pub languages: Vec<String>,
    /// 下載的字幕存到影片旁邊（下次開啟自動載入）；關掉、或寫不進去時放快取資料夾
    pub save_next_to_video: bool,
    /// 包含聽障字幕（關掉 = 搜尋時排除）
    pub hearing_impaired: bool,
    /// 包含機器翻譯的字幕（OpenSubtitles 預設排除）
    pub machine_translated: bool,
    /// 打開視窗、換了影片時自動搜尋
    pub auto_search: bool,
    /// OpenSubtitles 的帳號名稱（只記名稱，登入視窗先填好；密碼一律不存）
    pub opensubtitles_user: String,
    /// 射手網（偽）的 API Token（使用者自己的）。照使用者輸入的樣子保存，用的時候才檢查格式（R-C7）
    pub assrt_token: String,
}

impl Default for OnlineSubSettings {
    fn default() -> Self {
        OnlineSubSettings {
            provider: Provider::Opensubtitles,
            languages: Vec::new(),
            save_next_to_video: true,
            hearing_impaired: true,
            machine_translated: false,
            auto_search: true,
            opensubtitles_user: String::new(),
            assrt_token: String::new(),
        }
    }
}

/// Token 不印出來（設定可能被整份印到記錄裡）
impl std::fmt::Debug for OnlineSubSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnlineSubSettings")
            .field("provider", &self.provider)
            .field("languages", &self.languages)
            .field("save_next_to_video", &self.save_next_to_video)
            .field("hearing_impaired", &self.hearing_impaired)
            .field("machine_translated", &self.machine_translated)
            .field("auto_search", &self.auto_search)
            .field("opensubtitles_user", &self.opensubtitles_user)
            .field("assrt_token", &redacted(&self.assrt_token))
            .finish()
    }
}

impl OnlineSubSettings {
    /// 讀檔後整理：語言碼去空白、轉小寫，只留 `xx`、`xxx`、`xx-yy` 這種寫法，去掉重複，最多 [`MAX_LANGUAGES`] 個。
    /// Token 不動（照使用者輸入的保存，用的時候才檢查：R-C7）
    pub fn sanitized(mut self) -> Self {
        let mut seen = std::collections::HashSet::new();
        self.languages = self
            .languages
            .iter()
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| valid_language(l) && seen.insert(l.clone()))
            .take(MAX_LANGUAGES)
            .collect();
        self.opensubtitles_user = self.opensubtitles_user.trim().to_owned();
        self
    }

    /// 實際搜尋的語言：設定的，或依介面語言（中文：繁、簡、中英雙語、英文；English：英文）
    pub fn effective_languages(&self, lang: crate::i18n::Lang) -> Vec<String> {
        if !self.languages.is_empty() {
            return self.languages.clone();
        }
        let codes: &[&str] = match lang {
            crate::i18n::Lang::ZhTw => &["zh-tw", "zh-cn", "ze", "en"],
            crate::i18n::Lang::En => &["en"],
        };
        codes.iter().map(|c| (*c).to_owned()).collect()
    }
}

/// `xx`、`xxx`、`xx-yy`（OpenSubtitles 的語言碼）
fn valid_language(code: &str) -> bool {
    let (main, region) = match code.split_once('-') {
        Some((m, r)) => (m, Some(r)),
        None => (code, None),
    };
    let letters = |s: &str| s.bytes().all(|b| b.is_ascii_lowercase());
    (2..=3).contains(&main.len()) && letters(main) && region.is_none_or(|r| r.len() == 2 && letters(r))
}

// ───────────────────────────── 錯誤 ─────────────────────────────

/// 哪個網站
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Site {
    OpenSubtitles,
    Assrt,
}

impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Site::OpenSubtitles => "OpenSubtitles.com",
            Site::Assrt => crate::tr!("射手網（偽）", "assrt.net"),
        }
    }
}

/// 線上字幕失敗的原因（背景執行緒回傳；文字由介面執行緒的 [`NetError::message`] 產生）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// 離線（自動測試的預設環境）
    Offline,
    /// 這個版本沒有 OpenSubtitles 的金鑰（自己編譯的版本）
    NoKey,
    /// 沒有填射手網（偽）的 Token
    NoToken,
    /// 射手網（偽）的 Token 不正確（格式不對，或伺服器說無效）
    InvalidToken,
    /// 下載 OpenSubtitles 的字幕要登入（R-A1）
    NeedLogin,
    /// 登入已過期（伺服器不認得登入憑證了）；已經登出，不會改用未登入的身分再試（R-A1）
    LoginExpired,
    /// 登入時帳號或密碼不正確（不自動重試）
    BadLogin,
    /// OpenSubtitles 拒絕了這個版本（金鑰、User-Agent）
    Forbidden,
    /// 今天的下載次數用完了；`reset_at` = 重置的時間（Unix 秒），`signed_in` = 是不是已經登入
    Quota { reset_at: Option<u64>, signed_in: bool },
    /// 下載連結已失效
    LinkExpired,
    /// 查詢太頻繁
    RateLimited(Site),
    /// 搜尋文字太短（射手網（偽）的伺服器說的；用戶端不數字數：R-C6）
    QueryTooShort,
    /// 找不到（射手網（偽）的字幕詳細資料）
    NotFound(Site),
    /// 網站的錯誤：HTTP 狀態碼（< 1000），或射手網（偽）的錯誤碼
    Server { site: Site, code: u32 },
    /// 連不上：原文（技術細節，不翻譯）
    Connect { site: Site, detail: String },
    /// 逾時
    Timeout(Site),
    /// 回應看不懂（例如公共網路的登入頁、HTML）
    BadResponse(Site),
    /// 下載的檔案太大，不像字幕
    TooLarge,
    /// 下載的檔案不是字幕
    NotSubtitle,
    /// 只有壓縮檔（要到網頁下載）；`page` = 字幕的網頁
    Archive { page: Option<String> },
    /// 存檔失敗：原文（系統的錯誤訊息）
    Disk(String),
    /// 沒有可以存放的資料夾（影片旁邊寫不進去，也沒有快取資料夾）
    NoSaveFolder,
    /// 同一個名稱的字幕已經編到最後一號（`影片.zh-TW.99.srt`）
    NoFreeName,
    /// 網址（或轉址的目的地）不是 https
    NotHttps(Site),
    /// 使用者取消了
    Cancelled,
}

impl NetError {
    /// 給使用者看的說明（介面執行緒呼叫）
    pub fn message(&self) -> String {
        use crate::{tf, tr};
        match self {
            NetError::Offline => tr!("離線模式：影戲不會連到網路", "Offline: VitaScope won't connect to the network").into(),
            NetError::NoKey => tr!(
                "這個版本沒有 OpenSubtitles 的 API 金鑰（自行編譯的版本），請改用射手網（偽），或從 GitHub 下載正式版",
                "This build has no OpenSubtitles API key (self-built). Use assrt.net, or download an official release from GitHub"
            )
            .into(),
            NetError::NoToken => tr!(
                "射手網（偽）需要你自己的 API Token：到 assrt.net 註冊後，在「設定 → 字幕」填入",
                "assrt.net needs your own API token: register at assrt.net, then enter it in Settings → Subtitles"
            )
            .into(),
            NetError::InvalidToken => tr!("射手網（偽）的 Token 不正確", "The assrt.net token is not valid").into(),
            NetError::NeedLogin => tr!(
                "下載 OpenSubtitles 的字幕需要登入（免費帳號）",
                "Downloading from OpenSubtitles needs a (free) account"
            )
            .into(),
            NetError::LoginExpired => {
                tr!("登入已過期，請重新登入", "Your sign-in has expired; please sign in again").into()
            }
            NetError::BadLogin => tr!("帳號或密碼不正確", "Wrong username or password").into(),
            NetError::Forbidden => tr!(
                "OpenSubtitles 拒絕了這個版本的連線，請更新影戲",
                "OpenSubtitles refused this version. Please update VitaScope"
            )
            .into(),
            NetError::Quota { reset_at, signed_in } => {
                let wait = reset_at.map(|t| wait_text(t.saturating_sub(now_unix())));
                match (wait, signed_in) {
                    (Some(w), false) => tf!(
                        "今天的下載次數用完了，{w}後重置。登入 OpenSubtitles 帳號可以下載更多",
                        "You've used today's downloads; they reset in {w}. Sign in to OpenSubtitles to download more"
                    ),
                    (None, false) => tr!(
                        "今天的下載次數用完了。登入 OpenSubtitles 帳號可以下載更多",
                        "You've used today's downloads. Sign in to OpenSubtitles to download more"
                    )
                    .into(),
                    (Some(w), true) => tf!(
                        "今天的下載次數用完了，{w}後重置",
                        "You've used today's downloads; they reset in {w}"
                    ),
                    (None, true) => tr!("今天的下載次數用完了", "You've used today's downloads").into(),
                }
            }
            NetError::LinkExpired => tr!(
                "下載連結已失效，請再按一次下載",
                "The download link expired; click Download again"
            )
            .into(),
            NetError::RateLimited(Site::Assrt) => tr!(
                "查詢太頻繁，請稍等再試（射手網（偽）每分鐘 20 次）",
                "Too many requests; try again shortly (assrt.net allows 20 a minute)"
            )
            .into(),
            NetError::RateLimited(Site::OpenSubtitles) => tr!(
                "查詢太頻繁，請稍等幾秒再試",
                "Too many requests; try again in a few seconds"
            )
            .into(),
            NetError::QueryTooShort => tr!(
                "搜尋文字太短，請多打幾個字",
                "The search text is too short; type a few more characters"
            )
            .into(),
            NetError::NotFound(site) => {
                let site = site.name();
                tf!("{site} 找不到這個字幕", "{site} couldn't find this subtitle")
            }
            NetError::Server { site, code } => {
                let site = site.name();
                if *code < 1000 {
                    tf!(
                        "{site} 暫時無法使用（HTTP {code}）",
                        "{site} is temporarily unavailable (HTTP {code})"
                    )
                } else {
                    tf!(
                        "{site} 暫時無法使用（錯誤 {code}）",
                        "{site} is temporarily unavailable (error {code})"
                    )
                }
            }
            NetError::Connect { site, detail } => {
                let site = site.name();
                tf!("無法連線到 {site}：{detail}", "Cannot connect to {site}: {detail}")
            }
            NetError::Timeout(site) => {
                let site = site.name();
                tf!("連線逾時（{site}）", "{site} timed out")
            }
            NetError::BadResponse(site) => {
                let site = site.name();
                tf!("{site} 的回應格式不正確", "Unexpected response from {site}")
            }
            NetError::TooLarge => tr!(
                "下載的檔案太大，不像字幕",
                "The downloaded file is too large to be a subtitle"
            )
            .into(),
            NetError::NotSubtitle => tr!("下載的檔案不是字幕", "The downloaded file is not a subtitle").into(),
            NetError::Archive { .. } => tr!(
                "這個字幕只有壓縮檔，請在網頁下載後拖進影戲",
                "This subtitle is only available as an archive. Download it from the web page and drag it into VitaScope"
            )
            .into(),
            NetError::Disk(e) => tf!("無法儲存字幕：{e}", "Couldn't save the subtitle: {e}"),
            NetError::NoSaveFolder => tr!(
                "無法儲存字幕：沒有可以存放的資料夾",
                "Couldn't save the subtitle: there is no folder to save it in"
            )
            .into(),
            NetError::NoFreeName => tr!(
                "無法儲存字幕：同名的字幕太多了",
                "Couldn't save the subtitle: too many subtitles with this name"
            )
            .into(),
            NetError::NotHttps(site) => {
                let site = site.name();
                tf!(
                    "無法安全連線到 {site}（只接受 https）",
                    "Couldn't connect securely to {site} (https only)"
                )
            }
            NetError::Cancelled => tr!("已取消", "Cancelled").into(),
        }
    }

    /// 這個錯誤要打開 OpenSubtitles 的登入視窗（要登入、登入過期、未登入時次數用完）
    pub fn offers_login(&self) -> bool {
        matches!(
            self,
            NetError::NeedLogin | NetError::LoginExpired | NetError::Quota { signed_in: false, .. }
        )
    }

    /// 連線層的失敗換成這個網站的錯誤
    pub(crate) fn transport(site: Site, e: http::Transport) -> NetError {
        match e {
            http::Transport::Offline => NetError::Offline,
            http::Transport::Timeout => NetError::Timeout(site),
            http::Transport::TooLarge => NetError::TooLarge,
            http::Transport::Connect(detail) => NetError::Connect { site, detail },
            http::Transport::NotHttps => NetError::NotHttps(site),
        }
    }
}

/// 「3 小時 12 分」「5 分」
fn wait_text(secs: u64) -> String {
    let (h, m) = (secs / 3600, (secs % 3600).div_ceil(60).min(59));
    let m = if h == 0 { m.max(1) } else { m };
    match h {
        0 => crate::tf!("{m} 分鐘", "{m} min"),
        _ => crate::tf!("{h} 小時 {m} 分", "{h} h {m} min"),
    }
}

/// 現在的 Unix 時間（秒）
pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// `2022-04-08T13:03:15.000Z` → Unix 秒（只認 UTC 的寫法）
pub(crate) fn parse_utc(s: &str) -> Option<u64> {
    let s = s.trim().strip_suffix('Z').unwrap_or(s.trim());
    let (date, time) = s.split_once(['T', ' '])?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, mo, da) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec: f64 = t.next().unwrap_or("0").parse().ok()?;
    // 年份限定在合理的範圍：伺服器（或公共網路的登入頁）送來很大的年份時，下面的乘法才不會溢位
    if !(1970..=9999).contains(&y)
        || !(1..=12).contains(&mo)
        || !(1..=31).contains(&da)
        || !(0..24).contains(&h)
        || !(0..60).contains(&mi)
        || !(0.0..61.0).contains(&sec)
    {
        return None;
    }
    // 西元日期 → 1970-01-01 起的天數（Howard Hinnant 的 days_from_civil）
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec as i64).ok()
}

// ───────────────────────────── 結果 ─────────────────────────────

/// 搜尋、下載是為了哪部影片（G6：結果綁在這部影片上，換了影片就不再用；R-A2）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRef {
    /// 播放器的路徑（本機檔案）或網址
    pub path: String,
    /// 檔案特徵碼（本機檔案、≥ 128 KiB；搜尋時算好填進來）
    pub hash: Option<FileHash>,
}

impl VideoRef {
    pub fn new(path: impl Into<String>) -> VideoRef {
        VideoRef {
            path: path.into(),
            hash: None,
        }
    }

    /// 本機檔案（不是網址）
    pub fn local_path(&self) -> Option<&Path> {
        (!self.path.is_empty() && !self.path.contains("://")).then(|| Path::new(&self.path))
    }

    /// 是同一部影片：路徑一樣，兩邊都有特徵碼時特徵碼也一樣（同一個路徑換成別的檔案）
    pub fn same_video(&self, other: &VideoRef) -> bool {
        self.path == other.path
            && match (self.hash, other.hash) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
    }
}

/// 射手網（偽）字幕裡的一個檔案
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssrtFile {
    /// 檔名（只用來決定副檔名、語言標記；不當成路徑）
    pub name: String,
    /// 下載網址（會過期，不存檔）
    pub url: String,
    /// 網站寫的大小（例如 `48KB`）
    pub size: String,
}

/// 結果是哪一個
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultId {
    /// OpenSubtitles 的檔案
    Os { file_id: u64 },
    /// 射手網（偽）的字幕（`file` = None：還沒展開的一筆；Some：裡面的一個檔案）
    Assrt { sub_id: u64, file: Option<AssrtFile> },
}

/// 搜尋結果的一列
#[derive(Debug, Clone, PartialEq)]
pub struct SubResult {
    pub provider: Provider,
    pub id: ResultId,
    /// 網站的語言標示（OpenSubtitles 的語言碼 `zh-tw`；射手網（偽）的說明 `简 繁 英`）
    pub lang_code: String,
    pub lang: SubLang,
    /// 名稱 / 版本
    pub name: String,
    /// 格式（SRT、ASS…）
    pub format: String,
    pub downloads: Option<u64>,
    pub rating: Option<f64>,
    /// 跟這個檔案完全相符（OpenSubtitles 的 moviehash 相符）
    pub hash_match: bool,
    pub hearing_impaired: bool,
    /// OpenSubtitles 的 `ai_translated`：介面寫「自動翻譯」
    pub auto_translated: bool,
    /// 機器翻譯
    pub machine_translated: bool,
    /// 來自信任的上傳者
    pub trusted: bool,
    /// 第幾片（多片的字幕）
    pub cd: Option<u32>,
    /// 片名（年份、季、集）
    pub feature: String,
    pub uploader: String,
    pub upload_date: String,
    /// 射手網（偽）：字幕組
    pub release_site: String,
    /// 射手網（偽）：只有壓縮檔或 VobSub，要到網頁下載
    pub archive_only: bool,
    /// 字幕的網頁
    pub page_url: Option<String>,
}

impl SubResult {
    pub(crate) fn new(provider: Provider, id: ResultId) -> SubResult {
        SubResult {
            provider,
            id,
            lang_code: String::new(),
            lang: SubLang::Unknown,
            name: String::new(),
            format: String::new(),
            downloads: None,
            rating: None,
            hash_match: false,
            hearing_impaired: false,
            auto_translated: false,
            machine_translated: false,
            trusted: false,
            cd: None,
            feature: String::new(),
            uploader: String::new(),
            upload_date: String::new(),
            release_site: String::new(),
            archive_only: false,
            page_url: None,
        }
    }

    /// 可以直接下載（OpenSubtitles 的檔案、射手網（偽）展開後的檔案）
    pub fn downloadable(&self) -> bool {
        match &self.id {
            ResultId::Os { .. } => true,
            ResultId::Assrt { file, .. } => file.is_some() && !self.archive_only,
        }
    }
}

/// 一頁搜尋結果
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub provider: Provider,
    /// 搜尋的是哪部影片（含算好的特徵碼）
    pub video: VideoRef,
    /// 送出的搜尋文字
    pub query: String,
    /// 第幾頁（從 1 開始）
    pub page: u32,
    /// 總共幾頁（射手網（偽）不說：這一頁滿了就當成還有下一頁）
    pub total_pages: u32,
    /// 總共幾個（OpenSubtitles 才有）
    pub total: Option<u64>,
    pub results: Vec<SubResult>,
}

impl SearchPage {
    /// 跟這個檔案完全相符的有幾個
    pub fn matched(&self) -> usize {
        self.results.iter().filter(|r| r.hash_match).count()
    }
}

/// OpenSubtitles 的下載次數
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    /// 今天還能下載幾個
    pub remaining: i64,
    /// 重置的時間（Unix 秒）
    pub reset_at: Option<u64>,
}

/// 登入成功
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginInfo {
    pub user: String,
    /// 帳號每天能下載幾個（伺服器說的；剩幾個要等下載後才知道：R-C14）
    pub allowed_downloads: Option<i64>,
    pub vip: bool,
}

// ───────────────────────────── 工作 ─────────────────────────────

/// 一次搜尋
#[derive(Clone, PartialEq)]
pub struct SearchJob {
    pub provider: Provider,
    pub video: VideoRef,
    /// 搜尋文字（通常是 [`clean_query`] 整理過的檔名）
    pub query: String,
    /// 第幾頁（從 1 開始）
    pub page: u32,
    /// OpenSubtitles 的語言碼（依偏好排序）
    pub languages: Vec<String>,
    pub hearing_impaired: bool,
    pub machine_translated: bool,
    /// 射手網（偽）的 Token
    pub assrt_token: String,
}

/// Token 不印出來
impl std::fmt::Debug for SearchJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchJob")
            .field("provider", &self.provider)
            .field("video", &self.video)
            .field("query", &self.query)
            .field("page", &self.page)
            .field("languages", &self.languages)
            .field("hearing_impaired", &self.hearing_impaired)
            .field("machine_translated", &self.machine_translated)
            .field("assrt_token", &redacted(&self.assrt_token))
            .finish()
    }
}

/// 印出來時代替秘密的文字
fn redacted(secret: &str) -> &'static str {
    if secret.is_empty() { "" } else { "<hidden>" }
}

impl SearchJob {
    /// 照設定建立（第 1 頁）
    pub fn new(
        provider: Provider,
        video: VideoRef,
        query: impl Into<String>,
        settings: &OnlineSubSettings,
        lang: crate::i18n::Lang,
    ) -> SearchJob {
        SearchJob {
            provider,
            video,
            query: query.into(),
            page: 1,
            languages: settings.effective_languages(lang),
            hearing_impaired: settings.hearing_impaired,
            machine_translated: settings.machine_translated,
            assrt_token: settings.assrt_token.clone(),
        }
    }
}

/// 要下載的是什麼
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadItem {
    /// OpenSubtitles 的檔案；`lang_code` = 網站標的語言（內容判斷不出繁簡時用）
    Os { file_id: u64, lang_code: String },
    /// 射手網（偽）的檔案
    Assrt { file: AssrtFile },
}

/// 一次下載
#[derive(Debug, Clone)]
pub struct DownloadJob {
    /// 是為了哪部影片（存在它旁邊；G6 只在它還在播時載入）
    pub video: VideoRef,
    pub item: DownloadItem,
    /// 存到影片旁邊（設定）
    pub next_to_video: bool,
    /// 取消（G6：關掉視窗時設成 true；換成正式名稱之前會看，暫存檔刪掉：R-B10）
    pub cancel: Arc<AtomicBool>,
    /// 之前拿到、還有效的下載連結（OpenSubtitles：先用它，不再花一次下載次數：R-B11）
    pub known_link: Option<String>,
    /// 這個檔案這次已經存過的地方（還在就直接用，不再下載：R-B11）
    pub saved_before: Option<PathBuf>,
}

impl DownloadJob {
    /// 下載搜尋結果的這一列；不能直接下載的（射手網（偽）還沒展開、只有壓縮檔）回傳 None
    pub fn for_result(r: &SubResult, video: VideoRef, next_to_video: bool) -> Option<DownloadJob> {
        if !r.downloadable() {
            return None;
        }
        let item = match &r.id {
            ResultId::Os { file_id } => DownloadItem::Os {
                file_id: *file_id,
                lang_code: r.lang_code.clone(),
            },
            ResultId::Assrt { file, .. } => DownloadItem::Assrt { file: file.clone()? },
        };
        Some(DownloadJob {
            video,
            item,
            next_to_video,
            cancel: Arc::new(AtomicBool::new(false)),
            known_link: None,
            saved_before: None,
        })
    }
}

/// 下載好的字幕
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// 存在哪裡（G6 交給 `Player::add_subtitle`）
    pub path: PathBuf,
    /// 存在影片旁邊（下次開啟自動載入）
    pub in_video_dir: bool,
    /// 想存到影片旁邊但寫不進去，改放快取資料夾（G6 的 OSD 要說）
    pub fell_back: bool,
    /// 已經有內容一樣的檔案，直接用它（沒有另外存一份）
    pub reused: bool,
    /// 檔名裡的語言標記（`zh-TW`…；空的 = 沒有）
    pub tag: String,
}

/// 背景工作
#[derive(Clone)]
pub enum Job {
    Search(SearchJob),
    /// 射手網（偽）：展開一筆字幕，列出裡面的檔案
    AssrtDetail {
        sub_id: u64,
        token: String,
    },
    Download(DownloadJob),
    /// OpenSubtitles 登入（密碼只送這一次，之後丟掉）
    Login {
        user: String,
        password: String,
    },
    /// OpenSubtitles 登出（只在使用者按「登出」時：R-C2）
    Logout,
}

/// 密碼、Token 不印出來
impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Job::Search(job) => f.debug_tuple("Search").field(job).finish(),
            Job::AssrtDetail { sub_id, token } => f
                .debug_struct("AssrtDetail")
                .field("sub_id", sub_id)
                .field("token", &redacted(token))
                .finish(),
            Job::Download(job) => f.debug_tuple("Download").field(job).finish(),
            Job::Login { user, password } => f
                .debug_struct("Login")
                .field("user", user)
                .field("password", &redacted(password))
                .finish(),
            Job::Logout => f.write_str("Logout"),
        }
    }
}

/// 背景工作的結果
#[derive(Debug, Clone)]
pub enum JobResult {
    Search {
        provider: Provider,
        /// 搜尋的影片（含特徵碼；失敗時也有，介面用來比對）
        video: VideoRef,
        result: Result<SearchPage, NetError>,
    },
    Detail {
        sub_id: u64,
        result: Result<Vec<SubResult>, NetError>,
    },
    Download {
        video: VideoRef,
        item: DownloadItem,
        /// 下一次可以再用的 OpenSubtitles 下載連結（3 小時內有效：R-B11）：這次用的連結，失效（410）的也是
        /// （下一次先重試同一個連結）。None = 沒有拿到連結，或之前的連結重試過也失效、沒有拿到新的
        link: Option<String>,
        /// 伺服器說的下載次數（OpenSubtitles）
        quota: Option<Quota>,
        result: Result<Downloaded, NetError>,
    },
    Login(Result<LoginInfo, NetError>),
    Logout,
}

/// 在背景執行一個工作，完成時呼叫 `wake`（介面重畫）；結果從回傳的通道拿
pub fn spawn(
    job: Job,
    env: &OnlineEnv,
    account: &Arc<Account>,
    wake: impl Fn() + Send + 'static,
) -> mpsc::Receiver<JobResult> {
    let (tx, rx) = mpsc::channel();
    let env = env.clone();
    let account = account.clone();
    let started = std::thread::Builder::new()
        .name("vitascope-online".into())
        .spawn(move || {
            let result = run(job, &env, &account);
            let _ = tx.send(result);
            wake();
        });
    if let Err(e) = started {
        eprintln!("[vitascope] 無法開始線上字幕的工作：{e}");
    }
    rx
}

/// 執行一個工作（會等網路；在背景執行緒呼叫）
pub fn run(job: Job, env: &OnlineEnv, account: &Account) -> JobResult {
    account.restore();
    match job {
        Job::Search(job) => {
            let mut video = job.video.clone();
            let result = search(&job, env, account, &mut video);
            JobResult::Search {
                provider: job.provider,
                video,
                result,
            }
        }
        Job::AssrtDetail { sub_id, token } => JobResult::Detail {
            sub_id,
            result: assrt::detail(env, &token, sub_id),
        },
        Job::Download(job) => download(job, env, account),
        Job::Login { user, password } => JobResult::Login(opensubtitles::login(env, account, &user, &password)),
        Job::Logout => {
            opensubtitles::logout(env, account);
            JobResult::Logout
        }
    }
}

fn search(job: &SearchJob, env: &OnlineEnv, account: &Account, video: &mut VideoRef) -> Result<SearchPage, NetError> {
    match job.provider {
        Provider::Opensubtitles => {
            if env.os_key.is_none() {
                return Err(NetError::NoKey);
            }
            // 特徵碼只算本機檔案（讀開頭、結尾各 64 KiB；網路磁碟上可能要一點時間，所以在背景執行緒）
            if video.hash.is_none()
                && let Some(path) = video.local_path()
            {
                video.hash = crate::filehash::moviehash(path).ok().flatten();
            }
            opensubtitles::search(env, account, job, video)
        }
        Provider::Assrt => assrt::search(env, job, video),
    }
}

fn download(job: DownloadJob, env: &OnlineEnv, account: &Account) -> JobResult {
    let mut link = None;
    let mut quota = None;
    let result = (|| -> Result<Downloaded, NetError> {
        // 這次已經存過、檔案還在：直接用，不再花一次下載次數
        if let Some(path) = job.saved_before.as_ref().filter(|p| p.is_file()) {
            let in_video_dir =
                job.video.local_path().and_then(Path::parent) == path.parent() && path.parent().is_some();
            return Ok(Downloaded {
                path: path.clone(),
                in_video_dir,
                fell_back: false,
                reused: true,
                tag: String::new(),
            });
        }
        let (bytes, ext, fallback_tag) = match &job.item {
            DownloadItem::Os { file_id, lang_code } => {
                let got = opensubtitles::download(env, account, *file_id, job.known_link.as_deref(), &job.cancel);
                link = got.link;
                quota = got.quota;
                (got.bytes?, "srt".to_owned(), os_tag(lang_code))
            }
            DownloadItem::Assrt { file } => {
                let ext = file_ext(&file.name)
                    .filter(|e| allowed_ext(e))
                    .ok_or(NetError::NotSubtitle)?;
                let bytes = assrt::fetch_file(env, &file.url)?;
                (bytes, ext, assrt::name_tag(&file.name))
            }
        };
        if job.cancel.load(Ordering::SeqCst) {
            return Err(NetError::Cancelled);
        }
        sniff(&bytes, &ext)?;
        let tag = content_tag(&bytes, &ext, &fallback_tag);
        save_subtitle(
            &bytes,
            &job.video,
            &tag,
            &ext,
            job.next_to_video,
            env.cache_dir.as_deref(),
            &job.cancel,
        )
    })();
    JobResult::Download {
        video: job.video,
        item: job.item,
        link,
        quota,
        result,
    }
}

// ───────────────────────────── 環境、帳號 ─────────────────────────────

/// 線上字幕用的環境：連線、金鑰、存檔的地方（`Launch.online`；預設離線，只有 main.rs 設成真的）
#[derive(Clone)]
pub struct OnlineEnv {
    pub http: Arc<dyn Http>,
    /// OpenSubtitles 的 API 金鑰；None = 這個版本不能用 OpenSubtitles
    pub os_key: Option<String>,
    /// OpenSubtitles API 的位址（測試指向本機的假伺服器）
    pub os_root: String,
    /// 射手網（偽）API 的位址
    pub assrt_root: String,
    /// 射手網（偽）連不上時再試一次的位址（網站文件寫的另一個網域）
    pub assrt_alt_root: Option<String>,
    /// 影片旁邊寫不進去時放的地方（快取資料夾的 `subs/online`）；None = 沒有地方放
    pub cache_dir: Option<PathBuf>,
    /// OpenSubtitles 登入狀態的檔案；None = 只放在記憶體
    pub session_file: Option<PathBuf>,
    /// OpenSubtitles 的請求間隔（每秒最多 5 次、登入每秒 1 次）
    pub throttle: Arc<Throttle>,
}

impl Default for OnlineEnv {
    fn default() -> Self {
        OnlineEnv::offline()
    }
}

/// 金鑰不印出來
impl std::fmt::Debug for OnlineEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnlineEnv")
            .field("offline", &self.http.is_offline())
            .field("os_key", &self.os_key.as_ref().map(|_| "<set>"))
            .field("os_root", &self.os_root)
            .field("assrt_root", &self.assrt_root)
            .field("cache_dir", &self.cache_dir)
            .field("session_file", &self.session_file)
            .finish()
    }
}

impl OnlineEnv {
    /// 離線（`Launch` 的預設：自動測試、`--shot` 絕不連網，也不碰使用者的資料夾）
    pub fn offline() -> OnlineEnv {
        OnlineEnv {
            http: Arc::new(http::OfflineHttp),
            os_key: None,
            os_root: opensubtitles::OFFICIAL_ROOT.into(),
            assrt_root: assrt::ROOT.into(),
            assrt_alt_root: None,
            cache_dir: None,
            session_file: None,
            throttle: Arc::new(Throttle::none()),
        }
    }

    /// 真的連網（main.rs）：`cache_root` = 快取的根資料夾（[`crate::paths::cache_root`]）
    pub fn real(cache_root: &Path) -> OnlineEnv {
        OnlineEnv {
            http: Arc::new(http::UreqHttp::new()),
            os_key: opensubtitles::api_key(),
            os_root: opensubtitles::OFFICIAL_ROOT.into(),
            assrt_root: assrt::ROOT.into(),
            assrt_alt_root: Some(assrt::ALT_ROOT.into()),
            cache_dir: Some(cache_root.join("subs").join("online")),
            session_file: Some(cache_root.join("online").join(SESSION_FILE)),
            throttle: Arc::new(Throttle::opensubtitles()),
        }
    }

    /// 測試用：用指定的連線（假的、或連到本機的測試伺服器），有一個假的金鑰，請求不間隔
    #[doc(hidden)]
    pub fn testing(http: Arc<dyn Http>, cache_dir: Option<PathBuf>) -> OnlineEnv {
        OnlineEnv {
            http,
            os_key: Some("test-key".into()),
            os_root: "https://os.test/api/v1".into(),
            assrt_root: "https://assrt.test/v1".into(),
            assrt_alt_root: None,
            cache_dir,
            session_file: None,
            throttle: Arc::new(Throttle::none()),
        }
    }

    /// 是不是離線的環境
    pub fn is_offline(&self) -> bool {
        self.http.is_offline()
    }
}

/// OpenSubtitles 登入狀態的檔名（快取資料夾的 `online` 裡）
pub const SESSION_FILE: &str = "opensubtitles-session.json";

/// 登入憑證多久有效（OpenSubtitles 的 JWT 是 24 小時）
pub const SESSION_LIFETIME: Duration = Duration::from_secs(24 * 3600);

/// OpenSubtitles 兩個請求之間至少隔多久（規定每個 IP 每秒最多 5 次，多留一點）
pub const REQUEST_GAP: Duration = Duration::from_millis(220);
/// OpenSubtitles 的 `/login` 之前至少隔多久（規定每秒最多 1 次，多留一點）
pub const LOGIN_GAP: Duration = Duration::from_millis(1100);

/// 請求的間隔（OpenSubtitles：每個 IP 每秒 5 次；`/login` 每秒 1 次）
#[derive(Debug)]
pub struct Throttle {
    gap: Duration,
    login_gap: Duration,
    last: Mutex<Option<Instant>>,
}

impl Throttle {
    /// 不間隔（測試）
    pub fn none() -> Throttle {
        Throttle::new(Duration::ZERO, Duration::ZERO)
    }

    /// OpenSubtitles 的規定，多留一點：一般 [`REQUEST_GAP`]、登入 [`LOGIN_GAP`]
    pub fn opensubtitles() -> Throttle {
        Throttle::new(REQUEST_GAP, LOGIN_GAP)
    }

    /// （一般的間隔, 登入的間隔）
    pub fn gaps(&self) -> (Duration, Duration) {
        (self.gap, self.login_gap)
    }

    pub fn new(gap: Duration, login_gap: Duration) -> Throttle {
        Throttle {
            gap,
            login_gap,
            last: Mutex::new(None),
        }
    }

    /// 等到可以送下一個請求（`login` = 這次是登入）
    pub fn wait(&self, login: bool) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let gap = if login { self.login_gap } else { self.gap };
        if let Some(t) = *last {
            let since = t.elapsed();
            if since < gap {
                std::thread::sleep(gap - since);
            }
        }
        *last = Some(Instant::now());
    }
}

/// OpenSubtitles 的登入狀態（存在快取資料夾；沒有密碼）
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// 登入憑證（JWT）
    pub token: String,
    /// 之後的請求送到哪台主機（`api.opensubtitles.com` 或 `vip-api.opensubtitles.com`）
    pub base_url: String,
    pub user: String,
    /// 過期的時間（Unix 秒）
    pub expires: u64,
}

/// 登入憑證不印出來
impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("token", &"<hidden>")
            .field("base_url", &self.base_url)
            .field("user", &self.user)
            .field("expires", &self.expires)
            .finish()
    }
}

#[derive(Default)]
struct AccountState {
    /// 已經讀過登入狀態的檔案
    restored: bool,
    session: Option<Session>,
    quota: Option<Quota>,
}

/// OpenSubtitles 的帳號：登入狀態（記憶體 + 檔案）與下載次數。介面與背景工作共用
pub struct Account {
    file: Option<PathBuf>,
    state: Mutex<AccountState>,
}

impl Account {
    /// `file` = 登入狀態的檔案（[`OnlineEnv::session_file`]）；None = 只放在記憶體
    pub fn new(file: Option<PathBuf>) -> Arc<Account> {
        Arc::new(Account {
            file,
            state: Mutex::new(AccountState::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, AccountState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 讀登入狀態的檔案（只讀一次；過期、壞掉的檔案刪掉）。每個工作開始時都會叫；讀使用者資料夾，在背景執行緒呼叫
    pub fn restore(&self) {
        let mut st = self.state();
        if st.restored {
            return;
        }
        st.restored = true;
        if let Some(file) = &self.file {
            st.session = read_session(file, now_unix());
        }
    }

    /// 登入的帳號名稱（還沒讀檔、沒登入、已過期都是 None）
    pub fn user(&self) -> Option<String> {
        self.session().map(|s| s.user)
    }

    /// 伺服器上次說的下載次數（還沒下載過是 None：不顯示數字，R-C14）
    pub fn quota(&self) -> Option<Quota> {
        self.state().quota
    }

    /// 目前有效的登入狀態（過期的在這裡丟掉）
    pub(crate) fn session(&self) -> Option<Session> {
        let mut st = self.state();
        if st.session.as_ref().is_some_and(|s| s.expires <= now_unix()) {
            st.session = None;
            self.remove_file();
            return None;
        }
        st.session.clone()
    }

    pub(crate) fn set_quota(&self, quota: Quota) {
        self.state().quota = Some(quota);
    }

    // 檔案都在拿著鎖的時候寫、刪：登入、登出、過期同時發生時，檔案與記憶體才不會不一致

    /// 登入成功：記在記憶體、寫進檔案（寫不了也照樣登入，只是下次要重新登入）
    pub(crate) fn sign_in(&self, session: Session) {
        let mut st = self.state();
        if let Some(file) = &self.file
            && let Err(e) = write_session(file, &session)
        {
            eprintln!("[vitascope] 無法儲存 OpenSubtitles 的登入狀態：{e}");
        }
        st.restored = true;
        st.session = Some(session);
        st.quota = None;
    }

    /// 伺服器說登入憑證 `token` 過期了：目前的登入還是它時才登出（記憶體與檔案）。
    /// 用舊憑證、還在等網路的請求晚一點才失敗時，不會把使用者剛剛重新登入的狀態清掉
    pub(crate) fn expire(&self, token: &str) {
        let mut st = self.state();
        if st.session.as_ref().is_none_or(|s| s.token != token) {
            return;
        }
        st.session = None;
        st.quota = None;
        self.remove_file();
    }

    /// 登出：記憶體與檔案都清掉
    pub(crate) fn sign_out(&self) {
        let mut st = self.state();
        st.restored = true;
        st.session = None;
        st.quota = None;
        self.remove_file();
    }

    fn remove_file(&self) {
        if let Some(file) = &self.file {
            let _ = std::fs::remove_file(file);
        }
    }
}

/// 讀登入狀態；過期、看不懂的檔案刪掉
fn read_session(file: &Path, now: u64) -> Option<Session> {
    let text = std::fs::read_to_string(file).ok()?;
    let session = serde_json::from_str::<Session>(&text)
        .ok()
        .filter(|s| !s.token.is_empty() && s.expires > now && s.expires <= now + SESSION_LIFETIME.as_secs());
    if session.is_none() {
        let _ = std::fs::remove_file(file);
    }
    session
}

/// 寫登入狀態：先寫暫存檔再改名；Unix 上只有自己能讀（裡面是登入憑證）
fn write_session(file: &Path, session: &Session) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut out = options.open(&tmp)?;
    out.write_all(
        serde_json::to_string(session)
            .map_err(std::io::Error::other)?
            .as_bytes(),
    )?;
    out.sync_all()?;
    drop(out);
    std::fs::rename(&tmp, file)
}

/// 這次下載過的（G6 用：同一個檔案不再花一次下載次數；R-B11）
#[derive(Debug, Default)]
pub struct Downloads {
    /// (影片, OpenSubtitles 的 file_id) → 存在哪裡
    saved: HashMap<(String, u64), PathBuf>,
    /// file_id → (下載連結, 拿到的時間)
    links: HashMap<u64, (String, Instant)>,
}

/// OpenSubtitles 的下載連結多久內可以再用（官方說 3 小時，留一點餘裕）
pub const LINK_LIFETIME: Duration = Duration::from_secs(3 * 3600 - 10 * 60);

impl Downloads {
    /// 下載前：填進之前拿到的連結、之前存的位置
    pub fn prepare(&self, job: &mut DownloadJob) {
        if let DownloadItem::Os { file_id, .. } = &job.item {
            job.known_link = self
                .links
                .get(file_id)
                .filter(|(_, at)| at.elapsed() < LINK_LIFETIME)
                .map(|(l, _)| l.clone());
            job.saved_before = self.saved.get(&(job.video.path.clone(), *file_id)).cloned();
        }
    }

    /// 下載的結果：記下連結、存的位置。
    ///
    /// 410（連結失效）時**不馬上忘掉連結**（R-B11）：下一次先用同一個連結再抓一次（不花下載次數），
    /// 還是失效才 `POST /download` 要新的。工作回傳的 `link` 是 None、又失敗了，表示之前的連結重試過也失效、
    /// 而且沒有拿到新的：這時才忘掉
    pub fn record(&mut self, result: &JobResult) {
        let JobResult::Download {
            video,
            item: DownloadItem::Os { file_id, .. },
            link,
            result,
            ..
        } = result
        else {
            return;
        };
        match (result, link) {
            (_, Some(l)) => {
                let keep = self.links.get(file_id).is_some_and(|(old, _)| old == l);
                if !keep {
                    self.links.insert(*file_id, (l.clone(), Instant::now()));
                }
            }
            (Err(_), None) => {
                self.links.remove(file_id);
            }
            (Ok(_), None) => {}
        }
        if let Ok(d) = result {
            self.saved.insert((video.path.clone(), *file_id), d.path.clone());
        }
    }
}

// ───────────────────────────── 搜尋文字 ─────────────────────────────

/// 一定不是片名的字（畫質、來源、編碼…）：檔名在第一個這種字的地方截斷
const TECH_WORDS: &[&str] = &[
    "4k", "8k", "uhd", "fhd", "hdr", "hdr10", "dovi", "sdr", "10bit", "8bit", "bluray", "bdrip", "brrip", "bdremux",
    "remux", "webdl", "webrip", "hdtv", "dvdrip", "tvrip", "hdrip", "x264", "x265", "h264", "h265", "hevc", "avc",
    "av1", "vp9", "xvid", "divx", "aac", "aac2", "ac3", "eac3", "ddp", "ddp5", "dd5", "dts", "truehd", "atmos", "flac",
    "chs", "cht", "big5", "jpsc", "jptc", "amzn", "dsnp", "hmax",
];

/// 也可能是片名的字（`The Web`、`Multi`…）：接在年份、集數、季集後面時才截斷
const WEAK_TECH_WORDS: &[&str] = &[
    "web", "dl", "hd", "sd", "bd", "dvd", "dv", "blu", "ray", "proper", "repack", "internal", "multi", "subbed",
    "dubbed", "nf", "cr", "baha", "tc", "sc", "gb", "mp3", "opus", "dd",
];

fn is_tech_word(w: &str) -> bool {
    if TECH_WORDS.contains(&w) {
        return true;
    }
    // 480p、720p、1080p、2160p、1080i
    let b = w.as_bytes();
    (4..=5).contains(&b.len())
        && matches!(b[b.len() - 1], b'p' | b'i')
        && b[..b.len() - 1].iter().all(u8::is_ascii_digit)
}

/// 年份、集數（`01`）、季集（`s02e05`、`e05`）：片名到這裡為止的記號
fn is_marker(w: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if digits(w) {
        return true;
    }
    // s02e05、s02、e05
    match w
        .strip_prefix('s')
        .map(|r| r.trim_start_matches(|c: char| c.is_ascii_digit()))
    {
        Some(rest) if rest.len() + 1 < w.len() => rest.is_empty() || rest.strip_prefix('e').is_some_and(digits),
        _ => w.strip_prefix('e').is_some_and(digits),
    }
}

/// 檔名整理成搜尋文字（小寫）：去掉影音的副檔名、開頭的字幕組 `[Group]`、畫質之後的部分、分隔符號。
/// 例：`Movie.Name.2019.1080p.BluRay.x264-GROUP` → `movie name 2019`；
/// `[Nekomoe kissaten][Sousou no Frieren][01][1080p][CHS]` → `sousou no frieren 01`
pub fn clean_query(name: &str) -> String {
    let mut name = name.trim();
    // 影音檔的副檔名
    if let Some((stem, ext)) = name.rsplit_once('.')
        && crate::formats::all_media().contains(&ext.to_ascii_lowercase().as_str())
    {
        name = stem;
    }
    // 開頭的 [字幕組]（後面還有東西時才拿掉）
    if name.starts_with('[')
        && let Some(end) = name.find(']')
        && !name[end + 1..].trim().is_empty()
    {
        name = &name[end + 1..];
    }
    let lower = name.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || "._-[](){}【】「」+,;~".contains(c))
        .filter(|w| !w.is_empty())
        .collect();
    let cut = (1..words.len())
        .find(|&i| {
            is_tech_word(words[i])
                || (WEAK_TECH_WORDS.contains(&words[i]) && (is_marker(words[i - 1]) || is_tech_word(words[i - 1])))
        })
        .unwrap_or(words.len());
    words[..cut].join(" ")
}

/// 網址的查詢值編碼：英數與 `-._~` 照原樣，其他的 `%XX`；`space_plus` = 空白寫成 `+`（OpenSubtitles 的規定）
pub(crate) fn encode_query(s: &str, space_plus: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            b' ' if space_plus => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ───────────────────────────── 內容、存檔 ─────────────────────────────

/// 下載的檔案最大多大（字幕不會這麼大）
pub const MAX_SUBTITLE_SIZE: u64 = 8 * 1024 * 1024;

/// 下載的字幕可以存成哪些副檔名：文字字幕與 PGS
fn allowed_ext(ext: &str) -> bool {
    crate::subs::TEXT_EXTS.contains(&ext) || ext == "sup"
}

/// 檔名的副檔名（小寫）
fn file_ext(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty())
}

/// 檢查下載的內容像不像字幕：網頁、壓縮檔、VobSub（MPEG-PS）、太大、空的都不收；`.sup` 要是 PGS（`PG` 開頭）
pub fn sniff(bytes: &[u8], ext: &str) -> Result<(), NetError> {
    if bytes.len() as u64 > MAX_SUBTITLE_SIZE {
        return Err(NetError::TooLarge);
    }
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"PK\x03\x04") || starts(b"Rar!") || starts(b"7z\xBC\xAF\x27\x1C") {
        return Err(NetError::Archive { page: None });
    }
    // VobSub 的 .sub（MPEG-PS）、gzip 壓縮過的檔案
    if starts(b"\x00\x00\x01\xBA") || starts(b"\x1F\x8B") {
        return Err(NetError::NotSubtitle);
    }
    if ext == "sup" {
        return if starts(b"PG") {
            Ok(())
        } else {
            Err(NetError::NotSubtitle)
        };
    }
    let head: String = crate::subs::decode(&bytes[..bytes.len().min(512)])
        .0
        .trim_start_matches('\u{feff}')
        .trim_start()
        .chars()
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    if head.is_empty() || head.starts_with("<!doctype") || head.starts_with("<html") {
        return Err(NetError::NotSubtitle);
    }
    // `.txt` 也可能是壓縮檔裡的說明、版權宣告：要看得到時間碼才收（不然下次開啟會被當成字幕自動載入）
    if ext == "txt" && !looks_like_timed_text(&crate::subs::decode(bytes).0) {
        return Err(NetError::NotSubtitle);
    }
    Ok(())
}

/// 文字裡看得到字幕的時間碼：SRT 的 `-->`、MicroDVD 的 `{1}{25}`、MPL2 的 `[1][25]`、TMPlayer 的 `0:00:01:`
fn looks_like_timed_text(text: &str) -> bool {
    let frames = |line: &str, open: char, close: char| {
        let mut rest = line;
        for _ in 0..2 {
            let Some(inner) = rest.strip_prefix(open) else {
                return false;
            };
            let Some((n, after)) = inner.split_once(close) else {
                return false;
            };
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            rest = after;
        }
        true
    };
    let clock = |line: &str| {
        let mut parts = line.splitn(4, [':', '=']);
        let mut digits = |max: usize| {
            parts
                .next()
                .is_some_and(|p| (1..=max).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()))
        };
        digits(2) && digits(2) && digits(2) && parts.next().is_some()
    };
    text.lines()
        .map(|l| l.trim_start_matches('\u{feff}').trim())
        .filter(|l| !l.is_empty())
        .take(200)
        .any(|l| l.contains("-->") || frames(l, '{', '}') || frames(l, '[', ']') || clock(l))
}

/// OpenSubtitles 的語言碼 → 檔名的語言標記（`zh-tw` → `zh-TW`，`ze`（中英雙語）→ `zh`）
pub fn os_tag(code: &str) -> String {
    let code = code.trim().to_ascii_lowercase();
    let tag = match code.as_str() {
        "ze" => "zh".to_owned(),
        _ => match code.split_once('-') {
            Some((main, region)) => format!("{main}-{}", region.to_ascii_uppercase()),
            None => code,
        },
    };
    sanitize_tag(&tag)
}

/// 檔名的語言標記只留 `[A-Za-z0-9&-]`，最多 16 個字（不讓網站送來的名稱變成路徑的一部分）
pub fn sanitize_tag(tag: &str) -> String {
    tag.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '&' || *c == '-')
        .take(16)
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// 依內容決定語言標記（R-B12）：看得出繁體 → `zh-TW`、簡體 → `zh-CN`；看不出來用網站標的（`fallback`）。
/// 網站標錯的檔案（標 zh-TW、內容是簡體）才不會存成錯的名稱。
///
/// 只修正繁簡：網站標的是中文（或沒標、看不懂）時才看內容。標日文、英文…的照網站的，內容是假名為主的也不改：
/// 繁簡的判斷字有很多是日文也常用的漢字（時、間、見、話、会、来…），日文字幕常常「看起來像繁體」
pub fn content_tag(bytes: &[u8], ext: &str, fallback: &str) -> String {
    let labelled_chinese = matches!(
        crate::subs::classify_label(fallback),
        None | Some(SubLang::Chinese | SubLang::Traditional | SubLang::Simplified)
    );
    if labelled_chinese && crate::subs::TEXT_EXTS.contains(&ext) {
        let text = crate::subs::decode(bytes).0;
        let kana = text.chars().filter(|c| ('\u{3040}'..='\u{30ff}').contains(c)).count();
        let han = text.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
        if !(kana >= 20 && kana * 2 >= han) {
            match crate::subs::classify_content(&text) {
                Some(SubLang::Traditional) => return "zh-TW".into(),
                Some(SubLang::Simplified) => return "zh-CN".into(),
                _ => {}
            }
        }
    }
    sanitize_tag(fallback)
}

/// 同一個名稱最多編到幾號（`影片.zh-TW.99.srt`）
const MAX_NUMBER: u32 = 99;

/// 存下載的字幕：設定要存到影片旁邊、影片是本機檔案時存成 `影片名.標記.副檔名`（有同名的檔案時 `.2`、`.3`…；
/// 內容一樣就直接用那個檔案）；寫不進去、或不存到影片旁邊時放 `cache_dir`。絕不覆蓋已經有的檔案。
/// 換成正式名稱之前看 `cancel`：取消時刪掉暫存檔
pub fn save_subtitle(
    bytes: &[u8],
    video: &VideoRef,
    tag: &str,
    ext: &str,
    next_to_video: bool,
    cache_dir: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<Downloaded, NetError> {
    let local = video.local_path();
    // 影片旁邊：用影片檔名原本的樣子（那個資料夾本來就能用這個名稱）。整理過的名稱（換掉 `:`、截短）
    // 跟影片名稱對不起來，下次開啟就不會自動載入（`subs::find_external` 比對檔名開頭）
    let own_stem = local
        .and_then(Path::file_stem)
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    // 快取資料夾：影片名稱或網址的最後一段，整理成各系統都能用的檔名
    let cache_stem = match local {
        Some(_) => own_stem.clone(),
        None => url::Url::parse(&video.path).ok().and_then(|u| {
            let last = u.path_segments()?.rev().find(|s| !s.is_empty())?.to_owned();
            Some(last.rsplit_once('.').map_or(last.clone(), |(s, _)| s.to_owned()))
        }),
    }
    .map(|s| crate::screenshot::sanitize_stem(&s))
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "subtitle".to_owned());
    let cancelled = || cancel.load(Ordering::SeqCst);
    let mut fell_back = false;
    if next_to_video
        && let Some(stem) = &own_stem
        && let Some(dir) = local.and_then(Path::parent).filter(|d| !d.as_os_str().is_empty())
    {
        match save_in(dir, stem, tag, ext, bytes, &cancelled) {
            Ok((path, reused)) => {
                return Ok(Downloaded {
                    path,
                    in_video_dir: true,
                    fell_back: false,
                    reused,
                    tag: tag.to_owned(),
                });
            }
            Err(SaveError::Cancelled) => return Err(NetError::Cancelled),
            // 唯讀的資料夾、權限、磁碟滿了、名稱太長：改放快取資料夾
            Err(SaveError::Io(e)) => {
                eprintln!("[vitascope] 字幕存不到影片旁邊，改放快取資料夾：{e}");
                fell_back = true;
            }
            Err(SaveError::NoFreeName) => {
                eprintln!("[vitascope] 影片旁邊同名的字幕太多，改放快取資料夾");
                fell_back = true;
            }
        }
    }
    // 背景執行緒不產生給使用者看的文字（介面語言是每個執行緒自己的）：只回傳錯誤的種類
    let Some(dir) = cache_dir else {
        return Err(NetError::NoSaveFolder);
    };
    let saved = std::fs::create_dir_all(dir)
        .map_err(SaveError::Io)
        .and_then(|()| save_in(dir, &cache_stem, tag, ext, bytes, &cancelled));
    match saved {
        Ok((path, reused)) => Ok(Downloaded {
            path,
            in_video_dir: false,
            fell_back,
            reused,
            tag: tag.to_owned(),
        }),
        Err(SaveError::Cancelled) => Err(NetError::Cancelled),
        Err(SaveError::NoFreeName) => Err(NetError::NoFreeName),
        Err(SaveError::Io(e)) => Err(NetError::Disk(e.to_string())),
    }
}

enum SaveError {
    Cancelled,
    /// `.2` 到 `.99` 都用掉了
    NoFreeName,
    Io(std::io::Error),
}

/// 第 `n` 個名稱：`影片.zh-TW.srt`、`影片.zh-TW.2.srt`…（沒有標記時 `影片.srt`、`影片.2.srt`）
fn numbered_name(stem: &str, tag: &str, n: u32, ext: &str) -> String {
    let mut name = stem.to_owned();
    if !tag.is_empty() {
        name.push('.');
        name.push_str(tag);
    }
    if n > 1 {
        name.push_str(&format!(".{n}"));
    }
    format!("{name}.{ext}")
}

/// 存到 `dir`：回傳（路徑, 是不是直接用已經有的檔案）。`cancelled` 在暫存檔寫好之後、換成正式名稱之前問
fn save_in(
    dir: &Path,
    stem: &str,
    tag: &str,
    ext: &str,
    bytes: &[u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<(PathBuf, bool), SaveError> {
    // 暫存檔的副檔名不是字幕的（`.part`）：寫到一半、當掉留下的都不會被自動載入
    let temp = crate::save::temp_in(dir, stem, "part");
    let finish = |result: Result<(PathBuf, bool), SaveError>| {
        let _ = std::fs::remove_file(&temp);
        result
    };
    if let Err(e) = std::fs::write(&temp, bytes) {
        return finish(Err(SaveError::Io(e)));
    }
    let mut n = 1;
    let mut races = 0;
    while n <= MAX_NUMBER {
        let target = dir.join(numbered_name(stem, tag, n, ext));
        match std::fs::symlink_metadata(&target) {
            Ok(meta) => {
                // 已經有這個名稱：內容一樣就直接用它
                if meta.is_file()
                    && meta.len() == bytes.len() as u64
                    && std::fs::read(&target).is_ok_and(|old| old == bytes)
                {
                    // 取消了就是取消了（介面已經不等這個結果），即使不用新增檔案
                    if cancelled() {
                        return finish(Err(SaveError::Cancelled));
                    }
                    return finish(Ok((target, true)));
                }
                n += 1;
            }
            Err(_) => {
                // 換成正式名稱之前最後看一次取消（R-B10）
                if cancelled() {
                    return finish(Err(SaveError::Cancelled));
                }
                match crate::save::finish_exact(&temp, &target) {
                    Ok(()) => return Ok((target, false)),
                    // 剛好被別人用掉：再看一次這個名稱（可能內容一樣）
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && races < 10 => races += 1,
                    Err(e) => return finish(Err(SaveError::Io(e))),
                }
            }
        }
    }
    finish(Err(SaveError::NoFreeName))
}

#[cfg(test)]
mod tests;
