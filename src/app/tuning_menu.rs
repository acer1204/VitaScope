//! 右鍵選單的「畫質」與「音效」：整個程式共用的設定，沒有開檔也能改。
//! 跟每個檔案各自的「畫面」（長寬比、裁切、旋轉…）、「音軌」分開。

use super::quality::{dumb_hover, follow_quality, scaler_choice};
use super::sound::{auto_device_label, leveling_hover, volume_max_hover, volume_max_label};
use super::{Action, VitascopeApp};
use crate::pacing::{Plan, SmoothMode};
use crate::picture::{
    ChromaScaler, Downscaler, Quality, Strength, ToneCurve, ToneSettings, Upscaler, peak_hover, peak_label,
    peak_menu_label,
};
use crate::sound::{EqPreset, Leveling, VOLUME_MAX_CHOICES};
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
            let shown = self.panel_open && self.panel_tab == super::control_panel::PanelTab::Picture;
            let panel = egui::Button::selectable(shown, crate::tr!("影像調整…", "Image adjustments…"))
                .shortcut_text(format!("{}+G", super::ALT_KEY));
            // 跟設定頁的按鈕一樣只負責打開畫質分頁（已經開著就換到畫質分頁；Alt+G 才是開關）
            if ui.add(panel).clicked() {
                action = Some(Action::ShowAdjustments);
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
                // 「自動」= 203 nits；超過 203 vo_gpu 會裁切亮部，所以只列比它小的
                for p in std::iter::once(None).chain(ToneSettings::PEAK_PRESETS.map(Some)) {
                    let r = ui
                        .selectable_label(tone.target_peak == p, peak_menu_label(p))
                        .on_hover_text(peak_hover());
                    if r.clicked() {
                        action = Some(Action::SetTargetPeak(p));
                    }
                }
            });
            // 動態峰值偵測要畫面輸出的 OpenGL 有 GLSL 4.20 + compute shader：不能用時不顯示
            if self.caps.compute_peak {
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

    /// 右鍵選單的「音效」（緊接在「畫質」後面）：等化器、音量平衡、多聲道轉成立體聲、音量上限、
    /// 輸出裝置（＋獨佔模式）、音訊直通。一直可以用（設定是整個程式共用的）
    pub(super) fn sound_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let a = self.settings.audio.clone();
        ui.menu_button(tr!("音效", "Sound"), |ui| {
            // 跟「畫質 ▸ 影像調整…」一樣只負責打開（控制面板的音效分頁有十段滑桿）
            if ui.button(tr!("等化器…", "Equalizer…")).clicked() {
                action = Some(Action::ShowEqualizer);
            }
            let eq_off = self.eq_disabled();
            let mut eq_on = a.eq.enabled;
            let r = ui
                .add_enabled(
                    eq_off.is_none(),
                    egui::Checkbox::new(&mut eq_on, tr!("等化器", "Equalizer")),
                )
                .on_disabled_hover_text(eq_off.unwrap_or_default());
            if r.changed() {
                action = Some(Action::ToggleEq);
            }
            submenu(ui, tr!("等化器預設", "Equalizer preset"), eq_off, |ui| {
                for p in EqPreset::ALL {
                    if ui.selectable_label(a.eq.preset == p, p.label()).clicked() {
                        action = Some(Action::SetEqPreset(p));
                    }
                }
            });
            // 目前選的在子選單裡標出來（跟「等化器預設」一樣）
            submenu(
                ui,
                tr!("音量平衡", "Volume leveling"),
                self.leveling_disabled(),
                |ui| {
                    for mode in Leveling::ALL {
                        let missing = self.leveling_missing(mode);
                        let r = ui
                            .add_enabled(
                                missing.is_none(),
                                egui::Button::selectable(a.leveling == mode, mode.menu_label()),
                            )
                            .on_hover_text(leveling_hover());
                        let r = match &missing {
                            Some(why) => r.on_disabled_hover_text(why),
                            None => r,
                        };
                        if r.clicked() {
                            action = Some(Action::SetLeveling(mode));
                        }
                    }
                },
            );
            let mut downmix = a.downmix;
            let disabled = self.downmix_disabled();
            let r = ui
                .add_enabled(
                    disabled.is_none(),
                    egui::Checkbox::new(
                        &mut downmix,
                        tr!(
                            "多聲道轉成立體聲（5.1／7.1 → 2.0）",
                            "Downmix to stereo (5.1/7.1 → 2.0)"
                        ),
                    ),
                )
                .on_hover_text(downmix_hover())
                .on_disabled_hover_text(disabled.unwrap_or_default());
            if r.changed() {
                action = Some(Action::ToggleDownmix);
            }
            // 音量上限：超過 100% 經過限幅器（沒有限幅器時用 mpv 自己的音量，可能破音）
            let limiter = !self.af_locked() && self.caps.af.alimiter;
            submenu(ui, tr!("音量上限", "Volume limit"), None, |ui| {
                for v in VOLUME_MAX_CHOICES {
                    if ui
                        .selectable_label(a.volume_max == v, volume_max_label(v))
                        .on_hover_text(volume_max_hover(limiter))
                        .clicked()
                    {
                        action = Some(Action::SetVolumeMax(v));
                    }
                }
            });
            ui.separator();
            if let Some(a) = self.device_menu(ui) {
                action = Some(a);
            }
            let mut passthrough = a.passthrough.enabled;
            let label = match self.spdif_active() {
                Some(f) => tf!(
                    "音訊直通（使用中：{}）",
                    "Passthrough (active: {})",
                    crate::sound::spdif_label(f)
                ),
                None => tr!("音訊直通", "Passthrough").to_owned(),
            };
            let r = ui
                .add_enabled(
                    !self.sound_locked("audio-spdif"),
                    egui::Checkbox::new(&mut passthrough, label),
                )
                .on_hover_text(passthrough_hover())
                .on_disabled_hover_text(super::control_panel::adjust_locked_hover());
            if r.changed() {
                action = Some(Action::TogglePassthrough);
            }
        });
        action
    }

    /// 「音效 ▸ 輸出裝置」：預設裝置 + 目前輸出方式的裝置；下面是獨佔模式。選裝置直接處理（名稱是字串）
    fn device_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let devices = self.device_choices();
        let saved = self
            .settings
            .audio
            .device
            .clone()
            .filter(|d| d != crate::sound::AUTO_DEVICE);
        let saved_label = self.settings.audio.device_label.clone();
        let missing = self.device_missing();
        let exclusive_shown = self.exclusive_shown();
        let mut chosen: Option<Option<crate::sound::AudioDevice>> = None;
        // VITASCOPE_MPV_OPTS 指定了 audio-device：只停用裝置，子選單照樣打得開（獨佔模式是另一個選項）
        let locked = self.sound_disabled("audio-device");
        ui.menu_button(tr!("輸出裝置", "Output device"), |ui| {
            ui.add_enabled_ui(locked.is_none(), |ui| {
                let item = |ui: &mut egui::Ui, on: bool, label: &str| {
                    let r = ui.selectable_label(on, label);
                    match locked {
                        Some(why) => r.on_disabled_hover_text(why),
                        None => r,
                    }
                };
                if item(ui, saved.is_none(), auto_device_label()).clicked() {
                    chosen = Some(None);
                }
                for d in &devices {
                    if item(ui, saved.as_deref() == Some(d.name.as_str()), d.label())
                        .on_hover_text(&d.name)
                        .clicked()
                    {
                        chosen = Some(Some(d.clone()));
                    }
                }
                // 存下的裝置拔掉了（或還不知道在不在）：照樣列出但不能選，插回來時會自動切回去
                if let Some(name) = saved.as_deref()
                    && (missing || !devices.iter().any(|d| d.name == name))
                {
                    let label = saved_label.as_deref().unwrap_or(name);
                    let text = if missing {
                        tf!(
                            "{label}（找不到，暫用預設裝置）",
                            "{label} (not found; using the default)"
                        )
                    } else {
                        label.to_owned()
                    };
                    ui.add_enabled(false, egui::Button::selectable(true, text))
                        .on_disabled_hover_text(locked.unwrap_or(tr!(
                            "裝置插回來時會自動切回去",
                            "VitaScope switches back when the device is plugged in again"
                        )));
                }
            });
            if exclusive_shown {
                ui.separator();
                let mut on = self.settings.audio.exclusive;
                let r = ui
                    .add_enabled(
                        !self.sound_locked("audio-exclusive"),
                        egui::Checkbox::new(&mut on, tr!("獨佔模式", "Exclusive mode")),
                    )
                    .on_hover_text(exclusive_hover())
                    .on_disabled_hover_text(super::control_panel::adjust_locked_hover());
                if r.changed() {
                    action = Some(Action::ToggleExclusive);
                }
            }
        });
        if let Some(d) = chosen {
            self.select_audio_device(d.as_ref());
        }
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

/// 「多聲道轉成立體聲」的說明（右鍵選單、設定頁共用）
pub(super) fn downmix_hover() -> &'static str {
    tr!(
        "5.1／7.1 聲道的影片混成雙聲道，用耳機、電視喇叭時對白比較清楚；切換時聲音會中斷一下",
        "Mixes 5.1/7.1 audio down to two channels, so dialogue is clearer on headphones and TV speakers. \
         The sound cuts out briefly when you switch."
    )
}

/// 「音訊直通」的說明（右鍵選單、設定頁共用）
pub(super) fn passthrough_hover() -> &'static str {
    tr!(
        "AC-3、DTS 之類的音訊不解碼，原封不動經 HDMI、光纖送到擴大機（格式在「設定 → 音效」選）",
        "Sends AC-3, DTS and similar audio undecoded to your amplifier over HDMI or S/PDIF \
         (choose the formats in Settings → Sound)"
    )
}

/// 「獨佔模式」的說明（右鍵選單、設定頁共用）
pub(super) fn exclusive_hover() -> &'static str {
    tr!(
        "直接使用音訊裝置、不經過系統混音（其他程式暫時沒有聲音）",
        "Uses the audio device directly, bypassing the system mixer (other apps go silent meanwhile)"
    )
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
