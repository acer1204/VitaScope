//! 淺色主題的截圖檢查：真的開一個視窗，用 `--shot` 截下 egui 實際畫出來的畫面，
//! 看控制列是淺色的、影片畫面還是黑的。播放中、沒開檔（起始畫面）各截一次，深色主題也截一次對照。
//!
//! 會在螢幕上開視窗（每次約 5 秒），所以預設不跑（#[ignore]），在開發機上手動跑：
//!
//! ```text
//! cargo test --test theme_shot -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 截圖留在暫存資料夾（`vitascope-theme-shot`），路徑會印出來，可以打開來看。
//! 設定檔用暫存資料夾裡的（APPDATA 之類的環境變數換掉），不會動到使用者的設定。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 一次只開一個視窗
static SCREEN: Mutex<()> = Mutex::new(());
/// 測試畫面（彩色條紋）；播放時縮成一半（`video-zoom=-1`），四周露出影片畫面的黑底
const MEDIA: &str = "av://lavfi:testsrc2=size=640x360:rate=30:duration=20";

/// 截圖（RGBA）
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

    /// 一塊長方形（比例，0–1）裡的平均亮度
    fn mean_luma(&self, x: (f64, f64), y: (f64, f64)) -> f64 {
        let (x0, x1) = ((x.0 * self.width as f64) as usize, (x.1 * self.width as f64) as usize);
        let (y0, y1) = ((y.0 * self.height as f64) as usize, (y.1 * self.height as f64) as usize);
        let mut sum = 0.0;
        let mut n = 0.0;
        for yy in y0..y1.max(y0 + 1) {
            for xx in x0..x1.max(x0 + 1) {
                sum += self.luma_at(xx.min(self.width - 1), yy.min(self.height - 1));
                n += 1.0;
            }
        }
        sum / n
    }

    /// 最下面 `rows` 列像素的平均亮度（控制列的底）
    fn bottom_luma(&self, rows: usize) -> f64 {
        let mut sum = 0.0;
        for y in self.height - rows..self.height {
            for x in 0..self.width {
                sum += self.luma_at(x, y);
            }
        }
        sum / (rows * self.width) as f64
    }
}

/// 用暫存的設定檔（`settings` 是 settings.json 的內容）開影戲（`media` 是 None 就是起始畫面），截圖後自己關閉
fn shot(name: &str, settings: &str, media: Option<&str>) -> Shot {
    let _screen = SCREEN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("vitascope-theme-shot").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // 三個平台的設定檔位置都放一份（見 settings::config_dir）
    let home = dir.join("home");
    for config in [
        dir.join("appdata").join("Vitascope"),
        dir.join("xdg").join("vitascope"),
        home.join("Library/Application Support/Vitascope"),
    ] {
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("settings.json"), settings).unwrap();
    }
    let png = dir.join("shot.png");
    let stderr_path = dir.join("stderr.txt");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vitascope"));
    cmd.arg("--new-window");
    if let Some(media) = media {
        cmd.arg(media);
    }
    let mut child = cmd
        .arg("--shot")
        .arg(&png)
        .arg("--shot-delay")
        .arg("2")
        .env("APPDATA", dir.join("appdata"))
        .env("LOCALAPPDATA", dir.join("localappdata"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("HOME", &home)
        .env("VITASCOPE_MPV_OPTS", "video-zoom=-1")
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

fn check(name: &str, media: Option<&str>) {
    let light = shot(&format!("{name}-light"), r#"{"theme": "light"}"#, media);
    let dark = shot(&format!("{name}-dark"), r#"{"theme": "dark"}"#, media);
    for (s, theme) in [(&light, "淺色"), (&dark, "深色")] {
        // 影片畫面的左上角（起始畫面的字、縮小的影片都在中間）
        let video = s.mean_luma((0.05, 0.2), (0.05, 0.2));
        let center = s.mean_luma((0.4, 0.6), (0.3, 0.5));
        let bar = s.bottom_luma(3);
        eprintln!("{name}／{theme}：影片畫面的角落 {video:.1}、中央 {center:.1}、控制列底 {bar:.1}");
        assert!(video < 16.0, "{name}／{theme}：影片畫面應該是黑的（{video:.1}）");
        if media.is_some() {
            // 真的在播（彩色條紋畫出來了），不是開檔失敗停在起始畫面
            assert!(center > 40.0, "{name}／{theme}：影片沒有畫出來（中央 {center:.1}）");
        }
    }
    assert!(light.bottom_luma(3) > 200.0, "{name}：淺色主題的控制列應該是淺色的");
    assert!(dark.bottom_luma(3) < 60.0, "{name}：深色主題的控制列應該是深色的");
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒，兩次）；在開發機上手動跑"]
fn light_theme_keeps_the_video_black_while_playing() {
    check("playing", Some(MEDIA));
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒，兩次）；在開發機上手動跑"]
fn light_theme_keeps_the_idle_screen_black() {
    check("idle", None);
}
