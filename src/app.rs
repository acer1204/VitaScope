//! 播放器視窗：影片畫面、控制列、快捷鍵、全螢幕。

use crate::autoshot::AutoShot;
use crate::formats;
use crate::history::History;
use crate::player::{MAX_SPEED, MIN_SPEED, Player, PlayerEvent, TrackKind};
use crate::playlist::Playlist;
use crate::settings::{Settings, WindowGeometry};
use crate::update::{self, UpdateStatus};
use crate::video::VideoView;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Rect,
    Sense, Stroke, Vec2, ViewportCommand, pos2, vec2,
};
use eframe::glow;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const APP_NAME: &str = "影戲 VitaScope";
/// 全螢幕時，滑鼠多久沒動就隱藏控制列
const HIDE_AFTER: Duration = Duration::from_secs(2);
const OSD_DURATION: Duration = Duration::from_millis(1500);
/// 控制列完整顯示需要的寬度（也是視窗的最小寬度）
pub const MIN_WINDOW_WIDTH: f32 = 640.0;
/// 播放中每隔多久把續播位置存起來（當機、關機時才不會整段遺失）
const AUTOSAVE_EVERY: Duration = Duration::from_secs(30);
/// 右鍵選單的播放速度選項
const SPEED_PRESETS: [f64; 10] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 3.0, 4.0];
const ACCENT: Color32 = Color32::from_rgb(0x4f, 0x9d, 0xff);
/// A-B 重播在進度條上的顏色
const AB_COLOR: Color32 = Color32::from_rgb(0xff, 0xc1, 0x07);
/// 起始畫面列出幾個最近開啟的檔案
const RECENT_ON_START: usize = 6;
/// 右鍵選單列出幾個最近開啟的檔案
const RECENT_IN_MENU: usize = 10;

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
    /// 播放清單的上一個 / 下一個檔案
    PrevFile,
    NextFile,
    /// 播放速度加快（+1）或減慢（-1）0.1 倍
    SpeedStep(i32),
    SpeedReset,
    /// 逐格：true = 前進
    FrameStep(bool),
    AbLoop,
    /// 跳到前 / 後幾個章節
    Chapter(i64),
    /// 回到開頭
    Restart,
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
    /// 最近開啟的檔案、續播位置
    history: History,
    /// 同資料夾的播放清單（開網址時沒有）
    playlist: Option<Playlist>,
    /// 上一幀是否已經播到結尾（偵測「剛播完」，自動接下一個）
    was_eof: bool,
    /// 滑鼠滾輪還沒湊滿一格的量（觸控板的捲動是連續的）
    wheel: f32,
    /// 控制列右側（音量、選單、按鈕）上一幀的寬度，用來判斷左側還放不放得下速度標示
    right_controls_width: f32,
    egui_ctx: egui::Context,
    /// 背景掃描資料夾的結果（網路磁碟上的大資料夾要掃一陣子，不能卡住畫面）
    playlist_scan: Option<Receiver<Playlist>>,
    /// 上一幀是否正在播放（暫停中逐格、跳轉到結尾不算「播完」，不自動接下一個）
    was_playing: bool,
    /// 上一幀是否暫停（剛暫停時順便存續播位置）
    was_paused: bool,
    last_autosave: Instant,
    /// 開檔的次數；拖曳進度條時記下是哪個檔案開始拖的，換檔後就不再跟著拖曳跳轉
    file_gen: u64,
    drag_gen: Option<u64>,
    /// 上一次單擊影片畫面的時間（egui 的時間），雙擊要兩下都點在畫面上才算
    video_click_time: Option<f64>,
    /// 拖曳進度條期間播到結尾（mpv 會自動暫停）：放開後要繼續播，才會接著播下一個檔案
    resume_after_drag: bool,
}

/// 啟動參數
#[derive(Default)]
pub struct Launch {
    pub file: Option<PathBuf>,
    pub fullscreen: bool,
    pub autoshot: Option<AutoShot>,
    /// 播放紀錄；預設只放在記憶體（自動測試用），播放器用 `History::load()`
    pub history: History,
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
            history: launch.history,
            playlist: None,
            was_eof: false,
            wheel: 0.0,
            right_controls_width: 0.0,
            egui_ctx: cc.egui_ctx.clone(),
            playlist_scan: None,
            was_playing: false,
            was_paused: false,
            last_autosave: Instant::now(),
            file_gen: 0,
            drag_gen: None,
            video_click_time: None,
            resume_after_drag: false,
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

    /// 播放紀錄（介面測試用）
    pub fn history(&self) -> &History {
        &self.history
    }

    /// 目前的播放清單（介面測試用）
    pub fn playlist(&self) -> Option<&Playlist> {
        self.playlist.as_ref()
    }

    /// 目前的設定（介面測試用）
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    // ───────────── 操作 ─────────────

    fn open(&mut self, path: &Path) {
        // 先記下目前的檔案看到哪裡
        self.remember_position();
        let path = if is_url(&path.to_string_lossy()) {
            path.to_path_buf()
        } else {
            // 續播紀錄、播放清單都用完整路徑比對（命令列可能給相對路徑）
            std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
        };
        // 同一份清單裡的檔案只移動位置；其他檔案先自己成一份清單，背景再掃描同資料夾的檔案
        let in_list = self.playlist.as_mut().is_some_and(|list| list.select(&path));
        if !in_list {
            self.playlist_scan = None;
            self.playlist = None;
            if !is_url(&path.to_string_lossy()) {
                self.playlist = Some(Playlist::from_files(vec![path.clone()]));
                let (tx, rx) = mpsc::channel();
                let (scan_path, ctx) = (path.clone(), self.egui_ctx.clone());
                std::thread::spawn(move || {
                    if tx.send(Playlist::for_file(&scan_path)).is_ok() {
                        ctx.request_repaint();
                    }
                });
                self.playlist_scan = Some(rx);
            }
        }
        self.video_click_time = None;
        // 開新檔一律從播放開始（mpv 會沿用上一個檔案的暫停狀態）；A-B 重播也會沿用，要清掉
        let _ = self.player.set_pause(false);
        let _ = self.player.clear_ab_loop();
        if let Err(e) = self.player.open(&path.to_string_lossy()) {
            self.player.state.last_error = Some(format!("無法開啟：{e}"));
        }
    }

    /// 打開最近開啟清單裡的檔案。不先檢查檔案在不在：網路磁碟暫時連不上時檢查會卡住畫面，
    /// 也分不出是「刪掉了」還是「暫時連不上」；打不開時 mpv 會回報原因
    fn open_recent(&mut self, path: &str) {
        self.open(Path::new(path));
    }

    /// 背景掃描完同資料夾的檔案：換成完整的清單（掃描期間已經換到別的資料夾就不理）
    fn poll_playlist_scan(&mut self) {
        let Some(rx) = &self.playlist_scan else { return };
        match rx.try_recv() {
            Ok(mut list) => {
                self.playlist_scan = None;
                let current = self.playlist.as_ref().and_then(|l| l.current()).map(Path::to_path_buf);
                if let Some(current) = current
                    && list.select(&current)
                {
                    self.playlist = Some(list);
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.playlist_scan = None,
        }
    }

    /// 記下目前的檔案看到哪裡（換檔、停止、關閉時）
    fn remember_position(&mut self) {
        let st = &self.player.state;
        if !st.loaded {
            return;
        }
        let (Some(path), Some(duration)) = (st.path.clone(), st.duration) else {
            return;
        };
        if is_url(&path) {
            return;
        }
        let time = st.time_pos;
        self.update_history(|h| h.remember(&path, time, duration));
        self.last_autosave = Instant::now();
    }

    /// 修改播放紀錄並存檔
    fn update_history(&mut self, change: impl FnOnce(&mut History)) {
        if let Err(e) = self.history.update(change) {
            eprintln!("[vitascope] 無法儲存播放紀錄：{e}");
        }
    }

    fn save_settings(&mut self) {
        self.settings.volume = self.player.state.volume;
        self.settings.muted = self.player.state.muted;
        if let Err(e) = self.settings.save() {
            eprintln!("[vitascope] 無法儲存設定：{e}");
        }
    }

    /// 播放中定時、以及剛暫停時，存一下續播位置（當機、關機時才不會整段遺失）
    fn autosave(&mut self) {
        let st = &self.player.state;
        let paused_now = st.loaded && st.paused;
        let just_paused = paused_now && !self.was_paused;
        self.was_paused = paused_now;
        let playing = st.loaded && !st.paused;
        if just_paused || (playing && self.last_autosave.elapsed() >= AUTOSAVE_EVERY) {
            self.remember_position();
        }
    }

    /// 播放清單的上一個 / 下一個檔案
    fn step_file(&mut self, forward: bool) {
        let target = self
            .playlist
            .as_ref()
            .and_then(|l| if forward { l.next() } else { l.prev() });
        match target.map(Path::to_path_buf) {
            Some(path) => {
                self.open(&path);
                let (pos, len) = self.playlist.as_ref().map_or((1, 1), |l| (l.position(), l.len()));
                let dir = if forward { "下一個" } else { "上一個" };
                self.osd(format!("{dir}（{pos}/{len}）：{}", file_name(&path)));
            }
            None => self.osd(if forward {
                "已經是最後一個檔案"
            } else {
                "已經是第一個檔案"
            }),
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
                self.remember_position();
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
                // 直接問 mpv 目前的音量：連續捲動滾輪時，屬性變化的通知可能還沒送到
                let current = self.player.get_f64("volume").unwrap_or(st.volume);
                let v = (current + delta).clamp(0.0, 100.0);
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
            Action::PrevFile => self.step_file(false),
            Action::NextFile => self.step_file(true),
            Action::SpeedStep(dir) => {
                let current = self.player.get_f64("speed").unwrap_or(st.speed);
                let speed = ((current + 0.1 * f64::from(dir)) * 100.0).round() / 100.0;
                self.set_speed(speed);
            }
            Action::SpeedReset => self.set_speed(1.0),
            Action::FrameStep(forward) if loaded => {
                let _ = self.player.frame_step(forward);
                self.osd(if forward {
                    "逐格前進 ▶"
                } else {
                    "◀ 逐格後退"
                });
            }
            Action::AbLoop if loaded => {
                let msg = match st.ab_loop {
                    [None, _] => format!("A-B 重播：起點 {}", fmt_time(st.time_pos)),
                    [Some(a), None] => format!("A-B 重播：{} → {}", fmt_time(a), fmt_time(st.time_pos)),
                    [Some(_), Some(_)] => "取消 A-B 重播".to_owned(),
                };
                let _ = self.player.cycle_ab_loop();
                self.osd(msg);
            }
            Action::Chapter(delta) if loaded => self.step_chapter(delta),
            Action::Restart if loaded && st.seekable => {
                let _ = self.player.seek_to(0.0, true);
                // 播完停在最後一格（暫停中）時也要開始播
                let _ = self.player.set_pause(false);
                self.osd("從頭播放");
            }
            _ => {}
        }
    }

    fn set_speed(&mut self, speed: f64) {
        let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        let _ = self.player.set_speed(speed);
        self.osd(format!("速度 {}×", fmt_speed(speed)));
    }

    /// 跳到前 / 後幾個章節，OSD 顯示章節名稱。目前在哪一章交給 mpv 判斷：
    /// 跳完章節後畫面的時間常常比章節時間早一點點，自己用時間算會卡在同一章；
    /// mpv 也會處理「進入本章超過幾秒，往回跳先回到本章開頭」
    fn step_chapter(&mut self, delta: i64) {
        let total = self.player.state.chapters.len() as i64;
        if total == 0 {
            self.osd("這個檔案沒有章節");
            return;
        }
        let current = self.player.current_chapter().unwrap_or(-1);
        // 最後一章再往後，mpv 會跳到片尾（然後自動播下一個檔案），這裡先擋下來
        if delta > 0 && current + delta >= total {
            self.osd("已經是最後一章");
            return;
        }
        let _ = self.player.add_chapter(delta);
        let now = self.player.current_chapter().unwrap_or(current + delta);
        let msg = match usize::try_from(now) {
            Ok(i) => format!(
                "章節 {}/{total}：{}",
                i + 1,
                chapter_label(&self.player.state.chapters, i)
            ),
            Err(_) => "回到開頭".to_owned(),
        };
        self.osd(msg);
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
                self.was_eof = false;
                self.was_playing = false;
                self.resume_after_drag = false;
                self.file_gen += 1;
            }
            PlayerEvent::FileLoaded => self.on_file_loaded(),
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

    /// 檔案載入完成：加進最近開啟，有上次的位置就從那裡繼續
    fn on_file_loaded(&mut self) {
        let Ok(path) = self.player.get_string("path") else {
            return;
        };
        if is_url(&path) {
            return;
        }
        self.update_history(|h| h.add_recent(&path));
        // 自動截圖要固定的畫面，不續播
        if self.settings.resume
            && self.autoshot.is_none()
            && let Some(t) = self.history.resume_point(&path)
        {
            // 同名檔案可能被換成較短的版本：位置已經不合理就不跳（會直接播完、跳下一個檔案）
            let duration = self.player.get_f64("duration").ok();
            if duration.is_none_or(|d| crate::history::worth_resuming(t, d)) {
                let _ = self.player.seek_to(t, true);
                self.osd(format!("從 {} 繼續播放（Home 從頭播放）", fmt_time(t)));
            } else {
                self.update_history(|h| h.forget(&path));
            }
        }
    }

    /// 播完時自動播放清單的下一個檔案。只算「播放中播到結尾」：
    /// 暫停中逐格、跳轉到結尾不算；拖曳進度條期間也先不動，放開後再說
    fn auto_next(&mut self) {
        if self.seek_drag.is_some() || self.drag_gen.is_some() {
            let st = &self.player.state;
            if st.loaded && st.eof && self.was_playing {
                self.resume_after_drag = true;
            }
            return;
        }
        let st = &self.player.state;
        let eof = st.loaded && st.eof;
        let just_ended = eof && !self.was_eof && self.was_playing;
        self.was_eof = eof;
        // mpv 播到結尾時會同時設定暫停，所以看的是上一幀還在播放
        self.was_playing = st.loaded && !st.paused && !st.eof;
        if !just_ended || !self.settings.auto_next {
            return;
        }
        if let Some(next) = self.playlist.as_ref().and_then(|l| l.next()).map(Path::to_path_buf) {
            self.open(&next);
            let (pos, len) = self.playlist.as_ref().map_or((1, 1), |l| (l.position(), l.len()));
            self.osd(format!("下一個（{pos}/{len}）：{}", file_name(&next)));
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
            key(Modifiers::COMMAND, Key::PageUp, Action::Chapter(-1));
            key(Modifiers::COMMAND, Key::PageDown, Action::Chapter(1));
            key(Modifiers::NONE, Key::PageUp, Action::PrevFile);
            key(Modifiers::NONE, Key::PageDown, Action::NextFile);
            key(Modifiers::NONE, Key::C, Action::SpeedStep(1));
            key(Modifiers::NONE, Key::X, Action::SpeedStep(-1));
            key(Modifiers::NONE, Key::Z, Action::SpeedReset);
            key(Modifiers::NONE, Key::Period, Action::FrameStep(true));
            key(Modifiers::NONE, Key::Comma, Action::FrameStep(false));
            key(Modifiers::NONE, Key::L, Action::AbLoop);
            key(Modifiers::NONE, Key::Home, Action::Restart);
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
        let Some(first) = dropped.first().cloned() else { return };
        if formats::is_subtitle(&first) && self.player.state.loaded {
            match self.player.add_subtitle(&first.to_string_lossy()) {
                Ok(()) => self.osd(format!("載入字幕：{}", file_name(&first))),
                Err(e) => self.osd(format!("無法載入字幕：{e}")),
            }
            return;
        }
        // 一次拖放多個影音檔：播放清單就是這幾個檔案，依檔名排序
        //（拖放的順序跟系統有關，Windows 會把滑鼠抓著的那個檔案放在最前面）
        let mut media: Vec<PathBuf> = dropped
            .into_iter()
            .filter(|p| formats::media_kind(p).is_some())
            .collect();
        crate::playlist::sort_by_name(&mut media);
        match media.len() {
            0 => self.open(&first),
            1 => self.open(&media[0]),
            _ => {
                let first = media[0].clone();
                self.remember_position();
                self.playlist_scan = None;
                self.playlist = Some(Playlist::from_files(media));
                self.open(&first);
            }
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
        // 無障礙資訊：螢幕閱讀器、介面測試（例如打開右鍵選單）找得到影片畫面
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "影片畫面"));
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
        if !st.loaded
            && !st.loading
            && let Some(path) = self.placeholder(ui, rect)
        {
            self.open_recent(&path);
        }

        // 選單開著時點畫面只是關掉選單，不要順便暫停
        let menu_was_open = egui::Popup::is_any_open(ui.ctx());
        let now = ui.ctx().input(|i| i.time);
        let max_delay = ui.ctx().options(|o| o.input_options.max_double_click_delay);
        let first_click_on_video = self.video_click_time.is_some_and(|t| now - t <= max_delay);
        if menu_was_open {
            self.video_click_time = None;
        } else if response.double_clicked() {
            if first_click_on_video {
                // 第一下單擊已經切換過暫停，這裡切回來，結果只有全螢幕改變（跟 PotPlayer 一樣）
                self.run(ui.ctx(), Action::TogglePause);
                self.run(ui.ctx(), Action::ToggleFullscreen);
                self.osd = None;
            }
            // 第一下點在別的地方（例如起始畫面的「最近開啟」）：這一下不算
            self.video_click_time = None;
        } else if response.clicked() {
            self.video_click_time = Some(now);
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

        // 滑鼠滾輪調音量（比照 PotPlayer）
        let steps = self.wheel_steps(ui.ctx(), response.hovered());
        if steps != 0 {
            self.run(ui.ctx(), Action::Volume(5.0 * f64::from(steps)));
        }
        response.context_menu(|ui| self.context_menu(ui));

        self.paint_osd(ui, rect);
    }

    /// 這一幀滑鼠滾輪轉了幾格（往上為正）。觸控板的捲動是連續的，累積滿一格才算
    fn wheel_steps(&mut self, ctx: &egui::Context, over_video: bool) -> i32 {
        let delta: f32 = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel {
                        unit, delta, modifiers, ..
                    } if modifiers.is_none() => Some(match unit {
                        egui::MouseWheelUnit::Line => delta.y,
                        egui::MouseWheelUnit::Page => delta.y * 3.0,
                        egui::MouseWheelUnit::Point => delta.y / 50.0,
                    }),
                    _ => None,
                })
                .sum()
        });
        if !over_video {
            self.wheel = 0.0;
            return 0;
        }
        // 換方向就重新累積
        if self.wheel != 0.0 && delta != 0.0 && delta.signum() != self.wheel.signum() {
            self.wheel = 0.0;
        }
        self.wheel += delta;
        let steps = self.wheel.trunc();
        self.wheel -= steps;
        steps as i32
    }

    /// 在影片上按右鍵的選單
    fn context_menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut action = None;
        let mut open_recent = None;
        let mut set_speed = None;
        let mut seek_chapter = None;

        if menu_item(ui, true, "開啟檔案…", OPEN_SHORTCUT) {
            action = Some(Action::Open);
        }
        let recent: Vec<String> = self.history.recent.iter().take(RECENT_IN_MENU).cloned().collect();
        let mut clear_recent = false;
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button("最近開啟的檔案", |ui| {
                for p in &recent {
                    if ui.button(file_name(Path::new(p))).on_hover_text(p).clicked() {
                        open_recent = Some(p.clone());
                    }
                }
                ui.separator();
                if ui.button("清除清單").clicked() {
                    clear_recent = true;
                }
            });
        });
        ui.separator();

        let st = &self.player.state;
        let loaded = st.loaded;
        let (has_prev, has_next) = self
            .playlist
            .as_ref()
            .map_or((false, false), |l| (l.prev().is_some(), l.next().is_some()));
        if menu_item(ui, loaded, if loaded && !st.paused { "暫停" } else { "播放" }, "空白鍵") {
            action = Some(Action::TogglePause);
        }
        if menu_item(ui, loaded, "停止", "") {
            action = Some(Action::Stop);
        }
        if menu_item(ui, has_prev, "上一個檔案", "PgUp") {
            action = Some(Action::PrevFile);
        }
        if menu_item(ui, has_next, "下一個檔案", "PgDn") {
            action = Some(Action::NextFile);
        }
        let mut settings_changed = ui
            .checkbox(&mut self.settings.auto_next, "播完自動播放下一個")
            .changed();
        settings_changed |= ui.checkbox(&mut self.settings.resume, "從上次的位置繼續播放").changed();
        ui.separator();

        let speed = st.speed;
        ui.menu_button(format!("播放速度（{}×）", fmt_speed(speed)), |ui| {
            for preset in SPEED_PRESETS {
                let label = format!("{}×", fmt_speed(preset));
                if ui.selectable_label((speed - preset).abs() < 1e-6, label).clicked() {
                    set_speed = Some(preset);
                }
            }
            ui.separator();
            ui.weak("C 加快、X 減慢、Z 恢復正常");
        });
        if menu_item(ui, loaded, "逐格前進", ".") {
            action = Some(Action::FrameStep(true));
        }
        if menu_item(ui, loaded, "逐格後退", ",") {
            action = Some(Action::FrameStep(false));
        }
        let ab_label = match st.ab_loop {
            [None, _] => "A-B 重播：設定起點",
            [Some(_), None] => "A-B 重播：設定終點",
            [Some(_), Some(_)] => "取消 A-B 重播",
        };
        if menu_item(ui, loaded, ab_label, "L") {
            action = Some(Action::AbLoop);
        }
        if !st.chapters.is_empty() {
            let chapters = st.chapters.clone();
            let current = st.chapter;
            ui.menu_button("章節", |ui| {
                // 章節很多（例如整季合集）時選單會超出畫面，要能捲動
                let max_height = (ui.ctx().content_rect().height() - 80.0).max(120.0);
                egui::ScrollArea::vertical().max_height(max_height).show(ui, |ui| {
                    for (i, c) in chapters.iter().enumerate() {
                        let label = format!("{}  {}", fmt_time(c.time), chapter_label(&chapters, i));
                        if ui.selectable_label(current == Some(i), label).clicked() {
                            seek_chapter = Some(i);
                        }
                    }
                });
                ui.separator();
                ui.weak(format!("上一章 / 下一章：{CHAPTER_SHORTCUT}"));
            });
        }
        ui.separator();
        self.track_menu(ui, TrackKind::Audio, "音軌");
        self.track_menu(ui, TrackKind::Sub, "字幕");
        ui.separator();
        if menu_item(ui, true, "全螢幕", "F") {
            action = Some(Action::ToggleFullscreen);
        }
        if menu_item(ui, true, "關於影戲", "F1") {
            action = Some(Action::About);
        }

        if settings_changed {
            self.save_settings();
        }
        if clear_recent {
            self.update_history(History::clear_recent);
        }
        if let Some(path) = open_recent {
            self.open_recent(&path);
        }
        if let Some(speed) = set_speed {
            self.set_speed(speed);
        }
        if let Some(i) = seek_chapter {
            let _ = self.player.seek_chapter(i);
            let st = &self.player.state;
            let msg = format!(
                "章節 {}/{}：{}",
                i + 1,
                st.chapters.len(),
                chapter_label(&st.chapters, i)
            );
            self.osd(msg);
        }
        if let Some(a) = action {
            self.run(&ctx, a);
        }
    }

    /// 沒有開檔時的畫面。用一般的 label（不是直接畫字），螢幕閱讀器和介面測試才讀得到。
    /// 回傳使用者在「最近開啟」裡點選的檔案
    fn placeholder(&self, ui: &mut egui::Ui, rect: Rect) -> Option<String> {
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink(40.0))
                .layout(Layout::top_down(Align::Center)),
        );
        let recent: Vec<&String> = self.history.recent.iter().take(RECENT_ON_START).collect();
        let recent_height = if recent.is_empty() {
            0.0
        } else {
            40.0 + 24.0 * recent.len() as f32
        };
        ui.add_space((rect.height() / 2.0 - 90.0 - recent_height / 2.0).max(0.0));
        ui.label(egui::RichText::new(APP_NAME).size(32.0).color(Color32::from_gray(220)));
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("把影片拖放到這裡，或按 {OPEN_SHORTCUT} 開啟檔案"))
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
        // 最近開啟的檔案，點一下就開
        let mut chosen = None;
        if !recent.is_empty() {
            ui.add_space(28.0);
            ui.label(
                egui::RichText::new("最近開啟")
                    .size(14.0)
                    .color(Color32::from_gray(150)),
            );
            ui.add_space(4.0);
            for path in recent {
                let name = egui::RichText::new(file_name(Path::new(path)))
                    .size(14.0)
                    .color(Color32::from_gray(200));
                if ui
                    .add(egui::Button::new(name).frame(false))
                    .on_hover_text(path)
                    .clicked()
                {
                    chosen = Some(path.clone());
                }
            }
        }
        chosen
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
            let (has_prev, has_next) = self
                .playlist
                .as_ref()
                .map_or((false, false), |l| (l.prev().is_some(), l.next().is_some()));
            if ui
                .add_enabled(has_prev, icon_button("⏮"))
                .on_hover_text("上一個檔案（PgUp）")
                .clicked()
            {
                self.run(ui.ctx(), Action::PrevFile);
            }
            if ui
                .add_enabled(loaded, icon_button(play_icon))
                .on_hover_text("播放 / 暫停（空白鍵）")
                .clicked()
            {
                self.run(ui.ctx(), Action::TogglePause);
            }
            if ui
                .add_enabled(has_next, icon_button("⏭"))
                .on_hover_text("下一個檔案（PgDn）")
                .clicked()
            {
                self.run(ui.ctx(), Action::NextFile);
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
            // 速度不是 1× 時顯示在時間旁邊；視窗太窄、會擠到右邊的按鈕時就不顯示（OSD 和右鍵選單還看得到）
            if (st.speed - 1.0).abs() > 1e-6 {
                let font = ui.style().text_styles[&egui::TextStyle::Monospace].clone();
                let galley = ui
                    .painter()
                    .layout_no_wrap(format!("{}×", fmt_speed(st.speed)), font, ACCENT);
                let needed = galley.size().x + ui.spacing().item_spacing.x;
                if ui.available_width() >= self.right_controls_width + needed {
                    ui.label(galley).on_hover_text("播放速度（C 加快、X 減慢、Z 恢復正常）");
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(icon_button("⛶")).on_hover_text("全螢幕（F / Enter）").clicked() {
                    self.run(ui.ctx(), Action::ToggleFullscreen);
                }
                if ui
                    .add(icon_button("🗁"))
                    .on_hover_text(format!("開啟檔案（{OPEN_SHORTCUT}）"))
                    .clicked()
                {
                    self.run(ui.ctx(), Action::Open);
                }
                if ui.add(icon_button("ℹ")).on_hover_text("關於影戲（F1）").clicked() {
                    self.run(ui.ctx(), Action::About);
                }
                self.track_menu(ui, TrackKind::Sub, "字幕");
                self.track_menu(ui, TrackKind::Audio, "音軌");
                self.volume_controls(ui);
                self.right_controls_width = ui.min_rect().width();
            });
        });
    }

    fn volume_controls(&mut self, ui: &mut egui::Ui) {
        // 音量條短一點，窄視窗時左邊的時間、速度才放得下（Slider 的寬度看 spacing，不看 add_sized）
        ui.spacing_mut().slider_width = 70.0;
        let st = &self.player.state;
        let mut volume = st.volume;
        let slider = egui::Slider::new(&mut volume, 0.0..=100.0)
            .show_value(false)
            .trailing_fill(true);
        let response = ui
            .add_sized([70.0, 20.0], slider)
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
        // 拖曳只對開始拖的那個檔案有效：拖到結尾、換到下一個檔案後，不要繼續把新檔案也拖到結尾
        if response.drag_started() {
            self.drag_gen = Some(self.file_gen);
        }
        let drag_is_ours = self.drag_gen == Some(self.file_gen);
        if !response.is_pointer_button_down_on() && !response.drag_stopped() {
            self.drag_gen = None;
        }
        if can_seek {
            if let Some(p) = response.interact_pointer_pos() {
                let t = time_at(p.x);
                if !drag_is_ours && (response.dragged() || response.drag_stopped()) {
                    // 換檔前開始的拖曳：放開時也不跳轉
                } else if response.dragged() && self.seek_drag.is_none_or(|old| (old - t).abs() > 0.05) {
                    // 拖曳中跳到關鍵影格（快），放開時再精準跳轉
                    let _ = self.player.seek_to(t, false);
                    self.seek_drag = Some(t);
                    self.seek_released = false;
                }
                if (response.drag_stopped() && drag_is_ours) || response.clicked() {
                    let _ = self.player.seek_to(t, true);
                    if std::mem::take(&mut self.resume_after_drag) {
                        let _ = self.player.set_pause(false);
                    }
                    self.seek_drag = Some(t);
                    self.seek_released = true;
                }
                if response.drag_stopped() {
                    self.drag_gen = None;
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
        if duration > 0.0 {
            let x_of = |t: f64| bar.left() + bar.width() * (t / duration).clamp(0.0, 1.0) as f32;
            // A-B 重播：區段塗上顏色；只設了起點時畫一條線
            match st.ab_loop {
                [Some(a), Some(b)] => {
                    let section = Rect::from_x_y_ranges(x_of(a.min(b))..=x_of(a.max(b)), bar.y_range());
                    painter.rect_filled(section, CornerRadius::ZERO, AB_COLOR.gamma_multiply(0.6));
                }
                [Some(a), None] => {
                    let x = x_of(a);
                    painter.line_segment(
                        [pos2(x, bar.top() - 4.0), pos2(x, bar.bottom() + 4.0)],
                        Stroke::new(2.0, AB_COLOR),
                    );
                }
                _ => {}
            }
            // 章節：在進度條上切出間隔
            for c in st.chapters.iter().filter(|c| c.time > 0.0) {
                let x = x_of(c.time);
                painter.line_segment(
                    [pos2(x, bar.top() - 1.0), pos2(x, bar.bottom() + 1.0)],
                    Stroke::new(2.0, Color32::from_gray(24)),
                );
            }
        }
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
            let t = time_at(hover.x);
            let label = match st.chapter_at(t) {
                Some(i) => format!("{} · {}", fmt_time(t), chapter_label(&st.chapters, i)),
                None => fmt_time(t),
            };
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
        self.poll_playlist_scan();
        self.auto_next();
        self.autosave();
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
        self.remember_position();
        self.save_settings();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// 網址（串流、mpv 的 av:// 之類），不是本機檔案
fn is_url(path: &str) -> bool {
    path.contains("://")
}

/// 1.0 → 「1」、1.25 →「1.25」、0.5 →「0.5」
fn fmt_speed(speed: f64) -> String {
    let s = format!("{speed:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// 章節名稱；沒有名稱就用「第 n 章」
fn chapter_label(chapters: &[crate::player::Chapter], index: usize) -> String {
    chapters
        .get(index)
        .and_then(|c| c.title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("第 {} 章", index + 1))
}

/// 選單項目：文字 + 右側的快捷鍵說明，回傳是否被點選
fn menu_item(ui: &mut egui::Ui, enabled: bool, text: &str, shortcut: &str) -> bool {
    let mut button = egui::Button::new(text);
    if !shortcut.is_empty() {
        button = button.shortcut_text(shortcut);
    }
    ui.add_enabled(enabled, button).clicked()
}

/// 跳章節的快捷鍵說明
const CHAPTER_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "Cmd+PgUp / PgDn"
} else {
    "Ctrl+PgUp / PgDn"
};

/// 開檔快捷鍵的說明文字（macOS 用 Command 鍵）
const OPEN_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+O" } else { "Ctrl+O" };

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
    use super::{fmt_speed, fmt_time, is_mesa_software_renderer, short_versions};

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
    fn formats_speed() {
        assert_eq!(fmt_speed(1.0), "1");
        assert_eq!(fmt_speed(1.25), "1.25");
        assert_eq!(fmt_speed(0.5), "0.5");
        assert_eq!(fmt_speed(1.1), "1.1");
        assert_eq!(fmt_speed(4.0), "4");
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
