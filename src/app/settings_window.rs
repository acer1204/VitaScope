//! 設定視窗（F5、右鍵選單「設定…」）：改了馬上生效、馬上存檔。

use super::{Action, VitascopeApp};
use crate::i18n::{self, Lang};
use crate::{tf, tr};
use eframe::egui::{self, Id, pos2, vec2};

/// 設定視窗的分頁
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Page {
    #[default]
    General,
    Playback,
    Subtitles,
    Screenshot,
    Shortcuts,
}

impl Page {
    const ALL: [Page; 5] = [
        Page::General,
        Page::Playback,
        Page::Subtitles,
        Page::Screenshot,
        Page::Shortcuts,
    ];

    fn title(self) -> &'static str {
        match self {
            Page::General => tr!("一般", "General"),
            Page::Playback => tr!("播放", "Playback"),
            Page::Subtitles => tr!("字幕", "Subtitles"),
            Page::Screenshot => tr!("截圖", "Screenshots"),
            Page::Shortcuts => tr!("快捷鍵", "Shortcuts"),
        }
    }
}

impl VitascopeApp {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
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
                                Page::Subtitles => self.subtitles_page(ui, &mut action),
                                Page::Screenshot => changed |= self.screenshot_page(ui, &mut action),
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
            });
        ui.add_space(6.0);
        let mut on_top = self.settings.always_on_top;
        if ui.checkbox(&mut on_top, tr!("視窗置頂", "Always on top")).changed() {
            // 跟快捷鍵、右鍵選單走同一條路（Wayland 不支援時會提示）
            *action = Some(Action::ToggleOnTop);
        }
        changed
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
        egui::Grid::new("settings_playback")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label(tr!("← / → 跳轉", "← / → seek"));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut s.seek_short)
                            .range(1.0..=60.0)
                            .speed(0.2)
                            .suffix(tr!(" 秒", " s")),
                    )
                    .changed();
                ui.end_row();
                ui.label(tr!("Ctrl + ← / → 跳轉", "Ctrl + ← / → seek"));
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut s.seek_long)
                            .range(5.0..=600.0)
                            .speed(1.0)
                            .suffix(tr!(" 秒", " s")),
                    )
                    .changed();
                ui.end_row();
            });
        changed
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
fn shortcuts_page(ui: &mut egui::Ui) {
    let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
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
        ("- / =".to_owned(), tr!("聲音提早 / 延後", "Audio delay")),
        ("A".to_owned(), tr!("畫面比例", "Aspect ratio")),
        (
            format!("{} + Q", if cfg!(target_os = "macos") { "Control" } else { "Ctrl" }),
            tr!("裁切", "Crop"),
        ),
        (
            "9 / 1 / 5".to_owned(),
            tr!("放大 / 縮小 / 100%", "Zoom in / out / 100%"),
        ),
        (format!("{alt} + ←↑↓→"), tr!("移動畫面", "Move the picture")),
        (format!("{alt} + K"), tr!("旋轉 90°", "Rotate 90°")),
        (
            format!("{cmd} + Z / P"),
            tr!("左右 / 上下翻轉", "Flip horizontally / vertically"),
        ),
        (format!("{alt} + Backspace"), tr!("畫面調整還原", "Reset the picture")),
        (format!("{cmd} + T"), tr!("視窗置頂", "Always on top")),
        ("F6".to_owned(), tr!("播放清單", "Playlist")),
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
