//! 擷取畫面（Ctrl+E 存檔、Ctrl+C 複製到剪貼簿、右鍵選單「擷取畫面」）。截圖的處理見 `screenshot.rs`。

use super::{Action, DialogKind, Pick, VitascopeApp, file_name, menu_item};
use crate::keymap::Command;
use crate::screenshot::{self, Done, Fixup, Target};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc;

/// 截圖用的非同步指令編號從這裡開始（跟其他非同步指令分開）
const SHOT_ID_BASE: u64 = 1 << 40;

/// 第 n 張截圖的指令編號：流水號只用低 40 位元，編號一定在 [1<<40, 1<<41) 之間，
/// 不會跑進設定選項用的那一段（`player::ASYNC_BASE` = 1<<44 起算）
fn shot_id(n: u64) -> u64 {
    SHOT_ID_BASE | (n & (SHOT_ID_BASE - 1))
}

/// 截圖相關的狀態
pub(super) struct Capture {
    seq: u64,
    /// 已經請 mpv 截圖、還沒回覆的（指令編號 → 存到哪裡、要怎麼轉正）
    pending: Vec<(u64, Target, Fixup)>,
    /// 背景還在轉正、寫檔的截圖
    writing: Vec<PathBuf>,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
}

impl Default for Capture {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            seq: 0,
            pending: Vec::new(),
            writing: Vec::new(),
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
            self.osd(crate::tr!(
                "這個檔案沒有影像，不能擷取畫面",
                "This file has no video to capture"
            ));
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
                            self.osd(crate::tf!(
                                "無法建立截圖資料夾：{e}",
                                "Cannot create the screenshot folder: {e}"
                            ));
                            return;
                        }
                        let source = st.path.clone().unwrap_or_default();
                        let name =
                            screenshot::file_name(&source, self.titles.get(&source).map(String::as_str), st.time_pos);
                        // 還在寫的截圖還沒出現在資料夾裡：連按（例如暫停時）檔名也不能重複
                        let taken: Vec<PathBuf> = self
                            .capture
                            .pending
                            .iter()
                            .filter_map(|(_, t, _)| match t {
                                Target::File { path, .. } => Some(path.clone()),
                                Target::Clipboard { .. } => None,
                            })
                            .chain(self.capture.writing.iter().cloned())
                            .collect();
                        screenshot::unique_path_except(&dir, &name, &taken)
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
        let id = shot_id(n);
        let wanted = self.settings.screenshot_subtitles;
        let subtitles = fix.keeps_subtitles(wanted);
        match self.player.screenshot_to_file(id, &tmp, subtitles) {
            Ok(()) => {
                self.capture.pending.push((id, target, fix));
                // 有字幕在顯示時才提醒（手機直拍的影片本來就要轉正，沒有字幕就不用說）
                if wanted && !subtitles && self.player.state.sid.is_some() {
                    self.osd(crate::tr!(
                        "畫面旋轉或翻轉時，截圖不含字幕",
                        "Screenshots of a rotated or flipped picture leave out subtitles"
                    ));
                }
            }
            Err(e) => self.osd(crate::tf!("無法擷取畫面：{e}", "Cannot capture the frame: {e}")),
        }
    }

    /// mpv 回覆非同步指令：截圖寫好了就交給背景執行緒轉正、存檔或讀回來
    pub(super) fn on_command_reply(&mut self, id: u64, error: Option<String>) {
        let Some(i) = self.capture.pending.iter().position(|(pid, ..)| *pid == id) else {
            return;
        };
        let (_, target, fix) = self.capture.pending.remove(i);
        if let Some(e) = error {
            self.osd(crate::tf!("無法擷取畫面：{e}", "Cannot capture the frame: {e}"));
            return;
        }
        if let Target::File { path, .. } = &target {
            self.capture.writing.push(path.clone());
        }
        let tx = self.capture.tx.clone();
        let ctx = self.egui_ctx.clone();
        let lang = crate::i18n::lang();
        std::thread::spawn(move || {
            crate::i18n::set_lang(lang);
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
                Done::Saved(path) => {
                    self.capture.writing.retain(|p| *p != path);
                    self.osd(crate::tf!("已儲存截圖：{}", "Screenshot saved: {}", file_name(&path)));
                }
                Done::Copied(img) => {
                    ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied([img.w, img.h], &img.rgba));
                    self.osd(crate::tf!("已複製畫面（{}×{}）", "Frame copied ({}×{})", img.w, img.h));
                }
                Done::Failed { path, error } => {
                    if let Some(path) = path {
                        self.capture.writing.retain(|p| *p != path);
                    }
                    self.osd(crate::tf!("無法擷取畫面：{error}", "Cannot capture the frame: {error}"));
                }
            }
        }
    }

    pub(super) fn screenshot_as_dialog(&mut self) {
        let st = &self.player.state;
        if !st.loaded || !st.has_video() {
            return;
        }
        // 已經開著別的對話框（Linux 的對話框不一定擋得住主視窗）：不開
        let kind = DialogKind::ScreenshotSaveAs;
        if self.refuse_second_dialog(kind) {
            return;
        }
        // 對話框開著時 mpv 照樣在播：先暫停，存的才是選「另存新檔」時的畫面（檔名的時間也一樣）。
        // 所以這個對話框照樣在介面的執行緒上開（開著時本來就暫停）
        let was_playing = self.pause_for_dialog();
        let time = self.player.get_f64("time-pos").unwrap_or(self.player.state.time_pos);
        let source = self.player.state.path.clone().unwrap_or_default();
        let name = screenshot::file_name(&source, self.titles.get(&source).map(String::as_str), time);
        let dialog = self
            .file_dialog()
            .set_title(crate::tr!("另存截圖", "Save screenshot as"))
            .add_filter(crate::tr!("PNG 圖片", "PNG image"), &["png"])
            .set_directory(self.screenshot_dir())
            .set_file_name(&name);
        let chosen = self.dialog_runner(kind, Pick::Save, dialog)();
        if let Some(paths) = chosen.filter(|p| !p.is_empty()) {
            self.on_dialog_result(kind, paths);
        }
        self.resume_after_dialog(was_playing);
    }

    /// 截圖存到 `path`（另存新檔對話框選好的；沒有 .png 就補上）
    pub(super) fn save_screenshot_as(&mut self, mut path: PathBuf) {
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
            path.set_extension("png");
        }
        // mpv 收到指令時就取下畫面（之後才在背景編碼），接著繼續播沒關係
        self.take_screenshot(ShotDest::File(path));
    }

    pub(super) fn choose_screenshot_dir(&mut self) {
        let dialog = self
            .file_dialog()
            .set_title(crate::tr!("選擇截圖資料夾", "Choose the screenshot folder"))
            .set_directory(self.screenshot_dir());
        self.show_dialog(DialogKind::ScreenshotDir, Pick::Folder, dialog);
    }

    /// 截圖資料夾改成 `dir`（資料夾對話框選好的）
    pub(super) fn set_screenshot_dir(&mut self, dir: PathBuf) {
        self.osd(crate::tf!("截圖資料夾：{}", "Screenshot folder: {}", dir.display()));
        self.settings.screenshot_dir = Some(dir);
        self.save_settings();
    }

    /// 右鍵選單「擷取畫面」
    pub(super) fn screenshot_menu(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<Action> {
        let mut action = None;
        ui.add_enabled_ui(enabled, |ui| {
            ui.menu_button(crate::tr!("擷取畫面", "Screenshot"), |ui| {
                if self.cmd_item(
                    ui,
                    true,
                    crate::tr!("存到截圖資料夾", "Save to the screenshot folder"),
                    Command::Screenshot,
                ) {
                    action = Some(Action::Screenshot);
                }
                if self.cmd_item(ui, true, crate::tr!("另存新檔…", "Save as…"), Command::ScreenshotAs) {
                    action = Some(Action::ScreenshotAs);
                }
                if self.cmd_item(
                    ui,
                    true,
                    crate::tr!("複製到剪貼簿", "Copy to clipboard"),
                    Command::CopyFrame,
                ) {
                    action = Some(Action::CopyFrame);
                }
                ui.separator();
                if ui
                    .checkbox(
                        &mut self.settings.screenshot_subtitles,
                        crate::tr!("包含字幕", "Include subtitles"),
                    )
                    .changed()
                {
                    self.save_settings();
                }
                ui.separator();
                if menu_item(ui, true, crate::tr!("開啟截圖資料夾", "Open the screenshot folder"), "") {
                    action = Some(Action::OpenScreenshotDir);
                }
                if menu_item(
                    ui,
                    true,
                    crate::tr!("變更截圖資料夾…", "Change the screenshot folder…"),
                    "",
                ) {
                    action = Some(Action::ChooseScreenshotDir);
                }
                ui.weak(self.screenshot_dir().display().to_string());
            });
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::{SHOT_ID_BASE, shot_id};
    use crate::player::{ASYNC_BASE, AsyncKey, async_key};

    #[test]
    fn screenshot_ids_never_look_like_option_replies() {
        for n in [1, 2, 1000, (1 << 40) - 1, 1 << 40, (1 << 44) + 7, u64::MAX] {
            let id = shot_id(n);
            assert!((SHOT_ID_BASE..SHOT_ID_BASE << 1).contains(&id), "{n:#x} → {id:#x}");
            assert!(id < ASYNC_BASE);
            assert_eq!(async_key(id), None, "{n:#x} → {id:#x}");
        }
        // 平常的編號跟以前一樣
        assert_eq!(shot_id(3), SHOT_ID_BASE + 3);
        // 反過來：設定選項的編號（一定 ≥ ASYNC_BASE）也不在截圖那一段
        const { assert!(ASYNC_BASE >= SHOT_ID_BASE << 1) };
        assert!(AsyncKey::ALL.iter().all(|k| (*k as u64) < 1 << 20));
    }
}
