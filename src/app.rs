//! 播放器視窗：影片畫面、控制列、快捷鍵、全螢幕。

mod capture;
mod control_panel;
mod info_panel;
mod pacing;
mod playlist_panel;
mod preview;
mod quality;
mod settings_window;
mod shaders;
mod sound;
mod tuning_menu;

use crate::autoshot::AutoShot;
use crate::formats;
use crate::geometry::{self, ASPECTS, CROPS, Geometry, PAN_STEP, ZOOM_STEP};
use crate::history::History;
use crate::picture::{
    Adjust, AdjustKind, ChromaScaler, Deinterlace, Downscaler, Gamut, PictureDefaults, Quality, Strength, ToneCurve,
    Upscaler,
};
use crate::player::{AsyncKey, EngineCaps, MAX_SPEED, MIN_SPEED, Player, PlayerEvent, TrackKind};
use crate::playlist::Playlist;
use crate::settings::{Settings, SubStyle, WindowGeometry};
use crate::sound::{EqPreset, Leveling};
use crate::update::{self, UpdateStatus};
use crate::video::VideoView;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Layout, Margin, Modifiers, Rect,
    Sense, Stroke, Vec2, ViewportCommand, pos2, vec2,
};
use eframe::glow;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use pacing::{PacingStatus, PlatformProbe};

pub const APP_NAME: &str = "影戲 VitaScope";

/// 程式名稱（英文介面只寫 VitaScope）
pub fn app_name() -> &'static str {
    crate::tr!(APP_NAME, "VitaScope")
}
/// 全螢幕時，滑鼠多久沒動就隱藏控制列
const HIDE_AFTER: Duration = Duration::from_secs(2);
const OSD_DURATION: Duration = Duration::from_millis(1500);
/// 控制列完整顯示需要的寬度（也是視窗的最小寬度）
pub const MIN_WINDOW_WIDTH: f32 = 640.0;
/// 螢幕下方留給工作列（Windows）/ Dock（macOS）的高度，視窗不要被蓋住
const TASKBAR_ALLOWANCE: f32 = 48.0;
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
/// 改了長寬比、裁切、旋轉之後，多久之內的畫面設定都跟著調整視窗高度（實測兩次 VideoReconfig 相隔 50–160 毫秒）
/// 暫停時改旋轉要從前一個關鍵影格解到目前這一格，長 GOP 的 4K 影片要等比較久
const REFIT_WINDOW: Duration = Duration::from_secs(5);

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
    /// 字幕 / 音訊延遲加減（秒）；None = 歸零
    SubDelay(Option<f64>),
    AudioDelay(Option<f64>),
    LoadSubtitle,
    LoadAudio,
    SubtitleStyle,
    /// 視窗置頂（開 / 關）
    ToggleOnTop,
    /// 畫面比例：依序切換 / 指定（None = 原始比例）
    AspectCycle,
    SetAspect(Option<usize>),
    /// 裁切：依序切換 / 指定（None = 不裁切）
    CropCycle,
    SetCrop(Option<usize>),
    /// 填滿視窗（裁掉黑邊）
    ToggleFill,
    /// 縮放（log2 的增減）/ 重設
    Zoom(f64),
    ZoomReset,
    /// 移動畫面（影片大小的比例）/ 置中
    Pan(f64, f64),
    PanCenter,
    /// 順時針轉 90° / 指定角度
    RotateCw,
    SetRotate(u32),
    /// 翻轉：true = 左右、false = 上下
    Flip(bool),
    /// 畫面的調整全部還原
    ResetView,
    /// 播放清單面板（F6）
    TogglePlaylist,
    /// 把清單上選取的項目移出清單（Delete）
    PlaylistRemove,
    /// 媒體資訊面板（Ctrl+F1 / Ctrl+I）
    ToggleInfo,
    CopyInfo,
    /// 擷取畫面：存到截圖資料夾（Ctrl+E）、另存新檔、複製到剪貼簿（Ctrl+C）
    Screenshot,
    ScreenshotAs,
    CopyFrame,
    OpenScreenshotDir,
    ChooseScreenshotDir,
    /// 設定視窗（F5）
    Settings,
    /// 流暢播放：開（使用電池時暫停）/ 關
    ToggleSmooth,
    /// 影像調整：某一項加減（W/E、R/T、Y/U、I/O），限制在 −100…100
    Adjust(AdjustKind, i32),
    /// 影像調整全部還原（Q）
    AdjustReset,
    /// 控制面板（Alt+G）：開關，記得上次的分頁
    ToggleControlPanel,
    /// 「影像調整…」（右鍵選單、設定頁）：打開控制面板的畫質分頁
    ShowAdjustments,
    /// 畫質（整個程式共用、存檔）：去交錯、去色帶、銳化、縮放演算法
    SetDeinterlace(Deinterlace),
    SetDeband(Strength),
    SetSharpen(Strength),
    SetQuality(Quality),
    /// 個別指定放大 / 縮小 / 色度的演算法；None = 跟隨畫質
    SetUpscaler(Option<Upscaler>),
    SetDownscaler(Option<Downscaler>),
    SetChromaScaler(Option<ChromaScaler>),
    /// HDR 轉 SDR：曲線、目標亮度（None = 自動）、色域對應、依畫面動態調整亮度
    SetTone(ToneCurve),
    SetTargetPeak(Option<u32>),
    SetGamut(Gamut),
    SetComputePeak(bool),
    /// 像素著色器：使用的組合（編號）；None = 不使用
    SetShaderPreset(Option<u32>),
    /// 音效（整個程式共用、存檔）：獨佔模式、多聲道轉成立體聲、音訊直通（開 / 關）。
    /// 輸出裝置的名稱是字串，在選單、設定頁裡直接處理
    ToggleExclusive,
    ToggleDownmix,
    TogglePassthrough,
    /// 等化器（開 / 關）、預設（選了順便打開）；「等化器…」打開控制面板的音效分頁
    ToggleEq,
    SetEqPreset(EqPreset),
    ShowEqualizer,
    /// 音量平衡：關閉、夜間模式、人聲平衡、音量平均
    SetLeveling(Leveling),
    /// 音量上限（%）：100 / 130 / 150 / 200
    SetVolumeMax(u32),
}

pub struct VitascopeApp {
    player: Player,
    video: Option<VideoView>,
    settings: Settings,
    /// 影片畫面無法初始化之類的嚴重錯誤
    fatal: Option<String>,
    last_activity: Instant,
    osd: Option<(String, Instant)>,
    /// 最近一次開檔的時間：之後才出現的提示（下一個檔案、續播、字幕載入失敗）不被影像調整的提醒蓋掉
    opened_at: Instant,
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
    /// 播放引擎是 LGPL 建置（本專案建置的 libmpv；Linux tar.gz 用的系統 libmpv 依發行版而定）
    engine_lgpl: bool,
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
    /// 上一幀結束時有文字輸入框在輸入（快捷鍵先停用）
    typing_last_frame: bool,
    /// 「字幕外觀」視窗
    sub_style_open: bool,
    /// 正在逐格（暫停中）：mpv 逐格時會短暫取消暫停，這段期間不算「播放中」，到結尾也不換檔
    frame_stepping: bool,
    /// 逐格中卻一直沒有暫停（從什麼時候開始）：超過一下子就當作已經回到一般播放
    stepping_unpaused_since: Option<Instant>,
    /// 續播：已經送出跳到上次位置、還沒跳到（開始播放的那一下時間是 0，不能拿去存檔）
    resume_target: Option<(f64, Instant)>,
    /// 跟影片一起拖放進來的字幕：等那個影片載入完再加上去
    pending_subs: Option<(PathBuf, Vec<PathBuf>)>,
    /// 播完時資料夾還沒掃描完（不知道有沒有下一個）：掃描完再接下一個
    pending_auto_next: bool,
    /// 這一幀開始時是否有選單開著（點畫面關選單時，不要順便暫停）
    popup_open_at_start: bool,
    /// 畫面調整（長寬比、裁切、縮放、旋轉、翻轉）；換檔時還原
    geometry: Geometry,
    /// 這個檔案原本的顯示比例（已含檔案本身的旋轉）與畫面輸出的旋轉，換長寬比、裁切時的基準
    natural: Option<(f64, i64)>,
    /// 長寬比、裁切、旋轉改了之後的一小段時間：這段期間每次畫面設定好（VideoReconfig）都重新調整視窗高度。
    /// mpv 換旋轉、長寬比時會送兩次 VideoReconfig，第一次還是舊的參數，不能只看第一次
    refit_until: Option<Instant>,
    /// 這一幀要依新的畫面尺寸調整視窗高度
    refit_now: bool,
    /// 已經送出開新檔、新檔案還沒開始：這段期間舊檔案的事件不能拿來補設畫面調整
    ///（mpv 換檔時會先把畫面選項還原，補設的話會帶到新檔案）
    switching_file: bool,
    /// 播放清單面板上選取的項目
    playlist_selected: Option<usize>,
    /// 播放清單面板這一幀的寬度（沒打開是 0）
    playlist_width: f32,
    /// 使用者調整過的面板寬度（下次打開時用）
    playlist_width_pref: f32,
    /// 打開播放清單時視窗加寬了多少、加寬後的寬度（關掉時縮回去；使用者自己調整過視窗就不縮）
    playlist_grew: Option<(f32, f32)>,
    /// 滑鼠在播放清單上（全螢幕時控制列、滑鼠游標不隱藏）
    pointer_over_playlist: bool,
    /// 「加入資料夾」背景掃描的結果
    folder_add: Option<Receiver<Vec<PathBuf>>>,
    /// 手動整理的清單要不要存起來（自動測試、`--shot` 不存）
    persist_playlist: bool,
    /// 存下的清單是這次還原的、或這次手動整理過：才可以覆蓋 / 刪掉（雙擊一個影片開起來的不能刪掉上次存的清單）
    owns_session: bool,
    /// 單一執行個體：收別的程式送來的檔案，同時到達的合併成一批
    instance: Option<crate::instance::Primary>,
    batch: crate::instance::Batcher,
    /// 叫視窗到前面之後，什麼時候檢查有沒有成功（沒有就閃工作列）
    attention_at: Option<Instant>,
    /// 清單已經捲到哪一項（正在播的換了才再捲）
    playlist_follow: Option<usize>,
    /// 清單上一幀的捲動位置、看得到的高度
    playlist_view: (f32, f32),
    /// 主視窗（開檔對話框的擁有者；視窗置頂時對話框才不會被蓋在下面）
    owner: Option<Owner>,
    /// 上一幀是否全螢幕（macOS 離開全螢幕時會把「置頂」拿掉，要再設一次）
    was_fullscreen: bool,
    /// 什麼時候再設一次視窗置頂（macOS 離開全螢幕的動畫結束之後）
    reapply_level_at: Option<Instant>,
    /// 設定視窗
    settings_open: bool,
    settings_page: settings_window::Page,
    /// 擷取畫面
    capture: capture::Capture,
    /// 進度條預覽縮圖（第一次停在進度條上才建立）
    thumbs: Option<crate::thumbs::Thumbnailer>,
    preview: preview::PreviewCache,
    /// 媒體資訊面板
    info_open: bool,
    info_cache: Option<info_panel::InfoCache>,
    /// 音訊裝置的名稱（面板打開時查一次）
    audio_device: Option<String>,
    /// 播放引擎有哪些 L3 功能（啟動時偵測）
    caps: EngineCaps,
    /// 這個引擎的縮放預設值（「標準」畫質）
    picture_defaults: PictureDefaults,
    /// 已送出、還沒回覆的非同步設定：指令編號 → 選項名稱（失敗時提示用）
    async_pending: HashMap<u64, String>,
    /// 流暢播放：螢幕更新率、電源、決定
    pacing: pacing::PacingCtl,
    /// 影像調整（亮度、對比…）：這次執行跨檔案沿用（mpv 的這些選項換檔時不會還原）；
    /// 勾了「下次開啟時沿用」才存進設定
    adjust: Adjust,
    /// 控制面板（Alt+G）
    panel_open: bool,
    panel_tab: control_panel::PanelTab,
    /// 改去交錯時的提示（顯示的時間）：「目前」的狀態要等 mpv 換好濾鏡，提示還在時跟著更新
    deint_osd: Option<Instant>,
    /// 換了像素著色器之後，看它能不能用（畫不出來就還原）
    shader_watch: crate::picture::shader::ShaderApply,
    /// 著色器檔案的檢查結果（設定頁顯示說明或問題）；設定視窗關掉時清掉，下次打開重新檢查
    shader_info: HashMap<String, Result<crate::picture::shader::ShaderInfo, crate::picture::shader::ShaderProblem>>,
    /// 這次執行畫不出來的著色器檔案 → 原因（設定頁標出來）
    shader_failures: HashMap<String, String>,
    /// 最近一次加入檔案時被拒絕的（檔名, 原因）
    shader_rejected: Option<(String, String)>,
    /// 剛新增的組合：設定頁展開它一次
    shader_new: Option<u32>,
    /// 自動測試沒有畫面時當成畫了幾格（見 `simulate_video_frames`）
    simulated_frames: Option<u64>,
    /// 存下的輸出裝置拔掉了，暫時用預設裝置（插回來時切回去）
    device_fallback: bool,
    /// 上次處理過的裝置清單（變了才重新對照存下的裝置）
    devices_seen: Option<Vec<crate::sound::AudioDevice>>,
    /// 上次看到的音訊直通格式（開始直通時提示一次）
    spdif_seen: Option<String>,
    /// 上次看到音訊輸出開不起來、改用 null（改用時提示一次）
    ao_fallback_seen: bool,
    /// 改用 null 之後重開了音訊輸出（換檔、裝置清單變了，見 `retry_audio_output`）：什麼時候送的，
    /// 過一下再看結果（還是 null 的話再提示一次）
    ao_retry: Option<Instant>,
    /// 重開音訊輸出的次數（介面測試用）
    ao_retries: u32,
    /// 上次看到 mpv 選的音軌（變了才更新 `audio_restore`）
    audio_seen: Option<i64>,
    /// 換輸出裝置之後要選回來的音軌：這個檔案最後選的音軌（使用者自己關掉音軌時是 None）。
    /// 播放中拔掉裝置時 mpv 會先自己重開音訊輸出、開不起來就把音軌關掉，之後改裝置也不會再開
    audio_restore: Option<i64>,
    /// 上次送給 mpv 的 af（等化器、音量平衡、限幅器的濾鏡鏈）；None = 不確定（mpv 不接受、或還沒管）
    af_applied: Option<String>,
    /// mpv 不接受的 af：同一條不再送（設定改了才再試）
    af_failed: Option<String>,
    /// 還沒回覆的非同步 af：指令編號 → 送出的值
    af_inflight: HashMap<u64, String>,
    /// 即時調整（af-command）之後什麼時候改寫整條 af
    af_debounce: crate::sound::AfDebounce,
    /// 預測接下來的音訊會直通（濾鏡鏈先清空）；真的開始直通、或一直是 PCM 時取消
    spdif_expect: bool,
    spdif_expect_at: Instant,
    /// 上次問 mpv 解碼格式的時間（等直通時，見 `spdif_refused`）
    spdif_polled_at: Instant,
    /// 送出的 af-command 數（介面測試確認即時調整走 af-command、不是整條改寫）
    af_commands_sent: u64,
    /// 上一幀濾鏡鏈是不是當成直通中（實際的或預測的）：變了才換濾鏡鏈
    af_spdif_seen: bool,
}

/// 主視窗的 handle（給開檔對話框當擁有者）
#[derive(Clone, Copy)]
struct Owner {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

impl Owner {
    fn from_creation(cc: &eframe::CreationContext<'_>) -> Option<Self> {
        Some(Self {
            window: cc.window_handle().ok()?.as_raw(),
            display: cc.display_handle().ok()?.as_raw(),
        })
    }

    /// Wayland 不讓程式自己把視窗設成置頂
    fn is_wayland(&self) -> bool {
        matches!(self.window, RawWindowHandle::Wayland(_))
    }
}

impl HasWindowHandle for Owner {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: 主視窗在整個程式執行期間都存在，handle 不會變
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for Owner {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: 同上
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}

/// 啟動參數
#[derive(Default)]
pub struct Launch {
    /// 要開的檔案（命令列；好幾個 = 一次開一個播放清單）
    pub files: Vec<PathBuf>,
    pub fullscreen: bool,
    pub autoshot: Option<AutoShot>,
    /// 播放紀錄；預設只放在記憶體（自動測試用），播放器用 `History::load()`
    pub history: History,
    /// 上次手動整理的播放清單（沒有指定要開的檔案時還原）
    pub playlist: Option<Playlist>,
    /// 手動整理的播放清單要存起來（預設不存：自動測試不能動到使用者的檔案）
    pub persist_playlist: bool,
    /// 單一執行個體：自己是主視窗時，收別的程式送來的檔案
    pub instance: Option<crate::instance::Primary>,
    /// 查螢幕更新率、電源的方法；None = 問作業系統（自動測試沒有視窗，查不到）
    pub platform: Option<Box<dyn PlatformProbe>>,
    /// 環境變數 VITASCOPE_PACING（啟動時讀一次；自動測試預設沒有）
    pub pacing: crate::pacing::Overrides,
}

impl VitascopeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, player: Player, settings: Settings, launch: Launch) -> Self {
        crate::i18n::set_lang(settings.language);
        crate::fonts::install_cjk(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        // 啟動時音量最多 100%：上次放大到 150% 也從 100% 開始，開啟時不會突然很大聲（set_volume 限制在 0–100）
        let _ = player.set_volume(settings.volume);
        let _ = player.set_mute(settings.muted);
        player.apply_sub_style(&settings.subtitle);

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
            if is_mesa_software_renderer(&renderer) && !mpv_opts_override(&player, "gpu-dumb-mode") {
                eprintln!("[vitascope] 偵測到軟體繪圖，mpv 改用簡化的繪圖流程");
                let _ = player.mpv().set_property("gpu-dumb-mode", "yes");
            }
        }
        let (video, fatal) = match &cc.get_proc_address {
            Some(gpa) => match VideoView::new(
                player.mpv().clone(),
                gpa.clone(),
                cc.egui_ctx.clone(),
                !launch.pacing.block,
            ) {
                Ok(v) => (Some(v), None),
                Err(e) => (
                    None,
                    Some(crate::tf!(
                        "無法初始化影片畫面：{e}",
                        "Cannot initialize the video view: {e}"
                    )),
                ),
            },
            None => (
                None,
                Some(
                    crate::tr!(
                        "無法初始化影片畫面：沒有 OpenGL context",
                        "Cannot initialize the video view: no OpenGL context"
                    )
                    .into(),
                ),
            ),
        };
        if let Some(msg) = &fatal {
            eprintln!("[vitascope] {msg}");
        }
        let owner = Owner::from_creation(cc);
        // 自動測試沒有真的視窗（沒有 handle），查不到更新率，跟以前一樣播放
        let probe = launch.platform.or_else(|| {
            owner.map(|o| Box::new(pacing::RealProbe::new(o.window, &cc.egui_ctx)) as Box<dyn PlatformProbe>)
        });
        let user_sync = mpv_opts_override(&player, "video-sync") || mpv_opts_override(&player, "display-fps-override");
        let pacing = pacing::PacingCtl::new(probe, user_sync, launch.pacing);
        // 影像調整預設每次啟動從 0 開始；勾了「下次開啟時沿用」才用上次存的
        let adjust = if settings.video.keep_adjust {
            settings.video.adjust.clamped()
        } else {
            Adjust::default()
        };

        let mut app = Self {
            player,
            video,
            settings,
            fatal,
            last_activity: Instant::now(),
            osd: None,
            opened_at: Instant::now(),
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
            engine_lgpl: false,
            history: launch.history,
            playlist: launch.playlist.clone(),
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
            typing_last_frame: false,
            sub_style_open: false,
            frame_stepping: false,
            stepping_unpaused_since: None,
            resume_target: None,
            pending_subs: None,
            pending_auto_next: false,
            popup_open_at_start: false,
            geometry: Geometry::default(),
            natural: None,
            refit_until: None,
            refit_now: false,
            switching_file: false,
            playlist_selected: None,
            playlist_width: 0.0,
            playlist_width_pref: playlist_panel::PANEL_WIDTH,
            playlist_grew: None,
            pointer_over_playlist: false,
            folder_add: None,
            persist_playlist: launch.persist_playlist,
            owns_session: launch.playlist.is_some(),
            instance: launch.instance,
            batch: crate::instance::Batcher::default(),
            attention_at: None,
            playlist_follow: None,
            playlist_view: (0.0, 0.0),
            owner,
            was_fullscreen: false,
            reapply_level_at: None,
            settings_open: false,
            settings_page: settings_window::Page::default(),
            capture: capture::Capture::default(),
            thumbs: None,
            preview: preview::PreviewCache::default(),
            info_open: false,
            info_cache: None,
            audio_device: None,
            caps: EngineCaps::default(),
            picture_defaults: PictureDefaults::default(),
            async_pending: HashMap::new(),
            pacing,
            adjust,
            panel_open: false,
            panel_tab: control_panel::PanelTab::default(),
            deint_osd: None,
            shader_watch: Default::default(),
            shader_info: HashMap::new(),
            shader_failures: HashMap::new(),
            shader_rejected: None,
            shader_new: None,
            simulated_frames: None,
            device_fallback: false,
            devices_seen: None,
            spdif_seen: None,
            ao_fallback_seen: false,
            ao_retry: None,
            ao_retries: 0,
            audio_seen: None,
            audio_restore: None,
            af_applied: None,
            af_failed: None,
            af_inflight: HashMap::new(),
            af_debounce: Default::default(),
            spdif_expect: false,
            spdif_expect_at: Instant::now(),
            spdif_polled_at: Instant::now(),
            af_commands_sent: 0,
            af_spdif_seen: false,
        };
        app.engine_versions = short_versions(
            &app.player.get_string("mpv-version").unwrap_or_default(),
            &app.player.get_string("ffmpeg-version").unwrap_or_default(),
        );
        app.engine_lgpl = app
            .player
            .get_string("mpv-configuration")
            .is_ok_and(|c| c.contains("gpl=false"));
        // 軟體繪圖的判斷（gpu-dumb-mode）已經做完、還沒開任何檔案：這時才能同步設定 mpv
        app.apply_startup();
        if launch.files.is_empty() {
            // 沒有要開檔：截的是起始畫面，現在就開始計時
            if let Some(shot) = &mut app.autoshot {
                shot.arm();
            }
        } else if app.instance.is_some() {
            // 跟同時啟動的其他程式（檔案總管多選按 Enter）送來的檔案合併成一批再開
            let request = crate::instance::Request {
                paths: launch.files,
                fullscreen: false,
            };
            app.batch.push(request, Instant::now());
        } else {
            app.open_paths(launch.files, false);
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

    /// 目前的畫面調整（介面測試用）
    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// 播放引擎偵測到的功能（介面測試用）
    pub fn engine_caps(&self) -> &EngineCaps {
        &self.caps
    }

    /// 這個播放引擎的縮放演算法預設值（介面測試用）
    pub fn picture_defaults(&self) -> &PictureDefaults {
        &self.picture_defaults
    }

    /// 目前畫面上的提示文字（介面測試用）
    pub fn osd_text(&self) -> Option<&str> {
        self.osd.as_ref().map(|(text, _)| text.as_str())
    }

    /// 送出的 af-command 數（介面測試用：拖滑桿、調音量時先用 af-command 即時調整）
    pub fn af_commands_sent(&self) -> u64 {
        self.af_commands_sent
    }

    /// 還沒回覆的非同步 af 設定數（介面測試用：啟動時是同步設定的，一個都沒有）
    pub fn af_sets_in_flight(&self) -> usize {
        self.af_inflight.len()
    }

    /// 即時調整之後還在等著改寫整條 af（介面測試用）
    pub fn af_rewrite_pending(&self) -> bool {
        self.af_debounce.due().is_some()
    }

    /// 這次執行的影像調整（介面測試用）
    pub fn adjust(&self) -> Adjust {
        self.adjust
    }

    /// 啟動時（還沒開任何檔案）偵測播放引擎的功能，同步套用要在第一個檔案就生效的設定
    /// （流暢播放打開而且已經查得到更新率時、畫質、音效）
    fn apply_startup(&mut self) {
        self.caps = self.player.probe_caps();
        self.picture_defaults = self.player.picture_defaults();
        if std::env::var_os("VITASCOPE_DEBUG").is_some() {
            eprintln!(
                "[vitascope] 播放引擎功能：{:?}；縮放預設值：{:?}",
                self.caps, self.picture_defaults
            );
        }
        // 要等軟體繪圖的判斷（caps.dumb）
        self.pacing_startup();
        self.adjust_startup();
        // 去交錯（預設自動）、去色帶、銳化、縮放演算法、HDR：第一個檔案就要生效
        self.video_startup();
        // 輸出裝置、獨佔模式、轉成立體聲、音訊直通
        self.sound_startup();
    }

    // ───────────── 非同步設定 mpv 選項 ─────────────

    /// 非同步設定一個 mpv 選項（播放中改設定都用這個，不會卡住介面）；失敗時提示「無法套用」
    pub fn set_option_async(&mut self, k: AsyncKey, name: &str, value: &str) {
        let result = self.player.set_async(k, name, value);
        self.track_async(k, name, result);
    }

    /// 非同步指令（change-list、af-command…），回覆依種類 `k` 分派
    pub fn command_async_keyed(&mut self, k: AsyncKey, args: &[&str]) {
        let result = self.player.command_async_keyed(k, args);
        if k == AsyncKey::AfCommand && result.is_ok() {
            self.af_commands_sent += 1;
        }
        // 提示裡寫選項名稱（set、change-list 的第二個參數），其他指令寫指令名稱
        let name = match args {
            [cmd, name, ..] if matches!(*cmd, "set" | "change-list") => *name,
            [cmd, ..] => *cmd,
            [] => "",
        };
        self.track_async(k, name, result);
    }

    fn track_async(&mut self, k: AsyncKey, name: &str, result: crate::mpv::Result<u64>) {
        match result {
            Ok(id) => {
                self.async_pending.insert(id, name.to_owned());
            }
            Err(e) => self.async_failed(k, name, &e.to_string()),
        }
    }

    /// mpv 回覆了 `set_option_async` / `command_async_keyed` 送出的指令
    fn on_async_reply(&mut self, id: u64, k: AsyncKey, error: Option<String>) {
        // 不是這裡送的（Player 開始播新檔時自己重送的 glsl-shaders）：用選項名稱
        let name = self.async_pending.remove(&id).unwrap_or_else(|| match k {
            AsyncKey::Shaders => "glsl-shaders".to_owned(),
            _ => format!("{k:?}"),
        });
        if k == AsyncKey::Af {
            self.af_reply(id, error.is_some());
        }
        if let Some(e) = error {
            // 流暢播放的設定 mpv 不接受：這次執行改回一般播放（改設定時再試）
            if matches!(k, AsyncKey::VideoSync | AsyncKey::DisplayFps) {
                self.pacing.apply_failed();
            }
            // 畫質選項沒設成功：忘掉記下的值，下次改設定時整組再送一次
            if matches!(
                k,
                AsyncKey::Deinterlace | AsyncKey::Deband | AsyncKey::Sharpen | AsyncKey::Scaler | AsyncKey::Tone
            ) {
                self.player.forget_picture(&name);
            }
            // 音效選項也一樣
            self.sound_reply_failed(k, &name);
            self.async_failed(k, &name, &e);
            // 像素著色器 mpv 不接受：剛換的組合就還原（提示換成「已還原」）
            if k == AsyncKey::Shaders {
                self.shader_reply_failed(&e);
            }
        }
    }

    fn async_failed(&mut self, k: AsyncKey, name: &str, e: &str) {
        // af-command 失敗不提示：沒開檔、濾鏡剛重建時本來就會失敗，之後會改寫整條 af
        if k == AsyncKey::AfCommand {
            if std::env::var_os("VITASCOPE_DEBUG").is_some() {
                eprintln!("[vitascope] af-command 失敗（{name}）：{e}");
            }
            return;
        }
        eprintln!("[vitascope] 無法套用 {name}：{e}");
        self.osd(crate::tf!("無法套用 {name}：{e}", "Couldn't apply {name}: {e}"));
    }

    // ───────────── 操作 ─────────────

    fn open(&mut self, path: &Path) {
        self.open_at(path, None);
    }

    /// 開檔。`list_index`：從播放清單上開的（上一個 / 下一個、雙擊）是第幾項，
    /// 同一個檔案在清單上出現兩次時才不會跳回第一個
    fn open_at(&mut self, path: &Path, list_index: Option<usize>) {
        // 播放清單檔：換成檔案裡的清單（只讀一次；HLS 串流的 .m3u8 交給 mpv）
        if formats::is_playlist(path) && !is_url(&path.to_string_lossy()) {
            match crate::m3u::read_text(path) {
                Ok(text) if !crate::m3u::is_hls(&text) => {
                    self.open_playlist_text(path, &text);
                    return;
                }
                // HLS，或讀不到（交給 mpv，它會說明原因）
                _ => {}
            }
        }
        // 先記下目前的檔案看到哪裡
        self.remember_position();
        let path = if is_url(&path.to_string_lossy()) {
            path.to_path_buf()
        } else {
            // 續播紀錄、播放清單都用完整路徑比對（命令列可能給相對路徑）
            std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
        };
        // 同一份清單裡的檔案只移動位置；其他檔案先自己成一份清單，背景再掃描同資料夾的檔案
        let in_list = self.playlist.as_mut().is_some_and(|list| match list_index {
            Some(i)
                if list
                    .items()
                    .get(i)
                    .is_some_and(|p| crate::playlist::same_file(p, &path)) =>
            {
                list.select_index(i)
            }
            _ => list.select(&path),
        });
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
        self.pending_auto_next = false;
        self.switching_file = true;
        self.opened_at = Instant::now();
        self.info_cache = None;
        // 開新檔一律從播放開始（mpv 會沿用上一個檔案的暫停狀態）；A-B 重播也會沿用，要清掉
        let _ = self.player.set_pause(false);
        let _ = self.player.clear_ab_loop();
        // 第二字幕的軌道編號在每個檔案都不一樣，換檔時關掉；字幕延遲也是每個檔案各自的
        //（音訊延遲通常是藍牙耳機之類的裝置延遲，保留）
        let _ = self.player.set_secondary_sub(None);
        let _ = self.player.set_sub_delay(0.0);
        // 開了音訊直通：新檔案可能會直通，濾鏡鏈先清空（同步，排在開檔之前）
        self.sound_before_open();
        if let Err(e) = self.player.open(&path.to_string_lossy()) {
            self.player.state.last_error = Some(crate::tf!("無法開啟：{e}", "Cannot open: {e}"));
            // 不會有 StartFile 了：舊檔案照樣在播，畫面調整要繼續同步；濾鏡鏈照舊檔案的音軌
            self.switching_file = false;
            self.sound_file_loaded();
        } else {
            // 再取消一次暫停：舊檔案播完停在最後一格時（keep-open 會暫停），開檔前的取消暫停
            // 會被 mpv 在 loadfile 生效前又暫停回去，新檔案就停著不播。loadfile 之後舊檔案已經在結束，不會再暫停
            let _ = self.player.set_pause(false);
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
                    let st = &self.player.state;
                    let still_allowed = self.settings.auto_next
                        && self.seek_drag.is_none()
                        && self.drag_gen.is_none()
                        && self.autoshot.is_none();
                    if std::mem::take(&mut self.pending_auto_next) && still_allowed && st.loaded && st.eof {
                        self.play_next_in_list();
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.playlist_scan = None,
        }
    }

    /// 記下目前的檔案看到哪裡（換檔、停止、關閉時）
    fn remember_position(&mut self) {
        let st = &self.player.state;
        if !st.loaded || self.resume_target.is_some() {
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
        // 存總音量（超過 100% 的放大也算）；下次啟動時最多從 100% 開始
        self.settings.volume = self.player.volume_total();
        self.settings.muted = self.player.state.muted;
        self.store_adjust();
        if let Err(e) = self.settings.save() {
            eprintln!("[vitascope] 無法儲存設定：{e}");
        }
    }

    /// 播放中定時、以及剛暫停時，存一下續播位置（當機、關機時才不會整段遺失）
    fn autosave(&mut self) {
        let st = &self.player.state;
        // 續播的跳轉完成了（或等太久，例如檔案不能跳轉）：之後的位置才是真的
        if let Some((target, since)) = self.resume_target
            && ((st.time_pos - target).abs() < 3.0 || since.elapsed() > Duration::from_secs(10))
        {
            self.resume_target = None;
        }
        let paused_now = st.loaded && st.paused;
        // 逐格每一下都會暫停一次，不用每次都存
        let just_paused = paused_now && !self.was_paused && !self.frame_stepping;
        self.was_paused = paused_now;
        let playing = st.loaded && !st.paused;
        if just_paused || (playing && self.last_autosave.elapsed() >= AUTOSAVE_EVERY) {
            self.remember_position();
        }
    }

    /// 播放清單的上一個 / 下一個檔案
    fn step_file(&mut self, forward: bool) {
        let target = self.playlist.as_ref().and_then(|l| {
            let i = if forward { l.next_index() } else { l.prev_index() }?;
            Some((i, l.items()[i].clone()))
        });
        match target {
            Some((i, path)) => {
                self.open_at(&path, Some(i));
                let (pos, len) = self.playlist.as_ref().map_or((1, 1), |l| (l.position(), l.len()));
                let dir = if forward {
                    crate::tr!("下一個", "Next")
                } else {
                    crate::tr!("上一個", "Previous")
                };
                self.osd(crate::tf!(
                    "{dir}（{pos}/{len}）：{}",
                    "{dir} ({pos}/{len}): {}",
                    file_name(&path)
                ));
            }
            None => self.osd(if forward {
                crate::tr!("已經是最後一個檔案", "This is the last file")
            } else {
                crate::tr!("已經是第一個檔案", "This is the first file")
            }),
        }
    }

    /// 開檔對話框，擁有者是主視窗（視窗置頂時才不會被蓋在下面）
    fn file_dialog(&self) -> rfd::FileDialog {
        let dialog = rfd::FileDialog::new();
        match &self.owner {
            Some(owner) => dialog.set_parent(owner),
            None => dialog,
        }
    }

    fn open_dialog(&mut self) {
        // 播放清單檔（.m3u / .m3u8）也可以從這裡開
        let mut exts = formats::all_media();
        exts.extend(formats::PLAYLIST);
        let mut dialog = self
            .file_dialog()
            .set_title(crate::tr!("開啟影片", "Open video"))
            .add_filter(crate::tr!("影音檔案", "Media files"), &exts)
            .add_filter(crate::tr!("所有檔案", "All files"), &["*"]);
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
        // 畫面調整：要有影像（純音訊、專輯封面不算，跟右鍵選單的「畫面」一樣），
        // 也不能在換檔途中（會設到下一個檔案上，跟面板顯示的不一致）
        let view = loaded && st.has_video() && !self.switching_file;
        match action {
            Action::TogglePause if loaded => {
                let msg = if st.paused {
                    crate::tr!("▶ 播放", "▶ Play")
                } else {
                    crate::tr!("⏸ 暫停", "⏸ Pause")
                };
                if st.paused {
                    self.frame_stepping = false;
                }
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
                let dir = if delta < 0.0 {
                    crate::tr!("◀◀ 後退", "◀◀ Back")
                } else {
                    crate::tr!("▶▶ 前進", "▶▶ Forward")
                };
                self.osd(crate::tf!(
                    "{dir} {} 秒   {} / {}",
                    "{dir} {} s   {} / {}",
                    delta.abs(),
                    fmt_time(target),
                    fmt_time(duration)
                ));
            }
            // 音訊直通中：音量交給擴大機
            Action::Volume(_) if st.audio_spdif.is_some() => self.osd(sound::spdif_volume_hover()),
            Action::Volume(delta) => {
                // 直接問 mpv 目前的音量：連續捲動滾輪時，屬性變化的通知可能還沒送到。
                // 總音量 = mpv 的音量 + 限幅器放大的部分，上限是設定的音量上限（預設 100%）
                let muted = st.muted;
                let current = match self.player.get_f64("volume") {
                    Ok(v) => v + self.player.boost_pct(),
                    Err(_) => self.player.volume_total(),
                };
                let v = (current + delta).clamp(0.0, self.volume_cap());
                self.set_volume_total(v, false);
                if muted {
                    let _ = self.player.set_mute(false);
                }
                self.osd(sound::volume_osd(v));
            }
            // 音訊直通中：靜音跟音量一樣沒有作用（mpv 的靜音是軟體音量，直通的資料不經過它），交給擴大機。
            // 存下的靜音不動，改回一般輸出時照樣靜音
            Action::ToggleMute if st.audio_spdif.is_some() => self.osd(sound::spdif_volume_hover()),
            Action::ToggleMute => {
                let muted = !st.muted;
                let _ = self.player.set_mute(muted);
                self.osd(if muted {
                    crate::tr!("靜音", "Mute")
                } else {
                    crate::tr!("取消靜音", "Unmute")
                });
            }
            Action::ToggleFullscreen => {
                let fullscreen = is_fullscreen(ctx);
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
            }
            Action::ExitFullscreen => ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false)),
            Action::Open => self.open_dialog(),
            Action::About => self.about_open = true,
            Action::TogglePlaylist => self.toggle_playlist(ctx),
            Action::ToggleInfo => self.toggle_info(),
            Action::CopyInfo => self.copy_info(ctx),
            Action::Screenshot => self.take_screenshot(capture::ShotDest::Folder),
            Action::ScreenshotAs => self.screenshot_as_dialog(),
            Action::CopyFrame => self.take_screenshot(capture::ShotDest::Clipboard),
            Action::OpenScreenshotDir => {
                let dir = self.screenshot_dir();
                if let Err(e) = crate::screenshot::open_folder(&dir) {
                    self.osd(crate::tf!(
                        "無法開啟截圖資料夾：{e}",
                        "Cannot open the screenshot folder: {e}"
                    ));
                }
            }
            Action::ChooseScreenshotDir => self.choose_screenshot_dir(),
            Action::Settings => self.settings_open = !self.settings_open,
            Action::ToggleSmooth => self.toggle_smooth(),
            Action::Adjust(kind, delta) => self.step_adjust(kind, delta),
            Action::AdjustReset => self.reset_adjust(),
            Action::ToggleControlPanel => self.panel_open = !self.panel_open,
            Action::ShowAdjustments => self.show_adjustments(),
            Action::SetDeinterlace(d) => self.set_deinterlace(d),
            Action::SetDeband(s) => self.set_deband(s),
            Action::SetSharpen(s) => self.set_sharpen(s),
            Action::SetQuality(q) => self.set_quality(q),
            Action::SetUpscaler(s) => self.set_upscaler(s),
            Action::SetDownscaler(s) => self.set_downscaler(s),
            Action::SetChromaScaler(s) => self.set_chroma_scaler(s),
            Action::SetTone(c) => self.set_tone(c),
            Action::SetTargetPeak(p) => self.set_target_peak(p),
            Action::SetGamut(g) => self.set_gamut(g),
            Action::SetComputePeak(on) => self.set_compute_peak(on),
            Action::SetShaderPreset(id) => self.set_shader_preset(id),
            Action::ToggleExclusive => self.set_exclusive(!self.settings.audio.exclusive),
            Action::ToggleDownmix => self.set_downmix(!self.settings.audio.downmix),
            Action::TogglePassthrough => self.set_passthrough(!self.settings.audio.passthrough.enabled),
            Action::ToggleEq => self.set_eq_enabled(!self.settings.audio.eq.enabled),
            Action::SetEqPreset(p) => self.set_eq_preset(p),
            Action::ShowEqualizer => self.show_equalizer(),
            Action::SetLeveling(l) => self.set_leveling(l),
            Action::SetVolumeMax(v) => self.set_volume_max(v),
            Action::PlaylistRemove => {
                if let Some(i) = self.playlist_selected {
                    self.remove_from_playlist(i);
                }
            }
            Action::PrevFile => self.step_file(false),
            Action::NextFile => self.step_file(true),
            Action::SpeedStep(dir) => {
                let current = self.player.get_f64("speed").unwrap_or(st.speed);
                let speed = ((current + 0.1 * f64::from(dir)) * 100.0).round() / 100.0;
                self.set_speed(speed);
            }
            Action::SpeedReset => self.set_speed(1.0),
            Action::FrameStep(_) if loaded && !st.tracks_of(TrackKind::Video).any(|t| t.selected && !t.albumart) => {
                // 沒有畫面（純音訊、專輯封面）時 mpv 不會逐格，會一直播下去
                self.osd(crate::tr!(
                    "這個檔案沒有影像，不能逐格",
                    "This file has no video to step through"
                ));
            }
            Action::FrameStep(forward) if loaded => {
                self.frame_stepping = true;
                self.stepping_unpaused_since = None;
                self.was_playing = false;
                let _ = self.player.frame_step(forward);
                self.osd(if forward {
                    crate::tr!("逐格前進 ▶", "Next frame ▶")
                } else {
                    crate::tr!("◀ 逐格後退", "◀ Previous frame")
                });
            }
            Action::AbLoop if loaded => {
                let msg = match st.ab_loop {
                    [None, _] => crate::tf!("A-B 重播：起點 {}", "A-B loop: start {}", fmt_time(st.time_pos)),
                    [Some(a), None] => crate::tf!(
                        "A-B 重播：{} → {}",
                        "A-B loop: {} → {}",
                        fmt_time(a),
                        fmt_time(st.time_pos)
                    ),
                    [Some(_), Some(_)] => crate::tr!("取消 A-B 重播", "Cancel A-B loop").to_owned(),
                };
                let _ = self.player.cycle_ab_loop();
                self.osd(msg);
            }
            Action::Chapter(delta) if loaded => self.step_chapter(delta),
            Action::SubDelay(delta) if loaded => {
                // 直接問 mpv 目前的值：連按時屬性變化的通知可能還沒送到
                let current = self.player.get_f64("sub-delay").unwrap_or(st.sub_delay);
                let delay = delta.map_or(0.0, |d| current + d);
                let _ = self.player.set_sub_delay(delay);
                self.osd(crate::tf!("字幕延遲 {}", "Subtitle delay {}", fmt_delay(delay)));
            }
            Action::AudioDelay(delta) if loaded => {
                let current = self.player.get_f64("audio-delay").unwrap_or(st.audio_delay);
                let delay = delta.map_or(0.0, |d| current + d);
                let _ = self.player.set_audio_delay(delay);
                self.osd(crate::tf!("音訊延遲 {}", "Audio delay {}", fmt_delay(delay)));
            }
            Action::LoadSubtitle if loaded => self.load_file_dialog(true),
            Action::SubtitleStyle => self.sub_style_open = true,
            Action::AspectCycle if view => {
                let next = geometry::cycle(self.geometry.aspect, ASPECTS.len());
                self.set_aspect(next);
            }
            Action::SetAspect(aspect) if view => self.set_aspect(aspect),
            Action::CropCycle if view => {
                let next = geometry::cycle(self.geometry.crop, CROPS.len());
                self.set_crop(next);
            }
            Action::SetCrop(crop) if view => self.set_crop(crop),
            Action::ToggleFill if view => {
                self.geometry.fill = !self.geometry.fill;
                let _ = self
                    .player
                    .mpv()
                    .set_property("panscan", if self.geometry.fill { 1.0 } else { 0.0 });
                self.osd(if self.geometry.fill {
                    crate::tr!("填滿視窗：開啟", "Fill window: on")
                } else {
                    crate::tr!("填滿視窗：關閉", "Fill window: off")
                });
            }
            Action::Zoom(delta) if view => {
                self.geometry.zoom = geometry::zoom_after(self.geometry.zoom, delta);
                let _ = self.player.mpv().set_property("video-zoom", self.geometry.zoom);
                self.osd(crate::tf!("縮放 {:.0}%", "Zoom {:.0}%", self.geometry.zoom_percent()));
            }
            Action::ZoomReset if view => {
                self.geometry.zoom = 0.0;
                self.geometry.pan = [0.0, 0.0];
                self.apply_zoom_and_pan();
                self.osd(crate::tr!("縮放 100%", "Zoom 100%"));
            }
            Action::Pan(dx, dy) if view => {
                self.geometry.pan = [
                    geometry::pan_after(self.geometry.pan[0], dx),
                    geometry::pan_after(self.geometry.pan[1], dy),
                ];
                self.apply_zoom_and_pan();
                self.osd(crate::tr!("移動畫面", "Move picture"));
            }
            Action::PanCenter if view => {
                self.geometry.pan = [0.0, 0.0];
                self.apply_zoom_and_pan();
                self.osd(crate::tr!("畫面置中", "Picture centered"));
            }
            Action::RotateCw if view => self.set_rotate((self.geometry.rotate + 90) % 360),
            Action::SetRotate(deg) if view => self.set_rotate(deg),
            Action::Flip(horizontal) if view => {
                let on = if horizontal {
                    self.geometry.hflip = !self.geometry.hflip;
                    self.geometry.hflip
                } else {
                    self.geometry.vflip = !self.geometry.vflip;
                    self.geometry.vflip
                };
                self.apply_flip(horizontal);
                let name = if horizontal {
                    crate::tr!("左右翻轉", "Flip horizontally")
                } else {
                    crate::tr!("上下翻轉", "Flip vertically")
                };
                self.osd(crate::tf!(
                    "{name}：{}",
                    "{name}: {}",
                    if on {
                        crate::tr!("開啟", "on")
                    } else {
                        crate::tr!("關閉", "off")
                    }
                ));
            }
            Action::ResetView if view => {
                let had_shape =
                    self.geometry.aspect.is_some() || self.geometry.crop.is_some() || self.geometry.rotate != 0;
                let (hflip, vflip) = (self.geometry.hflip, self.geometry.vflip);
                self.geometry = Geometry::default();
                let _ = self.player.mpv().set_property("panscan", 0.0);
                self.apply_zoom_and_pan();
                if hflip {
                    self.apply_flip(true);
                }
                if vflip {
                    self.apply_flip(false);
                }
                if had_shape {
                    self.sync_shape(true);
                }
                self.osd(crate::tr!("畫面已重設", "Picture reset"));
            }
            // Wayland 不能設成置頂（存下來的設定是在別的桌面環境開的，照樣可以關掉）
            Action::ToggleOnTop if !self.settings.always_on_top && self.owner.is_some_and(|o| o.is_wayland()) => {
                self.osd(crate::tr!(
                    "這個桌面環境（Wayland）不支援讓程式自己設定視窗置頂",
                    "This desktop (Wayland) does not let programs keep themselves on top"
                ));
            }
            Action::ToggleOnTop => {
                self.settings.always_on_top = !self.settings.always_on_top;
                self.apply_window_level(ctx);
                self.save_settings();
                self.osd(if self.settings.always_on_top {
                    crate::tr!("視窗置頂：開啟", "Always on top: on")
                } else {
                    crate::tr!("視窗置頂：關閉", "Always on top: off")
                });
            }
            Action::LoadAudio if loaded => self.load_file_dialog(false),
            Action::Restart if loaded && st.seekable => {
                let _ = self.player.seek_to(0.0, true);
                // 播完停在最後一格（暫停中）時也要開始播
                let _ = self.player.set_pause(false);
                self.frame_stepping = false;
                self.osd(crate::tr!("從頭播放", "Playing from the start"));
            }
            _ => {}
        }
    }

    /// 選單「載入字幕檔…」「載入音軌檔…」：從目前影片的資料夾開始找
    fn load_file_dialog(&mut self, subtitle: bool) {
        let (title, filter, exts) = if subtitle {
            (
                crate::tr!("載入字幕檔", "Load subtitle file"),
                crate::tr!("字幕檔", "Subtitle files"),
                formats::SUBTITLE,
            )
        } else {
            (
                crate::tr!("載入音軌檔", "Load audio file"),
                crate::tr!("音訊檔", "Audio files"),
                formats::AUDIO,
            )
        };
        let mut dialog = self
            .file_dialog()
            .set_title(title)
            .add_filter(filter, exts)
            .add_filter(crate::tr!("所有檔案", "All files"), &["*"]);
        if let Some(dir) = self.player.state.path.as_deref().and_then(|p| Path::new(p).parent()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.pick_file() else { return };
        self.load_extra_file(&path, subtitle);
    }

    /// 載入字幕檔、音軌檔（`subtitle` = 字幕）並切換過去。音軌跟選單換音軌一樣：會直通的話先清空濾鏡鏈
    #[doc(hidden)]
    pub fn load_extra_file(&mut self, path: &Path, subtitle: bool) {
        let path_str = path.to_string_lossy().into_owned();
        let result = if subtitle {
            self.player.add_subtitle(&path_str)
        } else {
            match self.player.add_audio(&path_str) {
                Ok(Some(id)) => self.switch_track(TrackKind::Audio, Some(id)),
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            }
        };
        let kind = if subtitle {
            crate::tr!("字幕", "subtitle")
        } else {
            crate::tr!("音軌", "audio track")
        };
        match result {
            Ok(()) => self.osd(crate::tf!("載入{kind}：{}", "Loaded {kind}: {}", file_name(path))),
            Err(e) => self.osd(crate::tf!("無法載入{kind}：{e}", "Cannot load {kind}: {e}")),
        }
    }

    fn set_aspect(&mut self, aspect: Option<usize>) {
        self.geometry.aspect = aspect;
        self.sync_shape(true);
        self.osd(crate::tf!(
            "畫面比例：{}",
            "Aspect ratio: {}",
            self.geometry.aspect_label()
        ));
    }

    fn set_crop(&mut self, crop: Option<usize>) {
        self.geometry.crop = crop;
        self.sync_shape(true);
        let label = self.geometry.crop_label();
        self.osd(if crop.is_some() {
            crate::tf!("裁切：{label}", "Crop: {label}")
        } else {
            label.to_owned()
        });
    }

    fn set_rotate(&mut self, deg: u32) {
        self.geometry.rotate = deg % 360;
        self.sync_shape(true);
        // 用濾鏡翻轉時（軟體繪圖），濾鏡在旋轉之前翻：轉了 90° 之後左右、上下要對調
        if self.flip_with_filter() {
            if self.geometry.hflip {
                self.apply_flip(true);
            }
            if self.geometry.vflip {
                self.apply_flip(false);
            }
        }
        self.osd(crate::tf!("旋轉 {}°", "Rotation {}°", self.geometry.rotate));
    }

    fn apply_zoom_and_pan(&self) {
        let mpv = self.player.mpv();
        let _ = mpv.set_property("video-zoom", self.geometry.zoom);
        let _ = mpv.set_property("video-pan-x", self.geometry.pan[0]);
        let _ = mpv.set_property("video-pan-y", self.geometry.pan[1]);
    }

    /// 軟體繪圖的簡化流程不跑著色器，翻轉改用 vf 濾鏡
    fn flip_with_filter(&self) -> bool {
        self.player.get_string("gpu-dumb-mode").is_ok_and(|v| v == "yes")
    }

    fn apply_flip(&mut self, horizontal: bool) {
        let on = if horizontal {
            self.geometry.hflip
        } else {
            self.geometry.vflip
        };
        // 濾鏡在畫面輸出旋轉之前翻：看的是檔案本身的旋轉加上使用者的旋轉
        let file_rotate = self.natural.map_or(0, |(_, r)| r);
        let quarter = (file_rotate + i64::from(self.geometry.rotate)).rem_euclid(180) == 90;
        let use_filter = self.flip_with_filter();
        match self.player.set_flip(horizontal, on, use_filter, quarter) {
            // 著色器的翻轉是非同步送的（跟使用者的著色器組合同一個清單）
            Ok(Some(id)) => {
                self.async_pending.insert(id, "glsl-shaders".to_owned());
            }
            Ok(None) => {}
            Err(e) => self.osd(crate::tf!("無法翻轉畫面：{e}", "Cannot flip the picture: {e}")),
        }
    }

    /// 長寬比、裁切、旋轉（會讓 mpv 重新設定畫面的選項）：依目前的模型算出 mpv 的值，有變才設定。
    /// 旋轉改了之後影格的方向也變了，裁切要等新的畫面參數出來再算一次，所以畫面設定好時（VideoReconfig）也會呼叫。
    /// 回傳是否有送出改變（還會再收到一次 VideoReconfig）
    fn sync_shape(&mut self, user_changed: bool) -> bool {
        // 還不知道原本的形狀（第一次 VideoReconfig 之前）：先不送，知道的時候會補做
        let Some((natural_aspect, natural_rotate)) = self.natural else {
            return false;
        };
        let Some(out) = self.player.out_params() else {
            return false;
        };
        let g = self.geometry.clone();
        let total_rotate = (natural_rotate + i64::from(g.rotate)).rem_euclid(360) as u32;
        let mut changed = false;
        let rotate_changed = self
            .player
            .set_option_if_changed("video-rotate", &g.rotate.to_string())
            .unwrap_or(false);
        changed |= rotate_changed;
        let aspect = g.aspect.map(|i| geometry::aspect_override(ASPECTS[i].1, total_rotate));
        changed |= self.player.set_aspect_override(aspect).unwrap_or(false);
        // 畫面輸出要做的旋轉：視窗裡由畫面輸出轉（vo_libmpv），沒有畫面時（vo=null）mpv 用濾鏡先轉好
        let vo_rotate = if self.video.is_some() { total_rotate } else { 0 };
        // 旋轉剛改、或 mpv 先送來還是舊參數的 VideoReconfig：影格的方向還是舊的，裁切等新的參數出來再算
        //（用舊的方向算會先閃一下錯的裁切，視窗也會跟著變成錯的高度）
        let stale = rotate_changed || out.rotate.rem_euclid(360) as u32 != vo_rotate;
        let crop = match g.crop {
            // 不裁切跟方向無關，馬上清掉（重設畫面時也是旋轉剛改，要不然舊的裁切會留著）
            None => Some(String::new()),
            _ if stale => None,
            Some(i) => {
                // 畫面上整張影片的比例：選了長寬比就是那個比例，不然是原本的比例（轉 90° 時倒過來）
                let display = match g.aspect {
                    Some(a) => ASPECTS[a].1,
                    None if g.rotate % 180 == 90 => 1.0 / natural_aspect,
                    None => natural_aspect,
                };
                let frame = geometry::Frame {
                    w: out.w as f64,
                    h: out.h as f64,
                    rotate: out.rotate.rem_euclid(360) as u32,
                };
                Some(geometry::crop_value(frame, geometry::crop_edges(display, CROPS[i].1)))
            }
        };
        if let Some(crop) = crop {
            changed |= self.player.set_option_if_changed("video-crop", &crop).unwrap_or(false);
        }
        if changed && user_changed {
            // 畫面形狀變了：接下來一小段時間內新的尺寸出來時，視窗寬度不變、高度跟著調
            self.refit_until = Some(Instant::now() + REFIT_WINDOW);
            self.video_reconfigured = false;
        }
        changed
    }

    /// macOS 離開全螢幕時會把視窗層級改回一般（設定還是「置頂」）：離開時再設一次，
    /// 動畫結束後（約一秒）再設一次。其他系統重設一次沒有影響
    fn keep_window_level(&mut self, ctx: &egui::Context) {
        let fullscreen = is_fullscreen(ctx);
        if std::mem::replace(&mut self.was_fullscreen, fullscreen) && !fullscreen && self.settings.always_on_top {
            self.apply_window_level(ctx);
            self.reapply_level_at = Some(Instant::now() + Duration::from_secs(1));
            ctx.request_repaint_after(Duration::from_millis(1100));
        }
        if self.reapply_level_at.is_some_and(|t| Instant::now() >= t) {
            self.reapply_level_at = None;
            if self.settings.always_on_top && !fullscreen {
                self.apply_window_level(ctx);
            }
        }
    }

    /// 單一執行個體：收別的程式送來的檔案。第一個到的時候先把視窗叫到前面，
    /// 一小段時間內沒有新的了才一起開（檔案總管多選按 Enter 會同時啟動好幾個程式）
    fn poll_instance(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let mut raise = false;
        if let Some(p) = &self.instance {
            while let Ok(req) = p.rx.try_recv() {
                raise |= self.batch.push(req, now);
            }
        }
        if raise {
            self.bring_to_front(ctx);
        }
        match self.batch.poll(now) {
            crate::instance::BatchPoll::Idle => {}
            crate::instance::BatchPoll::Wait(d) => ctx.request_repaint_after(d),
            crate::instance::BatchPoll::Ready { request, continues } => {
                if request.fullscreen {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(true));
                }
                if continues && self.player.state.path.is_some() {
                    self.continue_burst(request.paths);
                } else if !request.paths.is_empty() {
                    self.open_paths(request.paths, false);
                }
            }
        }
        if let Some(t) = self.attention_at {
            if now < t {
                ctx.request_repaint_after(t - now);
            } else {
                self.attention_at = None;
                // 系統不讓搶前景時（Windows 的前景鎖、Wayland）：閃工作列 / 跳 Dock
                if ctx.input(|i| i.viewport().focused) != Some(true) {
                    ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(
                        egui::UserAttentionType::Informational,
                    ));
                }
            }
        }
    }

    /// 多選的檔案很多時，送來的檔案被切成兩批：清單換成整串，正在播的照樣播
    fn continue_burst(&mut self, paths: Vec<PathBuf>) {
        let mut media: Vec<PathBuf> = paths
            .iter()
            .map(|p| crate::playlist::absolute(p))
            .filter(|p| formats::media_kind(p).is_some())
            .collect();
        crate::playlist::sort_by_name(&mut media);
        let current = self.player.state.path.clone().map(PathBuf::from);
        let mut list = Playlist::from_files(media).manual();
        match current {
            Some(c) if list.len() > 1 && list.select(&c) => {
                self.playlist_scan = None;
                self.playlist = Some(list);
                self.owns_session = true;
                self.persist_playlist();
            }
            // 正在播的不在這一串裡（不太可能）：當成新的一批
            _ => self.open_paths(paths, false),
        }
    }

    fn bring_to_front(&mut self, ctx: &egui::Context) {
        // 縮小的視窗不能直接叫到前面，先還原
        if ctx.input(|i| i.viewport().minimized) == Some(true) {
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        }
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.attention_at = Some(Instant::now() + Duration::from_millis(300));
    }

    fn apply_window_level(&self, ctx: &egui::Context) {
        let level = if self.settings.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        };
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
    }

    fn set_speed(&mut self, speed: f64) {
        let speed = speed.clamp(MIN_SPEED, MAX_SPEED);
        // 音訊直通中不能變速（改回正常速度可以）
        if self.speed_blocked(speed) {
            return;
        }
        let _ = self.player.set_speed(speed);
        self.osd(crate::tf!("速度 {}×", "Speed {}×", fmt_speed(speed)));
    }

    /// 跳到前 / 後幾個章節，OSD 顯示章節名稱。目前在哪一章交給 mpv 判斷：
    /// 跳完章節後畫面的時間常常比章節時間早一點點，自己用時間算會卡在同一章；
    /// mpv 也會處理「進入本章超過幾秒，往回跳先回到本章開頭」
    fn step_chapter(&mut self, delta: i64) {
        let total = self.player.state.chapters.len() as i64;
        if total == 0 {
            self.osd(crate::tr!("這個檔案沒有章節", "This file has no chapters"));
            return;
        }
        let current = self.player.current_chapter().unwrap_or(-1);
        // 最後一章再往後，mpv 會跳到片尾（然後自動播下一個檔案），這裡先擋下來
        if delta > 0 && current + delta >= total {
            self.osd(crate::tr!("已經是最後一章", "This is the last chapter"));
            return;
        }
        let _ = self.player.add_chapter(delta);
        let now = self.player.current_chapter().unwrap_or(current + delta);
        let msg = match usize::try_from(now) {
            Ok(i) => crate::tf!(
                "章節 {}/{total}：{}",
                "Chapter {}/{total}: {}",
                i + 1,
                chapter_label(&self.player.state.chapters, i)
            ),
            Err(_) => crate::tr!("回到開頭", "Back to the start").to_owned(),
        };
        self.osd(msg);
    }

    /// 切換軌道（不提示）。音軌：換成會直通的濾鏡鏈先清空（同步，排在選音軌之前），記下使用者選的音軌
    fn switch_track(&mut self, kind: TrackKind, id: Option<i64>) -> crate::mpv::Result<()> {
        if kind == TrackKind::Audio {
            self.sound_before_track(id);
        }
        if let Err(e) = self.player.select_track(kind, id) {
            if kind == TrackKind::Audio {
                let current = self.player.state.selected(TrackKind::Audio).map(|t| t.id);
                self.sound_before_track(current);
            }
            return Err(e);
        }
        // 使用者自己選的音軌（包括關掉）：換裝置之後照這個
        if kind == TrackKind::Audio {
            self.audio_restore = id;
        }
        Ok(())
    }

    fn select_track(&mut self, kind: TrackKind, id: Option<i64>) {
        let name = if kind == TrackKind::Sub {
            crate::tr!("字幕", "Subtitles")
        } else {
            crate::tr!("音軌", "Audio")
        };
        if let Err(e) = self.switch_track(kind, id) {
            self.osd(crate::tf!("無法切換{name}：{e}", "Cannot switch {name}: {e}"));
            return;
        }
        let label = id
            .and_then(|id| self.player.state.tracks_of(kind).find(|t| t.id == id))
            .map_or_else(|| crate::tr!("關閉", "Off").to_owned(), |t| t.label());
        self.osd(crate::tf!("{name}：{label}", "{name}: {label}"));
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
                self.frame_stepping = false;
                self.stepping_unpaused_since = None;
                self.resume_target = None;
                // 畫面調整是每個檔案各自的（mpv 那邊由 reset-on-next-file 還原）
                self.geometry = Geometry::default();
                self.natural = None;
                self.refit_until = None;
                self.refit_now = false;
                self.switching_file = false;
                self.pending_auto_next = false;
                // 自動存檔從開檔起重新計時（不然停在起始畫面很久再開檔，第一幀就會存到 0）
                self.last_autosave = Instant::now();
                self.file_gen += 1;
                self.pacing.start_file(self.file_gen);
                self.audio_seen = None;
                self.audio_restore = None;
                // 上一個檔案的音訊輸出開不起來、改用 null：mpv 換檔時沿用同一個輸出，不重開的話之後的檔案都沒有聲音
                if self.player.audio_fell_back() {
                    self.retry_audio_output();
                }
            }
            PlayerEvent::FileLoaded => self.on_file_loaded(),
            PlayerEvent::CommandReply { id, error } => match crate::player::async_key(id) {
                Some(k) => self.on_async_reply(id, k, error),
                // 截圖
                None => self.on_command_reply(id, error),
            },
            // 新檔案的影像設定好了，尺寸才是新的
            PlayerEvent::VideoReconfig => {
                // 記下檔案原本的形狀（解碼器的參數，不受任何調整影響），之後換長寬比、裁切都以它為準
                let mut early_shape = false;
                // 讀不到解碼器參數時（不太會發生），還沒有任何調整的話用畫面輸出的參數
                let fallback = || {
                    let out = self.player.out_params()?;
                    let [w, h] = self.player.state.video_size?;
                    self.geometry
                        .is_default()
                        .then(|| (w as f64 / h.max(1) as f64, out.rotate.rem_euclid(360)))
                };
                if self.natural.is_none()
                    && !self.switching_file
                    && let Some(natural) = self.player.natural_shape().or_else(fallback)
                {
                    self.natural = Some(natural);
                    // 畫面設定好之前就按了長寬比、裁切、旋轉：現在才真的套用，視窗也要跟著調
                    let g = &self.geometry;
                    early_shape = g.aspect.is_some() || g.crop.is_some() || g.rotate != 0;
                    // 用濾鏡翻轉時要看檔案本身的旋轉，之前翻的要重新套一次
                    if self.flip_with_filter() {
                        for (horizontal, on) in [(true, self.geometry.hflip), (false, self.geometry.vflip)] {
                            if on {
                                self.apply_flip(horizontal);
                            }
                        }
                    }
                }
                // 有調整的話依新的畫面參數再對一次（例如旋轉之後要重算裁切）；有改就再等下一次
                let resent = !self.switching_file && !self.geometry.is_default() && self.sync_shape(early_shape);
                if !resent {
                    self.video_reconfigured = true;
                    if self.refit_until.is_some_and(|t| Instant::now() < t) {
                        self.refit_now = true;
                    }
                }
            }
            PlayerEvent::PlaybackRestart => {
                // 跳轉完成（開檔後開始播放也是）：流暢播放的量測重新開始
                self.pacing.seeked();
                if let Some(shot) = &mut self.autoshot {
                    shot.arm();
                }
                if self.seek_released {
                    self.seek_drag = None;
                    self.seek_released = false;
                }
            }
            PlayerEvent::Seek => self.pacing.seeked(),
            PlayerEvent::EndFile { error, .. } => {
                self.fit_window_pending = false;
                self.seek_drag = None;
                self.seek_released = false;
                if let Some(e) = error {
                    eprintln!("[vitascope] {e}");
                    // 開檔失敗：不會直通，濾鏡鏈設回來
                    self.sound_open_failed();
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
        // 選上的音軌會不會直通（開了直通時，開檔前先清空了濾鏡鏈）
        self.sound_file_loaded();
        self.preview_file_loaded();
        let Ok(path) = self.player.get_string("path") else {
            return;
        };
        if let Some((video, subs)) = self.pending_subs.take()
            && crate::playlist::same_file(&video, Path::new(&path))
        {
            // 一起拖進來的字幕：第一個選上，其他的加到選單裡
            for (i, sub) in subs.iter().enumerate() {
                if let Err(e) = self.player.add_subtitle_as(&sub.to_string_lossy(), i == 0) {
                    self.osd(crate::tf!("無法載入字幕：{e}", "Cannot load the subtitle: {e}"));
                }
            }
        }
        if is_url(&path) {
            self.adjust_reminder();
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
                self.resume_target = Some((t, Instant::now()));
                self.osd(crate::tf!(
                    "從 {} 繼續播放（Home 從頭播放）",
                    "Resuming from {} (Home plays from the start)",
                    fmt_time(t)
                ));
            } else {
                self.update_history(|h| h.forget(&path));
            }
        }
        self.adjust_reminder();
    }

    /// 播完時自動播放清單的下一個檔案。只算「播放中播到結尾」：
    /// 暫停中逐格或跳轉到結尾不算；拖曳進度條期間也先不動，放開後再說
    fn auto_next(&mut self) {
        if self.seek_drag.is_some() || self.drag_gen.is_some() {
            let st = &self.player.state;
            if st.loaded && st.eof && self.was_playing {
                self.resume_after_drag = true;
            }
            return;
        }
        let st = &self.player.state;
        // 逐格一下最多播一格就會暫停；一直沒暫停代表其實是在播放（例如 mpv 沒辦法逐格）
        if self.frame_stepping && st.loaded && !st.paused {
            let since = *self.stepping_unpaused_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(1) {
                self.frame_stepping = false;
                self.stepping_unpaused_since = None;
            }
        } else {
            self.stepping_unpaused_since = None;
        }
        let eof = st.loaded && st.eof;
        let just_ended = eof && !self.was_eof && self.was_playing;
        self.was_eof = eof;
        // mpv 播到結尾時會同時設定暫停，所以看的是上一幀還在播放
        self.was_playing = st.loaded && !st.paused && !st.eof && !self.frame_stepping;
        // 自動截圖（開發、CI 用）要固定的畫面，不換檔
        if !just_ended || !self.settings.auto_next || self.autoshot.is_some() {
            return;
        }
        let has_next = self.playlist.as_ref().is_some_and(|l| l.next().is_some());
        if has_next {
            self.play_next_in_list();
        } else if self.playlist_scan.is_some() {
            // 資料夾還在掃描：掃完再看有沒有下一個
            self.pending_auto_next = true;
        }
    }

    fn play_next_in_list(&mut self) {
        let target = self
            .playlist
            .as_ref()
            .and_then(|l| l.next_index().map(|i| (i, l.items()[i].clone())));
        if let Some((i, next)) = target {
            self.open_at(&next, Some(i));
            let (pos, len) = self.playlist.as_ref().map_or((1, 1), |l| (l.position(), l.len()));
            self.osd(crate::tf!(
                "下一個（{pos}/{len}）：{}",
                "Next ({pos}/{len}): {}",
                file_name(&next)
            ));
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
        // 「關於」視窗開著時，按鍵都交給它（Esc 關閉視窗，而不是離開全螢幕）；
        // 正在輸入文字（例如字幕外觀的字型名稱）時，字母鍵不能變成快捷鍵。
        // 也看上一幀：在輸入框按 Esc 時，egui 會先取消焦點，這一幀的 Esc 不能拿去離開全螢幕
        if self.about_open || self.typing_last_frame || ctx.text_edit_focused() {
            return;
        }
        // Esc 先關「字幕外觀」視窗（不要直接離開全螢幕）
        if self.sub_style_open
            && !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.sub_style_open = false;
            self.save_settings();
            return;
        }
        // Esc 也先關控制面板
        if self.panel_open
            && !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.panel_open = false;
            return;
        }
        // Esc 也先關設定視窗
        if self.settings_open
            && !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.settings_open = false;
            return;
        }
        // Esc 也先關媒體資訊
        if self.info_open
            && !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
        {
            self.info_open = false;
            return;
        }
        // Esc 只在全螢幕、而且沒有選單開著時才用來離開全螢幕；其他時候留給 egui 關選單
        let esc_exits_fullscreen = is_fullscreen(ctx) && !egui::Popup::is_any_open(ctx);
        let playlist_open = self.settings.show_playlist;
        let (seek_short, seek_long) = (self.settings.seek_short, self.settings.seek_long);
        let text_selected = ctx
            .with_plugin::<egui::text_selection::LabelSelectionState, _>(|s| s.has_selection())
            .unwrap_or(false);
        // 先把快捷鍵吃掉，避免同一個按鍵又觸發 egui 的按鈕（例如空白鍵按下有焦點的按鈕）
        let mut actions = Vec::new();
        ctx.input_mut(|i| {
            // Ctrl+C 不會變成按鍵事件：egui 把它轉成「複製」（Event::Copy）。有選取文字時是複製文字
            if !text_selected && i.events.iter().any(|e| matches!(e, egui::Event::Copy)) {
                actions.push(Action::CopyFrame);
            }
            let mut key = |mods: Modifiers, key: Key, action: Action| {
                if i.consume_key(mods, key) {
                    actions.push(action);
                }
            };
            // Alt + 方向鍵要排在沒有修飾鍵的方向鍵前面：egui 比對時會忽略多按的 Alt
            key(Modifiers::ALT, Key::ArrowLeft, Action::Pan(-PAN_STEP, 0.0));
            key(Modifiers::ALT, Key::ArrowRight, Action::Pan(PAN_STEP, 0.0));
            key(Modifiers::ALT, Key::ArrowUp, Action::Pan(0.0, -PAN_STEP));
            key(Modifiers::ALT, Key::ArrowDown, Action::Pan(0.0, PAN_STEP));
            key(Modifiers::ALT, Key::K, Action::RotateCw);
            key(Modifiers::ALT, Key::Backspace, Action::ResetView);
            key(Modifiers::ALT, Key::G, Action::ToggleControlPanel);
            // 裁切用 Ctrl（macOS 也是 Control 鍵）：Cmd+Q 是結束程式
            key(Modifiers::CTRL, Key::Q, Action::CropCycle);
            key(Modifiers::COMMAND, Key::Z, Action::Flip(true));
            key(Modifiers::COMMAND, Key::P, Action::Flip(false));
            key(Modifiers::COMMAND, Key::T, Action::ToggleOnTop);
            key(Modifiers::COMMAND, Key::Num5, Action::PanCenter);
            key(Modifiers::COMMAND, Key::F6, Action::AspectCycle);
            key(Modifiers::NONE, Key::A, Action::AspectCycle);
            key(Modifiers::NONE, Key::Num9, Action::Zoom(ZOOM_STEP));
            key(Modifiers::NONE, Key::Num1, Action::Zoom(-ZOOM_STEP));
            key(Modifiers::NONE, Key::Num5, Action::ZoomReset);
            key(Modifiers::COMMAND, Key::O, Action::Open);
            key(Modifiers::COMMAND, Key::ArrowLeft, Action::Seek(-seek_long));
            key(Modifiers::COMMAND, Key::ArrowRight, Action::Seek(seek_long));
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
            key(Modifiers::NONE, Key::OpenBracket, Action::SubDelay(Some(-0.1)));
            key(Modifiers::NONE, Key::CloseBracket, Action::SubDelay(Some(0.1)));
            key(Modifiers::NONE, Key::Minus, Action::AudioDelay(Some(-0.1)));
            key(Modifiers::NONE, Key::Equals, Action::AudioDelay(Some(0.1)));
            // 數字鍵盤的 +、德文鍵盤的 + 鍵是 Plus，不是 Equals
            key(Modifiers::NONE, Key::Plus, Action::AudioDelay(Some(0.1)));
            key(Modifiers::NONE, Key::ArrowLeft, Action::Seek(-seek_short));
            key(Modifiers::NONE, Key::ArrowRight, Action::Seek(seek_short));
            key(Modifiers::NONE, Key::ArrowUp, Action::Volume(5.0));
            key(Modifiers::NONE, Key::ArrowDown, Action::Volume(-5.0));
            key(Modifiers::NONE, Key::Space, Action::TogglePause);
            key(Modifiers::NONE, Key::M, Action::ToggleMute);
            key(Modifiers::NONE, Key::F, Action::ToggleFullscreen);
            key(Modifiers::NONE, Key::Enter, Action::ToggleFullscreen);
            key(Modifiers::COMMAND, Key::E, Action::Screenshot);
            key(Modifiers::COMMAND, Key::F1, Action::ToggleInfo);
            key(Modifiers::COMMAND, Key::I, Action::ToggleInfo);
            key(Modifiers::NONE, Key::F1, Action::About);
            key(Modifiers::NONE, Key::F6, Action::TogglePlaylist);
            key(Modifiers::NONE, Key::F5, Action::Settings);
            // 影像調整（PotPlayer 的按鍵）：排在所有 Ctrl / Cmd 組合鍵後面。egui 比對時會分辨 Ctrl / Cmd，
            // 所以 Ctrl+E（截圖）、Ctrl+T（置頂）、Ctrl+I（媒體資訊）、Ctrl+Q（裁切）不會變成調整
            key(Modifiers::NONE, Key::Q, Action::AdjustReset);
            for (kind, minus, plus) in [
                (AdjustKind::Brightness, Key::W, Key::E),
                (AdjustKind::Contrast, Key::R, Key::T),
                (AdjustKind::Saturation, Key::Y, Key::U),
                (AdjustKind::Hue, Key::I, Key::O),
            ] {
                key(Modifiers::NONE, minus, Action::Adjust(kind, -1));
                key(Modifiers::NONE, plus, Action::Adjust(kind, 1));
            }
            if playlist_open {
                key(Modifiers::NONE, Key::Delete, Action::PlaylistRemove);
                // Mac 的鍵盤沒有 Delete 鍵（Alt+Backspace 已經在前面處理掉了）
                if cfg!(target_os = "macos") {
                    key(Modifiers::NONE, Key::Backspace, Action::PlaylistRemove);
                }
            }
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
        self.open_paths(dropped, true);
    }

    /// 開一組檔案（拖放、命令列、別的程式送來的）：好幾個影音檔 = 依檔名排序的播放清單；
    /// 只有字幕 = 加到正在播的影片；播放清單檔照樣開。`dropped` = 拖放進來的（播放清單開著時加到清單最後）
    pub(super) fn open_paths(&mut self, dropped: Vec<PathBuf>, from_drop: bool) {
        let dropped: Vec<PathBuf> = dropped.iter().map(|p| crate::playlist::absolute(p)).collect();
        let Some(first) = dropped.first().cloned() else { return };
        let dropped_count = dropped.len();
        let dropped_paths = dropped.clone();
        let mut subs: Vec<PathBuf> = dropped.iter().filter(|p| formats::is_subtitle(p)).cloned().collect();
        crate::playlist::sort_by_name(&mut subs);
        // 一次拖放多個影音檔：播放清單就是這幾個檔案，依檔名排序
        //（拖放的順序跟系統有關，Windows 會把滑鼠抓著的那個檔案放在最前面）
        let mut media: Vec<PathBuf> = dropped
            .into_iter()
            .filter(|p| formats::media_kind(p).is_some())
            .collect();
        crate::playlist::sort_by_name(&mut media);
        let all_subs = subs.len() == dropped_count;
        if media.is_empty() && all_subs {
            // 只拖了字幕檔：加到正在播（或正在開）的影片
            let st = &self.player.state;
            if st.loaded {
                for (i, sub) in subs.iter().enumerate() {
                    match self.player.add_subtitle_as(&sub.to_string_lossy(), i == 0) {
                        Ok(()) => self.osd(crate::tf!("載入字幕：{}", "Loaded subtitle: {}", file_name(sub))),
                        Err(e) => self.osd(crate::tf!("無法載入字幕：{e}", "Cannot load the subtitle: {e}")),
                    }
                }
            } else if st.loading
                && let Ok(path) = self.player.get_string("path")
            {
                self.pending_subs = Some((PathBuf::from(path), subs));
            } else {
                self.osd(crate::tr!(
                    "請先開啟影片，再拖放字幕檔",
                    "Open a video first, then drop the subtitle file"
                ));
            }
            return;
        }
        if media.is_empty() {
            // 副檔名不在清單上的檔案：照樣交給 mpv 試試看，字幕等它載入完再加
            let other = dropped_paths
                .into_iter()
                .find(|p| !formats::is_subtitle(p))
                .unwrap_or(first);
            self.open(&other);
            if !subs.is_empty() {
                let other = std::path::absolute(&other).unwrap_or(other);
                self.pending_subs = Some((other, subs));
            }
            return;
        }
        // 屬於某個拖進來的影片的字幕（同資料夾、檔名以影片名稱開頭），那個影片載入時就會自動找到；
        // 只有其他的字幕才加到第一個影片
        let belongs_to_some_video = |sub: &Path| {
            let name = sub
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            media.iter().any(|v| {
                v.parent() == sub.parent()
                    && v.file_stem()
                        .is_some_and(|stem| crate::subs::belongs_to(&name, &stem.to_string_lossy().to_lowercase()))
            })
        };
        let extra_subs: Vec<PathBuf> = subs.into_iter().filter(|s| !belongs_to_some_video(s)).collect();
        // 播放清單開著時拖放進來的：加到清單最後（沒有在播的話播第一個），不換掉清單
        if from_drop && self.settings.show_playlist {
            self.add_to_playlist(media);
            if !extra_subs.is_empty() {
                self.osd(crate::tr!(
                    "播放清單開著時，字幕要拖到正在播的影片上（關掉清單再拖）",
                    "While the playlist is open, close it before dropping subtitles onto the video"
                ));
            }
            return;
        }
        let video = media[0].clone();
        if media.len() > 1 {
            self.remember_position();
            self.playlist_scan = None;
            self.playlist = Some(Playlist::from_files(media).manual());
            self.owns_session = true;
            self.persist_playlist();
        }
        self.open(&video);
        // 影片載入完再加上去（open 會把路徑轉成完整路徑，這裡也一樣）
        if !extra_subs.is_empty() {
            let video = std::path::absolute(&video).unwrap_or(video);
            self.pending_subs = Some((video, extra_subs));
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let st = &self.player.state;
        let title = match &st.title {
            Some(t) if st.loaded => format!("{t} — {}", app_name()),
            _ => app_name().to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// 新檔案的影片尺寸確定後，把視窗調整成影片比例（不超過螢幕的 80%）
    fn fit_window(&mut self, ctx: &egui::Context) {
        if self.refit_now && !self.fit_window_pending {
            self.refit_now = false;
            self.refit_height(ctx);
            return;
        }
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
        let max = monitor.unwrap_or(vec2(1920.0, 1080.0)) * 0.8 - vec2(self.playlist_width, self.controls_height);
        let fit = (max.x / video.x).min(max.y / video.y);
        // 原尺寸優先；太小的影片（例如 320×240）放大到 640 寬；兩者都不超過螢幕
        let want = if video.x < 640.0 { 640.0 / video.x } else { 1.0 };
        let size = video * want.min(fit);
        // 直式影片很窄，控制列放不下，兩側補黑邊
        // 控制列是整個視窗寬（含播放清單），至少要放得下按鈕
        let width = (size.x + self.playlist_width).max(MIN_WINDOW_WIDTH);
        let target = vec2(width, size.y + self.controls_height);
        // 播放清單開著時換了檔：記下這次實際為清單加了多寬（面板可能被拖寬過；當初右邊放不下沒加寬，
        // 這次也加了），關掉清單時縮回只有影片的寬度
        if self.settings.show_playlist && self.playlist_width > 0.0 {
            let added = target.x - size.x.max(MIN_WINDOW_WIDTH);
            self.playlist_grew = (added > 0.5).then_some((added, target.x));
        }
        eprintln!("[vitascope] 視窗配合影片 {w}×{h} → {:.0}×{:.0}", target.x, target.y);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(target));
        self.keep_on_screen(ctx, target);
    }

    /// 視窗改大之後會超出螢幕的話（例如直式影片，下面的控制列被工作列蓋住），往上、往左移回螢幕裡。
    /// 只知道螢幕大小、不知道螢幕在哪裡：視窗在主螢幕的範圍內才調整
    fn keep_on_screen(&self, ctx: &egui::Context, inner: Vec2) {
        let (outer, inner_rect, monitor) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.inner_rect, v.monitor_size)
        });
        let (Some(outer), Some(inner_rect), Some(monitor)) = (outer, inner_rect, monitor) else {
            return;
        };
        if outer.min.x < 0.0 || outer.min.y < 0.0 || outer.min.x >= monitor.x || outer.min.y >= monitor.y {
            return;
        }
        // 標題列與邊框
        let frame = (outer.size() - inner_rect.size()).max(Vec2::ZERO);
        let size = inner + frame;
        let limit = monitor - vec2(0.0, TASKBAR_ALLOWANCE);
        let mut pos = outer.min;
        if pos.y + size.y > limit.y {
            pos.y = (limit.y - size.y).max(0.0);
        }
        if pos.x + size.x > limit.x {
            pos.x = (limit.x - size.x).max(0.0);
        }
        if pos != outer.min {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        }
    }

    /// 長寬比、裁切、旋轉改了：視窗寬度不變，高度配合新的比例（不超過螢幕）
    fn refit_height(&mut self, ctx: &egui::Context) {
        let Some([w, h]) = self.player.state.video_size else {
            return;
        };
        let (fullscreen, maximized, monitor) = ctx.input(|i| {
            (
                i.viewport().fullscreen.unwrap_or(false),
                i.viewport().maximized.unwrap_or(false),
                i.viewport().monitor_size,
            )
        });
        let content = ctx.content_rect();
        if fullscreen || maximized || w <= 0 || h <= 0 {
            return;
        }
        let max_height = monitor.map_or(f32::INFINITY, |m| m.y * 0.9);
        // 影片的寬度（不含播放清單）
        let width = (content.width() - self.playlist_width).max(1.0);
        let height = (width * h as f32 / w as f32 + self.controls_height).min(max_height);
        let size = vec2(content.width(), height);
        ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        self.keep_on_screen(ctx, size);
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
                ui.heading(app_name());
                ui.label(crate::tf!("版本 {}", "Version {}", update::current_version()));
            });
            ui.add_space(8.0);
            ui.label(crate::tr!("跨平台影片播放器，以 libmpv 為播放引擎。", "A cross-platform video player powered by libmpv."));
            ui.add_space(8.0);
            egui::Grid::new("about_grid")
                .num_columns(2)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    ui.label(crate::tr!("作者", "Author"));
                    ui.hyperlink_to(update::AUTHOR, update::AUTHOR_URL);
                    ui.end_row();
                    ui.label("GitHub");
                    ui.hyperlink_to("acer1204/VitaScope", update::REPO_URL);
                    ui.end_row();
                    ui.label(crate::tr!("授權", "License"));
                    ui.hyperlink_to(crate::tr!("GPL-3.0-or-later（開放原始碼）", "GPL-3.0-or-later (open source)"), update::LICENSE_URL);
                    ui.end_row();
                    ui.label(crate::tr!("播放引擎", "Engine"));
                    ui.label(&self.engine_versions);
                    ui.end_row();
                    if self.engine_lgpl {
                        ui.label(crate::tr!("引擎授權", "Engine license"));
                        ui.hyperlink_to("LGPL-2.1-or-later", update::NOTICES_URL);
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(crate::tr!("可以自由使用、修改、散布；散布修改後的版本時，也必須公開原始碼。", "Free to use, modify and share; modified versions you distribute must also publish their source code."))
                    .small()
                    .color(Color32::from_gray(150)),
            );
            ui.separator();

            match &status {
                None => {
                    if ui.button(crate::tr!("檢查更新", "Check for updates")).clicked() {
                        start_check = true;
                    }
                }
                Some(UpdateStatus::Checking) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(crate::tr!("正在檢查更新…", "Checking for updates…"));
                    });
                }
                Some(UpdateStatus::UpToDate { latest }) => {
                    ui.label(crate::tf!("已經是最新版本（{latest}）", "You have the latest version ({latest})"));
                }
                Some(UpdateStatus::NoRelease) => {
                    ui.label(crate::tr!("GitHub 上還沒有發佈任何版本", "No version has been released on GitHub yet"));
                }
                Some(UpdateStatus::Failed(e)) => {
                    ui.colored_label(Color32::from_rgb(0xff, 0x8a, 0x80), crate::tf!("檢查更新失敗：{e}", "Update check failed: {e}"));
                    ui.horizontal(|ui| {
                        if ui.button(crate::tr!("再試一次", "Try again")).clicked() {
                            start_check = true;
                        }
                        ui.hyperlink_to(crate::tr!("開啟發佈頁面", "Open the releases page"), update::RELEASES_URL);
                    });
                }
                Some(UpdateStatus::Available { latest }) => {
                    ui.label(crate::tf!(
                        "有新版本 {latest}（目前 {}），要開啟下載頁面嗎？", "Version {latest} is available (you have {}). Open the download page?",
                        update::current_version()
                    ));
                    ui.horizontal(|ui| {
                        if ui.button(crate::tr!("是", "Yes")).clicked() {
                            open_releases = true;
                        }
                        if ui.button(crate::tr!("否", "No")).clicked() {
                            dismiss_update = true;
                        }
                    });
                }
            }
            ui.separator();
            ui.vertical_centered(|ui| {
                if ui.button(crate::tr!("關閉", "Close")).clicked() {
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

    /// 「字幕外觀」視窗：改了馬上套用（影片繼續播，看得到效果），關掉時存檔
    fn subtitle_style_window(&mut self, ctx: &egui::Context) {
        if !self.sub_style_open {
            return;
        }
        let mut open = true;
        let mut changed = false;
        let mut reset = false;
        egui::Window::new(crate::tr!("字幕外觀", "Subtitle style"))
            .id(Id::new("subtitle_style"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(pos2(40.0, 40.0))
            .show(ctx, |ui| {
                let style = &mut self.settings.subtitle;
                egui::Grid::new("subtitle_style_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(crate::tr!("字型", "Font"));
                        let current = SubStyle::font_choices()
                            .iter()
                            .find(|(name, _)| *name == style.effective_font())
                            .map_or_else(
                                || style.effective_font().to_owned(),
                                |(name, label)| crate::tr!(*label, *name).to_owned(),
                            );
                        egui::ComboBox::from_id_salt("subtitle_font")
                            .selected_text(current)
                            .show_ui(ui, |ui| {
                                for (name, label) in SubStyle::font_choices() {
                                    changed |= ui
                                        .selectable_value(&mut style.font, (*name).to_owned(), crate::tr!(*label, *name))
                                        .changed();
                                }
                            });
                        ui.end_row();

                        ui.label(crate::tr!("其他字型", "Other font"));
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut style.font)
                                    .hint_text(crate::tr!("輸入字型名稱", "Type a font name"))
                                    .desired_width(180.0),
                            )
                            .changed();
                        ui.end_row();

                        ui.label(crate::tr!("大小", "Size"));
                        changed |= ui
                            .add(egui::Slider::new(&mut style.size, 20.0..=100.0).step_by(1.0))
                            .changed();
                        ui.end_row();

                        ui.label(crate::tr!("文字顏色", "Text color"));
                        changed |= ui.color_edit_button_srgba_unmultiplied(&mut style.color).changed();
                        ui.end_row();

                        ui.label(crate::tr!("邊框顏色", "Outline color"));
                        changed |= ui
                            .color_edit_button_srgba_unmultiplied(&mut style.border_color)
                            .changed();
                        ui.end_row();

                        ui.label(crate::tr!("邊框粗細", "Outline width"));
                        changed |= ui
                            .add(egui::Slider::new(&mut style.border_size, 0.0..=8.0).step_by(0.5))
                            .changed();
                        ui.end_row();

                        ui.label(crate::tr!("陰影", "Shadow"));
                        changed |= ui
                            .add(egui::Slider::new(&mut style.shadow, 0.0..=4.0).step_by(0.5))
                            .changed();
                        ui.end_row();

                        ui.label(crate::tr!("位置", "Position"));
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut style.position, 0.0..=100.0)
                                    .step_by(1.0)
                                    .custom_formatter(|v, _| match v {
                                        v if v >= 100.0 => crate::tr!("最下面", "Bottom").to_owned(),
                                        v if v <= 0.0 => crate::tr!("最上面", "Top").to_owned(),
                                        v => format!("{v:.0}"),
                                    }),
                            )
                            .changed();
                        ui.end_row();

                        ui.label("");
                        changed |= ui.checkbox(&mut style.bold, crate::tr!("粗體", "Bold")).changed();
                        ui.end_row();

                        ui.label("");
                        changed |= ui
                            .checkbox(&mut style.override_ass, crate::tr!("也套用到 ASS 字幕", "Also apply to ASS subtitles"))
                            .on_hover_text(crate::tr!("字幕組的 ASS 字幕有自己的字型、顏色和特效，勾選後會被這裡的設定蓋掉", "ASS subtitles carry their own fonts, colors and effects; this replaces them with the settings here"))
                            .changed();
                        ui.end_row();
                    });
                ui.separator();
                if ui.button(crate::tr!("恢復預設", "Restore defaults")).clicked() {
                    reset = true;
                }
            });
        if reset {
            self.settings.subtitle = SubStyle::default();
            changed = true;
        }
        if changed {
            self.player.apply_sub_style(&self.settings.subtitle);
        }
        if !open {
            self.sub_style_open = false;
            self.save_settings();
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
            || self.pointer_over_playlist
            || egui::DragAndDrop::has_any_payload(ctx)
            || self.seek_drag.is_some()
            || menu_open
            // 設定、字幕外觀、控制面板、關於這些視窗開著時，滑鼠游標不能消失
            || self.settings_open
            || self.sub_style_open
            || self.panel_open
            || self.about_open;
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
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, crate::tr!("影片畫面", "Video")));
        let st = &self.player.state;

        // 沒有任何影像（連專輯封面都沒有）的檔案不畫：不然會留著上一個檔案的最後一格
        let has_picture = st.tracks_of(TrackKind::Video).next().is_some();
        if (st.loading || (st.loaded && has_picture))
            && let Some(video) = &self.video
        {
            video.paint(ui, rect);
        }
        if st.loaded && !st.has_video() && !st.tracks.is_empty() {
            audio_info(ui, rect, st);
        }
        if !st.loaded
            && !st.loading
            && let Some(path) = self.placeholder(ui, rect)
        {
            self.open_recent(&path);
        }

        // 選單開著時點畫面只是關掉選單，不要順便暫停
        let menu_was_open = self.popup_open_at_start;
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
            let hint = if self.settings.show_playlist {
                crate::tr!("放開以加入播放清單", "Drop to add to the playlist")
            } else {
                crate::tr!("放開以播放", "Drop to play")
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                hint,
                FontId::proportional(28.0),
                Color32::WHITE,
            );
        }

        // Ctrl / Cmd + 滾輪、觸控板捏合：縮放畫面
        let zoom_delta = ui.ctx().input(|i| i.zoom_delta());
        if response.hovered() && (zoom_delta - 1.0).abs() > 1e-3 {
            self.run(ui.ctx(), Action::Zoom(f64::from(zoom_delta.log2())));
        }
        // 滑鼠滾輪調音量（比照 PotPlayer）
        let steps = self.wheel_steps(ui.ctx(), response.hovered());
        if steps != 0 {
            self.run(ui.ctx(), Action::Volume(5.0 * f64::from(steps)));
        }
        response.context_menu(|ui| self.context_menu(ui));

        self.paint_info(&ui.ctx().clone(), rect);
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

    /// 在影片上按右鍵的選單。視窗矮（例如小影片的 640×421）時選單會超出視窗，下面的項目點不到，
    /// 所以放在可以捲動的區域裡
    fn context_menu(&mut self, ui: &mut egui::Ui) {
        // 扣掉選單的邊框和上下的空隙
        let max_height = (ui.ctx().content_rect().height() - 48.0).max(120.0);
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .show(ui, |ui| self.context_menu_items(ui));
    }

    fn context_menu_items(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut action = None;
        let mut open_recent = None;
        let mut set_speed = None;
        let mut seek_chapter = None;

        if menu_item(ui, true, crate::tr!("開啟檔案…", "Open file…"), OPEN_SHORTCUT) {
            action = Some(Action::Open);
        }
        let recent: Vec<String> = self.history.recent.iter().take(RECENT_IN_MENU).cloned().collect();
        let mut clear_recent = false;
        ui.add_enabled_ui(!recent.is_empty(), |ui| {
            ui.menu_button(crate::tr!("最近開啟的檔案", "Recent files"), |ui| {
                for p in &recent {
                    if ui.button(file_name(Path::new(p))).on_hover_text(p).clicked() {
                        open_recent = Some(p.clone());
                    }
                }
                ui.separator();
                if ui.button(crate::tr!("清除清單", "Clear list")).clicked() {
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
        if menu_item(
            ui,
            loaded,
            if loaded && !st.paused {
                crate::tr!("暫停", "Pause")
            } else {
                crate::tr!("播放", "Play")
            },
            crate::tr!("空白鍵", "Space"),
        ) {
            action = Some(Action::TogglePause);
        }
        if menu_item(ui, loaded, crate::tr!("停止", "Stop"), "") {
            action = Some(Action::Stop);
        }
        if menu_item(ui, has_prev, crate::tr!("上一個檔案", "Previous file"), "PgUp") {
            action = Some(Action::PrevFile);
        }
        if menu_item(ui, has_next, crate::tr!("下一個檔案", "Next file"), "PgDn") {
            action = Some(Action::NextFile);
        }
        let mut settings_changed = ui
            .checkbox(
                &mut self.settings.auto_next,
                crate::tr!("播完自動播放下一個", "Play the next file automatically"),
            )
            .changed();
        settings_changed |= ui
            .checkbox(
                &mut self.settings.resume,
                crate::tr!("從上次的位置繼續播放", "Resume from where I left off"),
            )
            .changed();
        ui.separator();

        let speed = st.speed;
        ui.menu_button(
            crate::tf!("播放速度（{}×）", "Speed ({}×)", fmt_speed(speed)),
            |ui| {
                for preset in SPEED_PRESETS {
                    let label = format!("{}×", fmt_speed(preset));
                    if ui.selectable_label((speed - preset).abs() < 1e-6, label).clicked() {
                        set_speed = Some(preset);
                    }
                }
                ui.separator();
                ui.weak(crate::tr!("C 加快、X 減慢、Z 恢復正常", "C faster, X slower, Z normal"));
            },
        );
        if menu_item(ui, loaded, crate::tr!("逐格前進", "Next frame"), ".") {
            action = Some(Action::FrameStep(true));
        }
        if menu_item(ui, loaded, crate::tr!("逐格後退", "Previous frame"), ",") {
            action = Some(Action::FrameStep(false));
        }
        let ab_label = match st.ab_loop {
            [None, _] => crate::tr!("A-B 重播：設定起點", "A-B loop: set start"),
            [Some(_), None] => crate::tr!("A-B 重播：設定終點", "A-B loop: set end"),
            [Some(_), Some(_)] => crate::tr!("取消 A-B 重播", "Cancel A-B loop"),
        };
        if menu_item(ui, loaded, ab_label, "L") {
            action = Some(Action::AbLoop);
        }
        if !st.chapters.is_empty() {
            let chapters = st.chapters.clone();
            let current = st.chapter;
            ui.menu_button(crate::tr!("章節", "Chapters"), |ui| {
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
                ui.weak(crate::tf!(
                    "上一章 / 下一章：{CHAPTER_SHORTCUT}",
                    "Previous / next chapter: {CHAPTER_SHORTCUT}"
                ));
            });
        }
        ui.separator();
        self.track_menu(ui, TrackKind::Audio, crate::tr!("音軌", "Audio"));
        self.track_menu(ui, TrackKind::Sub, crate::tr!("字幕", "Subtitles"));
        if let Some(a) = self.view_menu(ui) {
            action = Some(a);
        }
        if let Some(a) = self.picture_menu(ui) {
            action = Some(a);
        }
        if let Some(a) = self.sound_menu(ui) {
            action = Some(a);
        }
        let has_video = self.player.state.loaded && self.player.state.has_video();
        if let Some(a) = self.screenshot_menu(ui, has_video) {
            action = Some(a);
        }
        ui.separator();
        if menu_item(ui, true, crate::tr!("全螢幕", "Fullscreen"), "F") {
            action = Some(Action::ToggleFullscreen);
        }
        let playlist = egui::Button::selectable(self.settings.show_playlist, crate::tr!("播放清單", "Playlist"))
            .shortcut_text("F6");
        if ui.add(playlist).clicked() {
            action = Some(Action::TogglePlaylist);
        }
        let info =
            egui::Button::selectable(self.info_open, crate::tr!("媒體資訊", "Media info")).shortcut_text(INFO_SHORTCUT);
        if ui.add_enabled(loaded, info).clicked() {
            action = Some(Action::ToggleInfo);
        }
        if menu_item(ui, loaded, crate::tr!("複製媒體資訊", "Copy media info"), "") {
            action = Some(Action::CopyInfo);
        }
        let on_top = egui::Button::selectable(self.settings.always_on_top, crate::tr!("視窗置頂", "Always on top"))
            .shortcut_text(ON_TOP_SHORTCUT);
        if ui.add(on_top).clicked() {
            action = Some(Action::ToggleOnTop);
        }
        if menu_item(ui, true, crate::tr!("設定…", "Settings…"), "F5") {
            action = Some(Action::Settings);
        }
        if menu_item(ui, true, crate::tr!("關於影戲", "About VitaScope"), "F1") {
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
            let msg = crate::tf!(
                "章節 {}/{}：{}",
                "Chapter {}/{}: {}",
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
        ui.label(
            egui::RichText::new(app_name())
                .size(32.0)
                .color(Color32::from_gray(220)),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(crate::tf!(
                "把影片拖放到這裡，或按 {OPEN_SHORTCUT} 開啟檔案",
                "Drop a video here, or press {OPEN_SHORTCUT} to open a file"
            ))
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
                egui::RichText::new(crate::tr!("最近開啟", "Recent"))
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
                .on_hover_text(crate::tr!("上一個檔案（PgUp）", "Previous file (PgUp)"))
                .clicked()
            {
                self.run(ui.ctx(), Action::PrevFile);
            }
            if ui
                .add_enabled(loaded, icon_button(play_icon))
                .on_hover_text(crate::tr!("播放 / 暫停（空白鍵）", "Play / pause (Space)"))
                .clicked()
            {
                self.run(ui.ctx(), Action::TogglePause);
            }
            if ui
                .add_enabled(has_next, icon_button("⏭"))
                .on_hover_text(crate::tr!("下一個檔案（PgDn）", "Next file (PgDn)"))
                .clicked()
            {
                self.run(ui.ctx(), Action::NextFile);
            }
            if ui
                .add_enabled(loaded, icon_button("⏹"))
                .on_hover_text(crate::tr!("停止", "Stop"))
                .clicked()
            {
                self.run(ui.ctx(), Action::Stop);
            }
            let st = &self.player.state;
            let pos = self.seek_drag.unwrap_or(st.time_pos);
            let time = if loaded {
                format!("{} / {}", fmt_time(pos), fmt_time(st.duration.unwrap_or(0.0)))
            } else {
                "--:-- / --:--".to_owned()
            };
            // 視窗窄（最小寬度、直式影片）時右邊的按鈕會蓋到總長度（英文的按鈕比較寬、一小時以上的影片時間比較長）：
            // 放不下就只顯示目前時間，總長度放在提示裡
            let mono = ui.style().text_styles[&egui::TextStyle::Monospace].clone();
            let full_width = ui.painter().layout_no_wrap(time.clone(), mono, Color32::WHITE).size().x;
            if loaded && ui.available_width() < self.right_controls_width + full_width + ui.spacing().item_spacing.x {
                ui.label(egui::RichText::new(fmt_time(pos)).monospace())
                    .on_hover_text(time);
            } else {
                ui.label(egui::RichText::new(time).monospace());
            }
            // 速度不是 1× 時顯示在時間旁邊；視窗太窄、會擠到右邊的按鈕時就不顯示（OSD 和右鍵選單還看得到）
            if (st.speed - 1.0).abs() > 1e-6 {
                let font = ui.style().text_styles[&egui::TextStyle::Monospace].clone();
                let galley = ui
                    .painter()
                    .layout_no_wrap(format!("{}×", fmt_speed(st.speed)), font, ACCENT);
                let needed = galley.size().x + ui.spacing().item_spacing.x;
                if ui.available_width() >= self.right_controls_width + needed {
                    ui.label(galley).on_hover_text(crate::tr!(
                        "播放速度（C 加快、X 減慢、Z 恢復正常）",
                        "Playback speed (C faster, X slower, Z normal)"
                    ));
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(icon_button("⛶"))
                    .on_hover_text(crate::tr!("全螢幕（F / Enter）", "Fullscreen (F / Enter)"))
                    .clicked()
                {
                    self.run(ui.ctx(), Action::ToggleFullscreen);
                }
                if ui
                    .add(icon_button("🗁"))
                    .on_hover_text(crate::tf!("開啟檔案（{OPEN_SHORTCUT}）", "Open file ({OPEN_SHORTCUT})"))
                    .clicked()
                {
                    self.run(ui.ctx(), Action::Open);
                }
                if ui
                    .add(icon_button("ℹ"))
                    .on_hover_text(crate::tr!("關於影戲（F1）", "About VitaScope (F1)"))
                    .clicked()
                {
                    self.run(ui.ctx(), Action::About);
                }
                let list_button = egui::Button::selectable(self.settings.show_playlist, "☰").min_size(vec2(28.0, 22.0));
                if ui
                    .add(list_button)
                    .on_hover_text(crate::tr!("播放清單（F6）", "Playlist (F6)"))
                    .clicked()
                {
                    self.run(ui.ctx(), Action::TogglePlaylist);
                }
                self.track_menu(ui, TrackKind::Sub, crate::tr!("字幕", "Subtitles"));
                self.track_menu(ui, TrackKind::Audio, crate::tr!("音軌", "Audio"));
                self.volume_controls(ui);
                self.right_controls_width = ui.min_rect().width();
            });
        });
    }

    fn volume_controls(&mut self, ui: &mut egui::Ui) {
        // 音量條短一點，窄視窗時左邊的時間、速度才放得下（Slider 的寬度看 spacing，不看 add_sized）
        ui.spacing_mut().slider_width = 70.0;
        let total = self.player.volume_total();
        let mut volume = total;
        // 範圍到音量上限（預設 100%；調高之後超過 100% 的部分經過限幅器放大）
        let slider = egui::Slider::new(&mut volume, 0.0..=self.volume_cap())
            .show_value(false)
            .trailing_fill(true);
        // 音訊直通中：音量交給擴大機，滑桿停用
        let st = &self.player.state;
        let spdif = st.audio_spdif.is_some();
        let muted = st.muted;
        let response = ui
            .add_enabled_ui(!spdif, |ui| ui.add_sized([70.0, 20.0], slider))
            .inner
            .on_hover_text(crate::tf!("音量 {:.0}%（↑ ↓）", "Volume {:.0}% (↑ ↓)", total))
            .on_disabled_hover_text(sound::spdif_volume_hover());
        if response.changed() || response.drag_stopped() {
            // 拖曳中先即時調整，放開（或點一下）時改寫濾鏡鏈（超過 100% 時）
            self.set_volume_total(volume, response.drag_stopped() || !response.dragged());
            if muted && response.changed() {
                let _ = self.player.set_mute(false);
            }
        }
        // 直通中靜音、0% 都沒有作用（擴大機照樣出聲）：不顯示靜音的圖示，按鈕停用
        let icon = if !spdif && (muted || self.player.state.volume == 0.0) {
            "🔇"
        } else {
            "🔊"
        };
        if ui
            .add_enabled(!spdif, icon_button(icon))
            .on_hover_text(crate::tr!("靜音（M）", "Mute (M)"))
            .on_disabled_hover_text(sound::spdif_volume_hover())
            .clicked()
        {
            self.run(ui.ctx(), Action::ToggleMute);
        }
    }

    /// 右鍵選單的「畫面」：長寬比、裁切、縮放、移動、旋轉、翻轉。沒有影像（純音訊）時停用
    fn view_menu(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let st = &self.player.state;
        let has_video = st.loaded && st.has_video();
        let g = self.geometry.clone();
        let mut action = None;
        ui.add_enabled_ui(has_video, |ui| {
            // 英文是 View：新的「畫質」子選單叫 Picture
            ui.menu_button(crate::tr!("畫面", "Picture"), |ui| {
                ui.menu_button(
                    crate::tf!("畫面比例（{}）", "Aspect ratio ({})", g.aspect_label()),
                    |ui| {
                        if ui
                            .selectable_label(g.aspect.is_none(), crate::tr!("原始比例", "Original"))
                            .clicked()
                        {
                            action = Some(Action::SetAspect(None));
                        }
                        for (i, (label, _)) in ASPECTS.iter().enumerate() {
                            if ui.selectable_label(g.aspect == Some(i), *label).clicked() {
                                action = Some(Action::SetAspect(Some(i)));
                            }
                        }
                        ui.separator();
                        ui.weak(crate::tf!(
                            "A 或 {ASPECT_SHORTCUT} 依序切換",
                            "A or {ASPECT_SHORTCUT} cycles"
                        ));
                    },
                );
                ui.menu_button(crate::tf!("裁切（{}）", "Crop ({})", g.crop_label()), |ui| {
                    if ui
                        .selectable_label(g.crop.is_none(), crate::tr!("不裁切", "No crop"))
                        .clicked()
                    {
                        action = Some(Action::SetCrop(None));
                    }
                    for (i, (label, _)) in CROPS.iter().enumerate() {
                        if ui
                            .selectable_label(g.crop == Some(i), crate::tf!("裁成 {label}", "Crop to {label}"))
                            .clicked()
                        {
                            action = Some(Action::SetCrop(Some(i)));
                        }
                    }
                    ui.separator();
                    if ui
                        .selectable_label(
                            g.fill,
                            crate::tr!("填滿視窗（裁掉黑邊）", "Fill window (cut the black bars)"),
                        )
                        .clicked()
                    {
                        action = Some(Action::ToggleFill);
                    }
                    ui.weak(crate::tf!("{CROP_SHORTCUT} 依序切換", "{CROP_SHORTCUT} cycles"));
                });
                ui.separator();
                if menu_item(ui, true, crate::tr!("放大", "Zoom in"), "9") {
                    action = Some(Action::Zoom(ZOOM_STEP));
                }
                if menu_item(ui, true, crate::tr!("縮小", "Zoom out"), "1") {
                    action = Some(Action::Zoom(-ZOOM_STEP));
                }
                if menu_item(
                    ui,
                    true,
                    &crate::tf!("重設縮放（{:.0}%）", "Reset zoom ({:.0}%)", g.zoom_percent()),
                    "5",
                ) {
                    action = Some(Action::ZoomReset);
                }
                ui.menu_button(crate::tr!("移動畫面", "Move picture"), |ui| {
                    for (label, key, dx, dy) in [
                        (crate::tr!("左移", "Left"), "←", -PAN_STEP, 0.0),
                        (crate::tr!("右移", "Right"), "→", PAN_STEP, 0.0),
                        (crate::tr!("上移", "Up"), "↑", 0.0, -PAN_STEP),
                        (crate::tr!("下移", "Down"), "↓", 0.0, PAN_STEP),
                    ] {
                        if menu_item(ui, true, label, &format!("{ALT_KEY}+{key}")) {
                            action = Some(Action::Pan(dx, dy));
                        }
                    }
                    ui.separator();
                    if menu_item(ui, true, crate::tr!("置中", "Center"), PAN_CENTER_SHORTCUT) {
                        action = Some(Action::PanCenter);
                    }
                });
                ui.separator();
                ui.menu_button(crate::tf!("旋轉（{}°）", "Rotate ({}°)", g.rotate), |ui| {
                    for (deg, label) in [
                        (0, crate::tr!("不旋轉", "No rotation")),
                        (90, crate::tr!("順時針 90°", "90° clockwise")),
                        (180, "180°"),
                        (270, crate::tr!("逆時針 90°", "90° counter-clockwise")),
                    ] {
                        if ui.selectable_label(g.rotate == deg, label).clicked() {
                            action = Some(Action::SetRotate(deg));
                        }
                    }
                    ui.separator();
                    ui.weak(crate::tf!("{ALT_KEY}+K 依序旋轉", "{ALT_KEY}+K rotates"));
                });
                let hflip =
                    egui::Button::selectable(g.hflip, crate::tr!("左右翻轉（鏡像）", "Flip horizontally (mirror)"))
                        .shortcut_text(FLIP_H_SHORTCUT);
                if ui.add(hflip).clicked() {
                    action = Some(Action::Flip(true));
                }
                let vflip = egui::Button::selectable(g.vflip, crate::tr!("上下翻轉", "Flip vertically"))
                    .shortcut_text(FLIP_V_SHORTCUT);
                if ui.add(vflip).clicked() {
                    action = Some(Action::Flip(false));
                }
                ui.separator();
                if menu_item(
                    ui,
                    !g.is_default(),
                    crate::tr!("重設畫面", "Reset picture"),
                    &format!("{ALT_KEY}+Backspace"),
                ) {
                    action = Some(Action::ResetView);
                }
            });
        });
        action
    }

    /// 控制列與右鍵選單的「字幕」「音軌」選單：選軌道、延遲、載入檔案；字幕另有第二字幕、編碼、外觀
    fn track_menu(&mut self, ui: &mut egui::Ui, kind: TrackKind, name: &str) {
        let is_sub = kind == TrackKind::Sub;
        let st = &self.player.state;
        let loaded = st.loaded;
        let tracks: Vec<(i64, String)> = st.tracks_of(kind).map(|t| (t.id, t.label())).collect();
        let selected = st.selected(kind).map(|t| t.id);
        let secondary = st.secondary_sid;
        let delay = if is_sub { st.sub_delay } else { st.audio_delay };
        // 選中的是外掛文字字幕：可以換編碼重新載入
        let encoding = st
            .selected(kind)
            .filter(|_| is_sub)
            .and_then(|t| self.player.external_sub_info(t))
            .and_then(|info| {
                info.encoding
                    .map(|used| (used, info.detected.unwrap_or(used), info.forced))
            });

        let mut choice: Option<Option<i64>> = None;
        let mut secondary_choice: Option<Option<i64>> = None;
        let mut reload: Option<Option<&'static encoding_rs::Encoding>> = None;
        let mut action = None;
        ui.add_enabled_ui(loaded, |ui| {
            ui.menu_button(name, |ui| {
                if is_sub
                    && ui
                        .selectable_label(selected.is_none(), crate::tr!("關閉字幕", "Subtitles off"))
                        .clicked()
                {
                    choice = Some(None);
                }
                for (id, label) in &tracks {
                    if ui.selectable_label(selected == Some(*id), label).clicked() {
                        choice = Some(Some(*id));
                    }
                }
                if tracks.is_empty() {
                    ui.weak(if is_sub {
                        crate::tr!("（沒有字幕）", "(no subtitles)")
                    } else {
                        crate::tr!("（沒有音軌）", "(no audio)")
                    });
                }
                ui.separator();
                if is_sub && !tracks.is_empty() {
                    ui.menu_button(crate::tr!("第二字幕", "Secondary subtitle"), |ui| {
                        if ui
                            .selectable_label(secondary.is_none(), crate::tr!("關閉", "Off"))
                            .clicked()
                        {
                            secondary_choice = Some(None);
                        }
                        for (id, label) in tracks.iter().filter(|(id, _)| Some(*id) != selected) {
                            if ui.selectable_label(secondary == Some(*id), label).clicked() {
                                secondary_choice = Some(Some(*id));
                            }
                        }
                        ui.separator();
                        ui.weak(crate::tr!(
                            "和主字幕同時顯示，在畫面上方",
                            "Shown together with the main subtitle, at the top"
                        ));
                    });
                }
                // 延遲的子選單點了不關閉，才能連按
                let delay_menu = egui::containers::menu::SubMenuButton::new(crate::tf!(
                    "{}延遲：{}",
                    "{} delay: {}",
                    if is_sub {
                        crate::tr!("字幕", "Subtitle")
                    } else {
                        crate::tr!("音訊", "Audio")
                    },
                    fmt_delay(delay)
                ))
                .config(
                    egui::containers::menu::MenuConfig::new()
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                );
                delay_menu.ui(ui, |ui| {
                    let step = |d| {
                        if is_sub {
                            Action::SubDelay(d)
                        } else {
                            Action::AudioDelay(d)
                        }
                    };
                    ui.horizontal(|ui| {
                        if ui.button(crate::tr!("−0.1 秒", "−0.1 s")).clicked() {
                            action = Some(step(Some(-0.1)));
                        }
                        if ui.button(crate::tr!("+0.1 秒", "+0.1 s")).clicked() {
                            action = Some(step(Some(0.1)));
                        }
                        if ui.button(crate::tr!("歸零", "Reset")).clicked() {
                            action = Some(step(None));
                        }
                    });
                    ui.weak(if is_sub {
                        crate::tr!(
                            "快捷鍵 [ / ]。正數 = 字幕晚一點出現",
                            "Keys [ / ]. Positive = subtitles appear later"
                        )
                    } else {
                        crate::tr!(
                            "快捷鍵 - / =。正數 = 聲音晚一點",
                            "Keys - / =. Positive = sound plays later"
                        )
                    });
                });
                if let Some((current, detected, forced)) = encoding {
                    ui.menu_button(crate::tr!("字幕編碼", "Subtitle encoding"), |ui| {
                        let auto = crate::tf!("自動判斷（{detected}）", "Auto-detect ({detected})");
                        if ui.selectable_label(!forced, auto).clicked() {
                            reload = Some(None);
                        }
                        ui.separator();
                        for (zh, en, enc) in crate::subs::ENCODINGS {
                            if ui
                                .selectable_label(forced && enc.name() == current, crate::tr!(*zh, *en))
                                .clicked()
                            {
                                reload = Some(Some(*enc));
                            }
                        }
                    });
                }
                ui.separator();
                let load = if is_sub {
                    crate::tr!("載入字幕檔…", "Load subtitle file…")
                } else {
                    crate::tr!("載入音軌檔…", "Load audio file…")
                };
                if ui.button(load).clicked() {
                    action = Some(if is_sub {
                        Action::LoadSubtitle
                    } else {
                        Action::LoadAudio
                    });
                }
                if is_sub && ui.button(crate::tr!("字幕外觀…", "Subtitle style…")).clicked() {
                    action = Some(Action::SubtitleStyle);
                }
            });
        });
        if let Some(id) = choice {
            self.select_track(kind, id);
        }
        if let Some(id) = secondary_choice {
            let _ = self.player.set_secondary_sub(id);
            let label = id
                .and_then(|id| self.player.state.tracks_of(kind).find(|t| t.id == id))
                .map_or_else(|| crate::tr!("關閉", "Off").to_owned(), |t| t.label());
            self.osd(crate::tf!("第二字幕：{label}", "Secondary subtitle: {label}"));
        }
        if let (Some(encoding), Some(id)) = (reload, selected) {
            match self.player.reload_subtitle(id, encoding) {
                Ok(()) => self.osd(crate::tf!(
                    "字幕編碼：{}",
                    "Subtitle encoding: {}",
                    encoding.map_or(crate::tr!("自動判斷", "auto-detect"), |e| e.name())
                )),
                Err(e) => self.osd(crate::tf!("無法重新載入字幕：{e}", "Cannot reload the subtitle: {e}")),
            }
        }
        if let Some(a) = action {
            let ctx = ui.ctx().clone();
            self.run(&ctx, a);
        }
    }

    fn progress_bar(&mut self, ui: &mut egui::Ui) {
        let st = &self.player.state;
        let duration = st.duration.unwrap_or(0.0);
        let can_seek = st.loaded && st.seekable && duration > 0.0;
        // 固定的 id：拖曳中切換全螢幕時，進度條換到浮動的控制列，拖曳還是同一個
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
        let response = ui.interact(rect, Id::new("seek_bar"), Sense::click_and_drag());
        let active = can_seek && (response.hovered() || response.dragged());
        // 自己畫的元件也要有無障礙資訊：螢幕閱讀器、介面測試才找得到
        response.widget_info(|| egui::WidgetInfo::slider(can_seek, st.time_pos, crate::tr!("進度", "Progress")));

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
            // 時間上面是那個位置的畫面
            let ctx = ui.ctx().clone();
            self.paint_preview(&ctx, t, hover.x, text_pos.y - 3.0, rect);
        }
    }
}

impl eframe::App for VitascopeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.render_tick(ctx);
        for ev in self.player.poll() {
            self.on_player_event(ev);
        }
        self.shader_tick();
        self.sound_tick();
        // 視窗出現之後才有螢幕可查
        if self.frames >= 2 {
            self.pacing_tick(ctx);
        }
        self.poll_playlist_scan();
        self.poll_screenshots(ctx);
        self.poll_previews(ctx);
        self.poll_folder_add();
        self.poll_instance(ctx);
        // macOS：已經開著時從 Finder 開的檔案
        #[cfg(target_os = "macos")]
        {
            let files = crate::macos_open::take();
            if !files.is_empty() {
                self.bring_to_front(ctx);
                self.open_paths(files, false);
            }
        }
        self.auto_next();
        self.autosave();
        self.refresh_deint_osd();
        if ctx.input(|i| i.pointer.delta() != Vec2::ZERO || i.pointer.any_down()) {
            self.last_activity = Instant::now();
        }
        self.handle_keys(ctx);
        self.handle_drops(ctx);
        self.update_title(ctx);
        self.keep_window_level(ctx);
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
        if let Some(shot) = &mut self.autoshot
            && shot.tick(ctx)
        {
            // 實機測試比對播放位置（縮到最小時聲音照樣播、位置照樣走）
            eprintln!("[vitascope] 截圖時的播放位置：{:.3} 秒", self.player.state.time_pos);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        let ctx = ui.ctx().clone();
        // 控制列的選單在畫面之前處理，點畫面時那個選單已經關掉了；所以在一開始就記下來
        self.popup_open_at_start = egui::Popup::is_any_open(&ctx);
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
        // 播放清單在影片右邊（控制列在下面、整個視窗寬，按鈕才放得下）
        self.playlist_width = 0.0;
        self.pointer_over_playlist = false;
        if self.settings.show_playlist {
            let r = egui::Panel::right("playlist")
                .frame(Self::playlist_frame())
                .resizable(true)
                .default_size(self.playlist_width_pref)
                .size_range(180.0..=600.0)
                .show(ui, |ui| self.playlist_panel(ui));
            self.playlist_width = r.response.rect.width();
            self.playlist_width_pref = self.playlist_width;
            self.pointer_over_playlist = r.response.contains_pointer();
        }

        egui::CentralPanel::no_frame()
            .frame(Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| self.video_area(ui));

        let mut overlay_height = 0.0;
        if fullscreen {
            if show_controls {
                let screen = ctx.content_rect();
                // 播放清單開著時，控制列只蓋在影片上
                let width = screen.width() - self.playlist_width;
                let r = egui::Area::new(Id::new("overlay_controls"))
                    .anchor(Align2::LEFT_BOTTOM, Vec2::ZERO)
                    .show(&ctx, |ui| {
                        ui.set_width(width);
                        Frame::NONE
                            .fill(Color32::from_black_alpha(170))
                            .inner_margin(Margin::symmetric(16, 10))
                            .show(ui, |ui| {
                                ui.set_width(width - 32.0);
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
        self.subtitle_style_window(&ctx);
        self.settings_window(&ctx);
        self.control_panel(&ctx);
        self.typing_last_frame = ctx.text_edit_focused();
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        // 先停止接收：之後送來的檔案由下一個啟動的程式自己開（不會收了又沒開）
        let mut leftover = Vec::new();
        if let Some(p) = &self.instance {
            p.stop_accepting();
            while let Ok(req) = p.rx.try_recv() {
                self.batch.push(req, Instant::now());
            }
            leftover = self.batch.take_pending();
        }
        if let Some(video) = &self.video {
            video.destroy(gl);
        }
        self.remember_position();
        self.save_settings();
        self.persist_playlist();
        // 之後啟動的程式自己當主視窗
        if let Some(p) = &mut self.instance {
            p.shutdown();
        }
        // 剛送來、還沒開的檔案（例如雙擊影片後馬上關掉視窗）：交給新開的程式（自動測試不開）
        if !leftover.is_empty()
            && self.persist_playlist
            && let Ok(exe) = std::env::current_exe()
        {
            let _ = std::process::Command::new(exe).args(&leftover).spawn();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// 網址（串流、mpv 的 av:// 之類），不是本機檔案
fn is_url(path: &str) -> bool {
    path.contains("://")
}

/// 延遲：0 →「0 秒」、0.3 →「+0.3 秒」、-0.25 →「-0.25 秒」
fn fmt_delay(seconds: f64) -> String {
    if seconds.abs() < 0.0005 {
        return crate::tr!("0 秒", "0 s").to_owned();
    }
    let s = format!("{seconds:+.3}");
    crate::tf!("{} 秒", "{} s", s.trim_end_matches('0').trim_end_matches('.'))
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
        .unwrap_or_else(|| crate::tf!("第 {} 章", "Chapter {}", index + 1))
}

/// 純音訊檔：歌名、演出者、專輯。有專輯封面時（mpv 把封面當成畫面畫出來）字放在下方，沒有封面就放中間
fn audio_info(ui: &mut egui::Ui, rect: Rect, st: &crate::player::State) {
    let has_cover = st.tracks_of(TrackKind::Video).any(|t| t.albumart);
    let title = st.tag("title").or(st.title.as_deref()).unwrap_or_default().to_owned();
    let details: Vec<&str> = ["artist", "album"].iter().filter_map(|k| st.tag(k)).collect();
    let lines = 1 + details.len();
    let height = 24.0 + 22.0 * lines as f32 + 24.0;
    let area = if has_cover {
        let band = Rect::from_min_max(pos2(rect.left(), rect.bottom() - height), rect.max);
        ui.painter()
            .rect_filled(band, CornerRadius::ZERO, Color32::from_black_alpha(150));
        band
    } else {
        Rect::from_center_size(rect.center(), vec2(rect.width(), height))
    };
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(area.shrink2(vec2(16.0, 12.0)))
            .layout(Layout::top_down(Align::Center)),
    );
    let prefix = if has_cover { "" } else { "♪  " };
    // 不能選取文字：可選取的文字會攔下滑鼠點擊，點在歌名上就不能暫停、開右鍵選單
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("{prefix}{title}"))
                .size(22.0)
                .color(Color32::from_gray(230)),
        )
        .selectable(false),
    );
    for d in details {
        ui.add(egui::Label::new(egui::RichText::new(d).size(16.0).color(Color32::from_gray(170))).selectable(false));
    }
}

/// 選單項目：文字 + 右側的快捷鍵說明，回傳是否被點選
fn menu_item(ui: &mut egui::Ui, enabled: bool, text: &str, shortcut: &str) -> bool {
    let mut button = egui::Button::new(text);
    if !shortcut.is_empty() {
        button = button.shortcut_text(shortcut);
    }
    ui.add_enabled(enabled, button).clicked()
}

/// 畫面相關的快捷鍵說明（macOS 的按鍵名稱不一樣）
const ALT_KEY: &str = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
const CROP_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "Control+Q"
} else {
    "Ctrl+Q"
};
const ASPECT_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+F6" } else { "Ctrl+F6" };
const FLIP_H_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+Z" } else { "Ctrl+Z" };
const FLIP_V_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+P" } else { "Ctrl+P" };
const PAN_CENTER_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+5" } else { "Ctrl+5" };
const ON_TOP_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+T" } else { "Ctrl+T" };

/// 跳章節的快捷鍵說明
const CHAPTER_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "Cmd+PgUp / PgDn"
} else {
    "Ctrl+PgUp / PgDn"
};

/// 開檔快捷鍵的說明文字（macOS 用 Command 鍵）
const OPEN_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+O" } else { "Ctrl+O" };
/// macOS 的 Ctrl+F1 是系統的「鍵盤操作」快捷鍵，用 Cmd+I（QuickTime 的「影片檢閱器」）
const INFO_SHORTCUT: &str = if cfg!(target_os = "macos") { "Cmd+I" } else { "Ctrl+F1" };

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

/// 使用者用 VITASCOPE_MPV_OPTS（或測試的 `Options.extra`）自己指定了這個 mpv 選項（就不自動調整）
fn mpv_opts_override(player: &Player, name: &str) -> bool {
    player.user_overrides().contains(name)
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
    use super::{fmt_delay, fmt_speed, fmt_time, is_mesa_software_renderer, mpv_opts_override, short_versions};

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
    fn formats_delay() {
        assert_eq!(fmt_delay(0.0), "0 秒");
        assert_eq!(fmt_delay(0.0001), "0 秒");
        assert_eq!(fmt_delay(0.3), "+0.3 秒");
        assert_eq!(fmt_delay(-0.25), "-0.25 秒");
        assert_eq!(fmt_delay(1.0), "+1 秒");
        assert_eq!(fmt_delay(-0.1 - 0.2), "-0.3 秒");
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

    #[test]
    fn user_options_stop_the_automatic_dumb_mode() {
        use crate::player::{Options, Player};
        let player = |extra: Vec<(String, String)>| {
            Player::new(Options {
                extra,
                ..Options::headless()
            })
            .unwrap()
        };
        let own = player(vec![("gpu-dumb-mode".into(), "no".into())]);
        assert!(mpv_opts_override(&own, "gpu-dumb-mode"));
        assert!(!mpv_opts_override(&own, "scale"));
        if std::env::var_os("VITASCOPE_MPV_OPTS").is_none() {
            assert!(!mpv_opts_override(&player(Vec::new()), "gpu-dumb-mode"));
        }
    }
}
