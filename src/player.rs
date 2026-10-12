//! 播放器核心：把 mpv 的屬性與事件整理成 Rust 狀態。
//! 介面（app）和自動測試（tests/）都透過這一層操作 mpv。

use crate::approx_seek::{self, Check, Kind, Landing, Verdict};
use crate::inhibit::StopScreensaver;
use crate::mpv::{self, EndReason, Event, Format, Mpv, Node, Value};
use crate::net;
use crate::subs::{self, ExternalSub, SubLang};
use crate::ytdl::hook::{self, Job, Progress, Route};
use crate::ytdl::plan::{self, ChapterMark, Choice, MediaPlan, Mode, NetInfo, Plan, PlanConfig};
use crate::ytdl::{self, Failure, Hint, Request, Resolve, Resolved, YtdlError};
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
    /// 註冊網路功能的 hook（開檔前 on_load、開檔失敗 on_load_fail；網站影片用它換成 yt-dlp 取得的網址）。
    /// 播放器介面要；headless 的自動測試預設不要（每個 hook 要等播放器處理一輪事件才放行，開檔的時間會不一樣）
    pub net_hooks: bool,
    /// 網站影片用的 yt-dlp（`net_hooks` 開著才用）；None = 沒有 yt-dlp（影片網站的網址說明要 yt-dlp）。
    /// 播放器介面用 `ytdl::ProcessResolver`，自動測試用假的（不執行程式、不連網）
    pub net_resolver: Option<Arc<dyn Resolve>>,
    /// 測試用：額外當成影片網站的主機（先問 yt-dlp；自動測試用本機的伺服器 127.0.0.1）
    #[doc(hidden)]
    pub net_sites: Vec<String>,
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
            net_hooks: true,
            net_resolver: None,
            net_sites: Vec::new(),
        }
    }
}

impl Options {
    pub fn headless() -> Self {
        Self {
            headless: true,
            hwdec: "no".into(),
            keep_open: false,
            net_hooks: false,
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

/// mpv 記錄裡沒有編碼名稱時（`<?>`）、軌道清單沒有 codec 時，當成這個名稱
const UNKNOWN_CODEC: &str = "<?>";

/// 軌道的編碼名稱（沒有時是 [`UNKNOWN_CODEC`]，跟 mpv 記錄裡的寫法一樣）
fn track_codec(t: &Track) -> &str {
    t.codec.as_deref().filter(|c| !c.is_empty()).unwrap_or(UNKNOWN_CODEC)
}

/// FFmpeg 沒有（或播放引擎沒有建置）解碼器的字幕格式：開檔自動選字幕時跳過，不然每次開檔都選到、
/// 每次都失敗（「這條字幕的格式影戲不支援」）。使用者自己選還是可以選。
/// - dvb_teletext：要 libzvbi（有 GPL 的部分，引擎不收；有的系統 FFmpeg 有，但自動選字幕一樣跳過，各平台一致）
/// - arib_caption：要 libaribcaption（引擎沒有建置）
/// - ttml（MP4 裡的 stpp）、hdmv_text_subtitle（藍光的文字字幕）：FFmpeg 沒有解碼器
const KNOWN_UNDECODABLE_SUBS: [&str; 4] = ["dvb_teletext", "arib_caption", "ttml", "hdmv_text_subtitle"];

/// 解不了的編碼：mpv 開不起這條軌道的解碼器（記錄裡的「Failed to initialize a decoder for codec …」、
/// 「Could not find subtitle decoder for format …」）。mpv 把這條軌道關掉，其他的照常播
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingDecoder {
    pub kind: TrackKind,
    /// mpv 的編碼名稱（`avs2`、`h264`、`dvb_teletext`…；不知道時是 `<?>`）
    pub codec: String,
    /// 播放引擎根本沒有這個編碼的解碼器（`decoder-list` 裡沒有；字幕一律是）。
    /// false = 有解碼器、但開不起來：檔案可能損壞，或用了解碼器不支援的設定（profile、參數）
    pub unsupported: bool,
    /// 解不了的是專輯封面（不是影片）
    pub cover: bool,
    /// 不提示（媒體資訊照樣標）：解不了的字幕是自動選的（mpv 選了標成預設的、影戲開檔時選的），
    /// 不是使用者自己選的。不然這種檔案每次開都提示一次
    pub quiet: bool,
}

impl MissingDecoder {
    /// 給人看的編碼名稱（「AVS2」「H.264」）
    pub fn name(&self) -> String {
        codec_display_name(&self.codec)
    }

    /// 媒體資訊上的一行：「AVS2（不支援）」「H.264（無法解碼）」
    pub fn label(&self) -> String {
        let name = self.name();
        if self.unsupported {
            crate::tf!("{name}（不支援）", "{name} (unsupported)")
        } else {
            crate::tf!("{name}（無法解碼）", "{name} (can't decode)")
        }
    }

    /// 播放中的提示（OSD）。`rest_plays`：其他的照常播（影像解不了時有聲音、聲音解不了時有影像）
    pub fn notice(&self, rest_plays: bool) -> String {
        let name = self.name();
        match (self.kind, self.unsupported, rest_plays) {
            (TrackKind::Video, true, true) => crate::tf!(
                "這個影片的影像編碼（{name}）影戲不支援，只播放聲音",
                "This video's codec ({name}) isn't supported; playing the audio only"
            ),
            (TrackKind::Video, true, false) => crate::tf!(
                "這個影片的影像編碼（{name}）影戲不支援",
                "This video's codec ({name}) isn't supported"
            ),
            (TrackKind::Video, false, true) => crate::tf!(
                "無法解碼 {name} 影像（檔案可能損壞，或用了引擎不支援的設定），只播放聲音",
                "Can't decode the {name} video (the file may be damaged or use settings the engine doesn't support); playing the audio only"
            ),
            (TrackKind::Video, false, false) => crate::tf!(
                "無法解碼 {name} 影像（檔案可能損壞，或用了引擎不支援的設定）",
                "Can't decode the {name} video (the file may be damaged or use settings the engine doesn't support)"
            ),
            (TrackKind::Audio, true, true) => crate::tf!(
                "這個檔案的音訊編碼（{name}）影戲不支援，只播放影像",
                "This file's audio codec ({name}) isn't supported; playing the video only"
            ),
            (TrackKind::Audio, true, false) => crate::tf!(
                "這個檔案的音訊編碼（{name}）影戲不支援",
                "This file's audio codec ({name}) isn't supported"
            ),
            (TrackKind::Audio, false, true) => crate::tf!(
                "無法解碼 {name} 聲音（檔案可能損壞，或用了引擎不支援的設定），只播放影像",
                "Can't decode the {name} audio (the file may be damaged or use settings the engine doesn't support); playing the video only"
            ),
            (TrackKind::Audio, false, false) => crate::tf!(
                "無法解碼 {name} 聲音（檔案可能損壞，或用了引擎不支援的設定）",
                "Can't decode the {name} audio (the file may be damaged or use settings the engine doesn't support)"
            ),
            _ => crate::tf!(
                "這條字幕的格式（{name}）影戲不支援",
                "This subtitle format ({name}) isn't supported"
            ),
        }
    }
}

/// 整個檔案因為解不了而開不起來時的說明：引擎沒有的編碼優先（「不支援的編碼：AVS2」），
/// 都有解碼器的話是「無法解碼 H.264（…）」。只看影像、聲音（字幕解不了不會讓檔案開不起來）；沒有的話 None
fn decoder_failure(missing: &[MissingDecoder]) -> Option<String> {
    let av: Vec<&MissingDecoder> = missing.iter().filter(|m| m.kind != TrackKind::Sub).collect();
    let names = |unsupported: bool| {
        let mut names: Vec<String> = av
            .iter()
            .filter(|m| m.unsupported == unsupported)
            .map(|m| m.name())
            .collect();
        names.dedup();
        names.join(crate::tr!("、", ", "))
    };
    if av.iter().any(|m| m.unsupported) {
        let names = names(true);
        Some(crate::tf!("不支援的編碼：{names}", "Unsupported codec: {names}"))
    } else if !av.is_empty() {
        let names = names(false);
        Some(crate::tf!(
            "無法解碼 {names}（檔案可能損壞，或用了引擎不支援的設定）",
            "Can't decode {names} (the file may be damaged or use settings the engine doesn't support)"
        ))
    } else {
        None
    }
}

/// mpv 的解碼器錯誤記錄 →（軌道種類, 編碼名稱）。種類看記錄的來源：`vd` 影像、`ad` 聲音；
/// 其他來源（現在的 mpv 沒有）是 None，由呼叫的一方查軌道清單。不是這兩種記錄時 None
/// - `[vd] Failed to initialize a decoder for codec 'avs2'.`（filters/f_decoder_wrapper.c，0.37 起都一樣）
/// - `[sub] Could not find subtitle decoder for format 'dvb_teletext'.`（sub/dec_sub.c）
fn missing_decoder(prefix: &str, text: &str) -> Option<(Option<TrackKind>, String)> {
    // 分離器認不出編碼時名稱是空的（FFmpeg 6.1 的 MP4 裡的 VVC 之類）：跟 `<?>` 一樣當成不知道
    let quoted = |head: &str| {
        let rest = text.trim_end().strip_prefix(head)?.strip_suffix("'.")?;
        Some(if rest.is_empty() { UNKNOWN_CODEC } else { rest }.to_owned())
    };
    if let Some(codec) = quoted("Failed to initialize a decoder for codec '") {
        let kind = match prefix {
            "vd" => Some(TrackKind::Video),
            "ad" => Some(TrackKind::Audio),
            _ => None,
        };
        return Some((kind, codec));
    }
    quoted("Could not find subtitle decoder for format '").map(|codec| (Some(TrackKind::Sub), codec))
}

/// 自動選字幕時解得了的字幕軌：不是 FFmpeg 解不了的格式（[`KNOWN_UNDECODABLE_SUBS`]），
/// 也不是這個播放器之前解不了的（`learned`）
fn auto_sub_decodable(t: &Track, learned: &HashSet<String>) -> bool {
    let codec = track_codec(t);
    !KNOWN_UNDECODABLE_SUBS.contains(&codec) && !learned.contains(codec)
}

/// 開檔後自動選字幕要改選的那條（None = 照 mpv 的選擇）：
/// - 有繁中就選繁中（其次中文、簡中），依語言碼、標題判斷；同語言時外掛優先，再來是軌道順序
/// - 沒有中文字幕時尊重 mpv 的選擇；mpv 什麼都沒選時選第一條
/// - 解不了的格式（[`auto_sub_decodable`]）不選；mpv 自己選了解不了的（標成預設的 Teletext 之類）當成沒選
fn pick_auto_sub<'a>(subs: &[&'a Track], learned: &HashSet<String>) -> Option<&'a Track> {
    let lang_of = |t: &Track| {
        let label = format!(
            "{} {}",
            t.lang.as_deref().unwrap_or(""),
            t.title.as_deref().unwrap_or("")
        );
        subs::classify_label(&label).unwrap_or(SubLang::Unknown)
    };
    let decodable = |t: &Track| auto_sub_decodable(t, learned);
    // 語言最優先；同語言時外掛優先（字幕組另外附的通常比內嵌的好）；再來是軌道順序
    let best = subs
        .iter()
        .filter(|t| !t.forced && decodable(t))
        .max_by_key(|t| (lang_of(t), t.external, std::cmp::Reverse(t.id)))
        .or_else(|| subs.iter().find(|t| decodable(t)))
        .copied();
    let current = subs.iter().find(|t| t.selected && decodable(t));
    match (current, best) {
        (None, Some(b)) => Some(b),
        (Some(c), Some(b)) if lang_of(b).is_chinese() && lang_of(b) > lang_of(c) => Some(b),
        _ => None,
    }
}

/// 編碼名稱 → 給人看的名稱（「H.264」「AVS2」「Teletext（圖文電視）」）；不在表上的照 mpv 的名稱轉大寫
pub fn codec_display_name(codec: &str) -> String {
    match codec {
        "h264" => "H.264",
        "hevc" => "HEVC",
        "vvc" => "H.266 / VVC",
        "av1" => "AV1",
        "vp9" => "VP9",
        "avs2" => "AVS2",
        "avs3" => "AVS3",
        "cavs" => "AVS",
        "evc" => "MPEG-5 EVC",
        "apv" => "APV",
        "ac4" => "AC-4",
        "mpegh_3d_audio" => "MPEG-H 3D Audio",
        "codec2" => "Codec 2",
        "lc3" => "LC3",
        "ttml" => "TTML",
        "dvb_teletext" => crate::tr!("Teletext（圖文電視）", "Teletext"),
        "arib_caption" => crate::tr!("ARIB 字幕", "ARIB captions"),
        "hdmv_text_subtitle" => crate::tr!("藍光文字字幕", "Blu-ray text subtitles"),
        "" | "null" | UNKNOWN_CODEC => crate::tr!("未知的編碼", "unknown codec"),
        other => return other.to_uppercase(),
    }
    .to_owned()
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
    /// 網路串流的快取不夠、暫停等待中（paused-for-cache）
    pub paused_for_cache: bool,
    /// 快取等待的進度（0–100 %，cache-buffering-state；不在等待時是 100）；沒有開檔時 None
    pub cache_buffering: Option<i64>,
    /// 網站影片（yt-dlp 解析出來的）：標題、網站、畫質選單、續播的代號…；不是網站影片時 None（開新檔時清掉）
    pub net: Option<Arc<NetInfo>>,
    /// 正在等 yt-dlp 解析網址（連線中的畫面顯示「正在取得網站影片」）
    pub net_busy: Option<hook::NetBusy>,
    /// 影片網站的網址，但沒有 yt-dlp（起始畫面說明怎麼取得）
    pub net_need_ytdl: bool,
    /// 其他網頁（不是已知的影片網站）打不開、又沒有 yt-dlp：說明裡提一句要 yt-dlp，起始畫面也放「網路設定…」
    pub net_page_needs_ytdl: bool,
    /// 網站影片播不了的原因（跟 `last_error` 同時設定）：介面每次畫的時候才轉成文字（目前的語言），旁邊列出提醒與建議
    pub net_failure: Option<Failure>,
    /// yt-dlp 成功了、但警告裡看得出的提醒（沒有 deno 時 YouTube 只有部分畫質之類）
    pub net_hints: Vec<Hint>,
    /// 純音訊檔（沒有影像，可以有專輯封面）正在顯示的字幕文字（LRC 歌詞之類），介面畫在歌名下面；
    /// 沒有字幕、兩句之間、有影像的檔案都是 None（只在純音訊檔時才向 mpv 要，見 `SUB_TEXT_ID`）
    pub sub_text: Option<String>,
    /// 這個檔案解不了的編碼（mpv 關掉了那條軌道，其他的照常播；見 [`MissingDecoder`]）。開新檔時清掉
    pub missing_decoders: Vec<MissingDecoder>,
}

impl State {
    /// 這條軌道解得了：這個檔案還沒有因為解不了而被 mpv 關掉
    pub fn decodable(&self, t: &Track) -> bool {
        !self
            .missing_decoders
            .iter()
            .any(|m| m.kind == t.kind && m.codec == track_codec(t))
    }

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

    /// 有實際影像（不是只有專輯封面）。影像解不了（mpv 關掉了影片軌，只播聲音）的不算：介面當成純音訊檔
    pub fn has_video(&self) -> bool {
        self.tracks_of(TrackKind::Video)
            .any(|t| !t.albumart && self.decodable(t))
    }

    /// 有畫面可畫（影片或專輯封面，而且解得了）
    pub fn has_picture(&self) -> bool {
        self.tracks_of(TrackKind::Video).any(|t| self.decodable(t))
    }

    /// 歌詞（目前的字幕）由介面畫在歌名下面、mpv 自己的那份藏起來（見 `Player::watch_lyrics`）。
    /// 只限純音訊檔，而且：
    /// - 純文字的字幕（LRC、SRT 之類）：一律由介面畫；
    /// - ASS／SSA：沒有封面時由介面畫成純文字（mpv 沒有畫面可畫），有封面時照舊交給 mpv（保留樣式、位置、卡拉 OK 效果）；
    /// - 圖片字幕（PGS、VobSub…）與不認得的：交給 mpv（它們沒有文字可畫）
    pub fn lyrics_in_app(&self) -> bool {
        if self.tracks.is_empty() || self.has_video() {
            return false;
        }
        let Some(codec) = self.selected(TrackKind::Sub).and_then(|t| t.codec.as_deref()) else {
            return false;
        };
        // 解不了的封面不算（mpv 沒有畫面可畫，ASS 歌詞也要由介面畫）
        let has_cover = self
            .tracks_of(TrackKind::Video)
            .any(|t| t.albumart && self.decodable(t));
        is_plain_text_sub(codec) || (!has_cover && matches!(codec, "ass" | "ssa"))
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
    /// 網站的網址是播放清單（yt-dlp 解析出來的）：（網址, 標題）照順序，從第 `start` 個開始播。
    /// 這個網址本身已經安靜地停下（結束的原因是 stop、沒有錯誤）；介面把項目變成自己的播放清單、照一般的開檔開要播的那一個
    NetPlaylist {
        /// 播放清單本身的網址（使用者開的）
        source: String,
        entries: Vec<(String, Option<String>)>,
        start: usize,
        /// `start` 是網址指定的那一部（`v=`）：隨機播放時也從它開始（見 `ytdl::plan::Plan::Playlist`）
        chosen: bool,
        /// 略過了幾個項目（不能開的）
        dropped: usize,
        /// yt-dlp 只讀了前 [`ytdl::PLAYLIST_MAX`] 個（清單可能更長）
        capped: bool,
    },
    /// 這個檔案有一條軌道解不了（同一個檔案、同一種軌道的同一個編碼只送一次；`state.missing_decoders` 也記著）。
    /// mpv 的記錄比一般事件晚到，可能在 FileLoaded 之前或之後，也可能在整個檔案開不起來的 EndFile 之後
    DecoderMissing(MissingDecoder),
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
    // 網路串流：快取不夠時的暫停、等待的進度（緩衝中的畫面用）
    ("paused-for-cache", Format::Flag),
    ("cache-buffering-state", Format::Int64),
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
    Net,
    LoopFile,
    Cover,
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
    /// 匯出片段（不重新編碼）：`dump-cache` 指令（command-list）
    pub dump_cache: bool,
    /// 匯出片段時把起點、終點對齊關鍵影格：`ab-loop-align-cache` 指令（command-list）
    pub align_cache: bool,
    /// 轉成 GIF：有 gif 編碼器（encoder-list）
    pub gif: bool,
    /// 轉成 GIF、縮圖總覽圖的 HDR 轉一般畫面：zscale 與 tonemap 濾鏡
    pub zscale: bool,
    pub tonemap: bool,
    /// GIF 自己轉正（旋轉要在色盤之前做）：transpose 濾鏡
    pub transpose: bool,
    /// GIF 的色盤：palettegen 濾鏡（paletteuse 不能單獨偵測，有 palettegen 的 FFmpeg 都有）
    pub palettegen: bool,
}

/// mpv 的版本（`mpv-version`：「mpv 0.37.0」「mpv v0.41.0-1102-g6c092d978」）→（主版本, 次版本）；看不懂時 None
fn mpv_version(text: &str) -> Option<(u32, u32)> {
    let v = text.strip_prefix("mpv ")?.trim_start_matches('v');
    let mut parts = v.split(['.', '-']);
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// 這個版本讀不讀得了 `memory://` 的 Cookie 檔（0.38 起 Cookie 檔不限本機檔案）。看不懂的版本字串當成新版
fn memory_cookies(version: &str) -> bool {
    mpv_version(version).is_none_or(|v| v >= (0, 38))
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

/// 錯誤記錄最多留幾筆
const RECENT_ERRORS_CAP: usize = 8;

/// 記下一筆錯誤（最多 [`RECENT_ERRORS_CAP`] 筆，留最新的）。FFmpeg 的記錄（`[ffmpeg…]`）跟 mpv 自己的分開算：
/// FFmpeg 的記錄一律送到行程裡第一個建立的 mpv（見設計 §1.9），匯出、其他播放器解碼的錯誤也會進來，
/// 一大串的話會把這個檔案 mpv 自己的錯誤（音訊輸出、解碼器開不起來…）擠掉。滿了的時候，
/// 同一類已經佔了一半就丟掉同一類最舊的那筆，不然丟掉另一類最舊的（每一類至少留得住一半）
fn push_recent_error(list: &mut Vec<String>, entry: String) {
    if list.len() >= RECENT_ERRORS_CAP {
        let is_ffmpeg = |e: &str| e.starts_with("[ffmpeg");
        let class = is_ffmpeg(&entry);
        let same = list.iter().filter(|e| is_ffmpeg(e) == class).count();
        let drop_class = if same >= RECENT_ERRORS_CAP / 2 { class } else { !class };
        let drop = list.iter().position(|e| is_ffmpeg(e) == drop_class).unwrap_or(0);
        list.remove(drop);
    }
    list.push(entry);
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

/// 觀察 sub-text 用的編號：只在純音訊檔播放時觀察（歌詞），關檔時停止。
/// 不放進 OBSERVED：mpv 每一格都重新讀 sub-text 比較有沒有變，ASS 字幕要掃過全部的字幕事件
///（特效字幕有好幾萬筆），每部有字幕的影片都多這份工作會影響播放的流暢
const SUB_TEXT_ID: u64 = OBSERVED.len() as u64 + 2;

/// 純文字的字幕編碼（mpv 軌道清單的 codec）：sub-text 就是畫面上的字，介面畫得出一樣的內容。
/// ASS／SSA 有樣式、位置、特效，圖片字幕（PGS、VobSub…）沒有文字，都不在這裡（見 `State::lyrics_in_app`）
fn is_plain_text_sub(codec: &str) -> bool {
    matches!(
        codec,
        "text"
            | "subrip"
            | "srt"
            | "webvtt"
            | "mov_text"
            | "mpl2"
            | "microdvd"
            | "subviewer"
            | "subviewer1"
            | "jacosub"
            | "realtext"
            | "pjs"
            | "vplayer"
            | "sami"
            | "stl"
            | "eia_608"
            | "ttml"
    )
}

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
/// 先登記一份程式層級的 MTA 使用（不再減回去），mpv 收掉的就只是它自己那一份。
/// 系統媒體控制的背景執行緒也靠這一份當隱含的 MTA（`media_keys::windows`；沒有播放器的測試也會先呼叫）
#[cfg(windows)]
pub(crate) fn keep_com_mta_alive() {
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

/// mpv 的記錄等級，從少到多
const LOG_LEVELS: [&str; 7] = ["fatal", "error", "warn", "info", "v", "debug", "trace"];

/// VITASCOPE_DEBUG 的值 → 要印出的記錄等級。
/// 沒設定只收錯誤；1 之類的值 = 警告與錯誤（印到 stderr，排查顯示卡、驅動之類的問題）；
/// 也可以直接寫 mpv 的記錄等級，例如 VITASCOPE_DEBUG=v。看不懂的值當成 1，除錯設定不能讓播放器開不起來
fn debug_log_level(value: Option<&str>) -> &'static str {
    match value {
        None => "error",
        Some(v) => LOG_LEVELS.into_iter().find(|level| *level == v).unwrap_or("warn"),
    }
}

/// 收集錯誤和網路警告最少要的記錄等級：至少要警告。FFmpeg 的「HTTP error 404 Not Found」是警告，
/// 網址開不了時靠它說明原因（只留下 `is_net_warning` 的那幾種，其他警告照舊不收）；VITASCOPE_DEBUG 要更多時照它的。
/// 實際向 mpv 要的是 [`requested_log_level`]（再提高到 v）
fn client_log_level(debug: Option<&str>) -> &'static str {
    let wanted = debug_log_level(debug);
    if log_rank(wanted) >= log_rank("warn") {
        wanted
    } else {
        "warn"
    }
}

/// 主播放器向 mpv 要的記錄等級：至少 v。跳轉不準的格式（TS、MPEG-PS）要讀 mpv 的「跳轉完成」記錄
/// 核對跳轉停在哪裡（見 `approx_seek`）。中途改等級的話 mpv 會丟掉還沒送出的記錄（開檔失敗的原因之類），
/// 所以一開始就要。正常播放時 v 等級幾乎沒有記錄，只有開檔、跳轉、改設定時有幾筆
fn requested_log_level(debug: Option<&str>) -> &'static str {
    let wanted = client_log_level(debug);
    if log_rank(wanted) >= log_rank("v") { wanted } else { "v" }
}

/// 記錄等級的順序（越大越詳細）；不認得的當成最詳細
fn log_rank(level: &str) -> usize {
    LOG_LEVELS.iter().position(|l| *l == level).unwrap_or(LOG_LEVELS.len())
}

/// 要收進錯誤記錄的警告：FFmpeg 的 HTTP 錯誤（伺服器回應 4xx / 5xx）
fn is_net_warning(prefix: &str, text: &str) -> bool {
    prefix.starts_with("ffmpeg") && text.contains("HTTP error ")
}

/// 網路功能的 hook（`Options.net_hooks`）在事件裡的編號（mpv 的 reply_userdata）
const HOOK_ON_LOAD: u64 = 1;
const HOOK_ON_LOAD_FAIL: u64 = 2;
/// hook 的優先順序（小的先跑）：跟 mpv 自己的 ytdl_hook 一樣
const HOOK_PRIORITY: i32 = 10;
/// 開檔失敗、讓 mpv 馬上結束的網址（空的資料：不會再去讀一次網頁）
const FAIL_FAST: &str = "memory://";

fn hook_name(after_failure: bool) -> &'static str {
    if after_failure { "on_load_fail" } else { "on_load" }
}

/// 網站影片（yt-dlp）的狀態，見 `ytdl::hook` 與下面「網站影片」一節
#[derive(Default)]
struct Site {
    resolver: Option<Arc<dyn Resolve>>,
    /// 額外當成影片網站的主機（測試用）
    extra_sites: Vec<String>,
    /// 解析用的網路設定與偏好（`set_net_config`）
    net: net::NetSettings,
    prefs: ytdl::SitePrefs,
    /// 正在背景解析（mpv 停在 hook 等）
    job: Option<Job>,
    /// 開不起來（`on_load_fail`）的 hook：要不要問 yt-dlp 看 mpv 的錯誤記錄，但記錄比 hook 晚送到，
    /// 要等事件都處理完才決定（`net_tick`）
    deferred_fail: Option<u64>,
    /// 下一次開檔只對那次有效的要求（網址, 要求）：`open_with_mode`
    mode: Option<(String, Mode)>,
    cache: hook::Cache,
    /// 目前（或剛結束）這個檔案的狀態
    file: SiteFile,
    /// mpv 拿掉的 hook（事件佇列滿了送不出去時）：下一次 `net_tick` 重新註冊（hook 的編號, 名稱）
    lost_hooks: Vec<(u64, &'static str)>,
    /// 重新註冊過幾次（自動測試用）
    hooks_readded: u64,
    /// 最近處理過 `on_load` 的播放清單項目（mpv 給的代號，每次 loadfile 都不一樣）：
    /// 同一個 hook 註冊了兩次時（舊的 libmpv 看不出拿掉的是哪一個，兩個都重新註冊），第二次收到的直接放行
    load_entry: Option<i64>,
    /// 這個檔案用 `file-local-options` 設定的選項名稱（檔案結束時 mpv 還原成之前的值）
    file_local: Vec<String>,
    /// 有 file-local 的選項時又改了網路設定：mpv 在檔案結束時會把它還原成舊的值，結束時要再送一次
    net_changed: bool,
    /// 最近一次要套用的網路選項（`apply_net`）
    wanted: Option<net::NetOptions>,
}

/// 一個檔案的網站影片狀態（開新檔時重來）
#[derive(Default)]
struct SiteFile {
    /// 網頁的網址（mpv 的 path）
    url: String,
    route: Option<Route>,
    mode: Mode,
    /// 已經問過 yt-dlp（或正在問）
    tried: bool,
    /// 播不了的原因（說明開檔失敗時優先用）
    failure: Option<Failure>,
    /// mpv 認不出內容（是網頁），想問 yt-dlp 但沒有 yt-dlp：說明裡提一句
    missing: bool,
    /// 載入後要設定的章節
    chapters: Vec<ChapterMark>,
    /// 網站影片：mpv 實際開的（解析出來的網址或 EDL）與連線用的 file-local 選項（`net_stream`）
    stream: Option<(String, Vec<(String, Node)>)>,
}

/// 另一個 mpv（匯出片段、GIF）開同一個網路串流要的東西（`Player::net_stream`）
#[derive(Debug, Clone, PartialEq)]
pub struct NetStream {
    /// 使用者開的網址（網站影片是網頁）
    pub page_url: String,
    /// 要開的：網站影片是 yt-dlp 解析出來的網址或 `edl://`，其他網路串流是網址本身
    pub open: String,
    /// 開之前要設定的連線選項（照順序設定；後面的同名選項蓋過前面的）：「設定 → 網路」的設定
    /// （User-Agent、proxy、逾時、憑證、標頭、重新連線），網站影片再加上網站要的標頭、User-Agent、Cookie
    pub options: Vec<(String, Node)>,
    /// yt-dlp 解析出來的網站影片
    pub site: bool,
}

/// 網站影片的 file-local 選項裡，跟連線有關、另一個 mpv 開同一個串流也要的
const STREAM_ACCESS: [&str; 5] = [
    "user-agent",
    "http-header-fields",
    "cookies",
    "cookies-file",
    "stream-lavf-o",
];

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
    /// 正在觀察 sub-text（目前的檔案是純音訊，見 `SUB_TEXT_ID`）
    watching_sub_text: bool,
    /// 這個檔案的 sub-visibility 已經設成 no（歌詞由介面畫，見 `sync_lyrics_visibility`）
    lyrics_hidden: bool,
    /// 測試用的假裝置清單：有的話不讀 mpv 的（見 `set_fake_audio_devices`）
    fake_devices: bool,
    /// 最近一次開檔（`open`）換掉的檔案，音訊輸出是改用的 null（見 `take_fell_back_before_open`）
    fell_back_before_open: bool,
    /// 送出了開檔（`open`），還沒處理到 StartFile：沒有的話 StartFile 是 mpv 自己開始的下一個項目
    open_pending: bool,
    /// 測試用：影片軌一律當成這個杜比視界 profile（見 `set_fake_dolby_vision`）
    fake_dolby_vision: Option<i64>,
    /// 測試用：介面記下的軌道清單固定是這些，mpv 送來的不再蓋掉（見 `set_fake_tracks`）
    fake_tracks: Option<Vec<Track>>,
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
    /// mpv 的 stop-screensaver（建立時讀；VITASCOPE_MPV_OPTS、include= 可以改）
    stop_screensaver: StopScreensaver,
    /// 最近一次精準跳轉的目標和送出的時間（見 `position_now`）；其他跳轉、換檔時清掉
    exact_seek: std::sync::Mutex<Option<(f64, Instant)>>,
    /// 跳轉不準的格式（TS、M2TS、MPEG-PS）：分離器往前多跳的秒數、正在核對的跳轉（見 `approx_seek`）
    approx: std::sync::Mutex<Approx>,
    /// 目前（或最近一次）開的檔案或網址（`open` 時記下，StartFile 時讀得到 mpv 的 path 就換成它）：
    /// 開檔失敗時判斷是不是網路的原因
    opening: Option<String>,
    /// 這次開檔從什麼時候開始（`open`；mpv 自己開的檔案是處理 StartFile 的時候）；載入成功、結束時清掉
    load_started: Option<Instant>,
    /// 最近一次開檔失敗花了多久（從 `load_started` 算）：記錄裡看不出原因時，花滿逾時的算逾時
    failed_after: Option<Duration>,
    /// 已經放行的 hook 數（自動測試用）
    hooks_continued: u64,
    /// 網路上的播放清單（IPTV 的 .m3u 網址）讀完時展開的項目：mpv 結束那個網址（redirect）的當下讀出來，
    /// 等介面拿去變成播放清單（見 `take_remote_playlist`）
    remote_playlist: Option<net::RemotePlaylist>,
    /// VITASCOPE_DEBUG：印出這個等級（`log_rank`）以內的 mpv 記錄；沒設定時不印
    debug_print: Option<usize>,
    /// 註冊了網路功能的 hook（`Options.net_hooks`）
    net_hooks: bool,
    /// 網站影片（yt-dlp）
    site: Site,
    /// 播放引擎讀得了 `memory://` 的 Cookie 檔（mpv 0.38 起；系統的 libmpv 0.37 只讀本機檔案，改用 Cookie 標頭）
    memory_cookies: bool,
    /// 播放引擎有解碼器的編碼（`decoder-list` 的 codec，只有影像、聲音）：第一次有軌道解不了時才讀（之後不會變）
    decoders: Option<HashSet<String>>,
    /// 這個播放器開過的檔案裡解不了的字幕格式（引擎沒有解碼器，換檔也一樣）：自動選字幕時跳過
    undecodable_subs: HashSet<String>,
    /// 這個檔案開檔時自動選的字幕（軌道編號, 編碼）：它解不了的話（記錄晚到）改選別條
    auto_sub: Option<(i64, String)>,
    /// 這個檔案開始播了（PlaybackRestart）
    played: bool,
    /// 影像解不了、mpv 0.37 卻整個停下（見 `audio_only_retry`）：重開的檔案, 要帶過去的「解不了」記錄。
    /// 只給同一個檔案用（重開之前介面又開了別的，就丟掉）
    audio_only_carry: Option<(String, Vec<MissingDecoder>)>,
    /// 這個檔案是只播聲音重開的（不再重開第二次）
    audio_only_retried: bool,
    /// loadfile 有「插入位置」參數（mpv 0.38 起；選項是第四個參數，0.37 是第三個）
    loadfile_index: bool,
    /// 載入完成、還沒開始播就失敗的結束：等事件取完才決定要只播聲音重開，還是通知介面（見 `events_drained`）
    held_end: Option<PlayerEvent>,
    /// 先通知留著的結束，晚一步才處理的 mpv 事件（下一個檔案的 StartFile；見 `next_event`）
    replay: Option<Event>,
    /// 載入中、mpv 讀不到軌道時收到的「解不了」記錄（種類, 編碼）：可能是上一個檔案晚到的，也可能是這個檔案
    /// 整個開不起來（mpv 已經關掉分離器；mpv 0.37 影像解不了時，在載入完成之後才停下）。載入時因為沒有東西可播而失敗、
    /// 或載入完成之後還沒開始播就失敗，才算這個檔案的（見 `EndFile`、`decoder_failed`）
    pending_missing: Vec<(TrackKind, String)>,
    /// 這個檔案使用者自己選過的字幕編碼（`select_track`、`set_secondary_sub`）：只有這些解不了才提示（見 `MissingDecoder::quiet`）
    user_subs: std::sync::Mutex<Vec<String>>,
}

/// 跳轉不準的格式（`approx_seek::approximate_seeks`）在這個檔案的狀態；換檔時清掉
#[derive(Debug, Default)]
struct Approx {
    /// 這個檔案要核對跳轉（開檔完成時決定：格式跳轉不準、有影像、使用者沒有自己指定 hr-seek-demuxer-offset）
    on: bool,
    /// 分離器往前多跳的秒數（file-local 的 `hr-seek-demuxer-offset`）。加大了才跳對的話（`Landing::lead`）
    /// 這個檔案之後一直用；只是重試時加大、沒有用的不算
    offset: f64,
    /// 目前設給 mpv 的 hr-seek-demuxer-offset（重試時是那一次的秒數，核對完還原成 `offset`）
    applied: f64,
    /// 一格多久（秒，判斷有沒有跳過頭）
    frame: f64,
    /// 正在核對的跳轉（最新的那一次）
    check: Option<Check>,
    /// 最近一次開始核對播放器送出的跳轉的時間（`approx_watch`）
    watched: Option<Instant>,
    /// 標記的流水號
    next_id: u64,
    /// 最近一次核對完的結果（自動測試讀它）
    landing: Option<Landing>,
}

/// 送出精準跳轉後這麼久（秒）以內，`position_now` 用跳轉的目標：上一次跳轉 0.3 秒內，
/// mpv 會等上一次的畫面出來才做下一次（playloop.c `execute_queued_seek`），這段時間 time-pos 還是舊的位置
const SEEK_PENDING: f64 = 0.35;
/// 跳轉不準的格式：關鍵影格跳轉還沒停住時 `position_now` 用目標，最多這麼久（秒；正常不到半秒就停住了）
const KEYFRAME_PENDING: f64 = 5.0;

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
        // 網路選項也一樣（user-agent、逾時、快取…、標頭清單、FFmpeg 的連線選項）
        // 重複單一檔案的 loop-file 也一樣（別名 loop 改的也是它）；播放時螢幕不關閉的 stop-screensaver 也是
        for name in crate::picture::MANAGED
            .into_iter()
            .chain(crate::sound::MANAGED)
            .chain(["af"])
            .chain(net::MANAGED)
            .chain([net::HEADERS, net::LAVF])
            .chain(["loop-file", "stop-screensaver"])
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
        // loop 是 loop-file 的別名：VITASCOPE_MPV_OPTS 寫 loop=no（跟預設一樣，上面看不出來）也算指定了 loop-file
        if user_overrides.contains("loop") {
            user_overrides.insert("loop-file".to_owned());
        }
        let mut shader_base = mpv.get_string_list("glsl-shaders").unwrap_or_default();
        shader_base.retain(|s| !s.is_empty());
        let names_shader_list =
            |n: &String| n == "glsl-shader" || n == "glsl-shaders" || n.starts_with("glsl-shaders-");
        if !shader_base.is_empty() || user_overrides.iter().any(names_shader_list) {
            user_overrides.insert("glsl-shaders".to_owned());
        }
        let ao_null_wanted = crate::sound::ao_list_has_null(&mpv.get_string("ao").unwrap_or_default());
        // 播放時螢幕不關閉：照 mpv 的值（vo=libmpv 時 mpv 自己不做，影戲照它的值做，見 `inhibit`）
        let stop_screensaver = StopScreensaver::from_mpv(&mpv.get_string("stop-screensaver").unwrap_or_default());
        let mpv_text = mpv.get_string("mpv-version").unwrap_or_default();
        if let Some(wakeup) = opts.wakeup {
            mpv.set_wakeup_callback(wakeup);
        }
        let debug = std::env::var("VITASCOPE_DEBUG").ok();
        mpv.request_log_messages(requested_log_level(debug.as_deref()))?;
        for (i, (name, format)) in OBSERVED.iter().enumerate() {
            mpv.observe(i as u64 + 1, name, *format)?;
        }
        if opts.net_hooks {
            mpv.hook_add(HOOK_ON_LOAD, "on_load", HOOK_PRIORITY)?;
            mpv.hook_add(HOOK_ON_LOAD_FAIL, "on_load_fail", HOOK_PRIORITY)?;
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
            watching_sub_text: false,
            lyrics_hidden: false,
            fake_devices: false,
            fell_back_before_open: false,
            open_pending: false,
            fake_dolby_vision: None,
            fake_tracks: None,
            shaders_applied: Some(shader_base.clone()),
            shader_user: shader_base,
            shader_flip: [None, None],
            shaders_inflight: HashSet::new(),
            boost_pct: 0.0,
            ao_null_wanted,
            stop_screensaver,
            exact_seek: std::sync::Mutex::new(None),
            approx: std::sync::Mutex::new(Approx::default()),
            opening: None,
            load_started: None,
            failed_after: None,
            hooks_continued: 0,
            remote_playlist: None,
            debug_print: debug.as_deref().map(|v| log_rank(debug_log_level(Some(v)))),
            net_hooks: opts.net_hooks,
            memory_cookies: memory_cookies(&mpv_text),
            decoders: None,
            undecodable_subs: HashSet::new(),
            auto_sub: None,
            played: false,
            audio_only_carry: None,
            audio_only_retried: false,
            loadfile_index: mpv_version(&mpv_text).is_none_or(|v| v >= (0, 38)),
            held_end: None,
            replay: None,
            pending_missing: Vec::new(),
            user_subs: std::sync::Mutex::new(Vec::new()),
            site: Site {
                resolver: opts.net_resolver,
                extra_sites: opts.net_sites,
                ..Default::default()
            },
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

    /// mpv 的 stop-screensaver（建立時讀到的）：播放時要不要讓螢幕不關閉（見 `inhibit`）
    pub fn stop_screensaver(&self) -> StopScreensaver {
        self.stop_screensaver
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

    // ───────────── 網路選項 ─────────────

    /// 這個播放引擎的網路選項預設值（使用者把 User-Agent 清空時送回這個）
    pub fn net_defaults(&self) -> net::NetDefaults {
        let mut d = net::NetDefaults::default();
        if let Ok(ua) = self.mpv.get_string("option-info/user-agent/default-value") {
            d.user_agent = ua;
        }
        d
    }

    /// 套用網路選項（`net::mpv_options` 的結果），跟 `apply_picture` 一樣只送有變的、略過使用者自己指定的。
    /// `sync`：啟動時（還沒開檔）同步設定；不然非同步（`AsyncKey::Net`）。
    /// HTTP 標頭是字串清單，項目裡可能有逗號：同步時用 node 整個設定；非同步時先清空、再一項一項加
    /// （change-list 的 append 不會用逗號拆開），回傳裡有好幾筆 `http-header-fields`
    pub fn apply_net(
        &mut self,
        o: &net::NetOptions,
        sync: bool,
    ) -> Vec<(&'static str, AsyncKey, mpv::Result<Option<u64>>)> {
        self.site.wanted = Some(o.clone());
        let sent = self.apply_net_inner(o, sync);
        // 網站影片用 file-local-options 蓋過了 User-Agent 之類：mpv 在檔案結束時把它們還原成「設定 file-local 之前」的值，
        // 這時送出的新設定會被蓋回去（記下的值卻說已經送了）。記下來，檔案結束時再送一次
        if !sent.is_empty() && !self.site.file_local.is_empty() {
            self.site.net_changed = true;
        }
        sent
    }

    fn apply_net_inner(
        &mut self,
        o: &net::NetOptions,
        sync: bool,
    ) -> Vec<(&'static str, AsyncKey, mpv::Result<Option<u64>>)> {
        let net_key: fn(&str) -> AsyncKey = |_| AsyncKey::Net;
        let mut sent = self.apply_cached(&o.scalars, sync, net_key);
        // stream-lavf-o 是鍵值清單，字串「名稱=值,名稱=值」就是它的寫法（名稱、值都沒有逗號）
        sent.extend(self.apply_cached(&[(net::LAVF, o.lavf.clone())], sync, net_key));
        let value = o.headers.join("\n");
        if self.user_overrides.contains(net::HEADERS) || self.options_applied.get(net::HEADERS) == Some(&value) {
            return sent;
        }
        let results: Vec<mpv::Result<Option<u64>>> = if sync {
            vec![
                self.mpv
                    .set_node(net::HEADERS, &Node::strings(o.headers.iter().cloned()))
                    .map(|()| None),
            ]
        } else {
            let clear = ["change-list", net::HEADERS, "clr", ""];
            std::iter::once(self.command_async_keyed(AsyncKey::Net, &clear).map(Some))
                .chain(o.headers.iter().map(|h| {
                    self.command_async_keyed(AsyncKey::Net, &["change-list", net::HEADERS, "append", h])
                        .map(Some)
                }))
                .collect()
        };
        if results.iter().all(Result::is_ok) {
            self.options_applied.insert(net::HEADERS, value);
        } else {
            self.options_applied.remove(net::HEADERS);
        }
        sent.extend(results.into_iter().map(|r| (net::HEADERS, AsyncKey::Net, r)));
        sent
    }

    /// 非同步設定的網路選項 mpv 不接受：忘掉記下的值，下次套用時再送
    pub fn forget_net(&mut self, name: &str) {
        self.options_applied.remove(name);
    }

    /// 已經放行的 hook 數（自動測試用：確認 hook 真的有註冊、有收到）
    #[doc(hidden)]
    pub fn hooks_continued(&self) -> u64 {
        self.hooks_continued
    }

    /// 音訊輸出開不起來，mpv 改用 null 輸出（沒有聲音；ao 本來就指定 null 的不算）
    pub fn audio_fell_back(&self) -> bool {
        !self.ao_null_wanted && self.state.current_ao.as_deref() == Some("null")
    }

    /// 最近一次開檔（`open`）換掉的檔案，音訊輸出是不是改用了 null；讀過就清掉（介面在新檔案的 StartFile 讀，
    /// 是的話重開一次音訊輸出：同樣格式時 mpv 換檔沿用同一個輸出，不重開的話新檔案一直沒有聲音）。
    ///
    /// 不看觀察到的 current-ao：屬性的變化不跟事件排順序（mpv 先送完排著的事件才送變了的屬性，屬性的值
    /// 每輪播放迴圈才取一次），慢的電腦上上一個檔案的 null 可能在 StartFile 之後好幾批才收到，新檔案自己的
    /// null 也可能跟 StartFile 同一批，分不出是誰的。所以在送出 loadfile 之前直接問 mpv（見 `open_inner`）：
    /// 那時新檔案還沒開始，看到的一定是要換掉的檔案的輸出。新檔案自己開不起來（第一個檔案、格式不同而重開）
    /// 不算：mpv 剛剛才為它試過裝置。
    ///
    /// mpv 自己開始的下一個項目（交給 mpv 讀的本機清單 .pls 之類，沒有經過 `open`）在處理它的 StartFile 時才問：
    /// 介面放行 on_load hook 之前 mpv 還沒為它開音訊輸出，看到的是上一個項目的。沒有 hook 時（headless）
    /// 可能已經是這個項目自己的，盡量而已
    pub(crate) fn take_fell_back_before_open(&mut self) -> bool {
        std::mem::take(&mut self.fell_back_before_open)
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

    // ───────────── 歌詞（純音訊檔的字幕） ─────────────

    /// 開檔後：純音訊檔（沒有影像，專輯封面不算）開始觀察 sub-text，介面自己把歌詞畫在歌名下面
    ///（之後才拖進來、「載入字幕檔…」的歌詞也一樣）
    fn watch_lyrics(&mut self) {
        if self.state.tracks.is_empty() || self.state.has_video() || self.watching_sub_text {
            return;
        }
        match self.mpv.observe(SUB_TEXT_ID, "sub-text", Format::String) {
            Ok(()) => self.watching_sub_text = true,
            Err(e) => eprintln!("[vitascope] 無法觀察字幕文字：{e}"),
        }
        self.sync_lyrics_visibility();
    }

    /// 介面畫歌詞時（`State::lyrics_in_app`），mpv 自己畫在專輯封面上的那份藏起來：不然會跟介面畫的重複，
    /// 而且被歌名的底色蓋住。只對這個檔案（file-local；關檔時 mpv 自己還原）。
    /// 換字幕（選別條、拖進 ASS 或圖片字幕）時重新判斷：介面畫不出來的交還給 mpv
    fn sync_lyrics_visibility(&mut self) {
        if !self.watching_sub_text {
            return;
        }
        let hide = self.state.lyrics_in_app();
        if hide == self.lyrics_hidden {
            return;
        }
        let value = if hide { "no" } else { "yes" };
        match self.mpv.set_property("file-local-options/sub-visibility", value) {
            Ok(()) => self.lyrics_hidden = hide,
            Err(e) => eprintln!("[vitascope] 無法設定封面上的字幕（{value}）：{e}"),
        }
    }

    /// 關檔時停止觀察 sub-text（下一個檔案是影片時不能留著，見 `SUB_TEXT_ID`）
    fn unwatch_lyrics(&mut self) {
        if std::mem::take(&mut self.watching_sub_text)
            && let Err(e) = self.mpv.unobserve(SUB_TEXT_ID)
        {
            eprintln!("[vitascope] 無法停止觀察字幕文字：{e}");
        }
        // file-local 的 sub-visibility 由 mpv 在關檔時還原
        self.lyrics_hidden = false;
        self.state.sub_text = None;
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

    /// 測試用：介面記下的軌道清單固定是 `tracks`，之後 mpv 送來的清單不再蓋掉它（None = 照 mpv 的，馬上重讀）。
    /// 模擬 mpv 還沒送軌道清單、或還沒送選了哪些。mpv 什麼時候再送一次清單說不準（解碼器、音訊輸出開好、加了字幕都會再送），
    /// 直接改 `state.tracks` 的話，慢的電腦上下一幀就可能被蓋掉
    #[doc(hidden)]
    pub fn set_fake_tracks(&mut self, tracks: Option<Vec<Track>>) {
        self.fake_tracks = tracks;
        if self.fake_tracks.is_some() {
            self.apply_fake_tracks();
        } else {
            self.refresh_tracks();
        }
    }

    /// 收到 mpv 的軌道清單之後：換成測試用的（軌道清單、杜比視界）
    fn apply_fake_tracks(&mut self) {
        if let Some(tracks) = &self.fake_tracks {
            self.state.tracks = tracks.clone();
        }
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
        let commands = self.list_field("command-list", "name");
        let encoders = self.list_field("encoder-list", "driver");
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
            dump_cache: commands.iter().any(|c| c == "dump-cache"),
            align_cache: commands.iter().any(|c| c == "ab-loop-align-cache"),
            gif: encoders.iter().any(|e| e == "gif"),
            zscale: self.probe_vf("zscale"),
            tonemap: self.probe_vf("tonemap"),
            transpose: self.probe_vf("transpose"),
            palettegen: self.probe_vf("palettegen"),
        }
    }

    /// 清單屬性（`command-list`、`encoder-list`…）裡每一項的某個欄位；讀不到時是空的
    fn list_field(&self, property: &str, field: &str) -> Vec<String> {
        self.mpv
            .get_string(property)
            .ok()
            .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item[field].as_str().map(str::to_owned))
            .collect()
    }

    /// 這個引擎有沒有這個 FFmpeg 音訊濾鏡。mpv 加進 af 時就會檢查濾鏡名稱（不用開檔）；
    /// 要用「@標籤:名稱」的寫法，「lavfi=[名稱]」要到建立濾鏡圖時才檢查。偵測完 af 恢復原狀
    pub fn probe_af(&mut self, name: &str) -> bool {
        self.probe_filter("af", name)
    }

    /// 同 `probe_af`，影像濾鏡（vf）。匯出用（GIF、縮圖總覽圖在另外的 mpv 裡用這些濾鏡），主播放器只是拿來問
    pub fn probe_vf(&mut self, name: &str) -> bool {
        self.probe_filter("vf", name)
    }

    /// `chain` 是 "af" 或 "vf"
    fn probe_filter(&mut self, chain: &str, name: &str) -> bool {
        // 播放中加濾鏡會重建整條濾鏡鏈（聲音會斷一下、畫面會重新設定）
        debug_assert!(
            !self.state.loaded && !self.state.loading,
            "probe_{chain} 只能在開檔之前用"
        );
        let before = self.mpv.get_string(chain).unwrap_or_default();
        let ok = self
            .mpv
            .command(&[chain, "add", &format!("{PROBE_LABEL}:{name}")])
            .is_ok();
        let _ = self.mpv.command(&[chain, "remove", PROBE_LABEL]);
        if self.mpv.get_string(chain).unwrap_or_default() != before {
            let _ = self.mpv.set_property(chain, before.as_str());
        }
        if !ok {
            // 失敗時 mpv 記一筆「Option af-add: 'xxx' isn't supported.」
            // （0.37 是「Option af-add: xxx doesn't exist.」；vf 是 vf-add）
            self.probe_noise.push([name.to_owned(), format!("{chain}-add")]);
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
        self.open_inner(path, None)
    }

    /// 開檔，這次有只對這次有效的要求（換畫質、載入整個播放清單、從某個位置開始）：網址一定先問 yt-dlp
    ///（20 分鐘內解析過的直接用之前的結果）
    pub fn open_with_mode(&mut self, path: &str, mode: Mode) -> mpv::Result<()> {
        self.open_inner(path, Some(mode))
    }

    fn open_inner(&mut self, path: &str, mode: Option<Mode>) -> mpv::Result<()> {
        self.state.last_error = None;
        self.state.net_failure = None;
        let flips = self.shader_flip.clone();
        self.reset_shaders_for_next_file();
        // 要換掉的檔案的音訊輸出是不是改用的 null（見 `take_fell_back_before_open`）：送出 loadfile 之前問，
        // 之後新檔案可能已經開了自己的輸出（沒有 on_load hook 時 mpv 不等介面）
        let fell_back = self.audio_fell_back_now();
        let result = self.mpv.command(&["loadfile", path, "replace"]);
        if result.is_ok() {
            self.fell_back_before_open = fell_back;
            self.open_pending = true;
            // 開檔失敗時判斷是不是網路的原因（StartFile 時讀 mpv 的 path 可能已經太晚）
            self.opening = Some(path.to_owned());
            self.load_started = Some(Instant::now());
            // on_load 要等處理事件時才收到，現在記下來還來得及；之前沒用到的要求（那個檔案還沒開就換掉了）丟掉
            self.site.mode = mode.map(|m| (path.to_owned(), m));
            // 還沒開始的只播聲音重開（mpv 0.37）被這次換掉了：記錄不帶給新的檔案
            self.audio_only_carry = None;
            // 舊的檔案還停在 hook 等 yt-dlp：mpv 已經收到換檔（loadfile 在前），現在放行，舊的就直接結束、不會再去讀網頁
            self.site_abandon();
        }
        if result.is_err() && flips.iter().any(Option::is_some) {
            // 沒有換檔：舊檔案照樣在播，翻轉放回去
            self.shader_flip = flips;
            let _ = self.push_shaders(false);
        }
        result
    }

    /// 停止。正在等 yt-dlp 的話，下一次處理事件時（`poll`、`wait`）發現檔案被拿掉了，就不等、放行 hook
    /// （先停止再放行，mpv 直接結束：原因是 stop、沒有錯誤）
    pub fn stop(&self) -> mpv::Result<()> {
        self.mpv.command(&["stop"])
    }

    /// 正在開檔：已經送出開檔（或 mpv 開始了下一個檔案），還沒載入完成也還沒結束
    pub fn loading_now(&self) -> bool {
        self.state.loading || self.load_started.is_some()
    }

    /// 正在連線的網路串流（網址, 從什麼時候開始）：連線中的畫面、Esc 取消用。
    /// 舊的檔案還在播（換檔途中）時不算
    pub fn net_loading(&self) -> Option<(&str, Instant)> {
        let since = self.load_started?;
        let url = self.opening.as_deref().filter(|u| net::is_network(u))?;
        (!self.state.loaded).then_some((url, since))
    }

    /// 取消正在進行的開檔（Esc、連線中畫面的「取消」、停止）：跟停止一樣，mpv 中斷連線，
    /// 結束的原因是 stop、沒有錯誤（正在等 yt-dlp 的也不等了，停住的 hook 放行）
    pub fn cancel_loading(&mut self) -> mpv::Result<()> {
        self.stop()?;
        self.site_abandon();
        // 馬上不算載入中（連線中的畫面馬上消失）。mpv 還沒開始的檔案（loadfile 之後馬上停止）不會再有 EndFile，
        // 不清掉的話會一直算「載入中」
        self.load_started = None;
        self.state.loading = false;
        self.audio_only_carry = None;
        Ok(())
    }

    /// 讀 mpv 現在的播放清單，只留網路串流（網路上的播放清單展開後；見 `net::remote_playlist`）
    pub fn import_playlist(&self) -> net::RemotePlaylist {
        let json = self.mpv.get_string("playlist").unwrap_or_default();
        net::remote_playlist(self.opening.as_deref().unwrap_or_default(), &json)
    }

    /// 網路上的播放清單剛展開（EndFile 的原因是 redirect）：拿走展開的項目（只有一次）
    pub fn take_remote_playlist(&mut self) -> Option<net::RemotePlaylist> {
        self.remote_playlist.take()
    }

    pub fn set_pause(&self, paused: bool) -> mpv::Result<()> {
        self.mpv.set_property("pause", paused)
    }

    /// 現在是不是暫停：直接問 mpv，問不到時用介面記下的。屬性通知可能還沒到：
    /// 逐格到結尾時 mpv 先取消暫停、到結尾再暫停，介面記下的可能還是「播放中」
    pub fn paused_now(&self) -> bool {
        self.mpv.get_property::<bool>("pause").unwrap_or(self.state.paused)
    }

    /// 現在是不是停在結尾：直接問 mpv（同 [`Self::paused_now`]），問不到時用介面記下的
    pub fn eof_now(&self) -> bool {
        self.mpv.get_property::<bool>("eof-reached").unwrap_or(self.state.eof)
    }

    pub fn toggle_pause(&self) -> mpv::Result<()> {
        // 播完停在最後一格時按播放 = 從頭開始（跳轉不準的格式：mpv 開始做時從記錄核對，見 `approx_log`）。
        // 看 mpv 現在的狀態：介面記下的可能還沒更新（見 `paused_now`），以為在播放就只會暫停，不會從頭開始
        if self.eof_now() && self.paused_now() {
            self.note_seek(None);
            self.approx_forget();
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
            self.note_seek(None);
            // 跳轉不準的格式：分離器往前多跳的秒數不夠時一樣沒有新的畫面（最後一個關鍵影格離片尾太遠），要核對
            let watch = self.approx_watch(Kind::Exact, duration, true);
            return self.send_seek(watch.map(|(id, _)| id), 100.0, "absolute-percent+exact");
        }
        if self.approx_on() {
            let target = (self.position_now() + seconds).max(0.0);
            // 跳轉不準的格式往回跳：mpv 的相對跳轉常落在目標之後的關鍵影格，甚至原地不動。
            // 跟拖曳進度條一樣往前多跳，落在目標之前的關鍵影格
            if seconds < 0.0 {
                return self.seek_to(target, false);
            }
            // 往後跳照 mpv 原本的做法，沒有新的畫面時才改成精確跳到這裡
            //（mpv 從它自己的目前位置算，跟這裡的差不到一格）
            let watch = self.approx_watch(Kind::Forward, target, false);
            self.note_seek(None);
            return self.send_seek(watch.map(|(id, _)| id), seconds, "relative");
        }
        self.note_seek(None);
        self.mpv.command(&["seek", &format!("{seconds}"), "relative"])
    }

    /// 跳到絕對時間（秒）。`exact` = 精準到影格（較慢）；拖曳進度條時用關鍵影格比較順
    pub fn seek_to(&self, seconds: f64, exact: bool) -> mpv::Result<()> {
        // 跳轉不準的格式：關鍵影格跳轉也往前多跳（見下面）。目標離開頭比往前多跳的秒數還近時改成精確跳轉：
        // 關鍵影格跳轉的時間最多到 0（負的是從片尾算），跳到第一個時間戳常落到下一個關鍵影格
        let exact = exact || self.approx_lead().is_some_and(|lead| seconds <= lead);
        let mode = if exact { "absolute+exact" } else { "absolute+keyframes" };
        // 關鍵影格跳轉落在哪裡要等 mpv 做了才知道：不記目標
        self.note_seek(exact.then_some(seconds));
        let kind = if exact { Kind::Exact } else { Kind::Keyframe };
        let watch = self.approx_watch(kind, seconds, true);
        let at = match watch {
            // 跳轉不準的格式：關鍵影格跳轉往前多跳，落在目標之前的關鍵影格（跟其他格式一樣），
            // 不會跑到目標後面。精確跳轉往前多跳的秒數是 file-local 的 hr-seek-demuxer-offset，mpv 自己減
            Some((_, lead)) if !exact => seconds - lead,
            _ => seconds,
        };
        self.send_seek(watch.map(|(id, _)| id), at, mode)
    }

    /// 送出跳轉。要核對時（`watch` = 標記的編號）標記和跳轉放在同一個指令清單（見 `approx_seek::seek_command`）
    fn send_seek(&self, watch: Option<u64>, amount: f64, mode: &str) -> mpv::Result<()> {
        let Some(id) = watch else {
            return self.mpv.command(&["seek", &format!("{amount}"), mode]);
        };
        let sent = self.mpv.command_string(&approx_seek::seek_command(id, amount, mode));
        if sent.is_err() {
            self.approx_forget();
        }
        sent
    }

    /// 這個檔案的格式跳轉不準，跳轉要核對（見 `approx_seek`）
    fn approx_on(&self) -> bool {
        self.approx.lock().is_ok_and(|a| a.on)
    }

    /// 跳轉要核對時，分離器目前往前多跳的秒數
    fn approx_lead(&self) -> Option<f64> {
        self.approx.lock().ok().filter(|a| a.on).map(|a| a.offset)
    }

    /// 跳轉不準的格式：開始核對這次跳轉（之前在核對的跳轉一律作廢）。`lead` = 分離器要不要往前多跳
    /// （方向鍵的相對跳轉照 mpv 原本的做法，不多跳）。回傳標記的編號（跳轉要用 [`Self::send_seek`] 帶著它送出）
    /// 和往前多跳的秒數；不用核對時 None
    fn approx_watch(&self, kind: Kind, target: f64, lead: bool) -> Option<(u64, f64)> {
        let mut a = self.approx.lock().ok()?;
        a.check = None;
        if !a.on {
            return None;
        }
        a.next_id += 1;
        let lead = if lead { a.offset } else { 0.0 };
        a.check = Some(Check::new(a.next_id, target, kind, lead));
        a.watched = Some(Instant::now());
        Some((a.next_id, lead))
    }

    /// 不核對（其他種類的跳轉）：之前在核對的跳轉作廢
    fn approx_forget(&self) {
        if let Ok(mut a) = self.approx.lock() {
            a.check = None;
        }
    }

    /// 開檔完成：格式跳轉不準（TS、M2TS、MPEG-PS）、有影像時，精確跳轉的分離器先往前多跳
    /// [`approx_seek::FIRST_OFFSET`] 秒（file-local，換檔時 mpv 自己還原），之後的跳轉都核對。
    /// 使用者自己指定了 hr-seek-demuxer-offset（VITASCOPE_MPV_OPTS）時照使用者的，不核對
    fn approx_setup(&mut self) {
        let format = self.mpv.get_string("file-format").unwrap_or_default();
        let mut on = approx_seek::approximate_seeks(&format)
            && self.state.has_video()
            && !self.user_overrides.contains("hr-seek-demuxer-offset");
        if on
            && let Err(e) = self
                .mpv
                .set_property("file-local-options/hr-seek-demuxer-offset", approx_seek::FIRST_OFFSET)
        {
            eprintln!("[vitascope] 無法設定 hr-seek-demuxer-offset：{e}");
            on = false;
        }
        let frame = approx_seek::frame_duration(self.mpv.get_property::<f64>("container-fps").ok());
        if let Ok(mut a) = self.approx.lock() {
            *a = Approx {
                on,
                offset: approx_seek::FIRST_OFFSET,
                applied: approx_seek::FIRST_OFFSET,
                frame,
                check: None,
                watched: None,
                next_id: a.next_id,
                landing: None,
            };
        }
    }

    /// 換檔、檔案結束：不再核對
    fn approx_reset(&mut self) {
        if let Ok(mut a) = self.approx.lock() {
            a.on = false;
            a.check = None;
        }
    }

    /// mpv 的記錄：核對中的跳轉（見 `approx_seek::Check::feed`）完成時，跳過頭或沒有新的畫面的話
    /// 分離器往前多跳的秒數加大、再精確跳一次（見 `approx_seek::judge`）。
    /// 沒有在核對的跳轉時，mpv 自己做的精確跳轉（A-B 重播繞回 A、逐格往回、章節、播完從頭播）從它的
    /// 「hr-seek, skipping to」記錄開始核對
    fn approx_log(&mut self, text: &str) {
        let (id, target, offset) = {
            let Ok(mut a) = self.approx.lock() else { return };
            if !a.on {
                return;
            }
            let Some(check) = a.check.as_mut() else {
                if let Some((target, backstep)) = approx_seek::parse_hr_seek(text) {
                    a.next_id += 1;
                    let kind = if backstep { Kind::Backstep } else { Kind::Exact };
                    a.check = Some(Check::started(a.next_id, target, kind, a.offset));
                }
                return;
            };
            let Some(restart) = check.feed(text) else { return };
            let Some(check) = a.check.take() else { return };
            match approx_seek::judge(&check, &restart, a.offset, a.frame) {
                Verdict::Done(landing) => {
                    // 加大了才跳對：關鍵影格隔得比較久，這個檔案之後都用這麼多
                    if let Some(lead) = landing.lead
                        && lead > a.offset
                    {
                        a.offset = lead;
                    }
                    a.landing = Some(landing);
                    if a.applied == a.offset {
                        return;
                    }
                    // 重試時暫時加大、沒有用的秒數還原（之後的跳轉、mpv 自己的跳轉用回這個檔案的秒數）
                    a.applied = a.offset;
                    (None, 0.0, a.offset)
                }
                Verdict::Retry { offset } => {
                    a.next_id += 1;
                    let next = check.retry(a.next_id, &restart, offset, a.frame);
                    let target = next.target;
                    a.check = Some(next);
                    a.applied = offset;
                    (Some(a.next_id), target, offset)
                }
            }
        };
        let set = self
            .mpv
            .set_property("file-local-options/hr-seek-demuxer-offset", offset);
        let Some(id) = id else {
            if let Err(e) = set {
                eprintln!("[vitascope] 無法設定 hr-seek-demuxer-offset：{e}");
            }
            return;
        };
        // 這也是一次精確跳轉：剛送出時 `position_now` 用目標
        self.note_seek(Some(target));
        if let Err(e) = set.and_then(|()| self.send_seek(Some(id), target, "absolute+exact")) {
            eprintln!("[vitascope] 無法重新跳轉：{e}");
            self.approx_forget();
        }
    }

    /// 測試用：最近一次核對完的跳轉（跳轉不準的格式才有）
    #[doc(hidden)]
    pub fn seek_landing(&self) -> Option<Landing> {
        self.approx.lock().ok()?.landing
    }

    /// 測試用：還在核對跳轉（可能還會再跳一次）
    #[doc(hidden)]
    pub fn seek_checking(&self) -> bool {
        self.approx.lock().is_ok_and(|a| a.check.is_some())
    }

    /// 測試用：這個檔案的跳轉要核對時，分離器目前往前多跳的秒數（不用核對時 None）
    #[doc(hidden)]
    pub fn seek_lead(&self) -> Option<f64> {
        self.approx_lead()
    }

    /// 記下（或清掉）最近一次精準跳轉的目標
    fn note_seek(&self, target: Option<f64>) {
        if let Ok(mut slot) = self.exact_seek.lock() {
            *slot = target.map(|t| (t, Instant::now()));
        }
    }

    /// 目前的位置（秒），按下按鍵的那一刻用（新增書籤、上一個 / 下一個書籤）：
    /// - 直接問 mpv 的 time-pos：屬性通知可能還沒到，介面看到的狀態可能還是跳轉前的。
    /// - 剛送出精準跳轉（[`SEEK_PENDING`] 秒內）時用跳轉的目標：連續跳轉時 mpv 可能還沒開始做這一次，
    ///   time-pos 還是上一次的位置（連按「下一個書籤」會一直跳到同一個）
    /// - 跳轉不準的格式的關鍵影格跳轉還沒停住時也用目標（見 `approx_keyframe_target`）
    pub fn position_now(&self) -> f64 {
        let pending = self.exact_seek.lock().ok().and_then(|slot| *slot);
        if let Some((t, at)) = pending
            && at.elapsed().as_secs_f64() < SEEK_PENDING
        {
            return t;
        }
        if let Some(t) = self.approx_keyframe_target() {
            return t;
        }
        self.get_f64("time-pos").unwrap_or(self.state.time_pos)
    }

    /// 跳轉不準的格式的關鍵影格跳轉（拖曳進度條、方向鍵往回跳）還沒停住時，使用者要的位置是它的目標：
    /// mpv 的 time-pos 這時是送出的時間（目標 − 往前多跳的秒數，playloop.c `last_seek_pts`），
    /// 或 mpv 還沒開始做時的舊位置。按住方向鍵往回跳時每一下從目標算，跟 mpv 的相對跳轉一樣一次退 5 秒，
    /// 不會每一下多退往前多跳的秒數。萬一一直沒停住（[`KEYFRAME_PENDING`] 秒）就照 mpv 的
    fn approx_keyframe_target(&self) -> Option<f64> {
        let a = self.approx.lock().ok()?;
        let check = a.check.as_ref().filter(|c| c.kind == Kind::Keyframe)?;
        a.watched
            .is_some_and(|at| at.elapsed().as_secs_f64() < KEYFRAME_PENDING)
            .then_some(check.target)
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
        self.note_seek(None);
        // 跳轉不準的格式：往回一格是 mpv 自己的精確跳轉，從它的記錄開始核對（見 `approx_log`）
        self.approx_forget();
        self.mpv
            .command(&[if forward { "frame-step" } else { "frame-back-step" }])
    }

    /// A-B 重播：第一次設起點、第二次設終點、第三次取消
    pub fn cycle_ab_loop(&self) -> mpv::Result<()> {
        self.mpv.command(&["ab-loop"])
    }

    /// A-B 重播的起點、終點（None = 沒設）。匯出片段也用這裡設定範圍。
    /// 時間用數字設定，不能轉成字串：mpv 只在設定的那一刻看「目前位置 <= 終點」才會在終點繞回起點，
    /// 終點四捨五入到比目前那一格的時間小一點點（例如 30 fps 的 1.0333…→「1.033333」），就永遠不會繞回去。
    /// None 用 "no" 清掉
    pub fn set_ab_loop(&self, a: Option<f64>, b: Option<f64>) -> mpv::Result<()> {
        for (name, t) in [("ab-loop-a", a), ("ab-loop-b", b)] {
            match t {
                Some(t) => self.mpv.set_property(name, t)?,
                None => self.mpv.set_property(name, "no")?,
            }
        }
        Ok(())
    }

    /// 直接問 mpv 目前的 A-B 重播點（剛設定的值，屬性變化的通知可能還沒到）。
    /// 用數字讀（字串只到小數 6 位）；沒設時 mpv 給的是 "no"，讀不成數字就是 None
    pub fn ab_loop_points(&self) -> [Option<f64>; 2] {
        ["ab-loop-a", "ab-loop-b"].map(|name| self.mpv.get_property::<f64>(name).ok())
    }

    /// 分離器的起始時間（`demuxer-start-time`，秒；沒有檔案、讀不到時 0）。
    /// mpv 的播放時間（time-pos、A-B 重播點）是「影片的時間戳 − 這個值」；匯出另外開的 mpv 可能用不同的分離器，
    /// 起始時間不一定一樣，換算時兩邊都要看
    pub fn demuxer_start_time(&self) -> f64 {
        self.mpv
            .get_property::<f64>("demuxer-start-time")
            .ok()
            .filter(|t| t.is_finite())
            .unwrap_or(0.0)
    }

    /// 取消 A-B 重播（mpv 換檔時會沿用，所以開新檔時要清掉）
    pub fn clear_ab_loop(&self) -> mpv::Result<()> {
        self.mpv.set_property("ab-loop-a", "no")?;
        self.mpv.set_property("ab-loop-b", "no")
    }

    /// 跳到前 / 後幾個章節
    pub fn add_chapter(&self, delta: i64) -> mpv::Result<()> {
        self.note_seek(None);
        self.approx_forget();
        self.mpv.command(&["add", "chapter", &delta.to_string()])
    }

    /// 跳到第 `index` 個章節（從 0 開始）
    pub fn seek_chapter(&self, index: usize) -> mpv::Result<()> {
        self.note_seek(None);
        self.approx_forget();
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
        self.note_user_sub(id);
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

    /// 選擇軌道；`None` = 關閉這類軌道（例如關字幕）。使用者（介面）選的；影戲自己自動選的用 `set_track`
    pub fn select_track(&self, kind: TrackKind, id: Option<i64>) -> mpv::Result<()> {
        if kind == TrackKind::Sub {
            self.note_user_sub(id);
        }
        self.set_track(kind, id)
    }

    /// 記下使用者自己選的字幕的編碼：這條解不了的話要提示（見 `MissingDecoder::quiet`）
    fn note_user_sub(&self, id: Option<i64>) {
        let Some(id) = id else { return };
        let codec = self
            .current_tracks()
            .iter()
            .find(|t| t.kind == TrackKind::Sub && t.id == id)
            .map(|t| track_codec(t).to_owned());
        if let (Some(codec), Ok(mut list)) = (codec, self.user_subs.lock())
            && !list.contains(&codec)
        {
            list.push(codec);
        }
    }

    fn set_track(&self, kind: TrackKind, id: Option<i64>) -> mpv::Result<()> {
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
            // 轉不了的 TTML：說明原因（原檔交給 mpv 也讀不了，只會說「載入失敗」）
            Err(e) if subs::is_conversion_error(&e) => Err(mpv::Error {
                code: libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED,
                context: e.to_string(),
            }),
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
        while let Some(ev) = self.next_event(0.0) {
            if let Some(pe) = self.handle(ev) {
                out.push(pe);
            }
        }
        // 事件（含比較晚送到的錯誤記錄）都處理完了：留著的結束、網站影片的背景工作
        out.extend(self.events_drained());
        out.extend(self.net_tick());
        out
    }

    /// 下一個要處理的 mpv 事件：晚一步處理的（`replay`）優先，再來才問 mpv（最多等 `timeout` 秒）
    fn next_event(&mut self, timeout: f64) -> Option<Event> {
        if let Some(ev) = self.replay.take() {
            return Some(ev);
        }
        self.mpv.wait_event(timeout)
    }

    /// 等待下一個播放事件，最多 `timeout`（自動測試用）。
    pub fn wait(&mut self, timeout: Duration) -> Option<PlayerEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            // 留著的結束：佇列一空就要決定，不等
            let wait = if self.held_end.is_some() {
                0.0
            } else {
                self.net_wait(remaining).as_secs_f64()
            };
            match self.next_event(wait) {
                Some(ev) => {
                    if let Some(pe) = self.handle(ev) {
                        return Some(pe);
                    }
                }
                // 佇列空了（或背景的解析做完、叫醒了這裡）
                None => {
                    if let Some(pe) = self.events_drained().or_else(|| self.net_tick()) {
                        return Some(pe);
                    }
                }
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
            let ev = match self.next_event(self.net_wait(remaining).as_secs_f64().min(0.1)) {
                Some(ev) => self.handle(ev),
                None => self.events_drained().or_else(|| self.net_tick()),
            };
            if let Some(PlayerEvent::EndFile { error: Some(e), .. }) = ev {
                return Err(e);
            }
        }
        Ok(())
    }

    // ───────────── 網站影片（yt-dlp） ─────────────
    //
    // mpv 開網址之前停在 `on_load` hook：
    // - 影片網站（`hook::route`）：在背景問 yt-dlp（`hook::Job`），做完時 `mpv_wakeup` 叫醒處理事件的這裡（`net_tick`），
    //   把網頁換成真正的影片網址（`stream-open-filename`，通常是影像、聲音合成的 EDL）、設定這個檔案專用的標頭與 Cookie
    //   （`file-local-options`）、加入網站的字幕，再放行；章節等載入完成（FileLoaded）才設定。
    // - 其他網頁：照原樣開；mpv 認不出內容（是網頁）時，在 `on_load_fail` 問 yt-dlp。要不要問看錯誤記錄，
    //   記錄比 hook 晚送到，所以等事件都處理完（佇列空了）才決定。
    // - 一定剛好放行一次：解析完、失敗、看門狗（等太久）、換檔或停止（`site_abandon`；先送換檔、停止再放行，
    //   舊的檔案就直接結束）。忘了放行的話 mpv 永遠停在載入中。
    // - 播不了：原因記下來（說明開檔失敗時優先用），網址換成空的 `memory://`，mpv 馬上失敗、不再去讀網頁。
    // - 播放清單：交給介面（`PlayerEvent::NetPlaylist`），這個網址安靜地停下。

    /// 網站影片用的網路設定與偏好（啟動時、改設定時）。正在解析的不受影響，下一次開檔才用
    pub fn set_net_config(&mut self, net: &net::NetSettings, prefs: &ytdl::SitePrefs) {
        self.site.net = net.clone().sanitized();
        self.site.prefs = prefs.clone();
    }

    /// 用別的畫質重開目前的網站影片，從現在的位置接著播（20 分鐘內解析過，不用再執行 yt-dlp）。
    /// 不是網站影片時不做任何事
    pub fn reload_net(&mut self, choice: Choice) -> mpv::Result<()> {
        let Some((url, mode)) = self.reload_mode(choice) else {
            return Ok(());
        };
        self.open_with_mode(&url, mode)
    }

    /// 最近一次要開的（`open` 給的路徑或網址；開不了時也留著）：網站影片播不了、下載或更新 yt-dlp 之後「再試一次」用
    pub fn opening(&self) -> Option<&str> {
        self.opening.as_deref()
    }

    /// 換畫質重開目前的網站影片要用的（網頁的網址, 要求）：從現在的位置接著播。不是網站影片時 None。
    /// 介面自己開（`open_with_mode`），換檔時的設定（播放清單的位置、要保留的暫停、字幕…）才照它的規則做
    pub fn reload_mode(&self, choice: Choice) -> Option<(String, Mode)> {
        let url = self.state.net.as_ref().map(|n| n.page_url.clone())?;
        let at = self.position_now();
        let mode = Mode {
            choice,
            start_at: (at > 0.0).then_some(at),
            this_video: true,
            ..Default::default()
        };
        Some((url, mode))
    }

    /// 另一個 mpv（匯出片段、GIF）開目前的網路串流要的東西：要開的網址（網站影片是解析出來的網址或 EDL）
    /// 與連線的選項（`NetStream::options`）。沒有在播網路串流時 None
    pub fn net_stream(&self) -> Option<NetStream> {
        let st = &self.state;
        let path = st.path.as_deref().filter(|p| st.loaded && net::is_network(p))?;
        let wanted = self
            .site
            .wanted
            .clone()
            .unwrap_or_else(|| net::mpv_options(&self.site.net, &self.net_defaults()));
        let mut options: Vec<(String, Node)> = Vec::new();
        // 使用者用 VITASCOPE_MPV_OPTS 指定的：影戲沒有送，讀 mpv 現在的值
        let scalar = |name: &str, value: &str| {
            if self.user_overrides.contains(name) {
                self.mpv.get_string(name).ok()
            } else {
                Some(value.to_owned())
            }
        };
        for (name, value) in &wanted.scalars {
            // 快取大小、HLS 的起始畫質是這個播放器自己的，不是連線的方式
            if matches!(*name, "demuxer-max-bytes" | "demuxer-max-back-bytes" | "hls-bitrate") {
                continue;
            }
            if let Some(v) = scalar(name, value) {
                options.push(((*name).to_owned(), Node::Str(v)));
            }
        }
        let headers = if self.user_overrides.contains(net::HEADERS) {
            self.mpv.get_string_list(net::HEADERS).unwrap_or_default()
        } else {
            wanted.headers.clone()
        };
        options.push((net::HEADERS.to_owned(), Node::strings(headers)));
        if let Some(lavf) = scalar(net::LAVF, &wanted.lavf) {
            let map = lavf
                .split(',')
                .filter_map(|kv| kv.split_once('='))
                .filter(|(k, _)| !k.is_empty())
                .map(|(k, v)| (k.to_owned(), Node::Str(v.to_owned())))
                .collect();
            options.push((net::LAVF.to_owned(), Node::Map(map)));
        }
        let site = st
            .net
            .as_ref()
            .filter(|n| n.page_url == path && self.site.file.url == path)
            .and(self.site.file.stream.as_ref());
        let open = match site {
            Some((open, access)) => {
                for (name, value) in access {
                    options.retain(|(n, _)| n != name);
                    options.push((name.clone(), value.clone()));
                }
                open.clone()
            }
            None => path.to_owned(),
        };
        Some(NetStream {
            page_url: path.to_owned(),
            open,
            options,
            site: site.is_some(),
        })
    }

    /// 正在背景等 yt-dlp（自動測試用）
    #[doc(hidden)]
    pub fn site_resolving(&self) -> bool {
        self.site.job.is_some()
    }

    /// mpv 拿掉的 hook 重新註冊過幾次（自動測試用）
    #[doc(hidden)]
    pub fn hooks_readded(&self) -> u64 {
        self.site.hooks_readded
    }

    /// 測試用：當成從 mpv 收到這個事件（例如舊版 mpv 才有的記錄）
    #[doc(hidden)]
    pub fn inject_event(&mut self, ev: Event) -> Option<PlayerEvent> {
        self.handle(ev)
    }

    /// 放行 hook（每個剛好一次）
    fn continue_hook(&mut self, id: u64, name: &str) {
        match self.mpv.hook_continue(id) {
            Ok(()) => self.hooks_continued += 1,
            Err(e) => eprintln!("[vitascope] 無法繼續 hook {name}：{e}"),
        }
    }

    /// 這個 hook 是已經換掉（又開了別的、停止了）的檔案的：mpv 正在載入的已經不在它的播放清單上
    /// （`loadfile replace`、`stop` 把它拿掉了；處理到 hook 的時候可能已經換檔了）
    fn hook_is_stale(&self) -> bool {
        let playing = self.mpv.get_property::<i64>("playlist-playing-pos");
        let current = self.mpv.get_property::<i64>("playlist-current-pos");
        matches!((playing, current), (Ok(p), Ok(c)) if p < 0 || p != c)
    }

    /// `on_load`：決定這個網址要不要先問 yt-dlp
    fn site_on_load(&mut self, id: u64) -> Option<PlayerEvent> {
        if self.hook_is_stale() {
            self.continue_hook(id, "on_load");
            return None;
        }
        let entry = self.playing_entry();
        if entry.is_some() && entry == self.site.load_entry {
            self.continue_hook(id, "on_load");
            return None;
        }
        self.site.load_entry = entry;
        let url = self.mpv.get_string("path").unwrap_or_default();
        // 只對這次有效的要求（換畫質之類）：只給那個網址用一次
        let mode = self.site.mode.take().filter(|(u, _)| *u == url).map(|(_, m)| m);
        let mut route = hook::route(&url, mode.is_some(), &self.site.extra_sites);
        // 關掉了「用 yt-dlp 播放網站影片」：其他網頁照一般的網址開（開不起來也不問）；影片網站說明要打開
        if !self.site.net.ytdl && route == Route::Fallback {
            route = Route::Native;
        }
        self.site.file = SiteFile {
            url,
            route: Some(route),
            mode: mode.unwrap_or_default(),
            ..Default::default()
        };
        match route {
            Route::Site => self.site_start(id, false),
            Route::Native | Route::Fallback => {
                self.continue_hook(id, "on_load");
                None
            }
        }
    }

    /// mpv 正在載入的播放清單項目的代號（每次 loadfile 都是新的項目）
    fn playing_entry(&self) -> Option<i64> {
        let pos = self
            .mpv
            .get_property::<i64>("playlist-playing-pos")
            .ok()
            .filter(|p| *p >= 0)?;
        self.mpv.get_property::<i64>(&format!("playlist/{pos}/id")).ok()
    }

    /// `on_load_fail`：其他網頁開不起來時，看錯誤記錄決定要不要問 yt-dlp（等記錄都收到了才決定，見 `net_tick`）
    fn site_on_load_fail(&mut self, id: u64) {
        let f = &self.site.file;
        let undecided = f.route == Some(Route::Fallback) && !f.tried && f.failure.is_none();
        if undecided && self.site.deferred_fail.is_none() && !self.hook_is_stale() {
            self.site.deferred_fail = Some(id);
        } else {
            self.continue_hook(id, "on_load_fail");
        }
    }

    /// 問 yt-dlp：有快取的結果就直接用，不然在背景解析（hook 等到做完）
    fn site_start(&mut self, id: u64, after_failure: bool) -> Option<PlayerEvent> {
        if !self.site.net.ytdl {
            return self.site_failed(id, after_failure, YtdlError::Disabled.into());
        }
        let Some(resolver) = self.site.resolver.clone().filter(|r| r.available()) else {
            return self.site_failed(id, after_failure, YtdlError::Missing.into());
        };
        let url = self.site.file.url.clone();
        let mut prefs = self.site.prefs.clone();
        if self.site.file.mode.yes_playlist {
            prefs.list_mode = ytdl::ListMode::Playlist;
        } else if self.site.file.mode.this_video {
            // 換畫質：重開的是正在播的這部（播放清單的設定之後改成整個清單也一樣）
            prefs.list_mode = ytdl::ListMode::Video;
        }
        let Some(request) = Request::new(&url, &self.site.net, &prefs) else {
            self.continue_hook(id, hook_name(after_failure));
            return None;
        };
        self.site.file.tried = true;
        // 選單選的畫質、只播聲音：只用之前的格式清單，畫質、編碼的偏好改了也不用再問 yt-dlp
        let hit = if self.site.file.mode.choice == Choice::Default {
            self.site.cache.get(&request)
        } else {
            self.site.cache.get_any_format(&request)
        };
        if let Some(hit) = hit {
            return self.site_apply(id, after_failure, &hit);
        }
        let playlist = request.playlist;
        match Job::spawn(resolver, Arc::downgrade(&self.mpv), id, after_failure, request) {
            Ok(job) => {
                self.site.job = Some(job);
                self.state.net_busy = Some(hook::NetBusy {
                    url,
                    since: Instant::now(),
                    playlist,
                });
                None
            }
            Err(e) => self.site_failed(id, after_failure, YtdlError::Spawn(e.kind()).into()),
        }
    }

    /// yt-dlp 的結果 → mpv 要開的，放行 hook
    fn site_apply(&mut self, id: u64, after_failure: bool, resolved: &Resolved) -> Option<PlayerEvent> {
        self.state.net_busy = None;
        let cfg = PlanConfig {
            memory_cookies: self.memory_cookies,
            ..PlanConfig::new(
                &self.site.net,
                &self.net_defaults(),
                &self.site.prefs,
                self.user_overrides.contains("user-agent"),
            )
        };
        let url = self.site.file.url.clone();
        let planned = plan::plan(&resolved.info, &url, &self.site.file.mode, &cfg);
        match planned {
            Err(error) => self.site_failed(
                id,
                after_failure,
                Failure {
                    error,
                    hints: resolved.hints.clone(),
                },
            ),
            // 網址本身就是媒體檔：照原樣開
            Ok(Plan::Native) => {
                self.continue_hook(id, hook_name(after_failure));
                None
            }
            Ok(Plan::Media(m)) => {
                if let Err(e) = self.site_open(*m, &resolved.hints) {
                    return self.site_failed(id, after_failure, YtdlError::Other(e.to_string()).into());
                }
                self.continue_hook(id, hook_name(after_failure));
                None
            }
            Ok(Plan::Playlist {
                entries,
                start,
                chosen,
                dropped,
                capped,
            }) => {
                // 項目交給介面變成播放清單；這個網址安靜地停下（先停止再放行：原因是 stop、沒有錯誤，也不去讀網頁）
                if let Err(e) = self.mpv.command(&["stop"]) {
                    eprintln!("[vitascope] 無法停止網站的播放清單：{e}");
                }
                self.continue_hook(id, hook_name(after_failure));
                Some(PlayerEvent::NetPlaylist {
                    source: url,
                    entries,
                    start,
                    chosen,
                    dropped,
                    capped,
                })
            }
        }
    }

    /// 一部網站影片：這個檔案專用的選項（標頭、Cookie、標題、開始時間…）、真正要開的網址、網站的字幕
    fn site_open(&mut self, m: MediaPlan, hints: &[Hint]) -> mpv::Result<()> {
        for (name, value) in &m.options {
            match self.mpv.set_node(&format!("file-local-options/{name}"), value) {
                Ok(()) => self.site.file_local.push(name.clone()),
                // 少一個選項（例如舊版的 mpv 不認得）照樣試著播
                Err(e) => eprintln!("[vitascope] 網站影片：無法設定 {name}：{e}"),
            }
        }
        self.mpv.set_property("stream-open-filename", m.open.as_str())?;
        for sub in &m.subs {
            // 選到才下載（!delay_open）；不選（auto），開檔後照一般的規則選字幕（繁中優先）
            if let Err(e) = self.mpv.command(&["sub-add", &sub.url, "auto", &sub.title, &sub.lang]) {
                eprintln!("[vitascope] 網站影片：無法加入字幕 {}：{e}", sub.lang);
            }
        }
        self.site.file.chapters = m.chapters;
        let access = m
            .options
            .iter()
            .filter(|(name, _)| STREAM_ACCESS.contains(&name.as_str()))
            .cloned()
            .collect();
        self.site.file.stream = Some((m.open.clone(), access));
        self.state.net = Some(Arc::new(m.info));
        self.state.net_hints = hints.to_vec();
        Ok(())
    }

    /// 播不了：記下原因，放行 hook。`on_load` 的網址換成空的 `memory://`，mpv 馬上失敗、不再去讀網頁
    fn site_failed(&mut self, id: u64, after_failure: bool, failure: Failure) -> Option<PlayerEvent> {
        self.state.net_busy = None;
        match failure.error {
            YtdlError::Cancelled => {}
            // 開不起來之後才問、yt-dlp 也不認得：不是網站影片，照 mpv 原本的說明（是網頁、不是影片檔）
            YtdlError::Unsupported if after_failure => {}
            // 其他網頁、沒有 yt-dlp（可能找完才知道）：照 mpv 原本的說明，另外提一句要 yt-dlp
            YtdlError::Missing if after_failure => {
                self.site.file.missing = true;
                self.state.net_page_needs_ytdl = true;
            }
            _ => {
                // 影片網站、沒有 yt-dlp：起始畫面說明怎麼取得
                if failure.error == YtdlError::Missing {
                    self.state.net_need_ytdl = true;
                }
                if !after_failure && let Err(e) = self.mpv.set_property("stream-open-filename", FAIL_FAST) {
                    eprintln!("[vitascope] 網站影片：無法中止開檔：{e}");
                }
                self.site.file.failure = Some(failure);
            }
        }
        self.continue_hook(id, hook_name(after_failure));
        None
    }

    /// 換檔、停止時（mpv 已經收到）：正在等的 yt-dlp 不等了（背景的程式也通知取消），停住的 hook 放行
    fn site_abandon(&mut self) {
        if let Some(job) = self.site.job.take() {
            self.continue_hook(job.hook_id, hook_name(job.after_failure));
        }
        if let Some(id) = self.site.deferred_fail.take() {
            self.continue_hook(id, "on_load_fail");
        }
        self.state.net_busy = None;
    }

    /// 處理事件的執行緒最多等多久就要回來做網站影片的事（`wait` 系列）：等 yt-dlp 的時候至少每 0.1 秒看一次
    /// （有人直接停止、換檔時 mpv 不一定有事件；看門狗）
    fn net_wait(&self, wanted: Duration) -> Duration {
        if self.site.deferred_fail.is_some() || !self.site.lost_hooks.is_empty() {
            return Duration::ZERO;
        }
        match &self.site.job {
            Some(job) => wanted.min(job.time_left()).min(Duration::from_millis(100)),
            None => wanted,
        }
    }

    /// 網站影片的背景工作。事件都處理完時呼叫（`poll` 的最後、`wait` 系列佇列空了的時候）：
    /// 重新註冊被拿掉的 hook、決定開不起來的網頁要不要問 yt-dlp（這時錯誤記錄都收到了）、收解析的結果、看門狗
    fn net_tick(&mut self) -> Option<PlayerEvent> {
        for (userdata, name) in std::mem::take(&mut self.site.lost_hooks) {
            match self.mpv.hook_add(userdata, name, HOOK_PRIORITY) {
                Ok(()) => {
                    self.site.hooks_readded += 1;
                    eprintln!("[vitascope] mpv 拿掉了 hook {name}（事件太多），已重新註冊");
                }
                Err(e) => eprintln!("[vitascope] 無法重新註冊 hook {name}：{e}"),
            }
        }
        // 檔案已經被拿掉了（停止、直接對 mpv 換檔）：不等了
        if (self.site.job.is_some() || self.site.deferred_fail.is_some()) && self.hook_is_stale() {
            self.site_abandon();
        }
        let mut out = None;
        if let Some(id) = self.site.deferred_fail.take() {
            if !self.hook_is_stale() && hook::fallback_wanted(&self.recent_errors.join("\n")) {
                out = self.site_start(id, true);
            } else {
                self.continue_hook(id, "on_load_fail");
            }
        }
        let progress = self.site.job.as_ref().map(Job::progress);
        match progress {
            None | Some(Progress::Waiting) => {}
            Some(Progress::Done(result)) => {
                let job = self.site.job.take()?;
                let event = match *result {
                    Ok(resolved) => {
                        let resolved = Arc::new(resolved);
                        self.site.cache.insert(job.request.clone(), resolved.clone());
                        self.site_apply(job.hook_id, job.after_failure, &resolved)
                    }
                    Err(failure) => self.site_failed(job.hook_id, job.after_failure, failure),
                };
                out = out.or(event);
            }
            Some(Progress::TimedOut) => {
                let job = self.site.job.take()?;
                eprintln!(
                    "[vitascope] yt-dlp 超過 {:.0} 秒沒有回應，放棄",
                    job.watchdog.as_secs_f64()
                );
                self.site_failed(job.hook_id, job.after_failure, YtdlError::NoResponse.into());
            }
        }
        out
    }

    /// 記錄說 mpv 拿掉了 hook（「Failed sending hook command main/on_load. Removing hook.」）：記下來重新註冊。
    /// 舊的 libmpv（0.37 是「Sending hook command failed. Removing hook.」）沒寫是哪一個：兩個都重新註冊，
    /// 沒被拿掉的那一個就有兩份：重複收到的 `on_load` 認得出來（`load_entry`）、直接放行；
    /// 重複的 `on_load_fail` 已經問過 yt-dlp 就直接放行，沒問過的照同樣的記錄再決定一次（結果一樣）
    fn hook_removed(&mut self, text: &str) {
        if !self.net_hooks {
            return;
        }
        let ours = [(HOOK_ON_LOAD, "on_load"), (HOOK_ON_LOAD_FAIL, "on_load_fail")];
        let named: Vec<(u64, &'static str)> = ours
            .into_iter()
            .filter(|(_, name)| text.contains(&format!("/{name}.")))
            .collect();
        let lost = if named.is_empty() { ours.to_vec() } else { named };
        for (userdata, name) in lost {
            if !self.site.lost_hooks.iter().any(|(u, _)| *u == userdata) {
                self.site.lost_hooks.push((userdata, name));
            }
        }
    }

    /// 網站影片的章節（載入完成後才能設定）
    fn site_chapters(&mut self) {
        let chapters = std::mem::take(&mut self.site.file.chapters);
        if chapters.is_empty() {
            return;
        }
        let list = chapters
            .into_iter()
            .map(|c| {
                let mut entry = vec![("time".to_owned(), Node::Double(c.time))];
                // 沒有標題的不寫，選單上顯示「第 n 章」
                if !c.title.is_empty() {
                    entry.push(("title".to_owned(), Node::Str(c.title)));
                }
                Node::Map(entry)
            })
            .collect();
        if let Err(e) = self.mpv.set_node("chapter-list", &Node::Array(list)) {
            eprintln!("[vitascope] 網站影片：無法設定章節：{e}");
        }
    }

    /// 檔案結束：mpv 已經把 file-local 的選項還原成設定之前的值（結束的事件之前就還原了）。這段期間改過網路設定的話，
    /// 那些新的值也被蓋回去了：記下的值作廢、再送一次。用同步設定：兩個檔案之間（下一個網址的 on_load 還在等這裡放行），
    /// 網路選項只是寫進設定、不會等畫面；非同步的話下一個網址可能已經用舊的值連線了
    fn site_file_ended(&mut self) {
        let names = std::mem::take(&mut self.site.file_local);
        if !std::mem::take(&mut self.site.net_changed) || names.is_empty() {
            return;
        }
        for name in &names {
            self.options_applied.remove(name.as_str());
        }
        let Some(wanted) = self.site.wanted.clone() else {
            return;
        };
        for (name, _, result) in self.apply_net(&wanted, true) {
            if let Err(e) = result {
                eprintln!("[vitascope] 無法重新套用 {name}：{e}");
            }
        }
    }

    fn handle(&mut self, ev: Event) -> Option<PlayerEvent> {
        match ev {
            Event::PropertyChange { name, value, .. } => {
                self.apply_property(&name, value);
                // 純音訊檔換了字幕：重新判斷歌詞由誰畫
                if matches!(name.as_str(), "sid" | "track-list") {
                    self.sync_lyrics_visibility();
                }
                None
            }
            Event::Log { prefix, level, text } => {
                // 跳轉不準的格式：核對跳轉停在哪裡（分離器、cplayer 的記錄）
                self.approx_log(&text);
                // 印出的跟以前一樣照 VITASCOPE_DEBUG 的等級（向 mpv 要的至少是 v，跳轉核對要用，比要印的多）
                if self.debug_print.is_some_and(|max| log_rank(&level) <= max) {
                    eprint!("[mpv/{level}] [{prefix}] {text}");
                }
                let noise = is_harmless_error(&text)
                    || self
                        .probe_noise
                        .iter()
                        .any(|parts| parts.iter().all(|p| text.contains(p.as_str())));
                let error = matches!(level.as_str(), "error" | "fatal") && !noise;
                // 解不了的編碼（mpv 的 MP_ERR）：先記下，下面重新整理開檔失敗的說明時才用得到
                let missing = if error {
                    self.decoder_failed(&prefix, &text)
                } else {
                    None
                };
                if error && is_render_log(&prefix) {
                    if self.render_errors.len() >= RENDER_ERRORS_CAP {
                        self.render_errors.pop_front();
                    }
                    self.render_errors
                        .push_back((Instant::now(), format!("[{prefix}] {}", text.trim_end())));
                }
                // mpv 送不出 hook 的事件（佇列滿了）就把 hook 拿掉，之後的檔案都不會再問：重新註冊
                if level == "warn" && text.contains("Removing hook") {
                    self.hook_removed(&text);
                }
                // 警告只收網路的（HTTP 404 之類），其他照舊不收
                if error || (level == "warn" && is_net_warning(&prefix, &text)) {
                    push_recent_error(&mut self.recent_errors, format!("[{prefix}] {}", text.trim_end()));
                    if let Some(base) = &self.failure {
                        self.state.last_error = Some(self.compose_failure(base));
                    }
                }
                missing.map(PlayerEvent::DecoderMissing)
            }
            Event::StartFile => {
                // 留著的上一個檔案的結束（載入完成、還沒開始播就失敗）：mpv 已經開始下一個（mpv 自己的播放清單往下播、
                // 介面開了別的），不會再只播聲音重開了。照順序先通知那個結束，這個開始下一次才處理（`next_event`）
                if let Some(end) = self.held_end.take() {
                    self.replay = Some(Event::StartFile);
                    return Some(end);
                }
                // 上一個檔案的跳轉目標、核對結果不算這個檔案的
                self.note_seek(None);
                self.approx_reset();
                if let Ok(mut a) = self.approx.lock() {
                    a.landing = None;
                }
                // mpv 自己開始的下一個項目（沒有 `open`）：上一個項目的音訊輸出是不是改用的 null，現在才問
                //（見 `take_fell_back_before_open`）。兩次 `open` 只開始一個檔案時，之後的 StartFile 也當成 mpv 自己開始的
                if !std::mem::take(&mut self.open_pending) {
                    self.fell_back_before_open = self.audio_fell_back_now();
                }
                self.state.loading = true;
                self.state.loaded = false;
                self.state.last_error = None;
                self.failure = None;
                self.recent_errors.clear();
                self.loaded_subs.clear();
                self.auto_sub = None;
                self.played = false;
                self.pending_missing.clear();
                if let Ok(mut list) = self.user_subs.lock() {
                    list.clear();
                }
                // 網站影片的資料、原因是每個檔案各自的（on_load 時才設定）
                self.state.net = None;
                self.state.net_need_ytdl = false;
                self.state.net_page_needs_ytdl = false;
                self.state.net_failure = None;
                self.state.net_hints.clear();
                self.site.file = SiteFile::default();
                // 開的是什麼（網址的話，失敗時說明網路的原因）。`open` 已經記下要開的；這裡再讀 mpv 的 path，
                // 只在讀得到時換掉。很快就失敗的檔案（本機就拒絕的主機名稱只要幾毫秒），處理到這個事件時
                // mpv 可能已經結束它、讀不到 path：那就留著 `open` 記的，不能因為讀不到就當成本機檔案
                if let Ok(path) = self.mpv.get_string("path") {
                    self.opening = Some(path);
                }
                // 解不了的編碼是每個檔案各自的（之前的檔案晚到的記錄見 `decoder_failed`）；
                // 只播聲音重開的（`audio_only_retry`）帶著上一次的記錄：這次沒選影片軌，mpv 不會再說。
                // 只給重開的那個檔案（重開之前又開了別的：mpv 不會開重開的那個）
                let carry = self
                    .audio_only_carry
                    .take()
                    .filter(|(path, _)| self.opening.as_deref() == Some(path.as_str()));
                self.audio_only_retried = carry.is_some();
                self.state.missing_decoders = carry.map(|(_, m)| m).unwrap_or_default();
                // 從 `open` 算（比 mpv 真的開始早一點點，不會比較晚）；前一個檔案結束時清掉了的才從現在算
                self.load_started.get_or_insert_with(Instant::now);
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
                self.load_started = None;
                if self.external_subs {
                    self.load_external_subs();
                }
                self.load_external_audio();
                if self.auto_select_subs {
                    self.choose_subtitle();
                }
                self.site_chapters();
                self.refresh_tracks();
                self.watch_lyrics();
                self.approx_setup();
                Some(PlayerEvent::FileLoaded)
            }
            Event::EndFile { reason, error } => {
                self.state.loading = false;
                self.state.loaded = false;
                self.unwatch_lyrics();
                self.approx_reset();
                // 網路上的播放清單（IPTV 的 .m3u 網址）：mpv 讀完清單、把項目放進自己的播放清單，接著就會自己開第一個。
                // 這時就讀出來（mpv 接著會開別的項目）；介面照一般的開檔重新開，mpv 的清單回到只有一個
                if reason == EndReason::Redirect {
                    let list = self.import_playlist();
                    // 本機的清單（.pls 之類交給 mpv 讀的）照舊由 mpv 播：項目可以是本機檔案
                    if net::is_network(&list.source) {
                        self.remote_playlist = Some(list);
                    }
                }
                self.failed_after = self.load_started.take().map(|t| t.elapsed());
                self.site_file_ended();
                // 載入中讀不到軌道時收到的「解不了」記錄（mpv 關掉分離器才讀不到軌道）是這個檔案的：
                // - 載入時因為沒有東西可播而失敗（其他原因的載入失敗不算）
                // - 載入完成、還沒開始播就失敗（mpv 0.37 影像解不了時在載入完成之後才停下，錯誤碼不一定）
                let pending = std::mem::take(&mut self.pending_missing);
                let nothing_to_play = error
                    .as_ref()
                    .is_some_and(|e| e.code == libmpv2_sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY);
                let stopped_after_load = error.is_some() && self.failed_after.is_none() && !self.played;
                let mut notices = Vec::new();
                if (self.failed_after.is_some() && nothing_to_play) || stopped_after_load {
                    for (kind, codec) in pending {
                        notices.extend(self.record_missing(kind, codec, false));
                    }
                }
                // mpv 0.37：影像解不了就整個停下（聲音還沒開始）。不當成失敗，只播聲音重開一次（見 `audio_only_retry`）。
                // 載入完成、還沒開始播就失敗的：解碼器的記錄可能還沒收到（記錄排在一般事件後面），先留著這個結束，
                // 事件取完（記錄一定已經收到了）再決定（`events_drained`）
                let early =
                    error.is_some() && reason == EndReason::Error && self.failed_after.is_none() && !self.played;
                if early && self.audio_only_retry() {
                    // 這次的結束不通知；剛記下的「解不了」照樣通知（整個檔案開不起來時錯誤說明就夠了）
                    return notices.into_iter().next().map(PlayerEvent::DecoderMissing);
                }
                let error = error.map(|e| {
                    let base = failure_reason(e.code);
                    let full = self.compose_failure(base);
                    self.failure = Some(base.to_owned());
                    full
                });
                if error.is_some() {
                    self.state.last_error = error.clone();
                    self.state.net_failure = self.site.file.failure.clone();
                }
                let ev = PlayerEvent::EndFile { reason, error };
                if early && !self.audio_only_retried {
                    self.held_end = Some(ev);
                    return None;
                }
                Some(ev)
            }
            Event::PlaybackRestart => {
                // 已經結束的檔案晚到的不算（只播聲音重開看的是「結束前開始播了沒」）
                if self.state.loaded {
                    self.played = true;
                }
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
            // 網路功能的 hook：網站影片（見「網站影片」一節）。沒人處理的 hook 一律立刻放行，載入才不會永遠卡住
            Event::Hook { id, name, userdata } => match userdata {
                HOOK_ON_LOAD if self.net_hooks => self.site_on_load(id),
                HOOK_ON_LOAD_FAIL if self.net_hooks => {
                    self.site_on_load_fail(id);
                    None
                }
                _ => {
                    self.continue_hook(id, &name);
                    None
                }
            },
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
                self.apply_fake_tracks();
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
            // 關檔時 mpv 送「不可用」：跟著變回 false / None
            "paused-for-cache" => s.paused_for_cache = value.as_bool().unwrap_or(false),
            "cache-buffering-state" => s.cache_buffering = value.as_i64(),
            // 兩句之間是空字串、沒有字幕時「不可用」：都當成沒有字。停止觀察後才到的舊通知不算
            "sub-text" if self.watching_sub_text => {
                s.sub_text = value.as_str().filter(|t| !t.trim().is_empty()).map(str::to_owned);
            }
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
            self.apply_fake_tracks();
        }
    }

    /// 開檔後選字幕：
    /// - 有繁中就選繁中（其次中文、簡中），依語言碼、標題和外掛字幕的內容判斷
    /// - 沒有中文字幕時尊重 mpv 的選擇；mpv 什麼都沒選時（很多 MKV 的字幕軌沒有語言標籤）選第一條，比照 PotPlayer
    /// - 解不了的字幕格式（Teletext、ARIB、這個播放器之前解不了的…）不選：選了也只會失敗、每次開檔都提示一次
    fn choose_subtitle(&mut self) {
        let Ok(json) = self.mpv.get_string("track-list") else {
            return;
        };
        let tracks: Vec<Track> = serde_json::from_str(&json).unwrap_or_default();
        let subs: Vec<&Track> = tracks.iter().filter(|t| t.kind == TrackKind::Sub).collect();
        if subs.is_empty() {
            return;
        }
        let pick = pick_auto_sub(&subs, &self.undecodable_subs);
        let pick = pick.map(|t| (t.id, track_codec(t).to_owned()));
        if let Some((id, _)) = &pick {
            let _ = self.set_track(TrackKind::Sub, Some(*id));
        }
        self.auto_sub = pick;
    }

    /// 影像解不了時 mpv 0.37（Linux tar.gz 用的系統 libmpv）整個停下：它先開影像、才開聲音，影像失敗的當下還沒有
    /// 音訊輸出，就當成沒有東西可播（新版看的是選了哪些軌道，聲音照常播）。載入完成、還沒開始播就失敗、
    /// 而且這個檔案有影像解不了（不是封面）時，同一個檔案關掉影片軌（只對這個檔案）重開一次，「解不了」的記錄帶過去。
    /// 重開了回傳 true（這次的結束不算失敗、不通知介面）
    fn audio_only_retry(&mut self) -> bool {
        let video_failed = self
            .state
            .missing_decoders
            .iter()
            .any(|m| m.kind == TrackKind::Video && !m.cover);
        // 載入完成過（`failed_after` 是 None）、還沒開始播
        if !video_failed || self.failed_after.is_some() || self.played || self.audio_only_retried {
            return false;
        }
        let Some(path) = self.state.path.clone().or_else(|| self.opening.clone()) else {
            return false;
        };
        // mpv 自己的播放清單（交給 mpv 讀的本機 .pls 之類）：mpv 已經往下播了，重開（replace）會把清單整個換掉。
        // 照舊當成開檔失敗
        if self.mpv.get_property::<i64>("playlist-count").is_ok_and(|n| n > 1) {
            return false;
        }
        let mut args = vec!["loadfile", path.as_str(), "replace"];
        if self.loadfile_index {
            args.push("-1");
        }
        args.push("vid=no");
        if let Err(e) = self.mpv.command(&args) {
            eprintln!("[vitascope] 無法只播聲音重開：{e}");
            return false;
        }
        self.audio_only_carry = Some((path.clone(), self.state.missing_decoders.clone()));
        self.load_started = Some(Instant::now());
        // 只對這次開檔有效的要求（網站影片換畫質）：第一次開的 on_load 已經用掉了，重開的再用一次
        if self.site.file.url == path && self.site.file.mode != Mode::default() {
            self.site.mode = Some((path, self.site.file.mode.clone()));
        }
        true
    }

    /// mpv 的事件、記錄都取完了：留著的結束（載入完成、還沒開始播就失敗）現在決定。記錄一定已經收到了
    ///（mpv 寫記錄在送出結束的事件之前，取事件時記錄排在一般事件後面、同一次取完）：
    /// 影像解不了的話只播聲音重開，不然照常通知介面開檔失敗
    fn events_drained(&mut self) -> Option<PlayerEvent> {
        let ev = self.held_end.take()?;
        if self.audio_only_retry() {
            self.state.last_error = None;
            self.failure = None;
            return None;
        }
        Some(ev)
    }

    /// 播放引擎有沒有這個編碼的解碼器（影像、聲音；`decoder-list`）。讀不到清單時當成有（只說「無法解碼」，
    /// 不說引擎不支援）
    fn engine_decodes(&mut self, codec: &str) -> bool {
        let mpv = &self.mpv;
        let decoders = self.decoders.get_or_insert_with(|| {
            mpv.get_string("decoder-list")
                .ok()
                .and_then(|j| serde_json::from_str::<Vec<serde_json::Value>>(&j).ok())
                .unwrap_or_default()
                .iter()
                .filter_map(|d| d["codec"].as_str().map(str::to_owned))
                .collect()
        });
        decoders.is_empty() || decoders.contains(codec)
    }

    /// 目前檔案的軌道：測試用的假清單，或直接問 mpv（介面記下的可能還沒更新）；讀不到時是空的
    fn current_tracks(&self) -> Vec<Track> {
        if let Some(fake) = &self.fake_tracks {
            return fake.clone();
        }
        self.mpv
            .get_string("track-list")
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    /// mpv 的錯誤記錄是解不了某條軌道的話：記進 `state.missing_decoders`，回傳要通知介面的（見 `record_missing`）。
    /// 記錄比一般事件晚到，可能是上一個檔案的：
    /// - mpv 現在的檔案有軌道、但沒有這個編碼的，不算這個檔案的
    /// - 讀不到軌道、檔案已經結束了（整個檔案開不起來）：照樣記下，開檔失敗的說明才寫得出原因
    /// - 讀不到軌道、下一個檔案還在載入：先留著（`pending_missing`），這個檔案開不起來才算它的
    fn decoder_failed(&mut self, prefix: &str, text: &str) -> Option<MissingDecoder> {
        let (kind, codec) = missing_decoder(prefix, text)?;
        let tracks = self.current_tracks();
        let has = |k: TrackKind| tracks.iter().find(|t| t.kind == k && track_codec(t) == codec);
        let kind = kind.or_else(|| {
            [TrackKind::Video, TrackKind::Audio]
                .into_iter()
                .find(|k| has(*k).is_some())
        })?;
        let track = has(kind);
        if track.is_none() && !tracks.is_empty() {
            return None;
        }
        if track.is_none() && self.state.loading {
            self.pending_missing.push((kind, codec));
            return None;
        }
        let cover = track.is_some_and(|t| t.albumart);
        self.record_missing(kind, codec, cover)
    }

    /// 記下這個檔案有一條軌道解不了，回傳要通知介面的（同一個檔案同一條只通知一次；自動選的字幕不通知）。
    /// - 影像解不了（只播聲音）：歌詞、字幕改由介面畫（跟純音訊檔一樣，mpv 沒有畫面可畫）
    /// - 開檔時自動選的字幕解不了：改選別條
    fn record_missing(&mut self, kind: TrackKind, codec: String, cover: bool) -> Option<MissingDecoder> {
        let auto_sub_failed = kind == TrackKind::Sub && self.auto_sub.as_ref().is_some_and(|(_, c)| *c == codec);
        if kind == TrackKind::Sub {
            self.undecodable_subs.insert(codec.clone());
        }
        // 字幕：使用者自己選的才提示（mpv 選了標成預設的、影戲開檔時選的都不提示）
        let quiet = kind == TrackKind::Sub && !self.user_subs.lock().is_ok_and(|list| list.contains(&codec));
        if let Some(m) = self
            .state
            .missing_decoders
            .iter_mut()
            .find(|m| m.kind == kind && m.codec == codec)
        {
            // 開檔時自動選的解不了（沒提示），現在使用者自己選了這條：提示
            if m.quiet && !quiet {
                m.quiet = false;
                return Some(m.clone());
            }
            return None;
        }
        let unsupported = kind == TrackKind::Sub || !self.engine_decodes(&codec);
        let missing = MissingDecoder {
            kind,
            codec,
            unsupported,
            cover,
            quiet,
        };
        self.state.missing_decoders.push(missing.clone());
        if self.state.loaded {
            if kind == TrackKind::Video {
                self.watch_lyrics();
                self.sync_lyrics_visibility();
            }
            if auto_sub_failed && self.auto_select_subs {
                self.choose_subtitle();
            }
        }
        (!quiet).then_some(missing)
    }

    /// 這個檔案開始播了（載入完成之後的第一個 PlaybackRestart）：介面等到這時才提示解不了的編碼
    ///（mpv 0.37 影像解不了時，載入完成之後還會整個停下、只播聲音重開，見 `audio_only_retry`）
    pub fn playback_started(&self) -> bool {
        self.played
    }

    /// 現在這個檔案是影像解不了、只播聲音重開的（mpv 0.37；見 `audio_only_retry`）：
    /// 對介面來說還是同一個檔案（解不了的已經提示過、一起拖進來的字幕要再載入一次）
    pub fn reopened_audio_only(&self) -> bool {
        self.audio_only_retried
    }

    /// 基本原因加上 mpv 記錄裡的細節，整理成給使用者看的說明。
    fn compose_failure(&self, base: &str) -> String {
        // 載入時影像、聲音都解不了：說是哪個編碼（記錄裡同時有「No video or audio streams」，不能說成檔案裡沒有影像或聲音）。
        // 播了一陣子才失敗的（只播聲音的檔案網路斷了之類）照下面的原因說明
        if self.failed_after.is_some()
            && let Some(msg) = decoder_failure(&self.state.missing_decoders)
        {
            return msg;
        }
        // 網站影片：yt-dlp 說的原因（記錄裡只有「讀不到 memory://」之類）
        if let Some(f) = &self.site.file.failure {
            return f.error.message();
        }
        // 常見情況直接翻成中文；其他的附上 mpv 的原始訊息，方便回報問題
        let all = self.recent_errors.join("\n");
        // 網址：記錄裡看得出網路的原因（找不到伺服器、HTTP 404、逾時…）就用它
        let url = self.opening.as_deref().filter(|p| crate::m3u::is_url(p));
        let timeout = || self.mpv.get_property::<f64>("network-timeout").unwrap_or(60.0);
        if let Some(f) = url.and_then(|u| net::classify_failure(&all, u)) {
            let msg = f.message(timeout());
            // 網頁、沒有 yt-dlp：網頁上的影片要 yt-dlp 才能播
            if f == net::NetFailure::WebPage && self.site.file.missing {
                return crate::tf!(
                    "{msg}（網站上的影片要用 yt-dlp 播放）",
                    "{msg} (videos on websites need yt-dlp)"
                );
            }
            return msg;
        }
        // 網路串流沒有「檔案」：本機檔案的說明（檔案不存在、沒有讀取權限）不適用
        let network = url.is_some_and(net::is_network);
        // 記錄裡看不出原因、但花滿了逾時才失敗：是逾時。FFmpeg 7 以前（系統的 libmpv 0.37）
        // 讀取逾時不記錄，只有 mpv 的「Failed to open」
        if network && self.failed_after.is_some_and(|d| d.as_secs_f64() >= timeout() * 0.95) {
            return net::NetFailure::TimedOut.message(timeout());
        }
        let file_only = ["No such file", "Failed to open", "Permission denied"];
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
        if let Some((_, zh)) = known
            .iter()
            .filter(|(en, _)| !(network && file_only.contains(en)))
            .find(|(en, _)| all.contains(en))
        {
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
        ASYNC_BASE, AsyncKey, State, Track, TrackKind, async_id, async_key, client_log_level, debug_log_level,
        display_size, env_option_names, env_options, has_out_format, is_harmless_error, is_hdr_gamma, is_net_warning,
        is_render_log, memory_cookies, mpv_version, picture_key, requested_log_level, sound_key, spdif_format,
        spdif_live,
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
        assert!(!memory_cookies("mpv 0.37.0"));
        assert!(memory_cookies("mpv 0.38.0"));
        assert!(memory_cookies("mpv v0.41.0-1102-g6c092d978"));
        assert!(memory_cookies("mpv git-2024"));
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
    fn main_player_asks_for_verbose_logs() {
        // 跳轉不準的格式要讀 v 等級的「跳轉完成」記錄（見 approx_seek）；除錯要更多時照除錯的
        assert_eq!(requested_log_level(None), "v");
        assert_eq!(requested_log_level(Some("1")), "v");
        assert_eq!(requested_log_level(Some("error")), "v");
        assert_eq!(requested_log_level(Some("debug")), "debug");
        assert_eq!(requested_log_level(Some("trace")), "trace");
        // 印出的等級不變：沒設定時只印錯誤
        assert_eq!(debug_log_level(None), "error");
    }

    #[test]
    fn client_always_gets_warnings() {
        // 至少要警告（FFmpeg 的 HTTP 錯誤是警告）；除錯要更多時照除錯的
        assert_eq!(client_log_level(None), "warn");
        assert_eq!(client_log_level(Some("error")), "warn");
        assert_eq!(client_log_level(Some("fatal")), "warn");
        assert_eq!(client_log_level(Some("1")), "warn");
        assert_eq!(client_log_level(Some("v")), "v");
        assert_eq!(client_log_level(Some("trace")), "trace");
        // 印出的等級（debug_log_level）不變：沒設定時只印錯誤
        assert_eq!(debug_log_level(None), "error");
        // 印不印照等級的順序：VITASCOPE_DEBUG=error 不印警告
        use super::log_rank;
        assert!(log_rank("error") <= log_rank(debug_log_level(Some("error"))));
        assert!(log_rank("warn") > log_rank(debug_log_level(Some("error"))));
        assert!(log_rank("warn") <= log_rank(debug_log_level(Some("1"))));
        assert!(log_rank("fatal") < log_rank("error") && log_rank("v") < log_rank("trace"));
        assert!(log_rank("???") > log_rank("trace"));
    }

    #[test]
    fn only_http_errors_from_ffmpeg_are_kept_as_warnings() {
        assert!(is_net_warning("ffmpeg", "http: HTTP error 404 Not Found\n"));
        assert!(is_net_warning("ffmpeg/demuxer", "hls: HTTP error 403 Forbidden\n"));
        assert!(!is_net_warning("ffmpeg", "http: Stream ends prematurely at 100\n"));
        assert!(!is_net_warning("cplayer", "HTTP error 404\n"), "不是 FFmpeg 的");
        assert!(!is_net_warning("vo/gpu", "just a warning\n"));
        // 送進錯誤記錄（開網址失敗時用來說明），其他警告不收
        use crate::mpv::Event;
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let log = |prefix: &str, level: &str, text: &str| Event::Log {
            prefix: prefix.into(),
            level: level.into(),
            text: text.into(),
        };
        p.handle(log("ffmpeg", "warn", "http: HTTP error 404 Not Found\n"));
        p.handle(log("ffmpeg", "warn", "http: Stream ends prematurely\n"));
        p.handle(log("ffmpeg", "info", "http: HTTP error 500 (info)\n"));
        assert_eq!(p.recent_errors(), ["[ffmpeg] http: HTTP error 404 Not Found"]);
        assert!(p.take_render_errors().is_empty());
    }

    /// 開網址失敗的說明不靠事件處理得多快：`open` 記下網址，StartFile 時讀不到 mpv 的 path
    ///（檔案已經結束）也照樣當網址說明；看不出原因時不說「檔案不存在」；花滿逾時的算逾時
    #[test]
    fn url_failures_are_explained_without_racing_mpv() {
        use crate::mpv::{EndReason, Error, Event};
        use std::time::{Duration, Instant};
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let log = |prefix: &str, text: &str| Event::Log {
            prefix: prefix.into(),
            level: "error".into(),
            text: text.into(),
        };
        let failed = || Event::EndFile {
            reason: EndReason::Error,
            error: Some(Error {
                code: libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED,
                context: String::new(),
            }),
        };
        let last = |p: &super::Player| p.state.last_error.clone().unwrap_or_default();

        // open 記下要開的（這裡開的是本機產生的影像，不會連網路）
        let src = "av://lavfi:color=duration=60";
        p.open(src).unwrap();
        assert_eq!(p.opening.as_deref(), Some(src));
        p.stop().unwrap();
        // 等 mpv 真的停下（path 讀不到）；事件不處理，下面自己送
        let start = Instant::now();
        while p.mpv.get_string("path").is_ok() {
            assert!(start.elapsed() < Duration::from_secs(30), "mpv 停不下來");
            std::thread::sleep(Duration::from_millis(10));
        }
        // 沒在播：StartFile 讀不到 path（像是很快就失敗、已經結束的網址）
        let url = "http://unreachable.invalid/a.mp4";
        p.opening = Some(url.into());
        p.handle(Event::StartFile);
        assert_eq!(p.opening.as_deref(), Some(url), "讀不到 path 時留著 open 記的");
        p.handle(log(
            "ffmpeg",
            "tcp: Failed to resolve hostname unreachable.invalid: No such host\n",
        ));
        p.handle(failed());
        assert_eq!(last(&p), "無法開啟網址：找不到伺服器（請確認網址與網路連線）");

        // 記錄裡只有一般的開檔失敗：網址不說「檔案不存在」，本機檔案照舊
        for (opening, want_file_text) in [(url, false), (r"C:\影片\a.mp4", true)] {
            p.opening = Some(opening.into());
            p.handle(Event::StartFile);
            p.handle(log("stream", &format!("Failed to open {opening}.\n")));
            p.handle(failed());
            let err = last(&p);
            assert_eq!(
                err.contains("檔案不存在，或沒有讀取權限"),
                want_file_text,
                "{opening}: {err}"
            );
            assert!(err.starts_with("無法載入檔案"), "{err}");
        }

        // 看不出原因、花滿了逾時（FFmpeg 6 讀取逾時不記錄）：逾時；沒花滿的不算
        p.mpv.set_property("network-timeout", 2.0).unwrap();
        for (took, want_timeout) in [(Duration::from_secs(3), true), (Duration::from_millis(100), false)] {
            p.opening = Some(url.into());
            p.handle(Event::StartFile);
            p.load_started = Instant::now().checked_sub(took);
            p.handle(log("stream", &format!("Failed to open {url}.\n")));
            p.handle(failed());
            let err = last(&p);
            assert_eq!(
                err == "無法開啟網址：連線逾時（2 秒內沒有回應）",
                want_timeout,
                "{took:?}: {err}"
            );
        }
        // 載入成功過的不算開檔花的時間
        p.load_started = Instant::now().checked_sub(Duration::from_secs(3));
        p.handle(Event::FileLoaded);
        assert_eq!(p.load_started, None);
    }

    #[test]
    fn network_state_is_observed_last() {
        // 只能加在最後面（前面的編號不能變）；裝置清單的編號接在後面
        let names: Vec<&str> = super::OBSERVED.iter().map(|(n, _)| *n).collect();
        assert_eq!(names[names.len() - 2..], ["paused-for-cache", "cache-buffering-state"]);
        assert_eq!(super::DEVICE_LIST_ID, super::OBSERVED.len() as u64 + 1);
        // Net 接在 C2 之前最後一個的後面（之後的會再接在它後面，見設計 §1.9）
        assert_eq!(AsyncKey::Net as usize, AsyncKey::AfCommand as usize + 1);
        // 重複單一檔案（E2）接在 Net 後面
        assert_eq!(AsyncKey::LoopFile as usize, AsyncKey::Net as usize + 1);
        assert_eq!(
            super::async_key(super::async_id(AsyncKey::LoopFile, 7)),
            Some(AsyncKey::LoopFile)
        );
    }

    /// 系統媒體控制的封面（E6）接在重複單一檔案後面；之後的再接在它後面
    #[test]
    fn cover_key_is_appended_after_loop_file() {
        assert_eq!(AsyncKey::Cover as usize, AsyncKey::LoopFile as usize + 1);
        assert_eq!(
            super::async_key(super::async_id(AsyncKey::Cover, 3)),
            Some(AsyncKey::Cover)
        );
    }

    /// 換掉的檔案的音訊輸出是不是改用的 null，開檔（送出 loadfile）之前直接問 mpv：
    /// 跟事件、屬性通知怎麼分批無關（這裡一個事件都不處理）
    #[test]
    fn fallback_before_open_is_asked_when_opening() {
        use std::time::{Duration, Instant};
        // 引擎沒有 FFmpeg 的 sine 來源濾鏡：用產生的樣本
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/samples/generated/general/audio_flac.flac");
        let player = |ao: &str, file: &str| {
            let extra = [("ao", ao), ("ao-pcm-file", file), ("pause", "yes")];
            super::Player::new(super::Options {
                extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                ..super::Options::headless()
            })
            .unwrap()
        };
        // 暫停中 mpv 照樣開音訊輸出
        let wait_ao = |p: &super::Player, want: &str| {
            let start = Instant::now();
            while p.mpv.get_string("current-ao").ok().as_deref() != Some(want) {
                assert!(
                    start.elapsed() < Duration::from_secs(30),
                    "current-ao 沒有變成 {want}：{:?} {:?} {:?} {:?}",
                    p.mpv.get_string("current-ao"),
                    p.mpv.get_string("path"),
                    p.mpv.get_string("audio-params"),
                    p.mpv.get_string("core-idle"),
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let missing = std::env::temp_dir()
            .join(format!("vitascope-no-such-dir-{}", std::process::id()))
            .join("out.wav");

        // 開不起來：第一個檔案沒有換掉的輸出，不算；第二個檔案換掉的是改用的 null
        let mut p = player("pcm", &missing.to_string_lossy());
        p.open(src).unwrap();
        assert!(!p.take_fell_back_before_open(), "第一個檔案沒有上一個");
        wait_ao(&p, "null");
        assert_eq!(p.state.current_ao, None, "沒有處理事件：觀察到的還不知道");
        p.open(src).unwrap();
        assert!(p.take_fell_back_before_open());
        assert!(!p.take_fell_back_before_open(), "讀過就清掉");
        // 下一次開檔照當時的：新檔案還是沿用 null
        wait_ao(&p, "null");
        p.open(src).unwrap();
        assert!(p.take_fell_back_before_open());

        // 真正的輸出開得起來：不算
        let mut q = player("pcm", null_device);
        q.open(src).unwrap();
        wait_ao(&q, "pcm");
        q.open(src).unwrap();
        assert!(!q.take_fell_back_before_open());

        // ao 本來就指定 null：不是開不起來改用的
        let mut r = player("null", null_device);
        r.open(src).unwrap();
        wait_ao(&r, "null");
        r.open(src).unwrap();
        assert!(!r.take_fell_back_before_open());
    }

    #[test]
    fn network_state_follows_mpv() {
        // 本機的測試伺服器不會卡住，等不到真的暫停等待快取；直接送 mpv 的屬性變化事件（不靠時間）
        use crate::mpv::{Event, Value};
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let change = |name: &str, value: Value| Event::PropertyChange {
            id: super::OBSERVED.iter().position(|(n, _)| *n == name).unwrap() as u64 + 1,
            name: name.into(),
            value,
        };
        assert!(!p.state.paused_for_cache);
        assert_eq!(p.state.cache_buffering, None);
        p.handle(change("paused-for-cache", Value::Flag(true)));
        p.handle(change("cache-buffering-state", Value::Int64(37)));
        assert!(p.state.paused_for_cache);
        assert_eq!(p.state.cache_buffering, Some(37));
        p.handle(change("paused-for-cache", Value::Flag(false)));
        p.handle(change("cache-buffering-state", Value::None));
        assert!(!p.state.paused_for_cache);
        assert_eq!(p.state.cache_buffering, None);
    }

    #[test]
    fn sub_text_is_observed_on_demand() {
        // sub-text 不放進 OBSERVED（每部影片每一格都要多算一次），用自己的編號、要用時才觀察
        assert!(super::OBSERVED.iter().all(|(name, _)| *name != "sub-text"));
        assert_eq!(super::SUB_TEXT_ID, super::OBSERVED.len() as u64 + 2);
        assert_ne!(super::SUB_TEXT_ID, super::DEVICE_LIST_ID);

        use crate::mpv::{Event, Value};
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let text = |v: Value| Event::PropertyChange {
            id: super::SUB_TEXT_ID,
            name: "sub-text".into(),
            value: v,
        };
        // 沒在觀察（影片、停止觀察後才到的舊通知）：不算
        p.handle(text(Value::String("舊的歌詞".into())));
        assert_eq!(p.state.sub_text, None);
        p.watching_sub_text = true;
        p.handle(text(Value::String("影戲播放器測試".into())));
        assert_eq!(p.state.sub_text.as_deref(), Some("影戲播放器測試"));
        // 兩句之間（空字串）、不可用：沒有字
        p.handle(text(Value::String(" \n".into())));
        assert_eq!(p.state.sub_text, None);
        p.handle(text(Value::String("下一句".into())));
        p.handle(text(Value::None));
        assert_eq!(p.state.sub_text, None);
        // 關檔：停止觀察、清掉
        p.handle(text(Value::String("最後一句".into())));
        p.handle(Event::EndFile {
            reason: crate::mpv::EndReason::Stop,
            error: None,
        });
        assert!(!p.watching_sub_text);
        assert_eq!(p.state.sub_text, None);
        p.handle(text(Value::String("晚到的".into())));
        assert_eq!(p.state.sub_text, None);
    }

    #[test]
    fn sub_text_is_unobserved_when_the_file_ends() {
        use std::time::Duration;
        // 關檔時真的叫 mpv 停止觀察（不只是不理它的通知）：不然之後的影片每一格都還要多算一次，
        // 每開一個音樂檔又多疊一個觀察
        let song = concat!(env!("CARGO_MANIFEST_DIR"), "/samples/generated/general/audio_flac.flac");
        assert!(
            std::path::Path::new(song).exists(),
            "找不到樣本 {song}，請先執行：python scripts/gen_samples.py"
        );
        let timeout = Duration::from_secs(15);
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        p.set_pause(true).unwrap();
        let open = |p: &mut super::Player, path: &str| {
            p.open(path).unwrap();
            p.wait_for(timeout, |e| *e == super::PlayerEvent::FileLoaded).unwrap();
        };
        // 連開兩個音樂檔：只有一個觀察（第一個檔案關掉時停止了）
        open(&mut p, song);
        assert!(p.watching_sub_text);
        open(&mut p, song);
        assert!(p.watching_sub_text);
        assert_eq!(
            p.mpv.unobserve(super::SUB_TEXT_ID).unwrap(),
            1,
            "只有一個 sub-text 的觀察"
        );
        p.mpv
            .observe(super::SUB_TEXT_ID, "sub-text", super::Format::String)
            .unwrap();
        // 換成影片：已經停止觀察
        open(&mut p, "av://lavfi:testsrc2=size=320x240:rate=30:duration=3");
        assert!(!p.watching_sub_text);
        assert_eq!(p.mpv.unobserve(super::SUB_TEXT_ID).unwrap(), 0, "影片不觀察 sub-text");
    }

    #[test]
    fn lyrics_in_app_only_for_text_it_can_draw() {
        let track = |json: &str| serde_json::from_str::<super::Track>(json).unwrap();
        let audio = || track(r#"{"id":1,"type":"audio","codec":"flac","selected":true}"#);
        let cover = || track(r#"{"id":1,"type":"video","codec":"mjpeg","albumart":true,"selected":true}"#);
        let sub = |codec: &str| track(&format!(r#"{{"id":1,"type":"sub","codec":"{codec}","selected":true}}"#));
        let state = |tracks: Vec<super::Track>| super::State {
            tracks,
            ..Default::default()
        };
        // 沒有封面：文字字幕都由介面畫（ASS 也是，mpv 沒有畫面可畫）；圖片字幕沒有文字
        for codec in ["text", "subrip", "webvtt", "mpl2", "pjs", "eia_608", "ass"] {
            assert!(state(vec![audio(), sub(codec)]).lyrics_in_app(), "{codec}");
        }
        for codec in ["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle"] {
            assert!(!state(vec![audio(), sub(codec)]).lyrics_in_app(), "{codec}");
        }
        // 有封面：純文字由介面畫在底色上；ASS 的樣式、卡拉 OK 效果和圖片字幕交給 mpv 畫在封面上
        assert!(state(vec![cover(), audio(), sub("text")]).lyrics_in_app());
        assert!(state(vec![cover(), audio(), sub("subrip")]).lyrics_in_app());
        for codec in ["ass", "ssa", "hdmv_pgs_subtitle", "dvd_subtitle"] {
            assert!(!state(vec![cover(), audio(), sub(codec)]).lyrics_in_app(), "{codec}");
        }
        // 沒選字幕、影片、還不知道有哪些軌道：都不是
        assert!(!state(vec![audio()]).lyrics_in_app());
        let video = track(r#"{"id":1,"type":"video","codec":"h264","selected":true}"#);
        assert!(!state(vec![video, audio(), sub("text")]).lyrics_in_app());
        assert!(!state(vec![]).lyrics_in_app());
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

    /// 別的播放器、匯出的 FFmpeg 錯誤一大串（都送到第一個建立的 mpv）：這個檔案 mpv 自己的錯誤不會被擠掉
    #[test]
    fn ffmpeg_floods_do_not_push_out_mpv_errors() {
        use super::push_recent_error;
        let mut list = Vec::new();
        for i in 0..3 {
            push_recent_error(&mut list, format!("[ffmpeg] early {i}"));
        }
        push_recent_error(&mut list, "[ao/wasapi] cannot open device".into());
        push_recent_error(
            &mut list,
            "[vd] Failed to initialize a decoder for codec 'avs2'.".into(),
        );
        for i in 0..20 {
            push_recent_error(&mut list, format!("[ffmpeg/demuxer] partial file {i}"));
        }
        assert_eq!(list.len(), 8);
        assert_eq!(list[0], "[ao/wasapi] cannot open device", "{list:?}");
        assert_eq!(list[1], "[vd] Failed to initialize a decoder for codec 'avs2'.");
        assert_eq!(list[7], "[ffmpeg/demuxer] partial file 19", "最新的在最後面：{list:?}");
        // mpv 自己的錯誤一大串也一樣：FFmpeg 的留一半
        for i in 0..20 {
            push_recent_error(&mut list, format!("[cplayer] e{i}"));
        }
        assert_eq!(list.iter().filter(|e| e.starts_with("[ffmpeg")).count(), 4, "{list:?}");
        assert_eq!(
            list[4..],
            ["[cplayer] e16", "[cplayer] e17", "[cplayer] e18", "[cplayer] e19"].map(String::from)
        );
        // 一開始 mpv 的只有兩筆：先丟 FFmpeg 的，mpv 的兩筆留到 mpv 的佔滿一半才輪到
        let mut few = Vec::new();
        push_recent_error(&mut few, "[ao] a".into());
        for i in 0..7 {
            push_recent_error(&mut few, format!("[ffmpeg] {i}"));
        }
        push_recent_error(&mut few, "[cplayer] b".into());
        assert_eq!(few[0], "[ao] a", "{few:?}");
        assert_eq!(few.iter().filter(|e| e.starts_with("[ffmpeg")).count(), 6, "{few:?}");
        // 只有一類時跟以前一樣：留最新的 8 筆
        let mut only = Vec::new();
        for i in 0..20 {
            push_recent_error(&mut only, format!("[ffmpeg] {i}"));
        }
        assert_eq!(only.first().map(String::as_str), Some("[ffmpeg] 12"));
    }

    #[test]
    fn decoder_log_lines_are_parsed() {
        use super::missing_decoder;
        assert_eq!(
            missing_decoder("vd", "Failed to initialize a decoder for codec 'avs2'.\n"),
            Some((Some(TrackKind::Video), "avs2".to_owned()))
        );
        assert_eq!(
            missing_decoder("ad", "Failed to initialize a decoder for codec 'ac4'.\n"),
            Some((Some(TrackKind::Audio), "ac4".to_owned()))
        );
        // 沒有編碼名稱時 mpv 寫 <?>
        assert_eq!(
            missing_decoder("vd", "Failed to initialize a decoder for codec '<?>'.\n"),
            Some((Some(TrackKind::Video), "<?>".to_owned()))
        );
        assert_eq!(
            missing_decoder(
                "vd",
                "Failed to initialize a decoder for codec ''.
"
            ),
            Some((Some(TrackKind::Video), "<?>".to_owned()))
        );
        // 其他來源：種類由呼叫的一方查軌道清單
        assert_eq!(
            missing_decoder("lavfi", "Failed to initialize a decoder for codec 'aac'.\n"),
            Some((None, "aac".to_owned()))
        );
        assert_eq!(
            missing_decoder("sub", "Could not find subtitle decoder for format 'dvb_teletext'.\n"),
            Some((Some(TrackKind::Sub), "dvb_teletext".to_owned()))
        );
        // 其他的錯誤、單一解碼器失敗的警告都不是
        assert_eq!(missing_decoder("vd", "Decoder init failed for libdav1d\n"), None);
        assert_eq!(
            missing_decoder("sub/ass", "Could not open libavcodec subtitle converter\n"),
            None
        );
        assert_eq!(
            missing_decoder("cplayer", "No video or audio streams selected.\n"),
            None
        );
    }

    #[test]
    fn codec_names_for_people() {
        use super::codec_display_name as name;
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(name("h264"), "H.264");
        assert_eq!(name("hevc"), "HEVC");
        assert_eq!(name("avs2"), "AVS2");
        assert_eq!(name("avs3"), "AVS3");
        assert_eq!(name("cavs"), "AVS");
        assert_eq!(name("evc"), "MPEG-5 EVC");
        assert_eq!(name("vvc"), "H.266 / VVC");
        assert_eq!(name("apv"), "APV");
        assert_eq!(name("ac4"), "AC-4");
        assert_eq!(name("ttml"), "TTML");
        assert_eq!(name("dvb_teletext"), "Teletext（圖文電視）");
        assert_eq!(name("arib_caption"), "ARIB 字幕");
        assert_eq!(name("<?>"), "未知的編碼");
        assert_eq!(name("null"), "未知的編碼");
        // 不在表上的：mpv 的名稱轉大寫
        assert_eq!(name("wmv3"), "WMV3");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(name("dvb_teletext"), "Teletext");
        assert_eq!(name("arib_caption"), "ARIB captions");
        assert_eq!(name("<?>"), "unknown codec");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }

    #[test]
    fn decoder_messages_say_what_is_unsupported() {
        use super::{MissingDecoder, decoder_failure};
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        let m = |kind, codec: &str, unsupported| MissingDecoder {
            kind,
            codec: codec.into(),
            unsupported,
            cover: false,
            quiet: false,
        };
        // 引擎沒有的編碼：不支援；有解碼器、開不起來：無法解碼（不能說成不支援）
        assert_eq!(
            m(TrackKind::Video, "avs2", true).notice(true),
            "這個影片的影像編碼（AVS2）影戲不支援，只播放聲音"
        );
        assert_eq!(
            m(TrackKind::Video, "h264", false).notice(true),
            "無法解碼 H.264 影像（檔案可能損壞，或用了引擎不支援的設定），只播放聲音"
        );
        assert_eq!(
            m(TrackKind::Audio, "ac4", true).notice(true),
            "這個檔案的音訊編碼（AC-4）影戲不支援，只播放影像"
        );
        assert_eq!(
            m(TrackKind::Audio, "aac", false).notice(true),
            "無法解碼 AAC 聲音（檔案可能損壞，或用了引擎不支援的設定），只播放影像"
        );
        assert_eq!(
            m(TrackKind::Audio, "ac4", true).notice(false),
            "這個檔案的音訊編碼（AC-4）影戲不支援"
        );
        assert_eq!(
            m(TrackKind::Sub, "dvb_teletext", true).notice(true),
            "這條字幕的格式（Teletext（圖文電視））影戲不支援"
        );
        assert_eq!(m(TrackKind::Video, "avs2", true).label(), "AVS2（不支援）");
        assert_eq!(m(TrackKind::Video, "h264", false).label(), "H.264（無法解碼）");
        // 整個檔案開不起來的說明：引擎沒有的優先；字幕不算
        assert_eq!(decoder_failure(&[]), None);
        assert_eq!(decoder_failure(&[m(TrackKind::Sub, "ttml", true)]), None);
        assert_eq!(
            decoder_failure(&[m(TrackKind::Video, "avs2", true)]).as_deref(),
            Some("不支援的編碼：AVS2")
        );
        assert_eq!(
            decoder_failure(&[m(TrackKind::Video, "avs2", true), m(TrackKind::Audio, "ac4", true)]).as_deref(),
            Some("不支援的編碼：AVS2、AC-4")
        );
        assert_eq!(
            decoder_failure(&[m(TrackKind::Audio, "aac", false), m(TrackKind::Video, "avs2", true)]).as_deref(),
            Some("不支援的編碼：AVS2")
        );
        assert_eq!(
            decoder_failure(&[m(TrackKind::Video, "h264", false)]).as_deref(),
            Some("無法解碼 H.264（檔案可能損壞，或用了引擎不支援的設定）")
        );
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(
            m(TrackKind::Video, "avs2", true).notice(true),
            "This video's codec (AVS2) isn't supported; playing the audio only"
        );
        assert_eq!(
            m(TrackKind::Sub, "ttml", true).notice(true),
            "This subtitle format (TTML) isn't supported"
        );
        assert_eq!(m(TrackKind::Audio, "aac", false).label(), "AAC (can't decode)");
        assert_eq!(
            decoder_failure(&[m(TrackKind::Video, "avs2", true)]).as_deref(),
            Some("Unsupported codec: AVS2")
        );
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }

    #[test]
    fn undecodable_video_does_not_count_as_video() {
        use super::MissingDecoder;
        let track = |json: &str| serde_json::from_str::<Track>(json).unwrap();
        let mut state = State {
            tracks: vec![
                track(r#"{"id":1,"type":"video","codec":"avs2"}"#),
                track(r#"{"id":1,"type":"audio","codec":"aac","selected":true}"#),
            ],
            ..Default::default()
        };
        assert!(state.has_video() && state.has_picture());
        state.missing_decoders.push(MissingDecoder {
            kind: TrackKind::Video,
            codec: "avs2".into(),
            unsupported: true,
            cover: false,
            quiet: false,
        });
        // 只播聲音：介面當成純音訊檔（不畫上一個檔案留下的畫面）
        assert!(!state.has_video() && !state.has_picture());
        assert!(state.decodable(&state.tracks[1]));
        // 同一個編碼名稱、別種軌道不受影響
        state.missing_decoders[0].kind = TrackKind::Audio;
        assert!(state.has_video());
    }

    /// mpv 的解碼器錯誤記錄 → `state.missing_decoders`、`PlayerEvent::DecoderMissing`、開檔失敗的說明。
    /// 用假的軌道清單（沒有真的檔案），記錄照各種順序送
    #[test]
    fn decoder_failures_are_recorded_once_and_explain_the_failure() {
        use super::MissingDecoder;
        use crate::mpv::{EndReason, Error, Event};
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let log = |prefix: &str, level: &str, text: &str| Event::Log {
            prefix: prefix.into(),
            level: level.into(),
            text: text.into(),
        };
        let failed = |code| Event::EndFile {
            reason: EndReason::Error,
            error: Some(Error {
                code,
                context: String::new(),
            }),
        };
        let track = |json: &str| serde_json::from_str::<Track>(json).unwrap();
        let missing = |kind, codec: &str, unsupported, cover| {
            Some(super::PlayerEvent::DecoderMissing(MissingDecoder {
                kind,
                codec: codec.into(),
                unsupported,
                cover,
                quiet: false,
            }))
        };
        // 播放引擎有的解碼器（每個版本的 FFmpeg 都有）、沒有的（AVS2 要 GPL 的 libdavs2）
        assert!(p.engine_decodes("aac") && p.engine_decodes("h264") && p.engine_decodes("mjpeg"));
        assert!(!p.engine_decodes("avs2"));
        p.set_fake_tracks(Some(vec![
            track(r#"{"id":1,"type":"video","codec":"avs2","selected":true}"#),
            track(r#"{"id":2,"type":"video","codec":"mjpeg","albumart":true}"#),
            track(r#"{"id":1,"type":"audio","codec":"aac","selected":true}"#),
            track(r#"{"id":1,"type":"sub","codec":"dvb_teletext"}"#),
        ]));
        p.handle(Event::StartFile);
        let avs2 = "Failed to initialize a decoder for codec 'avs2'.\n";
        // 引擎沒有 AVS2 的解碼器（decoder-list 裡沒有）：不支援
        assert_eq!(
            p.handle(log("vd", "error", avs2)),
            missing(TrackKind::Video, "avs2", true, false)
        );
        // 同一條再失敗一次（又選了這條軌道）：不再通知
        assert_eq!(p.handle(log("vd", "error", avs2)), None);
        // 這個檔案沒有 HEVC 的軌道：上一個檔案晚到的記錄，不算
        assert_eq!(
            p.handle(log("vd", "error", "Failed to initialize a decoder for codec 'hevc'.\n")),
            None
        );
        // 不是錯誤等級的不看
        assert_eq!(
            p.handle(log("ad", "warn", "Failed to initialize a decoder for codec 'aac'.\n")),
            None
        );
        // 引擎有 AAC 的解碼器：無法解碼（不是不支援）；來源不是 vd / ad 的查軌道清單
        assert_eq!(
            p.handle(log(
                "lavfi",
                "error",
                "Failed to initialize a decoder for codec 'aac'.\n"
            )),
            missing(TrackKind::Audio, "aac", false, false)
        );
        // 字幕一律是不支援。mpv 自動選的（使用者沒選過）：記下、不通知（每次開檔都提示的話很煩）
        let teletext = "Could not find subtitle decoder for format 'dvb_teletext'.\n";
        assert_eq!(p.handle(log("sub", "error", teletext)), None);
        assert!(
            p.state
                .missing_decoders
                .iter()
                .any(|m| m.codec == "dvb_teletext" && m.quiet),
            "{:?}",
            p.state.missing_decoders
        );
        // 使用者自己選了這條、又解不了：通知（只一次）
        p.select_track(TrackKind::Sub, Some(1)).unwrap();
        assert_eq!(
            p.handle(log("sub", "error", teletext)),
            missing(TrackKind::Sub, "dvb_teletext", true, false)
        );
        assert_eq!(p.handle(log("sub", "error", teletext)), None);
        // 專輯封面記下是封面
        assert_eq!(
            p.handle(log(
                "vd",
                "error",
                "Failed to initialize a decoder for codec 'mjpeg'.\n"
            )),
            missing(TrackKind::Video, "mjpeg", false, true)
        );
        assert_eq!(p.state.missing_decoders.len(), 4);
        assert!(p.undecodable_subs.contains("dvb_teletext"));
        // 影像、聲音都解不了，mpv 說「No video or audio streams」：說明是哪個編碼（引擎沒有的優先）
        p.handle(log("cplayer", "fatal", "No video or audio streams selected.\n"));
        p.handle(failed(libmpv2_sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY));
        assert_eq!(p.state.last_error.as_deref(), Some("不支援的編碼：AVS2"));
        // 開新檔：清掉；字幕格式解不了這件事留著（引擎沒有的解碼器換檔也一樣）
        p.handle(Event::StartFile);
        assert!(p.state.missing_decoders.is_empty());
        assert!(p.undecodable_subs.contains("dvb_teletext"));

        // 記錄比 EndFile 晚到：收到時再補進說明
        p.set_fake_tracks(Some(vec![track(r#"{"id":1,"type":"video","codec":"h264"}"#)]));
        p.handle(log("cplayer", "fatal", "No video or audio streams selected.\n"));
        p.handle(failed(libmpv2_sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY));
        assert!(
            p.state
                .last_error
                .as_deref()
                .is_some_and(|e| e.starts_with("檔案裡沒有可播放的影像或聲音")),
            "{:?}",
            p.state.last_error
        );
        assert_eq!(
            p.handle(log("vd", "error", "Failed to initialize a decoder for codec 'h264'.\n")),
            missing(TrackKind::Video, "h264", false, false)
        );
        assert_eq!(
            p.state.last_error.as_deref(),
            Some("無法解碼 H.264（檔案可能損壞，或用了引擎不支援的設定）")
        );

        // 整個檔案結束了才收到（mpv 讀不到軌道）：照樣算這個檔案的
        p.set_fake_tracks(None);
        p.handle(Event::StartFile);
        p.handle(failed(libmpv2_sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY));
        assert!(p.handle(log("vd", "error", avs2)).is_some());
        assert_eq!(p.state.last_error.as_deref(), Some("不支援的編碼：AVS2"));

        // 播了一陣子（載入完成過）才失敗：原因不是解不了的編碼
        p.set_fake_tracks(Some(vec![
            track(r#"{"id":1,"type":"video","codec":"avs2"}"#),
            track(r#"{"id":1,"type":"audio","codec":"aac","selected":true}"#),
        ]));
        p.handle(Event::StartFile);
        p.handle(log("vd", "error", avs2));
        p.handle(Event::FileLoaded);
        p.handle(log("stream", "error", "Failed to open C:\\影片\\a.mkv.\n"));
        p.handle(failed(libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED));
        let err = p.state.last_error.clone().unwrap_or_default();
        assert!(err.starts_with("無法載入檔案") && !err.contains("AVS2"), "{err}");
    }

    fn decoder_log(prefix: &str, codec: &str) -> crate::mpv::Event {
        crate::mpv::Event::Log {
            prefix: prefix.into(),
            level: "error".into(),
            text: format!("Failed to initialize a decoder for codec '{codec}'.\n"),
        }
    }

    fn ended_with(code: i32) -> crate::mpv::Event {
        crate::mpv::Event::EndFile {
            reason: crate::mpv::EndReason::Error,
            error: Some(crate::mpv::Error {
                code,
                context: String::new(),
            }),
        }
    }

    /// 換檔時上一個檔案晚到的記錄：下一個檔案還在載入、mpv 讀不到軌道時先留著，
    /// 下一個檔案載入完成就丟掉；它因為沒有東西可播而開不起來，才算它的
    #[test]
    fn decoder_log_while_the_next_file_loads_waits_for_its_outcome() {
        use crate::mpv::Event;
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        // mpv 還沒開分離器：讀不到軌道
        p.set_fake_tracks(Some(Vec::new()));
        p.handle(Event::StartFile);
        assert!(p.state.loading);
        assert_eq!(p.handle(decoder_log("vd", "h264")), None);
        assert!(p.state.missing_decoders.is_empty(), "不算載入中的這個檔案的");
        // 開始播了：是上一個檔案的（之後結束也不算）
        p.handle(Event::FileLoaded);
        p.handle(Event::PlaybackRestart);
        p.handle(ended_with(libmpv2_sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY));
        assert!(p.state.missing_decoders.is_empty(), "{:?}", p.state.missing_decoders);

        // mpv 0.37：影像解不了時載入完成之後才停下，記錄可能在載入完成之前、讀不到軌道時收到：是這個檔案的
        p.handle(Event::StartFile);
        assert_eq!(p.handle(decoder_log("vd", "h264")), None);
        p.handle(Event::FileLoaded);
        // 0.37 的錯誤碼不是「沒有東西可播」
        p.handle(ended_with(libmpv2_sys::mpv_error_MPV_ERROR_GENERIC));
        assert_eq!(p.state.missing_decoders.len(), 1, "{:?}", p.state.missing_decoders);
        // 留著的結束（沒有檔案可以重開）：事件取完時通知
        assert!(
            p.poll()
                .iter()
                .any(|e| matches!(e, super::PlayerEvent::EndFile { error: Some(_), .. }))
        );

        // 沒有東西可播、開不起來（mpv 已經關掉分離器才讀不到軌道）：是這個檔案的
        p.handle(Event::StartFile);
        assert_eq!(p.handle(decoder_log("vd", "h264")), None);
        p.handle(ended_with(libmpv2_sys::mpv_error_MPV_ERROR_NOTHING_TO_PLAY));
        assert_eq!(p.state.missing_decoders.len(), 1);
        assert_eq!(
            p.state.last_error.as_deref(),
            Some("無法解碼 H.264（檔案可能損壞，或用了引擎不支援的設定）")
        );

        // 其他原因開不起來（讀不到檔案）：不是解不了
        p.handle(Event::StartFile);
        assert_eq!(p.handle(decoder_log("vd", "h264")), None);
        p.handle(ended_with(libmpv2_sys::mpv_error_MPV_ERROR_LOADING_FAILED));
        assert!(p.state.missing_decoders.is_empty(), "{:?}", p.state.missing_decoders);
        let err = p.state.last_error.clone().unwrap_or_default();
        assert!(err.starts_with("無法載入檔案"), "{err}");
    }

    /// 載入完成、還沒開始播就失敗（留著的結束），接著 mpv 自己開始了下一個項目（mpv 的播放清單）：
    /// 先通知那個結束、再通知下一個開始（不能吞掉）
    #[test]
    fn held_end_is_reported_before_the_next_file_starts() {
        use crate::mpv::Event;
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        p.handle(Event::StartFile);
        p.handle(Event::FileLoaded);
        assert_eq!(p.handle(ended_with(-1)), None, "結束先留著");
        let ev = p.handle(Event::StartFile);
        assert!(
            matches!(ev, Some(super::PlayerEvent::EndFile { error: Some(_), .. })),
            "{ev:?}"
        );
        assert_eq!(p.poll().first(), Some(&super::PlayerEvent::StartFile));
        assert!(p.state.loading, "下一個檔案照常開始");
        assert_eq!(p.poll(), Vec::new(), "只通知一次");
    }

    /// 只播聲音重開（mpv 0.37）帶過去的記錄只給重開的那個檔案
    #[test]
    fn audio_only_carry_goes_only_to_the_reopened_file() {
        use super::MissingDecoder;
        use crate::mpv::Event;
        let mut p = super::Player::new(super::Options::headless()).unwrap();
        let carried = vec![MissingDecoder {
            kind: TrackKind::Video,
            codec: "h264".into(),
            unsupported: false,
            cover: false,
            quiet: false,
        }];
        let a = "C:/影片/a.mp4".to_owned();
        // mpv 開始的是別的檔案（重開之前又開了別的）：不帶過去
        p.audio_only_carry = Some((a.clone(), carried.clone()));
        p.opening = Some("C:/影片/b.mp4".into());
        p.handle(Event::StartFile);
        assert!(p.state.missing_decoders.is_empty());
        assert!(!p.reopened_audio_only());
        // 重開的那個：帶過去
        p.audio_only_carry = Some((a.clone(), carried.clone()));
        p.opening = Some(a.clone());
        p.handle(Event::StartFile);
        assert_eq!(p.state.missing_decoders, carried);
        assert!(p.reopened_audio_only());
        // 介面開了別的檔案：丟掉
        p.audio_only_carry = Some((a, carried));
        let _ = p.open("C:/不存在/c.mp4");
        assert!(p.audio_only_carry.is_none());
    }

    /// 開檔自動選字幕：FFmpeg 解不了的格式（Teletext、ARIB、MP4 裡的 TTML、藍光文字字幕）和這個播放器
    /// 之前解不了的都不選；mpv 自己選了解不了的當成沒選
    #[test]
    fn auto_subtitle_choice_skips_undecodable_formats() {
        use super::pick_auto_sub;
        use std::collections::HashSet;
        let t = |json: &str| serde_json::from_str::<Track>(json).unwrap();
        let none = HashSet::new();
        let srt = t(r#"{"id":9,"type":"sub","codec":"subrip","lang":"eng"}"#);
        let pick = |subs: &[&Track], learned: &HashSet<String>| pick_auto_sub(subs, learned).map(|t| t.id);
        // mpv 選了標成預設的 Teletext：當成沒選，改選解得了的
        let teletext = t(r#"{"id":1,"type":"sub","codec":"dvb_teletext","lang":"ger","default":true,"selected":true}"#);
        assert_eq!(pick(&[&teletext, &srt], &none), Some(9));
        // 只有 ARIB 字幕：不選
        let arib = t(r#"{"id":1,"type":"sub","codec":"arib_caption","lang":"jpn"}"#);
        assert_eq!(pick(&[&arib], &none), None);
        // 標成繁中、但解不了的：不選（不然繁中優先會選它）
        for codec in ["ttml", "hdmv_text_subtitle", "dvb_teletext", "arib_caption"] {
            let zh = t(&format!(r#"{{"id":1,"type":"sub","codec":"{codec}","lang":"zh-TW"}}"#));
            assert_eq!(pick(&[&zh, &srt], &none), Some(9), "{codec}");
        }
        // 這個播放器之前解不了的格式
        let ass = t(r#"{"id":1,"type":"sub","codec":"ass","lang":"zh-TW"}"#);
        assert_eq!(pick(&[&ass, &srt], &none), Some(1));
        let learned = HashSet::from(["ass".to_owned()]);
        assert_eq!(pick(&[&ass, &srt], &learned), Some(9));
    }

    /// 解不了的專輯封面不算封面：沒有畫面，ASS 歌詞由介面畫
    #[test]
    fn undecodable_cover_does_not_hide_ass_lyrics() {
        use super::MissingDecoder;
        let track = |json: &str| serde_json::from_str::<Track>(json).unwrap();
        let mut state = State {
            tracks: vec![
                track(r#"{"id":1,"type":"video","codec":"jpegxl","albumart":true,"selected":true}"#),
                track(r#"{"id":1,"type":"audio","codec":"flac","selected":true}"#),
                track(r#"{"id":1,"type":"sub","codec":"ass","selected":true}"#),
            ],
            ..Default::default()
        };
        assert!(!state.lyrics_in_app(), "有封面：ASS 歌詞由 mpv 畫在封面上");
        state.missing_decoders.push(MissingDecoder {
            kind: TrackKind::Video,
            codec: "jpegxl".into(),
            unsupported: true,
            cover: true,
            quiet: false,
        });
        assert!(state.lyrics_in_app());
        assert!(!state.has_picture());
    }
}
