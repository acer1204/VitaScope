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

/// 播放清單檔（開啟 = 載入清單，見 `m3u.rs`）
pub const PLAYLIST: &[&str] = &["m3u", "m3u8"];

fn ext_of(path: &std::path::Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase())
}

pub fn is_subtitle(path: &std::path::Path) -> bool {
    ext_of(path).is_some_and(|e| SUBTITLE.contains(&e.as_str()))
}

pub fn is_playlist(path: &std::path::Path) -> bool {
    ext_of(path).is_some_and(|e| PLAYLIST.contains(&e.as_str()))
}

/// 開檔對話框用：所有能播的副檔名
pub fn all_media() -> Vec<&'static str> {
    VIDEO.iter().chain(VIDEO_RARE).chain(AUDIO).copied().collect()
}

/// 影片旁邊同名的外掛音軌（字幕組常附的 `.mka`：評論音軌、5.1、其他語言的配音）。
/// 檔名要跟影片一樣（不分大小寫），或中間多一段語言之類的標記，例如 `第1集.mka`、`第1集.jpn.mka`
pub fn find_external_audio(video: &std::path::Path) -> Vec<std::path::PathBuf> {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem()) else {
        return Vec::new();
    };
    let stem = stem.to_string_lossy().to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p != video && ext_of(p).is_some_and(|e| AUDIO.contains(&e.as_str())))
        .filter(|p| {
            let name_stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            match name_stem.strip_prefix(&stem) {
                Some("") => true,
                // 「影片.jpn」：多一段短短的標記，不能是另一集（例如「第1集」對「第10集」）
                Some(tag) => tag.starts_with('.') && tag.len() <= 17 && !tag[1..].contains('.'),
                None => false,
            }
        })
        .collect();
    found.sort();
    found
}

/// 影片或純音訊（同資料夾播放清單只收同一類的檔案）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

pub fn media_kind(path: &std::path::Path) -> Option<MediaKind> {
    let ext = ext_of(path)?;
    if VIDEO.contains(&ext.as_str()) || VIDEO_RARE.contains(&ext.as_str()) {
        Some(MediaKind::Video)
    } else if AUDIO.contains(&ext.as_str()) {
        Some(MediaKind::Audio)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_same_name_external_audio() {
        let dir = std::env::temp_dir().join(format!("vitascope-extaudio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "S01E01.mkv",
            "S01E01.mka",
            "s01e01.JPN.mka",
            "S01E01.srt",
            "S01E010.mka",
            "S01E01.extra.part2.mka",
            "S01E02.mka",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let found: Vec<String> = find_external_audio(&dir.join("S01E01.mkv"))
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(found, ["S01E01.mka", "s01e01.JPN.mka"]);
        // 直接開 .mka 時，不會把自己當成外掛音軌
        assert!(find_external_audio(&dir.join("S01E02.mka")).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
