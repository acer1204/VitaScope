//! Linux 的系統媒體控制：MPRIS（D-Bus 的工作階段匯流排上的 `org.mpris.MediaPlayer2`）。GNOME、KDE 的媒體控制、
//! 鎖定畫面、媒體鍵（桌面環境在全域攔下媒體鍵、轉給 MPRIS 的播放程式）與 `playerctl` 都透過它控制影戲。
//!
//! - 純邏輯（屬性的值、Metadata、哪些屬性變了、系統的指令）不碰 D-Bus，在每個系統的測試都跑；
//!   連 D-Bus 的部分（[`create`]、[`MprisSession`]）只有 Linux 有。
//! - 名稱：第一個影戲拿 `org.mpris.MediaPlayer2.vitascope`，之後開的（「只開一個視窗」關掉、`--new-window`）拿
//!   `org.mpris.MediaPlayer2.vitascope.instance<pid>`（MPRIS 規格建議的做法）。要名稱時只帶 `DoNotQueue`：
//!   zbus 預設的旗標是 AllowReplacement | ReplaceExisting | DoNotQueue，第二個影戲會把第一個的名稱搶走（S-A1），
//!   桌面的媒體控制就變成在控制另一個視窗。
//! - 執行緒：zbus 自己的執行緒（async-io 的內部執行器）處理系統的方法呼叫、讀屬性：只把指令送進通道、叫醒介面，
//!   讀屬性看共用的 [`Snapshot`]。背景執行緒 `vitascope-mpris` 收 [`Update`]、改 [`Snapshot`]、送出訊號：
//!   PropertiesChanged 只帶真的變了的屬性（位置不算，系統要的時候才讀）；跳轉時送 Seeked。介面不會等 D-Bus。
//! - 可以寫的屬性（LoopStatus、Rate、Shuffle、Volume）：zbus 的 Properties.Set 在設定之後會自己送一次那個屬性的
//!   PropertiesChanged，帶的是那時讀到的值。設定時先把系統顯示的值改成要的值（[`Snapshot::request`]）、記成已經送出，
//!   再把指令送給介面：送出去的就是新的值，影戲照做之後也不會再送一次；沒照做時介面之後送的播放狀態會改回真的值。
//! - 工作階段匯流排斷線（重新登入、dbus-daemon 重開）：[`MediaSession::alive`] 變成 false，介面在下一個檔案時重新建立。
//! - `VITASCOPE_DEBUG_MEDIA=1`：印出系統送來的指令與送不出去的訊號。

use super::{MediaCommand, PlaybackInfo, Status, TrackInfo, Update, file_url, loop_status, uri_target};
use crate::player::{MAX_SPEED, MIN_SPEED};
use crate::playlist::Repeat;
use std::path::PathBuf;
use std::time::Instant;

/// 第一個影戲的名稱
pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.vitascope";
/// MPRIS 規定的物件路徑
pub const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
pub const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
pub const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
/// 桌面環境用它找圖示與名稱（`packaging/linux/` 的 .desktop 檔）
pub const DESKTOP_ENTRY: &str = "io.github.acer1204.vitascope";
/// 檔案的 track id 前面的部分（後面接第幾個開的檔案）
const TRACK_PREFIX: &str = "/io/github/acer1204/vitascope/track/";
/// 沒有檔案時的 track id（MPRIS 規定的）
pub const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";
/// OpenUri 能開的網址（[`super::uri_target`]）
pub const URI_SCHEMES: [&str; 3] = ["file", "http", "https"];
/// 能開的檔案類型：跟 .desktop 檔的 MimeType 一樣（測試比對）
pub const MIME_TYPES: &[&str] = &[
    "video/mp4",
    "video/x-m4v",
    "video/x-matroska",
    "video/webm",
    "video/quicktime",
    "video/x-msvideo",
    "video/vnd.avi",
    "video/avi",
    "video/mp2t",
    "video/mpeg",
    "video/x-ms-wmv",
    "video/x-ms-asf",
    "application/vnd.ms-asf",
    "video/x-flv",
    "video/3gpp",
    "video/3gpp2",
    "video/ogg",
    "audio/mpeg",
    "audio/mp4",
    "audio/x-m4a",
    "audio/aac",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
    "audio/x-vorbis+ogg",
    "audio/x-opus+ogg",
    "audio/opus",
    "audio/x-wav",
    "audio/wav",
    "audio/vnd.wave",
    "audio/x-ms-wma",
    "audio/ac3",
    "audio/vnd.dts",
    "audio/x-matroska",
    "audio/x-ape",
    "audio/x-wavpack",
    "audio/x-tta",
    "audio/AMR",
    "audio/x-speex+ogg",
    "audio/x-dsf",
    "audio/x-dff",
    "audio/x-mpegurl",
    "application/vnd.apple.mpegurl",
];

/// 第二個以後的影戲的名稱
pub fn instance_name(pid: u32) -> String {
    format!("{BUS_NAME}.instance{pid}")
}

/// 第 `file_gen` 個開的檔案的 track id（D-Bus 的物件路徑）
pub fn track_id(file_gen: u64) -> String {
    format!("{TRACK_PREFIX}{file_gen}")
}

/// track id → 第幾個開的檔案；不是影戲的 track id 時 None
pub fn track_from_id(id: &str) -> Option<u64> {
    let digits = id.strip_prefix(TRACK_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// 秒 → 微秒（MPRIS 的時間單位）；負的、不是數字的當成 0
fn to_us(secs: f64) -> i64 {
    if secs.is_finite() {
        (secs.max(0.0) * 1e6).round() as i64
    } else {
        0
    }
}

// ───────────── 系統顯示的內容 ─────────────

/// 系統目前顯示的檔案與播放狀態（背景執行緒寫，D-Bus 讀屬性時讀）
#[derive(Debug, Clone)]
pub struct Snapshot {
    track: Option<TrackInfo>,
    /// 目前檔案的封面（第幾個檔案、存在快取資料夾的檔案）
    cover: Option<(u64, PathBuf)>,
    playback: PlaybackInfo,
    /// `playback.position` 是什麼時候的位置（讀 Position 時從這裡往後推算）
    stamp: Instant,
}

impl Snapshot {
    /// 還沒有任何檔案（停止）
    pub fn new(now: Instant) -> Self {
        Self {
            track: None,
            cover: None,
            playback: PlaybackInfo {
                status: Status::Stopped,
                position: 0.0,
                rate: 1.0,
                seekable: false,
                can_next: false,
                can_prev: false,
                shuffle: false,
                repeat: Repeat::Off,
                volume: 1.0,
                buffering: false,
                can_play: false,
            },
            stamp: now,
        }
    }

    /// 套用一個更新；回傳位置有沒有跳過（要送 Seeked）
    pub fn apply(&mut self, update: &Update, now: Instant) -> bool {
        match update {
            Update::Track(t) => {
                // 換了檔案：上一個檔案的封面不能用（同一個檔案換標題時 SessionSync 會再送一次封面）
                if self.cover.as_ref().is_some_and(|(g, _)| *g != t.file_gen) {
                    self.cover = None;
                }
                self.track = Some(t.clone());
                false
            }
            Update::Playback { info, seeked } => {
                self.playback = info.clone();
                self.stamp = now;
                *seeked
            }
            Update::Cover(c) => {
                if self.track.as_ref().is_some_and(|t| t.file_gen == c.file_gen) {
                    self.cover = Some((c.file_gen, c.path.clone()));
                }
                false
            }
            Update::Clear => {
                self.track = None;
                self.cover = None;
                self.playback = self.playback.cleared();
                self.stamp = now;
                false
            }
        }
    }

    /// 系統設定了可以寫的屬性（Properties.Set 的 LoopStatus、Rate、Shuffle、Volume）：系統顯示的值先改成要的值
    /// （影戲會照做），回傳改了的屬性名稱；不會改的（速度 0 是暫停、沒有檔案時不能改速度、不是數字）None。
    /// zbus 在設定之後會自己送一次這個屬性的 PropertiesChanged，帶的是那時讀到的值：先改好，送出去的就是新的值，
    /// 不會先送一次舊的值、等影戲改好再送新的。影戲沒照做時（音量超過上限、S/PDIF…），介面之後送的播放狀態
    /// 會改回真的值（[`super::SessionSync::confirm`]）
    pub fn request(&mut self, cmd: &MediaCommand, now: Instant) -> Option<&'static str> {
        match *cmd {
            MediaCommand::SetVolume(v) if v.is_finite() => {
                self.playback.volume = v.max(0.0);
                Some("Volume")
            }
            MediaCommand::SetShuffle(on) => {
                self.playback.shuffle = on;
                Some("Shuffle")
            }
            MediaCommand::SetRepeat(r) => {
                self.playback.repeat = r;
                Some("LoopStatus")
            }
            MediaCommand::SetRate(r) if r.is_finite() && r > 0.0 && self.playback.status != Status::Stopped => {
                // 位置從現在起照新的速度推算
                self.playback.position = self.position_us(now) as f64 / 1e6;
                self.stamp = now;
                self.playback.rate = r.clamp(RATE_RANGE.0, RATE_RANGE.1);
                Some("Rate")
            }
            _ => None,
        }
    }

    /// 目前檔案的 track id（沒有檔案時 None）
    pub fn track_gen(&self) -> Option<u64> {
        self.track.as_ref().map(|t| t.file_gen)
    }

    fn duration(&self) -> Option<f64> {
        self.track
            .as_ref()
            .and_then(|t| t.duration)
            .filter(|d| d.is_finite() && *d > 0.0)
    }

    /// 現在的位置（微秒）：播放中照速度往後推算（等快取時不動），不超過總長度
    pub fn position_us(&self, now: Instant) -> i64 {
        let p = &self.playback;
        if p.status == Status::Stopped || !p.position.is_finite() {
            return 0;
        }
        let mut pos = p.position;
        if p.status == Status::Playing && !p.buffering {
            pos += now.saturating_duration_since(self.stamp).as_secs_f64() * rate(p.rate);
        }
        if let Some(d) = self.duration() {
            pos = pos.min(d);
        }
        to_us(pos)
    }

    /// 所有會送 PropertiesChanged 的屬性（Player 介面；Position 不算）
    pub fn props(&self) -> Props {
        let p = &self.playback;
        let loaded = p.status != Status::Stopped;
        Props {
            status: match p.status {
                Status::Stopped => "Stopped",
                Status::Playing => "Playing",
                Status::Paused => "Paused",
            },
            loop_status: loop_status(p.repeat),
            rate: rate(p.rate),
            shuffle: p.shuffle,
            metadata: self.metadata(),
            volume: if p.volume.is_finite() { p.volume.max(0.0) } else { 0.0 },
            can_go_next: p.can_next,
            can_go_previous: p.can_prev,
            can_play: p.can_play || loaded,
            can_pause: loaded,
            can_seek: loaded && p.seekable,
        }
    }

    fn metadata(&self) -> Meta {
        let Some(t) = &self.track else {
            return Meta {
                track_id: NO_TRACK.to_owned(),
                ..Default::default()
            };
        };
        let list = |v: &Option<String>| v.iter().cloned().collect::<Vec<_>>();
        Meta {
            track_id: track_id(t.file_gen),
            length_us: self.duration().map(to_us),
            title: Some(t.title.clone()),
            artist: list(&t.artist),
            album: t.album.clone(),
            album_artist: list(&t.album_artist),
            track_number: t.track_no.and_then(|n| i32::try_from(n).ok()),
            url: t.url.clone(),
            // 隱藏片名與封面時不給（SessionSync 那時也不會送封面，這裡再擋一次）
            art_url: self
                .cover
                .as_ref()
                .filter(|(g, _)| *g == t.file_gen && t.details)
                .and_then(|(_, path)| file_url(path)),
        }
    }
}

/// 播放速度：MPRIS 不能是 0 或負的（暫停是 PlaybackStatus）
fn rate(r: f64) -> f64 {
    if r.is_finite() && r > 0.0 { r } else { 1.0 }
}

/// 目前檔案的 Metadata（`a{sv}`；沒有的欄位不放）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    /// `mpris:trackid`（物件路徑）
    pub track_id: String,
    /// `mpris:length`（微秒）
    pub length_us: Option<i64>,
    /// `xesam:title`
    pub title: Option<String>,
    /// `xesam:artist`（字串陣列）
    pub artist: Vec<String>,
    /// `xesam:album`
    pub album: Option<String>,
    /// `xesam:albumArtist`（字串陣列）
    pub album_artist: Vec<String>,
    /// `xesam:trackNumber`
    pub track_number: Option<i32>,
    /// `xesam:url`
    pub url: Option<String>,
    /// `mpris:artUrl`（快取資料夾裡的封面，`file://`）
    pub art_url: Option<String>,
}

/// Player 介面會送 PropertiesChanged 的屬性
#[derive(Debug, Clone, PartialEq)]
pub struct Props {
    pub status: &'static str,
    pub loop_status: &'static str,
    pub rate: f64,
    pub shuffle: bool,
    pub metadata: Meta,
    pub volume: f64,
    pub can_go_next: bool,
    pub can_go_previous: bool,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_seek: bool,
}

impl Props {
    /// 跟 `old` 比，變了的屬性（D-Bus 的屬性名稱）
    pub fn changed_since(&self, old: &Self) -> Vec<&'static str> {
        let mut out = Vec::new();
        let mut check = |same: bool, name: &'static str| {
            if !same {
                out.push(name);
            }
        };
        check(self.status == old.status, "PlaybackStatus");
        check(self.loop_status == old.loop_status, "LoopStatus");
        check(self.rate == old.rate, "Rate");
        check(self.shuffle == old.shuffle, "Shuffle");
        check(self.metadata == old.metadata, "Metadata");
        check(self.volume == old.volume, "Volume");
        check(self.can_go_next == old.can_go_next, "CanGoNext");
        check(self.can_go_previous == old.can_go_previous, "CanGoPrevious");
        check(self.can_play == old.can_play, "CanPlay");
        check(self.can_pause == old.can_pause, "CanPause");
        check(self.can_seek == old.can_seek, "CanSeek");
        out
    }

    /// 只把 `name` 這個屬性（[`Snapshot::request`] 改的）改成 `from` 的：zbus 已經送出它了；
    /// 其他屬性還沒送出的變化留給背景執行緒送
    pub fn adopt(&mut self, from: &Self, name: &str) {
        match name {
            "LoopStatus" => self.loop_status = from.loop_status,
            "Rate" => self.rate = from.rate,
            "Shuffle" => self.shuffle = from.shuffle,
            "Volume" => self.volume = from.volume,
            _ => {}
        }
    }
}

// ───────────── 系統的指令 ─────────────

/// `Seek(offset)`：相對跳轉（微秒）
pub fn seek_command(offset_us: i64) -> MediaCommand {
    MediaCommand::SeekBy(offset_us as f64 / 1e6)
}

/// `SetPosition(track_id, position)`：不是目前的檔案、位置是負的或超過總長度時不理（MPRIS 規格）。
/// 帶上 track id 的檔案：介面那邊已經換檔時也不理
pub fn set_position_command(snapshot: &Snapshot, id: &str, position_us: i64) -> Option<MediaCommand> {
    let file_gen = track_from_id(id)?;
    if snapshot.track_gen() != Some(file_gen) || position_us < 0 {
        return None;
    }
    let secs = position_us as f64 / 1e6;
    if snapshot.duration().is_some_and(|d| secs > d) {
        return None;
    }
    Some(MediaCommand::SeekTo {
        secs,
        track: Some(file_gen),
    })
}

/// 設定 LoopStatus：`None` / `Track` / `Playlist`；不認得的值 None
pub fn loop_command(value: &str) -> Option<MediaCommand> {
    super::repeat_from_loop_status(value).map(MediaCommand::SetRepeat)
}

/// `OpenUri`：`file://`、`http(s)://`；其他的（不支援的網址）None
pub fn open_uri_command(uri: &str) -> Option<MediaCommand> {
    uri_target(uri).map(|_| MediaCommand::OpenUri(uri.trim().to_owned()))
}

/// 速度的範圍（MinimumRate / MaximumRate）：跟介面一樣
pub const RATE_RANGE: (f64, f64) = (MIN_SPEED, MAX_SPEED);

// ───────────── D-Bus（只有 Linux）─────────────

#[cfg(all(unix, not(target_os = "macos")))]
pub use bus::{MprisSession, connect_to, create};

#[cfg(all(unix, not(target_os = "macos")))]
mod bus {
    use super::super::{MediaSession, Opened, Wake};
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::time::Duration;
    use zbus::blocking::Connection;
    use zbus::blocking::connection::Builder;
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    /// D-Bus 的呼叫最多等多久（建立時在介面執行緒上；正常是幾毫秒）
    const CALL_TIMEOUT: Duration = Duration::from_secs(5);
    /// 背景執行緒多久看一次連線還在不在（沒有更新時）
    const WATCH_EVERY: Duration = Duration::from_secs(1);
    /// 結束時最多等背景執行緒多久（放掉名稱、關掉連線）
    const SHUTDOWN_WAIT: Duration = Duration::from_secs(2);

    fn debug_enabled() -> bool {
        std::env::var_os("VITASCOPE_DEBUG_MEDIA").is_some()
    }

    /// 連上使用者的工作階段匯流排，建立 MPRIS 工作階段（`identity`：程式名稱）
    pub fn create(identity: &str, wake: Wake) -> Result<Opened, String> {
        let builder = Builder::session().map_err(|e| no_bus(&e))?;
        let (session, rx) = open(builder, std::process::id(), identity, wake)?;
        Ok((Box::new(session), rx))
    }

    /// 測試用：連到指定的匯流排（測試自己開的 dbus-daemon，絕不是使用者的），`pid` 決定第二個名稱
    #[doc(hidden)]
    pub fn connect_to(
        address: &str,
        pid: u32,
        identity: &str,
        wake: Wake,
    ) -> Result<(MprisSession, Receiver<MediaCommand>), String> {
        let builder = Builder::address(address).map_err(|e| no_bus(&e))?;
        open(builder, pid, identity, wake)
    }

    fn no_bus(e: &zbus::Error) -> String {
        crate::tf!(
            "連不上 D-Bus 的工作階段匯流排：{e}",
            "Couldn't connect to the D-Bus session bus: {e}"
        )
    }

    fn open(
        builder: Builder<'_>,
        pid: u32,
        identity: &str,
        wake: Wake,
    ) -> Result<(MprisSession, Receiver<MediaCommand>), String> {
        let (tx, rx) = mpsc::channel();
        let out = Out {
            tx,
            wake: wake.clone(),
            debug: debug_enabled(),
        };
        let now = Snapshot::new(Instant::now());
        let shared = Arc::new(Mutex::new(Shared {
            emitted: now.props(),
            now,
        }));
        let root = Root {
            out: out.clone(),
            identity: identity.to_owned(),
        };
        let player = Player {
            out: out.clone(),
            shared: shared.clone(),
        };
        // 先掛好介面再要名稱：拿到名稱的那一刻起，系統的呼叫都有人接
        let conn = builder
            .method_timeout(CALL_TIMEOUT)
            .serve_at(OBJECT_PATH, root)
            .and_then(|b| b.serve_at(OBJECT_PATH, player))
            .and_then(|b| b.build())
            .map_err(|e| no_bus(&e))?;
        let bus_name = take_name(&conn, pid)?;
        let unique_name = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
        let alive = Arc::new(AtomicBool::new(true));
        let (jobs, job_rx) = mpsc::channel();
        let worker = Worker {
            conn: Some(conn),
            bus_name: bus_name.clone(),
            shared,
            alive: alive.clone(),
            wake,
            debug: out.debug,
        };
        std::thread::Builder::new()
            .name("vitascope-mpris".into())
            .spawn(move || worker.run(job_rx))
            // 執行緒開不起來：worker 已經被丟掉了（Drop 放掉名稱、關掉連線）
            .map_err(|e| e.to_string())?;
        let session = MprisSession {
            jobs: Some(jobs),
            alive,
            bus_name,
            unique_name,
        };
        Ok((session, rx))
    }

    /// 要名稱：先要 `…vitascope`，被別的影戲拿走了就要 `…vitascope.instance<pid>`。只帶 DoNotQueue：
    /// 不排隊、不搶別人的，也不讓別人搶（zbus 預設的旗標會互相搶，S-A1）
    fn take_name(conn: &Connection, pid: u32) -> Result<String, String> {
        for name in [BUS_NAME.to_owned(), instance_name(pid)] {
            match conn.request_name_with_flags(name.as_str(), RequestNameFlags::DoNotQueue.into()) {
                Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => return Ok(name),
                // 已經有人用了：試下一個（zbus 把 Exists 變成 NameTaken 錯誤）
                Ok(RequestNameReply::Exists) | Err(zbus::Error::NameTaken) => {}
                // DoNotQueue 不會排隊；萬一排了，退出來（不要之後突然拿到名稱）
                Ok(RequestNameReply::InQueue) => {
                    let _ = conn.release_name(name.as_str());
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        Err(crate::tr!(
            "MPRIS 的名稱已經被其他程式使用",
            "The MPRIS name is already taken by another program"
        )
        .to_owned())
    }

    /// 背景執行緒與 zbus 的執行緒共用的
    struct Shared {
        /// 系統目前顯示的（讀屬性時讀這個）
        now: Snapshot,
        /// 上次送出（或一開始）的屬性：只送變了的（zbus 設定屬性之後自己送的也算）
        emitted: Props,
    }

    fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
        // 拿著鎖的地方不會 panic；萬一有，照樣用裡面的資料
        shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 介面那一端：更新交給背景執行緒，不等
    pub struct MprisSession {
        /// None = 已經結束
        jobs: Option<Sender<Job>>,
        alive: Arc<AtomicBool>,
        bus_name: String,
        unique_name: String,
    }

    impl MprisSession {
        /// 拿到的名稱（`…vitascope` 或 `…vitascope.instance<pid>`）
        pub fn bus_name(&self) -> &str {
            &self.bus_name
        }

        /// 這條連線在匯流排上的唯一名稱（`:1.42`；測試比對名稱的擁有者）
        pub fn unique_name(&self) -> &str {
            &self.unique_name
        }
    }

    enum Job {
        Apply(Update),
        /// 結束；做完回覆
        Shutdown(Sender<()>),
    }

    impl MediaSession for MprisSession {
        fn apply(&mut self, update: &Update) {
            if let Some(jobs) = &self.jobs {
                let _ = jobs.send(Job::Apply(update.clone()));
            }
        }

        fn shutdown(&mut self) {
            let Some(jobs) = self.jobs.take() else { return };
            let (done, wait) = mpsc::channel();
            // 斷線了：放掉名稱、關掉連線也只會失敗，不等
            if jobs.send(Job::Shutdown(done)).is_ok() && self.alive() {
                let _ = wait.recv_timeout(SHUTDOWN_WAIT);
            }
        }

        fn alive(&self) -> bool {
            self.alive.load(Ordering::Relaxed)
        }
    }

    impl Drop for MprisSession {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    // ───────────── 系統 → 影戲（zbus 的執行緒）─────────────

    /// 把系統的指令送給介面
    #[derive(Clone)]
    struct Out {
        tx: Sender<MediaCommand>,
        wake: Wake,
        debug: bool,
    }

    impl Out {
        fn send(&self, cmd: MediaCommand) {
            if self.debug {
                eprintln!("[media] MPRIS → {cmd:?}");
            }
            if self.tx.send(cmd).is_ok() {
                (self.wake)();
            }
        }
    }

    /// `org.mpris.MediaPlayer2`
    struct Root {
        out: Out,
        identity: String,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl Root {
        fn raise(&self) {
            self.out.send(MediaCommand::Raise);
        }

        fn quit(&self) {
            self.out.send(MediaCommand::Quit);
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn can_quit(&self) -> bool {
            true
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn can_raise(&self) -> bool {
            true
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn has_track_list(&self) -> bool {
            false
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn identity(&self) -> String {
            self.identity.clone()
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn desktop_entry(&self) -> String {
            DESKTOP_ENTRY.to_owned()
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn supported_uri_schemes(&self) -> Vec<String> {
            URI_SCHEMES.iter().map(|s| (*s).to_owned()).collect()
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn supported_mime_types(&self) -> Vec<String> {
            MIME_TYPES.iter().map(|s| (*s).to_owned()).collect()
        }
    }

    /// `org.mpris.MediaPlayer2.Player`
    struct Player {
        out: Out,
        shared: Arc<Mutex<Shared>>,
    }

    impl Player {
        fn props(&self) -> Props {
            lock(&self.shared).now.props()
        }

        /// 設定可以寫的屬性：先改系統顯示的值、記成已經送出（zbus 設定完會自己送這個屬性），再送指令給介面
        fn set(&self, cmd: MediaCommand) {
            {
                let mut s = lock(&self.shared);
                if let Some(name) = s.now.request(&cmd, Instant::now()) {
                    let props = s.now.props();
                    s.emitted.adopt(&props, name);
                }
            }
            self.out.send(cmd);
        }
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl Player {
        fn next(&self) {
            self.out.send(MediaCommand::Next);
        }

        fn previous(&self) {
            self.out.send(MediaCommand::Previous);
        }

        fn pause(&self) {
            self.out.send(MediaCommand::Pause);
        }

        fn play_pause(&self) {
            self.out.send(MediaCommand::PlayPause);
        }

        fn stop(&self) {
            self.out.send(MediaCommand::Stop);
        }

        fn play(&self) {
            self.out.send(MediaCommand::Play);
        }

        fn seek(&self, offset: i64) {
            self.out.send(seek_command(offset));
        }

        fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
            let cmd = set_position_command(&lock(&self.shared).now, track_id.as_str(), position);
            if let Some(cmd) = cmd {
                self.out.send(cmd);
            }
        }

        fn open_uri(&self, uri: String) -> zbus::fdo::Result<()> {
            let cmd = open_uri_command(&uri).ok_or_else(|| zbus::fdo::Error::NotSupported(uri.clone()))?;
            self.out.send(cmd);
            Ok(())
        }

        #[zbus(signal)]
        async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

        #[zbus(property)]
        fn playback_status(&self) -> String {
            self.props().status.to_owned()
        }

        #[zbus(property)]
        fn loop_status(&self) -> String {
            self.props().loop_status.to_owned()
        }

        #[zbus(property)]
        fn set_loop_status(&self, value: String) -> zbus::fdo::Result<()> {
            let cmd = loop_command(&value).ok_or_else(|| zbus::fdo::Error::InvalidArgs(value.clone()))?;
            self.set(cmd);
            Ok(())
        }

        #[zbus(property)]
        fn rate(&self) -> f64 {
            self.props().rate
        }

        #[zbus(property)]
        fn set_rate(&self, value: f64) {
            self.set(MediaCommand::SetRate(value));
        }

        #[zbus(property)]
        fn shuffle(&self) -> bool {
            self.props().shuffle
        }

        #[zbus(property)]
        fn set_shuffle(&self, value: bool) {
            self.set(MediaCommand::SetShuffle(value));
        }

        #[zbus(property)]
        fn metadata(&self) -> HashMap<String, OwnedValue> {
            meta_map(&self.props().metadata)
        }

        #[zbus(property)]
        fn volume(&self) -> f64 {
            self.props().volume
        }

        #[zbus(property)]
        fn set_volume(&self, value: f64) {
            self.set(MediaCommand::SetVolume(value));
        }

        /// 系統要的時候才讀（照速度往後推算），不送 PropertiesChanged（MPRIS 規格）
        #[zbus(property(emits_changed_signal = "false"))]
        fn position(&self) -> i64 {
            lock(&self.shared).now.position_us(Instant::now())
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn minimum_rate(&self) -> f64 {
            RATE_RANGE.0
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn maximum_rate(&self) -> f64 {
            RATE_RANGE.1
        }

        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            self.props().can_go_next
        }

        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            self.props().can_go_previous
        }

        #[zbus(property)]
        fn can_play(&self) -> bool {
            self.props().can_play
        }

        #[zbus(property)]
        fn can_pause(&self) -> bool {
            self.props().can_pause
        }

        #[zbus(property)]
        fn can_seek(&self) -> bool {
            self.props().can_seek
        }

        #[zbus(property(emits_changed_signal = "const"))]
        fn can_control(&self) -> bool {
            true
        }
    }

    /// Metadata → `a{sv}`
    fn meta_map(m: &Meta) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        let mut put = |key: &str, v: Value<'_>| {
            if let Ok(v) = OwnedValue::try_from(v) {
                map.insert(key.to_owned(), v);
            }
        };
        if let Ok(id) = ObjectPath::try_from(m.track_id.as_str()) {
            put("mpris:trackid", Value::from(id));
        }
        if let Some(us) = m.length_us {
            put("mpris:length", Value::from(us));
        }
        if let Some(t) = &m.title {
            put("xesam:title", Value::from(t.as_str()));
        }
        if !m.artist.is_empty() {
            put("xesam:artist", Value::from(m.artist.clone()));
        }
        if let Some(a) = &m.album {
            put("xesam:album", Value::from(a.as_str()));
        }
        if !m.album_artist.is_empty() {
            put("xesam:albumArtist", Value::from(m.album_artist.clone()));
        }
        if let Some(n) = m.track_number {
            put("xesam:trackNumber", Value::from(n));
        }
        if let Some(u) = &m.url {
            put("xesam:url", Value::from(u.as_str()));
        }
        if let Some(u) = &m.art_url {
            put("mpris:artUrl", Value::from(u.as_str()));
        }
        map
    }

    /// 一個屬性的值（PropertiesChanged 用）
    fn prop_value(p: &Props, name: &str) -> Option<Value<'static>> {
        Some(match name {
            "PlaybackStatus" => Value::from(p.status),
            "LoopStatus" => Value::from(p.loop_status),
            "Rate" => Value::from(p.rate),
            "Shuffle" => Value::from(p.shuffle),
            "Metadata" => Value::from(meta_map(&p.metadata)),
            "Volume" => Value::from(p.volume),
            "CanGoNext" => Value::from(p.can_go_next),
            "CanGoPrevious" => Value::from(p.can_go_previous),
            "CanPlay" => Value::from(p.can_play),
            "CanPause" => Value::from(p.can_pause),
            "CanSeek" => Value::from(p.can_seek),
            _ => return None,
        })
    }

    // ───────────── 影戲 → 系統（背景執行緒）─────────────

    struct Worker {
        /// 結束時 take() 出來關掉
        conn: Option<Connection>,
        bus_name: String,
        shared: Arc<Mutex<Shared>>,
        alive: Arc<AtomicBool>,
        wake: Wake,
        debug: bool,
    }

    impl Drop for Worker {
        /// 放掉名稱、關掉連線（結束時；執行緒開不起來、worker 直接被丟掉時也一樣）
        fn drop(&mut self) {
            if let Some(conn) = self.conn.take() {
                if !conn.is_closed() {
                    let _ = conn.release_name(self.bus_name.as_str());
                }
                let _ = conn.close();
            }
        }
    }

    impl Worker {
        fn run(mut self, jobs: Receiver<Job>) {
            let done = loop {
                let first = match jobs.recv_timeout(WATCH_EVERY) {
                    Ok(job) => job,
                    Err(RecvTimeoutError::Timeout) => {
                        self.check_connection();
                        continue;
                    }
                    // 介面那一端沒說結束就不見了：一樣收拾
                    Err(RecvTimeoutError::Disconnected) => break None,
                };
                // 一次處理排著的所有更新（換檔時的 Track、Cover、Playback），合成一個 PropertiesChanged
                let mut seeked = false;
                let mut done = None;
                for job in std::iter::once(first).chain(jobs.try_iter()) {
                    match job {
                        Job::Apply(u) => seeked |= lock(&self.shared).now.apply(&u, Instant::now()),
                        Job::Shutdown(ack) => {
                            done = Some(ack);
                            break;
                        }
                    }
                }
                if done.is_some() {
                    break done;
                }
                self.publish(seeked);
            };
            // 收拾（Drop）完才回覆
            drop(self);
            if let Some(done) = done {
                let _ = done.send(());
            }
        }

        /// 送出變了的屬性；`seeked` 時再送 Seeked（現在的位置）
        fn publish(&mut self, seeked: bool) {
            let (props, names, position) = {
                let mut s = lock(&self.shared);
                let props = s.now.props();
                let names = props.changed_since(&s.emitted);
                s.emitted = props.clone();
                (props, names, s.now.position_us(Instant::now()))
            };
            if !names.is_empty() {
                let changed: HashMap<&str, Value<'_>> = names
                    .iter()
                    .filter_map(|&n| prop_value(&props, n).map(|v| (n, v)))
                    .collect();
                self.emit(
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(PLAYER_IFACE, changed, Vec::<&str>::new()),
                );
            }
            if seeked {
                self.emit(PLAYER_IFACE, "Seeked", &(position,));
            }
        }

        fn emit<B>(&self, iface: &str, signal: &str, body: &B)
        where
            B: serde::Serialize + zbus::zvariant::DynamicType,
        {
            let Some(conn) = &self.conn else { return };
            if let Err(e) = conn.emit_signal(None::<&str>, OBJECT_PATH, iface, signal, body) {
                if self.debug {
                    eprintln!("[media] MPRIS 無法送出 {signal}：{e}");
                }
                self.check_connection();
            }
        }

        /// 連線斷了（工作階段匯流排不見了）：通知介面，下一個檔案時重新建立
        fn check_connection(&self) {
            let closed = self.conn.as_ref().is_none_or(Connection::is_closed);
            if closed && self.alive.swap(false, Ordering::Relaxed) {
                if self.debug {
                    eprintln!("[media] MPRIS 的 D-Bus 連線斷了");
                }
                (self.wake)();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Cover, MediaKind};
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    fn track(file_gen: u64) -> TrackInfo {
        TrackInfo {
            file_gen,
            kind: MediaKind::Audio,
            title: "歌".into(),
            artist: Some("人".into()),
            album: Some("專輯".into()),
            album_artist: None,
            track_no: Some(3),
            duration: Some(100.0),
            url: Some("file:///a/b.mp3".into()),
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
            shuffle: false,
            repeat: Repeat::Off,
            volume: 0.5,
            buffering: false,
            can_play: true,
        }
    }

    fn cover(file_gen: u64) -> Cover {
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"C:\cache\covers\cover-1-{file_gen}.png"));
        #[cfg(not(windows))]
        let path = PathBuf::from(format!("/cache/covers/cover-1-{file_gen}.png"));
        Cover {
            file_gen,
            png: Arc::new(vec![1]),
            path,
        }
    }

    fn secs(s: f64) -> Duration {
        Duration::from_secs_f64(s)
    }

    #[test]
    fn names_and_track_ids() {
        assert_eq!(instance_name(4242), "org.mpris.MediaPlayer2.vitascope.instance4242");
        assert_eq!(track_id(7), "/io/github/acer1204/vitascope/track/7");
        assert_eq!(track_from_id(&track_id(7)), Some(7));
        assert_eq!(track_from_id(NO_TRACK), None);
        assert_eq!(track_from_id("/io/github/acer1204/vitascope/track/"), None);
        assert_eq!(track_from_id("/io/github/acer1204/vitascope/track/1/2"), None);
        assert_eq!(track_from_id("/io/github/acer1204/vitascope/track/-1"), None);
        // D-Bus 的名稱：每一段不能用數字開頭、只能是英數字與底線
        for name in [BUS_NAME.to_owned(), instance_name(u32::MAX)] {
            assert!(
                name.split('.').all(|part| !part.is_empty()
                    && !part.starts_with(|c: char| c.is_ascii_digit())
                    && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')),
                "{name}"
            );
        }
    }

    #[test]
    fn mpris_metadata_types() {
        let t0 = Instant::now();
        let mut s = Snapshot::new(t0);
        // 還沒有檔案：MPRIS 規定的 NoTrack，其他都沒有
        assert_eq!(
            s.props().metadata,
            Meta {
                track_id: NO_TRACK.into(),
                ..Default::default()
            }
        );
        s.apply(&Update::Track(track(5)), t0);
        let m = s.props().metadata;
        assert_eq!(m.track_id, "/io/github/acer1204/vitascope/track/5");
        assert_eq!(m.length_us, Some(100_000_000), "微秒");
        assert_eq!(m.title.as_deref(), Some("歌"));
        assert_eq!(m.artist, ["人"], "演出者是字串陣列");
        assert_eq!(m.album.as_deref(), Some("專輯"));
        assert!(m.album_artist.is_empty());
        assert_eq!(m.track_number, Some(3));
        assert_eq!(m.url.as_deref(), Some("file:///a/b.mp3"));
        assert_eq!(m.art_url, None);
        // 封面：這個檔案的才用，網址是 file://
        s.apply(&Update::Cover(cover(4)), t0);
        assert_eq!(s.props().metadata.art_url, None, "別的檔案的封面");
        s.apply(&Update::Cover(cover(5)), t0);
        let art = s.props().metadata.art_url.unwrap();
        assert!(art.starts_with("file:///") && art.ends_with("cover-1-5.png"), "{art}");
        assert_eq!(super::super::uri_target(&art), Some(cover(5).path));
        // 同一個檔案換標題：封面還在；隱藏片名與封面時不給
        let mut renamed = track(5);
        renamed.title = "ICY".into();
        s.apply(&Update::Track(renamed.clone()), t0);
        assert!(s.props().metadata.art_url.is_some());
        renamed.details = false;
        s.apply(&Update::Track(renamed), t0);
        assert_eq!(s.props().metadata.art_url, None);
        // 換檔：上一個檔案的封面丟掉；直播沒有長度
        let mut live = track(6);
        live.duration = None;
        s.apply(&Update::Track(live), t0);
        let m = s.props().metadata;
        assert_eq!((m.art_url, m.length_us), (None, None));
        // 停止：回到 NoTrack
        s.apply(&Update::Clear, t0);
        assert_eq!(s.props().metadata.track_id, NO_TRACK);
    }

    #[test]
    fn props_follow_the_playback_state() {
        let t0 = Instant::now();
        let mut s = Snapshot::new(t0);
        let p = s.props();
        assert_eq!(p.status, "Stopped");
        assert!(!p.can_play && !p.can_pause && !p.can_seek && !p.can_go_next);
        s.apply(&Update::Track(track(1)), t0);
        let mut info = playback(Status::Playing, 0.0);
        info.repeat = Repeat::All;
        info.shuffle = true;
        info.rate = 1.5;
        assert!(
            s.apply(&Update::Playback { info, seeked: true }, t0),
            "跳轉：要送 Seeked"
        );
        let p = s.props();
        assert_eq!(
            (p.status, p.loop_status, p.rate, p.shuffle, p.volume),
            ("Playing", "Playlist", 1.5, true, 0.5)
        );
        assert!(p.can_play && p.can_pause && p.can_seek && p.can_go_next && !p.can_go_previous);
        assert!(!s.apply(&Update::Track(track(1)), t0));
        // 不能跳轉（直播）、暫停
        let mut info = playback(Status::Paused, 3.0);
        info.seekable = false;
        assert!(!s.apply(&Update::Playback { info, seeked: false }, t0));
        let p = s.props();
        assert_eq!(p.status, "Paused");
        assert!(!p.can_seek && p.can_pause);
        // 速度不能是 0 或不是數字（暫停是 PlaybackStatus）；音量不能是負的
        let mut info = playback(Status::Playing, 3.0);
        info.rate = 0.0;
        info.volume = f64::NAN;
        s.apply(&Update::Playback { info, seeked: false }, t0);
        assert_eq!((s.props().rate, s.props().volume), (1.0, 0.0));
        // 停止之後清單上還有停止的檔案：可以播，不能暫停、跳轉
        s.apply(&Update::Clear, t0);
        let p = s.props();
        assert_eq!(p.status, "Stopped");
        assert!(p.can_play && !p.can_pause && !p.can_seek);
        assert!(p.can_go_next, "清單的狀態不變");
        // 停止時清單上沒有東西可播了：SessionSync 在 Clear 之前送停止後的狀態 → 不能播
        s.apply(&Update::Track(track(2)), t0);
        s.apply(
            &Update::Playback {
                info: playback(Status::Playing, 1.0),
                seeked: true,
            },
            t0,
        );
        let mut stopped = playback(Status::Playing, 1.0).cleared();
        stopped.can_play = false;
        s.apply(
            &Update::Playback {
                info: stopped,
                seeked: false,
            },
            t0,
        );
        s.apply(&Update::Clear, t0);
        let p = s.props();
        assert_eq!(p.status, "Stopped");
        assert!(!p.can_play && !p.can_pause && !p.can_seek);
        assert_eq!(p.metadata.track_id, NO_TRACK);
        assert_eq!(RATE_RANGE, (0.25, 4.0));
    }

    /// 系統設定可以寫的屬性：系統顯示的值先改成要的值（zbus 設定完自己送的 PropertiesChanged 才是新的值）
    #[test]
    fn set_properties_show_the_requested_value() {
        let t0 = Instant::now();
        let mut s = Snapshot::new(t0);
        // 沒有檔案：不能改速度（影戲不會照做）；其他可以
        assert_eq!(s.request(&MediaCommand::SetRate(2.0), t0), None);
        assert_eq!(s.request(&MediaCommand::SetShuffle(true), t0), Some("Shuffle"));
        assert!(s.props().shuffle);
        s.apply(&Update::Track(track(1)), t0);
        s.apply(
            &Update::Playback {
                info: playback(Status::Playing, 10.0),
                seeked: true,
            },
            t0,
        );
        let before = s.props();
        assert_eq!(s.request(&MediaCommand::SetRepeat(Repeat::One), t0), Some("LoopStatus"));
        assert_eq!(s.request(&MediaCommand::SetVolume(-1.0), t0), Some("Volume"));
        assert_eq!(s.request(&MediaCommand::SetRate(9.0), t0 + secs(2.0)), Some("Rate"));
        let p = s.props();
        assert_eq!(
            (p.loop_status, p.volume, p.rate),
            ("Track", 0.0, 4.0),
            "跟影戲一樣限制範圍"
        );
        // 換速度：之前的照舊速度算，之後照新的
        assert_eq!(s.position_us(t0 + secs(2.0)), 12_000_000);
        assert_eq!(s.position_us(t0 + secs(3.0)), 16_000_000);
        // 不會改的：速度 0（暫停）、不是數字、不是設定
        for cmd in [
            MediaCommand::SetRate(0.0),
            MediaCommand::SetRate(f64::NAN),
            MediaCommand::SetVolume(f64::INFINITY),
            MediaCommand::Next,
        ] {
            assert_eq!(s.request(&cmd, t0), None, "{cmd:?}");
        }
        // 已經送出的只算那一個屬性：其他還沒送出的變化照樣要送
        let mut emitted = before.clone();
        emitted.adopt(&s.props(), "Volume");
        assert_eq!(s.props().changed_since(&emitted), ["LoopStatus", "Rate"]);
    }

    /// PropertiesChanged 只帶變了的屬性；位置一直在變，但不算
    #[test]
    fn only_changed_properties_are_reported() {
        let t0 = Instant::now();
        let mut s = Snapshot::new(t0);
        let start = s.props();
        assert!(start.changed_since(&start).is_empty());
        s.apply(&Update::Track(track(1)), t0);
        s.apply(
            &Update::Playback {
                info: playback(Status::Playing, 0.0),
                seeked: true,
            },
            t0,
        );
        let playing = s.props();
        assert_eq!(
            playing.changed_since(&start),
            [
                "PlaybackStatus",
                "Metadata",
                "Volume",
                "CanGoNext",
                "CanPlay",
                "CanPause",
                "CanSeek"
            ]
        );
        // 播放中每 5 秒的進度更新：只有位置變了，什麼都不送
        s.apply(
            &Update::Playback {
                info: playback(Status::Playing, 5.0),
                seeked: false,
            },
            t0 + secs(5.0),
        );
        assert!(s.props().changed_since(&playing).is_empty());
        // 暫停：只有 PlaybackStatus
        s.apply(
            &Update::Playback {
                info: playback(Status::Paused, 5.0),
                seeked: false,
            },
            t0 + secs(5.1),
        );
        assert_eq!(s.props().changed_since(&playing), ["PlaybackStatus"]);
        // 一個一個改：每次只有那一個
        for (change, name) in [
            (
                (|p: &mut PlaybackInfo| p.repeat = Repeat::One) as fn(&mut PlaybackInfo),
                "LoopStatus",
            ),
            (|p| p.rate = 2.0, "Rate"),
            (|p| p.shuffle = true, "Shuffle"),
            (|p| p.volume = 1.2, "Volume"),
            (|p| p.can_prev = true, "CanGoPrevious"),
            (|p| p.seekable = false, "CanSeek"),
        ] {
            let before = s.props();
            let mut info = playback(Status::Paused, 5.0);
            change(&mut info);
            s.apply(&Update::Playback { info, seeked: false }, t0);
            assert_eq!(s.props().changed_since(&before), [name]);
            s.apply(
                &Update::Playback {
                    info: playback(Status::Paused, 5.0),
                    seeked: false,
                },
                t0,
            );
        }
        // 封面到了：只有 Metadata
        let before = s.props();
        s.apply(&Update::Cover(cover(1)), t0);
        assert_eq!(s.props().changed_since(&before), ["Metadata"]);
    }

    #[test]
    fn position_is_extrapolated_while_playing() {
        let t0 = Instant::now();
        let mut s = Snapshot::new(t0);
        assert_eq!(s.position_us(t0 + secs(3.0)), 0, "停止");
        s.apply(&Update::Track(track(1)), t0);
        let mut info = playback(Status::Playing, 10.0);
        info.rate = 2.0;
        s.apply(&Update::Playback { info, seeked: true }, t0);
        assert_eq!(s.position_us(t0), 10_000_000);
        assert_eq!(s.position_us(t0 + secs(1.5)), 13_000_000, "照速度往後推算");
        assert_eq!(s.position_us(t0 + secs(100.0)), 100_000_000, "不超過總長度");
        // 暫停、等快取：不動
        s.apply(
            &Update::Playback {
                info: playback(Status::Paused, 20.0),
                seeked: false,
            },
            t0,
        );
        assert_eq!(s.position_us(t0 + secs(9.0)), 20_000_000);
        let mut stalled = playback(Status::Playing, 21.0);
        stalled.buffering = true;
        s.apply(
            &Update::Playback {
                info: stalled,
                seeked: false,
            },
            t0,
        );
        assert_eq!(s.position_us(t0 + secs(9.0)), 21_000_000);
        // 時間是從收到更新的那一刻算
        s.apply(
            &Update::Playback {
                info: playback(Status::Playing, 30.0),
                seeked: false,
            },
            t0 + secs(50.0),
        );
        assert_eq!(s.position_us(t0 + secs(51.0)), 31_000_000);
        s.apply(&Update::Clear, t0 + secs(52.0));
        assert_eq!(s.position_us(t0 + secs(60.0)), 0);
    }

    #[test]
    fn commands_from_the_bus() {
        let t0 = Instant::now();
        let mut s = Snapshot::new(t0);
        assert_eq!(seek_command(-5_500_000), MediaCommand::SeekBy(-5.5));
        // SetPosition：沒有檔案時不理
        assert_eq!(set_position_command(&s, &track_id(2), 1_000_000), None);
        s.apply(&Update::Track(track(2)), t0);
        assert_eq!(
            set_position_command(&s, &track_id(2), 12_500_000),
            Some(MediaCommand::SeekTo {
                secs: 12.5,
                track: Some(2)
            })
        );
        // 別的檔案的（換檔之前拖的）、負的、超過總長度、不是影戲的 track id：不理
        assert_eq!(set_position_command(&s, &track_id(1), 1_000_000), None);
        assert_eq!(set_position_command(&s, &track_id(2), -1), None);
        assert_eq!(set_position_command(&s, &track_id(2), 100_000_001), None);
        assert!(set_position_command(&s, &track_id(2), 100_000_000).is_some());
        assert_eq!(set_position_command(&s, NO_TRACK, 0), None);
        // LoopStatus
        assert_eq!(loop_command("Track"), Some(MediaCommand::SetRepeat(Repeat::One)));
        assert_eq!(loop_command("Playlist"), Some(MediaCommand::SetRepeat(Repeat::All)));
        assert_eq!(loop_command("None"), Some(MediaCommand::SetRepeat(Repeat::Off)));
        assert_eq!(loop_command("track"), None);
        // OpenUri：file://、http(s)://（N-M12）；其他的不開
        #[cfg(windows)]
        let file = super::super::file_url(Path::new(r"C:\影片\a b.mkv")).unwrap();
        #[cfg(not(windows))]
        let file = super::super::file_url(Path::new("/影片/a b.mkv")).unwrap();
        assert_eq!(open_uri_command(&file), Some(MediaCommand::OpenUri(file.clone())));
        for uri in ["https://example.com/a.m3u8", " http://example.com/a.mp4 "] {
            assert_eq!(
                open_uri_command(uri),
                Some(MediaCommand::OpenUri(uri.trim().to_owned()))
            );
        }
        for uri in ["ftp://example.com/a.mp4", "smb://x/y", "不是網址", ""] {
            assert_eq!(open_uri_command(uri), None, "{uri}");
        }
        // SupportedUriSchemes 列的就是能開的
        assert_eq!(URI_SCHEMES, ["file", "http", "https"]);
    }

    /// 能開的檔案類型跟 .desktop 檔一致（桌面環境用它們決定能不能交給影戲開）
    #[test]
    fn mime_types_match_the_desktop_file() {
        // DesktopEntry 是 .desktop 檔的名稱（不含副檔名）
        let desktop = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("packaging/linux/{DESKTOP_ENTRY}.desktop")),
        )
        .unwrap();
        let line = desktop
            .lines()
            .find_map(|l| l.strip_prefix("MimeType="))
            .expect(".desktop 檔有 MimeType");
        let listed: Vec<&str> = line.split(';').filter(|m| !m.is_empty()).collect();
        assert_eq!(listed, MIME_TYPES);
    }
}
