//! 迷你播放器（F1；子母畫面在 F2 加進來）：進入、離開、切換途中要等的事、精簡的控制列。
//!
//! - 進入時記下一般視窗的位置大小（要求的那一刻，還沒送出離開全螢幕、取消最大化之前，P-B1），回來時照樣還原
//!   （原本最大化的再最大化）。切換途中不記視窗的位置（[`Self::remember_window`]），也不配合影片調整大小。
//! - 全螢幕一律經過 [`VitascopeApp::request_fullscreen`]：迷你播放器裡要全螢幕時，先回到一般視窗、等它還原好才全螢幕
//!   （單一執行個體的 `--fullscreen` 也是）。
//! - 系統把迷你播放器變成全螢幕或最大化（macOS 的綠色按鈕、雙擊標題列、Win+↑）時，回到一般模式（P-B2）；
//!   全螢幕 / 最大化結束後還原一般視窗的位置大小。
//! - 視窗層級只有一個來源：`on_top_now`（迷你播放器開著「迷你播放器置頂」時一律置頂，不然照「視窗置頂」的模式）。
//! - 要大視窗的功能（側邊面板、設定、控制面板、字幕外觀、匯出、關於）先回到一般視窗；
//!   進入時已經開著的這些視窗在迷你播放器裡先藏起來，回來時照原樣出現。
//! - 系統的媒體控制叫視窗到前面（Raise）、播完後的倒數都不會離開迷你播放器（S-B9）。

use super::{Action, VitascopeApp, fmt_time, icon_button};
use crate::keymap::Command;
use crate::screens::Area;
use crate::settings::{SideTab, WindowGeometry};
use crate::system_power::AfterPlayback;
use crate::theme::Palette;
use crate::tr;
use crate::window_mode::{self, NormalRestore, Seen, Stage, Switch, Timing, WinRect, WindowMode};
use eframe::egui::{self, Align, Layout, ViewportCommand, pos2, vec2};
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

/// 迷你播放器裡藏起來的視窗：進入時開著的（設定、控制面板、字幕外觀、匯出）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct BigWindows {
    settings: bool,
    panel: bool,
    sub_style: bool,
    export: bool,
    about: bool,
}

impl BigWindows {
    /// 這次比 `before` 多開了哪一個
    fn opened_since(&self, before: &BigWindows) -> bool {
        (self.settings && !before.settings)
            || (self.panel && !before.panel)
            || (self.sub_style && !before.sub_style)
            || (self.export && !before.export)
            || (self.about && !before.about)
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
    /// 現在是精簡的模式（迷你播放器）
    pub(super) fn compact(&self) -> bool {
        self.modes.mode.is_compact()
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

    /// 迷你播放器開 / 關（Ctrl+Shift+M、控制列的 🗗、右鍵選單）
    pub(super) fn toggle_mini(&mut self, ctx: &egui::Context) {
        let entering = self.modes.switch.is_some_and(|s| s.to == WindowMode::Mini);
        if self.compact() || entering {
            self.leave_compact(ctx, Then::Nothing);
        } else {
            self.enter_compact(ctx, WindowMode::Mini);
        }
    }

    /// 進入精簡的模式：記下一般視窗，全螢幕的先離開全螢幕、最大化的先取消最大化（做完才送新的大小）
    fn enter_compact(&mut self, ctx: &egui::Context, to: WindowMode) {
        if self.compact() {
            return;
        }
        let seen = self.seen(ctx);
        // 正在回到一般視窗（還沒還原好，視窗可能還是迷你播放器的大小）：用原本記下的一般視窗
        let leaving = self.modes.switch.is_some_and(|s| s.to == WindowMode::Normal);
        let restore = match self.modes.restore {
            Some(r) if leaving => r,
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
        // Wayland 不能置頂（這次執行只提示一次）
        if self.wayland && self.settings.compact.mini_on_top && !std::mem::replace(&mut self.modes.wayland_told, true) {
            self.osd(tr!(
                "這個桌面環境（Wayland）不讓程式自己置頂，視窗可能被其他視窗蓋住",
                "This desktop (Wayland) doesn't let programs keep themselves on top; the window may be covered"
            ));
        }
        ctx.request_repaint();
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

    /// 送出進入迷你播放器的指令（大小、位置；層級照 `on_top_now`），換成這個模式
    fn apply_compact(&mut self, ctx: &egui::Context, to: WindowMode) {
        let (outer, inner, monitor, scale) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.outer_rect,
                v.inner_rect,
                v.monitor_size,
                v.native_pixels_per_point.unwrap_or(i.pixels_per_point),
            )
        });
        let st = &self.player.state;
        let aspect = st
            .video_size
            .filter(|[w, h]| *w > 0 && *h > 0 && st.has_video())
            .map(|[w, h]| w as f32 / h as f32);
        let remembered = self.settings.compact.mini;
        let width = remembered.map_or(window_mode::MINI_WIDTH, |g| g.size[0]);
        let size = window_mode::mini_size(aspect, width, monitor);
        let areas = self.screen_areas();
        let frame = match (outer, inner) {
            (Some(o), Some(i)) => (o.size() - i.size()).max(egui::Vec2::ZERO),
            _ => egui::Vec2::ZERO,
        };
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
        for cmd in window_mode::plan_enter(size, pos) {
            ctx.send_viewport_cmd(cmd);
        }
        self.modes.entered_from = Some(self.seen(ctx).rect);
        self.modes.mode = to;
        self.modes.changed_at = Some(Instant::now());
        // 進入時開著的大視窗先藏起來（回來時照原樣出現）；「關於」是對話框（開著時按鍵都給它），直接關掉
        self.about_open = false;
        self.modes.hidden = self.big_windows();
        // 側邊面板的書籤改到一半：跟面板關掉一樣，改好
        self.finish_hidden_rename();
        self.apply_window_level(ctx);
        eprintln!(
            "[vitascope] 視窗模式：一般視窗 → 迷你播放器（{:.0}×{:.0}）",
            size.x, size.y
        );
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
        self.modes.mode = WindowMode::Normal;
        self.modes.changed_at = Some(Instant::now());
        self.modes.then = then;
        self.modes.switch = Some(Switch::resizing(WindowMode::Normal, Instant::now(), from));
        self.apply_window_level(ctx);
        eprintln!(
            "[vitascope] 視窗模式：迷你播放器 → 一般視窗（{:.0}×{:.0}{}）",
            restore.size.x,
            restore.size.y,
            if restore.maximized { "，最大化" } else { "" }
        );
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
            Then::Fullscreen => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true)),
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
            None => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true)),
        }
    }

    /// 全螢幕的開關要切到哪一邊（已經全螢幕，或正在等回到一般視窗後全螢幕：關）
    pub(super) fn fullscreen_wanted(&self, ctx: &egui::Context) -> bool {
        super::is_fullscreen(ctx) || self.modes.then == Then::Fullscreen
    }

    /// 迷你播放器裡（或正在回到一般視窗時）按了 F6 / H：回到一般視窗，打開側邊面板的這一頁
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
        // 系統把迷你播放器變成全螢幕或最大化（綠色按鈕、雙擊標題列、Win+↑）：回到一般模式（P-B2）
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
        }
    }

    /// 系統把迷你播放器變成全螢幕 / 最大化：回到一般模式（最小大小、層級改回來）；
    /// 一般視窗的位置大小等那個狀態結束時再還原（全螢幕之前是迷你播放器的大小，系統會還原成那個）
    fn os_left_compact(&mut self, ctx: &egui::Context, fullscreen: bool) {
        self.modes.mode = WindowMode::Normal;
        self.modes.changed_at = Some(Instant::now());
        ctx.send_viewport_cmd(ViewportCommand::MinInnerSize(window_mode::NORMAL_MIN));
        self.apply_window_level(ctx);
        if let Some(restore) = self.modes.restore.take() {
            self.modes.restore_later = Some((restore, fullscreen));
        }
        eprintln!(
            "[vitascope] 視窗模式：迷你播放器被系統改成{}，回到一般視窗",
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
            about: self.about_open,
        }
    }

    /// 迷你播放器裡打開了大視窗（設定、控制面板、字幕外觀、匯出、關於；從哪裡打開的都一樣）：回到一般視窗。
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

    /// 迷你播放器配合影片調整大小（換檔、改長寬比、旋轉）：寬度不變、高度照影片的比例（直式的改縮窄寬度），
    /// 右下角不動（預設就放在右下角），超出所在的螢幕時往上、往左移回來（P§2.4）
    pub(super) fn compact_refit(&mut self, ctx: &egui::Context) {
        let st = &self.player.state;
        let Some([w, h]) = st.video_size.filter(|[w, h]| *w > 0 && *h > 0) else {
            return;
        };
        if !st.has_video() {
            return;
        }
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let content = ctx.content_rect().size();
        let size = window_mode::mini_size(Some(w as f32 / h as f32), content.x, monitor);
        if (size - content).abs().max_elem() < 0.5 {
            return;
        }
        eprintln!("[vitascope] 迷你播放器配合影片 {w}×{h} → {:.0}×{:.0}", size.x, size.y);
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

    /// 記下迷你播放器的位置大小（下次進入時用）：換好模式 0.6 秒之後、而且跟換之前的一般視窗不一樣了才記
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
        self.settings.compact.mini = Some(WindowGeometry {
            pos,
            size: rect.size.into(),
            maximized: false,
        });
    }

    /// 迷你播放器下面的一列：⏸ · 時間 · 進度 · ⏳（播完後的動作）· 🔊（點一下靜音、滾輪調音量）· 🗖（回到一般視窗）
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
}
