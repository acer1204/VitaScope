//! 擷取畫面：存成 PNG（預設在「圖片」資料夾裡的 VitaScope）、複製到剪貼簿。
//!
//! 截圖由 mpv 產生（`screenshot-to-file`，原始解析度，可選含不含字幕；長寬比、裁切、硬體解碼都處理好了），
//! 但 mpv 給 libmpv 的軟體截圖不含旋轉、也不含我們的翻轉著色器（見 ROADMAP「學到的事」），
//! 這兩種情況在背景執行緒把圖讀回來、轉正之後再存。

use std::path::{Path, PathBuf};

/// 預設的截圖資料夾：系統的「圖片」資料夾裡的 VitaScope
pub fn default_dir() -> PathBuf {
    pictures_dir()
        .or_else(|| home().map(|h| h.join("Pictures")))
        .unwrap_or_else(std::env::temp_dir)
        .join("VitaScope")
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

/// Windows：已知資料夾 API（「圖片」可能被 OneDrive 或使用者搬到別的地方）
#[cfg(windows)]
fn pictures_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath};
    let mut raw: *mut u16 = std::ptr::null_mut();
    // SAFETY: 成功時 raw 是系統配置、以 0 結尾的字串，用完要 CoTaskMemFree（失敗時也要）
    unsafe {
        let hr = SHGetKnownFolderPath(&FOLDERID_Pictures, 0, std::ptr::null_mut(), &mut raw);
        let path = (hr >= 0 && !raw.is_null()).then(|| {
            let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
            PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(raw, len)))
        });
        CoTaskMemFree(raw.cast());
        path
    }
}

/// Linux：XDG 的使用者資料夾設定（中文系統常是「~/圖片」）
#[cfg(all(unix, not(target_os = "macos")))]
fn pictures_dir() -> Option<PathBuf> {
    let home = home()?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    parse_xdg_pictures(&text, &home)
}

#[cfg(target_os = "macos")]
fn pictures_dir() -> Option<PathBuf> {
    home().map(|h| h.join("Pictures"))
}

/// `XDG_PICTURES_DIR="$HOME/圖片"` → `/home/me/圖片`
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn parse_xdg_pictures(text: &str, home: &Path) -> Option<PathBuf> {
    let value = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("XDG_PICTURES_DIR="))?
        .trim()
        .trim_matches('"');
    let path = match value.strip_prefix("$HOME") {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(value),
    };
    // 設成家目錄本身 = 沒有設定
    (path.has_root() && path != home).then_some(path)
}

/// 檔名不能用的字元換成「_」（各系統都一樣處理，存到網路磁碟、隨身碟也不會出問題）
pub fn sanitize_stem(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.ends_with(['.', ' ']) {
        out.pop();
    }
    // Windows 的保留名稱（CON、NUL、COM1…）
    let base = out.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit());
    if reserved {
        out.insert(0, '_');
    }
    // 太長的檔名（ext4 最多 255 位元組）
    let mut end = out.len().min(150);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    if out.trim().is_empty() {
        "VitaScope".to_owned()
    } else {
        out
    }
}

/// 截圖檔名：「影片名稱 01.23.45.678.png」
pub fn file_name(source: &str, time: f64) -> String {
    let stem = if crate::m3u::is_url(source) {
        source
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("VitaScope")
            .to_owned()
    } else {
        Path::new(source)
            .file_stem()
            .map_or_else(|| "VitaScope".to_owned(), |s| s.to_string_lossy().into_owned())
    };
    let ms = (time.max(0.0) * 1000.0).round() as u64;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    format!("{} {h:02}.{m:02}.{s:02}.{ms:03}.png", sanitize_stem(&stem))
}

/// 不覆蓋已有的檔案：「名稱 (2).png」「名稱 (3).png」…
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    unique_path_except(dir, name, &[])
}

/// 同上，`taken` 是還在寫、還沒出現在資料夾裡的
pub fn unique_path_except(dir: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    let free = |p: &PathBuf| !p.exists() && !taken.contains(p);
    let first = dir.join(name);
    if free(&first) {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(free)
        .expect("總有一個名稱沒被用過")
}

/// 截圖要再轉正的部分（mpv 的軟體截圖不含）
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Fixup {
    /// 順時針旋轉（0、90、180、270）
    pub rotate: u32,
    pub hflip: bool,
    pub vflip: bool,
}

impl Fixup {
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    /// 截圖能不能含字幕：要轉正（旋轉、翻轉）的截圖，字幕會跟著轉（畫面上的字幕是正的），這時不含字幕
    pub fn keeps_subtitles(&self, wanted: bool) -> bool {
        wanted && self.is_none()
    }
}

/// RGBA 圖（不預乘透明度，由上而下）
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    /// 先旋轉、再翻轉（跟畫面上的順序一樣：翻轉著色器在旋轉之後）
    pub fn fixed(self, fix: Fixup) -> Image {
        let mut img = match fix.rotate % 360 {
            90 => self.rotated(1),
            180 => self.rotated(2),
            270 => self.rotated(3),
            _ => self,
        };
        if fix.hflip {
            for row in img.rgba.chunks_exact_mut(img.w * 4) {
                let px: Vec<[u8; 4]> = row.chunks_exact(4).rev().map(|p| [p[0], p[1], p[2], p[3]]).collect();
                row.copy_from_slice(px.concat().as_slice());
            }
        }
        if fix.vflip {
            let rows: Vec<&[u8]> = img.rgba.chunks_exact(img.w * 4).rev().collect();
            img.rgba = rows.concat();
        }
        img
    }

    /// 順時針轉 q 個 90°
    fn rotated(&self, q: u8) -> Image {
        let (w, h) = (self.w, self.h);
        let (nw, nh) = if q == 2 { (w, h) } else { (h, w) };
        let mut out = vec![0; self.rgba.len()];
        for y in 0..h {
            for x in 0..w {
                let (nx, ny) = match q {
                    1 => (h - 1 - y, x),
                    2 => (w - 1 - x, h - 1 - y),
                    _ => (y, w - 1 - x),
                };
                out[(ny * nw + nx) * 4..][..4].copy_from_slice(&self.rgba[(y * w + x) * 4..][..4]);
            }
        }
        Image {
            w: nw,
            h: nh,
            rgba: out,
        }
    }
}

/// 讀 PNG（mpv 存的截圖）
pub fn decode_png(path: &Path) -> std::io::Result<Image> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut decoder = png::Decoder::new(file);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(std::io::Error::other)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| std::io::Error::other(crate::tr!("圖太大", "image too large")))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(std::io::Error::other)?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        other => {
            return Err(std::io::Error::other(crate::tf!(
                "不支援的 PNG 格式 {other:?}",
                "unsupported PNG format {other:?}"
            )));
        }
    };
    Ok(Image {
        w: info.width as usize,
        h: info.height as usize,
        rgba,
    })
}

/// 存成 PNG（RGB，不含透明度；壓縮快一點，4K 的圖也不會等太久）
pub fn encode_png(img: &Image, path: &Path) -> std::io::Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, img.w as u32, img.h as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    let rgb: Vec<u8> = img.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    writer.write_image_data(&rgb).map_err(std::io::Error::other)?;
    writer.finish().map_err(std::io::Error::other)
}

/// 截圖存到哪裡
#[derive(Debug, Clone)]
pub enum Target {
    /// 存檔（mpv 寫到 `tmp`，要轉正的話轉好再存到 `path`；不用轉正時 `tmp` 就是 `path`）
    File { tmp: PathBuf, path: PathBuf },
    /// 複製到剪貼簿（mpv 寫到 `tmp`，讀回來後刪掉）
    Clipboard { tmp: PathBuf },
}

/// 背景處理的結果
#[derive(Debug)]
pub enum Done {
    Saved(PathBuf),
    Copied(Image),
    /// 失敗；存檔時帶著原本要存的位置
    Failed {
        path: Option<PathBuf>,
        error: String,
    },
}

/// mpv 寫好截圖之後的處理（在背景執行緒跑：4K 的圖解碼、旋轉、壓縮要零點幾秒）
pub fn finish(target: Target, fix: Fixup) -> Done {
    match target {
        Target::File { tmp, path } => {
            if tmp == path {
                return Done::Saved(path);
            }
            let result = decode_png(&tmp).and_then(|img| encode_png(&img.fixed(fix), &path));
            let _ = std::fs::remove_file(&tmp);
            match result {
                Ok(()) => Done::Saved(path),
                Err(e) => Done::Failed {
                    path: Some(path),
                    error: e.to_string(),
                },
            }
        }
        Target::Clipboard { tmp } => {
            let result = decode_png(&tmp);
            let _ = std::fs::remove_file(&tmp);
            match result {
                // 剪貼簿不吃透明度（egui 交給系統的是預乘過的顏色），一律不透明
                Ok(img) => {
                    let mut img = img.fixed(fix);
                    for p in img.rgba.chunks_exact_mut(4) {
                        p[3] = 255;
                    }
                    Done::Copied(img)
                }
                Err(e) => Done::Failed {
                    path: None,
                    error: e.to_string(),
                },
            }
        }
    }
}

/// mpv 先把截圖寫到這個暫存檔（要轉正、或是要複製到剪貼簿時）
pub fn temp_path(n: u64) -> PathBuf {
    let dir = std::env::temp_dir().join("vitascope-shots");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{}-{n}.png", std::process::id()))
}

/// 用系統的檔案管理員打開資料夾
pub fn open_folder(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    crate::syscmd::command(program).arg(dir).spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles_are_left_out_when_the_shot_is_turned_or_flipped() {
        let none = Fixup::default();
        assert!(none.keeps_subtitles(true));
        assert!(!none.keeps_subtitles(false));
        for fix in [
            Fixup { rotate: 90, ..none },
            Fixup { rotate: 180, ..none },
            Fixup { hflip: true, ..none },
            Fixup { vflip: true, ..none },
        ] {
            assert!(!fix.keeps_subtitles(true), "{fix:?}");
        }
    }

    /// 2×1 的圖：左紅、右藍
    fn two_pixels() -> Image {
        Image {
            w: 2,
            h: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        }
    }

    #[test]
    fn rotation_and_flips() {
        let r90 = two_pixels().fixed(Fixup {
            rotate: 90,
            ..Default::default()
        });
        // 順時針轉 90°：左邊的紅色到上面
        assert_eq!((r90.w, r90.h), (1, 2));
        assert_eq!(&r90.rgba[..4], &[255, 0, 0, 255]);
        let r270 = two_pixels().fixed(Fixup {
            rotate: 270,
            ..Default::default()
        });
        assert_eq!(&r270.rgba[..4], &[0, 0, 255, 255], "逆時針：右邊的藍色到上面");
        let h = two_pixels().fixed(Fixup {
            hflip: true,
            ..Default::default()
        });
        assert_eq!(&h.rgba[..4], &[0, 0, 255, 255]);
        // 先轉再翻：轉 90° 之後上下翻，紅色到下面
        let rv = two_pixels().fixed(Fixup {
            rotate: 90,
            vflip: true,
            ..Default::default()
        });
        assert_eq!(&rv.rgba[4..], &[255, 0, 0, 255]);
    }

    #[test]
    fn png_round_trip() {
        let dir = std::env::temp_dir().join(format!("vitascope-shot-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        encode_png(&two_pixels(), &path).unwrap();
        assert_eq!(decode_png(&path).unwrap(), two_pixels());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_names_are_safe_and_unique() {
        assert_eq!(file_name("C:/影片/第1集.mkv", 3723.456), "第1集 01.02.03.456.png");
        assert_eq!(
            file_name("https://x.com/live/stream.m3u8", 0.0),
            "stream.m3u8 00.00.00.000.png"
        );
        assert_eq!(sanitize_stem("a:b?c*"), "a_b_c_");
        assert_eq!(sanitize_stem("CON"), "_CON");
        assert_eq!(sanitize_stem("結尾. "), "結尾");
        assert_eq!(sanitize_stem(""), "VitaScope");
        assert!(sanitize_stem(&"長".repeat(200)).len() <= 150);
        let dir = std::env::temp_dir().join(format!("vitascope-unique-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"").unwrap();
        std::fs::write(dir.join("a (2).png"), b"").unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a (3).png"));
        assert_eq!(unique_path(&dir, "b.png"), dir.join("b.png"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn xdg_pictures_dir() {
        let home = Path::new("/home/me");
        let text = "# 註解\nXDG_DESKTOP_DIR=\"$HOME/桌面\"\nXDG_PICTURES_DIR=\"$HOME/圖片\"\n";
        assert_eq!(parse_xdg_pictures(text, home), Some(PathBuf::from("/home/me/圖片")));
        assert_eq!(parse_xdg_pictures("XDG_PICTURES_DIR=\"$HOME/\"\n", home), None);
        assert_eq!(
            parse_xdg_pictures("XDG_PICTURES_DIR=\"/data/pics\"\n", home),
            Some(PathBuf::from("/data/pics"))
        );
    }
}
