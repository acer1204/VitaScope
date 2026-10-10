//! yt-dlp 的資料 → mpv 要開什麼（純邏輯，不碰 mpv）。
//!
//! - 單一影片：選到的格式（影像、聲音分開時是兩個）→ 一個網址，或把兩個合成一個 EDL（`edl://`）；
//!   DASH 的分段 → `!mp4_dash` EDL。這個檔案專用的 mpv 選項（User-Agent、Referer、Cookie、一次要求的大小、標題、
//!   開始時間）放在 `file-local-options`，下一個檔案自動還原。字幕用「選到才下載」的 EDL 加入。
//! - 播放清單：項目交給影戲自己的播放清單（[`Plan::Playlist`]），不用 mpv 的播放清單。
//! - 從網站資料讀到的網址都要檢查（[`crate::net::Origin::Site`]：只開 http、https）：
//!   mpv 只依「誰開的」限制網址，我們交給它的它都接受，`file://`、`edl://` 之類會讀本機檔案。
//!
//! 參考 mpv 的 `player/lua/ytdl_hook.lua`（我們的播放引擎沒有 Lua，系統的 libmpv 也關掉了它的 ytdl）。

use super::json::{Format, Fragment, Info};
use super::{CodecPref, SitePrefs, YtdlError};
use crate::mpv::Node;
use crate::net::{self, NetDefaults, NetSettings, Origin};

/// 標題最多幾個字
const MAX_TITLE_CHARS: usize = 300;

/// 怎麼把資料變成 mpv 的選項
#[derive(Debug, Clone, PartialEq)]
pub struct PlanConfig {
    /// 載入網站的字幕
    pub site_subs: bool,
    /// 使用者自己設了 User-Agent（設定或 `VITASCOPE_MPV_OPTS`）：不換成網站要的
    pub keep_user_agent: bool,
    /// 全域的 HTTP 標頭（「設定 → 網路」的 Referer 與其他標頭；網站給了同名的標頭時用網站的）
    pub headers: Vec<String>,
    /// 全域的 `stream-lavf-o`（`名稱=值,名稱=值`；加上一次要求的大小）
    pub lavf: String,
    /// 畫質清單（選單）的編碼偏好
    pub codec: CodecPref,
}

impl PlanConfig {
    /// `user_agent_override` = `VITASCOPE_MPV_OPTS` 之類設了 user-agent
    pub fn new(net: &NetSettings, defaults: &NetDefaults, prefs: &SitePrefs, user_agent_override: bool) -> Self {
        let o = net::mpv_options(net, defaults);
        Self {
            site_subs: prefs.site_subs,
            keep_user_agent: user_agent_override || !net.user_agent.trim().is_empty(),
            headers: o.headers,
            lavf: o.lavf,
            codec: prefs.codec,
        }
    }
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self::new(
            &NetSettings::default(),
            &NetDefaults::default(),
            &SitePrefs::default(),
            false,
        )
    }
}

/// 這次要播哪個格式
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Choice {
    /// yt-dlp 依設定選的
    #[default]
    Default,
    /// 選單選的（影像格式，加上要配的聲音格式）
    Format { video: String, audio: Option<String> },
    /// 只播聲音
    AudioOnly,
}

/// 只對這次開檔有效的要求（換畫質、載入整個播放清單時）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mode {
    pub choice: Choice,
    /// 網址也是播放清單時整個載入
    pub yes_playlist: bool,
    /// 從這裡開始（秒；換畫質時接著播）
    pub start_at: Option<f64>,
}

/// mpv 要開什麼
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// 網址本身就是媒體檔：照原樣開
    Native,
    /// 一部影片
    Media(Box<MediaPlan>),
    /// 播放清單：（網址, 標題），從第 `start` 個開始播
    Playlist {
        entries: Vec<(String, Option<String>)>,
        start: usize,
    },
}

/// 一部影片
#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlan {
    /// mpv 實際開的（`stream-open-filename`）：一個網址或 `edl://…`
    pub open: String,
    /// `file-local-options/<名稱>` 的值，照順序設定
    pub options: Vec<(String, Node)>,
    /// 網站的字幕（`sub-add <url> auto <title> <lang>`）
    pub subs: Vec<SubTrack>,
    /// 章節（開檔後設定 `chapter-list`）
    pub chapters: Vec<ChapterMark>,
    pub info: NetInfo,
}

/// 一個網站字幕：選到才下載的 EDL
#[derive(Debug, Clone, PartialEq)]
pub struct SubTrack {
    pub url: String,
    pub title: String,
    pub lang: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChapterMark {
    pub time: f64,
    /// 空的 = 之後顯示「第 n 章」
    pub title: String,
}

/// 網站影片的資料（媒體資訊、選單、續播用）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NetInfo {
    /// 使用者開的網址（網頁）
    pub page_url: String,
    pub title: Option<String>,
    /// 網站的擷取器（`Youtube`）
    pub extractor: Option<String>,
    /// 影片在網站上的代號
    pub id: Option<String>,
    pub uploader: Option<String>,
    pub thumbnail: Option<String>,
    pub duration: Option<f64>,
    pub live: bool,
    /// 正在用的格式（媒體資訊、選單打勾）
    pub chosen: Vec<FormatSummary>,
    /// 畫質選單
    pub choices: Vec<QualityChoice>,
    /// 網址裡有 `list=`（選單提供「載入整個播放清單」）
    pub list_in_url: bool,
}

impl NetInfo {
    /// 續播、書籤的代號：同一部影片的不同網址是同一個（`ytdl://youtube/代號`）
    pub fn resume_key(&self) -> Option<String> {
        let site = self.extractor.as_deref().zip(self.id.as_deref());
        net::resume_key(&self.page_url, site)
    }
}

/// 一個格式的摘要
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormatSummary {
    pub format_id: Option<String>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// 總位元率（kbit/s）
    pub tbr: Option<f64>,
}

impl FormatSummary {
    fn of(f: &Format) -> Self {
        Self {
            format_id: f.format_id.clone(),
            vcodec: f.vcodec.clone(),
            acodec: f.acodec.clone(),
            height: height(f),
            fps: f.fps,
            tbr: f.tbr,
        }
    }
}

/// 影像編碼（選單上顯示）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    Av1,
    Vp9,
    Hevc,
    Avc,
    Other,
}

impl VideoCodec {
    pub fn of(vcodec: &str) -> Self {
        let c = vcodec.to_ascii_lowercase();
        if c.starts_with("av01") || c == "av1" {
            VideoCodec::Av1
        } else if c.starts_with("vp9") || c.starts_with("vp09") {
            VideoCodec::Vp9
        } else if c.starts_with("hev1") || c.starts_with("hvc1") || c == "h265" || c == "hevc" {
            VideoCodec::Hevc
        } else if c.starts_with("avc1") || c.starts_with("avc3") || c == "h264" {
            VideoCodec::Avc
        } else {
            VideoCodec::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::Av1 => "AV1",
            VideoCodec::Vp9 => "VP9",
            VideoCodec::Hevc => "HEVC",
            VideoCodec::Avc => "AVC",
            VideoCodec::Other => "",
        }
    }

    /// 編碼偏好下的順序（小的優先）。自動 = yt-dlp 預設的順序（AV1、VP9、HEVC、H.264）
    fn rank(self, pref: CodecPref) -> u8 {
        let base = match self {
            VideoCodec::Av1 => 1,
            VideoCodec::Vp9 => 2,
            VideoCodec::Hevc => 3,
            VideoCodec::Avc => 4,
            VideoCodec::Other => 5,
        };
        let first = match pref {
            CodecPref::Auto => None,
            CodecPref::H264 => Some(VideoCodec::Avc),
            CodecPref::Av1 => Some(VideoCodec::Av1),
            CodecPref::Vp9 => Some(VideoCodec::Vp9),
        };
        if first == Some(self) { 0 } else { base }
    }
}

/// 畫質選單的一項
#[derive(Debug, Clone, PartialEq)]
pub enum QualityChoice {
    Video {
        height: u32,
        /// 48 fps 以上
        fps60: bool,
        codec: VideoCodec,
        video: String,
        /// 配的聲音格式（影像本身有聲音時 None）
        audio: Option<String>,
    },
    AudioOnly {
        audio: String,
    },
}

impl QualityChoice {
    /// 選單上的文字（`1080p60 · VP9`、「只播聲音」）
    pub fn label(&self) -> String {
        match self {
            QualityChoice::Video {
                height, fps60, codec, ..
            } => {
                let fps = if *fps60 { "60" } else { "" };
                match codec.label() {
                    "" => format!("{height}p{fps}"),
                    c => format!("{height}p{fps} · {c}"),
                }
            }
            QualityChoice::AudioOnly { .. } => crate::tr!("只播聲音", "Audio only").to_owned(),
        }
    }

    pub fn choice(&self) -> Choice {
        match self {
            QualityChoice::Video { video, audio, .. } => Choice::Format {
                video: video.clone(),
                audio: audio.clone(),
            },
            QualityChoice::AudioOnly { .. } => Choice::AudioOnly,
        }
    }

    /// 正在播的是這一項（選單打勾）
    pub fn is_chosen(&self, chosen: &[FormatSummary]) -> bool {
        let ids: Vec<&str> = chosen.iter().filter_map(|f| f.format_id.as_deref()).collect();
        match self {
            QualityChoice::Video { video, .. } => ids.contains(&video.as_str()),
            QualityChoice::AudioOnly { audio } => ids == [audio.as_str()],
        }
    }
}

// ───────────── 計畫 ─────────────

/// yt-dlp 的資料 → mpv 要開什麼。`page_url` 是使用者開的網址
pub fn plan(info: &Info, page_url: &str, mode: &Mode, cfg: &PlanConfig) -> Result<Plan, YtdlError> {
    if info.direct == Some(true) {
        return Ok(Plan::Native);
    }
    if info.is_playlist() {
        // 只有一個項目、指回這個網頁（multi_video 把一部影片包成清單）：當成一部影片，不然會一直重開自己
        if let [only] = info.entries.as_slice()
            && points_back(only, info, page_url)
            && has_media(only)
        {
            return media(only, page_url, mode, cfg);
        }
        return playlist(info, page_url, mode);
    }
    media(info, page_url, mode, cfg)
}

/// 播放清單的項目指回清單本身的網頁
fn points_back(entry: &Info, list: &Info, page_url: &str) -> bool {
    entry.kind.as_deref() != Some("url_transparent")
        && entry
            .webpage_url
            .as_deref()
            .is_some_and(|u| Some(u) == list.webpage_url.as_deref() || u == page_url)
}

/// 項目本身有完整的格式資料（不是 `--flat-playlist` 的簡短項目）
fn has_media(entry: &Info) -> bool {
    !entry.formats.is_empty() || !entry.requested_formats.is_empty() || entry.format.protocol.is_some()
}

/// 標題：空白（含換行）整理成一個、太長的截掉；空的 None
fn clean_title(t: Option<&str>) -> Option<String> {
    let t: String = t?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect();
    (!t.is_empty()).then_some(t)
}

/// 從網站資料讀到的網址能不能交給 mpv
fn site_ok(url: &str) -> Result<String, YtdlError> {
    if net::allowed(url, Origin::Site) {
        Ok(url.to_owned())
    } else {
        Err(YtdlError::NoPlayable)
    }
}

/// 播放清單 → 項目
fn playlist(info: &Info, page_url: &str, mode: &Mode) -> Result<Plan, YtdlError> {
    let mut entries = Vec::new();
    let mut ids: Vec<Option<String>> = Vec::new();
    for e in &info.entries {
        let Some(url) = entry_url(e, info, page_url) else {
            continue;
        };
        entries.push((url, clean_title(e.title.as_deref())));
        ids.push(e.id.clone());
    }
    if entries.is_empty() {
        return Err(YtdlError::EmptyPlaylist);
    }
    let start = if mode.yes_playlist {
        start_index(page_url, &ids).unwrap_or(0)
    } else {
        0
    };
    Ok(Plan::Playlist { entries, start })
}

/// 項目的網址：網頁的網址（指回清單本身的不算），不然是項目的 `url`；
/// 只有代號的（YouTube 的 `--flat-playlist`）補成影片網址。不能開的、會開回自己的略過
fn entry_url(e: &Info, list: &Info, page_url: &str) -> Option<String> {
    let page = e.webpage_url.as_deref().filter(|_| !points_back(e, list, page_url));
    let raw = page.or(e.format.url.as_deref())?.trim();
    let url = if raw.contains("://") {
        raw.to_owned()
    } else {
        let youtube = [e.ie_key.as_deref(), e.extractor_key.as_deref()]
            .iter()
            .flatten()
            .any(|k| k.eq_ignore_ascii_case("youtube"));
        let id = e.id.as_deref().unwrap_or(raw);
        let id_ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
        if !(youtube && id_ok) {
            return None;
        }
        format!("https://www.youtube.com/watch?v={id}")
    };
    (net::allowed(&url, Origin::Site) && url != page_url).then_some(url)
}

/// 「載入整個播放清單」時從哪一個開始：網址 `v=` 指的那部影片（`index=` 指的剛好是它時用那一個）
fn start_index(page_url: &str, ids: &[Option<String>]) -> Option<usize> {
    let u = url::Url::parse(page_url).ok()?;
    let mut v = None;
    let mut index = None;
    for (k, val) in u.query_pairs() {
        match k.as_ref() {
            "v" => v = Some(val.into_owned()),
            "index" => index = val.parse::<usize>().ok(),
            _ => {}
        }
    }
    let v = v?;
    let is_v = |i: usize| ids.get(i).and_then(|id| id.as_deref()) == Some(v.as_str());
    if let Some(i) = index.and_then(|n| n.checked_sub(1)).filter(|&i| is_v(i)) {
        return Some(i);
    }
    (0..ids.len()).find(|&i| is_v(i))
}

/// 一部影片
fn media(info: &Info, page_url: &str, mode: &Mode, cfg: &PlanConfig) -> Result<Plan, YtdlError> {
    let live = info.live();
    let (streams, audio_only) = pick_streams(info, &mode.choice, cfg.codec)?;
    if streams.iter().any(|f| f.has_drm == Some(true)) {
        return Err(YtdlError::Drm);
    }
    let urls = streams
        .iter()
        .map(|f| stream_url(f, live))
        .collect::<Result<Vec<_>, _>>()?;
    let open = match urls.as_slice() {
        [one] => one.clone(),
        many => {
            let parts: Vec<String> = many
                .iter()
                .map(|u| format!("!new_stream;!no_clip;!no_chapters;{}", edl_escape(u)))
                .collect();
            format!("edl://{}", parts.join(";"))
        }
    };

    // 標頭、Cookie 照 ytdl_hook：第一個選到的格式的，沒有時用最外層的
    let first = streams[0];
    let headers = if first.http_headers.is_empty() {
        &info.format.http_headers
    } else {
        &first.http_headers
    };
    let header = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
            .filter(|v| !v.is_empty() && !v.chars().any(char::is_control))
    };
    let mut options: Vec<(String, Node)> = Vec::new();
    let mut opt = |name: &str, value: Node| options.push((name.to_owned(), value));
    if !cfg.keep_user_agent
        && let Some(ua) = header("User-Agent")
    {
        opt("user-agent", Node::Str(ua.to_owned()));
    }
    let site: Vec<(&str, &str)> = ["Referer", "Cookie", "X-Forwarded-For"]
        .into_iter()
        .filter_map(|n| header(n).map(|v| (n, v)))
        .collect();
    if !site.is_empty() {
        // 全域的標頭照樣送，網站給了同名的就用網站的
        let mut all: Vec<String> = cfg
            .headers
            .iter()
            .filter(|h| {
                let name = h.split_once(':').map_or("", |(n, _)| n.trim());
                !site.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
        all.extend(site.iter().map(|(n, v)| format!("{n}: {v}")));
        opt("http-header-fields", Node::strings(all));
    }
    let cookies = first.cookies.as_deref().or(info.format.cookies.as_deref());
    let host = first.url.as_deref().and_then(net::host);
    if let Some(data) = cookies.and_then(|c| netscape_cookies(c, host.as_deref())) {
        opt("cookies", Node::Flag(true));
        opt("cookies-file", Node::Str(format!("memory://{data}")));
    }
    // YouTube 對沒有分段的要求限速：一次要求 yt-dlp 建議的大小（所有選到的格式裡最小的）
    let chunk = streams
        .iter()
        .filter_map(|f| f.downloader_options.http_chunk_size)
        .filter(|c| *c >= 1.0)
        .fold(None, |m: Option<f64>, c| Some(m.map_or(c, |m| m.min(c))));
    if let Some(chunk) = chunk {
        let mut map: Vec<(String, Node)> = cfg
            .lavf
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, _)| !k.is_empty() && *k != "request_size")
            .map(|(k, v)| (k.to_owned(), Node::Str(v.to_owned())))
            .collect();
        map.push(("request_size".into(), Node::Str(format!("{}", chunk.round() as u64))));
        opt("stream-lavf-o", Node::Map(map));
    }
    let title = clean_title(info.title.as_deref());
    if let Some(t) = &title {
        opt("force-media-title", Node::Str(t.clone()));
    }
    let start = mode
        .start_at
        .or(if live { None } else { info.start_time })
        .filter(|s| s.is_finite() && *s > 0.0);
    if let Some(s) = start {
        opt("start", Node::Str(format!("{s}")));
    }
    if audio_only {
        opt("vid", Node::Str("no".into()));
    }

    let subs = if cfg.site_subs { subtitles(info) } else { Vec::new() };
    let chapters = info
        .chapters
        .iter()
        .filter_map(|c| {
            let time = c.start_time.filter(|t| t.is_finite() && *t >= 0.0)?;
            Some(ChapterMark {
                time,
                title: clean_title(c.title.as_deref()).unwrap_or_default(),
            })
        })
        .collect();
    let list_in_url = url::Url::parse(page_url).is_ok_and(|u| u.query_pairs().any(|(k, _)| k == "list"));
    let net_info = NetInfo {
        page_url: page_url.to_owned(),
        title,
        extractor: info.extractor_name().map(str::to_owned),
        id: info.id.clone(),
        uploader: clean_title(info.uploader.as_deref()),
        thumbnail: info.thumbnail.clone().filter(|t| net::allowed(t, Origin::Site)),
        duration: info.duration.filter(|d| *d > 0.0),
        live,
        chosen: streams.iter().map(|f| FormatSummary::of(f)).collect(),
        choices: choices(info, cfg.codec),
        list_in_url,
    };
    Ok(Plan::Media(Box::new(MediaPlan {
        open,
        options,
        subs,
        chapters,
        info: net_info,
    })))
}

/// 要播的格式，以及是不是只有聲音（要關掉影像軌）
fn pick_streams<'a>(info: &'a Info, choice: &Choice, codec: CodecPref) -> Result<(Vec<&'a Format>, bool), YtdlError> {
    match choice {
        Choice::Format { video, audio } => {
            let find = |id: &str| {
                info.formats
                    .iter()
                    .find(|f| f.format_id.as_deref() == Some(id))
                    .ok_or(YtdlError::FormatGone)
            };
            let mut v = vec![find(video)?];
            if let Some(a) = audio {
                v.push(find(a)?);
            }
            Ok((v, false))
        }
        Choice::AudioOnly => match best_audio(&info.formats, codec, info.live()) {
            Some(a) => Ok((vec![a], true)),
            // 沒有單獨的聲音：播影音合在一起的，只是關掉影像
            None => Ok((default_streams(info)?, true)),
        },
        Choice::Default => {
            let streams = default_streams(info)?;
            let audio_only = streams.iter().all(|f| f.audio_only());
            Ok((streams, audio_only))
        }
    }
}

/// yt-dlp 依設定選的格式
fn default_streams(info: &Info) -> Result<Vec<&Format>, YtdlError> {
    if !info.requested_formats.is_empty() {
        return Ok(info.requested_formats.iter().collect());
    }
    if info.format.url.is_some() || !info.format.fragments.is_empty() {
        return Ok(vec![&info.format]);
    }
    Err(YtdlError::NoPlayable)
}

/// 格式的傳輸方式（沒寫時看網址）
fn protocol(f: &Format) -> String {
    f.protocol
        .clone()
        .or_else(|| f.url.as_deref().and_then(net::scheme))
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// 一個格式 → mpv 開的網址
fn stream_url(f: &Format, live: bool) -> Result<String, YtdlError> {
    let proto = protocol(f);
    match proto.as_str() {
        "http" | "https" | "m3u8" | "m3u8_native" => site_ok(f.url.as_deref().ok_or(YtdlError::NoPlayable)?),
        "http_dash_segments" if !live => dash_edl(f),
        "" => Err(YtdlError::NoPlayable),
        _ => Err(YtdlError::UnsupportedProtocol(proto)),
    }
}

/// EDL 的值：`%位元組數%內容`（內容裡可以有分號、逗號）
fn edl_escape(s: &str) -> String {
    format!("%{}%{s}", s.len())
}

/// DASH 的分段 → `edl://!mp4_dash,init=…;分段,length=秒;…`。第一段沒有長度（而且不只一段）時是初始化段
fn dash_edl(f: &Format) -> Result<String, YtdlError> {
    let frags = &f.fragments;
    if frags.is_empty() {
        return Err(YtdlError::UnsupportedProtocol("http_dash_segments".into()));
    }
    let base = f.fragment_base_url.as_deref().and_then(|b| url::Url::parse(b).ok());
    let join = |fr: &Fragment| -> Result<String, YtdlError> {
        let u = match (&base, fr.path.as_deref()) {
            (Some(b), Some(p)) => b.join(p).map(String::from).map_err(|_| YtdlError::NoPlayable)?,
            _ => fr.url.clone().ok_or(YtdlError::NoPlayable)?,
        };
        site_ok(&u)
    };
    let mut head = String::from("!mp4_dash");
    let mut rest = frags.as_slice();
    if frags[0].duration.is_none() && frags.len() > 1 {
        head.push_str(&format!(",init={}", edl_escape(&join(&frags[0])?)));
        rest = &frags[1..];
    }
    let mut parts = vec![head];
    for fr in rest {
        // 每一段都要有長度，不然 EDL 沒辦法接起來
        let d = fr
            .duration
            .filter(|d| *d > 0.0)
            .ok_or_else(|| YtdlError::UnsupportedProtocol("http_dash_segments".into()))?;
        parts.push(format!("{},length={d}", edl_escape(&join(fr)?)));
    }
    Ok(format!("edl://{};", parts.join(";")))
}

/// yt-dlp 的 Cookie（一行：`名稱=值; Domain=…; Path=…; Secure; Expires=…; 下一個名稱=值; …`）→ Netscape cookies.txt。
/// 沒寫網域的用影片網址的主機；有 tab、換行的整個略過（會弄亂檔案格式）
pub fn netscape_cookies(line: &str, fallback_host: Option<&str>) -> Option<String> {
    #[derive(Default)]
    struct Cookie {
        name: String,
        value: String,
        domain: Option<String>,
        path: Option<String>,
        expires: Option<String>,
        secure: bool,
    }
    let mut all: Vec<Cookie> = Vec::new();
    let mut cur: Option<Cookie> = None;
    for stem in line.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        match stem.split_once('=') {
            Some((name, value)) if !name.trim().is_empty() && !value.is_empty() => {
                let (name, value) = (name.trim(), value.trim());
                let lower = name.to_ascii_lowercase();
                if ["expires", "max-age", "domain", "path"].contains(&lower.as_str()) {
                    if let Some(c) = cur.as_mut() {
                        match lower.as_str() {
                            "domain" => c.domain = Some(value.to_owned()),
                            "path" => c.path = Some(value.to_owned()),
                            "expires" => c.expires = Some(value.to_owned()),
                            _ => {}
                        }
                    }
                } else {
                    all.extend(cur.take());
                    cur = Some(Cookie {
                        name: name.to_owned(),
                        value: value.to_owned(),
                        ..Default::default()
                    });
                }
            }
            None if stem.eq_ignore_ascii_case("secure") => {
                if let Some(c) = cur.as_mut() {
                    c.secure = true;
                }
            }
            _ => {}
        }
    }
    all.extend(cur);
    let bad = |s: &str| s.chars().any(char::is_control);
    let mut out = String::new();
    for c in all {
        let Some(domain) = c.domain.or_else(|| fallback_host.map(str::to_owned)) else {
            continue;
        };
        let path = c.path.unwrap_or_else(|| "/".into());
        let expires = c
            .expires
            .filter(|e| e.chars().all(|ch| ch.is_ascii_digit()))
            .unwrap_or_else(|| "0".into());
        let value = c
            .value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(&c.value)
            .to_owned();
        if [&domain, &path, &c.name, &value].iter().any(|s| bad(s)) {
            continue;
        }
        let flag = |b: bool| if b { "TRUE" } else { "FALSE" };
        out.push_str(&format!(
            "{domain}\t{}\t{path}\t{}\t{expires}\t{}\t{value}\n",
            flag(domain.starts_with('.')),
            flag(c.secure),
            c.name
        ));
    }
    (!out.is_empty()).then_some(out)
}

/// 網站的字幕 → 選到才下載的 EDL（語言照字母順序；只要播放引擎看得懂的格式）
fn subtitles(info: &Info) -> Vec<SubTrack> {
    let mut out = Vec::new();
    for (lang, s) in &info.requested_subtitles {
        let codec = match s.ext.as_deref().map(str::to_ascii_lowercase).as_deref() {
            Some("vtt") => "webvtt",
            Some("srt") => "subrip",
            Some("ass" | "ssa") => "ass",
            _ => continue,
        };
        let src = match (&s.data, &s.url) {
            (Some(data), _) => format!("memory://{data}"),
            (None, Some(u)) if net::allowed(u, Origin::Site) => u.clone(),
            _ => continue,
        };
        let lang = lang.trim();
        if lang.is_empty() || lang.chars().any(char::is_control) {
            continue;
        }
        out.push(SubTrack {
            url: format!(
                "edl://!no_clip;!delay_open,media_type=sub,codec={codec};{}",
                edl_escape(&src)
            ),
            title: clean_title(s.name.as_deref()).unwrap_or_else(|| lang.to_owned()),
            lang: lang.to_owned(),
        });
    }
    out
}

// ───────────── 畫質選單 ─────────────

fn height(f: &Format) -> Option<u32> {
    f.height
        .filter(|h| *h >= 1.0 && *h < 100_000.0)
        .map(|h| h.round() as u32)
}

/// 能播的格式（傳輸方式播放引擎看得懂、網址能開、沒有 DRM）
fn usable(f: &Format, live: bool) -> bool {
    f.has_drm != Some(true) && stream_url(f, live).is_ok()
}

/// 最好的單獨聲音格式（H.264 優先時先挑 AAC，比較多裝置能硬體處理、相容性好）
fn best_audio(formats: &[Format], codec: CodecPref, live: bool) -> Option<&Format> {
    let rate = |f: &Format| f.abr.or(f.tbr).unwrap_or(0.0);
    formats
        .iter()
        .filter(|f| f.audio_only() && usable(f, live))
        .min_by(|a, b| {
            let aac = |f: &Format| {
                codec == CodecPref::H264
                    && f.acodec
                        .as_deref()
                        .is_some_and(|c| c.to_ascii_lowercase().starts_with("mp4a"))
            };
            aac(b).cmp(&aac(a)).then(rate(b).total_cmp(&rate(a)))
        })
}

/// 畫質選單：每個（高度、是否 60fps）一項，挑編碼偏好裡最好的、位元率最高的；
/// 只有影像的配上最好的聲音。由高到低，最後是「只播聲音」
pub fn choices(info: &Info, codec: CodecPref) -> Vec<QualityChoice> {
    let live = info.live();
    let mut best: Vec<(u32, bool, &Format)> = Vec::new();
    for f in &info.formats {
        let Some(h) = height(f) else { continue };
        if !f.vcodec.as_deref().is_some_and(|c| c != "none") || !usable(f, live) {
            continue;
        }
        let fps60 = f.fps.is_some_and(|fps| fps >= 48.0);
        let key = |f: &Format| {
            (
                VideoCodec::of(f.vcodec.as_deref().unwrap_or_default()).rank(codec),
                std::cmp::Reverse(ordered(f.tbr.unwrap_or(0.0))),
            )
        };
        match best.iter_mut().find(|(bh, b60, _)| *bh == h && *b60 == fps60) {
            Some(slot) => {
                if key(f) < key(slot.2) {
                    slot.2 = f;
                }
            }
            None => best.push((h, fps60, f)),
        }
    }
    best.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    let audio = best_audio(&info.formats, codec, live);
    let mut out: Vec<QualityChoice> = best
        .into_iter()
        .filter_map(|(height, fps60, f)| {
            let video = f.format_id.clone()?;
            let muxed = f.acodec.as_deref().is_some_and(|c| c != "none");
            Some(QualityChoice::Video {
                height,
                fps60,
                codec: VideoCodec::of(f.vcodec.as_deref().unwrap_or_default()),
                video,
                audio: if muxed {
                    None
                } else {
                    audio.and_then(|a| a.format_id.clone())
                },
            })
        })
        .collect();
    if let Some(a) = audio.and_then(|a| a.format_id.clone()) {
        out.push(QualityChoice::AudioOnly { audio: a });
    }
    out
}

/// f64 的排序（位元率；不會是 NaN，json 已經濾掉了）
fn ordered(x: f64) -> u64 {
    (x.max(0.0) * 1000.0) as u64
}

#[cfg(test)]
mod tests {
    use super::super::json;
    use super::*;

    const YOUTUBE: &str = include_str!("../../tests/fixtures/ytdl/youtube_like.json");
    const PLAYLIST: &str = include_str!("../../tests/fixtures/ytdl/playlist.json");
    const DASH: &str = include_str!("../../tests/fixtures/ytdl/dash.json");
    const PAGE: &str = "https://www.youtube.com/watch?v=BaW_jenozKc";

    fn info(text: &str) -> Info {
        json::parse(text.as_bytes()).unwrap()
    }

    fn media_plan(text: &str, mode: &Mode, cfg: &PlanConfig) -> MediaPlan {
        match plan(&info(text), PAGE, mode, cfg).unwrap() {
            Plan::Media(m) => *m,
            other => panic!("應該是一部影片：{other:?}"),
        }
    }

    fn option<'a>(m: &'a MediaPlan, name: &str) -> Option<&'a Node> {
        m.options.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    #[test]
    fn separate_video_and_audio_become_one_edl_with_exact_lengths() {
        let m = media_plan(YOUTUBE, &Mode::default(), &PlanConfig::default());
        let v = "https://rr1.googlevideo.com/videoplayback?itag=399&id=1;x,y";
        let a = "https://rr1.googlevideo.com/videoplayback?itag=251&id=1";
        assert_eq!(
            m.open,
            format!(
                "edl://!new_stream;!no_clip;!no_chapters;%{}%{v};!new_stream;!no_clip;!no_chapters;%{}%{a}",
                v.len(),
                a.len()
            )
        );
        assert_eq!(m.info.chosen.len(), 2);
        assert_eq!(m.info.chosen[0].format_id.as_deref(), Some("399"));
        assert_eq!(m.info.title.as_deref(), Some("youtube-dl test video \"'/\\ä↭𝕐"));
        assert_eq!(m.info.resume_key().as_deref(), Some("ytdl://youtube/BaW_jenozKc"));
        assert!(!m.info.live && !m.info.list_in_url);
        assert_eq!(m.info.duration, Some(10.0));
        // 不是只有聲音：不關影像
        assert!(option(&m, "vid").is_none());
    }

    #[test]
    fn site_headers_cookies_and_request_size() {
        let m = media_plan(YOUTUBE, &Mode::default(), &PlanConfig::default());
        assert_eq!(
            option(&m, "user-agent"),
            Some(&Node::Str("Mozilla/5.0 (Site UA)".into()))
        );
        // 換行的值不送（會多塞一個標頭）
        assert_eq!(
            option(&m, "http-header-fields"),
            Some(&Node::strings(["Referer: https://www.youtube.com/"]))
        );
        assert_eq!(option(&m, "cookies"), Some(&Node::Flag(true)));
        let Some(Node::Str(file)) = option(&m, "cookies-file") else {
            panic!("要有 cookies-file");
        };
        assert_eq!(
            file,
            "memory://.youtube.com\tTRUE\t/\tTRUE\t1767225600\tVISITOR_INFO1_LIVE\tabc\n\
             rr1.googlevideo.com\tFALSE\t/watch\tFALSE\t0\tPREF\tf6=8\n"
        );
        // 全域的重新連線選項保留，加上 yt-dlp 建議的（最小的）分段大小
        assert_eq!(
            option(&m, "stream-lavf-o"),
            Some(&Node::Map(vec![
                ("reconnect".into(), Node::Str("1".into())),
                ("reconnect_streamed".into(), Node::Str("1".into())),
                ("reconnect_delay_max".into(), Node::Str("5".into())),
                ("request_size".into(), Node::Str("10485760".into())),
            ]))
        );
        // 最小的是聲音那一個時也用它（不是第一個串流的）
        let bigger_video = YOUTUBE.replace("\"http_chunk_size\": 10485760", "\"http_chunk_size\": 31457280");
        let m2 = media_plan(&bigger_video, &Mode::default(), &PlanConfig::default());
        let Some(Node::Map(lavf)) = option(&m2, "stream-lavf-o") else {
            panic!("要有 stream-lavf-o");
        };
        assert_eq!(
            lavf.iter().find(|(k, _)| k == "request_size").map(|(_, v)| v),
            Some(&Node::Str("20971520".into()))
        );
        // 標題：換行變成空白
        assert_eq!(
            option(&m, "force-media-title"),
            Some(&Node::Str("youtube-dl test video \"'/\\ä↭𝕐".into()))
        );
        assert_eq!(option(&m, "start"), Some(&Node::Str("2".into())));
    }

    #[test]
    fn user_settings_win_where_they_should() {
        let net = NetSettings {
            user_agent: "Mine/1.0".into(),
            referrer: "https://mine.test/".into(),
            headers: vec!["X-Token: 1".into()],
            reconnect: false,
            ..Default::default()
        };
        let cfg = PlanConfig::new(&net, &NetDefaults::default(), &SitePrefs::default(), false);
        let m = media_plan(YOUTUBE, &Mode::default(), &cfg);
        // 使用者設了 User-Agent：不換成網站的
        assert!(option(&m, "user-agent").is_none());
        // 網站給了 Referer：用網站的，其他全域標頭照送
        assert_eq!(
            option(&m, "http-header-fields"),
            Some(&Node::strings(["X-Token: 1", "Referer: https://www.youtube.com/"]))
        );
        assert_eq!(
            option(&m, "stream-lavf-o"),
            Some(&Node::Map(vec![
                ("reconnect".into(), Node::Str("0".into())),
                ("request_size".into(), Node::Str("10485760".into())),
            ]))
        );
        // VITASCOPE_MPV_OPTS 設了 user-agent 也一樣
        let cfg = PlanConfig::new(
            &NetSettings::default(),
            &NetDefaults::default(),
            &SitePrefs::default(),
            true,
        );
        assert!(option(&media_plan(YOUTUBE, &Mode::default(), &cfg), "user-agent").is_none());
    }

    #[test]
    fn subtitles_and_chapters() {
        let m = media_plan(YOUTUBE, &Mode::default(), &PlanConfig::default());
        let en = "https://www.youtube.com/api/timedtext?v=BaW_jenozKc&lang=en&fmt=vtt";
        let zh = "memory://WEBVTT\n\n00:00.000 --> 00:01.000\n嗨\n";
        assert_eq!(
            m.subs,
            vec![
                SubTrack {
                    url: format!(
                        "edl://!no_clip;!delay_open,media_type=sub,codec=webvtt;%{}%{en}",
                        en.len()
                    ),
                    title: "English".into(),
                    lang: "en".into(),
                },
                SubTrack {
                    url: format!(
                        "edl://!no_clip;!delay_open,media_type=sub,codec=subrip;%{}%{zh}",
                        zh.len()
                    ),
                    title: "zh-TW".into(),
                    lang: "zh-TW".into(),
                },
            ],
            "json3 不支援、file:// 的網址不開"
        );
        assert_eq!(
            m.chapters,
            vec![
                ChapterMark {
                    time: 0.0,
                    title: "Intro".into()
                },
                ChapterMark {
                    time: 3.0,
                    title: String::new()
                },
                ChapterMark {
                    time: 6.5,
                    title: "End".into()
                },
            ]
        );
        // 關掉網站字幕
        let cfg = PlanConfig {
            site_subs: false,
            ..Default::default()
        };
        assert!(media_plan(YOUTUBE, &Mode::default(), &cfg).subs.is_empty());
    }

    #[test]
    fn quality_menu() {
        let i = info(YOUTUBE);
        let labels: Vec<String> = choices(&i, CodecPref::Auto).iter().map(QualityChoice::label).collect();
        assert_eq!(
            labels,
            ["1080p60 · VP9", "1080p · AV1", "720p · AVC", "360p · AVC", "只播聲音"]
        );
        // H.264 優先：同一個畫質挑 AVC，聲音挑 AAC
        let c = choices(&i, CodecPref::H264);
        assert_eq!(
            c[1],
            QualityChoice::Video {
                height: 1080,
                fps60: false,
                codec: VideoCodec::Avc,
                video: "137".into(),
                audio: Some("140".into()),
            }
        );
        // 影音合在一起的（18）不配聲音；DRM、看不懂的傳輸方式不列
        assert_eq!(
            c[3],
            QualityChoice::Video {
                height: 360,
                fps60: false,
                codec: VideoCodec::Avc,
                video: "18".into(),
                audio: None,
            }
        );
        // 自動：聲音挑位元率最高的（opus 251）
        let QualityChoice::Video { audio, .. } = &choices(&i, CodecPref::Auto)[0] else {
            panic!()
        };
        assert_eq!(audio.as_deref(), Some("251"));
        // 正在播的打勾
        let m = media_plan(YOUTUBE, &Mode::default(), &PlanConfig::default());
        let chosen: Vec<&QualityChoice> = m.info.choices.iter().filter(|q| q.is_chosen(&m.info.chosen)).collect();
        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].label(), "1080p · AV1");
    }

    #[test]
    fn picking_a_quality_uses_the_cached_formats() {
        let mode = Mode {
            choice: Choice::Format {
                video: "248".into(),
                audio: Some("251".into()),
            },
            start_at: Some(42.5),
            ..Default::default()
        };
        let m = media_plan(YOUTUBE, &mode, &PlanConfig::default());
        assert!(m.open.contains("itag=248") && m.open.contains("itag=251"));
        // 換畫質時從原本的位置接著播（不用網址的開始時間）
        assert_eq!(option(&m, "start"), Some(&Node::Str("42.5".into())));
        // 不在清單上的格式
        let gone = Mode {
            choice: Choice::Format {
                video: "999".into(),
                audio: None,
            },
            ..Default::default()
        };
        assert_eq!(
            plan(&info(YOUTUBE), PAGE, &gone, &PlanConfig::default()),
            Err(YtdlError::FormatGone)
        );
        // 只播聲音：最好的聲音，關掉影像
        let audio = Mode {
            choice: Choice::AudioOnly,
            ..Default::default()
        };
        let m = media_plan(YOUTUBE, &audio, &PlanConfig::default());
        assert!(m.open.contains("itag=251") && !m.open.starts_with("edl://"));
        assert_eq!(option(&m, "vid"), Some(&Node::Str("no".into())));
    }

    #[test]
    fn single_muxed_format_and_direct_files() {
        let text = r#"{"id": "x", "title": "Clip", "extractor_key": "Generic", "url": "https://cdn.test/v.mp4",
                       "protocol": "https", "vcodec": "avc1", "acodec": "mp4a", "http_headers": {}}"#;
        let m = media_plan(text, &Mode::default(), &PlanConfig::default());
        assert_eq!(m.open, "https://cdn.test/v.mp4");
        assert!(option(&m, "http-header-fields").is_none() && option(&m, "cookies").is_none());
        assert!(option(&m, "stream-lavf-o").is_none(), "沒有分段大小時不動全域的選項");
        assert!(m.info.choices.is_empty());
        let direct = info(r#"{"direct": true, "url": "https://cdn.test/v.mp4"}"#);
        assert_eq!(
            plan(
                &direct,
                "https://cdn.test/v.mp4",
                &Mode::default(),
                &PlanConfig::default()
            ),
            Ok(Plan::Native)
        );
        // 只有聲音的網站（SoundCloud 之類）：關掉影像軌
        let audio = r#"{"title": "Song", "url": "https://cdn.test/a.mp3", "protocol": "https",
                        "vcodec": "none", "acodec": "mp3"}"#;
        assert_eq!(
            option(&media_plan(audio, &Mode::default(), &PlanConfig::default()), "vid"),
            Some(&Node::Str("no".into()))
        );
    }

    #[test]
    fn live_hls_plays_and_does_not_seek_to_start_time() {
        let text = r#"{"title": "Live", "is_live": true, "live_status": "is_live", "start_time": 30,
                       "url": "https://live.test/master.m3u8", "protocol": "m3u8_native", "vcodec": "avc1", "acodec": "mp4a"}"#;
        let m = media_plan(text, &Mode::default(), &PlanConfig::default());
        assert_eq!(m.open, "https://live.test/master.m3u8");
        assert!(m.info.live);
        assert!(option(&m, "start").is_none());
    }

    #[test]
    fn dash_fragments_become_an_mp4_dash_edl() {
        let m = media_plan(DASH, &Mode::default(), &PlanConfig::default());
        let init = "https://dash.test/base/init.mp4";
        let s1 = "https://dash.test/base/seg-1.m4s";
        let s2 = "https://cdn2.test/abs/seg-2.m4s";
        let video = format!(
            "edl://!mp4_dash,init=%{}%{init};%{}%{s1},length=4.004;%{}%{s2},length=2.5;",
            init.len(),
            s1.len(),
            s2.len()
        );
        let audio = "https://dash.test/audio.m4a";
        assert_eq!(
            m.open,
            format!(
                "edl://!new_stream;!no_clip;!no_chapters;%{}%{video};!new_stream;!no_clip;!no_chapters;%{}%{audio}",
                video.len(),
                audio.len()
            )
        );
        // 直播的 DASH 不支援
        let live = DASH.replacen("\"is_live\": false", "\"is_live\": true", 1);
        assert!(matches!(
            plan(&info(&live), PAGE, &Mode::default(), &PlanConfig::default()),
            Err(YtdlError::UnsupportedProtocol(_))
        ));
    }

    #[test]
    fn errors_from_the_data() {
        let p = |text: &str| plan(&info(text), PAGE, &Mode::default(), &PlanConfig::default());
        assert_eq!(
            p(r#"{"url": "https://x.test/v.mp4", "protocol": "https", "has_drm": true}"#),
            Err(YtdlError::Drm)
        );
        // 不安全的網址：沒有可以播放的格式
        assert_eq!(
            p(r#"{"url": "file:///etc/passwd", "protocol": "https"}"#),
            Err(YtdlError::NoPlayable)
        );
        assert_eq!(
            p(r#"{"url": "edl://%3%abc", "protocol": "http"}"#),
            Err(YtdlError::NoPlayable)
        );
        assert_eq!(
            p(
                r#"{"requested_formats": [{"url": "https://x.test/v", "protocol": "https"},
                                         {"url": "file:///etc/passwd", "protocol": "https"}]}"#
            ),
            Err(YtdlError::NoPlayable)
        );
        assert_eq!(
            p(r#"{"url": "wss://x.test/live", "protocol": "websocket_frag"}"#),
            Err(YtdlError::UnsupportedProtocol("websocket_frag".into()))
        );
        assert_eq!(p(r#"{"title": "nothing"}"#), Err(YtdlError::NoPlayable));
        // DASH 的分段裡有不安全的網址
        let bad = DASH.replacen("https://cdn2.test/abs/seg-2.m4s", "file:///etc/passwd", 1);
        assert_eq!(p(&bad), Err(YtdlError::NoPlayable));
    }

    #[test]
    fn playlists_become_our_own_entries() {
        let i = info(PLAYLIST);
        let page = "https://www.youtube.com/watch?v=bbbbbbbbbbb&list=PL1&index=2";
        let Plan::Playlist { entries, start } = plan(&i, page, &Mode::default(), &PlanConfig::default()).unwrap()
        else {
            panic!("應該是播放清單");
        };
        assert_eq!(
            entries,
            vec![
                (
                    "https://www.youtube.com/watch?v=aaaaaaaaaaa".to_owned(),
                    Some("First one".to_owned())
                ),
                (
                    "https://www.youtube.com/watch?v=bbbbbbbbbbb".to_owned(),
                    Some("Second two".to_owned())
                ),
                ("https://vimeo.com/123".to_owned(), None),
            ],
            "只有代號的補成網址、標題的換行變成空白、file:// 與不認得的代號略過"
        );
        assert_eq!(start, 0, "只播這部影片的模式不管 v=");
        let yes = Mode {
            yes_playlist: true,
            ..Default::default()
        };
        let Plan::Playlist { start, .. } = plan(&i, page, &yes, &PlanConfig::default()).unwrap() else {
            panic!()
        };
        assert_eq!(start, 1, "從網址 v= 的那一部開始");
        // 全部都不能開
        let empty = info(r#"{"_type": "playlist", "entries": [{"url": "file:///x"}, {"url": "rel"}]}"#);
        assert_eq!(
            plan(&empty, page, &Mode::default(), &PlanConfig::default()),
            Err(YtdlError::EmptyPlaylist)
        );
    }

    #[test]
    fn a_single_entry_pointing_back_plays_as_one_video() {
        let text = r#"{"_type": "multi_video", "webpage_url": "https://site.test/v/1", "entries": [
            {"webpage_url": "https://site.test/v/1", "title": "Part", "url": "https://cdn.test/1.mp4", "protocol": "https"}]}"#;
        let Plan::Media(m) = plan(
            &info(text),
            "https://site.test/v/1",
            &Mode::default(),
            &PlanConfig::default(),
        )
        .unwrap() else {
            panic!("應該是一部影片");
        };
        assert_eq!(m.open, "https://cdn.test/1.mp4");
        // 指回自己的項目不放進清單（會一直重開）
        let text = r#"{"_type": "playlist", "webpage_url": "https://site.test/l", "entries": [
            {"webpage_url": "https://site.test/l", "url": "https://site.test/l"},
            {"url": "https://site.test/v/2"}]}"#;
        let Plan::Playlist { entries, .. } = plan(
            &info(text),
            "https://site.test/l",
            &Mode::default(),
            &PlanConfig::default(),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(entries, vec![("https://site.test/v/2".to_owned(), None)]);
    }

    #[test]
    fn netscape_cookie_text() {
        assert_eq!(netscape_cookies("", Some("h")), None);
        // 沒有網域也沒有主機：略過
        assert_eq!(netscape_cookies("a=1", None), None);
        assert_eq!(
            netscape_cookies(
                "a=\"q\"; Max-Age=5; Expires=Thu, 01 Jan 2026; b=2; Domain=.x.test; Secure",
                Some("h.test")
            ),
            Some("h.test\tFALSE\t/\tFALSE\t0\ta\tq\n.x.test\tTRUE\t/\tTRUE\t0\tb\t2\n".into())
        );
        // tab 會弄亂格式：那一個略過
        assert_eq!(
            netscape_cookies("a=x\ty; b=2", Some("h")),
            Some("h\tFALSE\t/\tFALSE\t0\tb\t2\n".into())
        );
    }
}
