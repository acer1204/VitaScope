//! 測試用的假 yt-dlp（`Resolve`）：不執行任何程式、不連網，照測試指定的方式回應。
//!
//! - 每次解析都記下要求（`requests()`、`calls()`）：測試確認有沒有問、問了幾次（快取）。
//! - 回應由測試給的函式決定：JSON（`json`）、錯誤（`fail`）、一直等到取消（`block`）…
//! - 看門狗可以改短（`with_watchdog`），測試不用等真正的 70 秒。
//!
//! JSON 裡的網址指向本機的測試伺服器（`site_video_json`），播放器真的去讀，測試確認伺服器收到什麼。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vitascope::ytdl::json;
use vitascope::ytdl::{Failure, Hint, Request, Resolve, Resolved, YtdlError};

type Handler = dyn Fn(&Request, &AtomicBool) -> Result<Resolved, Failure> + Send + Sync;

pub struct FakeResolver {
    handler: Box<Handler>,
    requests: Mutex<Vec<Request>>,
    /// 正在解析的數量（背景執行緒裡）
    running: AtomicUsize,
    /// 收到取消的次數
    cancelled: AtomicUsize,
    available: AtomicBool,
    watchdog: Option<Duration>,
}

impl FakeResolver {
    pub fn new(handler: impl Fn(&Request, &AtomicBool) -> Result<Resolved, Failure> + Send + Sync + 'static) -> Self {
        Self {
            handler: Box::new(handler),
            requests: Mutex::new(Vec::new()),
            running: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
            available: AtomicBool::new(true),
            watchdog: None,
        }
    }

    /// 每次都回應這份 JSON
    pub fn json(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::new(move |_, _| Ok(resolved(&text, Vec::new())))
    }

    /// 每次都失敗
    pub fn fail(failure: Failure) -> Self {
        Self::new(move |_, _| Err(failure.clone()))
    }

    /// 一直等，直到取消（回傳 Cancelled）或過了 `max`（之後照 `then` 回應）；`ignore_cancel` = 不理會取消
    pub fn block(
        max: Duration,
        ignore_cancel: bool,
        then: impl Fn() -> Result<Resolved, Failure> + Send + Sync + 'static,
    ) -> Self {
        Self::new(move |_, cancel| {
            let until = Instant::now() + max;
            while Instant::now() < until {
                if !ignore_cancel && cancel.load(Ordering::Relaxed) {
                    return Err(YtdlError::Cancelled.into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            then()
        })
    }

    /// 當成沒有 yt-dlp（`available()` 是 false）
    pub fn missing() -> Self {
        let f = Self::fail(YtdlError::Missing.into());
        f.available.store(false, Ordering::Relaxed);
        f
    }

    /// 看門狗改成這麼久（預設照 `Request::deadline` + 10 秒）
    pub fn with_watchdog(mut self, d: Duration) -> Self {
        self.watchdog = Some(d);
        self
    }

    pub fn arc(self) -> Arc<FakeResolver> {
        Arc::new(self)
    }

    /// 到目前為止的解析要求
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    pub fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    pub fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// 解析時看到取消的次數（`block` 回傳 Cancelled 的次數）
    pub fn cancelled(&self) -> usize {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Resolve for FakeResolver {
    fn resolve(&self, req: &Request, cancel: &AtomicBool) -> Result<Resolved, Failure> {
        self.requests.lock().unwrap().push(req.clone());
        self.running.fetch_add(1, Ordering::SeqCst);
        let result = (self.handler)(req, cancel);
        if matches!(&result, Err(f) if f.error == YtdlError::Cancelled) {
            self.cancelled.fetch_add(1, Ordering::SeqCst);
        }
        self.running.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }

    fn watchdog(&self, req: &Request) -> Duration {
        self.watchdog
            .unwrap_or_else(|| req.deadline() + vitascope::ytdl::hook::WATCHDOG_GRACE)
    }
}

/// JSON → 解析的結果（加上警告裡的提醒）
pub fn resolved(text: &str, hints: Vec<Hint>) -> Resolved {
    Resolved {
        info: json::parse(text.as_bytes()).expect("假的 yt-dlp 的 JSON 讀不懂"),
        hints,
    }
}

/// 標題（JSON 字串的內容）
pub const SITE_TITLE: &str = "假的網站影片 \\\"測試\\\"";
/// 上面的標題解開之後
pub const SITE_TITLE_TEXT: &str = "假的網站影片 \"測試\"";
/// 網站要的 User-Agent、Referer、Cookie
pub const SITE_UA: &str = "FakeSite/1.0 (test, like Gecko)";
pub const SITE_REFERER: &str = "http://ref.test/watch?v=1,2";
pub const SITE_COOKIE: &str = "session=abc123";

/// 一部網站影片：影像、聲音分開（本機伺服器的 `net/video_only.mp4`、`net/audio_only.m4a`），網站要的標頭與 Cookie、
/// 三個章節（第三個沒有標題）、一個 vtt 字幕（`net/sub.vtt`）。`base` = 伺服器的網址（`http://127.0.0.1:埠`），
/// `page` = 網頁的網址，`id` = 影片代號
pub fn site_video_json(base: &str, page: &str, id: &str) -> String {
    format!(
        r#"{{"id": "{id}", "title": "{SITE_TITLE}", "extractor_key": "FakeSite", "webpage_url": "{page}",
        "duration": 3, "uploader": "測試",
        "requested_formats": [
          {{"format_id": "v1", "url": "{base}/f/net/video_only.mp4", "protocol": "http", "vcodec": "avc1.64000d",
            "acodec": "none", "width": 320, "height": 240, "fps": 24, "tbr": 300,
            "http_headers": {{"User-Agent": "{SITE_UA}", "Referer": "{SITE_REFERER}", "Accept": "*/*"}},
            "cookies": "{SITE_COOKIE}; Path=/"}},
          {{"format_id": "a1", "url": "{base}/f/net/audio_only.m4a", "protocol": "http", "vcodec": "none",
            "acodec": "mp4a.40.2", "abr": 128,
            "http_headers": {{"User-Agent": "{SITE_UA}", "Referer": "{SITE_REFERER}"}}}}
        ],
        "formats": [
          {{"format_id": "v1", "url": "{base}/f/net/video_only.mp4", "protocol": "http", "vcodec": "avc1.64000d",
            "acodec": "none", "width": 320, "height": 240, "fps": 24, "tbr": 300}},
          {{"format_id": "a1", "url": "{base}/f/net/audio_only.m4a", "protocol": "http", "vcodec": "none",
            "acodec": "mp4a.40.2", "abr": 128}}
        ],
        "requested_subtitles": {{"en": {{"ext": "vtt", "url": "{base}/f/net/sub.vtt", "name": "English (site)"}}}},
        "chapters": [{{"start_time": 0, "title": "開頭"}}, {{"start_time": 1, "title": "中間"}},
                     {{"start_time": 2, "title": null}}]}}"#
    )
}

/// 網站的播放清單（`--flat-playlist` 的簡短項目）：`urls` 照順序，標題「第 n 部」
pub fn site_playlist_json(page: &str, urls: &[(String, &str)]) -> String {
    let entries: Vec<String> = urls
        .iter()
        .enumerate()
        .map(|(i, (url, id))| {
            format!(
                r#"{{"_type": "url", "url": "{url}", "id": "{id}", "title": "第 {} 部\n", "ie_key": "FakeSite"}}"#,
                i + 1
            )
        })
        .collect();
    format!(
        r#"{{"_type": "playlist", "id": "PL1", "title": "清單", "webpage_url": "{page}", "entries": [{}]}}"#,
        entries.join(",")
    )
}
