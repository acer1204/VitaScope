//! 播放清單檔（.m3u / .m3u8）的讀寫。
//!
//! - 寫：一律 UTF-8（不加 BOM，有些播放器會把 BOM 當成第一行的一部分），`#EXTM3U` 開頭，
//!   每個檔案前面一行 `#EXTINF:秒數,標題`。清單檔所在資料夾（含子資料夾）裡的檔案寫相對路徑，
//!   整個資料夾搬到別處（例如隨身碟）也能用；其他的寫完整路徑
//! - 讀：BOM、UTF-8、舊式編碼（Big5 等，舊版播放器存的 .m3u）都能讀；相對路徑以清單檔所在的資料夾為準；
//!   `file://` 網址轉回路徑；Windows 存的反斜線在其他系統上換成斜線；網址（http 等）照原樣保留
//! - HLS 串流的 .m3u8（有 `#EXT-X-` 標籤）不是播放清單，整個交給 mpv 播
//! - 手動編輯過的清單在關閉程式時存起來（`playlist.m3u8`，在設定資料夾），下次開啟時還在

use std::path::{Component, Path, PathBuf};

/// 清單裡的一項
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// 檔案路徑；網址（http 之類）也放在這裡，原樣交給 mpv
    pub path: PathBuf,
    /// `#EXTINF` 的標題
    pub title: Option<String>,
    /// `#EXTINF` 的長度（秒）；不知道是 None
    pub duration: Option<f64>,
}

/// 播放清單檔最大多少（避免不小心開到很大的檔案）
const MAX_SIZE: u64 = 16 * 1024 * 1024;

fn read_text(path: &Path) -> std::io::Result<String> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_SIZE {
        return Err(std::io::Error::other(format!("檔案太大（{} MB）", size / 1024 / 1024)));
    }
    Ok(crate::subs::decode(&std::fs::read(path)?).0)
}

/// 讀取播放清單檔
pub fn read(path: &Path) -> std::io::Result<Vec<Entry>> {
    let text = read_text(path)?;
    let base = path.parent().unwrap_or(Path::new(""));
    Ok(parse(&text, base))
}

/// HLS 串流（有 `#EXT-X-` 標籤的 .m3u8）：裡面是一段一段的影片片段，不是播放清單
pub fn is_hls(text: &str) -> bool {
    text.lines()
        .any(|l| l.trim_start_matches('\u{feff}').trim_start().starts_with("#EXT-X-"))
}

/// 檔案是不是 HLS 串流（讀不到就當作不是，之後開啟時會回報錯誤）
pub fn is_hls_file(path: &Path) -> bool {
    read_text(path).is_ok_and(|t| is_hls(&t))
}

/// `#EXTINF:` 後面的部分在第一個不在引號裡的逗號切開：IPTV 清單的屬性裡會有逗號（`tvg-name="A, B"`）
fn split_extinf(rest: &str) -> (&str, &str) {
    let mut quoted = false;
    for (i, c) in rest.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => return (&rest[..i], &rest[i + 1..]),
            _ => {}
        }
    }
    (rest, "")
}

/// 解析清單內容；相對路徑以 `base` 為準
pub fn parse(text: &str, base: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut info: Option<(Option<f64>, Option<String>)> = None;
    for line in text.lines() {
        let line = line.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // #EXTINF:秒數 屬性="…",標題
            let (head, title) = split_extinf(rest);
            let duration = head
                .split_whitespace()
                .next()
                .and_then(|d| d.parse::<f64>().ok())
                .filter(|d| *d >= 0.0);
            let title = Some(title.trim().to_owned()).filter(|t| !t.is_empty());
            info = Some((duration, title));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (duration, title) = info.take().unwrap_or((None, None));
        entries.push(Entry {
            path: resolve(line, base),
            title,
            duration,
        });
    }
    entries
}

/// 清單裡的一行 → 路徑
fn resolve(line: &str, base: &Path) -> PathBuf {
    if let Some(rest) = line.strip_prefix("file://") {
        return file_url_to_path(rest);
    }
    if is_url(line) {
        return PathBuf::from(line);
    }
    // Windows 存的清單用反斜線；在其他系統上換成斜線才分得出資料夾
    let line = if cfg!(windows) {
        line.to_owned()
    } else {
        line.replace('\\', "/")
    };
    let p = Path::new(&line);
    if p.is_absolute() || p.has_root() {
        p.to_path_buf()
    } else {
        normalize(&base.join(p))
    }
}

/// `scheme://` 開頭的網址（但不是 Windows 的磁碟代號 `C:\`）
pub fn is_url(s: &str) -> bool {
    s.split_once("://").is_some_and(|(scheme, _)| {
        scheme.len() >= 2 && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    })
}

/// `file:///C:/影片/a.mp4`、`file:///home/a.mp4`、`file://server/share/a.mp4`（`file://` 之後的部分）
fn file_url_to_path(rest: &str) -> PathBuf {
    let decoded = percent_decode(rest);
    let s = decoded.as_str();
    if cfg!(windows) {
        // /C:/… → C:/…；//server/share → \\server\share
        let s = match s.strip_prefix('/') {
            Some(after) if after.as_bytes().get(1) == Some(&b':') => after.to_owned(),
            Some(after) => format!("/{after}"),
            None => format!("//{s}"),
        };
        PathBuf::from(s.replace('/', "\\"))
    } else {
        PathBuf::from(s.strip_prefix("localhost").unwrap_or(s))
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 去掉路徑裡的 `.`、`..`（不碰檔案系統）
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// 清單檔的內容。`entries` 的路徑要是完整路徑（網址照原樣寫）
pub fn format(entries: &[Entry], playlist_path: &Path) -> String {
    let base = playlist_path.parent().unwrap_or(Path::new(""));
    let mut out = String::from("#EXTM3U\n");
    for e in entries {
        let title = e
            .title
            .clone()
            .or_else(|| e.path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let duration = e.duration.map_or(-1, |d| d.round() as i64);
        // 標題裡的換行會讓下一行變成路徑
        let title = title.replace(['\r', '\n'], " ");
        out.push_str(&format!("#EXTINF:{duration},{title}\n"));
        let text = e.path.to_string_lossy();
        let line = if is_url(&text) {
            text.into_owned()
        } else {
            match e.path.strip_prefix(base) {
                // 相對路徑用「/」：各系統的 mpv、Windows 都看得懂（Linux、macOS 不認得「\」）
                Ok(rel) if !base.as_os_str().is_empty() => rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/"),
                _ => text.into_owned(),
            }
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// 存成播放清單檔（UTF-8）
pub fn write(path: &Path, entries: &[Entry]) -> std::io::Result<()> {
    std::fs::write(path, format(entries, path))
}

// ───────────── 下次開啟時還原 ─────────────

/// 記下「目前播到哪一項」的註解行（其他播放器會忽略 # 開頭的行）
const CURRENT_TAG: &str = "#VITASCOPE-CURRENT:";

fn session_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|d| d.join("playlist.m3u8"))
}

/// 存下手動整理的清單（完整路徑）與目前的位置；`items` 是空的就刪掉存檔
pub fn save_session(items: &[PathBuf], current: Option<usize>) -> std::io::Result<()> {
    let Some(path) = session_path() else { return Ok(()) };
    save_session_to(&path, items, current)
}

pub fn save_session_to(path: &Path, items: &[PathBuf], current: Option<usize>) -> std::io::Result<()> {
    if items.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let entries: Vec<Entry> = items
        .iter()
        .map(|p| Entry {
            path: p.clone(),
            title: None,
            duration: None,
        })
        .collect();
    // 寫完整路徑（設定資料夾跟影片沒有關係）：base 給一個不會是前綴的路徑
    let mut text = format(&entries, Path::new(""));
    if let Some(i) = current {
        text.push_str(&format!("{CURRENT_TAG}{i}\n"));
    }
    // 先寫暫存檔再改名：中途當掉也不會留下寫一半的檔案
    let tmp = path.with_extension(format!("m3u8.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// 上次存下的清單與位置
pub fn load_session() -> Option<(Vec<PathBuf>, Option<usize>)> {
    load_session_from(&session_path()?)
}

pub fn load_session_from(path: &Path) -> Option<(Vec<PathBuf>, Option<usize>)> {
    let text = read_text(path).ok()?;
    let items: Vec<PathBuf> = parse(&text, Path::new("")).into_iter().map(|e| e.path).collect();
    let current = text
        .lines()
        .find_map(|l| l.trim().strip_prefix(CURRENT_TAG)?.trim().parse::<usize>().ok())
        .filter(|i| *i < items.len());
    (!items.is_empty()).then_some((items, current))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str) -> Entry {
        Entry {
            path: PathBuf::from(path),
            title: None,
            duration: None,
        }
    }

    #[test]
    fn parses_extinf_and_skips_comments() {
        let text = "\u{feff}#EXTM3U\r\n#EXTINF:123,第一集\r\na.mp4\r\n\r\n# 註解\r\n#EXTINF:-1 tvg-id=\"x\",二\r\nb.mkv\r\nc.mp4\r\n";
        let list = parse(text, Path::new("base"));
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].title.as_deref(), Some("第一集"));
        assert_eq!(list[0].duration, Some(123.0));
        assert_eq!(list[0].path, Path::new("base").join("a.mp4"));
        assert_eq!(list[1].title.as_deref(), Some("二"));
        assert_eq!(list[1].duration, None, "-1 = 不知道長度");
        assert_eq!(list[2].title, None, "#EXTINF 只套用到下一個檔案");
    }

    #[test]
    fn relative_paths_resolve_against_the_playlist_folder() {
        let base = Path::new("music");
        let list = parse("sub/a.mp3\n../b.mp3\n./c.mp3\n", base);
        assert_eq!(list[0].path, Path::new("music").join("sub").join("a.mp3"));
        assert_eq!(list[1].path, Path::new("b.mp3"));
        assert_eq!(list[2].path, Path::new("music").join("c.mp3"));
    }

    #[test]
    fn urls_are_kept_and_file_urls_become_paths() {
        let list = parse(
            "https://example.com/live.m3u8\nfile:///C:/%E5%BD%B1%E7%89%87/a%20b.mp4\n",
            Path::new("x"),
        );
        assert_eq!(list[0].path, PathBuf::from("https://example.com/live.m3u8"));
        if cfg!(windows) {
            assert_eq!(list[1].path, PathBuf::from(r"C:\影片\a b.mp4"));
        } else {
            assert_eq!(list[1].path, PathBuf::from("/C:/影片/a b.mp4"));
        }
        assert!(!is_url(r"C:\a.mp4"));
        assert!(!is_url("C:/a.mp4"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_and_unc_paths() {
        let list = parse(
            "D:\\影片\\a.mp4\n\\\\nas\\share\\b.mp4\nfile://nas/share/c.mp4\n",
            Path::new("x"),
        );
        assert_eq!(list[0].path, PathBuf::from(r"D:\影片\a.mp4"));
        assert_eq!(list[1].path, PathBuf::from(r"\\nas\share\b.mp4"));
        assert_eq!(list[2].path, PathBuf::from(r"\\nas\share\c.mp4"));
    }

    #[cfg(not(windows))]
    #[test]
    fn backslashes_from_windows_playlists_become_separators() {
        let list = parse("sub\\a.mp4\n", Path::new("/media"));
        assert_eq!(list[0].path, PathBuf::from("/media/sub/a.mp4"));
    }

    #[test]
    fn writes_relative_paths_inside_the_playlist_folder() {
        let dir = std::env::temp_dir().join("vitascope-m3u-test");
        let inside = dir.join("sub").join("第一集.mp4");
        let outside = std::env::temp_dir().join("elsewhere.mp4");
        let mut with_info = entry(&inside.to_string_lossy());
        with_info.duration = Some(61.6);
        let entries = vec![
            with_info,
            entry(&outside.to_string_lossy()),
            entry("https://example.com/a.mp4"),
        ];
        let text = format(&entries, &dir.join("list.m3u8"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "#EXTM3U");
        assert_eq!(lines[1], "#EXTINF:62,第一集");
        assert_eq!(lines[2], "sub/第一集.mp4");
        assert_eq!(lines[3], "#EXTINF:-1,elsewhere");
        assert_eq!(lines[4], outside.to_string_lossy());
        assert_eq!(lines[6], "https://example.com/a.mp4");
        // 讀回來是同樣的路徑
        let back = parse(&text, &dir);
        assert_eq!(
            back.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>()
        );
        assert_eq!(back[0].title.as_deref(), Some("第一集"));
    }

    #[test]
    fn reads_legacy_big5_playlists() {
        let dir = std::env::temp_dir().join(format!("vitascope-m3u-big5-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.m3u");
        let (bytes, _, _) =
            encoding_rs::BIG5.encode("#EXTM3U\n#EXTINF:10,臺灣的影片名稱測試\n臺灣的影片名稱測試第一集.mp4\n");
        std::fs::write(&path, &bytes).unwrap();
        let list = read(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, dir.join("臺灣的影片名稱測試第一集.mp4"));
    }

    #[test]
    fn hls_and_iptv_details() {
        assert!(is_hls("#EXTM3U\n#EXT-X-VERSION:3\n#EXTINF:10,\nseg1.ts\n"));
        assert!(!is_hls("#EXTM3U\n#EXTINF:10,a\na.mp4\n"));
        // 屬性裡的逗號不算
        let list = parse(
            "#EXTINF:-1 tvg-name=\"新聞, 台\" group-title=\"A\",新聞台\nhttp://x/1.ts\n",
            Path::new(""),
        );
        assert_eq!(list[0].title.as_deref(), Some("新聞台"));
    }

    #[test]
    fn relative_paths_are_written_with_forward_slashes() {
        let dir = std::env::temp_dir().join("vitascope-m3u-slash");
        let e = entry(&dir.join("第一季").join("第1集.mp4").to_string_lossy());
        let text = format(&[e], &dir.join("list.m3u8"));
        assert!(text.contains("\n第一季/第1集.mp4\n"), "{text}");
    }

    #[test]
    fn session_round_trip() {
        let dir = std::env::temp_dir().join(format!("vitascope-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("playlist.m3u8");
        let items = vec![
            dir.join("a.mp4"),
            dir.join("影片").join("b.mkv"),
            PathBuf::from("https://x/c.m3u8"),
        ];
        save_session_to(&path, &items, Some(1)).unwrap();
        let (back, current) = load_session_from(&path).unwrap();
        assert_eq!(back, items);
        assert_eq!(current, Some(1));
        // 清單清空：存檔刪掉
        save_session_to(&path, &[], None).unwrap();
        assert!(!path.exists());
        assert!(load_session_from(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newlines_in_titles_do_not_break_the_file() {
        let mut e = entry("/x/a.mp4");
        e.title = Some("兩\n行".to_owned());
        let text = format(&[e], Path::new("/y/list.m3u8"));
        assert_eq!(parse(&text, Path::new("/y")).len(), 1);
    }
}
