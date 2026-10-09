//! 設定視窗（F5、右鍵選單「設定…」）：改了馬上生效、馬上存檔。

use super::control_panel::adjust_locked_hover;
use super::quality::{combo, dumb_hover, scaler_choice};
use super::sound::auto_device_label;
use super::{Action, VitascopeApp};
use crate::i18n::{self, Lang};
use crate::pacing::{Plan, SmoothMode};
use crate::picture::{
    ChromaScaler, Downscaler, Gamut, Quality, Strength, ToneCurve, ToneSettings, Upscaler, peak_hover,
};
use crate::sound::{AUTO_DEVICE, AudioDevice, SPDIF_CODECS, spdif_label};
use crate::theme::ThemeChoice;
use crate::{tf, tr};
use eframe::egui::{self, Id, pos2, vec2};

/// 設定視窗的分頁
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Page {
    #[default]
    General,
    Playback,
    /// 畫質（影像調整、去交錯、去色帶、銳化、縮放演算法、像素著色器、HDR）
    Picture,
    /// 音效（輸出裝置、獨佔模式、轉成立體聲、音訊直通）
    Sound,
    Subtitles,
    Screenshot,
    System,
    Shortcuts,
}

impl Page {
    const ALL: [Page; 8] = [
        Page::General,
        Page::Playback,
        Page::Picture,
        Page::Sound,
        Page::Subtitles,
        Page::Screenshot,
        Page::System,
        Page::Shortcuts,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::General => tr!("一般", "General"),
            Page::Playback => tr!("播放", "Playback"),
            Page::Picture => tr!("畫質", "Video quality"),
            Page::Sound => tr!("音效", "Sound"),
            Page::Subtitles => tr!("字幕", "Subtitles"),
            Page::Screenshot => tr!("截圖", "Screenshots"),
            Page::System => tr!("系統", "System"),
            Page::Shortcuts => tr!("快捷鍵", "Shortcuts"),
        }
    }
}

impl VitascopeApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            // 下次打開時重新檢查著色器檔案（可能換過內容）
            self.shader_info.clear();
            return;
        }
        let mut open = true;
        let mut changed = false;
        let mut action = None;
        egui::Window::new(tr!("設定", "Settings"))
            .id(Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_pos(pos2(60.0, 40.0))
            .default_size(vec2(520.0, 360.0))
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // 左邊：分頁
                    ui.vertical(|ui| {
                        ui.set_width(110.0);
                        for page in Page::ALL {
                            if ui.selectable_label(self.settings_page == page, page.title()).clicked() {
                                self.settings_page = page;
                            }
                        }
                    });
                    ui.separator();
                    // 右邊：內容
                    ui.vertical(|ui| {
                        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                            match self.settings_page {
                                Page::General => changed |= self.general_page(ui, &mut action),
                                Page::Playback => changed |= self.playback_page(ui),
                                Page::Picture => self.picture_page(ui, &mut action),
                                Page::Sound => self.sound_page(ui, &mut action),
                                Page::Subtitles => self.subtitles_page(ui, &mut action),
                                Page::Screenshot => changed |= self.screenshot_page(ui, &mut action),
                                Page::System => changed |= self.system_page(ui),
                                Page::Shortcuts => shortcuts_page(ui),
                            }
                        });
                    });
                });
            });
        if changed {
            self.save_settings();
        }
        if !open {
            self.settings_open = false;
        }
        if let Some(a) = action {
            self.run(ctx, a);
        }
    }

    fn general_page(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) -> bool {
        let mut changed = false;
        egui::Grid::new("settings_general")
            .num_columns(2)
            .spacing([12.0, 10.0])
            .show(ui, |ui| {
                ui.label(tr!("介面語言", "Language"));
                egui::ComboBox::from_id_salt("settings_language")
                    .selected_text(self.settings.language.name())
                    .show_ui(ui, |ui| {
                        for lang in Lang::ALL {
                            if ui
                                .selectable_value(&mut self.settings.language, lang, lang.name())
                                .changed()
                            {
                                i18n::set_lang(lang);
                                changed = true;
                            }
                        }
                    });
                ui.end_row();
                ui.label(tr!("外觀", "Appearance"));
                let current = self.settings.theme;
                egui::ComboBox::from_id_salt("settings_theme")
                    .selected_text(current.label())
                    .show_ui(ui, |ui| {
                        for choice in ThemeChoice::ALL {
                            if ui.selectable_label(current == choice, choice.label()).clicked() && choice != current {
                                // 馬上套用、存檔（跟切換深淺色的指令走同一條路）
                                *action = Some(Action::SetTheme(choice));
                            }
                        }
                    });
                ui.end_row();
            });
        if crate::theme::system_unknown(ui.ctx(), self.settings.theme) {
            ui.weak(tr!(
                "偵測不到系統的深淺色設定，暫時用深色",
                "Couldn't detect the system's light/dark setting; using dark for now"
            ));
        }
        ui.add_space(6.0);
        let mut on_top = self.settings.always_on_top;
        if ui.checkbox(&mut on_top, tr!("視窗置頂", "Always on top")).changed() {
            // 跟快捷鍵、右鍵選單走同一條路（Wayland 不支援時會提示）
            *action = Some(Action::ToggleOnTop);
        }
        changed
    }

    fn system_page(&mut self, ui: &mut egui::Ui) -> bool {
        let changed = ui
            .checkbox(
                &mut self.settings.single_instance,
                tr!("只開一個視窗", "Use a single window"),
            )
            .on_hover_text(tr!(
                "已經開著影戲時，雙擊其他影片會交給開著的視窗播放，不會再開一個（下次開啟時生效）",
                "When VitaScope is already open, files you open are sent to that window instead of starting another one \
                 (takes effect the next time it starts)"
            ))
            .changed();
        ui.add_space(10.0);
        ui.strong(tr!("檔案關聯", "File associations"));
        #[cfg(windows)]
        let changed = self.windows_associations(ui) || changed;
        #[cfg(target_os = "macos")]
        ui.weak(tr!(
            "在 Finder 對影片按右鍵 →「取得資訊」→「打開檔案的應用程式」選影戲，再按「全部更改」。",
            "In Finder, choose Get Info on a video, pick VitaScope under \"Open with\", then click \"Change All\"."
        ));
        #[cfg(all(unix, not(target_os = "macos")))]
        ui.weak(tr!(
            "tar.gz 版：執行裡面的 install.sh 後，檔案管理員的「開啟檔案」就會有影戲（install.sh --default 設成預設）。\
             AppImage 版：用 AppImageLauncher 或 Gear Lever 加入應用程式選單。",
            "tar.gz: after running its install.sh, VitaScope appears in your file manager's \"Open with\" \
             (install.sh --default makes it the default). AppImage: add it to the app menu with AppImageLauncher or Gear Lever."
        ));
        changed
    }

    /// 檔案關聯的選項（Windows）；回傳設定有沒有改
    #[cfg(windows)]
    fn windows_associations(&mut self, ui: &mut egui::Ui) -> bool {
        let mut on = self.settings.file_associations;
        let changed = ui
            .checkbox(
                &mut on,
                tr!(
                    "把影戲加入影片與音訊檔的「開啟檔案」選單",
                    "Add VitaScope to \"Open with\" for video and audio files"
                ),
            )
            .changed();
        if changed {
            self.set_file_associations(on);
        }
        ui.add_space(4.0);
        // 沒有登錄時「預設應用程式」裡不會列出影戲
        if ui
            .add_enabled(
                self.settings.file_associations,
                egui::Button::new(tr!("選擇預設播放器…", "Choose the default player…")),
            )
            .clicked()
        {
            crate::assoc::open_default_apps_settings();
        }
        ui.weak(tr!(
            "Windows 不讓程式自己設成預設，要在「設定 → 預設應用程式」裡選影戲。",
            "Windows doesn't let programs make themselves the default; pick VitaScope in Settings → Default apps."
        ));
        changed
    }

    /// 打開 / 關掉檔案關聯（Windows）
    #[cfg(windows)]
    pub(super) fn set_file_associations(&mut self, on: bool) {
        let places = crate::assoc::Places::default();
        let result = match std::env::current_exe() {
            Ok(exe) if on => crate::assoc::register(&exe, &places),
            Ok(_) => {
                crate::assoc::unregister(&places);
                Ok(())
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(()) => self.settings.file_associations = on,
            Err(e) => {
                self.settings.file_associations = false;
                self.osd(crate::tf!(
                    "無法設定檔案關聯：{e}",
                    "Cannot set up file associations: {e}"
                ));
            }
        }
    }

    fn playback_page(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let s = &mut self.settings;
        changed |= ui
            .checkbox(
                &mut s.auto_next,
                tr!("播完自動播放下一個", "Play the next file automatically"),
            )
            .changed();
        changed |= ui
            .checkbox(
                &mut s.resume,
                tr!("從上次的位置繼續播放", "Resume from where I left off"),
            )
            .changed();
        let hwdec = ui
            .checkbox(&mut s.hwdec, tr!("硬體解碼", "Hardware decoding"))
            .on_hover_text(tr!(
                "用顯示卡解碼，省電、播 4K 不吃力；畫面有問題時可以關掉試試看",
                "Decode on the graphics card (less power, smooth 4K). Turn it off if the picture looks wrong."
            ));
        if hwdec.changed() {
            changed = true;
            let _ = self.player.set_hwdec(s.hwdec);
        }
        ui.add_space(8.0);
        // 拖曳、打字時馬上生效，放開滑鼠或離開欄位時才存檔（不要每一幀都寫一次設定檔）
        let commit =
            |r: egui::Response| r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
        egui::Grid::new("settings_playback")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(tr!("← / → 跳轉", "← / → seek"));
                changed |= commit(
                    ui.add(
                        egui::DragValue::new(&mut s.seek_short)
                            .range(1.0..=60.0)
                            .speed(0.2)
                            .suffix(tr!(" 秒", " s")),
                    ),
                );
                ui.end_row();
                ui.label(crate::tf!("{CMD} + ← / → 跳轉", "{CMD} + ← / → seek"));
                changed |= commit(
                    ui.add(
                        egui::DragValue::new(&mut s.seek_long)
                            .range(5.0..=600.0)
                            .speed(1.0)
                            .suffix(tr!(" 秒", " s")),
                    ),
                );
                ui.end_row();
            });
        ui.add_space(10.0);
        self.smooth_section(ui);
        changed
    }

    /// 播放頁的「流暢播放」：兩個勾選對應三種設定
    /// （「流暢播放」沒勾 = 關；「使用電池時暫停」勾 = 開但用電池時暫停、沒勾 = 一直開）。改了馬上生效、存檔
    fn smooth_section(&mut self, ui: &mut egui::Ui) {
        self.read_sync_numbers();
        let mode = self.settings.smooth;
        let locked = self.pacing_status().plan == Some(Plan::Untouched);
        let (mut on, mut pause) = (mode != SmoothMode::Off, mode != SmoothMode::Always);
        let mut toggled = false;
        ui.add_enabled_ui(!locked, |ui| {
            toggled |= ui
                .checkbox(
                    &mut on,
                    tr!(
                        "流暢播放（對齊螢幕更新率）",
                        "Smooth playback (match the screen's refresh rate)"
                    ),
                )
                .on_hover_text(super::tuning_menu::smooth_hover())
                .on_disabled_hover_text(super::tuning_menu::smooth_locked_hover())
                .changed();
            ui.indent("smooth_battery", |ui| {
                ui.add_enabled_ui(on, |ui| {
                    toggled |= ui
                        .checkbox(
                            &mut pause,
                            tr!("使用電池時暫停（省電）", "Pause on battery (saves power)"),
                        )
                        .changed();
                });
            });
        });
        ui.indent("smooth_status", |ui| {
            ui.weak(self.pacing_status().describe());
        });
        if toggled {
            self.set_smooth(match (on, pause) {
                (false, _) => SmoothMode::Off,
                (true, true) => SmoothMode::Auto,
                (true, false) => SmoothMode::Always,
            });
        }
    }

    /// 畫質頁：影像調整（滑桿在控制面板裡），去交錯、去色帶、銳化、縮放演算法、像素著色器、HDR
    fn picture_page(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        ui.strong(tr!("影像調整", "Image adjustments"));
        ui.label(tf!(
            "亮度、對比、飽和度、色相、Gamma：{}",
            "Brightness, contrast, saturation, hue, gamma: {}",
            self.adjust_summary()
        ));
        ui.add_space(4.0);
        if ui.button(tr!("影像調整…", "Image adjustments…")).clicked() {
            *action = Some(Action::ShowAdjustments);
        }
        let mut keep = self.settings.video.keep_adjust;
        if ui
            .checkbox(&mut keep, super::control_panel::keep_adjust_label())
            .on_hover_text(tr!(
                "沒勾的話，調整只在這次執行有效（換檔案時會沿用），下次開啟影戲時從 0 開始",
                "If unticked, adjustments last only until VitaScope closes (they carry over to the next file) \
                 and start from 0 next time"
            ))
            .changed()
        {
            self.set_keep_adjust(keep);
        }
        self.processing_sections(ui, action);
    }

    /// 畫質頁的去交錯、去色帶／銳化、縮放演算法、像素著色器、HDR → SDR（選了馬上套用、存檔）
    fn processing_sections(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let v = self.settings.video.clone();
        let dumb = self.caps.dumb;
        ui.add_space(12.0);
        ui.strong(tr!("去交錯", "Deinterlacing"));
        let deint = self.deint_effective();
        let deint_locked = self.video_locked("deinterlace");
        ui.horizontal(|ui| {
            for d in self.deint_choices() {
                let r = ui
                    .add_enabled(!deint_locked, egui::RadioButton::new(deint == d, d.menu_label()))
                    .on_disabled_hover_text(adjust_locked_hover());
                if r.clicked() && deint != d {
                    *action = Some(Action::SetDeinterlace(d));
                }
            }
        });
        if let Some(now) = self.deint_status() {
            ui.weak(tf!("目前：{now}", "Now: {now}"));
        }
        ui.weak(tr!(
            "電視錄影、DVD 之類的交錯式影片才需要；自動 = 只處理交錯的影片",
            "Needed for interlaced video such as TV recordings and DVDs; Auto handles only interlaced video"
        ));

        ui.add_space(12.0);
        ui.strong(tr!("去色帶／銳化", "Debanding / sharpening"));
        egui::Grid::new("settings_deband_sharpen")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                if let Some(s) = combo(
                    ui,
                    tr!("去色帶", "Debanding"),
                    "settings_deband",
                    v.deband,
                    &Strength::ALL,
                    Strength::label,
                    self.video_disabled(true, "deband"),
                ) {
                    *action = Some(Action::SetDeband(s));
                }
                ui.end_row();
                if let Some(s) = combo(
                    ui,
                    tr!("銳化", "Sharpening"),
                    "settings_sharpen",
                    v.sharpen,
                    &Strength::ALL,
                    Strength::label,
                    self.video_disabled(true, "sharpen"),
                ) {
                    *action = Some(Action::SetSharpen(s));
                }
                ui.end_row();
            });

        ui.add_space(12.0);
        ui.strong(tr!("縮放演算法", "Scaling"));
        ui.add_enabled_ui(!dumb, |ui| {
            ui.horizontal(|ui| {
                for q in Quality::ALL {
                    if ui.selectable_label(v.quality == q, q.menu_label()).clicked() && v.quality != q {
                        *action = Some(Action::SetQuality(q));
                    }
                }
            });
            egui::CollapsingHeader::new(tr!("進階", "Advanced"))
                .id_salt("settings_scalers")
                .default_open(false)
                .show(ui, |ui| {
                    egui::Grid::new("settings_scalers_grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            let ups: Vec<Option<Upscaler>> =
                                std::iter::once(None).chain(Upscaler::ALL.map(Some)).collect();
                            if let Some(s) = combo(
                                ui,
                                tr!("放大", "Upscaling"),
                                "settings_scale",
                                v.scale,
                                &ups,
                                |s| scaler_choice(s.map(Upscaler::label)),
                                self.video_disabled(true, "scale"),
                            ) {
                                *action = Some(Action::SetUpscaler(s));
                            }
                            ui.end_row();
                            let downs: Vec<Option<Downscaler>> =
                                std::iter::once(None).chain(Downscaler::ALL.map(Some)).collect();
                            if let Some(s) = combo(
                                ui,
                                tr!("縮小", "Downscaling"),
                                "settings_dscale",
                                v.dscale,
                                &downs,
                                |s| scaler_choice(s.map(Downscaler::label)),
                                self.video_disabled(true, "dscale"),
                            ) {
                                *action = Some(Action::SetDownscaler(s));
                            }
                            ui.end_row();
                            let chromas: Vec<Option<ChromaScaler>> =
                                std::iter::once(None).chain(ChromaScaler::ALL.map(Some)).collect();
                            if let Some(s) = combo(
                                ui,
                                tr!("色度", "Chroma"),
                                "settings_cscale",
                                v.cscale,
                                &chromas,
                                |s| scaler_choice(s.map(ChromaScaler::label)),
                                self.video_disabled(true, "cscale"),
                            ) {
                                *action = Some(Action::SetChromaScaler(s));
                            }
                            ui.end_row();
                        });
                });
        })
        .response
        .on_disabled_hover_text(dumb_hover());
        if dumb {
            ui.weak(dumb_hover());
        }

        self.shader_section(ui, action);

        // HDR：色調映射在最後輸出到螢幕時做，軟體繪圖的簡化流程也有，所以不看 dumb
        ui.add_space(12.0);
        ui.strong("HDR → SDR");
        let tone = v.tone;
        // 拖曳、打字時馬上生效，放開滑鼠或離開欄位時才存檔（跟播放頁一樣）
        let commit =
            |r: &egui::Response| r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
        egui::Grid::new("settings_hdr")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                if let Some(c) = combo(
                    ui,
                    tr!("曲線", "Curve"),
                    "settings_tone",
                    tone.curve,
                    &ToneCurve::ALL,
                    ToneCurve::label,
                    self.video_disabled(false, "tone-mapping"),
                ) {
                    *action = Some(Action::SetTone(c));
                }
                ui.end_row();
                let name = ui
                    .label(tr!("目標亮度", "Target brightness"))
                    .on_hover_text(peak_hover());
                let locked = self.video_locked("target-peak");
                ui.horizontal(|ui| {
                    let mut auto = tone.target_peak.is_none();
                    let r = ui
                        .add_enabled(!locked, egui::Checkbox::new(&mut auto, tr!("自動", "Auto")))
                        .on_hover_text(peak_hover())
                        .on_disabled_hover_text(adjust_locked_hover());
                    if r.changed() {
                        // 取消自動時從自動的值（203 nits，SDR 的參考白）開始：畫面不會突然變
                        *action = Some(Action::SetTargetPeak((!auto).then_some(ToneSettings::AUTO_PEAK)));
                    }
                    let mut nits = tone.target_peak.unwrap_or(ToneSettings::AUTO_PEAK);
                    let mut r = ui
                        .add_enabled(
                            !locked && !auto,
                            egui::DragValue::new(&mut nits)
                                .range(ToneSettings::MIN_PEAK..=ToneSettings::MAX_PEAK)
                                .speed(5.0)
                                .suffix(" nits"),
                        )
                        .on_hover_text(peak_hover())
                        .labelled_by(name.id);
                    if locked {
                        r = r.on_disabled_hover_text(adjust_locked_hover());
                    }
                    if r.changed() && !auto {
                        self.settings.video.tone.target_peak = Some(nits);
                        self.apply_video();
                    }
                    if commit(&r) && !auto {
                        self.save_settings();
                    }
                });
                ui.end_row();
                if let Some(g) = combo(
                    ui,
                    tr!("色域對應", "Gamut mapping"),
                    "settings_gamut",
                    tone.gamut,
                    &Gamut::ALL,
                    Gamut::label,
                    self.video_disabled(false, "gamut-mapping-mode"),
                ) {
                    *action = Some(Action::SetGamut(g));
                }
                ui.end_row();
            });
        // 動態峰值偵測要畫面輸出的 OpenGL 有 GLSL 4.20 + compute shader（看驅動：NVIDIA 的 Windows 驅動給 3.3、macOS 是 4.1，都沒有）：
        // 不能用時不顯示（停用的話是一個說不清楚的死選項）
        if self.caps.compute_peak {
            let mut on = tone.compute_peak;
            let r = ui
                .add_enabled(
                    !self.video_locked("hdr-compute-peak"),
                    egui::Checkbox::new(&mut on, tr!("動態峰值偵測", "Dynamic peak detection")),
                )
                .on_hover_text(tr!(
                    "依每個畫面的實際亮度調整色調映射，亮暗變化大的影片比較自然",
                    "Adapts the tone mapping to the actual brightness of each scene"
                ))
                .on_disabled_hover_text(adjust_locked_hover());
            if r.changed() {
                *action = Some(Action::SetComputePeak(on));
            }
        }
        if self.video_not_hdr() {
            ui.weak(tr!("目前的影片不是 HDR", "The current video isn't HDR"));
        }
    }

    /// 音效頁：輸出裝置、獨佔模式、等化器、音量平衡、音量上限、轉成立體聲、音訊直通（選了馬上套用、存檔）
    fn sound_page(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let a = self.settings.audio.clone();
        ui.strong(tr!("輸出裝置", "Output device"));
        let devices = self.device_choices();
        let saved = a.device.clone().filter(|d| d != AUTO_DEVICE);
        // 下拉選單的文字：存下的裝置（拔掉了就註明）或預設裝置
        let current = match &saved {
            None => auto_device_label().to_owned(),
            Some(name) => {
                let label = devices
                    .iter()
                    .find(|d| &d.name == name)
                    .map(|d| d.label().to_owned())
                    .or_else(|| a.device_label.clone())
                    .unwrap_or_else(|| name.clone());
                if self.device_missing() {
                    tf!(
                        "{label}（找不到，暫用預設裝置）",
                        "{label} (not found; using the default)"
                    )
                } else {
                    label
                }
            }
        };
        let mut chosen: Option<Option<AudioDevice>> = None;
        let disabled = self.sound_disabled("audio-device");
        ui.horizontal(|ui| {
            let name = ui.label(tr!("裝置", "Device"));
            let r = ui
                .add_enabled_ui(disabled.is_none(), |ui| {
                    egui::ComboBox::from_id_salt("settings_audio_device")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(saved.is_none(), auto_device_label()).clicked() && saved.is_some() {
                                chosen = Some(None);
                            }
                            for d in &devices {
                                let on = saved.as_deref() == Some(d.name.as_str());
                                if ui.selectable_label(on, d.label()).on_hover_text(&d.name).clicked() && !on {
                                    chosen = Some(Some(d.clone()));
                                }
                            }
                        })
                        .response
                })
                .inner
                .labelled_by(name.id);
            if let Some(why) = disabled {
                r.on_disabled_hover_text(why);
            }
        });
        if let Some(d) = chosen {
            self.select_audio_device(d.as_ref());
        }
        if self.exclusive_shown() {
            let mut on = a.exclusive;
            let r = ui
                .add_enabled(
                    !self.sound_locked("audio-exclusive"),
                    egui::Checkbox::new(&mut on, tr!("獨佔模式", "Exclusive mode")),
                )
                .on_hover_text(super::tuning_menu::exclusive_hover())
                .on_disabled_hover_text(adjust_locked_hover());
            if r.changed() {
                *action = Some(Action::ToggleExclusive);
            }
        }

        ui.add_space(12.0);
        ui.strong(tr!("等化器", "Equalizer"));
        ui.horizontal(|ui| {
            ui.label(self.eq_summary());
            // 十段滑桿在控制面板（不擋住畫面，邊聽邊調）
            if ui.button(tr!("等化器…", "Equalizer…")).clicked() {
                *action = Some(Action::ShowEqualizer);
            }
        });
        if let Some(why) = self.eq_disabled() {
            ui.weak(why);
        }

        ui.add_space(12.0);
        ui.strong(tr!("音量", "Volume"));
        ui.horizontal(|ui| {
            if let Some(m) = self.leveling_combo(ui, "settings_leveling") {
                *action = Some(Action::SetLeveling(m));
            }
        });
        ui.horizontal(|ui| {
            if let Some(v) = self.volume_max_combo(ui, "settings_volume_max") {
                *action = Some(Action::SetVolumeMax(v));
            }
        });

        ui.add_space(12.0);
        ui.strong(tr!("聲道", "Channels"));
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
            .on_hover_text(super::tuning_menu::downmix_hover())
            .on_disabled_hover_text(disabled.unwrap_or_default());
        if r.changed() {
            *action = Some(Action::ToggleDownmix);
        }
        ui.indent("settings_normalize_downmix", |ui| {
            let mut on = a.normalize_downmix;
            let locked = self.sound_locked("audio-normalize-downmix");
            let r = ui
                .add_enabled(
                    a.downmix && !locked && disabled.is_none(),
                    egui::Checkbox::new(&mut on, tr!("混音時避免破音", "Avoid clipping when downmixing")),
                )
                .on_hover_text(tr!(
                    "混音時先把音量降低一些，大聲的地方不會破音（整體會小聲一點）",
                    "Lowers the level while mixing so loud parts don't clip (overall a little quieter)"
                ));
            let r = match (locked, disabled) {
                (true, _) => r.on_disabled_hover_text(adjust_locked_hover()),
                (false, Some(why)) => r.on_disabled_hover_text(why),
                (false, None) => r,
            };
            if r.changed() {
                self.set_normalize_downmix(on);
            }
        });

        ui.add_space(12.0);
        ui.strong(tr!("音訊直通", "Passthrough"));
        let p = a.passthrough;
        let locked = self.sound_locked("audio-spdif");
        let mut on = p.enabled;
        let r = ui
            .add_enabled(!locked, egui::Checkbox::new(&mut on, tr!("啟用", "Enable")))
            .on_hover_text(super::tuning_menu::passthrough_hover())
            .on_disabled_hover_text(adjust_locked_hover());
        if r.changed() {
            *action = Some(Action::TogglePassthrough);
        }
        let mut codecs = p;
        ui.indent("settings_spdif_codecs", |ui| {
            ui.horizontal_wrapped(|ui| {
                for c in SPDIF_CODECS {
                    let mut ticked = codecs.codec(c);
                    let r = ui.add_enabled(p.enabled && !locked, egui::Checkbox::new(&mut ticked, spdif_label(c)));
                    let r = if locked {
                        r.on_disabled_hover_text(adjust_locked_hover())
                    } else {
                        r
                    };
                    if r.changed() {
                        codecs.set_codec(c, ticked);
                    }
                }
            });
            if let Some(f) = self.spdif_active() {
                ui.weak(tf!("使用中：{}", "Active: {}", spdif_label(f)));
            }
            ui.weak(if cfg!(any(windows, target_os = "macos")) {
                tr!(
                    "直通時會獨佔這個裝置，其他程式暫時沒有聲音；TrueHD／DTS-HD 需要 HDMI 支援 HBR",
                    "Passthrough takes over the device, so other apps go silent meanwhile; \
                     TrueHD and DTS-HD need HDMI with HBR support"
                )
            } else {
                tr!("需要 HDMI／IEC958 裝置", "Needs an HDMI or IEC958 (S/PDIF) device")
            });
        });
        if codecs != p {
            self.set_passthrough_codecs(codecs);
        }
    }

    fn subtitles_page(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let style = &self.settings.subtitle;
        ui.label(tf!(
            "字型：{}　字級：{:.0}",
            "Font: {}   Size: {:.0}",
            style.effective_font(),
            style.size
        ));
        ui.add_space(6.0);
        if ui.button(tr!("字幕外觀…", "Subtitle style…")).clicked() {
            *action = Some(Action::SubtitleStyle);
        }
        ui.add_space(6.0);
        ui.weak(tr!(
            "外掛字幕會自動載入、自動判斷編碼，繁體中文優先。字幕延遲、第二字幕在控制列的「字幕」選單。",
            "External subtitles are loaded automatically with their encoding detected; Traditional Chinese comes first. \
             Subtitle delay and the secondary subtitle are in the Subtitles menu on the control bar."
        ));
    }

    fn screenshot_page(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) -> bool {
        let mut changed = false;
        ui.label(tr!("截圖資料夾", "Screenshot folder"));
        ui.add(egui::Label::new(egui::RichText::new(self.screenshot_dir().display().to_string()).monospace()).wrap());
        ui.horizontal(|ui| {
            if ui.button(tr!("變更…", "Change…")).clicked() {
                *action = Some(Action::ChooseScreenshotDir);
            }
            if ui.button(tr!("開啟", "Open")).clicked() {
                *action = Some(Action::OpenScreenshotDir);
            }
            if ui
                .add_enabled(
                    self.settings.screenshot_dir.is_some(),
                    egui::Button::new(tr!("還原預設", "Use default")),
                )
                .clicked()
            {
                self.settings.screenshot_dir = None;
                changed = true;
            }
        });
        ui.add_space(6.0);
        changed |= ui
            .checkbox(
                &mut self.settings.screenshot_subtitles,
                tr!("包含字幕", "Include subtitles"),
            )
            .changed();
        ui.weak(tr!(
            "截圖是原始解析度的 PNG 檔。",
            "Screenshots are PNG files at the video's original resolution."
        ));
        changed
    }
}

/// 快捷鍵一覽（唯讀）
/// 快捷鍵說明裡的 Ctrl（macOS 是 ⌘）
const CMD: &str = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };

fn shortcuts_page(ui: &mut egui::Ui) {
    let cmd = CMD;
    let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
    let rows: Vec<(String, &str)> = vec![
        (tr!("空白鍵", "Space").to_owned(), tr!("播放 / 暫停", "Play / pause")),
        ("← / →".to_owned(), tr!("後退 / 前進", "Seek backward / forward")),
        (format!("{cmd} + ← / →"), tr!("大幅後退 / 前進", "Seek further")),
        ("↑ / ↓".to_owned(), tr!("音量", "Volume")),
        ("M".to_owned(), tr!("靜音", "Mute")),
        (
            tr!("F、Enter、雙擊畫面", "F, Enter, double-click").to_owned(),
            tr!("全螢幕", "Fullscreen"),
        ),
        (
            "PgUp / PgDn".to_owned(),
            tr!("上一個 / 下一個檔案", "Previous / next file"),
        ),
        (
            format!("{cmd} + PgUp / PgDn"),
            tr!("上一章 / 下一章", "Previous / next chapter"),
        ),
        (
            "C / X / Z".to_owned(),
            tr!("加快 / 減慢 / 正常速度", "Faster / slower / normal speed"),
        ),
        (". / ,".to_owned(), tr!("逐格前進 / 後退", "Next / previous frame")),
        ("L".to_owned(), tr!("A-B 重播", "A-B loop")),
        ("Home".to_owned(), tr!("從頭播放", "Play from the start")),
        ("[ / ]".to_owned(), tr!("字幕提早 / 延後", "Subtitle delay")),
        ("- / = (+)".to_owned(), tr!("聲音提早 / 延後", "Audio delay")),
        (format!("A / {cmd} + F6"), tr!("畫面比例", "Aspect ratio")),
        (
            format!("{} + Q", if cfg!(target_os = "macos") { "Control" } else { "Ctrl" }),
            tr!("裁切", "Crop"),
        ),
        (
            "9 / 1 / 5".to_owned(),
            tr!("放大 / 縮小 / 100%", "Zoom in / out / 100%"),
        ),
        (format!("{alt} + ←↑↓→"), tr!("移動畫面", "Move the picture")),
        (format!("{cmd} + 5"), tr!("畫面移回中間", "Center the picture")),
        (format!("{alt} + K"), tr!("旋轉 90°", "Rotate 90°")),
        (
            format!("{cmd} + Z / P"),
            tr!("左右 / 上下翻轉", "Flip horizontally / vertically"),
        ),
        (format!("{alt} + Backspace"), tr!("畫面調整還原", "Reset the picture")),
        ("W / E".to_owned(), tr!("亮度 - / +", "Brightness - / +")),
        ("R / T".to_owned(), tr!("對比 - / +", "Contrast - / +")),
        ("Y / U".to_owned(), tr!("飽和度 - / +", "Saturation - / +")),
        ("I / O".to_owned(), tr!("色相 - / +", "Hue - / +")),
        ("Q".to_owned(), tr!("影像調整還原", "Reset image adjustments")),
        (
            format!("{alt} + G"),
            tr!(
                "控制面板（影像調整、等化器）",
                "Control panel (image adjustments, equalizer)"
            ),
        ),
        (format!("{cmd} + T"), tr!("視窗置頂", "Always on top")),
        ("F6".to_owned(), tr!("播放清單", "Playlist")),
        (
            if cfg!(target_os = "macos") {
                "Delete / ⌫".to_owned()
            } else {
                "Delete".to_owned()
            },
            tr!(
                "從播放清單移除（清單開著時）",
                "Remove from the playlist (when it is open)"
            ),
        ),
        (
            if cfg!(target_os = "macos") {
                "⌘ + I".to_owned()
            } else {
                "Ctrl + F1 / Ctrl + I".to_owned()
            },
            tr!("媒體資訊", "Media info"),
        ),
        (format!("{cmd} + E"), tr!("擷取畫面（存檔）", "Save a screenshot")),
        (format!("{cmd} + C"), tr!("擷取畫面（剪貼簿）", "Copy the frame")),
        (format!("{cmd} + O"), tr!("開啟檔案", "Open a file")),
        ("F5".to_owned(), tr!("設定", "Settings")),
        ("F1".to_owned(), tr!("關於", "About")),
        (
            "Esc".to_owned(),
            tr!("關閉視窗 / 離開全螢幕", "Close a panel / leave fullscreen"),
        ),
    ];
    egui::Grid::new("settings_shortcuts")
        .num_columns(2)
        .striped(true)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for (key, what) in rows {
                ui.label(egui::RichText::new(key).monospace());
                ui.label(what);
                ui.end_row();
            }
        });
}
