//! 匯出：片段（不重新編碼）、轉成 GIF、縮圖總覽圖的共用基礎。
//!
//! - 設定（[`ExportSettings`]）：存放的資料夾、片段的格式、GIF 與縮圖總覽圖上次的選擇。
//! - 背景工作（[`Job`] / [`Ctl`]）：一次一個，有進度、可以取消；`Job` 被丟掉時（關閉影戲、介面測試結束）
//!   取消、等一下、刪掉暫存檔。
//! - 結果與原因是列舉（[`Progress`]、[`Done`]、[`Failure`]、[`Note`]）：背景執行緒不組給使用者看的文字，
//!   介面執行緒呼叫 `message()`，切換語言後也是對的語言。
//! - 檔名、預設資料夾、磁碟空間、寫好之後重新打開檢查（[`verify_media`]）、mpv 記錄對應到原因（[`map_mpv_error`]）。
//!
//! 匯出都在另外開的 mpv 裡做（不動正在播放的那一個）。FFmpeg 的記錄只送到第一個建立的 mpv（主播放器），
//! 所以這裡判斷失敗只看 mpv 自己的訊息（「Failed writing packet」「Disabling filter」之類）。
//!
//! 各種匯出在子模組：[`clip`]（片段）、[`gif`]（轉成 GIF）。

pub mod clip;
pub mod gif;

use crate::geometry::Geometry;
use crate::instance::Wake;
use crate::mpv::{Event, Mpv};
use crate::player::{Track, TrackKind};
use crate::save;
use crate::screenshot::{self, Fixup, KnownDir};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

// ───────────── 設定 ─────────────

/// GIF 的長邊（像素）可以選的值
pub const GIF_LONG_SIDES: [u32; 4] = [320, 480, 640, 800];
/// GIF 的格率可以選的值
pub const GIF_FPS: [u32; 4] = [10, 15, 20, 25];
/// GIF 最長幾秒
pub const GIF_MAX_SECS: f64 = 30.0;
/// 縮圖總覽圖的寬度可以選的值
pub const SHEET_WIDTHS: [u32; 4] = [1280, 1920, 2560, 3840];
pub const SHEET_COLUMNS: std::ops::RangeInclusive<u32> = 1..=10;
pub const SHEET_ROWS: std::ops::RangeInclusive<u32> = 1..=20;
pub const JPEG_QUALITY: std::ops::RangeInclusive<u8> = 50..=100;

/// 匯出的設定（`settings.json` 的 `export`）。每一組都有自己的預設值（見各組的 `Default`）
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportSettings {
    /// 片段的資料夾；None = 「影片」資料夾裡的 VitaScope
    pub clip_dir: Option<PathBuf>,
    /// GIF、縮圖總覽圖的資料夾；None = 截圖資料夾
    pub image_dir: Option<PathBuf>,
    pub clip: ClipPrefs,
    pub gif: GifPrefs,
    pub sheet: SheetPrefs,
}

/// 片段
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipPrefs {
    pub format: ClipFormat,
}

/// 片段的格式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClipFormat {
    /// 照來源：MP4 → MP4、WebM → WebM、TS → TS，其他存 MKV（只有聲音時依音訊格式）
    #[default]
    Auto,
    Mkv,
    Mp4,
    Webm,
    Ts,
}

impl ClipFormat {
    pub const ALL: [ClipFormat; 5] = [
        ClipFormat::Auto,
        ClipFormat::Mkv,
        ClipFormat::Mp4,
        ClipFormat::Webm,
        ClipFormat::Ts,
    ];

    /// 設定頁、匯出視窗上的名稱（格式名稱不翻譯）
    pub fn label(self) -> &'static str {
        match self {
            ClipFormat::Auto => crate::tr!("自動（照來源）", "Automatic (like the source)"),
            ClipFormat::Mkv => "MKV",
            ClipFormat::Mp4 => "MP4",
            ClipFormat::Webm => "WebM",
            ClipFormat::Ts => "TS",
        }
    }
}

/// 轉成 GIF 上次的選擇
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GifPrefs {
    /// 長邊（像素）
    pub long_side: u32,
    pub fps: u32,
    /// 包含目前顯示的字幕
    pub subtitles: bool,
}

impl Default for GifPrefs {
    fn default() -> Self {
        Self {
            long_side: 480,
            fps: 15,
            subtitles: true,
        }
    }
}

/// 縮圖總覽圖上次的選擇
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SheetPrefs {
    pub columns: u32,
    pub rows: u32,
    /// 整張圖的寬度（像素）
    pub width: u32,
    /// 每張縮圖上的時間
    pub timestamps: bool,
    /// 最上面的檔案資訊（檔名、大小、長度、格式）
    pub header: bool,
    pub format: ImageFormat,
    pub jpeg_quality: u8,
}

impl Default for SheetPrefs {
    fn default() -> Self {
        Self {
            columns: 4,
            rows: 5,
            width: 1920,
            timestamps: true,
            header: true,
            format: ImageFormat::Jpeg,
            jpeg_quality: 90,
        }
    }
}

/// 縮圖總覽圖的圖檔格式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageFormat {
    #[default]
    Jpeg,
    Png,
}

impl ImageFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Png => "png",
        }
    }
}

/// 最接近的選項（一樣近時取小的）
fn snap(v: u32, choices: &[u32]) -> u32 {
    choices.iter().copied().min_by_key(|c| c.abs_diff(v)).unwrap_or(v)
}

impl ExportSettings {
    /// 讀檔後整理：選項對齊可以選的值、數字拉回範圍內、不是完整路徑的資料夾當成沒設定
    pub fn sanitized(mut self) -> Self {
        let g = &mut self.gif;
        g.long_side = snap(g.long_side, &GIF_LONG_SIDES);
        g.fps = snap(g.fps, &GIF_FPS);
        let s = &mut self.sheet;
        s.columns = s.columns.clamp(*SHEET_COLUMNS.start(), *SHEET_COLUMNS.end());
        s.rows = s.rows.clamp(*SHEET_ROWS.start(), *SHEET_ROWS.end());
        s.width = snap(s.width, &SHEET_WIDTHS);
        s.jpeg_quality = s.jpeg_quality.clamp(*JPEG_QUALITY.start(), *JPEG_QUALITY.end());
        // 相對路徑會跟著目前資料夾變（從不同的地方開影戲就存到不同的地方）
        for dir in [&mut self.clip_dir, &mut self.image_dir] {
            if dir.as_ref().is_some_and(|d| !d.is_absolute()) {
                *dir = None;
            }
        }
        self
    }

    /// 片段實際存放的資料夾
    pub fn clip_folder(&self) -> PathBuf {
        self.clip_dir.clone().unwrap_or_else(default_clip_dir)
    }

    /// GIF、縮圖總覽圖實際存放的資料夾（沒設定時跟截圖放一起；`screenshot_dir` 是截圖資料夾的設定）
    pub fn image_folder(&self, screenshot_dir: Option<&Path>) -> PathBuf {
        self.image_dir
            .clone()
            .or_else(|| screenshot_dir.map(Path::to_path_buf))
            .unwrap_or_else(screenshot::default_dir)
    }
}

/// 片段預設的資料夾：系統的「影片」資料夾裡的 VitaScope（Windows 的「影片」常由 OneDrive 同步）
pub fn default_clip_dir() -> PathBuf {
    screenshot::known_dir(KnownDir::Videos)
        .or_else(|| screenshot::home().map(|h| h.join(KnownDir::Videos.fallback_name())))
        .unwrap_or_else(std::env::temp_dir)
        .join("VitaScope")
}

// ───────────── 工作、進度、結果 ─────────────

/// 匯出的種類
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Clip,
    Gif,
    Sheet,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Clip => crate::tr!("片段", "Clip"),
            Kind::Gif => "GIF",
            Kind::Sheet => crate::tr!("縮圖總覽圖", "Thumbnail sheet"),
        }
    }
}

/// 進行到哪個階段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 讀進快取
    Reading,
    /// 寫檔（片段：不能中斷）
    Writing,
    /// 重新打開檢查
    Checking,
    /// 轉換畫面（GIF）
    Converting,
    /// 產生色盤、寫入 GIF
    Palette,
    /// 擷取第幾張縮圖
    Grabbing { done: u32, total: u32 },
    /// 合成整張圖
    Composing,
}

impl Phase {
    pub fn label(self) -> String {
        match self {
            Phase::Reading => crate::tr!("讀取中", "Reading").into(),
            Phase::Writing => crate::tr!("寫入中", "Writing").into(),
            Phase::Checking => crate::tr!("檢查中", "Checking").into(),
            Phase::Converting => crate::tr!("轉換中", "Converting").into(),
            Phase::Palette => crate::tr!("產生色盤、寫入 GIF…", "Building the palette and writing the GIF…").into(),
            Phase::Grabbing { done, total } => {
                crate::tf!("擷取畫面 {done}/{total}", "Grabbing frames {done}/{total}")
            }
            Phase::Composing => crate::tr!("合成圖片…", "Composing…").into(),
        }
    }
}

/// 進度
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub phase: Phase,
    /// 0–1；None = 不知道（進度條顯示動畫）
    pub fraction: Option<f32>,
}

/// 完成時附帶的說明
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// 不重新編碼：起點、終點對齊關鍵影格，片段可能比選的長一點
    KeyframeAligned,
    /// 片段不含字幕軌、不保留語言標籤（目前播放引擎的限制）
    NoSubtitleTrack,
    /// 片段沒有聲音：正在播放的音軌放不進片段（外掛的音軌、不能直接存的音訊格式），檔案裡也沒有別的放得進的
    NoAudioTrack,
    /// 播放引擎沒有色調映射濾鏡：HDR 影片的亮部會變白
    HdrClipped,
    /// 杜比視界 Profile 5 不轉換顏色（顏色可能不對）
    DolbyVision5,
}

impl Note {
    pub fn message(self) -> &'static str {
        match self {
            Note::KeyframeAligned => crate::tr!(
                "不重新編碼：起點、終點對齊關鍵影格，片段可能比選的長幾秒",
                "No re-encoding: the start and end snap to keyframes, so the clip can be a few seconds longer"
            ),
            Note::NoSubtitleTrack => crate::tr!(
                "片段不含字幕，也不保留音軌的語言標籤",
                "Clips contain no subtitles and don't keep the audio language tag"
            ),
            Note::NoAudioTrack => crate::tr!(
                "片段沒有聲音：目前的音軌是外掛的，或是不能直接存成片段的格式",
                "The clip has no sound: the current audio track is external, or its format can't be saved as a clip"
            ),
            Note::HdrClipped => crate::tr!(
                "播放引擎沒有色調映射濾鏡：HDR 影片的亮部會變白",
                "The playback engine has no tone-mapping filter: bright parts of HDR video clip to white"
            ),
            Note::DolbyVision5 => crate::tr!(
                "杜比視界 Profile 5 的顏色無法轉換，可能偏色",
                "Dolby Vision Profile 5 colors can't be converted and may look wrong"
            ),
        }
    }
}

/// 完成
#[derive(Debug, Clone, PartialEq)]
pub struct Done {
    pub kind: Kind,
    /// 存好的檔案（名稱可能是「… (2).mkv」）
    pub path: PathBuf,
    pub bytes: u64,
    /// 片段實際的範圍（主播放器的時間；對齊關鍵影格之後）
    pub actual: Option<(f64, f64)>,
    pub notes: Vec<Note>,
}

impl Done {
    /// 提示文字：「已儲存片段：名稱（大小）」
    pub fn message(&self) -> String {
        let name = self
            .path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let size = crate::mediainfo::fmt_size(i64::try_from(self.bytes).unwrap_or(i64::MAX));
        match self.kind {
            Kind::Clip => crate::tf!("已儲存片段：{name}（{size}）", "Clip saved: {name} ({size})"),
            Kind::Gif => crate::tf!("已儲存 GIF：{name}（{size}）", "GIF saved: {name} ({size})"),
            Kind::Sheet => crate::tf!("已儲存縮圖總覽圖：{name}", "Thumbnail sheet saved: {name}"),
        }
    }
}

/// 失敗的原因
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// 使用者取消
    Cancelled,
    /// 影片檔不見了（被移動、刪除）
    SourceMissing,
    /// 影片檔還在，但打不開（沒有讀取的權限、別的程式鎖著）
    SourceNoAccess,
    /// 網路影片的網址打不開（連結過期、網路斷線；原因只在主播放器的 FFmpeg 記錄裡）
    SourceUnreachable,
    /// 看不懂影片檔的格式
    SourceUnreadable,
    /// 無法在這裡建立檔案（沒有寫入的權限）；知道的話帶著資料夾
    NoPermission(Option<PathBuf>),
    /// 開始前檢查：磁碟空間不夠（需要、剩下的位元組）
    DiskFull { need: u64, free: u64 },
    /// 寫到一半失敗（多半是磁碟滿了）
    WriteFailed,
    /// 無法建立讀取用的暫存檔（暫存資料夾的磁碟滿了）
    CacheFailed,
    /// 選的格式放不下其中一條軌道（看 mpv 的記錄知道的，不知道是哪一條）
    CantMux,
    /// 選的格式放不下這種編碼（開始前就知道）
    Incompatible { container: String, codec: String },
    /// 播放引擎沒有這種格式的寫檔程式
    FormatMissing,
    /// 這段範圍讀不到資料
    NoData,
    /// 寫出來的檔案比預期短（多半是寫到一半磁碟滿了）；秒
    TooShort { got: f64, want: f64 },
    /// 寫出來的檔案少了這種軌道
    MissingTrack(TrackKind),
    /// 寫出來的檔案打不開；mpv 的原文
    Unplayable(Option<String>),
    /// 一段時間讀不到新的資料（網路磁碟、網路斷線）
    ReadTimeout,
    /// 播放引擎沒有需要的編碼器（GIF）
    EncoderMissing,
    /// 畫面的濾鏡失敗（例如色調映射處理不了這種 HDR）
    FilterFailed,
    /// 匯出用的 mpv 裡找不到對應的軌道
    TrackNotFound,
    /// 用了章節連結（ordered chapters）、EDL、CUE：時間跟檔案對不上
    Timeline,
    /// 直播不能匯出片段
    Live,
    /// 直播、長度不明的串流（GIF、縮圖總覽圖要知道長度）
    Unbounded,
    /// 這個檔案沒有影像（只有聲音、專輯封面）
    NoVideo,
    /// GIF 太長（最長 `GIF_MAX_SECS` 秒）
    GifTooLong,
    /// 範圍太短（GIF 至少 0.2 秒）
    RangeTooShort,
    /// 網路影片有總長度、但不能跳轉（伺服器不支援 Range）：匯出用的 mpv 讀不到中間的段落
    NotSeekable,
    /// 播放引擎沒有匯出片段要的指令（dump-cache）
    NoDump,
    /// 這種音訊格式不能直接存成片段（APE、DSD、Musepack）
    AudioCodec,
    /// 寫好了，但換不成正式的名稱（暫存檔留著，內容是完整的；登記在快取資料夾的清單裡，
    /// 啟動時清暫存檔不會刪它，見 [`keep_file`]）
    Finish { error: String, temp: PathBuf },
    /// 其他檔案操作的錯誤（系統的原文）
    Io(String),
    /// 其他 mpv 的錯誤（mpv 的原文，第一行錯誤）
    Engine(String),
    /// 匯出的程式出錯（不會發生）
    Crashed,
}

impl Failure {
    /// 原因（介面執行緒呼叫，用目前的語言）
    pub fn message(&self) -> String {
        use crate::{tf, tr};
        match self {
            Failure::Cancelled => tr!("已取消", "Cancelled").into(),
            Failure::SourceMissing => tr!(
                "找不到影片檔（可能已經被移動或刪除）",
                "The video file can't be found (it may have been moved or deleted)"
            )
            .into(),
            Failure::SourceNoAccess => tr!(
                "無法讀取影片檔（沒有讀取的權限，或正被別的程式使用）",
                "Couldn't read the video file (no permission, or another program is using it)"
            )
            .into(),
            Failure::SourceUnreachable => tr!(
                "無法讀取影片的網址（連結可能已經過期，或網路斷線）",
                "Couldn't open the video's address (the link may have expired, or the connection dropped)"
            )
            .into(),
            Failure::SourceUnreadable => tr!("看不懂這個檔案的格式", "The file format isn't recognized").into(),
            Failure::NoPermission(Some(dir)) => {
                let dir = dir.display();
                tf!("沒有寫入的權限：{dir}", "No permission to write to {dir}")
            }
            Failure::NoPermission(None) => tr!(
                "無法建立檔案（沒有寫入的權限？）",
                "Couldn't create the file (no permission to write?)"
            )
            .into(),
            Failure::DiskFull { need, free } => {
                let need = size_text(*need);
                let free = size_text(*free);
                tf!(
                    "磁碟空間不足（需要約 {need}，剩 {free}）",
                    "Not enough disk space (needs about {need}, {free} free)"
                )
            }
            Failure::WriteFailed => tr!(
                "無法寫入檔案（磁碟空間可能不足）",
                "Couldn't write the file (the disk may be full)"
            )
            .into(),
            Failure::CacheFailed => tr!(
                "無法建立暫存檔（暫存資料夾所在的磁碟可能已滿）",
                "Couldn't create the temporary file (the disk with the cache folder may be full)"
            )
            .into(),
            Failure::CantMux => tr!(
                "這個格式放不下其中一條軌道，請改用 MKV",
                "This format can't hold one of the tracks; use MKV instead"
            )
            .into(),
            Failure::Incompatible { container, codec } => tf!(
                "{container} 不能放 {codec}，請改用 MKV",
                "{container} can't hold {codec}; use MKV instead"
            ),
            Failure::FormatMissing => tr!(
                "播放引擎不支援這種格式",
                "The playback engine doesn't support this format"
            )
            .into(),
            Failure::NoData => tr!("這段範圍讀不到資料", "No data in this range").into(),
            Failure::TooShort { got, want } => tf!(
                "匯出的檔案不完整（{got:.1} 秒，應該約 {want:.1} 秒），已刪除",
                "The exported file is incomplete ({got:.1} s instead of about {want:.1} s) and was deleted"
            ),
            Failure::MissingTrack(kind) => {
                let what = match kind {
                    TrackKind::Video => tr!("影像", "the video"),
                    TrackKind::Audio => tr!("聲音", "the audio"),
                    TrackKind::Sub => tr!("字幕", "the subtitles"),
                    TrackKind::Other => tr!("一條軌道", "a track"),
                };
                tf!(
                    "匯出的檔案少了{what}，已刪除",
                    "The exported file is missing {what} and was deleted"
                )
            }
            Failure::Unplayable(Some(reason)) => tf!(
                "匯出的檔案無法播放，已刪除（{reason}）",
                "The exported file doesn't play and was deleted ({reason})"
            ),
            Failure::Unplayable(None) => tr!(
                "匯出的檔案無法播放，已刪除",
                "The exported file doesn't play and was deleted"
            )
            .into(),
            Failure::ReadTimeout => tr!("讀取逾時", "Reading timed out").into(),
            Failure::EncoderMissing => tr!("播放引擎不支援轉成 GIF", "The playback engine can't make GIFs").into(),
            Failure::FilterFailed => tr!(
                "無法轉換這部影片的畫面（濾鏡失敗）",
                "Couldn't convert this video's picture (a filter failed)"
            )
            .into(),
            Failure::TrackNotFound => tr!("找不到對應的軌道", "Couldn't find the matching track").into(),
            Failure::Timeline => tr!(
                "這個檔案用了章節連結（ordered chapters），不能匯出",
                "This file uses linked chapters (ordered chapters) and can't be exported"
            )
            .into(),
            Failure::Live => tr!("直播不能匯出片段", "Clips can't be saved from live streams").into(),
            Failure::Unbounded => tr!(
                "直播或長度不明的串流不能使用",
                "Not available for live streams or streams of unknown length"
            )
            .into(),
            Failure::NoVideo => tr!("這個檔案沒有影像", "This file has no video").into(),
            Failure::GifTooLong => {
                let max = GIF_MAX_SECS;
                tf!(
                    "GIF 最長 {max:.0} 秒，請縮短 A-B 段落",
                    "GIFs can be at most {max:.0} seconds; shorten the A-B range"
                )
            }
            Failure::RangeTooShort => {
                let min = gif::MIN_SECS;
                tf!("範圍太短（至少 {min} 秒）", "The range is too short (at least {min} s)")
            }
            Failure::NotSeekable => tr!(
                "這個網路影片不能跳轉（伺服器不支援）",
                "This online video isn't seekable (the server doesn't support it)"
            )
            .into(),
            Failure::NoDump => tr!("播放引擎不支援匯出片段", "The playback engine can't save clips").into(),
            Failure::AudioCodec => tr!(
                "這種音訊格式不能直接存成片段",
                "This audio format can't be saved as a clip"
            )
            .into(),
            Failure::Finish { error, temp } => {
                let temp = temp.display();
                // 留下的檔案名稱還是暫存檔的樣子，請使用者自己改名（登記過，啟動時清暫存檔不會刪它）
                tf!(
                    "無法完成存檔：{error}。完整的檔案留在 {temp}，請自己改成想要的名稱",
                    "Couldn't finish saving: {error}. The complete file is at {temp}; rename it yourself"
                )
            }
            Failure::Io(e) | Failure::Engine(e) => e.clone(),
            Failure::Crashed => tr!("匯出時發生錯誤", "Something went wrong while exporting").into(),
        }
    }

    /// 提示文字（一行）：取消時「已取消匯出」，其他「無法匯出：原因」。
    /// 原因裡有路徑、系統或 mpv 的原文的，提示只說個大概（完整的在匯出視窗，[`details`](Self::details)）
    pub fn osd(&self) -> String {
        use crate::{tf, tr};
        let see = tr!("（詳情見匯出視窗）", " (see the Export window)");
        let reason = match self {
            Failure::Cancelled => return tr!("已取消匯出", "Export cancelled").into(),
            Failure::NoPermission(Some(_)) => Failure::NoPermission(None).message(),
            Failure::Unplayable(Some(_)) => Failure::Unplayable(None).message(),
            Failure::Finish { .. } => format!("{}{see}", tr!("無法完成存檔", "Couldn't finish saving")),
            Failure::Io(_) | Failure::Engine(_) => format!("{}{see}", tr!("發生錯誤", "An error occurred")),
            other => other.message(),
        };
        tf!("無法匯出：{reason}", "Export failed: {reason}")
    }

    /// 匯出視窗裡的完整說明：「無法匯出：原因」（含路徑、原文）；取消時「已取消匯出」
    pub fn details(&self) -> String {
        match self {
            Failure::Cancelled => self.osd(),
            other => {
                let reason = other.message();
                crate::tf!("無法匯出：{reason}", "Export failed: {reason}")
            }
        }
    }
}

fn size_text(bytes: u64) -> String {
    crate::mediainfo::fmt_size(i64::try_from(bytes).unwrap_or(i64::MAX))
}

/// 背景工作送給介面的消息
#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    Progress(Progress),
    Finished(Result<Done, Failure>),
}

/// 進度最多這麼久送一次（換階段時馬上送）
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// `Job` 被丟掉時最多等工作結束這麼久（關閉影戲時不要卡太久）
pub const DROP_WAIT: Duration = Duration::from_secs(3);

/// 工作和介面共用的狀態
#[derive(Default)]
struct Shared {
    cancel: AtomicBool,
    /// 正在寫檔、不能中斷（片段的 dump-cache）
    writing: AtomicBool,
    /// 還沒換成正式名稱的暫存檔（取消、失敗、丟掉 `Job` 時刪掉）
    temp: Mutex<Option<PathBuf>>,
    /// 換不成正式名稱、留下的完整檔案登記在這個資料夾的清單裡（[`keep_file`]）；None = 不登記
    keep_dir: Mutex<Option<PathBuf>>,
}

impl Shared {
    fn temp(&self) -> Option<PathBuf> {
        self.temp.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn take_temp(&self) -> Option<PathBuf> {
        self.temp.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// 一個在背景執行的匯出工作（同時只有一個）
pub struct Job {
    kind: Kind,
    shared: Arc<Shared>,
    rx: Receiver<JobEvent>,
    thread: Option<JoinHandle<()>>,
}

impl Job {
    /// 開一個背景執行緒跑 `run`；有新的進度、做完時呼叫 `wake`（叫介面重畫）。
    /// `run` 回傳錯誤、出錯（panic）時刪掉登記的暫存檔
    pub fn spawn(kind: Kind, wake: Wake, run: impl FnOnce(&Ctl) -> Result<Done, Failure> + Send + 'static) -> Job {
        let shared = Arc::new(Shared::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let ctl = Ctl {
            tx,
            shared: shared.clone(),
            wake,
            last: Cell::new(None),
        };
        let thread = std::thread::Builder::new()
            .name("vitascope-export".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&ctl)))
                    .unwrap_or(Err(Failure::Crashed));
                // 還登記著的暫存檔：失敗、取消（成功時已經換成正式名稱；`Finish` 失敗時刻意留著）
                if let Some(temp) = ctl.shared.take_temp() {
                    let _ = std::fs::remove_file(temp);
                }
                let _ = ctl.tx.send(JobEvent::Finished(result));
                (ctl.wake)();
            })
            .expect("無法建立匯出的執行緒");
        Job {
            kind,
            shared,
            rx,
            thread: Some(thread),
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// 要求取消（工作在下一個檢查點停下；寫檔中不能中斷的，寫完才停，寫好的也不會變成正式的檔案）
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
    }

    /// 現在能不能取消（片段寫檔中不能中斷：取消按鈕停用）
    pub fn cancellable(&self) -> bool {
        !self.shared.writing.load(Ordering::SeqCst)
    }

    /// 下一個消息（沒有時 None）
    pub fn try_recv(&self) -> Option<JobEvent> {
        self.rx.try_recv().ok()
    }

    /// 背景執行緒結束了（結果可能還在等 `try_recv`）
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// 放棄這個工作：取消、最多等 `timeout`，刪掉還在的暫存檔（`Drop` 用 `DROP_WAIT`）
    pub fn abandon(mut self, timeout: Duration) {
        self.stop(timeout);
    }

    fn stop(&mut self, timeout: Duration) {
        let Some(thread) = self.thread.take() else { return };
        self.cancel();
        let deadline = Instant::now() + timeout;
        while !thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if thread.is_finished() {
            // 執行緒結束時已經刪掉登記的暫存檔
            let _ = thread.join();
            return;
        }
        // 工作還沒停（寫檔中、讀網路磁碟卡住）：先試著刪掉暫存檔，但登記留著，執行緒結束時會再刪一次：
        // Windows 上 mpv 還開著檔案時刪不掉（FFmpeg 開檔時不允許別人刪除），檔案也可能在這之後才建立。
        // 工作已經取消，之後的 `Ctl::finish` 不會把它換成正式的檔案
        if let Some(temp) = self.shared.temp() {
            let _ = std::fs::remove_file(temp);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop(DROP_WAIT);
    }
}

/// 背景工作用來回報進度、看是否取消、登記暫存檔
pub struct Ctl {
    tx: Sender<JobEvent>,
    shared: Arc<Shared>,
    wake: Wake,
    /// 上次送出進度的時間與階段
    last: Cell<Option<(Instant, Phase)>>,
}

impl Ctl {
    /// 回報進度（同一個階段最多每 0.1 秒送一次）
    pub fn progress(&self, p: Progress) {
        let now = Instant::now();
        let due = self
            .last
            .get()
            .is_none_or(|(at, phase)| phase != p.phase || now.duration_since(at) >= PROGRESS_EVERY);
        if due {
            self.last.set(Some((now, p.phase)));
            let _ = self.tx.send(JobEvent::Progress(p));
            (self.wake)();
        }
    }

    /// 使用者按了取消
    pub fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::SeqCst)
    }

    /// 取消了就回傳 `Err(Cancelled)`（`?` 用）
    pub fn check(&self) -> Result<(), Failure> {
        if self.cancelled() {
            Err(Failure::Cancelled)
        } else {
            Ok(())
        }
    }

    /// 開始、結束不能中斷的寫檔（這段時間介面的取消按鈕停用）
    pub fn set_writing(&self, on: bool) {
        self.shared.writing.store(on, Ordering::SeqCst);
    }

    /// 在 `dir` 準備一個暫存檔名稱並登記（失敗、取消時刪掉）
    pub fn temp_in(&self, dir: &Path, stem: &str, ext: &str) -> PathBuf {
        let temp = save::temp_in(dir, stem, ext);
        self.set_temp(Some(temp.clone()));
        temp
    }

    /// 登記（或取消登記）暫存檔
    pub fn set_temp(&self, temp: Option<PathBuf>) {
        *self.shared.temp.lock().unwrap_or_else(|e| e.into_inner()) = temp;
    }

    /// 登記的暫存檔
    pub fn temp(&self) -> Option<PathBuf> {
        self.shared.temp()
    }

    /// 換不成正式名稱時，留下的完整檔案登記在 `dir`（快取的 `export` 資料夾，[`cache_dir`]）的清單裡：
    /// 啟動時清暫存檔（[`sweep_leftovers`]）不會刪它。每種匯出開始時都要設定（App 有快取資料夾時才會清暫存檔）
    pub fn set_keep_dir(&self, dir: Option<PathBuf>) {
        *self.shared.keep_dir.lock().unwrap_or_else(|e| e.into_inner()) = dir;
    }

    /// 登記留下的檔案的資料夾（[`set_keep_dir`](Self::set_keep_dir)；測試確認每種匯出都有設定）
    #[doc(hidden)]
    pub fn keep_dir(&self) -> Option<PathBuf> {
        self.shared.keep_dir.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 把登記的暫存檔換成正式的名稱（絕不覆蓋，見 `save::finish_new`）。
    /// 已經取消時（包括 `Job` 被丟掉時還在寫檔的工作）不換，回傳 `Cancelled`，暫存檔由執行緒結束時刪掉。
    /// 換名稱失敗時暫存檔留著（內容是完整的），原因裡寫它在哪裡
    pub fn finish(&self, wanted: &Path) -> Result<PathBuf, Failure> {
        self.check()?;
        // 沒有登記暫存檔、沒有檔名：匯出的程式寫錯了（不給使用者看英文的內部說明）
        let temp = self.temp().ok_or(Failure::Crashed)?;
        if wanted.file_name().is_none() {
            return Err(Failure::Crashed);
        }
        match save::finish_new(&temp, wanted) {
            Ok(path) => {
                self.set_temp(None);
                Ok(path)
            }
            // 暫存檔不見了（被別人刪掉、根本沒寫出來）：沒有東西可以留
            Err(e) if !temp.exists() => Err(Failure::Io(e.to_string())),
            Err(e) => {
                // 內容是完整的，留著：提示裡寫出位置，請使用者自己改名。名稱還是暫存檔的樣子，
                // 登記起來，啟動時清暫存檔才不會把它當成當掉留下的刪掉
                self.set_temp(None);
                let keep_dir = self.shared.keep_dir.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(dir) = keep_dir
                    && let Err(err) = keep_file(&dir, &temp)
                {
                    eprintln!("[vitascope] 無法登記留下的檔案 {}：{err}", temp.display());
                }
                Err(Failure::Finish {
                    error: e.to_string(),
                    temp,
                })
            }
        }
    }
}

// ───────────── 檔名、時間 ─────────────

/// 檔名裡的時間：`HH.MM.SS`（無條件捨去到秒）
pub fn name_time(t: f64) -> String {
    let s = t.max(0.0).floor() as u64;
    format!("{:02}.{:02}.{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// 片段、GIF 的檔名（不含副檔名）：「第1集 00.12.03-00.12.45」
pub fn range_stem(source: &str, title: Option<&str>, a: f64, b: f64) -> String {
    format!(
        "{} {}-{}",
        screenshot::source_stem(source, title),
        name_time(a),
        name_time(b)
    )
}

/// 縮圖總覽圖的檔名（不含副檔名）：「第1集 縮圖」/「第1集 thumbnails」
pub fn sheet_stem(source: &str, title: Option<&str>) -> String {
    let stem = screenshot::source_stem(source, title);
    crate::tf!("{stem} 縮圖", "{stem} thumbnails")
}

/// 使用者改過的檔名：換掉不能用的字元；空白時用產生的名稱
pub fn chosen_stem(edited: &str, generated: &str) -> String {
    if edited.trim().is_empty() {
        generated.to_owned()
    } else {
        screenshot::sanitize_stem(edited.trim())
    }
}

/// 看懂使用者打的時間：`83.5`、`1:23.5`、`01:02:03.250`，全形冒號「：」也可以。
/// 負數、看不懂的、分秒超過 59 的回傳 None
pub fn parse_time(text: &str) -> Option<f64> {
    let text = text.trim().replace('：', ":");
    let parts: Vec<&str> = text.split(':').map(str::trim).collect();
    if parts.len() > 3 {
        return None;
    }
    let (last, init) = parts.split_last()?;
    // 只接受數字和小數點（不接受 -1、1e3、inf 這類 f64 看得懂的寫法）
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !number(last)
        || init
            .iter()
            .any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let secs: f64 = last.parse().ok()?;
    if !init.is_empty() && secs >= 60.0 {
        return None;
    }
    let mut total = secs;
    for (i, p) in init.iter().rev().enumerate() {
        let v: u64 = p.parse().ok()?;
        // 時:分:秒 的分要小於 60；最前面的那一個（分或時）不限
        if i + 1 < init.len() && v >= 60 {
            return None;
        }
        total += v as f64 * 60f64.powi(i as i32 + 1);
    }
    total.is_finite().then_some(total)
}

/// 範圍欄位顯示的時間：`00:12:03.120`
pub fn format_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

// ───────────── 留下的暫存檔 ─────────────

/// 快取資料夾底下匯出用的子資料夾（片段太大時 mpv 的磁碟快取放這裡）
pub const CACHE_SUBDIR: &str = "export";
/// 超過這麼久沒有修改的暫存檔才刪（還在寫的檔案一直在更新）
pub const LEFTOVER_AGE: Duration = Duration::from_secs(3600);

/// 匯出用的快取資料夾：`<快取>/export`
pub fn cache_dir(cache_root: &Path) -> PathBuf {
    cache_root.join(CACHE_SUBDIR)
}

/// mpv 的磁碟快取檔（`demuxer-cache-dir` 裡的 `mpv-cache-XXXXXX.dat`；Windows 上影戲被強制結束時會留下）
fn is_mpv_cache(name: &str) -> bool {
    name.starts_with("mpv-cache-") && name.ends_with(".dat")
}

/// 啟動時清掉上次留下的暫存檔（當掉、被強制結束、關機時沒機會刪）：
/// `<快取>/export` 裡影戲的暫存檔與 mpv 的磁碟快取檔，以及匯出資料夾（`folders`）裡影戲的暫存檔，
/// 都只刪超過一小時沒有修改的。登記過的完整檔案（換不成正式名稱留下的，[`keep_file`]）不刪。
/// 回傳刪了幾個。資料夾可能在網路磁碟上，要在背景執行緒呼叫
pub fn sweep_leftovers(cache_root: &Path, folders: &[PathBuf]) -> usize {
    let cache = cache_dir(cache_root);
    // 比對檔名就好：暫存檔的名稱有行程編號與流水號，不會重複（資料夾的寫法可能不同：大小寫、結尾的斜線）
    let kept: Vec<String> = kept_files(&cache)
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let is_kept = |name: &str| kept.iter().any(|k| k == name);
    let mut removed = save::sweep_matching(
        &cache,
        |n| (save::is_part(n) && !is_kept(n)) || is_mpv_cache(n),
        LEFTOVER_AGE,
    );
    let mut seen: Vec<&PathBuf> = Vec::new();
    for dir in folders {
        if !seen.contains(&dir) {
            seen.push(dir);
            removed += save::sweep_matching(dir, |n| save::is_part(n) && !is_kept(n), LEFTOVER_AGE);
        }
    }
    removed
}

/// 留下的完整檔案的清單（`<快取>/export/kept.json`，JSON 的路徑陣列）
pub const KEPT_LIST: &str = "kept.json";

/// 同一個行程裡改清單的不要同時做（讀、加、寫回）
static KEPT_LOCK: Mutex<()> = Mutex::new(());

/// 讀清單（壞掉、沒有時是空的）
fn read_kept(dir: &Path) -> Vec<PathBuf> {
    std::fs::read(dir.join(KEPT_LIST))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<PathBuf>>(&b).ok())
        .unwrap_or_default()
}

/// 寫回清單：先寫暫存檔再換掉，寫到一半不會留下壞掉的清單
fn write_kept(dir: &Path, list: &[PathBuf]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let temp = save::temp_in(dir, "kept", "json");
    let text = serde_json::to_vec_pretty(list).map_err(std::io::Error::other)?;
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, dir.join(KEPT_LIST)).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// 登記一個換不成正式名稱、留下的完整檔案（`dir` = 快取的 `export` 資料夾）：
/// 名稱還是暫存檔的樣子，啟動時清暫存檔看到清單裡有它就不刪（使用者改名或刪掉之後才從清單拿掉）
pub fn keep_file(dir: &Path, path: &Path) -> std::io::Result<()> {
    let _guard = KEPT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = read_kept(dir);
    if !list.iter().any(|p| p == path) {
        list.push(path.to_path_buf());
    }
    write_kept(dir, &list)
}

/// 登記過、還在的檔案（`dir` = 快取的 `export` 資料夾）。確定已經不在的（使用者改名、刪掉了）從清單拿掉
pub fn kept_files(dir: &Path) -> Vec<PathBuf> {
    let _guard = KEPT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let list = read_kept(dir);
    // 確定不在 = 所在的資料夾讀得到、檔案不在裡面。資料夾查不到（隨身碟拔掉、網路磁碟還沒連上、
    // 沒有權限：系統一樣回報「找不到」）的算還在，拿掉的話之後接回來時就會被當成暫存檔刪掉
    let gone = |p: &PathBuf| {
        let folder_there = p
            .parent()
            .is_some_and(|d| std::fs::metadata(d).is_ok_and(|m| m.is_dir()));
        folder_there && matches!(std::fs::symlink_metadata(p), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    };
    let alive: Vec<PathBuf> = list.iter().filter(|p| !gone(p)).cloned().collect();
    if alive.len() != list.len() {
        let _ = write_kept(dir, &alive);
    }
    alive
}

// ───────────── 磁碟空間 ─────────────

/// 最接近的已經存在的資料夾（匯出資料夾可能還沒建立）
fn existing_ancestor(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|d| !d.as_os_str().is_empty() && d.is_dir())
}

/// `dir` 所在的磁碟還剩多少空間（這個使用者能用的）；查不到時 None（不算錯誤）
pub fn free_space(dir: &Path) -> Option<u64> {
    sys_free_space(existing_ancestor(dir)?)
}

#[cfg(windows)]
fn sys_free_space(dir: &Path) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide = wide(dir);
    let mut avail = 0u64;
    // SAFETY: 以 0 結尾的 UTF-16 字串；用不到的輸出給 null
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(avail)
}

#[cfg(unix)]
fn sys_free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: c 是以 0 結尾的路徑，st 由 statvfs 填好（成功時）
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: 成功時 statvfs 已經填好整個結構
    let st = unsafe { st.assume_init() };
    // 各系統的欄位型別不同（macOS 的 f_bavail 是 32 位元）
    #[allow(clippy::unnecessary_cast)]
    let (blocks, size) = (st.f_bavail as u64, st.f_frsize as u64);
    Some(blocks.saturating_mul(size))
}

#[cfg(windows)]
fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// `dir` 在哪個磁碟（同一個磁碟的需求要加起來算）；查不到時 None
fn volume_of(dir: &Path) -> Option<String> {
    let dir = existing_ancestor(dir)?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
        let wide = wide(dir);
        let mut buf = [0u16; 1024];
        // SAFETY: 以 0 結尾的輸入；輸出緩衝區的長度照實給
        let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if ok == 0 {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]).to_uppercase())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(dir).ok().map(|m| m.dev().to_string())
    }
}

/// 一個資料夾要用的空間
#[derive(Debug, Clone, PartialEq)]
struct Space {
    /// 在哪個磁碟（None = 不知道，自己算一組）
    volume: Option<String>,
    /// 剩多少（None = 查不到，不檢查）
    free: Option<u64>,
    need: u64,
}

/// 同一個磁碟的需求加起來，跟剩下的空間比；不夠時回傳（需要, 剩下）
fn shortfall(spaces: &[Space]) -> Option<(u64, u64)> {
    let mut groups: Vec<(Option<&str>, Option<u64>, u64)> = Vec::new();
    for s in spaces {
        let vol = s.volume.as_deref();
        match groups.iter_mut().find(|g| vol.is_some() && g.0 == vol) {
            Some(g) => g.2 = g.2.saturating_add(s.need),
            None => groups.push((vol, s.free, s.need)),
        }
    }
    groups
        .into_iter()
        .find_map(|(_, free, need)| free.filter(|&f| f < need).map(|f| (need, f)))
}

/// 開始前檢查空間：每一項是（資料夾, 需要的位元組）。片段要同時看目的地與磁碟快取的資料夾，
/// 兩個在同一個磁碟時加起來算。查不到剩多少的不算錯誤
pub fn check_space(needs: &[(&Path, u64)]) -> Result<(), Failure> {
    let spaces: Vec<Space> = needs
        .iter()
        .map(|&(dir, need)| Space {
            volume: volume_of(dir),
            free: free_space(dir),
            need,
        })
        .collect();
    match shortfall(&spaces) {
        Some((need, free)) => Err(Failure::DiskFull { need, free }),
        None => Ok(()),
    }
}

// ───────────── 轉正、mpv 記錄 ─────────────

/// GIF、縮圖總覽圖要轉正的部分：檔案本身的旋轉 + 使用者的旋轉、翻轉（跟畫面上看到的一樣，跟 Ctrl+E 截圖同一個規則）。
/// 不用 `video-out-params`，自動測試（沒有畫面輸出）也算得對
pub fn fixup(file_rotate: i64, geometry: &Geometry) -> Fixup {
    Fixup {
        rotate: (file_rotate.rem_euclid(360) as u32 + geometry.rotate) % 360,
        hflip: geometry.hflip,
        vflip: geometry.vflip,
    }
}

/// 匯出用的 mpv 都要的選項：不跑腳本、不呼叫 yt-dlp、不處理按鍵、不去影片的資料夾找外掛字幕、音軌
pub const INSTANCE_OPTIONS: &[(&str, &str)] = &[
    ("load-scripts", "no"),
    ("ytdl", "no"),
    ("osc", "no"),
    ("input-default-bindings", "no"),
    ("sub-auto", "no"),
    ("audio-file-auto", "no"),
];

/// mpv 的一行記錄
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub level: String,
    pub prefix: String,
    pub text: String,
}

impl LogLine {
    pub fn new(level: &str, prefix: &str, text: &str) -> Self {
        Self {
            level: level.to_owned(),
            prefix: prefix.to_owned(),
            text: text.trim_end().to_owned(),
        }
    }

    fn is_error(&self) -> bool {
        matches!(self.level.as_str(), "error" | "fatal")
    }
}

/// 最近的警告、錯誤（匯出用的 mpv 要 `request_log_messages("warn")`）
#[derive(Debug, Clone, Default)]
pub struct LogTail {
    lines: VecDeque<LogLine>,
}

impl LogTail {
    /// 最多留幾行
    pub const CAP: usize = 16;

    /// 收一個 mpv 事件裡的記錄（不是記錄、不是警告以上的不收）
    pub fn push_event(&mut self, ev: &Event) {
        if let Event::Log { prefix, level, text } = ev {
            self.push(LogLine::new(level, prefix, text));
        }
    }

    pub fn push(&mut self, line: LogLine) {
        if !matches!(line.level.as_str(), "warn" | "error" | "fatal") {
            return;
        }
        if self.lines.len() >= Self::CAP {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.iter().cloned().collect()
    }

    /// 清掉收過的（只看接下來的記錄，例如寫檔那一段）
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    /// 從收到的記錄判斷原因（見 `map_mpv_error`）
    pub fn failure(&self) -> Option<Failure> {
        map_mpv_error(&self.lines())
    }
}

/// mpv 自己的訊息 → 原因（依重要性：先找已知的訊息，都沒有時用第一行錯誤的原文；沒有錯誤時 None）。
/// 只看 mpv 的訊息：FFmpeg 的記錄只送到第一個建立的 mpv（主播放器），匯出用的 mpv 收不到
pub fn map_mpv_error(lines: &[LogLine]) -> Option<Failure> {
    /// 一種原因：mpv 的訊息（任何一個出現在錯誤記錄裡就算；`*` 代表中間任意的文字）、
    /// 從符合的那一行得出原因。依重要性排列：同時出現時前面的優先
    type Rule = (&'static [&'static str], fn(&str) -> Failure);
    const KNOWN: &[Rule] = &[
        // demux/cache.c、demux/demux.c：磁碟快取建立失敗、讀不回來
        (
            &[
                "Failed to create file cache",
                "Failed to create cache temporary file",
                "Failed to retrieve packet from cache",
                // 讀快取時寫不進磁碟快取（快取所在的磁碟滿了）
                "Failed to write to cache file",
                "Could not write all data",
            ],
            |_| Failure::CacheFailed,
        ),
        // common/recorder.c、common/encode_lavc.c：寫到一半失敗（磁碟滿了、隨身碟拔掉）
        (
            &[
                "Failed writing packet",
                "Writing trailer failed",
                "Writing packet failed",
                "error writing trailer",
                "Closing file failed",
            ],
            |_| Failure::WriteFailed,
        ),
        // 建立不了輸出檔
        (&["Failed opening output file", "could not open '"], |_| {
            Failure::NoPermission(None)
        }),
        (&["Output format not found", "format not found"], |_| {
            Failure::FormatMissing
        }),
        (
            &[
                "Can't mux one of the input streams",
                "Can't mux one of the attachments",
                "Writing header failed",
            ],
            |_| Failure::CantMux,
        ),
        // filters/f_output_chain.c、filters/f_lavfi.c：濾鏡失敗（mpv 會停用它，畫面照樣送出，要當成失敗）
        (
            &[
                "Disabling filter",
                "Cannot convert decoder/filter output",
                "filter failed to initialize",
                "parsing the filter graph failed",
                "not found or failed to allocate",
                "failed to configure the filter graph",
            ],
            |_| Failure::FilterFailed,
        ),
        // common/encode_lavc.c：沒有這種編碼器（「codec 'gif' not found.」「codec for video not found」）、
        // 編碼器打不開
        (
            &[
                "codec '*' not found",
                "codec for * not found",
                "Could not initialize encoder",
                "Failed to initialize muxer",
            ],
            |_| Failure::EncoderMissing,
        ),
        // stream/stream_file.c、stream/stream.c：來源打不開
        (&["Cannot open file '", "Failed to open "], source_failure),
        (&["Failed to recognize file format"], |_| Failure::SourceUnreadable),
        (&["No streams.", "no data written to target file"], |_| Failure::NoData),
    ];
    for (patterns, failure) in KNOWN {
        let hit = lines
            .iter()
            .filter(|l| l.is_error())
            .find(|l| patterns.iter().any(|p| matches_pattern(&l.text, p)));
        if let Some(line) = hit {
            return Some(failure(&line.text));
        }
    }
    lines
        .iter()
        .find(|l| l.is_error())
        .map(|l| Failure::Engine(l.text.trim().to_owned()))
}

/// `text` 含有 `pattern`；`*` 代表中間任意的文字（各段依序出現）
fn matches_pattern(text: &str, pattern: &str) -> bool {
    let mut rest = text;
    for piece in pattern.split('*') {
        match rest.find(piece) {
            Some(i) => rest = &rest[i + piece.len()..],
            None => return false,
        }
    }
    true
}

/// 來源打不開的原因：
/// - 網址（stream.c 的「Failed to open https://….」；HTTP 403、連結過期、斷線都是這一句，細節只在 FFmpeg 的記錄）
///   → 網址打不開，不是「檔案不見了」；
/// - 本機檔案（stream_file.c 的「Cannot open file '…': 系統的說明」）：不存在 → 不見了，其他（沒有權限、被鎖著）→ 讀不到
fn source_failure(text: &str) -> Failure {
    if let Some((_, rest)) = text.split_once("Failed to open ")
        && crate::net::is_network(rest.trim())
    {
        return Failure::SourceUnreachable;
    }
    let reason = text
        .split_once("Cannot open file '")
        .and_then(|(_, rest)| rest.rsplit_once("': "))
        .map(|(_, reason)| reason);
    match reason {
        Some(reason) if !reason.contains("No such file") => Failure::SourceNoAccess,
        _ => Failure::SourceMissing,
    }
}

// ───────────── 寫好之後檢查 ─────────────

/// 寫出來的檔案應該有的樣子
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Expect {
    /// 要有這種編碼的影像、聲音、字幕（None = 不檢查）
    pub video: Option<String>,
    pub audio: Option<String>,
    pub sub: Option<String>,
    /// 至少要這麼長（秒）
    pub min_len: f64,
}

/// 檢查的結果
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub duration: f64,
    pub tracks: Vec<Track>,
}

/// 重新打開等多久（網路磁碟、很慢的電腦）
pub const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// mpv 回報的編碼名稱，兩種分離器不一樣的換成同一個（mpv 自己的 MKV 分離器：`webvtt-webm`，FFmpeg 的：`webvtt`）
pub fn normalize_codec(codec: &str) -> &str {
    match codec {
        "webvtt-webm" => "webvtt",
        other => other,
    }
}

/// 重新打開寫好的檔案，確認播得了、夠長、有預期的軌道。用另外一個不出畫面、不出聲音的 mpv：
/// 不用 `Player`（它會去目的地的資料夾找外掛字幕、清字幕快取）
pub fn verify_media(path: &Path, expect: &Expect) -> Result<Verified, Failure> {
    verify_media_within(path, expect, VERIFY_TIMEOUT)
}

/// 同上，自己指定等多久（測試用）
pub fn verify_media_within(path: &Path, expect: &Expect, timeout: Duration) -> Result<Verified, Failure> {
    let mut opts = vec![
        ("vo", "null"),
        ("ao", "null"),
        ("idle", "yes"),
        ("pause", "yes"),
        ("hwdec", "no"),
        ("cover-art-auto", "no"),
    ];
    opts.extend_from_slice(INSTANCE_OPTIONS);
    let mpv = Mpv::new(&opts).map_err(|e| Failure::Engine(e.to_string()))?;
    let _ = mpv.request_log_messages("warn");
    let path_text = path.to_string_lossy();
    mpv.command(&["loadfile", &path_text])
        .map_err(|e| Failure::Engine(e.description()))?;
    let mut log = LogTail::default();
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::Unplayable(None));
        }
        let Some(ev) = mpv.wait_event(left.as_secs_f64().min(0.5)) else {
            continue;
        };
        log.push_event(&ev);
        match ev {
            Event::FileLoaded => break,
            Event::EndFile { .. } | Event::Shutdown => {
                let reason = log.lines().iter().find(|l| l.is_error()).map(|l| l.text.clone());
                return Err(Failure::Unplayable(reason));
            }
            _ => {}
        }
    }
    let duration = mpv.get_property::<f64>("duration").ok().filter(|d| d.is_finite());
    let tracks: Vec<Track> = mpv
        .get_string("track-list")
        .ok()
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    check_expect(duration, &tracks, expect)?;
    Ok(Verified {
        duration: duration.unwrap_or_default(),
        tracks,
    })
}

/// 長度與軌道是不是預期的
fn check_expect(duration: Option<f64>, tracks: &[Track], expect: &Expect) -> Result<(), Failure> {
    let Some(duration) = duration.filter(|d| *d > 0.0) else {
        return Err(Failure::NoData);
    };
    for (kind, codec) in [
        (TrackKind::Video, &expect.video),
        (TrackKind::Audio, &expect.audio),
        (TrackKind::Sub, &expect.sub),
    ] {
        let Some(codec) = codec else { continue };
        let found = tracks.iter().any(|t| {
            t.kind == kind
                && !t.albumart
                && t.codec
                    .as_deref()
                    .is_some_and(|c| normalize_codec(c) == normalize_codec(codec))
        });
        if !found {
            return Err(Failure::MissingTrack(kind));
        }
    }
    if duration < expect.min_len {
        return Err(Failure::TooShort {
            got: duration,
            want: expect.min_len,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(level: &str, text: &str) -> LogLine {
        LogLine::new(level, "x", text)
    }

    #[test]
    fn settings_defaults_and_sanitize() {
        let d = ExportSettings::default();
        assert_eq!((d.clip_dir.as_ref(), d.image_dir.as_ref()), (None, None));
        assert_eq!(d.clip.format, ClipFormat::Auto);
        assert_eq!((d.gif.long_side, d.gif.fps, d.gif.subtitles), (480, 15, true));
        let s = d.sheet;
        assert_eq!((s.columns, s.rows, s.width), (4, 5, 1920));
        assert!(s.timestamps && s.header);
        assert_eq!((s.format, s.jpeg_quality), (ImageFormat::Jpeg, 90));
        // 對齊選項、拉回範圍
        let mut x = ExportSettings::default();
        x.gif.long_side = 500;
        x.gif.fps = 60;
        x.sheet.columns = 0;
        x.sheet.rows = 99;
        x.sheet.width = 2000;
        x.sheet.jpeg_quality = 10;
        x.clip_dir = Some(PathBuf::from("relative/clips"));
        let abs = std::env::temp_dir().join("pics");
        x.image_dir = Some(abs.clone());
        let x = x.sanitized();
        assert_eq!((x.gif.long_side, x.gif.fps), (480, 25));
        assert_eq!((x.sheet.columns, x.sheet.rows, x.sheet.width), (1, 20, 1920));
        assert_eq!(x.sheet.jpeg_quality, 50);
        assert_eq!(x.clip_dir, None, "相對路徑當成沒設定");
        assert_eq!(x.image_dir, Some(abs));
        // 一樣近時取小的
        assert_eq!(snap(400, &GIF_LONG_SIDES), 320);
        assert_eq!(ExportSettings::default().sanitized(), ExportSettings::default());
        // 列舉的名稱
        assert_eq!(serde_json::to_string(&ClipFormat::Webm).unwrap(), "\"webm\"");
        assert_eq!(serde_json::to_string(&ImageFormat::Png).unwrap(), "\"png\"");
    }

    #[test]
    fn default_folders() {
        let s = ExportSettings::default();
        assert!(s.clip_folder().ends_with("VitaScope"), "{}", s.clip_folder().display());
        assert_eq!(s.clip_folder(), default_clip_dir());
        // 片段放「影片」資料夾，不是截圖的「圖片」資料夾
        if screenshot::home().is_some() {
            assert_ne!(default_clip_dir(), screenshot::default_dir());
        }
        #[cfg(windows)]
        {
            // 已知資料夾 API 一定問得到（OneDrive 搬過也一樣）
            let videos = screenshot::known_dir(KnownDir::Videos).expect("問不到「影片」資料夾");
            assert_ne!(Some(&videos), screenshot::known_dir(KnownDir::Pictures).as_ref());
            assert_eq!(default_clip_dir(), videos.join("VitaScope"));
        }
        #[cfg(target_os = "macos")]
        assert!(
            default_clip_dir().ends_with("Movies/VitaScope"),
            "{}",
            default_clip_dir().display()
        );
        // GIF、縮圖總覽圖：沒設定時跟截圖放一起
        let shots = std::env::temp_dir().join("shots");
        assert_eq!(s.image_folder(Some(&shots)), shots);
        assert_eq!(s.image_folder(None), screenshot::default_dir());
        let mine = std::env::temp_dir().join("mine");
        let s = ExportSettings {
            clip_dir: Some(mine.clone()),
            image_dir: Some(mine.clone()),
            ..Default::default()
        };
        assert_eq!(s.clip_folder(), mine);
        assert_eq!(s.image_folder(Some(&shots)), mine);
    }

    #[test]
    fn clip_gif_sheet_names() {
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(
            range_stem("C:/影片/第1集.mkv", None, 723.12, 765.0),
            "第1集 00.12.03-00.12.45"
        );
        assert_eq!(range_stem("/v/a.mp4", None, 3723.9, 3725.0), "a 01.02.03-01.02.05");
        // 網路串流：有標題用標題（不能用的字元換掉），不然用網址的最後一段
        assert_eq!(
            range_stem("https://x.com/live/stream.m3u8", Some("新聞: 直播"), 0.0, 1.5),
            "新聞_ 直播 00.00.00-00.00.01"
        );
        assert_eq!(
            range_stem("https://x.com/v/clip.mp4?x=1", None, 0.0, 1.0),
            "clip.mp4?x=1 00.00.00-00.00.01".replace('?', "_")
        );
        assert_eq!(sheet_stem("C:/影片/第1集.mkv", None), "第1集 縮圖");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(sheet_stem("C:/影片/第1集.mkv", None), "第1集 thumbnails");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        // 改過的檔名
        assert_eq!(chosen_stem("  我的:片段 ", "預設"), "我的_片段");
        assert_eq!(chosen_stem("   ", "預設"), "預設");
        assert_eq!(name_time(-3.0), "00.00.00");
    }

    #[test]
    fn parse_time_formats() {
        assert_eq!(parse_time("83.5"), Some(83.5));
        assert_eq!(parse_time("1:23.5"), Some(83.5));
        assert_eq!(parse_time("01:02:03.250"), Some(3723.25));
        assert_eq!(parse_time("1：02"), Some(62.0), "全形冒號");
        assert_eq!(parse_time(" 0:01.5 "), Some(1.5));
        assert_eq!(parse_time("90:00"), Some(5400.0), "最前面的分鐘不限");
        assert_eq!(parse_time("0"), Some(0.0));
        for bad in [
            "-1", "abc", "", "1:60", "1:75:00", "1:2:3:4", "1e3", "inf", "1:", ":5", "1:-5", "1.2.3",
        ] {
            assert_eq!(parse_time(bad), None, "{bad:?}");
        }
        // 顯示的格式讀得回來
        for t in [0.0, 1.5, 83.25, 3723.12, 36000.0] {
            assert!((parse_time(&format_time(t)).unwrap() - t).abs() < 0.001, "{t}");
        }
        assert_eq!(format_time(723.12), "00:12:03.120");
    }

    #[test]
    fn export_fixup_adds_file_and_user_rotation() {
        let g = Geometry {
            rotate: 90,
            hflip: true,
            ..Default::default()
        };
        assert_eq!(
            fixup(90, &g),
            Fixup {
                rotate: 180,
                hflip: true,
                vflip: false
            }
        );
        assert_eq!(fixup(270, &g).rotate, 0);
        assert_eq!(fixup(-90, &Geometry::default()).rotate, 270);
        assert!(fixup(0, &Geometry::default()).is_none());
        // 縮放、平移、裁切、比例不套用
        let g = Geometry {
            zoom: 1.0,
            pan: [0.2, 0.0],
            crop: Some(1),
            aspect: Some(1),
            ..Default::default()
        };
        assert!(fixup(0, &g).is_none());
    }

    #[test]
    fn map_mpv_error_known_lines() {
        let cases: [(&str, Failure); 23] = [
            ("Failed writing packet.", Failure::WriteFailed),
            ("Writing trailer failed.", Failure::WriteFailed),
            ("Failed opening output file.", Failure::NoPermission(None)),
            ("Output format not found.", Failure::FormatMissing),
            ("Can't mux one of the input streams.", Failure::CantMux),
            ("Writing header failed.", Failure::CantMux),
            ("Disabling filter lavfi because it has failed.", Failure::FilterFailed),
            // filters/f_lavfi.c：濾鏡圖接不起來（zscale 轉不了這種顏色之類）
            ("failed to configure the filter graph", Failure::FilterFailed),
            (
                "Cannot convert decoder/filter output to any format supported by the output.",
                Failure::FilterFailed,
            ),
            ("Failed to create file cache.", Failure::CacheFailed),
            // 讀快取時磁碟快取寫不進去（demux/cache.c）：快取所在的磁碟滿了，不是讀取逾時
            (
                "Failed to write to cache file: No space left on device",
                Failure::CacheFailed,
            ),
            ("Could not write all data.", Failure::CacheFailed),
            (
                "Cannot open file '/x/a.mkv': No such file or directory",
                Failure::SourceMissing,
            ),
            ("Failed to open /x/a.mkv.", Failure::SourceMissing),
            // 本機檔案還在、只是打不開：不是「被移動或刪除」
            (
                "Cannot open file 'C:\\x\\a.mkv': Permission denied",
                Failure::SourceNoAccess,
            ),
            // 網址：HTTP 403、連結過期、斷線在匯出用的 mpv 裡都只有這一句
            ("Failed to open https://example.com/a.mp4.", Failure::SourceUnreachable),
            (
                "Failed to open http://127.0.0.1:8080/v.m3u8.",
                Failure::SourceUnreachable,
            ),
            // 沒有這種編碼器（encode_lavc.c）
            ("codec 'gif' not found.", Failure::EncoderMissing),
            ("codec for video not found", Failure::EncoderMissing),
            ("Could not initialize encoder.", Failure::EncoderMissing),
            // 解碼器的訊息不算
            (
                "Cannot find codec 'xyz' in libavcodec...",
                Failure::Engine("Cannot find codec 'xyz' in libavcodec...".into()),
            ),
            ("Failed to recognize file format.", Failure::SourceUnreadable),
            ("No streams.", Failure::NoData),
        ];
        for (text, want) in cases {
            assert_eq!(map_mpv_error(&[line("error", text)]), Some(want), "{text}");
        }
        // 本機檔案不見了：mpv 先說「Cannot open file」再說「Failed to open」，看第一行
        let lines = [
            line("error", "Cannot open file '/x/a.mkv': Permission denied"),
            line("error", "Failed to open /x/a.mkv."),
        ];
        assert_eq!(map_mpv_error(&lines), Some(Failure::SourceNoAccess));
        // 磁碟滿了常常接著「Closing file failed」、標頭失敗：寫入失敗優先
        let lines = [
            line("error", "Writing header failed."),
            line("error", "Failed writing packet."),
        ];
        assert_eq!(map_mpv_error(&lines), Some(Failure::WriteFailed));
        // 警告不算（「This is an experimental feature」之類）；沒有錯誤時 None
        assert_eq!(
            map_mpv_error(&[line(
                "warn",
                "This is an experimental feature. Output files might be broken."
            )]),
            None
        );
        assert_eq!(map_mpv_error(&[line("warn", "Failed writing packet.")]), None);
        assert_eq!(map_mpv_error(&[]), None);
        // 不認得的錯誤：第一行錯誤的原文
        let lines = [
            line("warn", "something"),
            line("error", "Something strange happened.\n"),
            line("fatal", "Later."),
        ];
        assert_eq!(
            map_mpv_error(&lines),
            Some(Failure::Engine("Something strange happened.".into()))
        );
    }

    #[test]
    fn log_tail_keeps_the_last_warnings_and_errors() {
        let mut tail = LogTail::default();
        tail.push_event(&Event::Log {
            prefix: "cplayer".into(),
            level: "info".into(),
            text: "Playing: x\n".into(),
        });
        assert!(tail.lines().is_empty(), "資訊等級不收");
        for i in 0..20 {
            tail.push(line("error", &format!("line {i}")));
        }
        let lines = tail.lines();
        assert_eq!(lines.len(), LogTail::CAP);
        assert_eq!(lines[0].text, "line 4");
        assert_eq!(tail.failure(), Some(Failure::Engine("line 4".into())));
        tail.push(line("error", "Failed writing packet."));
        assert_eq!(tail.failure(), Some(Failure::WriteFailed));
    }

    #[test]
    fn space_on_the_same_disk_is_added_up() {
        let space = |vol: Option<&str>, free: Option<u64>, need: u64| Space {
            volume: vol.map(str::to_owned),
            free,
            need,
        };
        // 目的地與磁碟快取各要 60，同一個磁碟剩 100：不夠
        assert_eq!(
            shortfall(&[space(Some("C:"), Some(100), 60), space(Some("C:"), Some(100), 60)]),
            Some((120, 100))
        );
        // 不同的磁碟各自夠
        assert_eq!(
            shortfall(&[space(Some("C:"), Some(100), 60), space(Some("D:"), Some(100), 60)]),
            None
        );
        // 查不到剩多少：不算錯誤；不知道是哪個磁碟：自己算一組
        assert_eq!(shortfall(&[space(Some("C:"), None, u64::MAX)]), None);
        assert_eq!(
            shortfall(&[space(None, Some(100), 60), space(None, Some(100), 60)]),
            None
        );
        assert_eq!(shortfall(&[space(None, Some(10), 60)]), Some((60, 10)));
    }

    #[test]
    fn free_space_of_a_real_folder() {
        // 還沒建立的資料夾：看最接近的已經存在的上層
        let dir = std::env::temp_dir().join(format!("vitascope-free-{}/a/b", std::process::id()));
        let free = free_space(&dir);
        assert!(free.is_some_and(|f| f > 0), "{free:?}");
        assert!(volume_of(&dir).is_some());
        assert_eq!(volume_of(&dir), volume_of(&std::env::temp_dir()));
        assert!(check_space(&[(dir.as_path(), 1)]).is_ok());
        assert!(matches!(
            check_space(&[
                (dir.as_path(), u64::MAX / 2),
                (std::env::temp_dir().as_path(), u64::MAX / 2)
            ]),
            Err(Failure::DiskFull { .. })
        ));
    }

    #[test]
    fn expected_tracks_and_length() {
        let track = |kind: TrackKind, codec: &str| Track {
            id: 1,
            kind,
            title: None,
            lang: None,
            codec: Some(codec.into()),
            selected: true,
            external: false,
            external_filename: None,
            default: false,
            forced: false,
            albumart: false,
            width: None,
            height: None,
            channels: None,
            samplerate: None,
            dolby_vision_profile: None,
        };
        let tracks = [
            track(TrackKind::Video, "h264"),
            track(TrackKind::Audio, "aac"),
            track(TrackKind::Sub, "webvtt-webm"),
        ];
        let expect = Expect {
            video: Some("h264".into()),
            audio: Some("aac".into()),
            sub: Some("webvtt".into()),
            min_len: 2.0,
        };
        assert_eq!(check_expect(Some(2.5), &tracks, &expect), Ok(()));
        assert_eq!(check_expect(None, &tracks, &expect), Err(Failure::NoData));
        assert_eq!(check_expect(Some(0.0), &tracks, &expect), Err(Failure::NoData));
        assert_eq!(
            check_expect(Some(1.0), &tracks, &expect),
            Err(Failure::TooShort { got: 1.0, want: 2.0 })
        );
        let no_audio = [track(TrackKind::Video, "h264")];
        assert_eq!(
            check_expect(Some(3.0), &no_audio, &expect),
            Err(Failure::MissingTrack(TrackKind::Audio))
        );
        // 專輯封面不算影像
        let mut cover = track(TrackKind::Video, "mjpeg");
        cover.albumart = true;
        let audio_only = Expect {
            video: Some("mjpeg".into()),
            ..Default::default()
        };
        assert_eq!(
            check_expect(Some(3.0), &[cover], &audio_only),
            Err(Failure::MissingTrack(TrackKind::Video))
        );
    }

    #[test]
    fn failure_osd_is_one_short_line_without_paths_or_raw_text() {
        // 提示只有一行（約 40 個字以內）；路徑、系統或 mpv 的原文只在匯出視窗裡（details）
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        let temp = PathBuf::from(
            r"C:\Users\someone\Videos\VitaScope\很長的影片名稱 00.12.03-00.12.45.12345-7.vitascope-part.mkv",
        );
        let raw = "The process cannot access the file because it is being used by another process. (os error 32)";
        let cases = [
            Failure::Finish {
                error: raw.into(),
                temp: temp.clone(),
            },
            Failure::NoPermission(Some(temp.clone())),
            Failure::Unplayable(Some(raw.into())),
            Failure::Io(raw.into()),
            Failure::Engine(raw.into()),
        ];
        for f in &cases {
            let osd = f.osd();
            assert!(osd.starts_with("無法匯出："), "{osd}");
            assert!(osd.chars().count() <= 40, "提示太長：{osd}");
            assert!(!osd.contains("os error") && !osd.contains("VitaScope"), "{osd}");
            // 視窗裡看得到完整的原因
            let details = f.details();
            assert!(details.starts_with("無法匯出："), "{details}");
            assert!(
                details.contains(raw) || details.contains(&temp.display().to_string()),
                "{details}"
            );
        }
        // 短的原因：提示與視窗一樣
        assert_eq!(Failure::WriteFailed.osd(), Failure::WriteFailed.details());
        assert_eq!(Failure::Cancelled.details(), "已取消匯出");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let osd = cases[0].osd();
        assert_eq!(osd, "Export failed: Couldn't finish saving (see the Export window)");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }

    #[test]
    fn messages_are_in_the_current_language() {
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let done = Done {
            kind: Kind::Clip,
            path: PathBuf::from("/x/a 00.00.01-00.00.02.mkv"),
            bytes: 2_500_000,
            actual: Some((0.0, 2.0)),
            notes: vec![Note::KeyframeAligned],
        };
        assert_eq!(done.message(), "Clip saved: a 00.00.01-00.00.02.mkv (2.5 MB)");
        assert_eq!(Failure::Cancelled.osd(), "Export cancelled");
        assert_eq!(
            Failure::WriteFailed.osd(),
            "Export failed: Couldn't write the file (the disk may be full)"
        );
        assert_eq!(Phase::Grabbing { done: 7, total: 20 }.label(), "Grabbing frames 7/20");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(done.message(), "已儲存片段：a 00.00.01-00.00.02.mkv（2.5 MB）");
        assert_eq!(Failure::Cancelled.osd(), "已取消匯出");
        assert_eq!(
            Failure::DiskFull {
                need: 3_000_000_000,
                free: 1_000_000
            }
            .osd(),
            "無法匯出：磁碟空間不足（需要約 3.00 GB，剩 1.0 MB）"
        );
        assert_eq!(Failure::Engine("Odd.".into()).message(), "Odd.");
        // 換不成正式名稱時留下的完整檔案：名稱還是暫存檔的樣子，要提醒使用者改名。
        // 它登記過、啟動時清暫存檔不會刪（D2），說明裡不能再說「會被刪掉」
        let temp = PathBuf::from("/x/a.1-2.vitascope-part.mkv");
        let finish = Failure::Finish {
            error: "busy".into(),
            temp: temp.clone(),
        };
        let text = finish.message();
        assert!(
            text.contains(&temp.display().to_string()) && text.contains("改") && !text.contains("刪"),
            "{text}"
        );
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let text = finish.message();
        assert!(text.contains("rename it") && !text.contains("delete"), "{text}");
        assert!(!text.contains("  "), "英文的說明裡不能有連續的空白：{text}");
        // 美式拼法（跟其他英文介面一樣）
        assert!(Failure::SourceUnreadable.message().contains("recognized"));
        assert!(Note::DolbyVision5.message().contains("colors"));
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        // 「匯出」，不用「輸出」（「輸出」是音訊的輸出裝置）
        for f in [
            Failure::Live,
            Failure::Unplayable(None),
            Failure::Unplayable(Some("x".into())),
            Failure::MissingTrack(TrackKind::Audio),
            Failure::TooShort { got: 1.0, want: 2.0 },
        ] {
            assert!(!f.osd().contains("輸出"), "{}", f.osd());
        }
        assert!(!Note::NoSubtitleTrack.message().is_empty());
        // 片段不能存的原因（D2）
        assert_eq!(
            Failure::NotSeekable.osd(),
            "無法匯出：這個網路影片不能跳轉（伺服器不支援）"
        );
        assert_eq!(Failure::NoDump.message(), "播放引擎不支援匯出片段");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert!(Failure::NotSeekable.message().contains("isn't seekable"));
        assert_eq!(Failure::NoDump.message(), "The playback engine can't save clips");
        assert!(Note::NoAudioTrack.message().contains("no sound"));
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert!(Note::NoAudioTrack.message().contains("沒有聲音"));
        assert_eq!(Kind::Sheet.label(), "縮圖總覽圖");
    }
}
