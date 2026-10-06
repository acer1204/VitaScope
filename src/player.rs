//! 播放器核心：把 mpv 的屬性與事件整理成 Rust 狀態。
//! 介面（app）和自動測試（tests/）都透過這一層操作 mpv。

use crate::mpv::{self, EndReason, Event, Format, Mpv, Value};
use crate::subs::{self, ExternalSub, SubLang};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 建立播放器的設定。
pub struct Options {
    /// true = 不開視窗、不出聲音（自動測試用）
    pub headless: bool,
    /// 硬體解碼模式：`auto-safe`（預設，失敗自動退回軟解）、`auto-copy`、`no`…
    pub hwdec: String,
    /// 播完停在最後一格（播放器介面用）；false = 播完就卸載檔案（測試用）
    pub keep_open: bool,
    /// 開檔後自動選字幕：繁中優先；沒有字幕被選上時也選一條（比照 PotPlayer）
    pub auto_select_subs: bool,
    /// 外掛字幕自己找、自己判斷編碼和語言（見 `subs` 模組）；false = 交給 mpv 的 sub-auto
    pub external_subs: bool,
    /// 有新事件時呼叫（在 mpv 的執行緒上，只能用來喚醒 UI）
    pub wakeup: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            headless: false,
            hwdec: "auto-safe".into(),
            keep_open: true,
            auto_select_subs: true,
            external_subs: true,
            wakeup: None,
        }
    }
}

impl Options {
    pub fn headless() -> Self {
        Self {
            headless: true,
            hwdec: "no".into(),
            keep_open: false,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Sub,
    #[serde(other)]
    Other,
}

impl TrackKind {
    /// 選擇這類軌道的 mpv 屬性名稱
    fn property(self) -> Option<&'static str> {
        match self {
            TrackKind::Video => Some("vid"),
            TrackKind::Audio => Some("aid"),
            TrackKind::Sub => Some("sid"),
            TrackKind::Other => None,
        }
    }
}

/// `track-list` 裡的一條軌道。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Track {
    pub id: i64,
    #[serde(rename = "type")]
    pub kind: TrackKind,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub external: bool,
    #[serde(default)]
    pub external_filename: Option<String>,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub forced: bool,
    #[serde(default)]
    pub albumart: bool,
    #[serde(default, rename = "demux-w")]
    pub width: Option<i64>,
    #[serde(default, rename = "demux-h")]
    pub height: Option<i64>,
    #[serde(default, rename = "demux-channel-count")]
    pub channels: Option<i64>,
    #[serde(default, rename = "demux-samplerate")]
    pub samplerate: Option<i64>,
}

impl Track {
    /// 選單上顯示的名稱，例如「#2 日本語 (jpn) · aac 2ch」
    pub fn label(&self) -> String {
        let mut s = format!("#{}", self.id);
        // 外掛檔案沒有標題時，mpv 用檔名的一部分當標題（新版是「mka」、「jpn.mka」，舊版是完整檔名）：
        // 這種情況顯示完整檔名，比較認得出來
        let file_name = self
            .external_filename
            .as_deref()
            .and_then(|f| Path::new(f).file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let title = self.title.as_deref().filter(|t| !t.is_empty());
        let title = match (&file_name, title) {
            (Some(name), Some(t)) if name.ends_with(t) => Some(name.as_str()),
            (Some(name), None) => Some(name.as_str()),
            (_, t) => t,
        };
        if let Some(t) = title {
            s += &format!(" {t}");
        }
        if let Some(l) = self.lang.as_deref().filter(|l| !l.is_empty()) {
            s += &format!(" ({l})");
        }
        let mut detail = Vec::new();
        if let Some(c) = &self.codec {
            detail.push(c.clone());
        }
        if let (Some(w), Some(h)) = (self.width, self.height) {
            detail.push(format!("{w}×{h}"));
        }
        if let Some(ch) = self.channels {
            detail.push(format!("{ch}ch"));
        }
        if self.external {
            detail.push(crate::tr!("外掛", "external").into());
        }
        if !detail.is_empty() {
            s += &format!(" · {}", detail.join(" "));
        }
        s
    }
}

/// `chapter-list` 裡的一個章節。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Chapter {
    #[serde(default)]
    pub title: Option<String>,
    /// 開始時間（秒）
    pub time: f64,
}

/// 介面需要的播放狀態，由 mpv 的屬性變化事件即時更新。
#[derive(Debug, Clone, Default)]
pub struct State {
    pub path: Option<String>,
    pub title: Option<String>,
    /// 已送出開檔、還沒載入完成
    pub loading: bool,
    /// 有檔案載入中（FileLoaded 到 EndFile 之間）
    pub loaded: bool,
    pub time_pos: f64,
    pub duration: Option<f64>,
    pub paused: bool,
    pub volume: f64,
    pub muted: bool,
    pub eof: bool,
    pub seekable: bool,
    pub tracks: Vec<Track>,
    /// 實際顯示尺寸（已套用旋轉與像素比例）
    pub video_size: Option<[i64; 2]>,
    /// 目前使用的硬體解碼器；None 或 "no" = 軟解
    pub hwdec: Option<String>,
    /// 最近一次開檔失敗的原因（中文）
    pub last_error: Option<String>,
    /// 播放速度（1.0 = 正常）
    pub speed: f64,
    pub chapters: Vec<Chapter>,
    /// 目前的章節（從 0 開始）；None = 沒有章節，或還沒到第一章
    pub chapter: Option<usize>,
    /// A-B 重播的起點、終點（秒）
    pub ab_loop: [Option<f64>; 2],
    /// 字幕延遲（秒，正數 = 字幕晚一點出現）
    pub sub_delay: f64,
    /// 音訊延遲（秒，正數 = 聲音晚一點）
    pub audio_delay: f64,
    /// 主字幕、第二字幕的軌道編號（直接看 mpv 的 sid / secondary-sid，
    /// 開了第二字幕時 track-list 的 selected 兩條都是 true，分不出哪條是主字幕）
    pub sid: Option<i64>,
    pub secondary_sid: Option<i64>,
    /// 標籤（歌名、演出者、專輯…），鍵是 mpv 整理過的名稱：Title、Artist、Album…
    pub metadata: std::collections::BTreeMap<String, String>,
}

impl State {
    pub fn tracks_of(&self, kind: TrackKind) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(move |t| t.kind == kind)
    }

    pub fn selected(&self, kind: TrackKind) -> Option<&Track> {
        if kind == TrackKind::Sub
            && let Some(id) = self.sid
        {
            return self.tracks_of(kind).find(|t| t.id == id);
        }
        // 第二字幕也是 selected；軌道編號各類分開算，所以只有字幕要排除第二字幕
        self.tracks_of(kind)
            .find(|t| t.selected && (kind != TrackKind::Sub || Some(t.id) != self.secondary_sid))
    }

    /// 有實際影像（不是只有專輯封面）
    pub fn has_video(&self) -> bool {
        self.tracks_of(TrackKind::Video).any(|t| !t.albumart)
    }

    /// 讀取標籤（不分大小寫），例如 `tag("artist")`
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.metadata
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.trim().is_empty())
    }

    /// 某個時間點所在的章節（從 0 開始）。跳到章節後顯示的影格時間常常比章節時間早一點點
    /// （MKV 的時間只到毫秒），所以留一點誤差
    pub fn chapter_at(&self, time: f64) -> Option<usize> {
        self.chapters.iter().rposition(|c| c.time <= time + 0.005)
    }
}

/// 介面關心的播放事件（屬性變化已經反映在 `State`，不另外回報）。
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    StartFile,
    FileLoaded,
    PlaybackRestart,
    VideoReconfig,
    Seek,
    EndFile {
        reason: EndReason,
        error: Option<String>,
    },
    /// 非同步指令（例如截圖）完成；`error` 是失敗的原因
    CommandReply {
        id: u64,
        error: Option<String>,
    },
    Shutdown,
}

const OBSERVED: &[(&str, Format)] = &[
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("volume", Format::Double),
    ("mute", Format::Flag),
    ("track-list", Format::String),
    ("media-title", Format::String),
    ("path", Format::String),
    ("eof-reached", Format::Flag),
    ("seekable", Format::Flag),
    ("hwdec-current", Format::String),
    // 不用 dwidth/dheight：GPU 負責旋轉時（GUI 的情況），它們是旋轉前的尺寸
    ("video-out-params", Format::String),
    ("speed", Format::Double),
    ("chapter-list", Format::String),
    ("chapter", Format::Int64),
    // 沒設定時是 "no"，所以用字串讀
    ("ab-loop-a", Format::String),
    ("ab-loop-b", Format::String),
    ("sub-delay", Format::Double),
    ("audio-delay", Format::Double),
    ("sid", Format::String),
    ("secondary-sid", Format::String),
    ("filtered-metadata", Format::String),
];

/// VITASCOPE_DEBUG 的值 → 要 mpv 送出的記錄等級。
/// 沒設定只收錯誤；1 之類的值 = 警告與錯誤（印到 stderr，排查顯示卡、驅動之類的問題）；
/// 也可以直接寫 mpv 的記錄等級，例如 VITASCOPE_DEBUG=v。看不懂的值當成 1，除錯設定不能讓播放器開不起來
fn debug_log_level(value: Option<&str>) -> &'static str {
    const LEVELS: [&str; 7] = ["fatal", "error", "warn", "info", "v", "debug", "trace"];
    match value {
        None => "error",
        Some(v) => LEVELS.into_iter().find(|level| *level == v).unwrap_or("warn"),
    }
}

/// 一條載入過的外掛字幕
#[derive(Debug, Clone)]
pub struct LoadedSub {
    /// 交給 mpv 的檔案（文字字幕是轉成 UTF-8 的暫存檔）
    pub load_path: String,
    pub original: PathBuf,
    /// 讀取時用的編碼（自動判斷或使用者指定）；圖形字幕是 None
    pub encoding: Option<&'static str>,
    /// 第一次載入時自動判斷出的編碼
    pub detected: Option<&'static str>,
    /// 編碼是使用者指定的（不是自動判斷）
    pub forced: bool,
}

/// 加入外掛字幕的方式（mpv sub-add 的旗標）
#[derive(Debug, Clone, Copy)]
enum AddMode {
    /// 加入但不選（自動載入的外掛字幕，由選字幕的規則決定）
    Auto,
    /// 加入並選上
    Select,
    /// 選上；同一個檔案已經加入過就選那一條，不重複加入
    SelectExisting,
}

impl AddMode {
    fn flag(self) -> &'static str {
        match self {
            AddMode::Auto => "auto",
            AddMode::Select => "select",
            AddMode::SelectExisting => "cached",
        }
    }
}

/// 延遲以毫秒為單位，連按 ±0.1 秒不會累積出 0.30000000000000004 這種數字
fn round_ms(seconds: f64) -> f64 {
    (seconds * 1000.0).round() / 1000.0
}

/// 換檔時還原的畫面選項（翻轉用的是 vf 與 glsl-shaders）
const GEOMETRY_OPTIONS: &str =
    "video-aspect-override,video-crop,video-rotate,video-zoom,video-pan-x,video-pan-y,panscan,vf,glsl-shaders";

/// 畫面輸出收到的影格參數（`video-out-params`）
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct OutParams {
    /// 影格大小（裁切、旋轉之前）
    #[serde(default)]
    pub w: i64,
    #[serde(default)]
    pub h: i64,
    /// 顯示大小（已套用裁切與長寬比，旋轉之前）
    pub dw: i64,
    pub dh: i64,
    /// 畫面輸出還要做的旋轉（順時針）
    #[serde(default)]
    pub rotate: i64,
}

pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 4.0;

/// 字幕語言偏好：繁中優先，其次中文，再來英文
const SUB_LANGS: &str = "zh-TW,zh-Hant,cht,tc,zht,zh-HK,zh,chi,zho,en,eng";

pub struct Player {
    mpv: Arc<Mpv>,
    pub state: State,
    /// 這次開檔期間 mpv 回報的錯誤訊息（開檔失敗時拿來說明原因）
    recent_errors: Vec<String>,
    /// 這次開檔載入的外掛字幕（轉碼後的檔案 ↔ 原始檔）
    loaded_subs: Vec<LoadedSub>,
    auto_select_subs: bool,
    external_subs: bool,
    /// 最近一次開檔失敗的基本原因。mpv 的記錄訊息要等一般事件都取完才會送出，
    /// 常常比 EndFile 晚到，所以失敗後收到的錯誤記錄還要補進說明裡
    failure: Option<String>,
}

impl Player {
    pub fn new(opts: Options) -> mpv::Result<Self> {
        let keep_open = if opts.keep_open { "yes" } else { "no" };
        let mut options: Vec<(&str, &str)> = vec![
            ("vo", if opts.headless { "null" } else { "libmpv" }),
            ("hwdec", &opts.hwdec),
            ("keep-open", keep_open),
            ("idle", "yes"),
            // 介面和按鍵都由我們處理
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            // 外掛字幕預設由 subs 模組處理（mpv 對 GBK / UTF-16 字幕常猜錯編碼）
            ("sub-auto", if opts.external_subs { "no" } else { "fuzzy" }),
            ("slang", SUB_LANGS),
            // 網站影片（yt-dlp）屬於 L3，先關掉避免開檔時意外呼叫外部程式
            ("ytdl", "no"),
            ("audio-client-name", "VitaScope"),
            // 畫面調整（長寬比、裁切、縮放、旋轉、翻轉）每個檔案各自的：換檔時 mpv 自動還原，不會閃一下
            ("reset-on-next-file", GEOMETRY_OPTIONS),
            // 截圖：8 位元 PNG（10 位元影片預設會存 16 位元，檔案大、壓縮慢）、壓縮快一點
            ("screenshot-format", "png"),
            ("screenshot-high-bit-depth", "no"),
            ("screenshot-png-compression", "3"),
        ];
        if opts.headless {
            options.push(("ao", "null"));
        }
        // 排查用：VITASCOPE_MPV_OPTS="名稱=值 名稱=值" 額外指定 mpv 選項（以空白分隔）
        let extra = std::env::var("VITASCOPE_MPV_OPTS").unwrap_or_default();
        options.extend(extra.split_whitespace().filter_map(|kv| kv.split_once('=')));
        let mut mpv = Mpv::new(&options)?;
        if let Some(wakeup) = opts.wakeup {
            mpv.set_wakeup_callback(wakeup);
        }
        mpv.request_log_messages(debug_log_level(std::env::var("VITASCOPE_DEBUG").ok().as_deref()))?;
        for (i, (name, format)) in OBSERVED.iter().enumerate() {
            mpv.observe(i as u64 + 1, name, *format)?;
        }
        Ok(Self {
            mpv: Arc::new(mpv),
            state: State {
                volume: 100.0,
                speed: 1.0,
                ..Default::default()
            },
            recent_errors: Vec::new(),
            loaded_subs: Vec::new(),
            auto_select_subs: opts.auto_select_subs,
            external_subs: opts.external_subs,
            failure: None,
        })
        .inspect(|_| subs::clean_cache())
    }

    pub fn mpv(&self) -> &Arc<Mpv> {
        &self.mpv
    }

    /// 這次開檔以來 mpv 回報的錯誤訊息（最多 8 筆）
    pub fn recent_errors(&self) -> &[String] {
        &self.recent_errors
    }

    // ───────────── 操作 ─────────────

    pub fn open(&mut self, path: &str) -> mpv::Result<()> {
        self.state.last_error = None;
        self.mpv.command(&["loadfile", path, "replace"])
    }

    pub fn stop(&self) -> mpv::Result<()> {
        self.mpv.command(&["stop"])
    }

    pub fn set_pause(&self, paused: bool) -> mpv::Result<()> {
        self.mpv.set_property("pause", paused)
    }

    pub fn toggle_pause(&self) -> mpv::Result<()> {
        // 播完停在最後一格時按播放 = 從頭開始
        if self.state.eof && self.state.paused {
            self.mpv.command(&["seek", "0", "absolute"])?;
        }
        self.mpv.command(&["cycle", "pause"])
    }

    /// 相對跳轉（秒），可為負數
    pub fn seek_relative(&self, seconds: f64) -> mpv::Result<()> {
        self.mpv.command(&["seek", &format!("{seconds}"), "relative"])
    }

    /// 跳到絕對時間（秒）。`exact` = 精準到影格（較慢）；拖曳進度條時用關鍵影格比較順
    pub fn seek_to(&self, seconds: f64, exact: bool) -> mpv::Result<()> {
        let mode = if exact { "absolute+exact" } else { "absolute+keyframes" };
        self.mpv.command(&["seek", &format!("{seconds}"), mode])
    }

    pub fn set_volume(&self, volume: f64) -> mpv::Result<()> {
        self.mpv.set_property("volume", volume.clamp(0.0, 100.0))
    }

    pub fn set_mute(&self, muted: bool) -> mpv::Result<()> {
        self.mpv.set_property("mute", muted)
    }

    /// 播放速度，限制在 0.25×–4×（mpv 預設會保持音調）
    pub fn set_speed(&self, speed: f64) -> mpv::Result<()> {
        self.mpv.set_property("speed", speed.clamp(MIN_SPEED, MAX_SPEED))
    }

    /// 逐格前進 / 後退（會暫停播放）
    pub fn frame_step(&self, forward: bool) -> mpv::Result<()> {
        self.mpv
            .command(&[if forward { "frame-step" } else { "frame-back-step" }])
    }

    /// A-B 重播：第一次設起點、第二次設終點、第三次取消
    pub fn cycle_ab_loop(&self) -> mpv::Result<()> {
        self.mpv.command(&["ab-loop"])
    }

    /// 取消 A-B 重播（mpv 換檔時會沿用，所以開新檔時要清掉）
    pub fn clear_ab_loop(&self) -> mpv::Result<()> {
        self.mpv.set_property("ab-loop-a", "no")?;
        self.mpv.set_property("ab-loop-b", "no")
    }

    /// 跳到前 / 後幾個章節
    pub fn add_chapter(&self, delta: i64) -> mpv::Result<()> {
        self.mpv.command(&["add", "chapter", &delta.to_string()])
    }

    /// 跳到第 `index` 個章節（從 0 開始）
    pub fn seek_chapter(&self, index: usize) -> mpv::Result<()> {
        self.mpv.set_property("chapter", index as i64)
    }

    /// 畫面輸出目前的影格參數（直接讀，不等屬性通知）
    pub fn out_params(&self) -> Option<OutParams> {
        let json = self.mpv.get_string("video-out-params").ok()?;
        serde_json::from_str(&json)
            .ok()
            .filter(|p: &OutParams| p.w > 0 && p.h > 0)
    }

    /// 檔案原本的形狀，不含任何畫面調整（`video-dec-params`，長寬比、旋轉、裁切都不影響）：
    /// （畫面上的比例（已含檔案本身的旋轉）, 檔案本身的旋轉）
    pub fn natural_shape(&self) -> Option<(f64, i64)> {
        #[derive(Deserialize)]
        struct Dec {
            dw: i64,
            dh: i64,
            #[serde(default)]
            rotate: i64,
        }
        let d: Dec = serde_json::from_str(&self.mpv.get_string("video-dec-params").ok()?).ok()?;
        if d.dw <= 0 || d.dh <= 0 {
            return None;
        }
        let aspect = d.dw as f64 / d.dh as f64;
        // 檔案標示的旋轉：mpv 自己的 MKV 解析器放在容器層（demux-rotation），FFmpeg 解的 MP4 放在影格上
        let rotate = self
            .mpv
            .get_property::<i64>("current-tracks/video/demux-rotation")
            .unwrap_or(d.rotate)
            .rem_euclid(360);
        Some((if rotate % 180 == 90 { 1.0 / aspect } else { aspect }, rotate))
    }

    /// 「原始比例」：mpv 的 video-aspect-override 預設值（0.37 是 -1、0.40 起是 -2，不能寫 "no"）
    pub fn aspect_default(&self) -> String {
        self.mpv
            .get_string("option-info/video-aspect-override/default-value")
            .unwrap_or_else(|_| "-1".to_owned())
    }

    /// 設定一個畫面選項（字串值）；跟目前的值一樣就不設，免得 mpv 又重新設定一次畫面
    pub fn set_option_if_changed(&self, name: &str, value: &str) -> mpv::Result<bool> {
        if self.mpv.get_string(name).ok().as_deref() == Some(value) {
            return Ok(false);
        }
        self.mpv.set_property(name, value)?;
        Ok(true)
    }

    /// 長寬比（旋轉之前整張影格的比例）；None = 原始比例
    pub fn set_aspect_override(&self, value: Option<f64>) -> mpv::Result<bool> {
        let current = self
            .mpv
            .get_string("video-aspect-override")
            .ok()
            .and_then(|v| v.parse::<f64>().ok());
        let wanted = match value {
            Some(v) => v,
            None => self.aspect_default().parse().unwrap_or(-1.0),
        };
        if current.is_some_and(|c| (c - wanted).abs() < 1e-4) {
            return Ok(false);
        }
        self.mpv
            .set_property("video-aspect-override", format!("{wanted:.6}").as_str())?;
        Ok(true)
    }

    /// 翻轉。`use_filter` = 用 vf 濾鏡（軟體繪圖的簡化流程不跑著色器）；
    /// 濾鏡在旋轉之前翻，轉了 90° / 270° 時左右、上下要對調
    pub fn set_flip(&self, horizontal: bool, on: bool, use_filter: bool, quarter_turn: bool) -> mpv::Result<()> {
        let label = if horizontal { "@vs-hflip" } else { "@vs-vflip" };
        if use_filter {
            let _ = self.mpv.command(&["vf", "remove", label]);
            if on {
                let filter = if horizontal != quarter_turn { "hflip" } else { "vflip" };
                self.mpv.command(&["vf", "add", &format!("{label}:{filter}")])?;
            }
            return Ok(());
        }
        let path = crate::geometry::flip_shader_path(horizontal).map_err(|e| mpv::Error {
            code: libmpv2_sys::mpv_error_MPV_ERROR_GENERIC,
            context: crate::tf!("無法寫出翻轉用的著色器：{e}", "Cannot write the flip shader: {e}"),
        })?;
        let path = path.to_string_lossy();
        let listed = self
            .mpv
            .get_string("glsl-shaders")
            .is_ok_and(|list| list.contains(path.as_ref()));
        match (on, listed) {
            (true, false) => self.mpv.command(&["change-list", "glsl-shaders", "append", &path]),
            (false, true) => self.mpv.command(&["change-list", "glsl-shaders", "remove", &path]),
            _ => Ok(()),
        }
    }

    /// 目前的章節，直接問 mpv（剛跳完章節時也是新的值，連按才會累加）；-1 = 第一章之前
    pub fn current_chapter(&self) -> Option<i64> {
        self.mpv.get_property::<i64>("chapter").ok()
    }

    /// 字幕延遲（秒，正數 = 字幕晚一點出現）。主字幕、第二字幕一起調：
    /// mpv 0.38 起第二字幕有自己的 secondary-sub-delay（0.37 沒有，sub-delay 本來就兩條一起動）
    pub fn set_sub_delay(&self, seconds: f64) -> mpv::Result<()> {
        let seconds = round_ms(seconds);
        let _ = self.mpv.set_property("secondary-sub-delay", seconds);
        self.mpv.set_property("sub-delay", seconds)
    }

    /// 套用字幕外觀（每一項都設定，各版本 mpv 的預設值不同）
    pub fn apply_sub_style(&self, style: &crate::settings::SubStyle) {
        for (name, value) in style.mpv_options() {
            if let Err(e) = self.mpv.set_property(name, value.as_str()) {
                eprintln!("[vitascope] 無法設定 {name}={value}：{e}");
            }
        }
    }

    /// 音訊延遲（秒，正數 = 聲音晚一點）
    pub fn set_audio_delay(&self, seconds: f64) -> mpv::Result<()> {
        self.mpv.set_property("audio-delay", round_ms(seconds))
    }

    /// 第二字幕（跟主字幕同時顯示，在畫面上方）；`None` = 關閉
    pub fn set_secondary_sub(&self, id: Option<i64>) -> mpv::Result<()> {
        match id {
            Some(id) => self.mpv.set_property("secondary-sid", id),
            None => self.mpv.set_property("secondary-sid", "no"),
        }
    }

    /// 截圖存成檔案（原始解析度；`subtitles` = 含字幕）。非同步：寫完時送出 `CommandReply { id }`。
    /// 一定要用非同步：同步呼叫會卡住介面（4K 的 PNG 要壓縮零點幾秒，還可能等畫面輸出交出下一格）
    pub fn screenshot_to_file(&self, id: u64, path: &str, subtitles: bool) -> mpv::Result<()> {
        let mode = if subtitles { "subtitles" } else { "video" };
        self.mpv.command_async(id, &["screenshot-to-file", path, mode])
    }

    /// 載入外部音軌檔並切換過去
    pub fn add_audio(&self, path: &str) -> mpv::Result<()> {
        self.mpv.command(&["audio-add", path, "select"])
    }

    /// 外掛字幕的原始檔與自動判斷出的編碼（`track` 是 mpv 的軌道；內嵌字幕回傳 None）
    pub fn external_sub_info(&self, track: &Track) -> Option<&LoadedSub> {
        let file = track.external_filename.as_deref()?;
        self.loaded_subs.iter().find(|s| s.load_path == file)
    }

    /// 用指定的編碼重新載入一條外掛字幕（自動判斷猜錯、顯示亂碼時用）；`None` = 改回自動判斷。
    /// 換成新的軌道並選上，舊的那條移除
    pub fn reload_subtitle(
        &mut self,
        track_id: i64,
        encoding: Option<&'static encoding_rs::Encoding>,
    ) -> mpv::Result<()> {
        // 軌道編號是各類分開算的（影片、音軌、字幕都有 1 號），要限定字幕
        let Some(track) = self.state.tracks_of(TrackKind::Sub).find(|t| t.id == track_id).cloned() else {
            return Ok(());
        };
        let Some(info) = self.external_sub_info(&track).cloned() else {
            return Ok(());
        };
        let video = self
            .state
            .path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| info.original.clone());
        let sub = subs::load_as(&info.original, &video, encoding).map_err(|e| mpv::Error {
            code: libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED,
            context: crate::tf!(
                "無法讀取字幕 {}：{e}",
                "Cannot read subtitle {}: {e}",
                info.original.display()
            ),
        })?;
        // 用 select（不是 cached）：換成同樣內容的編碼時，cached 會選回舊的那條，接著就被移除了
        self.add_external(&sub, AddMode::Select)?;
        if let Some(last) = self.loaded_subs.last_mut() {
            last.forced = encoding.is_some();
        }
        self.mpv.command(&["sub-remove", &track_id.to_string()])?;
        self.refresh_tracks();
        Ok(())
    }

    /// 選擇軌道；`None` = 關閉這類軌道（例如關字幕）
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) -> mpv::Result<()> {
        let Some(prop) = kind.property() else { return Ok(()) };
        // 把目前的第二字幕選成主字幕：兩條對調（mpv 不讓同一條同時當主字幕和第二字幕，直接設定會沒有反應）
        if kind == TrackKind::Sub
            && let Some(new) = id
            && Some(new) == self.state.secondary_sid
        {
            self.mpv.set_property("secondary-sid", "no")?;
            self.mpv.set_property("sid", new)?;
            if let Some(old) = self.state.sid {
                let _ = self.mpv.set_property("secondary-sid", old);
            }
            return Ok(());
        }
        match id {
            Some(id) => self.mpv.set_property(prop, id),
            None => self.mpv.set_property(prop, "no"),
        }
    }

    /// 載入外掛字幕並立刻顯示（拖放字幕檔、選單「載入字幕」）。編碼和語言一樣自動判斷
    pub fn add_subtitle(&mut self, path: &str) -> mpv::Result<()> {
        self.add_subtitle_as(path, true)
    }

    /// 載入外掛字幕；`select` = 選上它（已經載入過的同一個檔案就直接選那一條，不會重複加入）
    pub fn add_subtitle_as(&mut self, path: &str, select: bool) -> mpv::Result<()> {
        let mut sub_path = PathBuf::from(path);
        // VobSub 是 .idx + .sub 一組：拖進來的是 .sub 時改用 .idx（.sub 不是文字字幕）
        if sub_path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sub")) {
            let idx = sub_path.with_extension("idx");
            if idx.is_file() {
                sub_path = idx;
            }
        }
        let video = self
            .state
            .path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| sub_path.clone());
        let already = self
            .loaded_subs
            .iter()
            .any(|s| crate::playlist::same_file(&s.original, &sub_path));
        if already && !select {
            return Ok(());
        }
        let mode = if select { AddMode::SelectExisting } else { AddMode::Auto };
        match subs::load(&sub_path, &video) {
            Ok(sub) => self.add_external(&sub, mode),
            // 讀不了（例如不是本機檔案）就直接交給 mpv
            Err(_) => self.mpv.command(&["sub-add", path, mode.flag()]),
        }
    }

    fn add_external(&mut self, sub: &ExternalSub, mode: AddMode) -> mpv::Result<()> {
        let path = sub.load_path().map_err(|e| mpv::Error {
            code: libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED,
            context: crate::tf!(
                "無法轉換字幕 {}：{e}",
                "Cannot convert subtitle {}: {e}",
                sub.path.display()
            ),
        })?;
        let path = path.to_string_lossy().into_owned();
        // 記住轉碼後的檔案對應到哪個原始檔，之後才能用別的編碼重新載入
        // 自動判斷出的編碼：用別的編碼重新載入時沿用，選單上才一直看得到原本判斷的結果
        let detected = self
            .loaded_subs
            .iter()
            .find(|s| crate::playlist::same_file(&s.original, &sub.path))
            .and_then(|s| s.detected)
            .or(sub.encoding);
        self.loaded_subs.retain(|s| s.load_path != path);
        self.loaded_subs.push(LoadedSub {
            load_path: path.clone(),
            original: sub.path.clone(),
            encoding: sub.encoding,
            detected,
            forced: false,
        });
        let mut args = vec!["sub-add", &path, mode.flag(), &sub.title];
        if let Some(lang) = sub.lang.code() {
            args.push(lang);
        }
        self.mpv.command(&args)
    }

    /// 載入影片旁邊同名的外掛音軌（`.mka`）。用 `auto` 加入、不自動選：
    /// 讓 mpv 照原本的方式選音軌（預設是影片內建的音軌）。mpv 自己的 audio-file-auto 會優先選外掛的，
    /// 實際影片庫裡很多外掛音軌是另一種語言的配音，一開就換成配音不對
    fn load_external_audio(&mut self) {
        let Ok(path) = self.mpv.get_string("path") else { return };
        if path.contains("://") {
            return;
        }
        let tracks: Vec<Track> = self
            .mpv
            .get_string("track-list")
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();
        // 只有影片才找外掛音軌（開音樂檔時，同名的其他音樂檔不是它的音軌）
        if !tracks.iter().any(|t| t.kind == TrackKind::Video && !t.albumart) {
            return;
        }
        // 影片自己沒有音軌：選上第一個外掛音軌，不然會沒有聲音
        let mut select_first = !tracks.iter().any(|t| t.kind == TrackKind::Audio && !t.external);
        for audio in crate::formats::find_external_audio(Path::new(&path)) {
            let file = audio.to_string_lossy();
            // 不指定標題：保留 .mka 裡每條音軌自己的名稱（評論音軌、國語 5.1…）
            let flag = if std::mem::take(&mut select_first) {
                "select"
            } else {
                "auto"
            };
            if let Err(e) = self.mpv.command(&["audio-add", &file, flag]) {
                self.recent_errors
                    .push(format!("[audio] 無法載入 {}：{e}", audio.display()));
            }
        }
    }

    /// 找出並載入目前影片的外掛字幕
    fn load_external_subs(&mut self) {
        let Ok(path) = self.mpv.get_string("path") else { return };
        if path.contains("://") {
            return; // 網路串流沒有「同資料夾」可找
        }
        let video = PathBuf::from(path);
        for p in subs::find_external(&video) {
            let result = subs::load(&p, &video)
                .map_err(|e| e.to_string())
                .and_then(|sub| self.add_external(&sub, AddMode::Auto).map_err(|e| e.to_string()));
            if let Err(e) = result {
                self.recent_errors.push(format!("[subs] 無法載入 {}：{e}", p.display()));
            }
        }
    }

    pub fn get_string(&self, name: &str) -> mpv::Result<String> {
        self.mpv.get_string(name)
    }

    pub fn get_f64(&self, name: &str) -> mpv::Result<f64> {
        self.mpv.get_property::<f64>(name)
    }

    /// 硬體解碼開關（設定視窗；換檔時才完全生效，mpv 會盡量馬上切換）
    pub fn set_hwdec(&self, on: bool) -> mpv::Result<()> {
        self.mpv.set_property("hwdec", if on { "auto-safe" } else { "no" })
    }

    pub fn get_i64(&self, name: &str) -> mpv::Result<i64> {
        self.mpv.get_property::<i64>(name)
    }

    // ───────────── 事件 ─────────────

    /// 處理所有待處理的事件（介面每一幀呼叫），回傳介面需要反應的事件。
    pub fn poll(&mut self) -> Vec<PlayerEvent> {
        let mut out = Vec::new();
        while let Some(ev) = self.mpv.wait_event(0.0) {
            if let Some(pe) = self.handle(ev) {
                out.push(pe);
            }
        }
        out
    }

    /// 等待下一個播放事件，最多 `timeout`（自動測試用）。
    pub fn wait(&mut self, timeout: Duration) -> Option<PlayerEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            if let Some(ev) = self.mpv.wait_event(remaining.as_secs_f64())
                && let Some(pe) = self.handle(ev)
            {
                return Some(pe);
            }
        }
    }

    /// 等到符合條件的事件。檔案播放失敗或逾時都回傳 Err（自動測試用）。
    pub fn wait_for(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&PlayerEvent) -> bool,
    ) -> Result<PlayerEvent, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(ev) = self.wait(remaining) else {
                return Err(format!("等待逾時（{:.1} 秒）", timeout.as_secs_f64()));
            };
            if pred(&ev) {
                return Ok(ev);
            }
            if let PlayerEvent::EndFile { error: Some(e), .. } = &ev {
                return Err(e.clone());
            }
        }
    }

    /// 一直處理事件，直到狀態符合條件（屬性變化是非同步送達的，自動測試用）。
    pub fn wait_state(&mut self, timeout: Duration, mut pred: impl FnMut(&State) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while !pred(&self.state) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("等待狀態逾時（{:.1} 秒）", timeout.as_secs_f64()));
            }
            if let Some(ev) = self.mpv.wait_event(remaining.as_secs_f64().min(0.1))
                && let Some(PlayerEvent::EndFile { error: Some(e), .. }) = self.handle(ev)
            {
                return Err(e);
            }
        }
        Ok(())
    }

    fn handle(&mut self, ev: Event) -> Option<PlayerEvent> {
        match ev {
            Event::PropertyChange { name, value, .. } => {
                self.apply_property(&name, value);
                None
            }
            Event::Log { prefix, level, text } => {
                if std::env::var_os("VITASCOPE_DEBUG").is_some() {
                    eprint!("[mpv/{level}] [{prefix}] {text}");
                }
                if matches!(level.as_str(), "error" | "fatal") {
                    if self.recent_errors.len() >= 8 {
                        self.recent_errors.remove(0);
                    }
                    self.recent_errors.push(format!("[{prefix}] {}", text.trim_end()));
                    if let Some(base) = &self.failure {
                        self.state.last_error = Some(self.compose_failure(base));
                    }
                }
                None
            }
            Event::StartFile => {
                self.state.loading = true;
                self.state.loaded = false;
                self.state.last_error = None;
                self.failure = None;
                self.recent_errors.clear();
                self.loaded_subs.clear();
                Some(PlayerEvent::StartFile)
            }
            Event::FileLoaded => {
                self.state.loading = false;
                self.state.loaded = true;
                if self.external_subs {
                    self.load_external_subs();
                }
                self.load_external_audio();
                if self.auto_select_subs {
                    self.choose_subtitle();
                }
                self.refresh_tracks();
                Some(PlayerEvent::FileLoaded)
            }
            Event::EndFile { reason, error } => {
                self.state.loading = false;
                self.state.loaded = false;
                let error = error.map(|e| {
                    let base = failure_reason(e.code);
                    let full = self.compose_failure(base);
                    self.failure = Some(base.to_owned());
                    full
                });
                if error.is_some() {
                    self.state.last_error = error.clone();
                }
                Some(PlayerEvent::EndFile { reason, error })
            }
            Event::PlaybackRestart => {
                self.refresh_tracks();
                Some(PlayerEvent::PlaybackRestart)
            }
            Event::VideoReconfig => {
                // 直接讀當下的值：屬性通知是非同步的，換檔時可能還是上一部影片的尺寸
                self.state.video_size = self
                    .mpv
                    .get_string("video-out-params")
                    .ok()
                    .and_then(|j| display_size(&j));
                Some(PlayerEvent::VideoReconfig)
            }
            Event::Seek => Some(PlayerEvent::Seek),
            Event::Shutdown => Some(PlayerEvent::Shutdown),
            Event::CommandReply { id, result } => Some(PlayerEvent::CommandReply {
                id,
                error: result.err().map(|e| e.to_string()),
            }),
            _ => None,
        }
    }

    fn apply_property(&mut self, name: &str, value: Value) {
        let s = &mut self.state;
        match name {
            "time-pos" => s.time_pos = value.as_f64().unwrap_or(0.0),
            "duration" => s.duration = value.as_f64(),
            "pause" => s.paused = value.as_bool().unwrap_or(false),
            "volume" => s.volume = value.as_f64().unwrap_or(s.volume),
            "mute" => s.muted = value.as_bool().unwrap_or(false),
            "track-list" => {
                s.tracks = value
                    .as_str()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
            }
            "media-title" => s.title = value.as_str().map(str::to_owned),
            "path" => s.path = value.as_str().map(str::to_owned),
            "eof-reached" => s.eof = value.as_bool().unwrap_or(false),
            "seekable" => s.seekable = value.as_bool().unwrap_or(false),
            "hwdec-current" => s.hwdec = value.as_str().map(str::to_owned),
            "video-out-params" => s.video_size = value.as_str().and_then(display_size),
            "speed" => s.speed = value.as_f64().unwrap_or(1.0),
            "chapter-list" => {
                s.chapters = value
                    .as_str()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
            }
            "chapter" => s.chapter = value.as_i64().and_then(|c| usize::try_from(c).ok()),
            "ab-loop-a" => s.ab_loop[0] = value.as_str().and_then(|v| v.parse().ok()),
            "ab-loop-b" => s.ab_loop[1] = value.as_str().and_then(|v| v.parse().ok()),
            "sub-delay" => s.sub_delay = value.as_f64().unwrap_or(0.0),
            "audio-delay" => s.audio_delay = value.as_f64().unwrap_or(0.0),
            // "no" / "auto" 之類的值 = 沒有選
            "sid" => s.sid = value.as_str().and_then(|v| v.parse().ok()),
            "secondary-sid" => s.secondary_sid = value.as_str().and_then(|v| v.parse().ok()),
            "filtered-metadata" => {
                s.metadata = value
                    .as_str()
                    .and_then(|j| serde_json::from_str(j).ok())
                    .unwrap_or_default();
            }
            _ => {}
        }
    }

    /// mpv 只會自動選「語言符合 slang」或「標記為預設」的字幕軌，
    /// 很多 MKV 的字幕軌兩者都沒有，結果有字幕卻不顯示。
    /// 這裡比照 PotPlayer：有字幕軌但沒選上時，選第一條非強制字幕。
    /// 直接讀當下的軌道清單。開檔時 mpv 會先通知一份「還沒選軌」的清單，
    /// 選好軌的版本可能晚於 FileLoaded / PlaybackRestart 才送達，介面和測試都可能讀到舊的
    fn refresh_tracks(&mut self) {
        if let Ok(json) = self.mpv.get_string("track-list")
            && let Ok(tracks) = serde_json::from_str(&json)
        {
            self.state.tracks = tracks;
        }
    }

    /// 開檔後選字幕：
    /// - 有繁中就選繁中（其次中文、簡中），依語言碼、標題和外掛字幕的內容判斷
    /// - 沒有中文字幕時尊重 mpv 的選擇；mpv 什麼都沒選時（很多 MKV 的字幕軌沒有語言標籤）選第一條，比照 PotPlayer
    fn choose_subtitle(&self) {
        let Ok(json) = self.mpv.get_string("track-list") else {
            return;
        };
        let tracks: Vec<Track> = serde_json::from_str(&json).unwrap_or_default();
        let subs: Vec<&Track> = tracks.iter().filter(|t| t.kind == TrackKind::Sub).collect();
        if subs.is_empty() {
            return;
        }
        let lang_of = |t: &Track| {
            let label = format!(
                "{} {}",
                t.lang.as_deref().unwrap_or(""),
                t.title.as_deref().unwrap_or("")
            );
            subs::classify_label(&label).unwrap_or(SubLang::Unknown)
        };
        // 語言最優先；同語言時外掛優先（字幕組另外附的通常比內嵌的好）；再來是軌道順序
        let best = subs
            .iter()
            .filter(|t| !t.forced)
            .max_by_key(|t| (lang_of(t), t.external, std::cmp::Reverse(t.id)))
            .or(subs.first());
        let current = subs.iter().find(|t| t.selected);
        let pick = match (current, best) {
            (None, Some(b)) => Some(b),
            (Some(c), Some(b)) if lang_of(b).is_chinese() && lang_of(b) > lang_of(c) => Some(b),
            _ => None,
        };
        if let Some(t) = pick {
            let _ = self.select_track(TrackKind::Sub, Some(t.id));
        }
    }

    /// 基本原因加上 mpv 記錄裡的細節，整理成給使用者看的說明。
    fn compose_failure(&self, base: &str) -> String {
        // 常見情況直接翻成中文；其他的附上 mpv 的原始訊息，方便回報問題
        let all = self.recent_errors.join("\n");
        let known = [
            ("No such file", crate::tr!("找不到檔案", "file not found")),
            (
                "Failed to open",
                crate::tr!(
                    "檔案不存在，或沒有讀取權限",
                    "the file does not exist or cannot be read"
                ),
            ),
            ("Permission denied", crate::tr!("沒有讀取權限", "permission denied")),
            (
                "Failed to recognize file format",
                crate::tr!("無法辨識的檔案格式", "Unrecognized file format"),
            ),
            (
                "No video or audio streams",
                crate::tr!("檔案裡沒有可播放的影像或聲音", "No playable video or audio in the file"),
            ),
        ];
        if let Some((_, zh)) = known.iter().find(|(en, _)| all.contains(en)) {
            return crate::tf!("{base}：{zh}", "{base}: {zh}");
        }
        match self.recent_errors.last() {
            Some(detail) => crate::tf!("{base}（{detail}）", "{base} ({detail})"),
            None => base.to_owned(),
        }
    }
}

/// 從 `video-out-params`（JSON）算出實際顯示尺寸：
/// dw/dh 已套用像素比例；如果旋轉交給 GPU 處理（rotate 是 90 或 270），還要交換寬高。
/// 用濾鏡旋轉時（例如 headless），濾鏡之後的 rotate 已經是 0、寬高也已交換。
fn display_size(json: &str) -> Option<[i64; 2]> {
    let p: OutParams = serde_json::from_str(json).ok()?;
    if p.dw <= 0 || p.dh <= 0 {
        return None;
    }
    Some(if p.rotate.rem_euclid(180) == 90 {
        [p.dh, p.dw]
    } else {
        [p.dw, p.dh]
    })
}

/// mpv 錯誤碼 → 中文
fn failure_reason(code: i32) -> &'static str {
    use libmpv2_sys as sys;
    match code {
        sys::mpv_error_MPV_ERROR_LOADING_FAILED => crate::tr!("無法載入檔案", "Cannot load the file"),
        sys::mpv_error_MPV_ERROR_UNKNOWN_FORMAT => crate::tr!("無法辨識的檔案格式", "Unrecognized file format"),
        sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY => {
            crate::tr!("檔案裡沒有可播放的影像或聲音", "No playable video or audio in the file")
        }
        sys::mpv_error_MPV_ERROR_AO_INIT_FAILED => crate::tr!("無法開啟音訊裝置", "Cannot open the audio device"),
        sys::mpv_error_MPV_ERROR_VO_INIT_FAILED => crate::tr!("無法初始化影像輸出", "Cannot initialize video output"),
        sys::mpv_error_MPV_ERROR_UNSUPPORTED => crate::tr!("不支援的格式", "Unsupported format"),
        _ => crate::tr!("播放失敗", "Playback failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::{State, Track, TrackKind, debug_log_level, display_size};

    #[test]
    fn secondary_subtitle_id_does_not_hide_other_kinds() {
        let audio: Track = serde_json::from_str(r#"{"id":1,"type":"audio","selected":true}"#).unwrap();
        let sub: Track = serde_json::from_str(r#"{"id":1,"type":"sub","selected":true}"#).unwrap();
        let state = State {
            tracks: vec![audio, sub],
            secondary_sid: Some(1),
            ..Default::default()
        };
        assert_eq!(state.selected(TrackKind::Audio).map(|t| t.id), Some(1));
        assert!(
            state.selected(TrackKind::Sub).is_none(),
            "1 號字幕是第二字幕，不是主字幕"
        );
    }

    #[test]
    fn external_track_label_uses_the_file_name() {
        let t: Track = serde_json::from_str(
            r#"{"id":2,"type":"audio","title":"mka","external":true,"external-filename":"C:/動畫/S01E01.mka"}"#,
        )
        .unwrap();
        assert!(t.label().starts_with("#2 S01E01.mka"), "{}", t.label());
        let named: Track = serde_json::from_str(
            r#"{"id":3,"type":"audio","title":"評論音軌","external":true,"external-filename":"C:/動畫/S01E01.mka"}"#,
        )
        .unwrap();
        assert!(named.label().starts_with("#3 評論音軌"), "{}", named.label());
    }

    #[test]
    fn debug_env_never_blocks_startup() {
        assert_eq!(debug_log_level(None), "error");
        assert_eq!(debug_log_level(Some("1")), "warn");
        assert_eq!(debug_log_level(Some("")), "warn");
        assert_eq!(debug_log_level(Some("v")), "v");
        assert_eq!(debug_log_level(Some("trace")), "trace");
        // mpv 不認得的值會讓 request_log_messages 失敗，播放器就開不起來
        for odd in ["true", "yes", "0", "verbose", "V"] {
            assert_eq!(debug_log_level(Some(odd)), "warn", "{odd}");
        }
    }

    #[test]
    fn display_size_accounts_for_gpu_rotation() {
        // 濾鏡已經轉好（headless）：照原樣
        assert_eq!(
            display_size(r#"{"w":240,"h":320,"dw":240,"dh":320,"rotate":0}"#),
            Some([240, 320])
        );
        // GPU 負責旋轉（GUI）：寬高交換
        assert_eq!(
            display_size(r#"{"w":320,"h":240,"dw":320,"dh":240,"rotate":90}"#),
            Some([240, 320])
        );
        assert_eq!(display_size(r#"{"dw":320,"dh":240,"rotate":270}"#), Some([240, 320]));
        assert_eq!(display_size(r#"{"dw":320,"dh":240,"rotate":180}"#), Some([320, 240]));
        // 沒有影像
        assert_eq!(display_size("null"), None);
        assert_eq!(display_size(r#"{"dw":0,"dh":0}"#), None);
    }
}
