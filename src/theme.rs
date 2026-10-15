//! 外觀：深色 / 淺色 / 跟隨系統，以及介面自己畫的顏色（控制列、播放清單、進度條…）。
//!
//! - 視窗、選單、設定視窗跟著主題；自己畫的顏色從 [`Palette`] 拿，不要寫死 `Color32::from_gray`。
//! - 影片畫面和蓋在影片上的東西（OSD、媒體資訊、進度條的提示與預覽、全螢幕的控制列、純音訊的歌名）
//!   兩種主題都是深色：有 `Ui` 的用 [`dark_overlay`]，直接畫的用固定的深色。
//! - egui 的 `set_visuals` 只改「目前」那個主題的樣式，而 eframe 預設跟隨系統：
//!   以前啟動時設深色，系統是淺色的 Windows、macOS 第一幀就換成 egui 的淺色樣式，蓋在寫死的深色面板上。
//!   所以一定要明確設定主題偏好（[`apply`]）。
//! - 跟隨系統：Windows、macOS 由 winit 回報系統的深淺色；Linux 回報不了，從桌面的設定 portal 問（`portal`，
//!   回答當成 egui 的 `fallback_theme`）。標題列（系統畫的邊框）的深淺色見 [`TitleBar`]；
//!   macOS 換回跟隨系統時 winit 回報的可能是舊的，見 [`StaleSystemTheme`]。

use eframe::egui::{self, Color32, SystemTheme, Theme, ThemePreference, Visuals};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 使用者選的外觀（「設定 → 一般 → 外觀」）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    /// 預設深色（影片播放器多半是深色；升級後外觀不變）
    #[default]
    Dark,
    Light,
    /// 跟隨系統：Windows、macOS 由 egui 依系統回報；Linux 問桌面的設定 portal（問不到時用深色）
    System,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::Dark, ThemeChoice::Light, ThemeChoice::System];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::Dark => crate::tr!("深色", "Dark"),
            ThemeChoice::Light => crate::tr!("淺色", "Light"),
            ThemeChoice::System => crate::tr!("跟隨系統", "Follow the system"),
        }
    }

    /// 對應的 egui 主題偏好
    pub fn preference(self) -> ThemePreference {
        match self {
            ThemeChoice::Dark => ThemePreference::Dark,
            ThemeChoice::Light => ThemePreference::Light,
            ThemeChoice::System => ThemePreference::System,
        }
    }

    /// 標題列（系統畫的邊框）要的深淺色
    pub fn window_theme(self) -> SystemTheme {
        match self {
            ThemeChoice::Dark => SystemTheme::Dark,
            ThemeChoice::Light => SystemTheme::Light,
            ThemeChoice::System => SystemTheme::SystemDefault,
        }
    }

    /// 「切換深色／淺色」：深色 ↔ 淺色；跟隨系統時換成跟現在看到的（`shown`）相反的那個
    pub fn toggled(self, shown: Theme) -> Self {
        let now = match self {
            ThemeChoice::Dark => Theme::Dark,
            ThemeChoice::Light => Theme::Light,
            ThemeChoice::System => shown,
        };
        match now {
            Theme::Dark => ThemeChoice::Light,
            Theme::Light => ThemeChoice::Dark,
        }
    }
}

/// 跟隨系統卻問不到系統的主題時（Linux、沒有視窗的自動測試）用的主題
pub const FALLBACK: Theme = Theme::Dark;

/// 啟動時兩種主題都放 egui 的標準樣式（`set_visuals` 只會改到目前那一個）
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Dark, Visuals::dark());
    ctx.set_visuals_of(Theme::Light, Visuals::light());
}

/// 套用使用者選的外觀（啟動時、改設定時；有另外問系統的深淺色時用 [`apply_with`]）
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    apply_with(ctx, choice, None);
}

/// 套用使用者選的外觀。`reported` = winit 以外的地方回報的系統深淺色（Linux 的 portal）：
/// 跟隨系統時 egui 先看 winit 回報的（Linux 一律沒有），沒有才用 `fallback_theme`，所以放在那裡；問不到時用深色。
/// 標題列由 egui 送 `SetTheme` 跟著換，Windows、Linux 另外要處理的見 [`TitleBar`]
pub fn apply_with(ctx: &egui::Context, choice: ThemeChoice, reported: Option<Theme>) {
    ctx.options_mut(|o| {
        o.theme_preference = choice.preference();
        o.fallback_theme = reported.unwrap_or(FALLBACK);
    });
    ctx.request_repaint();
}

/// 選了跟隨系統、但作業系統沒有回報深淺色（Linux 的 winit 都是這樣；還要看 portal 回答了沒）：設定頁提示暫時用深色
pub fn system_unknown(ctx: &egui::Context, choice: ThemeChoice) -> bool {
    choice == ThemeChoice::System && ctx.system_theme().is_none()
}

/// 桌面的設定 portal 的 `color-scheme`：1 = 深色；0（沒有偏好，GNOME 的「預設」樣式）、2 = 淺色；其他的不認得
pub fn scheme_theme(scheme: u32) -> Option<Theme> {
    match scheme {
        1 => Some(Theme::Dark),
        0 | 2 => Some(Theme::Light),
        _ => None,
    }
}

/// 跟隨系統時，winit 回報不了系統深淺色的平台另外問的地方（Linux：桌面的設定 portal，`portal::Portal`）。
/// 由 `Launch.theme_source` 給 App（只有 main.rs 接上真的；自動測試、`--shot` 沒有，測試用假的）
pub trait ThemeSource: Send {
    /// 開始注意系統的深淺色（第一次選跟隨系統時才叫，之後一直注意）。回答了、改變了就叫 `wake`（請介面重畫）
    fn watch(&mut self, wake: crate::instance::Wake);
    /// 目前知道的系統深淺色；None = 還沒回答、問不到（介面每一幀都問：只讀已經知道的值）
    fn get(&self) -> Option<Theme>;
}

/// 標題列（系統畫的視窗邊框）的深淺色：哪一幀要自己送 `ViewportCommand::SetTheme`。
/// egui 只在主題偏好變了時送（`sync_window_theme`，有快取），跟隨系統時送的是「系統預設」。
/// - Windows：winit 設深淺色時沒有記下來，系統每次廣播設定變更（`WM_SETTINGCHANGE`，常常發生）就把標題列改回系統的顏色
///   （P-B9）：選了深色或淺色時，視窗重新取得焦點就再送一次（繞過 egui 的快取）。
/// - Linux：系統的深淺色是 App 自己問的（portal），X11 的「系統預設」一律是深色（`_GTK_THEME_VARIANT`）：
///   由 App 自己送看到的深淺色，egui 的同步關掉（P-C10，`own`）。
#[derive(Debug, Default)]
pub struct TitleBar {
    /// App 自己管標題列（egui 的同步關掉）：看到的深淺色變了就送
    own: bool,
    /// 重新取得焦點時再送一次（Windows）
    refocus: bool,
    /// 自己送過的（`own` 時）
    sent: Option<SystemTheme>,
    focused: Option<bool>,
}

impl TitleBar {
    pub fn new(own: bool, refocus: bool) -> Self {
        Self {
            own,
            refocus,
            ..Default::default()
        }
    }

    /// App 自己管標題列（要關掉 egui 的 `sync_window_theme`）
    pub fn owned(&self) -> bool {
        self.own
    }

    /// 每一幀：`shown` = 現在看到的主題，`focused` = 視窗有沒有焦點（`ViewportInfo::focused`）。回傳這一幀要送的 `SetTheme`
    pub fn tick(&mut self, choice: ThemeChoice, shown: Theme, focused: Option<bool>) -> Option<SystemTheme> {
        let regained = self.focused == Some(false) && focused == Some(true);
        self.focused = focused;
        if self.own {
            let want = match shown {
                Theme::Dark => SystemTheme::Dark,
                Theme::Light => SystemTheme::Light,
            };
            if self.sent == Some(want) {
                return None;
            }
            self.sent = Some(want);
            return Some(want);
        }
        // 跟隨系統：系統的廣播本來就是對的（標題列跟著系統）
        (self.refocus && regained && choice != ThemeChoice::System).then(|| choice.window_theme())
    }
}

/// 讀系統現在的外觀的方法（macOS：NSApp 的 `effectiveAppearance`；讀不到時 None）
pub type OsAppearance = Arc<dyn Fn() -> Option<Theme> + Send + Sync>;

/// macOS：強制深色或淺色的期間（視窗設了 `appearance`）winit 不理系統的外觀變化；換回跟隨系統時，
/// winit 只在它的說法有變時才通知（舊值 ≠ 新值），所以「淺色啟動 → 選深色 → 系統改成深色 → 選跟隨系統」
/// 之後 egui 的 `system_theme` 一直是淺色（P-C9）。
///
/// 換回跟隨系統的那一刻讀一次系統的外觀，跟 winit 回報的不一樣就在 `raw_input_hook` 蓋掉 winit 回報的，
/// 直到 winit 回報的值變了（收到真的 ThemeChanged，之後 winit 就對了）。蓋掉的期間每一批輸入都再讀一次：
/// 系統改回 winit 那個舊的值時 winit 也會通知，但值跟舊的一樣看不出來，只讀一次的話會一直停在讀到的那個
#[derive(Debug, Default)]
pub struct StaleSystemTheme {
    /// 最近一批輸入裡 winit 回報的（還沒蓋掉的）
    raw: Option<Theme>,
    /// 蓋掉中：開始時 winit 回報的（舊的）值
    stale: Option<Option<Theme>>,
}

impl StaleSystemTheme {
    /// 換回跟隨系統：`now` = 剛讀到的系統外觀（讀不到時 None）
    pub fn start(&mut self, now: Option<Theme>) {
        self.stale = match now {
            // 跟 winit 回報的一樣：winit 是對的，之後的變化它也會通知
            Some(now) if Some(now) != self.raw => Some(self.raw),
            _ => None,
        };
    }

    /// 不再跟隨系統（強制深色或淺色時 egui 不看系統的）
    pub fn stop(&mut self) {
        self.stale = None;
    }

    /// 正在蓋掉 winit 回報的值
    pub fn active(&self) -> bool {
        self.stale.is_some()
    }

    /// 每一批輸入（`raw_input_hook`）：`system_theme` 是 winit 回報的，蓋掉中改成 `read()` 讀到的
    pub fn hook(&mut self, system_theme: &mut Option<Theme>, read: impl FnOnce() -> Option<Theme>) {
        self.raw = *system_theme;
        let Some(stale) = self.stale else { return };
        if *system_theme != stale {
            // winit 收到真的變化了：之後照它的
            self.stale = None;
            return;
        }
        if let Some(now) = read() {
            *system_theme = Some(now);
        }
    }
}

/// 這個平台讀系統外觀的方法（只有 macOS 需要：見 [`StaleSystemTheme`]）
pub fn os_appearance() -> Option<OsAppearance> {
    #[cfg(target_os = "macos")]
    return Some(Arc::new(macos::app_appearance));
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
mod macos {
    use eframe::egui::Theme;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication};
    use objc2_foundation::NSArray;

    /// NSApp 現在的外觀（系統設定的深淺色；跟視窗設的 `appearance` 無關）。不是主執行緒時 None
    pub fn app_appearance() -> Option<Theme> {
        let mtm = MainThreadMarker::new()?;
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: AppKit 的常數字串（macOS 10.14 起都有）
        let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
        let names = NSArray::from_slice(&[aqua, dark]);
        let best = app.effectiveAppearance().bestMatchFromAppearancesWithNames(&names)?;
        Some(if &*best == dark { Theme::Dark } else { Theme::Light })
    }
}

/// 蓋在影片上的 `Ui`：不管目前的主題，一律用深色的樣式（影片是黑的，淺色的字和按鈕底色看不清楚）
pub fn dark_overlay(ui: &mut egui::Ui) {
    let dark = ui.ctx().style_of(Theme::Dark).visuals.clone();
    *ui.visuals_mut() = dark;
}

/// 介面自己畫的顏色（依目前 `Ui` 的樣式選深色或淺色的一組）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// 影片畫面的底色：兩種主題都是黑色
    pub video: Color32,
    /// 視窗模式的控制列
    pub panel: Color32,
    /// 播放清單面板
    pub side: Color32,
    /// 進度條的底
    pub track: Color32,
    /// 進度條上的章節間隔（跟控制列同色，看起來像切開）
    pub tick: Color32,
    /// 強調色：已播放的進度、播放中的項目、播放速度
    pub accent: Color32,
    /// 進度條上的 A-B 重播區段（半透明疊在進度條上）、只設了起點時的那條線
    pub ab: Color32,
    /// 進度條上方的書籤標記（小三角形）
    pub bookmark: Color32,
    /// 有問題的檔案、錯誤訊息
    pub problem: Color32,
    /// 次要的小字（例如「關於」的授權說明）
    pub faint: Color32,
}

impl Palette {
    /// 深色：跟以前寫死的顏色一樣
    const DARK: Palette = Palette {
        video: Color32::BLACK,
        panel: Color32::from_gray(24),
        side: Color32::from_gray(28),
        track: Color32::from_gray(70),
        tick: Color32::from_gray(24),
        accent: Color32::from_rgb(0x4f, 0x9d, 0xff),
        ab: Color32::from_rgb(0xff, 0xc1, 0x07),
        bookmark: Color32::from_rgb(0x66, 0xbb, 0x6a),
        problem: Color32::from_rgb(0xff, 0x8a, 0x80),
        faint: Color32::from_gray(150),
    };

    /// 淺色：淺底上的強調色、錯誤色要深一點才看得清楚
    fn light(v: &Visuals) -> Palette {
        Palette {
            video: Color32::BLACK,
            panel: v.panel_fill,
            side: Color32::from_gray(240),
            track: Color32::from_gray(200),
            tick: v.panel_fill,
            accent: Color32::from_rgb(0x1a, 0x6f, 0xd8),
            // 原本的琥珀色（#FFC107）跟淺灰的進度條底差不多亮，淺色主題看不出區段：換深一點的琥珀色
            ab: Color32::from_rgb(0xa6, 0x6f, 0x00),
            // 淺底上原本的綠色太淡：深一點的綠
            bookmark: Color32::from_rgb(0x2e, 0x7d, 0x32),
            problem: Color32::from_rgb(0xc6, 0x28, 0x28),
            faint: Color32::from_gray(110),
        }
    }

    pub fn of(visuals: &Visuals) -> Palette {
        if visuals.dark_mode {
            Self::DARK
        } else {
            Self::light(visuals)
        }
    }
}

/// 介面上用到的圖示字元（按鈕、選單、提示）。只能用 egui 內建字型（一般文字的字體）有的字，
/// 不然在沒有那些字的系統字型上會變成方塊（豆腐字）：新增圖示時加在這裡，`icons_have_glyphs` 會檢查。
/// - ⧉ ⤢ ✎ ⏻ ⏯ ＋（全形）內建字型沒有，不要用。
/// - 像素著色器組合裡的 ✕ ↑ ↓ 也不在內建字型裡（現在靠系統的中文字型畫出來），以後換圖示時再加進來；
///   ← → ↑ ↓ 在快捷鍵說明裡是按鍵名稱（文字），不算圖示。
pub const ICONS: &[&str] = &[
    "⏮", "⏸", "▶", "⏭", "⏹", "⛶", "🗁", "ℹ", "☰", "🔇", "🔊", "↺", "◀", "⚠", "♪", "×", "•", "🔀", "🔁", "🔂", "⏳", "🗗",
    "🗖", "▣", "✔",
];

/// Linux：桌面的設定 portal（`org.freedesktop.portal.Settings`）的深淺色（`org.freedesktop.appearance` 的 `color-scheme`）。
/// winit 在 Linux 不回報系統的深淺色（X11、Wayland 都沒有 ThemeChanged）；GNOME、KDE 等桌面都透過 portal 提供。
/// - 背景執行緒 `vitascope-theme`，同步的 `zbus::blocking`。先訂閱 `SettingChanged`（只要這一項），再讀現在的值：
///   讀和訂閱之間改的設定不會漏掉。讀用 `ReadOne`；舊的 portal（設定介面第 1 版）沒有，改用 `Read`（值多包一層 variant）。
/// - 匯流排斷線（重新登入、dbus-daemon 重開）：保留最後的值，30 秒後再連；連續兩次都問不到就不再試（沒有 portal 的桌面）。
/// - 測試只連自己開的 dbus-daemon（[`Portal::at`]），絕不是使用者的工作階段匯流排；只有 main.rs 建立 [`Portal::session`]。
#[cfg(all(unix, not(target_os = "macos")))]
pub mod portal {
    use super::{Theme, ThemeSource, scheme_theme};
    use crate::instance::Wake;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use zbus::blocking::Connection;
    use zbus::blocking::connection::Builder;
    use zbus::blocking::proxy::{Builder as ProxyBuilder, Proxy};
    use zbus::proxy::CacheProperties;
    use zbus::zvariant::{OwnedValue, Value};

    pub const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    pub const PATH: &str = "/org/freedesktop/portal/desktop";
    pub const INTERFACE: &str = "org.freedesktop.portal.Settings";
    pub const NAMESPACE: &str = "org.freedesktop.appearance";
    pub const KEY: &str = "color-scheme";
    /// 正常是幾毫秒；portal 卡住時不要讓背景執行緒一直等
    const CALL_TIMEOUT: Duration = Duration::from_secs(5);
    /// 斷線、問不到之後多久再試（不要一直重試）
    const RETRY_AFTER: Duration = Duration::from_secs(30);

    /// portal 回的值拆成 `color-scheme` 的數字：`ReadOne` 回 `v`（裡面是 u32），舊的 `Read` 回 `v(v)`（多包一層）
    pub fn color_scheme(value: &Value<'_>) -> Option<u32> {
        match value {
            Value::Value(inner) => color_scheme(inner),
            Value::U32(n) => Some(*n),
            _ => None,
        }
    }

    /// portal 回的值對應的深淺色（不認得的值 None）
    pub fn theme_of(value: &Value<'_>) -> Option<Theme> {
        scheme_theme(color_scheme(value)?)
    }

    /// `SettingChanged` 訊號：是深淺色這一項時回傳新的值（Some(None) = 不認得的值），別的設定 None
    pub fn setting_changed(msg: &zbus::Message) -> Option<Option<Theme>> {
        let (namespace, key, value) = msg.body().deserialize::<(String, String, OwnedValue)>().ok()?;
        (namespace == NAMESPACE && key == KEY).then(|| theme_of(&value))
    }

    fn encode(theme: Option<Theme>) -> u8 {
        match theme {
            None => 0,
            Some(Theme::Dark) => 1,
            Some(Theme::Light) => 2,
        }
    }

    fn decode(v: u8) -> Option<Theme> {
        match v {
            1 => Some(Theme::Dark),
            2 => Some(Theme::Light),
            _ => None,
        }
    }

    /// 背景執行緒和 [`Portal`] 共用的
    #[derive(Default)]
    struct Shared {
        /// 0 = 不知道；1 深色；2 淺色
        value: AtomicU8,
        /// 讀了幾次現在的值（有沒有回答都算；測試等它）
        reads: AtomicU32,
        /// 執行緒還在跑
        running: AtomicBool,
        /// 目前的連線：結束時關掉，等訊號的執行緒才會醒來
        conn: Mutex<Option<Connection>>,
    }

    /// 桌面的設定 portal（見模組說明）。[`ThemeSource::watch`] 才開始連線；丟掉時關掉連線、背景執行緒結束
    pub struct Portal {
        /// None = 使用者的工作階段匯流排；測試是自己開的 dbus-daemon
        address: Option<String>,
        /// 斷線、問不到之後多久再試（產品一律 [`RETRY_AFTER`]；測試縮短才等得到重試與放棄）
        retry_after: Duration,
        shared: Arc<Shared>,
        /// 丟掉時（`Portal` 結束）等著重試的執行緒馬上醒來結束
        stop: Option<Sender<()>>,
    }

    impl Portal {
        /// 使用者的工作階段匯流排（只有 main.rs 用）
        pub fn session() -> Self {
            Self::new(None)
        }

        /// 測試用：連到指定的匯流排（測試自己開的 dbus-daemon，絕不是使用者的）
        pub fn at(address: &str) -> Self {
            Self::new(Some(address.to_owned()))
        }

        fn new(address: Option<String>) -> Self {
            Self {
                address,
                retry_after: RETRY_AFTER,
                shared: Arc::default(),
                stop: None,
            }
        }

        /// 測試用：斷線、問不到之後多久再試（產品是 30 秒）
        #[doc(hidden)]
        pub fn retry_after(mut self, wait: Duration) -> Self {
            self.retry_after = wait;
            self
        }

        /// 讀了幾次現在的值（連不上、沒有 portal 也算一次）
        pub fn reads(&self) -> u32 {
            self.shared.reads.load(Ordering::SeqCst)
        }

        /// 背景執行緒還在跑（還沒開始、已經放棄、結束了都是 false）
        pub fn running(&self) -> bool {
            self.shared.running.load(Ordering::SeqCst)
        }

        /// 測試用：背景執行緒的「還在跑」（`Portal` 丟掉之後也看得到）
        #[doc(hidden)]
        pub fn running_flag(&self) -> impl Fn() -> bool + Send + 'static {
            let shared = self.shared.clone();
            move || shared.running.load(Ordering::SeqCst)
        }
    }

    impl ThemeSource for Portal {
        fn watch(&mut self, wake: Wake) {
            if self.stop.is_some() {
                return;
            }
            let (tx, rx) = mpsc::channel();
            self.stop = Some(tx);
            let address = self.address.clone();
            let retry_after = self.retry_after;
            let shared = self.shared.clone();
            shared.running.store(true, Ordering::SeqCst);
            let spawned = std::thread::Builder::new().name("vitascope-theme".into()).spawn({
                let shared = shared.clone();
                move || {
                    run(address.as_deref(), retry_after, &shared, &rx, &wake);
                    shared.running.store(false, Ordering::SeqCst);
                }
            });
            if let Err(e) = spawned {
                eprintln!("[vitascope] 無法建立查詢系統深淺色的執行緒：{e}");
                shared.running.store(false, Ordering::SeqCst);
            }
        }

        fn get(&self) -> Option<Theme> {
            decode(self.shared.value.load(Ordering::SeqCst))
        }
    }

    impl Drop for Portal {
        fn drop(&mut self) {
            // 先放掉 stop（執行緒之後放上來的連線會自己關掉），再關掉目前的連線（等訊號的執行緒醒來）
            self.stop = None;
            let conn = self.shared.conn.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(conn) = conn {
                let _ = conn.close();
            }
        }
    }

    fn stopped(stop: &Receiver<()>) -> bool {
        matches!(stop.try_recv(), Err(TryRecvError::Disconnected))
    }

    fn run(address: Option<&str>, retry_after: Duration, shared: &Shared, stop: &Receiver<()>, wake: &Wake) {
        let mut misses = 0;
        let mut logged = false;
        loop {
            let answered = session(address, shared, stop, wake, &mut logged);
            if stopped(stop) {
                return;
            }
            misses = if answered { 0 } else { misses + 1 };
            if misses >= 2 {
                return;
            }
            match stop.recv_timeout(retry_after) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return,
            }
        }
    }

    /// 連一次：讀現在的值，之後跟著 `SettingChanged`，直到斷線。回傳 portal 有沒有回答
    fn session(address: Option<&str>, shared: &Shared, stop: &Receiver<()>, wake: &Wake, logged: &mut bool) -> bool {
        let mut log = |what: &str, e: &zbus::Error| {
            if !std::mem::replace(logged, true) {
                eprintln!("[vitascope] 偵測不到系統的深淺色（{what}：{e}）");
            }
        };
        // 先放上值、再數、再叫醒：介面醒來時一定讀得到新的值（測試要等的是自己檢查的那一項，不能只等值）
        let read_done = || {
            shared.reads.fetch_add(1, Ordering::SeqCst);
            wake();
        };
        let builder = match address {
            Some(a) => Builder::address(a),
            None => Builder::session(),
        };
        let conn = match builder.and_then(|b| b.method_timeout(CALL_TIMEOUT).build()) {
            Ok(conn) => conn,
            Err(e) => {
                log("D-Bus 的工作階段匯流排", &e);
                read_done();
                return false;
            }
        };
        *shared.conn.lock().unwrap_or_else(|e| e.into_inner()) = Some(conn.clone());
        // `Portal` 已經丟掉了（關連線的時候這條還沒放上去）：自己關
        if stopped(stop) {
            let _ = conn.close();
            return false;
        }
        let proxy = match ProxyBuilder::<Proxy>::new(&conn)
            .destination(DESTINATION)
            .and_then(|b| b.path(PATH))
            .and_then(|b| b.interface(INTERFACE))
            .and_then(|b| b.cache_properties(CacheProperties::No).build())
        {
            Ok(proxy) => proxy,
            Err(e) => {
                log("portal", &e);
                read_done();
                return false;
            }
        };
        // 先訂閱再讀：中間改的設定不會漏掉
        let signals = proxy.receive_signal_with_args("SettingChanged", &[(0, NAMESPACE), (1, KEY)]);
        match read(&proxy) {
            Ok(theme) => {
                shared.value.store(encode(theme), Ordering::SeqCst);
                read_done();
            }
            Err(e) => {
                log("portal", &e);
                read_done();
                return false;
            }
        }
        match signals {
            Ok(signals) => {
                for msg in signals {
                    if let Some(theme) = setting_changed(&msg) {
                        shared.value.store(encode(theme), Ordering::SeqCst);
                        wake();
                    }
                }
            }
            // 訂閱不了（不會發生）：30 秒後重新連線時再讀一次
            Err(e) => log("SettingChanged", &e),
        }
        true
    }

    /// 現在的值：`ReadOne`，舊的 portal 沒有時用 `Read`
    fn read(proxy: &Proxy<'_>) -> zbus::Result<Option<Theme>> {
        let args = (NAMESPACE, KEY);
        let value: OwnedValue = match proxy.call("ReadOne", &args) {
            Ok(value) => value,
            Err(_) => proxy.call("Read", &args)?,
        };
        Ok(theme_of(&value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 亮度（粗略）：判斷顏色是深還是淺
    fn luma(c: Color32) -> u32 {
        (u32::from(c.r()) * 299 + u32::from(c.g()) * 587 + u32::from(c.b()) * 114) / 1000
    }

    #[test]
    fn serialized_names() {
        for (choice, name) in [
            (ThemeChoice::Dark, "\"dark\""),
            (ThemeChoice::Light, "\"light\""),
            (ThemeChoice::System, "\"system\""),
        ] {
            assert_eq!(serde_json::to_string(&choice).unwrap(), name);
            assert_eq!(serde_json::from_str::<ThemeChoice>(name).unwrap(), choice);
        }
        assert_eq!(ThemeChoice::default(), ThemeChoice::Dark, "預設深色");
    }

    #[test]
    fn labels_in_both_languages() {
        let zh: Vec<&str> = ThemeChoice::ALL.iter().map(|c| c.label()).collect();
        assert_eq!(zh, ["深色", "淺色", "跟隨系統"]);
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let en: Vec<&str> = ThemeChoice::ALL.iter().map(|c| c.label()).collect();
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(en, ["Dark", "Light", "Follow the system"]);
    }

    #[test]
    fn toggled_switches_between_dark_and_light() {
        assert_eq!(ThemeChoice::Dark.toggled(Theme::Dark), ThemeChoice::Light);
        assert_eq!(ThemeChoice::Light.toggled(Theme::Light), ThemeChoice::Dark);
        // 跟隨系統：換成跟現在看到的相反
        assert_eq!(ThemeChoice::System.toggled(Theme::Light), ThemeChoice::Dark);
        assert_eq!(ThemeChoice::System.toggled(Theme::Dark), ThemeChoice::Light);
    }

    #[test]
    fn apply_sets_the_preference_and_the_fallback() {
        let ctx = egui::Context::default();
        ctx.options_mut(|o| o.fallback_theme = Theme::Light);
        for choice in ThemeChoice::ALL {
            apply(&ctx, choice);
            let (pref, fallback) = ctx.options(|o| (o.theme_preference, o.fallback_theme));
            assert_eq!(pref, choice.preference());
            assert_eq!(fallback, Theme::Dark, "問不到系統時用深色");
        }
        // 問不到系統（沒有回報）：跟隨系統 = 深色
        apply(&ctx, ThemeChoice::System);
        assert_eq!(ctx.theme(), Theme::Dark);
        assert!(system_unknown(&ctx, ThemeChoice::System));
        assert!(!system_unknown(&ctx, ThemeChoice::Light));
        apply(&ctx, ThemeChoice::Light);
        assert_eq!(ctx.theme(), Theme::Light);
    }

    #[test]
    fn install_keeps_both_styles_standard() {
        let ctx = egui::Context::default();
        // 以前的寫法：只改到目前的主題
        ctx.set_visuals_of(Theme::Light, Visuals::dark());
        install(&ctx);
        assert!(ctx.style_of(Theme::Dark).visuals.dark_mode);
        assert!(!ctx.style_of(Theme::Light).visuals.dark_mode);
    }

    #[test]
    fn palettes_differ_but_the_video_stays_black() {
        let dark = Palette::of(&Visuals::dark());
        let light = Palette::of(&Visuals::light());
        assert_eq!(dark.video, Color32::BLACK);
        assert_eq!(light.video, Color32::BLACK, "淺色主題的影片畫面也是黑的");
        assert_ne!(dark.panel, light.panel);
        assert_ne!(dark.track, light.track);
        assert_ne!(dark.problem, light.problem);
        // 深色跟以前寫死的顏色一樣（升級後外觀不變）
        assert_eq!(dark.panel, Color32::from_gray(24));
        assert_eq!(dark.side, Color32::from_gray(28));
        assert_eq!(dark.track, Color32::from_gray(70));
        assert_eq!(dark.problem, Color32::from_rgb(0xff, 0x8a, 0x80));
        assert_eq!(dark.accent, Color32::from_rgb(0x4f, 0x9d, 0xff));
        assert_eq!(dark.ab, Color32::from_rgb(0xff, 0xc1, 0x07));
        // 深色的控制列是深的、淺色是淺的；章節間隔跟控制列同色
        assert!(luma(dark.panel) < 60 && luma(dark.side) < 60);
        assert!(luma(light.panel) > 200 && luma(light.side) > 200);
        assert_eq!(dark.tick, dark.panel);
        assert_eq!(light.tick, light.panel);
        // 進度條的底跟控制列看得出差別；淺底上的錯誤色、強調色要夠深
        for p in [dark, light] {
            assert!(luma(p.panel).abs_diff(luma(p.track)) >= 40, "{p:?}");
        }
        // A-B 區段（六成不透明疊在進度條的底上）要看得出來：疊上去後亮度至少差 40
        for p in [dark, light] {
            let over = luma(p.ab) * 6 / 10 + luma(p.track) * 4 / 10;
            assert!(over.abs_diff(luma(p.track)) >= 40, "{p:?}");
        }
        assert!(luma(light.problem) < 110 && luma(light.accent) < 130, "{light:?}");
        // 書籤標記畫在控制列的底上：深色是亮的綠、淺色是深的綠，跟底色看得出差別
        assert_eq!(dark.bookmark, Color32::from_rgb(0x66, 0xbb, 0x6a));
        for p in [dark, light] {
            assert!(luma(p.panel).abs_diff(luma(p.bookmark)) >= 80, "{p:?}");
        }
        assert!(luma(light.faint) < luma(light.panel) - 100);
        assert!(luma(dark.faint) > luma(dark.panel) + 100);
    }

    #[test]
    fn dark_overlay_forces_dark_visuals_in_the_light_theme() {
        let ctx = egui::Context::default();
        install(&ctx);
        apply(&ctx, ThemeChoice::Light);
        let mut inside = None;
        let mut outside = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            outside = Some(ui.visuals().dark_mode);
            ui.scope(|ui| {
                dark_overlay(ui);
                inside = Some((ui.visuals().dark_mode, Palette::of(ui.visuals())));
            });
        });
        output.textures_delta.clear();
        assert_eq!(outside, Some(false), "外面是淺色");
        let (dark_mode, palette) = inside.unwrap();
        assert!(dark_mode, "蓋在影片上的是深色");
        assert_eq!(palette, Palette::DARK);
    }

    #[test]
    fn icons_have_glyphs() {
        use egui::epaint::text::{Fonts, TextOptions};
        // egui 內建的字型（不含系統的中文字型）：圖示要在這裡面，不然在沒有那些字型的電腦上會是方塊。
        // 不能用 `Fonts::has_glyphs`：跟「替代字元」在同一個字型裡的字（例如 ▶ ← ✕）它也回答沒有。
        // 改成實際排版，看畫出來的是不是替代字元的那一格
        let mut fonts = Fonts::new(TextOptions::default(), egui::FontDefinitions::default());
        let mut drawn_as = |c: char| {
            let galley = fonts.with_pixels_per_point(1.0).layout_no_wrap(
                c.to_string(),
                egui::FontId::proportional(16.0),
                Color32::WHITE,
            );
            galley.rows[0].row.glyphs[0].uv_rect
        };
        // 一定沒有的字（私人使用區）畫出來就是替代字元
        let replacement = drawn_as('\u{10FFFD}');
        let mut missing: Vec<char> = ICONS
            .iter()
            .flat_map(|s| s.chars())
            .filter(|&c| drawn_as(c) == replacement)
            .collect();
        missing.dedup();
        assert!(missing.is_empty(), "內建字型沒有這些圖示：{missing:?}");
        // 檢查本身有效：已知沒有的字要被找出來
        for absent in ['⧉', '⤢', '✎', '⏻', '⏯', '＋', '✕'] {
            assert_eq!(drawn_as(absent), replacement, "{absent}");
        }
    }

    #[test]
    fn portal_color_scheme_values() {
        // 1 = 深色；0（沒有偏好：GNOME 的「預設」是淺色）、2 = 淺色；其他的不認得
        assert_eq!(scheme_theme(1), Some(Theme::Dark));
        assert_eq!(scheme_theme(0), Some(Theme::Light));
        assert_eq!(scheme_theme(2), Some(Theme::Light));
        assert_eq!(scheme_theme(3), None);
        assert_eq!(scheme_theme(u32::MAX), None);
    }

    #[test]
    fn window_theme_of_each_choice() {
        assert_eq!(ThemeChoice::Dark.window_theme(), SystemTheme::Dark);
        assert_eq!(ThemeChoice::Light.window_theme(), SystemTheme::Light);
        assert_eq!(ThemeChoice::System.window_theme(), SystemTheme::SystemDefault);
    }

    #[test]
    fn apply_with_puts_the_reported_theme_in_the_fallback() {
        let ctx = egui::Context::default();
        // Linux 的 portal 說淺色：跟隨系統是淺色（winit 沒有回報）
        apply_with(&ctx, ThemeChoice::System, Some(Theme::Light));
        assert_eq!(ctx.options(|o| o.fallback_theme), Theme::Light);
        assert_eq!(ctx.theme(), Theme::Light);
        apply_with(&ctx, ThemeChoice::System, Some(Theme::Dark));
        assert_eq!(ctx.theme(), Theme::Dark);
        // 強制的選擇不看系統的
        apply_with(&ctx, ThemeChoice::Dark, Some(Theme::Light));
        assert_eq!(ctx.theme(), Theme::Dark);
        apply_with(&ctx, ThemeChoice::Light, Some(Theme::Dark));
        assert_eq!(ctx.theme(), Theme::Light);
        // 問不到：深色
        apply_with(&ctx, ThemeChoice::System, None);
        assert_eq!(ctx.options(|o| o.fallback_theme), Theme::Dark);
        assert_eq!(ctx.theme(), Theme::Dark);
    }

    #[test]
    fn title_bar_owned_sends_what_is_shown_once() {
        // Linux：App 自己送看到的深淺色（X11 的「系統預設」一律是深色）
        let mut bar = TitleBar::new(true, false);
        assert!(bar.owned());
        assert_eq!(
            bar.tick(ThemeChoice::System, Theme::Dark, None),
            Some(SystemTheme::Dark)
        );
        assert_eq!(bar.tick(ThemeChoice::System, Theme::Dark, None), None, "沒變不重送");
        // portal 改成淺色：跟隨系統送的是淺色，不是「系統預設」
        assert_eq!(
            bar.tick(ThemeChoice::System, Theme::Light, Some(true)),
            Some(SystemTheme::Light)
        );
        assert_eq!(bar.tick(ThemeChoice::System, Theme::Light, Some(false)), None);
        assert_eq!(
            bar.tick(ThemeChoice::System, Theme::Light, Some(true)),
            None,
            "焦點不重送"
        );
        assert_eq!(
            bar.tick(ThemeChoice::Dark, Theme::Dark, Some(true)),
            Some(SystemTheme::Dark)
        );
    }

    #[test]
    fn title_bar_resent_when_focus_comes_back() {
        // Windows：系統的設定廣播把標題列改回系統的顏色，重新取得焦點時再送一次
        let mut bar = TitleBar::new(false, true);
        assert!(!bar.owned());
        // 一開始（還不知道焦點、第一次拿到焦點）：egui 自己會送，不用
        assert_eq!(bar.tick(ThemeChoice::Dark, Theme::Dark, None), None);
        assert_eq!(bar.tick(ThemeChoice::Dark, Theme::Dark, Some(true)), None);
        assert_eq!(
            bar.tick(ThemeChoice::Dark, Theme::Dark, Some(true)),
            None,
            "一直有焦點不送"
        );
        assert_eq!(bar.tick(ThemeChoice::Dark, Theme::Dark, Some(false)), None);
        assert_eq!(
            bar.tick(ThemeChoice::Dark, Theme::Dark, Some(true)),
            Some(SystemTheme::Dark)
        );
        assert_eq!(bar.tick(ThemeChoice::Dark, Theme::Dark, Some(true)), None, "只送一次");
        bar.tick(ThemeChoice::Light, Theme::Light, Some(false));
        assert_eq!(
            bar.tick(ThemeChoice::Light, Theme::Light, Some(true)),
            Some(SystemTheme::Light)
        );
        // 跟隨系統：系統的廣播本來就是對的
        bar.tick(ThemeChoice::System, Theme::Light, Some(false));
        assert_eq!(bar.tick(ThemeChoice::System, Theme::Light, Some(true)), None);
        // macOS（強制的外觀不會被改回去）：不送
        let mut bar = TitleBar::new(false, false);
        bar.tick(ThemeChoice::Dark, Theme::Dark, Some(false));
        assert_eq!(bar.tick(ThemeChoice::Dark, Theme::Dark, Some(true)), None);
    }

    /// 跑一批輸入：winit 回報 `raw`、系統現在是 `os`，回傳 egui 看到的
    fn hooked(fix: &mut StaleSystemTheme, raw: Option<Theme>, os: Option<Theme>) -> Option<Theme> {
        let mut seen = raw;
        fix.hook(&mut seen, || os);
        seen
    }

    #[test]
    fn stale_system_theme_is_replaced_until_winit_reports_a_change() {
        use Theme::{Dark, Light};
        let mut fix = StaleSystemTheme::default();
        // 淺色啟動 → 選深色（強制）→ 系統改成深色（winit 不理）：winit 一直回報淺色
        assert_eq!(
            hooked(&mut fix, Some(Light), Some(Dark)),
            Some(Light),
            "還沒開始：照 winit 的"
        );
        // 選跟隨系統：讀到深色，跟 winit 的不一樣 → 蓋掉
        fix.start(Some(Dark));
        assert!(fix.active());
        assert_eq!(hooked(&mut fix, Some(Light), Some(Dark)), Some(Dark));
        assert_eq!(hooked(&mut fix, Some(Light), Some(Dark)), Some(Dark));
        // 系統改回淺色：winit 通知了，但值跟舊的一樣（看不出來）→ 每一批都再讀，所以跟著換
        assert_eq!(hooked(&mut fix, Some(Light), Some(Light)), Some(Light));
        assert!(fix.active());
        // 系統又改成深色：winit 回報的變了（真的 ThemeChanged）→ 之後照 winit 的
        assert_eq!(hooked(&mut fix, Some(Dark), Some(Dark)), Some(Dark));
        assert!(!fix.active());
        assert_eq!(hooked(&mut fix, Some(Dark), Some(Light)), Some(Dark), "不再讀系統的");
    }

    #[test]
    fn stale_system_theme_only_when_winit_disagrees() {
        use Theme::{Dark, Light};
        let mut fix = StaleSystemTheme::default();
        hooked(&mut fix, Some(Light), Some(Light));
        // 讀到的跟 winit 的一樣：winit 是對的，不蓋
        fix.start(Some(Light));
        assert!(!fix.active());
        assert_eq!(hooked(&mut fix, Some(Light), Some(Dark)), Some(Light));
        // 讀不到（不是主執行緒）：不蓋
        fix.start(None);
        assert!(!fix.active());
        // 蓋掉中改成強制的深色或淺色：停止
        fix.start(Some(Dark));
        assert!(fix.active());
        fix.stop();
        assert!(!fix.active());
        assert_eq!(hooked(&mut fix, Some(Light), Some(Dark)), Some(Light));
        // 蓋掉中讀不到：保留 winit 的（不會變成 None）
        fix.start(Some(Dark));
        assert_eq!(hooked(&mut fix, Some(Light), None), Some(Light));
        assert!(fix.active());
    }

    #[test]
    fn os_appearance_only_on_macos() {
        assert_eq!(os_appearance().is_some(), cfg!(target_os = "macos"));
        // 不是主執行緒（測試的執行緒）：讀不到
        if let Some(read) = os_appearance() {
            let other = std::thread::spawn(move || read()).join().unwrap();
            assert_eq!(other, None);
        }
    }

    /// 連使用者的工作階段匯流排的 `Portal::session` 只有 main.rs 建立，而且只在不是自動截圖（`--shot`）的時候：
    /// 其他程式碼、任何測試都不能連（測試用 `Portal::at` 連自己開的 dbus-daemon）
    #[test]
    fn session_portal_is_only_created_by_main() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let created = ["Portal::", "session("].concat();
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
        assert_eq!(creators, ["src/main.rs"], "只有 main.rs 可以連使用者的 portal");
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
            assert!(depth > 0, "Portal 要在 main.rs 的 `{block}` 區塊裡建立");
        }
    }

    /// portal 回的兩種樣子（P-C8）：`ReadOne` 是 `v`、舊的 `Read` 是 `v(v)`；用真的 D-Bus 訊息編碼再解開（不用匯流排）
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn portal_values_in_both_shapes() {
        use portal::{color_scheme, setting_changed, theme_of};
        use zbus::zvariant::{OwnedValue, Value};
        // 直接的 OwnedValue
        let plain = OwnedValue::from(1u32);
        let nested = Value::Value(Box::new(Value::U32(2))).try_into_owned().unwrap();
        assert_eq!(color_scheme(&plain), Some(1));
        assert_eq!(color_scheme(&nested), Some(2));
        assert_eq!(theme_of(&plain), Some(Theme::Dark));
        assert_eq!(theme_of(&nested), Some(Theme::Light));
        assert_eq!(theme_of(&OwnedValue::from(0u32)), Some(Theme::Light));
        assert_eq!(theme_of(&OwnedValue::from(7u32)), None, "不認得的值");
        assert_eq!(
            theme_of(&Value::from("dark").try_into_owned().unwrap()),
            None,
            "不是數字"
        );
        // 回覆的內容（簽章 v）編碼成 D-Bus 訊息、照 portal 的回覆那樣解成 OwnedValue
        let decoded = |body: &Value<'_>| -> OwnedValue {
            let msg = zbus::Message::method_call("/", "ReadOne")
                .unwrap()
                .build(&(body,))
                .unwrap();
            assert_eq!(msg.body().signature().to_string(), "v");
            msg.body().deserialize::<OwnedValue>().unwrap()
        };
        for (n, theme) in [(0u32, Theme::Light), (1, Theme::Dark), (2, Theme::Light)] {
            let v = decoded(&Value::U32(n));
            assert_eq!(theme_of(&v), Some(theme), "ReadOne 的 v：{v:?}");
            let vv = decoded(&Value::Value(Box::new(Value::U32(n))));
            assert!(matches!(&*vv, Value::Value(_)), "Read 多包一層：{vv:?}");
            assert_eq!(theme_of(&vv), Some(theme), "Read 的 v(v)：{vv:?}");
        }
        // SettingChanged 訊號（ssv）：只認深淺色這一項
        let signal = |namespace: &str, key: &str, value: Value<'_>| {
            zbus::Message::signal(portal::PATH, portal::INTERFACE, "SettingChanged")
                .unwrap()
                .build(&(namespace, key, value))
                .unwrap()
        };
        let msg = signal(portal::NAMESPACE, portal::KEY, Value::U32(1));
        assert_eq!(msg.body().signature().to_string(), "(ssv)");
        assert_eq!(setting_changed(&msg), Some(Some(Theme::Dark)));
        assert_eq!(
            setting_changed(&signal(portal::NAMESPACE, portal::KEY, Value::U32(2))),
            Some(Some(Theme::Light))
        );
        assert_eq!(
            setting_changed(&signal(portal::NAMESPACE, portal::KEY, Value::U32(9))),
            Some(None)
        );
        assert_eq!(
            setting_changed(&signal(portal::NAMESPACE, "accent-color", Value::U32(1))),
            None
        );
        assert_eq!(
            setting_changed(&signal("org.gnome.desktop.interface", portal::KEY, Value::U32(1))),
            None
        );
    }
}
