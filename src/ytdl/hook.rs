//! 網站影片接上播放器：mpv 開網址之前（`on_load` hook）先問 yt-dlp，把網頁換成真正的影片網址。
//!
//! 這裡是不碰 `Player` 狀態的部分（路線判斷、背景解析、結果的快取）；狀態機本身在 `Player` 裡
//! （它擁有 mpv、播放狀態與錯誤記錄），見 `player.rs` 的「網站影片」一節。
//!
//! - 只用兩個 hook：`on_load`（開檔前）與 `on_load_fail`（開不起來時）。章節在 FileLoaded 之後設定，不用第三個 hook
//!   （每個 hook 都要等介面處理一輪事件，本機檔案也一樣）。
//! - 路線（[`route`]）：
//!   - 不是 http / https：照原樣開。
//!   - 網址看得出是媒體檔（副檔名）：照原樣開，開不起來也不問 yt-dlp。
//!   - 常見的影片網站（YouTube、Bilibili…）：先問 yt-dlp。
//!   - 其他網頁：先照原樣開，mpv 讀到了內容卻認不出是什麼（是網頁）時才問 yt-dlp（[`fallback_wanted`]）；
//!     連不上、HTTP 404 之類照原本的說明，不多等 yt-dlp。
//! - 解析在背景執行緒做（[`Job`]），做完用 `mpv_wakeup` 叫醒處理事件的執行緒。背景執行緒只拿著 mpv 的 `Weak`，
//!   不會讓播放引擎在關閉時還活著；mpv 會自己放行已經不在的 client 的 hook。
//! - 等太久（[`super::Resolve::watchdog`]）就放棄，hook 一定會放行：忘了放行的話 mpv 永遠停在載入中。

use super::{Failure, Request, Resolve, Resolved, YtdlError};
use crate::mpv::Mpv;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// 記住幾個解析結果（換畫質、重開同一部影片時不用再執行 yt-dlp）
pub const CACHE_SIZE: usize = 8;
/// 解析結果多久之內能直接用（YouTube 的影片網址大約 6 小時後失效）
pub const CACHE_AGE: Duration = Duration::from_secs(20 * 60);
/// 看門狗：yt-dlp 自己的時間限制之後再等多久
pub const WATCHDOG_GRACE: Duration = Duration::from_secs(10);

/// 先問 yt-dlp 的網站（含子網域）。只是「直接問、不先試著自己開」的清單：不在清單上的網站，
/// mpv 開不起來（是網頁）時照樣會問 yt-dlp
pub const KNOWN_SITES: [&str; 25] = [
    "youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "twitch.tv",
    "bilibili.com",
    "b23.tv",
    "nicovideo.jp",
    "nico.ms",
    "vimeo.com",
    "dailymotion.com",
    "dai.ly",
    "x.com",
    "twitter.com",
    "facebook.com",
    "fb.watch",
    "instagram.com",
    "tiktok.com",
    "soundcloud.com",
    "bandcamp.com",
    "streamable.com",
    "reddit.com",
    "v.redd.it",
    "rumble.com",
    "kick.com",
    "twitcasting.tv",
];

/// 串流清單的副檔名（媒體檔、字幕、播放清單之外，網址看得出是串流的）
const STREAM_EXTENSIONS: [&str; 2] = ["mpd", "ism"];

/// 一個網址怎麼開
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// 照原樣交給 mpv（本機檔案、媒體檔的網址、mpv 自己的網址…）
    Native,
    /// 先問 yt-dlp（影片網站；換畫質之類只對這次有效的要求）
    Site,
    /// 先照原樣開，mpv 認不出內容時才問 yt-dlp
    Fallback,
}

/// 網址怎麼開。`forced` = 這次開檔有只對這次有效的要求（換畫質、載入整個播放清單），一定要問 yt-dlp；
/// `extra_sites` = 額外當成影片網站的主機（自動測試用本機的伺服器）
pub fn route(url: &str, forced: bool, extra_sites: &[String]) -> Route {
    let Some(u) = super::site_url(url).and_then(|s| url::Url::parse(&s).ok()) else {
        return Route::Native;
    };
    if forced {
        return Route::Site;
    }
    if media_path(u.path()) {
        return Route::Native;
    }
    let host = u
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    if known_site(&host, extra_sites) {
        Route::Site
    } else {
        Route::Fallback
    }
}

/// 網址的路徑看得出是媒體檔、字幕、播放清單或串流清單（最後一段的副檔名）
fn media_path(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or_default();
    let p = std::path::Path::new(last);
    let ext = p
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    crate::formats::media_kind(p).is_some()
        || crate::formats::is_subtitle(p)
        || crate::formats::is_playlist(p)
        || STREAM_EXTENSIONS.contains(&ext.as_str())
}

/// 主機是清單上的網站，或它的子網域（`www.youtube.com`、`m.bilibili.com`）
fn known_site(host: &str, extra: &[String]) -> bool {
    let matches = |site: &str| {
        let site = site.to_ascii_lowercase();
        host == site || host.strip_suffix(site.as_str()).is_some_and(|rest| rest.ends_with('.'))
    };
    KNOWN_SITES.iter().any(|s| matches(s)) || extra.iter().any(|s| matches(s))
}

/// mpv 自己開不起來（`on_load_fail`）時，這次開檔的錯誤記錄看得出「讀到了內容、只是認不出是什麼」（網頁）：
/// 這時才值得問 yt-dlp。連不上、找不到伺服器、HTTP 404、逾時、憑證錯誤之類照原本的說明
/// （問 yt-dlp 只是多等幾秒，而且會蓋掉真正的原因）。
///
/// 只能從「連線失敗的記錄」反過來判斷：連線失敗時 mpv 在 hook 之前一定先記一筆「Failed to open <網址>」
/// （不支援的協定、播放清單裡不安全的網址也各有一筆）；讀到內容但認不出格式時，這時候還沒有任何錯誤記錄
/// （「Failed to recognize file format」要等檔案結束、hook 之後才記）。
/// 記錄比 hook 晚送到：要等事件都處理完（記錄也收到了）才能判斷，不然連線失敗也會被當成網頁
pub fn fallback_wanted(log: &str) -> bool {
    !["Failed to open", "No protocol handler found", "Refusing to load"]
        .iter()
        .any(|s| log.contains(s))
}

/// 正在等 yt-dlp 解析網址
#[derive(Debug, Clone, PartialEq)]
pub struct NetBusy {
    /// 網頁的網址
    pub url: String,
    /// 從什麼時候開始等
    pub since: Instant,
    /// 解析的是整個播放清單（比較久）
    pub playlist: bool,
}

/// 背景解析的進度
pub enum Progress {
    /// 還在等
    Waiting,
    /// 解析完了（資料很大：放在 Box 裡）
    Done(Box<Result<Resolved, Failure>>),
    /// 等太久（看門狗）
    TimedOut,
}

/// 一次背景解析（等 yt-dlp 的時候 mpv 停在 hook）。丟掉（不管是做完、放棄或換檔）時一定會通知背景執行緒取消
pub struct Job {
    /// 要放行的 hook（mpv 給的序號）
    pub hook_id: u64,
    /// 是開不起來之後才問的（`on_load_fail`）
    pub after_failure: bool,
    pub request: Request,
    pub started: Instant,
    /// 最多等多久（之後當成沒有回應）
    pub watchdog: Duration,
    rx: Receiver<Result<Resolved, Failure>>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// 在背景執行緒開始解析。做完時用 `mpv` 叫醒處理事件的執行緒（`mpv_wakeup`；播放器已經關了就不叫）
    pub fn spawn(
        resolver: Arc<dyn Resolve>,
        mpv: Weak<Mpv>,
        hook_id: u64,
        after_failure: bool,
        request: Request,
    ) -> std::io::Result<Job> {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let watchdog = resolver.watchdog(&request);
        let (req, flag) = (request.clone(), cancel.clone());
        std::thread::Builder::new()
            .name("vitascope-ytdl-resolve".into())
            .spawn(move || {
                let result = resolver.resolve(&req, &flag);
                // 已經放棄了（換檔、取消）就沒有人收：送不出去也沒關係
                let _ = tx.send(result);
                if let Some(m) = mpv.upgrade() {
                    m.wakeup();
                }
            })?;
        Ok(Job {
            hook_id,
            after_failure,
            request,
            started: Instant::now(),
            watchdog,
            rx,
            cancel,
        })
    }

    /// 看看做完了沒（不等）
    pub fn progress(&self) -> Progress {
        match self.rx.try_recv() {
            Ok(result) => Progress::Done(Box::new(result)),
            // 背景執行緒沒送結果就結束了（不該發生）：當成沒有回應
            Err(TryRecvError::Disconnected) => Progress::Done(Box::new(Err(YtdlError::NoResponse.into()))),
            Err(TryRecvError::Empty) if self.started.elapsed() >= self.watchdog => Progress::TimedOut,
            Err(TryRecvError::Empty) => Progress::Waiting,
        }
    }

    /// 離看門狗還有多久（處理事件的執行緒最多等這麼久就要再看一次）
    pub fn time_left(&self) -> Duration {
        self.watchdog.saturating_sub(self.started.elapsed())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // 不再等結果：yt-dlp 盡快放棄（Windows 是放手讓它自己結束，見 `run`）
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 最近的解析結果（依要求，最多 [`CACHE_SIZE`] 個、[`CACHE_AGE`] 之內）
#[derive(Default)]
pub struct Cache {
    entries: Vec<(Request, Instant, Arc<Resolved>)>,
}

impl Cache {
    /// 同樣的要求（網址、播放清單、畫質、編碼、字幕、Cookie…都一樣）、還沒過期的結果
    pub fn get(&mut self, req: &Request) -> Option<Arc<Resolved>> {
        self.entries.retain(|(_, at, _)| at.elapsed() < CACHE_AGE);
        self.entries
            .iter()
            .find(|(r, _, _)| r == req)
            .map(|(_, _, v)| v.clone())
    }

    pub fn insert(&mut self, req: Request, resolved: Arc<Resolved>) {
        self.entries.retain(|(r, _, _)| *r != req);
        self.entries.push((req, Instant::now(), resolved));
        if self.entries.len() > CACHE_SIZE {
            self.entries.remove(0);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        let none: &[String] = &[];
        for (url, want) in [
            ("https://www.youtube.com/watch?v=BaW_jenozKc", Route::Site),
            ("https://youtu.be/BaW_jenozKc", Route::Site),
            ("https://m.bilibili.com/video/BV1xx", Route::Site),
            ("HTTPS://WWW.YOUTUBE.COM/watch?v=x", Route::Site),
            ("https://x.com/user/status/1", Route::Site),
            // 只是名稱裡有，不是子網域
            ("https://notyoutube.com/watch", Route::Fallback),
            ("https://youtube.com.evil.test/watch", Route::Fallback),
            ("https://example.com/page", Route::Fallback),
            ("https://example.com/", Route::Fallback),
            // 看得出是媒體檔、串流清單、字幕：照原樣開（影片網站上的也是）
            ("https://cdn.test/a/video.MP4?sig=1", Route::Native),
            ("https://cdn.test/live/index.m3u8", Route::Native),
            ("https://cdn.test/v/manifest.mpd", Route::Native),
            ("https://www.youtube.com/a.mp3", Route::Native),
            ("https://cdn.test/s.vtt", Route::Native),
            // 不是網頁
            ("C:\\Videos\\a.mp4", Route::Native),
            ("/home/me/a.mkv", Route::Native),
            ("av://lavfi:testsrc", Route::Native),
            ("rtsp://cam.test/stream", Route::Native),
            ("memory://abc", Route::Native),
            ("edl://%3%abc", Route::Native),
            ("ftp://h/x", Route::Native),
        ] {
            assert_eq!(route(url, false, none), want, "{url}");
        }
        // 只對這次有效的要求：一定問 yt-dlp（http / https 才算）
        assert_eq!(route("https://cdn.test/a.mp4", true, none), Route::Site);
        assert_eq!(route("C:\\a.mp4", true, none), Route::Native);
        // 額外的網站（測試用本機的伺服器）
        let extra = vec!["127.0.0.1".to_owned()];
        assert_eq!(route("http://127.0.0.1:8080/watch?v=1", false, &extra), Route::Site);
        assert_eq!(route("http://127.0.0.1:8080/f/a.mp4", false, &extra), Route::Native);
        assert_eq!(route("http://127.0.0.1:8080/watch?v=1", false, none), Route::Fallback);
    }

    #[test]
    fn fallback_only_when_mpv_got_content_it_did_not_recognise() {
        // 讀到了內容、認不出格式：hook 的時候還沒有錯誤記錄（或只有跟連線無關的）
        assert!(fallback_wanted(""));
        assert!(fallback_wanted("[ffmpeg/demuxer] mov,mp4: moov atom not found"));
        // 連線失敗：mpv 一定記一筆 Failed to open（FFmpeg 的原因可能只送到別的播放器，見 net::classify_failure）
        for log in [
            "[ffmpeg] http: HTTP error 404 Not Found\n[stream] Failed to open http://h/x.",
            "[stream] Failed to open http://h/x.",
            "[ffmpeg] tcp: Failed to resolve hostname h: Unknown host\n[stream] Failed to open http://h/.",
            "[ffmpeg] tls: Creating security context failed (0x80092013)\n[stream] Failed to open https://h/.",
            "[stream] No protocol handler found to open URL gopher://h/",
            "[stream] Refusing to load potentially unsafe URL from a playlist.",
        ] {
            assert!(!fallback_wanted(log), "{log}");
        }
    }

    fn req(url: &str) -> Request {
        Request::new(
            url,
            &crate::net::NetSettings::default(),
            &crate::ytdl::SitePrefs::default(),
        )
        .unwrap()
    }

    fn resolved() -> Arc<Resolved> {
        Arc::new(Resolved {
            info: Default::default(),
            hints: Vec::new(),
        })
    }

    #[test]
    fn cache_keeps_the_newest_eight_by_request() {
        let mut c = Cache::default();
        for i in 0..10 {
            c.insert(req(&format!("https://h.test/v/{i}")), resolved());
        }
        assert_eq!(c.len(), CACHE_SIZE);
        assert!(c.get(&req("https://h.test/v/0")).is_none(), "最舊的丟掉");
        assert!(c.get(&req("https://h.test/v/9")).is_some());
        // 要求不一樣（整個播放清單、別的畫質）就不算
        let mut list = req("https://h.test/v/9");
        list.playlist = true;
        assert!(c.get(&list).is_none());
        let mut q = req("https://h.test/v/9");
        q.quality = crate::ytdl::SiteQuality::P720;
        assert!(c.get(&q).is_none());
        // 同一個要求再放一次：換成新的，不會變成兩個
        c.insert(req("https://h.test/v/9"), resolved());
        assert_eq!(c.len(), CACHE_SIZE);
    }

    #[test]
    fn dropping_a_job_cancels_it() {
        struct Blocking;
        impl Resolve for Blocking {
            fn resolve(&self, _: &Request, cancel: &AtomicBool) -> Result<Resolved, Failure> {
                let until = Instant::now() + Duration::from_secs(30);
                while !cancel.load(Ordering::Relaxed) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(if cancel.load(Ordering::Relaxed) {
                    YtdlError::Cancelled
                } else {
                    YtdlError::NoResponse
                }
                .into())
            }
            fn available(&self) -> bool {
                true
            }
            fn watchdog(&self, _: &Request) -> Duration {
                Duration::from_millis(50)
            }
        }
        let job = Job::spawn(Arc::new(Blocking), Weak::new(), 1, false, req("https://h.test/v")).unwrap();
        // 還沒有結果（看門狗很短：慢的機器上這時可能已經到了，所以不要求是 Waiting）
        assert!(!matches!(job.progress(), Progress::Done(_)));
        let flag = job.cancel.clone();
        // 看門狗到了
        let until = Instant::now() + Duration::from_secs(10);
        while !matches!(job.progress(), Progress::TimedOut) {
            assert!(Instant::now() < until, "看門狗沒有到");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!flag.load(Ordering::Relaxed));
        drop(job);
        assert!(flag.load(Ordering::Relaxed), "丟掉就通知取消");
    }
}
