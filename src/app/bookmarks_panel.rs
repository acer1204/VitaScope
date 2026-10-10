//! 書籤：新增、上一個 / 下一個、跳到某個書籤；右鍵選單「書籤 ▸」；進度條上的標記；
//! 側邊面板的「書籤」分頁（H）：點一下跳過去、雙擊改名、右鍵選單、Delete 刪除、「全部刪除…」。
//! 存檔交給 `crate::bookmarks` 的寫入執行緒，這裡只改記憶體、不碰檔案。

use super::playlist_panel::SideOp;
use super::{Action, VitascopeApp, fmt_time};
use crate::bookmarks::{self, AddError, Mark};
use crate::keymap::Command;
use crate::player::Chapter;
use crate::settings::SideTab;
use crate::theme::Palette;
use crate::{tf, tr};
use eframe::egui::{
    self, Align2, Color32, Id, Rect, RichText, Sense, Shape, Stroke, TextWrapMode, WidgetInfo, WidgetType, pos2, vec2,
};

/// 右鍵選單最多列出幾個書籤（目前位置附近的）
const MENU_MARKS: usize = 20;
/// 滑鼠離標記這麼近（點）就算指著它：顯示書籤名稱，點了精準跳到書籤的時間
pub(super) const SNAP_DISTANCE: f32 = 5.0;
/// 標記（往下的小三角形）的寬、高
const MARKER_WIDTH: f32 = 6.0;
const MARKER_HEIGHT: f32 = 5.0;
/// 書籤分頁每一列左邊留給「▶」（目前的位置）的寬度
const ROW_GUTTER: f32 = 16.0;

/// 這個檔案的書籤只放在記憶體、不存檔：開的是網址，而書籤的代號含帳號密碼、token 之類的（`net::storable`；
/// 跟最近開啟、續播一樣不記到磁碟），關閉影戲就沒有了。網站影片的代號是 `ytdl://網站/影片代號`，網址裡的 token 不在代號裡：照常存檔
pub(super) fn marks_in_memory_only(path: &str, key: &str) -> bool {
    crate::net::is_network(path) && !crate::net::storable(key)
}

/// 書籤分頁上正在改名的書籤（Enter、點別的地方 = 改好；Esc = 不改）
pub(super) struct RenameEdit {
    /// 哪個檔案的書籤（換了檔案就不改了）
    key: String,
    id: u64,
    text: String,
    /// 剛開始改名：下一次畫的時候把輸入焦點給它
    focus: bool,
}

/// 書籤分頁上的操作（畫完再做，畫的時候還借著書籤）
enum MarkOp {
    Side(SideOp),
    /// 點一下：選取、跳過去
    Jump(u64),
    StartRename(u64),
    /// 改名改好了（編號、新的名稱）
    Rename(u64, String),
    CancelRename,
    Remove(u64),
    CopyTime(f64),
    Add,
    /// 「全部刪除…」：先問
    ClearAll,
}

impl VitascopeApp {
    /// 書籤用的檔案代號：本機檔案是完整路徑，網路串流是續播的代號（`network::history_key`：網址去掉 `#` 之後的部分，
    /// 網站影片是同一部影片的代號），書籤跟續播用同一個；mpv 自己的網址（`av://` 之類）照開啟時的網址
    pub(super) fn media_key(&self) -> Option<String> {
        let st = &self.player.state;
        let path = st.path.clone().filter(|_| st.loaded)?;
        Some(super::network::history_key(&path, st.net.as_deref()).unwrap_or(path))
    }

    /// 目前檔案的書籤（依時間排序）
    pub(super) fn current_marks(&self) -> &[Mark] {
        match self.media_key() {
            Some(key) => self.bookmarks.marks(&key),
            None => &[],
        }
    }

    /// 書籤（介面測試用）
    pub fn bookmarks(&self) -> &crate::bookmarks::Bookmarks {
        &self.bookmarks
    }

    /// 書籤存檔的結果（每一幀）：失敗時提示，記憶體裡照樣保留這次的修改
    pub(super) fn poll_bookmarks(&mut self) {
        for e in self.bookmarks.poll() {
            self.osd(crate::tf!("無法儲存書籤：{e}", "Couldn't save bookmarks: {e}"));
        }
    }

    /// P：在目前的位置新增書籤。時間直接問 mpv（按下的那一刻）：看到的狀態可能還是跳轉前的；
    /// 剛送出、mpv 還沒做的精準跳轉用它的目標（見 `Player::position_now`）
    pub(super) fn add_bookmark(&mut self) {
        let Some(key) = self.media_key() else {
            self.osd(crate::tr!("請先開啟影片", "Open a video first"));
            return;
        };
        if !self.player.state.seekable {
            self.osd(crate::tr!(
                "這個檔案不能跳轉，無法加書籤",
                "This file isn't seekable, so it can't be bookmarked"
            ));
            return;
        }
        let t = self.player.position_now().max(0.0);
        let path = self.player.state.path.clone().unwrap_or_default();
        let private = marks_in_memory_only(&path, &key);
        if private {
            self.bookmarks.keep_in_memory(&key);
        }
        let msg = match self.bookmarks.add(&key, t) {
            // 第一次在這種網址上加書籤：說一次書籤不會留下來
            Ok(_) if private && self.private_marks_told.insert(key.clone()) => crate::tr!(
                "這個網址含登入資訊，書籤只保留到關閉影戲",
                "This URL contains sign-in data; its bookmarks are kept until VitaScope closes"
            )
            .to_owned(),
            Ok(m) => crate::tf!("新增書籤 {}", "Bookmark added at {}", fmt_time(m.time)),
            Err(AddError::Duplicate(at)) => {
                crate::tf!("{} 已經有書籤", "There's already a bookmark at {}", fmt_time(at))
            }
            Err(AddError::Full) => crate::tf!(
                "這個檔案的書籤已經滿了（{} 個）",
                "This file already has {} bookmarks",
                bookmarks::MAX_MARKS
            ),
        };
        self.osd(msg);
    }

    /// Shift+PgUp / PgDn：上一個（`dir` < 0）/ 下一個書籤。目前的時間也問 mpv（連按時看到的狀態可能還沒跳到；
    /// 上一次按的跳轉 mpv 還沒做時用它的目標，連按三次就前進三個）
    pub(super) fn step_bookmark(&mut self, dir: i32) {
        let marks = self.current_marks().to_vec();
        if marks.is_empty() {
            self.osd(self.keymap.no_bookmarks_osd());
            return;
        }
        let now = self.player.position_now();
        let found = if dir > 0 {
            bookmarks::next_after(&marks, now)
        } else {
            bookmarks::prev_before(&marks, now)
        };
        match found {
            Some(i) => self.jump_to_mark(&marks, i),
            None if dir > 0 => self.osd(crate::tr!("後面沒有書籤", "No more bookmarks after this point")),
            None => self.osd(crate::tr!("前面沒有書籤", "No bookmarks before this point")),
        }
    }

    /// 跳到編號 `id` 的書籤（右鍵選單）
    pub(super) fn jump_bookmark(&mut self, id: u64) {
        let marks = self.current_marks().to_vec();
        if let Some(i) = marks.iter().position(|m| m.id == id) {
            self.jump_to_mark(&marks, i);
        }
    }

    /// 精準跳到第 `i` 個書籤；暫停中照樣暫停
    fn jump_to_mark(&mut self, marks: &[Mark], i: usize) {
        if !self.player.state.seekable {
            self.osd(crate::tr!("這個檔案不能跳轉", "This file isn't seekable"));
            return;
        }
        let m = &marks[i];
        let _ = self.player.seek_to(m.time, true);
        self.osd(crate::tf!(
            "書籤 {}/{}：{}",
            "Bookmark {}/{}: {}",
            i + 1,
            marks.len(),
            mark_title(m)
        ));
    }

    /// 側邊面板書籤分頁的標題：「書籤（5）」
    pub(super) fn bookmarks_title(&self) -> String {
        let n = self.current_marks().len();
        tf!("書籤（{n}）", "Bookmarks ({n})")
    }

    /// 刪掉一個書籤（Delete、右鍵選單）；刪的是選取的那一個時，選取移到下一個（可以連按 Delete）
    fn remove_bookmark(&mut self, id: u64) {
        let Some(key) = self.media_key() else { return };
        let Some(i) = self.current_marks().iter().position(|m| m.id == id) else {
            return;
        };
        self.bookmarks.remove(&key, id);
        if self.bookmark_selected == Some(id) {
            let marks = self.current_marks();
            self.bookmark_selected = marks.get(i.min(marks.len().saturating_sub(1))).map(|m| m.id);
        }
    }

    /// Delete：刪掉書籤分頁上選取的書籤
    pub(super) fn remove_selected_bookmark(&mut self) {
        if let Some(id) = self.bookmark_selected {
            self.remove_bookmark(id);
        }
    }

    /// 側邊面板的「書籤」分頁：目前檔案的書籤（依時間），目前的位置標「▶」；
    /// 點一下跳過去、雙擊改名、右鍵選單；最下面是「+ 新增書籤」
    pub(super) fn bookmarks_panel(&mut self, ui: &mut egui::Ui) {
        let key = self.media_key();
        let marks = self.current_marks().to_vec();
        let st = &self.player.state;
        let (loaded, seekable) = (st.loaded, st.seekable);
        let current = bookmarks::current_at(&marks, st.time_pos);
        let chapters = st.chapters.clone();
        // 選取的書籤已經不在了（刪掉了、換了檔案）
        if self
            .bookmark_selected
            .is_some_and(|id| !marks.iter().any(|m| m.id == id))
        {
            self.bookmark_selected = None;
        }
        // 改名到一半換了檔案（自動接下一個、拖放進來的…）：跟點了別的地方一樣，改好（改的是原來那個檔案的書籤）；
        // 同一個檔案、書籤卻不在了（別的視窗刪掉了）：不改了
        let other_file = |e: &RenameEdit| key.as_deref() != Some(e.key.as_str());
        if let Some(e) = self
            .bookmark_edit
            .take_if(|e| other_file(e) || !marks.iter().any(|m| m.id == e.id))
            && other_file(&e)
        {
            self.bookmarks.rename(&e.key, e.id, &e.text);
        }
        let selected = self.bookmark_selected;
        let add_hint = self.keymap.hint(Command::BookmarkAdd);
        let mut op = None;
        // 改名結束（改好或不改）另外記：點別的地方讓輸入框失去焦點時，那一下的操作（`op`）也要做
        let mut done = None;
        let side = self.side_header(ui, |ui| {
            ui.menu_button("…", |ui| {
                let mut add = egui::Button::new(tr!("新增書籤", "Add bookmark"));
                if !add_hint.is_empty() {
                    add = add.shortcut_text(add_hint.as_str());
                }
                if ui.add_enabled(loaded && seekable, add).clicked() {
                    op = Some(MarkOp::Add);
                }
                if ui
                    .add_enabled(
                        selected.is_some(),
                        egui::Button::new(tr!("刪除選取的書籤", "Delete selected")).shortcut_text("Delete"),
                    )
                    .clicked()
                    && let Some(id) = selected
                {
                    op = Some(MarkOp::Remove(id));
                }
                if ui
                    .add_enabled(!marks.is_empty(), egui::Button::new(tr!("全部刪除…", "Delete all…")))
                    .clicked()
                {
                    op = Some(MarkOp::ClearAll);
                }
            })
            .response
            .on_hover_text(tr!("新增、刪除書籤", "Add or delete bookmarks"));
        });
        if let Some(side) = side {
            op = Some(MarkOp::Side(side));
        }

        // 最下面：新增書籤（先畫，清單才知道剩下多高）
        egui::Panel::bottom("bookmarks_footer")
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 4)))
            .resizable(false)
            .show(ui, |ui| {
                let label = self
                    .keymap
                    .labeled(tr!("+ 新增書籤", "+ Add bookmark"), Command::BookmarkAdd);
                let r = ui.add_enabled(loaded && seekable, egui::Button::new(label));
                if r.clicked() {
                    op = Some(MarkOp::Add);
                }
                if !loaded {
                    r.on_disabled_hover_text(tr!("請先開啟影片", "Open a video first"));
                } else if !seekable {
                    r.on_disabled_hover_text(tr!(
                        "這個檔案不能跳轉，無法加書籤",
                        "This file isn't seekable, so it can't be bookmarked"
                    ));
                }
            });

        if !loaded {
            ui.weak(tr!("沒有開啟檔案", "No file is open"));
        } else if marks.is_empty() {
            if seekable {
                ui.weak(self.keymap.bookmarks_empty_hint());
            } else {
                ui.weak(tr!(
                    "這個檔案不能跳轉，無法加書籤",
                    "This file isn't seekable, so it can't be bookmarked"
                ));
            }
        } else {
            let mut edit = self.bookmark_edit.take();
            self.bookmark_rows(ui, &marks, &chapters, selected, current, &mut edit, &mut op, &mut done);
            self.bookmark_edit = edit;
        }
        let ctx = ui.ctx().clone();
        // 先把改名做完，再做讓輸入框失去焦點的那一下（點了另一個分頁、×、新增、別的書籤…）
        self.apply_mark_op(&ctx, key.clone(), &marks, done);
        self.apply_mark_op(&ctx, key, &marks, op);
    }

    /// 書籤分頁的每一列（`edit` 是正在改名的那一個：那一列換成輸入框；改名結束時放到 `done`）
    #[allow(clippy::too_many_arguments)]
    fn bookmark_rows(
        &self,
        ui: &mut egui::Ui,
        marks: &[Mark],
        chapters: &[Chapter],
        selected: Option<u64>,
        current: Option<usize>,
        edit: &mut Option<RenameEdit>,
        op: &mut Option<MarkOp>,
        done: &mut Option<MarkOp>,
    ) {
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        let delete_hint = "Delete";
        // show_rows 用外層的 item_spacing 算每一列的間距：設成 0，跟列高、捲動的計算一致
        ui.spacing_mut().item_spacing.y = 0.0;
        // 改名的那一列這一幀有沒有畫（show_rows 只畫看得到的列）
        let mut edit_drawn = false;
        egui::ScrollArea::vertical()
            .id_salt("bookmark_rows")
            .auto_shrink([false, false])
            .show_rows(ui, row_h, marks.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    let m = &marks[i];
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    let label = menu_label(m);
                    // 無障礙資訊：螢幕閱讀器、介面測試找得到每一列
                    resp.widget_info(|| {
                        WidgetInfo::selected(WidgetType::SelectableLabel, true, selected == Some(m.id), &label)
                    });
                    let hint = if m.name.is_empty() {
                        chapter_hint(chapters, m.time)
                    } else {
                        None
                    };
                    let name_x = paint_mark_row(
                        ui,
                        rect,
                        &resp,
                        m,
                        hint,
                        selected == Some(m.id),
                        current == Some(i),
                        edit.as_ref().is_some_and(|e| e.id == m.id),
                    );
                    // 改名中：名稱的位置換成輸入框（Enter、點別的地方改好，Esc 不改）
                    if let Some(e) = edit.as_mut().filter(|e| e.id == m.id) {
                        edit_drawn = true;
                        let field =
                            Rect::from_min_max(pos2(name_x, rect.top() + 1.0), rect.right_bottom() - vec2(2.0, 1.0));
                        let r = ui.put(
                            field,
                            egui::TextEdit::singleline(&mut e.text)
                                .id(Id::new(("bookmark_rename", m.id)))
                                .char_limit(bookmarks::MAX_NAME)
                                .hint_text(hint.unwrap_or(tr!("名稱", "Name"))),
                        );
                        if std::mem::take(&mut e.focus) {
                            r.request_focus();
                        } else if r.lost_focus() {
                            // egui 收到 Esc 時也會拿掉焦點：同一幀有 Esc 就是取消
                            *done = Some(if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                MarkOp::CancelRename
                            } else {
                                MarkOp::Rename(m.id, e.text.clone())
                            });
                        }
                        continue;
                    }
                    // 先點 A 再很快地雙擊 B，egui 會把 B 的第二下算成三連擊（跟播放清單一樣）
                    if resp.double_clicked() || resp.triple_clicked() {
                        *op = Some(MarkOp::StartRename(m.id));
                    } else if resp.clicked() {
                        *op = Some(MarkOp::Jump(m.id));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(tr!("跳到這裡", "Jump here")).clicked() {
                            *op = Some(MarkOp::Jump(m.id));
                        }
                        if ui.button(tr!("改名", "Rename")).clicked() {
                            *op = Some(MarkOp::StartRename(m.id));
                        }
                        if ui.button(tr!("複製時間", "Copy time")).clicked() {
                            *op = Some(MarkOp::CopyTime(m.time));
                        }
                        if ui
                            .add(egui::Button::new(tr!("刪除", "Delete")).shortcut_text(delete_hint))
                            .clicked()
                        {
                            *op = Some(MarkOp::Remove(m.id));
                        }
                    });
                }
            });
        // 改名的那一列捲到看不見了：輸入框沒畫，egui 也拿掉了它的焦點（之後不會再「失去焦點」，Enter、Esc 也收不到）。
        // 跟點了別的地方一樣，改好（剛開始改名、還沒拿到焦點的不算）
        if let Some(e) = edit.as_ref()
            && !edit_drawn
            && !e.focus
            && done.is_none()
        {
            *done = Some(MarkOp::Rename(e.id, e.text.clone()));
        }
    }

    fn apply_mark_op(&mut self, ctx: &egui::Context, key: Option<String>, marks: &[Mark], op: Option<MarkOp>) {
        let Some(op) = op else { return };
        match op {
            MarkOp::Side(side) => self.apply_side_op(ctx, side),
            MarkOp::Jump(id) => {
                self.bookmark_selected = Some(id);
                self.run(ctx, Action::BookmarkJump(id));
            }
            MarkOp::StartRename(id) => {
                self.bookmark_selected = Some(id);
                if let Some(key) = key
                    && let Some(m) = marks.iter().find(|m| m.id == id)
                {
                    self.bookmark_edit = Some(RenameEdit {
                        key,
                        id,
                        text: m.name.clone(),
                        focus: true,
                    });
                }
            }
            MarkOp::Rename(id, name) => {
                self.bookmark_edit = None;
                if let Some(key) = key {
                    self.bookmarks.rename(&key, id, &name);
                }
            }
            MarkOp::CancelRename => self.bookmark_edit = None,
            MarkOp::Remove(id) => self.remove_bookmark(id),
            MarkOp::CopyTime(t) => {
                let time = fmt_time(t);
                ctx.copy_text(time.clone());
                self.osd(tf!("已複製時間：{time}", "Time copied: {time}"));
            }
            MarkOp::Add => self.run(ctx, Action::BookmarkAdd),
            MarkOp::ClearAll => self.bookmarks_clear = key,
        }
    }

    /// 改名到一半、書籤分頁就不見了（例如按了控制列的 ☰ 換到播放清單）：跟點了別的地方一樣，改好。
    /// 不然輸入框下次出現時沒有焦點，也不會再「失去焦點」，一直停在改名的樣子
    pub(super) fn finish_hidden_rename(&mut self) {
        let shown = self.settings.show_playlist && self.settings.side_tab == SideTab::Bookmarks;
        if !shown && let Some(e) = self.bookmark_edit.take() {
            self.bookmarks.rename(&e.key, e.id, &e.text);
        }
    }

    /// 「全部刪除…」的確認對話框（蓋住整個畫面；Esc、點外面 = 取消）
    pub(super) fn bookmarks_clear_modal(&mut self, ctx: &egui::Context) {
        let Some(key) = self.bookmarks_clear.clone() else {
            return;
        };
        let n = self.bookmarks.marks(&key).len();
        if n == 0 {
            // 已經沒有了（例如別的視窗刪光了）
            self.bookmarks_clear = None;
            return;
        }
        let mut answer = None;
        let modal = egui::Modal::new(Id::new("bookmarks_clear")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.label(clear_question(n));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr!("刪除", "Delete")).clicked() {
                    answer = Some(true);
                }
                if ui.button(tr!("取消", "Cancel")).clicked() {
                    answer = Some(false);
                }
            });
        });
        if answer == Some(true) {
            let n = self.bookmarks.clear(&key);
            if self.media_key().as_deref() == Some(key.as_str()) {
                self.bookmark_selected = None;
            }
            self.bookmark_edit.take_if(|e| e.key == key);
            self.osd(cleared_osd(n));
        }
        if answer.is_some() || modal.should_close() {
            self.bookmarks_clear = None;
        }
        // 對話框開著：下一幀的按鍵都交給它（空白鍵不會暫停、Esc 只關對話框）
        self.modal_open |= self.bookmarks_clear.is_some();
    }

    /// 右鍵選單「書籤 ▸」：新增書籤、目前位置附近最多 20 個書籤（點了跳過去）、書籤清單、上一個 / 下一個的按鍵
    pub(super) fn bookmarks_menu(&self, ui: &mut egui::Ui) -> Option<Action> {
        let st = &self.player.state;
        let marks = self.current_marks();
        let mut action = None;
        ui.menu_button(crate::tr!("書籤", "Bookmarks"), |ui| {
            let mut add = egui::Button::new(crate::tr!("新增書籤", "Add bookmark"));
            let hint = self.keymap.hint(Command::BookmarkAdd);
            if !hint.is_empty() {
                add = add.shortcut_text(hint);
            }
            let r = ui.add_enabled(st.loaded && st.seekable, add);
            if r.clicked() {
                action = Some(Action::BookmarkAdd);
            }
            if !st.loaded {
                r.on_disabled_hover_text(crate::tr!("請先開啟影片", "Open a video first"));
            } else if !st.seekable {
                r.on_disabled_hover_text(crate::tr!(
                    "這個檔案不能跳轉，無法加書籤",
                    "This file isn't seekable, so it can't be bookmarked"
                ));
            }
            if !marks.is_empty() {
                ui.separator();
                let current = bookmarks::current_at(marks, st.time_pos);
                let (from, to) = menu_window(marks.len(), current.unwrap_or(0), MENU_MARKS);
                for (i, m) in marks.iter().enumerate().take(to).skip(from) {
                    if ui.selectable_label(current == Some(i), menu_label(m)).clicked() {
                        action = Some(Action::BookmarkJump(m.id));
                    }
                }
                if marks.len() > MENU_MARKS {
                    ui.weak(crate::tf!("共 {} 個書籤", "{} bookmarks in all", marks.len()));
                }
            } else if st.loaded {
                ui.weak(crate::tr!("這個檔案還沒有書籤", "No bookmarks in this file yet"));
            }
            ui.separator();
            // 側邊面板的書籤分頁（開著時標成選取，按了關掉）
            let shown = self.settings.show_playlist && self.settings.side_tab == SideTab::Bookmarks;
            let mut list = egui::Button::selectable(shown, crate::tr!("書籤清單", "Bookmark list"));
            let hint = self.keymap.hint(Command::BookmarkList);
            if !hint.is_empty() {
                list = list.shortcut_text(hint);
            }
            if ui.add(list).clicked() {
                action = Some(Action::ToggleBookmarks);
            }
            let keys = self.keymap.pair(Command::BookmarkPrev, Command::BookmarkNext);
            if !keys.is_empty() {
                ui.weak(crate::tf!(
                    "上一個 / 下一個書籤：{keys}",
                    "Previous / next bookmark: {keys}"
                ));
            }
        });
        action
    }
}

/// 書籤的稱呼：名稱，沒有名稱時是時間
fn mark_title(m: &Mark) -> String {
    if m.name.is_empty() {
        fmt_time(m.time)
    } else {
        m.name.clone()
    }
}

/// 選單上的一行：「12:34  OP 結束」，沒有名稱時只有時間
fn menu_label(m: &Mark) -> String {
    if m.name.is_empty() {
        fmt_time(m.time)
    } else {
        format!("{}  {}", fmt_time(m.time), m.name)
    }
}

/// 滑鼠停在標記上時，進度條上方的時間改成「12:34 · 書籤：OP 結束」
pub(super) fn hover_label(m: &Mark) -> String {
    if m.name.is_empty() {
        crate::tf!("{} · 書籤", "{} · Bookmark", fmt_time(m.time))
    } else {
        crate::tf!("{} · 書籤：{}", "{} · Bookmark: {}", fmt_time(m.time), m.name)
    }
}

/// 沒有名稱的書籤：在哪個有名稱的章節裡（清單上淡淡地寫出來）
fn chapter_hint(chapters: &[Chapter], time: f64) -> Option<&str> {
    let i = chapters.iter().rposition(|c| c.time <= time + 1e-3)?;
    chapters[i].title.as_deref().map(str::trim).filter(|t| !t.is_empty())
}

/// 「全部刪除…」的問題
fn clear_question(n: usize) -> String {
    if n == 1 {
        tr!("刪除這個檔案的 1 個書籤？", "Delete the bookmark of this file?").to_owned()
    } else {
        tf!("刪除這個檔案的 {n} 個書籤？", "Delete all {n} bookmarks of this file?")
    }
}

/// 全部刪除之後的提示
fn cleared_osd(n: usize) -> String {
    if n == 1 {
        tr!("已刪除 1 個書籤", "Deleted 1 bookmark").to_owned()
    } else {
        tf!("已刪除 {n} 個書籤", "Deleted {n} bookmarks")
    }
}

/// 書籤分頁的一列：選取的底色、目前位置的「▶」與強調色、時間、名稱（沒有名稱時淡淡地寫章節名稱），
/// 太長時截斷。回傳名稱開始的 x（改名的輸入框放在那裡）；`editing` 時不畫名稱
#[allow(clippy::too_many_arguments)]
fn paint_mark_row(
    ui: &egui::Ui,
    rect: Rect,
    resp: &egui::Response,
    m: &Mark,
    hint: Option<&str>,
    selected: bool,
    current: bool,
    editing: bool,
) -> f32 {
    let visuals = ui.visuals();
    if selected {
        ui.painter().rect_filled(rect, 2.0, visuals.selection.bg_fill);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let accent = Palette::of(visuals).accent;
    let color = if current { accent } else { visuals.text_color() };
    if current {
        ui.painter().text(
            rect.left_center() + vec2(3.0, 0.0),
            Align2::LEFT_CENTER,
            "▶",
            egui::FontId::proportional(10.0),
            accent,
        );
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let time = ui.painter().layout_no_wrap(fmt_time(m.time), font, color);
    let left = rect.left() + ROW_GUTTER;
    ui.painter()
        .galley(pos2(left, rect.center().y - time.size().y / 2.0), time.clone(), color);
    let name_x = left + time.size().x + 10.0;
    if editing {
        return name_x;
    }
    let (text, text_color) = match (m.name.as_str(), hint) {
        ("", Some(h)) => (
            RichText::new(h).color(visuals.weak_text_color()),
            visuals.weak_text_color(),
        ),
        ("", None) => return name_x,
        (name, _) => {
            let mut t = RichText::new(name).color(color);
            if current {
                t = t.strong();
            }
            (t, color)
        }
    };
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(TextWrapMode::Truncate),
        (rect.right() - name_x - 4.0).max(0.0),
        egui::TextStyle::Body,
    );
    ui.painter().galley(
        pos2(name_x, rect.center().y - galley.size().y / 2.0),
        galley,
        text_color,
    );
    name_x
}

/// 列出 `len` 個裡的哪一段（最多 `max` 個，盡量讓 `center` 在中間）：[from, to)
fn menu_window(len: usize, center: usize, max: usize) -> (usize, usize) {
    if len <= max {
        return (0, len);
    }
    let from = center.saturating_sub(max / 2).min(len - max);
    (from, from + max)
}

/// 離 `x` 最近、在 [`SNAP_DISTANCE`] 以內的標記（`x_of` = 時間在進度條上的位置）
pub(super) fn mark_near(marks: &[Mark], x: f32, x_of: impl Fn(f64) -> f32) -> Option<&Mark> {
    marks
        .iter()
        .map(|m| (m, (x_of(m.time) - x).abs()))
        .filter(|&(_, d)| d <= SNAP_DISTANCE)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(m, _)| m)
}

/// 進度條上的書籤標記：往下的小三角形，畫在進度條（`bar`）上方、元件（`rect`）的範圍內
pub(super) fn paint_markers(
    painter: &egui::Painter,
    rect: Rect,
    bar: Rect,
    xs: impl Iterator<Item = f32>,
    color: Color32,
) {
    let top = rect.top() + 0.5;
    let tip = (top + MARKER_HEIGHT).min(bar.top() - 0.5);
    for x in xs {
        painter.add(Shape::convex_polygon(
            vec![
                pos2(x - MARKER_WIDTH / 2.0, top),
                pos2(x + MARKER_WIDTH / 2.0, top),
                pos2(x, tip),
            ],
            color,
            Stroke::NONE,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(time: f64, name: &str) -> Mark {
        Mark {
            id: 1,
            time,
            name: name.into(),
            added: 0,
        }
    }

    #[test]
    fn menu_window_keeps_the_current_mark_in_view() {
        assert_eq!(menu_window(5, 3, 20), (0, 5));
        assert_eq!(menu_window(100, 0, 20), (0, 20));
        assert_eq!(menu_window(100, 50, 20), (40, 60));
        assert_eq!(menu_window(100, 99, 20), (80, 100));
        assert_eq!(menu_window(21, 15, 20), (1, 21));
    }

    #[test]
    fn labels_and_snapping() {
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(hover_label(&mark(754.0, "OP 結束")), "12:34 · 書籤：OP 結束");
        assert_eq!(hover_label(&mark(754.0, "")), "12:34 · 書籤");
        assert_eq!(menu_label(&mark(754.0, "OP 結束")), "12:34  OP 結束");
        assert_eq!(menu_label(&mark(754.0, "")), "12:34");
        assert_eq!(mark_title(&mark(754.0, "")), "12:34");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(hover_label(&mark(754.0, "OP")), "12:34 · Bookmark: OP");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        // 1 秒 = 1 點：5 點以內算指著，選最近的
        let marks = [mark(10.0, "a"), mark(14.0, "b"), mark(40.0, "c")];
        let x_of = |t: f64| t as f32;
        assert_eq!(mark_near(&marks, 12.5, x_of).unwrap().name, "b");
        assert_eq!(mark_near(&marks, 11.5, x_of).unwrap().name, "a");
        assert_eq!(mark_near(&marks, 35.0, x_of).unwrap().name, "c");
        assert!(mark_near(&marks, 25.0, x_of).is_none());
        assert!(mark_near(&[], 25.0, x_of).is_none());
    }

    #[test]
    fn chapter_hints_and_clear_texts() {
        let ch = |time: f64, title: Option<&str>| Chapter {
            title: title.map(str::to_owned),
            time,
        };
        let chapters = [
            ch(0.0, Some("片頭")),
            ch(90.0, None),
            ch(120.0, Some("  本篇 ")),
            ch(600.0, Some("")),
        ];
        assert_eq!(chapter_hint(&chapters, 10.0), Some("片頭"));
        assert_eq!(chapter_hint(&chapters, 100.0), None, "沒有名稱的章節");
        assert_eq!(chapter_hint(&chapters, 120.0), Some("本篇"), "剛好在章節開頭");
        assert_eq!(chapter_hint(&chapters, 700.0), None);
        assert_eq!(chapter_hint(&[], 10.0), None);
        assert_eq!(chapter_hint(&chapters[2..], 10.0), None, "第一個章節之前");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(clear_question(3), "刪除這個檔案的 3 個書籤？");
        assert_eq!(cleared_osd(3), "已刪除 3 個書籤");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(clear_question(1), "Delete the bookmark of this file?");
        assert_eq!(clear_question(2), "Delete all 2 bookmarks of this file?");
        assert_eq!(cleared_osd(1), "Deleted 1 bookmark");
        assert_eq!(cleared_osd(4), "Deleted 4 bookmarks");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }
}
