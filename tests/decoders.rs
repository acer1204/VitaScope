//! 解不了的編碼（headless）：mpv 開不起某條軌道的解碼器時，播放器記下是哪個編碼、通知介面一次，其他的照常播；
//! 整個檔案開不起來時說明是哪個編碼。
//! - 引擎有解碼器、但開不起來（`vd=-` / `ad=-` 讓 mpv 不用任何解碼器，每個引擎都一樣）：「無法解碼 H.264（…）」
//! - 引擎沒有解碼器（AVS2：libdavs2 是 GPL，播放引擎、系統的 FFmpeg 都沒有）：「不支援的編碼：AVS2」。
//!   樣本要有 libxavs2 的 FFmpeg 才產生得了（`unsupported/`，見 scripts/gen_samples.py），沒有的平台略過
//! - MP4 裡的 TTML 字幕（FFmpeg 沒有解碼器）：記下、自動選字幕時跳過它，改選解得了的
//!
//! 同一套判斷在播放引擎（FFmpeg 9）與 Linux tar.gz 用的系統 libmpv 0.37（FFmpeg 6.1）上一樣：
//! 「引擎有沒有這個解碼器」看的是引擎自己的 `decoder-list`

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vitascope::mpv::Event;
use vitascope::player::{MissingDecoder, Options, Player, PlayerEvent, State, TrackKind};

/// CI 的機器比較慢：等 mpv 的條件都給足時間
const TIMEOUT: Duration = Duration::from_secs(20);

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

/// 這個平台的 FFmpeg 產生不了的樣本（沒有那個編碼器）：略過，不算失敗
fn optional_sample(rel: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated")
        .join(rel);
    if !p.exists() {
        eprintln!("略過 {rel}：這個 FFmpeg 產生不了");
        return None;
    }
    Some(p)
}

/// 每個測試各自的暫存資料夾
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vitascope-decoders-{}-{}-{name}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// 播完停在最後一格的 headless 播放器（`extra`：額外的 mpv 選項），開好 `path`
fn open(path: &std::path::Path, extra: &[(&str, &str)]) -> Player {
    vitascope::i18n::set_lang(vitascope::i18n::Lang::ZhTw);
    let mut p = Player::new(Options {
        keep_open: true,
        extra: extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
        ..Options::headless()
    })
    .unwrap();
    p.open(&path.to_string_lossy()).unwrap();
    p
}

/// 一直處理事件到狀態符合條件，回傳這段時間收到的事件（開檔失敗也繼續等：說明可能比 EndFile 晚補上）
fn run_until(p: &mut Player, what: &str, cond: impl Fn(&State) -> bool) -> Vec<PlayerEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    while !cond(&p.state) {
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}\n收到的事件：{events:?}\n目前狀態：{:#?}",
            p.state
        );
        if let Some(ev) = p.wait(Duration::from_millis(100)) {
            events.push(ev);
        }
    }
    events
}

/// 收到的「解不了」通知
fn notices(events: &[PlayerEvent]) -> Vec<&MissingDecoder> {
    events
        .iter()
        .filter_map(|e| match e {
            PlayerEvent::DecoderMissing(m) => Some(m),
            _ => None,
        })
        .collect()
}

fn missing(kind: TrackKind, codec: &str, unsupported: bool) -> MissingDecoder {
    MissingDecoder {
        kind,
        codec: codec.into(),
        unsupported,
        cover: false,
        quiet: false,
    }
}

/// 這個播放引擎有沒有這個編碼的解碼器（`decoder-list`）
fn engine_decodes(p: &Player, codec: &str) -> bool {
    let list: Vec<serde_json::Value> = serde_json::from_str(&p.get_string("decoder-list").unwrap()).unwrap();
    list.iter().any(|d| d["codec"] == codec)
}

/// 影像解不了（引擎有 H.264 的解碼器，只是開不起來）：通知一次、聲音照常播，介面當成純音訊檔
#[test]
fn undecodable_video_keeps_the_audio_playing() {
    let mut p = open(&sample("common/mp4_h264_aac.mp4"), &[("vd", "-")]);
    let events = run_until(&mut p, "影像解不了、聲音在播", |s| {
        s.loaded && !s.missing_decoders.is_empty() && s.time_pos > 0.5
    });
    let want = missing(TrackKind::Video, "h264", false);
    assert_eq!(notices(&events), [&want], "通知一次");
    assert_eq!(p.state.missing_decoders, std::slice::from_ref(&want));
    assert_eq!(p.state.last_error, None, "其他的照常播，不是開檔失敗");
    assert!(p.state.selected(TrackKind::Audio).is_some(), "聲音照常播");
    assert_eq!(p.get_string("vid").unwrap(), "no", "mpv 關掉了影片軌");
    assert!(!p.state.has_video(), "介面當成純音訊檔");
    assert_eq!(
        want.notice(true),
        "無法解碼 H.264 影像（檔案可能損壞，或用了引擎不支援的設定），只播放聲音"
    );
    // 再選一次影片軌（又失敗）：同一個檔案不再通知。mpv 0.37 是只播聲音重開的（見下面的測試），之前的記錄已經清掉：
    // 等到比現在多一筆
    let failures = |p: &Player| {
        p.recent_errors()
            .iter()
            .filter(|e| e.contains("Failed to initialize"))
            .count()
    };
    let before = failures(&p);
    p.select_track(TrackKind::Video, Some(1)).unwrap();
    let start = Instant::now();
    let mut later = Vec::new();
    while failures(&p) <= before {
        assert!(
            start.elapsed() < TIMEOUT,
            "等不到第二次失敗的記錄：{:?}",
            p.recent_errors()
        );
        later.extend(p.wait(Duration::from_millis(100)));
    }
    assert!(notices(&later).is_empty(), "{later:?}");
    assert_eq!(p.state.missing_decoders.len(), 1);
}

/// 聲音解不了：影像照常播
#[test]
fn undecodable_audio_keeps_the_video_playing() {
    let mut p = open(&sample("common/mp4_h264_aac.mp4"), &[("ad", "-")]);
    let events = run_until(&mut p, "聲音解不了、影像在播", |s| {
        s.loaded && !s.missing_decoders.is_empty() && s.time_pos > 0.5 && s.video_size.is_some()
    });
    let want = missing(TrackKind::Audio, "aac", false);
    assert_eq!(notices(&events), [&want]);
    assert_eq!(p.state.last_error, None);
    assert!(p.state.has_video());
    assert_eq!(p.get_string("aid").unwrap(), "no");
}

/// 只有影像、又解不了：整個檔案開不起來，說明是哪個編碼（不是「檔案裡沒有可播放的影像或聲音」）
#[test]
fn undecodable_video_only_file_says_which_codec() {
    let mut p = open(&sample("common/mp4_h264_noaudio.mp4"), &[("vd", "-")]);
    run_until(&mut p, "開檔失敗、說明是 H.264", |s| {
        s.last_error.as_deref().is_some_and(|e| e.contains("H.264"))
    });
    assert_eq!(
        p.state.last_error.as_deref(),
        Some("無法解碼 H.264（檔案可能損壞，或用了引擎不支援的設定）")
    );
}

/// 引擎沒有解碼器的影像編碼（AVS2）：不支援，聲音照常播
#[test]
fn unsupported_video_codec_plays_the_audio() {
    let Some(file) = optional_sample("unsupported/mkv_avs2_aac.mkv") else {
        return;
    };
    let mut p = open(&file, &[]);
    assert!(!engine_decodes(&p, "avs2"), "播放引擎不該有 AVS2 的解碼器（GPL）");
    let events = run_until(&mut p, "AVS2 解不了、聲音在播", |s| {
        s.loaded && !s.missing_decoders.is_empty() && s.time_pos > 0.5
    });
    let want = missing(TrackKind::Video, "avs2", true);
    assert_eq!(notices(&events), [&want]);
    assert_eq!(p.state.last_error, None);
    assert!(p.state.selected(TrackKind::Audio).is_some());
    assert_eq!(want.notice(true), "這個影片的影像編碼（AVS2）影戲不支援，只播放聲音");
}

/// 只有 AVS2 影像的檔案：開不起來，「不支援的編碼：AVS2」
#[test]
fn unsupported_video_only_file_says_unsupported() {
    let Some(file) = optional_sample("unsupported/mkv_avs2.mkv") else {
        return;
    };
    let mut p = open(&file, &[]);
    run_until(&mut p, "開檔失敗、說明是 AVS2", |s| {
        s.last_error.as_deref().is_some_and(|e| e.contains("AVS2"))
    });
    assert_eq!(p.state.last_error.as_deref(), Some("不支援的編碼：AVS2"));
}

/// H.266 / VVC：播放引擎（FFmpeg 9）有解碼器，系統的 libmpv 0.37（FFmpeg 6.1）沒有。
/// 同一個檔案，照引擎自己的 decoder-list：有的照常播，沒有的說不支援、聲音照常播
#[test]
fn vvc_plays_or_is_reported_by_the_engine_decoder_list() {
    let Some(file) = optional_sample("rare/mp4_vvc_aac.mp4") else {
        return;
    };
    let mut p = open(&file, &[]);
    if engine_decodes(&p, "vvc") {
        run_until(&mut p, "VVC 在播", |s| {
            s.loaded && s.time_pos > 1.0 && s.video_size.is_some()
        });
        assert!(p.state.missing_decoders.is_empty(), "{:?}", p.state.missing_decoders);
        assert!(p.state.has_video());
        return;
    }
    run_until(&mut p, "聲音在播", |s| s.loaded && s.time_pos > 0.5);
    if p.state.tracks_of(TrackKind::Video).next().is_none() {
        eprintln!("略過：這個引擎的分離器認不出 VVC 的影片軌");
        return;
    }
    run_until(&mut p, "VVC 解不了", |s| !s.missing_decoders.is_empty());
    // FFmpeg 6.1 的 MP4 分離器認不出 VVC：編碼名稱是空的（說「未知的編碼」）
    let codec = p.state.missing_decoders[0].codec.clone();
    assert!(codec == "vvc" || codec == "<?>", "{codec}");
    assert_eq!(p.state.missing_decoders, [missing(TrackKind::Video, &codec, true)]);
    assert_eq!(p.state.last_error, None);
}

/// MP4 裡的 TTML 字幕（stpp）：FFmpeg 沒有解碼器。標成預設的 TTML 那條解不了，自動選字幕改選 mov_text，
/// 不提示（不然這種檔案每次開都提示）；自己選 TTML 那條：通知一次
#[test]
fn ttml_in_mp4_is_skipped_for_a_subtitle_that_works() {
    let Some(file) = optional_sample("unsupported/mp4_h264_aac_ttml.mp4") else {
        return;
    };
    let mut p = open(&file, &[]);
    p.set_pause(true).unwrap();
    let mut events = run_until(&mut p, "選好字幕", |s| {
        s.loaded
            && s.selected(TrackKind::Sub)
                .is_some_and(|t| t.codec.as_deref() == Some("mov_text"))
    });
    // mpv 先選了標成預設的 TTML（解不了）：等記錄到了，確定沒有提示
    events.extend(run_until(&mut p, "記下 mpv 選的 TTML 解不了", |s| {
        s.missing_decoders.iter().any(|m| m.codec == "ttml")
    }));
    assert!(notices(&events).is_empty(), "開檔時自動選的不提示：{events:?}");
    let ttml = p
        .state
        .tracks_of(TrackKind::Sub)
        .find(|t| t.codec.as_deref() == Some("ttml"))
        .map(|t| t.id)
        .expect("有 TTML 的字幕軌");
    // 字幕照常顯示
    p.seek_to(2.0, true).unwrap();
    let start = Instant::now();
    while p.get_string("sub-text").unwrap_or_default().trim() != "影戲播放器測試" {
        assert!(start.elapsed() < TIMEOUT, "2 秒的字幕：{:?}", p.get_string("sub-text"));
        events.extend(p.wait(Duration::from_millis(50)));
    }
    // 自己選 TTML：mpv 開不起來、關掉它
    p.select_track(TrackKind::Sub, Some(ttml)).unwrap();
    events.extend(run_until(&mut p, "記下 TTML 解不了", |s| {
        s.missing_decoders.contains(&missing(TrackKind::Sub, "ttml", true))
    }));
    assert_eq!(notices(&events), [&missing(TrackKind::Sub, "ttml", true)], "通知一次");
    assert_eq!(p.state.last_error, None);
    assert!(p.state.has_video());
    assert!(!p.state.missing_decoders.iter().any(|m| m.quiet));

    // 再開一次：mpv 又先選 TTML（預設）也好、沒選也好，影戲自動選的一定是 mov_text，也不提示
    p.open(&file.to_string_lossy()).unwrap();
    let again = run_until(&mut p, "重開後選好字幕", |s| {
        s.loaded
            && s.selected(TrackKind::Sub)
                .is_some_and(|t| t.codec.as_deref() == Some("mov_text"))
    });
    assert!(notices(&again).is_empty(), "{again:?}");
}

/// 開檔時自動選的字幕解不了（記錄比載入完成晚到；表上沒有的格式）：改選別條解得了的。
/// 用影片旁邊的繁中 ASS（一定會被選上）當成解不了的那條，記錄由測試送（不靠 mpv 什麼時候送）
#[test]
fn auto_picked_subtitle_that_cannot_be_decoded_is_replaced() {
    let dir = TempDir::new("auto-sub");
    let video = dir.0.join("影片.mkv");
    std::fs::copy(sample("common/mkv_h264_aac_srt.mkv"), &video).unwrap();
    std::fs::write(
        dir.0.join("影片.zh-TW.ass"),
        "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,20\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
         Dialogue: 0,0:00:00.20,0:00:02.90,Default,,0,0,0,,繁中字幕\n",
    )
    .unwrap();
    let mut p = open(&video, &[]);
    run_until(&mut p, "選了外掛的繁中 ASS", |s| {
        s.loaded
            && s.selected(TrackKind::Sub)
                .is_some_and(|t| t.external && t.codec.as_deref() == Some("ass"))
    });
    let ev = p.inject_event(Event::Log {
        prefix: "sub".into(),
        level: "error".into(),
        text: "Could not find subtitle decoder for format 'ass'.\n".into(),
    });
    // 自動選的（不是使用者選的）：記下、不提示
    assert_eq!(ev, None);
    assert_eq!(
        p.state.missing_decoders,
        [MissingDecoder {
            quiet: true,
            ..missing(TrackKind::Sub, "ass", true)
        }]
    );
    run_until(&mut p, "改選內嵌的 SRT", |s| {
        s.selected(TrackKind::Sub)
            .is_some_and(|t| !t.external && t.codec.as_deref() == Some("subrip"))
    });
}

/// 這個檔案沒有的編碼（上一個檔案晚到的記錄）不算；有的才算
#[test]
fn decoder_logs_for_codecs_the_file_does_not_have_are_ignored() {
    let mut p = open(&sample("common/mp4_h264_aac.mp4"), &[]);
    run_until(&mut p, "開始播放", |s| {
        s.loaded && s.time_pos > 0.2 && s.has_video()
    });
    let log = |codec: &str| Event::Log {
        prefix: "vd".into(),
        level: "error".into(),
        text: format!("Failed to initialize a decoder for codec '{codec}'.\n"),
    };
    assert_eq!(p.inject_event(log("avs2")), None);
    assert!(p.state.missing_decoders.is_empty());
    assert!(p.state.has_video());
    assert_eq!(
        p.inject_event(log("h264")),
        Some(PlayerEvent::DecoderMissing(missing(TrackKind::Video, "h264", false)))
    );
    assert!(!p.state.has_video());
}

/// mpv 0.37（Linux tar.gz 的系統 libmpv）影像解不了就整個停下（載入完成、還沒開始播；聲音還沒開）：
/// 播放器不當成失敗，關掉影片軌重開同一個檔案，聲音照常播、「解不了」的記錄帶過去。
/// 這裡的引擎不會停下：載入完成的當下照 0.37 的順序送記錄與 EndFile（真的 0.37 由 Linux 的檢查跑上面的測試）
#[test]
fn video_failure_that_stops_the_file_is_retried_with_audio_only() {
    let mut p = open(&sample("common/mp4_h264_aac.mp4"), &[("vd", "-")]);
    // 0.37 自己就會停下（真的流程由上面的測試跑），再送一次假的會跟真的混在一起
    let version = p.get_string("mpv-version").unwrap();
    if version.starts_with("mpv 0.3") {
        eprintln!("略過：{version} 自己就會停下，由 undecodable_video_keeps_the_audio_playing 檢查");
        return;
    }
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    let _ = p.inject_event(Event::Log {
        prefix: "vd".into(),
        level: "error".into(),
        text: "Failed to initialize a decoder for codec 'h264'.\n".into(),
    });
    let stopped = || Event::EndFile {
        reason: vitascope::mpv::EndReason::Error,
        error: Some(vitascope::mpv::Error {
            code: -1,
            context: String::new(),
        }),
    };
    assert_eq!(p.inject_event(stopped()), None, "不通知介面開檔失敗");
    let events = run_until(&mut p, "只播聲音重開、聲音在播", |s| {
        s.loaded && s.time_pos > 0.5 && !s.missing_decoders.is_empty()
    });
    assert!(events.contains(&PlayerEvent::StartFile), "重開了：{events:?}");
    assert!(p.reopened_audio_only());
    assert!(p.playback_started());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PlayerEvent::EndFile { error: Some(_), .. })),
        "{events:?}"
    );
    assert_eq!(
        p.state.missing_decoders,
        [missing(TrackKind::Video, "h264", false)],
        "記錄帶過去"
    );
    assert_eq!(p.state.last_error, None);
    assert_eq!(p.get_string("vid").unwrap(), "no");
    assert!(!p.state.has_video());
    assert!(p.state.selected(TrackKind::Audio).is_some());
}

/// 同上，但解碼器的記錄比 EndFile 晚到（介面的事件迴圈常常這樣）：結束先留著，事件取完再決定，
/// 不通知介面開檔失敗
#[test]
fn late_video_failure_log_still_retries_with_audio_only() {
    let mut p = open(&sample("common/mp4_h264_aac.mp4"), &[("vd", "-")]);
    let version = p.get_string("mpv-version").unwrap();
    if version.starts_with("mpv 0.3") {
        eprintln!("略過：{version} 自己就會停下，由 undecodable_video_keeps_the_audio_playing 檢查");
        return;
    }
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    // 真的記錄可能已經收到了：清掉，照「記錄晚到」的順序重送
    p.state.missing_decoders.clear();
    let stopped = Event::EndFile {
        reason: vitascope::mpv::EndReason::Error,
        error: Some(vitascope::mpv::Error {
            code: -1,
            context: String::new(),
        }),
    };
    assert_eq!(p.inject_event(stopped), None, "結束先留著");
    let _ = p.inject_event(Event::Log {
        prefix: "vd".into(),
        level: "error".into(),
        text: "Failed to initialize a decoder for codec 'h264'.
"
        .into(),
    });
    let events = run_until(&mut p, "只播聲音重開、聲音在播", |s| {
        s.loaded && s.time_pos > 0.5 && !s.missing_decoders.is_empty()
    });
    assert!(events.contains(&PlayerEvent::StartFile), "重開了：{events:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PlayerEvent::EndFile { error: Some(_), .. })),
        "{events:?}"
    );
    assert_eq!(p.get_string("vid").unwrap(), "no");
}

/// 載入完成、還沒開始播就失敗，但不是影像解不了：留著的結束在事件取完時照常通知
#[test]
fn early_failure_without_a_decoder_problem_is_still_reported() {
    let mut p = open(&sample("common/mp4_h264_aac.mp4"), &[]);
    p.set_pause(true).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    let stopped = Event::EndFile {
        reason: vitascope::mpv::EndReason::Error,
        error: Some(vitascope::mpv::Error {
            code: -1,
            context: String::new(),
        }),
    };
    // FileLoaded 一處理完就送（PlaybackRestart 排在 FILE_LOADED 後面，還沒處理）：一定是「還沒開始播」
    assert!(!p.playback_started());
    assert_eq!(p.inject_event(stopped), None, "結束先留著");
    let ev = p
        .wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. }))
        .unwrap();
    assert!(
        matches!(&ev, PlayerEvent::EndFile { error: Some(e), .. } if e.starts_with("播放失敗")),
        "事件取完時通知開檔失敗：{ev:?}"
    );
}

/// mpv 自己的播放清單（本機的 .m3u 交給 mpv 播）裡的項目影像解不了、又照 0.37 的順序整個停下：
/// 不只播聲音重開（loadfile replace 會把 mpv 的清單整個換掉），照舊通知開檔失敗、清單還在
#[test]
fn early_failure_in_an_mpv_playlist_is_not_reopened() {
    let dir = TempDir::new("mpv-list");
    let list = dir.0.join("清單.m3u");
    let video = sample("common/mp4_h264_aac.mp4");
    std::fs::write(&list, format!("{}\n{}\n", video.display(), video.display())).unwrap();
    let mut p = open(&list, &[("vd", "-")]);
    let version = p.get_string("mpv-version").unwrap();
    if version.starts_with("mpv 0.3") {
        eprintln!("略過：{version} 自己就會停下，再送一次假的會跟真的混在一起");
        return;
    }
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded).unwrap();
    assert_eq!(p.get_string("playlist-count").unwrap(), "2");
    let _ = p.inject_event(Event::Log {
        prefix: "vd".into(),
        level: "error".into(),
        text: "Failed to initialize a decoder for codec 'h264'.\n".into(),
    });
    let stopped = Event::EndFile {
        reason: vitascope::mpv::EndReason::Error,
        error: Some(vitascope::mpv::Error {
            code: -1,
            context: String::new(),
        }),
    };
    let ev = match p.inject_event(stopped) {
        Some(ev) => ev,
        None => p
            .wait_for(TIMEOUT, |e| matches!(e, PlayerEvent::EndFile { .. }))
            .expect("事件取完時通知開檔失敗（或下一個開始之前先通知）"),
    };
    assert!(
        matches!(&ev, PlayerEvent::EndFile { error: Some(_), .. }),
        "照舊通知開檔失敗：{ev:?}"
    );
    assert!(!p.reopened_audio_only());
    assert_eq!(p.get_string("playlist-count").unwrap(), "2", "mpv 的清單還在");
}
