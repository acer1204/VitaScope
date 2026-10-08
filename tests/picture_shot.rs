//! 影像調整的截圖檢查：真的開一個視窗播測試畫面，用 `--shot` 截下 egui 實際畫出來的畫面，
//! 比較亮度 +50 跟沒有調整時畫面中央的平均亮度。一般的繪圖流程、軟體繪圖的簡化流程（gpu-dumb-mode，
//! Linux CI 的 llvmpipe 用的）各比一次。
//!
//! 會在螢幕上開視窗（每次約 5 秒），所以預設不跑（#[ignore]）：
//!
//! ```text
//! cargo test --test picture_shot -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Linux CI 在虛擬螢幕（xvfb）上跑。設定檔用暫存資料夾裡的（APPDATA 之類的環境變數換掉），不會動到使用者的設定。

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 一次只開一個視窗
static SCREEN: Mutex<()> = Mutex::new(());
/// 跟 CI 的介面測試同一個測試畫面（彩色條紋、漸層、會動的數字）
const MEDIA: &str = "av://lavfi:testsrc2=size=640x360:rate=30:duration=20";

/// 用暫存的設定檔（`settings` 是 settings.json 的內容）開影戲，等它截圖後自己關閉。
/// 回傳（畫面中央的平均亮度, 影戲的記錄）
fn shot(name: &str, settings: &str, mpv_opts: &str) -> (f64, String) {
    let _screen = SCREEN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("vitascope-picture-shot").join(name);
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
    let stderr_path = dir.join("stderr.txt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_vitascope"))
        .arg("--new-window")
        .arg(MEDIA)
        .arg("--shot")
        .arg(dir.join("shot.png"))
        .arg("--shot-delay")
        .arg("3")
        .env("APPDATA", dir.join("appdata"))
        .env("LOCALAPPDATA", dir.join("localappdata"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("HOME", &home)
        .env("VITASCOPE_MPV_OPTS", mpv_opts)
        .env_remove("VITASCOPE_PACING")
        .env_remove("VITASCOPE_TEST_MINIMIZE")
        .env_remove("VITASCOPE_TEST_BUSY_UI")
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
    let stderr = read(&stderr_path);
    assert!(status.success(), "影戲結束時出錯：{status}\n{stderr}");
    let luma = center_luma(&stderr).unwrap_or_else(|| panic!("沒有截到圖（{}）：\n{stderr}", dir.display()));
    eprintln!("{name}：中央平均亮度 {luma:.1}");
    (luma, stderr)
}

fn read(p: &Path) -> String {
    String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned()
}

/// 記錄裡的「中央平均亮度 87.2」
fn center_luma(stderr: &str) -> Option<f64> {
    let rest = stderr.split("中央平均亮度 ").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

/// 設定檔：勾了「下次開啟時沿用」的亮度
fn brightness_settings(brightness: i32) -> String {
    format!(r#"{{"video": {{"keep_adjust": true, "adjust": {{"brightness": {brightness}}}}}}}"#)
}

fn compare(label: &str, mpv_opts: &str) {
    let (plain, _) = shot(&format!("{label}-0"), &brightness_settings(0), mpv_opts);
    let (bright, log) = shot(&format!("{label}-50"), &brightness_settings(50), mpv_opts);
    assert!(!log.contains("無法套用"), "{log}");
    // 測試畫面中央平均大約 125；亮度 +50 實測（RTX 3090）一般流程 125.0 → 197.0、簡化流程 125.9 → 198.2
    assert!(
        plain > 20.0,
        "{label}：沒有調整的畫面太暗（{plain:.1}），畫面可能沒畫出來"
    );
    assert!(
        bright > plain + 30.0,
        "{label}：亮度 +50 應該明顯比較亮（{plain:.1} → {bright:.1}）"
    );
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn brightness_raises_the_picture() {
    compare("gpu", "");
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn brightness_raises_the_picture_in_dumb_mode() {
    // 軟體繪圖的簡化流程（Linux CI 的 llvmpipe、沒有顯示卡驅動的虛擬機）也有影像調整
    compare("dumb", "gpu-dumb-mode=yes");
}

#[test]
fn reads_the_luma_from_the_log() {
    let log = "[vitascope] 截圖已存到 x.png\n[vitascope] 截圖統計：非黑色像素 99.1%，中央平均亮度 87.2\n";
    assert_eq!(center_luma(log), Some(87.2));
    assert_eq!(center_luma("[vitascope] 截圖存檔失敗"), None);
}
