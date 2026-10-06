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
