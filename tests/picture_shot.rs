//! 影像調整的截圖檢查：真的開一個視窗播測試畫面，用 `--shot` 截下 egui 實際畫出來的畫面，
//! 比較亮度 +50 跟沒有調整時畫面中央的平均亮度。一般的繪圖流程、軟體繪圖的簡化流程（gpu-dumb-mode，
//! Linux CI 的 llvmpipe 用的）各比一次。HDR10 影片在每一種色調映射曲線下都要畫得出來（不是黑的、沒有著色器錯誤）。
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

/// 用暫存的設定檔（`settings` 是 settings.json 的內容）開影戲播 `media`，等它截圖後自己關閉。
/// 回傳（畫面中央的平均亮度, 影戲的記錄）
fn shot(name: &str, settings: &str, mpv_opts: &str, media: &str) -> (f64, String) {
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
        .arg(media)
        .arg("--shot")
        .arg(dir.join("shot.png"))
        .arg("--shot-delay")
        .arg("3")
        .env("APPDATA", dir.join("appdata"))
        .env("LOCALAPPDATA", dir.join("localappdata"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("HOME", &home)
        .env("VITASCOPE_MPV_OPTS", mpv_opts)
        // mpv 的記錄印到 stderr：著色器編譯失敗之類的錯誤、設定了哪些選項（Set property）
        .env("VITASCOPE_DEBUG", "v")
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
    let (plain, _) = shot(&format!("{label}-0"), &brightness_settings(0), mpv_opts, MEDIA);
    let (bright, log) = shot(&format!("{label}-50"), &brightness_settings(50), mpv_opts, MEDIA);
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

/// 記錄裡畫面輸出（libmpv_render、vo）的錯誤
fn render_errors(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|l| l.starts_with("[mpv/error]") || l.starts_with("[mpv/fatal]"))
        .filter(|l| l.contains("] [libmpv_render]") || l.contains("] [vo/"))
        .collect()
}

/// 記錄裡有沒有「Set property: tone-mapping="hable" -> 1」（mpv 接受了這個值）
fn property_set(log: &str, name: &str, value: &str) -> bool {
    let prefix = format!("Set property: {name}=");
    log.lines().any(|l| {
        l.split_once(&prefix).is_some_and(|(_, rest)| {
            let (v, result) = rest.trim_end().rsplit_once(" -> ").unwrap_or((rest, ""));
            v.trim_matches('"') == value && result == "1"
        })
    })
}

/// HDR10 樣本在每一條曲線下都要畫得出來。色調映射在最後輸出到螢幕時做，軟體繪圖的簡化流程
/// （Linux CI 的 llvmpipe）也一樣會做，所以 CI 上也測得到曲線。
/// 測的是「畫得出來、沒有著色器錯誤、曲線有送到 mpv」，不是各曲線的觀感（這個樣本的亮度大多在曲線的轉折點以下，
/// 各曲線中央亮度只差幾個單位）
#[test]
#[ignore = "會在螢幕上開視窗（約 40 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn hdr_is_not_black_under_any_tone_curve() {
    let media =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated/general/mkv_hevc10_hdr10.mkv");
    assert!(
        media.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        media.display()
    );
    let media = media.to_string_lossy();
    for curve in vitascope::picture::ToneCurve::ALL {
        let name = serde_json::to_value(curve).unwrap();
        let name = name.as_str().unwrap();
        let settings = format!(r#"{{"video": {{"tone": {{"curve": "{name}"}}}}}}"#);
        let (luma, log) = shot(&format!("hdr-{name}"), &settings, "", &media);
        assert!(!log.contains("無法套用"), "{name}：{log}");
        // 設定檔的曲線真的送到了 mpv（讀設定檔出錯的話每次都會是「自動」）
        assert!(
            property_set(&log, "tone-mapping", curve.mpv()),
            "{name}：mpv 沒有收到 tone-mapping={}\n{log}",
            curve.mpv()
        );
        // 著色器編譯失敗時 mpv 在 libmpv_render 記錯誤，畫面變成一片藍（gamma 曲線在 OpenGL 3.3 實測中央 18.5）
        let errors = render_errors(&log);
        assert!(errors.is_empty(), "{name}：畫面輸出出錯\n{}", errors.join("\n"));
        // 實測各曲線中央平均：RTX 3090 137–142；Linux 的 llvmpipe（簡化流程、沒有動態峰值偵測）72.6–100.1。
        // 著色器壞掉時的一片藍是 18.5
        assert!(luma > 40.0, "{name}：HDR 畫面太暗（中央平均亮度 {luma:.1}）");
    }
}

/// 軟體繪圖的簡化流程也做色調映射（所以選單上 HDR 的選項不停用）：目標亮度 100 跟 1000 nits 的畫面要不一樣
#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn hdr_target_peak_applies_in_dumb_mode() {
    let media =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated/general/mkv_hevc10_hdr10.mkv");
    assert!(
        media.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        media.display()
    );
    let media = media.to_string_lossy();
    let shot_peak = |peak: u32| {
        let settings = format!(r#"{{"video": {{"tone": {{"target_peak": {peak}}}}}}}"#);
        let (luma, log) = shot(&format!("hdr-dumb-{peak}"), &settings, "gpu-dumb-mode=yes", &media);
        assert!(property_set(&log, "target-peak", &peak.to_string()), "{peak}：\n{log}");
        let errors = render_errors(&log);
        assert!(errors.is_empty(), "{peak}：畫面輸出出錯\n{}", errors.join("\n"));
        luma
    };
    let (low, high) = (shot_peak(100), shot_peak(1000));
    // 實測簡化流程：RTX 3090 141.3 → 161.6（一般流程 142.2 → 162.1）；Linux 的 llvmpipe 94.0 → 162.2，
    // 系統的 libmpv 0.37 94.0 → 117.1
    assert!(
        (high - low).abs() > 8.0,
        "簡化流程的目標亮度沒有作用（100 nits {low:.1}、1000 nits {high:.1}）"
    );
}

/// 單色的測試畫面：testsrc2 左上角深色的一小塊放大成整個畫面（這個 FFmpeg 只有 testsrc2，沒有 color）。
/// 反相之後很亮，一看就知道著色器有沒有作用
const DARK: &str = "av://lavfi:testsrc2=size=640x360:rate=30:duration=20,crop=w=8:h=8:x=0:y=0,scale=640:360";

/// 設定檔：一個像素著色器組合，使用中
fn shader_settings(files: &[&Path]) -> String {
    let files: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
    serde_json::json!({"video": {"shaders": {"presets": [{"id": 1, "name": "測試", "files": files}], "active": 1}}})
        .to_string()
}

/// 測試用的著色器：反相、編譯不過的（放在暫存資料夾）
fn test_shaders(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir()
        .join("vitascope-picture-shot")
        .join(format!("shader-files-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    let invert = dir.join("反相 測試.glsl");
    std::fs::write(
        &invert,
        "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC 反相\n\
         vec4 hook() { vec4 c = HOOKED_tex(HOOKED_pos); return vec4(1.0 - c.rgb, c.a); }\n",
    )
    .unwrap();
    let broken = dir.join("broken.glsl");
    std::fs::write(
        &broken,
        "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC 壞掉的\n\
         vec4 hook() { return HOOKED_tex(HOOKED_pos) * no_such_variable; }\n",
    )
    .unwrap();
    (invert, broken)
}

/// 記錄裡有「偵測到軟體繪圖」：自動改用簡化流程（Linux CI 的 llvmpipe），使用者的著色器不跑
fn auto_dumb(log: &str) -> bool {
    log.contains("偵測到軟體繪圖")
}

/// 使用者的像素著色器真的畫在畫面上（反相的著色器讓深色變成淺色）；壞掉的著色器（編譯不過）
/// 記下錯誤後自動還原成不使用，截圖時畫面是正常的。
/// 軟體繪圖（Linux CI 的 llvmpipe 自動改用簡化流程）不跑著色器：畫面跟沒有著色器一樣
#[test]
#[ignore = "會在螢幕上開視窗（約 15 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn invert_shader_applies_and_a_broken_one_is_reverted() {
    let (invert, broken) = test_shaders("gpu");
    let (plain, log) = shot("shader-none", "{}", "", DARK);
    // 實測 RTX 3090：沒有著色器 16.9、反相 238.4
    assert!(plain < 80.0, "沒有著色器的畫面應該是深色的（{plain:.1}）");
    let (inverted, log2) = shot("shader-invert", &shader_settings(&[&invert]), "", DARK);
    let errors = render_errors(&log2);
    assert!(errors.is_empty(), "反相的著色器不能有錯誤\n{}", errors.join("\n"));
    assert!(!log2.contains("像素著色器無法使用"), "{log2}");
    if auto_dumb(&log) {
        eprintln!("軟體繪圖（簡化流程）：使用者的著色器不跑");
        assert!(
            (inverted - plain).abs() < 10.0,
            "簡化流程不送著色器，畫面不變（{plain:.1} → {inverted:.1}）"
        );
        return;
    }
    assert!(
        inverted > 170.0,
        "反相的著色器應該讓畫面變亮（{plain:.1} → {inverted:.1}）"
    );
    let (after, log) = shot("shader-broken", &shader_settings(&[&broken]), "", DARK);
    let errors = render_errors(&log);
    assert!(!errors.is_empty(), "壞掉的著色器要有畫面輸出的錯誤記錄\n{log}");
    let reverted = log
        .lines()
        .find(|l| l.contains("像素著色器無法使用，已還原：broken.glsl"))
        .unwrap_or_else(|| panic!("要自動還原\n{log}"));
    eprintln!("{reverted}");
    assert!(
        (after - plain).abs() < 10.0,
        "還原之後的截圖是正常的畫面（{plain:.1}、{after:.1}）"
    );
}

/// 軟體繪圖的簡化流程不跑使用者的著色器：使用中的組合不送給 mpv，畫面跟沒有著色器一樣、也不會誤判成壞掉而還原
#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn shaders_are_not_used_in_dumb_mode() {
    let (invert, broken) = test_shaders("dumb");
    let (plain, _) = shot("shader-dumb-none", "{}", "gpu-dumb-mode=yes", DARK);
    for (name, file) in [("invert", &invert), ("broken", &broken)] {
        let (luma, log) = shot(
            &format!("shader-dumb-{name}"),
            &shader_settings(&[file]),
            "gpu-dumb-mode=yes",
            DARK,
        );
        assert!(
            (luma - plain).abs() < 10.0,
            "{name}：簡化流程不送著色器，畫面不變（{plain:.1} → {luma:.1}）"
        );
        assert!(!log.contains("像素著色器無法使用"), "{name}：\n{log}");
        let errors = render_errors(&log);
        assert!(errors.is_empty(), "{name}：畫面輸出出錯\n{}", errors.join("\n"));
    }
}

#[test]
fn reads_the_luma_from_the_log() {
    let log = "[vitascope] 截圖已存到 x.png\n[vitascope] 截圖統計：非黑色像素 99.1%，中央平均亮度 87.2\n";
    assert_eq!(center_luma(log), Some(87.2));
    assert_eq!(center_luma("[vitascope] 截圖存檔失敗"), None);
}

#[test]
fn reads_render_errors_and_set_properties_from_the_log() {
    let log = "[mpv/v] [cplayer] Set property: tone-mapping=\"hable\" -> 1\n\
               [mpv/v] [cplayer] Set property: target-peak=\"auto\" -> 1\n\
               [mpv/v] [cplayer] Set property: tone-mapping=gamma -> -2\n\
               [mpv/error] [libmpv_render] fragment shader source:\n\
               [mpv/error] [cplayer] Option af-add: 'x' isn't supported.\n\
               [mpv/v] [libmpv_render] shader compile log (status=1): ok\n";
    assert!(property_set(log, "tone-mapping", "hable"));
    assert!(property_set(log, "target-peak", "auto"));
    assert!(!property_set(log, "tone-mapping", "gamma"), "-> -2 是失敗");
    assert!(!property_set(log, "tone-mapping", "clip"));
    assert_eq!(
        render_errors(log),
        ["[mpv/error] [libmpv_render] fragment shader source:"]
    );
}
