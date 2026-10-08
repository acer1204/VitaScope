//! 流暢播放的接線：查視窗所在螢幕的更新率、電源，用 `crate::pacing::decide` 算出要怎麼做，
//! 再套用到 mpv（`display-fps-override` + `video-sync`），播放中量畫面更新的間隔防呆。
//! 預設關閉；做法沒變就不送任何設定，所以沒打開的使用者 mpv 的選項完全不動。

use super::VitascopeApp;
use crate::pacing::{
    self as rules, Guard, Inputs, Overrides, Plan, Presents, Reason, RenderStats, SmoothMode, Verdict,
};
use crate::player::AsyncKey;
use crate::power::{self, PowerSource};
use crate::screens::{self, Refresh};
use eframe::egui;
use raw_window_handle::RawWindowHandle;
use std::time::{Duration, Instant};

/// 多久看一次視窗在哪個螢幕上（Windows 約 1 µs）
const KEY_POLL: Duration = Duration::from_millis(500);
/// 視窗移動、改大小停下來這麼久之後重查更新率
const RECT_SETTLE: Duration = Duration::from_millis(300);
/// 沒有任何變化也每隔這麼久重查一次（在系統設定改了更新率）
const REQUERY: Duration = Duration::from_secs(5);
/// 多久查一次電源、遠端桌面
const POWER_POLL: Duration = Duration::from_secs(10);
/// 改成依螢幕同步之前，做法要先維持這麼久不變（拖曳到別的螢幕、開檔的空檔不會來回切換）；
/// 改回一般播放馬上做
const DEBOUNCE: Duration = Duration::from_millis(500);
/// 視窗看不到時多久醒來一次，logic() 才會繼續跑（eframe 會再放慢到 100 ms 以上）
const HIDDEN_TICK: Duration = Duration::from_millis(40);
/// 設定頁上的「每格幾次更新」多久讀一次
const NUMBERS_EVERY: Duration = Duration::from_secs(1);
/// VITASCOPE_DEBUG=pacing：畫面輸出的統計多久印一次
const RENDER_LOG_EVERY: Duration = Duration::from_secs(10);

/// 查螢幕、電源的方法（自動測試換成假的）
pub trait PlatformProbe {
    /// 視窗所在螢幕的更新率
    fn refresh_rate(&self) -> Option<Refresh>;
    /// 視窗在哪個螢幕上（只用來比較有沒有換螢幕）
    fn monitor_key(&self) -> Option<u64>;
    fn power(&self) -> PowerSource;
    /// 遠端桌面連線中
    fn remote_session(&self) -> bool;
}

/// 問作業系統（主視窗的 handle）
pub(super) struct RealProbe {
    window: RawWindowHandle,
    /// X11：自己的連線，留著重複用
    #[cfg(all(unix, not(target_os = "macos")))]
    x11: screens::X11Probe,
}

impl RealProbe {
    pub(super) fn new(window: RawWindowHandle) -> Self {
        Self {
            window,
            #[cfg(all(unix, not(target_os = "macos")))]
            x11: screens::X11Probe::default(),
        }
    }
}

impl PlatformProbe for RealProbe {
    fn refresh_rate(&self) -> Option<Refresh> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.x11.refresh_rate(self.window)
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            screens::refresh_rate(self.window)
        }
    }

    fn monitor_key(&self) -> Option<u64> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            self.x11.monitor_key(self.window)
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            screens::monitor_key(self.window)
        }
    }

    fn power(&self) -> PowerSource {
        power::source()
    }

    fn remote_session(&self) -> bool {
        screens::remote_session()
    }
}

/// mpv 回報的顯示同步數字（設定頁、媒體資訊面板打開時才讀，不觀察）
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SyncNumbers {
    /// 每格影像顯示幾次螢幕更新（vsync-ratio）
    pub vsync_ratio: Option<f64>,
    /// 影片速度的修正倍數（video-speed-correction，1.001 = 快 0.1%）
    pub speed_correction: Option<f64>,
}

/// 流暢播放目前的狀態（媒體資訊面板、設定頁、右鍵選單顯示）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PacingStatus {
    /// 算出來的做法；None = 還沒算過（視窗還沒出現）
    pub plan: Option<Plan>,
    pub refresh: Option<Refresh>,
    pub power: PowerSource,
    /// 做法已經套用到 mpv（依螢幕同步要先維持 0.5 秒不變才套用）
    pub applied: bool,
    /// mpv 正在依螢幕同步
    pub sync_active: bool,
    /// 有檔案載入中（沒有檔案時不說「mpv 判斷不適用」）
    pub loaded: bool,
    /// VITASCOPE_PACING=off（不是 VITASCOPE_MPV_OPTS）讓這個功能不動作
    pub pacing_off: bool,
    pub numbers: SyncNumbers,
}

impl PacingStatus {
    /// 一行說明，例如「使用中：120.000 Hz（每格 5 次更新，影片快 0.10%）」「未使用：使用電池中」
    pub fn describe(&self) -> String {
        match self.plan {
            None => crate::tr!("計算中…", "Checking…").to_owned(),
            Some(Plan::Untouched) if self.pacing_off => crate::tr!(
                "未使用：已由 VITASCOPE_PACING=off 關閉",
                "Not in use: turned off by VITASCOPE_PACING=off"
            )
            .to_owned(),
            Some(Plan::Untouched) => crate::tr!("已由 VITASCOPE_MPV_OPTS 指定", "Set by VITASCOPE_MPV_OPTS").to_owned(),
            Some(Plan::Audio(reason)) => crate::tf!("未使用：{}", "Not in use: {}", reason_text(reason)),
            Some(Plan::Display { hz, .. }) if !self.applied => {
                crate::tf!("準備中：{hz:.3} Hz", "Starting: {hz:.3} Hz")
            }
            Some(Plan::Display { vdrop: true, .. }) => crate::tr!(
                "音訊直通中：以略過或重複影格對齊螢幕",
                "Audio passthrough: matching the screen by dropping or repeating frames"
            )
            .to_owned(),
            Some(Plan::Display { .. }) if self.loaded && !self.sync_active => crate::tr!(
                "已開啟，但 mpv 判斷這部影片不適用",
                "On, but mpv decided this video isn't suitable"
            )
            .to_owned(),
            Some(Plan::Display { hz, .. }) => match sync_detail(&self.numbers).filter(|_| self.sync_active) {
                Some(detail) => crate::tf!("使用中：{hz:.3} Hz（{detail}）", "In use: {hz:.3} Hz ({detail})"),
                None => crate::tf!("使用中：{hz:.3} Hz", "In use: {hz:.3} Hz"),
            },
        }
    }

    /// 媒體資訊面板「播放流暢度」的幾行
    pub fn info_lines(&self) -> Vec<String> {
        let refresh = match self.refresh {
            Some(r) => crate::tf!(
                "螢幕更新率：{:.3} Hz（{}）",
                "Refresh rate: {:.3} Hz ({})",
                r.hz,
                r.source.label()
            ),
            None => crate::tr!("螢幕更新率：偵測不到", "Refresh rate: not detected").to_owned(),
        };
        vec![
            refresh,
            crate::tf!("電源：{}", "Power: {}", self.power.label()),
            crate::tf!("流暢播放：{}", "Smooth playback: {}", self.describe()),
        ]
    }
}

/// 「每格 5 次更新，影片快 0.10%」；mpv 沒回報就沒有
fn sync_detail(n: &SyncNumbers) -> Option<String> {
    let (ratio, speed) = (n.vsync_ratio?, n.speed_correction?);
    let ratio = if (ratio - ratio.round()).abs() < 0.01 {
        format!("{:.0}", ratio.round())
    } else {
        format!("{ratio:.3}")
    };
    let pct = (speed - 1.0) * 100.0;
    let speed = if pct.abs() < 0.005 {
        crate::tr!("影片速度不變", "speed unchanged").to_owned()
    } else if pct > 0.0 {
        crate::tf!("影片快 {pct:.2}%", "video {pct:.2}% faster")
    } else {
        crate::tf!("影片慢 {:.2}%", "video {:.2}% slower", -pct)
    };
    Some(crate::tf!(
        "每格 {ratio} 次更新，{speed}",
        "{ratio} refreshes per frame, {speed}"
    ))
}

/// 不用流暢播放的原因
fn reason_text(reason: Reason) -> &'static str {
    match reason {
        Reason::Setting => crate::tr!("設定為關閉", "turned off in Settings"),
        Reason::NoVideo => crate::tr!("沒有影像", "no video"),
        Reason::Battery => crate::tr!("使用電池中", "on battery power"),
        Reason::SoftwareRenderer => crate::tr!("軟體繪圖", "software rendering"),
        Reason::RemoteSession => crate::tr!("遠端桌面連線", "remote desktop session"),
        Reason::ApplyFailed => crate::tr!("mpv 無法套用設定", "mpv rejected the settings"),
        Reason::NoVsync => crate::tr!(
            "顯示卡沒有等待垂直同步（G-SYNC／FreeSync 或關閉了垂直同步）",
            "the graphics driver isn't waiting for vsync (G-SYNC/FreeSync, or vsync turned off)"
        ),
        Reason::TooSlow => crate::tr!(
            "畫面更新跟不上，這部影片改用一般播放",
            "rendering can't keep up, so this video uses normal playback"
        ),
        Reason::Hidden => crate::tr!("視窗縮到最小", "the window is minimized"),
        Reason::NoRefresh => crate::tr!("偵測不到這個螢幕的更新率", "can't detect this screen's refresh rate"),
    }
}

/// 右鍵選單「流暢播放」括號裡的短說明：「119.88 Hz」「使用電池，暫停」「偵測不到更新率」…
fn short_text(plan: &Plan, refresh: Option<f64>, pacing_off: bool) -> Option<String> {
    let hz = || refresh.map(|hz| format!("{} Hz", rules::fmt_hz(hz)));
    let text = match plan {
        Plan::Untouched if pacing_off => "VITASCOPE_PACING=off",
        Plan::Untouched => crate::tr!("已由 VITASCOPE_MPV_OPTS 指定", "set by VITASCOPE_MPV_OPTS"),
        Plan::Display { hz, .. } => return Some(format!("{} Hz", rules::fmt_hz(*hz))),
        // 跟這個視窗、這個檔案有關的暫時狀態：寫更新率就好
        Plan::Audio(Reason::Setting | Reason::NoVideo | Reason::Hidden) => return hz(),
        Plan::Audio(Reason::Battery) => crate::tr!("使用電池，暫停", "paused on battery"),
        Plan::Audio(Reason::SoftwareRenderer) => crate::tr!("軟體繪圖，不能使用", "not with software rendering"),
        Plan::Audio(Reason::RemoteSession) => crate::tr!("遠端桌面，暫停", "paused in a remote session"),
        Plan::Audio(Reason::ApplyFailed) => crate::tr!("mpv 無法套用", "mpv rejected it"),
        Plan::Audio(Reason::NoVsync) => crate::tr!("顯示卡沒有等待垂直同步", "no vsync from the graphics driver"),
        Plan::Audio(Reason::TooSlow) => crate::tr!("這部影片跟不上，暫停", "paused: can't keep up with this video"),
        Plan::Audio(Reason::NoRefresh) => crate::tr!("偵測不到更新率", "refresh rate not detected"),
    };
    Some(text.to_owned())
}

/// 播放中自動切換時的提示（從選單、設定頁切換的提示由那邊自己顯示）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Notice {
    /// 拔掉電源：暫停流暢播放
    BatteryPause,
    /// 接上電源：恢復
    AcResume,
    NoVsync,
    TooSlow,
}

impl Notice {
    fn text(self) -> &'static str {
        match self {
            Notice::BatteryPause => crate::tr!(
                "使用電池：流暢播放暫停（省電）",
                "On battery: smooth playback paused to save power"
            ),
            Notice::AcResume => crate::tr!("接上電源：流暢播放恢復", "Plugged in: smooth playback resumed"),
            Notice::NoVsync => crate::tr!(
                "顯示卡沒有等待垂直同步，改用一般播放",
                "The graphics driver isn't waiting for vsync; using normal playback"
            ),
            Notice::TooSlow => crate::tr!(
                "畫面更新跟不上螢幕，這部影片改用一般播放",
                "Rendering can't keep up with the screen; this video uses normal playback"
            ),
        }
    }
}

/// 這一幀要做的事
#[derive(Debug, Default, PartialEq)]
pub(super) struct Tick {
    /// 依序要（非同步）設定的 mpv 選項；開啟時先設更新率，關閉時先換回音訊同步
    pub send: Vec<(&'static str, String)>,
    pub notice: Option<Notice>,
}

/// 這一幀視窗的狀態
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct View {
    pub visible: bool,
    pub outer_rect: Option<egui::Rect>,
    pub fullscreen: Option<bool>,
    pub monitor_size: Option<egui::Vec2>,
    /// egui 的幀編號（同一幀只量一次）
    pub frame_nr: u64,
}

/// 這一幀播放器的狀態
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Snapshot {
    pub mode: SmoothMode,
    pub dumb: bool,
    pub has_video: Option<bool>,
    pub passthrough: bool,
    pub file_gen: u64,
    /// 有檔案載入中
    pub loaded: bool,
    /// 正在播放（有檔案、沒暫停、不是逐格）
    pub playing: bool,
    pub display_sync_active: bool,
    /// 真的在畫影片（有 GL 的畫面）：每一幀之間的間隔才是 swap 等垂直同步的結果。
    /// 自動測試（kittest）沒有畫面，幀的間隔是測試自己決定的，不能拿來防呆
    pub real_window: bool,
}

/// 流暢播放的控制：記住查到的螢幕、電源，算出做法、套用到 mpv
pub(super) struct PacingCtl {
    probe: Option<Box<dyn PlatformProbe>>,
    /// 使用者自己指定了 video-sync / display-fps-override，或 VITASCOPE_PACING=off
    env_override: bool,
    refresh: Option<Refresh>,
    monitor_key: Option<u64>,
    power: PowerSource,
    /// 電源（接上、拔掉）變了，還沒套用到做法上：只有這時才提示「使用電池」「接上電源」，
    /// 在設定頁改「使用電池時暫停」造成的切換不算
    power_changed: bool,
    remote: bool,
    guard: Guard,
    /// mpv 不接受設定：這次執行不再試（改設定時重來）
    apply_failed: bool,
    status: PacingStatus,
    /// 目前套用到 mpv 的做法。一開始是 mpv 的預設（音訊同步、沒有指定更新率），
    /// 做法一直是一般播放就什麼都不送
    applied: Option<Plan>,
    /// 等著套用的依螢幕同步做法，從什麼時候開始一直是它
    pending: Option<(Plan, Instant)>,
    /// 送出了幾個設定（自動測試用：預設關閉時要是 0）
    sets: usize,
    /// 上一幀的條件（選單上預覽「打開的話會怎樣」用）
    last_inputs: Option<Inputs>,
    /// 上一次讀 mpv 顯示同步數字的時間
    numbers_read: Option<Instant>,
    /// 上一次查的時間；None = 還沒查過
    key_polled: Option<Instant>,
    refresh_queried: Option<Instant>,
    power_polled: Option<Instant>,
    rect: Option<egui::Rect>,
    /// 視窗最後一次移動、改大小的時間（停下來之後重查更新率）
    rect_moved: Option<Instant>,
    fullscreen: Option<bool>,
    monitor_size: Option<egui::Vec2>,
    /// 上一幀防呆有在量（依螢幕同步、播放中、看得到）
    sampling: bool,
    render_log: RenderLog,
}

/// VITASCOPE_DEBUG=pacing：每 10 秒印一次畫面輸出的統計
#[derive(Default)]
struct RenderLog {
    enabled: bool,
    /// 第一格畫出來的時間
    first: Option<Instant>,
    /// 這一段從什麼時候開始（第一格畫出來 1 秒之後才開始算）
    since: Option<Instant>,
    /// 上一段結束時的累計數字
    prev: RenderStats,
    /// mpv 的 avsync（每秒讀一次，秒）
    avsync: Vec<f64>,
    avsync_read: Option<Instant>,
}

impl PacingCtl {
    pub(super) fn new(probe: Option<Box<dyn PlatformProbe>>, user_set: bool, overrides: Overrides) -> Self {
        Self {
            probe,
            env_override: user_set || overrides.off,
            refresh: None,
            monitor_key: None,
            power: PowerSource::Unknown,
            power_changed: false,
            remote: false,
            guard: Guard::default(),
            apply_failed: false,
            status: PacingStatus {
                pacing_off: overrides.off,
                ..Default::default()
            },
            applied: Some(Plan::Audio(Reason::Setting)),
            pending: None,
            sets: 0,
            last_inputs: None,
            numbers_read: None,
            key_polled: None,
            refresh_queried: None,
            power_polled: None,
            rect: None,
            rect_moved: None,
            fullscreen: None,
            monitor_size: None,
            sampling: false,
            render_log: RenderLog {
                enabled: rules::debug(),
                ..Default::default()
            },
        }
    }

    pub(super) fn status(&self) -> &PacingStatus {
        &self.status
    }

    /// 開了新的檔案（「跟不上」只算那個檔案）
    pub(super) fn start_file(&mut self, file_gen: u64) {
        self.guard.start_file(file_gen);
    }

    /// 跳轉：重新開始量
    pub(super) fn seeked(&mut self) {
        self.guard.reset();
    }

    /// 使用者改了設定：之前量到的「沒等垂直同步」「mpv 不接受」都不算，重新來過
    pub(super) fn setting_changed(&mut self) {
        self.guard.clear_verdicts();
        self.apply_failed = false;
        self.power_changed = false;
    }

    /// mpv 回覆 video-sync / display-fps-override 設定失敗：這次執行改用一般播放
    pub(super) fn apply_failed(&mut self) {
        if !self.apply_failed {
            eprintln!("[vitascope] 流暢播放：mpv 不接受設定，改用一般播放");
        }
        self.apply_failed = true;
    }

    /// 下一幀重查螢幕、更新率、電源（自動測試改了假的平台資訊之後用）
    pub(super) fn requery(&mut self) {
        self.key_polled = None;
        self.refresh_queried = None;
        self.power_polled = None;
    }

    /// 啟動時（還沒開檔）就決定：已經要依螢幕同步的話回傳要同步設定的選項，第一個檔案一開始就同步。
    /// 一般播放什麼都不用設（mpv 的預設就是）
    pub(super) fn startup(&mut self, mode: SmoothMode, dumb: bool) -> Vec<(&'static str, String)> {
        if let Some(p) = &self.probe {
            self.monitor_key = p.monitor_key();
            self.refresh = p.refresh_rate();
            self.power = p.power();
            self.remote = p.remote_session();
        }
        let inputs = Inputs {
            mode,
            power: self.power,
            refresh: self.refresh.map(|r| r.hz),
            dumb,
            env_override: self.env_override,
            has_video: None,
            // 視窗還沒出現：當成看得到（出現之後的每一幀照實際的算）
            visible: true,
            passthrough: false,
            remote: self.remote,
            guard: self.guard.state(),
            file_gen: 0,
            apply_failed: false,
        };
        let plan = rules::decide(&inputs, None);
        if rules::debug() {
            eprintln!("[vitascope] 流暢播放（啟動）：{plan:?}，螢幕更新率 {:?}", self.refresh);
        }
        self.last_inputs = Some(inputs);
        self.status.plan = Some(plan);
        self.status.refresh = self.refresh;
        self.status.power = self.power;
        let send = rules::transition(self.applied.as_ref(), &plan);
        self.applied = Some(plan);
        self.status.applied = true;
        if !plan.is_display() {
            return Vec::new();
        }
        self.sets += send.len();
        send
    }

    /// 等著套用的做法還要等多久（沒有在等就是 None）
    pub(super) fn pending_wait(&self, now: Instant) -> Option<Duration> {
        let (_, since) = self.pending?;
        Some(DEBOUNCE.saturating_sub(now.saturating_duration_since(since)))
    }

    /// 送出了幾個設定（自動測試用）
    pub(super) fn sets(&self) -> usize {
        self.sets
    }

    /// 選單、提示上的短說明：設定改成 `mode` 的話，現在會怎樣（「119.88 Hz」「使用電池，暫停」…）
    pub(super) fn short_for(&self, mode: SmoothMode) -> Option<String> {
        // 改設定時「沒等垂直同步」「mpv 不接受」會重來，用現在的
        let inputs = Inputs {
            mode,
            guard: self.guard.state(),
            apply_failed: self.apply_failed,
            ..self.last_inputs?
        };
        let plan = rules::decide(&inputs, self.status.plan.as_ref());
        short_text(&plan, self.refresh.map(|r| r.hz), self.status.pacing_off)
    }

    /// 每一幀：依需要重查螢幕、電源，算出做法；回傳要送給 mpv 的設定與提示
    pub(super) fn tick(&mut self, now: Instant, view: &View, snap: &Snapshot) -> Tick {
        let due =
            |last: Option<Instant>, every: Duration| last.is_none_or(|t| now.saturating_duration_since(t) >= every);
        let first = self.key_polled.is_none();
        let mut requery = first;
        if let Some(probe) = &self.probe {
            // 換到別的螢幕
            if due(self.key_polled, KEY_POLL) {
                self.key_polled = Some(now);
                let key = probe.monitor_key();
                if key != self.monitor_key {
                    self.monitor_key = key;
                    requery = true;
                }
            }
        } else {
            self.key_polled = Some(now);
        }
        // 視窗移動、改大小：停下來之後重查；移動中量到的間隔不準（Windows 拖曳時介面會停住）
        if view.outer_rect != self.rect {
            if self.rect.is_some() {
                self.rect_moved = Some(now);
                self.guard.window_moved(now);
            }
            self.rect = view.outer_rect;
        }
        if self
            .rect_moved
            .is_some_and(|t| now.saturating_duration_since(t) >= RECT_SETTLE)
        {
            self.rect_moved = None;
            requery = true;
        }
        if view.fullscreen != self.fullscreen || view.monitor_size != self.monitor_size {
            self.fullscreen = view.fullscreen;
            self.monitor_size = view.monitor_size;
            requery = true;
        }
        if requery || due(self.refresh_queried, REQUERY) {
            self.refresh_queried = Some(now);
            let refresh = self.probe.as_ref().and_then(|p| p.refresh_rate());
            if refresh != self.refresh {
                if rules::debug() {
                    eprintln!(
                        "[vitascope] 流暢播放：螢幕更新率 {refresh:?}（螢幕 {:?}）",
                        self.monitor_key
                    );
                }
                self.refresh = refresh;
            }
        }
        if due(self.power_polled, POWER_POLL) {
            self.power_polled = Some(now);
            if let Some(p) = &self.probe {
                let (power, remote) = (p.power(), p.remote_session());
                if (power, remote) != (self.power, self.remote) && rules::debug() {
                    eprintln!("[vitascope] 流暢播放：電源 {power:?}、遠端桌面 {remote}");
                }
                if power != self.power {
                    self.power_changed = true;
                }
                self.power = power;
                self.remote = remote;
            }
        }
        // 換螢幕、換更新率：之前量到的結果不算
        if self.guard.set_display(self.monitor_key, self.refresh.map(|r| r.hz)) && rules::debug() {
            eprintln!("[vitascope] 流暢播放：換了螢幕或更新率，重新判斷");
        }
        let mut inputs = Inputs {
            mode: snap.mode,
            power: self.power,
            refresh: self.refresh.map(|r| r.hz),
            dumb: snap.dumb,
            env_override: self.env_override,
            has_video: snap.has_video,
            visible: view.visible,
            passthrough: snap.passthrough,
            remote: self.remote,
            guard: self.guard.state(),
            file_gen: snap.file_gen,
            apply_failed: self.apply_failed,
        };
        let previous = self.status.plan;
        let mut plan = rules::decide(&inputs, previous.as_ref());
        let mut tick = Tick::default();
        // 防呆只在真的依螢幕同步、每一幀都是一次等垂直同步的 swap 時量
        let sampling = plan.is_display()
            && self.applied == Some(plan)
            && snap.display_sync_active
            && snap.playing
            && snap.real_window
            && view.visible;
        // 開始或停止量（暫停、看不到、改回一般播放、mpv 不同步了）：之前量到一半的間隔與計時都不算，
        // 不然隔了一段時間回來，舊的間隔加上舊的起算時間會馬上判定
        if sampling != self.sampling {
            self.sampling = sampling;
            self.guard.reset();
        }
        if let Plan::Display { hz, .. } = plan
            && sampling
            && let Some(verdict) = self.guard.sample(now, view.frame_nr, 1.0 / hz)
        {
            eprintln!(
                "[vitascope] 流暢播放：{verdict:?}（{hz:.3} Hz，螢幕 {:?}）",
                self.monitor_key
            );
            tick.notice = match verdict {
                Verdict::NoVsync => Some(Notice::NoVsync),
                Verdict::TooSlow => Some(Notice::TooSlow),
                // 改用一半的更新率，播放照樣順，不用特別提示
                Verdict::HalfRate => None,
            };
            inputs.guard = self.guard.state();
            plan = rules::decide(&inputs, previous.as_ref());
        }
        if Some(plan) != previous && rules::debug() {
            eprintln!("[vitascope] 流暢播放：{plan:?}");
        }
        self.apply(now, plan, snap.playing, &mut tick);
        self.last_inputs = Some(inputs);
        self.status.plan = Some(plan);
        self.status.applied = rules::transition(self.applied.as_ref(), &plan).is_empty();
        self.status.refresh = self.refresh;
        self.status.power = self.power;
        self.status.sync_active = snap.display_sync_active;
        self.status.loaded = snap.loaded;
        tick
    }

    /// 跟目前套用的做法比：改回一般播放馬上送；改成依螢幕同步要先維持 0.5 秒不變
    fn apply(&mut self, now: Instant, plan: Plan, playing: bool, tick: &mut Tick) {
        let send = rules::transition(self.applied.as_ref(), &plan);
        if send.is_empty() {
            // 選項一樣（例如只是一般播放的原因不同）：記下新的原因就好
            self.applied = Some(plan);
            self.pending = None;
            self.power_changed = false;
            return;
        }
        if plan.is_display() {
            match self.pending {
                Some((p, since)) if p == plan => {
                    if now.saturating_duration_since(since) < DEBOUNCE {
                        return;
                    }
                }
                _ => {
                    self.pending = Some((plan, now));
                    return;
                }
            }
        }
        // 播放中真的因為接上、拔掉電源自動切換才提示（從選單改的由那邊提示；
        // 用電池時在設定頁改「使用電池時暫停」也會在 Audio(Battery) 與 Display 之間切換，但電源沒變）
        if playing && tick.notice.is_none() && self.power_changed {
            let was = self.applied;
            tick.notice = match (was, plan) {
                (Some(Plan::Display { .. }), Plan::Audio(Reason::Battery)) => Some(Notice::BatteryPause),
                (Some(Plan::Audio(Reason::Battery)), Plan::Display { .. }) => Some(Notice::AcResume),
                _ => None,
            };
        }
        if rules::debug() {
            eprintln!("[vitascope] 流暢播放：套用 {send:?}");
        }
        self.applied = Some(plan);
        self.pending = None;
        self.power_changed = false;
        self.sets += send.len();
        tick.send = send;
    }
}

impl VitascopeApp {
    /// 流暢播放的狀態（介面測試用）
    pub fn pacing_status(&self) -> &PacingStatus {
        self.pacing.status()
    }

    /// 防呆目前量的是第幾個檔案（介面測試用：開檔時要通知防呆）
    #[doc(hidden)]
    pub fn pacing_guard_file(&self) -> u64 {
        self.pacing.guard.file_gen()
    }

    /// 流暢播放送出了幾個 mpv 設定（介面測試用：預設關閉時一個都不能送）
    #[doc(hidden)]
    pub fn pacing_sets(&self) -> usize {
        self.pacing.sets()
    }

    /// 下一幀重查螢幕、電源（介面測試改了假的平台資訊之後用，不用等 10 秒）
    #[doc(hidden)]
    pub fn pacing_requery(&mut self) {
        self.pacing.requery();
    }

    /// 啟動時（還沒開檔）：已經要依螢幕同步的話同步設定好，第一個檔案一開始就同步
    pub(super) fn pacing_startup(&mut self) {
        let send = self.pacing.startup(self.settings.smooth, self.caps.dumb);
        let failed = self.player.apply_sync(&send);
        if !failed.is_empty() {
            for (name, e) in &failed {
                eprintln!("[vitascope] 無法套用 {name}：{e}");
            }
            // 已經設上去的部分下一幀改回一般播放
            self.pacing.apply_failed();
        }
    }

    /// 選單、設定頁切換流暢播放
    pub(super) fn set_smooth(&mut self, mode: SmoothMode) {
        if mode == self.settings.smooth {
            return;
        }
        self.settings.smooth = mode;
        self.pacing.setting_changed();
        self.save_settings();
    }

    /// 右鍵選單、提示上「流暢播放」括號裡的說明（設定是 `mode` 的話）
    pub(super) fn smooth_short(&self, mode: SmoothMode) -> Option<String> {
        self.pacing.short_for(mode)
    }

    /// 設定頁打開時讀 mpv 的顯示同步數字（每秒一次就夠；同步讀取要鎖住 mpv 的核心）
    pub(super) fn read_sync_numbers(&mut self) {
        if self.pacing.numbers_read.is_some_and(|t| t.elapsed() < NUMBERS_EVERY) {
            return;
        }
        let live = crate::mediainfo::read_live(&self.player);
        self.set_sync_numbers(&live);
    }

    /// 媒體資訊面板讀到的數字順便給「使用中」的說明用
    pub(super) fn set_sync_numbers(&mut self, live: &crate::mediainfo::LiveStats) {
        self.pacing.numbers_read = Some(Instant::now());
        self.pacing.status.numbers = if live.display_sync_active {
            SyncNumbers {
                vsync_ratio: live.vsync_ratio,
                speed_correction: live.video_speed_correction,
            }
        } else {
            SyncNumbers::default()
        };
    }

    /// 每一幀算一次流暢播放的做法（視窗出現之後），有變就套用到 mpv
    pub(super) fn pacing_tick(&mut self, ctx: &egui::Context) {
        // 在 input 的鎖裡不能再呼叫 ctx 的其他方法，先取幀編號
        let frame_nr = ctx.cumulative_frame_nr();
        let view = ctx.input(|i| {
            let v = i.viewport();
            View {
                // 不知道（Wayland、Windows 被別的視窗蓋住）當成看得到
                visible: v.visible() != Some(false),
                outer_rect: v.outer_rect,
                fullscreen: v.fullscreen,
                monitor_size: v.monitor_size,
                frame_nr,
            }
        });
        let st = &self.player.state;
        let snap = Snapshot {
            mode: self.settings.smooth,
            dumb: self.caps.dumb,
            // 軌道清單還沒到（開檔中）或沒有檔案：不知道，decide 沿用上一個檔案「有沒有影像」。
            // 沒有檔案也不當成有影像：播清單裡的純音訊檔時，換下一首的空檔不會跳去依螢幕同步
            has_video: (st.loaded && !st.tracks.is_empty()).then(|| st.has_video()),
            passthrough: st.audio_spdif.is_some(),
            file_gen: self.file_gen,
            loaded: st.loaded,
            playing: st.loaded && !st.paused && !self.frame_stepping,
            display_sync_active: st.display_sync_active,
            real_window: self.video.is_some(),
        };
        let now = Instant::now();
        let tick = self.pacing.tick(now, &view, &snap);
        for (name, value) in &tick.send {
            let k = if *name == "video-sync" {
                AsyncKey::VideoSync
            } else {
                AsyncKey::DisplayFps
            };
            self.set_option_async(k, name, value);
        }
        if let Some(notice) = tick.notice {
            self.osd(notice.text());
        }
        let st = &self.player.state;
        let w = wake(
            self.pacing.status.plan.is_some_and(|p| p.is_display()),
            st.display_sync_active,
            st.loaded && !st.paused,
            &view,
            self.video.is_some(),
            self.pacing.pending_wait(now),
        );
        if w.now {
            ctx.request_repaint();
        }
        if let Some(after) = w.after {
            ctx.request_repaint_after(after);
        }
    }
}

impl VitascopeApp {
    /// 每一輪開始時：給影片畫面 egui 的 `predicted_dt` 與螢幕更新率（取影格時最多讓 mpv 等一次更新）；
    /// VITASCOPE_DEBUG=pacing 時每 10 秒印一次畫面輸出的統計
    pub(super) fn render_tick(&mut self, ctx: &egui::Context) {
        let Some(video) = &self.video else { return };
        video.begin_pass(ctx, self.pacing.refresh.map(|r| r.hz));
        let log = &mut self.pacing.render_log;
        if !log.enabled {
            return;
        }
        let now = Instant::now();
        let stats = video.stats();
        let Some(since) = log.since else {
            // 剛開始播放的一秒（載入、建立著色器）不算
            if stats.frames > 0 && now - *log.first.get_or_insert(now) >= Duration::from_secs(1) {
                log.since = Some(now);
                log.prev = stats;
                video.take_presents();
                video.take_segment_max();
            }
            return;
        };
        if self.player.state.loaded && log.avsync_read.is_none_or(|t| now - t >= Duration::from_secs(1)) {
            log.avsync_read = Some(now);
            if let Ok(v) = self.player.mpv().get_property::<f64>("avsync") {
                log.avsync.push(v);
            }
        }
        if now - since < RENDER_LOG_EVERY {
            return;
        }
        let presents = video.take_presents();
        // 最久的 render、GPU 最多提早多少用這一段的最大值（環狀緩衝只有最近 128 次）
        let seg = video.take_segment_max();
        let stats = RenderStats {
            max_us: seg.render_us,
            blocking_max_us: seg.blocking_us,
            gpu_lead_us: seg.gpu_lead_us,
            ..stats
        };
        let line = render_summary(&log.prev, &stats, now - since, &presents, &log.avsync);
        eprintln!("[vitascope] 畫面輸出統計：{line}");
        log.since = Some(now);
        log.prev = stats;
        log.avsync.clear();
    }

    /// 媒體資訊面板「播放流暢度」裡畫面輸出的一行；還沒畫過影片（或沒有影片畫面）就沒有
    pub(super) fn render_line(&self) -> Option<String> {
        let s = self.video.as_ref()?.stats();
        (s.frames > 0).then(|| render_line(&s))
    }
}

/// 「畫面輸出 中位數 0.4 ms · 最久 3.2 ms · 每格 1.0 次重繪」
fn render_line(s: &RenderStats) -> String {
    let ms = |us: u32| f64::from(us) / 1000.0;
    crate::tf!(
        "畫面輸出 中位數 {:.1} ms · 最久 {:.1} ms · 每格 {:.1} 次重繪",
        "Render p50 {:.1} ms · max {:.1} ms · {:.1} redraws per frame",
        ms(s.p50_us),
        ms(s.max_us),
        s.passes_per_frame
    )
}

/// VITASCOPE_DEBUG=pacing 的一行（`key=value` 用空白隔開，測試會解析）：這一段的次數（`blocking` = render 等到預定時間的次數，
/// 後面是這一段平均等多久、最久等多久）、render 花的時間（最近 128 次的中位數、這一段最久）、
/// 交出影格比預定時間晚多少（5／50／95／99 百分位）、相鄰兩格隔幾次螢幕更新的分布、估計的顯示時間比預定晚多少（中位數）、
/// 取影格時離預定時間還有多久（5／50／95 百分位）、GPU 做完影格比預定時間晚多少（50／95 百分位）、
/// 讓 render 等的範圍（一次螢幕更新）、計時器叫醒的那一輪平均晚多少、GPU 來不及時最多提早多少取影格、mpv 的 avsync（平均）。
/// `now` 的 `max_us`、`blocking_max_us`、`gpu_lead_us` 是這一段的最大值（`render_tick` 換掉）
fn render_summary(prev: &RenderStats, now: &RenderStats, elapsed: Duration, p: &Presents, avsync: &[f64]) -> String {
    let secs = elapsed.as_secs_f64().max(1e-3);
    let mut hist: Vec<(u32, usize)> = Vec::new();
    for g in &p.gaps {
        match hist.iter_mut().find(|(k, _)| k == g) {
            Some((_, n)) => *n += 1,
            None => hist.push((*g, 1)),
        }
    }
    hist.sort_unstable();
    let hist = hist
        .iter()
        .map(|(k, n)| format!("{k}:{n}"))
        .collect::<Vec<_>>()
        .join(",");
    let ms = |v: &[i32], pct: u32| {
        rules::percentile(v, pct).map_or("-".to_owned(), |us| format!("{:.2}", f64::from(us) / 1000.0))
    };
    let avsync = if avsync.is_empty() {
        "-".to_owned()
    } else {
        format!("{:.2}", avsync.iter().sum::<f64>() / avsync.len() as f64 * 1000.0)
    };
    let passes = now.passes - prev.passes;
    let blocking = now.blocking - prev.blocking;
    let blocking_avg = if blocking == 0 {
        "-".to_owned()
    } else {
        format!(
            "{:.2}",
            (now.blocking_us - prev.blocking_us) as f64 / blocking as f64 / 1000.0
        )
    };
    format!(
        "secs={secs:.1} renders={} deferred={} blocking={blocking} blocking_avg_ms={blocking_avg} blocking_max_ms={:.2} \
         frames={} passes={passes} passes_per_s={:.1} \
         passes_per_frame={:.2} p50_ms={:.2} max_ms={:.2} done_p5_ms={} done_p50_ms={} done_p95_ms={} done_p99_ms={} \
         vsyncs={} late_p50_ms={} take_p5_ms={} take_p50_ms={} take_p95_ms={} gpu_p50_ms={} gpu_p95_ms={} \
         window_ms={:.2} wake_late_ms={:.2} gpu_lead_ms={:.2} avsync_ms={avsync}",
        now.renders - prev.renders,
        now.deferred - prev.deferred,
        f64::from(now.blocking_max_us) / 1000.0,
        now.frames - prev.frames,
        passes as f64 / secs,
        now.passes_per_frame,
        f64::from(now.p50_us) / 1000.0,
        f64::from(now.max_us) / 1000.0,
        ms(&p.done, 5),
        ms(&p.done, 50),
        ms(&p.done, 95),
        ms(&p.done, 99),
        if hist.is_empty() { "-" } else { &hist },
        ms(&p.late, 50),
        ms(&p.ahead, 5),
        ms(&p.ahead, 50),
        ms(&p.ahead, 95),
        ms(&p.gpu, 50),
        ms(&p.gpu, 95),
        f64::from(now.window_us) / 1000.0,
        f64::from(now.wake_late_us) / 1000.0,
        f64::from(now.gpu_lead_us) / 1000.0,
    )
}

/// 這一幀之後要怎麼叫醒介面
#[derive(Debug, Default, PartialEq)]
struct Wake {
    /// 馬上再畫一次
    now: bool,
    /// 最晚多久之後再跑一次 logic()
    after: Option<Duration>,
}

/// `display`：做法是依螢幕同步；`running`：有檔案、沒暫停；`real_window`：真的在畫影片；
/// `pending`：等著改成依螢幕同步還要多久
fn wake(
    display: bool,
    sync_active: bool,
    running: bool,
    view: &View,
    real_window: bool,
    pending: Option<Duration>,
) -> Wake {
    // 依螢幕同步時 mpv 每次螢幕更新都要一次畫面（重複的影格也是）：mpv 的更新通知本來就會叫，
    // 這裡多要一次當保險（只在真的有畫面時；自動測試沒有畫面，不能讓它一直重畫）
    let now = display && sync_active && running && view.visible && real_window;
    // 視窗看不到時 eframe 不畫，只有要求重畫才會跑 logic()：播放中定時醒來，
    // 切回一般播放、回來時改回依螢幕同步都要靠它
    let hidden = (!view.visible && running).then_some(HIDDEN_TICK);
    // 等著改成依螢幕同步：時間到要再跑一次（暫停中、沒開檔時介面不會自己重畫）
    let after = match (pending, hidden) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    Wake { now, after }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacing::GuardState;
    use crate::screens::RefreshSource;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 假的平台：值可以在測試中改，記下每個方法被呼叫幾次
    #[derive(Default)]
    struct Fake {
        hz: Option<f64>,
        key: Option<u64>,
        power: PowerSource,
        remote: bool,
        calls: [u32; 3],
    }

    struct Probe(Rc<RefCell<Fake>>);

    impl PlatformProbe for Probe {
        fn refresh_rate(&self) -> Option<Refresh> {
            let mut f = self.0.borrow_mut();
            f.calls[0] += 1;
            f.hz.map(|hz| Refresh {
                hz,
                source: RefreshSource::DisplayConfig,
            })
        }
        fn monitor_key(&self) -> Option<u64> {
            let mut f = self.0.borrow_mut();
            f.calls[1] += 1;
            f.key
        }
        fn power(&self) -> PowerSource {
            let mut f = self.0.borrow_mut();
            f.calls[2] += 1;
            f.power
        }
        fn remote_session(&self) -> bool {
            self.0.borrow().remote
        }
    }

    fn setup(fake: Fake) -> (PacingCtl, Rc<RefCell<Fake>>) {
        let fake = Rc::new(RefCell::new(fake));
        let ctl = PacingCtl::new(Some(Box::new(Probe(fake.clone()))), false, Overrides::default());
        (ctl, fake)
    }

    fn view() -> View {
        View {
            visible: true,
            outer_rect: Some(egui::Rect::from_min_size(
                egui::pos2(100.0, 100.0),
                egui::vec2(960.0, 600.0),
            )),
            fullscreen: Some(false),
            monitor_size: Some(egui::vec2(1707.0, 960.0)),
            frame_nr: 0,
        }
    }

    fn snap() -> Snapshot {
        Snapshot {
            mode: SmoothMode::Auto,
            dumb: false,
            has_video: Some(true),
            passthrough: false,
            file_gen: 1,
            loaded: true,
            playing: true,
            display_sync_active: false,
            real_window: true,
        }
    }

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn computes_the_plan_from_the_probe() {
        let (mut ctl, fake) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        assert_eq!(ctl.status().plan, None);
        assert_eq!(ctl.status().describe(), "計算中…");
        let t = Instant::now();
        ctl.tick(t, &view(), &snap());
        assert_eq!(
            ctl.status().plan,
            Some(Plan::Display {
                hz: 119.88,
                vdrop: false
            })
        );
        // 要先維持 0.5 秒才套用
        assert_eq!(ctl.status().describe(), "準備中：119.880 Hz");
        // 預設設定（關）
        let off = Snapshot {
            mode: SmoothMode::Off,
            ..snap()
        };
        ctl.tick(at(t, 16), &view(), &off);
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::Setting)));
        assert_eq!(ctl.status().describe(), "未使用：設定為關閉");
        // 使用電池
        fake.borrow_mut().power = PowerSource::Battery;
        ctl.tick(at(t, 10_100), &view(), &snap());
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::Battery)));
        assert_eq!(ctl.status().power, PowerSource::Battery);
        assert_eq!(
            ctl.status().info_lines(),
            vec![
                "螢幕更新率：119.880 Hz（QueryDisplayConfig）".to_owned(),
                "電源：使用電池".to_owned(),
                "流暢播放：未使用：使用電池中".to_owned(),
            ]
        );
    }

    #[test]
    fn requery_schedule() {
        let (mut ctl, fake) = setup(Fake {
            hz: Some(60.0),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        let t = Instant::now();
        let calls = |f: &Rc<RefCell<Fake>>| f.borrow().calls;
        ctl.tick(t, &view(), &snap());
        assert_eq!(calls(&fake), [1, 1, 1], "第一次全部查");
        // 每一幀都呼叫，但 500 ms 才看一次螢幕、5 秒才重查更新率、10 秒才查電源
        for ms in (16..4_990).step_by(16) {
            ctl.tick(at(t, ms), &view(), &snap());
        }
        assert_eq!(calls(&fake), [1, 10, 1]);
        ctl.tick(at(t, 5_000), &view(), &snap());
        assert_eq!(calls(&fake), [2, 10, 1], "5 秒重查一次");
        ctl.tick(at(t, 10_000), &view(), &snap());
        assert_eq!(calls(&fake), [3, 11, 2], "10 秒查電源");

        // 換到另一個螢幕：下一次看螢幕時就重查
        {
            let mut f = fake.borrow_mut();
            f.key = Some(2);
            f.hz = Some(144.0);
        }
        ctl.tick(at(t, 10_100), &view(), &snap());
        assert_eq!(calls(&fake)[0], 3, "還沒到看螢幕的時間");
        ctl.tick(at(t, 10_500), &view(), &snap());
        assert_eq!(calls(&fake)[0], 4);
        assert_eq!(
            ctl.status().plan,
            Some(Plan::Display {
                hz: 144.0,
                vdrop: false
            })
        );

        // 拖曳視窗：移動中不查，停下來 300 ms 後查一次
        let mut v = view();
        let before = calls(&fake)[0];
        for (i, ms) in (10_516..11_000).step_by(16).enumerate() {
            v.outer_rect = v.outer_rect.map(|r| r.translate(egui::vec2(i as f32 + 1.0, 0.0)));
            ctl.tick(at(t, ms), &v, &snap());
            assert_eq!(calls(&fake)[0], before, "移動中不查（{ms} ms）");
        }
        ctl.tick(at(t, 11_200), &v, &snap());
        assert_eq!(calls(&fake)[0], before, "停下來還不到 300 ms");
        ctl.tick(at(t, 11_300), &v, &snap());
        assert_eq!(calls(&fake)[0], before + 1);
        ctl.tick(at(t, 11_316), &v, &snap());
        assert_eq!(calls(&fake)[0], before + 1, "只查一次");

        // 全螢幕
        v.fullscreen = Some(true);
        ctl.tick(at(t, 11_332), &v, &snap());
        assert_eq!(calls(&fake)[0], before + 2);
        // 螢幕的大小變了（改了解析度、縮放）
        v.monitor_size = Some(egui::vec2(3840.0, 2160.0));
        ctl.tick(at(t, 11_348), &v, &snap());
        assert_eq!(calls(&fake)[0], before + 3);
        ctl.tick(at(t, 11_364), &v, &snap());
        assert_eq!(calls(&fake)[0], before + 3);

        // 遠端桌面：跟電源一起每 10 秒查
        fake.borrow_mut().remote = true;
        ctl.tick(at(t, 19_900), &v, &snap());
        assert!(ctl.status().plan.is_some_and(|p| p.is_display()));
        ctl.tick(at(t, 20_000), &v, &snap());
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::RemoteSession)));
        assert_eq!(calls(&fake)[2], 3);
    }

    #[test]
    fn idle_follows_the_screen() {
        // 沒有檔案（開檔前、播完之後）：不知道有沒有影像，但螢幕、電源的變化照樣反映
        let (mut ctl, fake) = setup(Fake {
            hz: Some(120.0),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        let idle = Snapshot {
            has_video: None,
            playing: false,
            ..snap()
        };
        let t = Instant::now();
        ctl.tick(t, &view(), &idle);
        assert_eq!(
            ctl.status().plan,
            Some(Plan::Display {
                hz: 120.0,
                vdrop: false
            })
        );
        fake.borrow_mut().hz = Some(60.0);
        ctl.tick(at(t, 5_000), &view(), &idle);
        assert_eq!(ctl.status().plan, Some(Plan::Display { hz: 60.0, vdrop: false }));
        fake.borrow_mut().power = PowerSource::Battery;
        ctl.tick(at(t, 10_000), &view(), &idle);
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::Battery)));
        fake.borrow_mut().power = PowerSource::Ac;
        ctl.tick(at(t, 20_000), &view(), &idle);
        assert_eq!(ctl.status().plan, Some(Plan::Display { hz: 60.0, vdrop: false }));
        // 上一個檔案沒有影像：播完之後還是當成沒有，等下一個檔案開完再說
        let audio_only = Snapshot {
            has_video: Some(false),
            ..snap()
        };
        ctl.tick(at(t, 20_016), &view(), &audio_only);
        ctl.tick(at(t, 20_032), &view(), &idle);
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::NoVideo)));
    }

    /// 依螢幕同步中的播放：每隔 `dt` 毫秒畫一幀
    struct Playback {
        ctl: PacingCtl,
        fake: Rc<RefCell<Fake>>,
        start: Instant,
        ms: f64,
        frame: u64,
        view: View,
        snap: Snapshot,
    }

    impl Playback {
        fn new() -> Self {
            let (mut ctl, fake) = setup(Fake {
                hz: Some(119.88),
                key: Some(1),
                power: PowerSource::Ac,
                ..Default::default()
            });
            // 啟動時就依螢幕同步（已經套用，不用等）
            assert_eq!(ctl.startup(SmoothMode::Auto, false).len(), 2);
            ctl.start_file(1);
            Self {
                ctl,
                fake,
                start: Instant::now(),
                ms: 0.0,
                frame: 0,
                view: view(),
                snap: Snapshot {
                    display_sync_active: true,
                    ..snap()
                },
            }
        }

        /// 跑 `for_ms` 毫秒；防呆第一次有結果時回傳那是這一段開始後幾毫秒
        fn run(&mut self, dt: f64, for_ms: f64) -> Option<f64> {
            let begin = self.ms;
            let mut verdict = None;
            while self.ms - begin < for_ms {
                self.ms += dt;
                self.frame += 1;
                self.view.frame_nr = self.frame;
                let now = self.start + Duration::from_secs_f64(self.ms / 1000.0);
                self.ctl.tick(now, &self.view, &self.snap);
                if verdict.is_none() && self.ctl.guard.state() != GuardState::default() {
                    verdict = Some(self.ms - begin);
                }
            }
            verdict
        }
    }

    #[test]
    fn guard_is_fed_only_while_display_synced() {
        // swap 沒等垂直同步（每 1 ms 一幀）：等 1 秒、湊滿 60 個間隔、再持續 1 秒
        let mut p = Playback::new();
        let at = p.run(1.0, 3_000.0).expect("要判定");
        assert!((2_000.0..2_200.0).contains(&at), "{at}");
        assert!(p.ctl.guard.state().no_vsync);
        assert_eq!(p.ctl.status().plan, Some(Plan::Audio(Reason::NoVsync)));

        // mpv 沒在依螢幕同步（例如判斷這部影片不適用）：不量
        let mut p = Playback::new();
        p.snap.display_sync_active = false;
        assert_eq!(p.run(1.0, 5_000.0), None);
        // 一般播放：不量
        let mut p = Playback::new();
        p.snap.mode = SmoothMode::Off;
        assert_eq!(p.run(1.0, 5_000.0), None);
        // 只跟得上一半：改用一半的更新率（量的週期跟著變）之後同樣的間隔就是正常的
        let mut p = Playback::new();
        let half = 2000.0 / 119.88;
        assert!(p.run(half, 5_000.0).is_some());
        assert!(p.ctl.guard.state().half_rate);
        p.run(half, 20_000.0);
        assert_eq!(
            p.ctl.guard.state(),
            GuardState {
                half_rate: true,
                ..Default::default()
            }
        );
        assert_eq!(
            p.ctl.status().plan,
            Some(Plan::Display {
                hz: 59.94,
                vdrop: false
            })
        );
    }

    #[test]
    fn pause_hide_or_drag_postpones_the_verdict() {
        fn pause(p: &mut Playback) {
            p.snap.playing = !p.snap.playing;
        }
        fn hide(p: &mut Playback) {
            p.view.visible = !p.view.visible;
        }
        for (name, toggle) in [("暫停", pause as fn(&mut Playback)), ("縮到最小", hide)] {
            // 差一點就判定的時候中斷，回來之後要重新開始量
            let mut p = Playback::new();
            assert_eq!(p.run(1.0, 1_900.0), None, "{name}");
            toggle(&mut p);
            assert_eq!(p.run(1.0, 300.0), None, "{name}");
            toggle(&mut p);
            assert_eq!(p.run(1.0, 1_900.0), None, "{name}：中斷之後重新量");
            assert!(p.run(1.0, 1_000.0).is_some(), "{name}：繼續下去才判定");
        }
        // 拖曳視窗 300 ms：停下來之後重新量
        let mut p = Playback::new();
        assert_eq!(p.run(1.0, 1_900.0), None);
        for _ in 0..300 {
            p.view.outer_rect = p.view.outer_rect.map(|r| r.translate(egui::vec2(1.0, 0.0)));
            assert_eq!(p.run(1.0, 1.0), None);
        }
        assert_eq!(p.run(1.0, 1_900.0), None, "拖曳之後重新量");
        assert!(p.run(1.0, 1_000.0).is_some());
    }

    #[test]
    fn returning_to_display_sync_measures_afresh() {
        // 慢了 3 秒（還沒到 5 秒），mpv 停止依螢幕同步一分鐘，回來之後不能拿舊的間隔、舊的起算時間馬上判定
        let mut p = Playback::new();
        assert_eq!(p.run(20.0, 3_000.0), None);
        p.snap.display_sync_active = false;
        assert_eq!(p.run(50.0, 60_000.0), None);
        p.snap.display_sync_active = true;
        assert_eq!(p.run(20.0, 5_000.0), None, "重新量：等 1 秒、湊滿間隔、再持續 5 秒");
        assert!(p.run(20.0, 3_000.0).is_some());
        assert_eq!(p.ctl.guard.state().too_slow, Some(1));

        // 用電池時改回一般播放，接上電源之後回來：同樣重新量
        let mut p = Playback::new();
        assert_eq!(p.run(1000.0 / 119.88, 9_000.0), None);
        assert_eq!(p.run(1.0, 990.0), None, "快要判定「沒等垂直同步」");
        p.fake.borrow_mut().power = PowerSource::Battery;
        p.run(1.0, 100.0);
        assert_eq!(p.ctl.status().plan, Some(Plan::Audio(Reason::Battery)));
        p.fake.borrow_mut().power = PowerSource::Ac;
        assert_eq!(p.run(50.0, 9_950.0), None);
        assert!(p.ctl.status().plan.is_some_and(|p| p.is_display()));
        assert_eq!(p.run(1.0, 1_900.0), None, "重新量");
        assert!(p.run(1.0, 1_000.0).is_some());
    }

    #[test]
    fn hidden_window_and_no_probe() {
        let (mut ctl, _) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            ..Default::default()
        });
        let hidden = View {
            visible: false,
            ..view()
        };
        ctl.tick(Instant::now(), &hidden, &snap());
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::Hidden)));
        assert_eq!(ctl.status().describe(), "未使用：視窗縮到最小");

        // 沒有平台資訊（自動測試的視窗）：偵測不到更新率
        let mut ctl = PacingCtl::new(None, false, Overrides::default());
        ctl.tick(Instant::now(), &view(), &snap());
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::NoRefresh)));
        assert_eq!(ctl.status().info_lines()[0], "螢幕更新率：偵測不到");
        assert_eq!(ctl.status().describe(), "未使用：偵測不到這個螢幕的更新率");
    }

    #[test]
    fn user_overrides_stand_down() {
        let mut ctl = PacingCtl::new(None, true, Overrides::default());
        ctl.tick(Instant::now(), &view(), &snap());
        assert_eq!(ctl.status().plan, Some(Plan::Untouched));
        assert_eq!(ctl.status().describe(), "已由 VITASCOPE_MPV_OPTS 指定");
        let off = rules::parse_overrides(Some("off"));
        let mut ctl = PacingCtl::new(None, false, off);
        ctl.tick(Instant::now(), &view(), &snap());
        assert_eq!(ctl.status().plan, Some(Plan::Untouched));
        assert_eq!(ctl.status().describe(), "未使用：已由 VITASCOPE_PACING=off 關閉");
    }

    #[test]
    fn status_strings_in_english() {
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let status = PacingStatus {
            plan: Some(Plan::Audio(Reason::NoRefresh)),
            ..Default::default()
        };
        assert_eq!(status.describe(), "Not in use: can't detect this screen's refresh rate");
        let applied = PacingStatus {
            plan: Some(Plan::Display {
                hz: 119.88,
                vdrop: false,
            }),
            applied: true,
            sync_active: true,
            ..Default::default()
        };
        assert_eq!(applied.describe(), "In use: 119.880 Hz");
        let declined = PacingStatus {
            sync_active: false,
            loaded: true,
            ..applied.clone()
        };
        assert_eq!(declined.describe(), "On, but mpv decided this video isn't suitable");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(applied.describe(), "使用中：119.880 Hz");
        let passthrough = PacingStatus {
            plan: Some(Plan::Display {
                hz: 119.88,
                vdrop: true,
            }),
            ..applied
        };
        assert_eq!(passthrough.describe(), "音訊直通中：以略過或重複影格對齊螢幕");
        // 每個原因都有中英文說明
        for reason in [
            Reason::Setting,
            Reason::NoVideo,
            Reason::Battery,
            Reason::SoftwareRenderer,
            Reason::RemoteSession,
            Reason::ApplyFailed,
            Reason::NoVsync,
            Reason::TooSlow,
            Reason::Hidden,
            Reason::NoRefresh,
        ] {
            let zh = reason_text(reason);
            crate::i18n::set_lang(crate::i18n::Lang::En);
            let en = reason_text(reason);
            crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
            assert!(!zh.is_ascii() && en.is_ascii() && !en.is_empty(), "{reason:?}");
        }
    }

    const DISPLAY: Plan = Plan::Display {
        hz: 119.88,
        vdrop: false,
    };

    fn on(hz: f64) -> Vec<(&'static str, String)> {
        vec![
            ("display-fps-override", format!("{hz:.6}")),
            ("video-sync", "display-resample".to_owned()),
        ]
    }

    fn off() -> Vec<(&'static str, String)> {
        vec![
            ("video-sync", "audio".to_owned()),
            ("display-fps-override", "0".to_owned()),
        ]
    }

    #[test]
    fn display_waits_half_a_second_audio_is_immediate() {
        let (mut ctl, _) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        let t = Instant::now();
        for ms in (0..500).step_by(16) {
            let tick = ctl.tick(at(t, ms), &view(), &snap());
            assert!(tick.send.is_empty(), "{ms} ms 還不到 0.5 秒");
            assert!(!ctl.status().applied);
        }
        let sent = ctl.tick(at(t, 500), &view(), &snap()).send;
        assert_eq!(sent, on(119.88), "先設更新率，再換同步方式");
        assert!(ctl.status().applied);
        // mpv 還沒開始同步
        assert_eq!(ctl.status().describe(), "已開啟，但 mpv 判斷這部影片不適用");
        // 一樣的做法不再送
        let synced = Snapshot {
            display_sync_active: true,
            ..snap()
        };
        assert_eq!(ctl.tick(at(t, 516), &view(), &synced), Tick::default());
        assert_eq!(ctl.status().describe(), "使用中：119.880 Hz");
        // 關掉：馬上送，先換回音訊同步
        let off_snap = Snapshot {
            mode: SmoothMode::Off,
            ..snap()
        };
        assert_eq!(ctl.tick(at(t, 532), &view(), &off_snap).send, off());
        assert_eq!(ctl.sets(), 4);
        // 只開一下又關掉（不到 0.5 秒）：什麼都不送
        ctl.tick(at(t, 548), &view(), &snap());
        ctl.tick(at(t, 800), &view(), &snap());
        assert_eq!(ctl.tick(at(t, 816), &view(), &off_snap), Tick::default());
        assert_eq!(ctl.tick(at(t, 2_000), &view(), &off_snap), Tick::default());
        // 等待中更新率變了：重新計時，用新的更新率
        ctl.tick(at(t, 2_016), &view(), &snap());
        let mut v = view();
        v.monitor_size = Some(egui::vec2(3840.0, 2160.0));
        ctl.probe = Some(Box::new(Probe(Rc::new(RefCell::new(Fake {
            hz: Some(60.0),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        })))));
        assert!(ctl.tick(at(t, 2_400), &v, &snap()).send.is_empty());
        assert!(
            ctl.tick(at(t, 2_516), &v, &snap()).send.is_empty(),
            "換了做法，重新等 0.5 秒"
        );
        assert_eq!(ctl.pending_wait(at(t, 2_516)), Some(Duration::from_millis(384)));
        assert_eq!(ctl.tick(at(t, 2_900), &v, &snap()).send, on(60.0));
        assert_eq!(ctl.pending_wait(at(t, 2_900)), None);
    }

    #[test]
    fn default_off_sends_nothing() {
        // 預設設定（關）：螢幕、電源、視窗怎麼變都不送任何設定
        let (mut ctl, fake) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        let off_snap = Snapshot {
            mode: SmoothMode::default(),
            ..snap()
        };
        assert!(ctl.startup(SmoothMode::default(), false).is_empty());
        let t = Instant::now();
        let mut v = view();
        for (i, ms) in (0..30_000).step_by(16).enumerate() {
            if i % 200 == 0 {
                let mut f = fake.borrow_mut();
                f.hz = if f.hz.is_some() { None } else { Some(60.0) };
                f.power = if f.power == PowerSource::Ac {
                    PowerSource::Battery
                } else {
                    PowerSource::Ac
                };
                v.visible = !v.visible;
            }
            assert_eq!(ctl.tick(at(t, ms), &v, &off_snap), Tick::default());
        }
        assert_eq!(ctl.sets(), 0);
        assert!(ctl.status().applied);
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::Setting)));
        // 一開始就當成 mpv 是預設的一般播放：沒經過 startup 也不送
        let (mut ctl, _) = setup(Fake {
            hz: Some(119.88),
            ..Default::default()
        });
        assert_eq!(ctl.tick(t, &view(), &off_snap), Tick::default());
        assert_eq!(ctl.sets(), 0);
    }

    #[test]
    fn startup_applies_display_synchronously() {
        let (mut ctl, _) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        assert_eq!(ctl.startup(SmoothMode::Auto, false), on(119.88));
        assert_eq!(ctl.status().plan, Some(DISPLAY));
        assert!(ctl.status().applied);
        // 之後（還在開檔，不知道有沒有影像）不用再送
        let loading = Snapshot {
            has_video: None,
            loaded: false,
            playing: false,
            ..snap()
        };
        assert_eq!(ctl.tick(Instant::now(), &view(), &loading), Tick::default());
        // 一般播放、查不到更新率、使用者自己指定：啟動時什麼都不送
        for (hz, mode, user_set) in [
            (Some(119.88), SmoothMode::Off, false),
            (None, SmoothMode::Auto, false),
            (Some(119.88), SmoothMode::Auto, true),
        ] {
            let probe = Probe(Rc::new(RefCell::new(Fake {
                hz,
                power: PowerSource::Ac,
                ..Default::default()
            })));
            let mut ctl = PacingCtl::new(Some(Box::new(probe)), user_set, Overrides::default());
            assert!(ctl.startup(mode, false).is_empty(), "{hz:?} {mode:?} {user_set}");
            assert_eq!(ctl.sets(), 0);
            let s = Snapshot { mode, ..snap() };
            assert_eq!(ctl.tick(Instant::now(), &view(), &s).send, Vec::new());
        }
        // 軟體繪圖：不用
        let (mut ctl, _) = setup(Fake {
            hz: Some(119.88),
            ..Default::default()
        });
        assert!(ctl.startup(SmoothMode::Always, true).is_empty());
        assert_eq!(ctl.status().plan, Some(Plan::Audio(Reason::SoftwareRenderer)));
    }

    #[test]
    fn battery_and_power_notices_only_while_playing() {
        let mut p = Playback::new();
        p.run(1000.0 / 119.88, 1_000.0);
        p.fake.borrow_mut().power = PowerSource::Battery;
        p.ctl.requery();
        let now = p.start + Duration::from_secs(2);
        let tick = p.ctl.tick(now, &p.view, &p.snap);
        assert_eq!(tick.send, off(), "改回一般播放馬上送");
        assert_eq!(tick.notice, Some(Notice::BatteryPause));
        assert_eq!(Notice::BatteryPause.text(), "使用電池：流暢播放暫停（省電）");
        p.fake.borrow_mut().power = PowerSource::Ac;
        p.ctl.requery();
        let tick = p.ctl.tick(now + Duration::from_millis(16), &p.view, &p.snap);
        assert_eq!(tick, Tick::default(), "依螢幕同步要等 0.5 秒");
        let tick = p.ctl.tick(now + Duration::from_millis(600), &p.view, &p.snap);
        assert_eq!(tick.send, on(119.88));
        assert_eq!(tick.notice, Some(Notice::AcResume));
        // 暫停中（或沒在播）不提示
        p.snap.playing = false;
        p.fake.borrow_mut().power = PowerSource::Battery;
        p.ctl.requery();
        let tick = p.ctl.tick(now + Duration::from_secs(1), &p.view, &p.snap);
        assert_eq!((tick.send.len(), tick.notice), (2, None));
        // 「一直開」用電池也照樣同步
        p.snap.mode = SmoothMode::Always;
        p.ctl.tick(now + Duration::from_secs(2), &p.view, &p.snap);
        let tick = p.ctl.tick(now + Duration::from_secs(3), &p.view, &p.snap);
        assert_eq!(tick.send, on(119.88));
    }

    #[test]
    fn verdicts_switch_to_audio_at_once_with_a_notice() {
        let mut p = Playback::new();
        // 沒等垂直同步：判定的那一幀就改回一般播放
        let mut ticks = Vec::new();
        for i in 1..=3_000u64 {
            p.view.frame_nr = i;
            let now = p.start + Duration::from_millis(i);
            let tick = p.ctl.tick(now, &p.view, &p.snap);
            if tick != Tick::default() {
                ticks.push(tick);
            }
        }
        assert_eq!(
            ticks,
            vec![Tick {
                send: off(),
                notice: Some(Notice::NoVsync)
            }]
        );
        assert_eq!(
            p.ctl.status().describe(),
            format!("未使用：{}", reason_text(Reason::NoVsync))
        );
        // 使用者改了設定：重新判斷，又可以用
        p.ctl.setting_changed();
        p.ms = 3_000.0;
        p.frame = 3_000;
        p.run(1000.0 / 119.88, 600.0);
        assert!(p.ctl.status().applied && p.ctl.status().plan == Some(DISPLAY));
        // 畫面更新跟不上：這個檔案改用一般播放，下一個檔案再試
        let mut p = Playback::new();
        let mut notice = None;
        for i in 1..=600u64 {
            p.view.frame_nr = i;
            let tick = p.ctl.tick(p.start + Duration::from_millis(i * 20), &p.view, &p.snap);
            if tick.notice.is_some() {
                notice = tick.notice;
                assert_eq!(tick.send, off());
            }
        }
        assert_eq!(notice, Some(Notice::TooSlow));
        assert_eq!(p.ctl.status().plan, Some(Plan::Audio(Reason::TooSlow)));
        p.ctl.start_file(2);
        p.snap.file_gen = 2;
        assert!(
            p.ctl
                .tick(p.start + Duration::from_secs(13), &p.view, &p.snap)
                .send
                .is_empty()
        );
        assert_eq!(
            p.ctl.tick(p.start + Duration::from_secs(14), &p.view, &p.snap).send,
            on(119.88)
        );
    }

    #[test]
    fn apply_failure_falls_back_for_the_session() {
        let mut p = Playback::new();
        p.ctl.apply_failed();
        let tick = p.ctl.tick(p.start, &p.view, &p.snap);
        assert_eq!(tick.send, off());
        assert_eq!(p.ctl.status().plan, Some(Plan::Audio(Reason::ApplyFailed)));
        // 換檔不再試
        p.ctl.start_file(2);
        p.snap.file_gen = 2;
        assert_eq!(
            p.ctl.tick(p.start + Duration::from_secs(5), &p.view, &p.snap),
            Tick::default()
        );
        // 改了設定才重試
        p.ctl.setting_changed();
        p.ctl.tick(p.start + Duration::from_secs(6), &p.view, &p.snap);
        assert_eq!(
            p.ctl.tick(p.start + Duration::from_secs(7), &p.view, &p.snap).send,
            on(119.88)
        );
    }

    #[test]
    fn passthrough_drops_frames_instead() {
        let mut p = Playback::new();
        p.snap.passthrough = true;
        assert!(p.ctl.tick(p.start, &p.view, &p.snap).send.is_empty());
        assert_eq!(
            p.ctl.tick(p.start + Duration::from_secs(1), &p.view, &p.snap).send,
            vec![("video-sync", "display-vdrop".to_owned())]
        );
        assert_eq!(p.ctl.status().describe(), "音訊直通中：以略過或重複影格對齊螢幕");
    }

    #[test]
    fn user_overrides_never_send() {
        let probe = Probe(Rc::new(RefCell::new(Fake {
            hz: Some(119.88),
            power: PowerSource::Ac,
            ..Default::default()
        })));
        let mut ctl = PacingCtl::new(Some(Box::new(probe)), true, Overrides::default());
        assert!(ctl.startup(SmoothMode::Auto, false).is_empty());
        let t = Instant::now();
        for ms in (0..3_000).step_by(16) {
            assert_eq!(ctl.tick(at(t, ms), &view(), &snap()), Tick::default());
        }
        assert_eq!(
            ctl.short_for(SmoothMode::Auto).as_deref(),
            Some("已由 VITASCOPE_MPV_OPTS 指定")
        );
    }

    #[test]
    fn guard_needs_a_real_window_and_the_applied_plan() {
        // 自動測試沒有畫面：幀的間隔是測試決定的，不能量
        let mut p = Playback::new();
        p.snap.real_window = false;
        assert_eq!(p.run(1.0, 10_000.0), None);
        // 還在等 0.5 秒（還沒套用）：不量
        let (mut ctl, _) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        let s = Snapshot {
            display_sync_active: true,
            ..snap()
        };
        let t = Instant::now();
        for i in 1..=499u64 {
            let v = View { frame_nr: i, ..view() };
            ctl.tick(at(t, i), &v, &s);
            assert!(!ctl.sampling, "{i}");
        }
    }

    #[test]
    fn short_labels_for_the_menu() {
        let (mut ctl, fake) = setup(Fake {
            hz: Some(119.88),
            key: Some(1),
            power: PowerSource::Ac,
            ..Default::default()
        });
        assert_eq!(ctl.short_for(SmoothMode::Auto), None, "還沒算過");
        let t = Instant::now();
        let off_snap = Snapshot {
            mode: SmoothMode::Off,
            ..snap()
        };
        ctl.tick(t, &view(), &off_snap);
        assert_eq!(ctl.short_for(SmoothMode::Auto).as_deref(), Some("119.88 Hz"));
        assert_eq!(ctl.short_for(SmoothMode::Off).as_deref(), Some("119.88 Hz"));
        fake.borrow_mut().power = PowerSource::Battery;
        ctl.requery();
        ctl.tick(at(t, 16), &view(), &off_snap);
        assert_eq!(ctl.short_for(SmoothMode::Auto).as_deref(), Some("使用電池，暫停"));
        assert_eq!(ctl.short_for(SmoothMode::Always).as_deref(), Some("119.88 Hz"));
        fake.borrow_mut().hz = None;
        ctl.requery();
        ctl.tick(at(t, 32), &view(), &off_snap);
        assert_eq!(ctl.short_for(SmoothMode::Always).as_deref(), Some("偵測不到更新率"));
        assert_eq!(ctl.short_for(SmoothMode::Off), None);
        // 每一種情況都有中英文
        for plan in [
            Plan::Untouched,
            Plan::Audio(Reason::Battery),
            Plan::Audio(Reason::SoftwareRenderer),
            Plan::Audio(Reason::RemoteSession),
            Plan::Audio(Reason::ApplyFailed),
            Plan::Audio(Reason::NoVsync),
            Plan::Audio(Reason::TooSlow),
            Plan::Audio(Reason::NoRefresh),
        ] {
            let zh = short_text(&plan, None, false).unwrap();
            crate::i18n::set_lang(crate::i18n::Lang::En);
            let en = short_text(&plan, None, false).unwrap();
            crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
            assert!(!zh.is_ascii() && en.is_ascii(), "{plan:?}");
        }
        assert_eq!(
            short_text(&Plan::Untouched, None, true).as_deref(),
            Some("VITASCOPE_PACING=off")
        );
    }

    #[test]
    fn in_use_detail_from_mpv_numbers() {
        let mut status = PacingStatus {
            plan: Some(Plan::Display {
                hz: 120.0,
                vdrop: false,
            }),
            applied: true,
            sync_active: true,
            loaded: true,
            numbers: SyncNumbers {
                vsync_ratio: Some(5.0004),
                speed_correction: Some(1.001),
            },
            ..Default::default()
        };
        assert_eq!(status.describe(), "使用中：120.000 Hz（每格 5 次更新，影片快 0.10%）");
        status.numbers.speed_correction = Some(0.9990);
        status.numbers.vsync_ratio = Some(2.5);
        assert_eq!(
            status.describe(),
            "使用中：120.000 Hz（每格 2.500 次更新，影片慢 0.10%）"
        );
        status.numbers.speed_correction = Some(1.0);
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(
            status.describe(),
            "In use: 120.000 Hz (2.500 refreshes per frame, speed unchanged)"
        );
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        // 沒有數字（面板沒打開過）、沒有檔案
        status.numbers = SyncNumbers::default();
        assert_eq!(status.describe(), "使用中：120.000 Hz");
        status.sync_active = false;
        status.loaded = false;
        assert_eq!(status.describe(), "使用中：120.000 Hz");
        status.loaded = true;
        assert_eq!(status.describe(), "已開啟，但 mpv 判斷這部影片不適用");
    }

    #[test]
    fn settings_change_on_battery_is_not_a_power_notice() {
        // 用電池、自動：一般播放（使用電池中）
        let mut p = Playback::new();
        p.fake.borrow_mut().power = PowerSource::Battery;
        p.ctl.requery();
        let t = p.start + Duration::from_secs(1);
        assert_eq!(p.ctl.tick(t, &p.view, &p.snap).notice, Some(Notice::BatteryPause));
        assert_eq!(p.ctl.status().plan, Some(Plan::Audio(Reason::Battery)));
        // 設定頁取消「使用電池時暫停」（一直開）：還是用電池，不能說「接上電源」
        p.snap.mode = SmoothMode::Always;
        p.ctl.setting_changed();
        let mut ticks = Vec::new();
        for ms in (16..2_000).step_by(16) {
            ticks.push(p.ctl.tick(t + Duration::from_millis(ms), &p.view, &p.snap));
        }
        let sent: Vec<&Tick> = ticks.iter().filter(|t| **t != Tick::default()).collect();
        assert_eq!(
            sent,
            vec![&Tick {
                send: on(119.88),
                notice: None
            }]
        );
        // 再勾回來（自動）：改回一般播放，也不是拔掉電源
        p.snap.mode = SmoothMode::Auto;
        p.ctl.setting_changed();
        let tick = p.ctl.tick(t + Duration::from_secs(3), &p.view, &p.snap);
        assert_eq!((tick.send, tick.notice), (off(), None));
        // 之後真的接上電源：照樣提示
        p.fake.borrow_mut().power = PowerSource::Ac;
        p.ctl.requery();
        p.ctl.tick(t + Duration::from_secs(4), &p.view, &p.snap);
        let tick = p.ctl.tick(t + Duration::from_millis(4_600), &p.view, &p.snap);
        assert_eq!((tick.send, tick.notice), (on(119.88), Some(Notice::AcResume)));
        // 「一直開」時拔掉電源：做法沒變，不提示；之後改成自動才暫停，也不提示
        p.snap.mode = SmoothMode::Always;
        p.ctl.setting_changed();
        p.fake.borrow_mut().power = PowerSource::Battery;
        p.ctl.requery();
        assert_eq!(
            p.ctl.tick(t + Duration::from_secs(5), &p.view, &p.snap),
            Tick::default()
        );
        p.snap.mode = SmoothMode::Auto;
        p.ctl.setting_changed();
        let tick = p.ctl.tick(t + Duration::from_secs(6), &p.view, &p.snap);
        assert_eq!((tick.send, tick.notice), (off(), None));
    }

    #[test]
    fn half_rate_switches_quietly() {
        // swap 只跟得上一半：改用一半的更新率（等 0.5 秒），播放照樣順，不提示
        let mut p = Playback::new();
        let half = 2000.0 / 119.88;
        let mut ticks = Vec::new();
        while p.ms < 6_000.0 {
            p.ms += half;
            p.frame += 1;
            p.view.frame_nr = p.frame;
            let now = p.start + Duration::from_secs_f64(p.ms / 1000.0);
            let tick = p.ctl.tick(now, &p.view, &p.snap);
            if tick != Tick::default() {
                ticks.push(tick);
            }
        }
        assert!(p.ctl.guard.state().half_rate);
        assert_eq!(
            ticks,
            vec![Tick {
                send: vec![("display-fps-override", "59.940000".to_owned())],
                notice: None
            }]
        );
        assert_eq!(
            p.ctl.status().plan,
            Some(Plan::Display {
                hz: 59.94,
                vdrop: false
            })
        );
    }

    #[test]
    fn wake_schedule() {
        let shown = view();
        let hidden = View {
            visible: false,
            ..view()
        };
        let wait = Some(Duration::from_millis(300));
        // 依螢幕同步、播放中、看得到、真的有畫面：每次螢幕更新都畫
        assert_eq!(
            wake(true, true, true, &shown, true, None),
            Wake { now: true, after: None }
        );
        // 少一個條件就不用（自動測試沒有畫面、一般播放、mpv 沒在同步、暫停、看不到）
        for (display, sync, running, v, real) in [
            (true, true, true, &shown, false),
            (false, true, true, &shown, true),
            (true, false, true, &shown, true),
            (true, true, false, &shown, true),
        ] {
            assert_eq!(wake(display, sync, running, v, real, None), Wake::default());
        }
        // 看不到、播放中：每 40 ms 醒來
        assert_eq!(
            wake(true, true, true, &hidden, true, None),
            Wake {
                now: false,
                after: Some(HIDDEN_TICK)
            }
        );
        assert_eq!(wake(false, false, true, &hidden, false, None).after, Some(HIDDEN_TICK));
        // 看不到但暫停、沒開檔：不用醒
        assert_eq!(wake(false, false, false, &hidden, true, None), Wake::default());
        // 等著改成依螢幕同步：時間到再跑一次；跟看不到的 40 ms 取早的
        assert_eq!(wake(false, false, false, &shown, true, wait).after, wait);
        assert_eq!(wake(false, false, true, &hidden, true, wait).after, Some(HIDDEN_TICK));
        assert_eq!(
            wake(false, false, true, &hidden, true, Some(Duration::from_millis(10))).after,
            Some(Duration::from_millis(10))
        );
    }

    #[test]
    fn render_line_in_both_languages() {
        let s = RenderStats {
            frames: 10,
            passes_per_frame: 1.04,
            p50_us: 420,
            max_us: 3_180,
            ..Default::default()
        };
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(
            render_line(&s),
            "Render p50 0.4 ms · max 3.2 ms · 1.0 redraws per frame"
        );
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(
            render_line(&s),
            "畫面輸出 中位數 0.4 ms · 最久 3.2 ms · 每格 1.0 次重繪"
        );
    }

    #[test]
    fn render_summary_line() {
        let prev = RenderStats {
            renders: 10,
            deferred: 5,
            blocking: 1,
            blocking_us: 9_000,
            frames: 10,
            passes: 30,
            ..Default::default()
        };
        let now = RenderStats {
            renders: 250,
            deferred: 485,
            blocking: 3,
            blocking_us: 18_000,
            blocking_max_us: 5_120,
            frames: 250,
            passes: 750,
            passes_per_frame: 1.876,
            p50_us: 812,
            max_us: 3_280,
            window_us: 8_333,
            wake_late_us: 1_420,
            gpu_lead_us: 2_500,
            ..Default::default()
        };
        let presents = Presents {
            done: vec![-1_200, -900, 300, -2_600],
            late: vec![3_100, 2_000, 4_000],
            gaps: vec![5, 5, 6, 4, 5],
            ahead: vec![4_100, 3_900, 6_000],
            gpu: vec![300, -200, 2_400],
        };
        let line = render_summary(&prev, &now, Duration::from_secs(10), &presents, &[0.001, 0.002]);
        assert_eq!(
            line,
            "secs=10.0 renders=240 deferred=480 blocking=2 blocking_avg_ms=4.50 blocking_max_ms=5.12 \
             frames=240 passes=720 passes_per_s=72.0 passes_per_frame=1.88 \
             p50_ms=0.81 max_ms=3.28 done_p5_ms=-2.60 done_p50_ms=-0.90 done_p95_ms=0.30 done_p99_ms=0.30 vsyncs=4:1,5:3,6:1 \
             late_p50_ms=3.10 take_p5_ms=3.90 take_p50_ms=4.10 take_p95_ms=6.00 gpu_p50_ms=0.30 gpu_p95_ms=2.40 \
             window_ms=8.33 wake_late_ms=1.42 gpu_lead_ms=2.50 avsync_ms=1.50"
        );
        // 量不到（不是 Windows、沒有預定時間）、這一段沒有等過的寫 -
        let none = RenderStats {
            blocking: prev.blocking,
            blocking_us: prev.blocking_us,
            ..now
        };
        let line = render_summary(&prev, &none, Duration::from_secs(10), &Presents::default(), &[]);
        assert!(line.contains(" blocking=0 blocking_avg_ms=- "), "{line}");
        assert!(
            line.contains(
                "done_p5_ms=- done_p50_ms=- done_p95_ms=- done_p99_ms=- vsyncs=- late_p50_ms=- take_p5_ms=- take_p50_ms=- \
                 take_p95_ms=- \
                 gpu_p50_ms=- gpu_p95_ms=- "
            ),
            "{line}"
        );
        assert!(line.ends_with(" avsync_ms=-"), "{line}");
    }
}
