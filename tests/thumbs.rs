//! 進度條預覽縮圖：另一個 mpv（軟體繪圖）在背景做縮圖。
//! 樣本來自 `python scripts/gen_samples.py`。

use std::path::PathBuf;
use std::time::{Duration, Instant};
use vitascope::thumbs::{Thumb, Thumbnailer, bucket_of};

fn sample(rel: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py",
        p.display()
    );
    p.to_string_lossy().into_owned()
}

/// 要一張縮圖並等它做好（第一張要等開檔，最多 10 秒）
fn get(t: &Thumbnailer, file: u64, time: f64, duration: f64) -> Thumb {
    let (bucket, target) = bucket_of(time, duration);
    let start = Instant::now();
    loop {
        // 檔案還沒載入時要求會被留著，載入後才開始；重送也沒關係（最新的贏）
        t.request(file, bucket, target);
        // 前一個檔案多做的縮圖（一直重送造成的）也可能還在佇列裡：看檔案編號
        if let Some(thumb) = t.try_recv()
            && thumb.bucket == bucket
            && thumb.file == file
        {
            return thumb;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "等不到 {time} 秒的縮圖");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn thumbnails_of_different_times_differ() {
    let t = Thumbnailer::new(|| {});
    t.open(1, &sample("common/mp4_long.mp4")); // 90 秒、160x90
    let a = get(&t, 1, 10.0, 90.0);
    assert_eq!((a.w, a.h), (240, 135));
    assert!(a.rgba.chunks_exact(4).all(|p| p[3] == 255), "不透明");
    let b = get(&t, 1, 60.0, 90.0);
    assert_ne!(a.rgba, b.rgba, "不同時間的畫面不一樣");
}

#[test]
fn phone_videos_are_turned_upright() {
    let t = Thumbnailer::new(|| {});
    t.open(1, &sample("common/mov_hevc_aac_rot90.mov")); // 320x240，檔案標示轉 90°
    let thumb = get(&t, 1, 1.0, 3.0);
    assert_eq!((thumb.w, thumb.h), (180, 240), "直的");
}

#[test]
fn switching_files_drops_old_requests() {
    let t = Thumbnailer::new(|| {});
    t.open(1, &sample("common/mp4_long.mp4"));
    let _ = get(&t, 1, 5.0, 90.0);
    t.open(2, &sample("common/mkv_multitrack.mkv")); // 640x360
    let thumb = get(&t, 2, 5.0, 20.0);
    assert_eq!((thumb.w, thumb.h), (240, 135), "新檔案的大小");
}

#[test]
fn requests_for_another_file_are_ignored() {
    let t = Thumbnailer::new(|| {});
    t.open(1, &sample("common/mp4_long.mp4"));
    let _ = get(&t, 1, 5.0, 90.0);
    // 介面已經換到下一個檔案（編號 2），縮圖產生器還在舊的檔案：不能拿舊檔案的畫面當新檔案的縮圖
    let (bucket, target) = bucket_of(30.0, 90.0);
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(800) {
        t.request(2, bucket, target);
        // 前面那張可能多做了幾次（`get` 一直重送），不算
        let made = t.try_recv().filter(|th| th.bucket == bucket);
        assert!(made.is_none(), "不應該做縮圖");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn thumbnails_come_back_after_an_idle_stop() {
    // 閒置 300 毫秒就停掉檔案（實際是 45 秒）
    let t = Thumbnailer::with_idle_stop(|| {}, Duration::from_millis(300));
    t.open(1, &sample("common/mp4_long.mp4"));
    let _ = get(&t, 1, 10.0, 90.0);
    std::thread::sleep(Duration::from_millis(1500));
    // 停掉之後又要縮圖：自己重新開檔
    let again = get(&t, 1, 60.0, 90.0);
    assert_eq!((again.w, again.h), (240, 135));
}
