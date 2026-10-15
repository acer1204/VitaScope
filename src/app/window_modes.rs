//! 迷你播放器（F1）、子母畫面（F2）：進入、離開、切換途中要等的事、精簡的控制列、子母畫面的移動與改大小。
//!
//! - 進入時記下一般視窗的位置大小（要求的那一刻，還沒送出離開全螢幕、取消最大化之前，P-B1），回來時照樣還原
//!   （原本最大化的再最大化）。切換途中不記視窗的位置（[`Self::remember_window`]），也不配合影片調整大小。
//!   迷你播放器、子母畫面之間直接換（一般視窗記的照舊）。
//! - 全螢幕一律經過 [`VitascopeApp::request_fullscreen`]：迷你播放器、子母畫面裡要全螢幕時，先回到一般視窗、
//!   等它還原好才全螢幕（單一執行個體的 `--fullscreen` 也是）。
//! - 系統把迷你播放器、子母畫面變成全螢幕或最大化（macOS 的綠色按鈕、雙擊標題列、Win+↑）時，回到一般模式（P-B2）；
//!   全螢幕 / 最大化結束後還原一般視窗的位置大小。標題列一律加回來。
//! - 視窗層級只有一個來源：`on_top_now`（子母畫面一律置頂；迷你播放器開著「迷你播放器置頂」時置頂，
//!   不然照「視窗置頂」的模式）。
//! - 要大視窗的功能（側邊面板、設定、控制面板、字幕外觀、匯出、關於；子母畫面裡還有媒體資訊）先回到一般視窗；
//!   進入時已經開著的這些視窗先藏起來，回來時照原樣出現。
//! - 系統的媒體控制叫視窗到前面（Raise）、播完後的倒數都不會離開迷你播放器、子母畫面（S-B9；
//!   子母畫面小到放不下倒數畫面時才回到一般視窗）。
//! - 子母畫面（Windows、Linux）：拖曳畫面移動（`StartDrag`）、拉邊緣改大小（`BeginResize`），改完停下來之後照影片的
//!   比例改回來；系統的拖曳結束時沒送「放開」的話在 `raw_input_hook` 補一個（P-A3）。單擊畫面不暫停（只是取得焦點），
//!   雙擊回到一般視窗；控制項在滑鼠移上去時才出現。沒有影像的檔案不能用。
//! - 子母畫面（macOS，F3）：改大小用系統的邊緣，視窗的內容比例鎖在影片的比例（`contentAspectRatio`，拉的時候就保持），
//!   影片的形狀變了（換檔、長寬比、裁切、旋轉）跟著換；Spaces 的行為盡力而為（P-B10、P-C14）。換模式的那一刻就改，
//!   所以回到一般視窗（之後才可能全螢幕）之前已經還原：Spaces 行為是「可以放在別的全螢幕旁邊」的視窗不能自己全螢幕。

use super::{Action, HIDE_AFTER, VitascopeApp, fmt_time, icon_button};
use crate::keymap::Command;
use crate::screens::Area;
use crate::settings::{SideTab, WindowGeometry};
use crate::system_power::AfterPlayback;
use crate::theme::{self, Palette};
use crate::tr;
use crate::window_mode::{
    self, DragEnd, NativePip, NormalRestore, OsDrag, Seen, SnapDebounce, Stage, Switch, Timing, WinRect, WindowMode,
};
use eframe::egui::{self, Align, Color32, CornerRadius, Layout, Rect, ResizeDirection, ViewportCommand, pos2, vec2};
use std::time::{Duration, Instant};

/// 切換做完（視窗真的變好）之後接著做的事
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Then {
    #[default]
    Nothing,
    /// 從迷你播放器要全螢幕：先回到一般視窗，還原好了才全螢幕（macOS 的動畫，P-A2）
    Fullscreen,
    /// 迷你播放器裡按了 F6 / H：回到一般視窗後打開側邊面板的這一頁（面板加寬視窗要用還原後的大小）
    Side(SideTab),
}

/// 迷你播放器、子母畫面裡藏起來的視窗：進入時開著的（設定、控制面板、字幕外觀、匯出、線上搜尋字幕；子母畫面還有媒體資訊）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct BigWindows {
    settings: bool,
    panel: bool,
    sub_style: bool,
    export: bool,
    online_subs: bool,
    about: bool,
    /// 媒體資訊（蓋在影片上）：迷你播放器看得到，子母畫面太小，算大視窗
    info: bool,
}

impl BigWindows {
    /// 這次比 `before` 多開了哪一個
    fn opened_since(&self, before: &BigWindows) -> bool {
        (self.settings && !before.settings)
            || (self.panel && !before.panel)
            || (self.sub_style && !before.sub_style)
            || (self.export && !before.export)
            || (self.online_subs && !before.online_subs)
            || (self.about && !before.about)
            || (self.info && !before.info)
    }
}

/// 視窗模式的狀態
#[derive(Default)]
pub(super) struct ModeUi {
    pub(super) mode: WindowMode,
    /// 進行中的切換（等離開全螢幕、取消最大化、視窗變好）
    switch: Option<Switch>,
    /// 切換做完之後接著做的事
    then: Then,
    /// 一般視窗的位置大小（要求進入的那一刻記下）
    restore: Option<NormalRestore>,
    /// 系統把迷你播放器變成全螢幕 / 最大化了：結束時還原一般視窗（`true` = 是全螢幕，還原後原本最大化的再最大化）
    restore_later: Option<(NormalRestore, bool)>,
    /// 換成這個模式的時間、換之前的視窗位置大小（剛換好時視窗還是舊的大小，不能記成迷你播放器的）
    changed_at: Option<Instant>,
    entered_from: Option<WinRect>,
    /// 進入時開著的大視窗（藏起來）
    hidden: BigWindows,
    /// 這次執行已經提示過 Wayland 不能置頂
    wayland_told: bool,
    /// 滑鼠在迷你控制列的 🔊 上（滾輪調音量）
    pub(super) wheel_on_volume: bool,
    /// 測試用：當成這些螢幕（None = 問作業系統）
    areas: Option<Vec<Area>>,
    /// 有標題列時外框比內容大多少（一般視窗、迷你播放器量的）：從子母畫面（沒有標題列）換到迷你播放器時放位置用
    frame: Option<egui::Vec2>,
    /// 子母畫面：改完大小之後把比例改回影片的
    snap: Option<SnapDebounce>,
    /// 子母畫面：系統的拖曳（移動、改大小）進行中，結束時沒送「放開」的話補一個（P-A3）
    os_drag: Option<OsDrag>,
    /// 子母畫面：最後一次拉邊緣改大小是哪一邊（改回比例時對面的邊不動）
    resize_dir: Option<ResizeDirection>,
    /// 子母畫面：中間的 ▶ / ⏸ 上一次單擊切換暫停的時間（egui 的時間；緊接著雙擊時切回來）
    play_click: Option<f64>,
    /// 從子母畫面換到迷你播放器時還不知道標題列多大（一直是全螢幕、子母畫面）：量到之後重新放位置
    place_after_frame: bool,
    /// 有檔案載入好了（`FileLoaded`）：子母畫面裡要看它有沒有影像。只在載入好的那一刻看：換檔途中 mpv 先送空的軌道清單，
    /// 那時還是「載入了、沒有影像」，不能當成純音訊
    pub(super) file_loaded: bool,
    /// 子母畫面的控制項這一幀有顯示（測試用）
    pub(super) pip_controls: bool,
    /// 子母畫面的控制項蓋住畫面下面多少（佔畫面高度的比例；字幕跟著往上推）
    pub(super) pip_lift: f32,
    /// macOS 子母畫面告訴視窗的內容比例鎖、Spaces 行為（其他系統只記，不碰視窗）
    native: NativePip,
}

impl ModeUi {
    /// `areas`：測試用的螢幕（None = 問作業系統）
    pub(super) fn new(areas: Option<Vec<Area>>) -> Self {
        Self {
            areas,
            ..Default::default()
        }
    }

    /// 切換中（視窗的大小還會變）：這段期間不記視窗的位置、不配合影片調整大小。
    /// 系統的全螢幕 / 最大化還沒結束（等使用者自己結束）不算：那時記視窗、調整大小本來就照全螢幕、最大化的規則
    pub(super) fn switching(&self) -> bool {
        self.switch.is_some()
    }

    /// 正在回到一般視窗（還原的指令送了，視窗還沒變回來）：要用一般視窗大小的事（打開側邊面板、全螢幕）等它做完
    pub(super) fn restoring(&self) -> bool {
        self.switch.is_some_and(|s| s.to == WindowMode::Normal)
    }
}

/// 剛換好模式時，這麼久之內不把視窗的位置大小記成新模式的（指令要一兩幀才生效）
const REMEMBER_AFTER: Duration = Duration::from_millis(600);
/// 記不到位置（Wayland）時存的位置：不在任何螢幕上，下次一定用預設位置
const UNKNOWN_POS: [f32; 2] = [-100_000.0, -100_000.0];

impl VitascopeApp {
    /// 現在是精簡的模式（迷你播放器、子母畫面）
    pub(super) fn compact(&self) -> bool {
        self.modes.mode.is_compact()
    }

    /// 現在是子母畫面
    pub(super) fn pip(&self) -> bool {
        self.modes.mode == WindowMode::Pip
    }

    /// 測試用：子母畫面的控制項（▶ / ⏸、進度、🗖）這一幀有顯示
    #[doc(hidden)]
    pub fn pip_controls_shown(&self) -> bool {
        self.modes.pip_controls
    }

    /// 測試用：macOS 子母畫面告訴視窗的內容比例鎖、Spaces 行為（其他系統一樣記，只是不碰視窗）
    #[doc(hidden)]
    pub fn pip_native(&self) -> NativePip {
        self.modes.native
    }

    /// 測試用：系統的拖曳（子母畫面的移動、改大小）還沒結束
    #[doc(hidden)]
    pub fn os_drag_pending(&self) -> bool {
        self.modes.os_drag.is_some()
    }

    /// 這個檔案不能用子母畫面：已經載入、但沒有影像（純音訊、只有專輯封面）
    pub(super) fn pip_unavailable(&self) -> bool {
        let st = &self.player.state;
        st.loaded && !st.has_video()
    }

    /// 不能用子母畫面的原因（選單的說明、按下快捷鍵時的提示）
    pub(super) fn pip_unavailable_text() -> &'static str {
        tr!(
            "這個檔案沒有影像，不能用子母畫面",
            "This file has no video for picture-in-picture"
        )
    }

    /// 影片的長寬比（寬 / 高）；還不知道、沒有影像時 None
    fn video_aspect(&self) -> Option<f32> {
        let st = &self.player.state;
        st.video_size
            .filter(|[w, h]| *w > 0 && *h > 0 && st.has_video())
            .map(|[w, h]| w as f32 / h as f32)
    }

    /// 測試用：目前的視窗模式
    #[doc(hidden)]
    pub fn window_mode(&self) -> WindowMode {
        self.modes.mode
    }

    /// 測試用：模式切換還沒做完（等離開全螢幕、取消最大化、視窗變好，或等系統的全螢幕 / 最大化結束後還原）
    #[doc(hidden)]
    pub fn window_mode_pending(&self) -> bool {
        self.modes.switching() || self.modes.restore_later.is_some()
    }

    /// 螢幕範圍（迷你播放器的預設位置、記住的位置還在不在螢幕上）；Wayland 沒有
    fn screen_areas(&self) -> Option<Vec<Area>> {
        match &self.modes.areas {
            Some(fake) => (!fake.is_empty()).then(|| fake.clone()),
            None => crate::screens::areas(),
        }
    }

    /// 這一幀視窗的狀態
    fn seen(&self, ctx: &egui::Context) -> Seen {
        let content = ctx.content_rect().size();
        let maximized = self.maximized_now(ctx);
        ctx.input(|i| {
            let v = i.viewport();
            Seen {
                rect: WinRect {
                    size: v.inner_rect.map_or(content, |r| r.size()),
                    pos: v.outer_rect.map(|r| r.min),
                },
                fullscreen: v.fullscreen,
                maximized,
            }
        })
    }

    /// 視窗是不是最大化（macOS：放到最大，zoom）。egui-winit 在 macOS 執行中不更新 `maximized`
    /// （只在建立視窗時、跟程式自己送 `Maximized` 時改，egui#3494），使用者按綠色按鈕、雙擊標題列都看不到，
    /// 回到一般視窗時送的 `Maximized(true)` 也會一直留著：直接問 AppKit。其他系統（與自動測試）照 egui 的
    pub(super) fn maximized_now(&self, ctx: &egui::Context) -> Option<bool> {
        self.owner
            .and_then(|o| crate::screens::window_zoomed(o.window))
            .or_else(|| ctx.input(|i| i.viewport().maximized))
    }

    /// 迷你播放器的位置往下多留的空間：Windows 的螢幕範圍是扣掉工作列的工作區，macOS、X11 是整個螢幕
    fn bottom_allowance() -> f32 {
        if cfg!(windows) {
            0.0
        } else {
            window_mode::TASKBAR_ALLOWANCE
        }
    }

    /// 迷你播放器開 / 關（Ctrl+Shift+M、控制列的 🗗、右鍵選單）。子母畫面裡是換成迷你播放器
    pub(super) fn toggle_mini(&mut self, ctx: &egui::Context) {
        self.toggle_compact(ctx, WindowMode::Mini);
    }

    /// 子母畫面開 / 關（Ctrl+Shift+P、右鍵選單、迷你控制列的 ▣）。迷你播放器裡是換成子母畫面；
    /// 沒有影像的檔案不能用（提示原因）
    pub(super) fn toggle_pip(&mut self, ctx: &egui::Context) {
        let leaving = self.pip() || self.modes.switch.is_some_and(|s| s.to == WindowMode::Pip);
        if !leaving && self.pip_unavailable() {
            self.osd(Self::pip_unavailable_text());
            return;
        }
        self.toggle_compact(ctx, WindowMode::Pip);
    }

    /// 這個模式開 / 關：已經是（或正在進入）就回到一般視窗，不然進入（另一個精簡模式也是直接換過來）
    fn toggle_compact(&mut self, ctx: &egui::Context, mode: WindowMode) {
        let entering = self.modes.switch.is_some_and(|s| s.to == mode);
        if self.modes.mode == mode || entering {
            self.leave_compact(ctx, Then::Nothing);
        } else {
            self.enter_compact(ctx, mode);
        }
    }

    /// 實機測試（`VITASCOPE_TEST_WINDOW_MODE`）：換成這個模式
    pub(super) fn set_window_mode(&mut self, ctx: &egui::Context, mode: WindowMode) {
        if mode == WindowMode::Normal {
            self.leave_compact(ctx, Then::Nothing);
        } else if !(mode == WindowMode::Pip && self.pip_unavailable()) {
            self.enter_compact(ctx, mode);
        }
    }

    /// 進入精簡的模式：記下一般視窗，全螢幕的先離開全螢幕、最大化的先取消最大化（做完才送新的大小）。
    /// 已經是另一個精簡的模式時直接換（一般視窗記的照舊）
    fn enter_compact(&mut self, ctx: &egui::Context, to: WindowMode) {
        if self.modes.mode == to {
            return;
        }
        self.notify_wayland(to);
        // 正在進入另一個精簡的模式（等離開全螢幕、取消最大化）：改成進入這一個
        if !self.compact()
            && let Some(s) = &mut self.modes.switch
            && s.to.is_compact()
        {
            s.to = to;
            return;
        }
        let seen = self.seen(ctx);
        // 正在回到一般視窗（還沒還原好，視窗可能還是迷你播放器的大小）、或從另一個精簡的模式換過來：
        // 用原本記下的一般視窗
        let leaving = self.modes.switch.is_some_and(|s| s.to == WindowMode::Normal);
        let restore = match self.modes.restore {
            Some(r) if leaving || self.compact() => r,
            _ => self.normal_now(ctx, &seen),
        };
        self.modes.restore = Some(restore);
        self.modes.then = Then::Nothing;
        // 系統的最大化（從迷你播放器變的）還沒結束就又進入：那個還原不要了（不然進入後又被改回一般視窗的大小）
        self.modes.restore_later = None;
        let (switch, stage) = Switch::start(to, Instant::now(), &seen);
        self.modes.switch = Some(switch);
        match stage {
            Stage::LeaveFullscreen => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false)),
            Stage::Unmaximize => ctx.send_viewport_cmd(ViewportCommand::Maximized(false)),
            Stage::Resize => self.apply_compact(ctx, to),
        }
        // 正在錄的按鍵、「網路 → 進階」打到一半的文字：設定視窗要藏起來了
        self.keys_ui.close();
        self.commit_net_draft();
        ctx.request_repaint();
    }

    /// Wayland 不能置頂：要置頂的模式（子母畫面、開著「迷你播放器置頂」的迷你播放器）這次執行只提示一次
    fn notify_wayland(&mut self, to: WindowMode) {
        let on_top = to == WindowMode::Pip || self.settings.compact.mini_on_top;
        if self.wayland && on_top && !std::mem::replace(&mut self.modes.wayland_told, true) {
            self.osd(tr!(
                "這個桌面環境（Wayland）不讓程式自己置頂，視窗可能被其他視窗蓋住",
                "This desktop (Wayland) doesn't let programs keep themselves on top; the window may be covered"
            ));
        }
    }

    /// 一般視窗現在的位置大小（要求進入的那一刻）。全螢幕、最大化時用記下的一般視窗（`settings.window`
    /// 在全螢幕時不更新、最大化時只記旗標），所以還原後是原本的大小，最大化的再最大化
    fn normal_now(&self, ctx: &egui::Context, seen: &Seen) -> NormalRestore {
        let saved = self.settings.window.as_ref().map(NormalRestore::from_geometry);
        let current = || NormalRestore {
            pos: seen.rect.pos,
            size: seen.rect.size,
            maximized: false,
        };
        let restore = match (seen.fullscreen == Some(true), seen.maximized == Some(true)) {
            (true, _) => saved,
            (false, true) => saved.map(|r| NormalRestore { maximized: true, ..r }),
            (false, false) => None,
        };
        restore.unwrap_or_else(|| {
            let mut r = current();
            r.maximized = seen.maximized == Some(true) && seen.fullscreen != Some(true);
            if r.size.x < 1.0 || r.size.y < 1.0 {
                r.size = ctx.content_rect().size();
            }
            r
        })
    }

    /// 送出進入迷你播放器、子母畫面的指令（大小、位置、標題列；層級照 `on_top_now`），換成這個模式
    fn apply_compact(&mut self, ctx: &egui::Context, to: WindowMode) {
        let from = self.modes.mode;
        let (outer, inner, monitor, scale) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.monitor_size,
                v.native_pixels_per_point.unwrap_or(i.pixels_per_point),
            )
        });
        let aspect = self.video_aspect();
        let remembered = self.settings.compact.remembered(to);
        let size = match to {
            WindowMode::Pip => window_mode::pip_size(aspect, remembered.map(|g| g.size[0]), monitor),
            _ => {
                let width = remembered.map_or(window_mode::MINI_WIDTH, |g| g.size[0]);
                window_mode::mini_size(aspect, width, monitor)
            }
        };
        let areas = self.screen_areas();
        // 子母畫面沒有標題列、邊框（外框就是內容）；從子母畫面換到迷你播放器時現在量不到，用之前量的
        let measured = match (outer, inner) {
            (Some(o), Some(i)) => (o.size() - i.size()).max(egui::Vec2::ZERO),
            _ => egui::Vec2::ZERO,
        };
        let frame = match to {
            WindowMode::Pip => egui::Vec2::ZERO,
            _ if from == WindowMode::Pip => self.modes.frame.unwrap_or(measured),
            _ => measured,
        };
        // 還沒量過標題列（一開始就全螢幕，直接進入子母畫面）：先照沒有標題列放，加回來之後量到了再放一次
        //（不然視窗下面多出標題列的高度，跑到工作列下面）
        self.modes.place_after_frame = to == WindowMode::Mini && from == WindowMode::Pip && self.modes.frame.is_none();
        // 上次的位置還在某個螢幕上才用（螢幕可能拔掉了，P-C6）：高度照這次的影片重算，所以右下角對齊上次的，
        // 整個視窗放回它所在的螢幕裡（上次 16:9 放在右下角、這次是直式的，控制列不會跑到螢幕外面）；
        // 不然放在目前螢幕的右下角
        let allowance = Self::bottom_allowance();
        let pos = areas.as_deref().and_then(|a| match remembered {
            Some(g) if window_mode::position_usable(&g, Some(a)) => window_mode::anchored(
                pos2(g.pos[0], g.pos[1]),
                vec2(g.size[0], g.size[1]),
                size,
                frame,
                a,
                allowance,
                scale,
            ),
            _ => window_mode::placement(outer, frame, size, a, allowance, scale),
        });
        for cmd in window_mode::plan_enter(to, size, pos) {
            ctx.send_viewport_cmd(cmd);
        }
        let now = Instant::now();
        self.modes.entered_from = Some(self.seen(ctx).rect);
        self.modes.mode = to;
        self.modes.changed_at = Some(now);
        // 子母畫面：送出的大小比例是對的（視窗照著改了就不用再改）
        self.modes.snap = (to == WindowMode::Pip).then(|| {
            let mut snap = SnapDebounce::new(now, size);
            // 視窗還是舊的大小：這一幀先記下（下一幀變成新的大小時照常比較）
            let _ = snap.tick(
                now,
                self.seen(ctx).rect.size,
                aspect.unwrap_or(16.0 / 9.0),
                to.min_size(),
                window_mode::pip_max(monitor),
            );
            snap
        });
        self.modes.resize_dir = None;
        // 進入時開著的大視窗先藏起來（回來時照原樣出現）；「關於」是對話框（開著時按鍵都給它），直接關掉
        self.about_open = false;
        self.modes.hidden = self.big_windows();
        // 側邊面板的書籤改到一半：跟面板關掉一樣，改好
        self.finish_hidden_rename();
        self.apply_window_level(ctx);
        eprintln!(
            "[vitascope] 視窗模式：{} → {}（{:.0}×{:.0}）",
            from.log_name(),
            to.log_name(),
            size.x,
            size.y
        );
        // macOS：子母畫面的比例鎖、Spaces 行為（從子母畫面換到迷你播放器時解除、還原）
        self.pip_native_tick(ctx);
    }

    /// 回到一般視窗（`then`：還原好之後接著做的事）。還在等進入的（離開全螢幕、取消最大化）就取消
    pub(super) fn leave_compact(&mut self, ctx: &egui::Context, then: Then) {
        // 已經在回到一般視窗：還原好了再做（最後一次要的為準）
        if self.modes.restoring() {
            self.modes.then = then;
            return;
        }
        if !self.compact() {
            if self.modes.switch.take().is_some()
                && let Some(restore) = self.modes.restore.take()
            {
                // 進入途中改變主意：已經離開全螢幕、取消最大化的話照原本的還原（最大化的再最大化）
                let pos_ok = self.restore_pos_ok(&restore);
                for cmd in window_mode::plan_leave(&restore, pos_ok) {
                    ctx.send_viewport_cmd(cmd);
                }
            }
            self.modes.then = Then::Nothing;
            self.run_then(ctx, then);
            return;
        }
        let from = self.seen(ctx).rect;
        // 還原好之前（`Switch` 做完前）留著：這段期間又要進入迷你播放器時，記的還是原本的一般視窗
        let restore = self.modes.restore.unwrap_or(NormalRestore {
            pos: None,
            size: window_mode::NORMAL_MIN,
            maximized: false,
        });
        let pos_ok = self.restore_pos_ok(&restore);
        for cmd in window_mode::plan_leave(&restore, pos_ok) {
            ctx.send_viewport_cmd(cmd);
        }
        let was = std::mem::replace(&mut self.modes.mode, WindowMode::Normal);
        self.modes.changed_at = Some(Instant::now());
        self.modes.then = then;
        self.modes.switch = Some(Switch::resizing(WindowMode::Normal, Instant::now(), from));
        self.modes.snap = None;
        self.modes.resize_dir = None;
        self.apply_window_level(ctx);
        eprintln!(
            "[vitascope] 視窗模式：{} → 一般視窗（{:.0}×{:.0}{}）",
            was.log_name(),
            restore.size.x,
            restore.size.y,
            if restore.maximized { "，最大化" } else { "" }
        );
        // macOS：比例鎖、Spaces 行為馬上還原（之後可能接著全螢幕，P-B10）
        self.pip_native_tick(ctx);
        ctx.request_repaint();
    }

    /// 一般視窗原本的位置還在某個螢幕上（迷你播放器開著時螢幕可能拔掉了）；不知道有哪些螢幕時照送
    /// （Wayland 本來就不能指定位置）
    fn restore_pos_ok(&self, restore: &NormalRestore) -> bool {
        restore.pos.is_some_and(|p| match self.screen_areas() {
            Some(areas) => crate::screens::title_bar_visible([p.x, p.y], restore.size.x, &areas),
            None => true,
        })
    }

    /// 切換做完之後接著做的事
    fn run_then(&mut self, ctx: &egui::Context, then: Then) {
        match then {
            Then::Nothing => {}
            Then::Fullscreen => self.fullscreen_now(ctx),
            // 面板已經開在這一頁（例如等的時候又按了一次）就不動，不然會關掉
            Then::Side(tab) if self.settings.show_playlist && self.settings.side_tab == tab => {}
            Then::Side(tab) => self.toggle_side(ctx, tab),
        }
    }

    /// 全螢幕開 / 關（快捷鍵、按鈕、選單、雙擊、`--fullscreen`、單一執行個體送來的都走這裡）。
    /// 迷你播放器裡：先回到一般視窗，還原好了才全螢幕
    pub(super) fn request_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if !on {
            // 還在等回到一般視窗後全螢幕的：不要了
            if self.modes.then == Then::Fullscreen {
                self.modes.then = Then::Nothing;
            }
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            return;
        }
        match self.modes.switch {
            _ if self.compact() => self.leave_compact(ctx, Then::Fullscreen),
            // 正在回到一般視窗：還原好了再全螢幕
            Some(s) if s.to == WindowMode::Normal => self.modes.then = Then::Fullscreen,
            // 正在進入迷你播放器：取消，直接全螢幕
            Some(_) => self.leave_compact(ctx, Then::Fullscreen),
            None => self.fullscreen_now(ctx),
        }
    }

    /// 送出全螢幕。macOS 子母畫面的 Spaces 行為（「可以放在別的全螢幕旁邊」的視窗自己不能全螢幕）一定已經還原：
    /// 離開子母畫面的那一刻就還原了，這裡再確認一次（P-B10）
    fn fullscreen_now(&mut self, ctx: &egui::Context) {
        self.pip_native_tick(ctx);
        ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
    }

    /// 全螢幕的開關要切到哪一邊（已經全螢幕，或正在等回到一般視窗後全螢幕：關）
    pub(super) fn fullscreen_wanted(&self, ctx: &egui::Context) -> bool {
        super::is_fullscreen(ctx) || self.modes.then == Then::Fullscreen
    }

    /// 迷你播放器、子母畫面裡（或正在回到一般視窗時）按了 F6 / H：回到一般視窗，打開側邊面板的這一頁
    /// （面板已經開著的話換到這一頁，回來時就看得到）
    pub(super) fn side_after_leaving(&mut self, ctx: &egui::Context, tab: SideTab) {
        if self.settings.show_playlist {
            self.switch_side_tab(tab);
            self.leave_compact(ctx, Then::Nothing);
        } else {
            self.leave_compact(ctx, Then::Side(tab));
        }
    }

    /// 每一幀（處理完按鍵之後）：切換進行到哪裡；系統把迷你播放器變成全螢幕 / 最大化了沒
    pub(super) fn mode_tick(&mut self, ctx: &egui::Context) {
        let seen = self.seen(ctx);
        let now = Instant::now();
        if let Some(switch) = &mut self.modes.switch {
            match switch.tick(now, &seen, Timing::current()) {
                window_mode::Next::Wait => ctx.request_repaint_after(Duration::from_millis(50)),
                window_mode::Next::Unmaximize => {
                    ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                window_mode::Next::Apply => {
                    let to = switch.to;
                    if to.is_compact() {
                        self.apply_compact(ctx, to);
                    } else {
                        self.apply_restore_later(ctx);
                    }
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                window_mode::Next::Done => {
                    if self.modes.switch.take().is_some_and(|s| s.to == WindowMode::Normal) {
                        self.modes.restore = None;
                    }
                    let then = std::mem::take(&mut self.modes.then);
                    self.run_then(ctx, then);
                    // 切換途中載入的新檔案：現在才配合它調整視窗
                    ctx.request_repaint();
                }
            }
            return;
        }
        // 系統把迷你播放器、子母畫面變成全螢幕或最大化（綠色按鈕、雙擊標題列、Win+↑）：回到一般模式（P-B2）
        let os_state = seen.fullscreen == Some(true) || seen.maximized == Some(true);
        if self.compact() && os_state {
            self.os_left_compact(ctx, seen.fullscreen == Some(true));
            return;
        }
        // 那個全螢幕 / 最大化結束了：還原一般視窗（macOS 等動畫結束、位置大小穩定）
        if self.modes.restore_later.is_some() && !os_state {
            let mut switch = Switch::after_system(WindowMode::Normal, now, seen.rect);
            if switch.tick(now, &seen, Timing::current()) == window_mode::Next::Apply {
                self.apply_restore_later(ctx);
            } else {
                self.modes.switch = Some(switch);
                ctx.request_repaint_after(Duration::from_millis(50));
            }
            return;
        }
        // 子母畫面裡開了沒有影像的檔案（純音訊、只有專輯封面）：子母畫面只有影片，換成迷你播放器（純音訊可以用）
        if std::mem::take(&mut self.modes.file_loaded) && self.pip() && self.pip_unavailable() {
            eprintln!("[vitascope] 子母畫面：這個檔案沒有影像，換成迷你播放器");
            self.osd(tr!(
                "這個檔案沒有影像，子母畫面換成迷你播放器",
                "This file has no video; switched from picture-in-picture to the mini player"
            ));
            self.enter_compact(ctx, WindowMode::Mini);
            return;
        }
        match self.modes.mode {
            // 有標題列時量外框比內容大多少（從子母畫面換到迷你播放器時用）
            WindowMode::Normal | WindowMode::Mini if seen.fullscreen != Some(true) => {
                if let Some((o, i)) = ctx.input(|i| i.viewport().outer_rect.zip(i.viewport().inner_rect)) {
                    let frame = o.size() - i.size();
                    if frame.min_elem() >= 0.0 && frame.max_elem() > 0.0 {
                        self.modes.frame = Some(frame);
                        if std::mem::take(&mut self.modes.place_after_frame) {
                            self.place_mini_again(ctx, o, i, frame);
                        }
                    }
                }
            }
            WindowMode::Pip => self.pip_snap_tick(ctx, &seen, now),
            _ => {}
        }
    }

    /// 從子母畫面換到迷你播放器時還不知道標題列多大（照沒有標題列放的）：量到了，照迷你播放器的規則重新放
    ///（記住的位置的右下角、或螢幕右下角，整個在工作區裡）
    fn place_mini_again(&mut self, ctx: &egui::Context, outer: Rect, inner: Rect, frame: egui::Vec2) {
        if self.modes.mode != WindowMode::Mini || self.wayland {
            return;
        }
        let Some(areas) = self.screen_areas() else {
            return;
        };
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point.unwrap_or(i.pixels_per_point));
        let size = inner.size();
        let pos = match self.settings.compact.remembered(WindowMode::Mini) {
            Some(g) if window_mode::position_usable(&g, Some(&areas)) => window_mode::anchored(
                pos2(g.pos[0], g.pos[1]),
                vec2(g.size[0], g.size[1]),
                size,
                frame,
                &areas,
                Self::bottom_allowance(),
                scale,
            ),
            _ => window_mode::placement(Some(outer), frame, size, &areas, Self::bottom_allowance(), scale),
        };
        if let Some(pos) = pos.filter(|p| *p != outer.min) {
            eprintln!(
                "[vitascope] 迷你播放器量到標題列 {:.0}×{:.0}，重新放在 {:.0}, {:.0}",
                frame.x, frame.y, pos.x, pos.y
            );
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
    }

    /// 子母畫面：使用者改了大小、停下來之後照影片的比例改回來（拉的是左邊、上面時右下角不動，見 `snap_position`）
    fn pip_snap_tick(&mut self, ctx: &egui::Context, seen: &Seen, now: Instant) {
        let Some(aspect) = self.video_aspect() else {
            return;
        };
        // Windows：系統改大小、移動的期間（拖曳的迴圈裡程式照樣在畫）不改，不然停住一下就改、下一個滑鼠移動又被系統改回去；
        // 結束時一定會送放開（那時 `os_drag` 清掉）。X11、macOS 可能不送放開：照「大小停下來」的規則（P-A3）
        if cfg!(windows) && self.modes.os_drag.is_some() {
            return;
        }
        let size = seen.rect.size;
        let max = window_mode::pip_max(ctx.input(|i| i.viewport().monitor_size));
        let snap = self.modes.snap.get_or_insert_with(|| SnapDebounce::new(now, size));
        let Some(new) = snap.tick(now, size, aspect, WindowMode::Pip.min_size(), max) else {
            // 大小剛變：停下來之後要醒來看一次（沒有其他事時介面不會重畫）
            if snap.waiting() {
                ctx.request_repaint_after(window_mode::SETTLE);
            }
            return;
        };
        eprintln!(
            "[vitascope] 子母畫面照影片比例 {:.0}×{:.0} → {:.0}×{:.0}",
            size.x, size.y, new.x, new.y
        );
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(new));
        // 拖曳還沒結束（X11 停住一下）：拉的是哪一邊留著，結束後的那一次還要用
        let dir = if self.modes.os_drag.is_some() {
            self.modes.resize_dir
        } else {
            self.modes.resize_dir.take()
        };
        if !self.wayland
            && let Some(pos) = seen.rect.pos
            && let Some(p) = window_mode::snap_position(pos, size, new, dir)
        {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(p));
        }
    }

    /// 每一幀、換模式的那一刻：macOS 子母畫面的內容比例鎖跟著影片的形狀（`VideoReconfig` 之後的尺寸已經算進長寬比、
    /// 裁切、旋轉），Spaces 行為跟著模式（[`NativePip::step`]）。其他系統的視窗什麼都不做，自動測試沒有視窗（或是假的）
    pub(super) fn pip_native_tick(&mut self, ctx: &egui::Context) {
        let st = &self.player.state;
        let video = st
            .video_size
            .filter(|_| st.has_video())
            .map(|[w, h]| [w as f64, h as f64]);
        let size = ctx.content_rect().size();
        let step = self.modes.native.step(self.pip(), video, size);
        let window = self.pip_window.as_deref();
        if let Some(aspect) = step.aspect {
            // 真的設了才記（其他系統沒有這個鎖，靠改完之後的 SnapDebounce）
            let applied = window.is_some_and(|w| w.set_aspect(aspect));
            if applied && step.aspect_changed {
                match aspect {
                    Some([w, h]) => eprintln!("[vitascope] 子母畫面：改大小時保持 {w:.0}:{h:.0}"),
                    None => eprintln!("[vitascope] 子母畫面：不再固定比例"),
                }
            }
        }
        match step.spaces {
            Some(true) => self.modes.native.saved = window.and_then(|w| w.float_on_spaces()),
            Some(false) => {
                if let (Some(w), Some(saved)) = (window, self.modes.native.saved.take()) {
                    w.restore_spaces(saved);
                }
            }
            None => {}
        }
    }

    /// 系統把迷你播放器、子母畫面變成全螢幕 / 最大化：回到一般模式（標題列、最小大小、層級改回來）；
    /// 一般視窗的位置大小等那個狀態結束時再還原（全螢幕之前是迷你播放器的大小，系統會還原成那個）
    fn os_left_compact(&mut self, ctx: &egui::Context, fullscreen: bool) {
        let was = std::mem::replace(&mut self.modes.mode, WindowMode::Normal);
        self.pip_native_tick(ctx);
        self.modes.changed_at = Some(Instant::now());
        self.modes.snap = None;
        if was == WindowMode::Pip {
            ctx.send_viewport_cmd(ViewportCommand::Decorations(true));
        }
        ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(window_mode::NORMAL_MIN));
        self.apply_window_level(ctx);
        if let Some(restore) = self.modes.restore.take() {
            self.modes.restore_later = Some((restore, fullscreen));
        }
        eprintln!(
            "[vitascope] 視窗模式：{}被系統改成{}，回到一般視窗",
            was.log_name(),
            if fullscreen { "全螢幕" } else { "最大化" }
        );
    }

    /// 系統的全螢幕 / 最大化結束了：還原一般視窗的位置大小（全螢幕之前原本最大化的再最大化；
    /// 使用者自己取消最大化的，不再最大化）
    fn apply_restore_later(&mut self, ctx: &egui::Context) {
        let Some((restore, from_fullscreen)) = self.modes.restore_later.take() else {
            return;
        };
        let restore = NormalRestore {
            maximized: restore.maximized && from_fullscreen,
            ..restore
        };
        let pos_ok = self.restore_pos_ok(&restore);
        let from = self.seen(ctx).rect;
        for cmd in window_mode::plan_leave(&restore, pos_ok) {
            ctx.send_viewport_cmd(cmd);
        }
        self.modes.switch = Some(Switch::resizing(WindowMode::Normal, Instant::now(), from));
    }

    /// 開著的大視窗
    fn big_windows(&self) -> BigWindows {
        BigWindows {
            settings: self.settings_open,
            panel: self.panel_open,
            sub_style: self.sub_style_open,
            export: self.export.open,
            online_subs: self.online_subs.open,
            about: self.about_open,
            info: self.info_open && self.pip(),
        }
    }

    /// 迷你播放器、子母畫面裡打開了大視窗（設定、控制面板、字幕外觀、匯出、線上搜尋字幕、關於；子母畫面裡還有媒體資訊；
    /// 從哪裡打開的都一樣）：回到一般視窗。
    /// 進入時就開著的（藏起來的）不算；關掉的就不再藏
    pub(super) fn compact_window_check(&mut self, ctx: &egui::Context) {
        if !self.compact() {
            return;
        }
        let now = self.big_windows();
        if now.opened_since(&self.modes.hidden) {
            self.leave_compact(ctx, Then::Nothing);
        } else {
            self.modes.hidden = now;
        }
    }

    /// 迷你播放器、設定頁：「迷你播放器置頂」開 / 關（`osd`：快捷鍵、右鍵選單才提示）
    pub(super) fn set_mini_on_top(&mut self, ctx: &egui::Context, on: bool, osd: bool) {
        if on && self.wayland {
            self.osd(tr!(
                "這個桌面環境（Wayland）不支援讓程式自己設定視窗置頂",
                "This desktop (Wayland) does not let programs keep themselves on top"
            ));
            return;
        }
        if self.settings.compact.mini_on_top != on {
            self.settings.compact.mini_on_top = on;
            self.save_settings();
        }
        self.sync_window_level(ctx);
        if osd {
            self.osd(if on {
                tr!("迷你播放器置頂：開啟", "Keep the mini player on top: on")
            } else {
                tr!("迷你播放器置頂：關閉", "Keep the mini player on top: off")
            });
        }
    }

    /// 迷你播放器、子母畫面配合影片調整大小（換檔、改長寬比、旋轉）：寬度不變、高度照影片的比例（迷你播放器的直式影片
    /// 改縮窄寬度），右下角不動（預設就放在右下角），超出所在的螢幕時往上、往左移回來（P§2.4）
    pub(super) fn compact_refit(&mut self, ctx: &egui::Context) {
        let Some(aspect) = self.video_aspect() else {
            return;
        };
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let content = ctx.content_rect().size();
        let size = match self.modes.mode {
            WindowMode::Pip => window_mode::pip_size(Some(aspect), Some(content.x), monitor),
            _ => window_mode::mini_size(Some(aspect), content.x, monitor),
        };
        if (size - content).abs().max_elem() < 0.5 {
            return;
        }
        let [w, h] = self.player.state.video_size.unwrap_or_default();
        eprintln!(
            "[vitascope] {}配合影片 {w}×{h} → {:.0}×{:.0}",
            self.modes.mode.log_name(),
            size.x,
            size.y
        );
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        // 右下角不動，超出它所在的螢幕（不一定是主螢幕）時往上、往左移回來。Wayland 不能指定位置
        let (outer, inner, scale) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.native_pixels_per_point.unwrap_or(i.pixels_per_point),
            )
        });
        let areas = if self.wayland { None } else { self.screen_areas() };
        match (outer, inner, areas) {
            (Some(o), Some(i), Some(areas)) => {
                let frame = o.size() - i.size();
                let pos = window_mode::anchored(o.min, i.size(), size, frame, &areas, Self::bottom_allowance(), scale);
                if let Some(pos) = pos.filter(|p| *p != o.min) {
                    ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
                }
            }
            // 不知道有哪些螢幕：照一般視窗的方法（主螢幕裡才調整）
            _ if !self.wayland => self.keep_on_screen(ctx, size),
            _ => {}
        }
    }

    /// 記下迷你播放器、子母畫面的位置大小（各記各的，下次進入時用）：換好模式 0.6 秒之後、
    /// 而且跟換之前的視窗不一樣了才記
    pub(super) fn remember_compact(&mut self, ctx: &egui::Context) {
        if self.modes.changed_at.is_some_and(|t| t.elapsed() < REMEMBER_AFTER) {
            return;
        }
        let rect = self.seen(ctx).rect;
        if self.modes.entered_from == Some(rect) {
            return;
        }
        // 不知道位置（Wayland：程式拿不到、也不能指定）：只記大小（下次照用寬度），位置記成不在任何螢幕上
        // （下次放預設位置；換到 X11 也不會用到這個假的位置）
        let pos = rect.pos.map_or(UNKNOWN_POS, Into::into);
        let g = WindowGeometry {
            pos,
            size: rect.size.into(),
            maximized: false,
        };
        self.settings.compact.remember(self.modes.mode, g);
    }

    /// 迷你播放器下面的一列：⏸ · 時間 · 進度 · ⏳（播完後的動作）· 🔊（點一下靜音、滾輪調音量）· ▣（子母畫面）·
    /// 🗖（回到一般視窗）
    pub(super) fn mini_bar(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        ui.horizontal_centered(|ui| {
            let st = &self.player.state;
            let loaded = st.loaded;
            let play_icon = if !st.paused && loaded { "⏸" } else { "▶" };
            if ui
                .add_enabled(loaded, icon_button(play_icon))
                .on_hover_text(
                    self.keymap
                        .labeled(tr!("播放 / 暫停", "Play / pause"), Command::TogglePause),
                )
                .clicked()
            {
                self.run(ui.ctx(), Action::TogglePause);
            }
            let st = &self.player.state;
            let pos = self.seek_drag.unwrap_or(st.time_pos);
            let time = if loaded { fmt_time(pos) } else { "--:--".to_owned() };
            let total = st.duration.filter(|_| loaded && !self.live_now()).map(fmt_time);
            let label = ui.label(egui::RichText::new(time).monospace());
            if let Some(total) = total {
                label.on_hover_text(tr!("總長度 ", "Length ").to_owned() + &total);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let back = ui.add(icon_button("🗖")).on_hover_text(
                    self.keymap
                        .labeled(tr!("回到一般視窗", "Back to the normal window"), Command::MiniPlayer),
                );
                if back.clicked() {
                    self.run(ui.ctx(), Action::NormalWindow);
                }
                // ▣ 換成子母畫面（沒有影像的檔案不能用）
                let pip = ui
                    .add_enabled(!self.pip_unavailable(), icon_button("▣"))
                    .on_hover_text(self.keymap.labeled(Command::Pip.label(), Command::Pip))
                    .on_disabled_hover_text(Self::pip_unavailable_text());
                if pip.clicked() {
                    self.run(ui.ctx(), Action::TogglePip);
                }
                let st = &self.player.state;
                let spdif = st.audio_spdif.is_some();
                let icon = if !spdif && (st.muted || st.volume == 0.0) {
                    "🔇"
                } else {
                    "🔊"
                };
                let volume = ui
                    .add_enabled(!spdif, icon_button(icon))
                    .on_hover_text(crate::keymap::paren(
                        &crate::tf!(
                            "音量 {:.0}%（滾輪調整、點一下靜音）",
                            "Volume {:.0}% (scroll to change, click to mute)",
                            self.player.volume_total()
                        ),
                        &self.keymap.hint(Command::ToggleMute),
                    ))
                    .on_disabled_hover_text(super::sound::spdif_volume_hover());
                self.modes.wheel_on_volume = volume.hovered() && !spdif;
                if volume.clicked() {
                    self.run(ui.ctx(), Action::ToggleMute);
                }
                // 播完後的動作（⏳，點一下取消）：選了關機時一定要看得到（S-B9）
                if let Some((text, hover)) = self.after_indicator() {
                    let accent = Palette::of(ui.visuals()).accent;
                    if ui
                        .add(egui::Button::new(egui::RichText::new(text).color(accent)).frame(false))
                        .on_hover_text(hover)
                        .clicked()
                    {
                        self.run(ui.ctx(), Action::SetAfterPlayback(AfterPlayback::Nothing));
                    }
                }
                // 剩下的寬度給進度條
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.progress_bar(ui));
            });
        });
    }

    // ───────────── 子母畫面 ─────────────

    /// 子母畫面：影片畫面上的滑鼠——邊緣（8 點）按下去改大小、其他地方拖曳移動（Windows、Linux；macOS 改大小用系統自己的邊緣，
    /// `BeginResize` 在那裡不能用）——和滑鼠移上去時才出現的控制項（中間 ▶ / ⏸、下面細的進度條、右上 🗖），
    /// 停住 2 秒後消失。`input`：這一幀可以處理滑鼠（倒數畫面、選單開著時不行）。
    /// 回傳控制項蓋住畫面下面多少（佔畫面高度的比例，字幕往上推）
    pub(super) fn pip_ui(&mut self, ui: &mut egui::Ui, rect: Rect, response: &egui::Response, input: bool) -> f32 {
        let ctx = ui.ctx().clone();
        let (hover, pressed, origin) = ctx.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.primary_pressed(),
                i.pointer.press_origin(),
            )
        });
        let band = |p: Option<egui::Pos2>| {
            p.filter(|_| cfg!(not(target_os = "macos")))
                .and_then(|p| window_mode::resize_zone(p, rect, window_mode::RESIZE_BAND))
        };
        if input {
            // 邊緣：游標換成改大小的；按下去交給系統改大小（放開由系統處理，可能不送給程式，見 `fix_lost_release`）
            if let Some(dir) = band(hover) {
                ctx.set_cursor_icon(window_mode::resize_cursor(dir));
            }
            if pressed
                && self.modes.os_drag.is_none()
                && let Some(dir) = band(origin)
            {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
                self.start_os_drag(&ctx);
                self.modes.resize_dir = Some(dir);
            }
            // 其他地方：拖曳就交給系統移動視窗（用「開始拖曳」而不是按下：單擊、雙擊照常）
            if response.drag_started_by(egui::PointerButton::Primary) && self.modes.os_drag.is_none() {
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                self.start_os_drag(&ctx);
            }
        }
        // 控制項：滑鼠在畫面上、最近 2 秒有動（拖曳進度條時一直顯示）
        let idle = self.last_activity.elapsed();
        let inside = hover.is_some_and(|p| rect.contains(p));
        let show = inside && (idle < HIDE_AFTER || self.seek_drag.is_some());
        self.modes.pip_controls = show;
        if !show {
            return 0.0;
        }
        ctx.request_repaint_after(HIDE_AFTER.saturating_sub(idle) + Duration::from_millis(50));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        // 蓋在影片上：淺色主題也是深色的
        theme::dark_overlay(&mut ui);
        let shade = Color32::from_black_alpha(140);
        let inset = window_mode::RESIZE_BAND + 2.0;
        let mut actions = Vec::new();
        // 中間：播放 / 暫停（大一點，小視窗也好按）
        let st = &self.player.state;
        let icon = if !st.paused && st.loaded { "⏸" } else { "▶" };
        let loaded = st.loaded;
        let side = (rect.height() * 0.3).clamp(28.0, 56.0);
        ui.painter().circle_filled(rect.center(), side * 0.7, shade);
        let play = ui
            .put(
                Rect::from_center_size(rect.center(), vec2(side, side)),
                egui::Button::new(egui::RichText::new(icon).size(side * 0.6)).frame(false),
            )
            .on_hover_text(
                self.keymap
                    .labeled(tr!("播放 / 暫停", "Play / pause"), Command::TogglePause),
            );
        // 中間是最順手雙擊的地方：雙擊照樣回到一般視窗（第一下在這裡切換了暫停的話切回來，結果只有回到一般視窗）。
        // egui 的第二下同時是單擊和雙擊：雙擊時第二下不切換
        let now = ctx.input(|i| i.time);
        let max_delay = ctx.options(|o| o.input_options.max_double_click_delay);
        if play.double_clicked() {
            if self.modes.play_click.take().is_some_and(|t| now - t <= max_delay) {
                actions.push(Action::TogglePause);
            }
            actions.push(Action::NormalWindow);
        } else if play.clicked() && loaded {
            actions.push(Action::TogglePause);
            self.modes.play_click = Some(now);
        }
        // 在中間按右鍵（按到 ▶ / ⏸ 上）也是右鍵選單
        play.context_menu(|ui| self.context_menu(ui));
        // 右上：回到一般視窗（雙擊畫面也是）
        let back_rect = Rect::from_min_size(pos2(rect.right() - inset - 32.0, rect.top() + inset), vec2(32.0, 26.0));
        ui.painter().rect_filled(back_rect, CornerRadius::same(4), shade);
        let back = ui
            .put(back_rect, icon_button("🗖"))
            .on_hover_text(tr!("回到一般視窗（雙擊）", "Back to the normal window (double-click)"));
        if back.clicked() {
            actions.push(Action::NormalWindow);
        }
        // 下面：細的進度條（離邊緣留改大小的空間）
        let bar = Rect::from_min_max(
            pos2(rect.left() + inset, rect.bottom() - inset - 18.0),
            pos2(rect.right() - inset, rect.bottom() - inset),
        );
        let band_top = bar.top() - 4.0;
        ui.painter().rect_filled(
            Rect::from_min_max(pos2(rect.left(), band_top), rect.max),
            CornerRadius::ZERO,
            Color32::from_black_alpha(110),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| self.progress_bar(ui));
        let undo = actions.len() > 1;
        for a in actions {
            self.run(&ctx, a);
        }
        // 切回來的暫停不顯示（跟雙擊畫面一樣，只看到回到一般視窗）
        if undo {
            self.osd = None;
        }
        ((rect.bottom() - band_top) / rect.height().max(1.0)).clamp(0.0, 1.0)
    }

    /// 送出了 `StartDrag` / `BeginResize`：記下來，結束時沒有「放開」的話補一個（P-A3）
    fn start_os_drag(&mut self, ctx: &egui::Context) {
        let rect = ctx.input(|i| i.viewport().outer_rect.or(i.viewport().inner_rect));
        self.modes.os_drag = Some(OsDrag::new(Instant::now(), rect));
    }

    /// `raw_input_hook`（egui 處理這一批輸入之前）：系統的拖曳（子母畫面的移動、改大小）結束了卻沒送「放開」時補一個，
    /// egui 才不會一直以為滑鼠按著（控制項不會消失、下一下點擊被吃掉，P-A3）
    pub(super) fn fix_lost_release(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let Some(drag) = &mut self.modes.os_drag else {
            return;
        };
        let rect = raw
            .viewports
            .get(&raw.viewport_id)
            .and_then(|v| v.outer_rect.or(v.inner_rect));
        // 滑鼠離開過視窗（X11 視窗管理員接手拖曳時）egui 沒有位置：用按下的位置
        let (down, pointer, modifiers) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.latest_pos().or(i.pointer.press_origin()),
                i.modifiers,
            )
        });
        match drag.check(Instant::now(), rect, &raw.events, down, pointer) {
            DragEnd::Going => {}
            DragEnd::Released => self.modes.os_drag = None,
            DragEnd::Inject { at, pos } => {
                raw.events.insert(
                    at,
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers,
                    },
                );
                self.modes.os_drag = None;
            }
        }
    }

    /// 子母畫面的右鍵選單（視窗小，完整的選單放不下，P-C7）：播放 / 暫停、靜音、回到一般視窗、迷你播放器、結束影戲
    pub(super) fn pip_menu_items(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let st = &self.player.state;
        let loaded = st.loaded;
        let muted = st.muted;
        let pause_label = if loaded && !st.paused {
            tr!("暫停", "Pause")
        } else {
            tr!("播放", "Play")
        };
        let mut action = None;
        if self.cmd_item(ui, loaded, pause_label, Command::TogglePause) {
            action = Some(Action::TogglePause);
        }
        let mute =
            egui::Button::selectable(muted, tr!("靜音", "Mute")).shortcut_text(self.keymap.hint(Command::ToggleMute));
        if ui.add(mute).clicked() {
            action = Some(Action::ToggleMute);
        }
        ui.separator();
        if super::menu_item(ui, true, tr!("回到一般視窗", "Back to the normal window"), "Esc") {
            action = Some(Action::NormalWindow);
        }
        let mini = egui::Button::selectable(false, Command::MiniPlayer.label())
            .shortcut_text(self.keymap.hint(Command::MiniPlayer));
        if ui.add(mini).clicked() {
            action = Some(Action::ToggleMini);
        }
        ui.separator();
        if super::menu_item(ui, true, tr!("結束影戲", "Quit VitaScope"), "") {
            action = Some(Action::Quit);
        }
        action
    }

    /// 播完後的倒數要開始了：子母畫面小到放不下倒數畫面時回到一般視窗（倒數一定要看得到，S-B9）；
    /// 放得下就留在子母畫面（跟迷你播放器一樣）
    pub(super) fn make_room_for_countdown(&mut self, ctx: &egui::Context) {
        let size = ctx.content_rect().size();
        if self.pip() && (size.x < COUNTDOWN_ROOM.x || size.y < COUNTDOWN_ROOM.y) {
            eprintln!("[vitascope] 子母畫面放不下播完後的倒數，回到一般視窗");
            self.leave_compact(ctx, Then::Nothing);
        }
    }
}

/// 播完後的倒數畫面需要的大小（倒數畫面最寬 280 點加上邊框；子母畫面預設最小的 320×180 放得下）
const COUNTDOWN_ROOM: egui::Vec2 = egui::vec2(320.0, 180.0);
