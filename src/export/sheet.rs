//! 縮圖總覽圖：整部影片（或 A-B 段落）平均取幾張畫面，排成格線存成一張 JPEG / PNG，
//! 每張有時間標記，最上面可以有檔案資訊（檔名、大小、長度、格式、影像、聲音）。
//!
//! - **版面**（[`layout`]，純計算）：寬度、欄、列、畫面的比例（轉正之後）→ 每格的大小與位置。
//!   有上限（[`MAX_HEIGHT`]、[`MAX_PIXELS`]、[`MAX_CELL`]）：一欄二十列的 4K 直拍影片不會變成十幾萬像素高、幾 GB 的圖；
//!   欄少的時候列數會變少，匯出視窗上即時顯示實際的大小。
//! - **時間**（[`times`]）：範圍分成 n + 1 段，取中間的 n 個點（不取第一格（常常是黑的）、片尾）。
//! - **擷取**（[`grab`](super::grab)）：先跳到關鍵影格（快）；離目標超過半個間隔才精確跳轉。
//!   精確跳轉落在目標之後（TS、M2TS、MPEG-PS 沒有索引，FFmpeg 用時間戳搜尋常落在後面的關鍵影格、甚至檔尾），
//!   就讓分離器從更前面開始（5 秒、30 秒、檔案開頭，跟轉成 GIF 往前讀的一樣），之後的格子從同一級開始。
//!   時間標記寫的是**實際取到的那一格的時間**（不是目標）：圖跟時間一定對得上。
//! - **畫面**：縮成格子的大小、HDR 轉一般畫面（跟轉成 GIF 一樣的 zscale + tonemap，亮度照「畫質 → HDR」）、
//!   轉正（檔案本身的旋轉 + 使用者的旋轉、翻轉，跟 Ctrl+E 截圖一樣）。影像調整、像素著色器、縮放、裁切不套用。
//! - **合成**：RGB 畫布（[`text::Canvas`](super::text::Canvas)），文字用介面同一套字型；沒有中文字型時標頭改用英文。
//!   JPEG（品質照設定）或 PNG，寫到暫存檔、讀回檔頭核對大小，再換成正式的名稱（不覆蓋）。

use super::clip::{
    Inspected, Source, StreamPick, check_local, check_timeline, inspect_source, match_track, to_export_time,
    to_main_time,
};
use super::gif::{self, Tone};
use super::grab::{self, Frame, Grab, Grabber, Seek};
use super::text::{Canvas, TextPainter};
use super::{Ctl, Done, Failure, ImageFormat, Job, Kind, Note, Phase, Progress, SheetPrefs, check_space};
use crate::geometry::Geometry;
use crate::i18n::{self, Lang};
use crate::instance::Wake;
use crate::mediainfo::{self, MediaInfo, TrackInfo};
use crate::picture::{Deinterlace, ToneSettings};
use crate::player::{EngineCaps, Player, TrackKind};
use crate::screenshot::Fixup;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

// ───────────── 版面 ─────────────

/// 整張圖最高幾像素（JPEG 每邊最多 65535；太高的圖看圖軟體也開不了）
pub const MAX_HEIGHT: u32 = 16_384;
/// 整張圖最多幾像素（畫布、寫檔時的記憶體）
pub const MAX_PIXELS: u64 = 60_000_000;
/// 每格的長邊最多幾像素（每格是一次軟體繪圖，再大也看不出差別）
pub const MAX_CELL: u32 = 1280;
/// 背景、標頭的字、取不到的格子
pub const BACKGROUND: [u8; 3] = [0x14, 0x14, 0x14];
pub const HEADER_COLOR: [u8; 3] = [0xE6, 0xE6, 0xE6];
pub const MISSING_COLOR: [u8; 3] = [0x30, 0x30, 0x30];
/// 時間標記：底（半透明的黑）、字
pub const STAMP_BOX: [u8; 4] = [0, 0, 0, 0xB0];
pub const STAMP_COLOR: [u8; 3] = [0xF0, 0xF0, 0xF0];
/// 時間標記的方塊與字的間距、離格子邊緣的距離（像素）
pub const STAMP_PAD: f32 = 3.0;

/// 整張圖的版面（像素）
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// 整張圖的寬、高
    pub size: (u32, u32),
    /// 四周的邊、格子之間的間隔
    pub margin: u32,
    pub gap: u32,
    pub columns: u32,
    /// 實際的列數（超過上限時比要求的少）
    pub rows: u32,
    /// 每格的寬、高（轉正之後）
    pub cell: (u32, u32),
    /// 格線左上角
    pub grid: (u32, u32),
    /// 標頭的高度（沒有標頭時 0）、字的大小（第一行檔名大一點）
    pub header_h: u32,
    pub header_px: f32,
    pub name_px: f32,
    /// 時間標記的字的大小
    pub stamp_px: f32,
}

/// 往下取偶數、至少 2
fn even_floor(x: f64) -> u32 {
    ((x / 2.0).floor().max(1.0) as u32) * 2
}

/// 最近的偶數、至少 2
fn even_round(x: f64) -> u32 {
    ((x / 2.0).round().max(1.0) as u32) * 2
}

/// 一行字佔的高度
pub fn line_height(px: f32) -> u32 {
    (px * 1.45).round() as u32
}

impl Layout {
    /// 第 `i` 格（從 0 開始，一列一列排）的左上角
    pub fn cell_pos(&self, i: usize) -> (u32, u32) {
        let (col, row) = ((i as u32) % self.columns, (i as u32) / self.columns);
        (
            self.grid.0 + col * (self.cell.0 + self.gap),
            self.grid.1 + row * (self.cell.1 + self.gap),
        )
    }

    /// 格子數
    pub fn count(&self) -> usize {
        (self.columns * self.rows) as usize
    }

    /// 格線的寬度
    pub fn grid_width(&self) -> u32 {
        self.columns * self.cell.0 + (self.columns - 1) * self.gap
    }
}

/// 版面：寬度 `width`、`columns` 欄 × `rows` 列、畫面的比例 `aspect`（寬 ÷ 高，轉正之後）、標頭 `header_lines` 行。
/// - 邊 = 寬度的 0.8%（至少 8）、間隔 = 邊的一半（至少 4）；每格的寬是偶數，高照比例（偶數）。
/// - 每格的長邊最多 [`MAX_CELL`]：超過時格子縮小，整張圖的寬度跟著變窄（不留一大片空白）。
/// - 整張圖最高 [`MAX_HEIGHT`]、最多 [`MAX_PIXELS`] 像素：放不下時列數變少（至少一列）。
pub fn layout(width: u32, columns: u32, rows: u32, aspect: f64, header_lines: usize) -> Layout {
    let width = width.max(64);
    let columns = columns.max(1);
    let aspect = if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        16.0 / 9.0
    };
    let margin = ((f64::from(width) * 0.008).round() as u32).max(8);
    let gap = (margin / 2).max(4);
    let header_px = (width as f32 * 0.0125).clamp(14.0, 40.0);
    let name_px = (header_px * 1.25).round();
    let header_h = match header_lines {
        0 => 0,
        n => line_height(name_px) + (n as u32 - 1) * line_height(header_px) + margin,
    };
    let avail = width.saturating_sub(2 * margin + (columns - 1) * gap);
    let mut cw = even_floor(f64::from(avail) / f64::from(columns));
    let mut ch = even_round(f64::from(cw) / aspect);
    let capped = cw.max(ch) > MAX_CELL;
    if capped {
        if cw >= ch {
            cw = MAX_CELL;
            ch = even_round(f64::from(cw) / aspect).min(MAX_CELL);
        } else {
            ch = MAX_CELL;
            cw = even_round(f64::from(ch) * aspect).min(MAX_CELL);
        }
    }
    let grid_w = columns * cw + (columns - 1) * gap;
    // 格子縮小了：整張圖跟著變窄；沒縮小時寬度照選的，取偶數剩下的幾個像素分到兩邊
    let total_w = if capped {
        grid_w + 2 * margin
    } else {
        width.max(grid_w + 2 * margin)
    };
    let height = |r: u32| 2 * margin + header_h + r * ch + (r - 1) * gap;
    let mut rows = rows.clamp(1, *super::SHEET_ROWS.end());
    while rows > 1 && (height(rows) > MAX_HEIGHT || u64::from(total_w) * u64::from(height(rows)) > MAX_PIXELS) {
        rows -= 1;
    }
    Layout {
        size: (total_w, height(rows)),
        margin,
        gap,
        columns,
        rows,
        cell: (cw, ch),
        grid: ((total_w - grid_w) / 2, margin + header_h),
        header_h,
        header_px,
        name_px,
        stamp_px: (ch as f32 * 0.09).clamp(11.0, 28.0),
    }
}

/// 這個寬度、欄數、比例最多幾列（匯出視窗的列數選單）
pub fn max_rows(width: u32, columns: u32, aspect: f64, header_lines: usize) -> u32 {
    layout(width, columns, *super::SHEET_ROWS.end(), aspect, header_lines).rows
}

// ───────────── 時間 ─────────────

/// 範圍 `range`（秒）分成 n + 1 段，取中間的 n 個點：不取開頭（常常是黑的）、結尾（片尾）
pub fn times(range: (f64, f64), n: usize) -> Vec<f64> {
    let (s, e) = range;
    (0..n).map(|i| s + (e - s) * (i + 1) as f64 / (n + 1) as f64).collect()
}

/// 兩個點之間隔幾秒
pub fn spacing(range: (f64, f64), n: usize) -> f64 {
    (range.1 - range.0).max(0.0) / (n + 1) as f64
}

/// 跳到關鍵影格落在 `actual` 秒，離目標 `t` 夠近（半個間隔以內）就用它，不用精確跳轉
pub fn keyframe_close(actual: f64, t: f64, spacing: f64) -> bool {
    (actual - t).abs() <= spacing / 2.0
}

/// 精確跳轉落在 `actual`：是目標 `t` 那一格（mpv 取目標時間之後的第一格，最多晚一格；`frame` = 一格幾秒）。
/// 晚更多就是分離器落在目標之後的關鍵影格（跳轉不準的格式）。影片在目標之前就結束時是最後一格（比目標早，也算）
pub fn exact_landed(actual: f64, t: f64, frame: f64) -> bool {
    actual <= t + frame.max(0.0) * 1.5 + gif::HR_SEEK_TOLERANCE
}

/// 精確跳轉時分離器先往前跳多少
#[derive(Debug, Clone, Copy, PartialEq)]
enum Offset {
    Secs(f64),
    /// 從檔案開頭（往前跳到第一個時間戳之前）
    FromStart,
}

/// 精確跳轉落在目標之後時，依序試這些（跟轉成 GIF 往前讀的一樣），最後從檔案開頭
const OFFSETS: [Offset; 4] = [
    Offset::Secs(0.0),
    Offset::Secs(super::clip::PREROLL),
    Offset::Secs(super::clip::LONG_PREROLL),
    Offset::FromStart,
];

/// 時間標記：`MM:SS`、一小時以上 `H:MM:SS`（無條件捨去：標記不會比畫面晚）；`tenths` = 加上十分之一秒
/// （點很密的時候，不然相鄰兩格可能一樣）
pub fn stamp_text(t: f64, tenths: bool) -> String {
    let t = t.max(0.0);
    let s = t.floor() as u64;
    let (h, m, sec) = (s / 3600, s / 60 % 60, s % 60);
    let mut text = if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    };
    if tenths {
        text += &format!(".{}", ((t * 10.0).floor() as u64) % 10);
    }
    text
}

/// 間隔小於這麼多秒時，時間標記加上十分之一秒
pub const TENTHS_BELOW: f64 = 2.0;

// ───────────── 標頭 ─────────────

/// 編碼的簡短名稱 + profile：「HEVC Main 10」「H.264 High」「AAC LC」
fn codec_label(t: &TrackInfo) -> Option<String> {
    let codec = t.codec.as_deref()?;
    let name = match codec {
        "h264" => "H.264".to_owned(),
        "hevc" => "HEVC".to_owned(),
        "av1" => "AV1".to_owned(),
        "vp8" => "VP8".to_owned(),
        "vp9" => "VP9".to_owned(),
        "mpeg2video" => "MPEG-2".to_owned(),
        "mpeg1video" => "MPEG-1".to_owned(),
        "mpeg4" => "MPEG-4".to_owned(),
        "eac3" => "E-AC-3".to_owned(),
        "ac3" => "AC-3".to_owned(),
        "truehd" => "TrueHD".to_owned(),
        "opus" => "Opus".to_owned(),
        "vorbis" => "Vorbis".to_owned(),
        "theora" => "Theora".to_owned(),
        "prores" => "ProRes".to_owned(),
        "mp3" | "mp3float" => "MP3".to_owned(),
        "mp2" | "mp2float" => "MP2".to_owned(),
        other if other.starts_with("pcm_") => "PCM".to_owned(),
        other => other.to_uppercase(),
    };
    Some(
        match t.codec_profile.as_deref().filter(|p| !p.is_empty() && *p != "unknown") {
            Some(p) => format!("{name} {p}"),
            None => name,
        },
    )
}

/// 標頭的幾行（用目前的語言；沒有中文字型時呼叫的地方先換成英文）：
/// ```text
/// 第1集.mkv
/// 大小：1.42 GB　長度：23:41　格式：Matroska（MKV / WebM）
/// 影像：HEVC Main 10 · 1920×1080 · 23.976 fps · HDR10
/// 音訊：AAC LC · 48 kHz · 立體聲 · 日文
/// ```
/// 不知道的部分不寫；`name` 是第一行（檔名、網路影片的標題）
pub fn header_lines(info: &MediaInfo, name: &str) -> Vec<String> {
    use crate::{tf, tr};
    let mut lines = vec![name.to_owned()];
    let mut facts = Vec::new();
    if let Some(size) = info.file_size.filter(|s| *s > 0) {
        facts.push(tf!("大小：{}", "Size: {}", mediainfo::fmt_size(size)));
    }
    if let Some(d) = info.duration.filter(|d| d.is_finite() && *d > 0.0) {
        facts.push(tf!("長度：{}", "Length: {}", stamp_text(d, false)));
    }
    if let Some(f) = info.file_format.as_deref().filter(|f| !f.is_empty()) {
        facts.push(tf!("格式：{}", "Format: {}", mediainfo::container_name(f)));
    }
    if !facts.is_empty() {
        lines.push(facts.join(tr!("　", "   ")));
    }
    if let Some(v) = info.video.as_ref().filter(|v| !v.albumart && !v.image) {
        let mut parts: Vec<String> = codec_label(v).into_iter().collect();
        if let Some(vp) = &info.vparams
            && let (Some(w), Some(h)) = (vp.w, vp.h)
        {
            parts.push(format!("{w}×{h}"));
        }
        if let Some(fps) = info.container_fps {
            parts.push(format!("{} fps", mediainfo::fmt_fps(fps)));
        }
        if let Some(vp) = &info.vparams {
            let range = mediainfo::dynamic_range(vp, Some(v));
            if !range.starts_with("SDR") {
                parts.push(range);
            }
        }
        if !parts.is_empty() {
            lines.push(tf!("影像：{}", "Video: {}", parts.join(" · ")));
        }
    }
    if let Some(a) = &info.audio {
        let mut parts: Vec<String> = codec_label(a).into_iter().collect();
        if let Some(ap) = &info.aparams {
            if let Some(rate) = ap.samplerate.filter(|r| *r > 0) {
                parts.push(format!("{} kHz", mediainfo::fmt_khz(rate)));
            }
            if let Some(ch) = mediainfo::channels_label(ap.hr_channels.as_deref(), ap.channel_count) {
                parts.push(ch);
            }
        }
        if let Some(lang) = mediainfo::lang_label(a.lang.as_deref().filter(|l| !l.is_empty())) {
            parts.push(lang);
        }
        if !parts.is_empty() {
            lines.push(tf!("音訊：{}", "Audio: {}", parts.join(" · ")));
        }
    }
    lines
}

/// 用 `lang` 執行 `f`（這個執行緒的介面語言暫時換掉，做完換回來）
fn with_lang<T>(lang: Lang, f: impl FnOnce() -> T) -> T {
    let before = i18n::lang();
    i18n::set_lang(lang);
    let out = f();
    i18n::set_lang(before);
    out
}

/// 主播放器目前的檔案的標頭（介面執行緒；約 0.3 毫秒）。沒有中文字型時用英文（中文會變成方塊）；
/// `title` = 網路影片的標題（None 時用檔名）
pub fn header_for(player: &Player, title: Option<&str>) -> Vec<String> {
    let info = mediainfo::read(player, None);
    let name = title.map_or_else(|| info.file_name.clone(), str::to_owned);
    let lang = if crate::fonts::has_cjk() {
        i18n::lang()
    } else {
        Lang::En
    };
    with_lang(lang, || header_lines(&info, &name))
}

// ───────────── 要做的總覽圖 ─────────────

/// 測試用：擷取用的 mpv 開好時呼叫（讀它的選項）
#[doc(hidden)]
pub type OnReady = super::clip::OnReady;

/// 測試用：每取完一格呼叫（第幾格、目標、實際的時間（主播放器的；取不到時 None））
#[doc(hidden)]
pub type OnCell = Arc<dyn Fn(usize, f64, Option<f64>) + Send + Sync>;

/// 測試用：觀察、改變擷取
#[derive(Clone, Default)]
#[doc(hidden)]
pub struct TestHooks {
    /// 加在我們的濾鏡前面的 `vf`（測試「濾鏡失敗」）
    pub extra_vf: Option<String>,
    pub on_ready: Option<OnReady>,
    pub on_cell: Option<OnCell>,
    /// 每一格最多等多久（測試逾時）
    pub cell_timeout: Option<Duration>,
}

impl std::fmt::Debug for TestHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestHooks")
            .field("extra_vf", &self.extra_vf)
            .field("on_ready", &self.on_ready.is_some())
            .field("on_cell", &self.on_cell.is_some())
            .field("cell_timeout", &self.cell_timeout)
            .finish()
    }
}

/// 從設定與畫面來的選擇（介面執行緒準備）
#[derive(Debug, Clone, Copy)]
pub struct Choice<'a> {
    pub prefs: SheetPrefs,
    /// 使用者的旋轉、翻轉（縮放、平移、裁切不套用）
    pub geometry: &'a Geometry,
    /// 「畫質 → HDR」：曲線、目標亮度
    pub tone: &'a ToneSettings,
    pub deinterlace: Deinterlace,
    /// 只取 A-B 段落（主播放器的時間）；None = 整部
    pub range: Option<(f64, f64)>,
    /// 網路影片的標題（標頭的第一行；None 時用檔名）
    pub title: Option<&'a str>,
}

/// 一張總覽圖要的全部資料（介面執行緒從主播放器取得，背景執行緒只用這些）
#[derive(Debug, Clone)]
pub struct SheetSpec {
    pub source: Source,
    /// 範圍（主播放器的時間，秒）
    pub range: (f64, f64),
    /// 主播放器的 `demuxer-start-time`
    pub main_start: f64,
    /// 主播放器的總長度（擷取用的 mpv 開好後比一次：章節連結之類的時間線，長度會不一樣）
    pub main_duration: Option<f64>,
    /// 主播放器選的影像（None = 讓 mpv 選）
    pub video: Option<StreamPick>,
    pub layout: Layout,
    /// 擷取的大小（轉正之前）
    pub render: (u32, u32),
    /// 要轉正的部分（檔案本身的旋轉 + 使用者的旋轉、翻轉）
    pub fixup: Fixup,
    pub tone: Option<Tone>,
    pub timestamps: bool,
    /// 標頭的幾行（空的 = 沒有標頭）
    pub header: Vec<String>,
    pub format: ImageFormat,
    pub jpeg_quality: u8,
    /// 去交錯（主播放器的設定：auto / yes / no）
    pub deinterlace: &'static str,
    /// 來源的跳轉不準（TS、M2TS、MPEG-PS）：精確跳轉一開始就讓分離器從前面 5 秒開始
    pub approx_seek: bool,
    /// 影片的一格幾秒（判斷精確跳轉有沒有落在後面）；不知道時 0.1
    pub frame_secs: f64,
    /// 存放的資料夾、檔名（不含副檔名）
    pub dir: PathBuf,
    pub stem: String,
    /// 換不成正式名稱留下的檔案登記在這裡（`<快取>/export`）
    pub cache_dir: PathBuf,
    /// 完成時附上的說明（HDR 亮部裁切、杜比視界 Profile 5）
    pub notes: Vec<Note>,
    #[doc(hidden)]
    pub test: TestHooks,
}

/// 主播放器目前的檔案能不能做縮圖總覽圖；不能時回傳原因（右鍵選單停用時的說明）
pub fn unavailable(player: &Player) -> Option<Failure> {
    check(player).err()
}

fn check(player: &Player) -> Result<Inspected, Failure> {
    let st = &player.state;
    if !st.loaded {
        return Err(Failure::NoData);
    }
    if !st.has_video() {
        return Err(Failure::NoVideo);
    }
    let found = inspect_source(player).map_err(|f| match f {
        Failure::Live => Failure::Unbounded,
        other => other,
    })?;
    if st.duration.is_none_or(|d| !d.is_finite() || d <= 0.0) {
        return Err(Failure::Unbounded);
    }
    Ok(found)
}

/// 畫面上的比例（轉正之後）與要轉正的部分；還沒解出第一格時 None
fn shape(player: &Player, geometry: &Geometry) -> Option<(f64, Fixup)> {
    let (aspect, rotate) = gif::source_shape(player)?;
    let fix = super::fixup(rotate, geometry);
    let shown = if fix.rotate % 180 == 90 { 1.0 / aspect } else { aspect };
    Some((shown, fix))
}

/// 畫面上的比例（寬 ÷ 高，轉正之後；匯出視窗算版面用）：None = 還沒解出第一格
pub fn shown_aspect(player: &Player, geometry: &Geometry) -> Option<f64> {
    shape(player, geometry).map(|(aspect, _)| aspect)
}

/// 範圍：只取 A-B 段落時是 A-B（拉回影片裡），不然是整部
pub fn sheet_range(range: Option<(f64, f64)>, duration: f64) -> Result<(f64, f64), Failure> {
    match range {
        None => Ok((0.0, duration)),
        Some((a, b)) => {
            let (a, b) = (a.clamp(0.0, duration), b.clamp(0.0, duration));
            if b - a < gif::MIN_SECS {
                Err(Failure::RangeTooShort)
            } else {
                Ok((a, b))
            }
        }
    }
}

impl SheetSpec {
    /// 從主播放器準備一張總覽圖：設定與畫面的選擇、存放的資料夾。
    /// 不能做（沒有影像、直播、章節連結、還沒解出第一格…）時回傳原因
    pub fn from_player(
        player: &Player,
        caps: &EngineCaps,
        choice: &Choice<'_>,
        dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<SheetSpec, Failure> {
        let found = check(player)?;
        let st = &player.state;
        let duration = st.duration.unwrap_or_default();
        let range = sheet_range(choice.range, duration)?;
        let (aspect, fixup) = shape(player, choice.geometry).ok_or(Failure::NoData)?;
        let prefs = choice.prefs;
        let header = if prefs.header {
            header_for(player, choice.title)
        } else {
            Vec::new()
        };
        let layout = layout(prefs.width, prefs.columns, prefs.rows, aspect, header.len());
        let render = if fixup.rotate % 180 == 90 {
            (layout.cell.1, layout.cell.0)
        } else {
            layout.cell
        };
        let video_track = st.selected(TrackKind::Video);
        let dv = video_track.and_then(|t| t.dolby_vision_profile);
        let (tone, note) = gif::hdr_plan(gif::source_hdr(player), dv, caps.zscale && caps.tonemap, choice.tone);
        let fps = player
            .get_f64("container-fps")
            .or_else(|_| player.get_f64("estimated-vf-fps"))
            .ok()
            .filter(|f| f.is_finite() && *f > 0.0);
        Ok(SheetSpec {
            stem: super::sheet_stem(&found.path, choice.title.or(st.title.as_deref())),
            approx_seek: gif::approximate_seeks(&found.file_format),
            source: found.source,
            range,
            main_start: player.demuxer_start_time(),
            main_duration: Some(duration),
            video: video_track.and_then(|t| StreamPick::of(&st.tracks, t)),
            layout,
            render,
            fixup,
            tone,
            timestamps: prefs.timestamps,
            header,
            format: prefs.format,
            jpeg_quality: prefs.jpeg_quality,
            deinterlace: choice.deinterlace.effective(caps).mpv(),
            frame_secs: fps.map_or(0.1, |f| 1.0 / f),
            dir,
            cache_dir,
            notes: note.into_iter().collect(),
            test: TestHooks::default(),
        })
    }

    /// 存好的檔案預定的名稱
    pub fn wanted(&self) -> PathBuf {
        self.dir.join(format!("{}.{}", self.stem, self.format.ext()))
    }

    /// 擷取用的 mpv 的 `vf`：縮成格子的大小（轉正之前）、HDR 轉一般畫面
    pub fn vf(&self) -> String {
        let mut graph = format!("scale={}:{}:flags=lanczos", self.render.0, self.render.1);
        if let Some(t) = self.tone {
            graph = format!("{graph},{}", t.chain());
        }
        let vf = gif::lavfi(&graph);
        match &self.test.extra_vf {
            Some(extra) => format!("{extra},{vf}"),
            None => vf,
        }
    }

    /// 大概要多少空間：PNG 最多每個像素 3 位元組（JPEG 小很多）
    fn need_bytes(&self) -> u64 {
        u64::from(self.layout.size.0) * u64::from(self.layout.size.1) * 3 + SPARE
    }
}

// ───────────── 背景工作 ─────────────

/// 開始做這張總覽圖（背景執行緒）
pub fn spawn(spec: SheetSpec, wake: Wake) -> Job {
    Job::spawn(Kind::Sheet, wake, move |ctl| run(ctl, spec))
}

/// 一格最多等多久（網路磁碟、很慢的電腦）
pub const CELL_TIMEOUT: Duration = Duration::from_secs(10);
/// 連續幾格取不到就放棄（讀不到檔案）
const MAX_MISSES: usize = 2;
/// 目的地另外要留的空間
const SPARE: u64 = 1024 * 1024;

/// 取到的一格：時間（主播放器的）、圖（轉正之後）
type Cell = Option<(f64, crate::screenshot::Image)>;

/// 總覽圖的工作本體（在 `Job` 的背景執行緒裡）
pub fn run(ctl: &Ctl, spec: SheetSpec) -> Result<Done, Failure> {
    // 換不成正式名稱時留下的完整檔案登記在快取資料夾：啟動時清暫存檔不會刪掉它
    ctl.set_keep_dir(Some(spec.cache_dir.clone()));
    ctl.check()?;
    if !(spec.range.0.is_finite() && spec.range.1.is_finite() && spec.range.1 > spec.range.0) {
        return Err(Failure::NoData);
    }
    if let Source::File(path) = &spec.source {
        check_local(path)?;
    }
    let n = spec.layout.count();
    ctl.progress(Progress {
        phase: Phase::Grabbing {
            done: 0,
            total: n as u32,
        },
        fraction: Some(0.0),
    });
    std::fs::create_dir_all(&spec.dir).map_err(|_| Failure::NoPermission(Some(spec.dir.clone())))?;
    check_space(&[(spec.dir.as_path(), spec.need_bytes())])?;

    // 先畫好背景與標頭，每取到一格就貼上去（不留著每一格的圖：上限的大小時那些圖比畫布還大）
    let mut text = TextPainter::new();
    let mut canvas = start_canvas(&spec, &mut text);
    grab_all(ctl, &spec, &mut canvas, &mut text)?;
    ctl.check()?;
    ctl.progress(Progress {
        phase: Phase::Composing,
        fraction: None,
    });
    ctl.check()?;
    let temp = ctl.temp_in(&spec.dir, &spec.stem, spec.format.ext());
    write_image(&canvas, spec.format, spec.jpeg_quality, &temp).map_err(|e| write_failure(e, &spec.dir))?;
    drop(canvas);
    ctl.check()?;
    ctl.progress(Progress {
        phase: Phase::Checking,
        fraction: None,
    });
    let size = image_size(&temp, spec.format).map_err(|_| Failure::Unplayable(None))?;
    if size != spec.layout.size {
        return Err(Failure::Unplayable(None));
    }
    let path = ctl.finish(&spec.wanted())?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(Done {
        kind: Kind::Sheet,
        path,
        bytes,
        actual: None,
        notes: spec.notes.clone(),
    })
}

/// 開擷取用的 mpv、一格一格取，取到（或取不到）就畫到 `canvas` 上。連續 [`MAX_MISSES`] 格取不到就放棄
fn grab_all(ctl: &Ctl, spec: &SheetSpec, canvas: &mut Canvas, text: &mut TextPainter) -> Result<(), Failure> {
    let setup = grab::Setup {
        source: spec.source.clone(),
        size: spec.render,
        vf: spec.vf(),
        vid: spec.video.as_ref().map(|v| v.ordinal as i64),
        deinterlace: spec.deinterlace,
    };
    let mut g = Grabber::open(ctl, &setup)?;
    // 兩邊的時間線要一樣（靜態的檢查沒抓到的章節連結之類）：不然目標、時間標記都對到別的地方。
    // 網路影片在這裡沒有總長度是直播（做不了總覽圖）
    check_timeline(spec.main_duration, g.duration(), spec.source.is_network()).map_err(|f| match f {
        Failure::Live => Failure::Unbounded,
        other => other,
    })?;
    // 主播放器選的影像：預期的編號對不上時換成對的（還沒開始取，換了也沒關係）
    if let Some(pick) = &spec.video {
        match match_track(&g.tracks(), pick) {
            Some(id) if Some(id) != setup.vid => g.select_video(id)?,
            Some(_) => {}
            None => return Err(Failure::TrackNotFound),
        }
    }
    if let Some(hook) = &spec.test.on_ready {
        hook(g.mpv());
    }
    let n = spec.layout.count();
    let targets = times(spec.range, n);
    let gap = spacing(spec.range, n);
    let timeout = spec.test.cell_timeout.unwrap_or(CELL_TIMEOUT);
    // 精確跳轉從哪一級開始（之前的格子用到的那一級；跳轉不準的格式一開始就往前 5 秒）
    let mut level = usize::from(spec.approx_seek);
    let mut misses = 0;
    for (i, &t) in targets.iter().enumerate() {
        ctl.check()?;
        let frame = grab_cell(ctl, &mut g, spec, t, gap, timeout, &mut level)?;
        let cell = frame.map(|f| {
            let at = to_main_time(f.time, spec.main_start, g.start_time());
            (at, f.image.fixed(spec.fixup))
        });
        if let Some(hook) = &spec.test.on_cell {
            hook(i, t, cell.as_ref().map(|c| c.0));
        }
        misses = if cell.is_some() { 0 } else { misses + 1 };
        if misses >= MAX_MISSES {
            return Err(Failure::ReadTimeout);
        }
        draw_cell(canvas, text, spec, i, cell.as_ref());
        ctl.progress(Progress {
            phase: Phase::Grabbing {
                done: (i + 1) as u32,
                total: n as u32,
            },
            fraction: Some((i + 1) as f32 / n as f32 * 0.9),
        });
    }
    g.finish()?;
    Ok(())
}

/// 取目標 `t`（主播放器的時間）那一格：先跳到關鍵影格，夠近就用；不然精確跳轉，落在後面時讓分離器從更前面開始
/// （`level` 記住用到哪一級，下一格從那裡開始）。精確跳轉都落在後面時用最接近的那一格；
/// 一格都沒取到（逾時、都落在檔尾沒有新的影格）時 None
#[allow(clippy::too_many_arguments)]
fn grab_cell(
    ctl: &Ctl,
    g: &mut Grabber,
    spec: &SheetSpec,
    t: f64,
    gap: f64,
    timeout: Duration,
    level: &mut usize,
) -> Result<Option<Frame>, Failure> {
    let target = to_export_time(t, spec.main_start, g.start_time());
    let mut best: Option<Frame> = None;
    let keep = |f: Frame, best: &mut Option<Frame>| {
        if best
            .as_ref()
            .is_none_or(|b| (f.time - target).abs() < (b.time - target).abs())
        {
            *best = Some(f);
        }
    };
    // 落在檔尾（沒有新的影格）、逾時：都改用精確跳轉
    if let Grab::Frame(f) = g.grab(ctl, target, Seek::Keyframe, timeout)? {
        if keyframe_close(f.time, target, gap) {
            return Ok(Some(f));
        }
        keep(f, &mut best);
    }
    for (k, offset) in OFFSETS.iter().enumerate().skip(*level) {
        let demuxer_offset = match offset {
            Offset::Secs(s) => *s,
            Offset::FromStart => target + g.start_time() + 1.0,
        };
        match g.grab(ctl, target, Seek::Exact { demuxer_offset }, timeout)? {
            Grab::Frame(f) if exact_landed(f.time, target, spec.frame_secs) => {
                *level = k;
                return Ok(Some(f));
            }
            Grab::Frame(f) => keep(f, &mut best),
            // 分離器落在目標之後的檔尾：沒有畫面，從更前面開始
            Grab::Empty => {}
            // 逾時：再往前只會更慢
            Grab::TimedOut => break,
        }
    }
    Ok(best)
}

/// 合成整張圖：背景、標頭、每一格與時間標記；取不到的格子是灰色加「—」
pub fn compose(spec: &SheetSpec, cells: &[Cell]) -> Canvas {
    let mut text = TextPainter::new();
    let mut canvas = start_canvas(spec, &mut text);
    for (i, cell) in cells.iter().enumerate().take(spec.layout.count()) {
        draw_cell(&mut canvas, &mut text, spec, i, cell.as_ref());
    }
    canvas
}

/// 整張圖的背景與標頭（格子之後一格一格貼上）
fn start_canvas(spec: &SheetSpec, text: &mut TextPainter) -> Canvas {
    let l = &spec.layout;
    let mut canvas = Canvas::new(l.size.0 as usize, l.size.1 as usize, BACKGROUND);
    let max_w = l.grid_width() as f32;
    let mut y = l.margin as f32;
    for (i, line) in spec.header.iter().enumerate() {
        let px = if i == 0 { l.name_px } else { l.header_px };
        let line_h = line_height(px) as f32;
        let fitted = text.fit(line, px, max_w);
        let (_, h) = text.measure(&fitted, px);
        text.draw(
            &mut canvas,
            l.grid.0 as f32,
            y + (line_h - h) / 2.0,
            &fitted,
            px,
            HEADER_COLOR,
        );
        y += line_h;
    }
    canvas
}

/// 第 `i` 格：圖與時間標記；取不到（None）是灰色加「—」
fn draw_cell(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    spec: &SheetSpec,
    i: usize,
    cell: Option<&(f64, crate::screenshot::Image)>,
) {
    let l = &spec.layout;
    if i >= l.count() {
        return;
    }
    let tenths = spacing(spec.range, l.count()) < TENTHS_BELOW;
    let (cw, ch) = (l.cell.0 as usize, l.cell.1 as usize);
    let (x, y) = l.cell_pos(i);
    let (x, y) = (x as usize, y as usize);
    match cell {
        Some((at, img)) => {
            canvas.blit(x, y, img);
            if spec.timestamps {
                draw_stamp(canvas, text, (x, y), (cw, ch), &stamp_text(*at, tenths), l.stamp_px);
            }
        }
        None => {
            canvas.fill_rect(x, y, cw, ch, MISSING_COLOR);
            let px = (ch as f32 * 0.3).clamp(12.0, 60.0);
            let (w, h) = text.measure("—", px);
            text.draw(
                canvas,
                x as f32 + (cw as f32 - w) / 2.0,
                y as f32 + (ch as f32 - h) / 2.0,
                "—",
                px,
                HEADER_COLOR,
            );
        }
    }
}

/// 時間標記：格子右下角、半透明的圓角黑底
fn draw_stamp(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    pos: (usize, usize),
    cell: (usize, usize),
    s: &str,
    px: f32,
) {
    let (w, h) = text.measure(s, px);
    let (bw, bh) = ((w + 2.0 * STAMP_PAD).ceil(), (h + 2.0 * STAMP_PAD).ceil());
    let bx = (pos.0 + cell.0) as f32 - bw - STAMP_PAD;
    let by = (pos.1 + cell.1) as f32 - bh - STAMP_PAD;
    canvas.blend_rounded(bx as i64, by as i64, bw as i64, bh as i64, STAMP_BOX, STAMP_PAD);
    text.draw(canvas, bx + STAMP_PAD, by + STAMP_PAD, s, px, STAMP_COLOR);
}

/// 存成 JPEG（品質 `quality`）或 PNG（RGB）
pub fn write_image(canvas: &Canvas, format: ImageFormat, quality: u8, path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let file = std::fs::File::create(path)?;
    let mut w = std::io::BufWriter::new(file);
    let (width, height) = (canvas.w as u32, canvas.h as u32);
    match format {
        ImageFormat::Jpeg => {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, quality.clamp(1, 100))
                .encode(&canvas.rgb, width, height, image::ExtendedColorType::Rgb8)
                .map_err(|e| match e {
                    image::ImageError::IoError(e) => e,
                    other => std::io::Error::other(other),
                })?;
        }
        ImageFormat::Png => {
            let mut encoder = png::Encoder::new(&mut w, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().map_err(png_error)?;
            writer.write_image_data(&canvas.rgb).map_err(png_error)?;
            writer.finish().map_err(png_error)?;
        }
    }
    w.flush()?;
    w.into_inner().map_err(|e| e.into_error())?.sync_all()
}

fn png_error(e: png::EncodingError) -> std::io::Error {
    match e {
        png::EncodingError::IoError(e) => e,
        other => std::io::Error::other(other),
    }
}

/// 寫檔失敗的原因：磁碟滿了、沒有權限、其他（系統的原文）
fn write_failure(e: std::io::Error, dir: &Path) -> Failure {
    match e.kind() {
        std::io::ErrorKind::StorageFull => Failure::WriteFailed,
        std::io::ErrorKind::PermissionDenied => Failure::NoPermission(Some(dir.to_path_buf())),
        _ => Failure::Io(e.to_string()),
    }
}

/// 讀回寫好的圖的大小（只讀檔頭）：確認是完整、看得懂的圖
pub fn image_size(path: &Path, format: ImageFormat) -> std::io::Result<(u32, u32)> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    match format {
        ImageFormat::Jpeg => {
            use image::ImageDecoder;
            let d = image::codecs::jpeg::JpegDecoder::new(file).map_err(std::io::Error::other)?;
            Ok(d.dimensions())
        }
        ImageFormat::Png => {
            let reader = png::Decoder::new(file).read_info().map_err(std::io::Error::other)?;
            let info = reader.info();
            Ok((info.width, info.height))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_ok(l: &Layout) {
        assert!(l.size.1 <= MAX_HEIGHT, "{l:?}");
        assert!(u64::from(l.size.0) * u64::from(l.size.1) <= MAX_PIXELS, "{l:?}");
        assert!(l.cell.0.max(l.cell.1) <= MAX_CELL, "{l:?}");
        assert!(l.cell.0.is_multiple_of(2) && l.cell.1.is_multiple_of(2), "{l:?}");
        // 格子都在圖裡、不重疊
        let (x, y) = l.cell_pos(l.count() - 1);
        assert!(
            x + l.cell.0 + l.margin <= l.size.0 && y + l.cell.1 + l.margin <= l.size.1,
            "{l:?}"
        );
        assert!(l.grid.0 >= l.margin && l.grid.1 >= l.margin + l.header_h, "{l:?}");
    }

    #[test]
    fn layout_grid() {
        // 4×5、1920、16:9：邊 15、間隔 7，每格 466×262（偶數）
        let l = layout(1920, 4, 5, 16.0 / 9.0, 0);
        assert_eq!((l.margin, l.gap), (15, 7));
        assert_eq!(l.cell, (466, 262));
        assert_eq!(l.size.0, 1920, "寬度照選的");
        assert_eq!(l.rows, 5);
        assert_eq!(l.header_h, 0);
        assert_eq!(l.size.1, 2 * 15 + 5 * 262 + 4 * 7);
        assert_eq!(l.cell_pos(0), l.grid);
        assert_eq!(l.cell_pos(5), (l.grid.0 + 466 + 7, l.grid.1 + 262 + 7));
        caps_ok(&l);
        // 標頭四行：第一行（檔名）大一點，下面三行一樣高，再加一個邊
        let h = layout(1920, 4, 5, 16.0 / 9.0, 4);
        assert_eq!(h.header_px, 24.0);
        assert_eq!(h.name_px, 30.0);
        assert_eq!(h.header_h, line_height(30.0) + 3 * line_height(24.0) + 15);
        assert_eq!(h.size.1, l.size.1 + h.header_h);
        assert_eq!(h.grid.1, 15 + h.header_h);
        // 時間標記的字跟著格子的高度（有上下限）
        assert!((l.stamp_px - 262.0 * 0.09).abs() < 0.01);
        assert_eq!(layout(1280, 10, 1, 16.0 / 9.0, 0).stamp_px, 11.0);
        assert_eq!(layout(3840, 1, 1, 16.0 / 9.0, 0).stamp_px, 28.0);
    }

    #[test]
    fn layout_portrait() {
        // 直的畫面（9:16）：格子比高，比例照畫面
        let l = layout(1920, 4, 5, 9.0 / 16.0, 0);
        assert_eq!(l.cell.0, 466);
        assert_eq!(l.cell.1, even_round(466.0 * 16.0 / 9.0));
        assert!(l.cell.1 > l.cell.0);
        caps_ok(&l);
    }

    #[test]
    fn layout_caps() {
        // 一欄二十列、3840 寬的橫的影片：每格最長 1280（整張圖跟著變窄），二十列都放得下
        let l = layout(3840, 1, 20, 16.0 / 9.0, 4);
        assert_eq!(l.cell, (1280, 720));
        assert_eq!(l.size.0, 1280 + 2 * l.margin, "格子縮小時整張圖跟著變窄");
        assert_eq!(l.rows, 20);
        caps_ok(&l);
        // 同樣的直拍影片：不限制的話高 13 萬像素；格子最高 1280，列數變少讓整張圖不超過 16384
        let p = layout(3840, 1, 20, 9.0 / 16.0, 4);
        assert_eq!(p.cell, (720, 1280));
        assert!(p.rows < 20 && p.rows >= 10, "{p:?}");
        assert!(p.size.1 + p.cell.1 + p.gap > MAX_HEIGHT, "再多一列就超過：{p:?}");
        caps_ok(&p);
        assert_eq!(max_rows(3840, 1, 9.0 / 16.0, 4), p.rows);
        // 總像素的上限：3840 寬、四欄、接近正方形的格子：再多一列還不到 16384 高，但超過 6 千萬像素
        let px = layout(3840, 4, 20, 0.9, 4);
        caps_ok(&px);
        let next = px.size.1 + px.cell.1 + px.gap;
        assert!(
            next <= MAX_HEIGHT && u64::from(px.size.0) * u64::from(next) > MAX_PIXELS,
            "{px:?}"
        );
        // 各種寬度、欄、列、比例都不超過上限
        for width in crate::export::SHEET_WIDTHS {
            for cols in crate::export::SHEET_COLUMNS {
                for aspect in [21.0 / 9.0, 16.0 / 9.0, 4.0 / 3.0, 1.0, 9.0 / 16.0, 0.3, 8.0] {
                    for header in [0, 4] {
                        let l = layout(width, cols, 20, aspect, header);
                        caps_ok(&l);
                        assert!(l.size.0 <= width, "{width} {cols} {aspect}：{l:?}");
                        assert!(l.rows >= 1);
                    }
                }
            }
        }
        // 一列一定放得下
        assert_eq!(layout(1280, 1, 0, 9.0 / 16.0, 0).rows, 1);
        // 看不懂的比例當成 16:9
        assert_eq!(layout(1920, 4, 5, f64::NAN, 0), layout(1920, 4, 5, 16.0 / 9.0, 0));
    }

    #[test]
    fn times_whole_and_ab() {
        // 整部 100 秒取 4 張：20、40、60、80（不取開頭、結尾）
        assert_eq!(times((0.0, 100.0), 4), [20.0, 40.0, 60.0, 80.0]);
        assert_eq!(spacing((0.0, 100.0), 4), 20.0);
        // 只取 A-B（10–20 秒）取 1 張：中間
        assert_eq!(times((10.0, 20.0), 1), [15.0]);
        assert!(times((0.0, 1.0), 0).is_empty());
        // 範圍：A-B 拉回影片裡；太短不行
        assert_eq!(sheet_range(None, 50.0), Ok((0.0, 50.0)));
        assert_eq!(sheet_range(Some((5.0, 80.0)), 50.0), Ok((5.0, 50.0)));
        assert_eq!(sheet_range(Some((5.0, 5.1)), 50.0), Err(Failure::RangeTooShort));
        assert_eq!(sheet_range(Some((60.0, 70.0)), 50.0), Err(Failure::RangeTooShort));
    }

    #[test]
    fn exact_fallback_rule() {
        // 關鍵影格在目標的半個間隔以內就用（間隔 20 秒：差 10 秒以內）
        assert!(keyframe_close(48.0, 40.0, 20.0));
        assert!(keyframe_close(30.0, 40.0, 20.0));
        assert!(!keyframe_close(51.0, 40.0, 20.0));
        assert!(!keyframe_close(0.0, 40.0, 20.0), "長 GOP：關鍵影格在很前面");
        // 精確跳轉：目標那一格（目標之後的第一格，最多晚一格多）算到了；
        // 落在後面的關鍵影格（TS：晚 0.5 秒、10 秒）沒到；比目標早（影片已經結束，最後一格）也算
        let frame = 1.0 / 24.0;
        assert!(exact_landed(10.0, 10.0, frame));
        assert!(exact_landed(10.04, 10.0, frame));
        assert!(exact_landed(9.995, 10.0, frame));
        assert!(exact_landed(8.0, 10.0, frame));
        assert!(!exact_landed(10.5, 10.0, frame));
        assert!(!exact_landed(20.0, 10.0, frame));
        // 一級一級往前：0、5、30 秒，最後從檔案開頭
        assert_eq!(OFFSETS[0], Offset::Secs(0.0));
        assert_eq!(OFFSETS[3], Offset::FromStart);
    }

    #[test]
    fn stamp_texts() {
        assert_eq!(stamp_text(0.0, false), "00:00");
        assert_eq!(stamp_text(59.99, false), "00:59", "無條件捨去：標記不會比畫面晚");
        assert_eq!(stamp_text(83.5, false), "01:23");
        assert_eq!(stamp_text(3723.0, false), "1:02:03");
        assert_eq!(stamp_text(1.26, true), "00:01.2");
        assert_eq!(stamp_text(3600.95, true), "1:00:00.9");
        assert_eq!(stamp_text(-1.0, false), "00:00");
    }

    fn info() -> MediaInfo {
        let v: TrackInfo = serde_json::from_value(serde_json::json!({
            "codec": "hevc", "codec-profile": "Main 10"
        }))
        .unwrap();
        let a: TrackInfo = serde_json::from_value(serde_json::json!({
            "codec": "aac", "codec-profile": "LC", "lang": "jpn"
        }))
        .unwrap();
        MediaInfo {
            file_name: "第1集.mkv".into(),
            file_format: Some("mkv".into()),
            file_size: Some(1_420_000_000),
            duration: Some(1421.0),
            video: Some(v),
            audio: Some(a),
            vparams: serde_json::from_value(serde_json::json!({
                "w": 1920, "h": 1080, "gamma": "pq", "primaries": "bt.2020"
            }))
            .ok(),
            aparams: serde_json::from_value(serde_json::json!({
                "samplerate": 48000, "channel-count": 2, "hr-channels": "stereo"
            }))
            .ok(),
            container_fps: Some(23.976),
            ..Default::default()
        }
    }

    #[test]
    fn header_lines_from_info() {
        i18n::set_lang(Lang::ZhTw);
        assert_eq!(
            header_lines(&info(), "第1集.mkv"),
            [
                "第1集.mkv",
                "大小：1.42 GB　長度：23:41　格式：Matroska（MKV / WebM）",
                "影像：HEVC Main 10 · 1920×1080 · 23.976 fps · HDR10",
                "音訊：AAC LC · 48 kHz · 立體聲 · 日文",
            ]
        );
        // 英文；跟介面語言無關地換成英文（沒有中文字型時）
        let en = with_lang(Lang::En, || header_lines(&info(), "Episode 1.mkv"));
        assert_eq!(
            en,
            [
                "Episode 1.mkv",
                "Size: 1.42 GB   Length: 23:41   Format: Matroska (MKV / WebM)",
                "Video: HEVC Main 10 · 1920×1080 · 23.976 fps · HDR10",
                "Audio: AAC LC · 48 kHz · Stereo · Japanese",
            ]
        );
        assert_eq!(i18n::lang(), Lang::ZhTw, "換回原本的語言");
        // 不知道的部分不寫：一般畫面不寫 SDR、沒有聲音沒有那一行、什麼都不知道只有檔名
        let mut sdr = info();
        sdr.vparams = None;
        sdr.audio = None;
        sdr.container_fps = None;
        sdr.file_size = None;
        let lines = header_lines(&sdr, "a.mkv");
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[2], "影像：HEVC Main 10");
        assert!(!lines[1].contains("大小"));
        let empty = MediaInfo::default();
        assert_eq!(header_lines(&empty, "x"), ["x"]);
    }

    #[test]
    fn jpeg_round_trip_dims() {
        let dir = std::env::temp_dir().join(format!("vitascope-sheet-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = Canvas::new(321, 47, BACKGROUND);
        c.fill_rect(10, 10, 100, 20, [250, 30, 30]);
        for (format, name) in [(ImageFormat::Jpeg, "a.jpg"), (ImageFormat::Png, "a.png")] {
            let path = dir.join(name);
            write_image(&c, format, 90, &path).unwrap();
            assert_eq!(image_size(&path, format).unwrap(), (321, 47));
        }
        // JPEG 解得回來，顏色大致一樣（有損）
        let img =
            image::load_from_memory_with_format(&std::fs::read(dir.join("a.jpg")).unwrap(), image::ImageFormat::Jpeg)
                .unwrap()
                .to_rgb8();
        let p = img.get_pixel(50, 20).0;
        assert!(p[0] > 200 && p[1] < 80 && p[2] < 80, "{p:?}");
        let bg = img.get_pixel(300, 40).0;
        assert!(bg.iter().all(|&v| (10..=30).contains(&v)), "{bg:?}");
        // 壞掉的檔案讀不出大小
        std::fs::write(dir.join("bad.jpg"), b"not a jpeg").unwrap();
        assert!(image_size(&dir.join("bad.jpg"), ImageFormat::Jpeg).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_draws_header_cells_stamps_and_missing() {
        let l = layout(640, 2, 1, 4.0 / 3.0, 2);
        let spec = SheetSpec {
            source: Source::File(PathBuf::from("x.mkv")),
            range: (0.0, 30.0),
            main_start: 0.0,
            main_duration: Some(30.0),
            video: None,
            render: l.cell,
            layout: l.clone(),
            fixup: Fixup::default(),
            tone: None,
            timestamps: true,
            header: vec!["x.mkv".into(), "Size: 1 MB".into()],
            format: ImageFormat::Png,
            jpeg_quality: 90,
            deinterlace: "no",
            approx_seek: false,
            frame_secs: 0.04,
            dir: PathBuf::new(),
            stem: String::new(),
            cache_dir: PathBuf::new(),
            notes: Vec::new(),
            test: TestHooks::default(),
        };
        let (cw, ch) = (l.cell.0 as usize, l.cell.1 as usize);
        let img = crate::screenshot::Image {
            w: cw,
            h: ch,
            rgba: [[40u8, 90, 160, 255]].repeat(cw * ch).concat(),
        };
        let c = compose(&spec, &[Some((12.0, img)), None]);
        assert_eq!((c.w, c.h), (l.size.0 as usize, l.size.1 as usize));
        // 標頭有字（亮的像素），背景是 #141414
        let header_lit = (l.margin as usize..l.grid.1 as usize)
            .flat_map(|y| (0..c.w).map(move |x| (x, y)))
            .filter(|&(x, y)| c.pixel(x, y).unwrap()[0] > 150)
            .count();
        assert!(header_lit > 50, "{header_lit}");
        assert_eq!(c.pixel(1, c.h - 1), Some(BACKGROUND));
        // 第一格：畫面的顏色，右下角的時間標記有亮的字
        let (x0, y0) = l.cell_pos(0);
        assert_eq!(c.pixel(x0 as usize + 5, y0 as usize + 5), Some([40, 90, 160]));
        let stamp_lit = (y0 as usize + ch - 30..y0 as usize + ch)
            .flat_map(|y| (x0 as usize + cw - 70..x0 as usize + cw).map(move |x| (x, y)))
            .filter(|&(x, y)| c.pixel(x, y).unwrap()[0] > 200)
            .count();
        assert!(stamp_lit > 10, "{stamp_lit}");
        // 第二格取不到：灰色，中間有「—」
        let (x1, y1) = l.cell_pos(1);
        assert_eq!(c.pixel(x1 as usize + 3, y1 as usize + 3), Some(MISSING_COLOR));
        let dash = (ch / 4..ch * 3 / 4)
            .flat_map(|dy| (0..cw).map(move |dx| (x1 as usize + dx, y1 as usize + dy)))
            .filter(|&(x, y)| c.pixel(x, y).unwrap()[0] > 150)
            .count();
        assert!(dash > 5, "{dash}");
    }
}
