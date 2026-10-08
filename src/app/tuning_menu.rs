//! 右鍵選單的「畫質」（之後還有「音效」）：整個程式共用的設定，沒有開檔也能改。
//! 跟每個檔案各自的「畫面」（長寬比、裁切、旋轉…）、「音軌」分開。

use super::{Action, VitascopeApp};
use crate::pacing::{Plan, SmoothMode};
use eframe::egui;

/// 「流暢播放」的說明（右鍵選單、設定頁共用）
pub(super) fn smooth_hover() -> &'static str {
    crate::tr!(
        "依視窗所在螢幕的實際更新率（例如 119.88 Hz）微調播放速度，動作更順；會多用一些 CPU（4K 120 Hz 約半個核心）。",
        "Fine-tunes the playback speed to the actual refresh rate of the screen the window is on (for example \
         119.88 Hz) so motion is smoother. Uses a bit more CPU (about half a core at 4K 120 Hz)."
    )
}

/// 使用者用 VITASCOPE_MPV_OPTS（或 VITASCOPE_PACING=off）自己處理了：選項停用時的說明
pub(super) fn smooth_locked_hover() -> &'static str {
    crate::tr!(
        "已由 VITASCOPE_MPV_OPTS 或 VITASCOPE_PACING 指定",
        "Set by VITASCOPE_MPV_OPTS or VITASCOPE_PACING"
    )
}

impl VitascopeApp {
    /// 右鍵選單的「畫質」：影像調整、流暢播放。一直可以用（設定是整個程式共用的，不看有沒有開檔）
    pub(super) fn picture_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        ui.menu_button(crate::tr!("畫質", "Video quality"), |ui| {
            let panel = egui::Button::selectable(self.panel_open, crate::tr!("影像調整…", "Image adjustments…"))
                .shortcut_text(format!("{}+G", super::ALT_KEY));
            // 跟設定頁的按鈕一樣只負責打開（已經開著就不動；Alt+G 才是開關）
            if ui.add(panel).clicked() && !self.panel_open {
                action = Some(Action::ToggleControlPanel);
            }
            if super::menu_item(
                ui,
                !self.adjust_is_neutral(),
                crate::tr!("還原影像調整", "Reset image adjustments"),
                "Q",
            ) {
                action = Some(Action::AdjustReset);
            }
            ui.separator();
            let mode = self.settings.smooth;
            let mut on = mode != SmoothMode::Off;
            let label = match self.smooth_short(mode) {
                Some(s) => crate::tf!("流暢播放（{s}）", "Smooth playback ({s})"),
                None => crate::tr!("流暢播放", "Smooth playback").to_owned(),
            };
            let locked = self.pacing_status().plan == Some(Plan::Untouched);
            let r = ui
                .add_enabled(!locked, egui::Checkbox::new(&mut on, label))
                .on_hover_text(smooth_hover())
                .on_disabled_hover_text(smooth_locked_hover());
            if r.changed() {
                action = Some(Action::ToggleSmooth);
            }
        });
        action
    }

    /// 選單切換流暢播放：關 ↔ 開（使用電池時暫停）
    pub(super) fn toggle_smooth(&mut self) {
        let mode = if self.settings.smooth == SmoothMode::Off {
            SmoothMode::Auto
        } else {
            SmoothMode::Off
        };
        self.set_smooth(mode);
        let msg = match (mode, self.smooth_short(mode)) {
            (SmoothMode::Off, _) => crate::tr!("流暢播放：關", "Smooth playback: off").to_owned(),
            (_, Some(s)) => crate::tf!("流暢播放：開（{s}）", "Smooth playback: on ({s})"),
            (_, None) => crate::tr!("流暢播放：開", "Smooth playback: on").to_owned(),
        };
        self.osd(msg);
    }
}
