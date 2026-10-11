//! macOS 的系統媒體控制：「正在播放」（`MPNowPlayingInfoCenter`：控制中心、選單列、鎖定畫面顯示的片名、進度、封面）
//! 與遠端指令（`MPRemoteCommandCenter`：鍵盤的媒體鍵 F7–F9、AirPods 與耳機的按鍵、控制中心與 Touch Bar 的按鈕）。
//!
//! - 全部在主執行緒：[`MediaSession::apply`] 由介面的 `logic()` 呼叫，macOS 的介面執行緒就是主執行緒
//!   （winit 的事件迴圈只能在主執行緒跑）。建立時檢查（[`MainThreadMarker`]），工作階段帶著它所以不能送到別的執行緒。
//!   沒有背景執行緒：這些呼叫都很快，封面的 PNG 只在換封面時解碼一次。
//! - 遠端指令的處理在主執行緒的 run loop（winit 的 NSApplication）裡被呼叫：只轉成 [`MediaCommand`] 送進通道、叫醒介面，
//!   跟 Windows、Linux 走同一條路。系統只是「要求」；顯示的值等影戲真的改了、下一次更新時才變。
//! - `nowPlayingInfo` 每次都是整個換掉：每次重建的字典都放進封面（`MPMediaItemArtwork`），標題、標籤變了也不會掉（S-B12）。
//! - `playbackState` 一定要設：macOS 靠它決定哪個程式是「正在播放」、媒體鍵給誰。
//! - 播放中系統自己照速度往前推算位置（字典的 elapsed + rate × 經過的時間）：暫停、停止、等快取時速度給 0。
//! - 純邏輯（字典的內容、按鈕能不能按、系統的指令）不碰系統，在每個系統的測試都跑；真的呼叫系統的部分只有 macOS 有。
//! - `VITASCOPE_DEBUG_MEDIA=1`：印出系統送來的指令。

use super::{Cover, MediaCommand, MediaKind, PlaybackInfo, Status, TrackInfo, Update};
use crate::playlist::Repeat;

/// 「往前 / 往後跳」按鈕一開始的秒數（設定的「短跳轉」的預設；介面建立後會照設定改，[`super::MediaSession::set_skip_interval`]）
pub const DEFAULT_SKIP: f64 = 5.0;

/// 控制中心可以選的播放速度
pub const RATES: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// `MPNowPlayingInfoMediaType`：音訊
pub const MEDIA_TYPE_AUDIO: usize = 1;
/// `MPNowPlayingInfoMediaType`：影片
pub const MEDIA_TYPE_VIDEO: usize = 2;

/// `MPNowPlayingPlaybackState` 的值
pub fn playback_state(s: Status) -> usize {
    match s {
        Status::Playing => 1,
        Status::Paused => 2,
        Status::Stopped => 3,
    }
}

/// 重複播放 → `MPRepeatType`（關 / 單一檔案 / 全部 ↔ Off 0 / One 1 / All 2）
pub fn repeat_type(r: Repeat) -> isize {
    match r {
        Repeat::Off => 0,
        Repeat::One => 1,
        Repeat::All => 2,
    }
}

/// `MPRepeatType` → 重複播放（不認得的值 None）
pub fn repeat_from_type(t: isize) -> Option<Repeat> {
    match t {
        0 => Some(Repeat::Off),
        1 => Some(Repeat::One),
        2 => Some(Repeat::All),
        _ => None,
    }
}

/// 隨機播放 → `MPShuffleType`（Off 0 / Items 1）
pub fn shuffle_type(on: bool) -> isize {
    isize::from(on)
}

/// `MPShuffleType` → 隨機播放（Items、Collections 都算開；不認得的值 None）
pub fn shuffle_from_type(t: isize) -> Option<bool> {
    match t {
        0 => Some(false),
        1 | 2 => Some(true),
        _ => None,
    }
}

// ───────────── 系統的指令 ─────────────

/// 系統的遠端指令（`MPRemoteCommandCenter` 的哪一個）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    Play,
    Pause,
    TogglePlayPause,
    Stop,
    Next,
    Previous,
    /// 往前跳（值 = 系統給的秒數）
    SkipForward,
    /// 往後跳（值 = 系統給的秒數）
    SkipBackward,
    /// 拖動進度（值 = 秒）
    Position,
    /// 播放速度（值 = 速度）
    Rate,
    /// 隨機播放（值 = `MPShuffleType`）
    Shuffle,
    /// 重複播放（值 = `MPRepeatType`）
    Repeat,
}

impl Remote {
    pub const ALL: [Self; 12] = [
        Self::Play,
        Self::Pause,
        Self::TogglePlayPause,
        Self::Stop,
        Self::Next,
        Self::Previous,
        Self::SkipForward,
        Self::SkipBackward,
        Self::Position,
        Self::Rate,
        Self::Shuffle,
        Self::Repeat,
    ];
}

/// 系統的指令 → 送給介面的指令。`value`：事件帶的值（沒有、讀不到時 NaN）；`skip`：目前的「往前 / 往後跳」秒數
/// （事件沒帶秒數時用）；`shown_gen`：系統顯示的是哪個檔案（拖動進度時帶上，介面已經換檔時不理；0 = 沒有）。
/// 不能做的（不是數字、不認得的值）None：系統顯示失敗
pub fn command(remote: Remote, value: f64, skip: f64, shown_gen: u64) -> Option<MediaCommand> {
    let skip_by = |sign: f64| {
        let secs = if value.is_finite() && value > 0.0 { value } else { skip };
        (secs.is_finite() && secs > 0.0).then_some(MediaCommand::SeekBy(sign * secs))
    };
    match remote {
        Remote::Play => Some(MediaCommand::Play),
        Remote::Pause => Some(MediaCommand::Pause),
        Remote::TogglePlayPause => Some(MediaCommand::PlayPause),
        Remote::Stop => Some(MediaCommand::Stop),
        Remote::Next => Some(MediaCommand::Next),
        Remote::Previous => Some(MediaCommand::Previous),
        Remote::SkipForward => skip_by(1.0),
        Remote::SkipBackward => skip_by(-1.0),
        Remote::Position => value.is_finite().then(|| MediaCommand::SeekTo {
            secs: value.max(0.0),
            track: Some(shown_gen).filter(|&g| g != 0),
        }),
        Remote::Rate => value.is_finite().then_some(MediaCommand::SetRate(value)),
        Remote::Shuffle => to_type(value).and_then(shuffle_from_type).map(MediaCommand::SetShuffle),
        Remote::Repeat => to_type(value).and_then(repeat_from_type).map(MediaCommand::SetRepeat),
    }
}

/// 事件帶的列舉值（`MPShuffleType`、`MPRepeatType`）：整數才算
fn to_type(value: f64) -> Option<isize> {
    (value.is_finite() && value.fract() == 0.0 && value.abs() < 1e6).then_some(value as isize)
}

// ───────────── 系統顯示的內容 ─────────────

/// 「正在播放」字典的內容（純資料；macOS 上轉成 NSDictionary）
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    /// `MPMediaItemPropertyTitle`
    pub title: String,
    /// `MPMediaItemPropertyArtist`
    pub artist: Option<String>,
    /// `MPMediaItemPropertyAlbumTitle`
    pub album: Option<String>,
    /// `MPMediaItemPropertyAlbumArtist`
    pub album_artist: Option<String>,
    /// `MPMediaItemPropertyAlbumTrackNumber`
    pub track_no: Option<u32>,
    /// `MPMediaItemPropertyPlaybackDuration`（秒）；直播沒有
    pub duration: Option<f64>,
    /// `MPNowPlayingInfoPropertyIsLiveStream`：沒有總長度（直播）
    pub live: bool,
    /// `MPNowPlayingInfoPropertyElapsedPlaybackTime`（秒）
    pub elapsed: f64,
    /// `MPNowPlayingInfoPropertyPlaybackRate`：暫停、停止、等快取時 0（系統才不會自己把位置往前推）
    pub rate: f64,
    /// `MPNowPlayingInfoPropertyMediaType`（[`MEDIA_TYPE_AUDIO`] / [`MEDIA_TYPE_VIDEO`]）
    pub media_type: usize,
    /// 放進封面（`MPMediaItemPropertyArtwork`）：目前檔案的、顯示片名與封面時
    pub artwork: bool,
    /// `playbackState`（[`playback_state`]）
    pub state: usize,
}

/// 遠端指令的按鈕能不能按、隨機與重複目前的值
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controls {
    /// 播放、播放／暫停（有檔案，或停止之後按播放有東西可以開）
    pub play: bool,
    /// 暫停、停止（有檔案）
    pub pause: bool,
    pub next: bool,
    pub previous: bool,
    /// 拖動進度、往前 / 往後跳（有檔案、能跳轉）
    pub seek: bool,
    /// 改速度（有檔案）
    pub rate: bool,
    /// `MPShuffleType`
    pub shuffle: isize,
    /// `MPRepeatType`
    pub repeat: isize,
}

impl Controls {
    pub fn of(p: &PlaybackInfo) -> Self {
        let loaded = p.status != Status::Stopped;
        Self {
            play: p.can_play || loaded,
            pause: loaded,
            next: p.can_next,
            previous: p.can_prev,
            seek: loaded && p.seekable,
            rate: loaded,
            shuffle: shuffle_type(p.shuffle),
            repeat: repeat_type(p.repeat),
        }
    }

    /// 這個按鈕能不能按（隨機、重複一直都能按）
    pub fn enabled(&self, remote: Remote) -> bool {
        match remote {
            Remote::Play | Remote::TogglePlayPause => self.play,
            Remote::Pause | Remote::Stop => self.pause,
            Remote::Next => self.next,
            Remote::Previous => self.previous,
            // 往前 / 往後跳一直不能按：能按時控制中心、鎖定畫面會把「上一個 / 下一個」換成「± N 秒」，
            // F7 / F9、AirPods 也可能跟著變成跳秒，跟 Windows、Linux（換檔）不一樣。指令還是掛著（秒數跟著設定），之後要開只改這裡
            Remote::SkipForward | Remote::SkipBackward => false,
            Remote::Position => self.seek,
            Remote::Rate => self.rate,
            Remote::Shuffle | Remote::Repeat => true,
        }
    }
}

/// 系統目前顯示的檔案、播放狀態、封面（每個 [`Update`] 之後重建整個字典）
#[derive(Debug, Default)]
pub struct Shown {
    track: Option<TrackInfo>,
    playback: Option<PlaybackInfo>,
    cover: Option<Cover>,
}

impl Shown {
    /// 套用一個更新
    pub fn apply(&mut self, update: &Update) {
        match update {
            Update::Track(t) => {
                // 換了檔案：上一個檔案的封面不能用。同一個檔案換標題時留著（S-B12）
                if self.cover.as_ref().is_some_and(|c| c.file_gen != t.file_gen) {
                    self.cover = None;
                }
                self.track = Some(t.clone());
            }
            Update::Playback { info, .. } => self.playback = Some(info.clone()),
            Update::Cover(c) => {
                if self.track.as_ref().is_some_and(|t| t.file_gen == c.file_gen) {
                    self.cover = Some(c.clone());
                }
            }
            // 停止：跟 Windows 一樣留著標題（按播放鍵會再開它），顯示停止、位置 0
            Update::Clear => {
                if let Some(p) = &mut self.playback {
                    *p = p.cleared();
                }
            }
        }
    }

    /// 系統顯示的是哪個檔案（拖動進度時帶上；停止了、還沒有檔案時 0）
    pub fn shown_gen(&self) -> u64 {
        let stopped = self.playback.as_ref().is_some_and(|p| p.status == Status::Stopped);
        match &self.track {
            Some(t) if !stopped => t.file_gen,
            _ => 0,
        }
    }

    /// 要放進字典的封面（目前檔案的、顯示片名與封面時）
    pub fn cover(&self) -> Option<&Cover> {
        let t = self.track.as_ref()?;
        self.cover.as_ref().filter(|c| c.file_gen == t.file_gen && t.details)
    }

    /// 按鈕的狀態；還沒送過播放狀態時 None
    pub fn controls(&self) -> Option<Controls> {
        self.playback.as_ref().map(Controls::of)
    }

    /// 字典的內容；還沒有任何檔案時 None（不顯示）
    pub fn now_playing(&self) -> Option<NowPlaying> {
        let t = self.track.as_ref()?;
        let duration = t.duration.filter(|d| d.is_finite() && *d > 0.0);
        let (status, position, rate, buffering) =
            self.playback.as_ref().map_or((Status::Paused, 0.0, 1.0, false), |p| {
                (p.status, p.position, p.rate, p.buffering)
            });
        let elapsed = if status == Status::Stopped || !position.is_finite() {
            0.0
        } else {
            let p = position.max(0.0);
            duration.map_or(p, |d| p.min(d))
        };
        let rate = if status == Status::Playing && !buffering {
            if rate.is_finite() && rate > 0.0 { rate } else { 1.0 }
        } else {
            0.0
        };
        Some(NowPlaying {
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            album_artist: t.album_artist.clone(),
            track_no: t.track_no,
            duration,
            live: duration.is_none(),
            elapsed,
            rate,
            media_type: match t.kind {
                MediaKind::Audio => MEDIA_TYPE_AUDIO,
                MediaKind::Video => MEDIA_TYPE_VIDEO,
            },
            artwork: self.cover().is_some(),
            state: playback_state(status),
        })
    }
}

// ───────────── 系統（只有 macOS）─────────────

#[cfg(target_os = "macos")]
pub use platform::{MacSession, create, open};

#[cfg(target_os = "macos")]
mod platform {
    use super::{Controls, DEFAULT_SKIP, NowPlaying, RATES, Remote, Shown};
    use crate::media_keys::{Cover, MediaCommand, MediaSession, Opened, Update, Wake};
    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::NSImage;
    use objc2_foundation::{NSArray, NSData, NSMutableDictionary, NSNumber, NSSize, NSString};
    use objc2_media_player::{
        MPChangePlaybackPositionCommandEvent, MPChangePlaybackRateCommandEvent, MPChangeRepeatModeCommandEvent,
        MPChangeShuffleModeCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumArtist,
        MPMediaItemPropertyAlbumTitle, MPMediaItemPropertyAlbumTrackNumber, MPMediaItemPropertyArtist,
        MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle,
        MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyDefaultPlaybackRate,
        MPNowPlayingInfoPropertyElapsedPlaybackTime, MPNowPlayingInfoPropertyIsLiveStream,
        MPNowPlayingInfoPropertyMediaType, MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState,
        MPRemoteCommand, MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus, MPRepeatType,
        MPShuffleType, MPSkipIntervalCommandEvent,
    };
    use std::cell::Cell;
    use std::ptr::NonNull;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver, Sender};

    fn debug_enabled() -> bool {
        std::env::var_os("VITASCOPE_DEBUG_MEDIA").is_some()
    }

    /// 建立「正在播放」的工作階段（要在主執行緒呼叫：介面的執行緒）
    pub fn create(wake: Wake) -> Result<Opened, String> {
        let (session, rx) = open(wake)?;
        Ok((Box::new(session), rx))
    }

    /// [`create`] 本身（測試直接用 [`MacSession`]：送假的遠端指令、看按鈕的狀態）
    pub fn open(wake: Wake) -> Result<(MacSession, Receiver<MediaCommand>), String> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(crate::tr!(
                "macOS 的「正在播放」只能在主執行緒建立",
                "macOS Now Playing can only be set up on the main thread"
            )
            .to_owned());
        };
        let (tx, rx) = mpsc::channel();
        let out = Out {
            tx,
            wake,
            debug: debug_enabled(),
        };
        // SAFETY: 系統共用的物件，主執行緒上取得
        let (center, info) = unsafe {
            (
                MPRemoteCommandCenter::sharedCommandCenter(),
                MPNowPlayingInfoCenter::defaultCenter(),
            )
        };
        let mut session = MacSession {
            center,
            info,
            handlers: Vec::new(),
            shown: Shown::default(),
            artwork: None,
            skip: Rc::new(Cell::new(DEFAULT_SKIP)),
            shown_gen: Rc::new(Cell::new(0)),
            ended: false,
            _main: mtm,
        };
        for remote in Remote::ALL {
            session.add(remote, &out);
        }
        session.set_skip(DEFAULT_SKIP);
        // SAFETY: 主執行緒；陣列裡都是 NSNumber
        unsafe {
            let rates: Vec<Retained<NSNumber>> = RATES.iter().map(|&r| NSNumber::new_f64(r)).collect();
            session
                .center
                .changePlaybackRateCommand()
                .setSupportedPlaybackRates(&NSArray::from_retained_slice(&rates));
        }
        Ok((session, rx))
    }

    /// 把系統的要求送給介面
    #[derive(Clone)]
    struct Out {
        tx: Sender<MediaCommand>,
        wake: Wake,
        debug: bool,
    }

    impl Out {
        fn send(&self, cmd: MediaCommand) {
            if self.debug {
                eprintln!("[media] 正在播放 → {cmd:?}");
            }
            if self.tx.send(cmd).is_ok() {
                (self.wake)();
            }
        }
    }

    /// 一個遠端指令的處理：參數是事件帶的值（NaN = 沒有）
    type Handler = Rc<dyn Fn(f64) -> MPRemoteCommandHandlerStatus>;

    /// 做好的封面：哪個檔案、哪一份 PNG、做出來的 `MPMediaItemArtwork`（PNG 解不開時 None）
    type Artwork = (u64, Arc<Vec<u8>>, Option<Retained<MPMediaItemArtwork>>);

    /// 掛在遠端指令上的處理（結束時拿掉）
    struct Registered {
        remote: Remote,
        command: Retained<MPRemoteCommand>,
        /// `addTargetWithHandler` 回傳的對象（拿掉時要用）
        target: Retained<AnyObject>,
        handler: Handler,
    }

    /// macOS 的「正在播放」工作階段。只能在主執行緒用（帶著 [`MainThreadMarker`]，不能送到別的執行緒）
    pub struct MacSession {
        center: Retained<MPRemoteCommandCenter>,
        info: Retained<MPNowPlayingInfoCenter>,
        handlers: Vec<Registered>,
        shown: Shown,
        /// 目前封面做好的 `MPMediaItemArtwork`（哪個檔案、哪一份 PNG）：每次重建字典都放進去，不重新解碼
        artwork: Option<Artwork>,
        /// 「往前 / 往後跳」的秒數（事件沒帶秒數時用）
        skip: Rc<Cell<f64>>,
        /// 系統顯示的是哪個檔案（拖動進度時帶上）
        shown_gen: Rc<Cell<u64>>,
        ended: bool,
        _main: MainThreadMarker,
    }

    impl MacSession {
        fn remote_command(&self, remote: Remote) -> Retained<MPRemoteCommand> {
            let c = &self.center;
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

        fn add(&mut self, remote: Remote, out: &Out) {
            let command = self.remote_command(remote);
            let (o, skip, shown_gen) = (out.clone(), self.skip.clone(), self.shown_gen.clone());
            let handler: Handler =
                Rc::new(
                    move |value| match super::command(remote, value, skip.get(), shown_gen.get()) {
                        Some(cmd) => {
                            o.send(cmd);
                            MPRemoteCommandHandlerStatus::Success
                        }
                        None => MPRemoteCommandHandlerStatus::CommandFailed,
                    },
                );
            let h = handler.clone();
            let block = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
                // SAFETY: 系統給的事件在這次呼叫期間有效
                h(event_value(remote, unsafe { event.as_ref() }))
            });
            // SAFETY: 主執行緒；系統會保留（複製）這個 block
            let target = unsafe {
                let target = command.addTargetWithHandler(&block);
                // 還沒有檔案：等第一個播放狀態再決定（播放鍵先打開）
                command.setEnabled(matches!(
                    remote,
                    Remote::Play | Remote::TogglePlayPause | Remote::Shuffle | Remote::Repeat
                ));
                target
            };
            self.handlers.push(Registered {
                remote,
                command,
                target,
                handler,
            });
        }

        /// 測試用：像系統送來了遠端指令（`value` = 事件帶的值，NaN = 沒有）；回傳處理的結果是不是成功
        #[doc(hidden)]
        pub fn fire(&self, remote: Remote, value: f64) -> bool {
            self.handlers
                .iter()
                .find(|r| r.remote == remote)
                .is_some_and(|r| (r.handler)(value) == MPRemoteCommandHandlerStatus::Success)
        }

        /// 測試用：這個遠端指令的處理掛在哪個系統的指令物件上（沒有掛時 None）
        #[doc(hidden)]
        pub fn registered_command(&self, remote: Remote) -> Option<Retained<MPRemoteCommand>> {
            self.handlers
                .iter()
                .find(|r| r.remote == remote)
                .map(|r| r.command.clone())
        }

        fn set_skip(&self, secs: f64) {
            self.skip.set(secs);
            let n = [NSNumber::new_f64(secs)];
            let intervals = NSArray::from_retained_slice(&n);
            // SAFETY: 主執行緒；陣列裡是 NSNumber
            unsafe {
                self.center.skipForwardCommand().setPreferredIntervals(&intervals);
                self.center.skipBackwardCommand().setPreferredIntervals(&intervals);
            }
        }

        fn set_controls(&self, c: &Controls) {
            for r in &self.handlers {
                // SAFETY: 主執行緒
                unsafe { r.command.setEnabled(c.enabled(r.remote)) };
            }
            // SAFETY: 主執行緒
            unsafe {
                self.center
                    .changeShuffleModeCommand()
                    .setCurrentShuffleType(MPShuffleType(c.shuffle));
                self.center
                    .changeRepeatModeCommand()
                    .setCurrentRepeatType(MPRepeatType(c.repeat));
            }
        }

        /// 目前封面的 `MPMediaItemArtwork`（同一份 PNG 只解碼一次；解不開時 None）
        fn artwork(&mut self, cover: &Cover) -> Option<Retained<MPMediaItemArtwork>> {
            let same = self
                .artwork
                .as_ref()
                .is_some_and(|(g, png, _)| *g == cover.file_gen && Arc::ptr_eq(png, &cover.png));
            if !same {
                self.artwork = Some((cover.file_gen, cover.png.clone(), make_artwork(&cover.png)));
            }
            self.artwork.as_ref().and_then(|(_, _, a)| a.clone())
        }

        /// 照 [`Shown`] 重建整個字典、設定播放狀態
        fn publish(&mut self) {
            let Some(np) = self.shown.now_playing() else {
                return;
            };
            let cover = self.shown.cover().cloned();
            let art = cover.as_ref().and_then(|c| self.artwork(c));
            if cover.is_none() {
                self.artwork = None;
            }
            let dict = info_dictionary(&np, art.as_deref());
            // SAFETY: 主執行緒；字典的值都是 NSString、NSNumber、MPMediaItemArtwork
            unsafe {
                self.info.setNowPlayingInfo(Some(&**dict));
                self.info.setPlaybackState(MPNowPlayingPlaybackState(np.state));
            }
        }
    }

    impl MediaSession for MacSession {
        fn apply(&mut self, update: &Update) {
            if self.ended {
                return;
            }
            self.shown.apply(update);
            self.shown_gen.set(self.shown.shown_gen());
            if matches!(update, Update::Playback { .. } | Update::Clear)
                && let Some(c) = self.shown.controls()
            {
                self.set_controls(&c);
            }
            self.publish();
        }

        fn set_skip_interval(&mut self, secs: f64) {
            if !self.ended && secs.is_finite() && secs > 0.0 {
                self.set_skip(secs);
            }
        }

        fn shutdown(&mut self) {
            if std::mem::replace(&mut self.ended, true) {
                return;
            }
            for r in self.handlers.drain(..) {
                // SAFETY: 主執行緒；target 是 addTargetWithHandler 回傳的
                unsafe {
                    r.command.removeTarget(Some(&r.target));
                    r.command.setEnabled(false);
                }
            }
            self.artwork = None;
            // SAFETY: 主執行緒
            unsafe {
                self.info.setNowPlayingInfo(None);
                self.info.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
            }
        }
    }

    impl Drop for MacSession {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    /// 事件帶的值（跳的秒數、拖到的位置、速度、隨機、重複）；沒有、不是那種事件時 NaN
    fn event_value(remote: Remote, event: &MPRemoteCommandEvent) -> f64 {
        let any: &AnyObject = event;
        // SAFETY: 讀系統給的事件的屬性（這次呼叫期間有效）
        let value = unsafe {
            match remote {
                Remote::SkipForward | Remote::SkipBackward => {
                    any.downcast_ref::<MPSkipIntervalCommandEvent>().map(|e| e.interval())
                }
                Remote::Position => any
                    .downcast_ref::<MPChangePlaybackPositionCommandEvent>()
                    .map(|e| e.positionTime()),
                Remote::Rate => any
                    .downcast_ref::<MPChangePlaybackRateCommandEvent>()
                    .map(|e| f64::from(e.playbackRate())),
                Remote::Shuffle => any
                    .downcast_ref::<MPChangeShuffleModeCommandEvent>()
                    .map(|e| e.shuffleType().0 as f64),
                Remote::Repeat => any
                    .downcast_ref::<MPChangeRepeatModeCommandEvent>()
                    .map(|e| e.repeatType().0 as f64),
                _ => None,
            }
        };
        value.unwrap_or(f64::NAN)
    }

    /// PNG → 系統要的封面；解不開時 None
    fn make_artwork(png: &[u8]) -> Option<Retained<MPMediaItemArtwork>> {
        let data = NSData::with_bytes(png);
        let image = NSImage::initWithData(NSImage::alloc(), &data)?;
        let size: NSSize = image.size();
        if !(size.width > 0.0 && size.height > 0.0) {
            return None;
        }
        // 系統要哪個大小都給同一張（NSImage 自己會縮放）；block 留著圖片，回傳的指標一直有效
        let handler = RcBlock::new(move |_: NSSize| -> NonNull<NSImage> { NonNull::from(&*image) });
        // SAFETY: handler 回傳的是 block 自己留著的圖片
        Some(unsafe {
            MPMediaItemArtwork::initWithBoundsSize_requestHandler(MPMediaItemArtwork::alloc(), size, &handler)
        })
    }

    /// [`NowPlaying`] → `nowPlayingInfo` 的字典
    fn info_dictionary(
        np: &NowPlaying,
        artwork: Option<&MPMediaItemArtwork>,
    ) -> Retained<NSMutableDictionary<NSString, AnyObject>> {
        let dict = NSMutableDictionary::<NSString, AnyObject>::new();
        let text = |key: &NSString, value: &str| dict.insert(key, &*NSString::from_str(value));
        let number = |key: &NSString, value: Retained<NSNumber>| dict.insert(key, &*value);
        // SAFETY: 系統的常數字串（MediaPlayer.framework 的 extern static）
        unsafe {
            text(MPMediaItemPropertyTitle, &np.title);
            if let Some(a) = &np.artist {
                text(MPMediaItemPropertyArtist, a);
            }
            if let Some(a) = &np.album {
                text(MPMediaItemPropertyAlbumTitle, a);
            }
            if let Some(a) = &np.album_artist {
                text(MPMediaItemPropertyAlbumArtist, a);
            }
            if let Some(n) = np.track_no {
                number(MPMediaItemPropertyAlbumTrackNumber, NSNumber::new_u32(n));
            }
            if let Some(d) = np.duration {
                number(MPMediaItemPropertyPlaybackDuration, NSNumber::new_f64(d));
            }
            number(MPNowPlayingInfoPropertyIsLiveStream, NSNumber::new_bool(np.live));
            number(
                MPNowPlayingInfoPropertyElapsedPlaybackTime,
                NSNumber::new_f64(np.elapsed),
            );
            number(MPNowPlayingInfoPropertyPlaybackRate, NSNumber::new_f64(np.rate));
            number(MPNowPlayingInfoPropertyDefaultPlaybackRate, NSNumber::new_f64(1.0));
            number(MPNowPlayingInfoPropertyMediaType, NSNumber::new_usize(np.media_type));
            if let Some(art) = artwork {
                dict.insert(MPMediaItemPropertyArtwork, art);
            }
        }
        dict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn track(file_gen: u64, details: bool) -> TrackInfo {
        TrackInfo {
            file_gen,
            kind: MediaKind::Audio,
            title: "歌".into(),
            artist: Some("人".into()),
            album: Some("專輯".into()),
            album_artist: None,
            track_no: Some(3),
            duration: Some(60.0),
            url: None,
            details,
        }
    }

    fn playback(status: Status, position: f64) -> PlaybackInfo {
        PlaybackInfo {
            status,
            position,
            rate: 1.5,
            seekable: true,
            can_next: true,
            can_prev: false,
            shuffle: false,
            repeat: Repeat::Off,
            volume: 1.0,
            buffering: false,
            can_play: true,
        }
    }

    fn cover(file_gen: u64) -> Cover {
        Cover {
            file_gen,
            png: Arc::new(vec![1, 2, 3]),
            path: PathBuf::from(format!("cover-1-{file_gen}.png")),
        }
    }

    #[test]
    fn repeat_and_shuffle_map_to_media_player_types() {
        for r in [Repeat::Off, Repeat::One, Repeat::All] {
            assert_eq!(repeat_from_type(repeat_type(r)), Some(r));
        }
        // MPRepeatType：Off 0、One 1、All 2；MPShuffleType：Off 0、Items 1、Collections 2
        assert_eq!(repeat_type(Repeat::All), 2);
        assert_eq!(repeat_from_type(3), None);
        assert_eq!(shuffle_type(true), 1);
        assert_eq!(shuffle_from_type(shuffle_type(false)), Some(false));
        assert_eq!(shuffle_from_type(2), Some(true), "Collections 也算開");
        assert_eq!(shuffle_from_type(-1), None);
        // MPNowPlayingPlaybackState：Playing 1、Paused 2、Stopped 3
        assert_eq!(playback_state(Status::Playing), 1);
        assert_eq!(playback_state(Status::Paused), 2);
        assert_eq!(playback_state(Status::Stopped), 3);
    }

    #[test]
    fn remote_commands_map_to_media_commands() {
        use MediaCommand as M;
        let nan = f64::NAN;
        assert_eq!(command(Remote::Play, nan, 5.0, 1), Some(M::Play));
        assert_eq!(command(Remote::Pause, nan, 5.0, 1), Some(M::Pause));
        assert_eq!(command(Remote::TogglePlayPause, nan, 5.0, 1), Some(M::PlayPause));
        assert_eq!(command(Remote::Stop, nan, 5.0, 1), Some(M::Stop));
        assert_eq!(command(Remote::Next, nan, 5.0, 1), Some(M::Next));
        assert_eq!(command(Remote::Previous, nan, 5.0, 1), Some(M::Previous));
        // 往前 / 往後跳：系統給的秒數；沒給（0、NaN）時用設定的
        assert_eq!(command(Remote::SkipForward, 15.0, 5.0, 1), Some(M::SeekBy(15.0)));
        assert_eq!(command(Remote::SkipBackward, 15.0, 5.0, 1), Some(M::SeekBy(-15.0)));
        assert_eq!(command(Remote::SkipForward, nan, 7.0, 1), Some(M::SeekBy(7.0)));
        assert_eq!(command(Remote::SkipBackward, 0.0, 7.0, 1), Some(M::SeekBy(-7.0)));
        assert_eq!(command(Remote::SkipForward, nan, nan, 1), None);
        // 拖動進度：帶上系統顯示的檔案（0 = 沒有）
        assert_eq!(
            command(Remote::Position, 12.5, 5.0, 4),
            Some(M::SeekTo {
                secs: 12.5,
                track: Some(4)
            })
        );
        assert_eq!(
            command(Remote::Position, -1.0, 5.0, 0),
            Some(M::SeekTo { secs: 0.0, track: None })
        );
        assert_eq!(command(Remote::Position, nan, 5.0, 4), None);
        assert_eq!(command(Remote::Rate, 1.25, 5.0, 1), Some(M::SetRate(1.25)));
        assert_eq!(command(Remote::Rate, nan, 5.0, 1), None);
        assert_eq!(command(Remote::Shuffle, 1.0, 5.0, 1), Some(M::SetShuffle(true)));
        assert_eq!(command(Remote::Shuffle, 0.0, 5.0, 1), Some(M::SetShuffle(false)));
        assert_eq!(command(Remote::Shuffle, 0.5, 5.0, 1), None);
        assert_eq!(command(Remote::Repeat, 1.0, 5.0, 1), Some(M::SetRepeat(Repeat::One)));
        assert_eq!(command(Remote::Repeat, 2.0, 5.0, 1), Some(M::SetRepeat(Repeat::All)));
        assert_eq!(command(Remote::Repeat, 9.0, 5.0, 1), None);
        assert_eq!(command(Remote::Repeat, nan, 5.0, 1), None);
    }

    #[test]
    fn now_playing_follows_the_track_and_playback() {
        let mut s = Shown::default();
        assert_eq!(s.now_playing(), None, "還沒有檔案：不顯示");
        assert_eq!(s.controls(), None);
        s.apply(&Update::Track(track(1, true)));
        s.apply(&Update::Playback {
            info: playback(Status::Playing, 12.0),
            seeked: true,
        });
        let np = s.now_playing().unwrap();
        assert_eq!(
            np,
            NowPlaying {
                title: "歌".into(),
                artist: Some("人".into()),
                album: Some("專輯".into()),
                album_artist: None,
                track_no: Some(3),
                duration: Some(60.0),
                live: false,
                elapsed: 12.0,
                rate: 1.5,
                media_type: MEDIA_TYPE_AUDIO,
                artwork: false,
                state: playback_state(Status::Playing),
            }
        );
        assert_eq!(s.shown_gen(), 1);
        // 暫停、等快取：速度 0（系統不往前推算位置），狀態照舊
        s.apply(&Update::Playback {
            info: playback(Status::Paused, 13.0),
            seeked: false,
        });
        let np = s.now_playing().unwrap();
        assert_eq!((np.rate, np.elapsed, np.state), (0.0, 13.0, 2));
        let mut stalled = playback(Status::Playing, 13.0);
        stalled.buffering = true;
        s.apply(&Update::Playback {
            info: stalled,
            seeked: false,
        });
        let np = s.now_playing().unwrap();
        assert_eq!((np.rate, np.state), (0.0, 1));
        // 位置超過長度、負的：夾在範圍內
        s.apply(&Update::Playback {
            info: playback(Status::Playing, 75.0),
            seeked: true,
        });
        assert_eq!(s.now_playing().unwrap().elapsed, 60.0);
        s.apply(&Update::Playback {
            info: playback(Status::Playing, -2.0),
            seeked: true,
        });
        assert_eq!(s.now_playing().unwrap().elapsed, 0.0);
        // 影片、直播（沒有長度）
        let mut live = track(2, true);
        live.kind = MediaKind::Video;
        live.duration = None;
        s.apply(&Update::Track(live));
        let np = s.now_playing().unwrap();
        assert_eq!((np.media_type, np.live, np.duration), (MEDIA_TYPE_VIDEO, true, None));
        // 停止：標題留著（跟 Windows 一樣，按播放會再開），位置 0、速度 0；拖動進度不帶檔案
        s.apply(&Update::Clear);
        let np = s.now_playing().unwrap();
        assert_eq!((np.title.as_str(), np.elapsed, np.rate, np.state), ("歌", 0.0, 0.0, 3));
        assert_eq!(s.shown_gen(), 0);
    }

    /// 每次重建的字典都放封面：同一個檔案換標題（標籤、ICY 標題變了）時封面不會掉（S-B12）
    #[test]
    fn the_cover_is_in_every_rebuilt_dictionary() {
        let mut s = Shown::default();
        s.apply(&Update::Cover(cover(1)));
        assert!(s.cover().is_none(), "還沒有這個檔案：不要");
        s.apply(&Update::Track(track(1, true)));
        s.apply(&Update::Cover(cover(9)));
        assert!(s.cover().is_none(), "別的檔案的封面");
        s.apply(&Update::Cover(cover(1)));
        s.apply(&Update::Playback {
            info: playback(Status::Playing, 1.0),
            seeked: false,
        });
        assert!(s.now_playing().unwrap().artwork);
        // 同一個檔案換標題：封面還在
        let mut renamed = track(1, true);
        renamed.title = "ICY 標題".into();
        s.apply(&Update::Track(renamed.clone()));
        let np = s.now_playing().unwrap();
        assert_eq!((np.title.as_str(), np.artwork), ("ICY 標題", true));
        assert_eq!(s.cover().map(|c| c.file_gen), Some(1));
        // 每 5 秒的進度更新也帶著
        s.apply(&Update::Playback {
            info: playback(Status::Playing, 6.0),
            seeked: false,
        });
        assert!(s.now_playing().unwrap().artwork);
        // 隱藏片名與封面：不放；再打開：又放
        let mut private = renamed.clone();
        private.details = false;
        s.apply(&Update::Track(private));
        assert!(!s.now_playing().unwrap().artwork);
        s.apply(&Update::Track(renamed));
        assert!(s.now_playing().unwrap().artwork);
        // 換檔：上一個檔案的封面丟掉
        s.apply(&Update::Track(track(2, true)));
        assert!(!s.now_playing().unwrap().artwork);
        let mut back = track(1, true);
        back.file_gen = 2;
        s.apply(&Update::Track(back));
        assert!(s.cover().is_none(), "上一個檔案的封面不會回來");
    }

    #[test]
    fn buttons_follow_what_can_be_done() {
        let mut p = playback(Status::Playing, 0.0);
        p.shuffle = true;
        p.repeat = Repeat::One;
        let c = Controls::of(&p);
        assert!(c.play && c.pause && c.next && !c.previous && c.seek && c.rate);
        assert_eq!((c.shuffle, c.repeat), (1, 1));
        for r in [
            Remote::Play,
            Remote::TogglePlayPause,
            Remote::Pause,
            Remote::Stop,
            Remote::Next,
        ] {
            assert!(c.enabled(r), "{r:?}");
        }
        assert!(!c.enabled(Remote::Previous));
        assert!(c.enabled(Remote::Position) && c.enabled(Remote::Rate));
        // 往前 / 往後跳一直不能按（控制中心才會顯示上一個 / 下一個，跟 Windows、Linux 一樣）
        assert!(!c.enabled(Remote::SkipForward) && !c.enabled(Remote::SkipBackward));
        // 不能跳轉（直播）：進度不能拖
        p.seekable = false;
        let c = Controls::of(&p);
        assert!(!c.enabled(Remote::Position));
        // 停止：還有東西可以播時播放鍵能按；暫停、停止、跳轉、速度不能按；隨機、重複一直能按
        let stopped = playback(Status::Playing, 0.0).cleared();
        let c = Controls::of(&stopped);
        assert!(c.enabled(Remote::Play) && c.enabled(Remote::TogglePlayPause));
        for r in [
            Remote::Pause,
            Remote::Stop,
            Remote::Position,
            Remote::SkipBackward,
            Remote::Rate,
        ] {
            assert!(!c.enabled(r), "{r:?}");
        }
        assert!(c.enabled(Remote::Shuffle) && c.enabled(Remote::Repeat));
        let nothing = PlaybackInfo {
            can_play: false,
            ..stopped
        };
        assert!(!Controls::of(&nothing).enabled(Remote::Play));
        // Clear 之後按鈕照停止的狀態
        let mut s = Shown::default();
        s.apply(&Update::Track(track(1, true)));
        s.apply(&Update::Playback {
            info: playback(Status::Playing, 3.0),
            seeked: true,
        });
        s.apply(&Update::Clear);
        assert!(!s.controls().unwrap().pause);
        assert_eq!(Remote::ALL.len(), 12);
    }
}
