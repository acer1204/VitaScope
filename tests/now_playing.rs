//! macOS 的「正在播放」與遠端指令（`media_keys::macos`）：真的設定系統的 `nowPlayingInfo`、讀回來比對。
//!
//! - MediaPlayer.framework 的東西只能在主執行緒用，libtest 卻在別的執行緒跑測試：這個測試程式不用 libtest
//!   （Cargo.toml 的 `harness = false`），自己的 `main` 就在主執行緒，等系統的值時自己轉 run loop。
//! - 會出現在這台 Mac 的控制中心裡，所以只在 CI（`CI` 環境變數）或明確要求時（`VITASCOPE_MEDIA_TESTS=1`）跑；
//!   其他時候、其他系統顯示略過。
//! - 系統的遠端指令（真的按 F8、AirPods）測試送不出來：直接呼叫掛在指令上的處理（`MacSession::fire`），
//!   確認轉成正確的指令、叫醒介面。真的按鍵要在 Mac 上手動確認（ROADMAP 的手動確認）。

const NAME: &str = "now_playing_info_round_trip";

/// libtest 帶一個值（下一個參數）的選項：那個值不是名稱篩選
const VALUE_OPTIONS: [&str; 7] = [
    "--skip",
    "--test-threads",
    "--format",
    "--color",
    "--logfile",
    "--shuffle-seed",
    "-Z",
];

/// 照 libtest 的參數決定跑不跑：`--list`、名稱篩選（`--exact`）、`--skip <名稱>`、`--ignored`（這個測試不是 ignored 的）；
/// 帶值的選項（`--test-threads 1` 之類）的值不當成篩選。回傳 (只列出, 要跑)
fn selection(args: &[String]) -> (bool, bool) {
    let (mut list, mut ignored, mut exact) = (false, false, false);
    let (mut filters, mut skips) = (Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" => list = true,
            "--ignored" => ignored = true,
            "--exact" => exact = true,
            "--skip" => skips.extend(it.next().map(String::as_str)),
            o if VALUE_OPTIONS.contains(&o) => {
                it.next();
            }
            o if o.starts_with('-') => {}
            f => filters.push(f),
        }
    }
    let matches = |p: &str| if exact { NAME == p } else { NAME.contains(p) };
    let selected = (filters.is_empty() || filters.iter().any(|f| matches(f))) && !skips.iter().any(|s| matches(s));
    (list, selected && !ignored)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (list, selected) = selection(&args);
    if list {
        if selected {
            println!("{NAME}: test");
        }
        return;
    }
    if !selected {
        println!("\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out\n");
        return;
    }
    println!("\nrunning 1 test");
    match skip_reason() {
        Some(why) => {
            println!("test {NAME} ... ignored, {why}");
            println!("\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n");
        }
        None => {
            run();
            println!("test {NAME} ... ok");
            println!("\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n");
        }
    }
}

/// 不跑的原因（None = 要跑）
fn skip_reason() -> Option<&'static str> {
    if !cfg!(target_os = "macos") {
        return Some("macOS 才有「正在播放」");
    }
    let wanted = std::env::var_os("CI").is_some() || std::env::var_os("VITASCOPE_MEDIA_TESTS").is_some();
    (!wanted).then_some("會出現在這台 Mac 的控制中心：只在 CI 或 VITASCOPE_MEDIA_TESTS=1 時跑")
}

#[cfg(not(target_os = "macos"))]
fn run() {}

#[cfg(target_os = "macos")]
fn run() {
    mac::now_playing_info_round_trip();
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDate, NSDictionary, NSNumber, NSRunLoop, NSString};
    use objc2_media_player::{
        MPMediaItemArtwork, MPMediaItemPropertyAlbumArtist, MPMediaItemPropertyAlbumTitle,
        MPMediaItemPropertyAlbumTrackNumber, MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork,
        MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle, MPNowPlayingInfoCenter,
        MPNowPlayingInfoPropertyDefaultPlaybackRate, MPNowPlayingInfoPropertyElapsedPlaybackTime,
        MPNowPlayingInfoPropertyIsLiveStream, MPNowPlayingInfoPropertyMediaType, MPNowPlayingInfoPropertyPlaybackRate,
        MPNowPlayingPlaybackState, MPRemoteCommand, MPRemoteCommandCenter, MPRepeatType, MPShuffleType,
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};
    use vitascope::media_keys::macos::{self, Remote};
    use vitascope::media_keys::{
        Cover, MediaCommand, MediaKind, MediaSession, PlaybackInfo, Status, TrackInfo, Update, Wake,
    };
    use vitascope::playlist::Repeat;

    /// 慢的 CI 也要等得到
    const TIMEOUT: Duration = Duration::from_secs(15);

    /// 8×6 的紅色 PNG（封面）
    const PNG: [u8; 74] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x08, 0x00, 0x00, 0x00, 0x06, 0x08, 0x02, 0x00, 0x00, 0x00, 0x71, 0x67, 0x48, 0xac, 0x00, 0x00, 0x00,
        0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x38, 0xa1, 0xa1, 0x81, 0x15, 0x31, 0x0c, 0xa4, 0x04, 0x00,
        0x80, 0xf7, 0x34, 0x81, 0x38, 0x25, 0xbd, 0xe3, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42,
        0x60, 0x82,
    ];

    type Info = Retained<NSDictionary<NSString, AnyObject>>;

    fn center() -> Retained<MPNowPlayingInfoCenter> {
        // SAFETY: 主執行緒（這個測試程式的 main）
        unsafe { MPNowPlayingInfoCenter::defaultCenter() }
    }

    fn commands() -> Retained<MPRemoteCommandCenter> {
        // SAFETY: 主執行緒
        unsafe { MPRemoteCommandCenter::sharedCommandCenter() }
    }

    /// 讓主執行緒的 run loop 轉一下（系統的值可能要等它）
    fn spin() {
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
    }

    /// 一直轉 run loop，直到 `f` 回傳 Some
    fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
        let start = Instant::now();
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
            spin();
        }
    }

    fn info() -> Option<Info> {
        // SAFETY: 主執行緒
        unsafe { center().nowPlayingInfo() }
    }

    /// 等到系統的 nowPlayingInfo 符合條件，回傳它
    fn wait_info(what: &str, pred: impl Fn(&Info) -> bool) -> Info {
        wait_for(what, || info().filter(|d| pred(d)))
    }

    fn wait_state(state: MPNowPlayingPlaybackState) {
        // SAFETY: 主執行緒
        wait_for(&format!("playbackState = {state:?}"), || {
            (unsafe { center().playbackState() } == state).then_some(())
        });
    }

    fn text(d: &Info, key: &NSString) -> Option<String> {
        d.objectForKey(key)?.downcast::<NSString>().ok().map(|s| s.to_string())
    }

    fn number(d: &Info, key: &NSString) -> Option<f64> {
        d.objectForKey(key)?.downcast::<NSNumber>().ok().map(|n| n.as_f64())
    }

    fn has_artwork(d: &Info) -> bool {
        // SAFETY: 系統的常數字串
        let key = unsafe { MPMediaItemPropertyArtwork };
        d.objectForKey(key)
            .is_some_and(|a| a.downcast::<MPMediaItemArtwork>().is_ok())
    }

    fn title(d: &Info) -> Option<String> {
        // SAFETY: 系統的常數字串
        text(d, unsafe { MPMediaItemPropertyTitle })
    }

    fn track(file_gen: u64, kind: MediaKind, title: &str) -> TrackInfo {
        TrackInfo {
            file_gen,
            kind,
            title: title.into(),
            artist: Some("演出者 Artist".into()),
            album: Some("專輯 Album".into()),
            album_artist: Some("專輯演出者 Album Artist".into()),
            track_no: Some(3),
            duration: Some(60.0),
            url: None,
            details: true,
        }
    }

    fn playback(status: Status, position: f64) -> PlaybackInfo {
        PlaybackInfo {
            status,
            position,
            rate: 1.0,
            seekable: true,
            can_next: true,
            can_prev: false,
            shuffle: true,
            repeat: Repeat::All,
            volume: 1.0,
            buffering: false,
            can_play: true,
        }
    }

    /// 每個遠端指令應該掛在系統的哪個指令物件上（跟實作分開寫，對調了會抓到）
    fn expected_command(remote: Remote) -> Retained<MPRemoteCommand> {
        let c = commands();
        // SAFETY: 主執行緒；共用的指令物件
        unsafe {
            match remote {
                Remote::Play => c.playCommand(),
                Remote::Pause => c.pauseCommand(),
                Remote::TogglePlayPause => c.togglePlayPauseCommand(),
                Remote::Stop => c.stopCommand(),
                Remote::Next => c.nextTrackCommand(),
                Remote::Previous => c.previousTrackCommand(),
                Remote::SkipForward => Retained::into_super(c.skipForwardCommand()),
                Remote::SkipBackward => Retained::into_super(c.skipBackwardCommand()),
                Remote::Position => Retained::into_super(c.changePlaybackPositionCommand()),
                Remote::Rate => Retained::into_super(c.changePlaybackRateCommand()),
                Remote::Shuffle => Retained::into_super(c.changeShuffleModeCommand()),
                Remote::Repeat => Retained::into_super(c.changeRepeatModeCommand()),
            }
        }
    }

    /// 「往前 / 往後跳」的指令物件上設定的秒數
    fn intervals(forward: bool) -> Vec<f64> {
        let c = commands();
        // SAFETY: 主執行緒
        let list = unsafe {
            if forward {
                c.skipForwardCommand().preferredIntervals()
            } else {
                c.skipBackwardCommand().preferredIntervals()
            }
        };
        list.iter().map(|n| n.as_f64()).collect()
    }

    pub fn now_playing_info_round_trip() {
        // 不是主執行緒：不能建立（MediaPlayer 的東西只能在主執行緒用）
        let off_main = std::thread::spawn(|| macos::open(Arc::new(|| {})).err())
            .join()
            .unwrap();
        assert!(off_main.is_some(), "別的執行緒不能建立");

        let woke = Arc::new(AtomicUsize::new(0));
        let w = woke.clone();
        let wake: Wake = Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        let (mut session, rx) = macos::open(wake).expect("建立「正在播放」");

        // 每個遠端指令都掛在系統對應的那個指令物件上（例如往前跳不能掛到往後跳、F8 不能只掛到播放）
        for remote in Remote::ALL {
            let got = session
                .registered_command(remote)
                .unwrap_or_else(|| panic!("沒有掛上 {remote:?}"));
            assert!(
                Retained::as_ptr(&got) == Retained::as_ptr(&expected_command(remote)),
                "{remote:?} 掛在別的指令上"
            );
        }

        // 「往前 / 往後跳」的秒數跟著設定（兩個方向都是）
        session.set_skip_interval(7.0);
        assert_eq!(intervals(true), [7.0], "往前跳");
        assert_eq!(intervals(false), [7.0], "往後跳");

        // 跟 SessionSync 一樣的順序：檔案、封面、播放狀態
        session.apply(&Update::Track(track(1, MediaKind::Audio, "測試歌名 Song")));
        session.apply(&Update::Cover(Cover {
            file_gen: 1,
            png: Arc::new(PNG.to_vec()),
            path: PathBuf::from("cover-test-1.png"),
        }));
        session.apply(&Update::Playback {
            info: playback(Status::Playing, 12.0),
            seeked: true,
        });
        // 等到最後一次設定的整個字典（系統的值就算不是馬上讀得到，也不會比到中間的）
        // SAFETY: 系統的常數字串
        let elapsed = unsafe { MPNowPlayingInfoPropertyElapsedPlaybackTime };
        let rate = unsafe { MPNowPlayingInfoPropertyPlaybackRate };
        let d = wait_info("標題、進度", |d| {
            title(d).as_deref() == Some("測試歌名 Song") && number(d, elapsed) == Some(12.0)
        });
        // SAFETY: 系統的常數字串
        unsafe {
            assert_eq!(text(&d, MPMediaItemPropertyArtist).as_deref(), Some("演出者 Artist"));
            assert_eq!(text(&d, MPMediaItemPropertyAlbumTitle).as_deref(), Some("專輯 Album"));
            assert_eq!(
                text(&d, MPMediaItemPropertyAlbumArtist).as_deref(),
                Some("專輯演出者 Album Artist")
            );
            assert_eq!(number(&d, MPMediaItemPropertyAlbumTrackNumber), Some(3.0));
            assert_eq!(number(&d, MPMediaItemPropertyPlaybackDuration), Some(60.0));
            assert_eq!(number(&d, MPNowPlayingInfoPropertyElapsedPlaybackTime), Some(12.0));
            assert_eq!(number(&d, MPNowPlayingInfoPropertyPlaybackRate), Some(1.0));
            assert_eq!(number(&d, MPNowPlayingInfoPropertyDefaultPlaybackRate), Some(1.0));
            assert_eq!(number(&d, MPNowPlayingInfoPropertyMediaType), Some(1.0), "音訊");
            assert_eq!(number(&d, MPNowPlayingInfoPropertyIsLiveStream), Some(0.0));
        }
        assert!(has_artwork(&d), "封面：{d:?}");
        wait_state(MPNowPlayingPlaybackState::Playing);
        // 按鈕跟著能做的事；隨機、重複跟著影戲
        // SAFETY: 主執行緒
        unsafe {
            let c = commands();
            assert!(c.nextTrackCommand().isEnabled());
            assert!(!c.previousTrackCommand().isEnabled());
            assert!(c.togglePlayPauseCommand().isEnabled() && c.pauseCommand().isEnabled());
            assert!(c.changePlaybackPositionCommand().isEnabled());
            // 往前 / 往後跳不開（開了控制中心會把上一個 / 下一個換掉）
            assert!(!c.skipForwardCommand().isEnabled() && !c.skipBackwardCommand().isEnabled());
            assert_eq!(c.changeRepeatModeCommand().currentRepeatType(), MPRepeatType::All);
            assert_eq!(c.changeShuffleModeCommand().currentShuffleType(), MPShuffleType::Items);
        }

        // 同一個檔案換標題（標籤、ICY 標題變了）：重建的字典還有封面（S-B12）
        session.apply(&Update::Track(track(1, MediaKind::Audio, "新的標題 Renamed")));
        let d = wait_info("新的標題", |d| title(d).as_deref() == Some("新的標題 Renamed"));
        assert!(has_artwork(&d), "換標題之後封面還在：{d:?}");

        // 暫停：速度 0（系統不再往前推算位置）
        session.apply(&Update::Playback {
            info: playback(Status::Paused, 13.5),
            seeked: false,
        });
        let d = wait_info("暫停的速度", |d| number(d, rate) == Some(0.0));
        assert_eq!(number(&d, elapsed), Some(13.5));
        assert!(has_artwork(&d), "每次重建都有封面");
        wait_state(MPNowPlayingPlaybackState::Paused);

        // 系統的遠端指令 → 送給介面的指令，每個都叫醒介面
        let before = woke.load(Ordering::SeqCst);
        let fired = [
            (Remote::Play, f64::NAN, MediaCommand::Play),
            (Remote::TogglePlayPause, f64::NAN, MediaCommand::PlayPause),
            (Remote::Next, f64::NAN, MediaCommand::Next),
            (Remote::SkipBackward, f64::NAN, MediaCommand::SeekBy(-7.0)),
            (Remote::SkipForward, 15.0, MediaCommand::SeekBy(15.0)),
            (
                Remote::Position,
                30.0,
                MediaCommand::SeekTo {
                    secs: 30.0,
                    track: Some(1),
                },
            ),
            (Remote::Rate, 1.5, MediaCommand::SetRate(1.5)),
            (Remote::Shuffle, 0.0, MediaCommand::SetShuffle(false)),
            (Remote::Repeat, 1.0, MediaCommand::SetRepeat(Repeat::One)),
        ];
        for (remote, value, want) in &fired {
            assert!(session.fire(*remote, *value), "{remote:?}");
            assert_eq!(rx.try_recv().as_ref(), Ok(want), "{remote:?}");
        }
        assert!(!session.fire(Remote::Repeat, 9.0), "不認得的值：失敗");
        assert!(rx.try_recv().is_err());
        assert_eq!(woke.load(Ordering::SeqCst) - before, fired.len());

        // 換檔（影片、沒有封面）：上一個檔案的封面不見
        session.apply(&Update::Track(track(2, MediaKind::Video, "影片 Video")));
        session.apply(&Update::Playback {
            info: playback(Status::Playing, 0.0),
            seeked: true,
        });
        let d = wait_info("換檔", |d| title(d).as_deref() == Some("影片 Video"));
        assert!(!has_artwork(&d), "上一個檔案的封面：{d:?}");
        // SAFETY: 系統的常數字串
        assert_eq!(
            number(&d, unsafe { MPNowPlayingInfoPropertyMediaType }),
            Some(2.0),
            "影片"
        );

        // 停止：顯示停止、位置 0；暫停鈕不能按，播放鈕能按（按了開停止前的檔案）
        session.apply(&Update::Clear);
        wait_state(MPNowPlayingPlaybackState::Stopped);
        wait_info("停止：還顯示標題，位置與速度 0", |d| {
            title(d).as_deref() == Some("影片 Video") && number(d, elapsed) == Some(0.0) && number(d, rate) == Some(0.0)
        });
        // SAFETY: 主執行緒
        unsafe {
            assert!(!commands().pauseCommand().isEnabled());
            assert!(commands().playCommand().isEnabled());
        }

        // 播放中結束（例如在設定裡關掉）：先回到播放中，結束後一定要變成停止，不然 macOS 還當影戲在播
        session.apply(&Update::Playback {
            info: playback(Status::Playing, 2.0),
            seeked: true,
        });
        wait_state(MPNowPlayingPlaybackState::Playing);

        // 結束：系統不再顯示影戲，按鈕都關掉
        session.shutdown();
        wait_for("nowPlayingInfo 清掉", || info().is_none().then_some(()));
        wait_state(MPNowPlayingPlaybackState::Stopped);
        // SAFETY: 主執行緒
        unsafe {
            assert!(!commands().playCommand().isEnabled());
            assert!(!commands().nextTrackCommand().isEnabled());
        }
        drop(session);
    }
}
