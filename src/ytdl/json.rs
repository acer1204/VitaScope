//! yt-dlp `-J` 輸出的 JSON（只讀用得到的欄位）。
//!
//! 各網站的擷取器給的資料不一致：欄位常常是 `null`、數字有時是字串、代號有時是數字。
//! 這裡一律寬鬆地讀：型別不對的欄位當成沒有，清單裡看不懂的項目略過，整份資料不會因為一個欄位讀不進來而失敗。

use super::YtdlError;
use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::Value;
use std::collections::BTreeMap;

/// 一部影片、一個播放清單，或播放清單裡的一個項目（`--flat-playlist` 的項目只有少數欄位）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Info {
    /// `video`（或沒有）、`playlist`、`multi_video`、`url`、`url_transparent`
    #[serde(rename = "_type", deserialize_with = "text")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "text")]
    pub id: Option<String>,
    #[serde(deserialize_with = "text")]
    pub title: Option<String>,
    #[serde(deserialize_with = "text")]
    pub extractor: Option<String>,
    #[serde(deserialize_with = "text")]
    pub extractor_key: Option<String>,
    /// 播放清單項目的擷取器（`Youtube`）
    #[serde(deserialize_with = "text")]
    pub ie_key: Option<String>,
    #[serde(deserialize_with = "text")]
    pub webpage_url: Option<String>,
    #[serde(deserialize_with = "number")]
    pub duration: Option<f64>,
    #[serde(deserialize_with = "flag")]
    pub is_live: Option<bool>,
    /// `is_live`、`was_live`、`is_upcoming`、`not_live`、`post_live`
    #[serde(deserialize_with = "text")]
    pub live_status: Option<String>,
    #[serde(deserialize_with = "text")]
    pub uploader: Option<String>,
    #[serde(deserialize_with = "text")]
    pub thumbnail: Option<String>,
    /// 網址指定的開始時間（`&t=90`）
    #[serde(deserialize_with = "number")]
    pub start_time: Option<f64>,
    /// 網址本身就是媒體檔（yt-dlp 的 generic 擷取器直接給回原網址）
    #[serde(deserialize_with = "flag")]
    pub direct: Option<bool>,
    #[serde(deserialize_with = "list")]
    pub formats: Vec<Format>,
    /// `-f` 選到的格式（影像、聲音分開時兩個）
    #[serde(deserialize_with = "list")]
    pub requested_formats: Vec<Format>,
    /// 語言 → 字幕（`--write-subs` 選到的）
    #[serde(deserialize_with = "map")]
    pub requested_subtitles: BTreeMap<String, Subtitle>,
    #[serde(deserialize_with = "list")]
    pub chapters: Vec<Chapter>,
    /// 播放清單的項目
    #[serde(deserialize_with = "list")]
    pub entries: Vec<Info>,
    /// 只有一個格式時，那個格式的欄位直接放在最外層（`url`、`protocol`…）
    #[serde(flatten)]
    pub format: Format,
}

/// 一個格式（一個畫質的影像、一種聲音，或兩者合在一起）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Format {
    #[serde(deserialize_with = "text")]
    pub format_id: Option<String>,
    #[serde(deserialize_with = "text")]
    pub url: Option<String>,
    /// `https`、`m3u8_native`、`http_dash_segments`…
    #[serde(deserialize_with = "text")]
    pub protocol: Option<String>,
    /// `none` = 沒有影像；沒有這個欄位 = 不知道
    #[serde(deserialize_with = "text")]
    pub vcodec: Option<String>,
    #[serde(deserialize_with = "text")]
    pub acodec: Option<String>,
    #[serde(deserialize_with = "number")]
    pub width: Option<f64>,
    #[serde(deserialize_with = "number")]
    pub height: Option<f64>,
    #[serde(deserialize_with = "number")]
    pub fps: Option<f64>,
    /// 總位元率（kbit/s）
    #[serde(deserialize_with = "number")]
    pub tbr: Option<f64>,
    #[serde(deserialize_with = "number")]
    pub vbr: Option<f64>,
    #[serde(deserialize_with = "number")]
    pub abr: Option<f64>,
    #[serde(deserialize_with = "text")]
    pub ext: Option<String>,
    #[serde(deserialize_with = "text")]
    pub format_note: Option<String>,
    #[serde(deserialize_with = "list")]
    pub fragments: Vec<Fragment>,
    #[serde(deserialize_with = "text")]
    pub fragment_base_url: Option<String>,
    #[serde(deserialize_with = "text")]
    pub manifest_url: Option<String>,
    /// 播放時要送的標頭（User-Agent、Referer、Cookie…）；只留文字的值
    #[serde(deserialize_with = "headers")]
    pub http_headers: BTreeMap<String, String>,
    /// Cookie（一行，`名稱=值; Domain=…; Path=/; Secure; Expires=…; 名稱=值; …`）
    #[serde(deserialize_with = "text")]
    pub cookies: Option<String>,
    #[serde(deserialize_with = "object")]
    pub downloader_options: DownloaderOptions,
    #[serde(deserialize_with = "flag")]
    pub has_drm: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DownloaderOptions {
    /// 一次要求多少位元組（YouTube 對沒有分段的要求限速）
    #[serde(deserialize_with = "number")]
    pub http_chunk_size: Option<f64>,
}

/// DASH 的一段
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Fragment {
    #[serde(deserialize_with = "text")]
    pub url: Option<String>,
    /// 相對於 `fragment_base_url` 的路徑
    #[serde(deserialize_with = "text")]
    pub path: Option<String>,
    #[serde(deserialize_with = "number")]
    pub duration: Option<f64>,
}

/// 一個字幕
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Subtitle {
    /// `vtt`、`srt`、`ass`、`json3`…
    #[serde(deserialize_with = "text")]
    pub ext: Option<String>,
    #[serde(deserialize_with = "text")]
    pub url: Option<String>,
    /// 字幕內容本身（有些網站直接給內容）
    #[serde(deserialize_with = "text")]
    pub data: Option<String>,
    #[serde(deserialize_with = "text")]
    pub name: Option<String>,
}

/// 一個章節
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Chapter {
    #[serde(deserialize_with = "number")]
    pub start_time: Option<f64>,
    #[serde(deserialize_with = "text")]
    pub title: Option<String>,
}

impl Info {
    /// 直播中（不是還沒開始、不是已經結束的直播）
    pub fn live(&self) -> bool {
        self.is_live == Some(true) || self.live_status.as_deref() == Some("is_live")
    }

    /// 播放清單（不是單一影片）
    pub fn is_playlist(&self) -> bool {
        matches!(self.kind.as_deref(), Some("playlist" | "multi_video"))
    }

    /// 網站的擷取器名稱（續播的代號用；`extractor_key` 是固定的寫法，例如 `Youtube`）
    pub fn extractor_name(&self) -> Option<&str> {
        self.extractor_key
            .as_deref()
            .or(self.extractor.as_deref())
            .or(self.ie_key.as_deref())
    }
}

impl Format {
    /// 有影像（`vcodec` 不是 `none`；沒寫的當成有，例如只有網址的單一格式）
    pub fn has_video(&self) -> bool {
        self.vcodec.as_deref().is_none_or(|c| c != "none")
    }

    /// 有聲音
    pub fn has_audio(&self) -> bool {
        self.acodec.as_deref().is_none_or(|c| c != "none")
    }

    /// 確定只有影像（兩個欄位都寫了）
    pub fn video_only(&self) -> bool {
        self.vcodec.as_deref().is_some_and(|c| c != "none") && self.acodec.as_deref() == Some("none")
    }

    /// 確定只有聲音
    pub fn audio_only(&self) -> bool {
        self.vcodec.as_deref() == Some("none") && self.acodec.as_deref().is_some_and(|c| c != "none")
    }
}

/// yt-dlp 的標準輸出 → 資料。設定檔加了 `--print` 之類的選項時，JSON 前後可能多了別的行：
/// 整份讀不懂時，再試最後一行以 `{` 開頭的
pub fn parse(stdout: &[u8]) -> Result<Info, YtdlError> {
    let text = String::from_utf8_lossy(stdout);
    let text = text.trim();
    if text.is_empty() {
        return Err(YtdlError::NoResponse);
    }
    let parse_one = |s: &str| -> Option<Info> {
        let v: Value = serde_json::from_str(s).ok()?;
        v.is_object().then(|| serde_json::from_value(v).ok()).flatten()
    };
    parse_one(text)
        .or_else(|| {
            text.lines()
                .rev()
                .map(str::trim)
                .find(|l| l.starts_with('{'))
                .and_then(parse_one)
        })
        .ok_or(YtdlError::NotJson)
}

// ───────────── 寬鬆的讀法 ─────────────

/// 文字；數字轉成文字（有些網站的代號是數字），其他（null、陣列…）當成沒有
fn text<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// 數字；寫成文字的數字也接受。不是有限的數字時當成沒有
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let n = match Value::deserialize(d)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    Ok(n.filter(|n: &f64| n.is_finite()))
}

/// 是非；0 / 1 也接受
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => Some(b),
        Value::Number(n) => n.as_f64().map(|n| n != 0.0),
        _ => None,
    })
}

/// 清單；`null` 是空的，看不懂的項目略過
fn list<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Vec<T>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
        _ => Vec::new(),
    })
}

/// 鍵值；`null` 是空的，看不懂的值略過
fn map<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<BTreeMap<String, T>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Object(items) => items
            .into_iter()
            .filter_map(|(k, v)| Some((k, serde_json::from_value(v).ok()?)))
            .collect(),
        _ => BTreeMap::new(),
    })
}

/// 物件；`null` 或讀不懂時是預設值
fn object<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(d: D) -> Result<T, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).unwrap_or_default())
}

/// HTTP 標頭：只留文字的值
fn headers<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Object(items) => items
            .into_iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) => Some((k, s)),
                _ => None,
            })
            .collect(),
        _ => BTreeMap::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulls_and_odd_types_are_tolerated() {
        let info = parse(
            br#"{"id": 12345, "title": "T", "duration": "61.5", "is_live": null, "chapters": null,
                 "requested_subtitles": null, "formats": [{"format_id": "18", "height": 360, "fps": null},
                 "garbage", {"format_id": 7, "has_drm": 1}], "http_headers": {"User-Agent": "UA", "X": 3},
                 "downloader_options": {"http_chunk_size": 10485760}, "url": "https://h/v.mp4", "protocol": "https",
                 "entries": null, "unknown": {"deep": [1, 2]}}"#,
        )
        .unwrap();
        assert_eq!(info.id.as_deref(), Some("12345"));
        assert_eq!(info.duration, Some(61.5));
        assert_eq!(info.is_live, None);
        assert!(info.chapters.is_empty() && info.requested_subtitles.is_empty() && info.entries.is_empty());
        assert_eq!(info.formats.len(), 2);
        assert_eq!(info.formats[0].height, Some(360.0));
        assert_eq!(info.formats[1].format_id.as_deref(), Some("7"));
        assert_eq!(info.formats[1].has_drm, Some(true));
        // 最外層的單一格式
        assert_eq!(info.format.url.as_deref(), Some("https://h/v.mp4"));
        assert_eq!(info.format.protocol.as_deref(), Some("https"));
        assert_eq!(info.format.http_headers.len(), 1);
        assert_eq!(info.format.downloader_options.http_chunk_size, Some(10485760.0));
    }

    #[test]
    fn null_downloader_options_do_not_fail_the_whole_format() {
        let info = parse(br#"{"formats": [{"format_id": "a", "downloader_options": null}]}"#).unwrap();
        assert_eq!(info.formats.len(), 1);
        assert_eq!(info.formats[0].downloader_options.http_chunk_size, None);
    }

    #[test]
    fn extra_lines_around_the_json() {
        let info = parse(b"some printed line\n{\"title\": \"x\"}\n").unwrap();
        assert_eq!(info.title.as_deref(), Some("x"));
        assert!(matches!(parse(b"hello\nworld\n"), Err(YtdlError::NotJson)));
        assert!(matches!(parse(b"[1, 2]"), Err(YtdlError::NotJson)));
        assert!(matches!(parse(b"  \n"), Err(YtdlError::NoResponse)));
    }

    #[test]
    fn video_and_audio_flags() {
        let f = |v: Option<&str>, a: Option<&str>| Format {
            vcodec: v.map(Into::into),
            acodec: a.map(Into::into),
            ..Default::default()
        };
        assert!(f(Some("avc1"), Some("none")).video_only());
        assert!(f(Some("none"), Some("opus")).audio_only());
        assert!(!f(None, None).video_only() && f(None, None).has_video() && f(None, None).has_audio());
        assert!(!f(Some("none"), Some("mp4a")).has_video());
    }
}
