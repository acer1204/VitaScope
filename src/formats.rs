//! 支援的副檔名（對應 ROADMAP 第 2 節的格式分級）。
//! 開檔對話框的篩選、拖放判斷，以及 L2 的檔案關聯都用這份清單。

/// 影片：常見 + 通用
pub const VIDEO: &[&str] = &[
    // 常見
    "mp4", "m4v", "mkv", "webm", "mov", "avi", //
    // 通用
    "ts", "m2ts", "mts", "mpg", "mpeg", "vob", "wmv", "asf", "flv", "f4v", "3gp", "3g2", "ogv",
];

/// 影片：罕見（能播，但不預設關聯）
pub const VIDEO_RARE: &[&str] = &[
    "rm", "rmvb", "mxf", "dv", "ogm", "nut", "ivf", "y4m", "divx", "wtv", "dvr-ms",
];

/// 純音訊
pub const AUDIO: &[&str] = &[
    "mp3", "m4a", "aac", "flac", "opus", "ogg", "oga", "wav", "wma", "ac3", "dts", "mka", "alac", "ape", "wv", "tta",
    "amr", "spx", "dsf", "dff",
];

/// 字幕（拖放字幕檔到播放中的影片 = 載入外掛字幕）
pub const SUBTITLE: &[&str] = &["srt", "ass", "ssa", "vtt", "sub", "idx", "sup", "smi", "sami", "txt"];

fn ext_of(path: &std::path::Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

pub fn is_subtitle(path: &std::path::Path) -> bool {
    ext_of(path).is_some_and(|e| SUBTITLE.contains(&e.as_str()))
}

/// 開檔對話框用：所有能播的副檔名
pub fn all_media() -> Vec<&'static str> {
    VIDEO.iter().chain(VIDEO_RARE).chain(AUDIO).copied().collect()
}
