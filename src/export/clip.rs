//! 片段：把 A-B 段落存成檔案，不重新編碼（mpv 的 `dump-cache`）。
//!
//! 在背景執行緒（[`run`]）另外開一個不出畫面、不出聲音的 mpv（匯出用的），不動正在播放的那一個：
//! 1. **讀進快取**：從 A 前面 5 秒開始讀到 B 後面（下一個關鍵影格要在快取裡），大的段落放磁碟快取。
//!    只選一條影像、一條聲音，**不選字幕**：目前的播放引擎寫不出字幕軌與語言標籤，片段只有影像與一條聲音。
//!    `hr-seek=no`、`vd-lavc-threads=1`：開頭只解一格（不必從關鍵影格解到 A），不搶正在播放的那一個的 CPU。
//! 2. **對齊**：`ab-loop-align-cache` 把 A、B 換成快取裡真正寫得出來的範圍（A 之前的關鍵影格、
//!    B 之後下一個關鍵影格之前的最後一格）。聲音跟影像一起結束，結尾不會有一段沒有聲音的影像。
//! 3. **寫檔**：`dump-cache` 寫到目的地資料夾的暫存檔。寫的時候不能中斷，進度看暫存檔的大小。
//! 4. **檢查**：重新打開，長度、軌道對了才換成正式的名稱（絕不覆蓋）。
//!
//! 網路影片（能跳轉、有總長度）也一樣：匯出用的 mpv 用同樣的連線設定（[`Player::net_stream`]，
//! 網站影片是 yt-dlp 解析出來的網址）再讀一次這一段。直播不能匯出片段。
//!
//! 時間：介面給的 A、B 是主播放器的播放時間（影片的時間戳 − 主播放器的 `demuxer-start-time`）。
//! 匯出用的 mpv 用 FFmpeg 的分離器（主播放器開 MKV 用 mpv 自己的），起始時間可能不同，要換算（[`to_export_time`]）。
//! 開頭的 `start` 選項在知道匯出用的起始時間之前就要給，用主播放器的時間：差一點點沒關係，前面多讀了 5 秒。

use super::{
    ClipFormat, Ctl, Done, Expect, Failure, Job, Kind, LogLine, LogTail, Note, Phase, Progress, check_space,
    map_mpv_error, normalize_codec, verify_media,
};
use crate::instance::Wake;
use crate::mpv::{self, Event, Mpv};
use crate::player::{EngineCaps, NetStream, Player, Track, TrackKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ───────────── 容器 ─────────────

/// 片段的容器格式（副檔名決定 mpv 用哪一個寫檔程式：`av_guess_format`）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mkv,
    /// 只有聲音的 Matroska
    Mka,
    Mp4,
    /// 只有聲音的 MP4（ipod 寫檔程式）
    M4a,
    Webm,
    Ts,
    Mp3,
    Flac,
    Opus,
    Ogg,
    Wav,
}

impl Container {
    pub const ALL: [Container; 11] = [
        Container::Mkv,
        Container::Mka,
        Container::Mp4,
        Container::M4a,
        Container::Webm,
        Container::Ts,
        Container::Mp3,
        Container::Flac,
        Container::Opus,
        Container::Ogg,
        Container::Wav,
    ];

    pub fn ext(self) -> &'static str {
        match self {
            Container::Mkv => "mkv",
            Container::Mka => "mka",
            Container::Mp4 => "mp4",
            Container::M4a => "m4a",
            Container::Webm => "webm",
            Container::Ts => "ts",
            Container::Mp3 => "mp3",
            Container::Flac => "flac",
            Container::Opus => "opus",
            Container::Ogg => "ogg",
            Container::Wav => "wav",
        }
    }

    /// 介面上的名稱（格式名稱不翻譯）
    pub fn label(self) -> &'static str {
        match self {
            Container::Mkv => "MKV",
            Container::Mka => "MKA",
            Container::Mp4 => "MP4",
            Container::M4a => "M4A",
            Container::Webm => "WebM",
            Container::Ts => "TS",
            Container::Mp3 => "MP3",
            Container::Flac => "FLAC",
            Container::Opus => "Opus",
            Container::Ogg => "Ogg",
            Container::Wav => "WAV",
        }
    }

    /// 能不能放影像
    pub fn takes_video(self) -> bool {
        matches!(self, Container::Mkv | Container::Mp4 | Container::Webm | Container::Ts)
    }

    /// 這個容器能不能放這種編碼的軌道（保守的表：拿不準的不放，選 MKV；寫好之後還會重新打開檢查）。
    /// 字幕一律不放（目前的播放引擎寫不出字幕軌的語言、開頭還會多出空白）
    pub fn holds(self, kind: TrackKind, codec: &str) -> bool {
        let c = normalize_codec(codec);
        match kind {
            TrackKind::Video => match self {
                Container::Mkv => true,
                Container::Mp4 => matches!(c, "h264" | "hevc" | "av1" | "vp9" | "mpeg4"),
                Container::Webm => matches!(c, "vp8" | "vp9" | "av1"),
                Container::Ts => matches!(c, "h264" | "hevc" | "mpeg2video" | "mpeg1video"),
                _ => false,
            },
            TrackKind::Audio => {
                if refused_audio(c) {
                    return false;
                }
                match self {
                    Container::Mkv | Container::Mka => true,
                    Container::Mp4 => matches!(c, "aac" | "mp3" | "ac3" | "eac3" | "opus" | "flac" | "alac"),
                    Container::M4a => matches!(c, "aac" | "alac"),
                    Container::Webm => matches!(c, "opus" | "vorbis"),
                    Container::Ts => matches!(c, "aac" | "mp3" | "mp2" | "ac3" | "eac3"),
                    Container::Mp3 => c == "mp3",
                    Container::Flac => c == "flac",
                    Container::Opus => c == "opus",
                    Container::Ogg => c == "vorbis",
                    Container::Wav => wav_pcm(c),
                }
            }
            TrackKind::Sub | TrackKind::Other => false,
        }
    }
}

/// 不重新編碼就存不成檔案的音訊（APE、DSD、Musepack 沒有能放的容器；藍光、DVD 的 LPCM 只能放回原本的格式）
pub fn refused_audio(codec: &str) -> bool {
    let c = normalize_codec(codec);
    matches!(
        c,
        "ape" | "musepack7" | "musepack8" | "mpc7" | "mpc8" | "pcm_bluray" | "pcm_dvd"
    ) || c.starts_with("dsd_")
}

/// WAV 放得下的 PCM（小端序、常見的位元數）；大端序的（MOV 來的）放 MKA
fn wav_pcm(codec: &str) -> bool {
    matches!(
        codec,
        "pcm_u8" | "pcm_s16le" | "pcm_s24le" | "pcm_s32le" | "pcm_f32le" | "pcm_f64le" | "pcm_alaw" | "pcm_mulaw"
    )
}

/// 只有聲音時，依編碼選的格式
pub fn audio_container(codec: &str) -> Result<Container, Failure> {
    let c = normalize_codec(codec);
    if refused_audio(c) {
        return Err(Failure::AudioCodec);
    }
    Ok(match c {
        "aac" | "alac" => Container::M4a,
        "mp3" => Container::Mp3,
        "flac" => Container::Flac,
        "opus" => Container::Opus,
        "vorbis" => Container::Ogg,
        c if wav_pcm(c) => Container::Wav,
        _ => Container::Mka,
    })
}

/// 路徑（或網址，去掉 `?…`、`#…`）的副檔名，小寫
fn path_ext(path: &str) -> String {
    let p = if path.contains("://") {
        path.split(['?', '#']).next().unwrap_or(path)
    } else {
        path
    };
    Path::new(p)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 選片段的格式。`file_format` 是主播放器的 `file-format`（`mov,mp4,m4a,3gp,3g2,mj2`、`mkv`、`mpegts`…），
/// `video`、`audio` 是要放的軌道的編碼（None = 不放）。
/// - 自動、有影像：放得下時照來源（MP4 → MP4、WebM → WebM、TS → TS），其他存 MKV。
/// - 自動、只有聲音：依音訊的編碼（[`audio_container`]）。
/// - 指定的格式放不下其中一條軌道：`Incompatible`（介面用來停用那個選項）。只有聲音時 MKV → MKA、MP4 → M4A。
/// - 存不成檔案的音訊（APE、DSD…）：`AudioCodec`
pub fn plan_container(
    choice: ClipFormat,
    file_format: &str,
    path: &str,
    video: Option<&str>,
    audio: Option<&str>,
) -> Result<Container, Failure> {
    let video = video.map(normalize_codec);
    let audio = audio.map(normalize_codec);
    if audio.is_some_and(refused_audio) {
        return Err(Failure::AudioCodec);
    }
    let fits = |c: Container| -> Result<Container, Failure> {
        for (kind, codec) in [(TrackKind::Video, video), (TrackKind::Audio, audio)] {
            if let Some(codec) = codec
                && !c.holds(kind, codec)
            {
                return Err(Failure::Incompatible {
                    container: c.label().to_owned(),
                    codec: codec.to_owned(),
                });
            }
        }
        Ok(c)
    };
    match (choice, video, audio) {
        (_, None, None) => Err(Failure::NoData),
        (ClipFormat::Auto, None, Some(a)) => audio_container(a),
        (ClipFormat::Auto, Some(_), _) => {
            let family = if file_format.starts_with("mov,mp4") {
                Some(Container::Mp4)
            } else if (file_format == "mkv" || file_format.starts_with("matroska")) && path_ext(path) == "webm" {
                Some(Container::Webm)
            } else if file_format == "mpegts" {
                Some(Container::Ts)
            } else {
                None
            };
            Ok(family.and_then(|c| fits(c).ok()).unwrap_or(Container::Mkv))
        }
        (ClipFormat::Mkv, None, _) => fits(Container::Mka),
        (ClipFormat::Mkv, Some(_), _) => fits(Container::Mkv),
        (ClipFormat::Mp4, None, _) => fits(Container::M4a),
        (ClipFormat::Mp4, Some(_), _) => fits(Container::Mp4),
        (ClipFormat::Webm, ..) => fits(Container::Webm),
        (ClipFormat::Ts, ..) => fits(Container::Ts),
    }
}

// ───────────── 軌道 ─────────────

/// 要放進片段的一條軌道（主播放器裡選的）。兩個分離器給的軌道編號不一定一樣，
/// 用「同一類的內嵌軌道裡排第幾」對應，再核對編碼、語言
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPick {
    pub kind: TrackKind,
    /// 同一類的內嵌軌道（不含外掛的）裡排第幾，從 1 開始；匯出用的 mpv 沒有外掛軌道，預期的編號就是它
    pub ordinal: usize,
    pub codec: Option<String>,
    pub lang: Option<String>,
    pub title: Option<String>,
}

impl StreamPick {
    /// `track` 在 `tracks`（主播放器的軌道清單）裡的位置；外掛的軌道（另外載入的音軌）放不進片段：None
    pub fn of(tracks: &[Track], track: &Track) -> Option<StreamPick> {
        if track.external {
            return None;
        }
        let ordinal = tracks
            .iter()
            .filter(|t| t.kind == track.kind && !t.external)
            .position(|t| t.id == track.id)?
            + 1;
        Some(StreamPick {
            kind: track.kind,
            ordinal,
            codec: track.codec.clone(),
            lang: track.lang.clone().filter(|l| !l.is_empty()),
            title: track.title.clone().filter(|t| !t.is_empty()),
        })
    }
}

/// 要放進片段的影像、聲音（None = 不放）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Picks {
    pub video: Option<StreamPick>,
    pub audio: Option<StreamPick>,
}

/// 這條軌道放得進片段：內嵌的、不是專輯封面、聲音不是存不成檔案的格式（APE、藍光的 LPCM…）
fn clip_ready(t: &Track) -> bool {
    !t.external && !t.albumart && !(t.kind == TrackKind::Audio && t.codec.as_deref().is_some_and(refused_audio))
}

/// 預設放進片段的軌道：主播放器現在選的（內嵌的）；選的是外掛的（另外載入的音軌）或放不進片段的格式時，
/// 改用第一條放得進的內嵌軌道。主播放器沒選（關掉聲音）的那一類不放
pub fn default_picks(state: &crate::player::State) -> Picks {
    let pick = |kind: TrackKind| {
        let selected = state.selected(kind)?;
        let track = if clip_ready(selected) {
            selected
        } else {
            state.tracks_of(kind).find(|t| clip_ready(t))?
        };
        StreamPick::of(&state.tracks, track)
    };
    Picks {
        video: pick(TrackKind::Video),
        audio: pick(TrackKind::Audio),
    }
}

/// 在匯出用的 mpv 的軌道清單裡找對應的軌道，回傳它的編號：
/// 先看排在同一個位置的（編碼一樣、兩邊都有語言時語言也一樣），不對的話找編碼、語言、標題都一樣的，再來是編碼一樣的
pub fn match_track(tracks: &[Track], pick: &StreamPick) -> Option<i64> {
    let same_codec = |t: &Track| match (&t.codec, &pick.codec) {
        (Some(a), Some(b)) => normalize_codec(a) == normalize_codec(b),
        (None, None) => true,
        _ => false,
    };
    let same_lang = |t: &Track| match (t.lang.as_deref().filter(|l| !l.is_empty()), pick.lang.as_deref()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => true,
    };
    let embedded: Vec<&Track> = tracks.iter().filter(|t| t.kind == pick.kind && !t.external).collect();
    if let Some(t) = embedded.get(pick.ordinal.wrapping_sub(1))
        && same_codec(t)
        && same_lang(t)
    {
        return Some(t.id);
    }
    let usable = |t: &&&Track| !t.albumart && same_codec(t);
    embedded
        .iter()
        .filter(usable)
        .find(|t| t.lang.as_deref().filter(|l| !l.is_empty()) == pick.lang.as_deref() && t.title == pick.title)
        .or_else(|| embedded.iter().find(usable))
        .map(|t| t.id)
}

// ───────────── 快取、時間、範圍 ─────────────

/// A 前面多讀幾秒：分離器跳轉可能落在晚一點的地方（TS、AVI），A 之前的關鍵影格才會在快取裡
pub const PREROLL: f64 = 5.0;
/// B 後面多讀幾秒（下一個關鍵影格要在快取裡，對齊時才知道影像在哪裡結束）
pub const TAIL: f64 = 20.0;
/// 讀到 B 後面這麼多秒就夠了（比 `TAIL` 少一點，快取讀到上限之前就停）
pub const KEYFRAME_MARGIN: f64 = 15.0;
/// 估計超過這麼大的段落放磁碟快取
pub const RAM_LIMIT: u64 = 512 * 1024 * 1024;
/// 放記憶體時至少給這麼大
pub const MIN_RAM: u64 = 64 * 1024 * 1024;

/// 匯出用的 mpv 怎麼讀快取
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CachePlan {
    /// 從哪裡開始讀（主播放器的時間）
    pub start: f64,
    /// 讀多少秒（`cache-secs`）
    pub cache_secs: f64,
    /// 記憶體快取的上限（`demuxer-max-bytes`）；磁碟快取時資料不算在裡面，只算每個封包的一點記錄
    pub max_bytes: u64,
    /// 放磁碟快取（`cache-on-disk`）
    pub on_disk: bool,
}

impl CachePlan {
    /// 分離器往前讀多少秒（`cache-secs`、`demuxer-readahead-secs`）：mpv 從解碼器拿到的最後一個封包算起，
    /// 開頭只解一格（`hr-seek=no`）時那是 `start` 之前的關鍵影格，可能早很多（關鍵影格隔 10 秒以上的影片）。
    /// 多給 [`KEYFRAME_ALLOWANCE`]，才讀得到 B 後面；讀到 B 後面就停，多給的不一定會讀
    pub fn readahead_secs(self) -> f64 {
        self.cache_secs + KEYFRAME_ALLOWANCE
    }

    /// 改放磁碟快取（記憶體不夠時重來一次）
    pub fn to_disk(self) -> CachePlan {
        CachePlan {
            on_disk: true,
            max_bytes: RAM_LIMIT,
            ..self
        }
    }

    /// 從更前面開始讀（A 之前的關鍵影格不在讀到的範圍裡時重來一次：關鍵影格隔很久的 TS、跳轉不準的格式）
    pub fn earlier(self) -> CachePlan {
        let start = (self.start - LONG_PREROLL).max(0.0);
        CachePlan {
            start,
            cache_secs: self.cache_secs + (self.start - start),
            ..self
        }
    }
}

/// 重來一次時 A 前面多讀的秒數
pub const LONG_PREROLL: f64 = 30.0;
/// 往前讀的秒數多給這麼多（`start` 之前的關鍵影格可能在這麼前面，見 [`CachePlan::readahead_secs`]）
pub const KEYFRAME_ALLOWANCE: f64 = 60.0;

/// 讀到的範圍（`demuxer-cache-state` 的 `seekable-ranges`）有沒有從 A 之前（或 A 附近）開始：
/// 沒有的話 A 之前的關鍵影格不在快取裡，片段會從 A 之後才開始（`slack` = 容許晚一點，檔案開頭的第一格常常不在 0）
pub fn starts_by(ranges: &[(f64, f64)], a: f64, slack: f64) -> bool {
    ranges.iter().any(|&(s, e)| s <= a + slack && e > a)
}

/// A 不在讀到的範圍裡時，片段真正的起點：A 之後第一個讀到的範圍的開頭（那裡是關鍵影格）。
/// 這個範圍在 B 之後才開始（A-B 整段都沒讀到）時 None
pub fn first_cached_after(ranges: &[(f64, f64)], a: f64, b: f64) -> Option<f64> {
    ranges
        .iter()
        .filter(|&&(_, e)| e > a)
        .map(|&(s, _)| s.max(a))
        .min_by(f64::total_cmp)
        .filter(|&s| s < b)
}

/// 讀快取的計畫：`bytes_per_sec` = 檔案大小 ÷ 長度（不知道時 None：放磁碟快取）
pub fn cache_plan(bytes_per_sec: Option<f64>, a: f64, b: f64) -> CachePlan {
    let start = (a - PREROLL).max(0.0);
    let cache_secs = (b - a).max(0.0) + PREROLL + TAIL;
    let est = bytes_per_sec
        .filter(|r| r.is_finite() && *r > 0.0)
        .map(|r| r * cache_secs * 1.3);
    match est {
        Some(est) if est <= RAM_LIMIT as f64 => CachePlan {
            start,
            cache_secs,
            max_bytes: (est as u64).max(MIN_RAM),
            on_disk: false,
        },
        _ => CachePlan {
            start,
            cache_secs,
            max_bytes: RAM_LIMIT,
            on_disk: true,
        },
    }
}

/// 主播放器的時間 → 匯出用的 mpv 的時間（兩邊的 `demuxer-start-time` 可能不同）
pub fn to_export_time(t: f64, main_start: f64, export_start: f64) -> f64 {
    t + main_start - export_start
}

/// 匯出用的 mpv 的時間 → 主播放器的時間
pub fn to_main_time(t: f64, main_start: f64, export_start: f64) -> f64 {
    t - main_start + export_start
}

/// 兩邊的總長度差超過這麼多秒，就是時間對不上（章節連結之類）
pub const TIMELINE_TOLERANCE: f64 = 1.0;

/// 時間跟檔案對不上的來源：章節連結（ordered chapters，`file-format` 是 `mkv_oc/…`）、使用者開的 EDL、CUE。
/// 主播放器把好幾段（或好幾個檔案）接成一條時間線，匯出用的 mpv 讀的是檔案本身，A-B 會對到別的地方。
/// 網站影片（`site`）的 EDL 是 yt-dlp 的影像、聲音合起來，匯出用的 mpv 開的是同一個 EDL：不算
pub fn timeline_source(path: &str, file_format: &str, site: bool) -> bool {
    if site {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    // mpv 的時間線分離器：「<種類>/<格式>」（mkv_oc/mkv、edl/…、cue/…）；FFmpeg 的格式名稱沒有斜線
    lower.starts_with("edl://") || matches!(path_ext(path).as_str(), "edl" | "cue") || file_format.contains('/')
}

/// 匯出用的 mpv 開起來之後再確認一次：兩邊的總長度要差不多（章節連結之類偵測不到的，長度也會不一樣）。
/// 網路影片在匯出用的 mpv 裡沒有總長度：直播
pub fn check_timeline(main: Option<f64>, export: Option<f64>, network: bool) -> Result<(), Failure> {
    match (main, export) {
        (Some(m), Some(e)) if (m - e).abs() > TIMELINE_TOLERANCE => Err(Failure::Timeline),
        (_, None) if network => Err(Failure::Live),
        _ => Ok(()),
    }
}

/// 讀快取很久沒有進展時的原因（見 `Instance::stall_failure`）：記錄裡有磁碟快取的錯誤就是 `CacheFailed`，不然是逾時
pub fn stall_failure(log: &[LogLine]) -> Failure {
    match map_mpv_error(log) {
        Some(Failure::CacheFailed) => Failure::CacheFailed,
        _ => Failure::ReadTimeout,
    }
}

/// `dump-cache` 回覆之後判斷結果：回覆失敗時看記錄找原因；回覆成功但記錄裡有寫到一半失敗
/// （「Failed writing packet」：磁碟滿了、隨身碟拔掉；mpv 照樣回報成功），也是失敗
pub fn dump_result(reply: mpv::Result<()>, log: &[LogLine]) -> Result<(), Failure> {
    let mapped = map_mpv_error(log);
    match reply {
        Err(e) => Err(mapped.unwrap_or_else(|| Failure::Engine(e.description()))),
        Ok(()) => match mapped {
            Some(f @ (Failure::WriteFailed | Failure::CacheFailed)) => Err(f),
            _ => Ok(()),
        },
    }
}

// ───────────── 要匯出的片段 ─────────────

/// 片段的來源
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// 本機檔案（或 mpv 自己開得起來的其他路徑）
    File(PathBuf),
    /// 網路影片：要開的網址與連線的選項（[`Player::net_stream`]）
    Net(NetStream),
}

impl Source {
    /// 匯出用的 mpv 要開的
    pub fn target(&self) -> String {
        match self {
            Source::File(p) => p.to_string_lossy().into_owned(),
            Source::Net(s) => s.open.clone(),
        }
    }

    fn is_network(&self) -> bool {
        matches!(self, Source::Net(_))
    }

    /// 網站影片（影像、聲音分開）是 EDL：分離器要讓 mpv 自己選（不能指定 FFmpeg 的）
    fn is_edl(&self) -> bool {
        self.target().to_ascii_lowercase().starts_with("edl://")
    }
}

/// 測試用：匯出用的 mpv 開好時呼叫
#[doc(hidden)]
pub type OnReady = Arc<dyn Fn(&Mpv) + Send + Sync>;

/// 測試用：`dump-cache` 送出之後（寫檔中）呼叫
#[doc(hidden)]
pub type OnDump = Arc<dyn Fn() + Send + Sync>;

/// 測試用：寫好之後、檢查之前呼叫（參數是暫存檔）
#[doc(hidden)]
pub type OnWritten = Arc<dyn Fn(&Path) + Send + Sync>;

/// 測試用：觀察、限制匯出用的 mpv
#[derive(Clone, Default)]
#[doc(hidden)]
pub struct TestHooks {
    /// 第一次讀快取時的記憶體上限（測試「記憶體不夠時改放磁碟」）
    pub max_bytes: Option<u64>,
    /// 每個匯出用的 mpv 開好、停在開頭時呼叫一次（讀它的選項、位置）
    pub on_ready: Option<OnReady>,
    /// 第一個匯出用的 mpv 用這兩個軌道編號（影像、聲音），不用預期的（測試「軌道對不上時重開」）
    pub initial_ids: Option<(Option<i64>, Option<i64>)>,
    /// 讀到的範圍沒有 A 時不從更前面重讀（測試最後的退路：從讀得到的第一個關鍵影格開始）
    pub no_earlier: bool,
    /// 寫檔中（`dump-cache` 送出之後）呼叫一次
    pub on_dump: Option<OnDump>,
    /// 往前讀的秒數（測試「分離器在 B 之後自己停下來」）
    pub readahead: Option<f64>,
    /// 寫好之後、檢查之前呼叫一次（測試「寫出來的長度不夠」：換成比較短的檔案）
    pub after_dump: Option<OnWritten>,
}

impl std::fmt::Debug for TestHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestHooks")
            .field("max_bytes", &self.max_bytes)
            .field("on_ready", &self.on_ready.is_some())
            .field("initial_ids", &self.initial_ids)
            .field("no_earlier", &self.no_earlier)
            .field("on_dump", &self.on_dump.is_some())
            .field("readahead", &self.readahead)
            .field("after_dump", &self.after_dump.is_some())
            .finish()
    }
}

/// 一個片段要的全部資料（介面執行緒從主播放器取得，背景執行緒只用這些）
#[derive(Debug, Clone)]
pub struct ClipSpec {
    pub source: Source,
    /// 範圍（主播放器的時間，秒）
    pub a: f64,
    pub b: f64,
    /// 主播放器的 `demuxer-start-time`
    pub main_start: f64,
    /// 主播放器的總長度（確認兩邊的時間對得上）
    pub main_duration: Option<f64>,
    /// 要放的影像、聲音（None = 不放）
    pub video: Option<StreamPick>,
    pub audio: Option<StreamPick>,
    pub container: Container,
    /// 存放的資料夾、檔名（不含副檔名）
    pub dir: PathBuf,
    pub stem: String,
    /// 磁碟快取放這裡（`<快取>/export`）；換不成正式名稱留下的檔案也登記在這裡
    pub cache_dir: PathBuf,
    /// 檔案大小 ÷ 長度（估計要多少空間、放記憶體還是磁碟）；不知道時 None
    pub bytes_per_sec: Option<f64>,
    /// 播放引擎有 `ab-loop-align-cache`（沒有時不對齊，結尾可能有一段沒有聲音）
    pub align: bool,
    /// 主播放器顯示著字幕、或聲音有語言標籤：片段不會有（完成時附上說明）
    pub drops_subtitles: bool,
    /// 主播放器有聲音，但預設的軌道裡沒有放得進片段的（外掛的音軌、APE 之類）：片段沒有聲音（完成時附上說明）
    pub drops_audio: bool,
    #[doc(hidden)]
    pub test: TestHooks,
}

/// 主播放器目前的檔案能不能存片段（不管範圍、格式、選了哪些軌道）；不能時回傳原因（右鍵選單停用時的說明）。
/// 只要檔案裡有一條放得進片段的內嵌軌道就能存：主播放器關掉聲音（aid=no）時，匯出視窗照樣可以選音軌。
/// 有影像的檔案即使音軌的格式存不成檔案（藍光的 LPCM…）也能存：改用別的音軌，或只放影像
pub fn unavailable(player: &Player, caps: &EngineCaps) -> Option<Failure> {
    inspect(player, caps)
        .and_then(|found| usable_picks(&player.state, any_picks(&player.state)).map(|_| found))
        .err()
}

/// 放得進片段的軌道（每一類的第一條，不管主播放器選了沒有）：判斷這個檔案能不能存片段用
fn any_picks(state: &crate::player::State) -> Picks {
    let pick = |kind: TrackKind| {
        let track = state.tracks_of(kind).find(|t| clip_ready(t))?;
        StreamPick::of(&state.tracks, track)
    };
    Picks {
        video: pick(TrackKind::Video),
        audio: pick(TrackKind::Audio),
    }
}

/// 從主播放器看到的：要開什麼
struct Inspected {
    source: Source,
    /// 來源的路徑（本機檔案是檔案的路徑，`file://` 已經換成路徑）或網址
    path: String,
    file_format: String,
}

/// `file://` 網址 → 本機的路徑；其他的照原樣
fn local_path(text: &str) -> PathBuf {
    match text.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("file://") => crate::m3u::file_url_to_path(&text[7..]),
        _ => PathBuf::from(text),
    }
}

fn inspect(player: &Player, caps: &EngineCaps) -> Result<Inspected, Failure> {
    if !caps.dump_cache {
        return Err(Failure::NoDump);
    }
    let st = &player.state;
    let path = st.path.clone().filter(|_| st.loaded).ok_or(Failure::NoData)?;
    let file_format = player.get_string("file-format").unwrap_or_default();
    let (source, path, site) = if crate::net::is_network(&path) {
        if crate::net::is_live(&path, st.duration, st.seekable, Some(&file_format)) {
            return Err(Failure::Live);
        }
        if !st.seekable {
            return Err(Failure::NotSeekable);
        }
        let stream = player.net_stream().ok_or(Failure::SourceUnreachable)?;
        let site = stream.site;
        (Source::Net(stream), path, site)
    } else {
        // 命令列給的 file:// 網址：換成路徑（開檔前檢查檔案在不在、檔名）
        let local = local_path(&path);
        let path = local.to_string_lossy().into_owned();
        (Source::File(local), path, false)
    };
    if timeline_source(&path, &file_format, site) {
        return Err(Failure::Timeline);
    }
    Ok(Inspected {
        source,
        path,
        file_format,
    })
}

/// 要放的軌道至少有一條；都沒有時的原因：只有存不成檔案的音訊（APE、DSD…）是 `AudioCodec`，其他是沒有資料
fn usable_picks(state: &crate::player::State, picks: Picks) -> Result<Picks, Failure> {
    if picks.video.is_some() || picks.audio.is_some() {
        return Ok(picks);
    }
    let refused = state
        .tracks_of(TrackKind::Audio)
        .any(|t| !t.external && t.codec.as_deref().is_some_and(refused_audio));
    Err(if refused { Failure::AudioCodec } else { Failure::NoData })
}

impl ClipSpec {
    /// 從主播放器準備一個片段：範圍 `a`–`b`（主播放器的時間）、格式、存放的資料夾、磁碟快取的資料夾。
    /// 放預設的軌道（[`default_picks`]）。不能存（直播、章節連結、APE…）或指定的格式放不下時回傳原因
    pub fn from_player(
        player: &Player,
        caps: &EngineCaps,
        a: f64,
        b: f64,
        format: ClipFormat,
        dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<ClipSpec, Failure> {
        let st = &player.state;
        let picks = default_picks(st);
        // 主播放器有聲音、預設的軌道裡卻沒有（外掛的音軌、存不成檔案的格式）：完成時說明片段沒有聲音
        let drops_audio = picks.audio.is_none() && st.selected(TrackKind::Audio).is_some();
        let mut spec = Self::build(player, caps, a, b, format, picks, dir, cache_dir)?;
        spec.drops_audio = drops_audio;
        Ok(spec)
    }

    /// 同 [`from_player`](Self::from_player)，但放介面選的軌道（[`StreamPick::of`]；None = 不放那一類）。
    /// 格式、存不成檔案的音訊都依這些軌道判斷：指定的格式放不下其中一條時回傳 `Incompatible`
    #[allow(clippy::too_many_arguments)]
    pub fn from_player_with(
        player: &Player,
        caps: &EngineCaps,
        a: f64,
        b: f64,
        format: ClipFormat,
        picks: Picks,
        dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<ClipSpec, Failure> {
        Self::build(player, caps, a, b, format, picks, dir, cache_dir)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        player: &Player,
        caps: &EngineCaps,
        a: f64,
        b: f64,
        format: ClipFormat,
        picks: Picks,
        dir: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<ClipSpec, Failure> {
        let found = inspect(player, caps)?;
        let st = &player.state;
        let picks = usable_picks(st, picks)?;
        let codec = |p: &Option<StreamPick>| p.as_ref().and_then(|p| p.codec.clone());
        let (vcodec, acodec) = (codec(&picks.video), codec(&picks.audio));
        let container = plan_container(
            format,
            &found.file_format,
            &found.path,
            vcodec.as_deref(),
            acodec.as_deref(),
        )?;
        let duration = st.duration.filter(|d| d.is_finite() && *d > 0.0);
        let bytes_per_sec = player
            .get_i64("file-size")
            .ok()
            .filter(|s| *s > 0)
            .zip(duration)
            .map(|(size, d)| size as f64 / d);
        let title = st.title.as_deref();
        let drops_subtitles = st.sid.is_some() || picks.audio.as_ref().is_some_and(|a| a.lang.is_some());
        Ok(ClipSpec {
            stem: super::range_stem(&found.path, title, a, b),
            source: found.source,
            a,
            b,
            main_start: player.demuxer_start_time(),
            main_duration: duration,
            video: picks.video,
            audio: picks.audio,
            container,
            dir,
            cache_dir,
            bytes_per_sec,
            align: caps.align_cache,
            drops_subtitles,
            drops_audio: false,
            test: TestHooks::default(),
        })
    }

    /// 存好的檔案預定的名稱
    pub fn wanted(&self) -> PathBuf {
        self.dir.join(format!("{}.{}", self.stem, self.container.ext()))
    }
}

// ───────────── 背景工作 ─────────────

/// 開始匯出這個片段（背景執行緒）
pub fn spawn(spec: ClipSpec, wake: Wake) -> Job {
    Job::spawn(Kind::Clip, wake, move |ctl| run(ctl, spec))
}

/// 匯出用的 mpv 開檔最多等多久（網路很慢、CI 很慢）
const LOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// 這麼久讀不到新的資料就放棄
const READ_STALL: Duration = Duration::from_secs(20);
/// 等事件、看進度的間隔（秒）
const POLL: f64 = 0.1;
/// `dump-cache` 的非同步指令編號（匯出用的 mpv 只有這一個非同步指令）
const DUMP_ID: u64 = 1;
/// 估計片段大小時，關鍵影格對齊多出來的秒數
const ALIGN_SLACK: f64 = 4.0;
/// 目的地另外要留的空間
const SPARE: u64 = 16 * 1024 * 1024;
/// 讀到的範圍從 A 之後這麼近的地方開始也算有讀到 A（檔案開頭的第一格常常不在 0）
const START_SLACK: f64 = 0.5;
/// 分離器停下來這麼久就當成不會再讀（已經讀到 B 時不必等 `READ_STALL`）
const IDLE_SETTLE: Duration = Duration::from_secs(1);

/// 片段的工作本體（在 `Job` 的背景執行緒裡）
pub fn run(ctl: &Ctl, spec: ClipSpec) -> Result<Done, Failure> {
    // 換不成正式名稱時留下的完整檔案登記在快取資料夾：啟動時清暫存檔不會刪掉它
    ctl.set_keep_dir(Some(spec.cache_dir.clone()));
    ctl.check()?;
    if !(spec.a.is_finite() && spec.b.is_finite() && spec.b > spec.a) || (spec.video.is_none() && spec.audio.is_none())
    {
        return Err(Failure::NoData);
    }
    if let Source::File(path) = &spec.source {
        check_local(path)?;
    }
    ctl.progress(Progress {
        phase: Phase::Reading,
        fraction: Some(0.0),
    });
    std::fs::create_dir_all(&spec.dir).map_err(|_| Failure::NoPermission(Some(spec.dir.clone())))?;
    let mut plan = cache_plan(spec.bytes_per_sec, spec.a, spec.b);
    if let Some(max) = spec.test.max_bytes {
        plan.max_bytes = max;
        plan.on_disk = false;
    }
    let estimate = spec
        .bytes_per_sec
        .map(|r| (r * (spec.b - spec.a + ALIGN_SLACK)).max(0.0) as u64);
    check_room(&spec, &plan, estimate)?;

    // 預期的軌道編號：同一類的內嵌軌道裡排第幾（匯出用的 mpv 沒有外掛軌道）
    let predicted = |p: &Option<StreamPick>| p.as_ref().map(|p| p.ordinal as i64);
    let mut ids = spec
        .test
        .initial_ids
        .unwrap_or((predicted(&spec.video), predicted(&spec.audio)));
    let mut retried_ids = false;
    let mut retried_start = false;
    let (mut inst, export_start, a_e, b_e) = loop {
        let mut inst = Instance::open(ctl, &spec, &plan, ids)?;
        // 軌道對不上（兩個分離器排的順序不同）：用對的編號重開一次，不在讀快取時換（換軌道會重新跳轉）
        let tracks = inst.tracks();
        let want = (
            resolve(spec.video.as_ref(), &tracks)?,
            resolve(spec.audio.as_ref(), &tracks)?,
        );
        if want != ids {
            if retried_ids {
                return Err(Failure::TrackNotFound);
            }
            ids = want;
            retried_ids = true;
            continue;
        }
        let duration = inst.duration();
        check_timeline(spec.main_duration, duration, spec.source.is_network())?;
        let export_start = inst.start_time();
        let a_e = to_export_time(spec.a, spec.main_start, export_start);
        let b_e = to_export_time(spec.b, spec.main_start, export_start);
        if duration.is_some_and(|d| a_e >= d - 0.05) {
            return Err(Failure::NoData);
        }
        match inst.read(ctl, &spec, &plan, a_e, b_e)? {
            ReadEnd::Ready => {
                let ranges = inst.cache_state().map(|s| s.ranges).unwrap_or_default();
                if starts_by(&ranges, a_e, START_SLACK) || (plan.start <= 0.0 && !ranges.is_empty()) {
                    break (inst, export_start, a_e, b_e);
                }
                // A 之前的關鍵影格不在讀到的範圍裡（跳轉落在後面、關鍵影格隔很久）：從更前面開始讀，重來一次
                if plan.start > 0.0 && !retried_start && !spec.test.no_earlier {
                    drop(inst);
                    plan = plan.earlier();
                    retried_start = true;
                    continue;
                }
                // 還是不行：從讀得到的第一個關鍵影格開始（A 不在快取裡，mpv 對齊不了 A；
                // 起點改成讀到的範圍的開頭，長度的檢查、完成時的實際範圍才對）
                let Some(first) = first_cached_after(&ranges, a_e, b_e) else {
                    return Err(Failure::NoData);
                };
                break (inst, export_start, first, b_e);
            }
            // 記憶體快取的上限不夠讀到 B：改放磁碟快取重來一次
            ReadEnd::RamFull if !plan.on_disk => {
                drop(inst);
                plan = plan.to_disk();
                check_room(&spec, &plan, estimate)?;
            }
            ReadEnd::RamFull => break (inst, export_start, a_e, b_e),
        }
    };

    let (a1, b1) = inst.align(spec.align, a_e, b_e);
    let write_estimate = inst.estimate_bytes(a1, b1).or(estimate);
    // 寫檔開始之後就不能中斷：最後再看一次有沒有取消
    ctl.check()?;
    let temp = ctl.temp_in(&spec.dir, &spec.stem, spec.container.ext());
    ctl.set_writing(true);
    let written = inst.dump(ctl, a1, b1, &temp, write_estimate, spec.test.on_dump.as_ref());
    ctl.set_writing(false);
    // 放掉快取（磁碟快取的檔案由 mpv 刪掉）
    drop(inst);
    written.map_err(|f| match f {
        Failure::NoPermission(None) => Failure::NoPermission(Some(spec.dir.clone())),
        other => other,
    })?;
    ctl.check()?;
    if let Some(hook) = &spec.test.after_dump {
        hook(&temp);
    }

    ctl.progress(Progress {
        phase: Phase::Checking,
        fraction: None,
    });
    let codec = |p: &Option<StreamPick>| p.as_ref().and_then(|p| p.codec.clone());
    let expect = Expect {
        video: codec(&spec.video),
        audio: codec(&spec.audio),
        sub: None,
        // 寫到一半磁碟滿了的檔案也打得開：長度要夠
        min_len: (b1 - a1 - 1.0).max(0.0),
    };
    let verified = verify_media(&temp, &expect)?;
    let path = ctl.finish(&spec.wanted())?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    // 實際的範圍（主播放器的時間）：開頭是對齊後的關鍵影格，結尾 = 開頭 + 寫出來的長度（最後一格的時間 + 一格）
    let start = to_main_time(a1, spec.main_start, export_start).max(0.0);
    let mut end = start + verified.duration;
    if let Some(d) = spec.main_duration {
        end = end.min(d);
    }
    let mut notes = Vec::new();
    if spec.video.is_some() {
        notes.push(Note::KeyframeAligned);
    }
    if spec.drops_subtitles {
        notes.push(Note::NoSubtitleTrack);
    }
    if spec.drops_audio {
        notes.push(Note::NoAudioTrack);
    }
    Ok(Done {
        kind: Kind::Clip,
        path,
        bytes,
        actual: Some((start, end)),
        notes,
    })
}

/// 本機檔案還在、讀得到（網路磁碟可能很慢：在背景執行緒）
fn check_local(path: &Path) -> Result<(), Failure> {
    // mpv 自己開的其他路徑（av://、bd://…）不檢查；file:// 網址換成路徑
    let text = path.to_string_lossy();
    if text.contains("://") && !text.to_ascii_lowercase().starts_with("file://") {
        return Ok(());
    }
    match std::fs::File::open(local_path(&text)) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Failure::SourceMissing),
        Err(_) => Err(Failure::SourceNoAccess),
    }
}

/// 開始前檢查空間：目的地要放片段，磁碟快取的資料夾要放讀進來的段落（同一個磁碟時加起來）
fn check_room(spec: &ClipSpec, plan: &CachePlan, estimate: Option<u64>) -> Result<(), Failure> {
    let dest = estimate.map_or(SPARE, |e| (e as f64 * 1.1) as u64 + SPARE);
    let mut needs = vec![(spec.dir.as_path(), dest)];
    if plan.on_disk {
        let cache = spec.bytes_per_sec.map_or(0, |r| (r * plan.cache_secs * 1.3) as u64);
        needs.push((spec.cache_dir.as_path(), cache));
    }
    check_space(&needs)
}

/// 要放的軌道在匯出用的 mpv 裡的編號
fn resolve(pick: Option<&StreamPick>, tracks: &[Track]) -> Result<Option<i64>, Failure> {
    match pick {
        None => Ok(None),
        Some(p) => match_track(tracks, p).map(Some).ok_or(Failure::TrackNotFound),
    }
}

/// 讀快取的結果
enum ReadEnd {
    /// 夠了（讀到 B 後面、或檔尾）
    Ready,
    /// 記憶體快取滿了，還沒讀到 B
    RamFull,
}

/// `demuxer-cache-state` 裡用到的
#[derive(Debug, Default)]
struct CacheState {
    end: Option<f64>,
    eof: bool,
    idle: bool,
    fw_bytes: u64,
    total_bytes: u64,
    file_cache_bytes: Option<u64>,
    ranges: Vec<(f64, f64)>,
}

impl CacheState {
    fn parse(json: &str) -> Option<CacheState> {
        let v: serde_json::Value = serde_json::from_str(json).ok()?;
        let num = |k: &str| v[k].as_u64().or_else(|| v[k].as_i64().map(|n| n.max(0) as u64));
        Some(CacheState {
            end: v["cache-end"].as_f64(),
            eof: v["eof"].as_bool().unwrap_or(false),
            idle: v["idle"].as_bool().unwrap_or(false),
            fw_bytes: num("fw-bytes").unwrap_or(0),
            total_bytes: num("total-bytes").unwrap_or(0),
            file_cache_bytes: num("file-cache-bytes"),
            ranges: v["seekable-ranges"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|r| Some((r["start"].as_f64()?, r["end"].as_f64()?)))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// 匯出用的 mpv
struct Instance {
    mpv: Mpv,
    log: LogTail,
}

impl Instance {
    /// 開一個匯出用的 mpv，開 `spec.source`，等到檔案載入（`ids` = 要選的影像、聲音編號）
    fn open(
        ctl: &Ctl,
        spec: &ClipSpec,
        plan: &CachePlan,
        ids: (Option<i64>, Option<i64>),
    ) -> Result<Instance, Failure> {
        let id = |x: Option<i64>| x.map_or_else(|| "no".to_owned(), |i| i.to_string());
        let secs = format!("{:.3}", spec.test.readahead.unwrap_or(plan.readahead_secs()));
        let mut opts: Vec<(&str, String)> = vec![
            ("vo", "null".into()),
            ("ao", "null".into()),
            ("idle", "yes".into()),
            ("pause", "yes".into()),
            ("hwdec", "no".into()),
            // 開頭只解一格：跳到 start 不用從關鍵影格一路解碼過去；寫檔根本不需要解碼
            ("hr-seek", "no".into()),
            ("vd-lavc-threads", "1".into()),
            ("vid", id(ids.0)),
            ("aid", id(ids.1)),
            // 片段不放字幕（目前的播放引擎寫不出字幕軌的語言，開頭還會多出空白）
            ("sid", "no".into()),
            ("cover-art-auto", "no".into()),
            ("cache", "yes".into()),
            ("cache-secs", secs.clone()),
            ("demuxer-readahead-secs", secs),
            ("demuxer-max-bytes", plan.max_bytes.to_string()),
            ("cache-on-disk", if plan.on_disk { "yes" } else { "no" }.into()),
        ];
        // 從頭讀時不給 start：「start=0」也是一次跳轉，TS 之類用時間跳轉可能落在第一個關鍵影格之後，影像就整段讀不到
        if plan.start > 0.0 {
            opts.push(("start", format!("{:.3}", plan.start)));
        }
        if plan.on_disk {
            std::fs::create_dir_all(&spec.cache_dir).map_err(|_| Failure::CacheFailed)?;
            opts.push(("demuxer-cache-dir", spec.cache_dir.to_string_lossy().into_owned()));
        }
        // FFmpeg 的分離器才有 DTS（寫 MP4、TS 要），而且會先探測編碼參數；網站影片的 EDL 要讓 mpv 自己選
        if !spec.source.is_edl() {
            opts.push(("demuxer", "lavf".into()));
            opts.push(("demuxer-lavf-probe-info", "yes".into()));
        }
        let mut refs: Vec<(&str, &str)> = opts.iter().map(|(k, v)| (*k, v.as_str())).collect();
        refs.extend_from_slice(super::INSTANCE_OPTIONS);
        let mpv = Mpv::new(&refs).map_err(|e| Failure::Engine(e.description()))?;
        let _ = mpv.request_log_messages("warn");
        if let Source::Net(stream) = &spec.source {
            // 跟主播放器一樣的連線方式（User-Agent、標頭、proxy、憑證、逾時；網站影片的 Cookie）
            for (name, value) in &stream.options {
                if let Err(e) = mpv.set_node(name, value) {
                    eprintln!("[vitascope] 匯出：無法設定 {name}：{e}");
                }
            }
        }
        mpv.command(&["loadfile", &spec.source.target()])
            .map_err(|e| Failure::Engine(e.description()))?;
        let mut inst = Instance {
            mpv,
            log: LogTail::default(),
        };
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            ctl.check()?;
            if Instant::now() >= deadline {
                return Err(if spec.source.is_network() {
                    Failure::SourceUnreachable
                } else {
                    Failure::ReadTimeout
                });
            }
            match inst.next(POLL) {
                Some(Event::FileLoaded) => return Ok(inst),
                Some(Event::EndFile { .. } | Event::Shutdown) => return Err(inst.load_failure(&spec.source)),
                _ => {}
            }
        }
    }

    /// 下一個事件（記錄另外收起來）
    fn next(&mut self, timeout: f64) -> Option<Event> {
        let ev = self.mpv.wait_event(timeout)?;
        self.log.push_event(&ev);
        Some(ev)
    }

    /// 收完排著的事件（記錄要等其他事件都取完才送來）
    fn drain(&mut self) {
        while self.next(0.0).is_some() {}
    }

    /// 開檔失敗的原因
    fn load_failure(&mut self, source: &Source) -> Failure {
        self.drain();
        let mapped = self.log.failure();
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
                    Ok(()) => Failure::SourceUnreadable,
                },
            },
        }
    }

    fn tracks(&self) -> Vec<Track> {
        self.mpv
            .get_string("track-list")
            .ok()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    fn duration(&self) -> Option<f64> {
        self.mpv
            .get_property::<f64>("duration")
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0)
    }

    fn start_time(&self) -> f64 {
        self.mpv
            .get_property::<f64>("demuxer-start-time")
            .ok()
            .filter(|t| t.is_finite())
            .unwrap_or(0.0)
    }

    fn cache_state(&self) -> Option<CacheState> {
        CacheState::parse(&self.mpv.get_string("demuxer-cache-state").ok()?)
    }

    /// 讀進快取，直到 B 後面（下一個關鍵影格）或檔尾。記憶體快取滿了還沒讀到 B 時回傳 `RamFull`
    fn read(&mut self, ctl: &Ctl, spec: &ClipSpec, plan: &CachePlan, a_e: f64, b_e: f64) -> Result<ReadEnd, Failure> {
        let from = (a_e - PREROLL).max(0.0);
        let target = b_e + KEYFRAME_MARGIN;
        let mut restarted = false;
        // 測試要看停在開頭的樣子：等到開頭那一格解好（PlaybackRestart）才算讀完
        let mut hook_done = spec.test.on_ready.is_none();
        let mut last_seen = (None::<f64>, 0u64);
        let mut moved_at = Instant::now();
        // 分離器停下來不讀了（沒有到檔尾、也不是記憶體滿了）的時間
        let mut idle_since: Option<Instant> = None;
        loop {
            ctl.check()?;
            match self.next(POLL) {
                // 開頭就到了檔尾（A 超過結尾）
                Some(Event::EndFile {
                    reason: mpv::EndReason::Eof,
                    ..
                }) => return Err(Failure::NoData),
                // 讀到一半出錯（檔案被移走、網路斷線）
                Some(Event::EndFile { .. } | Event::Shutdown) => return Err(self.load_failure(&spec.source)),
                Some(Event::PlaybackRestart) => restarted = true,
                _ => {}
            }
            if restarted
                && !hook_done
                && let Some(hook) = &spec.test.on_ready
            {
                hook(&self.mpv);
                hook_done = true;
            }
            let Some(st) = self.cache_state() else {
                if moved_at.elapsed() > READ_STALL {
                    return Err(self.stall_failure());
                }
                continue;
            };
            if let Some(end) = st.end {
                let fraction = ((end - from) / (target - from).max(0.001)).clamp(0.0, 1.0);
                ctl.progress(Progress {
                    phase: Phase::Reading,
                    fraction: Some(fraction as f32),
                });
            }
            let enough = st.eof || st.end.is_some_and(|e| e >= target);
            // 停下來不讀了：記憶體快取滿了。已經讀到 B 就夠用（只是 B 後面的關鍵影格可能不在），不然改放磁碟
            let full = st.idle && !st.eof && !plan.on_disk && st.fw_bytes as f64 >= plan.max_bytes as f64 * 0.9;
            // 分離器自己停了（往前讀的秒數到了）：已經讀到 B 就夠用，不必等很久沒有新資料
            let stopped = st.idle && !st.eof && !full;
            idle_since = if stopped {
                idle_since.or(Some(Instant::now()))
            } else {
                None
            };
            let settled = idle_since.is_some_and(|t| t.elapsed() >= IDLE_SETTLE);
            let past_b = st.end.is_some_and(|e| e >= b_e);
            let outcome = if enough || (settled && past_b) {
                Some(ReadEnd::Ready)
            } else if full {
                Some(if past_b { ReadEnd::Ready } else { ReadEnd::RamFull })
            } else {
                None
            };
            if let Some(outcome) = outcome
                && hook_done
            {
                return Ok(outcome);
            }
            let seen = (st.end, st.total_bytes + st.file_cache_bytes.unwrap_or(0));
            if seen != last_seen {
                last_seen = seen;
                moved_at = Instant::now();
            } else if moved_at.elapsed() > READ_STALL {
                // 很久沒有新的資料：已經讀到 B 就用讀到的，不然放棄
                if past_b && hook_done {
                    return Ok(ReadEnd::Ready);
                }
                return Err(self.stall_failure());
            }
        }
    }

    /// 很久讀不到新資料的原因：磁碟快取寫不進去（快取所在的磁碟滿了）時 mpv 照樣讀，
    /// 封包留在記憶體裡、到上限就停，只有記錄裡有原因。其他情況是讀取逾時（網路、檔案很慢）
    fn stall_failure(&mut self) -> Failure {
        self.drain();
        stall_failure(&self.log.lines())
    }

    /// 對齊關鍵影格：mpv 告訴我們快取裡真正寫得出來的範圍（起點 = A 之前的關鍵影格，
    /// 終點 = B 之後下一個關鍵影格之前最後一格的時間）。沒有這個指令、找不到時用原本的 A、B
    fn align(&mut self, on: bool, a_e: f64, b_e: f64) -> (f64, f64) {
        if !on
            || self.mpv.set_property("ab-loop-a", a_e).is_err()
            || self.mpv.set_property("ab-loop-b", b_e).is_err()
            || self.mpv.command(&["ab-loop-align-cache"]).is_err()
        {
            return (a_e, b_e);
        }
        let read = |name: &str| self.mpv.get_property::<f64>(name).ok().filter(|t| t.is_finite());
        let a = read("ab-loop-a").unwrap_or(a_e);
        let b = read("ab-loop-b").unwrap_or(b_e);
        if b > a { (a, b) } else { (a_e, b_e) }
    }

    /// 估計寫出來的大小：快取裡這一段占的比例 × 快取的大小（進度條用）
    fn estimate_bytes(&self, a: f64, b: f64) -> Option<u64> {
        let st = self.cache_state()?;
        let bytes = st.file_cache_bytes.filter(|b| *b > 0).unwrap_or(st.total_bytes);
        let (start, end) = st.ranges.iter().copied().find(|(s, e)| *s <= a + 0.5 && *e >= a)?;
        let span = end - start;
        (span > 0.0 && bytes > 0).then(|| (bytes as f64 * ((b - a) / span).min(1.0)) as u64)
    }

    /// 寫檔（不能中斷：mpv 寫完才回覆）。進度看暫存檔的大小，不設逾時（網路磁碟可能很慢，但會寫完）
    fn dump(
        &mut self,
        ctl: &Ctl,
        a: f64,
        b: f64,
        temp: &Path,
        estimate: Option<u64>,
        on_dump: Option<&OnDump>,
    ) -> Result<(), Failure> {
        self.drain();
        self.log.clear();
        // 終點多 1 毫秒：B' 是最後一格的時間，影像寫到下一個關鍵影格之前、聲音寫到同一個時間
        let (start, end) = (format!("{:.6}", a.max(0.0)), format!("{:.6}", b + 0.001));
        let target = temp.to_string_lossy();
        self.mpv
            .command_async(DUMP_ID, &["dump-cache", &start, &end, &target])
            .map_err(|e| Failure::Engine(e.description()))?;
        if let Some(hook) = on_dump {
            hook();
        }
        loop {
            ctl.progress(Progress {
                phase: Phase::Writing,
                fraction: estimate.filter(|e| *e > 0).map(|e| {
                    let size = std::fs::metadata(temp).map(|m| m.len()).unwrap_or(0);
                    (size as f64 / e as f64).min(0.99) as f32
                }),
            });
            // 寫檔時 mpv 的核心停著（拿著分離器的鎖）：只等事件，不讀屬性
            if let Some(Event::CommandReply { id: DUMP_ID, result }) = self.next(POLL) {
                // 寫到一半失敗的記錄在回覆之前就記下了，跟在回覆後面送來
                self.drain();
                return dump_result(result, &self.log.lines());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::LogLine;

    fn track(id: i64, kind: TrackKind, codec: &str, lang: Option<&str>) -> Track {
        Track {
            id,
            kind,
            title: None,
            lang: lang.map(str::to_owned),
            codec: Some(codec.into()),
            selected: false,
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
        }
    }

    const MP4: &str = "mov,mp4,m4a,3gp,3g2,mj2";

    #[test]
    fn auto_container_matrix() {
        let auto =
            |fmt: &str, path: &str, v: Option<&str>, a: Option<&str>| plan_container(ClipFormat::Auto, fmt, path, v, a);
        assert_eq!(auto(MP4, "C:/v/a.mp4", Some("h264"), Some("aac")), Ok(Container::Mp4));
        assert_eq!(auto("mkv", "/v/a.mkv", Some("h264"), Some("aac")), Ok(Container::Mkv));
        assert_eq!(auto("mkv", "/v/a.webm", Some("vp9"), Some("opus")), Ok(Container::Webm));
        assert_eq!(
            auto("matroska,webm", "https://x/a.webm?sig=1", Some("vp9"), Some("opus")),
            Ok(Container::Webm),
            "網址看去掉參數之後的副檔名"
        );
        assert_eq!(auto("mpegts", "/v/a.ts", Some("h264"), Some("ac3")), Ok(Container::Ts));
        // 來源的格式放不下：存 MKV
        assert_eq!(auto(MP4, "/v/a.mp4", Some("h264"), Some("truehd")), Ok(Container::Mkv));
        assert_eq!(auto("mkv", "/v/a.webm", Some("vp9"), Some("aac")), Ok(Container::Mkv));
        assert_eq!(
            auto("mpegts", "/v/a.ts", Some("h264"), Some("pcm_s16le")),
            Ok(Container::Mkv)
        );
        // 其他格式（AVI、HLS、MOV 以外的）：MKV
        assert_eq!(auto("avi", "/v/a.avi", Some("mpeg4"), Some("mp3")), Ok(Container::Mkv));
        assert_eq!(
            auto("hls", "https://x/a.m3u8", Some("h264"), Some("aac")),
            Ok(Container::Mkv)
        );
        // 只有影像
        assert_eq!(auto(MP4, "/v/a.mp4", Some("h264"), None), Ok(Container::Mp4));
        // mpv 自己的 MKV 分離器的 WebVTT 名稱跟 FFmpeg 的不同：當成同一種
        assert!(Container::Webm.holds(TrackKind::Audio, "opus"));
        assert_eq!(normalize_codec("webvtt-webm"), "webvtt");
        // 字幕一律不放
        for c in Container::ALL {
            assert!(
                !c.holds(TrackKind::Sub, "subrip") && !c.holds(TrackKind::Sub, "ass"),
                "{c:?}"
            );
        }
        assert_eq!(auto(MP4, "/v/a.mp4", None, None), Err(Failure::NoData));
    }

    #[test]
    fn explicit_container_rejects() {
        let pick = |c: ClipFormat, v: Option<&str>, a: Option<&str>| plan_container(c, "mkv", "/v/a.mkv", v, a);
        let incompatible = |container: &str, codec: &str| {
            Err(Failure::Incompatible {
                container: container.into(),
                codec: codec.into(),
            })
        };
        assert_eq!(pick(ClipFormat::Mp4, Some("h264"), Some("aac")), Ok(Container::Mp4));
        assert_eq!(
            pick(ClipFormat::Mp4, Some("h264"), Some("dts")),
            incompatible("MP4", "dts")
        );
        assert_eq!(
            pick(ClipFormat::Webm, Some("h264"), Some("opus")),
            incompatible("WebM", "h264")
        );
        assert_eq!(
            pick(ClipFormat::Webm, Some("vp9"), Some("aac")),
            incompatible("WebM", "aac")
        );
        assert_eq!(
            pick(ClipFormat::Ts, Some("vp9"), Some("aac")),
            incompatible("TS", "vp9")
        );
        assert_eq!(
            pick(ClipFormat::Ts, Some("h264"), Some("flac")),
            incompatible("TS", "flac")
        );
        assert_eq!(pick(ClipFormat::Mkv, Some("wmv3"), Some("wmav2")), Ok(Container::Mkv));
        // 藍光、DVD 的 LPCM 哪裡都放不下（MKV 也不行）
        assert_eq!(
            pick(ClipFormat::Mkv, Some("h264"), Some("pcm_bluray")),
            Err(Failure::AudioCodec)
        );
        assert_eq!(
            pick(ClipFormat::Auto, Some("mpeg2video"), Some("pcm_dvd")),
            Err(Failure::AudioCodec)
        );
        // 只有聲音：MKV → MKA、MP4 → M4A
        assert_eq!(pick(ClipFormat::Mkv, None, Some("ac3")), Ok(Container::Mka));
        assert_eq!(pick(ClipFormat::Mp4, None, Some("aac")), Ok(Container::M4a));
        assert_eq!(pick(ClipFormat::Mp4, None, Some("mp3")), incompatible("M4A", "mp3"));
        assert_eq!(pick(ClipFormat::Webm, None, Some("opus")), Ok(Container::Webm));
        // 副檔名對應到播放引擎有的寫檔程式
        assert_eq!(
            Container::ALL.map(Container::ext),
            [
                "mkv", "mka", "mp4", "m4a", "webm", "ts", "mp3", "flac", "opus", "ogg", "wav"
            ]
        );
        assert!(Container::ALL.iter().all(|c| !c.label().is_empty()));
        assert!(Container::Ts.takes_video() && !Container::Mka.takes_video());
    }

    #[test]
    fn audio_only_containers() {
        let cases = [
            ("aac", Container::M4a),
            ("alac", Container::M4a),
            ("mp3", Container::Mp3),
            ("flac", Container::Flac),
            ("opus", Container::Opus),
            ("vorbis", Container::Ogg),
            ("pcm_s16le", Container::Wav),
            ("pcm_s24le", Container::Wav),
            ("pcm_f32le", Container::Wav),
            // 大端序的 PCM（MOV）：WAV 放不下，放 MKA
            ("pcm_s16be", Container::Mka),
            ("pcm_s24be", Container::Mka),
            ("ac3", Container::Mka),
            ("eac3", Container::Mka),
            ("dts", Container::Mka),
            ("truehd", Container::Mka),
            ("mp2", Container::Mka),
            ("wmav2", Container::Mka),
            ("tta", Container::Mka),
            ("wavpack", Container::Mka),
        ];
        for (codec, want) in cases {
            assert_eq!(audio_container(codec), Ok(want), "{codec}");
            assert!(want.holds(TrackKind::Audio, codec), "{codec} 要放得進 {want:?}");
        }
        assert!(!Container::Wav.holds(TrackKind::Audio, "pcm_s16be"));
        // 不重新編碼存不成檔案的：開始前就拒絕
        for codec in [
            "ape",
            "dsd_lsbf",
            "dsd_msbf_planar",
            "musepack7",
            "musepack8",
            "pcm_bluray",
            "pcm_dvd",
        ] {
            assert_eq!(audio_container(codec), Err(Failure::AudioCodec), "{codec}");
            assert!(!Container::Mka.holds(TrackKind::Audio, codec), "{codec}");
        }
    }

    #[test]
    fn cache_plan_ram_vs_disk_and_preroll() {
        // 1 MB/s、10 秒的段落：(10 + 25) × 1.3 MB，放記憶體（至少 64 MiB）
        let p = cache_plan(Some(1_000_000.0), 100.0, 110.0);
        assert_eq!(p.start, 95.0, "A 前面多讀 5 秒");
        assert_eq!(p.cache_secs, 35.0, "前面 5 秒 + 段落 + 後面 20 秒");
        assert!(!p.on_disk);
        assert_eq!(p.max_bytes, MIN_RAM);
        let p = cache_plan(Some(5_000_000.0), 100.0, 130.0);
        assert!(!p.on_disk);
        assert_eq!(p.max_bytes, (5_000_000.0 * 55.0 * 1.3) as u64);
        // 估計超過 512 MiB：磁碟
        let p = cache_plan(Some(10_000_000.0), 0.0, 600.0);
        assert!(p.on_disk);
        assert_eq!(p.start, 0.0, "開頭不會是負的");
        // 不知道位元率：磁碟
        assert!(cache_plan(None, 1.0, 2.0).on_disk);
        assert!(cache_plan(Some(f64::NAN), 1.0, 2.0).on_disk);
        // 記憶體不夠時改放磁碟：其他不變
        let p = cache_plan(Some(1_000_000.0), 100.0, 110.0);
        let d = p.to_disk();
        assert!(d.on_disk && d.max_bytes == RAM_LIMIT);
        assert_eq!((d.start, d.cache_secs), (p.start, p.cache_secs));
        // 讀到 B 後面 15 秒就停：在讀快取的上限（B 後面 20 秒）之前
        const { assert!(KEYFRAME_MARGIN < TAIL) };
        // 從更前面重讀：多讀 30 秒（不會是負的），讀到的終點不變
        let p = cache_plan(Some(1_000_000.0), 100.0, 110.0);
        let e = p.earlier();
        assert_eq!((e.start, e.start + e.cache_secs), (65.0, p.start + p.cache_secs));
        let e = cache_plan(Some(1_000_000.0), 8.0, 9.0).earlier();
        assert_eq!((e.start, e.cache_secs), (0.0, 29.0));
        // 往前讀的秒數從解碼器拿到的封包（start 之前的關鍵影格）算起：多給，關鍵影格在 start 前面很遠也讀得到 B 後面。
        // 估計記憶體、磁碟空間用的是要讀的秒數（多給的不一定會讀）
        let p = cache_plan(Some(1_000_000.0), 100.0, 110.0);
        assert_eq!(p.readahead_secs(), p.cache_secs + KEYFRAME_ALLOWANCE);
        const { assert!(KEYFRAME_ALLOWANCE >= PREROLL + LONG_PREROLL) };
        assert_eq!(p.max_bytes, MIN_RAM);
    }

    #[test]
    fn read_range_must_start_by_a() {
        // 讀到的範圍從 A 之前的關鍵影格開始
        assert!(starts_by(&[(2.0, 30.0)], 8.0, 0.5));
        // 從 A 之後的關鍵影格才開始：A 之前的不在
        assert!(!starts_by(&[(10.0, 30.0)], 8.0, 0.5));
        // 檔案開頭的第一格不在 0
        assert!(starts_by(&[(0.04, 30.0)], 0.0, 0.5));
        // 範圍在 A 之前就結束了
        assert!(!starts_by(&[(0.0, 5.0)], 8.0, 0.5));
        assert!(!starts_by(&[], 8.0, 0.5));
        assert!(starts_by(&[(20.0, 25.0), (0.0, 9.0)], 8.0, 0.5));
        // 重讀之後還是沒有 A：片段從 A 之後第一個讀到的範圍的開頭開始
        assert_eq!(first_cached_after(&[(10.0, 30.0)], 8.0, 14.0), Some(10.0));
        assert_eq!(
            first_cached_after(&[(25.0, 40.0), (0.0, 5.0), (12.0, 20.0)], 8.0, 30.0),
            Some(12.0)
        );
        // 範圍在 B 之後才開始、或沒有讀到任何東西：沒有資料
        assert_eq!(first_cached_after(&[(10.0, 30.0)], 8.0, 9.0), None);
        assert_eq!(first_cached_after(&[(0.0, 5.0)], 8.0, 14.0), None);
        assert_eq!(first_cached_after(&[], 8.0, 14.0), None);
    }

    #[test]
    fn default_picks_use_embedded_tracks() {
        let mut ext = track(3, TrackKind::Audio, "aac", Some("eng"));
        ext.external = true;
        let mut state = crate::player::State {
            tracks: vec![
                track(1, TrackKind::Video, "h264", None),
                track(1, TrackKind::Audio, "pcm_bluray", Some("jpn")),
                track(2, TrackKind::Audio, "ac3", Some("chi")),
                ext,
            ],
            ..Default::default()
        };
        let select = |state: &mut crate::player::State, kind: TrackKind, id: i64| {
            for t in state.tracks.iter_mut().filter(|t| t.kind == kind) {
                t.selected = t.id == id;
            }
        };
        select(&mut state, TrackKind::Video, 1);
        // 正在播放的是內嵌、放得進的：照用
        select(&mut state, TrackKind::Audio, 2);
        let p = default_picks(&state);
        assert_eq!(p.video.as_ref().map(|v| v.ordinal), Some(1));
        assert_eq!(
            p.audio.as_ref().map(|a| (a.ordinal, a.codec.as_deref())),
            Some((2, Some("ac3")))
        );
        // 正在播放外掛的音軌（另外載入的配音）：改用第一條放得進的內嵌音軌（藍光的 LPCM 放不進，跳過）
        select(&mut state, TrackKind::Audio, 3);
        let p = default_picks(&state);
        assert_eq!(p.audio.as_ref().map(|a| a.ordinal), Some(2), "{p:?}");
        // 正在播放的是藍光的 LPCM：一樣改用放得進的
        select(&mut state, TrackKind::Audio, 1);
        assert_eq!(default_picks(&state).audio.map(|a| a.ordinal), Some(2));
        // 關掉聲音：不放聲音
        select(&mut state, TrackKind::Audio, 0);
        assert_eq!(default_picks(&state).audio, None);
        // 只有聲音、主播放器又關掉了聲音：預設什麼都不放，但檔案照樣能存（匯出視窗裡可以選音軌）
        let mut muted = state.clone();
        muted.tracks.retain(|t| t.kind != TrackKind::Video);
        assert!(usable_picks(&muted, default_picks(&muted)).is_err());
        let any = any_picks(&muted);
        assert_eq!(
            any.audio.as_ref().map(|a| a.ordinal),
            Some(2),
            "跳過放不進的 LPCM：{any:?}"
        );
        assert!(usable_picks(&muted, any).is_ok());
        // 只有放不進的音軌：有影像時只放影像（選單照樣能用），沒有影像時說明音訊格式不能存
        state.tracks.retain(|t| t.kind != TrackKind::Audio || t.id == 1);
        select(&mut state, TrackKind::Audio, 1);
        let p = default_picks(&state);
        assert!(p.video.is_some() && p.audio.is_none(), "{p:?}");
        assert!(usable_picks(&state, p).is_ok());
        state.tracks.retain(|t| t.kind != TrackKind::Video);
        assert_eq!(usable_picks(&state, default_picks(&state)), Err(Failure::AudioCodec));
        assert_eq!(usable_picks(&state, any_picks(&state)), Err(Failure::AudioCodec));
        // 什麼都沒有
        let empty = crate::player::State::default();
        assert_eq!(usable_picks(&empty, default_picks(&empty)), Err(Failure::NoData));
    }

    #[test]
    fn file_urls_become_local_paths() {
        assert_eq!(local_path("/v/a.mkv"), PathBuf::from("/v/a.mkv"));
        assert_eq!(local_path("av://lavfi:testsrc"), PathBuf::from("av://lavfi:testsrc"));
        let p = local_path("FILE:///v/%E5%BD%B1%E7%89%87.mkv");
        assert!(!p.to_string_lossy().contains("://"), "{}", p.display());
        assert!(p.to_string_lossy().ends_with("影片.mkv"), "{}", p.display());
        // 不存在的 file:// 網址：檢查的是換好的路徑（找不到），存在的：讀得到
        let dir = std::env::temp_dir().join(format!("vitascope-clip-url-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("片 段.mkv");
        std::fs::write(&file, b"x").unwrap();
        let url = format!(
            "file://{}{}",
            if cfg!(windows) { "/" } else { "" },
            file.to_string_lossy().replace('\\', "/")
        );
        assert_eq!(check_local(Path::new(&url)), Ok(()), "{url}");
        assert_eq!(
            check_local(Path::new(&url.replace("片 段", "沒有"))),
            Err(Failure::SourceMissing)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn match_track_by_ordinal_then_codec() {
        let mut ext = track(3, TrackKind::Audio, "aac", None);
        ext.external = true;
        let main = [
            track(1, TrackKind::Video, "h264", None),
            track(1, TrackKind::Audio, "aac", Some("jpn")),
            track(2, TrackKind::Audio, "ac3", Some("chi")),
            ext.clone(),
        ];
        // 排第幾不算外掛的；外掛的放不進片段
        let pick = StreamPick::of(&main, &main[2]).unwrap();
        assert_eq!((pick.ordinal, pick.codec.as_deref()), (2, Some("ac3")));
        assert_eq!(StreamPick::of(&main, &ext), None);
        // 匯出用的 mpv 排的順序一樣：同一個位置
        let same = [
            track(1, TrackKind::Video, "h264", None),
            track(1, TrackKind::Audio, "aac", Some("jpn")),
            track(2, TrackKind::Audio, "ac3", Some("chi")),
        ];
        assert_eq!(match_track(&same, &pick), Some(2));
        // 順序不同：找編碼、語言一樣的
        let swapped = [
            track(1, TrackKind::Audio, "ac3", Some("chi")),
            track(2, TrackKind::Audio, "aac", Some("jpn")),
        ];
        assert_eq!(match_track(&swapped, &pick), Some(1));
        // 語言不同、編碼一樣：同一個位置不算，找語言一樣的
        let jpn = StreamPick::of(&main, &main[1]).unwrap();
        let langs = [
            track(1, TrackKind::Audio, "aac", Some("eng")),
            track(2, TrackKind::Audio, "aac", Some("jpn")),
        ];
        assert_eq!(match_track(&langs, &jpn), Some(2));
        // 一邊沒有語言：編碼一樣就算
        let nolang = [track(1, TrackKind::Audio, "aac", None)];
        assert_eq!(match_track(&nolang, &jpn), Some(1));
        // 名稱不同的寫法（webvtt-webm / webvtt）算同一種
        let mut sub_pick = jpn.clone();
        sub_pick.kind = TrackKind::Sub;
        sub_pick.codec = Some("webvtt-webm".into());
        assert_eq!(
            match_track(&[track(1, TrackKind::Sub, "webvtt", Some("jpn"))], &sub_pick),
            Some(1)
        );
        // 找不到：None
        assert_eq!(match_track(&[track(1, TrackKind::Audio, "opus", None)], &jpn), None);
        // 專輯封面不當成影像
        let mut cover = track(1, TrackKind::Video, "mjpeg", None);
        cover.albumart = true;
        let video = StreamPick {
            kind: TrackKind::Video,
            ordinal: 2,
            codec: Some("h264".into()),
            lang: None,
            title: None,
        };
        assert_eq!(match_track(&[cover.clone()], &video), None);
        assert_eq!(
            match_track(&[cover, track(2, TrackKind::Video, "h264", None)], &video),
            Some(2)
        );
    }

    #[test]
    fn start_time_correction() {
        // 主播放器（mpv 的 MKV 分離器）起始 0，匯出用的（FFmpeg）起始 0.08：同一個畫面早 0.08 秒
        assert!((to_export_time(10.0, 0.0, 0.08) - 9.92).abs() < 1e-9);
        assert!((to_main_time(9.92, 0.0, 0.08) - 10.0).abs() < 1e-9);
        // TS：兩邊都從 1.4 秒開始
        assert!((to_export_time(3.0, 1.4, 1.4) - 3.0).abs() < 1e-9);
        for t in [0.0, 1.5, 3600.25] {
            assert!((to_main_time(to_export_time(t, 0.3, 1.1), 0.3, 1.1) - t).abs() < 1e-9);
        }
    }

    #[test]
    fn timeline_guard() {
        // 章節連結（mpv 的時間線分離器：mkv_oc/mkv）、使用者開的 EDL、CUE
        assert!(timeline_source("/v/a.mkv", "mkv_oc/mkv", false));
        assert!(timeline_source("edl://a.mkv,0,10", "edl/mkv", false));
        assert!(timeline_source("C:/v/list.EDL", "edl/mkv", false));
        assert!(timeline_source("C:/v/album.cue", "cue/flac", false));
        assert!(timeline_source("/v/a.cue", "", false), "還沒讀到格式也算");
        // 一般的檔案
        assert!(!timeline_source("/v/a.mkv", "mkv", false));
        assert!(!timeline_source("/v/a.mp4", MP4, false));
        assert!(!timeline_source("https://x/a.m3u8", "hls", false));
        // 網站影片的 EDL（影像、聲音分開）：匯出用的 mpv 開同一個，不算
        assert!(!timeline_source(
            "https://site/watch?v=1",
            "multi/mov,mp4,m4a,3gp,3g2,mj2",
            true
        ));
        // 開起來之後：兩邊的長度差超過 1 秒就是對不上
        assert_eq!(check_timeline(Some(100.0), Some(100.4), false), Ok(()));
        assert_eq!(check_timeline(Some(100.0), Some(60.0), false), Err(Failure::Timeline));
        assert_eq!(check_timeline(Some(42.0), Some(43.5), true), Err(Failure::Timeline));
        // 本機：不知道長度的不檢查；網路：匯出用的 mpv 沒有長度就是直播
        assert_eq!(check_timeline(None, Some(3.0), false), Ok(()));
        assert_eq!(check_timeline(Some(3.0), None, false), Ok(()));
        assert_eq!(check_timeline(Some(3.0), None, true), Err(Failure::Live));
    }

    #[test]
    fn read_stall_reasons() {
        let line = |text: &str| LogLine::new("error", "cache", text);
        // 磁碟快取寫不進去（快取所在的磁碟滿了）：不是讀取逾時
        assert_eq!(
            stall_failure(&[line("Failed to write to cache file: No space left on device")]),
            Failure::CacheFailed
        );
        assert_eq!(
            stall_failure(&[line("Could not write all data.")]),
            Failure::CacheFailed
        );
        // 其他的（沒有記錄、別的錯誤）：讀取逾時
        assert_eq!(stall_failure(&[]), Failure::ReadTimeout);
        assert_eq!(stall_failure(&[line("Something odd.")]), Failure::ReadTimeout);
    }

    #[test]
    fn dump_reply_and_log_lines() {
        let line = |level: &str, text: &str| LogLine::new(level, "recorder", text);
        // 寫到一半磁碟滿了：mpv 照樣回報成功，看記錄
        assert_eq!(
            dump_result(Ok(()), &[line("error", "Failed writing packet.")]),
            Err(Failure::WriteFailed)
        );
        assert_eq!(
            dump_result(Ok(()), &[line("error", "Writing trailer failed.")]),
            Err(Failure::WriteFailed)
        );
        assert_eq!(
            dump_result(Ok(()), &[line("error", "Failed to retrieve packet from cache.")]),
            Err(Failure::CacheFailed)
        );
        // 實驗性功能的警告、Windows 刪不掉開著的磁碟快取檔：不是失敗
        assert_eq!(
            dump_result(
                Ok(()),
                &[
                    line("warn", "This is an experimental feature. Output files might be broken."),
                    line("error", "Failed to unlink cache temporary file after creation."),
                ]
            ),
            Ok(())
        );
        // 回覆失敗：看記錄，沒有認得的就用回覆的錯誤
        let err = || {
            Err(mpv::Error {
                code: -12,
                context: "dump".into(),
            })
        };
        assert_eq!(
            dump_result(err(), &[line("error", "Failed opening output file.")]),
            Err(Failure::NoPermission(None))
        );
        assert_eq!(
            dump_result(err(), &[line("error", "Can't mux one of the input streams.")]),
            Err(Failure::CantMux)
        );
        assert!(matches!(dump_result(err(), &[]), Err(Failure::Engine(_))));
    }

    #[test]
    fn cache_state_from_mpv_json() {
        let st = CacheState::parse(
            r#"{"cache-end": 12.5, "eof": false, "idle": true, "fw-bytes": 1000, "total-bytes": 5000,
                "file-cache-bytes": 7000, "seekable-ranges": [{"start": 1.0, "end": 12.5}]}"#,
        )
        .unwrap();
        assert_eq!(st.end, Some(12.5));
        assert!(st.idle && !st.eof);
        assert_eq!(
            (st.fw_bytes, st.total_bytes, st.file_cache_bytes),
            (1000, 5000, Some(7000))
        );
        assert_eq!(st.ranges, [(1.0, 12.5)]);
        // 剛開檔：還沒有任何欄位
        let st = CacheState::parse("{}").unwrap();
        assert_eq!((st.end, st.eof, st.ranges.len()), (None, false, 0));
        assert!(CacheState::parse("not json").is_none());
    }
}
