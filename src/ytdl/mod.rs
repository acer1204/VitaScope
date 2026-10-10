//! 網站影片：用 yt-dlp 取得 YouTube 之類網站的影片網址。
//!
//! - [`locate`]：找 yt-dlp 與 deno（在背景找、找到後記住；介面執行緒不等）。
//! - [`args`]：yt-dlp 的命令列（只取資料、不下載、不執行任何東西）。
//! - [`run`]：執行外部程式（不經過 shell、Windows 不閃黑色視窗、取消與逾時時連子程序一起結束）。
//! - [`json`]：yt-dlp 的 JSON；[`plan`]：JSON → mpv 要開的網址與選項（純邏輯）。
//! - [`errors`]：yt-dlp 的錯誤與警告 → 原因（介面執行緒再轉成文字）。
//! - [`hook`]：接上播放器的部分（哪些網址要問 yt-dlp、背景解析、結果的快取）。
//!
//! `Player` 在 mpv 開網址之前（`on_load` hook）呼叫 [`Resolve`]，把 [`plan::Plan`] 交給 mpv。

pub mod errors;
pub mod hook;
pub mod json;
pub mod locate;
pub mod plan;
pub mod run;

pub use errors::{Failure, Hint, Remedy, YtdlError};
pub use locate::{Located, Locator, SearchEnv, Source, Tools, Version};

use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// 網站影片的預設畫質
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SiteQuality {
    /// 最高
    #[default]
    Best,
    P2160,
    P1440,
    P1080,
    P720,
    P480,
    P360,
    /// 只播聲音
    AudioOnly,
}

impl SiteQuality {
    pub const ALL: [SiteQuality; 8] = [
        SiteQuality::Best,
        SiteQuality::P2160,
        SiteQuality::P1440,
        SiteQuality::P1080,
        SiteQuality::P720,
        SiteQuality::P480,
        SiteQuality::P360,
        SiteQuality::AudioOnly,
    ];

    /// 最高多少像素高（`Best`、`AudioOnly` 沒有上限）
    pub fn max_height(self) -> Option<u32> {
        match self {
            SiteQuality::P2160 => Some(2160),
            SiteQuality::P1440 => Some(1440),
            SiteQuality::P1080 => Some(1080),
            SiteQuality::P720 => Some(720),
            SiteQuality::P480 => Some(480),
            SiteQuality::P360 => Some(360),
            SiteQuality::Best | SiteQuality::AudioOnly => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            SiteQuality::Best => crate::tr!("最高", "Best").to_owned(),
            SiteQuality::AudioOnly => crate::tr!("只播聲音", "Audio only").to_owned(),
            q => {
                let h = q.max_height().unwrap_or_default();
                crate::tf!("最高 {h}p", "Up to {h}p")
            }
        }
    }
}

/// 優先的影像編碼
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodecPref {
    /// 照 yt-dlp 的預設（AV1、VP9、HEVC、H.264）
    #[default]
    Auto,
    /// H.264（舊電腦、沒有新編碼的硬體解碼時比較順）
    H264,
    Av1,
    Vp9,
}

impl CodecPref {
    pub const ALL: [CodecPref; 4] = [CodecPref::Auto, CodecPref::H264, CodecPref::Av1, CodecPref::Vp9];

    pub fn label(self) -> &'static str {
        match self {
            CodecPref::Auto => crate::tr!("自動", "Automatic"),
            CodecPref::H264 => "H.264",
            CodecPref::Av1 => "AV1",
            CodecPref::Vp9 => "VP9",
        }
    }

    /// yt-dlp `-S` 的編碼部分（`Auto` 沒有）
    fn sort(self) -> Option<&'static str> {
        match self {
            CodecPref::Auto => None,
            CodecPref::H264 => Some("vcodec:h264,acodec:m4a"),
            CodecPref::Av1 => Some("vcodec:av01"),
            CodecPref::Vp9 => Some("vcodec:vp9"),
        }
    }
}

/// 從哪個瀏覽器讀 Cookie（`--cookies-from-browser`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Browser {
    Firefox,
    Chrome,
    Chromium,
    Edge,
    Brave,
    Opera,
    Vivaldi,
    Safari,
    Whale,
}

impl Browser {
    pub const ALL: [Browser; 9] = [
        Browser::Firefox,
        Browser::Chrome,
        Browser::Chromium,
        Browser::Edge,
        Browser::Brave,
        Browser::Opera,
        Browser::Vivaldi,
        Browser::Safari,
        Browser::Whale,
    ];

    /// 顯示的名稱（產品名稱，不翻譯）
    pub fn label(self) -> &'static str {
        match self {
            Browser::Firefox => "Firefox",
            Browser::Chrome => "Chrome",
            Browser::Chromium => "Chromium",
            Browser::Edge => "Edge",
            Browser::Brave => "Brave",
            Browser::Opera => "Opera",
            Browser::Vivaldi => "Vivaldi",
            Browser::Safari => "Safari",
            Browser::Whale => "Whale",
        }
    }

    /// yt-dlp 的寫法
    pub fn arg(self) -> &'static str {
        match self {
            Browser::Firefox => "firefox",
            Browser::Chrome => "chrome",
            Browser::Chromium => "chromium",
            Browser::Edge => "edge",
            Browser::Brave => "brave",
            Browser::Opera => "opera",
            Browser::Vivaldi => "vivaldi",
            Browser::Safari => "safari",
            Browser::Whale => "whale",
        }
    }
}

/// 網址同時是影片和播放清單（`watch?v=…&list=…`）時播哪個
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListMode {
    /// 只播這部影片
    #[default]
    Video,
    /// 整個播放清單
    Playlist,
}

/// 網站影片的偏好（之後放進「設定 → 網路」；現在先用預設值）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePrefs {
    pub quality: SiteQuality,
    pub codec: CodecPref,
    /// 載入網站提供的字幕
    pub site_subs: bool,
    /// 也載入網站自動產生的字幕
    pub auto_subs: bool,
    pub cookies_from: Option<Browser>,
    pub list_mode: ListMode,
}

impl Default for SitePrefs {
    fn default() -> Self {
        Self {
            quality: SiteQuality::Best,
            codec: CodecPref::Auto,
            site_subs: true,
            auto_subs: false,
            cookies_from: None,
            list_mode: ListMode::Video,
        }
    }
}

// ───────────── 命令列 ─────────────

/// 要 yt-dlp 解析的一個網址
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// 網頁的網址：一定是 http / https，用 `url` 重新寫過一次（命令列上放在 `--` 後面）
    pub url: String,
    /// 網址也是播放清單時整個載入（`--yes-playlist`）
    pub playlist: bool,
    pub quality: SiteQuality,
    pub codec: CodecPref,
    pub subs: bool,
    pub auto_subs: bool,
    pub cookies_from: Option<Browser>,
    /// 網路設定的 proxy（空白 = yt-dlp 自己看環境變數、系統的 proxy）
    pub proxy: String,
    /// 連線逾時（秒）
    pub timeout_secs: u32,
    pub tls_verify: bool,
}

/// 單一影片最多等 yt-dlp 多久
pub const VIDEO_DEADLINE: Duration = Duration::from_secs(60);
/// 播放清單最多等多久（最多讀 [`PLAYLIST_ITEMS`] 個項目）
pub const PLAYLIST_DEADLINE: Duration = Duration::from_secs(120);
/// 播放清單最多讀幾個項目（頻道可能有上萬部影片）
pub const PLAYLIST_ITEMS: &str = "1:200";
/// [`PLAYLIST_ITEMS`] 的上限：yt-dlp 給了這麼多個項目時，清單可能還有更多（被截掉了）
pub const PLAYLIST_MAX: usize = 200;
/// 交給 yt-dlp 的連線逾時上限（秒）
const MAX_SOCKET_TIMEOUT: u32 = 30;
/// 要的字幕語言（中、英、日；不要直播聊天室）
const SUB_LANGS: &str = "zh.*,en.*,ja.*,-live_chat";
/// 字幕格式（播放引擎看得懂的優先）
const SUB_FORMAT: &str = "vtt/srt/ass/best";

/// http / https 網址，用 `url` 重新寫過（去掉前後空白、補上主機名稱的小寫…）；其他協定、看不懂的 None
pub fn site_url(s: &str) -> Option<String> {
    let u = url::Url::parse(s.trim()).ok()?;
    (matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())).then(|| u.to_string())
}

impl Request {
    /// 網址不是 http / https 時 None（yt-dlp 只處理網頁）
    pub fn new(url: &str, net: &crate::net::NetSettings, prefs: &SitePrefs) -> Option<Request> {
        let net = net.clone().sanitized();
        Some(Request {
            url: site_url(url)?,
            playlist: prefs.list_mode == ListMode::Playlist,
            quality: prefs.quality,
            codec: prefs.codec,
            subs: prefs.site_subs,
            auto_subs: prefs.auto_subs,
            cookies_from: prefs.cookies_from,
            proxy: net.proxy,
            timeout_secs: net.timeout_secs,
            tls_verify: net.tls_verify,
        })
    }

    /// 最多等 yt-dlp 多久
    pub fn deadline(&self) -> Duration {
        if self.playlist {
            PLAYLIST_DEADLINE
        } else {
            VIDEO_DEADLINE
        }
    }
}

/// 畫質、編碼 → yt-dlp 的 `-f` 與 `-S`。
/// 有編碼偏好時編碼放在 `-S` 的最前面：`res` 放前面的話會先挑最高解析度，「H.264、最高 1080p」反而拿到 VP9 / AV1
pub fn format_args(quality: SiteQuality, codec: CodecPref) -> (&'static str, Option<String>) {
    if quality == SiteQuality::AudioOnly {
        return ("ba/b", None);
    }
    let res = quality.max_height().map(|h| format!("res:{h}"));
    let sort: Vec<String> = codec.sort().map(str::to_owned).into_iter().chain(res).collect();
    ("bv*+ba/b", (!sort.is_empty()).then(|| sort.join(",")))
}

/// yt-dlp 的命令列（程式名稱之後的參數）。
/// - 只取資料：`-J --simulate --no-exec` 放在使用者的 yt-dlp 設定檔之後（後面的優先），設定檔裡的 `--no-simulate`、
///   `--exec` 不會讓 yt-dlp 下載或執行任何東西。
/// - 不加 `--no-warnings`：沒有 deno、yt-dlp 太舊時，yt-dlp 照樣成功、只剩部分畫質，原因只寫在警告裡（[`errors::hints`]）。
/// - 不加 `--js-runtimes`：子程序的 PATH 先放找到的 deno（[`Tools::child_env`]），任何版本的 yt-dlp 都找得到。
/// - 網址一律放在 `--` 後面，不會被當成選項
pub fn args(req: &Request) -> Vec<String> {
    let mut a: Vec<String> = [
        "--encoding",
        "utf-8",
        "--no-progress",
        "-J",
        "--simulate",
        "--no-exec",
        "--flat-playlist",
        if req.playlist {
            "--yes-playlist"
        } else {
            "--no-playlist"
        },
        "-I",
        PLAYLIST_ITEMS,
        "--socket-timeout",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    a.push(req.timeout_secs.clamp(1, MAX_SOCKET_TIMEOUT).to_string());
    let (format, sort) = format_args(req.quality, req.codec);
    a.extend(["-f".to_owned(), format.to_owned()]);
    if let Some(sort) = sort {
        a.extend(["-S".to_owned(), sort]);
    }
    if req.subs {
        a.push("--write-subs".into());
        if req.auto_subs {
            a.push("--write-auto-subs".into());
        }
        a.extend(["--sub-langs", SUB_LANGS, "--sub-format", SUB_FORMAT].map(str::to_owned));
    }
    if let Some(b) = req.cookies_from {
        a.extend(["--cookies-from-browser".to_owned(), b.arg().to_owned()]);
    }
    if !req.proxy.is_empty() {
        a.extend(["--proxy".to_owned(), req.proxy.clone()]);
    }
    if !req.tls_verify {
        a.push("--no-check-certificates".into());
    }
    a.extend(["--".to_owned(), req.url.clone()]);
    a
}

// ───────────── 解析 ─────────────

/// yt-dlp 解析的結果：資料，加上警告裡看得出的提醒
#[derive(Debug, Clone)]
pub struct Resolved {
    pub info: json::Info,
    pub hints: Vec<Hint>,
}

/// 解析網站影片的方法（測試換成假的，不執行真的 yt-dlp、不連網）
pub trait Resolve: Send + Sync {
    /// 在背景執行緒呼叫，會等很久（最多 [`Request::deadline`]，包括第一次找 yt-dlp 的時間）；
    /// `cancel` 變成 true 時盡快放棄（回傳 [`YtdlError::Cancelled`]）。失敗時也帶著警告裡的提醒（[`Failure`]）
    fn resolve(&self, req: &Request, cancel: &AtomicBool) -> Result<Resolved, Failure>;
    /// 能不能用：剛找過而且確定沒有 yt-dlp 時 false（還沒找完、要重新找時當成可以，真的沒有時 `resolve` 會回傳
    /// [`YtdlError::Missing`]）
    fn available(&self) -> bool;
    /// 播放器最多等多久就當成沒有回應、放行 hook（看門狗）：`resolve` 自己的時間限制再多 [`hook::WATCHDOG_GRACE`]。
    /// `resolve` 沒有照時間結束時（卡住、不理會取消），載入也不會永遠停住
    fn watchdog(&self, req: &Request) -> Duration {
        req.deadline() + hook::WATCHDOG_GRACE
    }
}

/// 上次沒找到 yt-dlp（或 deno）時，過了這麼久再開網站影片就重新找：使用者可能照起始畫面的說明裝好了，
/// 不用重開影戲。連續開好幾個網址時不每次都找
pub const RECHECK_MISSING: Duration = Duration::from_secs(5);

/// 執行真的 yt-dlp（用 [`Locator`] 找到的那一個）
pub struct ProcessResolver {
    locator: Locator,
    limits: run::Limits,
    /// 測試用：取代 [`Request::deadline`]
    deadline: Option<Duration>,
    /// 沒找到的結果過了多久要重新找（[`RECHECK_MISSING`]）
    recheck: Duration,
}

impl ProcessResolver {
    pub fn new(locator: Locator) -> Self {
        Self {
            locator,
            limits: run::Limits::default(),
            deadline: None,
            recheck: RECHECK_MISSING,
        }
    }

    /// 測試用：沒找到的結果過了多久要重新找
    #[doc(hidden)]
    pub fn with_recheck(mut self, recheck: Duration) -> Self {
        self.recheck = recheck;
        self
    }

    /// 測試用：換掉時間限制（等待時間仍然是 [`Request::deadline`]，見 [`Self::with_deadline`]）
    #[doc(hidden)]
    pub fn with_limits(mut self, limits: run::Limits) -> Self {
        self.limits = limits;
        self
    }

    /// 測試用：換掉等待時間
    #[doc(hidden)]
    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

impl Resolve for ProcessResolver {
    fn resolve(&self, req: &Request, cancel: &AtomicBool) -> Result<Resolved, Failure> {
        // 背景執行緒：可以等背景的尋找做完（第一次播網站影片時），等的時候也看取消。
        // 找的時間也算在等待時間裡：hook 的看門狗只多給 10 秒，不能先找 90 秒再給 yt-dlp 完整的 60 秒
        let started = std::time::Instant::now();
        let deadline = self.deadline.unwrap_or_else(|| req.deadline());
        let tools = loop {
            if let Some(t) = self.locator.wait(Duration::from_millis(100)) {
                break t;
            }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(YtdlError::Cancelled.into());
            }
            if started.elapsed() >= deadline.min(locate::SEARCH_WAIT) {
                return Err(YtdlError::NoResponse.into());
            }
        };
        let ytdl = tools.ytdl.as_ref().ok_or(YtdlError::Missing)?;
        let left = deadline.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(YtdlError::NoResponse.into());
        }
        let limits = run::Limits {
            deadline: left,
            ..self.limits.clone()
        };
        resolve_with(ytdl, &tools.child_env(), req, &limits, cancel)
    }

    fn available(&self) -> bool {
        // 還沒找完（第一次、正在重新找）：`resolve` 等找完的結果
        let Some(tools) = self.locator.get() else {
            return true;
        };
        if self.locator.searching() {
            return true;
        }
        // 上次沒找到 yt-dlp 或 deno、而且找過一陣子了：重新找（只看檔案在不在，版本查過的檔案不再執行，很快）。
        // 找完之前當成可以，`resolve` 等新的結果，還是沒有就回報缺 yt-dlp
        let missing = tools.ytdl.is_none() || tools.deno.is_none();
        if missing && self.locator.age().is_some_and(|a| a >= self.recheck) {
            self.locator.refresh();
            return true;
        }
        tools.ytdl.is_some()
    }

    fn watchdog(&self, req: &Request) -> Duration {
        self.deadline.unwrap_or_else(|| req.deadline()) + hook::WATCHDOG_GRACE
    }
}

/// 用指定的 yt-dlp 解析一個網址（`limits.deadline` 是等多久）。失敗時也留著警告裡的提醒
pub fn resolve_with(
    ytdl: &Located,
    env: &run::ChildEnv,
    req: &Request,
    limits: &run::Limits,
    cancel: &AtomicBool,
) -> Result<Resolved, Failure> {
    let args: Vec<OsString> = args(req).into_iter().map(OsString::from).collect();
    let out = match run::run(ytdl, &args, env, limits, run::Lock::Shared, cancel) {
        Ok(out) => out,
        Err(run::RunError::Spawn(e)) => return Err(errors::spawn_error(&e).into()),
        Err(run::RunError::Cancelled) => return Err(YtdlError::Cancelled.into()),
        Err(run::RunError::TimedOut) => return Err(YtdlError::NoResponse.into()),
    };
    let hints = errors::hints(&out.stderr);
    let fail = |error: YtdlError| Failure {
        error,
        hints: hints.clone(),
    };
    if !out.success {
        return Err(fail(errors::describe(&out.stderr, out.code, req.cookies_from)));
    }
    if out.stdout_truncated {
        return Err(fail(YtdlError::NotJson));
    }
    let info = json::parse(&out.stdout).map_err(fail)?;
    Ok(Resolved { info, hints })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(url: &str) -> Request {
        Request::new(url, &crate::net::NetSettings::default(), &SitePrefs::default()).unwrap()
    }

    #[test]
    fn format_table() {
        use CodecPref as C;
        use SiteQuality as Q;
        assert_eq!(format_args(Q::Best, C::Auto), ("bv*+ba/b", None));
        assert_eq!(format_args(Q::Best, C::Av1), ("bv*+ba/b", Some("vcodec:av01".into())));
        assert_eq!(format_args(Q::P1080, C::Auto), ("bv*+ba/b", Some("res:1080".into())));
        // 編碼在前、解析度在後（不然 res 先挑，會拿到 1440p 的 VP9）
        assert_eq!(
            format_args(Q::P1080, C::H264),
            ("bv*+ba/b", Some("vcodec:h264,acodec:m4a,res:1080".into()))
        );
        assert_eq!(
            format_args(Q::P360, C::Vp9),
            ("bv*+ba/b", Some("vcodec:vp9,res:360".into()))
        );
        assert_eq!(format_args(Q::AudioOnly, C::H264), ("ba/b", None));
    }

    #[test]
    fn playlist_limit_matches_the_command_line() {
        assert_eq!(PLAYLIST_ITEMS, format!("1:{PLAYLIST_MAX}"));
    }

    #[test]
    fn command_line_only_simulates_and_ends_with_the_url() {
        let a = args(&req("https://www.youtube.com/watch?v=BaW_jenozKc"));
        for must in ["-J", "--simulate", "--no-exec", "--flat-playlist", "--no-playlist"] {
            assert!(a.iter().any(|x| x == must), "缺 {must}：{a:?}");
        }
        // 不隱藏警告、不指定 JavaScript 執行環境
        assert!(!a.iter().any(|x| x == "--no-warnings" || x.starts_with("--js-runtimes")));
        let n = a.len();
        assert_eq!(a[n - 2], "--");
        assert_eq!(a[n - 1], "https://www.youtube.com/watch?v=BaW_jenozKc");
        assert_ne!(a[0], a[n - 1]);
        // 預設：網站字幕、不要自動字幕、沒有 Cookie、檢查憑證
        assert!(a.iter().any(|x| x == "--write-subs"));
        assert!(
            !a.iter()
                .any(|x| x == "--write-auto-subs" || x == "--cookies-from-browser")
        );
        assert!(!a.iter().any(|x| x == "--no-check-certificates" || x == "--proxy"));
        let pos = a.iter().position(|x| x == "--socket-timeout").unwrap();
        assert_eq!(a[pos + 1], "30");
    }

    #[test]
    fn options_reach_the_command_line() {
        let net = crate::net::NetSettings {
            proxy: "socks5://127.0.0.1:1080".into(),
            timeout_secs: 10,
            tls_verify: false,
            ..Default::default()
        };
        let prefs = SitePrefs {
            quality: SiteQuality::P720,
            codec: CodecPref::H264,
            auto_subs: true,
            cookies_from: Some(Browser::Firefox),
            list_mode: ListMode::Playlist,
            ..Default::default()
        };
        let r = Request::new("https://youtu.be/x?list=PL1", &net, &prefs).unwrap();
        assert_eq!(r.deadline(), PLAYLIST_DEADLINE);
        let a = args(&r);
        let after = |flag: &str| a[a.iter().position(|x| x == flag).unwrap() + 1].clone();
        assert_eq!(after("--proxy"), "socks5://127.0.0.1:1080");
        assert_eq!(after("--socket-timeout"), "10");
        assert_eq!(after("--cookies-from-browser"), "firefox");
        assert_eq!(after("-S"), "vcodec:h264,acodec:m4a,res:720");
        assert_eq!(after("-I"), "1:200");
        assert!(a.iter().any(|x| x == "--yes-playlist"));
        assert!(a.iter().any(|x| x == "--write-auto-subs"));
        assert!(a.iter().any(|x| x == "--no-check-certificates"));
        // 字幕關掉：沒有任何字幕選項
        let r = Request::new(
            "https://x.test/v",
            &net,
            &SitePrefs {
                site_subs: false,
                auto_subs: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!args(&r).iter().any(|x| x.contains("sub")));
    }

    #[test]
    fn only_web_addresses_are_resolved() {
        assert_eq!(
            site_url(" https://Example.COM/a b "),
            Some("https://example.com/a%20b".into())
        );
        assert_eq!(site_url("http://h/x?y=1#t=3").as_deref(), Some("http://h/x?y=1#t=3"));
        for bad in [
            "file:///etc/passwd",
            "ftp://h/x",
            "-o",
            "--exec=calc",
            "edl://x",
            "javascript:alert(1)",
            "https://",
        ] {
            assert_eq!(site_url(bad), None, "{bad}");
        }
        // 網址看起來像選項也一樣放在 -- 後面（url 重新寫過，開頭一定是協定）
        let a = args(&req("https://h/--exec"));
        assert_eq!(a.last().unwrap(), "https://h/--exec");
    }
}
