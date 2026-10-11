//! 系統的媒體控制（媒體鍵、Windows 的 SMTC、macOS 的「正在播放」、Linux 的 MPRIS）：各系統共用的部分。
//!
//! - 介面（`app/media.rs`）每一幀把目前的檔案（[`TrackInfo`]）與播放狀態（[`PlaybackInfo`]）交給 [`SessionSync`]，
//!   它決定要送哪些 [`Update`] 給系統（變了才送；播放中每 5 秒更新一次進度；跳轉時標出來）。
//! - 系統送來的 [`MediaCommand`] 經過 [`step`] 變成要做的事：播放、暫停、停止、上一個 / 下一個跟按鍵走同一條路
//!   （`keymap::Command` → `Action` → `run`），帶數值的（跳到某個時間、速度、音量、隨機、重複…）直接對應到操作。
//! - 真的系統只在 main.rs 接上（`Launch.media = MediaBackend::System`）；自動測試預設沒有（[`MediaBackend::None`]），
//!   要測的用假的（[`MediaBackend::Custom`]），絕不碰使用者的系統媒體控制。
//! - 音訊檔的專輯封面：mpv 把封面當成影像，`screenshot-to-file … video` 存成 PNG 放在快取資料夾的 `covers/`
//!   （`cover-<pid>-<第幾個檔案>.png`；MPRIS 要檔案，其他系統用內容）。換檔、關掉時刪掉自己的，啟動時清掉一天以上的。

use crate::keymap::Command;
use crate::playlist::Repeat;
use raw_window_handle::RawWindowHandle;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub use crate::instance::Wake;

/// 位置跟預期的差超過這麼多秒就當成跳轉（跳轉、換章節、重複單一檔案繞回開頭）
pub const SEEK_JUMP: f64 = 1.0;
/// 播放中每隔多久再送一次進度（Windows 的時間軸要定期更新；其他系統送了也沒關係）
pub const REFRESH_EVERY: Duration = Duration::from_secs(5);

// ───────────── 設定 ─────────────

/// 系統媒體控制（存檔）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SystemMedia {
    /// 讓系統的媒體控制、媒體鍵控制影戲
    pub enabled: bool,
    /// 系統的媒體控制顯示片名與封面（關掉時只顯示程式名稱：鎖定畫面、音量的浮動視窗看得到）
    pub details: bool,
}

impl Default for SystemMedia {
    fn default() -> Self {
        Self {
            enabled: true,
            details: true,
        }
    }
}

// ───────────── 系統 ↔ 影戲 ─────────────

/// 系統送來的指令
#[derive(Debug, Clone, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// 相對跳轉（秒，可以是負的）
    SeekBy(f64),
    /// 跳到某個時間（秒）。`track`：系統指定的是哪個檔案（[`TrackInfo::file_gen`]；MPRIS 的 track id），
    /// 已經換到別的檔案時不理
    SeekTo {
        secs: f64,
        track: Option<u64>,
    },
    /// 播放速度（1.0 = 正常）
    SetRate(f64),
    SetShuffle(bool),
    SetRepeat(Repeat),
    /// 音量（1.0 = 100%）
    SetVolume(f64),
    /// 把視窗叫到前面
    Raise,
    /// 結束影戲
    Quit,
    /// 開啟網址或檔案（`file://`、`http(s)://`）
    OpenUri(String),
}

/// 目前的檔案是影片還是音訊（系統顯示的樣子不一樣）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

/// 目前的檔案（標題、演出者…）。換檔或標籤變了才重新送
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    /// 第幾個開的檔案（同一個檔案重開也是新的）
    pub file_gen: u64,
    pub kind: MediaKind,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_no: Option<u32>,
    /// 總長度（秒）；直播沒有
    pub duration: Option<f64>,
    /// `file://…` 或網址
    pub url: Option<String>,
    /// 顯示片名與封面（設定；關掉時標題是程式名稱，沒有演出者、專輯、網址、封面）
    pub details: bool,
}

/// 播放狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 沒有開檔
    Stopped,
    Playing,
    /// 暫停（播完停在最後一格也是）
    Paused,
}

/// 播放狀態（每一幀都算，變了才送）
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackInfo {
    pub status: Status,
    /// 目前的時間（秒）
    pub position: f64,
    /// 播放速度（暫停時也是設定的速度）
    pub rate: f64,
    pub seekable: bool,
    pub can_next: bool,
    pub can_prev: bool,
    pub shuffle: bool,
    pub repeat: Repeat,
    /// 音量（1.0 = 100%，超過 100% 的放大也算）
    pub volume: f64,
}

impl PlaybackInfo {
    /// 除了位置以外都一樣（位置每一幀都在變，另外看有沒有跳轉）
    fn same_state(&self, other: &Self) -> bool {
        Self {
            position: other.position,
            ..self.clone()
        } == *other
    }
}

/// 專輯封面（PNG）
#[derive(Clone, PartialEq)]
pub struct Cover {
    /// 哪個檔案的（[`TrackInfo::file_gen`]）
    pub file_gen: u64,
    pub png: Arc<Vec<u8>>,
    /// 存在快取資料夾的檔案（MPRIS 的 artUrl）
    pub path: PathBuf,
}

impl std::fmt::Debug for Cover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cover")
            .field("file_gen", &self.file_gen)
            .field("png", &format_args!("{} bytes", self.png.len()))
            .field("path", &self.path)
            .finish()
    }
}

/// 送給系統的更新
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// 換了檔案、或標籤變了。系統顯示的標題、演出者…全部換掉（之前的封面也不算了，有封面時緊接著送 [`Update::Cover`]）
    Track(TrackInfo),
    /// 播放狀態。`seeked`：位置跳過（換檔、跳轉、重複單一檔案繞回開頭），系統要重新對時間
    Playback { info: PlaybackInfo, seeked: bool },
    /// 目前檔案的專輯封面（每次送 [`Update::Track`] 之後都會再送一次：Windows 換標題時會把封面清掉）
    Cover(Cover),
    /// 不再播放任何檔案（停止、播完卸載了）：系統顯示停止。工作階段還在，按播放鍵可以再開
    Clear,
}

/// 一個系統的媒體控制工作階段（Windows 的 SMTC、macOS 的「正在播放」、Linux 的 MPRIS）
pub trait MediaSession {
    /// 照 [`Update`] 更新系統顯示的內容。不能卡住介面（慢的工作交給自己的執行緒）
    fn apply(&mut self, update: &Update);
    /// 結束：系統不再顯示影戲，也不再送指令（關掉設定、結束影戲時）
    fn shutdown(&mut self);
}

/// 建立好的工作階段，和它送來的指令
pub type Opened = (Box<dyn MediaSession>, Receiver<MediaCommand>);

/// 建立工作階段的方法（測試用的假系統）：參數是叫醒介面的方法（系統的指令從別的執行緒送來時呼叫）
pub type Factory = Box<dyn Fn(Wake) -> Result<Opened, String>>;

/// 用哪個系統的媒體控制（`Launch.media`）
#[derive(Default)]
pub enum MediaBackend {
    /// 不用（自動測試、`--shot` 的預設：不碰使用者的系統媒體控制）
    #[default]
    None,
    /// 這個作業系統的（只有 main.rs 用）
    System,
    /// 測試用的假系統
    Custom(Factory),
}

impl MediaBackend {
    /// 能不能用（「設定 → 系統」、快捷鍵頁的「媒體鍵」只在能用時顯示）
    pub fn available(&self) -> bool {
        match self {
            Self::None => false,
            Self::System => system_supported(),
            Self::Custom(_) => true,
        }
    }

    /// 建立工作階段（`owner` = 主視窗：Windows 的 SMTC 是跟著視窗的）
    pub fn create(&self, owner: Option<RawWindowHandle>, identity: &str, wake: Wake) -> Result<Opened, String> {
        match self {
            Self::None => Err("no media backend".to_owned()),
            Self::System => create_system(owner, identity, wake),
            Self::Custom(factory) => factory(wake),
        }
    }
}

/// 這個作業系統的媒體控制做好了沒（各系統的實作加進來時打開）
pub const fn system_supported() -> bool {
    false
}

/// 建立這個作業系統的工作階段
pub fn create_system(owner: Option<RawWindowHandle>, identity: &str, wake: Wake) -> Result<Opened, String> {
    let _ = (owner, identity, wake);
    Err(crate::tr!("這個系統還不支援", "Not supported on this system yet").to_owned())
}

// ───────────── 什麼時候送什麼 ─────────────

/// 決定要送哪些更新給系統（純邏輯，不碰系統）：每一幀給目前的檔案與播放狀態，回傳要送的更新
#[derive(Debug, Default)]
pub struct SessionSync {
    /// 上次送出的檔案（None = 沒有在播，或還沒送過）
    track: Option<TrackInfo>,
    /// 上次送出的播放狀態、送的時間
    sent: Option<(PlaybackInfo, Instant)>,
    /// 目前檔案的封面（換檔、停止時丟掉）
    cover: Option<Cover>,
}

impl SessionSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// `track`：目前的檔案（None = 沒有開檔）。`now`：現在的時間（測試給假的）
    pub fn tick(&mut self, track: Option<&TrackInfo>, playback: &PlaybackInfo, now: Instant) -> Vec<Update> {
        let mut out = Vec::new();
        match track {
            Some(t) if self.track.as_ref() != Some(t) => {
                // 換了檔案：上一個檔案的封面不能用
                if self.cover.as_ref().is_some_and(|c| c.file_gen != t.file_gen) {
                    self.cover = None;
                }
                self.track = Some(t.clone());
                out.push(Update::Track(t.clone()));
                if let Some(c) = self.cover_to_show() {
                    out.push(Update::Cover(c.clone()));
                }
                out.push(self.send(playback, true, now));
                return out;
            }
            Some(_) => {}
            None if self.track.is_some() => {
                // 有在播 → 沒有：只送一次
                self.track = None;
                self.cover = None;
                out.push(Update::Clear);
                self.sent = Some((playback.clone(), now));
                return out;
            }
            None => {}
        }
        let Some((last, at)) = &self.sent else {
            // 還沒送過任何東西（沒有在播）：先記下來，之後變了才送
            self.sent = Some((playback.clone(), now));
            return out;
        };
        let elapsed = now.saturating_duration_since(*at);
        let expected = if last.status == Status::Playing {
            last.position + elapsed.as_secs_f64() * last.rate
        } else {
            last.position
        };
        let seeked = (playback.position - expected).abs() > SEEK_JUMP;
        let changed = !last.same_state(playback);
        let refresh = playback.status == Status::Playing && elapsed >= REFRESH_EVERY;
        if seeked || changed || refresh {
            out.push(self.send(playback, seeked, now));
        }
        out
    }

    /// 目前檔案的封面讀好了：是這個檔案的就記下來，顯示片名與封面時回傳要送的更新
    pub fn set_cover(&mut self, cover: Cover) -> Option<Update> {
        if self.track.as_ref().is_none_or(|t| t.file_gen != cover.file_gen) {
            return None;
        }
        self.cover = Some(cover);
        self.cover_to_show().map(|c| Update::Cover(c.clone()))
    }

    /// 上次送出的檔案（介面測試、封面用）
    pub fn sent_track(&self) -> Option<&TrackInfo> {
        self.track.as_ref()
    }

    fn cover_to_show(&self) -> Option<&Cover> {
        let t = self.track.as_ref()?;
        self.cover.as_ref().filter(|c| c.file_gen == t.file_gen && t.details)
    }

    fn send(&mut self, playback: &PlaybackInfo, seeked: bool, now: Instant) -> Update {
        self.sent = Some((playback.clone(), now));
        Update::Playback {
            info: playback.clone(),
            seeked,
        }
    }
}

// ───────────── 系統的指令 → 要做的事 ─────────────

/// 收到指令時的播放狀態
#[derive(Debug, Clone, Copy, Default)]
pub struct Now {
    /// 有檔案載入（FileLoaded 到 EndFile 之間）
    pub loaded: bool,
    /// 正在開檔（還沒載入完）
    pub opening: bool,
    pub paused: bool,
    /// 目前的檔案是第幾個開的（[`TrackInfo::file_gen`]）
    pub file_gen: u64,
}

/// 系統的指令要做的事（`app/media.rs` 執行）
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// 跟按鍵一樣（`Command` → `Action` → `run`）
    Command(Command),
    /// 沒有在播時的「播放」：開清單上停止前的那一個（還原的清單是接下來要播的那一個）
    PlayStopped,
    /// 不用做什麼（已經是要的狀態、別的檔案的跳轉、不支援的網址）：只取消播完後的倒數
    Nothing,
    SeekBy(f64),
    SeekTo(f64),
    Rate(f64),
    Shuffle(bool),
    Repeat(Repeat),
    /// 音量（1.0 = 100%）
    Volume(f64),
    Raise,
    Quit,
    Open(PathBuf),
}

/// 系統的指令 → 要做的事。
/// 播放 / 暫停是「變成那個狀態」：已經是了就不動（不是切換）；沒有開檔時的播放、播放／暫停開停止前的檔案（S-B10）
pub fn step(cmd: &MediaCommand, now: Now) -> Step {
    let idle = !now.loaded && !now.opening;
    match cmd {
        MediaCommand::Play | MediaCommand::PlayPause if idle => Step::PlayStopped,
        MediaCommand::Play if now.loaded && now.paused => Step::Command(Command::TogglePause),
        MediaCommand::Pause if now.loaded && !now.paused => Step::Command(Command::TogglePause),
        MediaCommand::PlayPause if now.loaded => Step::Command(Command::TogglePause),
        MediaCommand::Play | MediaCommand::Pause | MediaCommand::PlayPause => Step::Nothing,
        MediaCommand::Stop => Step::Command(Command::Stop),
        MediaCommand::Next => Step::Command(Command::NextFile),
        MediaCommand::Previous => Step::Command(Command::PrevFile),
        MediaCommand::SeekBy(d) if now.loaded && d.is_finite() => Step::SeekBy(*d),
        MediaCommand::SeekTo { secs, track }
            if now.loaded && secs.is_finite() && track.is_none_or(|g| g == now.file_gen) =>
        {
            Step::SeekTo(secs.max(0.0))
        }
        // MPRIS：速度設成 0 = 暫停
        MediaCommand::SetRate(r) if *r <= 0.0 && now.loaded && !now.paused => Step::Command(Command::TogglePause),
        MediaCommand::SetRate(r) if *r > 0.0 && r.is_finite() => Step::Rate(*r),
        MediaCommand::SetShuffle(on) => Step::Shuffle(*on),
        MediaCommand::SetRepeat(r) => Step::Repeat(*r),
        MediaCommand::SetVolume(v) if v.is_finite() => Step::Volume(v.max(0.0)),
        MediaCommand::Raise => Step::Raise,
        MediaCommand::Quit => Step::Quit,
        MediaCommand::OpenUri(uri) => uri_target(uri).map_or(Step::Nothing, Step::Open),
        _ => Step::Nothing,
    }
}

/// MPRIS 的 LoopStatus（`None` / `Track` / `Playlist`）
pub fn loop_status(repeat: Repeat) -> &'static str {
    match repeat {
        Repeat::Off => "None",
        Repeat::One => "Track",
        Repeat::All => "Playlist",
    }
}

/// MPRIS 的 LoopStatus → 重複播放（不認得的值 None）
pub fn repeat_from_loop_status(s: &str) -> Option<Repeat> {
    match s {
        "None" => Some(Repeat::Off),
        "Track" => Some(Repeat::One),
        "Playlist" => Some(Repeat::All),
        _ => None,
    }
}

// ───────────── 網址、標籤 ─────────────

/// 本機檔案的 `file://` 網址（中文、空白、`#`、`%` 都編碼；Windows 是 `file:///C:/…`）；不是完整路徑時 None
pub fn file_url(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

/// 系統要開的網址（MPRIS 的 OpenUri）→ 要開的路徑：`file://` 轉回本機路徑，`http(s)://` 照樣（開網址），其他的不開
pub fn uri_target(uri: &str) -> Option<PathBuf> {
    let parsed = url::Url::parse(uri.trim()).ok()?;
    match parsed.scheme() {
        "file" => parsed.to_file_path().ok(),
        "http" | "https" => Some(PathBuf::from(uri.trim())),
        _ => None,
    }
}

/// 標籤的音軌編號（`3`、`03`、`3/12`）→ 3；開頭不是數字時 None
pub fn track_number(tag: &str) -> Option<u32> {
    let digits: String = tag.trim().chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&n| n > 0)
}

// ───────────── 封面檔案 ─────────────

/// 封面放的資料夾：`<快取>/covers`
pub fn covers_dir(cache_root: &Path) -> PathBuf {
    cache_root.join("covers")
}

/// 這個程式第 `file_gen` 個檔案的封面
pub fn cover_path(dir: &Path, pid: u32, file_gen: u64) -> PathBuf {
    dir.join(format!("cover-{pid}-{file_gen}.png"))
}

/// 這個程式的封面檔案（`cover-<pid>-…`）
fn own_cover(name: &str, pid: u32) -> bool {
    name.strip_prefix(&format!("cover-{pid}-"))
        .and_then(|rest| rest.strip_suffix(".png"))
        .is_some_and(|file_gen| !file_gen.is_empty() && file_gen.bytes().all(|b| b.is_ascii_digit()))
}

/// 刪掉這個程式的封面（關掉系統媒體控制、結束時）；`keep` 留著不刪（目前的）。回傳刪了幾個
pub fn remove_own_covers(dir: &Path, pid: u32, keep: Option<&Path>) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| own_cover(&e.file_name().to_string_lossy(), pid))
        .filter(|e| keep.is_none_or(|k| e.path() != k))
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

/// 刪掉 `older_than` 以前留下的封面（當掉、被強制結束時沒刪的；不管是哪個程式的）。回傳刪了幾個
pub fn sweep_covers(dir: &Path, older_than: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.starts_with("cover-") && name.ends_with(".png")
        })
        .filter(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > older_than)
        })
        .filter(|e| std::fs::remove_file(e.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(file_gen: u64) -> TrackInfo {
        TrackInfo {
            file_gen,
            kind: MediaKind::Audio,
            title: "歌".into(),
            artist: Some("人".into()),
            album: None,
            album_artist: None,
            track_no: None,
            duration: Some(100.0),
            url: None,
            details: true,
        }
    }

    fn playing(position: f64) -> PlaybackInfo {
        PlaybackInfo {
            status: Status::Playing,
            position,
            rate: 1.0,
            seekable: true,
            can_next: false,
            can_prev: false,
            shuffle: false,
            repeat: Repeat::Off,
            volume: 1.0,
        }
    }

    fn cover(file_gen: u64) -> Cover {
        Cover {
            file_gen,
            png: Arc::new(vec![1, 2, 3]),
            path: PathBuf::from(format!("cover-1-{file_gen}.png")),
        }
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn sync_sends_track_then_playback() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        let out = s.tick(Some(&track(1)), &playing(0.0), t0);
        assert_eq!(
            out,
            [
                Update::Track(track(1)),
                Update::Playback {
                    info: playing(0.0),
                    seeked: true
                }
            ]
        );
        // 標籤變了（同一個檔案）：再送一次
        let mut renamed = track(1);
        renamed.title = "新的歌名".into();
        let out = s.tick(Some(&renamed), &playing(0.5), t0 + secs(0.5));
        assert!(matches!(&out[..], [Update::Track(t), Update::Playback { seeked: true, .. }] if t.title == "新的歌名"));
    }

    #[test]
    fn sync_no_updates_when_steady() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        s.tick(Some(&track(1)), &playing(0.0), t0);
        // 照正常速度播了 4.9 秒（每一幀都問）：什麼都不送
        for i in 1..=49 {
            let t = f64::from(i) * 0.1;
            assert!(s.tick(Some(&track(1)), &playing(t), t0 + secs(t)).is_empty(), "{t}");
        }
        // 暫停著：位置不動、多久都不送
        let mut paused = playing(4.9);
        paused.status = Status::Paused;
        assert_eq!(s.tick(Some(&track(1)), &paused, t0 + secs(4.95)).len(), 1);
        assert!(s.tick(Some(&track(1)), &paused, t0 + secs(60.0)).is_empty());
    }

    #[test]
    fn sync_detects_seek_discontinuity() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        s.tick(Some(&track(1)), &playing(10.0), t0);
        // 0.9 秒的誤差不算
        assert!(s.tick(Some(&track(1)), &playing(11.9), t0 + secs(1.0)).is_empty());
        // 往後跳
        assert_eq!(
            s.tick(Some(&track(1)), &playing(40.0), t0 + secs(1.1)),
            [Update::Playback {
                info: playing(40.0),
                seeked: true
            }]
        );
        // 重複單一檔案繞回開頭
        let out = s.tick(Some(&track(1)), &playing(0.1), t0 + secs(1.2));
        assert!(matches!(&out[..], [Update::Playback { seeked: true, .. }]));
        // 2 倍速：照速度算預期的位置
        let mut fast = playing(0.1);
        fast.rate = 2.0;
        assert!(matches!(
            &s.tick(Some(&track(1)), &fast, t0 + secs(1.3))[..],
            [Update::Playback { seeked: false, .. }]
        ));
        fast.position = 4.1;
        assert!(
            s.tick(Some(&track(1)), &fast, t0 + secs(3.3)).is_empty(),
            "2 秒走了 4 秒"
        );
        // 暫停中跳轉
        let mut paused = fast.clone();
        paused.status = Status::Paused;
        s.tick(Some(&track(1)), &paused, t0 + secs(3.4));
        paused.position = 30.0;
        assert!(matches!(
            &s.tick(Some(&track(1)), &paused, t0 + secs(3.5))[..],
            [Update::Playback { seeked: true, .. }]
        ));
    }

    #[test]
    fn sync_periodic_refresh_every_5s_only_while_playing() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        s.tick(Some(&track(1)), &playing(0.0), t0);
        assert!(s.tick(Some(&track(1)), &playing(4.99), t0 + secs(4.99)).is_empty());
        assert_eq!(
            s.tick(Some(&track(1)), &playing(5.0), t0 + secs(5.0)),
            [Update::Playback {
                info: playing(5.0),
                seeked: false
            }]
        );
        assert!(s.tick(Some(&track(1)), &playing(9.9), t0 + secs(9.9)).is_empty());
        assert_eq!(s.tick(Some(&track(1)), &playing(10.0), t0 + secs(10.0)).len(), 1);
        // 暫停中不定期送
        let mut paused = playing(10.0);
        paused.status = Status::Paused;
        assert_eq!(s.tick(Some(&track(1)), &paused, t0 + secs(10.1)).len(), 1);
        assert!(s.tick(Some(&track(1)), &paused, t0 + secs(30.0)).is_empty());
    }

    #[test]
    fn sync_sends_other_field_changes_at_once() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        s.tick(Some(&track(1)), &playing(0.0), t0);
        for change in [
            (|p: &mut PlaybackInfo| p.can_next = true) as fn(&mut PlaybackInfo),
            |p| p.shuffle = true,
            |p| p.repeat = Repeat::All,
            |p| p.volume = 0.5,
            |p| p.rate = 1.5,
            |p| p.seekable = false,
            |p| p.status = Status::Paused,
        ] {
            let mut p = playing(0.0);
            change(&mut p);
            let out = s.tick(Some(&track(1)), &p, t0);
            assert_eq!(out, [Update::Playback { info: p, seeked: false }]);
            s.tick(Some(&track(1)), &playing(0.0), t0);
        }
    }

    #[test]
    fn sync_clear_once_on_unload() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        let mut stopped = playing(0.0);
        stopped.status = Status::Stopped;
        // 還沒播過：不送
        assert!(s.tick(None, &stopped, t0).is_empty());
        s.tick(Some(&track(1)), &playing(0.0), t0);
        assert_eq!(s.tick(None, &stopped, t0 + secs(1.0)), [Update::Clear]);
        assert!(s.tick(None, &stopped, t0 + secs(2.0)).is_empty());
        assert!(s.tick(None, &stopped, t0 + secs(20.0)).is_empty(), "停止時不定期送");
        // 停止中改了隨機播放：系統的開關跟著變
        stopped.shuffle = true;
        assert!(matches!(
            &s.tick(None, &stopped, t0 + secs(21.0))[..],
            [Update::Playback { seeked: false, .. }]
        ));
        // 同一個檔案重開（新的 gen）：又是新的
        let out = s.tick(Some(&track(2)), &playing(0.0), t0 + secs(22.0));
        assert!(matches!(&out[0], Update::Track(t) if t.file_gen == 2));
        assert_eq!(s.sent_track().map(|t| t.file_gen), Some(2));
    }

    #[test]
    fn sync_cover_follows_its_track() {
        let mut s = SessionSync::new();
        let t0 = Instant::now();
        // 還沒送出這個檔案：不要（之後的 Track 也不會帶它）
        assert_eq!(s.set_cover(cover(1)), None);
        s.tick(Some(&track(1)), &playing(0.0), t0);
        assert_eq!(s.set_cover(cover(2)), None, "別的檔案的封面");
        assert_eq!(s.set_cover(cover(1)), Some(Update::Cover(cover(1))));
        // 同一個檔案的標籤變了：Track 之後再送一次封面（Windows 換標題時會清掉封面）
        let mut renamed = track(1);
        renamed.title = "ICY 標題".into();
        let out = s.tick(Some(&renamed), &playing(0.1), t0 + secs(0.1));
        assert!(matches!(&out[..], [Update::Track(_), Update::Cover(c), Update::Playback { .. }] if c.file_gen == 1));
        // 關掉「顯示片名與封面」：不送封面；再打開：又送
        let mut private = renamed.clone();
        private.details = false;
        let out = s.tick(Some(&private), &playing(0.2), t0 + secs(0.2));
        assert!(matches!(&out[..], [Update::Track(_), Update::Playback { .. }]));
        let out = s.tick(Some(&renamed), &playing(0.3), t0 + secs(0.3));
        assert!(matches!(
            &out[..],
            [Update::Track(_), Update::Cover(_), Update::Playback { .. }]
        ));
        // 換檔：舊的封面丟掉
        let out = s.tick(Some(&track(2)), &playing(0.0), t0 + secs(0.4));
        assert!(matches!(&out[..], [Update::Track(_), Update::Playback { .. }]));
        let mut back = track(1);
        back.file_gen = 2;
        assert!(
            !s.tick(Some(&back), &playing(0.0), t0 + secs(0.5))
                .iter()
                .any(|u| matches!(u, Update::Cover(_)))
        );
        // 停止：丟掉
        assert_eq!(s.set_cover(cover(2)), Some(Update::Cover(cover(2))));
        s.tick(None, &playing(0.0), t0 + secs(0.6));
        let out = s.tick(Some(&back), &playing(0.0), t0 + secs(0.7));
        assert!(!out.iter().any(|u| matches!(u, Update::Cover(_))));
    }

    fn now(loaded: bool, paused: bool) -> Now {
        Now {
            loaded,
            opening: false,
            paused,
            file_gen: 7,
        }
    }

    #[test]
    fn command_mapping() {
        use MediaCommand as M;
        let toggle = Step::Command(Command::TogglePause);
        // 播放 / 暫停是「變成那個狀態」
        assert_eq!(step(&M::Play, now(true, true)), toggle);
        assert_eq!(step(&M::Play, now(true, false)), Step::Nothing);
        assert_eq!(step(&M::Pause, now(true, false)), toggle);
        assert_eq!(step(&M::Pause, now(true, true)), Step::Nothing);
        assert_eq!(step(&M::PlayPause, now(true, true)), toggle);
        assert_eq!(step(&M::PlayPause, now(true, false)), toggle);
        // 沒有開檔：播放、播放／暫停開停止前的檔案；暫停不動。正在開檔時都不動
        assert_eq!(step(&M::Play, now(false, false)), Step::PlayStopped);
        assert_eq!(step(&M::PlayPause, now(false, true)), Step::PlayStopped);
        assert_eq!(step(&M::Pause, now(false, false)), Step::Nothing);
        let opening = Now {
            opening: true,
            ..now(false, false)
        };
        assert_eq!(step(&M::Play, opening), Step::Nothing);
        assert_eq!(step(&M::PlayPause, opening), Step::Nothing);
        // 停止、上一個、下一個：跟按鍵一樣
        assert_eq!(step(&M::Stop, now(false, false)), Step::Command(Command::Stop));
        assert_eq!(step(&M::Next, now(true, false)), Step::Command(Command::NextFile));
        assert_eq!(step(&M::Previous, now(true, false)), Step::Command(Command::PrevFile));
        // 跳轉：別的檔案的、沒有開檔、不是數字的不理
        assert_eq!(step(&M::SeekBy(-10.0), now(true, false)), Step::SeekBy(-10.0));
        assert_eq!(step(&M::SeekBy(5.0), now(false, false)), Step::Nothing);
        assert_eq!(step(&M::SeekBy(f64::NAN), now(true, false)), Step::Nothing);
        let to = |secs, track| M::SeekTo { secs, track };
        assert_eq!(step(&to(12.5, None), now(true, true)), Step::SeekTo(12.5));
        assert_eq!(step(&to(12.5, Some(7)), now(true, true)), Step::SeekTo(12.5));
        assert_eq!(step(&to(12.5, Some(6)), now(true, true)), Step::Nothing);
        assert_eq!(step(&to(-3.0, None), now(true, true)), Step::SeekTo(0.0));
        assert_eq!(step(&to(1.0, None), now(false, false)), Step::Nothing);
        // 速度：0 = 暫停（MPRIS）
        assert_eq!(step(&M::SetRate(1.5), now(true, false)), Step::Rate(1.5));
        assert_eq!(step(&M::SetRate(0.0), now(true, false)), toggle);
        assert_eq!(step(&M::SetRate(0.0), now(true, true)), Step::Nothing);
        assert_eq!(step(&M::SetRate(f64::INFINITY), now(true, false)), Step::Nothing);
        assert_eq!(step(&M::SetShuffle(true), now(false, false)), Step::Shuffle(true));
        assert_eq!(
            step(&M::SetRepeat(Repeat::All), now(false, false)),
            Step::Repeat(Repeat::All)
        );
        assert_eq!(step(&M::SetVolume(0.4), now(false, false)), Step::Volume(0.4));
        assert_eq!(step(&M::SetVolume(-1.0), now(false, false)), Step::Volume(0.0));
        assert_eq!(step(&M::Raise, now(false, false)), Step::Raise);
        assert_eq!(step(&M::Quit, now(true, false)), Step::Quit);
        assert_eq!(
            step(&M::OpenUri("https://example.com/a.mp4".into()), now(false, false)),
            Step::Open(PathBuf::from("https://example.com/a.mp4"))
        );
        assert_eq!(step(&M::OpenUri("ftp://x/y".into()), now(false, false)), Step::Nothing);
        assert_eq!(step(&M::OpenUri("不是網址".into()), now(false, false)), Step::Nothing);
    }

    #[test]
    fn repeat_maps_to_loop_status() {
        for r in [Repeat::Off, Repeat::One, Repeat::All] {
            assert_eq!(repeat_from_loop_status(loop_status(r)), Some(r));
        }
        assert_eq!(loop_status(Repeat::All), "Playlist");
        assert_eq!(repeat_from_loop_status("playlist"), None);
    }

    #[test]
    fn file_url_round_trip() {
        #[cfg(windows)]
        let paths = [r"C:\影片\我的 影片 #1 100%.mkv", r"D:\a\b.mp4"];
        #[cfg(not(windows))]
        let paths = ["/home/使用者/我的 影片 #1 100%.mkv", "/a/b.mp4"];
        for p in paths {
            let url = file_url(Path::new(p)).unwrap();
            assert!(url.starts_with("file:///"), "{url}");
            assert!(
                !url.contains(' ') && !url.contains('#') && !url.contains("100%."),
                "{url}"
            );
            assert_eq!(uri_target(&url).as_deref(), Some(Path::new(p)), "{url}");
        }
        #[cfg(windows)]
        assert_eq!(file_url(Path::new(r"C:\a b.mp4")).unwrap(), "file:///C:/a%20b.mp4");
        // 相對路徑沒有網址
        assert_eq!(file_url(Path::new("a.mp4")), None);
        assert_eq!(uri_target("mailto:x@y"), None);
    }

    #[test]
    fn track_numbers_from_tags() {
        assert_eq!(track_number("3"), Some(3));
        assert_eq!(track_number(" 03/12"), Some(3));
        assert_eq!(track_number("12 of 20"), Some(12));
        assert_eq!(track_number("A1"), None);
        assert_eq!(track_number("0"), None);
        assert_eq!(track_number(""), None);
    }

    #[test]
    fn system_media_defaults_on() {
        assert_eq!(
            SystemMedia::default(),
            SystemMedia {
                enabled: true,
                details: true
            }
        );
        let s: SystemMedia = serde_json::from_str(r#"{"details": false}"#).unwrap();
        assert!(s.enabled && !s.details);
    }

    #[test]
    fn tests_never_get_a_system_session() {
        // Launch 的預設：不用；沒做好的系統不算能用
        assert!(!MediaBackend::default().available());
        assert!(matches!(MediaBackend::default(), MediaBackend::None));
        assert_eq!(MediaBackend::System.available(), system_supported());
        let wake: Wake = Arc::new(|| {});
        assert!(MediaBackend::None.create(None, "x", wake).is_err());
    }

    #[test]
    fn cover_files_are_named_per_process_and_swept() {
        let dir = std::env::temp_dir().join(format!("vitascope-covers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mine = [cover_path(&dir, 42, 1), cover_path(&dir, 42, 2)];
        let other = cover_path(&dir, 4200, 1);
        let unrelated = dir.join("cover-42-x.png");
        for p in mine.iter().chain([&other, &unrelated]) {
            std::fs::write(p, b"png").unwrap();
        }
        assert_eq!(mine[0].file_name().unwrap(), "cover-42-1.png");
        // 只刪自己的（4200 雖然是 42 開頭，但是別的程式），留著目前的
        assert_eq!(remove_own_covers(&dir, 42, Some(&mine[1])), 1);
        assert!(!mine[0].exists() && mine[1].exists() && other.exists() && unrelated.exists());
        assert_eq!(remove_own_covers(&dir, 42, None), 1);
        // 一天以上的才清：剛寫的不動；0 秒以上的全部
        assert_eq!(sweep_covers(&dir, Duration::from_secs(24 * 3600)), 0);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(sweep_covers(&dir, Duration::from_millis(1)), 2);
        assert_eq!(sweep_covers(&dir.join("沒有"), Duration::ZERO), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
