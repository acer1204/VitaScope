//! 右鍵選單的「畫質」（之後還有「音效」）：整個程式共用的設定，沒有開檔也能改。
//! 跟每個檔案各自的「畫面」（長寬比、裁切、旋轉…）、「音軌」分開。

use super::quality::{dumb_hover, follow_quality, scaler_choice};
use super::{Action, VitascopeApp};
use crate::pacing::{Plan, SmoothMode};
use crate::picture::{ChromaScaler, Downscaler, Quality, Strength, ToneCurve, ToneSettings, Upscaler, peak_label};
use crate::{tf, tr};
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
    /// 右鍵選單的「畫質」：影像調整、去交錯、去色帶、銳化、縮放演算法、HDR、流暢播放。
    /// 一直可以用（設定是整個程式共用的，不看有沒有開檔）
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
            if let Some(a) = self.processing_items(ui) {
                action = Some(a);
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

    /// 「畫質」選單的去交錯、去色帶、銳化、縮放演算法、像素著色器、HDR 色調映射（設定頁、控制面板也有同樣的選項）
    fn processing_items(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let v = self.settings.video.clone();
        let mut action = None;
        let deint_label = match self.deint_status() {
            Some(now) => tf!("去交錯（目前：{now}）", "Deinterlacing (now: {now})"),
            None => tr!("去交錯", "Deinterlacing").to_owned(),
        };
        let deint = self.deint_effective();
        submenu(ui, deint_label, self.video_disabled(false, "deinterlace"), |ui| {
            // 引擎不支援自動（系統的 libmpv 0.37）時不列
            for d in self.deint_choices() {
                if ui.selectable_label(deint == d, d.menu_label()).clicked() {
                    action = Some(Action::SetDeinterlace(d));
                }
            }
        });
        for (label, current, name, set) in [
            (
                tr!("去色帶", "Debanding"),
                v.deband,
                "deband",
                Action::SetDeband as fn(Strength) -> Action,
            ),
            (tr!("銳化", "Sharpening"), v.sharpen, "sharpen", Action::SetSharpen),
        ] {
            submenu(ui, label, self.video_disabled(true, name), |ui| {
                for s in Strength::ALL {
                    if ui.selectable_label(current == s, s.label()).clicked() {
                        action = Some(set(s));
                    }
                }
            });
        }
        // 縮放演算法：軟體繪圖不支援時整個停用；VITASCOPE_MPV_OPTS 指定的只停用那一項
        let dumb = self.caps.dumb.then(dumb_hover);
        submenu(ui, tr!("縮放演算法", "Scaling"), dumb, |ui| {
            for q in Quality::ALL {
                if ui.selectable_label(v.quality == q, q.menu_label()).clicked() {
                    action = Some(Action::SetQuality(q));
                }
            }
            ui.separator();
            let up = tf!(
                "放大（{}）",
                "Upscaling ({})",
                scaler_choice(v.scale.map(Upscaler::label))
            );
            submenu(ui, up, self.video_disabled(true, "scale"), |ui| {
                scaler_items(ui, v.scale, &Upscaler::ALL, Upscaler::label, |s| {
                    action = Some(Action::SetUpscaler(s));
                });
            });
            let down = tf!(
                "縮小（{}）",
                "Downscaling ({})",
                scaler_choice(v.dscale.map(Downscaler::label))
            );
            submenu(ui, down, self.video_disabled(true, "dscale"), |ui| {
                scaler_items(ui, v.dscale, &Downscaler::ALL, Downscaler::label, |s| {
                    action = Some(Action::SetDownscaler(s));
                });
            });
            let chroma = tf!(
                "色度（{}）",
                "Chroma ({})",
                scaler_choice(v.cscale.map(ChromaScaler::label))
            );
            submenu(ui, chroma, self.video_disabled(true, "cscale"), |ui| {
                scaler_items(ui, v.cscale, &ChromaScaler::ALL, ChromaScaler::label, |s| {
                    action = Some(Action::SetChromaScaler(s));
                });
            });
        });
        // 像素著色器：使用中的組合；「管理著色器…」打開設定的畫質頁（編輯組合）
        let mut manage = false;
        submenu(
            ui,
            tr!("像素著色器", "Pixel shaders"),
            self.shaders_disabled(),
            |ui| {
                let active = v.shaders.active;
                if ui.selectable_label(active.is_none(), tr!("不使用", "None")).clicked() {
                    action = Some(Action::SetShaderPreset(None));
                }
                for p in &v.shaders.presets {
                    if ui.selectable_label(active == Some(p.id), p.label()).clicked() {
                        action = Some(Action::SetShaderPreset(Some(p.id)));
                    }
                }
                ui.separator();
                if ui.button(tr!("管理著色器…", "Manage shaders…")).clicked() {
                    manage = true;
                }
            },
        );
        if manage {
            self.settings_open = true;
            self.settings_page = super::settings_window::Page::Picture;
        }
        // HDR：色調映射在最後輸出到螢幕時做，軟體繪圖的簡化流程也有（只略過縮放、去色帶之類的處理）
        let tone = v.tone;
        submenu(ui, tr!("HDR 色調映射", "HDR tone mapping"), None, |ui| {
            let curve_locked = self.video_locked("tone-mapping");
            for c in ToneCurve::ALL {
                let r = ui
                    .add_enabled(!curve_locked, egui::Button::selectable(tone.curve == c, c.label()))
                    .on_disabled_hover_text(super::control_panel::adjust_locked_hover());
                if r.clicked() {
                    action = Some(Action::SetTone(c));
                }
            }
            ui.separator();
            let peak = tf!("目標亮度（{}）", "Target brightness ({})", peak_label(tone.target_peak));
            submenu(ui, peak, self.video_disabled(false, "target-peak"), |ui| {
                for p in std::iter::once(None).chain(ToneSettings::PEAK_PRESETS.map(Some)) {
                    if ui.selectable_label(tone.target_peak == p, peak_label(p)).clicked() {
                        action = Some(Action::SetTargetPeak(p));
                    }
                }
            });
            // macOS 的畫面輸出不支援（hdr-compute-peak 要 compute shader）
            if !self.caps.macos {
                let mut on = tone.compute_peak;
                let r = ui
                    .add_enabled(
                        !self.video_locked("hdr-compute-peak"),
                        egui::Checkbox::new(&mut on, tr!("依畫面動態調整亮度", "Adjust brightness to each scene")),
                    )
                    .on_disabled_hover_text(super::control_panel::adjust_locked_hover());
                if r.changed() {
                    action = Some(Action::SetComputePeak(on));
                }
            }
            if self.video_not_hdr() {
                ui.weak(tr!("目前的影片不是 HDR", "The current video isn't HDR"));
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

/// 子選單；`disabled` 有值時整個停用，滑鼠移上去顯示原因
fn submenu(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    disabled: Option<&str>,
    add: impl FnOnce(&mut egui::Ui),
) {
    let r = ui
        .add_enabled_ui(disabled.is_none(), |ui| ui.menu_button(label, add).response)
        .inner;
    if let Some(why) = disabled {
        r.on_disabled_hover_text(why);
    }
}

/// 放大 / 縮小 / 色度的選項：「跟隨畫質」+ 各個演算法；選了就呼叫 `chosen`
fn scaler_items<S: Copy + PartialEq>(
    ui: &mut egui::Ui,
    current: Option<S>,
    all: &[S],
    label: fn(S) -> &'static str,
    mut chosen: impl FnMut(Option<S>),
) {
    if ui.selectable_label(current.is_none(), follow_quality()).clicked() {
        chosen(None);
    }
    ui.separator();
    for s in all {
        if ui.selectable_label(current == Some(*s), label(*s)).clicked() {
            chosen(Some(*s));
        }
    }
}
