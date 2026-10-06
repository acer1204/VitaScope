//! 擷取畫面（Ctrl+E 存檔、Ctrl+C 複製到剪貼簿、右鍵選單「擷取畫面」）。截圖的處理見 `screenshot.rs`。

use super::{Action, VitascopeApp, file_name, menu_item};
use crate::screenshot::{self, Done, Fixup, Target};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc;

/// 截圖用的非同步指令編號從這裡開始（跟其他非同步指令分開）
const SHOT_ID_BASE: u64 = 1 << 40;

/// 截圖相關的狀態
pub(super) struct Capture {
    seq: u64,
    /// 已經請 mpv 截圖、還沒回覆的（指令編號 → 存到哪裡、要怎麼轉正）
    pending: Vec<(u64, Target, Fixup)>,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
}

impl Default for Capture {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            seq: 0,
            pending: Vec::new(),
            tx,
            rx,
        }
    }
}

/// 截圖存到哪裡
pub(super) enum ShotDest {
    /// 截圖資料夾，自動取檔名
    Folder,
    /// 使用者選的檔案
    File(PathBuf),
    Clipboard,
}

impl VitascopeApp {
    pub(super) fn screenshot_dir(&self) -> PathBuf {
        self.settings
            .screenshot_dir
            .clone()
            .unwrap_or_else(screenshot::default_dir)
    }

    /// 截圖要再轉正的部分：mpv 的軟體截圖不含畫面輸出做的旋轉（視窗裡由畫面輸出轉；
    /// 沒有畫面時 mpv 用濾鏡先轉好，截圖本來就是轉好的），也不含翻轉著色器
    fn shot_fixup(&self) -> Fixup {
        let rotate = self.player.out_params().map_or(0, |o| o.rotate.rem_euclid(360) as u32);
        // 用濾鏡翻轉時（軟體繪圖），截圖裡已經翻好了
        let shader_flip = !self.flip_with_filter();
        Fixup {
            rotate,
            hflip: shader_flip && self.geometry.hflip,
            vflip: shader_flip && self.geometry.vflip,
        }
    }

    pub(super) fn take_screenshot(&mut self, dest: ShotDest) {
        let st = &self.player.state;
        if !st.loaded || !st.has_video() {
            self.osd("這個檔案沒有影像，不能擷取畫面");
            return;
        }
        let fix = self.shot_fixup();
        self.capture.seq += 1;
        let n = self.capture.seq;
        let target = match dest {
            ShotDest::Clipboard => Target::Clipboard {
                tmp: screenshot::temp_path(n),
            },
            ShotDest::Folder | ShotDest::File(_) => {
                let path = match dest {
                    ShotDest::File(p) => p,
                    _ => {
                        let dir = self.screenshot_dir();
                        if let Err(e) = std::fs::create_dir_all(&dir) {
                            self.osd(format!("無法建立截圖資料夾：{e}"));
                            return;
                        }
                        let source = st.path.clone().unwrap_or_default();
                        screenshot::unique_path(&dir, &screenshot::file_name(&source, st.time_pos))
                    }
                };
                // 不用轉正：mpv 直接存到最後的位置
                let tmp = if fix.is_none() {
                    path.clone()
                } else {
                    screenshot::temp_path(n)
                };
                Target::File { tmp, path }
            }
        };
        let tmp = match &target {
            Target::File { tmp, .. } | Target::Clipboard { tmp } => tmp.to_string_lossy().into_owned(),
        };
        let id = SHOT_ID_BASE + n;
        // 畫面翻轉時要把截圖翻回來，字幕也會跟著變成鏡像（畫面上的字幕不會翻）：這時不含字幕
        let flipped = fix.hflip || fix.vflip;
        let subtitles = self.settings.screenshot_subtitles && !flipped;
        match self.player.screenshot_to_file(id, &tmp, subtitles) {
            Ok(()) => {
                self.capture.pending.push((id, target, fix));
                if flipped && self.settings.screenshot_subtitles {
                    self.osd("畫面翻轉時，截圖不含字幕");
                }
            }
            Err(e) => self.osd(format!("無法擷取畫面：{e}")),
        }
    }

    /// mpv 回覆非同步指令：截圖寫好了就交給背景執行緒轉正、存檔或讀回來
    pub(super) fn on_command_reply(&mut self, id: u64, error: Option<String>) {
        let Some(i) = self.capture.pending.iter().position(|(pid, ..)| *pid == id) else {
            return;
        };
        let (_, target, fix) = self.capture.pending.remove(i);
        if let Some(e) = error {
            self.osd(format!("無法擷取畫面：{e}"));
            return;
        }
        let tx = self.capture.tx.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let done = screenshot::finish(target, fix);
            if tx.send(done).is_ok() {
                ctx.request_repaint();
            }
        });
    }

    /// 背景處理完的截圖
    pub(super) fn poll_screenshots(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.capture.rx.try_recv() {
            match done {
                Done::Saved(path) => self.osd(format!("已儲存截圖：{}", file_name(&path))),
                Done::Copied(img) => {
                    ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.rgba));
                    self.osd(format!("已複製畫面（{}×{}）", img.w, img.h));
                }
                Done::Failed(e) => self.osd(format!("無法擷取畫面：{e}")),
            }
        }
    }

    pub(super) fn screenshot_as_dialog(&mut self) {
        let st = &self.player.state;
        if !st.loaded || !st.has_video() {
            return;
        }
        let name = screenshot::file_name(st.path.as_deref().unwrap_or_default(), st.time_pos);
        let Some(mut path) = self
            .file_dialog()
            .set_title("另存截圖")
            .add_filter("PNG 圖片", &["png"])
            .set_directory(self.screenshot_dir())
            .set_file_name(&name)
            .save_file()
        else {
            return;
        };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
            path.set_extension("png");
        }
        self.take_screenshot(ShotDest::File(path));
    }

    pub(super) fn choose_screenshot_dir(&mut self) {
        if let Some(dir) = self
            .file_dialog()
            .set_title("選擇截圖資料夾")
            .set_directory(self.screenshot_dir())
            .pick_folder()
        {
            self.osd(format!("截圖資料夾：{}", dir.display()));
            self.settings.screenshot_dir = Some(dir);
            self.save_settings();
        }
    }

    /// 右鍵選單「擷取畫面」
    pub(super) fn screenshot_menu(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<Action> {
        let mut action = None;
        ui.add_enabled_ui(enabled, |ui| {
            ui.menu_button("擷取畫面", |ui| {
                if menu_item(ui, true, "存到截圖資料夾", SHOT_SHORTCUT) {
                    action = Some(Action::Screenshot);
                }
                if menu_item(ui, true, "另存新檔…", "") {
                    action = Some(Action::ScreenshotAs);
                }
                if menu_item(ui, true, "複製到剪貼簿", COPY_SHORTCUT) {
                    action = Some(Action::CopyFrame);
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.settings.screenshot_subtitles, "包含字幕")
                    .changed()
                {
                    self.save_settings();
                }
                ui.separator();
                if menu_item(ui, true, "開啟截圖資料夾", "") {
                    action = Some(Action::OpenScreenshotDir);
                }
                if menu_item(ui, true, "變更截圖資料夾…", "") {
                    action = Some(Action::ChooseScreenshotDir);
                }
                ui.weak(self.screenshot_dir().display().to_string());
            });
        });
        action
    }
}

/// 截圖的快捷鍵說明
pub(super) const SHOT_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+E" } else { "Ctrl+E" };
pub(super) const COPY_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+C" } else { "Ctrl+C" };
