//! 縮圖總覽圖的擷取：在背景執行緒上同步地一張一張取畫面（另外開一個軟體繪圖的 mpv，不動正在播放的那一個）。
//!
//! 做法跟進度條的預覽縮圖（`thumbs.rs`）一樣，差在這裡是一張接一張、等每一張做完：
//! - `vo=libmpv` + 軟體繪圖：mpv 等我們把新的影格畫完才送出「跳轉完成」，收到時緩衝區裡就是那一格；
//!   收到之後再畫一次（保險：等太久時 mpv 會先送出），然後讀 `time-pos`：圖與時間一定是同一格。
//! - 每次跳轉先等 mpv 開始執行這次跳轉（`Seek` 事件）再收「跳轉完成」：上一次逾時的跳轉晚到的完成不算。
//! - 跳轉落在檔尾、沒有解出新的影格時 mpv 也送出「跳轉完成」，繪圖的 context 留著的是上一格（或開檔時那一格）的畫面，
//!   `time-pos` 是跳轉的目標或檔尾：這時 `eof-reached` 是真的（停在一格上時不會是），或這次跳轉之後根本沒有新的影格。
//!   兩個都看，是 [`Grab::Empty`]：不拿舊的畫面配新的時間。
//! - 畫面在我們的濾鏡裡縮成格子的大小（lanczos；HDR 也在這裡轉成一般畫面，格子小，4K HDR 也快），
//!   `keepaspect=no` 讓繪圖照格子的大小，不留黑邊。旋轉自己做（`video-rotate=no`：軟體繪圖不轉，見 `thumbs.rs`）。
//! - 濾鏡失敗（mpv 停用它、照原樣送出畫面）跟轉成 GIF 一樣看 mpv 的記錄，算失敗。
//!   收到過一次就一直記著（[`LogTail::graph_failed`]），最後收完全部的記錄再決定。
//!
//! 時間：這裡都是擷取用的 mpv 的時間（`time-pos`）；跟主播放器的換算見 [`Grabber::start_time`]。

use super::clip::Source;
use super::gif::load_failure;
use super::{Ctl, Failure, LogTail};
use crate::mpv::render::RenderContext;
use crate::mpv::{Event, Mpv};
use crate::player::Track;
use crate::screenshot::Image;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 開檔最多等多久（網路很慢、CI 很慢）
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// 停下來時最多等 mpv 結束這麼久
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// mpv 有新事件、有新影格要畫時叫醒擷取的執行緒
#[derive(Default)]
struct Signal {
    flag: Mutex<bool>,
    cv: Condvar,
}

impl Signal {
    fn poke(&self) {
        if let Ok(mut f) = self.flag.lock() {
            *f = true;
        }
        self.cv.notify_one();
    }

    /// 等到被叫醒（最多 `timeout`）
    fn wait(&self, timeout: Duration) {
        let Ok(mut f) = self.flag.lock() else { return };
        if !*f {
            f = match self.cv.wait_timeout(f, timeout) {
                Ok((g, _)) => g,
                Err(_) => return,
            };
        }
        *f = false;
    }
}

/// 怎麼開擷取用的 mpv
#[derive(Debug, Clone)]
pub struct Setup {
    pub source: Source,
    /// 繪圖的大小（轉正之前；就是格子轉正之前的大小）
    pub size: (u32, u32),
    /// 畫面的濾鏡（mpv 的 `vf`：縮放、HDR 轉一般畫面）
    pub vf: String,
    /// 主播放器選的影像在這裡預期的編號（None = 讓 mpv 選）
    pub vid: Option<i64>,
    /// 去交錯（主播放器的設定：auto / yes / no）
    pub deinterlace: &'static str,
}

/// 怎麼跳
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Seek {
    /// 跳到目標之前的關鍵影格（快，不用解碼到目標）
    Keyframe,
    /// 精確跳到目標那一格；分離器先往前跳 `demuxer_offset` 秒（跳轉不準的格式落在目標之後時用）
    Exact { demuxer_offset: f64 },
}

/// 一次跳轉的結果
#[derive(Debug, Clone, PartialEq)]
pub enum Grab {
    /// 這次跳轉解出的那一格
    Frame(Frame),
    /// 跳轉完成了，但沒有新的影格（落在檔尾：mpv 照樣送出完成，畫面還是上一格的）
    Empty,
    /// 時間內沒有完成
    TimedOut,
}

/// 擷取到的一格
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// 這一格的時間（擷取用的 mpv 的 `time-pos`）
    pub time: f64,
    /// RGBA（第 4 個位元組是 255），轉正之前，大小是 `Setup::size`
    pub image: Image,
}

/// 擷取用的 mpv（欄位的順序就是丟掉的順序：繪圖的 context 要在 mpv 之前釋放）
pub struct Grabber {
    rc: RenderContext,
    mpv: Arc<Mpv>,
    signal: Arc<Signal>,
    source: Source,
    size: (usize, usize),
    buf: Vec<u8>,
    log: LogTail,
    shut_down: bool,
    start_time: f64,
}

impl Grabber {
    /// 開一個擷取用的 mpv、開檔，等到檔案載入（`ctl` 取消時停下來）
    pub fn open(ctl: &Ctl, setup: &Setup) -> Result<Grabber, Failure> {
        let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(2));
        let vid = setup.vid.map_or_else(|| "auto".to_owned(), |i| i.to_string());
        let threads = threads.to_string();
        let mut opts: Vec<(&str, &str)> = vec![
            ("vo", "libmpv"),
            ("hwdec", "no"),
            ("ao", "null"),
            ("aid", "no"),
            ("sid", "no"),
            ("vid", &vid),
            ("audio-display", "no"),
            ("cover-art-auto", "no"),
            ("pause", "yes"),
            ("keep-open", "always"),
            ("idle", "yes"),
            // 解碼的執行緒不要搶走正在播放的那一個的 CPU
            ("vd-lavc-threads", &threads),
            // 不用字幕：第一次繪圖時不要去列舉系統字型
            ("sub-font-provider", "none"),
            ("osd-level", "0"),
            // 自己轉正（軟體繪圖不轉；旋轉 90° 時 mpv 算的範圍會超出影格，見 thumbs.rs）
            ("video-rotate", "no"),
            // 照格子的大小畫，不留黑邊（濾鏡已經縮成格子的大小，比例由我們算好）
            ("keepaspect", "no"),
            ("deinterlace", setup.deinterlace),
            ("vf", &setup.vf),
            ("load-stats-overlay", "no"),
            ("input-vo-keyboard", "no"),
        ];
        if matches!(setup.source, Source::File(_)) {
            // 本機檔案（可能在網路磁碟上）：不預先讀取，跳轉少讀很多（跟預覽縮圖一樣）
            opts.push(("cache", "no"));
            opts.push(("demuxer-readahead-secs", "0"));
        }
        opts.extend_from_slice(super::INSTANCE_OPTIONS);
        let mut mpv = Mpv::new(&opts).map_err(|e| Failure::Engine(e.description()))?;
        let _ = mpv.request_log_messages("warn");
        if let Source::Net(stream) = &setup.source {
            // 跟主播放器一樣的連線方式（User-Agent、標頭、proxy、憑證、逾時；網站影片的 Cookie）
            for (name, value) in &stream.options {
                if let Err(e) = mpv.set_node(name, value) {
                    eprintln!("[vitascope] 縮圖總覽圖：無法設定 {name}：{e}");
                }
            }
        }
        let signal = Arc::new(Signal::default());
        let poke = signal.clone();
        mpv.set_wakeup_callback(move || poke.poke());
        let mpv = Arc::new(mpv);
        let mut rc = RenderContext::new_sw(mpv.clone()).map_err(|e| Failure::Engine(e.description()))?;
        let poke = signal.clone();
        rc.set_update_callback(move || poke.poke());
        let size = (setup.size.0.max(2) as usize, setup.size.1.max(2) as usize);
        let mut g = Grabber {
            rc,
            mpv,
            signal,
            source: setup.source.clone(),
            size,
            buf: vec![0; size.0 * size.1 * 4],
            log: LogTail::default(),
            shut_down: false,
            start_time: 0.0,
        };
        if let Err(e) = g.mpv.command(&["loadfile", &setup.source.target()]) {
            g.stop();
            return Err(Failure::Engine(e.description()));
        }
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            if ctl.cancelled() {
                g.stop();
                return Err(Failure::Cancelled);
            }
            g.render_pending();
            while let Some(ev) = g.next() {
                match ev {
                    Event::FileLoaded => {
                        g.start_time = g
                            .mpv
                            .get_property::<f64>("demuxer-start-time")
                            .ok()
                            .filter(|t| t.is_finite())
                            .unwrap_or(0.0);
                        return Ok(g);
                    }
                    Event::EndFile { .. } | Event::Shutdown => {
                        let f = g.failure();
                        g.stop();
                        return Err(f);
                    }
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                g.stop();
                return Err(if matches!(setup.source, Source::Net(_)) {
                    Failure::SourceUnreachable
                } else {
                    Failure::ReadTimeout
                });
            }
            g.signal.wait(Duration::from_millis(50));
        }
    }

    /// 擷取用的 mpv（測試讀它的選項）
    pub fn mpv(&self) -> &Mpv {
        &self.mpv
    }

    /// 這裡的時間 0 是影片的哪個時間戳（`demuxer-start-time`）。主播放器的時間 t = 這裡的 t − 主播放器的 + 這個
    pub fn start_time(&self) -> f64 {
        self.start_time
    }

    /// 這裡看到的總長度（跟主播放器的比，確認時間對得上；網路直播沒有）
    pub fn duration(&self) -> Option<f64> {
        self.mpv
            .get_property::<f64>("duration")
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0)
    }

    /// 軌道清單
    pub fn tracks(&self) -> Vec<Track> {
        self.mpv
            .get_string("track-list")
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    /// 換成這條影像（開檔時預期的編號對不上時）
    pub fn select_video(&self, id: i64) -> Result<(), Failure> {
        self.mpv
            .set_property("vid", id)
            .map_err(|e| Failure::Engine(e.description()))
    }

    /// 跳到 `t`（這裡的時間）取一格：解出的那一格、沒有新的影格（檔尾）、`timeout` 內沒有完成
    /// （呼叫的地方決定要不要再試）。取消、濾鏡失敗、檔案讀到一半出錯時回傳原因
    pub fn grab(&mut self, ctl: &Ctl, t: f64, seek: Seek, timeout: Duration) -> Result<Grab, Failure> {
        // 之前排著的事件先收掉（上一次逾時的跳轉的完成之類）
        self.render_pending();
        while let Some(ev) = self.next() {
            if matches!(ev, Event::EndFile { .. } | Event::Shutdown) {
                return Err(self.failure());
            }
        }
        let target = format!("{t:.6}");
        let sent = match seek {
            Seek::Keyframe => self.mpv.command(&["seek", &target, "absolute+keyframes"]),
            Seek::Exact { demuxer_offset } => self
                .mpv
                .set_property("hr-seek-demuxer-offset", demuxer_offset.max(0.0))
                .and_then(|()| self.mpv.command(&["seek", &target, "absolute+exact"])),
        };
        sent.map_err(|e| Failure::Engine(e.description()))?;
        let deadline = Instant::now() + timeout;
        // mpv 開始執行這次跳轉了（之前收到的「跳轉完成」是上一次的）
        let mut started = false;
        // 這次跳轉之後畫過新的影格（`Seek` 事件之前就排著，所以跟它同一輪、或之後畫的才算）
        let mut fresh = false;
        loop {
            ctl.check()?;
            let rendered = self.render_pending();
            while let Some(ev) = self.next() {
                match ev {
                    Event::Seek => started = true,
                    Event::PlaybackRestart if started => {
                        // 這次跳轉停住的那一格：再畫一次（mpv 等太久時會先送出完成），時間就是畫面上那一格的
                        fresh |= rendered | self.render_now()?;
                        return self.settle(fresh);
                    }
                    Event::EndFile { .. } | Event::Shutdown => return Err(self.failure()),
                    _ => {}
                }
            }
            fresh |= rendered && started;
            // 濾鏡失敗：mpv 停用它、照原樣送出畫面（大小、顏色都不對）
            if self.log.graph_failed() {
                return Err(Failure::FilterFailed);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Grab::TimedOut);
            }
            self.signal.wait(left.min(Duration::from_millis(50)));
        }
    }

    /// 跳轉停住了（「跳轉完成」，畫面上那一格已經畫好）：決定取到的是什麼。`fresh` = 這次跳轉之後畫過新的影格。
    /// 排著的事件、記錄都收完再看濾鏡（mpv 先送事件、最後才送記錄：「Disabling filter」可能還排在後面）。
    /// 測試直接呼叫它，確定排著的濾鏡失敗不會被當成取到的畫面
    #[doc(hidden)]
    pub fn settle(&mut self, fresh: bool) -> Result<Grab, Failure> {
        // 落在檔尾：開檔時解出的那一格可能在跳轉之後才畫到（算成新的），所以也看 `eof-reached`
        let eof = self.mpv.get_property::<bool>("eof-reached").unwrap_or(false);
        let fresh = fresh_frame(fresh, self.drop_count(), eof);
        let time = self.mpv.get_property::<f64>("time-pos").ok().filter(|t| t.is_finite());
        // 跳轉已經完成、停住了，之後不會再有這次跳轉的事件；檔案出錯結束時照樣回報
        while let Some(ev) = self.next() {
            if matches!(ev, Event::EndFile { .. } | Event::Shutdown) {
                return Err(self.failure());
            }
        }
        if self.log.graph_failed() {
            return Err(Failure::FilterFailed);
        }
        Ok(match time {
            Some(time) if fresh => Grab::Frame(Frame {
                time,
                image: self.image(),
            }),
            // 沒有新的影格（落在檔尾）：畫面是上一格的，時間是跳轉的目標或檔尾，都不能用
            _ => Grab::Empty,
        })
    }

    /// 取完了：叫 mpv 結束、收完剩下的記錄，最後再看一次濾鏡有沒有失敗
    /// （mpv 先送排著的事件、最後才送記錄：「Disabling filter」可能比最後一格的「跳轉完成」晚到）
    pub fn finish(&mut self) -> Result<(), Failure> {
        self.stop();
        // Shutdown 之後不會再有事件，排著的記錄送完就是 None
        while self.next().is_some() {}
        if self.log.graph_failed() {
            return Err(Failure::FilterFailed);
        }
        Ok(())
    }

    /// 這次跳轉之後丟掉幾格（`frame-drop-count`：mpv 每次跳轉都歸零，見 [`fresh_frame`]）
    fn drop_count(&self) -> Option<i64> {
        self.mpv.get_property::<i64>("frame-drop-count").ok()
    }

    /// 下一個排著的事件（不等；記錄另外收起來，記下 Shutdown）
    fn next(&mut self) -> Option<Event> {
        let ev = self.mpv.wait_event(0.0)?;
        self.log.push_event(&ev);
        if matches!(ev, Event::Shutdown) {
            self.shut_down = true;
        }
        Some(ev)
    }

    /// 有新影格就畫（mpv 等我們畫完才往下走）；回傳有沒有新的影格
    fn render_pending(&mut self) -> bool {
        let new = self.rc.update();
        if new {
            let _ = self.rc.render_sw(self.size.0, self.size.1, &mut self.buf);
        }
        new
    }

    /// 現在畫一次（沒有新的影格時畫目前的那一格）；回傳有沒有新的影格
    fn render_now(&mut self) -> Result<bool, Failure> {
        let new = self.rc.update();
        self.rc
            .render_sw(self.size.0, self.size.1, &mut self.buf)
            .map_err(|e| Failure::Engine(e.description()))?;
        Ok(new)
    }

    /// 緩衝區（RGBX）→ RGBA 圖（不透明）
    fn image(&self) -> Image {
        let (w, h) = self.size;
        let mut rgba = self.buf[..w * h * 4].to_vec();
        for p in rgba.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        Image { w, h, rgba }
    }

    /// 開檔、讀檔失敗的原因（排著的記錄先收完）
    fn failure(&mut self) -> Failure {
        while self.next().is_some() {}
        load_failure(&self.log, &self.source)
    }

    /// 停下來：叫 mpv 結束，等它結束（最多一下子）
    fn stop(&mut self) {
        if self.shut_down || self.mpv.command(&["quit"]).is_err() {
            return;
        }
        let deadline = Instant::now() + QUIT_WAIT;
        while !self.shut_down && Instant::now() < deadline {
            self.render_pending();
            if self.next().is_none() {
                self.signal.wait(Duration::from_millis(20));
            }
        }
    }
}

/// 跳轉停住（「跳轉完成」）時，畫面上是不是這次跳轉解出的新的一格。
/// - `rendered`：mpv 開始執行這次跳轉（`Seek` 事件）之後畫過新的影格。
/// - `drops`：`frame-drop-count`。mpv 每次跳轉都把它歸零（`vo_seek_reset`），所以就是這次跳轉之後丟掉的：
///   我們太慢沒畫到時，mpv 等 0.2 秒後自己換上那一格、算一格丟掉，畫面上也是那一格。
///   讀不到時當成沒有丟（寧可算成沒有新的影格，也不拿舊的畫面配新的時間）。
/// - mpv 等這次跳轉的第一格畫完（或丟掉）才送出「跳轉完成」，所以跳轉之前就排著的那一格晚一點畫到、晚一點丟掉，
///   後面也還有這次的那一格；只有跳轉落在檔尾、沒有解出新的影格時不是，這時 `eof` 是真的。
fn fresh_frame(rendered: bool, drops: Option<i64>, eof: bool) -> bool {
    (rendered || drops.is_some_and(|n| n > 0)) && !eof
}

#[cfg(test)]
mod tests {
    use super::fresh_frame;

    #[test]
    fn a_frame_is_fresh_when_drawn_or_dropped_after_the_seek() {
        // 跳轉之後畫過
        assert!(fresh_frame(true, Some(0), false));
        assert!(fresh_frame(true, None, false));
        // 太慢沒畫到，mpv 自己換上、算一格丟掉：計數每次跳轉都歸零，丟過一格就算，不跟跳轉之前的比
        // （上一次跳轉丟了 1 格、這次又丟 1 格時也是 1）
        assert!(fresh_frame(false, Some(1), false));
        assert!(fresh_frame(false, Some(3), false));
        // 沒畫、沒丟、讀不到：沒有新的影格
        assert!(!fresh_frame(false, Some(0), false));
        assert!(!fresh_frame(false, None, false));
        // 落在檔尾：畫面是上一格的
        assert!(!fresh_frame(true, Some(1), true));
        assert!(!fresh_frame(false, Some(1), true));
    }
}
