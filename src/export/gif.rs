//! 轉成 GIF：把 A-B 段落轉成會動的 GIF（重新編碼，最長 30 秒）。
//!
//! 在背景執行緒（[`run`]）另外開一個編碼模式的 mpv（`o=….gif`），不動正在播放的那一個：
//! - **來源是 EDL 的一段**（`edl://…,start=A,length=B−A`）：mpv 的 `end` 在濾鏡之後才檢查，
//!   整段一個色盤（palettegen）要等輸入結束才輸出，用 `end` 會一路解碼、存著畫面到檔尾。
//!   EDL 的一段讀到 B 就結束，起點、終點都準，色盤也等得到結尾。
//! - **跳轉不準的格式**（TS、M2TS、MPEG-PS，[`approximate_seeks`]）：FFmpeg 用時間戳搜尋，
//!   跳到 A 常落在 A 之後的關鍵影格、甚至檔尾（GIF 晚開始、少一段，或整個失敗）。
//!   EDL 改從 A 前面幾秒開始（[`Lead`]），再精確跳到 A（`start` + `hr-seek`：分離器落在 EDL 的開頭）。
//!   A 之前多讀的畫面在我們的濾鏡裡就丟掉（fps 的 `start_time`，[`Plan::trim`]）：不進色盤、不佔記憶體，
//!   格率的格線也從 A 算起（第一格就是 A 那一格）。mpv 自己的 hr-seek 要到濾鏡之後才丟畫面，太晚了。
//!   多前面才夠先另外開一個 mpv 試跳（[`run`]，只解一格）：落點在 A 之後就再往前，最後從檔案開頭讀。
//! - **畫面**（[`Graph`]）：格率（每格取那個時間畫面上的那一格）、縮放（長邊）、HDR 轉一般畫面（zscale + tonemap，亮度照「畫質 → HDR」的目標亮度）、
//!   轉正（檔案本身的旋轉 + 使用者的旋轉、翻轉，跟 Ctrl+E 截圖一樣；`video-rotate=no`，自己在色盤之前轉）、
//!   色盤（短的整段一個色盤，大的每格一個）。影像調整、像素著色器、縮放、平移、裁切不套用。
//! - **字幕**（勾選時）：目前顯示的主字幕燒進畫面，外觀、延遲跟播放時一樣。在縮放、轉正之後畫
//!   （`vf=lavfi…,sub,lavfi…`）：使用者轉了畫面時字幕照樣是正的，大小跟畫面成比例。
//! - **濾鏡失敗**：mpv 會停用失敗的濾鏡、把原本的畫面照樣送出去（原始大小、沒有色盤的大檔案）。
//!   看到「Disabling filter」「Cannot convert」就停下來、算失敗；寫好之後再核對 GIF 的大小、長度。
//!
//! 時間：介面給的 A、B 是主播放器的播放時間；EDL 的 `start` 是影片的時間戳，要加上主播放器的 `demuxer-start-time`。
//! EDL 的時間從 0 開始（[`Plan::zero`]）：內嵌的字幕由 EDL 自己換算，外掛的字幕要把延遲減掉 EDL 的 0 秒（[`GifSub::delay`]）。

use super::clip::{OnReady, Source, StreamPick, check_local, inspect_source, match_track};
use super::{
    Ctl, Done, Failure, GIF_MAX_SECS, GifPrefs, Job, Kind, LogLine, LogTail, Note, Phase, Progress, check_space,
};
use crate::geometry::Geometry;
use crate::instance::Wake;
use crate::mpv::{EndReason, Event, Mpv};
use crate::picture::{Deinterlace, ToneCurve, ToneSettings};
use crate::player::{EngineCaps, Player, Track, TrackKind};
use crate::screenshot::Fixup;
use crate::settings::SubStyle;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ───────────── 大小、色盤 ─────────────

/// GIF 最短幾秒
pub const MIN_SECS: f64 = 0.2;
/// 整段一個色盤時，所有畫面要先存著（BGRA）：超過這麼多改成每格一個色盤（不用存著，檔案大一點）
pub const PALETTE_BUDGET: u64 = 384 * 1024 * 1024;

/// 色盤怎麼做
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// 整段一個色盤（畫質好、檔案小；要存著全部的畫面）
    Global,
    /// 每格一個色盤（不用存著畫面）
    PerFrame,
}

/// 大概有幾格
pub fn frame_count(length: f64, fps: u32) -> u64 {
    (length.max(0.0) * f64::from(fps)).ceil().max(1.0) as u64
}

/// 依要存著的畫面多大選色盤的做法
pub fn palette_mode(out: (u32, u32), frames: u64) -> PaletteMode {
    let bytes = u64::from(out.0) * u64::from(out.1) * 4 * frames;
    if bytes <= PALETTE_BUDGET {
        PaletteMode::Global
    } else {
        PaletteMode::PerFrame
    }
}

/// GIF 的大小
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sizes {
    /// 縮放成這個大小（轉正之前）
    pub scale: (u32, u32),
    /// 轉正之後（GIF 的大小）
    pub out: (u32, u32),
}

/// 偶數、至少 2（4:2:0 與 zimg 都要偶數）
fn even(x: f64) -> u32 {
    ((x / 2.0).round().max(1.0) as u32) * 2
}

/// 依顯示的大小（`video-dec-params` 的 dw×dh，已含像素比例、還沒轉正；只看比例）、要轉的角度、長邊算 GIF 的大小：
/// 轉正之後的長邊 = `long_side`，兩邊都是偶數
pub fn out_size(display: (f64, f64), rotate: u32, long_side: u32) -> Sizes {
    let (dw, dh) = (display.0.max(1.0), display.1.max(1.0));
    let swap = rotate % 180 == 90;
    let (fw, fh) = if swap { (dh, dw) } else { (dw, dh) };
    let long = f64::from(long_side);
    let (ow, oh) = if fw >= fh {
        (long, long * fh / fw)
    } else {
        (long * fw / fh, long)
    };
    let out = (even(ow), even(oh));
    Sizes {
        scale: if swap { (out.1, out.0) } else { out },
        out,
    }
}

// ───────────── HDR ─────────────

/// HDR 轉一般畫面
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tone {
    /// FFmpeg tonemap 濾鏡的曲線
    pub curve: &'static str,
    /// 參考白的亮度（nits）：跟播放時的目標亮度一樣，GIF 才跟畫面一樣亮
    pub npl: u32,
}

impl Tone {
    /// HDR 轉一般畫面的濾鏡（FFmpeg 的 zscale + tonemap；縮圖總覽圖也用）：轉成線性、參考白 `npl`、
    /// 轉到 BT.709 的色域、壓亮部，再轉回 BT.709 的 gamma 與有限範圍
    pub fn chain(&self) -> String {
        format!(
            "zscale=t=linear:npl={},format=gbrpf32le,zscale=p=bt709,tonemap=tonemap={}:desat=0,\
             zscale=t=bt709:m=bt709:r=tv,format=yuv444p",
            self.npl, self.curve
        )
    }
}

/// 「畫質 → HDR」的曲線 → FFmpeg tonemap 的曲線（它沒有 BT.2390，自動、BT.2390 用 Hable）
pub fn tone_curve(c: ToneCurve) -> &'static str {
    match c {
        ToneCurve::Auto | ToneCurve::Bt2390 | ToneCurve::Hable => "hable",
        ToneCurve::Mobius => "mobius",
        ToneCurve::Reinhard => "reinhard",
        ToneCurve::Clip => "clip",
        ToneCurve::Linear => "linear",
    }
}

/// HDR 影片怎麼轉：一般影片不轉；杜比視界 Profile 5 不轉（zimg 轉不了它的 IPT 色彩，附說明）；
/// 播放引擎沒有 zscale、tonemap 時不轉（亮部會變白，附說明）
pub fn hdr_plan(
    hdr: bool,
    dv_profile: Option<i64>,
    can_tonemap: bool,
    tone: &ToneSettings,
) -> (Option<Tone>, Option<Note>) {
    if !hdr && dv_profile != Some(5) {
        return (None, None);
    }
    if dv_profile == Some(5) {
        return (None, Some(Note::DolbyVision5));
    }
    if !can_tonemap {
        return (None, Some(Note::HdrClipped));
    }
    let tone = Tone {
        curve: tone_curve(tone.curve),
        npl: tone.target_peak.unwrap_or(ToneSettings::AUTO_PEAK),
    };
    (Some(tone), None)
}

// ───────────── 濾鏡 ─────────────

/// GIF 的畫面處理
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Graph {
    pub fps: u32,
    /// 縮放成這個大小（轉正之前）
    pub scale: (u32, u32),
    pub tone: Option<Tone>,
    /// 要轉正的部分（檔案本身的旋轉 + 使用者的旋轉、翻轉）
    pub fixup: Fixup,
    /// 播放引擎有 transpose（沒有時用 rotate：舊的系統 libmpv）
    pub transpose: bool,
    pub palette: PaletteMode,
    /// 在中間燒進字幕（mpv 的 sub 濾鏡）
    pub subtitles: bool,
}

/// mpv 的 lavfi 濾鏡：濾鏡圖用長度標示（裡面的逗號、分號不會被 mpv 拆開）
pub(super) fn lavfi(graph: &str) -> String {
    format!("lavfi=graph=%{}%{graph}", graph.len())
}

impl Graph {
    /// 畫面：格率、縮放、HDR 轉一般畫面、轉正（先旋轉、再翻轉，跟畫面上一樣）。
    /// 格率用 `round=up`：GIF 的第 k 格是 A + k/fps 那一刻畫面上的那一格（第一格就是 A 那一格）；
    /// 預設的四捨五入取的是半格之後的畫面，第一格常常是 A 的下一格
    pub fn picture(&self) -> String {
        self.picture_from(None)
    }

    /// 同 [`Graph::picture`]，`trim` = 格率從 EDL 的這一秒算起（[`Plan::trim`]）：之前的畫面在這裡就丟掉
    /// （只留最後一格當第一格），格線對齊這一秒。影像比這一秒晚開始時，第一格補到這一秒
    pub fn picture_from(&self, trim: Option<f64>) -> String {
        let fps = match trim {
            Some(t) => format!("fps=fps={}:start_time={t:.6}:round=up", self.fps),
            None => format!("fps=fps={}:round=up", self.fps),
        };
        let mut parts = vec![fps, format!("scale={}:{}:flags=lanczos", self.scale.0, self.scale.1)];
        if let Some(t) = self.tone {
            parts.push(t.chain());
        }
        match (self.fixup.rotate % 360, self.transpose) {
            (90, true) => parts.push("transpose=clock".into()),
            (270, true) => parts.push("transpose=cclock".into()),
            (90, false) => parts.push("rotate=PI/2:ow=ih:oh=iw".into()),
            (270, false) => parts.push("rotate=-PI/2:ow=ih:oh=iw".into()),
            (180, _) => parts.push("hflip,vflip".into()),
            _ => {}
        }
        if self.fixup.hflip {
            parts.push("hflip".into());
        }
        if self.fixup.vflip {
            parts.push("vflip".into());
        }
        parts.join(",")
    }

    /// 色盤：整段一個（只更新有變的範圍），或每格一個
    pub fn palette(&self) -> &'static str {
        match self.palette {
            PaletteMode::Global => {
                "split[a][b];[a]palettegen=max_colors=256:stats_mode=diff[p];\
                 [b][p]paletteuse=dither=sierra2_4a:diff_mode=rectangle"
            }
            PaletteMode::PerFrame => {
                "split[a][b];[a]palettegen=max_colors=256:stats_mode=single[p];\
                 [b][p]paletteuse=new=1:dither=sierra2_4a"
            }
        }
    }

    /// mpv 的 `vf`：有字幕時在畫面處理與色盤中間燒進字幕（色盤之後是 PAL8，不能再畫）
    pub fn vf(&self) -> String {
        self.vf_from(None)
    }

    /// 同 [`Graph::vf`]，格率從 EDL 的 `trim` 秒算起（[`Graph::picture_from`]）
    pub fn vf_from(&self, trim: Option<f64>) -> String {
        let picture = self.picture_from(trim);
        if self.subtitles {
            format!("{},sub,{}", lavfi(&picture), lavfi(self.palette()))
        } else {
            lavfi(&format!("{picture},{}", self.palette()))
        }
    }
}

/// 只有 A-B 這一段的 EDL（`start` 是影片的時間戳）。路徑用長度標示，逗號、分號、中文都沒問題
pub fn edl_url(target: &str, start: f64, length: f64) -> String {
    format!(
        "edl://!no_chapters;%{}%{target},start={:.6},length={:.6}",
        target.len(),
        start.max(0.0),
        length.max(0.0)
    )
}

// ───────────── 從哪裡開始讀 ─────────────

/// 這種封裝格式（主播放器的 `file-format`）跳轉不準：沒有索引，FFmpeg 用時間戳搜尋位置，
/// 常落在目標之後的關鍵影格、甚至檔尾（TS、M2TS、MTS 是 `mpegts`，MPEG-PS／VOB 是 `mpeg`）
pub fn approximate_seeks(file_format: &str) -> bool {
    matches!(file_format, "mpegts" | "mpeg")
}

/// mpv 精確跳轉（hr-seek）認定「到了」的誤差：比目標早這麼一點的畫面也算（跟 mpv 一樣）
pub const HR_SEEK_TOLERANCE: f64 = 0.005;

/// EDL 從 A 前面多早開始讀
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lead {
    /// 從 A 開始（跳轉準的格式：落在 A 之前的關鍵影格，EDL 自己丟掉 A 之前的畫面）
    None,
    /// 從 A 前面這麼多秒開始，再精確跳到 A（跳到 A − 秒數已經試過，落點在 A 之前）
    Secs(f64),
    /// 從檔案開頭讀（往前試了還是落在 A 之後；或 A 離開頭很近），再精確跳到 A
    FromStart,
}

/// 編碼用的 mpv 怎麼開這一段
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// 要開的 EDL
    pub url: String,
    /// 開好之後精確跳到這裡（EDL 的時間，就是 A）；None = 不跳（EDL 從 A 開始）
    pub start: Option<f64>,
    /// `hr-seek-demuxer-offset`：精確跳轉時分離器往前跳這麼多，落在試過的位置（EDL 的開頭）
    pub demuxer_offset: f64,
    /// EDL 的 0 秒是主播放器的幾秒
    pub zero: f64,
}

impl Plan {
    /// 範圍 `a`–`b`（主播放器的時間）、主播放器的 `demuxer-start-time`、往前讀多少
    pub fn new(target: &str, a: f64, b: f64, main_start: f64, lead: Lead) -> Plan {
        let length = b - a;
        match lead {
            Lead::None => Plan {
                url: edl_url(target, a + main_start, length),
                start: None,
                demuxer_offset: 0.0,
                zero: a,
            },
            // EDL 的開頭在檔案開頭之前：從開頭讀
            Lead::Secs(secs) if a - secs + main_start <= 0.0 => Plan::new(target, a, b, main_start, Lead::FromStart),
            Lead::Secs(secs) => Plan {
                url: edl_url(target, a - secs + main_start, length + secs),
                start: Some(secs),
                // 分離器跳到 EDL 的 0 秒：跟試跳的位置一樣
                demuxer_offset: secs,
                zero: a - secs,
            },
            Lead::FromStart => {
                // EDL 從影片的時間戳 0 開始：A 在 EDL 的 A + 開始時間
                let at = a + main_start;
                Plan {
                    url: edl_url(target, 0.0, length + at),
                    start: Some(at),
                    // 分離器跳到開頭之前（-1 秒）：剛好跳到第一個時間戳也可能落到下一個關鍵影格
                    demuxer_offset: at + 1.0,
                    zero: -main_start,
                }
            }
        }
    }

    /// A 在 EDL 的幾秒
    pub fn a_at(&self) -> f64 {
        self.start.unwrap_or(0.0)
    }

    /// 從 A 前面讀時，格率（fps 濾鏡）從 EDL 的這一秒算起：A 加上 [`HR_SEEK_TOLERANCE`]
    /// （A 是主播放器停住的那一格的時間，換算之後差一點點也要算 A 那一格）。EDL 從 A 開始時 None
    pub fn trim(&self) -> Option<f64> {
        self.start.map(|at| at + HR_SEEK_TOLERANCE)
    }
}

/// fps 濾鏡從 `trim` 秒算起時，送出的畫面時間比選的畫面晚多少秒：它的時間是 1/fps 的整數倍
/// （第一格 = `trim` 之後的第一個 1/fps），畫面是 `trim` + k/fps 那一刻的。燒進的字幕照送出的時間找，
/// 字幕延遲要多這麼多才跟畫面對齊
pub fn grid_shift(trim: f64, fps: u32) -> f64 {
    let fps = f64::from(fps.max(1));
    ((trim * fps).ceil() / fps - trim).max(0.0)
}

/// 往前讀的秒數依序試這些（片段輸出用的一樣），都落在 A 之後就從檔案開頭讀
pub const LEAD_STEPS: [f64; 2] = [super::clip::PREROLL, super::clip::LONG_PREROLL];

/// 收到的記錄裡有沒有表示 GIF 不能用的失敗：濾鏡失敗（mpv 停用它、照樣把原本的畫面編進去）、寫不進去。
/// 濾鏡失敗一直記著（[`LogTail::graph_failed`]）：Shutdown 之後才收完的記錄裡，最近幾行可能早就沒有它
pub(super) fn gif_failure(log: &LogTail) -> Option<Failure> {
    if log.graph_failed() {
        return Some(Failure::FilterFailed);
    }
    match super::map_mpv_error(&log.lines()) {
        Some(
            f @ (Failure::WriteFailed | Failure::NoPermission(_) | Failure::EncoderMissing | Failure::FormatMissing),
        ) => Some(f),
        _ => None,
    }
}

/// mpv 停用了失敗的濾鏡、或轉不成能編碼的格式（之後的畫面沒有經過我們的濾鏡）
pub fn graph_failed(lines: &[LogLine]) -> bool {
    lines.iter().any(LogLine::is_graph_failure)
}

// ───────────── 字幕 ─────────────

/// 燒進 GIF 的字幕（主播放器目前顯示的主字幕）
#[derive(Debug, Clone, PartialEq)]
pub enum GifSub {
    /// 影片檔裡的字幕
    Embedded(StreamPick),
    /// 外掛的字幕檔（主播放器載入的檔案：文字字幕是轉成 UTF-8 的暫存檔）；`embedded` = 影片檔裡有幾條字幕
    External { path: String, embedded: usize },
}

impl GifSub {
    /// 主播放器顯示的字幕 → 要燒進 GIF 的（主播放器沒選字幕時 None）
    pub fn of(tracks: &[Track], track: &Track) -> Option<GifSub> {
        if track.kind != TrackKind::Sub {
            return None;
        }
        if track.external {
            let path = track.external_filename.clone()?;
            let embedded = tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Sub && !t.external)
                .count();
            return Some(GifSub::External { path, embedded });
        }
        StreamPick::of(tracks, track).map(GifSub::Embedded)
    }

    /// 編碼用的 mpv 裡預期的軌道編號（外掛的排在內嵌的後面）
    pub fn predicted_id(&self) -> i64 {
        match self {
            GifSub::Embedded(p) => p.ordinal as i64,
            GifSub::External { embedded, .. } => *embedded as i64 + 1,
        }
    }

    /// 編碼用的 mpv 的字幕延遲：內嵌的字幕由 EDL 換算時間，延遲照舊；
    /// 外掛的字幕照播放時間走，EDL 的 0 秒是主播放器的 `zero` 秒（通常是 A），要提早 `zero` 秒
    pub fn delay(&self, main_delay: f64, zero: f64) -> f64 {
        match self {
            GifSub::Embedded(_) => main_delay,
            GifSub::External { .. } => main_delay - zero,
        }
    }

    /// 在編碼用的 mpv 的軌道清單裡找它
    fn find(&self, tracks: &[Track]) -> Option<i64> {
        match self {
            GifSub::Embedded(p) => match_track(tracks, p),
            GifSub::External { path, .. } => {
                let external = |t: &&Track| t.kind == TrackKind::Sub && t.external;
                tracks
                    .iter()
                    .filter(external)
                    .find(|t| t.external_filename.as_deref() == Some(path.as_str()))
                    .or_else(|| tracks.iter().find(external))
                    .map(|t| t.id)
            }
        }
    }

    /// 外掛的字幕檔
    fn file(&self) -> Option<&str> {
        match self {
            GifSub::External { path, .. } => Some(path),
            GifSub::Embedded(_) => None,
        }
    }
}

// ───────────── 要轉的 GIF ─────────────

/// 測試用：觀察、改變編碼用的 mpv
#[derive(Clone, Default)]
#[doc(hidden)]
pub struct TestHooks {
    /// 加在我們的濾鏡前面的 `vf`（測試「濾鏡失敗」：mpv 停用它、畫面照樣送出）
    pub extra_vf: Option<String>,
    /// 編碼用的 mpv 開好檔案時呼叫（讀它的選項、長度）
    pub on_ready: Option<OnReady>,
    /// 第一次用這個字幕編號，不用預期的（測試「字幕對不上時重開」）
    pub initial_sid: Option<Option<i64>>,
    /// 換掉色盤的做法（測試取消：每格一個色盤時畫面一開始就寫進檔案）
    pub palette: Option<PaletteMode>,
}

impl std::fmt::Debug for TestHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestHooks")
            .field("extra_vf", &self.extra_vf)
            .field("on_ready", &self.on_ready.is_some())
            .field("initial_sid", &self.initial_sid)
            .field("palette", &self.palette)
            .finish()
    }
}

/// 從設定與畫面來的選擇（介面執行緒準備）
#[derive(Debug, Clone, Copy)]
pub struct Choice<'a> {
    pub prefs: GifPrefs,
    /// 使用者的旋轉、翻轉（縮放、平移、裁切不套用）
    pub geometry: &'a Geometry,
    /// 「畫質 → HDR」：曲線、目標亮度
    pub tone: &'a ToneSettings,
    /// 字幕外觀
    pub style: &'a SubStyle,
    pub deinterlace: Deinterlace,
}

/// 一個 GIF 要的全部資料（介面執行緒從主播放器取得，背景執行緒只用這些）
#[derive(Debug, Clone)]
pub struct GifSpec {
    pub source: Source,
    /// 範圍（主播放器的時間，秒）
    pub a: f64,
    pub b: f64,
    /// 主播放器的 `demuxer-start-time`
    pub main_start: f64,
    /// 主播放器選的影像（None = 讓 mpv 選：外掛的影像）
    pub video: Option<StreamPick>,
    pub graph: Graph,
    /// GIF 的大小（轉正之後；寫好之後核對）
    pub out: (u32, u32),
    /// 燒進的字幕（None = 不燒）
    pub sub: Option<GifSub>,
    /// 編碼用的 mpv 的字幕延遲（已經換算好，見 [`GifSub::delay`]）：EDL 從 A 開始時的；
    /// 從 A 前面讀時用 [`GifSpec::sub_delay_from`]
    pub sub_delay: f64,
    /// 來源的跳轉不準（[`approximate_seeks`]）：先試跳轉，EDL 從 A 前面開始讀
    pub approx_seek: bool,
    /// 寫好之後核對長度時，GIF 可以比範圍短多少秒（[`length_slack`]）
    pub slack: f64,
    /// 字幕外觀（`SubStyle::mpv_options`）
    pub sub_options: Vec<(&'static str, String)>,
    /// 去交錯（主播放器的設定：auto / yes / no）
    pub deinterlace: &'static str,
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

/// 主播放器目前的檔案能不能轉成 GIF（不管範圍）；不能時回傳原因（右鍵選單停用時的說明）
pub fn unavailable(player: &Player, caps: &EngineCaps) -> Option<Failure> {
    check(player, caps).err()
}

fn check(player: &Player, caps: &EngineCaps) -> Result<super::clip::Inspected, Failure> {
    let st = &player.state;
    if !st.loaded {
        return Err(Failure::NoData);
    }
    if !st.has_video() {
        return Err(Failure::NoVideo);
    }
    // paletteuse 不能單獨偵測：有 palettegen 的 FFmpeg 都有
    if !(caps.gif && caps.palettegen) {
        return Err(Failure::EncoderMissing);
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

/// 範圍的長度能不能轉（0.2–30 秒）
pub fn check_length(length: f64) -> Result<(), Failure> {
    if !(length.is_finite() && length > 0.0) {
        Err(Failure::NoData)
    } else if length > GIF_MAX_SECS + 0.0005 {
        Err(Failure::GifTooLong)
    } else if length < MIN_SECS - 0.0005 {
        Err(Failure::RangeTooShort)
    } else {
        Ok(())
    }
}

/// 換成容器標示的像素比例：mpv 預設（`video-aspect-method=container`）照容器的（MKV 的顯示大小、MP4 的 pasp）顯示，
/// `video-dec-params` 的比例是影像資料本身的。`dec_par`、`container_par` = 像素的寬 ÷ 高。
/// 容器沒有標示（或不合理）時不換；影像資料沒有標示時 mpv 當成方形像素
pub fn container_aspect(dec_aspect: f64, dec_par: Option<f64>, container_par: Option<f64>) -> f64 {
    let ok = |x: &f64| x.is_finite() && *x > 0.0;
    match container_par.filter(ok) {
        Some(container) => dec_aspect / dec_par.filter(ok).unwrap_or(1.0) * container,
        None => dec_aspect,
    }
}

/// 檔案原本在畫面上的比例（還沒轉正、含容器的像素比例）與檔案本身的旋轉。
/// 還沒解出第一格（不知道檔案本身的旋轉、像素比例）時 None：開始的按鈕停用，請使用者稍候
pub(super) fn source_shape(player: &Player) -> Option<(f64, i64)> {
    let (aspect, rotate) = player.natural_shape()?;
    let raw = if rotate.rem_euclid(180) == 90 {
        1.0 / aspect
    } else {
        aspect
    };
    let container_par = player.get_f64("current-tracks/video/demux-par").ok();
    Some((container_aspect(raw, decoder_par(player), container_par), rotate))
}

/// 解碼器給的像素比例（`video-dec-params` 的 par；舊的 libmpv 沒有這一項時用 dw×h ÷ dh×w 算）
fn decoder_par(player: &Player) -> Option<f64> {
    #[derive(serde::Deserialize)]
    struct Dec {
        w: f64,
        h: f64,
        dw: f64,
        dh: f64,
        par: Option<f64>,
    }
    let d: Dec = serde_json::from_str(&player.get_string("video-dec-params").ok()?).ok()?;
    d.par
        .or_else(|| (d.w > 0.0 && d.h > 0.0 && d.dh > 0.0).then(|| d.dw * d.h / (d.dh * d.w)))
}

/// 目前的影片是不是 HDR：直接問解碼器（`State::video_hdr` 跟著畫面輸出的參數，剛開檔時可能還沒到）
pub fn source_hdr(player: &Player) -> bool {
    match player.get_string("video-dec-params/gamma") {
        Ok(g) => matches!(g.as_str(), "pq" | "hlg"),
        Err(_) => player.state.video_hdr,
    }
}

/// 這個檔案轉成 GIF 的大小（介面顯示用）：None = 還不知道畫面的比例
pub fn sizes_for(player: &Player, geometry: &Geometry, long_side: u32) -> Option<Sizes> {
    let (aspect, rotate) = source_shape(player)?;
    let fix = super::fixup(rotate, geometry);
    Some(out_size((aspect, 1.0), fix.rotate, long_side))
}

impl GifSpec {
    /// 從主播放器準備一個 GIF：範圍 `a`–`b`（主播放器的時間）、設定與畫面的選擇、存放的資料夾。
    /// 不能轉（沒有影像、直播、章節連結、長度不對…）時回傳原因
    pub fn from_player(
        player: &Player,
        caps: &EngineCaps,
        a: f64,
        b: f64,
        choice: &Choice<'_>,
        dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<GifSpec, Failure> {
        let found = check(player, caps)?;
        check_length(b - a)?;
        let st = &player.state;
        let (aspect, rotate) = source_shape(player).ok_or(Failure::NoData)?;
        let fix = super::fixup(rotate, choice.geometry);
        let sizes = out_size((aspect, 1.0), fix.rotate, choice.prefs.long_side);
        let palette = palette_mode(sizes.out, frame_count(b - a, choice.prefs.fps));
        let video_track = st.selected(TrackKind::Video);
        let dv = video_track.and_then(|t| t.dolby_vision_profile);
        let (tone, note) = hdr_plan(source_hdr(player), dv, caps.zscale && caps.tonemap, choice.tone);
        let sub = choice
            .prefs
            .subtitles
            .then(|| st.selected(TrackKind::Sub))
            .flatten()
            .and_then(|t| GifSub::of(&st.tracks, t));
        // 直接問 mpv（屬性的通知可能還沒到）
        let main_delay = player.get_f64("sub-delay").unwrap_or(st.sub_delay);
        let sub_delay = sub.as_ref().map_or(0.0, |s| s.delay(main_delay, a));
        // 影片本身的格率（核對長度用）：容器標示的，沒有時用 mpv 估計的
        let source_fps = player
            .get_f64("container-fps")
            .or_else(|_| player.get_f64("estimated-vf-fps"))
            .ok();
        let edge = a <= EDGE_SLACK || st.duration.is_some_and(|d| b >= d - EDGE_SLACK);
        Ok(GifSpec {
            stem: super::range_stem(&found.path, st.title.as_deref(), a, b),
            source: found.source,
            a,
            b,
            main_start: player.demuxer_start_time(),
            video: video_track.and_then(|t| StreamPick::of(&st.tracks, t)),
            graph: Graph {
                fps: choice.prefs.fps,
                scale: sizes.scale,
                tone,
                fixup: fix,
                transpose: caps.transpose,
                palette,
                subtitles: sub.is_some(),
            },
            out: sizes.out,
            sub,
            sub_delay,
            approx_seek: approximate_seeks(&found.file_format),
            slack: length_slack(b - a, choice.prefs.fps, source_fps, edge),
            sub_options: choice.style.mpv_options(),
            deinterlace: choice.deinterlace.effective(caps).mpv(),
            dir,
            cache_dir,
            notes: note.into_iter().collect(),
            test: TestHooks::default(),
        })
    }

    /// 存好的檔案預定的名稱
    pub fn wanted(&self) -> PathBuf {
        self.dir.join(format!("{}.gif", self.stem))
    }

    /// EDL 的 0 秒是主播放器的 `zero` 秒時（[`Plan::zero`]）編碼用的 mpv 的字幕延遲：
    /// 外掛的字幕跟著 EDL 的開頭移動（`sub_delay` 是 EDL 從 A 開始時的），內嵌的不變
    pub fn sub_delay_from(&self, zero: f64) -> f64 {
        match &self.sub {
            Some(GifSub::External { .. }) => self.sub_delay + (self.a - zero),
            _ => self.sub_delay,
        }
    }
}

// ───────────── 寫好的 GIF ─────────────

/// GIF 檔頭與區塊的資料
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GifInfo {
    /// 邏輯畫面的大小（檔頭的第 6–9 位元組）
    pub width: u16,
    pub height: u16,
    /// 影格數
    pub frames: usize,
    /// 每格停留時間的總和（1/100 秒）
    pub delay_cs: u64,
}

impl GifInfo {
    /// 總長度（秒）
    pub fn seconds(&self) -> f64 {
        self.delay_cs as f64 / 100.0
    }
}

/// 讀 GIF 的檔頭、逐一走過區塊（數影格、加總停留時間）。不是完整的 GIF（沒有結尾、區塊壞掉）時 None
pub fn gif_info(data: &[u8]) -> Option<GifInfo> {
    if data.len() < 13 || !(data.starts_with(b"GIF89a") || data.starts_with(b"GIF87a")) {
        return None;
    }
    let mut info = GifInfo {
        width: u16::from_le_bytes([data[6], data[7]]),
        height: u16::from_le_bytes([data[8], data[9]]),
        frames: 0,
        delay_cs: 0,
    };
    let mut i = 13;
    if data[10] & 0x80 != 0 {
        i += 3 << ((data[10] & 7) + 1); // 全域色盤
    }
    // 子區塊：長度 + 資料，長度 0 結束
    let skip_sub_blocks = |mut i: usize| -> Option<usize> {
        loop {
            let n = *data.get(i)? as usize;
            if n == 0 {
                return Some(i + 1);
            }
            i += n + 1;
        }
    };
    loop {
        match *data.get(i)? {
            0x21 => {
                // 圖形控制擴充區塊：0x21 0xF9 0x04 旗標 停留時間（2 位元組） 透明色 0x00
                if *data.get(i + 1)? == 0xF9 {
                    info.delay_cs += u64::from(u16::from_le_bytes([*data.get(i + 4)?, *data.get(i + 5)?]));
                }
                i = skip_sub_blocks(i + 2)?;
            }
            0x2c => {
                info.frames += 1;
                let flags = *data.get(i + 9)?;
                i += 10;
                if flags & 0x80 != 0 {
                    i += 3 << ((flags & 7) + 1); // 區域色盤
                }
                // LZW 最小碼長之後是影像資料
                i = skip_sub_blocks(i + 1)?;
            }
            0x3b => return Some(info),
            _ => return None,
        }
    }
}

/// 範圍碰到檔案的頭、尾（離開頭、結尾這麼多秒以內）時，GIF 可以多短這麼多：
/// 影像可能比聲音晚開始、早結束（主播放器的長度、時間是所有軌道合起來的）
pub const EDGE_SLACK: f64 = 1.0;

/// 寫好的 GIF 可以比範圍（`length` 秒）短多少秒：
/// - GIF 的兩格：fps 濾鏡頭尾各對齊一格，最後一格的停留時間各家不同；
/// - 影片本身的兩格（`source_fps`）：範圍的頭尾不一定在影片的影格上；
///   不知道影片的格率時照舊寬鬆：最多 1 秒、一半以內；
/// - 範圍碰到檔案的頭尾（`edge`）時多給 [`EDGE_SLACK`]。
///
/// 比這個短就是範圍的開頭沒讀到（跳轉落在 A 之後的關鍵影格）或讀不到資料，算失敗。
/// 從 A 前面讀時（[`Plan::trim`]）fps 會把第一格補到 A：開頭由試跳保證（[`lead_in`]），這裡只看得出結尾不夠
pub fn length_slack(length: f64, fps: u32, source_fps: Option<f64>, edge: bool) -> f64 {
    let base = match source_fps.filter(|f| f.is_finite() && *f > 0.0) {
        Some(f) => 2.0 / f64::from(fps.max(1)) + 2.0 / f,
        None => (length * 0.5).min(1.0),
    };
    base + if edge { EDGE_SLACK } else { 0.0 }
}

/// 寫好的 GIF 對不對：完整、有影格、大小是預期的（不是的話濾鏡沒有套用）、
/// 長度夠（`length` 秒，最多短 `slack` 秒：[`length_slack`]；開頭晚了、範圍讀不到資料時太短）
pub fn check_gif(data: &[u8], out: (u32, u32), length: f64, slack: f64) -> Result<GifInfo, Failure> {
    let info = gif_info(data).ok_or(Failure::Unplayable(None))?;
    if info.frames == 0 {
        return Err(Failure::NoData);
    }
    if (u32::from(info.width), u32::from(info.height)) != out {
        return Err(Failure::FilterFailed);
    }
    // 長度：看每格停留時間（1/100 秒）的總和
    let want = length - slack.max(0.0);
    if info.seconds() < want {
        return Err(Failure::TooShort {
            got: info.seconds(),
            want: length,
        });
    }
    Ok(info)
}

// ───────────── 背景工作 ─────────────

/// 開始轉這個 GIF（背景執行緒）
pub fn spawn(spec: GifSpec, wake: Wake) -> Job {
    Job::spawn(Kind::Gif, wake, move |ctl| run(ctl, spec))
}

/// 編碼用的 mpv 開檔最多等多久（網路很慢、CI 很慢）
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// 等事件、看進度的間隔（秒）
const POLL: f64 = 0.1;
/// 停下來（取消、失敗）時最多等 mpv 結束這麼久
const QUIT_WAIT: Duration = Duration::from_secs(5);
/// 目的地另外要留的空間
const SPARE: u64 = 1024 * 1024;

/// GIF 的工作本體（在 `Job` 的背景執行緒裡）
pub fn run(ctl: &Ctl, spec: GifSpec) -> Result<Done, Failure> {
    // 換不成正式名稱時留下的完整檔案登記在快取資料夾：啟動時清暫存檔不會刪掉它
    ctl.set_keep_dir(Some(spec.cache_dir.clone()));
    ctl.check()?;
    let length = spec.b - spec.a;
    if !(spec.a.is_finite() && spec.b.is_finite()) {
        return Err(Failure::NoData);
    }
    check_length(length)?;
    if let Source::File(path) = &spec.source {
        check_local(path)?;
    }
    ctl.progress(Progress {
        phase: Phase::Converting,
        fraction: Some(0.0),
    });
    std::fs::create_dir_all(&spec.dir).map_err(|_| Failure::NoPermission(Some(spec.dir.clone())))?;
    // 大概的上限：每格每個像素 0.3 位元組
    let frames = frame_count(length, spec.graph.fps);
    let need = (f64::from(spec.out.0) * f64::from(spec.out.1) * frames as f64 * 0.3) as u64 + SPARE;
    check_space(&[(spec.dir.as_path(), need)])?;
    let temp = ctl.temp_in(&spec.dir, &spec.stem, "gif");
    let with_dir = |f: Failure| match f {
        Failure::NoPermission(None) => Failure::NoPermission(Some(spec.dir.clone())),
        other => other,
    };

    // 影像、字幕的編號：先用預期的（同一類的內嵌軌道裡排第幾；外掛的字幕排在內嵌的後面）
    let mut ids = Ids {
        vid: spec.video.as_ref().map(|v| v.ordinal as i64),
        sid: spec
            .test
            .initial_sid
            .unwrap_or_else(|| spec.sub.as_ref().map(GifSub::predicted_id)),
    };
    // 跳轉不準的格式：先試跳，決定 EDL 從 A 前面多早開始讀
    let lead = if spec.approx_seek {
        lead_in(spec.a, spec.main_start, |at| Encoder::landing(ctl, &spec, ids.vid, at))?
    } else {
        Lead::None
    };
    let plan = Plan::new(&spec.source.target(), spec.a, spec.b, spec.main_start, lead);
    let mut retried = false;
    let mut enc = loop {
        let mut enc = Encoder::open(ctl, &spec, &plan, &temp, ids).map_err(with_dir)?;
        // 影像、字幕對不上（編號跟預期的不同）：用對的編號重開一次（開檔時已經開始編碼，不在中途換）
        let tracks = enc.tracks();
        let want = Ids {
            vid: match &spec.video {
                Some(v) => match_track(&tracks, v),
                None => ids.vid,
            },
            sid: match &spec.sub {
                Some(sub) => sub.find(&tracks),
                None => ids.sid,
            },
        };
        if want != ids {
            enc.stop();
            drop(enc);
            let _ = std::fs::remove_file(&temp);
            let lost = (spec.video.is_some() && want.vid.is_none()) || (spec.sub.is_some() && want.sid.is_none());
            if retried || lost {
                return Err(Failure::TrackNotFound);
            }
            ids = want;
            retried = true;
            continue;
        }
        if let Some(hook) = &spec.test.on_ready {
            hook(&enc.mpv);
        }
        break enc;
    };
    let encoded = enc.encode(ctl, &spec.source, plan.a_at(), length);
    // 先結束 mpv（Windows 上 mpv 開著檔案時讀不到完整的內容、刪不掉）
    drop(enc);
    encoded.map_err(with_dir)?;
    ctl.check()?;
    ctl.progress(Progress {
        phase: Phase::Checking,
        fraction: None,
    });
    let data = std::fs::read(&temp).map_err(|e| Failure::Io(e.to_string()))?;
    check_gif(&data, spec.out, length, spec.slack)?;
    drop(data);
    let path = ctl.finish(&spec.wanted())?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(Done {
        kind: Kind::Gif,
        path,
        bytes,
        actual: None,
        notes: spec.notes.clone(),
    })
}

/// 跳轉不準的格式：EDL 要從 A 前面多早開始讀（`a` = A、`main_start` = 主播放器的 `demuxer-start-time`）。
/// 依序試 [`LEAD_STEPS`]：`landing(A − 秒數)` 不精確地跳轉（[`Encoder::landing`]），回傳停住的那一格的時間
/// （落在檔尾時 None）。落點在 A 之前（或就是 A）就從那裡讀；落在 A 之後、檔尾，或 A − 秒數已經在檔案開頭之前，從開頭讀
pub fn lead_in(
    a: f64,
    main_start: f64,
    mut landing: impl FnMut(f64) -> Result<Option<f64>, Failure>,
) -> Result<Lead, Failure> {
    for secs in LEAD_STEPS {
        let at = a - secs;
        if at + main_start <= 0.0 {
            break;
        }
        if landing(at)?.is_some_and(|t| t <= a + HR_SEEK_TOLERANCE) {
            return Ok(Lead::Secs(secs));
        }
    }
    Ok(Lead::FromStart)
}

/// 編碼用的 mpv 讀到 EDL 的 `pts` 秒（`eof` = 讀完了）時轉換的進度：A 在 EDL 的 `a_at` 秒，之前多讀的部分算 0。
/// 回傳（進度 0–1、還在讀）：讀到 B 前面一點或讀完就不算在讀了（之後是產生色盤、寫檔）
pub fn read_progress(pts: Option<f64>, eof: bool, a_at: f64, length: f64) -> (f32, bool) {
    let pts = pts.map(|p| p - a_at);
    let reading = !(eof || pts.is_some_and(|p| p >= length - 0.1));
    let fraction = pts.map_or(0.0, |p| (p / length.max(0.001)).clamp(0.0, 1.0));
    (fraction as f32, reading)
}

/// 另外開的 mpv（編碼用的、縮圖總覽圖的擷取）開檔、讀檔失敗的原因：看 mpv 的記錄（`log` 要先收完排著的事件），
/// 寫檔、濾鏡的失敗優先；來源打不開時網址一律是「網址打不開」，本機檔案再看檔案還在不在、讀不讀得到
pub(super) fn load_failure(log: &LogTail, source: &Source) -> Failure {
    let mapped = log.failure();
    if let Some(
        f @ (Failure::EncoderMissing
        | Failure::FormatMissing
        | Failure::NoPermission(_)
        | Failure::WriteFailed
        | Failure::FilterFailed),
    ) = mapped
    {
        return f;
    }
    match source {
        // 網址：HTTP 403、連結過期、斷線在這裡都只有「Failed to open …」，細節只在主播放器的 FFmpeg 記錄裡
        Source::Net(_) => match mapped {
            Some(Failure::SourceUnreadable) => Failure::SourceUnreadable,
            _ => Failure::SourceUnreachable,
        },
        Source::File(path) => match mapped {
            Some(f @ (Failure::SourceMissing | Failure::SourceNoAccess | Failure::SourceUnreadable)) => f,
            _ => match check_local(path) {
                Err(f) => f,
                Ok(()) => mapped.unwrap_or(Failure::SourceUnreadable),
            },
        },
    }
}

/// 編碼用的 mpv 要選的影像、字幕編號（None = 影像讓 mpv 選、不要字幕）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ids {
    vid: Option<i64>,
    sid: Option<i64>,
}

/// 編碼用的 mpv
struct Encoder {
    mpv: Mpv,
    log: LogTail,
    /// 已經收到 Shutdown（mpv 只送一次：收完排著的事件時收到的也要記得）
    shut_down: bool,
}

impl Encoder {
    /// 開一個編碼模式的 mpv（寫到 `temp`），照 `plan` 開 A-B 這一段，等到檔案載入（`ids` = 影像、要燒進的字幕的編號）
    fn open(ctl: &Ctl, spec: &GifSpec, plan: &Plan, temp: &Path, ids: Ids) -> Result<Encoder, Failure> {
        let mut graph = spec.graph;
        if let Some(palette) = spec.test.palette {
            graph.palette = palette;
        }
        // 從 A 前面讀：A 之前的畫面在 fps 濾鏡就丟掉，格線從 A 算起
        let trim = plan.trim();
        let mut vf = graph.vf_from(trim);
        if let Some(extra) = &spec.test.extra_vf {
            vf = format!("{extra},{vf}");
        }
        // 解碼的執行緒不要搶走正在播放的那一個的 CPU
        let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(2));
        let mut opts: Vec<(&str, String)> = vec![
            ("o", temp.to_string_lossy().into_owned()),
            ("of", "gif".into()),
            ("ovc", "gif".into()),
            // 播完這一段就結束：GIF 在 mpv 結束時才寫完
            ("idle", "once".into()),
            ("hwdec", "no".into()),
            // 旋轉自己在色盤之前做（mpv 的自動旋轉在我們的濾鏡之後，會把 PAL8 又轉回 RGB）
            ("video-rotate", "no".into()),
            ("vf", vf),
            // 主播放器選的影像（多畫質、多角度的檔案）
            ("vid", ids.vid.map_or_else(|| "auto".to_owned(), |i| i.to_string())),
            ("aid", "no".into()),
            ("sid", ids.sid.map_or_else(|| "no".to_owned(), |i| i.to_string())),
            // fps 從 A 算起時送出的時間比畫面晚一點（[`grid_shift`]）：字幕跟著晚一點
            (
                "sub-delay",
                format!(
                    "{:.6}",
                    spec.sub_delay_from(plan.zero) + trim.map_or(0.0, |t| grid_shift(t, graph.fps))
                ),
            ),
            ("deinterlace", spec.deinterlace.into()),
            ("vd-lavc-threads", threads.to_string()),
            ("osd-level", "0".into()),
            ("cover-art-auto", "no".into()),
        ];
        if let Some(at) = plan.start {
            // EDL 從 A 前面開始：精確跳到 A，分離器跳到 EDL 的開頭（試過的位置）
            opts.push(("start", format!("{at:.6}")));
            opts.push(("hr-seek", "yes".into()));
            opts.push(("hr-seek-demuxer-offset", format!("{:.6}", plan.demuxer_offset)));
        }
        if spec.sub.is_some() {
            opts.extend(spec.sub_options.iter().map(|(k, v)| (*k, v.clone())));
        }
        // 建立失敗：編碼模式起不來（沒有 gif 編碼器、封裝格式）
        let mut enc = Encoder::create(spec, &opts).map_err(|e| {
            eprintln!("[vitascope] 轉 GIF：無法建立編碼用的 mpv：{}", e.description());
            Failure::EncoderMissing
        })?;
        // 沒有編碼模式的 libmpv 不認得 `o`（`Mpv::new` 會略過不認得的選項）：那樣會變成一般的播放、開出視窗
        if !enc.mpv.get_string("o").is_ok_and(|o| !o.is_empty()) {
            return Err(Failure::EncoderMissing);
        }
        // 外掛的字幕：加一個檔案（`sub-files-append` 經過 mpv_set_option 不認得，路徑也不能被拆開）
        if let Some(file) = spec.sub.as_ref().and_then(GifSub::file)
            && let Err(e) = enc.mpv.command(&["change-list", "sub-files", "append", file])
        {
            eprintln!("[vitascope] 轉 GIF：無法加入字幕檔：{}", e.description());
        }
        if let Err(e) = enc.mpv.command(&["loadfile", &plan.url]) {
            enc.stop();
            return Err(Failure::Engine(e.description()));
        }
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            if ctl.cancelled() {
                enc.stop();
                return Err(Failure::Cancelled);
            }
            if Instant::now() >= deadline {
                enc.stop();
                return Err(if matches!(spec.source, Source::Net(_)) {
                    Failure::SourceUnreachable
                } else {
                    Failure::ReadTimeout
                });
            }
            match enc.next(POLL) {
                Some(Event::FileLoaded) => return Ok(enc),
                Some(Event::EndFile { .. } | Event::Shutdown) => {
                    let f = enc.load_failure(&spec.source);
                    enc.stop();
                    return Err(f);
                }
                _ => {}
            }
            if enc.log.graph_failed() {
                enc.stop();
                return Err(Failure::FilterFailed);
            }
        }
    }

    /// 建立 mpv（`opts` 加上匯出共用的選項）：要警告以上的記錄、套用網路來源的連線方式
    fn create(spec: &GifSpec, opts: &[(&str, String)]) -> crate::mpv::Result<Encoder> {
        let mut refs: Vec<(&str, &str)> = opts.iter().map(|(k, v)| (*k, v.as_str())).collect();
        refs.extend_from_slice(super::INSTANCE_OPTIONS);
        let mpv = Mpv::new(&refs)?;
        let _ = mpv.request_log_messages("warn");
        if let Source::Net(stream) = &spec.source {
            // 跟主播放器一樣的連線方式（User-Agent、標頭、proxy、憑證、逾時；網站影片的 Cookie）
            for (name, value) in &stream.options {
                if let Err(e) = mpv.set_node(name, value) {
                    eprintln!("[vitascope] 轉 GIF：無法設定 {name}：{e}");
                }
            }
        }
        Ok(Encoder {
            mpv,
            log: LogTail::default(),
            shut_down: false,
        })
    }

    /// 試跳：另外開一個不編碼的 mpv（不出畫面、不出聲音、停住），在 `at`（主播放器的時間）不精確地跳轉，
    /// 跟編碼用的 mpv 從 EDL 的開頭跳轉一樣（同一個檔案、同一個分離器、同一個位置），
    /// 回傳停住的那一格（分離器落點之後第一個解得出來的畫面）的時間；落在檔尾、讀不到畫面時 None
    fn landing(ctl: &Ctl, spec: &GifSpec, vid: Option<i64>, at: f64) -> Result<Option<f64>, Failure> {
        let opts: Vec<(&str, String)> = vec![
            ("vo", "null".into()),
            ("ao", "null".into()),
            ("idle", "yes".into()),
            ("pause", "yes".into()),
            ("hwdec", "no".into()),
            ("hr-seek", "no".into()),
            ("start", format!("{at:.6}")),
            ("vid", vid.map_or_else(|| "auto".to_owned(), |i| i.to_string())),
            ("aid", "no".into()),
            ("sid", "no".into()),
            ("osd-level", "0".into()),
            ("cover-art-auto", "no".into()),
        ];
        let mut probe = Encoder::create(spec, &opts).map_err(|e| Failure::Engine(e.description()))?;
        if let Err(e) = probe.mpv.command(&["loadfile", &spec.source.target()]) {
            probe.stop();
            return Err(Failure::Engine(e.description()));
        }
        let deadline = Instant::now() + LOAD_TIMEOUT;
        let mut loaded = false;
        let landed = loop {
            if ctl.cancelled() {
                break Err(Failure::Cancelled);
            }
            if Instant::now() >= deadline {
                break Err(if matches!(spec.source, Source::Net(_)) {
                    Failure::SourceUnreachable
                } else {
                    Failure::ReadTimeout
                });
            }
            match probe.next(POLL) {
                Some(Event::FileLoaded) => loaded = true,
                // 停住的那一格解好了
                Some(Event::PlaybackRestart) => {
                    break Ok(probe.mpv.get_property::<f64>("time-pos").ok().filter(|t| t.is_finite()));
                }
                // 開好之後才結束：跳到檔尾，一格都沒有；開不起來：跟編碼用的 mpv 一樣回報原因
                Some(Event::EndFile { .. } | Event::Shutdown) if loaded => break Ok(None),
                Some(Event::EndFile { .. } | Event::Shutdown) => break Err(probe.load_failure(&spec.source)),
                _ => {}
            }
        };
        probe.stop();
        landed
    }

    /// 下一個事件（記錄另外收起來；記下 Shutdown）
    fn next(&mut self, timeout: f64) -> Option<Event> {
        let ev = self.mpv.wait_event(timeout)?;
        self.log.push_event(&ev);
        if matches!(ev, Event::Shutdown) {
            self.shut_down = true;
        }
        Some(ev)
    }

    /// 收完排著的事件與記錄（mpv 先送排著的事件、最後才送記錄：Shutdown 之後可能還有記錄沒收）。
    /// Shutdown 只送一次，收完就沒有新的事件，不會一直收下去
    fn drain(&mut self) {
        while self.next(0.0).is_some() {}
    }

    /// 停下來：叫 mpv 結束，等它結束（最多一下子）。已經結束了就不用等
    fn stop(&mut self) {
        if self.shut_down || self.mpv.command(&["quit"]).is_err() {
            return;
        }
        let deadline = Instant::now() + QUIT_WAIT;
        while !self.shut_down && Instant::now() < deadline {
            self.next(0.1);
        }
    }

    /// 收到的記錄裡有沒有表示 GIF 不能用的失敗（見 [`gif_failure`]）
    fn failed(&self) -> Option<Failure> {
        gif_failure(&self.log)
    }

    /// 開檔、讀檔失敗的原因（來源打不開、寫不了 GIF）
    fn load_failure(&mut self, source: &Source) -> Failure {
        self.drain();
        load_failure(&self.log, source)
    }

    fn tracks(&self) -> Vec<Track> {
        self.mpv
            .get_string("track-list")
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    /// 讀到這一段的哪裡（`demuxer-cache-state` 的 `reader-pts`，EDL 的時間）、讀完了沒有。
    /// 整段一個色盤時要到最後才有畫面送到編碼器，`time-pos` 一直沒有值，只能看讀到哪裡
    fn reading(&self) -> Option<(Option<f64>, bool)> {
        let v: serde_json::Value = serde_json::from_str(&self.mpv.get_string("demuxer-cache-state").ok()?).ok()?;
        Some((v["reader-pts"].as_f64(), v["eof"].as_bool().unwrap_or(false)))
    }

    /// 編碼到 mpv 結束（GIF 在結束時寫完）。`a_at` = A 在 EDL 的幾秒（之前是多讀的部分）。
    /// 取消、濾鏡失敗、寫不進去時叫 mpv 停下來
    fn encode(&mut self, ctl: &Ctl, source: &Source, a_at: f64, length: f64) -> Result<(), Failure> {
        // 很寬的上限：CI 的機器很慢；大的 GIF 產生色盤、寫檔要一段時間；A 前面多讀的部分也要解碼
        let deadline = Instant::now() + Duration::from_secs(120) + Duration::from_secs_f64(4.0 * (length + a_at));
        let mut reading = true;
        let mut ended: Option<Failure> = None;
        // 收完排著的事件時可能已經收到 Shutdown（例如讀到一半出錯時）：這時不用再等
        while !self.shut_down {
            if ctl.cancelled() {
                self.stop();
                return Err(Failure::Cancelled);
            }
            if Instant::now() >= deadline {
                self.stop();
                return Err(Failure::ReadTimeout);
            }
            let ev = self.next(POLL);
            // 濾鏡失敗：mpv 停用它、照樣把原本的畫面編進去，要自己停下來
            if let Some(f) = self.failed() {
                self.stop();
                return Err(f);
            }
            // 讀到一半出錯（檔案被移走、網路斷線）：等 mpv 結束再回報
            if let Some(Event::EndFile {
                reason: EndReason::Error,
                ..
            }) = ev
            {
                ended = Some(self.load_failure(source));
            }
            if reading {
                let (pts, eof) = self.reading().unwrap_or((None, false));
                let (fraction, more) = read_progress(pts, eof, a_at, length);
                reading = more;
                ctl.progress(Progress {
                    phase: Phase::Converting,
                    fraction: Some(fraction),
                });
            }
            if !reading {
                ctl.progress(Progress {
                    phase: Phase::Palette,
                    fraction: None,
                });
            }
        }
        // mpv 先送排著的事件、最後才送記錄：結束前最後幾句（例如色盤在檔尾才失敗）可能在 Shutdown 之後才收到
        self.drain();
        if let Some(f) = self.failed() {
            return Err(f);
        }
        match ended {
            Some(f) => Err(f),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_size_cases() {
        let size = |d: (f64, f64), rotate: u32, long: u32| out_size(d, rotate, long);
        // 16:9
        assert_eq!(
            size((1920.0, 1080.0), 0, 480),
            Sizes {
                scale: (480, 270),
                out: (480, 270)
            }
        );
        // 手機直拍（檔案標示旋轉 90°，解出來是橫的 1920×1080）：GIF 是直的，縮放在轉正之前
        assert_eq!(
            size((1920.0, 1080.0), 90, 480),
            Sizes {
                scale: (480, 270),
                out: (270, 480)
            }
        );
        assert_eq!(size((1080.0, 1920.0), 0, 480).out, (270, 480));
        // DVD 的 720×480、16:9（顯示寬度 853）
        assert_eq!(size((853.0, 480.0), 0, 480).out, (480, 270));
        // 一律偶數（4:3 的 320 → 240；奇數高度往最近的偶數）
        assert_eq!(size((4.0, 3.0), 180, 320).out, (320, 240));
        assert_eq!(size((2.39, 1.0), 0, 320).out, (320, 134));
        for long in crate::export::GIF_LONG_SIDES {
            for d in [(1920.0, 1080.0), (1000.0, 999.0), (21.0, 9.0), (1.0, 3.0)] {
                for rotate in [0, 90, 180, 270] {
                    let s = size(d, rotate, long);
                    assert!(s.out.0 % 2 == 0 && s.out.1 % 2 == 0, "{d:?} {rotate} {s:?}");
                    assert_eq!(s.out.0.max(s.out.1), long, "{d:?} {rotate} {s:?}");
                }
            }
        }
        // 很細長的畫面：短邊至少 2
        assert_eq!(size((1000.0, 1.0), 0, 320).out, (320, 2));
    }

    #[test]
    fn container_pixel_aspect() {
        // 影像資料是方形像素的 4:3、容器標示像素寬 2 倍（MP4 的 pasp、MKV 的顯示大小）：畫面上是 8:3
        assert!((container_aspect(4.0 / 3.0, Some(1.0), Some(2.0)) - 8.0 / 3.0).abs() < 1e-9);
        // DVD 的 720×480（影像資料標示 32:27）、容器也一樣：不變
        let dvd = 16.0 / 9.0;
        assert!((container_aspect(dvd, Some(32.0 / 27.0), Some(32.0 / 27.0)) - dvd).abs() < 1e-9);
        // 容器沒有標示、讀不到、不合理：照影像資料的
        for (dec, container) in [(Some(1.0), None), (Some(2.0), Some(0.0)), (Some(1.0), Some(f64::NAN))] {
            assert_eq!(container_aspect(1.5, dec, container), 1.5, "{dec:?} {container:?}");
        }
        // 影像資料沒有標示像素比例（mpv 當成方形）：照容器的
        for dec in [None, Some(0.0), Some(f64::NAN)] {
            assert_eq!(container_aspect(1.5, dec, Some(2.0)), 3.0, "{dec:?}");
        }
    }

    fn graph() -> Graph {
        Graph {
            fps: 15,
            scale: (480, 270),
            tone: None,
            fixup: Fixup::default(),
            transpose: true,
            palette: PaletteMode::Global,
            subtitles: false,
        }
    }

    #[test]
    fn graph_strings() {
        let g = graph();
        // 格率取「那一刻畫面上的那一格」（round=up）：第一格就是 A 那一格
        let picture = "fps=fps=15:round=up,scale=480:270:flags=lanczos";
        let palette = "split[a][b];[a]palettegen=max_colors=256:stats_mode=diff[p];\
                       [b][p]paletteuse=dither=sierra2_4a:diff_mode=rectangle";
        assert_eq!(g.picture(), picture);
        let all = format!("{picture},{palette}");
        assert_eq!(g.vf(), format!("lavfi=graph=%{}%{all}", all.len()));
        // 從 A 前面讀：格率從 EDL 的 A 算起（A 之前的畫面在這裡就丟掉，不進色盤）
        assert_eq!(g.picture_from(None), picture);
        let trimmed = "fps=fps=15:start_time=2.988333:round=up,scale=480:270:flags=lanczos";
        assert_eq!(g.picture_from(Some(2.988_333_3)), trimmed);
        let all = format!("{trimmed},{palette}");
        assert_eq!(
            g.vf_from(Some(2.988_333_3)),
            format!("lavfi=graph=%{}%{all}", all.len())
        );
        let with_subs = Graph {
            subtitles: true,
            ..graph()
        };
        assert_eq!(
            with_subs.vf_from(Some(2.988_333_3)),
            format!(
                "lavfi=graph=%{}%{trimmed},sub,lavfi=graph=%{}%{palette}",
                trimmed.len(),
                palette.len()
            )
        );
        // 轉 90° + 左右翻轉：先轉再翻（跟畫面上一樣）
        let g = Graph {
            fixup: Fixup {
                rotate: 90,
                hflip: true,
                vflip: false,
            },
            ..graph()
        };
        assert_eq!(g.picture(), format!("{picture},transpose=clock,hflip"));
        let turn = |rotate: u32, transpose: bool| {
            Graph {
                fixup: Fixup {
                    rotate,
                    ..Default::default()
                },
                transpose,
                ..graph()
            }
            .picture()
        };
        assert_eq!(turn(270, true), format!("{picture},transpose=cclock"));
        assert_eq!(turn(180, true), format!("{picture},hflip,vflip"));
        // 舊的系統 libmpv 沒有 transpose：用 rotate
        assert_eq!(turn(90, false), format!("{picture},rotate=PI/2:ow=ih:oh=iw"));
        assert_eq!(turn(270, false), format!("{picture},rotate=-PI/2:ow=ih:oh=iw"));
        // 上下翻轉
        let g = Graph {
            fixup: Fixup {
                rotate: 0,
                hflip: false,
                vflip: true,
            },
            ..graph()
        };
        assert_eq!(g.picture(), format!("{picture},vflip"));
        // HDR：轉成線性、BT.709、色調映射，再轉回一般畫面
        let g = Graph {
            tone: Some(Tone {
                curve: "mobius",
                npl: 150,
            }),
            ..graph()
        };
        let p = g.picture();
        assert!(
            p.starts_with(&format!(
                "{picture},zscale=t=linear:npl=150,format=gbrpf32le,zscale=p=bt709,"
            )),
            "{p}"
        );
        assert!(p.contains("tonemap=tonemap=mobius:desat=0"), "{p}");
        assert!(p.ends_with("zscale=t=bt709:m=bt709:r=tv,format=yuv444p"), "{p}");
        // 每格一個色盤
        let g = Graph {
            palette: PaletteMode::PerFrame,
            ..graph()
        };
        assert!(g.palette().contains("stats_mode=single") && g.palette().contains("new=1"));
        assert!(!g.palette().contains("diff_mode"));
        // 字幕：在畫面處理（縮放、轉正）之後、色盤之前燒進去，兩段各自是一個濾鏡圖
        let g = Graph {
            subtitles: true,
            ..graph()
        };
        assert_eq!(
            g.vf(),
            format!(
                "lavfi=graph=%{}%{picture},sub,lavfi=graph=%{}%{palette}",
                picture.len(),
                palette.len()
            )
        );
    }

    #[test]
    fn palette_mode_budget() {
        // 480×270、15 fps、30 秒：約 233 MB，整段一個色盤
        assert_eq!(palette_mode((480, 270), frame_count(30.0, 15)), PaletteMode::Global);
        // 800×450、25 fps、30 秒：約 1.08 GB，每格一個
        assert_eq!(palette_mode((800, 450), frame_count(30.0, 25)), PaletteMode::PerFrame);
        assert_eq!(frame_count(1.5, 10), 15);
        assert_eq!(frame_count(0.21, 10), 3);
        assert_eq!(frame_count(0.0, 10), 1);
    }

    #[test]
    fn edl_url_escapes() {
        // 逗號、分號、百分比、中文：用位元組長度標示，不會被 EDL 拆開
        let path = "C:/影片/第1集,a;b%c.mkv";
        let url = edl_url(path, 12.5, 3.25);
        let len = path.len();
        assert_eq!(len, 27, "位元組數（中文一個字 3 位元組）");
        assert_eq!(
            url,
            format!("edl://!no_chapters;%{len}%{path},start=12.500000,length=3.250000")
        );
        // 網址一樣
        let url = edl_url("https://x/a.mp4?x=1,2", 0.0, 1.0);
        assert_eq!(
            url,
            "edl://!no_chapters;%21%https://x/a.mp4?x=1,2,start=0.000000,length=1.000000"
        );
    }

    fn track(id: i64, kind: TrackKind, external: Option<&str>) -> Track {
        Track {
            id,
            kind,
            title: None,
            lang: None,
            codec: Some("subrip".into()),
            selected: false,
            external: external.is_some(),
            external_filename: external.map(str::to_owned),
            default: false,
            forced: false,
            albumart: false,
            width: None,
            height: None,
            channels: None,
            samplerate: None,
            dolby_vision_profile: None,
        }
    }

    #[test]
    fn external_sub_delay() {
        let tracks = [
            track(1, TrackKind::Video, None),
            track(1, TrackKind::Sub, None),
            track(2, TrackKind::Sub, None),
            track(3, TrackKind::Sub, Some("/tmp/a.utf8.srt")),
        ];
        // 內嵌的字幕：EDL 自己換算時間，延遲照舊；編號是內嵌字幕裡排第幾
        let embedded = GifSub::of(&tracks, &tracks[2]).unwrap();
        assert!(matches!(&embedded, GifSub::Embedded(p) if p.ordinal == 2));
        assert_eq!(embedded.predicted_id(), 2);
        assert_eq!(embedded.delay(0.5, 60.0), 0.5);
        // 外掛的字幕：照播放時間走，EDL 的 0 秒是 A 秒，延遲要減掉 A；編號排在內嵌的後面
        let external = GifSub::of(&tracks, &tracks[3]).unwrap();
        assert_eq!(
            external,
            GifSub::External {
                path: "/tmp/a.utf8.srt".into(),
                embedded: 2
            }
        );
        assert_eq!(external.predicted_id(), 3);
        assert_eq!(external.delay(0.5, 60.0), -59.5);
        // 編碼用的 mpv 裡找對應的軌道
        assert_eq!(external.find(&tracks), Some(3));
        assert_eq!(embedded.find(&tracks), Some(2));
        assert_eq!(external.find(&tracks[..3]), None);
        // 不是字幕：None
        assert_eq!(GifSub::of(&tracks, &tracks[0]), None);
    }

    #[test]
    fn hdr_plans() {
        let tone = ToneSettings::default();
        // 一般影片：不轉
        assert_eq!(hdr_plan(false, None, true, &tone), (None, None));
        // HDR：自動 = Hable、203 nits
        assert_eq!(
            hdr_plan(true, None, true, &tone),
            (
                Some(Tone {
                    curve: "hable",
                    npl: 203
                }),
                None
            )
        );
        // 目標亮度、曲線跟「畫質 → HDR」一樣
        let custom = ToneSettings {
            curve: ToneCurve::Reinhard,
            target_peak: Some(100),
            ..ToneSettings::default()
        };
        assert_eq!(
            hdr_plan(true, Some(8), true, &custom).0,
            Some(Tone {
                curve: "reinhard",
                npl: 100
            }),
            "杜比視界 Profile 8 有 HDR10 的底層，照常轉"
        );
        // 杜比視界 Profile 5：不轉（zimg 轉不了 IPT），附說明
        assert_eq!(hdr_plan(true, Some(5), true, &tone), (None, Some(Note::DolbyVision5)));
        assert_eq!(hdr_plan(false, Some(5), true, &tone), (None, Some(Note::DolbyVision5)));
        // 沒有 zscale、tonemap：不轉，附說明
        assert_eq!(hdr_plan(true, None, false, &tone), (None, Some(Note::HdrClipped)));
        // FFmpeg 的 tonemap 沒有 BT.2390
        assert_eq!(tone_curve(ToneCurve::Bt2390), "hable");
        assert_eq!(tone_curve(ToneCurve::Auto), "hable");
        for c in ToneCurve::ALL {
            assert!(
                ["hable", "mobius", "reinhard", "clip", "linear"].contains(&tone_curve(c)),
                "{c:?}"
            );
        }
    }

    #[test]
    fn length_limits() {
        assert_eq!(check_length(30.0), Ok(()));
        assert_eq!(check_length(0.2), Ok(()));
        assert_eq!(check_length(30.1), Err(Failure::GifTooLong));
        assert_eq!(check_length(0.1), Err(Failure::RangeTooShort));
        assert_eq!(check_length(0.0), Err(Failure::NoData));
        assert_eq!(check_length(f64::NAN), Err(Failure::NoData));
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(Failure::GifTooLong.message(), "GIF 最長 30 秒，請縮短 A-B 段落");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(
            Failure::GifTooLong.message(),
            "GIFs can be at most 30 seconds; shorten the A-B range"
        );
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }

    /// 最小的 GIF：w×h、`frames` 格、每格停留 `delay` 百分之一秒（1 色、1×1 的區塊）
    fn tiny_gif(w: u16, h: u16, frames: usize, delay: u16) -> Vec<u8> {
        let mut v = b"GIF89a".to_vec();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&[0x80, 0, 0]); // 全域色盤 2 色
        v.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        // 迴圈（NETSCAPE2.0）
        v.extend_from_slice(&[0x21, 0xFF, 11]);
        v.extend_from_slice(b"NETSCAPE2.0");
        v.extend_from_slice(&[3, 1, 0, 0, 0]);
        for _ in 0..frames {
            v.extend_from_slice(&[0x21, 0xF9, 4, 0]);
            v.extend_from_slice(&delay.to_le_bytes());
            v.extend_from_slice(&[0, 0]);
            v.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
            v.extend_from_slice(&[2, 2, 0x44, 0x01, 0]);
        }
        v.push(0x3b);
        v
    }

    #[test]
    fn gif_info_counts_frames_delays_and_size() {
        let data = tiny_gif(320, 180, 15, 10);
        assert_eq!(
            gif_info(&data),
            Some(GifInfo {
                width: 320,
                height: 180,
                frames: 15,
                delay_cs: 150
            })
        );
        assert!((gif_info(&data).unwrap().seconds() - 1.5).abs() < 1e-9);
        // 沒有結尾、不是 GIF：None（不會 panic）
        assert_eq!(gif_info(&data[..data.len() - 1]), None);
        assert_eq!(gif_info(&data[..40]), None);
        assert_eq!(gif_info(b"PNG"), None);
        assert_eq!(gif_info(&[]), None);
        let mut bad = data.clone();
        bad[13 + 6] = 0x99; // 不認得的區塊
        assert_eq!(gif_info(&bad), None);
    }

    #[test]
    fn written_gif_is_checked() {
        let slack = length_slack(1.5, 10, Some(24.0), false);
        assert!(check_gif(&tiny_gif(320, 180, 15, 10), (320, 180), 1.5, slack).is_ok());
        // 大小不對：濾鏡沒有套用（mpv 停用了失敗的濾鏡）
        assert_eq!(
            check_gif(&tiny_gif(1920, 1080, 15, 10), (320, 180), 1.5, slack),
            Err(Failure::FilterFailed)
        );
        // 太短：範圍讀不到資料
        assert!(matches!(
            check_gif(
                &tiny_gif(320, 180, 2, 10),
                (320, 180),
                10.0,
                length_slack(10.0, 10, None, false)
            ),
            Err(Failure::TooShort { .. })
        ));
        // 沒有影格、不完整
        assert_eq!(
            check_gif(&tiny_gif(320, 180, 0, 10), (320, 180), 1.5, slack),
            Err(Failure::NoData)
        );
        let data = tiny_gif(320, 180, 3, 10);
        assert_eq!(
            check_gif(&data[..data.len() - 1], (320, 180), 0.3, slack),
            Err(Failure::Unplayable(None))
        );
    }

    #[test]
    fn a_late_start_is_too_short() {
        // 1 秒、10 fps、24 fps 的影片：頭尾對齊少一格（0.9 秒）沒關係
        let slack = length_slack(1.0, 10, Some(24.0), false);
        assert!((slack - (0.2 + 2.0 / 24.0)).abs() < 1e-9, "{slack}");
        assert!(check_gif(&tiny_gif(320, 240, 9, 10), (320, 240), 1.0, slack).is_ok());
        // 開頭晚了半秒（關鍵影格隔 0.5 秒的 TS，跳轉落在 A 之後的關鍵影格）：太短
        assert_eq!(
            check_gif(&tiny_gif(320, 240, 5, 10), (320, 240), 1.0, slack),
            Err(Failure::TooShort { got: 0.5, want: 1.0 })
        );
        // 30 秒的 GIF 少 1 秒（以前的標準看不出來）
        let slack = length_slack(30.0, 15, Some(30.0), false);
        assert!(matches!(
            check_gif(&tiny_gif(320, 240, 290, 10), (320, 240), 30.0, slack),
            Err(Failure::TooShort { .. })
        ));
        // 範圍碰到檔案的頭尾：影像可能比聲音晚開始、早結束，多給 1 秒
        let edge = length_slack(30.0, 15, Some(30.0), true);
        assert!(check_gif(&tiny_gif(320, 240, 290, 10), (320, 240), 30.0, edge).is_ok());
        // 格率很低的影片（每秒一格的投影片）：影片本身的一格就很長
        let slow = length_slack(2.5, 10, Some(1.0), false);
        assert!(check_gif(&tiny_gif(320, 240, 19, 10), (320, 240), 2.5, slow).is_ok());
        // 不知道影片的格率：照舊寬鬆（最多 1 秒、一半以內）
        assert_eq!(length_slack(10.0, 10, None, false), 1.0);
        assert_eq!(length_slack(1.0, 10, None, false), 0.5);
        assert_eq!(length_slack(1.0, 10, Some(f64::NAN), false), 0.5);
        assert_eq!(length_slack(1.0, 10, Some(0.0), true), 1.5);
    }

    #[test]
    fn approximate_seek_formats() {
        // TS、M2TS、MTS（mpegts）、MPEG-PS／VOB（mpeg）：FFmpeg 用時間戳搜尋
        for f in ["mpegts", "mpeg"] {
            assert!(approximate_seeks(f), "{f}");
        }
        // 有索引的格式：跳轉落在目標之前的關鍵影格
        for f in [
            "mkv",
            "mov,mp4,m4a,3gp,3g2,mj2",
            "avi",
            "matroska,webm",
            "hls",
            "",
            "edl/mpegts",
        ] {
            assert!(!approximate_seeks(f), "{f}");
        }
    }

    #[test]
    fn plans_for_each_lead() {
        let edl = |start: f64, length: f64| edl_url("/v/a.ts", start, length);
        // 跳轉準的格式：EDL 從 A 開始（影片的時間戳 = A + 開始時間），不另外跳
        let p = Plan::new("/v/a.ts", 10.0, 12.0, 1.4, Lead::None);
        assert_eq!(p.url, edl(11.4, 2.0));
        assert_eq!((p.start, p.zero, p.a_at()), (None, 10.0, 0.0));
        // 從 A 前面 5 秒開始：精確跳到 EDL 的 5 秒（A），分離器跳到 EDL 的開頭（試跳過的位置）
        let p = Plan::new("/v/a.ts", 10.0, 12.0, 1.4, Lead::Secs(5.0));
        assert_eq!(p.url, edl(6.4, 7.0));
        assert_eq!(
            (p.start, p.demuxer_offset, p.zero, p.a_at()),
            (Some(5.0), 5.0, 5.0, 5.0)
        );
        // 從檔案開頭：EDL 從時間戳 0 開始，A 在 EDL 的 A + 開始時間；分離器跳到開頭之前
        let p = Plan::new("/v/a.ts", 10.0, 12.0, 1.4, Lead::FromStart);
        assert_eq!(p.url, edl(0.0, 13.4));
        assert_eq!(p.start, Some(11.4));
        assert!(
            (p.demuxer_offset - 12.4).abs() < 1e-9 && (p.zero + 1.4).abs() < 1e-9,
            "{p:?}"
        );
        // 往前讀超過檔案開頭：從開頭讀
        assert_eq!(
            Plan::new("/v/a.ts", 1.5, 2.5, 1.462, Lead::Secs(5.0)),
            Plan::new("/v/a.ts", 1.5, 2.5, 1.462, Lead::FromStart)
        );
        // 往前 30 秒
        let p = Plan::new("/v/a.ts", 100.0, 101.5, 1.4, Lead::Secs(30.0));
        assert_eq!(p.url, edl(71.4, 31.5));
        assert_eq!(
            (p.start, p.demuxer_offset, p.zero, p.a_at()),
            (Some(30.0), 30.0, 70.0, 30.0)
        );
        assert_eq!(
            Plan::new("/v/a.ts", 20.0, 21.0, 1.4, Lead::Secs(30.0)),
            Plan::new("/v/a.ts", 20.0, 21.0, 1.4, Lead::FromStart)
        );
        // 格率從 A（加一點誤差）算起；EDL 從 A 開始時不用
        assert_eq!(
            Plan::new("/v/a.ts", 100.0, 101.5, 1.4, Lead::Secs(30.0)).trim(),
            Some(30.005)
        );
        let p = Plan::new("/v/a.ts", 1.5, 2.5, 1.462, Lead::FromStart);
        assert!((p.trim().unwrap() - (2.962 + HR_SEEK_TOLERANCE)).abs() < 1e-9, "{p:?}");
        assert_eq!(Plan::new("/v/a.mkv", 1.5, 2.5, 0.0, Lead::None).trim(), None);
    }

    #[test]
    fn grid_shift_aligns_burned_subtitles() {
        // fps 從 2.988 秒算起、每秒 10 格：送出的第一格標 3.0 秒，畫面是 2.988 秒的，字幕延遲多 0.012 秒
        assert!((grid_shift(2.988, 10) - 0.012).abs() < 1e-9);
        // 剛好在格線上：不用
        assert!(grid_shift(5.0, 10).abs() < 1e-9);
        assert!(grid_shift(5.0, 15).abs() < 1e-9);
        // 最多差一格
        for t in [0.001, 1.234, 7.77, 100.05] {
            for fps in [5, 10, 15, 25] {
                let d = grid_shift(t, fps);
                assert!((0.0..1.0 / f64::from(fps) + 1e-9).contains(&d), "{t} {fps} {d}");
                assert!(
                    ((t + d) * f64::from(fps))
                        .fract()
                        .min(1.0 - ((t + d) * f64::from(fps)).fract())
                        < 1e-6
                );
            }
        }
    }

    #[test]
    fn lead_in_tries_each_step() {
        // 依序試跳 A − 5、A − 30，記下試了哪裡
        let run = |a: f64, main_start: f64, land: &dyn Fn(f64) -> Option<f64>| {
            let mut tried = Vec::new();
            let lead = lead_in(a, main_start, |at| {
                tried.push(at);
                Ok(land(at))
            })
            .unwrap();
            (lead, tried)
        };
        // A − 5 落在 A 之前：從 A 前面 5 秒讀
        assert_eq!(run(100.0, 1.4, &|at| Some(at + 2.0)), (Lead::Secs(5.0), vec![95.0]));
        // 落點剛好是 A（差不到誤差）也可以
        assert_eq!(run(100.0, 1.4, &|_| Some(100.004)).0, Lead::Secs(5.0));
        // A − 5 落在 A 之後、A − 30 落在 A 之前：從 A 前面 30 秒讀
        let late_then_early = |at: f64| Some(if at > 90.0 { 110.0 } else { at + 20.0 });
        assert_eq!(run(100.0, 1.4, &late_then_early), (Lead::Secs(30.0), vec![95.0, 70.0]));
        // A − 5 落到檔尾、A − 30 落在 A 之前
        let eof_then_early = |at: f64| if at > 90.0 { None } else { Some(at) };
        assert_eq!(run(100.0, 1.4, &eof_then_early).0, Lead::Secs(30.0));
        // 都落在 A 之後或檔尾：從頭讀
        assert_eq!(
            run(100.0, 1.4, &|at| if at > 90.0 { Some(101.0) } else { None }),
            (Lead::FromStart, vec![95.0, 70.0])
        );
        // A − 30 已經在檔案開頭之前：不用再試，從頭讀
        assert_eq!(run(15.0, 1.4, &|_| None), (Lead::FromStart, vec![10.0]));
        // A − 5 就在開頭之前：一次都不試
        assert_eq!(run(3.0, 1.4, &|_| Some(0.0)), (Lead::FromStart, vec![]));
        // 開始時間讓 A − 5 還在檔案裡（TS 從 1.4 秒開始）
        assert_eq!(run(4.0, 1.4, &|at| Some(at)), (Lead::Secs(5.0), vec![-1.0]));
        // 試跳失敗（取消、讀不到）：照實回報
        assert_eq!(
            lead_in(100.0, 0.0, |_| Err(Failure::Cancelled)),
            Err(Failure::Cancelled)
        );
    }

    #[test]
    fn progress_skips_the_lead_in() {
        // 從頭讀、A 在 EDL 的 16.48 秒、1 秒的 GIF：前面多讀的部分算 0，還在讀
        assert_eq!(read_progress(Some(5.0), false, 16.48, 1.0), (0.0, true));
        let (f, reading) = read_progress(Some(16.98), false, 16.48, 1.0);
        assert!((f - 0.5).abs() < 1e-6 && reading, "{f}");
        // 讀到 B 前面一點、讀完：不算在讀了（之後是色盤）
        assert!(!read_progress(Some(17.4), false, 16.48, 1.0).1);
        assert_eq!(read_progress(Some(5.0), true, 16.48, 1.0), (0.0, false));
        // 還不知道讀到哪裡
        assert_eq!(read_progress(None, false, 16.48, 1.0), (0.0, true));
        // EDL 從 A 開始
        assert!(!read_progress(Some(3.0), false, 0.0, 2.0).1);
        assert_eq!(read_progress(Some(1.0), false, 0.0, 2.0), (0.5, true));
    }

    #[test]
    fn external_sub_delay_follows_the_edl_start() {
        let tracks = [
            track(1, TrackKind::Video, None),
            track(1, TrackKind::Sub, None),
            track(2, TrackKind::Sub, Some("/tmp/a.srt")),
        ];
        let spec = |sub: &Track| GifSpec {
            source: Source::File("/v/a.ts".into()),
            a: 10.0,
            b: 12.0,
            main_start: 1.4,
            video: None,
            graph: graph(),
            out: (480, 270),
            sub: GifSub::of(&tracks, sub),
            sub_delay: GifSub::of(&tracks, sub).unwrap().delay(0.25, 10.0),
            approx_seek: true,
            slack: 0.3,
            sub_options: Vec::new(),
            deinterlace: "no",
            dir: PathBuf::new(),
            stem: String::new(),
            cache_dir: PathBuf::new(),
            notes: Vec::new(),
            test: TestHooks::default(),
        };
        // 外掛的字幕：EDL 從 A 前面 5 秒開始，0 秒是主播放器的 5 秒，延遲減 5 秒（不是減 A）
        let ext = spec(&tracks[2]);
        assert_eq!(ext.sub_delay, 0.25 - 10.0);
        assert_eq!(ext.sub_delay_from(10.0), 0.25 - 10.0);
        assert_eq!(ext.sub_delay_from(5.0), 0.25 - 5.0);
        let from_start = Plan::new("/v/a.ts", 10.0, 12.0, 1.4, Lead::FromStart);
        assert!((ext.sub_delay_from(from_start.zero) - (0.25 + 1.4)).abs() < 1e-9);
        // 內嵌的字幕：EDL 自己換算，延遲照舊
        let emb = spec(&tracks[1]);
        assert_eq!(emb.sub_delay_from(5.0), 0.25);
    }

    #[test]
    fn filter_failures_in_the_log() {
        let line = |level: &str, text: &str| LogLine::new(level, "vf", text);
        assert!(graph_failed(&[line(
            "error",
            "Disabling filter lavfi.00 because it has failed."
        )]));
        assert!(graph_failed(&[line(
            "fatal",
            "Cannot convert decoder/filter output to any format supported by the output."
        )]));
        assert!(!graph_failed(&[line(
            "warn",
            "Disabling filter x because it has failed."
        )]));
        assert!(!graph_failed(&[line("error", "Something else.")]));
        assert!(!graph_failed(&[]));
    }

    #[test]
    fn a_failed_filter_still_fails_the_gif_after_a_log_flood() {
        let mut log = LogTail::default();
        assert_eq!(gif_failure(&log), None);
        log.push(LogLine::new(
            "error",
            "vf",
            "Disabling filter lavfi.00 because it has failed.",
        ));
        // 之後收到一大堆別的警告、錯誤（別的 mpv 的 FFmpeg 記錄），最近幾行裡已經沒有它
        for i in 0..(LogTail::CAP * 2) {
            log.push(LogLine::new("warn", "ffmpeg/video", &format!("noise {i}")));
        }
        assert!(!graph_failed(&log.lines()));
        assert_eq!(gif_failure(&log), Some(Failure::FilterFailed));
        // 只有不相關的錯誤：不算 GIF 失敗；寫不進去才算
        let mut log = LogTail::default();
        log.push(LogLine::new("error", "cplayer", "Something else."));
        assert_eq!(gif_failure(&log), None);
        log.push(LogLine::new("error", "encode", "Failed writing packet."));
        assert_eq!(gif_failure(&log), Some(Failure::WriteFailed));
    }
}
