//! 歌詞的截圖檢查：真的開一個視窗播音樂（旁邊有同名的 LRC 歌詞），用 `--shot` 截下 egui 實際畫出來的畫面。
//! - 有專輯封面：mpv 自己畫在封面上的歌詞要藏起來（介面把歌詞畫在下方的底色上）。字幕位置設成 50
//!   （畫面中央），沒藏起來的話封面中央會多一行字；跟沒有歌詞時的截圖比對封面的部分
//! - 沒有封面：歌詞畫在歌名下面（比沒有歌詞時多出亮的字）
//!
//! 會在螢幕上開視窗（每次約 4 秒，共四次），所以預設不跑（#[ignore]），在開發機上手動跑：
//!
//! ```text
//! cargo test --test lyrics_shot -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 截圖留在暫存資料夾（`vitascope-lyrics-shot`），路徑會印出來，可以打開來看。
//! 設定檔、快取用暫存資料夾裡的（APPDATA 之類的環境變數換掉），不會動到使用者的設定。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 一次只開一個視窗
static SCREEN: Mutex<()> = Mutex::new(());
/// 靜音（不出聲音）；字幕放在畫面中央（mpv 畫的歌詞如果沒藏起來，會落在封面中央）
const SETTINGS: &str = r#"{"muted": true, "subtitle": {"position": 50}}"#;
const LYRIC: &str = "影戲播放器測試 歌詞 VITASCOPE LYRICS TEST";

struct Shot {
    path: PathBuf,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl Shot {
    fn luma_at(&self, x: usize, y: usize) -> f64 {
        let i = (y * self.width + x) * 4;
        let [r, g, b] = [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]].map(f64::from);
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// 一塊長方形（比例，0–1）的像素範圍
    fn area(&self, x: (f64, f64), y: (f64, f64)) -> impl Iterator<Item = (usize, usize)> + use<> {
        let (x0, x1) = ((x.0 * self.width as f64) as usize, (x.1 * self.width as f64) as usize);
        let (y0, y1) = ((y.0 * self.height as f64) as usize, (y.1 * self.height as f64) as usize);
        (y0..y1).flat_map(move |yy| (x0..x1).map(move |xx| (xx, yy)))
    }

    /// 一塊長方形裡亮的灰白色像素（介面畫的字）有幾個。封面（測試畫面）是鮮豔的顏色，不算
    fn bright(&self, x: (f64, f64), y: (f64, f64)) -> usize {
        self.area(x, y)
            .filter(|&(xx, yy)| {
                let i = (yy * self.width + xx) * 4;
                let rgb = &self.rgba[i..i + 3];
                let spread = rgb.iter().max().unwrap() - rgb.iter().min().unwrap();
                self.luma_at(xx, yy) > 180.0 && spread < 30
            })
            .count()
    }
}

/// 兩張截圖在一塊長方形裡，亮度差超過 40 的像素有幾個
fn differing(a: &Shot, b: &Shot, x: (f64, f64), y: (f64, f64)) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height), "兩張截圖一樣大");
    a.area(x, y)
        .filter(|&(xx, yy)| (a.luma_at(xx, yy) - b.luma_at(xx, yy)).abs() > 40.0)
        .count()
}

fn sample(rel: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p
}

/// 用暫存的設定檔開 `rel` 這首歌（`lyrics` = 旁邊放同名的 LRC），播放 2 秒後截圖、自己關閉
fn shot(name: &str, rel: &str, lyrics: bool) -> Shot {
    let _screen = SCREEN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("vitascope-lyrics-shot").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let home = dir.join("home");
    for config in [
        dir.join("appdata").join("Vitascope"),
        dir.join("xdg").join("vitascope"),
        home.join("Library/Application Support/Vitascope"),
    ] {
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("settings.json"), SETTINGS).unwrap();
    }
    let ext = Path::new(rel).extension().unwrap().to_string_lossy().into_owned();
    let song = dir.join(format!("song.{ext}"));
    std::fs::copy(sample(rel), &song).unwrap();
    if lyrics {
        std::fs::write(dir.join("song.lrc"), format!("[00:00.00]{LYRIC}\n")).unwrap();
    }
    let png = dir.join("shot.png");
    let stderr_path = dir.join("stderr.txt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vitascope"))
        .arg("--new-window")
        .arg(&song)
        .arg("--shot")
        .arg(&png)
        .arg("--shot-delay")
        .arg("2")
        .env("APPDATA", dir.join("appdata"))
        .env("LOCALAPPDATA", dir.join("localappdata"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("HOME", &home)
        .env_remove("VITASCOPE_MPV_OPTS")
        .env_remove("VITASCOPE_PACING")
        .env_remove("VITASCOPE_TEST_MINIMIZE")
        .env_remove("VITASCOPE_TEST_BUSY_UI")
        .env_remove("VITASCOPE_TEST_STALL")
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .expect("無法啟動影戲");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("影戲 60 秒還沒結束（{}）", dir.display());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let stderr = String::from_utf8_lossy(&std::fs::read(&stderr_path).unwrap_or_default()).into_owned();
    assert!(status.success(), "影戲結束時出錯：{status}\n{stderr}");
    assert!(
        stderr.contains("截圖已存到"),
        "沒有截到圖（{}）：\n{stderr}",
        dir.display()
    );
    let shot = read_png(&png);
    eprintln!(
        "{name}：截圖在 {}（{}×{}）",
        shot.path.display(),
        shot.width,
        shot.height
    );
    shot
}

fn read_png(path: &Path) -> Shot {
    let file = std::fs::File::open(path).unwrap();
    let decoder = png::Decoder::new(std::io::BufReader::new(file));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba, "--shot 存的是 RGBA");
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    buf.truncate(info.buffer_size());
    Shot {
        path: path.to_owned(),
        width: info.width as usize,
        height: info.height as usize,
        rgba: buf,
    }
}

#[test]
#[ignore = "會在螢幕上開視窗（約 4 秒，兩次）；在開發機上手動跑"]
fn lyrics_over_a_cover_are_drawn_once_in_the_band() {
    let with = shot("cover-lyrics", "general/audio_mp3_cover.mp3", true);
    let without = shot("cover-plain", "general/audio_mp3_cover.mp3", false);
    // 封面的上半部到中央（字幕位置 50）：mpv 的歌詞藏起來了，跟沒有歌詞時一樣。
    // 只比上面 60%：有歌詞時下方的底色比較高
    let cover = differing(&with, &without, (0.0, 1.0), (0.05, 0.6));
    eprintln!("封面上不一樣的像素：{cover}");
    assert!(cover < 50, "封面上多了 mpv 畫的歌詞（{cover} 個像素不一樣）");
    // 下方的底色：有歌詞時多一行亮的字
    let band = (
        with.bright((0.0, 1.0), (0.6, 1.0)),
        without.bright((0.0, 1.0), (0.6, 1.0)),
    );
    eprintln!("下方亮的像素：有歌詞 {}、沒有 {}", band.0, band.1);
    assert!(band.0 > band.1 + 200, "下方的底色上沒有畫出歌詞：{band:?}");
}

#[test]
#[ignore = "會在螢幕上開視窗（約 4 秒，兩次）；在開發機上手動跑"]
fn lyrics_without_a_cover_are_drawn_under_the_title() {
    let with = shot("plain-lyrics", "general/audio_flac.flac", true);
    let without = shot("plain-plain", "general/audio_flac.flac", false);
    let text = (
        with.bright((0.0, 1.0), (0.2, 0.8)),
        without.bright((0.0, 1.0), (0.2, 0.8)),
    );
    eprintln!("畫面中央亮的像素：有歌詞 {}、沒有 {}", text.0, text.1);
    assert!(text.0 > text.1 + 200, "歌名下面沒有畫出歌詞：{text:?}");
}
