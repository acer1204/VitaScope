//! 視窗模式：一般視窗、迷你播放器、子母畫面。
//!
//! 只有一個視窗：mpv 畫在主視窗的 GL context 裡，流暢播放也只看一個 swap chain，
//! 所以迷你播放器、子母畫面是主視窗的模式（跟 PotPlayer 的迷你模式一樣），不是另開一個視窗。
//!
//! 這裡是純邏輯（不碰視窗、可以在每個平台測）：
//! - 迷你播放器、子母畫面的大小、預設位置（目前螢幕的右下角）、記住的位置還能不能用（螢幕可能拔掉了）；
//! - 進入 / 離開時要送給視窗的指令（層級不在這裡：一律由 app 的 `on_top_now` 決定，只有一個來源）。
//!   子母畫面沒有標題列、邊框（`Decorations(false)`）；進入迷你播放器、回到一般視窗一律把它們加回來；
//! - 切換時要等什麼（[`Switch`]）：先離開全螢幕、取消最大化，送出新的大小之後等視窗真的變了。
//!   macOS 離開 / 進入全螢幕有約一秒的動畫，動畫結束時 AppKit 會把視窗改回動畫前的大小，
//!   中途送的大小、位置會被蓋掉，所以 macOS 要等位置大小穩定下來（P-A2）；
//! - 子母畫面的移動、改大小（Windows、Linux）：邊緣多寬算改大小（[`resize_zone`]）、改完停下來之後把比例改回影片的
//!   （[`SnapDebounce`]、[`snap_to_aspect`]）、系統的拖曳結束後沒送「放開」時補一個（[`OsDrag`]，P-A3）。

use crate::screens::Area;
use crate::settings::WindowGeometry;
use eframe::egui::{self, CursorIcon, Pos2, Rect, ResizeDirection, Vec2, ViewportCommand, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 視窗目前的模式（不存檔：下次啟動一律是一般視窗，跟不記全螢幕一樣）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Normal,
    /// 迷你播放器：小視窗（保留標題列，系統的移動、改大小、貼齊照常）、預設置頂、影片下面一列精簡的控制列
    Mini,
    /// 子母畫面：只有影片（沒有標題列、邊框），一律置頂；拖曳畫面移動、拉邊緣改大小（比例固定），
    /// 控制項在滑鼠移上去時才出現
    Pip,
}

impl WindowMode {
    /// 精簡的模式（不是一般視窗）：側邊面板、設定之類的大視窗不顯示
    pub fn is_compact(self) -> bool {
        self != WindowMode::Normal
    }

    /// 記錄、提示用的名稱
    pub fn label(self) -> &'static str {
        match self {
            WindowMode::Normal => crate::tr!("一般視窗", "Normal window"),
            WindowMode::Mini => crate::tr!("迷你播放器", "Mini player"),
            WindowMode::Pip => crate::tr!("子母畫面", "Picture-in-picture"),
        }
    }

    /// 記錄（stderr）用的名稱：一律中文（實機測試照這幾個字找切換的記錄）
    pub fn log_name(self) -> &'static str {
        match self {
            WindowMode::Normal => "一般視窗",
            WindowMode::Mini => "迷你播放器",
            WindowMode::Pip => "子母畫面",
        }
    }

    /// 這個模式的最小內容大小
    pub fn min_size(self) -> Vec2 {
        match self {
            WindowMode::Normal => NORMAL_MIN,
            WindowMode::Mini => MINI_MIN,
            WindowMode::Pip => PIP_MIN,
        }
    }

    /// `VITASCOPE_TEST_WINDOW_MODE` 裡的名稱（`normal` / `mini` / `pip`）
    pub fn from_test_name(name: &str) -> Option<Self> {
        match name.trim() {
            "normal" => Some(WindowMode::Normal),
            "mini" => Some(WindowMode::Mini),
            "pip" => Some(WindowMode::Pip),
            _ => None,
        }
    }
}

/// 一般視窗的最小內容大小（控制列的按鈕放得下）
pub const NORMAL_MIN: Vec2 = vec2(640.0, 300.0);
/// 迷你播放器的最小內容大小
pub const MINI_MIN: Vec2 = vec2(360.0, 200.0);
/// 迷你播放器預設的寬度（點）
pub const MINI_WIDTH: f32 = 480.0;
/// 迷你播放器下面那一列控制列的高度（含邊距；面板用固定的高度，算視窗大小時才準）
pub const MINI_BAR: f32 = 38.0;
/// 子母畫面的最小內容大小（16:9 的最小）
pub const PIP_MIN: Vec2 = vec2(160.0, 90.0);
/// 子母畫面預設的寬度（橫的影片；直式的是高度）：螢幕寬度的 ¼（直式：高度的 ⅓），限制在這個範圍
pub const PIP_DEFAULT_RANGE: (f32, f32) = (320.0, 640.0);
/// 子母畫面邊緣多寬（點）算是改大小（Windows、Linux；macOS 用系統的邊緣）
pub const RESIZE_BAND: f32 = 8.0;
/// 子母畫面的大小停下來這麼久才把比例改回影片的（使用者可能還在拉）；系統的拖曳結束後，視窗停下來這麼久之後的
/// 第一個滑鼠移動才補「放開」
pub const SETTLE: Duration = Duration::from_millis(150);
/// 比例差不到這麼多點就不改（四捨五入的誤差）
const SNAP_TOLERANCE: f32 = 2.0;
/// 預設位置離螢幕右邊、下邊的距離
const MARGIN: f32 = 24.0;
/// macOS（Dock）、X11（工作列）的螢幕範圍是整個螢幕，下面多留這麼多；Windows 的是扣掉工作列的工作區
pub const TASKBAR_ALLOWANCE: f32 = 48.0;

/// 迷你播放器、子母畫面的設定（`settings.json` 的 `compact`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactWindows {
    /// 迷你播放器置頂（預設開）。關掉時迷你播放器照「視窗置頂」的模式（不置頂 / 永遠 / 播放時）。
    /// 子母畫面沒有這個選項：一律置頂
    pub mini_on_top: bool,
    /// 迷你播放器上次的位置、大小（寬度沿用，高度依影片比例重算）。用之前先確認還在螢幕上（P-C6）
    pub mini: Option<WindowGeometry>,
    /// 子母畫面上次的位置、大小（寬度沿用，高度依影片比例重算；存下來的高度只是參考）。用之前一樣先確認
    pub pip: Option<WindowGeometry>,
}

impl Default for CompactWindows {
    fn default() -> Self {
        Self {
            mini_on_top: true,
            mini: None,
            pip: None,
        }
    }
}

impl CompactWindows {
    /// 讀檔後整理：不是數字的拿掉，大小至少是最小值（手動改過、或螢幕設定很怪時存下的）
    pub fn sanitized(mut self) -> Self {
        self.mini = self.mini.and_then(|g| sane_geometry(g, MINI_MIN));
        self.pip = self.pip.and_then(|g| sane_geometry(g, PIP_MIN));
        self
    }

    /// 這個模式記住的位置大小（一般視窗沒有：記在 `settings.window`）
    pub fn remembered(&self, mode: WindowMode) -> Option<WindowGeometry> {
        match mode {
            WindowMode::Normal => None,
            WindowMode::Mini => self.mini,
            WindowMode::Pip => self.pip,
        }
    }

    /// 記下這個模式的位置大小（一般視窗不在這裡記）
    pub fn remember(&mut self, mode: WindowMode, g: WindowGeometry) {
        match mode {
            WindowMode::Normal => {}
            WindowMode::Mini => self.mini = Some(g),
            WindowMode::Pip => self.pip = Some(g),
        }
    }
}

/// 位置、大小都是有限的數字才用；大小拉到 `min` 以上（太大的拉回一個合理的上限）
fn sane_geometry(g: WindowGeometry, min: Vec2) -> Option<WindowGeometry> {
    let finite = g.pos.iter().chain(&g.size).all(|v| v.is_finite());
    finite.then(|| WindowGeometry {
        size: [g.size[0].clamp(min.x, 16384.0), g.size[1].clamp(min.y, 16384.0)],
        ..g
    })
}

/// 一般視窗的位置大小：要求進入迷你播放器的那一刻記下（還沒送出取消最大化、離開全螢幕之前，P-B1），
/// 回到一般視窗時照這個還原
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalRestore {
    /// 外框左上角（點）；不知道時 None（不移動，交給系統）
    pub pos: Option<Pos2>,
    /// 內容大小（點）
    pub size: Vec2,
    /// 原本是最大化的：還原後再最大化
    pub maximized: bool,
}

impl NormalRestore {
    /// 從記下的一般視窗位置大小（`settings.window`）
    pub fn from_geometry(g: &WindowGeometry) -> Self {
        Self {
            pos: Some(pos2(g.pos[0], g.pos[1])),
            size: vec2(g.size[0], g.size[1]),
            maximized: g.maximized,
        }
    }
}

/// 迷你播放器的內容大小：寬度 `width`（記住的或預設的），高度 = 影片的高 + 控制列。
/// 直式影片不要變得很高：影片的高最多跟 `width` 一樣，改成縮窄寬度。`aspect` = 寬 / 高（不知道時當 16:9），
/// `limit` = 螢幕大小（點；大小不超過它的 90%）
pub fn mini_size(aspect: Option<f32>, width: f32, limit: Option<Vec2>) -> Vec2 {
    let aspect = aspect.filter(|a| a.is_finite() && *a > 0.01).unwrap_or(16.0 / 9.0);
    let max = limit.map_or(vec2(f32::INFINITY, f32::INFINITY), |m| m * 0.9);
    let width = width.max(MINI_MIN.x).min(max.x.max(MINI_MIN.x));
    let mut video = vec2(width, width / aspect);
    if video.y > width {
        video = vec2(width * aspect, width);
    }
    // 螢幕放不下：依比例縮小影片
    let room_y = (max.y - MINI_BAR).max(MINI_MIN.y - MINI_BAR);
    if video.y > room_y {
        video = vec2(room_y * aspect, room_y);
    }
    vec2(
        video.x.round().max(MINI_MIN.x),
        (video.y.round() + MINI_BAR).max(MINI_MIN.y),
    )
}

/// 影片的長寬比（寬 / 高）：不知道、不合理時當 16:9
fn sane_aspect(aspect: Option<f32>) -> f32 {
    aspect
        .filter(|a| a.is_finite() && *a > 0.01 && *a < 100.0)
        .unwrap_or(16.0 / 9.0)
}

/// 照比例縮放 `size`：放得進 `max`、不小於 `min`（兩個都做不到時以 `min` 為準），四捨五入到整數點
fn fit_aspect(size: Vec2, min: Vec2, max: Vec2) -> Vec2 {
    let mut s = size;
    let down = (max.x / s.x).min(max.y / s.y);
    if down < 1.0 {
        s *= down;
    }
    let up = (min.x / s.x).max(min.y / s.y);
    if up > 1.0 {
        s *= up;
    }
    s.round()
}

/// 子母畫面的內容大小（整個視窗就是影片，比例跟影片一樣）。`width` = 記住的寬度；沒有時橫的影片用螢幕寬度的 ¼、
/// 直式的用螢幕高度的 ⅓，限制在 320–640 點。`aspect` = 寬 / 高（不知道時當 16:9），`limit` = 螢幕大小（點；
/// 大小不超過它的 90%）
pub fn pip_size(aspect: Option<f32>, width: Option<f32>, limit: Option<Vec2>) -> Vec2 {
    let aspect = sane_aspect(aspect);
    let (lo, hi) = PIP_DEFAULT_RANGE;
    let size = match width.filter(|w| w.is_finite() && *w > 0.0) {
        Some(w) => vec2(w, w / aspect),
        None if aspect >= 1.0 => {
            let w = limit.map_or(480.0, |m| m.x / 4.0).clamp(lo, hi);
            vec2(w, w / aspect)
        }
        None => {
            let h = limit.map_or(480.0, |m| m.y / 3.0).clamp(lo, hi);
            vec2(h * aspect, h)
        }
    };
    fit_aspect(size, PIP_MIN, pip_max(limit))
}

/// 子母畫面最大的大小：螢幕（`limit`，點）的 90%；不知道螢幕時不限制
pub fn pip_max(limit: Option<Vec2>) -> Vec2 {
    limit.map_or(vec2(f32::INFINITY, f32::INFINITY), |m| m * 0.9)
}

/// 使用者改了子母畫面的大小（`inner`）之後，照影片的比例改回來：保留改得比較多的那一邊（拉右邊改寬度、拉下面改高度），
/// 另一邊照比例算；四捨五入到整數點，不小於最小值、放得進 `max`（螢幕的 90%：拉成很高的視窗時算出來的寬度可能比螢幕寬）。
/// `before` = 改之前的大小（看哪一邊改得多）。跟比例差不到 2 點時 None（不用改）
pub fn snap_to_aspect(inner: Vec2, before: Vec2, aspect: f32, min: Vec2, max: Vec2) -> Option<Vec2> {
    if !(aspect.is_finite() && aspect > 0.0) || inner.x < 1.0 || inner.y < 1.0 {
        return None;
    }
    let rel = |now: f32, was: f32| if was > 0.0 { ((now - was) / was).abs() } else { 0.0 };
    let want = if rel(inner.x, before.x) >= rel(inner.y, before.y) {
        vec2(inner.x, inner.x / aspect)
    } else {
        vec2(inner.y * aspect, inner.y)
    };
    let want = fit_aspect(want, min, max);
    ((want - inner).abs().max_elem() > SNAP_TOLERANCE).then_some(want)
}

/// 照比例改回來之後新的外框左上角（`dir` = 使用者拉的是哪一邊；`inner` = 拉完的大小、`snapped` = 改回來的大小）：
/// 拉的那一邊的對面不動；使用者沒拉的方向（只拉左、右邊時的上下，只拉上、下邊時的左右）照子母畫面放在右下角的習慣，
/// 下面、右邊不動（拉左邊、上面時右下角不動，設計 P§5.4；不然算出來的那一邊往下、往右長，跑到工作列下面、螢幕外面）。
/// 左上角不動時 None；不是拉我們的邊緣改的（Aero Snap、視窗管理員）也是 None
pub fn snap_position(pos: Pos2, inner: Vec2, snapped: Vec2, dir: Option<ResizeDirection>) -> Option<Pos2> {
    use ResizeDirection as D;
    // （右邊不動, 下面不動）
    let (keep_right, keep_bottom) = match dir? {
        D::West | D::North | D::NorthWest => (true, true),
        D::East | D::NorthEast => (false, true),
        D::South | D::SouthWest => (true, false),
        D::SouthEast => (false, false),
    };
    let d = inner - snapped;
    let moved = pos2(
        if keep_right { pos.x + d.x } else { pos.x },
        if keep_bottom { pos.y + d.y } else { pos.y },
    );
    (moved != pos).then_some(moved)
}

/// 子母畫面的邊緣：滑鼠在 `pos` 時按下去是改哪一邊的大小（離邊緣 `band` 點以內；角落是兩邊一起）。
/// 很小的視窗邊緣不超過寬、高的 ¼（中間還要能拖曳移動）
pub fn resize_zone(pos: Pos2, rect: Rect, band: f32) -> Option<ResizeDirection> {
    use ResizeDirection as D;
    if !rect.contains(pos) {
        return None;
    }
    let bx = band.min(rect.width() / 4.0);
    let by = band.min(rect.height() / 4.0);
    let left = pos.x < rect.left() + bx;
    let right = pos.x >= rect.right() - bx;
    let top = pos.y < rect.top() + by;
    let bottom = pos.y >= rect.bottom() - by;
    match (top, bottom, left, right) {
        (true, _, true, _) => Some(D::NorthWest),
        (true, _, _, true) => Some(D::NorthEast),
        (_, true, true, _) => Some(D::SouthWest),
        (_, true, _, true) => Some(D::SouthEast),
        (true, _, _, _) => Some(D::North),
        (_, true, _, _) => Some(D::South),
        (_, _, true, _) => Some(D::West),
        (_, _, _, true) => Some(D::East),
        _ => None,
    }
}

/// 改大小的那一邊用的滑鼠游標
pub fn resize_cursor(dir: ResizeDirection) -> CursorIcon {
    use ResizeDirection as D;
    match dir {
        D::East | D::West => CursorIcon::ResizeHorizontal,
        D::North | D::South => CursorIcon::ResizeVertical,
        D::NorthWest | D::SouthEast => CursorIcon::ResizeNwSe,
        D::NorthEast | D::SouthWest => CursorIcon::ResizeNeSw,
    }
}

/// 子母畫面改完大小之後把比例改回影片的：大小停下來 [`SETTLE`]（使用者可能還在拉）才改。
/// 不看滑鼠按鍵：系統的拖曳（`StartDrag` / `BeginResize`）結束時，X11、macOS 可能不送「放開」（P-A3）。
/// Aero Snap、視窗管理員的快捷鍵改的大小也一樣改回來（設計 P§8）。程式自己送的大小（進入、換影片）比例本來就對，不會再改。
/// 同一個大小只要求一次：要求了之後視窗還是那個大小（X11 的視窗管理員還沒照改，或不肯改：並排的視窗、限制大小）
/// 就不再送，不然每一幀都送、在兩個大小之間來回
#[derive(Debug, Clone, Copy)]
pub struct SnapDebounce {
    /// 上一幀的大小、從什麼時候開始沒變
    last: Vec2,
    since: Instant,
    /// 已經處理過的大小（比例對的、程式自己送的，或已經要求改回比例的那個大小）：大小停在這裡不用再改；
    /// 跟它比看使用者改的是哪一邊
    settled: Vec2,
}

impl SnapDebounce {
    pub fn new(now: Instant, size: Vec2) -> Self {
        Self {
            last: size,
            since: now,
            settled: size,
        }
    }

    /// 每一幀：`size` = 現在的內容大小、`aspect` = 影片的比例、`max` = 最大（螢幕的 90%）。要改的話回傳新的大小
    pub fn tick(&mut self, now: Instant, size: Vec2, aspect: f32, min: Vec2, max: Vec2) -> Option<Vec2> {
        let near = |a: Vec2, b: Vec2| (a - b).abs().max_elem() < 0.5;
        if !near(size, self.last) {
            self.last = size;
            self.since = now;
            return None;
        }
        if now.duration_since(self.since) < SETTLE || near(size, self.settled) {
            return None;
        }
        let snapped = snap_to_aspect(size, self.settled, aspect, min, max);
        // 要求改了也記成現在的大小：視窗照著改的話下一次比較時比例已經對了；沒改（還沒、或不肯）就不再要求
        self.settled = size;
        snapped
    }

    /// 大小跟上一次比例對的不一樣（還在等它停下來）
    pub fn waiting(&self) -> bool {
        (self.last - self.settled).abs().max_elem() >= 0.5
    }
}

/// 系統的拖曳（移動 `StartDrag`、改大小 `BeginResize`）進行中：結束時 X11、macOS（還有一些 Wayland 桌面）
/// 不一定送「放開」給程式，egui 會一直以為滑鼠按著（子母畫面的控制項不會消失、下一下點擊被吃掉）。
/// 視窗停下來之後的第一個滑鼠移動、或視窗的焦點變了，就補一個「放開」（P-A3）。
/// Windows 結束時會送真的放開（`WM_EXITSIZEMOVE` 之後的 `WM_LBUTTONUP`）：收到就不補
#[derive(Debug, Clone, Copy)]
pub struct OsDrag {
    last: Option<Rect>,
    changed_at: Instant,
}

/// 這一批輸入之後系統的拖曳怎麼了
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DragEnd {
    /// 還在拖
    Going,
    /// 結束了，不用補（真的放開了，或 egui 本來就不覺得按著）
    Released,
    /// 結束了，但沒有放開：在第 `at` 個事件之前補一個在 `pos` 的放開
    Inject { at: usize, pos: Pos2 },
}

impl OsDrag {
    /// 送出 `StartDrag` / `BeginResize` 的時候（`rect` = 那時的外框）
    pub fn new(now: Instant, rect: Option<Rect>) -> Self {
        Self {
            last: rect,
            changed_at: now,
        }
    }

    /// 每一批輸入（`raw_input_hook`，egui 處理之前）：`rect` = 這一批的外框，`primary_down` = egui 上一幀覺得左鍵按著，
    /// `pointer` = 上一幀最後的滑鼠位置（滑鼠離開過視窗時 egui 沒有位置，呼叫的一方用按下的位置）
    pub fn check(
        &mut self,
        now: Instant,
        rect: Option<Rect>,
        events: &[egui::Event],
        primary_down: bool,
        pointer: Option<Pos2>,
    ) -> DragEnd {
        let released = events.iter().any(|e| {
            matches!(
                e,
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    ..
                }
            )
        });
        if !primary_down || released {
            return DragEnd::Released;
        }
        if rect != self.last {
            self.last = rect;
            self.changed_at = now;
        }
        let settled = now.duration_since(self.changed_at) >= SETTLE;
        for (at, e) in events.iter().enumerate() {
            match e {
                // 換了焦點（拖曳到一半切到別的視窗、或拖完才拿到焦點）：拖曳結束了。一定要補放開（egui 還以為按著，
                // 之後真的放開也不會送來：egui-winit 沒有滑鼠位置時丟掉按鍵）；不知道滑鼠在哪就用這一批後面的移動，
                // 都沒有就用左上角（位置只影響放開時算不算點擊）
                egui::Event::WindowFocused(_) => {
                    let later = || {
                        events[at..].iter().find_map(|e| match e {
                            egui::Event::PointerMoved(p) => Some(*p),
                            _ => None,
                        })
                    };
                    let pos = pointer.or_else(later).unwrap_or(Pos2::ZERO);
                    return DragEnd::Inject { at, pos };
                }
                // 視窗停下來之後的第一個滑鼠移動：系統的拖曳已經結束（拖曳中滑鼠被系統拿走，程式收不到移動）
                egui::Event::PointerMoved(p) if settled => return DragEnd::Inject { at, pos: *p },
                _ => {}
            }
        }
        DragEnd::Going
    }
}

/// `VITASCOPE_TEST_WINDOW_MODE=pip@3,normal@9`（實機測試，只跟 `--shot` 一起用）：開始播放後第 3 秒進入子母畫面、
/// 第 9 秒回到一般視窗（也可以是 `mini@秒`）。看不懂的部分略過；照時間排好
pub fn parse_test_modes(value: &str) -> Vec<(WindowMode, Duration)> {
    let mut list: Vec<(WindowMode, Duration)> = value
        .split(',')
        .filter_map(|item| {
            let (name, at) = item.split_once('@')?;
            let at: f64 = at.trim().parse().ok()?;
            if !(at.is_finite() && at >= 0.0) {
                return None;
            }
            Some((WindowMode::from_test_name(name)?, Duration::from_secs_f64(at)))
        })
        .collect();
    list.sort_by_key(|(_, at)| *at);
    list
}

/// 一個螢幕範圍換成點（`Area` 在 Windows、X11 是實體像素，macOS 是點）
fn area_points(a: &Area) -> Rect {
    let s = a.scale.max(0.1) as f32;
    Rect::from_min_size(
        pos2(a.x as f32 / s, a.y as f32 / s),
        vec2(a.w as f32 / s, a.h as f32 / s),
    )
}

/// 視窗所在的螢幕（點的範圍）：先找比例跟視窗一樣、中心在裡面的螢幕，沒有的話（macOS 的範圍本來就是點）
/// 找中心在裡面的，再沒有就找離中心最近的（視窗一大半在螢幕外面）；不知道視窗在哪就用第一個。
///
/// 視窗的點座標是用它所在螢幕的比例換算的（實體像素 ÷ 那個螢幕的比例），不同比例的螢幕換回實體像素時會落在不同的地方，
/// 所以要先看比例
fn area_of(outer: Option<Rect>, areas: &[Area], scale: f32) -> Option<Rect> {
    let first = areas.first()?;
    let contains = |a: &Area, c: Pos2| {
        let s = a.scale as f32;
        let (x, y) = (f64::from(c.x * s), f64::from(c.y * s));
        x >= a.x && x < a.x + a.w && y >= a.y && y < a.y + a.h
    };
    let area = outer
        .and_then(|o| {
            let c = o.center();
            areas
                .iter()
                .find(|a| (a.scale as f32 - scale).abs() < 0.01 && contains(a, c))
                .or_else(|| areas.iter().find(|a| contains(a, c)))
                .or_else(|| {
                    areas.iter().min_by(|a, b| {
                        let d = |a: &Area| area_points(a).distance_sq_to_pos(c);
                        d(a).total_cmp(&d(b))
                    })
                })
        })
        .unwrap_or(first);
    Some(area_points(area))
}

/// 迷你播放器的預設位置（外框左上角，點）：視窗所在的螢幕（[`area_of`]）的右下角，離邊緣 24 點，
/// 下面再留 `allowance`（macOS、X11 的範圍是整個螢幕，要避開 Dock / 工作列）。
/// `outer` = 現在的外框（不知道時用第一個螢幕）、`frame` = 標題列與邊框（外框 − 內容）、`size` = 新的內容大小、
/// `scale` = 視窗現在的縮放比例。沒有螢幕資訊（Wayland：程式不能指定位置）時 None
pub fn placement(
    outer: Option<Rect>,
    frame: Vec2,
    size: Vec2,
    areas: &[Area],
    allowance: f32,
    scale: f32,
) -> Option<Pos2> {
    let r = area_of(outer, areas, scale)?;
    let outer_size = size + frame.max(Vec2::ZERO);
    let pos = pos2(
        r.right() - outer_size.x - MARGIN,
        r.bottom() - outer_size.y - MARGIN - allowance,
    );
    Some(pos2(pos.x.max(r.left()), pos.y.max(r.top())).round())
}

/// 迷你播放器換了大小（新檔案、記住的位置配上新影片的高度）：右下角不動（預設就放在右下角），
/// 超出它所在的螢幕時往上、往左移回來（比螢幕還大時對齊左上角）。
/// `pos` / `old` = 原本的外框左上角與內容大小，`size` = 新的內容大小，`frame` = 標題列與邊框；
/// 其他同 [`placement`]。沒有螢幕資訊（Wayland）時 None
pub fn anchored(
    pos: Pos2,
    old: Vec2,
    size: Vec2,
    frame: Vec2,
    areas: &[Area],
    allowance: f32,
    scale: f32,
) -> Option<Pos2> {
    let frame = frame.max(Vec2::ZERO);
    let r = area_of(Some(Rect::from_min_size(pos, old + frame)), areas, scale)?;
    let outer_size = size + frame;
    let want = pos + (old - size);
    let max = pos2(r.right() - outer_size.x, r.bottom() - allowance - outer_size.y);
    Some(pos2(want.x.min(max.x).max(r.left()), want.y.min(max.y).max(r.top())).round())
}

/// 記住的迷你播放器位置還能不能用：數字正常、標題列在某個螢幕上看得到（螢幕可能拔掉了、解析度變小了，R5 的教訓）。
/// 沒有螢幕資訊（Wayland）時不用：程式本來就不能指定位置
pub fn position_usable(g: &WindowGeometry, areas: Option<&[Area]>) -> bool {
    let finite = g.pos.iter().chain(&g.size).all(|v| v.is_finite());
    finite && areas.is_some_and(|a| !a.is_empty() && crate::screens::title_bar_visible(g.pos, g.size[0], a))
}

/// 進入迷你播放器、子母畫面：最小大小先放寬（不然縮不下去）、標題列（子母畫面拿掉；迷你播放器有，從子母畫面換過來時
/// 加回來）、新的大小、位置（`pos` 是 None 時交給系統）。標題列在大小之前：Windows 拿掉標題列時內容會變大，之後才定大小。
/// 視窗層級另外送（app 的 `on_top_now`，只有一個來源）
pub fn plan_enter(mode: WindowMode, size: Vec2, pos: Option<Pos2>) -> Vec<ViewportCommand> {
    let mut cmds = vec![
        ViewportCommand::MinInnerSize(mode.min_size()),
        ViewportCommand::Decorations(mode != WindowMode::Pip),
        ViewportCommand::InnerSize(size),
    ];
    cmds.extend(pos.map(ViewportCommand::OuterPosition));
    cmds
}

/// 回到一般視窗：標題列一律加回來（從子母畫面回來；本來就有時沒有影響）、最小大小改回來、還原大小與位置
/// （`pos_ok`：位置還在螢幕上），原本最大化的再最大化
pub fn plan_leave(restore: &NormalRestore, pos_ok: bool) -> Vec<ViewportCommand> {
    let mut cmds = vec![
        ViewportCommand::Decorations(true),
        ViewportCommand::MinInnerSize(NORMAL_MIN),
        ViewportCommand::InnerSize(restore.size.max(NORMAL_MIN)),
    ];
    if pos_ok && let Some(p) = restore.pos {
        cmds.push(ViewportCommand::OuterPosition(p));
    }
    if restore.maximized {
        cmds.push(ViewportCommand::Maximized(true));
    }
    cmds
}

/// 視窗的位置大小（切換時比較「變了沒」）：內容大小（點），外框左上角（不知道時 None）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WinRect {
    pub size: Vec2,
    pub pos: Option<Pos2>,
}

impl WinRect {
    /// 差不到半點算一樣
    fn same(&self, other: &WinRect) -> bool {
        let near = |a: Vec2, b: Vec2| (a - b).abs().max_elem() < 0.5;
        near(self.size, other.size)
            && match (self.pos, other.pos) {
                (Some(a), Some(b)) => near(a.to_vec2(), b.to_vec2()),
                (None, None) => true,
                _ => false,
            }
    }
}

/// 這一幀視窗的狀態（`ViewportInfo`）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub rect: WinRect,
    pub fullscreen: Option<bool>,
    pub maximized: Option<bool>,
}

/// 切換要等多久：`stable` = 位置大小停下來多久才算好，`cap` = 最多等多久（之後照樣繼續）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub stable: Duration,
    pub cap: Duration,
}

impl Timing {
    /// macOS：全螢幕的動畫約一秒，動畫結束時 AppKit 改回動畫前的大小（P-A2）：穩定 300 毫秒、最多 1.2 秒。
    /// 其他系統：變了就好，最多等 0.6 秒（指令被忽略時，例如有些 X11 視窗管理員）
    pub fn of(mac: bool) -> Self {
        if mac {
            Self {
                stable: Duration::from_millis(300),
                cap: Duration::from_millis(1200),
            }
        } else {
            Self {
                stable: Duration::ZERO,
                cap: Duration::from_millis(600),
            }
        }
    }

    /// 這個系統的
    pub fn current() -> Self {
        Self::of(cfg!(target_os = "macos"))
    }
}

/// 等全螢幕結束（系統一直沒有回報）最多多久
const FULLSCREEN_CAP: Duration = Duration::from_secs(3);
/// 等取消最大化最多多久
const UNMAXIMIZE_CAP: Duration = Duration::from_secs(1);

/// 等視窗真的變好：位置大小變了（`need_change`），而且停下來 `stable` 那麼久；最多等 `cap`
#[derive(Debug, Clone, Copy)]
struct Settle {
    started: Instant,
    from: WinRect,
    last: WinRect,
    stable_since: Instant,
    changed: bool,
}

impl Settle {
    fn new(now: Instant, rect: WinRect) -> Self {
        Self {
            started: now,
            from: rect,
            last: rect,
            stable_since: now,
            changed: false,
        }
    }

    fn done(&mut self, now: Instant, rect: WinRect, need_change: bool, stable: Duration, cap: Duration) -> bool {
        if !rect.same(&self.last) {
            self.last = rect;
            self.stable_since = now;
        }
        self.changed |= !rect.same(&self.from);
        now.duration_since(self.started) >= cap
            || ((self.changed || !need_change) && now.duration_since(self.stable_since) >= stable)
    }
}

/// 切換進行到哪裡
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 送了 `Fullscreen(false)`：等全螢幕結束（macOS 還要等動畫結束、位置大小穩定）
    LeaveFullscreen,
    /// 送了 `Maximized(false)`：等取消最大化、位置大小變了（Windows 晚一兩幀才給還原後的大小）
    Unmaximize,
    /// 新模式的大小、位置送出了：等視窗真的變了（之後才記住位置、接著做下一件事）
    Resize,
}

/// 這一幀要做什麼
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// 繼續等
    Wait,
    /// 送 `Maximized(false)`（離開全螢幕之後還是最大化的）
    Unmaximize,
    /// 送進入新模式的指令（[`plan_enter`]）；之後等它生效
    Apply,
    /// 好了
    Done,
}

/// 進行中的模式切換（`to` = 要切到的模式）。
/// 進入迷你播放器：全螢幕的先離開全螢幕、最大化的先取消最大化，等那些做完才送新的大小（P-B1、P-A2）；
/// 回到一般視窗：還原的指令馬上送，等視窗真的變回來（之後才可以接著全螢幕、打開側邊面板）
#[derive(Debug, Clone, Copy)]
pub struct Switch {
    pub to: WindowMode,
    stage: Stage,
    /// 這個階段開始的時間
    started: Instant,
    settle: Settle,
    /// 等全螢幕結束時：看到結束之後才開始等位置大小穩定（macOS 的動畫從這時才開始）
    after_fullscreen: Option<Settle>,
    /// 全螢幕是程式自己要求離開的（[`Switch::start`]）。macOS 的 winit 在送出時就把 `fullscreen` 改成 false，
    /// 動畫（約一秒）結束時 AppKit 才改回全螢幕之前的大小：要等大小真的離開全螢幕的大小再算穩定（P-A2）。
    /// 使用者自己結束的（[`Switch::after_system`]）在動畫結束時才回報，不用等變
    requested: bool,
}

impl Switch {
    /// 開始切換。`seen` = 要求的這一幀視窗的狀態；回傳切換與目前的階段
    /// （`Resize` = 已經可以送新模式的指令；不然 app 先送 `Fullscreen(false)` / `Maximized(false)`）
    pub fn start(to: WindowMode, now: Instant, seen: &Seen) -> (Self, Stage) {
        let stage = if seen.fullscreen == Some(true) {
            Stage::LeaveFullscreen
        } else if seen.maximized == Some(true) {
            Stage::Unmaximize
        } else {
            Stage::Resize
        };
        let mut switch = Self::at(to, stage, now, seen.rect);
        switch.requested = true;
        (switch, stage)
    }

    /// 指令已經送出、只等視窗變好（回到一般視窗）
    pub fn resizing(to: WindowMode, now: Instant, rect: WinRect) -> Self {
        Self::at(to, Stage::Resize, now, rect)
    }

    /// 系統的全螢幕 / 最大化剛結束（使用者自己按的）：只等位置大小穩定（macOS 的動畫），之後還原一般視窗
    pub fn after_system(to: WindowMode, now: Instant, rect: WinRect) -> Self {
        Self::at(to, Stage::LeaveFullscreen, now, rect)
    }

    fn at(to: WindowMode, stage: Stage, now: Instant, rect: WinRect) -> Self {
        Self {
            to,
            stage,
            started: now,
            settle: Settle::new(now, rect),
            after_fullscreen: None,
            requested: false,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// 每一幀：看視窗的狀態決定下一步
    pub fn tick(&mut self, now: Instant, seen: &Seen, timing: Timing) -> Next {
        let waited = now.duration_since(self.started);
        match self.stage {
            Stage::LeaveFullscreen => {
                if waited < FULLSCREEN_CAP {
                    if seen.fullscreen == Some(true) {
                        return Next::Wait;
                    }
                    // 要等穩定的系統（macOS）、而且是自己要求離開的：大小要先變（不然動畫中途就送了，會被蓋掉）
                    let need_change = self.requested && !timing.stable.is_zero();
                    let settle = self.after_fullscreen.get_or_insert_with(|| Settle::new(now, seen.rect));
                    if !settle.done(now, seen.rect, need_change, timing.stable, timing.cap) {
                        return Next::Wait;
                    }
                }
                // 全螢幕之前是最大化的：離開全螢幕後還是最大化，先取消
                if seen.maximized == Some(true) {
                    *self = Self::at(self.to, Stage::Unmaximize, now, seen.rect);
                    Next::Unmaximize
                } else {
                    *self = Self::at(self.to, Stage::Resize, now, seen.rect);
                    Next::Apply
                }
            }
            Stage::Unmaximize => {
                let unmaximized = seen.maximized != Some(true);
                let settled = self.settle.done(now, seen.rect, true, timing.stable, UNMAXIMIZE_CAP);
                if (unmaximized && settled) || waited >= UNMAXIMIZE_CAP {
                    *self = Self::at(self.to, Stage::Resize, now, seen.rect);
                    Next::Apply
                } else {
                    Next::Wait
                }
            }
            Stage::Resize => {
                if self.settle.done(now, seen.rect, true, timing.stable, timing.cap) {
                    Next::Done
                } else {
                    Next::Wait
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 沒有最大值（不用放進螢幕）
    const NO_MAX: Vec2 = vec2(f32::INFINITY, f32::INFINITY);

    fn area(x: f64, y: f64, w: f64, h: f64, scale: f64) -> Area {
        Area { x, y, w, h, scale }
    }

    fn rect(w: f32, h: f32) -> WinRect {
        WinRect {
            size: vec2(w, h),
            pos: None,
        }
    }

    fn seen(r: WinRect, fullscreen: bool, maximized: bool) -> Seen {
        Seen {
            rect: r,
            fullscreen: Some(fullscreen),
            maximized: Some(maximized),
        }
    }

    #[test]
    fn mini_size_follows_the_video_shape() {
        // 16:9：480 寬，影片 270 高 + 控制列
        assert_eq!(
            mini_size(Some(16.0 / 9.0), MINI_WIDTH, None),
            vec2(480.0, 270.0 + MINI_BAR)
        );
        // 4:3
        assert_eq!(mini_size(Some(4.0 / 3.0), 480.0, None), vec2(480.0, 360.0 + MINI_BAR));
        // 不知道比例（還在載入、純音訊）：當 16:9
        assert_eq!(mini_size(None, 480.0, None), mini_size(Some(16.0 / 9.0), 480.0, None));
        // 直式：影片的高最多跟寬度一樣，寬度縮窄（但不小於最小值）
        let portrait = mini_size(Some(9.0 / 16.0), 480.0, None);
        assert_eq!(portrait, vec2(MINI_MIN.x, 480.0 + MINI_BAR));
        // 記住的寬度太窄：拉到最小值；很寬的影片：高度至少是最小值
        assert_eq!(mini_size(Some(16.0 / 9.0), 100.0, None).x, MINI_MIN.x);
        assert_eq!(mini_size(Some(4.0), 360.0, None).y, MINI_MIN.y);
        // 螢幕小：不超過螢幕的 90%，比例照舊
        let s = mini_size(Some(16.0 / 9.0), 1200.0, Some(vec2(1000.0, 500.0)));
        assert!(s.x <= 900.0 && s.y <= 450.0 + 0.5, "{s:?}");
        assert!(((s.x / (s.y - MINI_BAR)) - 16.0 / 9.0).abs() < 0.02, "{s:?}");
    }

    #[test]
    fn placement_uses_the_bottom_right_of_the_monitor_the_window_is_on() {
        // 兩個螢幕：左邊 1920×1040 工作區（100%），右邊 3840×2100（225%，實體像素）
        let areas = [
            area(0.0, 0.0, 1920.0, 1040.0, 1.0),
            area(1920.0, 0.0, 3840.0, 2100.0, 2.25),
        ];
        let size = vec2(480.0, 308.0);
        let frame = vec2(16.0, 39.0);
        // 視窗在左邊的螢幕：右下角（Windows 的工作區不用多留）
        let left = Rect::from_min_size(pos2(100.0, 100.0), vec2(800.0, 500.0));
        let p = placement(Some(left), frame, size, &areas, 0.0, 1.0).unwrap();
        assert_eq!(p, pos2(1920.0 - 496.0 - 24.0, 1040.0 - 347.0 - 24.0));
        // 視窗在右邊的螢幕（點的座標 = 實體像素 / 2.25；換成左邊螢幕的比例也落在左邊螢幕裡，要看視窗的比例）
        let right = Rect::from_min_size(pos2(1000.0, 200.0), vec2(600.0, 400.0));
        let p = placement(Some(right), frame, size, &areas, 0.0, 2.25).unwrap();
        let r = Rect::from_min_size(pos2(1920.0 / 2.25, 0.0), vec2(3840.0 / 2.25, 2100.0 / 2.25));
        assert_eq!(p, pos2(r.right() - 496.0 - 24.0, r.bottom() - 347.0 - 24.0).round());
        // 比例都對不上（macOS：範圍是點、視窗是 2 倍）：中心在裡面的那一個
        let far = Rect::from_min_size(pos2(2000.0, 200.0), vec2(600.0, 400.0));
        assert_eq!(placement(Some(far), frame, size, &areas, 0.0, 2.0), Some(p));
        // macOS、X11：整個螢幕，下面多留 Dock / 工作列
        let p = placement(Some(left), frame, size, &areas, TASKBAR_ALLOWANCE, 1.0).unwrap();
        assert_eq!(p.y, 1040.0 - 347.0 - 24.0 - TASKBAR_ALLOWANCE);
        // 不知道視窗在哪：第一個螢幕；視窗不在任何螢幕上：也是第一個
        let first = placement(Some(left), frame, size, &areas, 0.0, 1.0);
        assert_eq!(placement(None, frame, size, &areas, 0.0, 1.0), first);
        let lost = Rect::from_min_size(pos2(-5000.0, -5000.0), vec2(10.0, 10.0));
        assert_eq!(placement(Some(lost), frame, size, &areas, 0.0, 2.25), first);
        // 螢幕比視窗還小：不跑到螢幕左上角外面
        let tiny = [area(10.0, 20.0, 300.0, 200.0, 1.0)];
        assert_eq!(placement(None, frame, size, &tiny, 0.0, 1.0), Some(pos2(10.0, 20.0)));
        // 沒有螢幕資訊（Wayland）：不指定位置
        assert_eq!(placement(Some(left), frame, size, &[], 0.0, 1.0), None);
    }

    #[test]
    fn remembered_position_is_checked_against_the_monitors() {
        let areas = [area(0.0, 0.0, 1920.0, 1040.0, 1.0)];
        let g = WindowGeometry {
            pos: [1400.0, 700.0],
            size: [480.0, 308.0],
            maximized: false,
        };
        assert!(position_usable(&g, Some(&areas)));
        // 那個螢幕拔掉了
        let gone = WindowGeometry {
            pos: [2500.0, 700.0],
            ..g
        };
        assert!(!position_usable(&gone, Some(&areas)));
        let nan = WindowGeometry {
            pos: [f32::NAN, 0.0],
            ..g
        };
        assert!(!position_usable(&nan, Some(&areas)));
        // Wayland：不能指定位置
        assert!(!position_usable(&g, None));
        assert!(!position_usable(&g, Some(&[])));
    }

    #[test]
    fn sanitized_drops_broken_geometry_and_clamps_the_size() {
        let c = CompactWindows {
            mini_on_top: false,
            mini: Some(WindowGeometry {
                pos: [10.0, 20.0],
                size: [50.0, 9.0],
                maximized: false,
            }),
            pip: None,
        }
        .sanitized();
        assert_eq!(c.mini.unwrap().size, [MINI_MIN.x, MINI_MIN.y]);
        assert!(!c.mini_on_top);
        let c = CompactWindows {
            mini: Some(WindowGeometry {
                pos: [f32::INFINITY, 20.0],
                size: [480.0, 300.0],
                maximized: false,
            }),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(c.mini, None);
        assert!(CompactWindows::default().mini_on_top, "迷你播放器預設置頂");
    }

    #[test]
    fn plans() {
        // 迷你播放器：保留標題列（從子母畫面換過來時加回來）
        let enter = plan_enter(WindowMode::Mini, vec2(480.0, 308.0), Some(pos2(1400.0, 700.0)));
        assert_eq!(
            enter,
            [
                ViewportCommand::MinInnerSize(MINI_MIN),
                ViewportCommand::Decorations(true),
                ViewportCommand::InnerSize(vec2(480.0, 308.0)),
                ViewportCommand::OuterPosition(pos2(1400.0, 700.0)),
            ]
        );
        // Wayland：不指定位置
        assert_eq!(plan_enter(WindowMode::Mini, vec2(480.0, 308.0), None).len(), 3);
        // 子母畫面：沒有標題列、最小 160×90；標題列在大小之前拿掉
        assert_eq!(
            plan_enter(WindowMode::Pip, vec2(480.0, 270.0), Some(pos2(1400.0, 700.0))),
            [
                ViewportCommand::MinInnerSize(PIP_MIN),
                ViewportCommand::Decorations(false),
                ViewportCommand::InnerSize(vec2(480.0, 270.0)),
                ViewportCommand::OuterPosition(pos2(1400.0, 700.0)),
            ]
        );
        assert!(
            !plan_enter(WindowMode::Pip, vec2(480.0, 270.0), None)
                .iter()
                .any(|c| matches!(c, ViewportCommand::OuterPosition(_)))
        );
        let restore = NormalRestore {
            pos: Some(pos2(100.0, 80.0)),
            size: vec2(960.0, 600.0),
            maximized: true,
        };
        // 回到一般視窗：一律把標題列加回來（從子母畫面回來時）
        assert_eq!(
            plan_leave(&restore, true),
            [
                ViewportCommand::Decorations(true),
                ViewportCommand::MinInnerSize(NORMAL_MIN),
                ViewportCommand::InnerSize(vec2(960.0, 600.0)),
                ViewportCommand::OuterPosition(pos2(100.0, 80.0)),
                ViewportCommand::Maximized(true),
            ]
        );
        // 原本的位置已經不在螢幕上：交給系統；記下的大小太小：至少是一般視窗的最小值
        let small = NormalRestore {
            size: vec2(300.0, 200.0),
            maximized: false,
            ..restore
        };
        assert_eq!(
            plan_leave(&small, false),
            [
                ViewportCommand::Decorations(true),
                ViewportCommand::MinInnerSize(NORMAL_MIN),
                ViewportCommand::InnerSize(NORMAL_MIN),
            ]
        );
    }

    #[test]
    fn modes_names_and_minimums() {
        assert_eq!(WindowMode::Pip.min_size(), PIP_MIN);
        assert_eq!(WindowMode::Mini.min_size(), MINI_MIN);
        assert_eq!(WindowMode::Normal.min_size(), NORMAL_MIN);
        assert!(WindowMode::Pip.is_compact() && WindowMode::Mini.is_compact());
        assert!(!WindowMode::Normal.is_compact());
        assert_eq!(WindowMode::Pip.log_name(), "子母畫面");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(WindowMode::Pip.label(), "Picture-in-picture");
        assert_eq!(WindowMode::Pip.log_name(), "子母畫面", "記錄一律中文");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(WindowMode::Pip.label(), "子母畫面");
    }

    /// 子母畫面的大小：比例跟影片一樣；預設寬度是螢幕寬度的 ¼（320–640），直式的是高度的 ⅓；記住的寬度沿用
    #[test]
    fn pip_size_keeps_the_video_shape() {
        let monitor = Some(vec2(1920.0, 1080.0));
        // 16:9、1920 寬的螢幕：480×270
        assert_eq!(pip_size(Some(16.0 / 9.0), None, monitor), vec2(480.0, 270.0));
        // 不知道比例：當 16:9；不知道螢幕：480 寬
        assert_eq!(pip_size(None, None, monitor), vec2(480.0, 270.0));
        assert_eq!(pip_size(None, None, None), vec2(480.0, 270.0));
        // 小螢幕：至少 320 寬；很大的螢幕（4K 100%）：最多 640 寬
        assert_eq!(pip_size(Some(16.0 / 9.0), None, Some(vec2(1024.0, 768.0))).x, 320.0);
        assert_eq!(pip_size(Some(16.0 / 9.0), None, Some(vec2(3840.0, 2160.0))).x, 640.0);
        // 225% 的 4K 電視（1707 點寬）：¼
        assert_eq!(pip_size(Some(16.0 / 9.0), None, Some(vec2(1706.7, 960.0))).x, 427.0);
        // 直式：高度是螢幕高度的 ⅓（1080 → 360），寬度照比例
        assert_eq!(pip_size(Some(9.0 / 16.0), None, monitor), vec2(203.0, 360.0));
        // 記住的寬度：高度照這次的影片
        assert_eq!(pip_size(Some(4.0 / 3.0), Some(400.0), monitor), vec2(400.0, 300.0));
        // 太小：拉到最小值（比例照舊）；很寬的影片：高度至少 90
        assert_eq!(pip_size(Some(16.0 / 9.0), Some(100.0), monitor), vec2(160.0, 90.0));
        let wide = pip_size(Some(4.0), Some(200.0), monitor);
        assert_eq!(wide, vec2(360.0, 90.0));
        // 太大（記住的寬度比螢幕還寬）：放進螢幕的 90%，比例照舊
        let s = pip_size(Some(16.0 / 9.0), Some(5000.0), monitor);
        assert!(s.x <= 1728.0 && s.y <= 972.0, "{s:?}");
        assert!((s.x / s.y - 16.0 / 9.0).abs() < 0.01, "{s:?}");
        // 每一個大小比例都對（±1 點）
        for (a, w) in [(16.0 / 9.0, 333.0), (2.39, 517.0), (4.0 / 3.0, 161.0), (1.0, 250.0)] {
            let s = pip_size(Some(a), Some(w), monitor);
            assert!((s.y * a - s.x).abs() <= a.max(1.0), "{a} {s:?}");
        }
    }

    /// 改完大小後照影片比例改回來：保留改得比較多的那一邊；差不到 2 點不改；不小於最小值
    #[test]
    fn snap_keeps_the_side_the_user_dragged() {
        let a = 16.0 / 9.0;
        let before = vec2(480.0, 270.0);
        // 拉寬：寬度不變，高度照比例
        assert_eq!(
            snap_to_aspect(vec2(640.0, 280.0), before, a, PIP_MIN, NO_MAX),
            Some(vec2(640.0, 360.0))
        );
        // 拉高：高度不變，寬度照比例
        assert_eq!(
            snap_to_aspect(vec2(500.0, 450.0), before, a, PIP_MIN, NO_MAX),
            Some(vec2(800.0, 450.0))
        );
        // 拉角落成正方形 500×500：高度改得比較多，以高度為準
        assert_eq!(
            snap_to_aspect(vec2(500.0, 500.0), before, a, PIP_MIN, NO_MAX),
            Some(vec2(889.0, 500.0))
        );
        // 從正方形附近拉寬：以寬度為準
        assert_eq!(
            snap_to_aspect(vec2(500.0, 500.0), vec2(400.0, 480.0), a, PIP_MIN, NO_MAX),
            Some(vec2(500.0, 281.0))
        );
        // 差不到 2 點：不改
        assert_eq!(snap_to_aspect(vec2(481.0, 271.0), before, a, PIP_MIN, NO_MAX), None);
        assert_eq!(snap_to_aspect(before, before, a, PIP_MIN, NO_MAX), None);
        // 縮得太小：最小值（比例照舊）
        assert_eq!(
            snap_to_aspect(vec2(100.0, 200.0), before, a, PIP_MIN, NO_MAX),
            Some(vec2(160.0, 90.0))
        );
        assert_eq!(
            snap_to_aspect(vec2(170.0, 60.0), before, a, PIP_MIN, NO_MAX),
            Some(vec2(160.0, 90.0))
        );
        // 比例不合理：不改
        assert_eq!(
            snap_to_aspect(vec2(500.0, 500.0), before, f32::NAN, PIP_MIN, NO_MAX),
            None
        );
        // 拉成很高的視窗（並排在螢幕右半邊）：照高度算的寬度比螢幕寬，縮到放得進最大值（比例照舊）
        assert_eq!(
            snap_to_aspect(vec2(960.0, 1040.0), before, a, PIP_MIN, vec2(1728.0, 972.0)),
            Some(vec2(1728.0, 972.0))
        );
    }

    /// 改回比例時：拉的那一邊的對面不動；沒拉的方向下面、右邊不動（拉左邊、上面時右下角不動，設計 P§5.4）
    #[test]
    fn snap_position_keeps_the_opposite_edge() {
        use ResizeDirection as D;
        let pos = pos2(1000.0, 500.0);
        // 寬度不變、高度從 500 改成 281（矮了 219）
        let (inner, snapped) = (vec2(500.0, 500.0), vec2(500.0, 281.0));
        for d in [D::North, D::NorthWest, D::NorthEast, D::West, D::East] {
            assert_eq!(
                snap_position(pos, inner, snapped, Some(d)),
                Some(pos2(1000.0, 719.0)),
                "{d:?}：下面不動"
            );
        }
        for d in [D::South, D::SouthWest, D::SouthEast] {
            assert_eq!(snap_position(pos, inner, snapped, Some(d)), None, "{d:?}：上面不動");
        }
        // 高度不變、寬度從 600 改成 498（窄了 102）
        let (inner, snapped) = (vec2(600.0, 280.0), vec2(498.0, 280.0));
        for d in [D::West, D::NorthWest, D::SouthWest, D::North, D::South] {
            assert_eq!(
                snap_position(pos, inner, snapped, Some(d)),
                Some(pos2(1102.0, 500.0)),
                "{d:?}：右邊不動"
            );
        }
        for d in [D::East, D::NorthEast, D::SouthEast] {
            assert_eq!(snap_position(pos, inner, snapped, Some(d)), None, "{d:?}：左邊不動");
        }
        // 預設放在右下角的 480×270 拉左邊到 640 寬：高度變 360，往上長（不跑到工作區下面）
        let pos = pos2(1416.0, 746.0);
        assert_eq!(
            snap_position(pos, vec2(640.0, 270.0), vec2(640.0, 360.0), Some(D::West)),
            Some(pos2(1416.0, 656.0))
        );
        // 拉上面到 360 高：寬度變 640，往左長（不跑到螢幕外面）
        assert_eq!(
            snap_position(pos, vec2(480.0, 360.0), vec2(640.0, 360.0), Some(D::North)),
            Some(pos2(1256.0, 746.0))
        );
        assert_eq!(
            snap_position(pos, inner, snapped, None),
            None,
            "不是拉邊緣改的（Aero Snap）"
        );
    }

    #[test]
    fn resize_zone_edges_and_corners() {
        use ResizeDirection as D;
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(480.0, 270.0));
        let at = |x: f32, y: f32| resize_zone(pos2(x, y), r, RESIZE_BAND);
        assert_eq!(at(240.0, 135.0), None, "中間是拖曳移動");
        assert_eq!(at(2.0, 135.0), Some(D::West));
        assert_eq!(at(478.0, 135.0), Some(D::East));
        assert_eq!(at(240.0, 1.0), Some(D::North));
        assert_eq!(at(240.0, 268.0), Some(D::South));
        assert_eq!(at(3.0, 3.0), Some(D::NorthWest));
        assert_eq!(at(477.0, 2.0), Some(D::NorthEast));
        assert_eq!(at(2.0, 266.0), Some(D::SouthWest));
        assert_eq!(at(479.0, 269.0), Some(D::SouthEast));
        assert_eq!(at(8.0, 135.0), None, "8 點以外");
        assert_eq!(at(-1.0, 135.0), None, "視窗外面");
        // 很小的視窗：邊緣最多是寬、高的 ¼
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(24.0, 16.0));
        // 24 寬：左右各 6 點；16 高：上下各 4 點（8 點的邊緣會把中間吃掉）
        assert_eq!(resize_zone(pos2(7.0, 8.0), tiny, RESIZE_BAND), None);
        assert_eq!(resize_zone(pos2(5.0, 3.0), tiny, RESIZE_BAND), Some(D::NorthWest));
        // 游標
        assert_eq!(resize_cursor(D::West), CursorIcon::ResizeHorizontal);
        assert_eq!(resize_cursor(D::South), CursorIcon::ResizeVertical);
        assert_eq!(resize_cursor(D::SouthEast), CursorIcon::ResizeNwSe);
        assert_eq!(resize_cursor(D::NorthEast), CursorIcon::ResizeNeSw);
    }

    /// 大小停下來 150 毫秒才改回比例；還在變就一直等；程式自己送的（比例對的）大小不改
    #[test]
    fn snap_debounce_waits_for_the_size_to_settle() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let a = 16.0 / 9.0;
        let mut d = SnapDebounce::new(t0, vec2(480.0, 270.0));
        assert_eq!(d.tick(ms(0), vec2(480.0, 270.0), a, PIP_MIN, NO_MAX), None);
        assert_eq!(
            d.tick(ms(500), vec2(480.0, 270.0), a, PIP_MIN, NO_MAX),
            None,
            "沒有改過"
        );
        // 使用者在拉
        assert_eq!(d.tick(ms(510), vec2(520.0, 280.0), a, PIP_MIN, NO_MAX), None);
        assert_eq!(d.tick(ms(600), vec2(560.0, 290.0), a, PIP_MIN, NO_MAX), None);
        assert_eq!(
            d.tick(ms(749), vec2(560.0, 290.0), a, PIP_MIN, NO_MAX),
            None,
            "停下來不到 150 毫秒"
        );
        assert_eq!(
            d.tick(ms(750), vec2(560.0, 290.0), a, PIP_MIN, NO_MAX),
            Some(vec2(560.0, 315.0))
        );
        // 改回來之後（視窗照著改了）不再改
        assert_eq!(d.tick(ms(760), vec2(560.0, 315.0), a, PIP_MIN, NO_MAX), None);
        assert_eq!(d.tick(ms(1000), vec2(560.0, 315.0), a, PIP_MIN, NO_MAX), None);
        // 換了影片、程式送了比例對的大小：不改
        let four_three = 4.0 / 3.0;
        assert_eq!(d.tick(ms(1100), vec2(560.0, 420.0), four_three, PIP_MIN, NO_MAX), None);
        assert_eq!(d.tick(ms(1300), vec2(560.0, 420.0), four_three, PIP_MIN, NO_MAX), None);
        // 不看滑鼠按鍵（放開可能永遠收不到）：只看大小停下來
        assert_eq!(
            d.tick(ms(1400), vec2(700.0, 420.0), four_three, PIP_MIN, NO_MAX),
            None,
            "剛改"
        );
        assert_eq!(
            d.tick(ms(1550), vec2(700.0, 420.0), four_three, PIP_MIN, NO_MAX),
            Some(vec2(700.0, 525.0))
        );
        // 視窗還沒照改（X11 的視窗管理員慢一點）：同一個大小不再要求（不會再送另一個、互相打架）
        for n in [1560, 1600, 2000, 5000] {
            assert_eq!(
                d.tick(ms(n), vec2(700.0, 420.0), four_three, PIP_MIN, NO_MAX),
                None,
                "{n}"
            );
        }
        assert!(!d.waiting());
        // 照改了：比例對，不再改
        assert_eq!(d.tick(ms(5010), vec2(700.0, 525.0), four_three, PIP_MIN, NO_MAX), None);
        assert_eq!(d.tick(ms(5500), vec2(700.0, 525.0), four_three, PIP_MIN, NO_MAX), None);
        // 視窗管理員不肯改（並排的視窗一直是 960×1040）：只要求一次，之後不再送
        let mut d = SnapDebounce::new(t0, vec2(480.0, 270.0));
        let tiled = vec2(960.0, 1040.0);
        let mut asked = Vec::new();
        for n in (0..3000).step_by(16) {
            asked.extend(d.tick(ms(n), tiled, a, PIP_MIN, vec2(1728.0, 972.0)));
        }
        assert_eq!(asked, [vec2(1728.0, 972.0)], "只要求一次");
        assert!(!d.waiting(), "不再等（不會一直叫醒介面）");
    }

    fn moved(x: f32, y: f32) -> egui::Event {
        egui::Event::PointerMoved(pos2(x, y))
    }

    fn release(x: f32, y: f32) -> egui::Event {
        egui::Event::PointerButton {
            pos: pos2(x, y),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// 系統的拖曳結束後沒送放開（X11、macOS）：視窗停下來之後的第一個滑鼠移動、或焦點變了，補一個放開；
    /// 真的放開了（Windows）、或 egui 本來就不覺得按著：不補（P-A3）
    #[test]
    fn os_drag_injects_the_missing_release() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let r = |x: f32| Some(Rect::from_min_size(pos2(x, 100.0), vec2(480.0, 270.0)));
        let p = Some(pos2(50.0, 50.0));
        let mut d = OsDrag::new(t0, r(0.0));
        // 拖曳中：視窗在動，收不到滑鼠移動
        assert_eq!(d.check(ms(16), r(10.0), &[], true, p), DragEnd::Going);
        assert_eq!(d.check(ms(100), r(40.0), &[], true, p), DragEnd::Going);
        // 視窗剛停（還不到 150 毫秒）：移動不算
        assert_eq!(d.check(ms(200), r(40.0), &[moved(60.0, 60.0)], true, p), DragEnd::Going);
        // 停了 150 毫秒以上之後的第一個移動：在它前面補放開
        let events = [egui::Event::Text("x".into()), moved(61.0, 61.0)];
        assert_eq!(
            d.check(ms(260), r(40.0), &events, true, p),
            DragEnd::Inject {
                at: 1,
                pos: pos2(61.0, 61.0)
            }
        );
        // 真的放開了（Windows）：不補
        let mut d = OsDrag::new(t0, r(0.0));
        assert_eq!(
            d.check(ms(400), r(40.0), &[moved(1.0, 1.0), release(1.0, 1.0)], true, p),
            DragEnd::Released
        );
        // egui 本來就不覺得按著：不補
        let mut d = OsDrag::new(t0, r(0.0));
        assert_eq!(
            d.check(ms(400), r(0.0), &[moved(1.0, 1.0)], false, p),
            DragEnd::Released
        );
        // 焦點變了：補（在那個事件前面，用上一幀的滑鼠位置）
        let mut d = OsDrag::new(t0, r(0.0));
        assert_eq!(
            d.check(ms(20), r(5.0), &[egui::Event::WindowFocused(true)], true, p),
            DragEnd::Inject {
                at: 0,
                pos: pos2(50.0, 50.0)
            }
        );
        // 不知道滑鼠在哪（離開過視窗）也一定補：用這一批後面的移動，沒有的話用左上角
        let mut d = OsDrag::new(t0, r(0.0));
        assert_eq!(
            d.check(
                ms(20),
                r(5.0),
                &[egui::Event::WindowFocused(true), moved(7.0, 8.0)],
                true,
                None
            ),
            DragEnd::Inject {
                at: 0,
                pos: pos2(7.0, 8.0)
            }
        );
        let mut d = OsDrag::new(t0, r(0.0));
        assert_eq!(
            d.check(ms(20), r(5.0), &[egui::Event::WindowFocused(false)], true, None),
            DragEnd::Inject { at: 0, pos: Pos2::ZERO }
        );
        // 視窗一直沒動（拖曳被系統忽略）：開始 150 毫秒後的移動就補
        let mut d = OsDrag::new(t0, r(0.0));
        assert_eq!(d.check(ms(100), r(0.0), &[moved(1.0, 1.0)], true, p), DragEnd::Going);
        assert!(matches!(
            d.check(ms(150), r(0.0), &[moved(1.0, 1.0)], true, p),
            DragEnd::Inject { .. }
        ));
    }

    #[test]
    fn test_window_mode_hook_parsing() {
        let s = Duration::from_secs_f64;
        assert_eq!(
            parse_test_modes("pip@3,normal@9"),
            vec![(WindowMode::Pip, s(3.0)), (WindowMode::Normal, s(9.0))]
        );
        assert_eq!(
            parse_test_modes(" normal@9 , mini@1.5,pip@x, tv@2, pip@-1,pip"),
            vec![(WindowMode::Mini, s(1.5)), (WindowMode::Normal, s(9.0))]
        );
        assert!(parse_test_modes("").is_empty());
    }

    #[test]
    fn compact_geometry_per_mode() {
        let g = WindowGeometry {
            pos: [10.0, 20.0],
            size: [480.0, 270.0],
            maximized: false,
        };
        let mut c = CompactWindows::default();
        assert_eq!(c.pip, None, "子母畫面預設放在右下角");
        c.remember(WindowMode::Pip, g);
        c.remember(WindowMode::Normal, g);
        assert_eq!(c.remembered(WindowMode::Pip), Some(g));
        assert_eq!(c.remembered(WindowMode::Mini), None, "各記各的");
        assert_eq!(c.remembered(WindowMode::Normal), None);
        // 太小的拉到子母畫面的最小值（不是迷你播放器的）
        let c = CompactWindows {
            pip: Some(WindowGeometry { size: [50.0, 9.0], ..g }),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(c.pip.unwrap().size, [PIP_MIN.x, PIP_MIN.y]);
        let c = CompactWindows {
            pip: Some(WindowGeometry {
                pos: [f32::NAN, 0.0],
                ..g
            }),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(c.pip, None);
    }

    #[test]
    fn entering_from_a_normal_window_applies_at_once_and_settles_on_the_change() {
        let t0 = Instant::now();
        let normal = rect(960.0, 600.0);
        let (mut sw, stage) = Switch::start(WindowMode::Mini, t0, &seen(normal, false, false));
        assert_eq!(stage, Stage::Resize, "一般視窗：馬上送");
        let timing = Timing::of(false);
        assert_eq!(sw.tick(t0, &seen(normal, false, false), timing), Next::Wait);
        // 視窗變了：好了
        assert_eq!(sw.tick(t0, &seen(rect(480.0, 308.0), false, false), timing), Next::Done);
        // 一直沒變（指令被忽略）：最多等 0.6 秒
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(normal, false, false));
        let later = t0 + Duration::from_millis(599);
        assert_eq!(sw.tick(later, &seen(normal, false, false), timing), Next::Wait);
        assert_eq!(
            sw.tick(t0 + Duration::from_millis(600), &seen(normal, false, false), timing),
            Next::Done
        );
    }

    #[test]
    fn entering_from_maximized_waits_for_the_restored_rect() {
        let t0 = Instant::now();
        let max = rect(1920.0, 1000.0);
        let (mut sw, stage) = Switch::start(WindowMode::Mini, t0, &seen(max, false, true));
        assert_eq!(stage, Stage::Unmaximize);
        let timing = Timing::of(false);
        // 還是最大化
        assert_eq!(sw.tick(t0, &seen(max, false, true), timing), Next::Wait);
        // 不是最大化了，但大小還沒給（Windows 晚一兩幀）：再等
        assert_eq!(sw.tick(t0, &seen(max, false, false), timing), Next::Wait);
        // 大小變了：送迷你播放器的指令
        let restored = rect(960.0, 600.0);
        assert_eq!(sw.tick(t0, &seen(restored, false, false), timing), Next::Apply);
        assert_eq!(sw.stage(), Stage::Resize);
        assert_eq!(sw.tick(t0, &seen(restored, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(t0, &seen(rect(480.0, 308.0), false, false), timing), Next::Done);
        // 系統一直不取消最大化：一秒後照樣送（InnerSize 也會取消最大化）
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(max, false, true));
        assert_eq!(
            sw.tick(t0 + Duration::from_millis(999), &seen(max, false, true), timing),
            Next::Wait
        );
        assert_eq!(
            sw.tick(t0 + UNMAXIMIZE_CAP, &seen(max, false, true), timing),
            Next::Apply
        );
    }

    #[test]
    fn entering_from_fullscreen_waits_for_the_exit() {
        let t0 = Instant::now();
        let full = rect(1920.0, 1080.0);
        let (mut sw, stage) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(stage, Stage::LeaveFullscreen);
        let timing = Timing::of(false);
        assert_eq!(sw.tick(t0, &seen(full, true, false), timing), Next::Wait);
        // Windows / Linux：全螢幕一結束就送
        assert_eq!(sw.tick(t0, &seen(full, false, false), timing), Next::Apply);
        // 離開全螢幕後是最大化的（全螢幕之前是最大化）：先取消最大化
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(sw.tick(t0, &seen(full, false, true), timing), Next::Unmaximize);
        assert_eq!(sw.stage(), Stage::Unmaximize);
        // 系統一直沒有回報離開全螢幕：最多等 3 秒
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(
            sw.tick(t0 + FULLSCREEN_CAP, &seen(full, true, false), timing),
            Next::Apply
        );
    }

    /// macOS：`fullscreen` 在動畫開始前就變成 false（winit 先改記錄），動畫結束時 AppKit 才改回原本的大小：
    /// 要等位置大小穩定 300 毫秒，最多 1.2 秒（P-A2）
    #[test]
    fn macos_waits_for_the_fullscreen_animation() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let full = rect(1920.0, 1080.0);
        let timing = Timing::of(true);
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        // 下一幀就說不是全螢幕了（動畫才開始）
        assert_eq!(sw.tick(ms(16), &seen(full, false, false), timing), Next::Wait);
        // 動畫中大小一直在變
        assert_eq!(
            sw.tick(ms(200), &seen(rect(1500.0, 900.0), false, false), timing),
            Next::Wait
        );
        assert_eq!(
            sw.tick(ms(500), &seen(rect(960.0, 600.0), false, false), timing),
            Next::Wait
        );
        // 停下來不到 300 毫秒
        assert_eq!(
            sw.tick(ms(700), &seen(rect(960.0, 600.0), false, false), timing),
            Next::Wait
        );
        // 穩定 300 毫秒：送
        assert_eq!(
            sw.tick(ms(800), &seen(rect(960.0, 600.0), false, false), timing),
            Next::Apply
        );
        // 一直在動：最多等 1.2 秒（從看到全螢幕結束起算）
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(sw.tick(ms(10), &seen(full, false, false), timing), Next::Wait);
        let mut next = Next::Wait;
        let mut t = 10;
        while next == Next::Wait && t < 3000 {
            t += 100;
            next = sw.tick(ms(t), &seen(rect(t as f32, 600.0), false, false), timing);
        }
        assert_eq!(next, Next::Apply);
        assert!((1200..=1400).contains(&t), "最多等約 1.2 秒：{t}");
        // 迷你播放器的大小送出後（回到一般視窗再全螢幕也是）：一樣要穩定 300 毫秒
        let mut sw = Switch::resizing(WindowMode::Normal, t0, rect(480.0, 308.0));
        assert_eq!(
            sw.tick(ms(100), &seen(rect(960.0, 600.0), false, false), timing),
            Next::Wait
        );
        assert_eq!(
            sw.tick(ms(399), &seen(rect(960.0, 600.0), false, false), timing),
            Next::Wait
        );
        assert_eq!(
            sw.tick(ms(400), &seen(rect(960.0, 600.0), false, false), timing),
            Next::Done
        );
    }

    /// macOS、程式自己要求離開全螢幕：`fullscreen` 馬上變 false，大小在動畫結束時才變。
    /// 大小一直是全螢幕的大小時不能因為「穩定 300 毫秒」就送（動畫中途送的會被蓋掉）；變了之後再穩定 300 毫秒
    #[test]
    fn macos_requested_exit_waits_for_the_frame_to_change() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let full = rect(1920.0, 1080.0);
        let restored = rect(960.0, 600.0);
        let timing = Timing::of(true);
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(sw.tick(ms(16), &seen(full, false, false), timing), Next::Wait);
        // 動畫中：回報的還是全螢幕的大小，超過 300 毫秒也還在等
        assert_eq!(sw.tick(ms(400), &seen(full, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(ms(800), &seen(full, false, false), timing), Next::Wait);
        // 動畫結束，改回全螢幕之前的大小：再穩定 300 毫秒
        assert_eq!(sw.tick(ms(900), &seen(restored, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(ms(1199), &seen(restored, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(ms(1200), &seen(restored, false, false), timing), Next::Apply);
        // 大小一直沒變（系統沒有回報）：最多等 1.2 秒（從看到全螢幕結束起算）
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(sw.tick(ms(16), &seen(full, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(ms(1215), &seen(full, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(ms(1216), &seen(full, false, false), timing), Next::Apply);
        // 使用者自己結束的全螢幕（回報時動畫已經結束）：不用等變，穩定 300 毫秒就好
        let mut sw = Switch::after_system(WindowMode::Normal, ms(0), full);
        assert_eq!(sw.tick(ms(0), &seen(full, false, false), timing), Next::Wait);
        assert_eq!(sw.tick(ms(300), &seen(full, false, false), timing), Next::Apply);
        // Windows / Linux：照舊，全螢幕一結束就送
        let (mut sw, _) = Switch::start(WindowMode::Mini, t0, &seen(full, true, false));
        assert_eq!(
            sw.tick(ms(16), &seen(full, false, false), Timing::of(false)),
            Next::Apply
        );
    }

    /// 換了大小時右下角不動，超出所在的螢幕時往上、往左移回來（第二個螢幕也一樣）
    #[test]
    fn anchored_keeps_the_bottom_right_corner_on_the_monitor() {
        let areas = [
            area(0.0, 0.0, 1920.0, 1040.0, 1.0),
            area(1920.0, 0.0, 1920.0, 1040.0, 1.0),
        ];
        let frame = vec2(16.0, 39.0);
        let old = vec2(480.0, 308.0);
        // 預設的位置（右下角）：變高往上長、變矮往下縮，右下角一樣
        let pos = pos2(1920.0 - 496.0 - 24.0, 1040.0 - 347.0 - 24.0);
        let taller = vec2(480.0, 398.0);
        let p = anchored(pos, old, taller, frame, &areas, 0.0, 1.0).unwrap();
        assert_eq!(p, pos2(pos.x, pos.y - 90.0));
        let p = anchored(pos, old, vec2(480.0, 238.0), frame, &areas, 0.0, 1.0).unwrap();
        assert_eq!(p, pos2(pos.x, pos.y + 70.0));
        // 第二個螢幕：貼著它的右下角時，變寬變高也不會跑出它的範圍
        let pos = pos2(3840.0 - 496.0, 1040.0 - 347.0);
        let p = anchored(pos, old, vec2(640.0, 398.0), frame, &areas, 0.0, 1.0).unwrap();
        assert_eq!(p, pos2(3840.0 - 656.0, 1040.0 - 437.0));
        // 使用者拉到螢幕下面一半：移回螢幕裡（Dock / 工作列多留）
        let low = pos2(2500.0, 900.0);
        let p = anchored(low, old, old, frame, &areas, TASKBAR_ALLOWANCE, 1.0).unwrap();
        assert_eq!(p, pos2(2500.0, 1040.0 - TASKBAR_ALLOWANCE - 347.0));
        // 比螢幕還大：對齊左上角
        let p = anchored(pos2(100.0, 100.0), old, vec2(2000.0, 1200.0), frame, &areas, 0.0, 1.0).unwrap();
        assert_eq!(p, pos2(0.0, 0.0));
        // 沒有螢幕資訊（Wayland）
        assert_eq!(anchored(pos, old, taller, frame, &[], 0.0, 1.0), None);
    }

    #[test]
    fn leaving_settles_when_the_window_is_restored() {
        let t0 = Instant::now();
        let mini = WinRect {
            size: vec2(480.0, 308.0),
            pos: Some(pos2(1400.0, 700.0)),
        };
        let timing = Timing::of(false);
        let mut sw = Switch::resizing(WindowMode::Normal, t0, mini);
        assert_eq!(sw.to, WindowMode::Normal);
        assert_eq!(sw.tick(t0, &seen(mini, false, false), timing), Next::Wait);
        // 只移動了位置也算變了
        let moved = WinRect {
            pos: Some(pos2(100.0, 80.0)),
            ..mini
        };
        assert_eq!(sw.tick(t0, &seen(moved, false, false), timing), Next::Done);
    }
}
