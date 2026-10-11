//! 播放影片時螢幕不關閉、播放音訊時電腦不睡眠。
//!
//! - mpv 的 `stop-screensaver`（預設 yes）只對它自己的視窗有效：嵌入用的 `vo=libmpv` 不處理
//!   （`vo_libmpv.c` 的 `control()` 沒有 `VOCTRL_KILL_SCREENSAVER`），所以影戲自己來。
//!   照 mpv 讀到的值（[`StopScreensaver`]，`Player::stop_screensaver`）：`VITASCOPE_MPV_OPTS=stop-screensaver=no` 可以關掉，
//!   `always` 是有影片就一直不關（暫停時也是）。
//! - 要什麼由純函式 [`want`] 決定：播放中的影片 → 螢幕不關（[`Want::Display`]）；只有聲音 → 電腦不睡眠
//!   （[`Want::System`]，螢幕照常關）；暫停、停止、播完、播完後的倒數與動作 → 都不要。
//!   播完後要睡眠、休眠、關機時一定先放掉（自己的要求不能擋住自己的動作）。
//! - 真的向系統要求的是注入的 [`Inhibitor`]（`Launch.inhibitor`）：自動測試、`--shot` 沒有（或用假的），
//!   [`RealInhibitor`] 只有 main.rs 建立。系統的呼叫在自己的執行緒上做（Linux 是 D-Bus），不卡住介面。
//! - 各平台：
//!   - Windows：`PowerCreateRequest` / `PowerSetRequest` / `PowerClearRequest`（`powercfg /requests` 看得到）；
//!   - macOS：IOKit 的 `IOPMAssertionCreateWithName` / `IOPMAssertionRelease`；
//!   - Linux：工作階段匯流排的 `org.freedesktop.ScreenSaver.Inhibit` / `UnInhibit`（KDE、Xfce、Cinnamon、MATE，
//!     GNOME 由 gsd 轉接）。沒有這個服務時只記錄一次。這個介面只管螢幕：只有聲音時不做什麼（跟 mpv 一樣）。
//!     連線斷了（匯流排重開）時重新連上、再要求一次。
//! - 結束程式時放掉；就算沒放掉，程式結束時系統也會收回（Windows 的 handle、macOS 的 assertion、D-Bus 的連線）。

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// mpv 的 `stop-screensaver`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StopScreensaver {
    /// `no`：不管螢幕保護程式、電源
    No,
    /// `yes`（預設）：播放中不關
    #[default]
    Yes,
    /// `always`：有影片時一直不關（暫停、播完停在最後一格也是）
    Always,
}

impl StopScreensaver {
    /// mpv 回報的值（`no` / `yes` / `always`；舊版的 mpv 是 flag，也只有 no / yes）。看不懂的當成預設的 yes
    pub fn from_mpv(value: &str) -> Self {
        match value.trim() {
            "no" => Self::No,
            "always" => Self::Always,
            _ => Self::Yes,
        }
    }
}

/// 要系統保持什麼
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Want {
    /// 都不要（照系統的設定關螢幕、睡眠）
    #[default]
    Nothing,
    /// 螢幕不關（電腦當然也不睡眠）
    Display,
    /// 電腦不睡眠，螢幕可以關（只有聲音）
    System,
}

/// 算 [`want`] 用的播放狀態
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Playing {
    /// 有檔案載入
    pub loaded: bool,
    pub paused: bool,
    /// 播到結尾（keep-open 停在最後一格）
    pub eof: bool,
    /// 有實際影像（不是只有專輯封面）
    pub has_video: bool,
    /// 播完後的動作正在進行：等匯出、倒數、睡眠 / 關機 / 關閉影戲已經開始
    pub ending: bool,
}

/// 現在要系統保持什麼（純函式）
pub fn want(mode: StopScreensaver, p: &Playing) -> Want {
    // 播完後要睡眠、關機、關閉影戲：一定放掉（`always` 也一樣）
    if mode == StopScreensaver::No || !p.loaded || p.ending {
        return Want::Nothing;
    }
    let playing = !p.paused && !p.eof;
    match (p.has_video, playing) {
        (true, true) => Want::Display,
        // always：有影片就一直不關（mpv 是只要有影片輸出）
        (true, false) if mode == StopScreensaver::Always => Want::Display,
        (false, true) => Want::System,
        _ => Want::Nothing,
    }
}

/// 向系統要求螢幕不關、電腦不睡眠的方法（`Launch.inhibitor`）。自動測試用假的；真的（[`RealInhibitor`]）只由 main.rs 建立
pub trait Inhibitor: Send + Sync {
    /// 改成要 `want`（介面的執行緒在變了的時候呼叫；不能卡住：慢的交給自己的執行緒）
    fn set(&self, want: Want);
    /// 等之前的 [`Inhibitor::set`] 都做完，最多 `timeout`；回傳有沒有做完（睡眠、關機之前在背景執行緒上、結束時在介面的執行緒上呼叫）
    fn settle(&self, timeout: Duration) -> bool {
        let _ = timeout;
        true
    }
}

// ───────────── 真的系統 ─────────────

/// 背景執行緒多久看一次連線還在不在（Linux：匯流排重開時重新要求）
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// 結束時最多等背景執行緒多久（放掉要求）
const QUIT_WAIT: Duration = Duration::from_secs(2);

/// 系統那一端的做法（每個平台一個；測試用假的）
#[doc(hidden)]
pub trait Backend: Send + 'static {
    /// 改成要 `want`；`label` 是系統顯示的程式名稱、原因
    fn apply(&mut self, want: Want, label: Label) -> Result<(), Failure>;
    /// 要求被系統收回了（Linux：匯流排斷線）：回傳 true 時背景執行緒重新要求一次
    fn lost(&mut self) -> bool {
        false
    }
}

/// 要求失敗的原因（只記錄，不顯示）
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Failure {
    /// 系統沒有這個服務（Linux 沒有 org.freedesktop.ScreenSaver）
    Missing(String),
    Failed(String),
}

/// 系統顯示的文字（語言記在執行緒上：在介面的執行緒上決定）
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Label {
    /// 程式名稱（Linux 的 ScreenSaver 介面；跟視窗標題、系統媒體控制一樣是 `app_name()`）
    pub app: &'static str,
    /// 原因（Windows 的 `powercfg /requests`、macOS 的 `pmset -g assertions`、Linux 的桌面）
    pub reason: &'static str,
}

/// 介面與背景執行緒共用
#[derive(Debug, Default)]
struct Progress {
    /// 送出的要求數、背景做完的
    sent: u64,
    done: u64,
    /// 系統現在真的拿著的（做完的最後一個成功的要求）
    held: Want,
    /// 記錄過的失敗（同樣的只記一次）
    logged: HashSet<Failure>,
}

struct Shared {
    progress: Mutex<Progress>,
    changed: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Progress> {
        // 拿著鎖的地方不會 panic；萬一有，照樣用裡面的資料
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }
}

enum Job {
    Set {
        seq: u64,
        want: Want,
        label: Label,
    },
    /// 放掉、結束；做完回覆
    Quit(Sender<()>),
}

/// 真的向系統要求（各平台的做法見模組說明）。**只有 main.rs 建立**（不是 `--shot` 時）：自動測試、介面測試一律用假的
pub struct RealInhibitor {
    jobs: Mutex<Option<Sender<Job>>>,
    shared: Arc<Shared>,
}

impl RealInhibitor {
    /// 這個作業系統的（Linux：使用者的工作階段匯流排）
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self::with_backend(platform::Backend::new())
    }

    /// 測試用：連到指定的 D-Bus 匯流排（測試自己開的 dbus-daemon，絕不是使用者的）
    #[doc(hidden)]
    #[cfg(all(unix, not(target_os = "macos")))]
    pub fn on_bus(address: &str) -> Self {
        Self::with_backend(platform::Backend::at(address))
    }

    /// 測試用：系統那一端換成 `backend`
    #[doc(hidden)]
    pub fn with_backend(backend: impl Backend) -> Self {
        let shared = Arc::new(Shared {
            progress: Mutex::new(Progress::default()),
            changed: Condvar::new(),
        });
        let (tx, rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("vitascope-inhibit".into())
            .spawn(move || run(backend, rx, &worker_shared));
        let jobs = match spawned {
            Ok(_) => Some(tx),
            Err(e) => {
                eprintln!("[vitascope] 無法讓螢幕保持開啟：{e}");
                None
            }
        };
        Self {
            jobs: Mutex::new(jobs),
            shared,
        }
    }

    /// 測試用：系統現在真的拿著的
    #[doc(hidden)]
    pub fn held(&self) -> Want {
        self.shared.lock().held
    }

    /// 測試用：記錄過的失敗（同樣的只記一次）
    #[doc(hidden)]
    pub fn failures(&self) -> Vec<Failure> {
        self.shared.lock().logged.iter().cloned().collect()
    }

    fn sender(&self) -> Option<Sender<Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Inhibitor for RealInhibitor {
    fn set(&self, want: Want) {
        // 系統顯示的名稱、原因（語言記在執行緒上：在介面的執行緒上決定）。影片、音訊共用一個原因：
        // Windows 的電源要求建立時就定了原因，之後只改要哪幾種
        let label = Label {
            app: crate::app::app_name(),
            reason: crate::tr!("影戲正在播放", "VitaScope is playing"),
        };
        let Some(jobs) = self.sender() else { return };
        let mut p = self.shared.lock();
        p.sent += 1;
        let seq = p.sent;
        if jobs.send(Job::Set { seq, want, label }).is_err() {
            // 背景執行緒不在了：不用等
            p.done = p.sent;
        }
    }

    fn settle(&self, timeout: Duration) -> bool {
        let p = self.shared.lock();
        let (p, _) = self
            .shared
            .changed
            .wait_timeout_while(p, timeout, |p| p.done < p.sent)
            .unwrap_or_else(|e| e.into_inner());
        p.done >= p.sent
    }
}

impl Drop for RealInhibitor {
    /// 放掉要求、結束背景執行緒（最多等 [`QUIT_WAIT`]；來不及的話程式結束時系統也會收回）
    fn drop(&mut self) {
        let Some(jobs) = self.jobs.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        let (done, wait) = mpsc::channel();
        if jobs.send(Job::Quit(done)).is_ok() {
            let _ = wait.recv_timeout(QUIT_WAIT);
        }
    }
}

/// 背景執行緒：一次處理排著的要求（只做最後一個），做完通知等的人
fn run(mut backend: impl Backend, jobs: Receiver<Job>, shared: &Shared) {
    let mut current = Want::Nothing;
    let mut label = Label::default();
    let quit = loop {
        let first = match jobs.recv_timeout(WATCH_EVERY) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => {
                // 系統收回了（Linux 的匯流排重開）：還要的話重新要求
                if backend.lost() && current != Want::Nothing {
                    apply(&mut backend, current, label, shared);
                }
                continue;
            }
            // 介面那一端沒說結束就不見了：一樣放掉
            Err(RecvTimeoutError::Disconnected) => break None,
        };
        let mut latest = None;
        let mut quit = None;
        for job in std::iter::once(first).chain(jobs.try_iter()) {
            match job {
                Job::Set { seq, want, label } => latest = Some((seq, want, label)),
                Job::Quit(ack) => {
                    quit = Some(ack);
                    break;
                }
            }
        }
        if let Some((seq, want, text)) = latest {
            current = want;
            label = text;
            apply(&mut backend, want, label, shared);
            {
                let mut p = shared.lock();
                p.done = p.done.max(seq);
            }
            shared.changed.notify_all();
        }
        if quit.is_some() {
            break quit;
        }
    };
    apply(&mut backend, Want::Nothing, label, shared);
    drop(backend);
    {
        let mut p = shared.lock();
        p.done = p.sent;
    }
    shared.changed.notify_all();
    if let Some(ack) = quit {
        let _ = ack.send(());
    }
}

fn apply(backend: &mut impl Backend, want: Want, label: Label, shared: &Shared) {
    match backend.apply(want, label) {
        Ok(()) => shared.lock().held = want,
        Err(e) => {
            // 失敗時系統可能還拿著一部分：當成沒有（下一次改變時再試）
            let mut p = shared.lock();
            p.held = Want::Nothing;
            if p.logged.insert(e.clone()) {
                match e {
                    Failure::Missing(what) => {
                        eprintln!("[vitascope] 系統沒有 {what}：播放時螢幕可能會關閉、進入螢幕保護程式")
                    }
                    Failure::Failed(what) => eprintln!("[vitascope] 無法讓螢幕保持開啟：{what}"),
                }
            }
        }
    }
}

/// 照 [`Want`] 要哪幾種（螢幕、電腦）：螢幕不關時電腦也不睡眠
#[cfg_attr(all(unix, not(target_os = "macos")), allow(dead_code))]
fn kinds(want: Want) -> (bool, bool) {
    match want {
        Want::Nothing => (false, false),
        Want::Display => (true, true),
        Want::System => (false, true),
    }
}

// ───────────── 各平台 ─────────────

#[cfg(windows)]
mod platform {
    use super::{Failure, Label, Want, kinds};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Power::{
        POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired,
        PowerRequestSystemRequired, PowerSetRequest,
    };
    use windows_sys::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT};

    /// SystemServices 的 POWER_REQUEST_CONTEXT_VERSION（只為了一個常數不多開一個 windows-sys 功能）
    const CONTEXT_VERSION: u32 = 0;

    /// 一個電源要求（第一次要時建立），和它現在設了哪幾種
    pub struct Backend {
        request: Option<HANDLE>,
        /// 現在設了哪幾種（測試也看）
        pub(super) display: bool,
        pub(super) system: bool,
    }

    // SAFETY: 電源要求的 handle 可以在任何執行緒上用；只在背景執行緒上用
    unsafe impl Send for Backend {}

    impl Backend {
        pub fn new() -> Self {
            Self {
                request: None,
                display: false,
                system: false,
            }
        }

        fn request(&mut self, reason: &str) -> Result<HANDLE, Failure> {
            if let Some(h) = self.request {
                return Ok(h);
            }
            // 系統會複製原因的文字
            let mut text: Vec<u16> = reason.encode_utf16().chain([0]).collect();
            let mut context = REASON_CONTEXT {
                Version: CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                ..Default::default()
            };
            context.Reason.SimpleReasonString = text.as_mut_ptr();
            // SAFETY: context 與文字在呼叫期間都有效
            let h = unsafe { PowerCreateRequest(&context) };
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                return Err(Failure::Failed(format!(
                    "PowerCreateRequest：{}",
                    std::io::Error::last_os_error()
                )));
            }
            self.request = Some(h);
            Ok(h)
        }
    }

    fn change(h: HANDLE, kind: POWER_REQUEST_TYPE, on: bool) -> Result<(), Failure> {
        // SAFETY: h 是 PowerCreateRequest 建立、還沒關掉的
        let ok = unsafe {
            if on {
                PowerSetRequest(h, kind)
            } else {
                PowerClearRequest(h, kind)
            }
        };
        if ok == 0 {
            let name = if on { "PowerSetRequest" } else { "PowerClearRequest" };
            return Err(Failure::Failed(format!("{name}：{}", std::io::Error::last_os_error())));
        }
        Ok(())
    }

    impl super::Backend for Backend {
        fn apply(&mut self, want: Want, label: Label) -> Result<(), Failure> {
            let (display, system) = kinds(want);
            if (display, system) == (self.display, self.system) {
                return Ok(());
            }
            let h = self.request(label.reason)?;
            // 先設要的、再清不要的：從影片換成音訊時，中間不會有一下什麼都沒有
            for (kind, on, held) in [
                (PowerRequestSystemRequired, system, &mut self.system),
                (PowerRequestDisplayRequired, display, &mut self.display),
            ] {
                if on && !*held {
                    change(h, kind, true)?;
                    *held = true;
                }
            }
            for (kind, on, held) in [
                (PowerRequestDisplayRequired, display, &mut self.display),
                (PowerRequestSystemRequired, system, &mut self.system),
            ] {
                if !on && *held {
                    change(h, kind, false)?;
                    *held = false;
                }
            }
            Ok(())
        }
    }

    impl Drop for Backend {
        /// 關掉 handle：系統收回這個要求的每一種
        fn drop(&mut self) {
            if let Some(h) = self.request.take() {
                // SAFETY: PowerCreateRequest 建立的，只關一次
                unsafe { CloseHandle(h) };
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Failure, Label, Want, kinds};
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    use std::ffi::c_void;

    /// IOPMLib.h：kIOPMAssertionLevelOn
    const LEVEL_ON: u32 = 255;
    /// kIOPMAssertionTypePreventUserIdleDisplaySleep / kIOPMAssertionTypePreventUserIdleSystemSleep
    pub(super) const DISPLAY: &str = "PreventUserIdleDisplaySleep";
    pub(super) const SYSTEM: &str = "PreventUserIdleSystemSleep";

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        /// `CFStringRef` 的參數傳 NSString（toll-free bridged）；成功回傳 0（kIOReturnSuccess）
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            name: *const c_void,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    /// 拿著的 assertion（螢幕、電腦各一個）
    pub struct Backend {
        display: Option<u32>,
        system: Option<u32>,
    }

    impl Backend {
        pub fn new() -> Self {
            Self {
                display: None,
                system: None,
            }
        }
    }

    fn cf(s: &str) -> Retained<NSString> {
        NSString::from_str(s)
    }

    fn create(kind: &str, reason: &str) -> Result<u32, Failure> {
        let (kind, name) = (cf(kind), cf(reason));
        let mut id = 0u32;
        // SAFETY: 兩個字串在呼叫期間都有效（IOKit 自己複製）；id 是輸出
        let status = unsafe {
            IOPMAssertionCreateWithName(
                Retained::as_ptr(&kind).cast(),
                LEVEL_ON,
                Retained::as_ptr(&name).cast(),
                &mut id,
            )
        };
        if status != 0 {
            return Err(Failure::Failed(format!("IOPMAssertionCreateWithName：{status:#x}")));
        }
        Ok(id)
    }

    fn release(id: u32) {
        // SAFETY: create 拿到的 id，只放一次
        unsafe { IOPMAssertionRelease(id) };
    }

    impl super::Backend for Backend {
        fn apply(&mut self, want: Want, label: Label) -> Result<(), Failure> {
            let (display, system) = kinds(want);
            let reason = label.reason;
            // 先拿要的、再放不要的（從影片換成音訊時中間不會有一下什麼都沒有）
            if system && self.system.is_none() {
                self.system = Some(create(SYSTEM, reason)?);
            }
            if display && self.display.is_none() {
                self.display = Some(create(DISPLAY, reason)?);
            }
            if !display && let Some(id) = self.display.take() {
                release(id);
            }
            if !system && let Some(id) = self.system.take() {
                release(id);
            }
            Ok(())
        }
    }

    impl Drop for Backend {
        fn drop(&mut self) {
            for id in [self.display.take(), self.system.take()].into_iter().flatten() {
                release(id);
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::{Failure, Label, Want};
    use std::time::Duration;
    use zbus::blocking::Connection;
    use zbus::blocking::connection::Builder;

    const DEST: &str = "org.freedesktop.ScreenSaver";
    const PATH: &str = "/org/freedesktop/ScreenSaver";
    const IFACE: &str = "org.freedesktop.ScreenSaver";
    /// 正常是幾毫秒；卡住的匯流排不要讓背景執行緒一直等（結束時最多等 2 秒）
    const CALL_TIMEOUT: Duration = Duration::from_secs(2);

    /// 工作階段匯流排上的 ScreenSaver：拿著的 cookie 屬於這條連線（連線關掉時系統自己收回）
    pub struct Backend {
        /// None = 使用者的工作階段匯流排；測試是自己開的 dbus-daemon
        address: Option<String>,
        conn: Option<Connection>,
        cookie: Option<u32>,
    }

    impl Backend {
        pub fn new() -> Self {
            Self {
                address: None,
                conn: None,
                cookie: None,
            }
        }

        pub fn at(address: &str) -> Self {
            Self {
                address: Some(address.to_owned()),
                conn: None,
                cookie: None,
            }
        }

        /// 連線（第一次要、或斷線之後才連）
        fn conn(&mut self) -> Result<&Connection, Failure> {
            if self.conn.as_ref().is_some_and(Connection::is_closed) {
                self.conn = None;
                self.cookie = None;
            }
            if self.conn.is_none() {
                let builder = match &self.address {
                    Some(a) => Builder::address(a.as_str()),
                    None => Builder::session(),
                };
                let conn = builder
                    .and_then(|b| b.method_timeout(CALL_TIMEOUT).build())
                    .map_err(|e| Failure::Missing(format!("D-Bus 的工作階段匯流排（{e}）")))?;
                self.conn = Some(conn);
            }
            Ok(self.conn.as_ref().expect("剛連上"))
        }
    }

    fn failure(e: zbus::Error) -> Failure {
        match e {
            zbus::Error::FDO(e)
                if matches!(
                    *e,
                    zbus::fdo::Error::ServiceUnknown(_)
                        | zbus::fdo::Error::NameHasNoOwner(_)
                        | zbus::fdo::Error::UnknownObject(_)
                        | zbus::fdo::Error::UnknownInterface(_)
                        | zbus::fdo::Error::UnknownMethod(_)
                ) =>
            {
                Failure::Missing(DEST.to_owned())
            }
            zbus::Error::MethodError(name, _, _)
                if matches!(
                    name.as_str(),
                    "org.freedesktop.DBus.Error.ServiceUnknown"
                        | "org.freedesktop.DBus.Error.NameHasNoOwner"
                        | "org.freedesktop.DBus.Error.UnknownObject"
                        | "org.freedesktop.DBus.Error.UnknownInterface"
                        | "org.freedesktop.DBus.Error.UnknownMethod"
                ) =>
            {
                Failure::Missing(DEST.to_owned())
            }
            e => Failure::Failed(format!("{DEST}：{e}")),
        }
    }

    impl super::Backend for Backend {
        fn apply(&mut self, want: Want, label: Label) -> Result<(), Failure> {
            // 這個介面只管螢幕：只有聲音時不要求
            let display = want == Want::Display;
            if display {
                if self.cookie.is_some() && self.conn.as_ref().is_some_and(|c| !c.is_closed()) {
                    return Ok(());
                }
                let conn = self.conn()?;
                let cookie = conn
                    .call_method(Some(DEST), PATH, Some(IFACE), "Inhibit", &(label.app, label.reason))
                    .map_err(failure)?
                    .body()
                    .deserialize::<u32>()
                    .map_err(|e| Failure::Failed(format!("{DEST}：{e}")))?;
                self.cookie = Some(cookie);
            } else if let Some(cookie) = self.cookie.take() {
                let released = match self.conn.as_ref().filter(|c| !c.is_closed()) {
                    Some(conn) => conn
                        .call_method(Some(DEST), PATH, Some(IFACE), "UnInhibit", &(cookie,))
                        .map(drop),
                    None => Ok(()),
                };
                if let Err(e) = released {
                    // 放不掉（逾時、桌面回錯誤）：關掉這條連線，桌面看到我們離開匯流排就收回（cookie 屬於這條連線）；
                    // 不關的話螢幕會一直不關到影戲結束。下次要的時候重新連
                    if let Some(conn) = self.conn.take() {
                        let _ = conn.close();
                    }
                    return Err(failure(e));
                }
            }
            Ok(())
        }

        fn lost(&mut self) -> bool {
            if self.cookie.is_some() && self.conn.as_ref().is_none_or(Connection::is_closed) {
                self.cookie = None;
                self.conn = None;
                return true;
            }
            false
        }
    }

    impl Drop for Backend {
        fn drop(&mut self) {
            if let Some(conn) = self.conn.take() {
                if let Some(cookie) = self.cookie.take()
                    && !conn.is_closed()
                {
                    let _ = conn.call_method(Some(DEST), PATH, Some(IFACE), "UnInhibit", &(cookie,));
                }
                let _ = conn.close();
            }
        }
    }
}

/// 其他系統：沒有做法
#[cfg(not(any(windows, unix)))]
mod platform {
    use super::{Failure, Label, Want};

    pub struct Backend;

    impl Backend {
        pub fn new() -> Self {
            Self
        }
    }

    impl super::Backend for Backend {
        fn apply(&mut self, want: Want, _label: Label) -> Result<(), Failure> {
            match want {
                Want::Nothing => Ok(()),
                _ => Err(Failure::Missing("power management".into())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn stop_screensaver_follows_mpv() {
        assert_eq!(StopScreensaver::from_mpv("no"), StopScreensaver::No);
        assert_eq!(StopScreensaver::from_mpv("yes"), StopScreensaver::Yes);
        assert_eq!(StopScreensaver::from_mpv("always"), StopScreensaver::Always);
        // 讀不到、看不懂：當成 mpv 的預設 yes
        assert_eq!(StopScreensaver::from_mpv(""), StopScreensaver::Yes);
        assert_eq!(StopScreensaver::default(), StopScreensaver::Yes);
    }

    fn playing(loaded: bool, paused: bool, eof: bool, has_video: bool, ending: bool) -> Playing {
        Playing {
            loaded,
            paused,
            eof,
            has_video,
            ending,
        }
    }

    #[test]
    fn want_matrix() {
        use StopScreensaver::{Always, No, Yes};
        use Want::{Display, Nothing, System};
        let video = playing(true, false, false, true, false);
        let audio = playing(true, false, false, false, false);
        // 預設（yes）：播放中的影片 → 螢幕不關；只有聲音 → 電腦不睡眠
        assert_eq!(want(Yes, &video), Display);
        assert_eq!(want(Yes, &audio), System);
        // 暫停、播完、沒有檔案、播完後的動作：都不要
        for p in [
            Playing { paused: true, ..video },
            Playing { eof: true, ..video },
            Playing { loaded: false, ..video },
            Playing { ending: true, ..video },
            Playing { paused: true, ..audio },
            Playing { eof: true, ..audio },
            Playing { loaded: false, ..audio },
            Playing { ending: true, ..audio },
            Playing::default(),
        ] {
            assert_eq!(want(Yes, &p), Nothing, "{p:?}");
        }
        // no：什麼都不要
        for p in [video, audio, Playing { paused: true, ..video }] {
            assert_eq!(want(No, &p), Nothing, "{p:?}");
        }
        // always：有影片就一直不關（暫停、停在最後一格也是），聲音照 yes
        assert_eq!(want(Always, &video), Display);
        assert_eq!(want(Always, &Playing { paused: true, ..video }), Display);
        assert_eq!(want(Always, &Playing { eof: true, ..video }), Display);
        assert_eq!(want(Always, &audio), System);
        assert_eq!(want(Always, &Playing { paused: true, ..audio }), Nothing);
        // 但沒有檔案、播完後要睡眠 / 關機時一定放掉
        assert_eq!(want(Always, &Playing { loaded: false, ..video }), Nothing);
        assert_eq!(want(Always, &Playing { ending: true, ..video }), Nothing);
        assert_eq!(
            want(
                Always,
                &Playing {
                    ending: true,
                    paused: true,
                    ..video
                }
            ),
            Nothing
        );
    }

    /// 窮舉：播完後的動作、沒有檔案、no 一定是 Nothing；Display 只給有影片的；System 只給播放中、沒有影片的
    #[test]
    fn want_is_never_held_through_an_ending() {
        for mode in [StopScreensaver::No, StopScreensaver::Yes, StopScreensaver::Always] {
            for bits in 0..32u8 {
                let b = |i: u8| bits & (1 << i) != 0;
                let p = playing(b(0), b(1), b(2), b(3), b(4));
                let w = want(mode, &p);
                if p.ending || !p.loaded || mode == StopScreensaver::No {
                    assert_eq!(w, Want::Nothing, "{mode:?} {p:?}");
                }
                if w == Want::Display {
                    assert!(p.has_video, "{mode:?} {p:?}");
                }
                if w == Want::System {
                    assert!(!p.has_video && !p.paused && !p.eof, "{mode:?} {p:?}");
                }
                if mode != StopScreensaver::Always && (p.paused || p.eof) {
                    assert_eq!(w, Want::Nothing, "{mode:?} {p:?}");
                }
            }
        }
    }

    #[test]
    fn display_keeps_the_system_awake_too() {
        assert_eq!(kinds(Want::Nothing), (false, false));
        assert_eq!(kinds(Want::Display), (true, true));
        assert_eq!(kinds(Want::System), (false, true));
    }

    // ───────────── 背景執行緒（假的系統） ─────────────

    #[derive(Clone, Default)]
    struct Fake {
        applied: Arc<Mutex<Vec<(Want, Label)>>>,
        /// apply 被呼叫了幾次（擋著之前就算）：測試確定背景已經在做某個要求，不用猜它多久會開始
        entered: Arc<(Mutex<usize>, Condvar)>,
        /// true = apply 先擋著，直到改回 false
        held: Arc<(Mutex<bool>, Condvar)>,
        fail: Arc<Mutex<Option<Failure>>>,
        lost: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl Fake {
        fn applied(&self) -> Vec<Want> {
            self.applied.lock().unwrap().iter().map(|(w, _)| *w).collect()
        }

        fn hold(&self, on: bool) {
            *self.held.0.lock().unwrap() = on;
            self.held.1.notify_all();
        }

        /// 等背景進到第 `n` 個 apply（可能還擋著）
        fn wait_entered(&self, n: usize) {
            let (count, changed) = &*self.entered;
            let (count, timeout) = changed
                .wait_timeout_while(count.lock().unwrap(), Duration::from_secs(10), |c| *c < n)
                .unwrap();
            assert!(!timeout.timed_out(), "背景沒有開始做：進了 {} 次", *count);
        }

        fn wait_applied(&self, n: usize) {
            let start = std::time::Instant::now();
            while self.applied.lock().unwrap().len() < n {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "背景沒有做：{:?}",
                    self.applied()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Backend for Fake {
        fn apply(&mut self, want: Want, label: Label) -> Result<(), Failure> {
            *self.entered.0.lock().unwrap() += 1;
            self.entered.1.notify_all();
            let mut held = self.held.0.lock().unwrap();
            while *held {
                held = self.held.1.wait(held).unwrap();
            }
            drop(held);
            self.applied.lock().unwrap().push((want, label));
            match self.fail.lock().unwrap().clone() {
                Some(e) if want != Want::Nothing => Err(e),
                _ => Ok(()),
            }
        }

        fn lost(&mut self) -> bool {
            self.lost.swap(false, Ordering::SeqCst)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    fn real(fake: &Fake) -> RealInhibitor {
        RealInhibitor::with_backend(fake.clone())
    }

    #[test]
    fn requests_reach_the_system_and_settle_waits_for_them() {
        let fake = Fake::default();
        let inh = real(&fake);
        inh.set(Want::Display);
        assert!(inh.settle(Duration::from_secs(10)));
        assert_eq!(fake.applied(), [Want::Display]);
        assert_eq!(inh.held(), Want::Display);
        let label = fake.applied.lock().unwrap()[0].1;
        assert!(!label.reason.is_empty(), "系統顯示的原因");
        assert_eq!(label.app, crate::app::app_name(), "系統顯示的名稱跟視窗、媒體控制一樣");
        inh.set(Want::System);
        assert!(inh.settle(Duration::from_secs(10)));
        assert_eq!(inh.held(), Want::System);
        inh.set(Want::Nothing);
        assert!(inh.settle(Duration::from_secs(10)));
        assert_eq!(fake.applied(), [Want::Display, Want::System, Want::Nothing]);
        assert_eq!(inh.held(), Want::Nothing);
    }

    #[test]
    fn settle_times_out_while_the_system_is_slow_and_only_the_latest_is_applied() {
        let fake = Fake::default();
        let inh = real(&fake);
        fake.hold(true);
        inh.set(Want::Display);
        // 背景卡在第一個要求裡（確定進去了，不是猜它多久會開始）：後面排著的只做最後一個
        fake.wait_entered(1);
        inh.set(Want::System);
        inh.set(Want::Display);
        inh.set(Want::Nothing);
        assert!(!inh.settle(Duration::from_millis(100)), "還沒做完");
        fake.hold(false);
        assert!(inh.settle(Duration::from_secs(10)));
        assert_eq!(fake.applied(), [Want::Display, Want::Nothing]);
        assert_eq!(inh.held(), Want::Nothing);
    }

    #[test]
    fn dropping_releases_and_stops_the_worker() {
        let fake = Fake::default();
        let inh = real(&fake);
        inh.set(Want::Display);
        assert!(inh.settle(Duration::from_secs(10)));
        // 這之前建立 / 複製時丟掉的不算
        fake.dropped.store(false, Ordering::SeqCst);
        drop(inh);
        assert_eq!(fake.applied(), [Want::Display, Want::Nothing], "結束時放掉");
        assert!(fake.dropped.load(Ordering::SeqCst), "背景執行緒結束、系統那一端關掉");
    }

    #[test]
    fn a_missing_service_is_logged_once_and_nothing_is_held() {
        let fake = Fake::default();
        *fake.fail.lock().unwrap() = Some(Failure::Missing("org.freedesktop.ScreenSaver".into()));
        let inh = real(&fake);
        for want in [Want::Display, Want::Nothing, Want::Display] {
            inh.set(want);
            assert!(inh.settle(Duration::from_secs(10)));
        }
        assert_eq!(
            fake.applied(),
            [Want::Display, Want::Nothing, Want::Display],
            "每次都再試"
        );
        assert_eq!(inh.held(), Want::Nothing);
        assert_eq!(inh.failures().len(), 1, "只記錄一次");
    }

    #[test]
    fn a_lost_request_is_made_again() {
        let fake = Fake::default();
        let inh = real(&fake);
        inh.set(Want::Display);
        assert!(inh.settle(Duration::from_secs(10)));
        fake.lost.store(true, Ordering::SeqCst);
        fake.wait_applied(2);
        assert_eq!(fake.applied(), [Want::Display, Want::Display]);
        // 不要的時候被收回：不用再要
        inh.set(Want::Nothing);
        assert!(inh.settle(Duration::from_secs(10)));
        fake.lost.store(true, Ordering::SeqCst);
        std::thread::sleep(WATCH_EVERY * 2);
        assert_eq!(fake.applied(), [Want::Display, Want::Display, Want::Nothing]);
    }

    /// 真的向系統要求的 `RealInhibitor` 只有 main.rs 建立：其他程式碼、任何測試都不能建立它
    /// （Linux 的測試用 `on_bus` 連到自己開的 dbus-daemon）
    #[test]
    fn real_inhibitor_is_only_created_by_main() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let created = ["RealInhibitor", "::new("].concat();
        let mut creators = Vec::new();
        let mut stack = vec![
            root.join("src"),
            root.join("tests"),
            root.join("benches"),
            root.join("examples"),
        ];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && std::fs::read_to_string(&path).unwrap().contains(&created)
                {
                    let name = path.strip_prefix(root).unwrap().to_string_lossy();
                    creators.push(name.replace(std::path::MAIN_SEPARATOR, "/"));
                }
            }
        }
        assert_eq!(creators, ["src/main.rs"], "只有 main.rs 可以建立 RealInhibitor");
        // 而且只在不是自動截圖（`--shot`，開發、CI 用）的時候：在 `if launch.autoshot.is_none() {` 的區塊裡
        let main = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
        let at = main.find(&created).unwrap();
        let block = ["if launch.autoshot", ".is_none() {"].concat();
        let open = main[..at].rfind(&block).expect("main.rs 沒有自動截圖的判斷") + block.len();
        let mut depth = 1;
        for c in main[open..at].chars() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            assert!(
                depth > 0,
                "RealInhibitor 要在 main.rs 的 `{block}` 區塊裡建立（自動截圖不要求螢幕不關）"
            );
        }
    }

    /// 真的向系統要求的測試只在 CI 上跑（CI 的機器沒有人在用）：開發機上跳過，不碰使用者電腦的電源。
    /// 在開發機上手動跑：`CI=1 cargo test --lib inhibit::tests::`（只拿著幾毫秒）
    #[cfg(any(windows, target_os = "macos"))]
    fn real_system_tests() -> bool {
        let on = std::env::var_os("CI").is_some();
        if !on {
            eprintln!("跳過：真的向系統要求螢幕不關的測試只在 CI 上跑（手動：CI=1 cargo test --lib inhibit::tests::）");
        }
        on
    }

    #[cfg(any(windows, target_os = "macos"))]
    const TEST_LABEL: Label = Label {
        app: "VitaScope",
        reason: "VitaScope test",
    };

    /// Windows 的電源要求真的設得上、清得掉（只在 CI 上跑；只拿著幾毫秒；出錯時 Drop 也會關掉 handle，程式結束時系統也會收回）
    #[cfg(windows)]
    #[test]
    fn windows_power_request_is_set_and_cleared() {
        if !real_system_tests() {
            return;
        }
        use windows_sys::Win32::System::Power::{
            CallNtPowerInformation, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SystemExecutionState,
        };
        fn state() -> u32 {
            let mut out = 0u32;
            // SAFETY: 輸出是一個 ULONG
            let status = unsafe {
                CallNtPowerInformation(
                    SystemExecutionState,
                    std::ptr::null(),
                    0,
                    (&mut out as *mut u32).cast(),
                    4,
                )
            };
            assert_eq!(status, 0, "CallNtPowerInformation");
            out
        }
        let both = ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED;
        let before = state() & both;
        let mut b = platform::Backend::new();
        // 每一步自己設了、清了哪幾種（PowerSetRequest / PowerClearRequest 都成功才記下；不受別的程式影響）
        b.apply(Want::Display, TEST_LABEL).unwrap();
        assert_eq!((b.display, b.system), (true, true));
        let held = state();
        b.apply(Want::System, TEST_LABEL).unwrap();
        assert_eq!((b.display, b.system), (false, true));
        let system = state();
        b.apply(Want::Nothing, TEST_LABEL).unwrap();
        assert_eq!((b.display, b.system), (false, false));
        let after = state() & both;
        drop(b);
        // 系統的狀態合併了所有程式的要求：確認我們要的那幾種在（別的程式可能也要了）
        assert_ne!(held & ES_DISPLAY_REQUIRED, 0, "螢幕不關：{held:#x}");
        assert_ne!(held & ES_SYSTEM_REQUIRED, 0, "電腦不睡眠：{held:#x}");
        assert_ne!(system & ES_SYSTEM_REQUIRED, 0, "只有聲音：電腦不睡眠 {system:#x}");
        // 本來沒有別的程式要求時（CI、沒在播東西的電腦）：放掉之後回到沒有；只有聲音時螢幕已經放掉
        if before == 0 {
            assert_eq!(system & ES_DISPLAY_REQUIRED, 0, "只有聲音：螢幕放掉 {system:#x}");
            assert_eq!(after, 0, "放掉：{after:#x}");
        } else {
            eprintln!("別的程式也在要求（{before:#x}）：系統狀態回到沒有的檢查跳過，只看自己設了、清了哪幾種");
        }
    }

    /// macOS 的 assertion 拿得到、放得掉（只在 CI 上跑）：用 `pmset -g assertions` 看這個程序拿著哪幾種
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_assertions_are_taken_and_released() {
        if !real_system_tests() {
            return;
        }
        // 每一行是一個 assertion：「pid 123(名稱): [id] 時間 種類 named: "原因"」。原因加上 pid，不會認到別的程序
        let reason: &'static str = Box::leak(format!("VitaScope test {}", std::process::id()).into_boxed_str());
        let label = Label { reason, ..TEST_LABEL };
        let named = format!("\"{reason}\"");
        let holding = || {
            let out = std::process::Command::new("pmset")
                .args(["-g", "assertions"])
                .output()
                .expect("pmset");
            let text = String::from_utf8_lossy(&out.stdout).into_owned();
            let ours = |kind: &str| text.lines().any(|l| l.contains(&named) && l.contains(kind));
            (ours(platform::DISPLAY), ours(platform::SYSTEM), text)
        };
        // 系統那一端可能晚一點才看到（慢的 CI）：等到對為止
        let expect = |display: bool, system: bool, what: &str| {
            let start = std::time::Instant::now();
            loop {
                let (d, s, text) = holding();
                if (d, s) == (display, system) {
                    return;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(15),
                    "{what}：螢幕 {d}、電腦 {s}（要 {display}、{system}）\n{text}"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        let mut b = platform::Backend::new();
        b.apply(Want::Display, label).unwrap();
        expect(true, true, "影片：螢幕不關、電腦不睡眠");
        b.apply(Want::System, label).unwrap();
        expect(false, true, "只有聲音：螢幕放掉、電腦不睡眠");
        b.apply(Want::Nothing, label).unwrap();
        expect(false, false, "都放掉");
        b.apply(Want::Display, label).unwrap();
        expect(true, true, "再要一次");
        drop(b);
        expect(false, false, "結束時放掉");
    }
}
