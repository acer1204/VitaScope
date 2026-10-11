//! Windows 的系統媒體控制（SMTC：SystemMediaTransportControls）：音量浮動視窗、快速設定、鎖定畫面、
//! 鍵盤的媒體鍵、藍牙耳機的按鍵。
//!
//! - 在介面執行緒用主視窗建立（`ISystemMediaTransportControlsInterop::GetForWindow`，SMTC 是跟著視窗的），
//!   掛好事件之後交給自己的執行緒 `vitascope-media`：SMTC 的物件是 agile 的（可以在任何執行緒用），
//!   更新與封面（要等非同步的寫入）都在那裡做，介面不會卡住。那個執行緒是隱含的 MTA（程式一直保有 MTA，
//!   見 `player::keep_com_mta_alive`）。
//! - 系統的按鈕、時間軸、隨機、重複、速度的要求在系統的執行緒集區送來：轉成 [`MediaCommand`] 送進通道、叫醒介面。
//!   系統只是「要求」；顯示的值等影戲真的改了、下一次更新時才變。
//! - 換標題時 `ClearAll` 會連封面一起清掉：記住目前檔案的封面，換標題時一起放回去（S-B12）。
//! - 視窗在最前面時的媒體鍵：見 [`msg_hook`]。
//! - `VITASCOPE_DEBUG_MEDIA=1`：印出系統送來的指令、媒體鍵、系統回報的錯誤。

use super::{Cover, MediaCommand, MediaKind, MediaSession, Opened, PlaybackInfo, Status, TrackInfo, Update, Wake};
use crate::playlist::Repeat;
use ::windows::Foundation::{TimeSpan, TypedEventHandler};
use ::windows::Media::{
    AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode, MediaPlaybackStatus, MediaPlaybackType,
    PlaybackPositionChangeRequestedEventArgs, PlaybackRateChangeRequestedEventArgs,
    ShuffleEnabledChangeRequestedEventArgs, SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs, SystemMediaTransportControlsTimelineProperties,
};
use ::windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream, RandomAccessStreamReference};
use ::windows::Win32::Foundation::{HWND, RPC_E_DISCONNECTED};
use ::windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use ::windows::core::{HSTRING, Ref};
use raw_window_handle::RawWindowHandle;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DefWindowProcW, MSG, WM_KEYDOWN, WM_KEYUP};

/// 結束時最多等背景執行緒多久（拿掉事件、關掉 SMTC）；系統卡住時不能讓關閉影戲跟著卡住
const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

/// RPC 伺服器無法使用（HRESULT_FROM_WIN32(RPC_S_SERVER_UNAVAILABLE)）：系統那一端不見了
const RPC_SERVER_UNAVAILABLE: ::windows::core::HRESULT = ::windows::core::HRESULT(0x8007_06BA_u32 as i32);

/// TimeSpan 的單位是 100 奈秒
const TICKS_PER_SEC: f64 = 10_000_000.0;

fn debug_enabled() -> bool {
    std::env::var_os("VITASCOPE_DEBUG_MEDIA").is_some()
}

// ───────────── 建立 ─────────────

/// 建立主視窗的 SMTC 工作階段（在介面執行緒呼叫）
pub fn create(owner: Option<RawWindowHandle>, wake: Wake) -> Result<Opened, String> {
    let Some(RawWindowHandle::Win32(handle)) = owner else {
        return Err(crate::tr!("找不到主視窗", "The main window isn't available").to_owned());
    };
    // 背景執行緒不必自己初始化 COM（隱含的 MTA），也不會因為某個執行緒結束而把 COM 拆掉
    crate::player::keep_com_mta_alive();
    open(HWND(handle.hwnd.get() as *mut c_void), wake).map_err(|e| format!("{e} ({:#010x})", e.code().0))
}

fn open(hwnd: HWND, wake: Wake) -> ::windows::core::Result<Opened> {
    let interop = ::windows::core::factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
    // SAFETY: hwnd 是這個程式還開著的主視窗（eframe 的視窗在 App 結束之前都在）
    let smtc: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd)? };
    let (tx, rx) = mpsc::channel();
    let alive = Arc::new(AtomicBool::new(true));
    let shown_gen = Arc::new(AtomicU64::new(0));
    let out = Out {
        tx,
        wake: wake.clone(),
        debug: debug_enabled(),
    };
    let handlers = Handlers::register(&smtc, &out, &shown_gen)?;
    let setup = || -> ::windows::core::Result<()> {
        smtc.SetIsPlayEnabled(true)?;
        smtc.SetIsPauseEnabled(true)?;
        smtc.SetIsStopEnabled(true)?;
        smtc.SetIsNextEnabled(false)?;
        smtc.SetIsPreviousEnabled(false)?;
        smtc.SetIsEnabled(true)
    };
    if let Err(e) = setup() {
        handlers.remove(&smtc);
        return Err(e);
    }
    let (jobs, job_rx) = mpsc::channel();
    let worker = Worker {
        smtc,
        handlers,
        shown: Shown::default(),
        shown_gen,
        alive: alive.clone(),
        wake,
        debug: out.debug,
    };
    let spawned = std::thread::Builder::new()
        .name("vitascope-media".into())
        .spawn(move || worker.run(job_rx));
    if let Err(e) = spawned {
        // 執行緒開不起來：worker 已經被丟掉了（Drop 拿掉事件、關掉 SMTC）
        return Err(::windows::core::Error::new(
            ::windows::core::HRESULT(0x8000_4005_u32 as i32),
            e.to_string(),
        ));
    }
    let session = SmtcSession {
        jobs: Some(jobs),
        alive,
    };
    Ok((Box::new(session), rx))
}

/// 介面那一端：更新交給背景執行緒，不等
struct SmtcSession {
    /// None = 已經結束
    jobs: Option<Sender<Job>>,
    alive: Arc<AtomicBool>,
}

enum Job {
    Apply(Update),
    /// 結束；做完回覆
    Shutdown(Sender<()>),
}

impl MediaSession for SmtcSession {
    fn apply(&mut self, update: &Update) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Apply(update.clone()));
        }
    }

    fn shutdown(&mut self) {
        let Some(jobs) = self.jobs.take() else { return };
        let (done, wait) = mpsc::channel();
        // 斷線了：收拾也只會失敗，不等
        if jobs.send(Job::Shutdown(done)).is_ok() && self.alive() {
            let _ = wait.recv_timeout(SHUTDOWN_WAIT);
        }
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl Drop for SmtcSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ───────────── 系統 → 影戲 ─────────────

/// 把系統的要求送給介面
#[derive(Clone)]
struct Out {
    tx: Sender<MediaCommand>,
    wake: Wake,
    debug: bool,
}

impl Out {
    fn send(&self, cmd: MediaCommand) {
        if self.debug {
            eprintln!("[media] SMTC → {cmd:?}");
        }
        if self.tx.send(cmd).is_ok() {
            (self.wake)();
        }
    }
}

/// 掛在 SMTC 上的事件（結束時要拿掉）
struct Handlers {
    button: Option<i64>,
    position: Option<i64>,
    rate: Option<i64>,
    shuffle: Option<i64>,
    repeat: Option<i64>,
}

impl Handlers {
    /// 掛上所有事件；有一個失敗就把掛好的拿掉
    fn register(
        smtc: &SystemMediaTransportControls,
        out: &Out,
        shown_gen: &Arc<AtomicU64>,
    ) -> ::windows::core::Result<Self> {
        let mut h = Self {
            button: None,
            position: None,
            rate: None,
            shuffle: None,
            repeat: None,
        };
        if let Err(e) = h.register_all(smtc, out, shown_gen) {
            h.remove(smtc);
            return Err(e);
        }
        Ok(h)
    }

    fn register_all(
        &mut self,
        smtc: &SystemMediaTransportControls,
        out: &Out,
        shown_gen: &Arc<AtomicU64>,
    ) -> ::windows::core::Result<()> {
        let o = out.clone();
        self.button = Some(smtc.ButtonPressed(&TypedEventHandler::new(
            move |_, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                if let Some(cmd) = button_command(args.ok()?.Button()?) {
                    o.send(cmd);
                }
                Ok(())
            },
        ))?);
        let (o, shown) = (out.clone(), shown_gen.clone());
        self.position = Some(smtc.PlaybackPositionChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
                let secs = from_ticks(args.ok()?.RequestedPlaybackPosition()?);
                // 系統顯示的是哪個檔案的時間軸：介面已經換到別的檔案時不理（0 = 還沒顯示過）
                let track = Some(shown.load(Ordering::Relaxed)).filter(|&g| g != 0);
                o.send(MediaCommand::SeekTo { secs, track });
                Ok(())
            },
        ))?);
        let o = out.clone();
        self.rate = Some(smtc.PlaybackRateChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<PlaybackRateChangeRequestedEventArgs>| {
                o.send(MediaCommand::SetRate(args.ok()?.RequestedPlaybackRate()?));
                Ok(())
            },
        ))?);
        let o = out.clone();
        self.shuffle = Some(smtc.ShuffleEnabledChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<ShuffleEnabledChangeRequestedEventArgs>| {
                o.send(MediaCommand::SetShuffle(args.ok()?.RequestedShuffleEnabled()?));
                Ok(())
            },
        ))?);
        let o = out.clone();
        self.repeat = Some(smtc.AutoRepeatModeChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<AutoRepeatModeChangeRequestedEventArgs>| {
                if let Some(r) = repeat_from_mode(args.ok()?.RequestedAutoRepeatMode()?) {
                    o.send(MediaCommand::SetRepeat(r));
                }
                Ok(())
            },
        ))?);
        Ok(())
    }

    /// 拿掉掛好的事件（拿不掉的也沒辦法，SMTC 釋放時一起沒了）
    fn remove(&self, smtc: &SystemMediaTransportControls) {
        if let Some(t) = self.button {
            let _ = smtc.RemoveButtonPressed(t);
        }
        if let Some(t) = self.position {
            let _ = smtc.RemovePlaybackPositionChangeRequested(t);
        }
        if let Some(t) = self.rate {
            let _ = smtc.RemovePlaybackRateChangeRequested(t);
        }
        if let Some(t) = self.shuffle {
            let _ = smtc.RemoveShuffleEnabledChangeRequested(t);
        }
        if let Some(t) = self.repeat {
            let _ = smtc.RemoveAutoRepeatModeChangeRequested(t);
        }
    }
}

/// SMTC 的按鈕 → 指令（錄影、快轉、倒轉、頻道沒有打開，不會送來）
fn button_command(b: SystemMediaTransportControlsButton) -> Option<MediaCommand> {
    use SystemMediaTransportControlsButton as B;
    Some(match b {
        B::Play => MediaCommand::Play,
        B::Pause => MediaCommand::Pause,
        B::Stop => MediaCommand::Stop,
        B::Next => MediaCommand::Next,
        B::Previous => MediaCommand::Previous,
        _ => return None,
    })
}

/// 重複播放 → SMTC 的 AutoRepeatMode（關 / 單一檔案 / 全部 ↔ None / Track / List）
pub(crate) fn repeat_mode(r: Repeat) -> MediaPlaybackAutoRepeatMode {
    match r {
        Repeat::Off => MediaPlaybackAutoRepeatMode::None,
        Repeat::One => MediaPlaybackAutoRepeatMode::Track,
        Repeat::All => MediaPlaybackAutoRepeatMode::List,
    }
}

/// SMTC 要求的 AutoRepeatMode → 重複播放（不認得的值 None）
pub(crate) fn repeat_from_mode(m: MediaPlaybackAutoRepeatMode) -> Option<Repeat> {
    match m {
        MediaPlaybackAutoRepeatMode::None => Some(Repeat::Off),
        MediaPlaybackAutoRepeatMode::Track => Some(Repeat::One),
        MediaPlaybackAutoRepeatMode::List => Some(Repeat::All),
        _ => None,
    }
}

fn status(s: Status) -> MediaPlaybackStatus {
    match s {
        Status::Stopped => MediaPlaybackStatus::Stopped,
        Status::Playing => MediaPlaybackStatus::Playing,
        Status::Paused => MediaPlaybackStatus::Paused,
    }
}

/// 秒 → TimeSpan（100 奈秒）；負的、不是數字的當成 0
fn to_ticks(secs: f64) -> TimeSpan {
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    TimeSpan {
        Duration: (secs * TICKS_PER_SEC).round() as i64,
    }
}

fn from_ticks(t: TimeSpan) -> f64 {
    t.Duration as f64 / TICKS_PER_SEC
}

/// 時間軸（秒）：開始一律 0
#[derive(Debug, Clone, Copy, PartialEq)]
struct Timeline {
    end: f64,
    max_seek: f64,
    position: f64,
}

/// 目前的檔案長度與播放狀態 → 時間軸。沒有長度（直播）、沒有在播時全部 0（系統不顯示進度）；
/// 不能跳轉時最遠只能跳到 0（系統的時間軸拖不動）
fn timeline(duration: Option<f64>, info: &PlaybackInfo) -> Timeline {
    let end = duration.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(0.0);
    if end == 0.0 || info.status == Status::Stopped {
        return Timeline {
            end: 0.0,
            max_seek: 0.0,
            position: 0.0,
        };
    }
    let position = if info.position.is_finite() {
        info.position.clamp(0.0, end)
    } else {
        0.0
    };
    Timeline {
        end,
        max_seek: if info.seekable { end } else { 0.0 },
        position,
    }
}

// ───────────── 影戲 → 系統（背景執行緒）─────────────

/// 系統目前顯示的東西（決定封面要不要放回去、要不要重設）
#[derive(Debug, Default)]
struct Shown {
    /// 系統顯示的檔案（file_gen、顯示片名與封面）
    track: Option<(u64, bool)>,
    /// 目前檔案的長度（時間軸用）
    duration: Option<f64>,
    /// 目前檔案的封面（換標題時放回去）
    cover: Option<Cover>,
    /// 系統顯示的封面就是 `cover`（`ClearAll` 之後 false）
    thumb_shown: bool,
}

impl Shown {
    /// 換標題（`ClearAll` 會清掉封面）：回傳要一起放回去的封面（同一個檔案、顯示片名與封面時）
    fn on_track(&mut self, t: &TrackInfo) -> Option<Cover> {
        self.track = Some((t.file_gen, t.details));
        self.duration = t.duration;
        self.thumb_shown = false;
        if self.cover.as_ref().is_some_and(|c| c.file_gen != t.file_gen) {
            self.cover = None;
        }
        self.cover.clone().filter(|_| t.details)
    }

    /// 封面讀好了：記下來；回傳要不要現在設定（別的檔案的、隱藏片名時、已經顯示的同一張不用）
    fn on_cover(&mut self, c: &Cover) -> bool {
        let Some((file_gen, details)) = self.track else {
            return false;
        };
        if c.file_gen != file_gen {
            return false;
        }
        let same = self.thumb_shown
            && self
                .cover
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(&old.png, &c.png) || old.png == c.png);
        self.cover = Some(c.clone());
        details && !same
    }

    /// 停止（沒有在播任何檔案）
    fn on_clear(&mut self) {
        *self = Self::default();
    }
}

struct Worker {
    smtc: SystemMediaTransportControls,
    handlers: Handlers,
    shown: Shown,
    /// 系統顯示的檔案（給時間軸的要求帶上；0 = 沒有）
    shown_gen: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    wake: Wake,
    debug: bool,
}

impl Drop for Worker {
    /// 拿掉事件、系統的媒體控制不再顯示影戲（結束時；執行緒開不起來、worker 直接被丟掉時也一樣）
    fn drop(&mut self) {
        self.handlers.remove(&self.smtc);
        let _ = self.smtc.SetIsEnabled(false);
    }
}

impl Worker {
    fn run(mut self, jobs: Receiver<Job>) {
        let done = loop {
            match jobs.recv() {
                Ok(Job::Apply(u)) => self.apply(&u),
                Ok(Job::Shutdown(done)) => break Some(done),
                // 介面那一端沒說結束就不見了：一樣收拾
                Err(_) => break None,
            }
        };
        // 收拾（Drop）完才回覆
        drop(self);
        if let Some(done) = done {
            let _ = done.send(());
        }
    }

    fn apply(&mut self, u: &Update) {
        let (what, result) = match u {
            Update::Track(t) => ("track", self.track(t)),
            Update::Playback { info, .. } => ("playback", self.playback(info)),
            Update::Cover(c) => ("cover", self.cover(c)),
            Update::Clear => ("clear", self.clear()),
        };
        if let Err(e) = result {
            self.failed(what, &e);
        }
    }

    fn failed(&self, what: &str, e: &::windows::core::Error) {
        report(self.debug, &self.alive, &self.wake, what, e);
    }

    fn track(&mut self, t: &TrackInfo) -> ::windows::core::Result<()> {
        let (smtc, alive, wake, debug) = (&self.smtc, &self.alive, &self.wake, self.debug);
        change_track(&mut self.shown, &self.shown_gen, t, |back| {
            let d = smtc.DisplayUpdater()?;
            d.ClearAll()?;
            match t.kind {
                MediaKind::Video => {
                    d.SetType(MediaPlaybackType::Video)?;
                    let v = d.VideoProperties()?;
                    v.SetTitle(&HSTRING::from(t.title.as_str()))?;
                    if let Some(a) = &t.artist {
                        v.SetSubtitle(&HSTRING::from(a.as_str()))?;
                    }
                }
                MediaKind::Audio => {
                    d.SetType(MediaPlaybackType::Music)?;
                    let m = d.MusicProperties()?;
                    m.SetTitle(&HSTRING::from(t.title.as_str()))?;
                    if let Some(a) = &t.artist {
                        m.SetArtist(&HSTRING::from(a.as_str()))?;
                    }
                    if let Some(a) = &t.album {
                        m.SetAlbumTitle(&HSTRING::from(a.as_str()))?;
                    }
                    if let Some(a) = &t.album_artist {
                        m.SetAlbumArtist(&HSTRING::from(a.as_str()))?;
                    }
                    if let Some(n) = t.track_no {
                        m.SetTrackNumber(n)?;
                    }
                }
            }
            // 同一個檔案換標題（標籤、ICY 標題變了）：ClearAll 清掉的封面一起放回去，系統一次更新，不會閃
            let mut thumb = false;
            if let Some(c) = back {
                match thumbnail(&c.png).and_then(|s| d.SetThumbnail(&s)) {
                    Ok(()) => thumb = true,
                    Err(e) => report(debug, alive, wake, "cover", &e),
                }
            }
            d.Update()?;
            Ok(thumb)
        })
    }

    fn cover(&mut self, c: &Cover) -> ::windows::core::Result<()> {
        if !self.shown.on_cover(c) {
            return Ok(());
        }
        let d = self.smtc.DisplayUpdater()?;
        d.SetThumbnail(&thumbnail(&c.png)?)?;
        d.Update()?;
        self.shown.thumb_shown = true;
        Ok(())
    }

    fn playback(&mut self, info: &PlaybackInfo) -> ::windows::core::Result<()> {
        let s = &self.smtc;
        s.SetPlaybackStatus(status(info.status))?;
        s.SetIsNextEnabled(info.can_next)?;
        s.SetIsPreviousEnabled(info.can_prev)?;
        s.SetShuffleEnabled(info.shuffle)?;
        s.SetAutoRepeatMode(repeat_mode(info.repeat))?;
        if info.rate.is_finite() && info.rate > 0.0 {
            s.SetPlaybackRate(info.rate)?;
        }
        let tl = timeline(self.shown.duration, info);
        let p = SystemMediaTransportControlsTimelineProperties::new()?;
        p.SetStartTime(to_ticks(0.0))?;
        p.SetEndTime(to_ticks(tl.end))?;
        p.SetMinSeekTime(to_ticks(0.0))?;
        p.SetMaxSeekTime(to_ticks(tl.max_seek))?;
        p.SetPosition(to_ticks(tl.position))?;
        s.UpdateTimelineProperties(&p)
    }

    /// 沒有在播任何檔案：顯示停止（工作階段留著，按播放鍵可以開停止前的檔案）
    fn clear(&mut self) -> ::windows::core::Result<()> {
        self.shown.on_clear();
        self.shown_gen.store(0, Ordering::Relaxed);
        self.smtc.SetPlaybackStatus(MediaPlaybackStatus::Stopped)
    }
}

/// 系統回報錯誤：印出來（除錯時）；SMTC 斷線了（檔案總管重開之類的）就通知介面，下一個檔案時重新建立
fn report(debug: bool, alive: &AtomicBool, wake: &Wake, what: &str, e: &::windows::core::Error) {
    if debug {
        eprintln!("[media] SMTC {what} 失敗：{e} ({:#010x})", e.code().0);
    }
    if (e.code() == RPC_E_DISCONNECTED || e.code() == RPC_SERVER_UNAVAILABLE) && alive.swap(false, Ordering::Relaxed) {
        wake();
    }
}

/// 換標題：先記下系統顯示的是哪個檔案（長度、封面、時間軸要求帶的 file_gen），再交給 `write` 寫進系統
/// （回傳封面放回去了沒有）。寫入失敗時（不是斷線；SessionSync 不會再送同一個 Track），之後的播放狀態、封面、
/// 拖動時間軸照樣是這個檔案的，不會整個檔案都停在上一個（S§10：下一次更新自然重試）
fn change_track<E>(
    shown: &mut Shown,
    shown_gen: &AtomicU64,
    t: &TrackInfo,
    write: impl FnOnce(Option<Cover>) -> Result<bool, E>,
) -> Result<(), E> {
    let back = shown.on_track(t);
    shown_gen.store(t.file_gen, Ordering::Relaxed);
    shown.thumb_shown = write(back)?;
    Ok(())
}

/// PNG → 系統要的封面（放在記憶體裡的資料流）
fn thumbnail(png: &[u8]) -> ::windows::core::Result<RandomAccessStreamReference> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(png)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    // 資料流還要用：先跟 writer 分開，不然 writer 釋放時會把它關掉
    writer.DetachStream()?;
    stream.Seek(0)?;
    RandomAccessStreamReference::CreateFromStream(&stream)
}

// ───────────── 視窗在最前面時的媒體鍵 ─────────────

/// 媒體鍵（`WM_KEYDOWN` / `WM_KEYUP` 的虛擬鍵碼）的名稱；其他訊息、其他按鍵 None
fn media_key(message: u32, vk: usize) -> Option<&'static str> {
    if message != WM_KEYDOWN && message != WM_KEYUP {
        return None;
    }
    let vk = u16::try_from(vk).ok()?;
    Some(match vk {
        VK_MEDIA_PLAY_PAUSE => "VK_MEDIA_PLAY_PAUSE",
        VK_MEDIA_NEXT_TRACK => "VK_MEDIA_NEXT_TRACK",
        VK_MEDIA_PREV_TRACK => "VK_MEDIA_PREV_TRACK",
        VK_MEDIA_STOP => "VK_MEDIA_STOP",
        _ => return None,
    })
}

/// winit 的訊息掛鉤（main.rs 用 `EventLoopBuilderExtWindows::with_msg_hook` 裝上）：視窗在最前面時按的媒體鍵。
///
/// winit 0.30.13 處理 `WM_KEYDOWN` / `WM_KEYUP` 時回傳 0、不交給 `DefWindowProcW`
/// （`platform_impl/windows/event_loop.rs` 1123–1131、1767–1778；`keyboard.rs` 113），egui 也沒有媒體鍵：
/// 一般的 Win32 視窗會把沒處理的按鍵交給 `DefWindowProcW`（再由它產生 `WM_APPCOMMAND` → 殼層 → 目前的 SMTC
/// 工作階段），winit 的視窗不會。這裡在 winit 分派之前把媒體鍵交給 `DefWindowProcW`，回傳 true（winit 不再處理），
/// 讓影戲在最前面時跟一般的視窗一樣（S-B1）。影戲自己不處理媒體鍵，指令照樣只從 SMTC 送來，所以不會重複。
/// 關掉「系統媒體控制」時也一樣交出去：媒體鍵照 Windows 本來的做法給目前的媒體程式。
///
/// 掛鉤在 `PeekMessageW` 之後、`TranslateMessage` / `DispatchMessageW` 之前呼叫（`event_loop.rs` 406–420），
/// 只看得到 post 來的訊息（`WM_APPCOMMAND` 是 send 的，看不到）。
/// 注意：post 的假按鍵 `DefWindowProcW` 不會變成 `WM_APPCOMMAND`（測試看得到）；真的按鍵要實機確認
/// （`VITASCOPE_DEBUG_MEDIA=1`：這裡的記錄後面接著「SMTC → …」一次）
pub fn msg_hook(msg: *const c_void) -> bool {
    hand_over(msg, def_window_proc)
}

/// 系統的預設處理（`DefWindowProcW`）。測試記下收到的按鍵：真的訊息迴圈的測試確認媒體鍵真的交給了它，
/// 不是只被掛鉤吃掉
fn def_window_proc(hwnd: windows_sys::Win32::Foundation::HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    #[cfg(test)]
    tests::DEF_WINDOW_PROC.with(|s| s.borrow_mut().push((message, wparam)));
    // SAFETY: 原封不動交給系統的預設處理（視窗是這個執行緒的）
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// [`msg_hook`] 本身：是媒體鍵就交給 `default_proc`（系統的 `DefWindowProcW`；測試換成記錄用的），回傳 true
fn hand_over(
    msg: *const c_void,
    default_proc: impl FnOnce(windows_sys::Win32::Foundation::HWND, u32, usize, isize) -> isize,
) -> bool {
    if msg.is_null() {
        return false;
    }
    // SAFETY: winit 傳來的是它 PeekMessageW 拿到的 MSG（這一次呼叫期間有效）
    let msg = unsafe { &*msg.cast::<MSG>() };
    let Some(name) = media_key(msg.message, msg.wParam) else {
        return false;
    };
    if debug_enabled() {
        let kind = if msg.message == WM_KEYDOWN {
            "WM_KEYDOWN"
        } else {
            "WM_KEYUP"
        };
        eprintln!("[media] {kind} {name} → DefWindowProcW");
    }
    default_proc(msg.hwnd, msg.message, msg.wParam, msg.lParam);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        /// [`def_window_proc`] 收到的按鍵（訊息、虛擬鍵碼）：每個測試執行緒各自一份
        pub(super) static DEF_WINDOW_PROC: RefCell<Vec<(u32, usize)>> = const { RefCell::new(Vec::new()) };
    }

    fn take_def_window_proc() -> Vec<(u32, usize)> {
        DEF_WINDOW_PROC.with(|s| std::mem::take(&mut *s.borrow_mut()))
    }

    #[test]
    fn repeat_maps_to_auto_repeat_mode() {
        assert_eq!(repeat_mode(Repeat::Off), MediaPlaybackAutoRepeatMode::None);
        assert_eq!(repeat_mode(Repeat::One), MediaPlaybackAutoRepeatMode::Track);
        assert_eq!(repeat_mode(Repeat::All), MediaPlaybackAutoRepeatMode::List);
        for r in [Repeat::Off, Repeat::One, Repeat::All] {
            assert_eq!(repeat_from_mode(repeat_mode(r)), Some(r));
        }
        assert_eq!(repeat_from_mode(MediaPlaybackAutoRepeatMode(7)), None);
    }

    #[test]
    fn buttons_map_to_commands() {
        use SystemMediaTransportControlsButton as B;
        assert_eq!(button_command(B::Play), Some(MediaCommand::Play));
        assert_eq!(button_command(B::Pause), Some(MediaCommand::Pause));
        assert_eq!(button_command(B::Stop), Some(MediaCommand::Stop));
        assert_eq!(button_command(B::Next), Some(MediaCommand::Next));
        assert_eq!(button_command(B::Previous), Some(MediaCommand::Previous));
        assert_eq!(button_command(B::FastForward), None);
        assert_eq!(button_command(B::ChannelUp), None);
        assert_eq!(status(Status::Paused), MediaPlaybackStatus::Paused);
        assert_eq!(status(Status::Stopped), MediaPlaybackStatus::Stopped);
    }

    #[test]
    fn ticks_are_100_ns() {
        assert_eq!(to_ticks(1.5).Duration, 15_000_000);
        assert_eq!(to_ticks(-3.0).Duration, 0);
        assert_eq!(to_ticks(f64::NAN).Duration, 0);
        assert_eq!(from_ticks(TimeSpan { Duration: 25_000_000 }), 2.5);
    }

    fn info(status: Status, position: f64, seekable: bool) -> PlaybackInfo {
        PlaybackInfo {
            status,
            position,
            rate: 1.0,
            seekable,
            can_next: false,
            can_prev: false,
            shuffle: false,
            repeat: Repeat::Off,
            volume: 1.0,
            buffering: false,
            can_play: true,
        }
    }

    #[test]
    fn timeline_follows_duration_and_seekability() {
        let t = timeline(Some(100.0), &info(Status::Playing, 30.0, true));
        assert_eq!(
            t,
            Timeline {
                end: 100.0,
                max_seek: 100.0,
                position: 30.0
            }
        );
        // 不能跳轉：系統的時間軸拖不動
        assert_eq!(timeline(Some(100.0), &info(Status::Paused, 30.0, false)).max_seek, 0.0);
        // 超過長度、負的：夾在範圍內
        assert_eq!(
            timeline(Some(100.0), &info(Status::Playing, 130.0, true)).position,
            100.0
        );
        assert_eq!(timeline(Some(100.0), &info(Status::Playing, -1.0, true)).position, 0.0);
        // 直播（沒有長度）、停止：不顯示進度
        let none = Timeline {
            end: 0.0,
            max_seek: 0.0,
            position: 0.0,
        };
        assert_eq!(timeline(None, &info(Status::Playing, 30.0, true)), none);
        assert_eq!(timeline(Some(f64::NAN), &info(Status::Playing, 30.0, true)), none);
        assert_eq!(timeline(Some(100.0), &info(Status::Stopped, 30.0, true)), none);
    }

    fn track(file_gen: u64, details: bool) -> TrackInfo {
        TrackInfo {
            file_gen,
            kind: MediaKind::Audio,
            title: "歌".into(),
            artist: None,
            album: None,
            album_artist: None,
            track_no: None,
            duration: Some(60.0),
            url: None,
            details,
        }
    }

    fn cover(file_gen: u64, bytes: &[u8]) -> Cover {
        Cover {
            file_gen,
            png: Arc::new(bytes.to_vec()),
            path: PathBuf::from(format!("cover-1-{file_gen}.png")),
        }
    }

    #[test]
    fn the_cover_is_put_back_after_clear_all() {
        let mut s = Shown::default();
        // 還沒有檔案：封面不要
        assert!(!s.on_cover(&cover(1, b"a")));
        assert_eq!(s.on_track(&track(1, true)), None);
        assert_eq!(s.duration, Some(60.0));
        assert!(s.on_cover(&cover(1, b"a")));
        s.thumb_shown = true;
        // 同一張又送來（SessionSync 在每個 Track 之後都會送）：不用重設
        assert!(!s.on_cover(&cover(1, b"a")));
        // 同一個檔案換標題（ClearAll）：封面一起放回去（S-B12）
        let back = s.on_track(&track(1, true));
        assert_eq!(back.as_ref().map(|c| c.file_gen), Some(1));
        s.thumb_shown = true;
        assert!(!s.on_cover(&cover(1, b"a")), "放回去之後緊接著送來的同一張不用再設");
        // 換了一張（同一個檔案）：設定
        assert!(s.on_cover(&cover(1, b"b")));
        // 隱藏片名與封面：換標題時不放回去，送來的也不設；再打開：放回去
        assert_eq!(s.on_track(&track(1, false)), None);
        assert!(!s.on_cover(&cover(1, b"b")));
        assert!(s.on_track(&track(1, true)).is_some());
        // 別的檔案的封面不要；換檔時舊的丟掉
        assert!(!s.on_cover(&cover(9, b"c")));
        assert_eq!(s.on_track(&track(2, true)), None);
        assert_eq!(s.on_track(&track(2, true)), None, "上一個檔案的封面不會回來");
        // 停止：全部忘掉
        assert!(s.on_cover(&cover(2, b"d")));
        s.on_clear();
        assert!(!s.on_cover(&cover(2, b"d")));
        assert_eq!(s.duration, None);
    }

    /// 換標題寫進系統失敗（不是斷線）：SessionSync 不會再送同一個 Track，所以系統顯示的檔案、長度、
    /// 時間軸要求帶的 file_gen 要先換成新的，之後的封面、時間軸、拖動進度條才不會整個檔案都停在上一個（S§10）
    #[test]
    fn a_failed_track_write_still_moves_to_the_new_file() {
        let mut s = Shown::default();
        let shown_gen = AtomicU64::new(0);
        assert_eq!(
            change_track(&mut s, &shown_gen, &track(1, true), |_| Ok::<_, ()>(false)),
            Ok(())
        );
        assert!(s.on_cover(&cover(1, b"a")));
        s.thumb_shown = true;
        let mut next = track(2, true);
        next.duration = Some(90.0);
        let mut wrote = false;
        let r = change_track(&mut s, &shown_gen, &next, |back| {
            assert_eq!(back, None, "上一個檔案的封面不放回去");
            wrote = true;
            Err("SetTitle 失敗")
        });
        assert_eq!(r, Err("SetTitle 失敗"));
        assert!(wrote);
        assert_eq!(shown_gen.load(Ordering::Relaxed), 2, "時間軸的要求帶新的檔案");
        assert_eq!(
            timeline(s.duration, &info(Status::Playing, 80.0, true)).end,
            90.0,
            "時間軸用新的長度"
        );
        assert!(s.on_cover(&cover(2, b"b")), "新的檔案的封面照樣設定");
        // 寫入成功：封面放回去了就記下來（緊接著送來的同一張不用再設）
        s.thumb_shown = true;
        assert_eq!(
            change_track(&mut s, &shown_gen, &next, |back| Ok::<_, ()>(back.is_some())),
            Ok(())
        );
        assert!(s.thumb_shown);
        assert!(!s.on_cover(&cover(2, b"b")));
        // 封面放不回去：之後送來的同一張要再設
        assert_eq!(change_track(&mut s, &shown_gen, &next, |_| Ok::<_, ()>(false)), Ok(()));
        assert!(s.on_cover(&cover(2, b"b")));
    }

    #[test]
    fn only_media_keys_are_handed_to_def_window_proc() {
        assert_eq!(
            media_key(WM_KEYDOWN, usize::from(VK_MEDIA_PLAY_PAUSE)),
            Some("VK_MEDIA_PLAY_PAUSE")
        );
        assert_eq!(
            media_key(WM_KEYUP, usize::from(VK_MEDIA_NEXT_TRACK)),
            Some("VK_MEDIA_NEXT_TRACK")
        );
        assert!(media_key(WM_KEYDOWN, usize::from(VK_MEDIA_PREV_TRACK)).is_some());
        assert!(media_key(WM_KEYDOWN, usize::from(VK_MEDIA_STOP)).is_some());
        // 一般的按鍵、音量鍵、其他訊息：照 winit 原本的處理
        assert_eq!(media_key(WM_KEYDOWN, 0x20), None);
        assert_eq!(media_key(WM_KEYDOWN, 0xAF), None);
        assert_eq!(media_key(0x0104, usize::from(VK_MEDIA_PLAY_PAUSE)), None);
        assert_eq!(media_key(WM_KEYDOWN, 0x1_00B3), None);
        assert!(!msg_hook(std::ptr::null()));
    }

    #[test]
    fn the_hook_hands_media_keys_to_the_default_proc_unchanged() {
        let msg = |message: u32, vk: u16, lparam: isize| MSG {
            hwnd: 0x1234 as _,
            message,
            wParam: usize::from(vk),
            lParam: lparam,
            ..Default::default()
        };
        let run = |m: &MSG| {
            let mut calls = Vec::new();
            let handled = hand_over(std::ptr::from_ref(m).cast(), |h, message, w, l| {
                calls.push((h as usize, message, w, l));
                0
            });
            (handled, calls)
        };
        // 媒體鍵按下、放開：原封不動交出去，winit 不再處理
        for m in [
            msg(WM_KEYDOWN, VK_MEDIA_PLAY_PAUSE, 0x0122_0001),
            msg(WM_KEYUP, VK_MEDIA_STOP, -0x3EDB_FFFF),
            msg(WM_KEYDOWN, VK_MEDIA_NEXT_TRACK, 1),
            msg(WM_KEYDOWN, VK_MEDIA_PREV_TRACK, 1),
        ] {
            assert_eq!(
                run(&m),
                (true, vec![(0x1234, m.message, m.wParam, m.lParam)]),
                "{:#x} {:#x}",
                m.message,
                m.wParam
            );
        }
        // 其他按鍵（空白鍵、音量鍵）、WM_SYSKEYDOWN（winit 本來就交給 DefWindowProcW）：不動
        for m in [
            msg(WM_KEYDOWN, 0x20, 1),
            msg(WM_KEYDOWN, 0xAF, 1),
            msg(0x0104, VK_MEDIA_PLAY_PAUSE, 1),
        ] {
            assert_eq!(run(&m), (false, vec![]));
        }
    }

    /// 測試自己的隱藏視窗（像 winit 的視窗程序：WM_KEYDOWN / WM_KEYUP 回傳 0、不交給 DefWindowProcW）
    mod probe {
        use std::cell::RefCell;
        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DefWindowProcW, FAPPCOMMAND_MASK, WM_APPCOMMAND, WM_KEYDOWN, WM_KEYUP,
        };

        thread_local! {
            /// 視窗程序收到的按鍵（訊息、虛擬鍵碼）
            pub static KEYS: RefCell<Vec<(u32, usize)>> = const { RefCell::new(Vec::new()) };
            /// 視窗程序收到的 WM_APPCOMMAND（`GET_APPCOMMAND_LPARAM`）
            pub static APP_COMMANDS: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
        }

        /// WM_APPCOMMAND 一律自己處理掉（回傳 TRUE），絕不往上交給殼層：不會動到使用者其他的媒體程式
        pub unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            match msg {
                WM_APPCOMMAND => {
                    let cmd = u32::from((lp as u32 >> 16) as u16) & !FAPPCOMMAND_MASK;
                    APP_COMMANDS.with(|s| s.borrow_mut().push(cmd));
                    1
                }
                WM_KEYDOWN | WM_KEYUP => {
                    KEYS.with(|s| s.borrow_mut().push((msg, wp)));
                    0
                }
                // SAFETY: 其他訊息照系統的預設處理
                _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
            }
        }

        pub fn take_keys() -> Vec<(u32, usize)> {
            KEYS.with(|s| std::mem::take(&mut *s.borrow_mut()))
        }

        pub fn take_app_commands() -> Vec<u32> {
            APP_COMMANDS.with(|s| std::mem::take(&mut *s.borrow_mut()))
        }
    }

    /// 真的視窗、真的訊息迴圈（PeekMessageW → 掛鉤 → TranslateMessage / DispatchMessageW，跟 winit 一樣）：
    /// 沒有掛鉤時媒體鍵進了 winit 的視窗程序（被吃掉）；有掛鉤時原封不動交給 DefWindowProcW（[`def_window_proc`]
    /// 記下的），視窗程序收不到，其他按鍵照舊。只 post 給測試自己的隱藏視窗，不送全域的按鍵。
    /// 這裡證明的只是「交給了 DefWindowProcW」：post 的假按鍵 DefWindowProcW 不會變成 WM_APPCOMMAND，
    /// 真的按鍵會不會變成 WM_APPCOMMAND → SMTC 只能實機確認（e7/NOTES.md）；這裡只檢查不會多出來
    #[test]
    fn a_real_message_loop_hands_media_keys_over_and_keeps_other_keys() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, PM_REMOVE, PeekMessageW, PostMessageW, RegisterClassW,
            TranslateMessage, UnregisterClassW, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        };
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let class = wide(&format!("VitaScopeMediaKeyTest{}", std::process::id()));
        // SAFETY: 這個測試執行緒自己的視窗類別與隱藏視窗（不顯示），測試結束前銷毀。hInstance 是 NULL：這個程式自己
        let hwnd = unsafe {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(probe::wndproc),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            assert_ne!(RegisterClassW(&wc), 0);
            let title = wide("VitaScope media key test");
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                100,
                100,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let pump = |hooked: bool| {
            // SAFETY: 只取這個測試視窗的訊息；MSG 由系統填好
            unsafe {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE) != 0 {
                    if !(hooked && msg_hook(std::ptr::from_ref(&msg).cast())) {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        };
        // 實際按鍵的 lParam：按一次、掃描碼、延伸鍵；放開時再加上前一個狀態與轉換旗標
        let press = |vk: u16, scan: isize| {
            let down = 1 | (scan << 16) | (1 << 24);
            // SAFETY: 送給這個執行緒自己的視窗
            unsafe {
                assert_ne!(PostMessageW(hwnd, WM_KEYDOWN, usize::from(vk), down), 0);
                assert_ne!(PostMessageW(hwnd, WM_KEYUP, usize::from(vk), down | (3 << 30)), 0);
            }
        };
        let play = usize::from(VK_MEDIA_PLAY_PAUSE);

        let next = usize::from(VK_MEDIA_NEXT_TRACK);
        take_def_window_proc();

        // winit 的做法：媒體鍵進了視窗程序，被吃掉
        press(VK_MEDIA_PLAY_PAUSE, 0x22);
        pump(false);
        assert_eq!(probe::take_keys(), [(WM_KEYDOWN, play), (WM_KEYUP, play)]);
        assert_eq!(take_def_window_proc(), []);
        // 經過掛鉤：按下與放開都原封不動交給 DefWindowProcW，視窗程序收不到
        press(VK_MEDIA_PLAY_PAUSE, 0x22);
        press(VK_MEDIA_NEXT_TRACK, 0x19);
        pump(true);
        assert_eq!(probe::take_keys(), []);
        assert_eq!(
            take_def_window_proc(),
            [
                (WM_KEYDOWN, play),
                (WM_KEYUP, play),
                (WM_KEYDOWN, next),
                (WM_KEYUP, next)
            ],
            "媒體鍵交給了 DefWindowProcW，不是只被掛鉤吃掉"
        );
        // 一般的按鍵照 winit 原本的處理
        press(0x20, 0x39);
        pump(true);
        assert_eq!(probe::take_keys(), [(WM_KEYDOWN, 0x20), (WM_KEYUP, 0x20)]);
        assert_eq!(take_def_window_proc(), []);
        // 多出來的 WM_APPCOMMAND（如果有）最多一個按鍵一個：不會重複
        assert!(probe::take_app_commands().len() <= 2);

        // SAFETY: 上面建立的視窗與類別
        unsafe {
            DestroyWindow(hwnd);
            UnregisterClassW(class.as_ptr(), std::ptr::null_mut());
        }
    }
}
