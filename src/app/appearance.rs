//! 外觀接上播放器：跟隨系統的深淺色從哪裡來、標題列（系統畫的邊框）的深淺色。不碰視窗的部分在 [`crate::theme`]。
//!
//! - Windows、macOS：系統的深淺色由 winit 回報（egui 的 `system_theme`）。
//!   - Windows：選了深色或淺色時，視窗重新取得焦點就再送一次 `SetTheme`（系統的設定廣播會把標題列改回系統的顏色，P-B9）。
//!   - macOS：換回跟隨系統時 winit 回報的可能是舊的：讀一次系統的外觀，在 `raw_input_hook` 蓋掉，直到 winit 回報的變了（P-C9）。
//! - Linux：winit 不回報；問桌面的設定 portal（`Launch.theme_source`，第一次選跟隨系統時才開始），
//!   回答放在 egui 的 `fallback_theme`。標題列由 App 自己送看到的深淺色，egui 的同步關掉（P-C10）。

use super::VitascopeApp;
use crate::theme::{self, OsAppearance, StaleSystemTheme, ThemeChoice, ThemeSource, TitleBar};
use eframe::egui::{self, Theme, ViewportCommand};
use std::sync::Arc;

/// 外觀的狀態
pub(super) struct Appearance {
    /// 另外問系統深淺色的地方（Linux 的 portal）；None = 由 winit 回報（Windows、macOS、自動測試）
    source: Option<Box<dyn ThemeSource>>,
    /// 已經請 `source` 開始注意了（第一次選跟隨系統時）
    watching: bool,
    /// `source` 回報、已經套用的（放在 egui 的 `fallback_theme`）
    reported: Option<Theme>,
    title_bar: TitleBar,
    /// macOS：換回跟隨系統時 winit 回報的可能是舊的
    stale: StaleSystemTheme,
    /// 讀系統現在的外觀的方法（macOS；其他系統 None）
    os_appearance: Option<OsAppearance>,
}

impl Appearance {
    /// `source`：Linux 的 portal（main.rs）或測試的假的；`os_appearance`：測試的假的，None = 這個平台的（只有 macOS 有）
    pub(super) fn new(source: Option<Box<dyn ThemeSource>>, os_appearance: Option<OsAppearance>) -> Self {
        // 系統的深淺色是 App 自己問的（Linux）：標題列也自己送（X11 的「系統預設」一律是深色）
        let own = source.is_some();
        Self {
            source,
            watching: false,
            reported: None,
            title_bar: TitleBar::new(own, cfg!(windows)),
            stale: StaleSystemTheme::default(),
            os_appearance: os_appearance.or_else(theme::os_appearance),
        }
    }

    /// 啟動時（建立 App 時，還沒有任何一幀）：兩種樣式、主題偏好、標題列由誰管；選了跟隨系統就開始問
    pub(super) fn start(&mut self, ctx: &egui::Context, choice: ThemeChoice) {
        theme::install(ctx);
        if self.title_bar.owned() {
            ctx.options_mut(|o| o.sync_window_theme = false);
        }
        if choice == ThemeChoice::System {
            self.watch(ctx);
        }
        theme::apply_with(ctx, choice, self.reported);
    }

    /// 請 `source` 開始注意系統的深淺色（只有第一次）
    fn watch(&mut self, ctx: &egui::Context) {
        if self.watching {
            return;
        }
        if let Some(source) = &mut self.source {
            self.watching = true;
            let ctx = ctx.clone();
            source.watch(Arc::new(move || ctx.request_repaint()));
        }
    }
}

impl VitascopeApp {
    /// 換外觀：馬上套用、存檔
    pub(super) fn set_theme(&mut self, ctx: &egui::Context, choice: ThemeChoice) {
        let before = self.settings.theme;
        self.settings.theme = choice;
        let a = &mut self.appearance;
        if choice == ThemeChoice::System {
            a.watch(ctx);
            if before != ThemeChoice::System
                && let Some(read) = &a.os_appearance
            {
                a.stale.start(read());
            }
        } else {
            a.stale.stop();
        }
        theme::apply_with(ctx, choice, a.reported);
        self.save_settings();
    }

    /// 每一幀（`logic`）：portal 的回答變了就重新套用；標題列要不要自己送 `SetTheme`
    pub(super) fn theme_tick(&mut self, ctx: &egui::Context) {
        let choice = self.settings.theme;
        let a = &mut self.appearance;
        if let Some(source) = &a.source {
            let now = source.get();
            if now != a.reported {
                a.reported = now;
                theme::apply_with(ctx, choice, now);
            }
        }
        let focused = ctx.input(|i| i.viewport().focused);
        if let Some(window) = a.title_bar.tick(choice, ctx.theme(), focused) {
            ctx.send_viewport_cmd(ViewportCommand::SetTheme(window));
        }
    }

    /// `raw_input_hook`（egui 處理這一批輸入之前）：macOS 換回跟隨系統後，蓋掉 winit 回報的舊值
    pub(super) fn theme_raw_input(&mut self, raw_input: &mut egui::RawInput) {
        let Appearance {
            stale, os_appearance, ..
        } = &mut self.appearance;
        stale.hook(&mut raw_input.system_theme, || {
            os_appearance.as_ref().and_then(|read| read())
        });
    }

    /// 選了跟隨系統、但系統的深淺色問不到（winit 沒有回報、portal 也沒有回答）：設定頁提示暫時用深色
    pub(super) fn system_theme_unknown(&self, ctx: &egui::Context) -> bool {
        theme::system_unknown(ctx, self.settings.theme) && self.appearance.reported.is_none()
    }
}
