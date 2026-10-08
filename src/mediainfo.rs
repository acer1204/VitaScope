//! 媒體資訊（Ctrl+F1 / Ctrl+I，比照 PotPlayer 的「播放資訊」）：容器、影像、音訊、字幕的格式，
//! 以及播放中的位元率、掉格、影音同步。
//!
//! 全部在面板打開時才讀（每秒一次），不用 `observe`：位元率、影音差這類屬性 mpv 每一格都會重算，
//! 觀察的話面板關著也會一直收到通知。
//!
//! 版本差異（見 ROADMAP「學到的事」）：0.37 沒有 `codec-desc`、`codec-profile`（0.38 起）、
//! `decoder`（0.39 起）；零複製硬體解碼時 `pixelformat` 是硬體表面的名稱（`cuda`、`d3d11`），
//! 真正的格式在 `hw-pixelformat`；非有限的浮點數 mpv 會寫成字串（例如 `"-nan(ind)"`）。

use crate::player::Player;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

/// 一條軌道（`current-tracks/video` 之類，JSON）
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct TrackInfo {
    pub codec: Option<String>,
    pub codec_desc: Option<String>,
    pub codec_profile: Option<String>,
    pub decoder_desc: Option<String>,
    #[serde(deserialize_with = "lenient_i64")]
    pub demux_bitrate: Option<i64>,
    pub dolby_vision_profile: Option<i64>,
    pub external: bool,
    pub albumart: bool,
    pub image: bool,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

impl TrackInfo {
    /// 編碼的完整名稱：0.38 起有 `codec-desc`，舊版用解碼器的說明
    pub fn long_name(&self) -> Option<&str> {
        self.codec_desc.as_deref().or(self.decoder_desc.as_deref())
    }
}

/// `video-params`
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct VideoParams {
    pub pixelformat: Option<String>,
    pub hw_pixelformat: Option<String>,
    pub w: Option<i64>,
    pub h: Option<i64>,
    #[serde(deserialize_with = "lenient_f64")]
    pub aspect: Option<f64>,
    #[serde(deserialize_with = "lenient_f64")]
    pub par: Option<f64>,
    pub colormatrix: Option<String>,
    pub colorlevels: Option<String>,
    pub primaries: Option<String>,
    pub gamma: Option<String>,
    pub rotate: Option<i64>,
    #[serde(deserialize_with = "lenient_f64")]
    pub max_cll: Option<f64>,
    #[serde(deserialize_with = "lenient_f64")]
    pub max_fall: Option<f64>,
    #[serde(deserialize_with = "lenient_f64")]
    pub scene_max_r: Option<f64>,
}

impl VideoParams {
    /// 實際的像素格式（零複製硬體解碼時 `pixelformat` 只是硬體表面的名稱）
    pub fn real_pixelformat(&self) -> Option<&str> {
        self.hw_pixelformat.as_deref().or(self.pixelformat.as_deref())
    }
}

/// `audio-params`
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct AudioParams {
    pub samplerate: Option<i64>,
    pub channel_count: Option<i64>,
    pub hr_channels: Option<String>,
}

/// 檔案層級、換軌道時才會變的資訊
#[derive(Debug, Clone, Default)]
pub struct MediaInfo {
    pub file_name: String,
    pub file_format: Option<String>,
    pub file_size: Option<i64>,
    pub duration: Option<f64>,
    pub chapters: Option<i64>,
    pub video: Option<TrackInfo>,
    pub audio: Option<TrackInfo>,
    pub sub: Option<TrackInfo>,
    pub sub2: Option<TrackInfo>,
    pub vparams: Option<VideoParams>,
    pub aparams: Option<AudioParams>,
    pub container_fps: Option<f64>,
    pub hwdec: Option<String>,
    pub ao: Option<String>,
    /// 音訊裝置的名稱（`audio-device` 是 `auto` 或 `wasapi/{id}`，要查 `audio-device-list`）
    pub audio_device: Option<String>,
}

/// 播放中一直在變的數字
#[derive(Debug, Clone, Default)]
pub struct LiveStats {
    pub video_bitrate: Option<i64>,
    pub audio_bitrate: Option<i64>,
    pub vf_fps: Option<f64>,
    pub vo_drops: Option<i64>,
    pub decoder_drops: Option<i64>,
    pub avsync: Option<f64>,
    /// 顯示同步（流暢播放）。mpv 的 display-sync-active 播放中不會更新，用播放器狀態裡的
    pub display_sync_active: bool,
    /// mpv 用來同步的更新率（display-fps-override 或查到的）、mpv 量到的更新率
    pub display_fps: Option<f64>,
    pub estimated_display_fps: Option<f64>,
    /// 每格影像顯示幾次螢幕更新、更新間隔的抖動（相對值）
    pub vsync_ratio: Option<f64>,
    pub vsync_jitter: Option<f64>,
    /// 影片速度的修正倍數（1.001 = 快 0.1%）
    pub video_speed_correction: Option<f64>,
    /// 顯示時間跟預定的差太多的影格、晚了的影格
    pub mistimed_frame_count: Option<i64>,
    pub vo_delayed_frame_count: Option<i64>,
}

fn lenient_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_f64().filter(|v| v.is_finite()))
}

fn lenient_i64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_i64())
}

fn json<T: serde::de::DeserializeOwned>(player: &Player, name: &str) -> Option<T> {
    serde_json::from_str(&player.get_string(name).ok()?).ok()
}

/// 讀檔案層級的資訊（約 0.3 毫秒；`audio_device` 第一次查裝置清單比較慢，另外快取）
pub fn read(player: &Player, audio_device: Option<String>) -> MediaInfo {
    let st = &player.state;
    MediaInfo {
        file_name: st
            .path
            .as_deref()
            .map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map_or_else(|| p.to_owned(), |n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default(),
        file_format: player.get_string("file-format").ok(),
        file_size: player.get_i64("file-size").ok(),
        duration: st.duration,
        chapters: player.get_i64("chapters").ok().filter(|n| *n > 0),
        video: json(player, "current-tracks/video"),
        audio: json(player, "current-tracks/audio"),
        sub: json(player, "current-tracks/sub"),
        sub2: json(player, "current-tracks/sub2"),
        vparams: json(player, "video-params"),
        aparams: json(player, "audio-params"),
        container_fps: player
            .get_f64("container-fps")
            .ok()
            .filter(|f| f.is_finite() && *f > 0.0),
        hwdec: st.hwdec.clone(),
        ao: player.get_string("current-ao").ok(),
        audio_device,
    }
}

/// 音訊裝置的名稱。查裝置清單第一次要十幾毫秒，面板打開時查一次就好
///（已經在觀察清單時直接用觀察到的，不再查）
pub fn audio_device_name(player: &Player) -> Option<String> {
    let current = player.get_string("audio-device").ok()?;
    if current == crate::sound::AUTO_DEVICE {
        return Some(crate::tr!("預設裝置", "Default device").to_owned());
    }
    let list = match &player.state.audio_devices {
        Some(list) => list.clone(),
        None => crate::sound::parse_devices(&player.get_string("audio-device-list").unwrap_or_default()),
    };
    Some(
        list.into_iter()
            .find(|d| d.name == current && !d.description.is_empty())
            .map_or(current, |d| d.description),
    )
}

pub fn read_live(player: &Player) -> LiveStats {
    let int = |n| player.get_i64(n).ok();
    let float = |n| player.get_f64(n).ok().filter(|v: &f64| v.is_finite());
    LiveStats {
        video_bitrate: int("video-bitrate"),
        audio_bitrate: int("audio-bitrate"),
        vf_fps: float("estimated-vf-fps"),
        vo_drops: int("frame-drop-count"),
        decoder_drops: int("decoder-frame-drop-count"),
        avsync: float("avsync"),
        display_sync_active: player.state.display_sync_active,
        display_fps: float("display-fps"),
        estimated_display_fps: float("estimated-display-fps"),
        vsync_ratio: float("vsync-ratio"),
        vsync_jitter: float("vsync-jitter"),
        video_speed_correction: float("video-speed-correction"),
        mistimed_frame_count: int("mistimed-frame-count"),
        vo_delayed_frame_count: int("vo-delayed-frame-count"),
    }
}

/// 媒體資訊面板「播放流暢度」裡 mpv 回報的顯示同步數字，例如
/// 「顯示同步：開 · 120.000 Hz（量到 119.998 Hz）」「每格 5.000 次更新 · 影片快 0.10% · 抖動 0.034」「錯時 0 · 延遲 0」
pub fn sync_lines(live: &LiveStats) -> Vec<String> {
    let hz = |v: Option<f64>| v.filter(|v| *v > 0.0).map(|v| format!("{v:.3} Hz"));
    let mut first = if live.display_sync_active {
        crate::tr!("顯示同步：開", "Display sync: on").to_owned()
    } else {
        crate::tr!("顯示同步：關", "Display sync: off").to_owned()
    };
    if let Some(fps) = hz(live.display_fps) {
        first += &format!(" · {fps}");
    }
    if let Some(est) = hz(live.estimated_display_fps) {
        first += &crate::tf!("（量到 {est}）", " (measured {est})");
    }
    let mut lines = vec![first];
    if !live.display_sync_active {
        return lines;
    }
    let mut timing = Vec::new();
    if let Some(r) = live.vsync_ratio {
        timing.push(crate::tf!("每格 {r:.3} 次更新", "{r:.3} refreshes per frame"));
    }
    if let Some(c) = live.video_speed_correction {
        let pct = (c - 1.0) * 100.0;
        timing.push(crate::tf!("影片速度 {pct:+.2}%", "video speed {pct:+.2}%"));
    }
    if let Some(j) = live.vsync_jitter {
        timing.push(crate::tf!("抖動 {j:.3}", "jitter {j:.3}"));
    }
    if !timing.is_empty() {
        lines.push(timing.join(" · "));
    }
    let n = |v: Option<i64>| v.map_or("-".to_owned(), |v| v.to_string());
    lines.push(crate::tf!(
        "錯時 {} · 延遲 {}",
        "Mistimed {} · delayed {}",
        n(live.mistimed_frame_count),
        n(live.vo_delayed_frame_count)
    ));
    lines
}

// ───────────── 顯示 ─────────────

/// 像素格式的位元深度（`p010` → 10、`yuv420p10` → 10、`nv12` → 8）
pub fn bit_depth(pixfmt: &str) -> u32 {
    let f = pixfmt.to_ascii_lowercase();
    let known: &[(&[&str], u32)] = &[
        (
            &[
                "nv12", "nv21", "nv16", "nv24", "yuyv422", "uyvy422", "rgb24", "bgr0", "rgb0", "bgra", "rgba",
            ],
            8,
        ),
        (&["p010", "p210", "p410", "y210", "xv30", "x2rgb10", "x2bgr10"], 10),
        (&["p012", "y212", "xv36"], 12),
        (&["p016", "rgb48", "rgba64", "y216"], 16),
    ];
    for (names, depth) in known {
        if names.iter().any(|n| f.starts_with(n)) {
            return *depth;
        }
    }
    // yuv420p10le、yuv444p12msb、gbrp10…：p 後面的數字
    let core = f.trim_end_matches("msb").trim_end_matches("le").trim_end_matches("be");
    let digits: String = core.chars().rev().take_while(char::is_ascii_digit).collect();
    let digits: String = digits.chars().rev().collect();
    if !digits.is_empty() && core[..core.len() - digits.len()].ends_with('p') {
        return digits.parse().unwrap_or(8);
    }
    8
}

/// 色度取樣（4:2:0 之類）；看不出來是 None
pub fn chroma(pixfmt: &str) -> Option<&'static str> {
    let f = pixfmt.to_ascii_lowercase();
    if f.contains("420") || f.starts_with("nv12") || f.starts_with("nv21") || f.starts_with("p01") {
        Some("4:2:0")
    } else if f.contains("422") || f.starts_with("nv16") || f.starts_with("p21") || f.starts_with("y21") {
        Some("4:2:2")
    } else if f.contains("444")
        || f.starts_with("nv24")
        || f.starts_with("p41")
        || f.starts_with("xv3")
        || f.starts_with("gbrp")
    {
        Some("4:4:4")
    } else {
        None
    }
}

/// 動態範圍：HDR10 / HDR10+ / Dolby Vision / HLG / SDR
pub fn dynamic_range(vp: &VideoParams, video: Option<&TrackInfo>) -> String {
    let wide = vp.primaries.as_deref() == Some("bt.2020");
    match vp.gamma.as_deref() {
        Some("pq") => {
            let dolby = video.is_some_and(|v| v.dolby_vision_profile.is_some())
                || vp.colormatrix.as_deref() == Some("dolbyvision");
            if dolby {
                "Dolby Vision".to_owned()
            } else if vp.scene_max_r.is_some() {
                "HDR10+".to_owned()
            } else {
                "HDR10".to_owned()
            }
        }
        Some("hlg") => "HLG".to_owned(),
        _ if wide => crate::tr!("SDR（廣色域）", "SDR (wide gamut)").to_owned(),
        _ => "SDR".to_owned(),
    }
}

/// 位元率：1810000 → 「1.81 Mbps」、640000 → 「640 kbps」
pub fn fmt_bitrate(bps: i64) -> String {
    if bps >= 1_000_000 {
        format!("{:.2} Mbps", bps as f64 / 1e6)
    } else {
        format!("{} kbps", (bps as f64 / 1e3).round())
    }
}

/// 檔案大小：1.42 GB、153 KB
pub fn fmt_size(bytes: i64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

/// 長寬比的名稱：16:9、4:3…；不是常見的比例就寫小數
pub fn aspect_name(aspect: f64) -> String {
    const NAMES: [(&str, f64); 8] = [
        ("16:9", 16.0 / 9.0),
        ("4:3", 4.0 / 3.0),
        ("16:10", 1.6),
        ("21:9", 64.0 / 27.0),
        ("1.85:1", 1.85),
        ("2.35:1", 2.35),
        ("1:1", 1.0),
        ("9:16", 9.0 / 16.0),
    ];
    NAMES
        .iter()
        .find(|(_, v)| (aspect - v).abs() < 0.02)
        .map_or_else(|| format!("{aspect:.2}:1"), |(n, _)| (*n).to_owned())
}

/// 容器名稱：mpv 的 `file-format` 是 FFmpeg 的格式清單（`mov,mp4,m4a,3gp,3g2,mj2`）
pub fn container_name(format: &str) -> String {
    let first = format.split(',').next().unwrap_or(format);
    match first {
        "mov" => "MP4 / MOV".to_owned(),
        "mkv" | "matroska" => crate::tr!("Matroska（MKV / WebM）", "Matroska (MKV / WebM)").to_owned(),
        "mpegts" => "MPEG-TS".to_owned(),
        "mpeg" => "MPEG-PS".to_owned(),
        "avi" => "AVI".to_owned(),
        "flv" => "FLV".to_owned(),
        "asf" => crate::tr!("ASF（WMV）", "ASF (WMV)").to_owned(),
        "ogg" => "Ogg".to_owned(),
        "mp3" => "MP3".to_owned(),
        "flac" => "FLAC".to_owned(),
        "wav" => "WAV".to_owned(),
        other => other.to_uppercase(),
    }
}

/// 硬體解碼的說明
pub fn hwdec_label(hwdec: Option<&str>) -> String {
    match hwdec {
        None | Some("") | Some("no") => crate::tr!("軟體解碼", "Software decoding").to_owned(),
        Some(h) => {
            let (api, copy) = match h.strip_suffix("-copy") {
                Some(api) => (api, true),
                None => (h, false),
            };
            let api = match api {
                "d3d11va" => "D3D11VA",
                "dxva2" => "DXVA2",
                "nvdec" | "cuda" => "NVDEC",
                "vaapi" => "VA-API",
                "vdpau" => "VDPAU",
                "videotoolbox" => "VideoToolbox",
                "vulkan" => "Vulkan",
                other => other,
            };
            if copy {
                crate::tf!(
                    "硬體解碼（{api}，複製回記憶體）",
                    "Hardware decoding ({api}, copy-back)"
                )
            } else {
                crate::tf!("硬體解碼（{api}）", "Hardware decoding ({api})")
            }
        }
    }
}

/// 聲道：stereo → 立體聲、5.1(side) → 5.1
pub fn channels_label(hr: Option<&str>, count: Option<i64>) -> Option<String> {
    let label = match hr {
        Some("mono") => crate::tr!("單聲道", "Mono").to_owned(),
        Some("stereo") => crate::tr!("立體聲", "Stereo").to_owned(),
        Some(h) => h.split('(').next().unwrap_or(h).to_owned(),
        None => crate::tf!("{} 聲道", "{} channels", count?),
    };
    Some(label)
}

/// 編碼的顯示名稱：「H.264 / AVC」+ profile
fn codec_line(t: &TrackInfo) -> String {
    let short = t.codec.as_deref().unwrap_or("?");
    let mut s = match t.long_name() {
        // 「H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10」這種很長的，只留前兩段
        Some(long) => long.split(" / ").take(2).collect::<Vec<_>>().join(" / "),
        None => short.to_uppercase(),
    };
    if let Some(p) = t.codec_profile.as_deref().filter(|p| !p.is_empty() && *p != "unknown") {
        s.push_str(&format!(" {p}"));
    }
    s
}

fn lang_label(lang: Option<&str>) -> Option<String> {
    let lang = lang?;
    Some(
        match lang.to_ascii_lowercase().as_str() {
            "chi" | "zho" | "zh" => crate::tr!("中文", "Chinese"),
            "zh-tw" | "zh-hant" | "cht" => crate::tr!("繁體中文", "Traditional Chinese"),
            "zh-cn" | "zh-hans" | "chs" => crate::tr!("簡體中文", "Simplified Chinese"),
            "eng" | "en" => crate::tr!("英文", "English"),
            "jpn" | "ja" => crate::tr!("日文", "Japanese"),
            "kor" | "ko" => crate::tr!("韓文", "Korean"),
            other => return Some(other.to_owned()),
        }
        .to_owned(),
    )
}

/// 面板上的一段：標題（檔案、影像…）+ 幾行
pub type Section = (&'static str, Vec<String>);

/// 整理成面板上的文字
pub fn sections(info: &MediaInfo, live: &LiveStats) -> Vec<Section> {
    let mut out = Vec::new();

    // 檔案
    let mut file = vec![info.file_name.clone()];
    let mut facts = Vec::new();
    if let Some(f) = &info.file_format {
        facts.push(container_name(f));
    }
    if let Some(size) = info.file_size.filter(|s| *s > 0) {
        facts.push(fmt_size(size));
    }
    if let Some(d) = info.duration.filter(|d| *d > 0.0) {
        facts.push(crate::app::fmt_time(d));
        if let Some(size) = info.file_size.filter(|s| *s > 0) {
            facts.push(crate::tf!(
                "總位元率 {}",
                "Overall bitrate {}",
                fmt_bitrate((size as f64 * 8.0 / d) as i64)
            ));
        }
    }
    if let Some(n) = info.chapters {
        facts.push(crate::tf!("{n} 個章節", "{n} chapters"));
    }
    if !facts.is_empty() {
        file.push(facts.join(" · "));
    }
    out.push((crate::tr!("檔案", "File"), file));

    // 影像（專輯封面不算）
    if let Some(v) = info.video.as_ref().filter(|v| !v.albumart && !v.image) {
        let mut lines = vec![format!("{} · {}", codec_line(v), hwdec_label(info.hwdec.as_deref()))];
        if let Some(vp) = &info.vparams {
            let mut size = Vec::new();
            if let (Some(w), Some(h)) = (vp.w, vp.h) {
                let aspect = vp
                    .aspect
                    .map(aspect_name)
                    .map(|a| crate::tf!("（{a}）", " ({a})"))
                    .unwrap_or_default();
                size.push(format!("{w}×{h}{aspect}"));
            }
            if let Some(fps) = info.container_fps {
                let mut s = format!("{} fps", fmt_fps(fps));
                if let Some(real) = live.vf_fps.filter(|r| (r - fps).abs() / fps > 0.02) {
                    s.push_str(&crate::tf!("（實測 {}）", " (measured {})", fmt_fps(real)));
                }
                size.push(s);
            }
            if let Some(par) = vp.par.filter(|p| (p - 1.0).abs() > 0.01) {
                size.push(crate::tf!("像素比例 {par:.3}", "Pixel aspect {par:.3}"));
            }
            if let Some(r) = vp.rotate.filter(|r| *r != 0) {
                size.push(crate::tf!("旋轉 {r}°", "Rotated {r}°"));
            }
            if !size.is_empty() {
                lines.push(size.join(" · "));
            }
            let mut color = Vec::new();
            if let Some(pf) = vp.real_pixelformat() {
                let mut s = format!("{pf} · {} bit", bit_depth(pf));
                if let Some(c) = chroma(pf) {
                    s.push_str(&format!(" {c}"));
                }
                color.push(s);
            }
            let range = dynamic_range(vp, Some(v));
            let space = [vp.primaries.as_deref(), vp.gamma.as_deref()]
                .into_iter()
                .flatten()
                .map(|s| s.to_uppercase())
                .collect::<Vec<_>>()
                .join(" / ");
            // HLG 的傳輸特性就叫 HLG，不用寫兩次
            let repeated = space.split(" / ").any(|part| part == range);
            color.push(match () {
                _ if space.is_empty() => range,
                _ if repeated => space,
                _ => format!("{space} · {range}"),
            });
            if let (Some(cll), Some(fall)) = (vp.max_cll.filter(|v| *v > 0.0), vp.max_fall.filter(|v| *v > 0.0)) {
                color.push(format!("MaxCLL {cll:.0} / MaxFALL {fall:.0} nits"));
            }
            lines.push(color.join(" · "));
        }
        let bitrate = live
            .video_bitrate
            .filter(|b| *b > 0)
            .map(fmt_bitrate)
            .or_else(|| {
                v.demux_bitrate
                    .filter(|b| *b > 0)
                    .map(|b| crate::tf!("{}（標示）", "{} (declared)", fmt_bitrate(b)))
            })
            .unwrap_or_else(|| crate::tr!("計算中…", "measuring…").to_owned());
        lines.push(crate::tf!("位元率 {bitrate}", "Bitrate {bitrate}"));
        out.push((crate::tr!("影像", "Video"), lines));
    }

    // 音訊
    if let Some(a) = &info.audio {
        let mut facts = vec![codec_line(a)];
        if let Some(ap) = &info.aparams {
            if let Some(rate) = ap.samplerate {
                facts.push(format!("{} kHz", fmt_khz(rate)));
            }
            if let Some(ch) = channels_label(ap.hr_channels.as_deref(), ap.channel_count) {
                facts.push(ch);
            }
        }
        if let Some(b) = live
            .audio_bitrate
            .filter(|b| *b > 0)
            .or(a.demux_bitrate.filter(|b| *b > 0))
        {
            facts.push(fmt_bitrate(b));
        }
        if let Some(l) = lang_label(a.lang.as_deref()) {
            facts.push(l);
        }
        if a.external {
            facts.push(crate::tr!("外掛", "external").to_owned());
        }
        let mut lines = vec![facts.join(" · ")];
        let output = [
            info.ao.as_deref().map(|ao| ao.to_uppercase()),
            info.audio_device.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        if !output.is_empty() {
            lines.push(crate::tf!("輸出 {}", "Output {}", output.join(" · ")));
        }
        out.push((crate::tr!("音訊", "Audio"), lines));
    }

    // 字幕
    let sub_line = |s: &TrackInfo| {
        let mut facts = vec![
            s.long_name()
                .map_or_else(|| s.codec.clone().unwrap_or_default().to_uppercase(), |l| l.to_owned()),
        ];
        if let Some(t) = s.title.as_deref().filter(|t| !t.is_empty()) {
            facts.push(t.to_owned());
        } else if let Some(l) = lang_label(s.lang.as_deref()) {
            facts.push(l);
        }
        facts.push(
            if s.external {
                crate::tr!("外掛", "external")
            } else {
                crate::tr!("內嵌", "embedded")
            }
            .to_owned(),
        );
        facts.join(" · ")
    };
    let mut subs = Vec::new();
    if let Some(s) = &info.sub {
        subs.push(sub_line(s));
    }
    if let Some(s) = &info.sub2 {
        subs.push(crate::tf!("第二字幕 {}", "Secondary {}", sub_line(s)));
    }
    if !subs.is_empty() {
        out.push((crate::tr!("字幕", "Subtitles"), subs));
    }

    // 播放狀態
    let mut stats = Vec::new();
    if info.video.as_ref().is_some_and(|v| !v.albumart && !v.image) {
        let drops = [live.vo_drops, live.decoder_drops];
        if drops.iter().any(Option::is_some) {
            let n = |v: Option<i64>| v.map_or("-".to_owned(), |v| v.to_string());
            stats.push(crate::tf!(
                "掉格 {}（畫面）/ {}（解碼）",
                "Dropped frames {} (output) / {} (decoder)",
                n(drops[0]),
                n(drops[1])
            ));
        }
    }
    if let Some(av) = live.avsync {
        stats.push(crate::tf!("影音差 {:+.0} ms", "A/V sync {:+.0} ms", av * 1000.0));
    }
    if !stats.is_empty() {
        out.push((crate::tr!("狀態", "Status"), vec![stats.join(" · ")]));
    }
    out
}

/// 複製到剪貼簿用的純文字
pub fn to_text(sections: &[Section]) -> String {
    let mut s = String::new();
    for (title, lines) in sections {
        for (i, line) in lines.iter().enumerate() {
            let head = if i == 0 { *title } else { "" };
            s.push_str(&format!("{head:\u{3000}<2}　{line}\n"));
        }
    }
    s
}

fn fmt_fps(fps: f64) -> String {
    let s = format!("{fps:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn fmt_khz(rate: i64) -> String {
    let s = format!("{:.1}", rate as f64 / 1000.0);
    s.trim_end_matches(".0").to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_depth_from_pixel_formats() {
        assert_eq!(bit_depth("nv12"), 8);
        assert_eq!(bit_depth("yuv420p"), 8);
        assert_eq!(bit_depth("p010"), 10);
        assert_eq!(bit_depth("yuv420p10"), 10);
        assert_eq!(bit_depth("yuv420p10le"), 10);
        assert_eq!(bit_depth("yuv444p12msb"), 12);
        assert_eq!(bit_depth("gbrp10"), 10);
        assert_eq!(bit_depth("rgb48"), 16);
        assert_eq!(bit_depth("yuvj420p"), 8);
    }

    #[test]
    fn chroma_subsampling() {
        assert_eq!(chroma("yuv420p10"), Some("4:2:0"));
        assert_eq!(chroma("nv12"), Some("4:2:0"));
        assert_eq!(chroma("p010"), Some("4:2:0"));
        assert_eq!(chroma("yuv422p"), Some("4:2:2"));
        assert_eq!(chroma("yuv444p12msb"), Some("4:4:4"));
        assert_eq!(chroma("bgra"), None);
    }

    #[test]
    fn dynamic_range_labels() {
        let pq = VideoParams {
            gamma: Some("pq".into()),
            primaries: Some("bt.2020".into()),
            ..Default::default()
        };
        assert_eq!(dynamic_range(&pq, None), "HDR10");
        let plus = VideoParams {
            scene_max_r: Some(100.0),
            ..pq.clone()
        };
        assert_eq!(dynamic_range(&plus, None), "HDR10+");
        let dv = TrackInfo {
            dolby_vision_profile: Some(8),
            ..Default::default()
        };
        assert_eq!(dynamic_range(&pq, Some(&dv)), "Dolby Vision");
        let hlg = VideoParams {
            gamma: Some("hlg".into()),
            ..Default::default()
        };
        assert_eq!(dynamic_range(&hlg, None), "HLG");
        let wide = VideoParams {
            primaries: Some("bt.2020".into()),
            gamma: Some("bt.1886".into()),
            ..Default::default()
        };
        assert_eq!(dynamic_range(&wide, None), "SDR（廣色域）");
        assert_eq!(dynamic_range(&VideoParams::default(), None), "SDR");
    }

    #[test]
    fn number_formats() {
        assert_eq!(fmt_bitrate(1_810_000), "1.81 Mbps");
        assert_eq!(fmt_bitrate(640_000), "640 kbps");
        assert_eq!(fmt_size(1_420_000_000), "1.42 GB");
        assert_eq!(fmt_size(153_344), "153 KB");
        assert_eq!(aspect_name(1.7777), "16:9");
        assert_eq!(aspect_name(1.5), "1.50:1");
        assert_eq!(fmt_fps(23.976), "23.976");
        assert_eq!(fmt_fps(30.0), "30");
        assert_eq!(fmt_khz(48000), "48");
        assert_eq!(fmt_khz(44100), "44.1");
        assert_eq!(container_name("mov,mp4,m4a,3gp,3g2,mj2"), "MP4 / MOV");
        assert_eq!(hwdec_label(Some("d3d11va-copy")), "硬體解碼（D3D11VA，複製回記憶體）");
        assert_eq!(hwdec_label(Some("nvdec")), "硬體解碼（NVDEC）");
        assert_eq!(hwdec_label(Some("no")), "軟體解碼");
        assert_eq!(channels_label(Some("5.1(side)"), Some(6)).as_deref(), Some("5.1"));
        assert_eq!(channels_label(None, Some(2)).as_deref(), Some("2 聲道"));
    }

    #[test]
    fn non_finite_numbers_from_mpv_are_ignored() {
        let vp: VideoParams = serde_json::from_str(
            r#"{"w":720,"h":480,"par":"-nan(ind)","aspect":1.777,"pixelformat":"cuda","hw-pixelformat":"p010"}"#,
        )
        .unwrap();
        assert_eq!(vp.par, None);
        assert_eq!(vp.real_pixelformat(), Some("p010"));
    }

    #[test]
    fn display_sync_lines() {
        let off = LiveStats {
            estimated_display_fps: Some(119.9981),
            ..Default::default()
        };
        assert_eq!(sync_lines(&off), vec!["顯示同步：關（量到 119.998 Hz）".to_owned()]);
        let on = LiveStats {
            display_sync_active: true,
            display_fps: Some(120.0),
            estimated_display_fps: Some(119.9981),
            vsync_ratio: Some(5.0),
            vsync_jitter: Some(0.0342),
            video_speed_correction: Some(1.001),
            mistimed_frame_count: Some(0),
            vo_delayed_frame_count: Some(2),
            ..Default::default()
        };
        assert_eq!(
            sync_lines(&on),
            vec![
                "顯示同步：開 · 120.000 Hz（量到 119.998 Hz）".to_owned(),
                "每格 5.000 次更新 · 影片速度 +0.10% · 抖動 0.034".to_owned(),
                "錯時 0 · 延遲 2".to_owned(),
            ]
        );
        crate::i18n::set_lang(crate::i18n::Lang::En);
        let en = sync_lines(&on);
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
        assert_eq!(
            en,
            vec![
                "Display sync: on · 120.000 Hz (measured 119.998 Hz)".to_owned(),
                "5.000 refreshes per frame · video speed +0.10% · jitter 0.034".to_owned(),
                "Mistimed 0 · delayed 2".to_owned(),
            ]
        );
    }

    #[test]
    fn sections_for_a_typical_video() {
        let info = MediaInfo {
            file_name: "a.mkv".into(),
            file_format: Some("mkv".into()),
            file_size: Some(1_000_000),
            duration: Some(8.0),
            video: Some(TrackInfo {
                codec: Some("hevc".into()),
                codec_desc: Some("H.265 / HEVC (High Efficiency Video Coding)".into()),
                codec_profile: Some("Main 10".into()),
                ..Default::default()
            }),
            vparams: Some(VideoParams {
                w: Some(3840),
                h: Some(2160),
                aspect: Some(16.0 / 9.0),
                pixelformat: Some("yuv420p10".into()),
                primaries: Some("bt.2020".into()),
                gamma: Some("pq".into()),
                max_cll: Some(1000.0),
                max_fall: Some(400.0),
                ..Default::default()
            }),
            container_fps: Some(23.976),
            hwdec: Some("d3d11va".into()),
            ..Default::default()
        };
        let live = LiveStats {
            avsync: Some(0.001),
            ..Default::default()
        };
        let s = sections(&info, &live);
        let text = to_text(&s);
        assert!(text.contains("Matroska"), "{text}");
        assert!(text.contains("總位元率 1.00 Mbps"), "{text}");
        assert!(
            text.contains("H.265 / HEVC (High Efficiency Video Coding) Main 10"),
            "{text}"
        );
        assert!(text.contains("3840×2160（16:9） · 23.976 fps"), "{text}");
        assert!(text.contains("yuv420p10 · 10 bit 4:2:0"), "{text}");
        assert!(text.contains("BT.2020 / PQ · HDR10"), "{text}");
        assert!(text.contains("MaxCLL 1000 / MaxFALL 400 nits"), "{text}");
        assert!(text.contains("位元率 計算中…"), "{text}");
        assert!(text.contains("影音差 +1 ms"), "{text}");
        assert!(!text.contains("音訊"), "沒有音軌就不列：{text}");
    }
}
