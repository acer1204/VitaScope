//! 書籤：新增、上一個 / 下一個、跳到某個書籤；右鍵選單「書籤 ▸」；進度條上的標記。
//! 存檔交給 `crate::bookmarks` 的寫入執行緒，這裡只改記憶體、不碰檔案。

use super::{Action, VitascopeApp, fmt_time};
use crate::bookmarks::{self, AddError, Mark};
use crate::keymap::Command;
use eframe::egui::{self, Color32, Rect, Shape, Stroke, pos2};

/// 右鍵選單最多列出幾個書籤（目前位置附近的）
const MENU_MARKS: usize = 20;
/// 滑鼠離標記這麼近（點）就算指著它：顯示書籤名稱，點了精準跳到書籤的時間
pub(super) const SNAP_DISTANCE: f32 = 5.0;
/// 標記（往下的小三角形）的寬、高
const MARKER_WIDTH: f32 = 6.0;
const MARKER_HEIGHT: f32 = 5.0;

impl VitascopeApp {
    /// 書籤用的檔案代號：本機檔案是完整路徑，網址是開啟時的網址（一字不差）。
    /// 之後網路來源改用續播的代號，書籤跟著用同一個
    pub(super) fn media_key(&self) -> Option<String> {
        let st = &self.player.state;
        if st.loaded { st.path.clone() } else { None }
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
        let msg = match self.bookmarks.add(&key, t) {
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

    /// 右鍵選單「書籤 ▸」：新增書籤、目前位置附近最多 20 個書籤（點了跳過去）、上一個 / 下一個的按鍵
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
            let keys = self.keymap.pair(Command::BookmarkPrev, Command::BookmarkNext);
            if !keys.is_empty() {
                ui.separator();
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
}
