//! 控制面板（Alt+G、右鍵選單「畫質 → 影像調整…」「音效 → 等化器…」）：不擋住操作的小視窗，
//! 滑桿一動畫面、聲音馬上跟著變。
//! 也放影像調整（亮度、對比、飽和度、色相、Gamma）本身的邏輯：快捷鍵、右鍵選單、設定頁都走這裡。

use super::quality::combo;
use super::{Action, VitascopeApp, mpv_opts_override};
use crate::picture::{Adjust, AdjustKind, Deinterlace, Strength, fmt_signed};
use crate::player::AsyncKey;
use crate::sound::{EQ_BAND_LABELS, EQ_MAX_GAIN, EqPreset};
use crate::{tf, tr};
use eframe::egui::{self, Align, Align2, Id, Layout, vec2};

/// 控制面板的分頁
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum PanelTab {
    #[default]
    Picture,
    /// 等化器、音量平衡、轉成立體聲、音量上限
    Sound,
}

impl PanelTab {
    const ALL: [PanelTab; 2] = [PanelTab::Picture, PanelTab::Sound];

    fn title(self) -> &'static str {
        match self {
            PanelTab::Picture => tr!("畫質", "Video quality"),
            PanelTab::Sound => tr!("音效", "Sound"),
        }
    }
}

/// 等化器沒開：滑桿停用時的說明
fn eq_off_hover() -> &'static str {
    tr!("先勾選「等化器」", "Tick \"Equalizer\" first")
}

/// 使用者用 VITASCOPE_MPV_OPTS 指定了這一項：停用時的說明
pub(super) fn adjust_locked_hover() -> &'static str {
    tr!("已由 VITASCOPE_MPV_OPTS 指定", "Set by VITASCOPE_MPV_OPTS")
}

impl VitascopeApp {
    /// 打開控制面板的「畫質」分頁（右鍵選單、設定頁的「影像調整…」）；已經開著就換到這一頁。
    /// Alt+G 只開關面板，分頁照上次的
    pub(super) fn show_adjustments(&mut self) {
        self.panel_open = true;
        self.panel_tab = PanelTab::Picture;
    }

    pub(super) fn control_panel(&mut self, ctx: &egui::Context) {
        if !self.panel_open {
            return;
        }
        let mut open = true;
        // 預設放在右上角，不蓋住畫面中間
        let corner = ctx.content_rect().right_top() + vec2(-16.0, 16.0);
        egui::Window::new(tr!("控制面板", "Control Panel"))
            .id(Id::new("control_panel"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::RIGHT_TOP)
            .default_pos(corner)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for tab in PanelTab::ALL {
                        if ui.selectable_label(self.panel_tab == tab, tab.title()).clicked() {
                            self.panel_tab = tab;
                        }
                    }
                });
                ui.separator();
                match self.panel_tab {
                    PanelTab::Picture => self.picture_tab(ui),
                    PanelTab::Sound => self.sound_tab(ui),
                }
            });
        if !open {
            self.panel_open = false;
        }
    }

    /// 「畫質」分頁：五個滑桿（拖曳時馬上套用，放開才存檔），銳化、去色帶、去交錯
    fn picture_tab(&mut self, ui: &mut egui::Ui) {
        // 跟設定頁一樣：拖曳、打字時馬上生效，放開滑鼠或離開欄位時才存檔
        let commit =
            |r: &egui::Response| r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged() && !r.has_focus());
        let mut save = false;
        egui::Grid::new("control_panel_adjust")
            .num_columns(3)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                for kind in AdjustKind::ALL {
                    let locked = self.adjust_locked(kind);
                    let name = ui.label(kind.label());
                    let mut value = self.adjust.get(kind);
                    let slider = egui::Slider::new(&mut value, Adjust::MIN..=Adjust::MAX)
                        .step_by(1.0)
                        .custom_formatter(|v, _| fmt_signed(v.round() as i32));
                    // 滑桿用左邊的名稱當無障礙標籤（螢幕閱讀器、介面測試找得到）
                    let r = ui
                        .add_enabled(!locked, slider)
                        .labelled_by(name.id)
                        .on_disabled_hover_text(adjust_locked_hover());
                    if r.changed() {
                        self.set_adjust(kind, value);
                    }
                    save |= commit(&r);
                    let reset = ui
                        .add_enabled(!locked && value != 0, egui::Button::new("↺").small())
                        .on_hover_text(tf!("{} 還原為 0", "Reset {} to 0", kind.label()));
                    if reset.clicked() {
                        self.set_adjust(kind, 0);
                        save = true;
                    }
                    ui.end_row();
                }
            });
        ui.add_space(6.0);
        // 銳化、去色帶、去交錯（整個程式共用的設定，選了馬上存檔）
        let mut action = None;
        let v = self.settings.video.clone();
        ui.horizontal_wrapped(|ui| {
            if let Some(s) = combo(
                ui,
                tr!("銳化", "Sharpening"),
                "panel_sharpen",
                v.sharpen,
                &Strength::ALL,
                Strength::label,
                self.video_disabled(true, "sharpen"),
            ) {
                action = Some(Action::SetSharpen(s));
            }
            ui.add_space(8.0);
            if let Some(s) = combo(
                ui,
                tr!("去色帶", "Debanding"),
                "panel_deband",
                v.deband,
                &Strength::ALL,
                Strength::label,
                self.video_disabled(true, "deband"),
            ) {
                action = Some(Action::SetDeband(s));
            }
            ui.add_space(8.0);
            if let Some(d) = combo(
                ui,
                tr!("去交錯", "Deinterlacing"),
                "panel_deinterlace",
                self.deint_effective(),
                &self.deint_choices(),
                Deinterlace::label,
                self.video_disabled(false, "deinterlace"),
            ) {
                action = Some(Action::SetDeinterlace(d));
            }
            if let Some(now) = self.deint_status() {
                ui.weak(tf!("（目前：{now}）", "(now: {now})"));
            }
        });
        if let Some(a) = action {
            let ctx = ui.ctx().clone();
            self.run(&ctx, a);
        }
        ui.add_space(6.0);
        if ui
            .add_enabled(
                !self.adjust_is_neutral(),
                egui::Button::new(tr!("全部還原（Q）", "Reset all (Q)")),
            )
            .clicked()
        {
            self.reset_adjust();
            save = true;
        }
        let mut keep = self.settings.video.keep_adjust;
        if ui.checkbox(&mut keep, keep_adjust_label()).changed() {
            self.set_keep_adjust(keep);
        }
        ui.weak(tr!(
            "W/E 亮度・R/T 對比・Y/U 飽和度・I/O 色相",
            "W/E brightness · R/T contrast · Y/U saturation · I/O hue"
        ));
        // 沒勾「下次開啟時沿用」時不用存（存檔也不會寫影像調整）
        if save && self.settings.video.keep_adjust {
            self.save_settings();
        }
    }

    /// 「音效」分頁：等化器（開關、預設、還原、自動防止破音、十段滑桿）、音量平衡、轉成立體聲、音量上限。
    /// 拖滑桿時馬上聽得到（af-command），放開才改寫濾鏡鏈、存檔
    fn sound_tab(&mut self, ui: &mut egui::Ui) {
        let commit = |r: &egui::Response| r.drag_stopped() || (r.changed() && !r.dragged());
        let a = self.settings.audio.clone();
        let eq_off = self.eq_disabled();
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            let mut on = a.eq.enabled;
            let r = ui
                .add_enabled(
                    eq_off.is_none(),
                    egui::Checkbox::new(&mut on, tr!("等化器", "Equalizer")),
                )
                .on_disabled_hover_text(eq_off.unwrap_or_default());
            if r.changed() {
                action = Some(Action::ToggleEq);
            }
            ui.add_space(8.0);
            if let Some(p) = combo(
                ui,
                tr!("預設", "Preset"),
                "panel_eq_preset",
                a.eq.preset,
                &EqPreset::ALL,
                EqPreset::label,
                eq_off,
            ) {
                action = Some(Action::SetEqPreset(p));
            }
            let flat = a.eq.effective_gains().iter().all(|g| *g == 0.0);
            let reset = ui
                .add_enabled(eq_off.is_none() && !flat, egui::Button::new(tr!("還原", "Reset")))
                .on_hover_text(tr!("十段都回到 0（平坦）", "Sets all ten bands back to 0 (flat)"));
            if reset.clicked() {
                self.reset_eq();
            }
        });
        ui.horizontal(|ui| {
            let mut auto = a.eq.auto_preamp;
            let r = ui
                .add_enabled(
                    eq_off.is_none(),
                    egui::Checkbox::new(&mut auto, tr!("自動防止破音", "Prevent clipping")),
                )
                .on_hover_text(tr!(
                    "有段落調高時，整體先降低同樣的量，大聲的地方不會破音",
                    "When bands are raised, the overall level is lowered by the same amount so loud parts don't clip"
                ))
                .on_disabled_hover_text(eq_off.unwrap_or_default());
            if r.changed() {
                self.set_auto_preamp(auto);
            }
            if let Some(db) = self.eq_preamp_db() {
                ui.weak(tf!("（前級 {db:.1} dB）", "(preamp {db:.1} dB)"));
            }
        });
        // 十段：由下往上排（頻率、滑桿、增益），頻率的標籤先建立，當滑桿的無障礙標籤
        let gains = a.eq.effective_gains();
        let bands_off = eq_off.or_else(|| (!a.eq.enabled).then(eq_off_hover));
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = 120.0;
            for (band, name) in EQ_BAND_LABELS.into_iter().enumerate() {
                ui.allocate_ui_with_layout(vec2(30.0, 170.0), Layout::bottom_up(Align::Center), |ui| {
                    let label = ui.label(name);
                    let mut g = gains[band];
                    let slider = egui::Slider::new(&mut g, -EQ_MAX_GAIN..=EQ_MAX_GAIN)
                        .vertical()
                        .step_by(0.5)
                        .show_value(false);
                    let r = ui
                        .add_enabled(bands_off.is_none(), slider)
                        .labelled_by(label.id)
                        .on_hover_text(tf!("{name} Hz：{} dB", "{name} Hz: {} dB", super::sound::fmt_gain(g)))
                        .on_disabled_hover_text(bands_off.unwrap_or_default());
                    ui.weak(super::sound::fmt_gain(g));
                    if r.changed() || r.drag_stopped() {
                        self.set_eq_band(band, g, commit(&r));
                    }
                });
            }
        });
        ui.weak(tr!("頻率（Hz）；增益 −12…+12 dB", "Frequency (Hz); gain −12…+12 dB"));
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if let Some(m) = self.leveling_combo(ui, "panel_leveling") {
                action = Some(Action::SetLeveling(m));
            }
            ui.add_space(8.0);
            let mut downmix = a.downmix;
            let disabled = self.downmix_disabled();
            let r = ui
                .add_enabled(
                    disabled.is_none(),
                    egui::Checkbox::new(&mut downmix, tr!("轉成立體聲", "Downmix to stereo")),
                )
                .on_hover_text(super::tuning_menu::downmix_hover())
                .on_disabled_hover_text(disabled.unwrap_or_default());
            if r.changed() {
                action = Some(Action::ToggleDownmix);
            }
            ui.add_space(8.0);
            if let Some(v) = self.volume_max_combo(ui, "panel_volume_max") {
                action = Some(Action::SetVolumeMax(v));
            }
        });
        if let Some(a) = action {
            let ctx = ui.ctx().clone();
            self.run(&ctx, a);
        }
    }

    // ───────────── 影像調整 ─────────────

    /// 使用者用 VITASCOPE_MPV_OPTS（或測試的 `Options.extra`）指定了這一項：不去改它
    pub(super) fn adjust_locked(&self, kind: AdjustKind) -> bool {
        mpv_opts_override(&self.player, kind.mpv())
    }

    /// 可以調的項目都是 0（還原沒有作用）
    pub(super) fn adjust_is_neutral(&self) -> bool {
        AdjustKind::ALL
            .into_iter()
            .all(|k| self.adjust.get(k) == 0 || self.adjust_locked(k))
    }

    /// 目前的調整（「亮度 +10、對比 -5」）；沒有調整時寫「沒有調整」
    pub(super) fn adjust_summary(&self) -> String {
        let summary = self.adjust.summary(|k| self.adjust_locked(k));
        if summary.is_empty() {
            tr!("沒有調整", "No adjustments").to_owned()
        } else {
            summary
        }
    }

    /// 啟動時（還沒開檔）同步套用：勾了「下次開啟時沿用」時是上次存的值，不然全是 0
    pub(super) fn adjust_startup(&mut self) {
        for (name, e) in self.player.apply_sync(&self.adjust.mpv_options()) {
            eprintln!("[vitascope] 無法套用 {name}：{e}");
        }
        // 使用者用 VITASCOPE_MPV_OPTS 指定的項目以 mpv 實際的值為準（面板上的數字才對得上畫面）
        for kind in AdjustKind::ALL {
            if self.adjust_locked(kind)
                && let Ok(v) = self.player.get_f64(kind.mpv())
            {
                self.adjust.set(kind, v.round() as i32);
            }
        }
    }

    /// 改一項：記下來，非同步送給 mpv（畫面輸出的選項同步設定要等畫面輸出執行緒，會卡住介面）
    pub(super) fn set_adjust(&mut self, kind: AdjustKind, value: i32) {
        if self.adjust_locked(kind) {
            return;
        }
        let before = self.adjust.get(kind);
        self.adjust.set(kind, value);
        let after = self.adjust.get(kind);
        if after != before {
            self.set_option_async(AsyncKey::Adjust, kind.mpv(), &after.to_string());
        }
    }

    /// 快捷鍵：加減 1，顯示「亮度 +3」
    pub(super) fn step_adjust(&mut self, kind: AdjustKind, delta: i32) {
        if self.adjust_locked(kind) {
            self.osd(tf!(
                "{}：已由 VITASCOPE_MPV_OPTS 指定",
                "{}: set by VITASCOPE_MPV_OPTS",
                kind.label()
            ));
            return;
        }
        self.set_adjust(kind, self.adjust.get(kind) + delta);
        self.osd(kind.describe(self.adjust.get(kind)));
    }

    /// 全部還原（Q、面板、右鍵選單）
    pub(super) fn reset_adjust(&mut self) {
        for kind in AdjustKind::ALL {
            self.set_adjust(kind, 0);
        }
        self.osd(tr!("影像調整已還原", "Image adjustments reset"));
    }

    /// 「下次開啟時沿用這些調整」。取消勾選時存的值也清成 0：設定檔裡的值就是下次啟動用的值，
    /// 不留用不到的舊數字（再勾回來時存的是當下的調整，不會少什麼）
    pub(super) fn set_keep_adjust(&mut self, on: bool) {
        let video = &mut self.settings.video;
        video.keep_adjust = on;
        if !on {
            video.adjust = Adjust::default();
        }
        self.save_settings();
    }

    /// 存檔前：勾了「下次開啟時沿用」才把這次的調整寫進設定（`save_settings` 呼叫）。
    /// VITASCOPE_MPV_OPTS 指定的項目不是使用者在影戲裡調的，保留設定裡原本的值
    pub(super) fn store_adjust(&mut self) {
        if !self.settings.video.keep_adjust {
            return;
        }
        let mut adjust = self.adjust;
        for kind in AdjustKind::ALL {
            if self.adjust_locked(kind) {
                adjust.set(kind, self.settings.video.adjust.get(kind));
            }
        }
        self.settings.video.adjust = adjust;
    }

    /// 開檔時有影像調整：提醒一下（不然使用者會以為影片本身偏暗、偏色）。
    /// 開檔後已經有別的提示（續播位置、「下一個（2/3）」、字幕載入失敗）時不蓋掉：那些比較要緊
    pub(super) fn adjust_reminder(&mut self) {
        if self.osd.as_ref().is_some_and(|(_, at)| *at >= self.opened_at) {
            return;
        }
        let summary = self.adjust.summary(|k| self.adjust_locked(k));
        if summary.is_empty() {
            return;
        }
        // 純音訊、只有專輯封面的檔案不用提醒
        let has_video = self
            .player
            .get_string("current-tracks/video/albumart")
            .is_ok_and(|v| v == "no");
        if has_video {
            self.osd(tf!(
                "影像調整中：{summary}（Q 還原）",
                "Image adjusted: {summary} (Q to reset)"
            ));
        }
    }
}

/// 「下次開啟時沿用這些調整」（控制面板、設定頁共用）
pub(super) fn keep_adjust_label() -> &'static str {
    tr!("下次開啟時沿用這些調整", "Keep these adjustments next time")
}
