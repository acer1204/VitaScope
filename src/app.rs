//! 播放器視窗：影片畫面、控制列、快捷鍵、全螢幕。

use crate::autoshot::AutoShot;
use crate::formats;
use crate::player::{Player, PlayerEvent, TrackKind};
use crate::settings::{Settings, WindowGeometry};
use crate::update::{self, UpdateStatus};
use crate::video::VideoView;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Rect,
    Sense, Stroke, Vec2, ViewportCommand, pos2, vec2,
};
use eframe::glow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const APP_NAME: &str = "影戲 VitaScope";
/// 全螢幕時，滑鼠多久沒動就隱藏控制列
const HIDE_AFTER: Duration = Duration::from_secs(2);
const OSD_DURATION: Duration = Duration::from_millis(1500);
/// 控制列完整顯示需要的寬度
const MIN_WINDOW_WIDTH: f32 = 560.0;
const ACCENT: Color32 = Color32::from_rgb(0x4f, 0x9d, 0xff);

/// 鍵盤或按鈕觸發的操作
#[derive(Debug, Clone, Copy)]
enum Action {
    TogglePause,
    Stop,
    Seek(f64),
    Volume(f64),
    ToggleMute,
    ToggleFullscreen,
    ExitFullscreen,
    Open,
    About,
}

pub struct VitascopeApp {
    player: Player,
    video: Option<VideoView>,
    settings: Settings,
    /// 影片畫面無法初始化之類的嚴重錯誤
    fatal: Option<String>,
    last_activity: Instant,
    osd: Option<(String, Instant)>,
    /// 拖曳進度條時預覽的時間；放開後保留到 mpv 跳轉完成，進度條才不會跳回舊位置
    seek_drag: Option<f64>,
    seek_released: bool,
    /// 新檔案載入後，依影片尺寸調整視窗一次
    fit_window_pending: bool,
    window_title: String,
    /// 視窗模式下控制列的高度（調整視窗大小時要算進去）
    controls_height: f32,
    pointer_over_controls: bool,
    /// 開發用：`--shot` 自動截圖
    autoshot: Option<AutoShot>,
    /// 上次記錄的硬體解碼器，改變時印到記錄裡
    logged_hwdec: Option<String>,
    /// 已畫出的幀數。視窗顯示之前送出的大小 / 全螢幕指令會被 eframe 還原的視窗狀態蓋掉，
    /// 所以這類指令等視窗出現後才送
    frames: u64,
    /// 啟動參數 --fullscreen，等視窗出現後執行
    start_fullscreen: bool,
    /// 以全螢幕啟動時，第一個檔案不調整視窗大小
    skip_next_fit: bool,
    /// 目前設定給 mpv 的字幕底部邊距（sub-margin-y）
    sub_margin: i64,
    /// 目前的檔案已經收到影像設定事件（影片尺寸是新的）
    video_reconfigured: bool,
    /// 「關於」視窗
    about_open: bool,
    /// 檢查更新的進度與結果（背景執行緒寫入）；None = 還沒檢查
    update_status: Option<Arc<Mutex<UpdateStatus>>>,
    /// 「關於」裡顯示的播放引擎版本
    engine_versions: String,
}

/// 啟動參數
#[derive(Default)]
pub struct Launch {
    pub file: Option<PathBuf>,
    pub fullscreen: bool,
    pub autoshot: Option<AutoShot>,
}

impl VitascopeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, player: Player, settings: Settings, launch: Launch) -> Self {
        crate::fonts::install_cjk(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let _ = player.set_volume(settings.volume);
        let _ = player.set_mute(settings.muted);

        if let Some(gl) = &cc.gl {
            use eframe::glow::HasContext;
            // SAFETY: 建立 App 時 GL context 是 current
            let (renderer, version) = unsafe {
                (
                    gl.get_parameter_string(glow::RENDERER),
                    gl.get_parameter_string(glow::VERSION),
                )
            };
            eprintln!("[vitascope] OpenGL：{renderer}（{version}）");
            // Mesa 的軟體繪圖（llvmpipe 等，常見於虛擬機、沒有顯示卡驅動的電腦）上，
            // mpv 完整的繪圖流程畫出來是全黑的；改用簡化流程（少了高品質縮放等效果，但看得到畫面）
            if is_mesa_software_renderer(&renderer) && !mpv_opts_override("gpu-dumb-mode") {
                eprintln!("[vitascope] 偵測到軟體繪圖，mpv 改用簡化的繪圖流程");
                let _ = player.mpv().set_property("gpu-dumb-mode", "yes");
            }
        }
        let (video, fatal) = match &cc.get_proc_address {
            Some(gpa) => match VideoView::new(player.mpv().clone(), gpa.clone(), cc.egui_ctx.clone()) {
                Ok(v) => (Some(v), None),
                Err(e) => (None, Some(format!("無法初始化影片畫面：{e}"))),
            },
            None => (None, Some("無法初始化影片畫面：沒有 OpenGL context".into())),
        };
        if let Some(msg) = &fatal {
            eprintln!("[vitascope] {msg}");
        }

        let mut app = Self {
            player,
            video,
            settings,
            fatal,
            last_activity: Instant::now(),
            osd: None,
            seek_drag: None,
            seek_released: false,
            fit_window_pending: false,
            window_title: String::new(),
            controls_height: 0.0,
            pointer_over_controls: false,
            autoshot: launch.autoshot,
            logged_hwdec: None,
            frames: 0,
            start_fullscreen: launch.fullscreen,
            skip_next_fit: launch.fullscreen,
            sub_margin: 22,
            video_reconfigured: false,
            about_open: false,
            update_status: None,
            engine_versions: String::new(),
        };
        app.engine_versions = short_versions(
            &app.player.get_string("mpv-version").unwrap_or_default(),
            &app.player.get_string("ffmpeg-version").unwrap_or_default(),
        );
        match launch.file {
            Some(path) => app.open(&path),
            // 沒有要開檔：截的是起始畫面，現在就開始計時
            None => {
                if let Some(shot) = &mut app.autoshot {
                    shot.arm();
                }
            }
        }
        app
    }

    /// 播放器核心（介面測試用來檢查狀態）
    pub fn player(&self) -> &Player {
        &self.player
    }

    // ───────────── 操作 ─────────────

    fn open(&mut self, path: &Path) {
        // 開新檔一律從播放開始（mpv 會沿用上一個檔案的暫停狀態）
        let _ = self.player.set_pause(false);
        if let Err(e) = self.player.open(&path.to_string_lossy()) {
            self.player.state.last_error = Some(format!("無法開啟：{e}"));
        }
    }

    fn open_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("開啟影片")
            .add_filter("影音檔案", &formats::all_media())
            .add_filter("所有檔案", &["*"]);
        if let Some(dir) = self.player.state.path.as_deref().and_then(|p| Path::new(p).parent()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open(&path);
        }
    }

    fn osd(&mut self, text: impl Into<String>) {
        self.osd = Some((text.into(), Instant::now()));
    }

    fn run(&mut self, ctx: &egui::Context, action: Action) {
        let st = &self.player.state;
        let loaded = st.loaded;
        match action {
            Action::TogglePause if loaded => {
                let msg = if st.paused { "▶ 播放" } else { "⏸ 暫停" };
                let _ = self.player.toggle_pause();
                self.osd(msg);
            }
            Action::Stop if loaded => {
                let _ = self.player.stop();
            }
            Action::Seek(delta) if loaded && st.seekable => {
                let duration = st.duration.unwrap_or(0.0);
                let target = (st.time_pos + delta).clamp(0.0, duration);
                let _ = self.player.seek_relative(delta);
                let dir = if delta < 0.0 { "◀◀ 後退" } else { "▶▶ 前進" };
                self.osd(format!(
                    "{dir} {} 秒   {} / {}",
                    delta.abs(),
                    fmt_time(target),
                    fmt_time(duration)
                ));
            }
            Action::Volume(delta) => {
                let v = (st.volume + delta).clamp(0.0, 100.0);
                let _ = self.player.set_volume(v);
                if st.muted {
                    let _ = self.player.set_mute(false);
                }
                self.osd(format!("音量 {v:.0}%"));
            }
            Action::ToggleMute => {
                let muted = !st.muted;
                let _ = self.player.set_mute(muted);
                self.osd(if muted { "靜音" } else { "取消靜音" });
            }
            Action::ToggleFullscreen => {
                let fullscreen = is_fullscreen(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
            }
            Action::ExitFullscreen => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false)),
            Action::Open => self.open_dialog(),
            Action::About => self.about_open = true,
            _ => {}
        }
    }

    fn select_track(&mut self, kind: TrackKind, id: Option<i64>) {
        let _ = self.player.select_track(kind, id);
        let name = if kind == TrackKind::Sub { "字幕" } else { "音軌" };
        let label = id
            .and_then(|id| self.player.state.tracks_of(kind).find(|t| t.id == id))
            .map_or_else(|| "關閉".to_owned(), |t| t.label());
        self.osd(format!("{name}：{label}"));
    }

    // ───────────── 每一幀的邏輯 ─────────────

    fn on_player_event(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::StartFile => {
                self.fit_window_pending = !std::mem::take(&mut self.skip_next_fit);
                self.video_reconfigured = false;
            }
            // 新檔案的影像設定好了，尺寸才是新的
            PlayerEvent::VideoReconfig => self.video_reconfigured = true,
            PlayerEvent::PlaybackRestart => {
                if let Some(shot) = &mut self.autoshot {
                    shot.arm();
                }
                if self.seek_released {
                    self.seek_drag = None;
                    self.seek_released = false;
                }
            }
            PlayerEvent::EndFile { error, .. } => {
                self.fit_window_pending = false;
                self.seek_drag = None;
                self.seek_released = false;
                if let Some(e) = error {
                    eprintln!("[vitascope] {e}");
                    // 開檔失敗也要截圖（截的是錯誤畫面）
                    if let Some(shot) = &mut self.autoshot {
                        shot.arm();
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if std::env::var_os("VITASCOPE_DEBUG_KEYS").is_some() {
            ctx.input(|i| {
                for e in &i.events {
                    if let egui::Event::Key {
                        key,
                        pressed,
                        modifiers,
                        ..
                    } = e
                    {
                        eprintln!("[keys] {key:?} pressed={pressed} {modifiers:?}");
                    }
                }
            });
        }
        // 「關於」視窗開著時，按鍵都交給它（Esc 關閉視窗，而不是離開全螢幕）
        if self.about_open {
            return;
        }
        // Esc 只在全螢幕、而且沒有選單開著時才用來離開全螢幕；其他時候留給 egui 關選單
        let esc_exits_fullscreen = is_fullscreen(ctx) && !egui::Popup::is_any_open(ctx);
        // 先把快捷鍵吃掉，避免同一個按鍵又觸發 egui 的按鈕（例如空白鍵按下有焦點的按鈕）
        let mut actions = Vec::new();
        ctx.input_mut(|i| {
            let mut key = |mods: Modifiers, key: Key, action: Action| {
                if i.consume_key(mods, key) {
                    actions.push(action);
                }
            };
            key(Modifiers::COMMAND, Key::O, Action::Open);
            key(Modifiers::COMMAND, Key::ArrowLeft, Action::Seek(-30.0));
            key(Modifiers::COMMAND, Key::ArrowRight, Action::Seek(30.0));
            key(Modifiers::NONE, Key::ArrowLeft, Action::Seek(-5.0));
            key(Modifiers::NONE, Key::ArrowRight, Action::Seek(5.0));
            key(Modifiers::NONE, Key::ArrowUp, Action::Volume(5.0));
            key(Modifiers::NONE, Key::ArrowDown, Action::Volume(-5.0));
            key(Modifiers::NONE, Key::Space, Action::TogglePause);
            key(Modifiers::NONE, Key::M, Action::ToggleMute);
            key(Modifiers::NONE, Key::F, Action::ToggleFullscreen);
            key(Modifiers::NONE, Key::Enter, Action::ToggleFullscreen);
            key(Modifiers::NONE, Key::F1, Action::About);
            if esc_exits_fullscreen {
                key(Modifiers::NONE, Key::Escape, Action::ExitFullscreen);
            }
        });
        if !actions.is_empty() {
            self.last_activity = Instant::now();
        }
        for a in actions {
            self.run(ctx, a);
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        let Some(first) = dropped.first() else { return };
        if formats::is_subtitle(first) && self.player.state.loaded {
            match self.player.add_subtitle(&first.to_string_lossy()) {
                Ok(()) => self.osd(format!("載入字幕：{}", file_name(first))),
                Err(e) => self.osd(format!("無法載入字幕：{e}")),
            }
        } else {
            self.open(first);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let st = &self.player.state;
        let title = match &st.title {
            Some(t) if st.loaded => format!("{t} — {APP_NAME}"),
            _ => APP_NAME.to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// 新檔案的影片尺寸確定後，把視窗調整成影片比例（不超過螢幕的 80%）
    fn fit_window(&mut self, ctx: &egui::Context) {
        if !self.fit_window_pending || !self.video_reconfigured {
            return;
        }
        let Some([w, h]) = self.player.state.video_size else {
            return;
        };
        self.fit_window_pending = false;
        let (fullscreen, maximized, monitor) = ctx.input(|i| {
            (
                i.viewport().fullscreen,
                i.viewport().maximized,
                i.viewport().monitor_size,
            )
        });
        if fullscreen.unwrap_or(false) || maximized.unwrap_or(false) {
            return;
        }
        // 影片像素 → egui 點數，高 DPI 螢幕上才會是 1:1 顯示
        let video = vec2(w as f32, h as f32) / ctx.pixels_per_point();
        let max = monitor.unwrap_or(vec2(1920.0, 1080.0)) * 0.8 - vec2(0.0, self.controls_height);
        let fit = (max.x / video.x).min(max.y / video.y);
        // 原尺寸優先；太小的影片（例如 320×240）放大到 640 寬；兩者都不超過螢幕
        let want = if video.x < 640.0 { 640.0 / video.x } else { 1.0 };
        let size = video * want.min(fit);
        // 直式影片很窄，控制列放不下，兩側補黑邊
        let width = size.x.max(MIN_WINDOW_WIDTH);
        let target = vec2(width, size.y + self.controls_height);
        eprintln!("[vitascope] 視窗配合影片 {w}×{h} → {:.0}×{:.0}", target.x, target.y);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(target));
    }

    /// 測試用：直接設定檢查更新的結果（不連網）
    #[doc(hidden)]
    pub fn set_update_status(&mut self, status: UpdateStatus) {
        self.update_status = Some(Arc::new(Mutex::new(status)));
    }

    /// 「關於」視窗：作者、GitHub、授權、播放引擎版本、檢查更新
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let status = self
            .update_status
            .as_ref()
            .and_then(|s| s.lock().ok().map(|g| g.clone()));
        let mut close = false;
        let mut start_check = false;
        let mut open_releases = false;
        let mut dismiss_update = false;

        let modal = egui::Modal::new(Id::new("about")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.vertical_centered(|ui| {
                ui.heading(APP_NAME);
                ui.label(format!("版本 {}", update::current_version()));
            });
            ui.add_space(8.0);
            ui.label("跨平台影片播放器，以 libmpv 為播放引擎。");
            ui.add_space(8.0);
            egui::Grid::new("about_grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label("作者");
                    ui.hyperlink_to(update::AUTHOR, update::AUTHOR_URL);
                    ui.end_row();
                    ui.label("GitHub");
                    ui.hyperlink_to("acer1204/VitaScope", update::REPO_URL);
                    ui.end_row();
                    ui.label("授權");
                    ui.hyperlink_to("GPL-3.0-or-later（開放原始碼）", update::LICENSE_URL);
                    ui.end_row();
                    ui.label("播放引擎");
                    ui.label(&self.engine_versions);
                    ui.end_row();
                });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("可以自由使用、修改、散布；散布修改後的版本時，也必須公開原始碼。")
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.separator();

            match &status {
                None => {
                    if ui.button("檢查更新").clicked() {
                        start_check = true;
                    }
                }
                Some(UpdateStatus::Checking) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("正在檢查更新…");
                    });
                }
                Some(UpdateStatus::UpToDate { latest }) => {
                    ui.label(format!("已經是最新版本（{latest}）"));
                }
                Some(UpdateStatus::NoRelease) => {
                    ui.label("GitHub 上還沒有發佈任何版本");
                }
                Some(UpdateStatus::Failed(e)) => {
                    ui.colored_label(Color32::from_rgb(0xff, 0x8a, 0x80), format!("檢查更新失敗：{e}"));
                    ui.horizontal(|ui| {
                        if ui.button("再試一次").clicked() {
                            start_check = true;
                        }
                        ui.hyperlink_to("開啟發佈頁面", update::RELEASES_URL);
                    });
                }
                Some(UpdateStatus::Available { latest }) => {
                    ui.label(format!(
                        "有新版本 {latest}（目前 {}），要開啟下載頁面嗎？",
                        update::current_version()
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("是").clicked() {
                            open_releases = true;
                        }
                        if ui.button("否").clicked() {
                            dismiss_update = true;
                        }
                    });
                }
            }
            ui.separator();
            ui.vertical_centered(|ui| {
                if ui.button("關閉").clicked() {
                    close = true;
                }
            });
        });

        if start_check {
            let ctx = ctx.clone();
            self.update_status = Some(update::check_in_background(move || ctx.request_repaint()));
        }
        if open_releases {
            ctx.open_url(egui::OpenUrl::new_tab(update::RELEASES_URL));
            self.update_status = None;
        }
        if dismiss_update {
            self.update_status = None;
        }
        if close || modal.should_close() {
            self.about_open = false;
            // 下次打開時可以重新檢查（除非還在檢查中）
            if status != Some(UpdateStatus::Checking) {
                self.update_status = None;
            }
        }
    }

    /// 全螢幕的控制列浮在畫面上時，把字幕往上推，不要被蓋住。
    /// `overlay`：控制列高度佔畫面高度的比例，0 = 沒有顯示
    fn lift_subtitles(&mut self, overlay: f32) {
        // mpv 的 sub-margin-y 以「畫面高 720」為單位，預設 22
        let margin = 22 + (overlay * 720.0).round() as i64;
        if margin != self.sub_margin {
            self.sub_margin = margin;
            let _ = self.player.mpv().set_property("sub-margin-y", margin);
        }
    }

    /// 記下一般模式的視窗位置大小；全螢幕不記，最大化只記旗標（還原時回到原本大小）
    fn remember_window(&mut self, ctx: &egui::Context) {
        let (fullscreen, maximized, outer, inner) = ctx.input(|i| {
            let v = i.viewport();
            (
                v.fullscreen.unwrap_or(false),
                v.maximized.unwrap_or(false),
                v.outer_rect,
                v.inner_rect,
            )
        });
        if fullscreen {
            return;
        }
        let previous = self.settings.window;
        self.settings.window = if maximized {
            previous.map(|g| WindowGeometry { maximized: true, ..g })
        } else if let (Some(outer), Some(inner)) = (outer, inner) {
            Some(WindowGeometry {
                pos: outer.min.into(),
                size: inner.size().into(),
                maximized: false,
            })
        } else {
            previous
        };
    }

    fn controls_visible(&self, ctx: &egui::Context, fullscreen: bool) -> bool {
        if !fullscreen {
            return true;
        }
        let st = &self.player.state;
        let idle = self.last_activity.elapsed();
        let menu_open = egui::Popup::is_any_open(ctx);
        let visible = !st.loaded
            || st.paused
            || idle < HIDE_AFTER
            || self.pointer_over_controls
            || self.seek_drag.is_some()
            || menu_open;
        if visible && st.loaded && !st.paused {
            // 時間到要重繪一次，控制列才會消失
            ctx.request_repaint_after(HIDE_AFTER.saturating_sub(idle) + Duration::from_millis(50));
        }
        visible
    }

    // ───────────── 畫面 ─────────────

    fn video_area(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, Sense::click());
        let st = &self.player.state;

        if (st.loaded || st.loading)
            && let Some(video) = &self.video
        {
            video.paint(ui, rect);
        }
        if st.loaded && !st.has_video() && !st.tracks.is_empty() {
            // 純音訊：顯示檔名
            let title = st.title.clone().unwrap_or_default();
            let mut ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
            );
            ui.label(
                egui::RichText::new(format!("♪  {title}"))
                    .size(22.0)
                    .color(Color32::from_gray(200)),
            );
        }
        if !st.loaded && !st.loading {
            self.placeholder(ui, rect);
        }

        if response.double_clicked() {
            // 第一下單擊已經切換過暫停，這裡切回來，結果只有全螢幕改變（跟 PotPlayer 一樣）
            self.run(ui.ctx(), Action::TogglePause);
            self.run(ui.ctx(), Action::ToggleFullscreen);
            self.osd = None;
        } else if response.clicked() {
            self.run(ui.ctx(), Action::TogglePause);
        }

        // 拖曳檔案到視窗上方時的提示
        if ui.ctx().input(|i| !i.raw.hovered_files.is_empty()) {
            ui.painter()
                .rect_filled(rect, CornerRadius::ZERO, Color32::from_black_alpha(160));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "放開以播放",
                FontId::proportional(28.0),
                Color32::WHITE,
            );
        }

        self.paint_osd(ui, rect);
    }

    /// 沒有開檔時的畫面。用一般的 label（不是直接畫字），螢幕閱讀器和介面測試才讀得到
    fn placeholder(&self, ui: &mut egui::Ui, rect: Rect) {
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink(40.0))
                .layout(Layout::top_down(Align::Center)),
        );
        ui.add_space((rect.height() / 2.0 - 90.0).max(0.0));
        ui.label(egui::RichText::new(APP_NAME).size(32.0).color(Color32::from_gray(220)));
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("把影片拖放到這裡，或按 Ctrl+O 開啟檔案")
                .size(16.0)
                .color(Color32::from_gray(140)),
        );
        // 兩種錯誤都要顯示：影片畫面初始化失敗時，開檔錯誤也不能被蓋掉
        for msg in [self.fatal.as_deref(), self.player.state.last_error.as_deref()]
            .into_iter()
            .flatten()
        {
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(msg)
                    .size(15.0)
                    .color(Color32::from_rgb(0xff, 0x8a, 0x80)),
            );
        }
    }

    fn paint_osd(&mut self, ui: &egui::Ui, rect: Rect) {
        let Some((text, since)) = &self.osd else { return };
        let age = since.elapsed();
        if age > OSD_DURATION {
            self.osd = None;
            return;
        }
        ui.ctx().request_repaint_after(OSD_DURATION - age);
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(text.clone(), FontId::proportional(20.0), Color32::WHITE);
        let pos = rect.left_top() + vec2(20.0, 20.0);
        let bg = Rect::from_min_size(pos, galley.size()).expand2(vec2(12.0, 6.0));
        painter.rect_filled(bg, CornerRadius::same(6), Color32::from_black_alpha(150));
        painter.galley(pos, galley, Color32::WHITE);
    }

    /// 控制列：上排進度條，下排按鈕
    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
        self.progress_bar(ui);
        ui.horizontal(|ui| {
            let st = &self.player.state;
            let loaded = st.loaded;
            let play_icon = if !st.paused && loaded { "⏸" } else { "▶" };
            if ui
                .add_enabled(loaded, icon_button(play_icon))
                .on_hover_text("播放 / 暫停（空白鍵）")
                .clicked()
            {
                self.run(ui.ctx(), Action::TogglePause);
            }
            if ui.add_enabled(loaded, icon_button("⏹")).on_hover_text("停止").clicked() {
                self.run(ui.ctx(), Action::Stop);
            }
            let st = &self.player.state;
            let pos = self.seek_drag.unwrap_or(st.time_pos);
            let time = if loaded {
                format!("{} / {}", fmt_time(pos), fmt_time(st.duration.unwrap_or(0.0)))
            } else {
                "--:-- / --:--".to_owned()
            };
            ui.label(egui::RichText::new(time).monospace());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(icon_button("⛶")).on_hover_text("全螢幕（F / Enter）").clicked() {
                    self.run(ui.ctx(), Action::ToggleFullscreen);
                }
                if ui.add(icon_button("🗁")).on_hover_text("開啟檔案（Ctrl+O）").clicked() {
                    self.run(ui.ctx(), Action::Open);
                }
                if ui.add(icon_button("ℹ")).on_hover_text("關於影戲（F1）").clicked() {
                    self.run(ui.ctx(), Action::About);
                }
                self.track_menu(ui, TrackKind::Sub, "字幕");
                self.track_menu(ui, TrackKind::Audio, "音軌");
                self.volume_controls(ui);
            });
        });
    }

    fn volume_controls(&mut self, ui: &mut egui::Ui) {
        let st = &self.player.state;
        let mut volume = st.volume;
        let slider = egui::Slider::new(&mut volume, 0.0..=100.0)
            .show_value(false)
            .trailing_fill(true);
        let response = ui
            .add_sized([90.0, 20.0], slider)
            .on_hover_text(format!("音量 {:.0}%（↑ ↓）", st.volume));
        if response.changed() {
            let _ = self.player.set_volume(volume);
            if st.muted {
                let _ = self.player.set_mute(false);
            }
        }
        let icon = if self.player.state.muted || self.player.state.volume == 0.0 {
            "🔇"
        } else {
            "🔊"
        };
        if ui.add(icon_button(icon)).on_hover_text("靜音（M）").clicked() {
            self.run(ui.ctx(), Action::ToggleMute);
        }
    }

    fn track_menu(&mut self, ui: &mut egui::Ui, kind: TrackKind, name: &str) {
        let tracks: Vec<(i64, String, bool)> = self
            .player
            .state
            .tracks_of(kind)
            .map(|t| (t.id, t.label(), t.selected))
            .collect();
        let none_selected = !tracks.iter().any(|t| t.2);
        let mut choice: Option<Option<i64>> = None;
        ui.add_enabled_ui(!tracks.is_empty(), |ui| {
            ui.menu_button(name, |ui| {
                if kind == TrackKind::Sub && ui.selectable_label(none_selected, "關閉字幕").clicked() {
                    choice = Some(None);
                }
                for (id, label, selected) in &tracks {
                    if ui.selectable_label(*selected, label).clicked() {
                        choice = Some(Some(*id));
                    }
                }
            });
        });
        if let Some(id) = choice {
            self.select_track(kind, id);
        }
    }

    fn progress_bar(&mut self, ui: &mut egui::Ui) {
        let st = &self.player.state;
        let duration = st.duration.unwrap_or(0.0);
        let can_seek = st.loaded && st.seekable && duration > 0.0;
        let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::click_and_drag());
        let active = can_seek && (response.hovered() || response.dragged());
        // 自己畫的元件也要有無障礙資訊：螢幕閱讀器、介面測試才找得到
        response.widget_info(|| egui::WidgetInfo::slider(can_seek, st.time_pos, "進度"));

        let time_at = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration;
        if can_seek {
            if let Some(p) = response.interact_pointer_pos() {
                let t = time_at(p.x);
                if response.dragged() && self.seek_drag.is_none_or(|old| (old - t).abs() > 0.05) {
                    // 拖曳中跳到關鍵影格（快），放開時再精準跳轉
                    let _ = self.player.seek_to(t, false);
                    self.seek_drag = Some(t);
                    self.seek_released = false;
                }
                if response.drag_stopped() || response.clicked() {
                    let _ = self.player.seek_to(t, true);
                    self.seek_drag = Some(t);
                    self.seek_released = true;
                }
            }
            if response.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
        }

        let painter = ui.painter();
        let thickness = if active { 6.0 } else { 4.0 };
        let bar = Rect::from_center_size(rect.center(), vec2(rect.width(), thickness));
        painter.rect_filled(bar, CornerRadius::same(3), Color32::from_gray(70));
        let pos = self.seek_drag.unwrap_or(st.time_pos);
        let frac = if duration > 0.0 {
            (pos / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let played = Rect::from_min_max(bar.min, pos2(bar.left() + bar.width() * frac, bar.max.y));
        painter.rect_filled(played, CornerRadius::same(3), ACCENT);
        if active {
            painter.circle(
                pos2(played.right(), bar.center().y),
                7.0,
                Color32::WHITE,
                Stroke::new(2.0, ACCENT),
            );
        }

        // 滑鼠停在進度條上：顯示該位置的時間
        if can_seek && let Some(hover) = response.hover_pos() {
            let label = fmt_time(time_at(hover.x));
            let layer = egui::LayerId::new(egui::Order::Tooltip, Id::new("seek_hover"));
            let p = ui.ctx().layer_painter(layer);
            let galley = p.layout_no_wrap(label, FontId::monospace(13.0), Color32::WHITE);
            let size = galley.size();
            let x = hover
                .x
                .clamp(rect.left() + size.x / 2.0 + 6.0, rect.right() - size.x / 2.0 - 6.0);
            let text_pos = pos2(x - size.x / 2.0, rect.top() - size.y - 8.0);
            p.rect_filled(
                Rect::from_min_size(text_pos, size).expand2(vec2(6.0, 3.0)),
                CornerRadius::same(4),
                Color32::from_black_alpha(200),
            );
            p.galley(text_pos, galley, Color32::WHITE);
        }
    }
}

impl eframe::App for VitascopeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for ev in self.player.poll() {
            self.on_player_event(ev);
        }
        if ctx.input(|i| i.pointer.delta() != Vec2::ZERO || i.pointer.any_down()) {
            self.last_activity = Instant::now();
        }
        self.handle_keys(ctx);
        self.handle_drops(ctx);
        self.update_title(ctx);
        if self.frames >= 2 {
            if std::mem::take(&mut self.start_fullscreen) {
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                self.fit_window_pending = false;
            }
            self.fit_window(ctx);
        }
        if self.player.state.hwdec != self.logged_hwdec {
            self.logged_hwdec = self.player.state.hwdec.clone();
            if let Some(hw) = &self.logged_hwdec {
                eprintln!("[vitascope] 解碼器：{}", if hw == "no" { "軟解" } else { hw });
            }
        }
        if self.frames >= 2 {
            self.remember_window(ctx);
        }
        if let Some(shot) = &mut self.autoshot {
            shot.tick(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let ctx = ui.ctx().clone();
        let fullscreen = is_fullscreen(&ctx);
        let show_controls = self.controls_visible(&ctx, fullscreen);

        let panel_frame = Frame::NONE
            .fill(Color32::from_gray(24))
            .inner_margin(Margin::symmetric(10, 6));
        if !fullscreen {
            let r = egui::Panel::bottom("controls")
                .frame(panel_frame)
                .resizable(false)
                .show(ui, |ui| self.controls(ui));
            self.controls_height = r.response.rect.height();
            self.pointer_over_controls = false;
        }

        egui::CentralPanel::no_frame()
            .frame(Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.video_area(ui));

        let mut overlay_height = 0.0;
        if fullscreen {
            if show_controls {
                let screen = ctx.content_rect();
                let r = egui::Area::new(Id::new("overlay_controls"))
                    .anchor(Align2::LEFT_BOTTOM, Vec2::ZERO)
                    .show(&ctx, |ui| {
                        ui.set_width(screen.width());
                        Frame::NONE
                            .fill(Color32::from_black_alpha(170))
                            .inner_margin(Margin::symmetric(16, 10))
                            .show(ui, |ui| {
                                ui.set_width(screen.width() - 32.0);
                                self.controls(ui);
                            });
                    });
                self.pointer_over_controls = r.response.contains_pointer();
                overlay_height = r.response.rect.height() / screen.height();
            } else {
                self.pointer_over_controls = false;
                ctx.set_cursor_icon(CursorIcon::None);
            }
        }
        self.lift_subtitles(overlay_height);
        self.about_window(&ctx);
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let Some(video) = &self.video {
            video.destroy(gl);
        }
        self.settings.volume = self.player.state.volume;
        self.settings.muted = self.player.state.muted;
        if let Err(e) = self.settings.save() {
            eprintln!("[vitascope] 無法儲存設定：{e}");
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

fn icon_button(icon: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(icon).size(16.0)).min_size(vec2(32.0, 26.0))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// 「mpv v0.41.0-1102-g6c092d978」「N-127218-g47313ad3f」→「mpv 0.41.0 · FFmpeg N-127218」
/// （去掉 git 雜湊，「關於」視窗才放得下）
fn short_versions(mpv: &str, ffmpeg: &str) -> String {
    let mpv = mpv.trim_start_matches("mpv ").trim_start_matches('v');
    let mpv = mpv.split('-').next().unwrap_or(mpv);
    // FFmpeg 每日建置是「N-<編號>-g<雜湊>」，正式版是「7.1.1」之類
    let parts: Vec<&str> = ffmpeg.split('-').collect();
    let ffmpeg = if parts.first() == Some(&"N") && parts.len() > 1 {
        format!("N-{}", parts[1])
    } else {
        parts.first().copied().unwrap_or(ffmpeg).to_owned()
    };
    format!("mpv {mpv} · FFmpeg {ffmpeg}")
}

/// Mesa 的軟體繪圖器：llvmpipe、softpipe、舊的 swrast（「Software Rasterizer」）
fn is_mesa_software_renderer(renderer: &str) -> bool {
    let r = renderer.to_ascii_lowercase();
    ["llvmpipe", "softpipe", "software rasterizer"]
        .iter()
        .any(|name| r.contains(name))
}

/// 使用者用 VITASCOPE_MPV_OPTS 自己指定了這個 mpv 選項（就不自動調整）
fn mpv_opts_override(name: &str) -> bool {
    std::env::var("VITASCOPE_MPV_OPTS")
        .is_ok_and(|opts| opts.split_whitespace().any(|kv| kv.split('=').next() == Some(name)))
}

/// 秒數 → 「1:23:45」或「03:21」
pub fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_time, is_mesa_software_renderer, short_versions};

    #[test]
    fn formats_time() {
        assert_eq!(fmt_time(0.0), "00:00");
        assert_eq!(fmt_time(83.4), "01:23");
        assert_eq!(fmt_time(3600.0 + 23.0 * 60.0 + 45.0), "1:23:45");
        assert_eq!(fmt_time(-3.0), "00:00");
    }

    #[test]
    fn shortens_engine_versions() {
        assert_eq!(
            short_versions("mpv v0.41.0-1102-g6c092d978", "N-127218-g47313ad3f"),
            "mpv 0.41.0 · FFmpeg N-127218"
        );
        // Linux 發行版的套件
        assert_eq!(
            short_versions("mpv 0.37.0", "6.1.1-3ubuntu5"),
            "mpv 0.37.0 · FFmpeg 6.1.1"
        );
    }

    #[test]
    fn detects_mesa_software_renderers() {
        assert!(is_mesa_software_renderer("llvmpipe (LLVM 20.1.2, 256 bits)"));
        assert!(is_mesa_software_renderer("softpipe"));
        assert!(is_mesa_software_renderer("Software Rasterizer"));
        // 實體顯示卡、macOS 的軟體繪圖（畫面正常）都不算
        assert!(!is_mesa_software_renderer("NVIDIA GeForce RTX 3090/PCIe/SSE2"));
        assert!(!is_mesa_software_renderer("Mesa Intel(R) UHD Graphics 630 (CFL GT2)"));
        assert!(!is_mesa_software_renderer("Apple Software Renderer"));
    }
}
