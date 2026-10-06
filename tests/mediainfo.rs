//! 媒體資訊：用真的樣本檔（headless mpv）檢查讀到的格式資訊，三個平台的 mpv 版本都要對。
//! 樣本來自 `python scripts/gen_samples.py`。

use std::path::PathBuf;
use std::time::Duration;
use vitascope::mediainfo;
use vitascope::player::{Options, Player, PlayerEvent};

const TIMEOUT: Duration = Duration::from_secs(10);

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

/// 打開樣本、解出第一格，回傳面板上的文字
fn info_text(rel: &str) -> String {
    let mut p = Player::new(Options::headless()).unwrap();
    p.set_pause(true).unwrap();
    p.open(sample(rel).to_str().unwrap()).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::PlaybackRestart).unwrap();
    p.wait_state(TIMEOUT, |s| !s.tracks.is_empty() && s.duration.is_some())
        .unwrap();
    let info = mediainfo::read(&p, None);
    let live = mediainfo::read_live(&p);
    let text = mediainfo::to_text(&mediainfo::sections(&info, &live));
    println!("── {rel}\n{text}");
    text
}

#[test]
fn h264_mp4_basics() {
    let text = info_text("common/mp4_h264_aac.mp4");
    assert!(text.contains("mp4_h264_aac.mp4"), "{text}");
    assert!(text.contains("MP4 / MOV"), "{text}");
    assert!(text.contains("H.264"), "{text}");
    assert!(text.contains("320×240（4:3）"), "{text}");
    assert!(text.contains("8 bit 4:2:0"), "{text}");
    assert!(text.contains("SDR"), "{text}");
    assert!(text.contains("AAC"), "{text}");
    assert!(text.contains("kHz"), "{text}");
    assert!(text.contains("軟體解碼"), "headless 不用硬體解碼：{text}");
}

#[test]
fn hdr10_and_hlg_are_recognised() {
    let hdr10 = info_text("general/mkv_hevc10_hdr10.mkv");
    assert!(hdr10.contains("HDR10"), "{hdr10}");
    assert!(hdr10.contains("10 bit"), "{hdr10}");
    assert!(hdr10.contains("BT.2020 / PQ"), "{hdr10}");
    let hlg = info_text("general/mkv_hevc10_hlg.mkv");
    assert!(hlg.contains("BT.2020 / HLG"), "{hlg}");
    assert!(!hlg.contains("HLG · HLG"), "{hlg}");
}

#[test]
fn chapters_and_subtitles_are_listed() {
    let text = info_text("common/mkv_chapters.mkv");
    assert!(text.contains("3 個章節"), "{text}");
    let subs = info_text("common/mkv_multitrack.mkv");
    assert!(subs.contains("字幕"), "{subs}");
    assert!(subs.contains("內嵌"), "{subs}");
}

#[test]
fn album_art_is_not_shown_as_video() {
    let text = info_text("general/audio_mp3_cover.mp3");
    assert!(!text.contains("影像"), "專輯封面不算影像：{text}");
    assert!(text.contains("音訊"), "{text}");
    assert!(text.contains("MP3"), "{text}");
}
