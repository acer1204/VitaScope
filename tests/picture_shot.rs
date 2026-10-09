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

/// 設定檔：HDR 目標亮度（None = 自動）
fn peak_settings(peak: Option<u32>) -> String {
    match peak {
        Some(p) => format!(r#"{{"video": {{"tone": {{"target_peak": {p}}}}}}}"#),
        None => "{}".to_owned(),
    }
}

/// 記錄裡這次的 GL context 能不能做動態峰值偵測：「OpenGL 3.3 · GLSL 3.30；HDR 動態峰值偵測：不能用…」。
/// 引擎功能的記錄（`compute_peak: …`）要跟它一樣（建立 App 時讀 GL context 的結果真的進了 `caps`），
/// GLSL 4.20 以下、OpenGL ES 一定不能用（mpv 關掉 compute shader）
fn gl_compute_peak(log: &str) -> bool {
    let line = log
        .lines()
        .find(|l| l.contains("HDR 動態峰值偵測："))
        .unwrap_or_else(|| panic!("沒有記下 GL context 能不能做動態峰值偵測\n{log}"));
    let can = line.contains("動態峰值偵測：可以用");
    let caps = log
        .lines()
        .find(|l| l.contains("播放引擎功能："))
        .unwrap_or_else(|| panic!("沒有引擎功能的記錄\n{log}"));
    assert!(
        caps.contains(&format!("compute_peak: {can}")),
        "引擎功能跟 GL context 的結果不一樣：\n{line}\n{caps}"
    );
    let glsl = line
        .split("GLSL ")
        .nth(1)
        .map_or(0, vitascope::video::parse_glsl_version);
    if line.contains("OpenGL ES") || glsl < 420 {
        assert!(!can, "GLSL 4.20 以下、OpenGL ES 不能用：{line}");
    }
    eprintln!("{line}");
    can
}

/// 不會動的測試畫面：testsrc2 只播 1 秒，截圖時（3 秒）停在最後一格，兩次截圖的影像完全一樣，才能逐像素比較
///（這個 FFmpeg 沒有 loop 濾鏡）
const STILL: &str = "av://lavfi:testsrc2=size=640x360:rate=30:duration=1";

/// HDR 影片：目標亮度 100 nits 比自動（203 nits）亮（數字越小，HDR 畫面越亮）。
/// 色調映射在最後輸出到螢幕時做，軟體繪圖的簡化流程（Linux CI 的 llvmpipe）也一樣
fn hdr_peak_100_is_brighter(label: &str, mpv_opts: &str) {
    // 產生的 HDR10 樣本（mkv_hevc10_hdr10）幾乎都是 1000 nits 以上的亮部，100 跟 203 都壓到最亮、看不出差別
    //（RTX 3090 實測中央平均 141.7 / 141.5）；這個樣本的亮度大多在 203 nits 以下
    let media =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated/general/mkv_hevc10_hdr10_mid.mkv");
    assert!(
        media.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        media.display()
    );
    let media = media.to_string_lossy();
    let shot_peak = |peak: Option<u32>| {
        let tag = peak.map_or("auto".to_owned(), |p| p.to_string());
        let (luma, log) = shot(&format!("hdr-{label}-{tag}"), &peak_settings(peak), mpv_opts, &media);
        // 知道是 HDR 之後才送設定的目標亮度（開檔前是 auto）
        assert!(property_set(&log, "target-peak", &tag), "{tag}：\n{log}");
        let errors = render_errors(&log);
        assert!(errors.is_empty(), "{tag}：畫面輸出出錯\n{}", errors.join("\n"));
        gl_compute_peak(&log);
        luma
    };
    let (low, auto) = (shot_peak(Some(100)), shot_peak(None));
    // 實測（100 nits / 自動）：RTX 3090 114.9 / 101.3、簡化流程 114.0 / 100.4；
    // Linux 的 llvmpipe（本專案建置的引擎、系統的 libmpv 0.37 都是）108.5 / 87.2
    assert!(
        low > auto + 6.0,
        "{label}：目標亮度 100 nits 應該比自動（203 nits）亮（100 nits {low:.1}、自動 {auto:.1}）"
    );
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn hdr_target_peak_100_is_brighter_than_auto() {
    hdr_peak_100_is_brighter("gpu", "");
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn hdr_target_peak_applies_in_dumb_mode() {
    hdr_peak_100_is_brighter("dumb", "gpu-dumb-mode=yes");
}

/// 截圖檔案（`shot` 存的）→（寬, 高, RGBA）
fn read_png(path: &Path) -> (usize, usize, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    buf.truncate(info.buffer_size());
    (info.width as usize, info.height as usize, buf)
}

/// 兩張截圖中央一半（影片畫面；不含控制列的時間）每個像素每個顏色的（平均差, 最大差）
fn center_diff(a: &Path, b: &Path) -> (f64, u8) {
    let (w, h, pa) = read_png(a);
    let (w2, h2, pb) = read_png(b);
    assert_eq!((w, h), (w2, h2), "兩張截圖大小不一樣");
    let (mut sum, mut max, mut n) = (0u64, 0u8, 0u64);
    for y in h / 4..h * 3 / 4 {
        for x in w / 4..w * 3 / 4 {
            let i = (y * w + x) * 4;
            for c in 0..3 {
                let d = pa[i + c].abs_diff(pb[i + c]);
                sum += u64::from(d);
                max = max.max(d);
                n += 1;
            }
        }
    }
    (sum as f64 / n as f64, max)
}

/// `shot` 存的截圖
fn shot_png(name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join("vitascope-picture-shot")
        .join(name)
        .join("shot.png")
}

/// SDR 影片：目標亮度設成 100 跟自動的畫面一模一樣（目標亮度只對 HDR 影片送；
/// 以前會送給 SDR 影片，mpv 把 SDR 當成 203 nits，100 nits 時 SDR 影片也被色調映射、變亮變平）
fn sdr_ignores_target_peak(label: &str, mpv_opts: &str) {
    let auto = format!("sdr-{label}-auto");
    let low = format!("sdr-{label}-100");
    let (luma_auto, _) = shot(&auto, &peak_settings(None), mpv_opts, STILL);
    let (luma_low, log) = shot(&low, &peak_settings(Some(100)), mpv_opts, STILL);
    let (mean, max) = center_diff(&shot_png(&auto), &shot_png(&low));
    eprintln!("{label}：SDR 自動 {luma_auto:.1}、100 nits {luma_low:.1}；逐像素平均差 {mean:.3}、最大差 {max}");
    assert!(
        mean < 0.5 && max <= 3,
        "{label}：SDR 影片的畫面不能因為目標亮度改變（平均差 {mean:.3}、最大差 {max}）"
    );
    assert!(!property_set(&log, "target-peak", "100"), "SDR 影片不能送 100\n{log}");
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn sdr_video_ignores_the_target_peak() {
    sdr_ignores_target_peak("gpu", "");
}

#[test]
#[ignore = "會在螢幕上開視窗（約 10 秒）；在開發機或 CI 的虛擬螢幕上跑"]
fn sdr_video_ignores_the_target_peak_in_dumb_mode() {
    sdr_ignores_target_peak("dumb", "gpu-dumb-mode=yes");
}

/// 真正的 HDR10 影片（不放進專案：VITASCOPE_HDR_SAMPLES=資料夾，裡面要有 hdr10_hevc_1080p.mp4）：
/// 目標亮度 100 比自動亮；記下這台電腦的 OpenGL 能不能做動態峰值偵測
#[test]
#[ignore = "要真正的 HDR 影片：VITASCOPE_HDR_SAMPLES=資料夾；會在螢幕上開視窗（約 10 秒）"]
fn real_hdr10_clip_target_peak() {
    let Some(dir) = std::env::var_os("VITASCOPE_HDR_SAMPLES").map(std::path::PathBuf::from) else {
        eprintln!("略過：沒有設定 VITASCOPE_HDR_SAMPLES");
        return;
    };
    let media = dir.join("hdr10_hevc_1080p.mp4");
    assert!(media.exists(), "找不到 {}", media.display());
    let media = media.to_string_lossy();
    let (low, log) = shot("real-hdr10-100", &peak_settings(Some(100)), "", &media);
    let (auto, _) = shot("real-hdr10-auto", &peak_settings(None), "", &media);
    gl_compute_peak(&log);
    eprintln!("hdr10_hevc_1080p.mp4：目標亮度 100 nits {low:.1}、自動 {auto:.1}");
    assert!(property_set(&log, "target-peak", "100"), "{log}");
    assert!(low > auto, "100 nits 應該比自動亮（{low:.1}、{auto:.1}）");
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
