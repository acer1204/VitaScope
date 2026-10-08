//! 最基本的 headless 測試：libmpv 能載入、能開檔、讀得到長度和編碼。
//! 樣本來自 `python scripts/gen_samples.py`。

use std::path::PathBuf;
use std::time::Duration;
use vitascope::player::{Options, Player, PlayerEvent, TrackKind};

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

#[test]
fn libmpv_loads_and_reports_version() {
    let player = Player::new(Options::headless()).expect("建立 mpv 失敗");
    let version = player.get_string("mpv-version").unwrap();
    let ffmpeg = player.get_string("ffmpeg-version").unwrap();
    println!("{version} / FFmpeg {ffmpeg}");
    assert!(version.starts_with("mpv"));
}

#[test]
fn mpv_clock_in_nanoseconds() {
    // Linux 執行時才找 mpv_get_time_ns（舊的 libmpv 才看得到版本太舊的說明）：要找得到，跟微秒的時鐘同一個基準
    assert!(vitascope::mpv::has_time_ns());
    let player = Player::new(Options::headless()).unwrap();
    let mpv = player.mpv();
    let (us, ns) = (mpv.time_us(), mpv.time_ns());
    assert!(ns > 0);
    assert!((ns / 1000 - us).abs() < 50_000, "{ns} ns / {us} µs");
    assert!(mpv.time_ns() >= ns, "不會倒退");
}

/// Linux：執行檔不能直接引用 client API 2.0（mpv 0.35）之後才有的函式（2.1 的 mpv_del_property、
/// 2.2 的 mpv_get_time_ns）。引用了的話，系統的 libmpv 太舊時在載入程式時就失敗，看不到 `Mpv::new` 的「版本太舊」說明
#[cfg(target_os = "linux")]
#[test]
fn binary_does_not_import_newer_mpv_functions() {
    let exe = env!("CARGO_BIN_EXE_vitascope");
    let out = match std::process::Command::new("nm")
        .args(["-D", "--undefined-only", exe])
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => {
            eprintln!("沒有 nm（binutils），略過");
            return;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // 「U mpv_create」；有版本的符號是「mpv_create@...」
    let imported: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().last())
        .map(|s| s.split('@').next().unwrap_or(s))
        .filter(|s| s.starts_with("mpv_"))
        .collect();
    assert!(imported.contains(&"mpv_create"), "動態連結 libmpv：{imported:?}");
    for newer in ["mpv_del_property", "mpv_get_time_ns"] {
        assert!(!imported.contains(&newer), "引用了 {newer}：{imported:?}");
    }
}

#[test]
fn opens_file_and_reads_basic_info() {
    let mut player = Player::new(Options::headless()).unwrap();
    let path = sample("common/mp4_h264_aac.mp4");
    player.open(path.to_str().unwrap()).unwrap();
    player
        .wait_for(Duration::from_secs(10), |e| *e == PlayerEvent::FileLoaded)
        .unwrap();
    // 屬性變化事件在 FileLoaded 之後才陸續送達，等到第一格解出來、尺寸也到了
    player
        .wait_for(Duration::from_secs(10), |e| *e == PlayerEvent::PlaybackRestart)
        .unwrap();
    player
        .wait_state(Duration::from_secs(5), |s| {
            s.video_size.is_some() && s.duration.is_some()
        })
        .unwrap();

    let s = &player.state;
    let duration = s.duration.expect("沒有讀到長度");
    assert!((duration - 3.0).abs() < 0.1, "長度 {duration}");
    assert_eq!(
        s.selected(TrackKind::Video).and_then(|t| t.codec.as_deref()),
        Some("h264")
    );
    assert_eq!(
        s.selected(TrackKind::Audio).and_then(|t| t.codec.as_deref()),
        Some("aac")
    );
    assert_eq!(s.video_size, Some([320, 240]));
}

#[test]
fn missing_file_reports_chinese_error() {
    let mut player = Player::new(Options::headless()).unwrap();
    player.open("Z:/不存在的資料夾/沒有這個檔案.mkv").unwrap();
    let err = player
        .wait_for(Duration::from_secs(10), |e| matches!(e, PlayerEvent::FileLoaded))
        .expect_err("不存在的檔案不應該載入成功");
    println!("錯誤訊息：{err}");
    assert!(err.contains("無法") || err.contains("失敗"), "{err}");
    assert_eq!(player.state.last_error.as_deref(), Some(err.as_str()));
}
