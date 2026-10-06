//! 進度條預覽縮圖：滑鼠停在進度條上時，顯示那個時間的畫面。
//!
//! 另開一個看不見的 mpv（軟體繪圖、不出聲音、只解影像），在自己的執行緒裡：
//! - 只跳到關鍵影格（精確跳轉 4K 要一秒以上），不用硬體解碼（每個檔案第一張要多等半秒以上），
//!   解碼器 4 個執行緒、略過迴圈濾波（縮圖看不出差別，快很多），不預先讀取（網路磁碟少讀很多）
//! - 一次只解一張；滑鼠一直移動時只留最新的要求（「最新的贏」），滑鼠停下來幾十毫秒內就有圖
//! - mpv 的 libmpv 軟體繪圖不做旋轉：手機直拍的影片要自己轉
//! - 一張超過 1.5 秒（例如網路磁碟上沒有索引的 MKV，跳轉要從頭讀）就停掉，連續兩次就放棄這個檔案
//!
//! 實測（研究筆記見 ROADMAP「學到的事」）：1080p 約 20 毫秒、4K HEVC 10 bit 約 40–70 毫秒、
//! 網路磁碟約 60–70 毫秒；跟主播放器同時播 4K 也不會掉格。

use crate::mpv::render::RenderContext;
use crate::mpv::{Event, Mpv};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 縮圖的長邊（像素）
pub const LONG_SIDE: usize = 240;
/// 一張縮圖最多等多久
const TIMEOUT: Duration = Duration::from_millis(1500);
/// 閒置多久就停掉目前的檔案（放掉解碼器的記憶體）
const IDLE_STOP: Duration = Duration::from_secs(45);

const OPTIONS: &[(&str, &str)] = &[
    ("vo", "libmpv"),
    ("hwdec", "no"),
    ("ao", "null"),
    ("aid", "no"),
    ("sid", "no"),
    ("audio-file-auto", "no"),
    ("sub-auto", "no"),
    ("audio-display", "no"),
    ("pause", "yes"),
    ("keep-open", "always"),
    ("idle", "yes"),
    ("hr-seek", "no"),
    ("vd-lavc-threads", "4"),
    ("vd-lavc-skiploopfilter", "all"),
    ("cache", "no"),
    ("demuxer-readahead-secs", "0"),
    // 第一次繪圖時不要去列舉系統字型（DirectWrite / fontconfig 要零點幾秒）
    ("sub-font-provider", "none"),
    ("osd-level", "0"),
    // 不讓 mpv 自己處理檔案標示的旋轉：軟體繪圖不會轉，而且旋轉 90° 時它算的來源範圍會超出影格，
    // 有開斷言的 libmpv（Linux、macOS）會直接中止整個程式。改成自己轉（解碼器參數裡的旋轉不受這個設定影響）
    ("video-rotate", "no"),
    ("osc", "no"),
    ("ytdl", "no"),
    ("load-scripts", "no"),
    ("load-stats-overlay", "no"),
    ("input-default-bindings", "no"),
    ("input-vo-keyboard", "no"),
];

/// 一張縮圖（RGBA，已轉正）
#[derive(Debug, Clone)]
pub struct Thumb {
    /// 哪一次開檔（換檔之後舊的結果不要）
    pub file: u64,
    /// 時間區段（見 `bucket_len`）
    pub bucket: u32,
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

/// 時間區段的長度（秒）：一部片最多 300 段，每段 1–20 秒
pub fn bucket_len(duration: f64) -> f64 {
    (duration / 300.0).clamp(1.0, 20.0)
}

/// 時間 → 區段、區段的中間時間（跳轉的目標，不超過片尾）
pub fn bucket_of(time: f64, duration: f64) -> (u32, f64) {
    let len = bucket_len(duration);
    let bucket = (time.max(0.0) / len) as u32;
    let target = ((f64::from(bucket) + 0.5) * len).min((duration - 0.5).max(0.0));
    (bucket, target)
}

/// 縮圖的大小（像素）：長邊 `LONG_SIDE`，比例跟影片一樣（旋轉之前）
pub fn size_for(dw: i64, dh: i64) -> (usize, usize) {
    let (dw, dh) = (dw.max(1) as f64, dh.max(1) as f64);
    let long = LONG_SIDE as f64;
    let (w, h) = if dw >= dh {
        (long, long * dh / dw)
    } else {
        (long * dw / dh, long)
    };
    ((w.round() as usize).max(2), (h.round() as usize).max(2))
}

#[derive(Default)]
struct Inbox {
    open: Option<(u64, String)>,
    want: Option<(u64, u32, f64)>,
    wake: bool,
    quit: bool,
}

struct Shared {
    inbox: Mutex<Inbox>,
    cv: Condvar,
}

impl Shared {
    fn poke(&self, f: impl FnOnce(&mut Inbox)) {
        if let Ok(mut ib) = self.inbox.lock() {
            f(&mut ib);
            ib.wake = true;
        }
        self.cv.notify_one();
    }
}

/// 縮圖產生器（背景執行緒）
pub struct Thumbnailer {
    shared: Arc<Shared>,
    rx: Receiver<Thumb>,
    thread: Option<JoinHandle<()>>,
}

impl Thumbnailer {
    /// 建立（mpv 在背景執行緒裡初始化）。`repaint`：有新縮圖時叫介面重畫
    pub fn new(repaint: impl Fn() + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            inbox: Mutex::new(Inbox::default()),
            cv: Condvar::new(),
        });
        let (tx, rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("vitascope-thumbs".into())
            .spawn(move || {
                if let Err(e) = worker(&worker_shared, &tx, &repaint) {
                    eprintln!("[vitascope] 預覽縮圖無法使用：{e}");
                }
            })
            .ok();
        Self { shared, rx, thread }
    }

    /// 換成另一個檔案（`file` 是開檔的次數，結果會帶著它）
    pub fn open(&self, file: u64, path: &str) {
        let path = path.to_owned();
        self.shared.poke(|ib| {
            ib.open = Some((file, path));
            ib.want = None;
        });
    }

    /// 要這個區段的縮圖（取代還沒開始的舊要求）
    pub fn request(&self, file: u64, bucket: u32, time: f64) {
        self.shared.poke(|ib| ib.want = Some((file, bucket, time)));
    }

    /// 做好的縮圖
    pub fn try_recv(&self) -> Option<Thumb> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Thumbnailer {
    fn drop(&mut self) {
        self.shared.poke(|ib| ib.quit = true);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// 正在解的那一張
struct InFlight {
    bucket: u32,
    started: Instant,
}

fn worker(shared: &Arc<Shared>, tx: &Sender<Thumb>, repaint: &dyn Fn()) -> crate::mpv::Result<()> {
    let mut mpv = Mpv::new(OPTIONS)?;
    let wake = shared.clone();
    mpv.set_wakeup_callback(move || wake.poke(|_| {}));
    let mpv = Arc::new(mpv);
    let mut rc = RenderContext::new_sw(mpv.clone())?;
    let wake = shared.clone();
    rc.set_update_callback(move || wake.poke(|_| {}));

    let mut file = 0u64;
    // 檔案開好、知道影片大小了（沒有影像的檔案永遠不會是 true）
    let mut loaded = false;
    let mut failed = false;
    let mut timeouts = 0;
    // 縮圖大小、檔案本身的旋轉（知道之前先畫到小緩衝區：mpv 要我們一直畫，不然會卡住）
    let mut size = (16usize, 16usize);
    let mut rotate = 0i64;
    let mut buf = vec![0u8; size.0 * size.1 * 4];
    let mut inflight: Option<InFlight> = None;
    let mut last_used = Instant::now();
    let mut stopped = true;

    loop {
        let (open, quit) = {
            let Ok(mut ib) = shared.inbox.lock() else { break };
            let wait = if inflight.is_some() {
                Duration::from_millis(50)
            } else {
                Duration::from_secs(1)
            };
            if !ib.wake {
                ib = match shared.cv.wait_timeout(ib, wait) {
                    Ok((g, _)) => g,
                    Err(_) => break,
                };
            }
            ib.wake = false;
            (ib.open.take(), ib.quit)
        };
        if quit {
            break;
        }
        if let Some((f, path)) = open {
            file = f;
            loaded = false;
            failed = false;
            timeouts = 0;
            inflight = None;
            stopped = false;
            last_used = Instant::now();
            if mpv.command(&["loadfile", &path, "replace"]).is_err() {
                failed = true;
            }
        }
        // 先畫：mpv 等我們把新的影格畫完才送出「跳轉完成」，所以收到那個事件時緩衝區裡就是新的畫面
        if rc.update() {
            let _ = rc.render_sw(size.0, size.1, &mut buf);
        }
        while let Some(ev) = mpv.wait_event(0.0) {
            match ev {
                // 第一格解出來、畫面設定好之後才知道影片的大小（開檔當下還不知道）：這時才開始做縮圖
                Event::VideoReconfig => {
                    if let Some((dw, dh, r)) = dec_params(&mpv) {
                        let new_size = size_for(dw, dh);
                        if new_size != size {
                            size = new_size;
                            buf = vec![0u8; size.0 * size.1 * 4];
                        }
                        rotate = r;
                        loaded = true;
                    }
                }
                Event::PlaybackRestart => {
                    if let Some(job) = inflight.take() {
                        timeouts = 0;
                        let thumb = to_thumb(file, job.bucket, size, rotate, &buf);
                        if tx.send(thumb).is_err() {
                            return Ok(());
                        }
                        repaint();
                    }
                }
                Event::EndFile { error: Some(_), .. } => {
                    failed = true;
                    inflight = None;
                }
                Event::Shutdown => return Ok(()),
                _ => {}
            }
        }
        if inflight.as_ref().is_some_and(|j| j.started.elapsed() > TIMEOUT) {
            inflight = None;
            timeouts += 1;
            if timeouts >= 2 {
                // 網路磁碟上沒有索引的大檔案之類的：停掉，這個檔案不再做縮圖
                let _ = mpv.command(&["stop"]);
                failed = true;
            }
        }
        if inflight.is_none() && loaded && !failed {
            let want = shared.inbox.lock().ok().and_then(|mut ib| ib.want.take());
            match want {
                Some((f, bucket, time)) if f == file => {
                    last_used = Instant::now();
                    let target = format!("{time:.3}");
                    if mpv.command(&["seek", &target, "absolute+keyframes"]).is_ok() {
                        inflight = Some(InFlight {
                            bucket,
                            started: Instant::now(),
                        });
                    }
                }
                _ => {}
            }
        }
        // 很久沒用：停掉檔案，放掉解碼器的記憶體（下次要縮圖時介面會重新開檔）
        if !stopped && inflight.is_none() && last_used.elapsed() > IDLE_STOP {
            let _ = mpv.command(&["stop"]);
            stopped = true;
            loaded = false;
        }
    }
    Ok(())
}

/// 解碼器給的顯示大小與檔案本身的旋轉（不受任何調整影響）
fn dec_params(mpv: &Mpv) -> Option<(i64, i64, i64)> {
    #[derive(serde::Deserialize)]
    struct Dec {
        dw: i64,
        dh: i64,
        #[serde(default)]
        rotate: i64,
    }
    let d: Dec = serde_json::from_str(&mpv.get_string("video-dec-params").ok()?).ok()?;
    (d.dw > 0 && d.dh > 0).then_some((d.dw, d.dh, d.rotate.rem_euclid(360)))
}

/// 緩衝區（RGBX）→ 縮圖（RGBA、不透明、轉正）
fn to_thumb(file: u64, bucket: u32, (w, h): (usize, usize), rotate: i64, buf: &[u8]) -> Thumb {
    let mut rgba = buf[..w * h * 4].to_vec();
    for p in rgba.chunks_exact_mut(4) {
        p[3] = 255;
    }
    let img = crate::screenshot::Image { w, h, rgba }.fixed(crate::screenshot::Fixup {
        rotate: rotate as u32,
        ..Default::default()
    });
    Thumb {
        file,
        bucket,
        w: img.w,
        h: img.h,
        rgba: img.rgba,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        // 90 秒的片：每段 1 秒
        assert_eq!(bucket_of(10.4, 90.0), (10, 10.5));
        // 片尾不超過結尾前 0.5 秒
        assert_eq!(bucket_of(89.9, 90.0), (89, 89.5));
        // 兩小時：每段 20 秒（最多 300 段裡的 20 秒上限）
        assert_eq!(bucket_len(7200.0), 20.0);
        assert_eq!(bucket_of(65.0, 7200.0), (3, 70.0));
    }

    #[test]
    fn thumbnail_sizes_keep_the_aspect() {
        assert_eq!(size_for(1920, 1080), (240, 135));
        assert_eq!(size_for(1080, 1920), (135, 240));
        assert_eq!(size_for(640, 480), (240, 180));
    }
}
