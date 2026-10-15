//! 測試用的假 yt-dlp（`Resolve`）：不執行任何程式、不連網，照測試指定的方式回應。
//!
//! - 每次解析都記下要求（`requests()`、`calls()`）：測試確認有沒有問、問了幾次（快取）。
//! - 回應由測試給的函式決定：JSON（`json`）、錯誤（`fail`）、一直等到取消（`block`）…
//! - 看門狗可以改短（`with_watchdog`），測試不用等真正的 70 秒。
//!
//! JSON 裡的網址指向本機的測試伺服器（`site_video_json`），播放器真的去讀，測試確認伺服器收到什麼。
//!
//! 用假的 yt-dlp 的播放器（`site_player`）、網頁本身沒被讀（`page_never_fetched`）：tests/net.rs、tests/net_errors.rs 共用。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vitascope::net::{self, NetSettings};
use vitascope::player::{Options, Player};
use vitascope::ytdl::json;
use vitascope::ytdl::{Failure, Hint, Request, Resolve, Resolved, SitePrefs, YtdlError};

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

/// 像真的網站一樣每次解析都給新的、有簽名的網址：第 n 次解析（從 1 開始）回應 [`site_video_json`]，
/// 影像、聲音、字幕都在本機伺服器的 `/signed/<n>/f/…`（跟 `/f/…` 一樣的檔案）。
/// 測試讓某一次給的網址失效：[`expire_signed`]
pub fn signed_site(base: &str, page: &str, id: &str) -> FakeResolver {
    let (base, page, id) = (base.to_owned(), page.to_owned(), id.to_owned());
    let n = AtomicUsize::new(0);
    FakeResolver::new(move |_, _| {
        let i = n.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(resolved(
            &site_video_json(&format!("{base}/signed/{i}"), &page, &id),
            Vec::new(),
        ))
    })
}

/// 第 n 次解析給的網址（[`signed_site`]）失效了：伺服器之後都回應 403（像過期、被網站撤銷的網址）
pub fn expire_signed(server: &super::http::Server, n: usize) {
    for rel in ["net/video_only.mp4", "net/audio_only.m4a", "net/sub.vtt"] {
        server.put(&format!("/signed/{n}/f/{rel}"), super::http::Canned::status(403));
    }
}

/// 有網路 hook、用假的 yt-dlp 的播放器選項（跟介面一樣）。`sites` = 本機的測試伺服器（127.0.0.1）當成影片網站（先問 yt-dlp）；
/// 不當成的話是「其他網頁」：先照原樣開，認不出內容才問。`extra` 是額外的 mpv 選項
pub fn site_options(fake: &Arc<FakeResolver>, sites: bool, extra: &[(&str, &str)]) -> Options {
    let resolver: Arc<dyn Resolve> = fake.clone();
    Options {
        net_hooks: true,
        net_resolver: Some(resolver),
        net_sites: if sites { vec!["127.0.0.1".into()] } else { Vec::new() },
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    }
}

/// 用 `opts` 建立播放器，網路設定照 `s` 同步套用（跟介面啟動時一樣）
pub fn site_player_with(opts: Options, s: &NetSettings) -> Player {
    let mut p = Player::new(opts).expect("建立 mpv 失敗");
    let net_opts = net::mpv_options(s, &p.net_defaults());
    for (name, _, r) in p.apply_net(&net_opts, true) {
        r.unwrap_or_else(|e| panic!("無法設定 {name}：{e}"));
    }
    p.set_net_config(s, &SitePrefs::default());
    p
}

/// [`site_options`] 的 headless 播放器，網路設定照 `s` 同步套用
pub fn site_player(fake: &Arc<FakeResolver>, sites: bool, s: &NetSettings, extra: &[(&str, &str)]) -> Player {
    site_player_with(site_options(fake, sites, extra), s)
}

/// 網頁本身（`/watch`、`/playlist`）一次都沒被讀：網站影片只讀 yt-dlp 給的網址
pub fn page_never_fetched(server: &super::http::Server) {
    let pages: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.starts_with("/watch") || r.path.starts_with("/playlist"))
        .collect();
    assert!(pages.is_empty(), "播放器自己去讀了網頁：{pages:#?}");
}

/// 兩種畫質的網站影片（都是影音合在一起的 90 秒 `common/mp4_long.mp4`，網址不同：`?q=hi` 720p、`?q=lo` 360p），
/// 一個只有聲音的格式（`net/audio_only.m4a`），三個網站字幕（en、ja、zh-TW：開檔時照一般的規則會選繁中）。
/// yt-dlp 自己挑的是 720p
pub fn two_quality_json(base: &str) -> String {
    let muxed = |id: &str, file: &str, height: u32, tbr: u32| {
        format!(
            r#"{{"format_id": "{id}", "url": "{base}/f/common/{file}?q={id}", "protocol": "http",
                "vcodec": "avc1.64001f", "acodec": "mp4a.40.2", "height": {height}, "fps": 24, "tbr": {tbr}}}"#
        )
    };
    format!(
        r#"{{"id": "two", "title": "兩種畫質", "extractor_key": "FakeSite", "duration": 90,
            "format_id": "hi", "url": "{base}/f/common/mp4_long.mp4?q=hi", "protocol": "http",
            "vcodec": "avc1.64001f", "acodec": "mp4a.40.2", "height": 720, "fps": 24, "tbr": 2000,
            "formats": [{}, {},
              {{"format_id": "a", "url": "{base}/f/net/audio_only.m4a", "protocol": "http", "vcodec": "none",
                "acodec": "mp4a.40.2", "abr": 128}}],
            "requested_subtitles": {{
              "en": {{"ext": "vtt", "url": "{base}/f/net/sub.vtt?en", "name": "English"}},
              "ja": {{"ext": "vtt", "url": "{base}/f/net/sub.vtt?ja", "name": "Japanese"}},
              "zh-TW": {{"ext": "vtt", "url": "{base}/f/net/sub.vtt?zh", "name": "中文（台灣）"}}}}}}"#,
        muxed("hi", "mp4_long.mp4", 720, 2000),
        muxed("lo", "mp4_long.mp4", 360, 500)
    )
}
