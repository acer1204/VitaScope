//! 流暢播放的接線：查視窗所在螢幕的更新率、電源，用 `crate::pacing::decide` 算出要怎麼做。
//! 這一版只計算、顯示在媒體資訊面板（「播放流暢度」），不改 mpv 的任何設定。

use super::VitascopeApp;
use crate::pacing::{self as rules, Guard, Inputs, Overrides, Plan, Reason, SmoothMode};
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

/// 流暢播放目前的狀態（媒體資訊面板、之後的設定頁顯示）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PacingStatus {
    /// 算出來的做法；None = 還沒算過（視窗還沒出現）
    pub plan: Option<Plan>,
    pub refresh: Option<Refresh>,
    pub power: PowerSource,
    /// 做法已經套用到 mpv（這一版只計算，一直是 false）
    pub applied: bool,
    /// mpv 正在依螢幕同步
    pub sync_active: bool,
    /// VITASCOPE_PACING=off（不是 VITASCOPE_MPV_OPTS）讓這個功能不動作
    pub pacing_off: bool,
}

impl PacingStatus {
    /// 一行說明，例如「未使用：偵測不到這個螢幕的更新率」
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
            Some(Plan::Display { hz, .. }) if !self.applied => crate::tf!(
                "可以使用：{hz:.3} Hz（尚未啟用）",
                "Available: {hz:.3} Hz (not enabled yet)"
            ),
            Some(Plan::Display { vdrop: true, .. }) => crate::tr!(
                "音訊直通中：以略過或重複影格對齊螢幕",
                "Audio passthrough: matching the screen by dropping or repeating frames"
            )
            .to_owned(),
            Some(Plan::Display { .. }) if !self.sync_active => crate::tr!(
                "已開啟，但 mpv 判斷這部影片不適用",
                "On, but mpv decided this video isn't suitable"
            )
            .to_owned(),
            Some(Plan::Display { hz, .. }) => crate::tf!("使用中：{hz:.3} Hz", "In use: {hz:.3} Hz"),
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
    /// 正在播放（有檔案、沒暫停、不是逐格）
    pub playing: bool,
    pub display_sync_active: bool,
}

/// 流暢播放的控制：記住查到的螢幕、電源，算出做法
pub(super) struct PacingCtl {
    probe: Option<Box<dyn PlatformProbe>>,
    /// 使用者自己指定了 video-sync / display-fps-override，或 VITASCOPE_PACING=off
    env_override: bool,
    refresh: Option<Refresh>,
    monitor_key: Option<u64>,
    power: PowerSource,
    remote: bool,
    guard: Guard,
    /// mpv 不接受設定（之後的批次套用失敗時設）
    apply_failed: bool,
    status: PacingStatus,
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
}

impl PacingCtl {
    pub(super) fn new(probe: Option<Box<dyn PlatformProbe>>, user_set: bool, overrides: Overrides) -> Self {
        Self {
            probe,
            env_override: user_set || overrides.off,
            refresh: None,
            monitor_key: None,
            power: PowerSource::Unknown,
            remote: false,
            guard: Guard::default(),
            apply_failed: false,
            status: PacingStatus {
                pacing_off: overrides.off,
                ..Default::default()
            },
            key_polled: None,
            refresh_queried: None,
            power_polled: None,
            rect: None,
            rect_moved: None,
            fullscreen: None,
            monitor_size: None,
            sampling: false,
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

    /// 每一幀：依需要重查螢幕、電源，再算一次做法
    pub(super) fn tick(&mut self, now: Instant, view: &View, snap: &Snapshot) {
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
                self.power = power;
                self.remote = remote;
            }
        }
        // 換螢幕、換更新率：之前量到的結果不算
        if self.guard.set_display(self.monitor_key, self.refresh.map(|r| r.hz)) && rules::debug() {
            eprintln!("[vitascope] 流暢播放：換了螢幕或更新率，重新判斷");
        }
        let inputs = Inputs {
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
        let plan = rules::decide(&inputs, self.status.plan.as_ref());
        if Some(plan) != self.status.plan && rules::debug() {
            eprintln!("[vitascope] 流暢播放：{plan:?}");
        }
        // 防呆只在真的依螢幕同步時量（這一版還不套用，所以不會發生；接線先接好）
        let sampling = plan.is_display() && snap.display_sync_active && snap.playing && view.visible;
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
            eprintln!("[vitascope] 流暢播放：{verdict:?}");
        }
        self.status.plan = Some(plan);
        self.status.refresh = self.refresh;
        self.status.power = self.power;
        self.status.sync_active = snap.display_sync_active;
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

    /// 每一幀算一次流暢播放的做法（視窗出現之後）
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
            playing: st.loaded && !st.paused && !self.frame_stepping,
            display_sync_active: st.display_sync_active,
        };
        self.pacing.tick(Instant::now(), &view, &snap);
    }
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
            playing: true,
            display_sync_active: false,
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
        assert_eq!(ctl.status().describe(), "可以使用：119.880 Hz（尚未啟用）");
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

        // mpv 沒在依螢幕同步（這一版一直是這樣）：不量
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
}
