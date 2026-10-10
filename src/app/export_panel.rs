//! 匯出視窗（右鍵選單「匯出 ▸」、快捷鍵）：把 A-B 段落存成片段（不重新編碼）、轉成 GIF。
//!
//! - 不擋住操作的小視窗（跟控制面板一樣），影片照樣播；關掉視窗不會取消正在做的匯出，再打開看得到進度。
//! - 範圍就是 A-B 重播：欄位顯示 mpv 現在的 A、B，改欄位、按「目前位置」、按 L 都是改同一個 A-B
//!   （進度條上的標記、重播一起變，等於預覽）。
//! - 片段只放一條影像、一條聲音，不放字幕（目前的播放引擎寫不出字幕軌與語言標籤，視窗上註明）。
//! - GIF：長邊、格率、要不要燒進字幕（記住上次的選擇）；旋轉、翻轉跟畫面一樣，最長 30 秒。
//!   預設跟截圖放在一起（「設定 → 截圖與匯出」可以另外指定 GIF 資料夾）。
//! - 一次只做一個匯出。背景工作送回來的是列舉（進度、`Done`、`Failure`），文字在這裡用目前的語言組。
//! - 關閉影戲時丟掉 `Job`：取消、等一下、刪掉暫存檔（介面測試丟掉整個 App 也一樣）。
//!
//! 縮圖總覽圖的分頁之後加（`ExportTab`）。

use super::{Action, DialogKind, Pick, VitascopeApp, menu_item};
use crate::export::clip::{self, ClipSpec, Container, Picks, StreamPick};
use crate::export::gif::{self, GifSpec};
use crate::export::{
    self, ClipFormat, Done, Failure, GIF_FPS, GIF_LONG_SIDES, GIF_MAX_SECS, Job, JobEvent, Progress, format_time,
    parse_time,
};
use crate::instance::Wake;
use crate::keymap::Command;
use crate::player::{Track, TrackKind};
use crate::{tf, tr};
use eframe::egui::{self, Align2, Id, RichText};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 匯出視窗的分頁
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum ExportTab {
    /// 片段（不重新編碼）
    #[default]
    Clip,
    /// 轉成 GIF
    Gif,
}

impl ExportTab {
    const ALL: [ExportTab; 2] = [ExportTab::Clip, ExportTab::Gif];

    fn title(self) -> &'static str {
        match self {
            ExportTab::Clip => tr!("片段", "Clip"),
            ExportTab::Gif => "GIF",
        }
    }
}

/// 匯出的資料夾
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExportDir {
    /// 片段（預設是「影片」資料夾裡的 VitaScope）
    Clip,
    /// GIF（之後還有縮圖總覽圖；預設跟截圖放在一起）
    Image,
}

/// 匯出視窗與正在做的工作
#[derive(Default)]
pub(super) struct ExportUi {
    /// 視窗開著
    pub(super) open: bool,
    tab: ExportTab,
    /// 起點、終點欄位的文字（有焦點時是使用者正在打的，沒有焦點時每一幀照 A-B 重寫）
    range_text: [String; 2],
    /// 欄位裡打的看不懂（顯示說明，欄位回到原本的時間）；A-B 用別的方法改了就不再顯示
    range_bad: [bool; 2],
    /// 上一幀的 A-B（變了就清掉 `range_bad`）
    range_last: [Option<f64>; 2],
    /// 下面這些選擇是給哪個檔案的（`file_gen`）；換了檔案就重新用預設的
    picks_for: Option<u64>,
    /// 主播放器的 `file-format`（照來源選格式用；換檔時讀一次，不要每一幀問好幾次 mpv）
    file_format: String,
    /// 放進片段的影像、聲音（主播放器的軌道編號；None = 不放）。
    /// 使用者在視窗裡改之前跟著主播放器現在選的（`clip::default_picks`），改過就照使用者的，直到換檔案
    video: Option<i64>,
    audio: Option<i64>,
    /// 使用者自己勾過影像
    video_touched: bool,
    /// 使用者自己選過聲音（選「不包含」就不用說明「片段沒有聲音」）
    audio_touched: bool,
    /// 只有聲音時選的格式（自動、MKA）。設定裡的「片段的格式」是有影像的片段用的，只有聲音時不改它；
    /// 這個只記到關閉影戲
    audio_format: ClipFormat,
    /// 檔名（不含副檔名）；沒改過時照來源與範圍產生
    name: String,
    name_edited: bool,
    /// 正在做的匯出（同時只有一個）
    job: Option<Job>,
    progress: Option<Progress>,
    /// 上一次匯出的結果（下次開始時清掉）
    result: Option<Result<Done, Failure>>,
    /// 測試用：匯出用的 mpv 的觀察點（見 `clip::TestHooks`）
    test: clip::TestHooks,
    /// 測試用：GIF 的編碼用的 mpv 的觀察點（見 `gif::TestHooks`）
    gif_test: gif::TestHooks,
}

impl ExportUi {
    /// 關閉影戲：丟掉正在做的工作（`Job` 的 Drop：取消、最多等一下、刪掉暫存檔）
    pub(super) fn abandon(&mut self) {
        self.job = None;
    }
}

/// 範圍欄位的時間拉回影片裡（打 99:00:00 不會變成「範圍外」的失敗）
fn clamp_point(t: f64, duration: Option<f64>) -> f64 {
    let t = t.max(0.0);
    duration.filter(|d| d.is_finite() && *d > 0.0).map_or(t, |d| t.min(d))
}

/// 主播放器裡對應 `pick`（同一類的內嵌軌道裡排第幾）的軌道編號
fn track_id(tracks: &[Track], pick: Option<&StreamPick>) -> Option<i64> {
    let pick = pick?;
    tracks
        .iter()
        .filter(|t| t.kind == pick.kind && !t.external)
        .nth(pick.ordinal.checked_sub(1)?)
        .map(|t| t.id)
}

/// 放得進片段的影像：內嵌的、不是專輯封面
fn clip_video(t: &Track) -> bool {
    t.kind == TrackKind::Video && !t.external && !t.albumart
}

/// 片段大概多大（位元組；不知道時 None）：
/// - 有影像，或檔案本來就沒有影像：檔案大小 × 長度 ÷ 總長度
/// - 只放聲音、檔案裡有影像：音軌標示的位元率（bit/s）× 長度
fn estimate_size(
    length: f64,
    duration: Option<f64>,
    with_video: bool,
    file_has_video: bool,
    file_size: impl FnOnce() -> Option<i64>,
    audio_bitrate: impl FnOnce() -> Option<i64>,
) -> Option<i64> {
    if !(length.is_finite() && length > 0.0) {
        return None;
    }
    if with_video || !file_has_video {
        let duration = duration.filter(|d| d.is_finite() && *d > 0.0)?;
        let size = file_size().filter(|s| *s > 0)?;
        Some((size as f64 * (length / duration).clamp(0.0, 1.0)) as i64)
    } else {
        let bitrate = audio_bitrate().filter(|b| *b > 0)?;
        Some((bitrate as f64 / 8.0 * length) as i64)
    }
}

impl VitascopeApp {
    /// 打開匯出視窗（沒有開檔、也沒有做過的匯出時只提示）
    pub(super) fn show_export(&mut self, tab: ExportTab) {
        let ex = &self.export;
        if !self.player.state.loaded && ex.job.is_none() && ex.result.is_none() {
            self.osd(tr!("先開啟影片才能匯出", "Open a video first to export"));
            return;
        }
        self.export.open = true;
        self.export.tab = tab;
        self.export.range_bad = [false; 2];
    }

    /// 匯出的資料夾（設定的，沒設定時是預設的）
    fn export_dir(&self, which: ExportDir) -> PathBuf {
        match which {
            ExportDir::Clip => self.settings.export.clip_folder(),
            // 沒有另外指定時跟截圖放在一起（截圖資料夾改了也跟著）
            ExportDir::Image => self
                .settings
                .export
                .image_folder(self.settings.screenshot_dir.as_deref()),
        }
    }

    pub(super) fn open_export_dir(&mut self, which: ExportDir) {
        let dir = self.export_dir(which);
        if let Err(e) = crate::screenshot::open_folder(&dir) {
            self.osd(match which {
                ExportDir::Clip => tf!("無法開啟片段資料夾：{e}", "Cannot open the clips folder: {e}"),
                ExportDir::Image => tf!("無法開啟 GIF 資料夾：{e}", "Cannot open the GIF folder: {e}"),
            });
        }
    }

    pub(super) fn choose_export_dir(&mut self, which: ExportDir) {
        let (kind, title) = match which {
            ExportDir::Clip => (
                DialogKind::ExportClipDir,
                tr!("選擇片段資料夾", "Choose the clips folder"),
            ),
            ExportDir::Image => (
                DialogKind::ExportImageDir,
                tr!("選擇 GIF 資料夾", "Choose the GIF folder"),
            ),
        };
        let dialog = self
            .file_dialog()
            .set_title(title)
            .set_directory(self.export_dir(which));
        self.show_dialog(kind, Pick::Folder, dialog);
    }

    /// 資料夾對話框選好了：存起來
    pub(super) fn set_export_dir(&mut self, which: ExportDir, dir: PathBuf) {
        match which {
            ExportDir::Clip => {
                self.osd(tf!("片段資料夾：{}", "Clips folder: {}", dir.display()));
                self.settings.export.clip_dir = Some(dir);
            }
            ExportDir::Image => {
                self.osd(tf!("GIF 資料夾：{}", "GIF folder: {}", dir.display()));
                self.settings.export.image_dir = Some(dir);
            }
        }
        self.save_settings();
    }

    /// 右鍵選單「匯出 ▸」：沒開檔時整個停用；存不了片段、轉不了 GIF（直播、章節連結、沒有影像…）時
    /// 那一項停用，滑鼠停在上面說明原因
    pub(super) fn export_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        ui.add_enabled_ui(self.player.state.loaded, |ui| {
            ui.menu_button(tr!("匯出", "Export"), |ui| {
                let items = [
                    (
                        tr!("儲存片段…", "Save clip…"),
                        Command::ExportClip,
                        ExportTab::Clip,
                        clip::unavailable(&self.player, &self.caps),
                    ),
                    (
                        tr!("轉成 GIF…", "Make GIF…"),
                        Command::ExportGif,
                        ExportTab::Gif,
                        gif::unavailable(&self.player, &self.caps),
                    ),
                ];
                for (label, cmd, tab, why) in items {
                    let mut button = egui::Button::new(label);
                    let hint = self.keymap.hint(cmd);
                    if !hint.is_empty() {
                        button = button.shortcut_text(hint);
                    }
                    let mut r = ui.add_enabled(why.is_none(), button);
                    if let Some(f) = &why {
                        r = r.on_disabled_hover_text(f.message());
                    }
                    if r.clicked() {
                        action = Some(Action::ShowExport(tab));
                    }
                }
                ui.separator();
                if menu_item(ui, true, tr!("開啟片段資料夾", "Open the clips folder"), "") {
                    action = Some(Action::OpenExportDir(ExportDir::Clip));
                }
                ui.weak(self.export_dir(ExportDir::Clip).display().to_string());
                if menu_item(ui, true, tr!("開啟 GIF 資料夾", "Open the GIF folder"), "") {
                    action = Some(Action::OpenExportDir(ExportDir::Image));
                }
                ui.weak(self.export_dir(ExportDir::Image).display().to_string());
            });
        });
        action
    }

    /// 每一幀：換了檔案時，軌道、檔名改用新檔案的預設；收背景工作的進度、結果（結果顯示提示，視窗關著也一樣）
    pub(super) fn poll_export(&mut self) {
        self.sync_export_file();
        let Some(job) = &self.export.job else { return };
        let mut finished = None;
        while let Some(ev) = job.try_recv() {
            match ev {
                JobEvent::Progress(p) => self.export.progress = Some(p),
                JobEvent::Finished(r) => {
                    finished = Some(r);
                    break;
                }
            }
        }
        if let Some(result) = finished {
            // 背景執行緒已經送出結果、正在結束：丟掉時不用等
            self.export.job = None;
            self.export.progress = None;
            self.osd(match &result {
                Ok(done) => done.message(),
                Err(f) => f.osd(),
            });
            self.export.result = Some(result);
        }
    }

    /// 換了檔案：使用者的選擇（軌道、檔名）重來。
    /// 使用者沒在視窗裡改過的軌道跟著主播放器現在選的內嵌軌道（`clip::default_picks`）：
    /// 開視窗之前、開著的時候在「音軌」選單換了音軌，片段放的就是正在聽的那一條
    fn sync_export_file(&mut self) {
        let st = &self.player.state;
        if !st.loaded || st.tracks.is_empty() {
            return;
        }
        if self.export.picks_for != Some(self.file_gen) {
            let file_format = self.player.get_string("file-format").unwrap_or_default();
            let ex = &mut self.export;
            ex.file_format = file_format;
            ex.video_touched = false;
            ex.audio_touched = false;
            ex.name_edited = false;
            ex.range_bad = [false; 2];
            ex.picks_for = Some(self.file_gen);
        }
        let ex = &mut self.export;
        if ex.video_touched && ex.audio_touched {
            return;
        }
        let picks = clip::default_picks(st);
        if !ex.video_touched {
            ex.video = track_id(&st.tracks, picks.video.as_ref());
        }
        if !ex.audio_touched {
            ex.audio = track_id(&st.tracks, picks.audio.as_ref());
        }
    }

    /// 要放進片段的軌道（視窗裡選的）
    fn export_picks(&self) -> Picks {
        let st = &self.player.state;
        let pick = |id: Option<i64>, kind: TrackKind| {
            let track = st.tracks_of(kind).find(|t| Some(t.id) == id)?;
            StreamPick::of(&st.tracks, track)
        };
        Picks {
            video: pick(self.export.video, TrackKind::Video),
            audio: pick(self.export.audio, TrackKind::Audio),
        }
    }

    /// 這個格式放不放得下選的軌道（放得下時是實際的容器）
    fn clip_plan(&self, format: ClipFormat, picks: &Picks) -> Result<Container, Failure> {
        let st = &self.player.state;
        let codec = |p: &Option<StreamPick>| p.as_ref().and_then(|p| p.codec.clone());
        clip::plan_container(
            format,
            &self.export.file_format,
            st.path.as_deref().unwrap_or_default(),
            codec(&picks.video).as_deref(),
            codec(&picks.audio).as_deref(),
        )
    }

    /// 視窗裡可以選的格式（只有聲音時：自動、MKA）
    fn clip_choices(picks: &Picks) -> &'static [ClipFormat] {
        if picks.video.is_some() {
            &ClipFormat::ALL
        } else {
            &[ClipFormat::Auto, ClipFormat::Mkv]
        }
    }

    /// 選的格式：有影像時是設定裡的「片段的格式」，只有聲音時是這次在視窗裡選的
    fn chosen_clip_format(&self, picks: &Picks) -> ClipFormat {
        if picks.video.is_some() {
            self.settings.export.clip.format
        } else {
            self.export.audio_format
        }
    }

    /// 實際用的格式：選的格式放不下這些軌道（或只有聲音時不能選）就用自動
    fn effective_clip_format(&self, picks: &Picks) -> ClipFormat {
        let chosen = self.chosen_clip_format(picks);
        if Self::clip_choices(picks).contains(&chosen) && self.clip_plan(chosen, picks).is_ok() {
            chosen
        } else {
            ClipFormat::Auto
        }
    }

    /// 片段產生的檔名（不含副檔名）：「第1集 00.12.03-00.12.45」；網路串流有標題時用標題
    fn export_stem(&self, a: f64, b: f64) -> String {
        let st = &self.player.state;
        let path = st.path.clone().unwrap_or_default();
        let title = self.titles.get(&path).map(String::as_str).or(st.title.as_deref());
        // 命令列給的 file:// 網址：用檔名
        let source = match path.get(..7) {
            Some(head) if head.eq_ignore_ascii_case("file://") => {
                crate::m3u::file_url_to_path(&path[7..]).to_string_lossy().into_owned()
            }
            _ => path.clone(),
        };
        export::range_stem(&source, title, a, b)
    }

    /// 範圍的一個點（0 = 起點、1 = 終點）改成 `t`（None = 清掉）：拉回影片裡，起點比終點晚時對調。
    /// 已經播過終點的話跳回起點（mpv 只在設定的那一刻看「目前位置 <= 終點」，過了就不會繞回起點；跟 A-B 重播的按鍵一樣）
    fn set_export_point(&mut self, which: usize, t: Option<f64>) {
        let duration = self.player.state.duration;
        let mut points = self.player.ab_loop_points();
        self.export.range_bad[which] = false;
        points[which] = t.map(|t| clamp_point(t, duration));
        if let [Some(a), Some(b)] = points
            && a > b
        {
            points = [Some(b), Some(a)];
        }
        let _ = self.player.set_ab_loop(points[0], points[1]);
        if let [Some(a), Some(b)] = points
            && self.player.get_f64("time-pos").is_ok_and(|now| now > b)
        {
            let _ = self.player.seek_to(a, true);
        }
    }

    pub(super) fn export_window(&mut self, ctx: &egui::Context) {
        if !self.export.open {
            return;
        }
        let mut open = true;
        let screen = ctx.content_rect();
        // 視窗比畫面高時（小影片的視窗）內容可以捲動
        let max_height = (screen.height() - 90.0).max(160.0);
        egui::Window::new(tr!("匯出", "Export"))
            .id(Id::new("export_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(screen.center())
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.horizontal(|ui| {
                    for tab in ExportTab::ALL {
                        if ui.selectable_label(self.export.tab == tab, tab.title()).clicked() {
                            self.export.tab = tab;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(max_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        match self.export.tab {
                            ExportTab::Clip => self.clip_tab(ui),
                            ExportTab::Gif => self.gif_tab(ui),
                        }
                        self.export_status(ui);
                    });
            });
        if !open {
            self.export.open = false;
        }
    }

    /// 「片段」分頁：範圍、軌道、格式、存到哪裡、檔名、開始
    fn clip_tab(&mut self, ui: &mut egui::Ui) {
        if !self.player.state.loaded {
            ui.weak(tr!("沒有開啟的影片", "No video is open"));
            return;
        }
        let points = self.player.ab_loop_points();
        self.range_rows(ui, points);
        ui.add_space(8.0);
        self.track_rows(ui);
        let picks = self.export_picks();
        ui.add_space(8.0);
        let format = self.format_row(ui, &picks);
        let container = self.clip_plan(format, &picks);
        self.clip_notes(ui, &picks, container.as_ref().ok().copied());
        ui.add_space(8.0);
        let ext = container.as_ref().ok().map(|c| c.ext());
        self.output_rows(ui, points, ExportDir::Clip, ext);
        if let [Some(a), Some(b)] = points
            && let Some(bytes) = self.estimated_bytes(b - a, &picks)
        {
            let size = crate::mediainfo::fmt_size(bytes);
            ui.weak(tf!("預估大小：約 {size}", "Estimated size: about {size}"));
        }
        ui.add_space(8.0);
        let why = self.clip_start_blocked(points, &picks, &container);
        let r = ui
            .add_enabled(why.is_none(), egui::Button::new(tr!("開始匯出", "Start export")))
            .on_disabled_hover_text(why.unwrap_or_default());
        if r.clicked() {
            self.start_clip_export();
        }
    }

    /// 起點、終點（文字欄位 + 「目前位置」）與長度
    fn range_rows(&mut self, ui: &mut egui::Ui, points: [Option<f64>; 2]) {
        let mut commit = None;
        let mut now = None;
        ui.strong(tr!("範圍", "Range"));
        egui::Grid::new("export_range")
            .num_columns(3)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                for (i, name) in [tr!("起點", "Start"), tr!("終點", "End")].into_iter().enumerate() {
                    let label = ui.label(name);
                    let id = Id::new(("export_range", i));
                    let shown = points[i].map(format_time).unwrap_or_default();
                    // 沒在打字時顯示 mpv 現在的 A-B（按 L、進度條、別的地方改了也跟著變）
                    if !ui.memory(|m| m.has_focus(id)) {
                        self.export.range_text[i] = shown.clone();
                    }
                    let r = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.export.range_text[i])
                                .id(id)
                                .desired_width(120.0),
                        )
                        .labelled_by(label.id);
                    if r.changed() {
                        self.export.range_bad[i] = false;
                    }
                    // 按 Enter、點別的地方：寫回 A-B（沒改的不寫：顯示的時間只到毫秒，寫回去會改掉原本的值）
                    if r.lost_focus() && self.export.range_text[i].trim() != shown {
                        commit = Some((i, self.export.range_text[i].clone()));
                    }
                    if ui.button(tr!("目前位置", "Current position")).clicked() {
                        now = Some(i);
                    }
                    ui.end_row();
                }
            });
        // A-B 用別的方法改了（「目前位置」、L、進度條、另一個欄位）：之前打錯的說明不再顯示
        if points != self.export.range_last {
            self.export.range_last = points;
            self.export.range_bad = [false; 2];
        }
        if let Some((i, text)) = commit {
            let text = text.trim();
            if text.is_empty() {
                self.set_export_point(i, None);
            } else if let Some(t) = parse_time(text) {
                self.set_export_point(i, Some(t));
            } else {
                self.export.range_bad[i] = true;
            }
        }
        if let Some(i) = now {
            let t = self.player.get_f64("time-pos").unwrap_or(self.player.state.time_pos);
            self.set_export_point(i, Some(t));
        }
        if self.export.range_bad.iter().any(|b| *b) {
            ui.colored_label(
                ui.visuals().error_fg_color,
                tr!(
                    "看不懂的時間（例如 83.5、1:23.5、01:02:03.250）",
                    "Unrecognised time (for example 83.5, 1:23.5, 01:02:03.250)"
                ),
            );
        }
        let key = self.keymap.hint(Command::AbLoop);
        let also = if key.is_empty() {
            String::new()
        } else {
            tf!(
                " · 也可以按 {key} 設定起點、終點",
                " · You can also press {key} to set the start and end"
            )
        };
        match points {
            [Some(a), Some(b)] => ui.weak(tf!("長度 {:.1} 秒{also}", "Length {:.1} s{also}", b - a)),
            _ => ui.weak(tf!("先設定起點和終點{also}", "Set the start and the end first{also}")),
        };
    }

    /// 軌道：影像（勾選）、聲音（下拉選單：內嵌的音軌或「不包含」）
    fn track_rows(&mut self, ui: &mut egui::Ui) {
        let st = &self.player.state;
        // 勾選時放的那一條：視窗裡的（跟著主播放器）；沒勾時是主播放器正在顯示的、或第一條
        let video = self
            .export
            .video
            .and_then(|id| st.tracks_of(TrackKind::Video).find(|t| t.id == id))
            .or_else(|| st.selected(TrackKind::Video))
            .filter(|t| clip_video(t))
            .or_else(|| st.tracks.iter().find(|t| clip_video(t)))
            .cloned();
        let audios: Vec<Track> = st
            .tracks_of(TrackKind::Audio)
            .filter(|t| !t.external)
            .cloned()
            .collect();
        ui.strong(tr!("軌道", "Tracks"));
        if let Some(v) = video {
            let mut detail = v.codec.clone().unwrap_or_default();
            if let (Some(w), Some(h)) = (v.width, v.height) {
                detail += &format!(" {w}×{h}");
            }
            let detail = detail.trim().to_owned();
            let mut on = self.export.video.is_some();
            if ui
                .checkbox(&mut on, tf!("影像（{detail}）", "Video ({detail})"))
                .changed()
            {
                self.export.video = on.then_some(v.id);
                self.export.video_touched = true;
            }
        }
        let none = tr!("不包含", "None");
        let current = self
            .export
            .audio
            .and_then(|id| audios.iter().find(|t| t.id == id))
            .map_or_else(|| none.to_owned(), Track::label);
        let mut chosen = None;
        ui.horizontal(|ui| {
            let label = ui.label(tr!("聲音", "Audio track"));
            egui::ComboBox::from_id_salt("export_audio")
                .selected_text(current)
                .width(320.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(self.export.audio.is_none(), none).clicked() {
                        chosen = Some(None);
                    }
                    for t in &audios {
                        let refused = t.codec.as_deref().is_some_and(clip::refused_audio);
                        let r = ui
                            .add_enabled(
                                !refused,
                                egui::Button::selectable(self.export.audio == Some(t.id), t.label()),
                            )
                            .on_disabled_hover_text(Failure::AudioCodec.message());
                        if r.clicked() {
                            chosen = Some(Some(t.id));
                        }
                    }
                })
                .response
                .labelled_by(label.id);
        });
        if let Some(id) = chosen {
            self.export.audio = id;
            self.export.audio_touched = true;
        }
    }

    /// 格式：放不下選的軌道的停用，滑鼠停在上面說明原因；有影像時選了存進設定。回傳實際用的格式
    fn format_row(&mut self, ui: &mut egui::Ui, picks: &Picks) -> ClipFormat {
        let effective = self.effective_clip_format(picks);
        let audio_only = picks.video.is_none();
        let name = |me: &Self, f: ClipFormat| -> String {
            match f {
                ClipFormat::Auto => match me.clip_plan(f, picks) {
                    Ok(c) => tf!("自動（{}）", "Automatic ({})", c.label()),
                    Err(_) => tr!("自動", "Automatic").to_owned(),
                },
                ClipFormat::Mkv if audio_only => "MKA".to_owned(),
                other => other.label().to_owned(),
            }
        };
        let mut chosen = None;
        ui.horizontal(|ui| {
            let label = ui.label(tr!("格式", "Format"));
            egui::ComboBox::from_id_salt("export_format")
                .selected_text(name(self, effective))
                .show_ui(ui, |ui| {
                    for &f in Self::clip_choices(picks) {
                        let plan = self.clip_plan(f, picks);
                        let r = ui
                            .add_enabled(plan.is_ok(), egui::Button::selectable(effective == f, name(self, f)))
                            .on_disabled_hover_text(plan.err().map(|e| e.message()).unwrap_or_default());
                        if r.clicked() {
                            chosen = Some(f);
                        }
                    }
                })
                .response
                .labelled_by(label.id);
        });
        match chosen {
            // 只有聲音（自動、MKA）：只記這次，不改有影像的片段用的設定
            Some(f) if audio_only => {
                self.export.audio_format = f;
                f
            }
            // 有影像：存進設定（「設定 → 截圖與匯出」的「片段的格式」）
            Some(f) => {
                if f != self.settings.export.clip.format {
                    self.settings.export.clip.format = f;
                    self.save_settings();
                }
                f
            }
            None => effective,
        }
    }

    /// 片段的說明：不重新編碼、只有影像與一條聲音（不含字幕）、網路影片會再下載一次
    fn clip_notes(&self, ui: &mut egui::Ui, picks: &Picks, container: Option<Container>) {
        if picks.video.is_some() {
            ui.weak(tr!(
                "不重新編碼：起點會提早到前一個關鍵影格、終點延到下一個關鍵影格之前，片段可能比選的長幾秒。\
                 畫面調整（旋轉、裁切…）與影像調整不會套用。",
                "No re-encoding: the clip starts at the keyframe before the start and ends just before the next \
                 keyframe after the end, so it can be a few seconds longer. Picture changes (rotation, crop…) and \
                 image adjustments are not applied."
            ));
        }
        ui.weak(tr!(
            "片段只包含影像與一條音軌，不含字幕，也不保留音軌的語言標籤；外掛的音軌、字幕不會放進去。",
            "Clips contain the video and one audio track only: no subtitles, and the audio language tag isn't kept. \
             External audio and subtitle files are not included."
        ));
        let hevc = picks
            .video
            .as_ref()
            .and_then(|v| v.codec.as_deref())
            .is_some_and(|c| c == "hevc");
        if container == Some(Container::Mp4) && hevc {
            ui.weak(tr!(
                "HEVC 的 MP4 在 Apple 的播放器可能打不開，建議用 MKV。",
                "Apple's players may not open HEVC in MP4; MKV is safer."
            ));
        }
        let path = self.player.state.path.as_deref().unwrap_or_default();
        if crate::net::is_network(path) {
            ui.weak(tr!(
                "網路影片：會另外再下載這一段，播放不受影響。",
                "Online video: this part is downloaded again; playback isn't affected."
            ));
        }
    }

    /// 存到哪裡、檔名（片段、GIF 共用一個檔名：都是照來源與範圍產生）；`ext` = 副檔名（不知道時不顯示）
    fn output_rows(&mut self, ui: &mut egui::Ui, points: [Option<f64>; 2], which: ExportDir, ext: Option<&str>) {
        let dir = self.export_dir(which);
        let mut choose = false;
        egui::Grid::new("export_output")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr!("存到", "Save to"));
                ui.horizontal_wrapped(|ui| {
                    ui.add(egui::Label::new(RichText::new(dir.display().to_string()).monospace()).wrap());
                    choose = ui.button(tr!("變更資料夾…", "Change folder…")).clicked();
                });
                ui.end_row();
                let label = ui.label(tr!("檔名", "File name"));
                ui.horizontal(|ui| {
                    let generated = match points {
                        [Some(a), Some(b)] => self.export_stem(a, b),
                        _ => String::new(),
                    };
                    let id = Id::new("export_name");
                    // 沒改過檔名：照來源與範圍產生（範圍改了跟著變）
                    if !self.export.name_edited && !ui.memory(|m| m.has_focus(id)) {
                        self.export.name = generated;
                    }
                    let r = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.export.name)
                                .id(id)
                                .desired_width(300.0),
                        )
                        .labelled_by(label.id);
                    if r.changed() {
                        // 清空 = 回到自動產生的名稱
                        self.export.name_edited = !self.export.name.trim().is_empty();
                    }
                    if let Some(ext) = ext {
                        ui.label(format!(".{ext}"));
                    }
                });
                ui.end_row();
            });
        if choose {
            self.choose_export_dir(which);
        }
    }

    /// 片段大概多大（不知道時 None：不顯示）
    fn estimated_bytes(&self, length: f64, picks: &Picks) -> Option<i64> {
        let st = &self.player.state;
        let file_size = || self.player.get_i64("file-size").ok();
        // 只放聲音、檔案裡卻有影像：用那條音軌標示的位元率（檔案大小大多是影像的）
        let audio_bitrate = || {
            let id = self.export.audio?;
            // track-list 的順序就是 `state.tracks` 的順序；核對編號，對不上時不估
            let index = st
                .tracks
                .iter()
                .position(|t| t.kind == TrackKind::Audio && t.id == id)?;
            let base = format!("track-list/{index}");
            let same = self.player.get_i64(&format!("{base}/id")).ok() == Some(id)
                && self.player.get_string(&format!("{base}/type")).ok().as_deref() == Some("audio");
            same.then(|| self.player.get_i64(&format!("{base}/demux-bitrate")).ok())
                .flatten()
        };
        let has_video = st.tracks.iter().any(clip_video);
        estimate_size(
            length,
            st.duration,
            picks.video.is_some(),
            has_video,
            file_size,
            audio_bitrate,
        )
    }

    /// 現在不能開始的原因（None = 可以開始）
    fn clip_start_blocked(
        &self,
        points: [Option<f64>; 2],
        picks: &Picks,
        container: &Result<Container, Failure>,
    ) -> Option<String> {
        if self.export.job.is_some() {
            return Some(
                tr!(
                    "正在匯出，請等這一個做完",
                    "An export is running; wait for it to finish"
                )
                .into(),
            );
        }
        if let Some(f) = clip::unavailable(&self.player, &self.caps) {
            return Some(f.message());
        }
        let [Some(a), Some(b)] = points else {
            return Some(tr!("先設定起點和終點", "Set the start and the end first").into());
        };
        if b <= a {
            return Some(tr!("終點要在起點之後", "The end must come after the start").into());
        }
        if picks.video.is_none() && picks.audio.is_none() {
            return Some(tr!("至少要包含影像或聲音", "Include the video or an audio track").into());
        }
        container.as_ref().err().map(Failure::message)
    }

    /// 「開始匯出」：從主播放器準備好片段（範圍、軌道、格式、資料夾、檔名），在背景開始
    fn start_clip_export(&mut self) {
        let [Some(a), Some(b)] = self.player.ab_loop_points() else {
            return;
        };
        if self.export.job.is_some() {
            return;
        }
        let picks = self.export_picks();
        let format = self.effective_clip_format(&picks);
        let dir = self.export_dir(ExportDir::Clip);
        // 磁碟快取：快取資料夾的 export；沒有快取資料夾（自動測試、`--shot`）時用系統的暫存資料夾
        let cache = self
            .export_cache_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("vitascope-export"));
        let main_audio = self.player.state.selected(TrackKind::Audio).is_some();
        match ClipSpec::from_player_with(&self.player, &self.caps, a, b, format, picks.clone(), dir, cache) {
            Ok(mut spec) => {
                let generated = self.export_stem(a, b);
                let edited = if self.export.name_edited {
                    self.export.name.as_str()
                } else {
                    ""
                };
                spec.stem = export::chosen_stem(edited, &generated);
                // 預設沒有放得進片段的聲音（正在播的是外掛的音軌、APE 之類），使用者也沒自己選：完成時說明片段沒有聲音
                spec.drops_audio = picks.audio.is_none() && main_audio && !self.export.audio_touched;
                spec.test = self.export.test.clone();
                let ctx = self.egui_ctx.clone();
                let wake: Wake = Arc::new(move || ctx.request_repaint());
                self.export.job = Some(clip::spawn(spec, wake));
                self.export.progress = None;
                self.export.result = None;
            }
            Err(f) => {
                self.osd(f.osd());
                self.export.result = Some(Err(f));
            }
        }
    }

    /// 「GIF」分頁：範圍、大小、格率、字幕、說明、存到哪裡、檔名、開始
    fn gif_tab(&mut self, ui: &mut egui::Ui) {
        if !self.player.state.loaded {
            ui.weak(tr!("沒有開啟的影片", "No video is open"));
            return;
        }
        let points = self.player.ab_loop_points();
        self.range_rows(ui, points);
        ui.add_space(8.0);
        self.gif_rows(ui);
        ui.add_space(8.0);
        self.output_rows(ui, points, ExportDir::Image, Some("gif"));
        ui.add_space(8.0);
        let why = self.gif_start_blocked(points);
        let r = ui
            .add_enabled(why.is_none(), egui::Button::new(tr!("開始匯出", "Start export")))
            .on_disabled_hover_text(why.unwrap_or_default());
        if r.clicked() {
            self.start_gif_export();
        }
    }

    /// GIF 的選擇：長邊（與算出來的大小）、格率、字幕；HDR 與轉正的說明。改了就存進設定（記住上次的選擇）
    fn gif_rows(&mut self, ui: &mut egui::Ui) {
        let mut prefs = self.settings.export.gif;
        let sizes = gif::sizes_for(&self.player, &self.geometry, prefs.long_side);
        egui::Grid::new("export_gif")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                let label = ui.label(tr!("大小（長邊）", "Size (long side)"));
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("export_gif_size")
                        .selected_text(format!("{} px", prefs.long_side))
                        .show_ui(ui, |ui| {
                            for side in GIF_LONG_SIDES {
                                ui.selectable_value(&mut prefs.long_side, side, format!("{side} px"));
                            }
                        })
                        .response
                        .labelled_by(label.id);
                    if let Some(s) = sizes {
                        ui.label(format!("→ {}×{}", s.out.0, s.out.1));
                    }
                });
                ui.end_row();
                let label = ui.label(tr!("格率", "Frame rate"));
                egui::ComboBox::from_id_salt("export_gif_fps")
                    .selected_text(format!("{} fps", prefs.fps))
                    .show_ui(ui, |ui| {
                        for fps in GIF_FPS {
                            ui.selectable_value(&mut prefs.fps, fps, format!("{fps} fps"));
                        }
                    })
                    .response
                    .labelled_by(label.id);
                ui.end_row();
            });
        // 字幕：主播放器顯示著字幕時才能勾（勾選的狀態照樣記著）
        let st = &self.player.state;
        let shown = st.selected(TrackKind::Sub).is_some();
        let mut on = prefs.subtitles && shown;
        let r = ui
            .add_enabled(
                shown,
                egui::Checkbox::new(
                    &mut on,
                    tr!(
                        "包含字幕（目前顯示的字幕，含外觀與延遲）",
                        "Include subtitles (as shown, with style and delay)"
                    ),
                ),
            )
            .on_disabled_hover_text(tr!("目前沒有顯示字幕", "No subtitles are shown"));
        if r.changed() {
            prefs.subtitles = on;
        }
        // HDR：會轉成一般畫面（目標亮度、曲線照「畫質 → HDR」；FFmpeg 沒有的曲線改用 Hable），或說明為什麼不轉
        let dv = st.selected(TrackKind::Video).and_then(|t| t.dolby_vision_profile);
        let user = self.settings.video.tone;
        let (tone, note) = gif::hdr_plan(
            gif::source_hdr(&self.player),
            dv,
            self.caps.zscale && self.caps.tonemap,
            &user,
        );
        if let Some(t) = tone {
            let npl = t.npl;
            if t.curve == user.curve.mpv() {
                let curve = user.curve.label();
                ui.weak(tf!(
                    "HDR 影片會轉成一般畫面（{curve}，目標亮度 {npl} nits，跟「畫質 → HDR」一樣）",
                    "HDR video is converted to SDR ({curve}, target {npl} nits, as in Video quality → HDR)"
                ));
            } else {
                // 自動、BT.2390：FFmpeg 的 tonemap 沒有，GIF 用 Hable（不說「一樣」）
                let curve = user.curve.label();
                ui.weak(tf!(
                    "HDR 影片會轉成一般畫面（Hable，目標亮度 {npl} nits）；「畫質 → HDR」的曲線「{curve}」GIF 做不到，改用 Hable",
                    "HDR video is converted to SDR (Hable, target {npl} nits); GIFs can't use the \"{curve}\" curve from Video quality → HDR, so they use Hable"
                ));
            }
        } else if let Some(n) = note {
            ui.weak(n.message());
        }
        let max = GIF_MAX_SECS;
        ui.weak(tf!(
            "旋轉、翻轉跟畫面一樣；影像調整、像素著色器、縮放、裁切不會套用。GIF 最長 {max:.0} 秒。",
            "Rotation and flips follow the picture; image adjustments, shaders, zoom and crop are not applied. \
             GIFs can be at most {max:.0} seconds."
        ));
        if prefs != self.settings.export.gif {
            self.settings.export.gif = prefs;
            self.save_settings();
        }
    }

    /// GIF 現在不能開始的原因（None = 可以開始）
    fn gif_start_blocked(&self, points: [Option<f64>; 2]) -> Option<String> {
        if self.export.job.is_some() {
            return Some(
                tr!(
                    "正在匯出，請等這一個做完",
                    "An export is running; wait for it to finish"
                )
                .into(),
            );
        }
        if let Some(f) = gif::unavailable(&self.player, &self.caps) {
            return Some(f.message());
        }
        let [Some(a), Some(b)] = points else {
            return Some(tr!("先設定起點和終點", "Set the start and the end first").into());
        };
        if b <= a {
            return Some(tr!("終點要在起點之後", "The end must come after the start").into());
        }
        if let Err(f) = gif::check_length(b - a) {
            return Some(f.message());
        }
        if gif::sizes_for(&self.player, &self.geometry, self.settings.export.gif.long_side).is_none() {
            return Some(
                tr!(
                    "還不知道畫面的大小，請稍候",
                    "The picture size isn't known yet; wait a moment"
                )
                .into(),
            );
        }
        None
    }

    /// GIF 的「開始匯出」：從主播放器準備好（範圍、大小、格率、字幕、轉正、HDR、資料夾、檔名），在背景開始
    fn start_gif_export(&mut self) {
        let [Some(a), Some(b)] = self.player.ab_loop_points() else {
            return;
        };
        if self.export.job.is_some() {
            return;
        }
        let dir = self.export_dir(ExportDir::Image);
        // 換不成正式名稱留下的檔案登記在快取資料夾；沒有快取資料夾（自動測試、`--shot`）時用系統的暫存資料夾
        let cache = self
            .export_cache_dir()
            .unwrap_or_else(|| std::env::temp_dir().join("vitascope-export"));
        let choice = gif::Choice {
            prefs: self.settings.export.gif,
            geometry: &self.geometry,
            tone: &self.settings.video.tone,
            style: &self.settings.subtitle,
            deinterlace: self.settings.video.deinterlace,
        };
        match GifSpec::from_player(&self.player, &self.caps, a, b, &choice, dir, cache) {
            Ok(mut spec) => {
                let generated = self.export_stem(a, b);
                let edited = if self.export.name_edited {
                    self.export.name.as_str()
                } else {
                    ""
                };
                spec.stem = export::chosen_stem(edited, &generated);
                spec.test = self.export.gif_test.clone();
                let ctx = self.egui_ctx.clone();
                let wake: Wake = Arc::new(move || ctx.request_repaint());
                self.export.job = Some(gif::spawn(spec, wake));
                self.export.progress = None;
                self.export.result = None;
            }
            Err(f) => {
                self.osd(f.osd());
                self.export.result = Some(Err(f));
            }
        }
    }

    /// 視窗下方：進度與取消（寫檔中不能取消）、完成的檔案與實際範圍、失敗的原因
    fn export_status(&mut self, ui: &mut egui::Ui) {
        let mut open_file = None;
        let mut reveal = None;
        if let Some(job) = &self.export.job {
            ui.separator();
            let (label, fraction) = match self.export.progress {
                Some(p) => (p.phase.label(), p.fraction),
                None => (tr!("準備中…", "Preparing…").to_owned(), None),
            };
            let text = match fraction {
                Some(f) => format!("{label} {:.0}%", f * 100.0),
                None => label,
            };
            ui.add(
                egui::ProgressBar::new(fraction.unwrap_or(0.0))
                    .text(text)
                    .animate(fraction.is_none()),
            );
            let r = ui
                .add_enabled(job.cancellable(), egui::Button::new(tr!("取消匯出", "Cancel export")))
                .on_disabled_hover_text(tr!("寫入中無法中斷", "Writing can't be interrupted"));
            if r.clicked() {
                job.cancel();
            }
        } else if let Some(result) = &self.export.result {
            ui.separator();
            match result {
                Ok(done) => {
                    let name = super::file_name(&done.path);
                    let size = crate::mediainfo::fmt_size(i64::try_from(done.bytes).unwrap_or(i64::MAX));
                    ui.label(tf!("完成：{name}（{size}）", "Done: {name} ({size})"));
                    if let Some((a, b)) = done.actual {
                        let (a, b) = (format_time(a), format_time(b));
                        ui.label(tf!(
                            "實際範圍：{a} – {b}（依關鍵影格）",
                            "Actual range: {a} – {b} (keyframes)"
                        ));
                    }
                    for note in &done.notes {
                        ui.weak(note.message());
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tr!("開啟檔案", "Open file")).clicked() {
                            open_file = Some(done.path.clone());
                        }
                        if ui.button(tr!("在資料夾中顯示", "Show in folder")).clicked() {
                            reveal = Some(done.path.clone());
                        }
                    });
                }
                Err(Failure::Cancelled) => {
                    ui.weak(tr!("已取消匯出", "Export cancelled"));
                }
                Err(f) => {
                    // 完整的原因（路徑、系統的原文）；提示只說個大概
                    ui.colored_label(ui.visuals().error_fg_color, f.details());
                }
            }
        }
        if let Some(path) = open_file {
            self.open_exported(&path, false);
        }
        if let Some(path) = reveal {
            self.open_exported(&path, true);
        }
    }

    /// 「開啟檔案」「在資料夾中顯示」
    fn open_exported(&mut self, path: &Path, reveal: bool) {
        let result = if reveal {
            crate::screenshot::reveal(path)
        } else {
            crate::screenshot::open_path(path)
        };
        if let Err(e) = result {
            self.osd(tf!("無法開啟：{e}", "Cannot open: {e}"));
        }
    }

    /// 匯出的設定（「設定 → 截圖與匯出」）：片段的資料夾、格式，GIF 的資料夾。回傳設定有沒有改（呼叫的地方存檔）
    pub(super) fn export_settings_section(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) -> bool {
        let mut changed = false;
        ui.strong(tr!("匯出", "Export"));
        ui.label(tr!("片段資料夾", "Clips folder"));
        ui.add(
            egui::Label::new(RichText::new(self.export_dir(ExportDir::Clip).display().to_string()).monospace()).wrap(),
        );
        // 按鈕的名稱跟上面截圖的不一樣（同一頁有兩組「變更…」，螢幕閱讀器分不出來）
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr!("變更片段資料夾…", "Change the clips folder…")).clicked() {
                *action = Some(Action::ChooseExportDir(ExportDir::Clip));
            }
            if ui.button(tr!("開啟片段資料夾", "Open the clips folder")).clicked() {
                *action = Some(Action::OpenExportDir(ExportDir::Clip));
            }
            if ui
                .add_enabled(
                    self.settings.export.clip_dir.is_some(),
                    egui::Button::new(tr!("用預設的片段資料夾", "Use the default clips folder")),
                )
                .clicked()
            {
                self.settings.export.clip_dir = None;
                changed = true;
            }
        });
        ui.weak(tr!(
            "預設：「影片」資料夾裡的 VitaScope。Windows 的「影片」資料夾常由 OneDrive 同步，大的片段會被上傳。",
            "Default: VitaScope in your Videos folder. On Windows, OneDrive often syncs the Videos folder, \
             so large clips get uploaded."
        ));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let label = ui.label(tr!("片段的格式", "Clip format"));
            let current = self.settings.export.clip.format;
            egui::ComboBox::from_id_salt("settings_clip_format")
                .selected_text(current.label())
                .show_ui(ui, |ui| {
                    for f in ClipFormat::ALL {
                        if ui.selectable_label(current == f, f.label()).clicked() && f != current {
                            self.settings.export.clip.format = f;
                            changed = true;
                        }
                    }
                })
                .response
                .labelled_by(label.id);
        });
        ui.weak(tr!(
            "片段不重新編碼，只能從關鍵影格切開；只包含影像與一條音軌，不含字幕。放不下選的軌道的格式改用自動。\
             在右鍵選單「匯出 ▸ 儲存片段…」用 A-B 重播的範圍匯出。",
            "Clips aren't re-encoded, so they can only be cut at keyframes; they contain the video and one audio \
             track, without subtitles. A format that can't hold the chosen tracks falls back to Automatic. \
             Export the A-B loop range from the right-click menu: Export ▸ Save clip…."
        ));
        ui.add_space(6.0);
        ui.label(tr!("GIF 資料夾", "GIF folder"));
        ui.add(
            egui::Label::new(RichText::new(self.export_dir(ExportDir::Image).display().to_string()).monospace()).wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr!("變更 GIF 資料夾…", "Change the GIF folder…")).clicked() {
                *action = Some(Action::ChooseExportDir(ExportDir::Image));
            }
            if ui.button(tr!("開啟 GIF 資料夾", "Open the GIF folder")).clicked() {
                *action = Some(Action::OpenExportDir(ExportDir::Image));
            }
            if ui
                .add_enabled(
                    self.settings.export.image_dir.is_some(),
                    egui::Button::new(tr!("用預設的 GIF 資料夾", "Use the default GIF folder")),
                )
                .clicked()
            {
                self.settings.export.image_dir = None;
                changed = true;
            }
        });
        let max = GIF_MAX_SECS;
        ui.weak(tf!(
            "預設：跟截圖放在一起（上面的截圖資料夾）。GIF 最長 {max:.0} 秒；大小、格率在匯出視窗裡選，會記住上次的選擇。",
            "Default: together with the screenshots (the screenshot folder above). GIFs can be at most {max:.0} seconds; \
             choose the size and frame rate in the Export window, and the last choice is remembered."
        ));
        changed
    }

    /// 正在匯出（介面測試用；之後「播完後」的動作也要等它做完）
    #[doc(hidden)]
    pub fn export_busy(&self) -> bool {
        self.export.job.is_some()
    }

    /// 匯出視窗開著（介面測試用）
    #[doc(hidden)]
    pub fn export_open(&self) -> bool {
        self.export.open
    }

    /// 測試用：之後開始的片段用這些觀察點（例如在寫好之後停住，確認關閉影戲時不留暫存檔）
    #[doc(hidden)]
    pub fn set_export_test_hooks(&mut self, hooks: clip::TestHooks) {
        self.export.test = hooks;
    }

    /// 測試用：之後開始的 GIF 用這些觀察點
    #[doc(hidden)]
    pub fn set_export_gif_test_hooks(&mut self, hooks: gif::TestHooks) {
        self.export.gif_test = hooks;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_points_are_clamped_into_the_video() {
        assert_eq!(clamp_point(-3.0, Some(20.0)), 0.0);
        assert_eq!(clamp_point(5.5, Some(20.0)), 5.5);
        assert_eq!(clamp_point(356400.0, Some(20.0)), 20.0, "99:00:00 拉回結尾");
        // 不知道長度（直播之類）：只拉回 0 以上
        assert_eq!(clamp_point(99.0, None), 99.0);
        assert_eq!(clamp_point(99.0, Some(f64::NAN)), 99.0);
    }

    fn track(id: i64, kind: TrackKind, external: bool) -> Track {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "type": match kind {
                TrackKind::Video => "video",
                TrackKind::Audio => "audio",
                _ => "sub",
            },
            "external": external,
            "codec": "aac",
        }))
        .unwrap()
    }

    #[test]
    fn estimated_size_follows_the_chosen_tracks() {
        let size = || Some(100_000_000);
        let bitrate = || Some(128_000);
        // 有影像：檔案大小 × 長度比例
        assert_eq!(
            estimate_size(10.0, Some(100.0), true, true, size, bitrate),
            Some(10_000_000)
        );
        // 只放聲音、檔案裡有影像：音軌的位元率 × 長度（128 kbit/s × 10 秒 = 160 kB），不是影像的大小
        assert_eq!(
            estimate_size(10.0, Some(100.0), false, true, size, bitrate),
            Some(160_000)
        );
        // 不知道位元率：不顯示
        assert_eq!(estimate_size(10.0, Some(100.0), false, true, size, || None), None);
        // 檔案本來就只有聲音：檔案大小 × 長度比例
        assert_eq!(
            estimate_size(10.0, Some(100.0), false, false, size, || None),
            Some(10_000_000)
        );
        // 不知道長度、範圍是空的：不顯示
        assert_eq!(estimate_size(10.0, None, true, true, size, bitrate), None);
        assert_eq!(estimate_size(0.0, Some(100.0), true, true, size, bitrate), None);
    }

    #[test]
    fn picks_map_back_to_track_ids() {
        // 外掛的音軌不算位置：第 2 條內嵌的音軌是編號 3
        let tracks = vec![
            track(1, TrackKind::Video, false),
            track(1, TrackKind::Audio, false),
            track(2, TrackKind::Audio, true),
            track(3, TrackKind::Audio, false),
        ];
        let pick = StreamPick::of(&tracks, &tracks[3]).unwrap();
        assert_eq!(pick.ordinal, 2);
        assert_eq!(track_id(&tracks, Some(&pick)), Some(3));
        assert_eq!(track_id(&tracks, None), None);
        let video = StreamPick::of(&tracks, &tracks[0]).unwrap();
        assert_eq!(track_id(&tracks, Some(&video)), Some(1));
    }
}
