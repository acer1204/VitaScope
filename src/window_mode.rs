//! 視窗模式：一般視窗、迷你播放器（子母畫面在 F2 加進來）。
//!
//! 只有一個視窗：mpv 畫在主視窗的 GL context 裡，流暢播放也只看一個 swap chain，
//! 所以迷你播放器是主視窗的一種模式（跟 PotPlayer 的迷你模式一樣），不是另開一個視窗。
//!
//! 這裡是純邏輯（不碰視窗、可以在每個平台測）：
//! - 迷你播放器的大小、預設位置（目前螢幕的右下角）、記住的位置還能不能用（螢幕可能拔掉了）；
//! - 進入 / 離開時要送給視窗的指令（層級不在這裡：一律由 app 的 `on_top_now` 決定，只有一個來源）；
//! - 切換時要等什麼（[`Switch`]）：先離開全螢幕、取消最大化，送出新的大小之後等視窗真的變了。
//!   macOS 離開 / 進入全螢幕有約一秒的動畫，動畫結束時 AppKit 會把視窗改回動畫前的大小，
//!   中途送的大小、位置會被蓋掉，所以 macOS 要等位置大小穩定下來（P-A2）。

use crate::screens::Area;
use crate::settings::WindowGeometry;
use eframe::egui::{Pos2, Rect, Vec2, ViewportCommand, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 視窗目前的模式（不存檔：下次啟動一律是一般視窗，跟不記全螢幕一樣）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Normal,
    /// 迷你播放器：小視窗（保留標題列，系統的移動、改大小、貼齊照常）、預設置頂、影片下面一列精簡的控制列
    Mini,
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
/// 預設位置離螢幕右邊、下邊的距離
const MARGIN: f32 = 24.0;
/// macOS（Dock）、X11（工作列）的螢幕範圍是整個螢幕，下面多留這麼多；Windows 的是扣掉工作列的工作區
pub const TASKBAR_ALLOWANCE: f32 = 48.0;

/// 迷你播放器（之後還有子母畫面）的設定（`settings.json` 的 `compact`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactWindows {
    /// 迷你播放器置頂（預設開）。關掉時迷你播放器照「視窗置頂」的模式（不置頂 / 永遠 / 播放時）
    pub mini_on_top: bool,
    /// 迷你播放器上次的位置、大小（寬度沿用，高度依影片比例重算）。用之前先確認還在螢幕上（P-C6）
    pub mini: Option<WindowGeometry>,
}

impl Default for CompactWindows {
    fn default() -> Self {
        Self {
            mini_on_top: true,
            mini: None,
        }
    }
}

impl CompactWindows {
    /// 讀檔後整理：不是數字的拿掉，大小至少是最小值（手動改過、或螢幕設定很怪時存下的）
    pub fn sanitized(mut self) -> Self {
        self.mini = self.mini.and_then(|g| sane_geometry(g, MINI_MIN));
        self
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

/// 進入迷你播放器：最小大小先放寬（不然縮不下去）、新的大小、位置（`pos` 是 None 時交給系統）。
/// 視窗層級另外送（app 的 `on_top_now`，只有一個來源）
pub fn plan_enter(size: Vec2, pos: Option<Pos2>) -> Vec<ViewportCommand> {
    let mut cmds = vec![
        ViewportCommand::MinInnerSize(MINI_MIN),
        ViewportCommand::InnerSize(size),
    ];
    cmds.extend(pos.map(ViewportCommand::OuterPosition));
    cmds
}

/// 回到一般視窗：最小大小改回來、還原大小與位置（`pos_ok`：位置還在螢幕上），原本最大化的再最大化
pub fn plan_leave(restore: &NormalRestore, pos_ok: bool) -> Vec<ViewportCommand> {
    let mut cmds = vec![
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
        let enter = plan_enter(vec2(480.0, 308.0), Some(pos2(1400.0, 700.0)));
        assert_eq!(
            enter,
            [
                ViewportCommand::MinInnerSize(MINI_MIN),
                ViewportCommand::InnerSize(vec2(480.0, 308.0)),
                ViewportCommand::OuterPosition(pos2(1400.0, 700.0)),
            ]
        );
        // Wayland：不指定位置
        assert_eq!(plan_enter(vec2(480.0, 308.0), None).len(), 2);
        let restore = NormalRestore {
            pos: Some(pos2(100.0, 80.0)),
            size: vec2(960.0, 600.0),
            maximized: true,
        };
        assert_eq!(
            plan_leave(&restore, true),
            [
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
                ViewportCommand::MinInnerSize(NORMAL_MIN),
                ViewportCommand::InnerSize(NORMAL_MIN),
            ]
        );
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
