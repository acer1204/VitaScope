//! 播放清單面板（F6，比照 PotPlayer）：目前的清單、雙擊播放、拖曳排序、Delete 移除，
//! 加入檔案 / 資料夾、依檔名排序、清空，開啟 / 儲存播放清單檔（.m3u8，見 `m3u.rs`）。
//!
//! 每一列是同一個能點也能拖的元件（`Ui::dnd_drag_source` 一按下就算開始拖，點擊會被吃掉）；
//! 放下的位置看滑鼠在第幾列之間（列與列的空隙也算），拖到上下邊緣會自動捲動。

use super::{ACCENT, VitascopeApp, file_name, icon_button};
use crate::formats;
use crate::m3u;
use crate::playlist::Playlist;
use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, DragAndDrop, Layout, Rect, RichText, Sense, Stroke, TextWrapMode,
    ViewportCommand, WidgetInfo, WidgetType, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// 面板預設寬度
pub(super) const PANEL_WIDTH: f32 = 280.0;
/// 拖到清單上下邊緣多近開始自動捲動
const AUTOSCROLL_MARGIN: f32 = 24.0;

/// 拖曳排序時帶著的資料：被拖的那一項在清單上的位置
#[derive(Clone, Copy)]
struct DragRow(usize);

/// 面板上的操作（畫完再做，畫的時候還借著清單）
enum ListOp {
    Close,
    Play(usize),
    Select(usize),
    Remove(usize),
    Move(usize, usize),
    CopyPath(usize),
    Sort,
    Clear,
    AddFiles,
    AddFolder,
    Open,
    Save,
}

impl VitascopeApp {
    /// 打開 / 關閉播放清單。一般視窗（沒有最大化）時視窗跟著變寬 / 變窄，影片的大小不變；
    /// 打開時沒有加寬（右邊放不下）的話，關掉時也不縮
    pub(super) fn toggle_playlist(&mut self, ctx: &egui::Context) {
        self.settings.show_playlist = !self.settings.show_playlist;
        self.save_settings();
        // 下次打開時捲到正在播的那一項
        self.playlist_follow = None;
        self.playlist_view = (0.0, 0.0);
        let grew = self.playlist_grew.take();
        let (fullscreen, maximized, outer, monitor) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.fullscreen.unwrap_or(false),
                v.maximized.unwrap_or(false),
                v.outer_rect,
                v.monitor_size,
            )
        });
        if fullscreen || maximized || self.frames < 2 {
            return;
        }
        // 視窗內容的大小（點數）
        let inner = ctx.content_rect();
        if self.settings.show_playlist {
            let width = self.playlist_width_pref;
            // 右邊放不下就不加寬（影片變窄），免得視窗跑出螢幕。只知道螢幕大小、不知道螢幕在哪裡，
            // 視窗在主螢幕左邊（座標是負的）時不判斷
            let room = match (outer, monitor) {
                (Some(outer), Some(monitor)) if outer.min.x >= 0.0 => monitor.x - outer.max.x,
                _ => f32::INFINITY,
            };
            if room >= width {
                let new_width = inner.width() + width;
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(new_width, inner.height())));
                self.playlist_grew = Some((width, new_width));
            }
        } else if let Some((by, width_after)) = grew
            && (inner.width() - width_after).abs() < 1.0
        {
            // 打開之後使用者沒有自己調整過視窗大小，才縮回去
            let new_width = (inner.width() - by).max(super::MIN_WINDOW_WIDTH);
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(vec2(new_width, inner.height())));
        }
    }

    /// 面板內容
    pub(super) fn playlist_panel(&mut self, ui: &mut egui::Ui) {
        let mut op = None;
        let count = self.playlist.as_ref().map_or(0, Playlist::len);
        let current = self.playlist.as_ref().and_then(Playlist::current_index);
        let selected = self.playlist_selected.filter(|i| *i < count);

        ui.horizontal(|ui| {
            let title = match current {
                Some(i) => crate::tf!("播放清單（{}/{count}）", "Playlist ({}/{count})", i + 1),
                None => crate::tf!("播放清單（{count}）", "Playlist ({count})"),
            };
            ui.strong(title);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(icon_button("×"))
                    .on_hover_text(crate::tr!("關閉（F6）", "Close (F6)"))
                    .clicked()
                {
                    op = Some(ListOp::Close);
                }
                ui.menu_button("…", |ui| {
                    if ui.button(crate::tr!("加入檔案…", "Add files…")).clicked() {
                        op = Some(ListOp::AddFiles);
                    }
                    if ui.button(crate::tr!("加入資料夾…", "Add folder…")).clicked() {
                        op = Some(ListOp::AddFolder);
                    }
                    if ui
                        .add_enabled(count > 1, egui::Button::new(crate::tr!("依檔名排序", "Sort by name")))
                        .clicked()
                    {
                        op = Some(ListOp::Sort);
                    }
                    if ui
                        .add_enabled(
                            selected.is_some(),
                            egui::Button::new(crate::tr!("移除選取的項目", "Remove selected")).shortcut_text("Delete"),
                        )
                        .clicked()
                        && let Some(i) = selected
                    {
                        op = Some(ListOp::Remove(i));
                    }
                    if ui
                        .add_enabled(count > 0, egui::Button::new(crate::tr!("清空清單", "Clear")))
                        .clicked()
                    {
                        op = Some(ListOp::Clear);
                    }
                    ui.separator();
                    if ui
                        .button(crate::tr!("開啟播放清單檔…", "Open playlist file…"))
                        .clicked()
                    {
                        op = Some(ListOp::Open);
                    }
                    if ui
                        .add_enabled(
                            count > 0,
                            egui::Button::new(crate::tr!("儲存播放清單檔…", "Save playlist file…")),
                        )
                        .clicked()
                    {
                        op = Some(ListOp::Save);
                    }
                })
                .response
                .on_hover_text(crate::tr!(
                    "加入檔案、排序、存成播放清單檔",
                    "Add files, sort, save as a playlist file"
                ));
            });
        });
        ui.separator();

        let Some(list) = &self.playlist else {
            ui.weak(crate::tr!(
                "還沒有播放清單。開啟或拖放影片，或從「…」加入檔案。",
                "No playlist yet. Open or drop videos, or add files from “…”."
            ));
            self.apply_list_op(ui.ctx(), op);
            return;
        };
        if count == 0 {
            ui.weak(crate::tr!("清單是空的", "The list is empty"));
        }
        let ctx = ui.ctx().clone();
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let dragging = DragAndDrop::payload::<DragRow>(&ctx);
        // show_rows 用外層的 item_spacing 算每一列的間距：設成 0，跟列高、放下的位置、捲動的計算一致
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("playlist_rows")
            .auto_shrink([false, false]);
        // 打開清單、換到下一個檔案時：正在播的那一項不在看得到的範圍就捲過去（使用者自己捲動不會被拉回來）
        if let Some(i) = current
            && self.playlist_follow != Some(i)
        {
            self.playlist_follow = Some(i);
            let (offset, height) = self.playlist_view;
            let top = i as f32 * row_h;
            if height <= 0.0 || top < offset || top + row_h > offset + height {
                let height = if height > 0.0 { height } else { ui.available_height() };
                area = area.vertical_scroll_offset((top - (height - row_h) / 2.0).max(0.0));
            }
        }
        let out = area.show_rows(ui, row_h, count, |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in range {
                let path = &list.items()[i];
                let name = format!("{}. {}", i + 1, file_name(path));
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                // 無障礙資訊：螢幕閱讀器、介面測試找得到每一列
                resp.widget_info(|| {
                    WidgetInfo::selected(WidgetType::SelectableLabel, true, selected == Some(i), &name)
                });
                paint_row(ui, rect, &resp, &name, selected == Some(i), current == Some(i));
                // 先點 A 再很快地雙擊 B，egui 會把 B 的第二下算成三連擊
                if resp.double_clicked() || resp.triple_clicked() {
                    op = Some(ListOp::Play(i));
                } else if resp.clicked() {
                    op = Some(ListOp::Select(i));
                }
                if resp.drag_started() {
                    DragAndDrop::set_payload(&ctx, DragRow(i));
                }
                let resp = resp.on_hover_text(path.to_string_lossy());
                resp.context_menu(|ui| {
                    if ui.button(crate::tr!("播放", "Play")).clicked() {
                        op = Some(ListOp::Play(i));
                    }
                    if ui.button(crate::tr!("從清單移除", "Remove from list")).clicked() {
                        op = Some(ListOp::Remove(i));
                    }
                    if ui.button(crate::tr!("複製路徑", "Copy path")).clicked() {
                        op = Some(ListOp::CopyPath(i));
                    }
                });
            }
        });

        self.playlist_view = (out.state.offset.y, out.inner_rect.height());
        // 拖曳中：依滑鼠的位置算出要放在第幾項前面，畫一條線；放開就移動
        if let Some(drag) = dragging
            && let Some(p) = ctx.pointer_latest_pos()
            && out.inner_rect.contains(p)
        {
            let content_top = out.inner_rect.top() - out.state.offset.y;
            let to = ((p.y - content_top) / row_h).round().clamp(0.0, count as f32) as usize;
            let y = content_top + to as f32 * row_h;
            ui.painter_at(out.inner_rect.expand(1.0))
                .hline(out.inner_rect.x_range(), y, Stroke::new(2.0, ACCENT));
            ctx.set_cursor_icon(CursorIcon::Grabbing);
            autoscroll(&ctx, &out, p);
            if ctx.input(|i| i.pointer.any_released()) && DragAndDrop::take_payload::<DragRow>(&ctx).is_some() {
                op = Some(ListOp::Move(drag.0, to));
            }
        }
        self.apply_list_op(&ctx, op);
    }

    fn apply_list_op(&mut self, ctx: &egui::Context, op: Option<ListOp>) {
        let Some(op) = op else { return };
        // 開對話框的操作取消的話什麼都沒變；真的改了清單時它們自己會處理
        let edits = !matches!(
            op,
            ListOp::Close
                | ListOp::Select(_)
                | ListOp::Play(_)
                | ListOp::Save
                | ListOp::CopyPath(_)
                | ListOp::AddFiles
                | ListOp::AddFolder
                | ListOp::Open
        );
        // 手動改過清單：背景還在掃描的資料夾結果不能再蓋掉它
        if edits {
            self.playlist_scan = None;
            self.owns_session = true;
        }
        match op {
            ListOp::Close => self.toggle_playlist(ctx),
            ListOp::Select(i) => self.playlist_selected = Some(i),
            ListOp::Play(i) => {
                self.playlist_selected = Some(i);
                let list = self.playlist.as_ref();
                let st = &self.player.state;
                let is_current = list.and_then(Playlist::current_index) == Some(i) && st.loaded;
                if is_current && st.eof {
                    // 播完停在最後一格的那一項：從頭再播
                    self.run(ctx, super::Action::Restart);
                } else if let Some(path) = list.and_then(|l| l.items().get(i)).cloned()
                    && !is_current
                {
                    // 正在播的那一項不重新開（三連擊之類的）
                    self.open_at(&path, Some(i));
                }
            }
            ListOp::Remove(i) => self.remove_from_playlist(i),
            ListOp::Move(from, to) => {
                if let Some(list) = &mut self.playlist {
                    list.move_item(from, to);
                    // 選取跟著被拖的那一項
                    let len = list.len();
                    self.playlist_selected = Some(if to > from { to - 1 } else { to }.min(len.saturating_sub(1)));
                }
            }
            ListOp::CopyPath(i) => {
                if let Some(path) = self.playlist.as_ref().and_then(|l| l.items().get(i)) {
                    ctx.copy_text(path.to_string_lossy().into_owned());
                    self.osd(crate::tr!("已複製路徑", "Path copied"));
                }
            }
            ListOp::Sort => {
                if let Some(list) = &mut self.playlist {
                    list.sort();
                }
                self.playlist_selected = None;
            }
            ListOp::Clear => {
                self.playlist = Some(Playlist::cleared());
                self.playlist_selected = None;
                self.pending_auto_next = false;
            }
            ListOp::AddFiles => self.add_files_dialog(),
            ListOp::AddFolder => self.add_folder_dialog(),
            ListOp::Open => self.open_playlist_dialog(),
            ListOp::Save => self.save_playlist_dialog(),
        }
        if edits {
            self.persist_playlist();
        }
    }

    /// 移出清單（Delete 鍵或選單）；正在播的照樣播完
    pub(super) fn remove_from_playlist(&mut self, i: usize) {
        let Some(list) = &mut self.playlist else { return };
        if i >= list.len() {
            return;
        }
        self.playlist_scan = None;
        list.remove(i);
        let len = list.len();
        // 選取移到下一項（刪掉最後一項時是新的最後一項），可以連按 Delete
        self.playlist_selected = (len > 0).then(|| i.min(len - 1));
        self.persist_playlist();
    }

    /// 加到清單最後；還沒有在播的話，播第一個加進來的
    pub(super) fn add_to_playlist(&mut self, files: Vec<PathBuf>) {
        let files: Vec<PathBuf> = files
            .into_iter()
            .map(|p| std::path::absolute(&p).unwrap_or(p))
            .collect();
        let Some(first) = files.first().cloned() else { return };
        self.playlist_scan = None;
        self.owns_session = true;
        let added = match &mut self.playlist {
            Some(list) => list.extend(files),
            None => {
                let n = files.len();
                self.playlist = Some(Playlist::from_files(files).manual());
                n
            }
        };
        self.persist_playlist();
        let st = &self.player.state;
        if !st.loaded && !st.loading {
            self.open(&first);
        } else {
            self.osd(crate::tf!(
                "加入播放清單：{added} 個檔案",
                "Added to the playlist: {added} file(s)"
            ));
        }
    }

    fn add_files_dialog(&mut self) {
        let mut dialog = self
            .file_dialog()
            .set_title(crate::tr!("加入播放清單", "Add to playlist"))
            .add_filter(crate::tr!("影音檔案", "Media files"), &formats::all_media())
            .add_filter(crate::tr!("所有檔案", "All files"), &["*"]);
        if let Some(dir) = self.player.state.path.as_deref().and_then(|p| Path::new(p).parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(mut files) = dialog.pick_files() {
            crate::playlist::sort_by_name(&mut files);
            self.add_to_playlist(files);
        }
    }

    /// 加入資料夾：裡面的影音檔依檔名排序（網路磁碟上的大資料夾要掃一陣子，在背景掃）
    fn add_folder_dialog(&mut self) {
        let Some(dir) = self
            .file_dialog()
            .set_title(crate::tr!("加入資料夾", "Add folder"))
            .pick_folder()
        else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            // 跟同資料夾播放清單一樣：隱藏檔（包括 macOS 留下的「._檔名」）、資料夾不算
            let files = crate::playlist::media_in_dir(&dir);
            if tx.send(files).is_ok() {
                ctx.request_repaint();
            }
        });
        self.folder_add = Some(rx);
    }

    pub(super) fn poll_folder_add(&mut self) {
        let Some(rx) = &self.folder_add else { return };
        match rx.try_recv() {
            Ok(files) => {
                self.folder_add = None;
                if files.is_empty() {
                    self.osd(crate::tr!("資料夾裡沒有影音檔", "No media files in that folder"));
                } else {
                    self.add_to_playlist(files);
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.folder_add = None,
        }
    }

    fn open_playlist_dialog(&mut self) {
        if let Some(path) = self
            .file_dialog()
            .set_title(crate::tr!("開啟播放清單檔", "Open playlist file"))
            .add_filter(crate::tr!("播放清單", "Playlist"), formats::PLAYLIST)
            .pick_file()
        {
            self.open_playlist_file(&path);
        }
    }

    /// 開啟播放清單檔：換成檔案裡的清單，從第一個開始播
    pub(super) fn open_playlist_file(&mut self, path: &Path) {
        match m3u::read_text(path) {
            Ok(text) => self.open_playlist_text(path, &text),
            Err(e) => self.osd(crate::tf!("無法開啟播放清單：{e}", "Cannot open the playlist: {e}")),
        }
    }

    /// 播放清單檔的內容（已經讀好的）
    pub(super) fn open_playlist_text(&mut self, path: &Path, text: &str) {
        // 相對路徑（命令列）先轉成完整路徑：清單裡的相對路徑以它所在的資料夾為準
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let path = path.as_path();
        let base = path.parent().unwrap_or(Path::new(""));
        let files: Vec<PathBuf> = m3u::parse(text, base)
            .into_iter()
            .map(|e| e.path)
            .filter(|p| m3u::keep_entry(p))
            .collect();
        let Some(first) = files.first().cloned() else {
            self.osd(crate::tf!(
                "播放清單是空的：{}",
                "The playlist is empty: {}",
                file_name(path)
            ));
            return;
        };
        self.remember_position();
        self.playlist_scan = None;
        self.playlist_selected = None;
        self.playlist = Some(Playlist::from_files(files).manual());
        self.owns_session = true;
        self.persist_playlist();
        self.open_at(&first, Some(0));
    }

    fn save_playlist_dialog(&mut self) {
        let Some(list) = &self.playlist else { return };
        let entries: Vec<m3u::Entry> = list
            .items()
            .iter()
            .map(|p| m3u::Entry {
                path: p.clone(),
                title: None,
                duration: None,
            })
            .collect();
        // 第一個篩選條件的副檔名是預設的（Windows）
        let mut dialog = self
            .file_dialog()
            .set_title(crate::tr!("儲存播放清單", "Save playlist"))
            .add_filter(crate::tr!("UTF-8 播放清單", "UTF-8 playlist"), &["m3u8"])
            .add_filter(crate::tr!("播放清單", "Playlist"), &["m3u"])
            .set_file_name(crate::tr!("播放清單.m3u8", "Playlist.m3u8"));
        if let Some(dir) = list.items().first().and_then(|p| p.parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else { return };
        // macOS / Linux 的對話框不會自己補副檔名
        if !formats::is_playlist(&path) {
            path.set_extension("m3u8");
        }
        match m3u::write(&path, &entries) {
            Ok(()) => self.osd(crate::tf!("已儲存播放清單：{}", "Playlist saved: {}", file_name(&path))),
            Err(e) => self.osd(crate::tf!("無法儲存播放清單：{e}", "Cannot save the playlist: {e}")),
        }
    }

    /// 手動整理的清單存起來（下次開啟時還在）；同資料夾掃描出來的清單不存（存檔刪掉）
    pub(super) fn persist_playlist(&self) {
        if !self.persist_playlist || !self.owns_session {
            return;
        }
        // 正在播的已經移出清單（或還原後還沒開始播）時，記下接下來要播的那一項
        let (items, current): (&[PathBuf], Option<usize>) = match &self.playlist {
            Some(list) if list.is_manual() => (list.items(), list.resume_index()),
            _ => (&[], None),
        };
        if let Err(e) = m3u::save_session(items, current) {
            eprintln!("[vitascope] 無法儲存播放清單：{e}");
        }
    }

    /// 清單面板的底色（跟控制列一樣）
    pub(super) fn playlist_frame() -> egui::Frame {
        egui::Frame::NONE
            .fill(Color32::from_gray(28))
            .inner_margin(egui::Margin::symmetric(8, 6))
    }
}

/// 畫一列：選取的底色、目前播放的「▶」與藍字、檔名太長時截斷
fn paint_row(ui: &egui::Ui, rect: Rect, resp: &egui::Response, name: &str, selected: bool, current: bool) {
    let visuals = ui.visuals();
    if selected {
        ui.painter().rect_filled(rect, 2.0, visuals.selection.bg_fill);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let gutter = 16.0;
    let color = if current { ACCENT } else { visuals.text_color() };
    if current {
        ui.painter().text(
            rect.left_center() + vec2(3.0, 0.0),
            Align2::LEFT_CENTER,
            "▶",
            egui::FontId::proportional(10.0),
            ACCENT,
        );
    }
    let mut text = RichText::new(name).color(color);
    if current {
        text = text.strong();
    }
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(TextWrapMode::Truncate),
        rect.width() - gutter - 4.0,
        egui::TextStyle::Body,
    );
    let pos = egui::pos2(rect.left() + gutter, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
}

/// 拖到清單的上下邊緣時自動捲動（只有真的捲動了才要求重畫，不然到底了還會一直重畫）
fn autoscroll(ctx: &egui::Context, out: &egui::scroll_area::ScrollAreaOutput<()>, p: egui::Pos2) {
    let inner = out.inner_rect;
    let delta = if p.y < inner.top() + AUTOSCROLL_MARGIN {
        -8.0
    } else if p.y > inner.bottom() - AUTOSCROLL_MARGIN {
        8.0
    } else {
        return;
    };
    let max = (out.content_size.y - inner.height()).max(0.0);
    let mut state = out.state;
    let offset = (state.offset.y + delta).clamp(0.0, max);
    if (offset - state.offset.y).abs() > 0.1 {
        state.offset.y = offset;
        state.store(ctx, out.id);
        ctx.request_repaint();
    }
}
