//! 像素著色器（使用者自己的 .glsl 組合，例如 Anime4K、FSRCNNX）：右鍵選單「畫質 ▸ 像素著色器」、
//! 「設定 → 畫質」的組合編輯，以及換了之後畫不出來時還原到之前的組合。
//! 組合整個程式共用、存檔；glsl-shaders 由 `Player` 組出來送（組合 + 翻轉）。

use super::control_panel::adjust_locked_hover;
use super::quality::dumb_hover;
use super::{Action, VitascopeApp, mpv_opts_override};
use crate::picture::shader::{self, ShaderApply, ShaderInfo, ShaderProblem, Verdict};
use crate::picture::{ShaderPreset, new_preset_id};
use crate::player::AsyncKey;
use crate::{tf, tr};
use eframe::egui::{self, Color32};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// 有問題的檔案的顏色
const PROBLEM: Color32 = Color32::from_rgb(0xff, 0x8a, 0x80);

/// 設定頁的組合編輯（每一幀收集，畫完再做）
enum Edit {
    NewPreset,
    Delete(u32),
    /// 名稱改好了（離開輸入框）：存檔
    Renamed,
    AddFiles(u32),
    /// 組合、第幾個檔案、true = 往上
    Move(u32, usize, bool),
    Remove(u32, usize),
}

/// 「Anime4K A（6 個檔案）」
fn preset_summary(p: &ShaderPreset) -> String {
    let (name, n) = (p.label(), p.files.len());
    if n == 1 {
        tf!("{name}（1 個檔案）", "{name} (1 file)")
    } else {
        tf!("{name}（{n} 個檔案）", "{name} ({n} files)")
    }
}

impl VitascopeApp {
    /// 像素著色器停用的原因：軟體繪圖的簡化流程不跑著色器、VITASCOPE_MPV_OPTS 指定了 glsl-shaders；可以用時 None
    pub(super) fn shaders_disabled(&self) -> Option<&'static str> {
        if self.caps.dumb {
            Some(dumb_hover())
        } else if mpv_opts_override(&self.player, "glsl-shaders") {
            Some(adjust_locked_hover())
        } else {
            None
        }
    }

    /// 使用中的組合實際要送的檔案，與找不到而略過的檔名。軟體繪圖不跑著色器、
    /// VITASCOPE_MPV_OPTS 指定了 glsl-shaders 時不用組合：一個都不送（也不提示找不到）
    fn shader_chain(&self) -> (Vec<String>, Vec<String>) {
        let disabled = self.shaders_disabled().is_some();
        let Some(p) = self.settings.video.shaders.active_preset().filter(|_| !disabled) else {
            return (Vec::new(), Vec::new());
        };
        let (found, missing): (Vec<String>, Vec<String>) =
            p.files.iter().cloned().partition(|f| Path::new(f).is_file());
        (found, missing.iter().map(|f| shader::file_name(f)).collect())
    }

    /// 畫出影格的次數（看著色器能不能用）。只算 mpv 有影格可畫的：沒開檔、沒有影片時、
    /// 只是視窗大小變了的重畫不跑著色器，不算。沒有畫面時 None（自動測試可以用 `simulate_video_frames` 代替）
    fn render_count(&self) -> Option<u64> {
        let st = &self.player.state;
        if !(st.loaded && st.has_video()) {
            return None;
        }
        match &self.video {
            Some(v) => Some(v.stats().frames),
            None => self.simulated_frames,
        }
    }

    /// 測試用：當成畫面輸出記錄了這行錯誤（見 `Player::push_render_error`）
    #[doc(hidden)]
    pub fn push_render_error(&mut self, text: &str) {
        self.player.push_render_error(text);
    }

    /// 測試用：沒有畫面時，當成影片已經畫了 `frames` 格（換了著色器之後的檢查要等畫出影格才開始算）
    #[doc(hidden)]
    pub fn simulate_video_frames(&mut self, frames: u64) {
        self.simulated_frames = Some(frames);
    }

    /// 依設定套用使用中的組合（`sync`：啟動時同步設定）。找不到的檔案略過並提示，組合照舊。
    /// `watch`：換了組合時是 Some(換之前的組合)：看新的能不能用，畫不出來就還原成它。
    /// 上一個還沒看完（還不知道能不能用）就又換了的話，還原的對象照舊是上一次看的那個「之前的組合」
    /// （最後一個確定能用的），不會還原到沒確認過的組合；也不會還原成正在換的這個
    pub(super) fn apply_shaders(&mut self, sync: bool, watch: Option<Option<u32>>) {
        let (chain, missing) = self.shader_chain();
        if !missing.is_empty() {
            let names = missing.join(tr!("、", ", "));
            self.osd(tf!(
                "找不到著色器檔案，先略過：{names}",
                "Shader files not found, skipped: {names}"
            ));
        }
        match self.player.set_user_shaders(chain.clone(), sync) {
            Ok(Some(id)) => {
                self.async_pending.insert(id, "glsl-shaders".to_owned());
            }
            Ok(None) => {}
            Err(e) => self.async_failed(AsyncKey::Shaders, "glsl-shaders", &e.to_string()),
        }
        if let Some(mut previous) = watch {
            if let ShaderApply::Pending { previous: verified, .. } = &self.shader_watch {
                previous = *verified;
            }
            let previous = previous.filter(|p| Some(*p) != self.settings.video.shaders.active);
            self.shader_watch = if chain.is_empty() || self.shaders_disabled().is_some() {
                ShaderApply::Idle
            } else {
                ShaderApply::start(previous, chain, Instant::now(), self.render_count())
            };
        }
    }

    /// 選單、設定頁選了組合（None = 不使用）
    pub(super) fn set_shader_preset(&mut self, id: Option<u32>) {
        let shaders = &self.settings.video.shaders;
        let id = id.filter(|i| shaders.preset(*i).is_some());
        let previous = shaders.active;
        let text = match id.and_then(|i| shaders.preset(i)) {
            Some(p) => tf!("像素著色器：{}", "Pixel shaders: {}", preset_summary(p)),
            None => tr!("像素著色器：不使用", "Pixel shaders: none").to_owned(),
        };
        self.osd(text);
        if previous != id {
            self.settings.video.shaders.active = id;
            self.save_settings();
            self.apply_shaders(false, Some(previous));
        } else {
            // 同一個：檔案補回來的話這次就會用到
            self.apply_shaders(false, None);
        }
    }

    /// 每一幀：換了著色器之後看它能不能用。畫面輸出的錯誤記錄每一幀都取走（不在看的時候也是，免得留到下次）
    pub(super) fn shader_tick(&mut self) {
        let errors = self.player.take_render_errors();
        if !self.shader_watch.is_pending() {
            return;
        }
        let renders = self.render_count();
        if let Verdict::Revert { previous, file, reason } = self.shader_watch.tick(Instant::now(), renders, &errors) {
            self.revert_shaders(previous, &file, &reason);
        }
    }

    /// glsl-shaders 的非同步設定失敗：正在看新的組合的話當成畫不出來，還原
    pub(super) fn shader_reply_failed(&mut self, error: &str) {
        if let ShaderApply::Pending { previous, files, .. } = std::mem::take(&mut self.shader_watch) {
            let file = files.first().cloned().unwrap_or_default();
            self.revert_shaders(previous, &file, error);
        }
    }

    /// 著色器畫不出來：改回之前的組合、重新套用、存檔、提示
    fn revert_shaders(&mut self, previous: Option<u32>, file: &str, reason: &str) {
        self.shader_failures.insert(file.to_owned(), reason.to_owned());
        let shaders = &mut self.settings.video.shaders;
        shaders.active = previous.filter(|id| shaders.preset(*id).is_some());
        self.shader_watch = ShaderApply::Idle;
        self.apply_shaders(false, None);
        self.save_settings();
        let name = shader::file_name(file);
        let msg = tf!(
            "像素著色器無法使用，已還原：{name}（{reason}）",
            "Couldn't use the pixel shader, switched back: {name} ({reason})"
        );
        eprintln!("[vitascope] {msg}");
        self.osd(msg);
    }

    /// 換了像素著色器之後，還在看它能不能用（介面測試用）
    pub fn shader_watch_pending(&self) -> bool {
        self.shader_watch.is_pending()
    }

    // ───────────── 組合編輯 ─────────────

    /// 新增一個空的組合，回傳它的編號
    pub fn add_shader_preset(&mut self) -> u32 {
        let shaders = &mut self.settings.video.shaders;
        let id = new_preset_id(&shaders.presets);
        let n = shaders.presets.len() + 1;
        shaders.presets.push(ShaderPreset {
            id,
            name: tf!("組合 {n}", "Preset {n}"),
            files: Vec::new(),
        });
        self.shader_new = Some(id);
        self.save_settings();
        id
    }

    /// 把檔案加進組合（設定頁的「加入檔案…」；介面測試直接呼叫）。不能用的檔案不加，
    /// 回傳被拒絕的（路徑, 原因），最後一個也顯示在設定頁上
    pub fn add_shader_files(&mut self, preset: u32, paths: &[PathBuf]) -> Vec<(PathBuf, ShaderProblem)> {
        let mut rejected = Vec::new();
        let mut added = false;
        for path in paths {
            let checked = shader::check_path(path)
                .and_then(|s| shader::inspect_file(path, self.caps.macos).map(|info| (s, info)));
            match checked {
                Ok((s, info)) => {
                    if let Some(p) = self.settings.video.shaders.preset_mut(preset) {
                        p.files.push(s.clone());
                        added = true;
                    }
                    self.shader_failures.remove(&s);
                    self.shader_info.insert(s, Ok(info));
                }
                Err(problem) => rejected.push((path.clone(), problem)),
            }
        }
        self.shader_rejected = rejected.last().map(|(path, problem)| {
            (
                path.file_name()
                    .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
                problem.message(),
            )
        });
        if let Some((name, why)) = &self.shader_rejected {
            let msg = tf!("無法加入 {name}：{why}", "Can't add {name}: {why}");
            self.osd(msg);
        }
        if added {
            self.shader_preset_edited(preset);
        }
        rejected
    }

    /// 組合的內容改了：存檔；是使用中的組合就重新套用（畫不出來的話改成不使用）
    fn shader_preset_edited(&mut self, preset: u32) {
        self.save_settings();
        if self.settings.video.shaders.active == Some(preset) {
            self.apply_shaders(false, Some(None));
        }
    }

    fn shader_files_dialog(&mut self, preset: u32) {
        let mut dialog = self
            .file_dialog()
            .set_title(tr!("加入著色器檔案", "Add shader files"))
            .add_filter(tr!("mpv 著色器（.glsl）", "mpv shaders (.glsl)"), &["glsl", "hook"])
            .add_filter(tr!("所有檔案", "All files"), &["*"]);
        let last_dir = self
            .settings
            .video
            .shaders
            .presets
            .iter()
            .flat_map(|p| p.files.last())
            .last()
            .and_then(|f| Path::new(f).parent().map(Path::to_path_buf));
        if let Some(dir) = last_dir {
            dialog = dialog.set_directory(dir);
        }
        if let Some(paths) = dialog.pick_files() {
            self.add_shader_files(preset, &paths);
        }
    }

    fn apply_shader_edit(&mut self, edit: Edit) {
        let shaders = &mut self.settings.video.shaders;
        match edit {
            Edit::NewPreset => {
                self.add_shader_preset();
            }
            Edit::Delete(id) => {
                shaders.presets.retain(|p| p.id != id);
                if shaders.active == Some(id) {
                    shaders.active = None;
                    // 正在看的是刪掉的組合：不再看（之後才到的錯誤記錄不能把別的組合換回來）
                    self.shader_watch = ShaderApply::Idle;
                    self.apply_shaders(false, None);
                }
                self.save_settings();
            }
            Edit::Renamed => self.save_settings(),
            Edit::AddFiles(id) => self.shader_files_dialog(id),
            Edit::Move(id, i, up) => {
                if let Some(p) = shaders.preset_mut(id) {
                    let j = if up { i.checked_sub(1) } else { Some(i + 1) };
                    if let Some(j) = j.filter(|j| *j < p.files.len()) {
                        p.files.swap(i, j);
                        self.shader_preset_edited(id);
                    }
                }
            }
            Edit::Remove(id, i) => {
                if let Some(p) = shaders.preset_mut(id)
                    && i < p.files.len()
                {
                    p.files.remove(i);
                    self.shader_preset_edited(id);
                }
            }
        }
    }

    /// 「設定 → 畫質」的像素著色器：使用中的組合、各組合的檔案（↑ ↓ ✕、加入檔案…）、新增 / 刪除 / 改名
    pub(super) fn shader_section(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        ui.add_space(12.0);
        ui.strong(tr!("像素著色器", "Pixel shaders"));
        // 檔案的說明或問題：沒檢查過的先檢查（設定視窗關掉時清掉，下次打開重新檢查）
        let macos = self.caps.macos;
        for f in self.settings.video.shaders.presets.iter().flat_map(|p| &p.files) {
            if !self.shader_info.contains_key(f) {
                self.shader_info
                    .insert(f.clone(), shader::inspect_file(Path::new(f), macos));
            }
        }
        let disabled = self.shaders_disabled();
        let active = self.settings.video.shaders.active;
        let choices: Vec<(u32, String)> = self
            .settings
            .video
            .shaders
            .presets
            .iter()
            .map(|p| (p.id, p.label().to_owned()))
            .collect();
        let r = ui.add_enabled_ui(disabled.is_none(), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr!("使用：", "Use:"));
                if ui.radio(active.is_none(), tr!("不使用", "None")).clicked() && active.is_some() {
                    *action = Some(Action::SetShaderPreset(None));
                }
                for (id, label) in &choices {
                    if ui.radio(active == Some(*id), label.as_str()).clicked() && active != Some(*id) {
                        *action = Some(Action::SetShaderPreset(Some(*id)));
                    }
                }
            });
        });
        if let Some(why) = disabled {
            r.response.on_disabled_hover_text(why);
        }
        if self.caps.dumb {
            ui.weak(tr!(
                "軟體繪圖模式不支援像素著色器",
                "Pixel shaders aren't available with software rendering"
            ));
        } else if disabled.is_some() {
            ui.weak(tr!(
                "glsl-shaders 已由 VITASCOPE_MPV_OPTS 指定",
                "glsl-shaders is set by VITASCOPE_MPV_OPTS"
            ));
        }

        let mut edit = None;
        let just_created = self.shader_new.take();
        for i in 0..self.settings.video.shaders.presets.len() {
            let p = self.settings.video.shaders.presets[i].clone();
            let mut header = egui::CollapsingHeader::new(preset_summary(&p)).id_salt(("shader_preset", p.id));
            if just_created == Some(p.id) {
                header = header.open(Some(true));
            }
            header.show(ui, |ui| {
                ui.horizontal(|ui| {
                    let name = ui.label(tr!("名稱", "Name"));
                    let r = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.settings.video.shaders.presets[i].name)
                                .desired_width(160.0),
                        )
                        .labelled_by(name.id);
                    // 打字時標題跟著變，離開輸入框才存檔
                    if r.lost_focus() || (r.changed() && !r.has_focus()) {
                        edit = Some(Edit::Renamed);
                    }
                    if ui.button(tr!("刪除組合", "Delete preset")).clicked() {
                        edit = Some(Edit::Delete(p.id));
                    }
                });
                for (j, f) in p.files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}. {}", j + 1, shader::file_name(f)))
                            .on_hover_text(f);
                        match self.shader_info.get(f) {
                            Some(Ok(ShaderInfo { desc: Some(d), .. })) => {
                                ui.weak(d);
                            }
                            Some(Err(problem)) => {
                                ui.colored_label(PROBLEM, format!("⚠ {}", problem.message()));
                            }
                            _ => {}
                        }
                        if let Some(reason) = self.shader_failures.get(f) {
                            ui.colored_label(PROBLEM, tf!("⚠ 無法使用：{reason}", "⚠ Couldn't be used: {reason}"));
                        }
                        let last = j + 1 == p.files.len();
                        if ui
                            .add_enabled(j > 0, egui::Button::new("↑").small())
                            .on_hover_text(tr!("上移", "Move up"))
                            .clicked()
                        {
                            edit = Some(Edit::Move(p.id, j, true));
                        }
                        if ui
                            .add_enabled(!last, egui::Button::new("↓").small())
                            .on_hover_text(tr!("下移", "Move down"))
                            .clicked()
                        {
                            edit = Some(Edit::Move(p.id, j, false));
                        }
                        if ui
                            .add(egui::Button::new("✕").small())
                            .on_hover_text(tr!("從組合移除", "Remove from the preset"))
                            .clicked()
                        {
                            edit = Some(Edit::Remove(p.id, j));
                        }
                    });
                }
                if p.files.is_empty() {
                    ui.weak(tr!("還沒有檔案", "No files yet"));
                }
                if ui.button(tr!("加入檔案…", "Add files…")).clicked() {
                    edit = Some(Edit::AddFiles(p.id));
                }
            });
        }
        if let Some((name, why)) = &self.shader_rejected {
            ui.colored_label(PROBLEM, tf!("{name}：{why}", "{name}: {why}"));
        }
        if ui.button(tr!("新增組合", "New preset")).clicked() {
            edit = Some(Edit::NewPreset);
        }
        ui.weak(tr!(
            "著色器檔案由你自己提供（例如 Anime4K、FSRCNNX 的 .glsl）",
            "Bring your own shader files (for example the .glsl files of Anime4K or FSRCNNX)"
        ));
        if self.caps.macos {
            ui.weak(tr!(
                "macOS 不支援 COMPUTE 著色器",
                "macOS doesn't support COMPUTE shaders"
            ));
        }
        if let Some(e) = edit {
            self.apply_shader_edit(e);
        }
    }
}
