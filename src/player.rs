//! 播放器核心：把 mpv 的屬性與事件整理成 Rust 狀態。
//! 介面（app）和自動測試（tests/）都透過這一層操作 mpv。

use crate::mpv::{self, EndReason, Event, Format, Mpv, Value};
use crate::subs::{self, ExternalSub, SubLang};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// 額外的 mpv 選項，排在 VITASCOPE_MPV_OPTS 之後設定（測試用，例如 `("vo-null-fps", "120")`；
    /// 環境變數是整個行程共用的，平行跑的測試會互相干擾）。跟環境變數一樣算是使用者指定的選項
    pub extra: Vec<(String, String)>,
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
            extra: Vec::new(),
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
    /// 杜比視界的 profile（只有主版本：8.1、8.4 都是 8）；不是杜比視界時 None
    #[serde(default)]
    pub dolby_vision_profile: Option<i64>,
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
    /// mpv 正在依螢幕更新率同步影像（video-sync=display-*，而且 mpv 判斷這部影片適用；
    /// 跟 mpv 的 display-sync-active 一樣，見 OBSERVED 的說明）
    pub display_sync_active: bool,
    /// 正在去交錯
    pub deinterlace_active: bool,
    /// 音訊直通中：直通的格式（"ac3"、"dts"…）；None = 一般 PCM 輸出或沒有聲音
    pub audio_spdif: Option<String>,
    /// 音訊輸出開著、是一般的 PCM（不是直通）
    pub audio_out_pcm: bool,
    /// 目前的音訊輸出方式（"wasapi"、"pipewire"、"null"…）；None = 音訊輸出沒開
    pub current_ao: Option<String>,
    /// 目前的影片是 HDR（video-params 的 gamma 是 pq 或 hlg）
    pub video_hdr: bool,
    /// 音訊輸出裝置清單（第一項是 auto）；None = 還沒讀過（見 `Player::read_audio_devices`、`watch_audio_devices`）
    pub audio_devices: Option<Vec<crate::sound::AudioDevice>>,
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
    // 以下是 L3 加的；只能加在最後面，前面的編號不能變
    // 顯示同步：不觀察 display-sync-active，mpv 只在開檔、關檔時重新檢查它，播放中開始同步了也不會通知。
    // mistimed-frame-count 每一輪都檢查，而且只在顯示同步時才有值（平常很少變，不會一直送通知）
    ("mistimed-frame-count", Format::Int64),
    ("deinterlace-active", Format::Flag),
    // 節點：用字串讀拿到 JSON，只看 format（spdif-ac3 之類 = 音訊直通）
    ("audio-out-params", Format::String),
    // 影片的轉換函數（pq、hlg = HDR）；只在換影片設定時變
    ("video-params/gamma", Format::String),
    // 音訊輸出開不起來時 mpv 改用 null（沒有聲音，見 Player::new 的 audio-fallback-to-null）
    ("current-ao", Format::String),
];

/// 非同步設定選項（`set_async`、`command_async_keyed`）的指令編號從這裡開始。
/// 截圖用 1<<40 起算（app/capture.rs），兩段不會重疊：這一段一定有第 44 位元，截圖的一定沒有
pub const ASYNC_BASE: u64 = 1 << 44;
/// 指令編號的低 24 位元是流水號
const ASYNC_SEQ_MASK: u64 = 0xFF_FFFF;

/// 定義 `AsyncKey` 與 `AsyncKey::ALL`：兩者由同一份清單產生，新增種類只要加在清單最後面。
/// 分開手寫的話，漏加進 `ALL` 的種類解不回來，它的回覆會被當成截圖的回覆、失敗也不會提示
macro_rules! async_keys {
    ($first:ident $(, $rest:ident)* $(,)?) => {
        /// 非同步指令是為了哪一項設定送的：回覆（成功或失敗）依這個分派。
        /// 編號從 1 開始連續（0 留給「不是設定送的」）
        #[repr(u16)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum AsyncKey {
            $first = 1,
            $($rest,)*
        }

        impl AsyncKey {
            /// 全部的種類，依編號排列
            pub const ALL: [AsyncKey; [stringify!($first) $(, stringify!($rest))*].len()] =
                [AsyncKey::$first $(, AsyncKey::$rest)*];
        }
    };
}

async_keys!(
    VideoSync,
    DisplayFps,
    Adjust,
    Deinterlace,
    Deband,
    Scaler,
    Shaders,
    Sharpen,
    Tone,
    AudioDevice,
    Exclusive,
    VolumeMax,
    Downmix,
    Spdif,
    Af,
    AfCommand,
);

impl AsyncKey {
    fn from_raw(v: u64) -> Option<Self> {
        Self::ALL.into_iter().find(|k| *k as u64 == v)
    }
}

/// 種類 + 流水號 → 非同步指令編號
fn async_id(k: AsyncKey, seq: u64) -> u64 {
    ASYNC_BASE | (k as u64) << 24 | (seq & ASYNC_SEQ_MASK)
}

/// 非同步指令編號 → 是哪一項設定送的；不是 `set_async` / `command_async_keyed` 送的（例如截圖）回傳 None
pub fn async_key(id: u64) -> Option<AsyncKey> {
    // 第 44 位元以上只能有第 44 位元
    if id >> 44 != 1 {
        return None;
    }
    AsyncKey::from_raw((id >> 24) & ((1 << 20) - 1))
}

/// 播放引擎有哪些 L3 功能（啟動時偵測一次；舊的引擎、系統的 libmpv 不一定有）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineCaps {
    pub af: AfCaps,
    /// deinterlace=auto（本專案建置的引擎有；Linux tar.gz 用的系統 libmpv 不一定有）
    pub deint_auto: bool,
    /// 有 deinterlace-active 屬性（看得到目前有沒有在去交錯；系統的 libmpv 0.37 沒有）
    pub deint_status: bool,
    /// 軟體繪圖的簡化流程（gpu-dumb-mode）：不跑著色器、縮放演算法之類的效果
    pub dumb: bool,
    pub macos: bool,
    /// 播放中改 audio-spdif 馬上生效（mpv 0.41 起會重新開啟音訊解碼器；
    /// 系統的 libmpv 0.37–0.40 要到下一個檔案才生效）
    pub spdif_live: bool,
    /// 畫面輸出的 OpenGL 能做 HDR 動態峰值偵測（hdr-compute-peak 要 GLSL 4.20 + compute shader + SSBO，
    /// 見 `video::GlInfo::compute_peak`）。看的是介面的 GL context，不是引擎：`probe_caps` 一律 false，
    /// 介面建立時依 GL context 設定（介面測試沒有 GL context，也是 false）
    pub compute_peak: bool,
}

/// mpv 的版本（`mpv-version`：「mpv 0.37.0」「mpv v0.41.0-1102-g6c092d978」）→（主版本, 次版本）；看不懂時 None
fn mpv_version(text: &str) -> Option<(u32, u32)> {
    let v = text.strip_prefix("mpv ")?.trim_start_matches('v');
    let mut parts = v.split(['.', '-']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// 這個版本播放中改 audio-spdif 會不會馬上生效（0.41 起 audio-spdif 有 UPDATE_AD）。
/// 看不懂的版本字串（自己建置的、git 版）當成新版
fn spdif_live(version: &str) -> bool {
    mpv_version(version).is_none_or(|v| v >= (0, 41))
}

/// 等化器、音量平衡用到的 FFmpeg 音訊濾鏡，各自有沒有
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AfCaps {
    pub equalizer: bool,
    pub acompressor: bool,
    pub alimiter: bool,
    pub dynaudnorm: bool,
    pub speechnorm: bool,
    pub aformat: bool,
}

impl AfCaps {
    /// 偵測的濾鏡名稱
    pub const NAMES: [&'static str; 6] = [
        "equalizer",
        "acompressor",
        "alimiter",
        "dynaudnorm",
        "speechnorm",
        "aformat",
    ];

    pub(crate) fn set(&mut self, name: &str, on: bool) {
        match name {
            "equalizer" => self.equalizer = on,
            "acompressor" => self.acompressor = on,
            "alimiter" => self.alimiter = on,
            "dynaudnorm" => self.dynaudnorm = on,
            "speechnorm" => self.speechnorm = on,
            "aformat" => self.aformat = on,
            _ => {}
        }
    }

    /// 有沒有這個濾鏡（`NAMES` 的名稱；其他名稱都是沒有）
    pub fn has(&self, name: &str) -> bool {
        match name {
            "equalizer" => self.equalizer,
            "acompressor" => self.acompressor,
            "alimiter" => self.alimiter,
            "dynaudnorm" => self.dynaudnorm,
            "speechnorm" => self.speechnorm,
            "aformat" => self.aformat,
            _ => false,
        }
    }

    /// 六個濾鏡都有
    pub fn all(&self) -> bool {
        self.equalizer && self.acompressor && self.alimiter && self.dynaudnorm && self.speechnorm && self.aformat
    }
}

/// 偵測濾鏡時暫時加進 af 的標籤
const PROBE_LABEL: &str = "@vs-probe";
/// 畫面輸出的錯誤記錄最多留幾筆（給著色器失敗之類的偵測用）
const RENDER_ERRORS_CAP: usize = 16;

/// 沒有害處、不用讓使用者看到的錯誤記錄：nvdec 的 bwdif_cuda 建不起來時 mpv 會自己改用
/// hwdownload + bwdif，播放不受影響
fn is_harmless_error(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    (t.contains("bwdif_cuda") && t.contains("failed")) || t.contains("creating deinterlacer failed")
}

/// 畫面輸出（render API、vo）的記錄：著色器編譯失敗之類的錯誤從這裡來
fn is_render_log(prefix: &str) -> bool {
    prefix.starts_with("libmpv_render") || prefix.starts_with("vo")
}

/// video-params 的 gamma（轉換函數）是 HDR 的：PQ（HDR10、杜比視界）或 HLG
fn is_hdr_gamma(gamma: &str) -> bool {
    matches!(gamma, "pq" | "hlg")
}

/// 音效選項 → 非同步設定時的種類（回覆依種類分派）
fn sound_key(name: &str) -> AsyncKey {
    match name {
        "audio-device" => AsyncKey::AudioDevice,
        "audio-exclusive" => AsyncKey::Exclusive,
        "audio-spdif" => AsyncKey::Spdif,
        _ => AsyncKey::Downmix,
    }
}

/// 觀察 audio-device-list 用的編號（接在 OBSERVED 後面；要用時才觀察，見 `Player::watch_audio_devices`）
const DEVICE_LIST_ID: u64 = OBSERVED.len() as u64 + 1;

/// 畫質選項 → 非同步設定時的種類（回覆依種類分派）
fn picture_key(name: &str) -> AsyncKey {
    match name {
        "deinterlace" => AsyncKey::Deinterlace,
        "sharpen" => AsyncKey::Sharpen,
        "scale" | "dscale" | "cscale" | "scale-antiring" => AsyncKey::Scaler,
        n if n.starts_with("deband") => AsyncKey::Deband,
        _ => AsyncKey::Tone,
    }
}

/// `audio-out-params`（JSON）→ 直通的格式：format 是 spdif-ac3 之類的時候
fn spdif_format(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v["format"].as_str()?.strip_prefix("spdif-").map(str::to_owned)
}

/// `audio-out-params`（JSON）有輸出格式（音訊輸出開著）
fn has_out_format(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v["format"].as_str().is_some_and(|f| !f.is_empty()))
}

/// Windows：整個程式一直保有多執行緒 COM（MTA）。mpv 偵測音訊裝置插拔（觀察 `audio-device-list`）時，
/// 在自己的核心執行緒上 `CoInitializeEx(MTA)`，關閉時 `CoUninitialize`；那是程式裡唯一的 MTA 時，
/// COM 整個被拆掉，系統的裝置通知卻還在用，關閉播放器時存取違規（GitHub 的 Windows 虛擬機上每次都會）。
/// 先登記一份程式層級的 MTA 使用（不再減回去），mpv 收掉的就只是它自己那一份
#[cfg(windows)]
fn keep_com_mta_alive() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut cookie = std::ptr::null_mut();
        // SAFETY: 只傳一個輸出用的指標；cookie 刻意不還（程式結束前都要保留）
        let hr = unsafe { windows_sys::Win32::System::Com::CoIncrementMTAUsage(&mut cookie) };
        if hr < 0 {
            eprintln!("[vitascope] CoIncrementMTAUsage 失敗：{hr:#x}");
        }
    });
}

/// VITASCOPE_MPV_OPTS 的內容 → (名稱, 值)；沒有「=」的項目略過（mpv 也不會收到）
fn env_options(value: &str) -> impl Iterator<Item = (&str, &str)> {
    value.split_whitespace().filter_map(|kv| kv.split_once('='))
}

/// VITASCOPE_MPV_OPTS 提到的選項名稱。沒有「=」的項目 mpv 收不到，但一樣算使用者自己處理的選項，
/// 自動設定照舊略過它（例如只寫 gpu-dumb-mode 也不會自動開軟體繪圖的簡化流程）
fn env_option_names(value: &str) -> impl Iterator<Item = &str> {
    value.split_whitespace().filter_map(|kv| kv.split('=').next())
}

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

/// 換檔時還原的畫面選項（軟體繪圖翻轉用的 vf 也是）。不含 glsl-shaders：它由影戲管理
///（使用者的著色器組合換檔照舊，翻轉的著色器在開新檔之前拿掉，見 `Player::open`）
const GEOMETRY_OPTIONS: &str =
    "video-aspect-override,video-crop,video-rotate,video-zoom,video-pan-x,video-pan-y,panscan,vf";

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
    /// 非同步指令的流水號
    async_seq: AtomicU64,
    /// 使用者用 VITASCOPE_MPV_OPTS 或 `Options.extra` 指定的選項：自動設定都要略過它們
    user_overrides: HashSet<String>,
    /// 畫面輸出的錯誤記錄（收到的時間, 內容），最多 16 筆
    render_errors: VecDeque<(Instant, String)>,
    /// 偵測引擎功能失敗時 mpv 會記一筆錯誤；這些不是真的問題，不放進 recent_errors。
    /// 每一項的兩個字串都出現在記錄裡才算（記錄訊息比較晚送達，可能開檔之後才收到）
    probe_noise: Vec<[String; 2]>,
    /// 上次由 `apply_picture`、`apply_sound` 送出的畫質、音效選項值（只送有變的）
    options_applied: HashMap<&'static str, String>,
    /// 已經開始觀察 audio-device-list（mpv 同時開始偵測裝置插拔）
    watching_devices: bool,
    /// 測試用的假裝置清單：有的話不讀 mpv 的（見 `set_fake_audio_devices`）
    fake_devices: bool,
    /// 測試用：影片軌一律當成這個杜比視界 profile（見 `set_fake_dolby_vision`）
    fake_dolby_vision: Option<i64>,
    /// 像素著色器：使用者的組合（app 給的；VITASCOPE_MPV_OPTS 指定了 glsl-shaders 時是使用者原本的清單，不改）
    shader_user: Vec<String>,
    /// 翻轉用的著色器（左右、上下）；每個檔案各自的，開新檔時拿掉
    shader_flip: [Option<PathBuf>; 2],
    /// 上次送出的 glsl-shaders；None = 不確定 mpv 現在的值（送出失敗、或可能被晚到的非同步指令蓋掉），下次一定送
    shaders_applied: Option<Vec<String>>,
    /// 還沒回覆的非同步 glsl-shaders 指令
    shaders_inflight: HashSet<u64>,
    /// 音量超過 100% 時，經過限幅器放大的部分（%）：總音量 = mpv 的 volume + 這個（見 `set_volume_total`）
    boost_pct: f64,
    /// ao 選項本來就有 null（headless、VITASCOPE_MPV_OPTS 指定）：用 null 輸出不是開不起來改用的
    ao_null_wanted: bool,
}

impl Player {
    pub fn new(opts: Options) -> mpv::Result<Self> {
        #[cfg(windows)]
        keep_com_mta_alive();
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
            // 音訊輸出開不起來（獨佔模式不被允許、選的裝置拔掉了…）時改用 null 輸出、繼續播放（沒有聲音）。
            // 不然 mpv 會把音軌關掉，純音樂檔整個停止；音訊輸出還在，之後改裝置、獨佔模式時 mpv 才會照新的設定重開。
            // 改用 null 時介面提示（見 `Player::audio_fell_back`）。VITASCOPE_MPV_OPTS 可以改回來（排在後面）
            ("audio-fallback-to-null", "yes"),
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
        let env = std::env::var("VITASCOPE_MPV_OPTS").unwrap_or_default();
        options.extend(env_options(&env));
        options.extend(opts.extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut user_overrides: HashSet<String> = env_option_names(&env)
            .map(str::to_owned)
            .chain(opts.extra.iter().map(|(k, _)| k.clone()))
            .collect();
        let mut mpv = Mpv::new(&options)?;
        // profile=high-quality、include=… 之類間接改到的畫質、音效選項也算使用者指定的：
        // 建立後跟引擎的預設值不一樣的就是（上面我們自己的選項都不是這些選項）
        // af（等化器、音量平衡的濾鏡鏈）也一樣：預設是空的
        for name in crate::picture::MANAGED
            .into_iter()
            .chain(crate::sound::MANAGED)
            .chain(["af"])
        {
            if let (Ok(now), Ok(default)) = (
                mpv.get_string(name),
                mpv.get_string(&format!("option-info/{name}/default-value")),
            ) && now != default
            {
                user_overrides.insert(name.to_owned());
            }
        }
        // 像素著色器也一樣：glsl-shaders 預設是空的，建立後有內容（include=、profile= 的設定檔改的也算），
        // 或 VITASCOPE_MPV_OPTS 寫了 glsl-shaders（即使是空的；glsl-shaders-append 之類、別名 glsl-shader 也算），
        // 就是使用者自己指定的。影戲不改它，翻轉的著色器接在它後面。glsl-shader-opts 是著色器的參數、不是清單，不算
        // 指定成空的（glsl-shaders=）時 mpv 的清單是一個空字串：不當成檔案（翻轉接在後面時不能多一個空的路徑）
        let mut shader_base = mpv.get_string_list("glsl-shaders").unwrap_or_default();
        shader_base.retain(|s| !s.is_empty());
        let names_shader_list =
            |n: &String| n == "glsl-shader" || n == "glsl-shaders" || n.starts_with("glsl-shaders-");
        if !shader_base.is_empty() || user_overrides.iter().any(names_shader_list) {
            user_overrides.insert("glsl-shaders".to_owned());
        }
        let ao_null_wanted = crate::sound::ao_list_has_null(&mpv.get_string("ao").unwrap_or_default());
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
            async_seq: AtomicU64::new(0),
            user_overrides,
            render_errors: VecDeque::new(),
            probe_noise: Vec::new(),
            options_applied: HashMap::new(),
            watching_devices: false,
            fake_devices: false,
            fake_dolby_vision: None,
            shaders_applied: Some(shader_base.clone()),
            shader_user: shader_base,
            shader_flip: [None, None],
            shaders_inflight: HashSet::new(),
            boost_pct: 0.0,
            ao_null_wanted,
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

    /// 使用者用 VITASCOPE_MPV_OPTS 或 `Options.extra` 指定的 mpv 選項名稱（這些不自動調整）
    pub fn user_overrides(&self) -> &HashSet<String> {
        &self.user_overrides
    }

    /// 取出畫面輸出的錯誤記錄（libmpv_render、vo；收到的時間, 內容）
    pub fn take_render_errors(&mut self) -> Vec<(Instant, String)> {
        self.render_errors.drain(..).collect()
    }

    /// 測試用：當成畫面輸出記錄了這行錯誤（例如著色器編譯失敗；介面測試沒有真的畫面）
    #[doc(hidden)]
    pub fn push_render_error(&mut self, text: &str) {
        self.render_errors.push_back((Instant::now(), text.to_owned()));
    }

    // ───────────── 非同步設定 ─────────────

    /// 非同步設定一個屬性（`set 名稱 值`）。結果以 `PlayerEvent::CommandReply { id }` 回報，
    /// `async_key(id)` 就是 `k`。播放中改設定都要用非同步：很多畫面選項要等畫面輸出執行緒處理完才會回傳
    pub fn set_async(&self, k: AsyncKey, name: &str, value: &str) -> mpv::Result<u64> {
        self.command_async_keyed(k, &["set", name, value])
    }

    /// 非同步指令，回覆的編號帶著種類 `k`（見 `set_async`）。
    /// 非同步指令之間照送出的順序執行；同步呼叫可能插隊到還沒執行的非同步指令前面
    pub fn command_async_keyed(&self, k: AsyncKey, args: &[&str]) -> mpv::Result<u64> {
        let id = async_id(k, self.async_seq.fetch_add(1, Ordering::Relaxed));
        self.mpv.command_async(id, args)?;
        Ok(id)
    }

    /// 同步設定一組選項，只在啟動時（還沒開任何檔案）用；使用者自己指定的選項略過。
    /// 回傳設定失敗的（名稱, 錯誤）
    pub fn apply_sync(&self, opts: &[(&str, String)]) -> Vec<(String, mpv::Error)> {
        debug_assert!(!self.state.loaded && !self.state.loading, "apply_sync 只能在開檔之前用");
        opts.iter()
            .filter(|(name, _)| !self.user_overrides.contains(*name))
            .filter_map(|(name, value)| {
                self.mpv
                    .set_property(name, value.as_str())
                    .err()
                    .map(|e| (name.to_string(), e))
            })
            .collect()
    }

    /// 套用畫質選項（`picture::mpv_options` 的結果），只送跟上次送出的不一樣的。
    /// `sync`：啟動時（還沒開檔）同步設定；不然非同步，依選項分成去交錯、去色帶、銳化、縮放、HDR 幾種回覆。
    /// 回傳送出的每一項：同步設定成功是 `Ok(None)`，非同步送出是 `Ok(Some(指令編號))`，失敗是 `Err`。
    /// 失敗的不記下來（下次再送）；非同步的回覆說失敗時呼叫 `forget_picture`
    pub fn apply_picture(
        &mut self,
        opts: &[(&'static str, String)],
        sync: bool,
    ) -> Vec<(&'static str, AsyncKey, mpv::Result<Option<u64>>)> {
        self.apply_cached(opts, sync, picture_key)
    }

    /// 套用音效選項（`sound::mpv_options` 的結果），跟 `apply_picture` 一樣只送有變的；
    /// 非同步時依選項分成輸出裝置、獨佔模式、轉成立體聲、音訊直通幾種回覆。
    /// 改輸出裝置、獨佔模式、聲道會重新開啟音訊輸出（聲音中斷一下），所以沒變的一定不能送
    pub fn apply_sound(
        &mut self,
        opts: &[(&'static str, String)],
        sync: bool,
    ) -> Vec<(&'static str, AsyncKey, mpv::Result<Option<u64>>)> {
        self.apply_cached(opts, sync, sound_key)
    }

    /// `apply_sound` 會送出哪些選項（跟上次送的不一樣、使用者沒有自己指定的）
    pub fn pending_sound<'a>(&'a self, opts: &'a [(&'static str, String)]) -> impl Iterator<Item = &'static str> + 'a {
        opts.iter()
            .filter(|(name, value)| {
                !self.user_overrides.contains(*name) && self.options_applied.get(name) != Some(value)
            })
            .map(|(name, _)| *name)
    }

    fn apply_cached(
        &mut self,
        opts: &[(&'static str, String)],
        sync: bool,
        key_of: fn(&str) -> AsyncKey,
    ) -> Vec<(&'static str, AsyncKey, mpv::Result<Option<u64>>)> {
        debug_assert!(
            !sync || (!self.state.loaded && !self.state.loading),
            "同步設定只能在開檔之前用"
        );
        let mut sent = Vec::new();
        for (name, value) in opts {
            if self.user_overrides.contains(*name) || self.options_applied.get(name) == Some(value) {
                continue;
            }
            let key = key_of(name);
            let result = if sync {
                self.mpv.set_property(name, value.as_str()).map(|()| None)
            } else {
                self.set_async(key, name, value).map(Some)
            };
            match &result {
                Ok(_) => self.options_applied.insert(name, value.clone()),
                Err(_) => self.options_applied.remove(name),
            };
            sent.push((*name, key, result));
        }
        sent
    }

    /// 非同步設定的畫質選項 mpv 不接受：忘掉記下的值，下次套用時再送
    pub fn forget_picture(&mut self, name: &str) {
        self.options_applied.remove(name);
    }

    /// 非同步設定的音效選項 mpv 不接受：忘掉記下的值，下次套用時再送
    pub fn forget_sound(&mut self, name: &str) {
        self.options_applied.remove(name);
    }

    /// 音訊輸出開不起來，mpv 改用 null 輸出（沒有聲音；ao 本來就指定 null 的不算）
    pub fn audio_fell_back(&self) -> bool {
        !self.ao_null_wanted && self.state.current_ao.as_deref() == Some("null")
    }

    /// 同 `audio_fell_back`，直接問 mpv（重開音訊輸出之後，觀察到的值可能還是重開前的）
    pub fn audio_fell_back_now(&self) -> bool {
        !self.ao_null_wanted && self.get_string("current-ao").is_ok_and(|ao| ao == "null")
    }

    // ───────────── 音訊輸出裝置 ─────────────

    /// 同步讀一次裝置清單（啟動時存了指定的裝置才用：要知道它還在不在）。
    /// 第一次讀要列舉所有輸出方式的裝置（Windows 約 30 毫秒；PulseAudio、PipeWire 不正常時會更久），
    /// 所以平常改用 `watch_audio_devices`。讀不到時回傳 None
    pub fn read_audio_devices(&mut self) -> Option<&[crate::sound::AudioDevice]> {
        if !self.fake_devices {
            let json = self.mpv.get_string("audio-device-list").ok()?;
            self.state.audio_devices = Some(crate::sound::parse_devices(&json));
        }
        self.state.audio_devices.as_deref()
    }

    /// 開始觀察裝置清單（`state.audio_devices` 跟著插拔更新；mpv 同時開始偵測插拔）。只做一次
    pub fn watch_audio_devices(&mut self) {
        if std::mem::replace(&mut self.watching_devices, true) {
            return;
        }
        if let Err(e) = self.mpv.observe(DEVICE_LIST_ID, "audio-device-list", Format::String) {
            eprintln!("[vitascope] 無法觀察音訊裝置清單：{e}");
        }
    }

    /// 測試用：當成 mpv 的裝置清單是這些（自動測試的電腦不一定有音訊裝置；之後 mpv 的清單不再蓋掉它）
    #[doc(hidden)]
    pub fn set_fake_audio_devices(&mut self, list: Vec<crate::sound::AudioDevice>) {
        self.fake_devices = true;
        self.state.audio_devices = Some(list);
    }

    /// 測試用：之後每個檔案的影片軌都當成杜比視界 `profile`（產生不了杜比視界的樣本；None = 照 mpv 的）
    #[doc(hidden)]
    pub fn set_fake_dolby_vision(&mut self, profile: Option<i64>) {
        self.fake_dolby_vision = profile;
        self.apply_fake_dolby_vision();
    }

    fn apply_fake_dolby_vision(&mut self) {
        if let Some(p) = self.fake_dolby_vision {
            for t in self.state.tracks.iter_mut().filter(|t| t.kind == TrackKind::Video) {
                t.dolby_vision_profile = Some(p);
            }
        }
    }

    // ───────────── 像素著色器（glsl-shaders） ─────────────

    /// mpv 目前的 glsl-shaders（每一項；直接問 mpv）
    pub fn shader_list(&self) -> mpv::Result<Vec<String>> {
        self.mpv.get_string_list("glsl-shaders")
    }

    /// 使用者的著色器組合（目前的；不含翻轉）
    pub fn user_shaders(&self) -> &[String] {
        &self.shader_user
    }

    /// 送出的 glsl-shaders 都已經生效（沒有還沒回覆的非同步指令，也確定 mpv 的值；自動測試用）
    pub fn shaders_settled(&self) -> bool {
        self.shaders_inflight.is_empty() && self.shaders_applied.is_some()
    }

    /// 換使用者的著色器組合（app 依使用中的組合算好，缺的檔案已經拿掉）：重新組出清單，有變才送。
    /// `sync`：啟動時（還沒開檔）同步設定；不然非同步（`AsyncKey::Shaders`，回傳指令編號）。
    /// VITASCOPE_MPV_OPTS 指定了 glsl-shaders 時不換（留著使用者自己的清單）
    pub fn set_user_shaders(&mut self, user: Vec<String>, sync: bool) -> mpv::Result<Option<u64>> {
        if self.user_overrides.contains("glsl-shaders") {
            return Ok(None);
        }
        self.shader_user = user;
        self.push_shaders(sync)
    }

    /// 目前該有的清單：使用者的組合 + 翻轉
    fn wanted_shaders(&self) -> Vec<String> {
        let [h, v] = &self.shader_flip;
        crate::picture::shader::compose(&self.shader_user, h.as_deref(), v.as_deref())
    }

    /// 把該有的清單送給 mpv（跟上次送的一樣就不送）。一次換掉整個清單（change-list set），
    /// mpv 只重新載入一次著色器；空的清單用 clr（set 空字串會變成一個空的項目）
    fn push_shaders(&mut self, sync: bool) -> mpv::Result<Option<u64>> {
        let wanted = self.wanted_shaders();
        if self.shaders_applied.as_ref() == Some(&wanted) {
            return Ok(None);
        }
        let value = crate::picture::shader::list_value(&wanted).map_err(|e| mpv::Error {
            code: libmpv2_sys::mpv_error_MPV_ERROR_INVALID_PARAMETER,
            context: e.message(),
        })?;
        let args: [&str; 4] = if wanted.is_empty() {
            ["change-list", "glsl-shaders", "clr", ""]
        } else {
            ["change-list", "glsl-shaders", "set", &value]
        };
        let result = if sync {
            self.mpv.command(&args).map(|()| None)
        } else {
            self.command_async_keyed(AsyncKey::Shaders, &args).map(Some)
        };
        match &result {
            Ok(Some(id)) => {
                self.shaders_inflight.insert(*id);
                self.shaders_applied = Some(wanted);
            }
            // 同步設定可能插隊到還沒執行的非同步指令前面（之後才執行的舊清單會蓋掉它）：
            // 還有沒回覆的話不確定最後的值，開始播新檔時（StartFile）再送一次非同步的
            Ok(None) => {
                self.shaders_applied = self.shaders_inflight.is_empty().then_some(wanted);
            }
            Err(_) => self.shaders_applied = None,
        }
        result
    }

    /// 開新檔之前：拿掉翻轉的著色器（每個檔案各自的）。同步設定，新檔案的第一格就不會翻轉
    ///（非同步的可能排在 loadfile 之後才生效）；換檔時頓一下看不出來
    fn reset_shaders_for_next_file(&mut self) {
        self.shader_flip = [None, None];
        if let Err(e) = self.push_shaders(true) {
            eprintln!("[vitascope] 無法設定 glsl-shaders：{e}");
        }
    }

    // ───────────── 引擎功能偵測 ─────────────

    /// 偵測播放引擎的功能（同步；只在啟動時、還沒開檔之前呼叫）
    pub fn probe_caps(&mut self) -> EngineCaps {
        let mut af = AfCaps::default();
        for name in AfCaps::NAMES {
            af.set(name, self.probe_af(name));
        }
        EngineCaps {
            af,
            deint_auto: self.probe_deint_auto(),
            deint_status: self
                .mpv
                .get_string("property-list")
                .is_ok_and(|list| list.split(',').any(|name| name == "deinterlace-active")),
            dumb: self.mpv.get_string("gpu-dumb-mode").is_ok_and(|v| v == "yes"),
            macos: cfg!(target_os = "macos"),
            spdif_live: spdif_live(&self.mpv.get_string("mpv-version").unwrap_or_default()),
            compute_peak: false,
        }
    }

    /// 這個引擎有沒有這個 FFmpeg 音訊濾鏡。mpv 加進 af 時就會檢查濾鏡名稱（不用開檔）；
    /// 要用「@標籤:名稱」的寫法，「lavfi=[名稱]」要到建立濾鏡圖時才檢查。偵測完 af 恢復原狀
    pub fn probe_af(&mut self, name: &str) -> bool {
        // 播放中加濾鏡會重建整條音訊濾鏡鏈（聲音會斷一下）
        debug_assert!(!self.state.loaded && !self.state.loading, "probe_af 只能在開檔之前用");
        let before = self.mpv.get_string("af").unwrap_or_default();
        let ok = self
            .mpv
            .command(&["af", "add", &format!("{PROBE_LABEL}:{name}")])
            .is_ok();
        let _ = self.mpv.command(&["af", "remove", PROBE_LABEL]);
        if self.mpv.get_string("af").unwrap_or_default() != before {
            let _ = self.mpv.set_property("af", before.as_str());
        }
        if !ok {
            // 失敗時 mpv 記一筆「Option af-add: 'xxx' isn't supported.」
            // （0.37 是「Option af-add: xxx doesn't exist.」）
            self.probe_noise.push([name.to_owned(), "af-add".to_owned()]);
        }
        ok
    }

    /// deinterlace=auto 能不能用（試設一次，再設回原本的值）
    fn probe_deint_auto(&mut self) -> bool {
        let before = self.mpv.get_string("deinterlace").unwrap_or_else(|_| "no".to_owned());
        let ok = self.mpv.set_property("deinterlace", "auto").is_ok();
        let _ = self.mpv.set_property("deinterlace", before.as_str());
        if !ok {
            // 失敗時 mpv 記一筆「Invalid value for option deinterlace: auto」之類的
            self.probe_noise.push(["deinterlace".to_owned(), "auto".to_owned()]);
        }
        ok
    }

    /// 縮放演算法的預設值（這個引擎的 option-info/…/default-value；讀不到的保留 mpv 文件寫的預設值）
    pub fn picture_defaults(&self) -> crate::picture::PictureDefaults {
        let mut d = crate::picture::PictureDefaults::default();
        for (name, field) in [
            ("scale", &mut d.scale),
            ("dscale", &mut d.dscale),
            ("cscale", &mut d.cscale),
            ("scale-antiring", &mut d.scale_antiring),
        ] {
            if let Ok(v) = self.mpv.get_string(&format!("option-info/{name}/default-value")) {
                *field = v;
            }
        }
        d
    }

    // ───────────── 操作 ─────────────

    pub fn open(&mut self, path: &str) -> mpv::Result<()> {
        self.state.last_error = None;
        let flips = self.shader_flip.clone();
        self.reset_shaders_for_next_file();
        let result = self.mpv.command(&["loadfile", path, "replace"]);
        if result.is_err() && flips.iter().any(Option::is_some) {
            // 沒有換檔：舊檔案照樣在播，翻轉放回去
            self.shader_flip = flips;
            let _ = self.push_shaders(false);
        }
        result
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
        // 往後跳會超過片尾時，精準跳到最後一格：相對跳轉對齊關鍵影格，超過片尾時 mpv 會退回最後一個關鍵影格
        // （可能在很前面），播放中還會從那裡繼續播
        if seconds > 0.0
            && let Some(duration) = self.state.duration
            && self.state.time_pos + seconds >= duration
        {
            return self.mpv.command(&["seek", "100", "absolute-percent+exact"]);
        }
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

    /// 總音量（%）：mpv 的音量 + 經過限幅器放大的部分（見 `set_volume_total`）。
    /// 有放大時 mpv 的音量一定是 100（直接用 100：剛設定完，屬性變化的通知可能還沒到）
    pub fn volume_total(&self) -> f64 {
        if self.boost_pct > 0.0 {
            100.0 + self.boost_pct
        } else {
            self.state.volume
        }
    }

    /// 經過限幅器放大的部分（%；沒有放大或用 mpv 自己的音量放大時是 0）
    pub fn boost_pct(&self) -> f64 {
        self.boost_pct
    }

    /// 設定總音量（0…`cap`，`cap` 是音量上限）。100% 以下就是 mpv 的音量。
    /// 超過 100%：`limiter`（濾鏡鏈裡有限幅器）時 mpv 的音量停在 100、超過的部分記在 `boost_pct`，
    /// 由呼叫的人改限幅器的輸入增益（`sound::limit_command`、改寫 af）；沒有限幅器時用 mpv 自己的音量放大
    ///（把 volume-max 提高到 `cap`；mpv 的音量在濾鏡鏈後面，可能破音）
    pub fn set_volume_total(&mut self, volume: f64, cap: f64, limiter: bool) -> mpv::Result<()> {
        let v = volume.clamp(0.0, cap.max(100.0));
        if v <= 100.0 {
            self.boost_pct = 0.0;
            return self.mpv.set_property("volume", v);
        }
        if limiter {
            self.boost_pct = v - 100.0;
            return self.mpv.set_property("volume", 100.0);
        }
        self.boost_pct = 0.0;
        if self.mpv.get_property::<f64>("volume-max").is_ok_and(|max| max < v) {
            self.mpv.set_property("volume-max", cap)?;
        }
        self.mpv.set_property("volume", v)
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

    /// 翻轉。`use_filter` = 用 vf 濾鏡（軟體繪圖的簡化流程不跑著色器；同步設定，回傳 None）；
    /// 濾鏡在旋轉之前翻，轉了 90° / 270° 時左右、上下要對調
    pub fn set_flip(
        &mut self,
        horizontal: bool,
        on: bool,
        use_filter: bool,
        quarter_turn: bool,
    ) -> mpv::Result<Option<u64>> {
        let label = if horizontal { "@vs-hflip" } else { "@vs-vflip" };
        if use_filter {
            let _ = self.mpv.command(&["vf", "remove", label]);
            if on {
                let filter = if horizontal != quarter_turn { "hflip" } else { "vflip" };
                self.mpv.command(&["vf", "add", &format!("{label}:{filter}")])?;
            }
            return Ok(None);
        }
        // 著色器：接在使用者的組合後面，整個清單重新送（非同步，回傳指令編號；沒有變就是 None）
        let path = if on {
            Some(crate::geometry::flip_shader_path(horizontal).map_err(|e| mpv::Error {
                code: libmpv2_sys::mpv_error_MPV_ERROR_GENERIC,
                context: crate::tf!("無法寫出翻轉用的著色器：{e}", "Cannot write the flip shader: {e}"),
            })?)
        } else {
            None
        };
        self.shader_flip[usize::from(!horizontal)] = path;
        self.push_shaders(false)
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

    /// 載入外部音軌檔（先不切換），回傳新加入的音軌編號（檔案裡有好幾條時是第一條；沒有音軌時 None）。
    /// 切換交給呼叫的人：跟選單換音軌走同一條路，開了音訊直通時才會先預測、清空濾鏡鏈
    pub fn add_audio(&mut self, path: &str) -> mpv::Result<Option<i64>> {
        // 直接讀當下的清單比對（屬性通知是非同步的，可能還沒到）
        self.refresh_tracks();
        let before: HashSet<i64> = self.state.tracks_of(TrackKind::Audio).map(|t| t.id).collect();
        // audio-add 是同步指令：回傳時軌道已經加進清單
        self.mpv.command(&["audio-add", path, "auto"])?;
        self.refresh_tracks();
        Ok(self
            .state
            .tracks_of(TrackKind::Audio)
            .map(|t| t.id)
            .filter(|id| !before.contains(id))
            .min())
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
                let noise = is_harmless_error(&text)
                    || self
                        .probe_noise
                        .iter()
                        .any(|parts| parts.iter().all(|p| text.contains(p.as_str())));
                if matches!(level.as_str(), "error" | "fatal") && !noise {
                    if is_render_log(&prefix) {
                        if self.render_errors.len() >= RENDER_ERRORS_CAP {
                            self.render_errors.pop_front();
                        }
                        self.render_errors
                            .push_back((Instant::now(), format!("[{prefix}] {}", text.trim_end())));
                    }
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
                // 翻轉是每個檔案各自的（通常 `open` 已經拿掉了）；清單跟該有的不一樣、或不確定
                //（開檔前的同步設定可能被晚到的非同步指令蓋掉）就再送一次。非同步指令照順序執行，這次的一定最後生效
                self.shader_flip = [None, None];
                if let Err(e) = self.push_shaders(false) {
                    eprintln!("[vitascope] 無法設定 glsl-shaders：{e}");
                }
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
            Event::CommandReply { id, result } => {
                // glsl-shaders 沒設成功：不知道 mpv 現在的值，下次一定再送
                if self.shaders_inflight.remove(&id) && result.is_err() {
                    self.shaders_applied = None;
                }
                Some(PlayerEvent::CommandReply {
                    id,
                    error: result.err().map(|e| e.to_string()),
                })
            }
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
                self.apply_fake_dolby_vision();
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
            // 關檔時 mpv 會送「不可用」（Value::None）過來，這三項就跟著歸零。
            // 不在 EndFile 自己歸零：換檔時值可能前後一樣，mpv 就不會再通知，狀態會一直是錯的
            "mistimed-frame-count" => s.display_sync_active = value.as_i64().is_some(),
            "deinterlace-active" => s.deinterlace_active = value.as_bool().unwrap_or(false),
            "audio-out-params" => {
                s.audio_spdif = value.as_str().and_then(spdif_format);
                s.audio_out_pcm = s.audio_spdif.is_none() && value.as_str().is_some_and(has_out_format);
            }
            "video-params/gamma" => s.video_hdr = value.as_str().is_some_and(is_hdr_gamma),
            "current-ao" => s.current_ao = value.as_str().filter(|v| !v.is_empty()).map(str::to_owned),
            // 讀不到（Value::None）時保留上一次的清單
            "audio-device-list" if !self.fake_devices => {
                if let Some(json) = value.as_str() {
                    s.audio_devices = Some(crate::sound::parse_devices(json));
                }
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
            self.apply_fake_dolby_vision();
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
    use super::{
        ASYNC_BASE, AsyncKey, State, Track, TrackKind, async_id, async_key, debug_log_level, display_size,
        env_option_names, env_options, has_out_format, is_harmless_error, is_hdr_gamma, is_render_log, mpv_version,
        picture_key, sound_key, spdif_format, spdif_live,
    };

    #[test]
    fn async_ids_round_trip() {
        for (i, k) in AsyncKey::ALL.into_iter().enumerate() {
            // 編號從 1 開始連續（0 留給「不是設定送的」）
            assert_eq!(k as usize, i + 1, "{k:?}");
            for seq in [0, 1, 0x7F_FFFF, 0xFF_FFFF, 0x100_0000, u64::MAX] {
                let id = async_id(k, seq);
                assert_eq!(async_key(id), Some(k), "{k:?} {seq:#x}");
                assert!((ASYNC_BASE..ASYNC_BASE << 1).contains(&id), "{id:#x}");
            }
        }
        // 流水號溢位不會影響種類
        assert_eq!(async_id(AsyncKey::Af, 0x100_0005), async_id(AsyncKey::Af, 5));
    }

    #[test]
    fn unknown_and_out_of_range_ids_are_not_async_keys() {
        let unknown_kind = ASYNC_BASE | (AsyncKey::ALL.len() as u64 + 1) << 24;
        for id in [
            0,
            1,
            ASYNC_BASE - 1,
            // 種類 0
            ASYNC_BASE,
            ASYNC_BASE | 5,
            unknown_kind,
            // 種類的欄位超過 u16
            ASYNC_BASE | 0x1_0001 << 24,
            // 第 44 位元以上還有別的位元
            ASYNC_BASE << 1 | (AsyncKey::Af as u64) << 24,
            (1 << 45) | ASYNC_BASE | (AsyncKey::Af as u64) << 24,
            u64::MAX,
            // 截圖的編號（1<<40 起算）
            (1 << 40) + 1,
            (1 << 41) - 1,
        ] {
            assert_eq!(async_key(id), None, "{id:#x}");
        }
    }

    #[test]
    fn harmless_nvdec_fallbacks_are_recognised() {
        assert!(is_harmless_error("filter bwdif_cuda: initialization failed\n"));
        assert!(is_harmless_error(
            "Disabling filter bwdif_cuda because it has FAILED.\n"
        ));
        assert!(is_harmless_error("creating deinterlacer failed\n"));
        assert!(!is_harmless_error("bwdif_cuda: using CUDA 12\n"), "沒有失敗就不是");
        assert!(!is_harmless_error("Failed to open file\n"));
        assert!(!is_harmless_error("shader compile failed: hook.glsl\n"));
        assert!(is_render_log("libmpv_render"));
        assert!(is_render_log("vo/gpu"));
        assert!(is_render_log("vo/libmpv/opengl"));
        assert!(!is_render_log("ffmpeg"));
        assert!(!is_render_log("cplayer"));
    }

    #[test]
    fn spdif_format_from_audio_out_params() {
        assert_eq!(
            spdif_format(r#"{"samplerate":48000,"channel-count":2,"format":"spdif-ac3"}"#).as_deref(),
            Some("ac3")
        );
        // mpv 的名稱是 spdif-dtshd（audio/format.c），不是 audio-spdif 選項的 dts-hd
        assert_eq!(spdif_format(r#"{"format":"spdif-dtshd"}"#).as_deref(), Some("dtshd"));
        assert_eq!(spdif_format(r#"{"format":"floatp"}"#), None);
        assert_eq!(spdif_format(r#"{"samplerate":48000}"#), None);
        assert_eq!(spdif_format("not json"), None);
        // 一般的 PCM 輸出（直通沒發生時，app 依這個把濾鏡鏈設回來）
        assert!(has_out_format(r#"{"samplerate":48000,"format":"floatp"}"#));
        assert!(has_out_format(r#"{"format":"spdif-ac3"}"#));
        assert!(!has_out_format(r#"{"samplerate":48000}"#));
        assert!(!has_out_format(r#"{"format":""}"#));
        assert!(!has_out_format("not json"));
    }

    #[test]
    fn hdr_gamma_and_picture_keys() {
        assert!(is_hdr_gamma("pq") && is_hdr_gamma("hlg"));
        assert!(!is_hdr_gamma("bt.1886") && !is_hdr_gamma("srgb") && !is_hdr_gamma(""));
        // 畫質選項的回覆依種類分派：每個選項都要歸到它那一組
        let keys: Vec<AsyncKey> = crate::picture::MANAGED.into_iter().map(picture_key).collect();
        use AsyncKey::*;
        assert_eq!(
            keys,
            [
                Deinterlace,
                Deband,
                Deband,
                Deband,
                Deband,
                Deband,
                Sharpen,
                Scaler,
                Scaler,
                Scaler,
                Scaler,
                Tone,
                Tone,
                Tone,
                Tone
            ]
        );
    }

    #[test]
    fn sound_keys() {
        // 音效選項的回覆依種類分派：每個選項都要歸到它那一組
        let keys: Vec<AsyncKey> = crate::sound::MANAGED.into_iter().map(sound_key).collect();
        use AsyncKey::*;
        assert_eq!(keys, [AudioDevice, Exclusive, Downmix, Downmix, Spdif]);
    }

    #[test]
    fn spdif_is_live_from_mpv_0_41() {
        assert_eq!(mpv_version("mpv 0.37.0"), Some((0, 37)));
        assert_eq!(mpv_version("mpv v0.41.0-1102-g6c092d978"), Some((0, 41)));
        assert_eq!(mpv_version("mpv 0.40"), Some((0, 40)));
        assert_eq!(mpv_version("mpv git-2024"), None);
        assert_eq!(mpv_version("libmpv 1.0"), None);
        assert!(!spdif_live("mpv 0.37.0"));
        assert!(!spdif_live("mpv 0.40.0"));
        assert!(spdif_live("mpv v0.41.0-1102-g6c092d978"));
        assert!(spdif_live("mpv 1.0.0"));
        assert!(spdif_live("mpv git-2024"), "看不懂的當成新版");
    }

    #[test]
    fn device_list_is_not_observed_at_startup() {
        // 裝置清單要用時才觀察（列舉裝置慢），不能放在一開始就觀察的清單裡
        assert!(super::OBSERVED.iter().all(|(name, _)| *name != "audio-device-list"));
    }

    #[test]
    fn env_options_skip_items_without_a_value() {
        let opts: Vec<_> = env_options(" vo-null-fps=120  bogus scale=ewa_lanczossharp af= ").collect();
        assert_eq!(
            opts,
            [("vo-null-fps", "120"), ("scale", "ewa_lanczossharp"), ("af", "")]
        );
        // 沒有值的項目 mpv 收不到，但還是算使用者自己處理的選項（跟加入 Options.extra 之前一樣）
        let names: Vec<_> = env_option_names(" vo-null-fps=120  gpu-dumb-mode scale=ewa_lanczossharp af= ").collect();
        assert_eq!(names, ["vo-null-fps", "gpu-dumb-mode", "scale", "af"]);
    }

    #[test]
    fn error_logs_are_tapped_and_noise_is_dropped() {
        use crate::mpv::Event;
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let log = |prefix: &str, level: &str, text: &str| Event::Log {
            prefix: prefix.into(),
            level: level.into(),
            text: text.into(),
        };
        // 畫面輸出的錯誤：兩邊都收
        p.handle(log("libmpv_render", "error", "shader compile failed: a.glsl\n"));
        p.handle(log("vo/gpu", "fatal", "Could not create shader\n"));
        // 不是錯誤、不是畫面輸出的：不進 render_errors
        p.handle(log("vo/gpu", "warn", "just a warning\n"));
        p.handle(log("ffmpeg", "error", "decoder broke\n"));
        // nvdec 的去交錯退回軟體：沒有害處，兩邊都不收
        p.handle(log("vf", "error", "filter bwdif_cuda failed\n"));
        p.handle(log("autoconvert", "error", "creating deinterlacer failed\n"));
        // 偵測引擎功能留下的記錄
        p.probe_noise.push(["nope".into(), "af-add".into()]);
        p.handle(log("cplayer", "error", "Option af-add: 'nope' isn't supported.\n"));
        // mpv 0.37 的寫法
        p.handle(log("cplayer", "error", "Option af-add: nope doesn't exist.\n"));
        let render: Vec<String> = p.take_render_errors().into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            render,
            [
                "[libmpv_render] shader compile failed: a.glsl",
                "[vo/gpu] Could not create shader"
            ]
        );
        assert!(p.take_render_errors().is_empty(), "取出後就清空");
        assert_eq!(
            p.recent_errors(),
            [
                "[libmpv_render] shader compile failed: a.glsl",
                "[vo/gpu] Could not create shader",
                "[ffmpeg] decoder broke"
            ]
        );
        // 最多留 16 筆（留最新的）
        for i in 0..20 {
            p.handle(log("libmpv_render", "error", &format!("e{i}\n")));
        }
        let render = p.take_render_errors();
        assert_eq!(render.len(), 16);
        assert_eq!(render[0].1, "[libmpv_render] e4");
        assert_eq!(render[15].1, "[libmpv_render] e19");
    }

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
