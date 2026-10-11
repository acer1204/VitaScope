//! 系統的媒體控制接上播放器（媒體鍵、Windows 的媒體控制、macOS 的「正在播放」、Linux 的 MPRIS；
//! 各系統共用的部分見 [`crate::media_keys`]）。
//!
//! - 第一次有檔案載入時才建立工作階段（沒有在播時系統的媒體控制不會多出影戲）；關掉設定、結束時結束。
//!   建立失敗：記下原因（「設定 → 系統」顯示），關掉再打開設定之前不再試。
//!   系統那一端斷線了（`MediaSession::alive`；例如 Windows 的檔案總管重開）：結束它，下一個檔案載入時重新建立。
//! - 系統的指令：播放、暫停、停止、上一個 / 下一個跟按鍵走同一條路（`Command` → `Action` → `run`），
//!   所以也會取消播完後的倒數；沒有在播時的「播放」開停止前的那個檔案（還原的清單開接下來要播的）。
//!   指令不會叫出控制列（不算滑鼠、鍵盤的動作），提示照常顯示。
//! - 送給系統的內容由 [`SessionSync`] 決定（變了才送）；標題、標籤只在換檔或標籤變了時重新整理（不是每一幀）。
//! - 音訊檔的專輯封面：影像設定好之後請 mpv 存成 PNG（`AsyncKey::Cover`，mpv 自己的執行緒），在背景讀進來再送。
//!   要有快取資料夾（`Launch.cache_root`；自動測試、`--shot` 沒有：不存封面、也不清）。
//!   失敗時在下一次開始播放（跳轉完成）時再試一次，之後就不要封面。

use super::after_playback::Cancel;
use super::{Action, VitascopeApp, app_name, display_label};
use crate::media_keys::{
    self, Cover, MediaBackend, MediaCommand, MediaKind, MediaSession, Now, PlaybackInfo, SessionSync, Status, Step,
    TrackInfo, Wake,
};
use crate::player::{AsyncKey, TrackKind};
use crate::theme::Palette;
use crate::ytdl::plan::NetInfo;
use crate::{tf, tr};
use eframe::egui;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// 啟動時清掉多久以前的封面（當掉、被強制結束時留下的）
const STALE_COVERS: Duration = Duration::from_secs(24 * 3600);

/// 系統媒體控制的狀態
#[derive(Default)]
pub(super) struct MediaUi {
    backend: MediaBackend,
    session: Option<Box<dyn MediaSession>>,
    commands: Option<Receiver<MediaCommand>>,
    /// 建立工作階段失敗的原因（「設定 → 系統」顯示；關掉再打開設定之前不再試）
    error: Option<String>,
    /// 工作階段斷線時系統顯示的檔案（這個檔案播放期間不重新建立，換檔再試：不會每一幀一直重建）
    lost_at: Option<u64>,
    sync: SessionSync,
    /// 目前檔案的資訊、算它用的來源（來源變了才重新整理）
    track: Option<(TrackKey, TrackInfo)>,
    /// 目前檔案的封面；None = 還沒開始處理這個檔案
    cover: Option<CoverJob>,
    /// 封面放的資料夾（`<快取>/covers`）；None = 不存封面（自動測試、`--shot`）
    covers_dir: Option<PathBuf>,
    /// 這次執行請 mpv 存過封面（之後換檔、結束時都要清：mpv 在自己的執行緒寫檔，可能換檔之後才寫好）
    covers_made: bool,
    /// 送給系統的更新數、整理過幾次檔案資訊（介面測試用）
    applied: u64,
    track_builds: u64,
}

impl MediaUi {
    /// `cache_root`：有的話，封面放在 `<快取>/covers`，在背景清掉一天以上的舊封面
    pub(super) fn new(backend: MediaBackend, cache_root: Option<&Path>) -> Self {
        let covers_dir = cache_root.map(media_keys::covers_dir);
        if let Some(dir) = covers_dir.clone() {
            std::thread::spawn(move || media_keys::sweep_covers(&dir, STALE_COVERS));
        }
        Self {
            backend,
            covers_dir,
            ..Default::default()
        }
    }
}

/// 算 [`TrackInfo`] 用的來源：跟上次一樣就不重新整理（每一幀只比對，不配置記憶體）
#[derive(Debug)]
pub(super) struct TrackKey {
    file_gen: u64,
    details: bool,
    en: bool,
    has_video: bool,
    path: Option<String>,
    title: Option<String>,
    metadata: BTreeMap<String, String>,
    duration: Option<f64>,
    net: Option<Arc<NetInfo>>,
    /// 播放清單、紀錄裡這個網址的標題
    known: Option<String>,
}

/// 目前檔案的封面處理到哪裡
struct CoverJob {
    file_gen: u64,
    state: CoverState,
    /// 已經重試過一次
    retried: bool,
}

enum CoverState {
    /// 還沒要（等影像設定好、等系統的工作階段）
    Idle,
    /// 請 mpv 存了，等回覆
    Asked { id: u64, path: PathBuf },
    /// mpv 存不了：下一次開始播放時再試一次
    Retry,
    /// 在背景讀檔
    Reading(Receiver<std::io::Result<Vec<u8>>>, PathBuf),
    /// 送出了、或不要了
    Done,
}

impl VitascopeApp {
    /// 測試用：系統媒體控制的工作階段開著
    #[doc(hidden)]
    pub fn media_session_active(&self) -> bool {
        self.media.session.is_some()
    }

    /// 測試用：建立工作階段失敗的原因
    #[doc(hidden)]
    pub fn media_error(&self) -> Option<&str> {
        self.media.error.as_deref()
    }

    /// 測試用：系統那一端斷線、等下一個檔案重新建立
    #[doc(hidden)]
    pub fn media_session_lost(&self) -> bool {
        self.media.lost_at.is_some()
    }

    /// 測試用：送給系統的更新數
    #[doc(hidden)]
    pub fn media_updates_applied(&self) -> u64 {
        self.media.applied
    }

    /// 測試用：目前檔案的封面處理到哪裡（`none`、`idle`、`asked`、`retry`、`reading`、`done`）
    #[doc(hidden)]
    pub fn media_cover_state(&self) -> &'static str {
        match self.media.cover.as_ref().map(|c| &c.state) {
            None => "none",
            Some(CoverState::Idle) => "idle",
            Some(CoverState::Asked { .. }) => "asked",
            Some(CoverState::Retry) => "retry",
            Some(CoverState::Reading(..)) => "reading",
            Some(CoverState::Done) => "done",
        }
    }

    /// 測試用：整理過幾次檔案資訊（換檔、標籤變了才整理，不是每一幀）
    #[doc(hidden)]
    pub fn media_track_builds(&self) -> u64 {
        self.media.track_builds
    }

    /// 每一幀（處理完 mpv 的事件之後；視窗縮到最小時也會跑）：建立 / 結束工作階段、處理系統的指令、送更新、封面
    pub(super) fn media_tick(&mut self, ctx: &egui::Context) {
        if !self.settings.system_media.enabled {
            self.media_end();
            // 關掉再打開時重新試
            self.media.error = None;
            self.media.lost_at = None;
            return;
        }
        if self.media.session.as_ref().is_some_and(|s| !s.alive()) {
            if std::env::var_os("VITASCOPE_DEBUG_MEDIA").is_some() {
                eprintln!("[media] 系統媒體控制斷線了，下一個檔案時重新建立");
            }
            // 記下斷線時系統顯示的檔案（不是 `file_gen`：換檔途中 StartFile 已經加過了，那是正在載入的檔案，
            // 它載入時要重新建立）。沒有顯示任何檔案時 None：下一個載入的檔案就重新建立
            self.media.lost_at = self.media.sync.sent_track().map(|t| t.file_gen);
            self.media_end();
        }
        if self.media.session.is_none() && !self.media_start(ctx) {
            return;
        }
        let commands: Vec<MediaCommand> = self
            .media
            .commands
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for cmd in commands {
            self.media_command(ctx, cmd);
        }
        self.media_sync(Instant::now());
        self.cover_tick();
    }

    /// 第一次有檔案載入時建立工作階段；回傳有沒有工作階段
    fn media_start(&mut self, ctx: &egui::Context) -> bool {
        if !self.media.backend.available()
            || self.media.error.is_some()
            || !self.player.state.loaded
            || self.media.lost_at == Some(self.file_gen)
        {
            return false;
        }
        let ctx = ctx.clone();
        let wake: Wake = Arc::new(move || ctx.request_repaint());
        let owner = self.owner.map(|o| o.window);
        match self.media.backend.create(owner, app_name(), wake) {
            Ok((session, commands)) => {
                self.media.session = Some(session);
                self.media.commands = Some(commands);
                self.media.sync = SessionSync::new();
                self.media.cover = None;
                self.media.lost_at = None;
                true
            }
            Err(e) => {
                eprintln!("[vitascope] 無法啟用系統媒體控制：{e}");
                self.media.error = Some(e);
                false
            }
        }
    }

    /// 結束工作階段（關掉設定、結束影戲），刪掉這次存的封面
    pub(super) fn media_end(&mut self) {
        if let Some(mut session) = self.media.session.take() {
            session.shutdown();
        }
        self.media.commands = None;
        self.media.sync = SessionSync::new();
        self.media.track = None;
        self.media.cover = None;
        self.remove_covers();
    }

    fn remove_covers(&mut self) {
        if self.media.covers_made
            && let Some(dir) = &self.media.covers_dir
        {
            media_keys::remove_own_covers(dir, std::process::id(), None);
        }
    }

    // ───────────── 系統的指令 ─────────────

    /// 系統送來的一個指令
    fn media_command(&mut self, ctx: &egui::Context, cmd: MediaCommand) {
        if std::env::var_os("VITASCOPE_DEBUG_MEDIA").is_some() {
            eprintln!("[media] {cmd:?}");
        }
        let st = &self.player.state;
        let now = Now {
            loaded: st.loaded,
            opening: st.loading || self.switching_file || self.player.loading_now(),
            // 直接問 mpv：介面記下的可能還沒更新（跟按鍵的播放 / 暫停一樣）
            paused: st.loaded && self.player.paused_now(),
            file_gen: self.file_gen,
        };
        match media_keys::step(&cmd, now) {
            Step::Command(c) => self.run_command(c),
            Step::PlayStopped => self.run(ctx, Action::Play),
            Step::Nothing => {
                if self.countdown_running() {
                    self.cancel_countdown(Cancel::Input);
                }
            }
            Step::SeekBy(d) => self.run(ctx, Action::Seek(d)),
            Step::SeekTo(t) => self.run(ctx, Action::SeekTo(t)),
            Step::Rate(r) => self.run(ctx, Action::SetSpeed(r)),
            Step::Shuffle(on) => self.run(ctx, Action::SetShuffle(on)),
            // VITASCOPE_MPV_OPTS 指定了 loop-file：跟快捷鍵一樣只提示原因（`cycle_repeat`）
            Step::Repeat(_) if self.repeat_locked() => self.run(ctx, Action::CycleRepeat),
            Step::Repeat(r) => self.run(ctx, Action::SetRepeat(r)),
            Step::Volume(v) => self.run(ctx, Action::SetVolumeTotal(v * 100.0)),
            Step::Raise => self.run(ctx, Action::Raise),
            Step::Quit => self.run(ctx, Action::Quit),
            // 開檔會取消倒數（不提示，跟開了別的檔案一樣）；只有字幕時不會開檔，這裡先取消
            Step::Open(path) => {
                if self.countdown_running() {
                    self.cancel_countdown(Cancel::Quiet);
                }
                self.open_paths(vec![path], false)
            }
        }
    }

    /// 沒有在播時的「播放」：開清單上停止前的那個檔案（停止不會改清單）；
    /// 目前的已經移出清單、或是還原的清單（還沒開始播）時，開接下來要播的那一個（S-B10）
    pub(super) fn play_stopped(&mut self) {
        let target = self
            .playlist
            .as_ref()
            .and_then(|l| l.current_index().or_else(|| l.next_index()));
        if let Some(i) = target {
            self.open_list_item(i);
        }
    }

    // ───────────── 送給系統的內容 ─────────────

    fn media_sync(&mut self, now: Instant) {
        let st = &self.player.state;
        // 換檔途中（舊的卸載了、新的還沒載入）：先不送「停止」，系統的顯示才不會閃一下
        if !st.loaded && (st.loading || self.switching_file) {
            return;
        }
        if st.loaded
            && !self
                .media
                .track
                .as_ref()
                .is_some_and(|(k, _)| self.track_key_matches(k))
        {
            let info = self.track_info();
            self.media.track = Some((self.track_key(), info));
            self.media.track_builds += 1;
        }
        let playback = self.playback_info();
        let loaded = self.player.state.loaded;
        let MediaUi {
            sync,
            track,
            session,
            applied,
            ..
        } = &mut self.media;
        let Some(session) = session else { return };
        let track = track.as_ref().filter(|_| loaded).map(|(_, t)| t);
        for u in sync.tick(track, &playback, now) {
            session.apply(&u);
            *applied += 1;
        }
    }

    fn track_key_matches(&self, k: &TrackKey) -> bool {
        let st = &self.player.state;
        let net_same = match (&k.net, &st.net) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        k.file_gen == self.file_gen
            && k.details == self.settings.system_media.details
            && k.en == crate::i18n::is_en()
            && k.has_video == st.has_video()
            && k.path == st.path
            && k.title == st.title
            && k.metadata == st.metadata
            && k.duration == st.duration
            && net_same
            && k.known.as_deref() == self.known_title()
    }

    fn track_key(&self) -> TrackKey {
        let st = &self.player.state;
        TrackKey {
            file_gen: self.file_gen,
            details: self.settings.system_media.details,
            en: crate::i18n::is_en(),
            has_video: st.has_video(),
            path: st.path.clone(),
            title: st.title.clone(),
            metadata: st.metadata.clone(),
            duration: st.duration,
            net: st.net.clone(),
            known: self.known_title().map(str::to_owned),
        }
    }

    /// 播放清單、紀錄裡這個網址的標題（m3u 的 #EXTINF 之類）
    fn known_title(&self) -> Option<&str> {
        let path = self.player.state.path.as_deref()?;
        self.titles.get(path).map(String::as_str)
    }

    /// 目前檔案的資訊。關掉「顯示片名與封面」時標題是程式名稱，沒有演出者、專輯、網址
    fn track_info(&self) -> TrackInfo {
        let st = &self.player.state;
        let kind = if st.has_video() {
            MediaKind::Video
        } else {
            MediaKind::Audio
        };
        let details = self.settings.system_media.details;
        let mut info = TrackInfo {
            file_gen: self.file_gen,
            kind,
            title: app_name().to_owned(),
            artist: None,
            album: None,
            album_artist: None,
            track_no: None,
            duration: st.duration,
            url: None,
            details,
        };
        if !details {
            return info;
        }
        let path = st.path.as_deref().unwrap_or_default();
        let net = st.net.as_deref();
        let tag = |name: &str| st.tag(name).map(str::to_owned);
        let audio = kind == MediaKind::Audio;
        // 網站影片：網站的標題；清單寫的標題；音訊檔：歌名；其他：mpv 的標題（影片的標題，沒有的話是檔名）。
        // 網路串流沒有標題時，mpv 的標題是網址的最後一段（含 `?token=…`）或整個網址（含帳號密碼）：
        // 只用串流自己的標題，沒有的話用跟播放清單一樣的名稱（最後一段，不含查詢參數）
        let mpv_title = if crate::net::is_network(path) {
            tag("service_name")
                .or_else(|| tag("title"))
                .or_else(|| tag("icy-title"))
        } else {
            st.title.clone().filter(|t| !t.trim().is_empty())
        };
        info.title = net
            .and_then(|n| n.title.clone())
            .or_else(|| self.known_title().map(str::to_owned))
            .or_else(|| if audio { tag("title") } else { None })
            .or(mpv_title)
            .unwrap_or_else(|| display_label(&self.titles, Path::new(path)));
        info.artist = tag("artist").or_else(|| net.and_then(|n| n.uploader.clone()));
        if audio {
            info.album = tag("album");
            info.album_artist = tag("album_artist");
            info.track_no = st.tag("track").and_then(media_keys::track_number);
        }
        // 網址裡有帳號密碼、token 的不給（跟最近開啟、續播一樣）
        info.url = match net {
            Some(n) => Some(n.page_url.clone()),
            None if path.contains("://") => Some(path.to_owned()),
            None => media_keys::file_url(Path::new(path)),
        }
        .filter(|u| !crate::net::is_network(u) || crate::net::storable(u));
        info
    }

    /// 目前的播放狀態（每一幀算，不問 mpv）
    fn playback_info(&self) -> PlaybackInfo {
        let st = &self.player.state;
        let status = match (st.loaded, st.paused) {
            (false, _) => Status::Stopped,
            (true, true) => Status::Paused,
            (true, false) => Status::Playing,
        };
        PlaybackInfo {
            status,
            position: if st.loaded { st.time_pos.max(0.0) } else { 0.0 },
            rate: st.speed,
            seekable: st.loaded && st.seekable && !self.live_now(),
            can_next: self.can_step(true),
            can_prev: self.can_step(false),
            shuffle: self.settings.play_order.shuffle,
            // `shown_repeat` 要問 mpv 的 loop-file：只在 VITASCOPE_MPV_OPTS 指定了它時才問（不要每一幀都問 mpv）
            repeat: if self.loop_file_overridden() {
                self.shown_repeat()
            } else {
                self.effective_repeat()
            },
            volume: self.player.volume_total() / 100.0,
            buffering: st.loaded && st.paused_for_cache,
        }
    }

    // ───────────── 封面 ─────────────

    /// 音訊檔的專輯封面：影像設定好了、系統的工作階段已經有這個檔案、顯示片名與封面時，請 mpv 存成 PNG
    fn cover_tick(&mut self) {
        let Some(dir) = self.media.covers_dir.clone() else {
            return;
        };
        let file_gen = self.file_gen;
        if self.media.cover.as_ref().is_none_or(|c| c.file_gen != file_gen) {
            // 換了檔案：上一個檔案的封面刪掉（MPRIS 只在那個檔案播放時用它）
            self.remove_covers();
            self.media.cover = Some(CoverJob {
                file_gen,
                state: CoverState::Idle,
                retried: false,
            });
        }
        let idle = match self.media.cover.as_ref().map(|c| &c.state) {
            Some(CoverState::Idle) => true,
            Some(CoverState::Reading(..)) => false,
            _ => return,
        };
        if idle {
            if !self.cover_wanted() {
                return;
            }
            let path = media_keys::cover_path(&dir, std::process::id(), file_gen);
            let asked = std::fs::create_dir_all(&dir).ok().and_then(|()| {
                let p = path.to_string_lossy();
                self.player
                    .command_async_keyed(AsyncKey::Cover, &["screenshot-to-file", &p, "video"])
                    .ok()
            });
            let state = match asked {
                Some(id) => {
                    self.media.covers_made = true;
                    CoverState::Asked { id, path }
                }
                None => CoverState::Done,
            };
            if let Some(job) = &mut self.media.cover {
                job.state = state;
            }
            return;
        }
        let Some(CoverJob {
            state: CoverState::Reading(rx, path),
            ..
        }) = &self.media.cover
        else {
            return;
        };
        let cover = match rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(Ok(png)) => Some(Cover {
                file_gen,
                png: Arc::new(png),
                path: path.clone(),
            }),
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => None,
        };
        if let Some(job) = &mut self.media.cover {
            job.state = CoverState::Done;
        }
        if let Some(cover) = cover
            && let Some(u) = self.media.sync.set_cover(cover)
            && let Some(session) = &mut self.media.session
        {
            session.apply(&u);
            self.media.applied += 1;
        }
    }

    /// 現在要不要請 mpv 存封面
    fn cover_wanted(&self) -> bool {
        let st = &self.player.state;
        self.media.session.is_some()
            && self.settings.system_media.details
            && st.loaded
            && !self.switching_file
            // 影像設定好了才有畫面可以存（載入完成時就要的話會失敗）
            && self.video_reconfigured
            && st.selected(TrackKind::Video).is_some_and(|t| t.albumart)
            // 網路串流不存封面（網站影片的縮圖之後再說）
            && !st.path.as_deref().is_some_and(crate::net::is_network)
            && self.media.sync.sent_track().is_some_and(|t| t.file_gen == self.file_gen)
    }

    /// mpv 回覆了存封面（`AsyncKey::Cover`）：成功就在背景讀進來；失敗等下一次開始播放時再試一次，之後就不要了（不提示）
    pub(super) fn cover_reply(&mut self, id: u64, error: Option<String>) {
        let current = matches!(
            &self.media.cover,
            Some(CoverJob { state: CoverState::Asked { id: asked, .. }, .. }) if *asked == id
        );
        if !current {
            self.stale_cover_reply();
            return;
        }
        let Some(job) = &mut self.media.cover else { return };
        let CoverState::Asked { path, .. } = &job.state else {
            return;
        };
        if let Some(e) = error {
            if std::env::var_os("VITASCOPE_DEBUG_MEDIA").is_some() {
                eprintln!("[media] 無法存封面：{e}");
            }
            job.state = if job.retried {
                CoverState::Done
            } else {
                CoverState::Retry
            };
            return;
        }
        let path = path.clone();
        let (tx, rx) = mpsc::channel();
        let (read_path, ctx) = (path.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(std::fs::read(&read_path));
            ctx.request_repaint();
        });
        job.state = CoverState::Reading(rx, path);
    }

    /// 換檔、關掉設定之前要的封面，mpv 現在才寫好（換檔、關掉時還刪不到）：刪掉，只留目前這個檔案的
    fn stale_cover_reply(&mut self) {
        let Some(dir) = &self.media.covers_dir else { return };
        let pid = std::process::id();
        let keep = self
            .media
            .cover
            .as_ref()
            .map(|c| media_keys::cover_path(dir, pid, c.file_gen));
        media_keys::remove_own_covers(dir, pid, keep.as_deref());
    }

    /// 開始播放（跳轉完成）：存封面失敗過的話再試一次
    pub(super) fn cover_retry_point(&mut self) {
        if let Some(job) = &mut self.media.cover
            && matches!(job.state, CoverState::Retry)
        {
            job.state = CoverState::Idle;
            job.retried = true;
        }
    }

    // ───────────── 設定頁 ─────────────

    /// 「設定 → 系統」的系統媒體控制（這個系統還不支援時不顯示）；回傳設定有沒有改
    pub(super) fn media_settings(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.media.backend.available() {
            return false;
        }
        ui.add_space(10.0);
        ui.strong(tr!("媒體鍵", "Media keys"));
        let mut changed = ui
            .checkbox(
                &mut self.settings.system_media.enabled,
                tr!("系統媒體控制", "System media controls"),
            )
            .on_hover_text(tr!(
                "讓鍵盤的媒體鍵、耳機的按鍵、系統的媒體控制可以播放、暫停、換上一個 / 下一個檔案",
                "Let media keys, headset buttons and the system's media controls play, pause and change files"
            ))
            .changed();
        let enabled = self.settings.system_media.enabled;
        ui.indent("system_media_details", |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                changed |= ui
                    .checkbox(
                        &mut self.settings.system_media.details,
                        tr!(
                            "在系統的媒體控制顯示片名與封面",
                            "Show the title and cover in the system's media controls"
                        ),
                    )
                    .on_hover_text(tr!(
                        "關掉時系統的媒體控制、鎖定畫面只顯示「影戲 VitaScope」，別人看不到在播什麼；媒體鍵照樣能用",
                        "When off, the system's media controls and lock screen show only \"VitaScope\", \
                         so others can't see what's playing; media keys still work"
                    ))
                    .changed();
            });
        });
        ui.weak(media_platform_note());
        if enabled && let Some(e) = &self.media.error {
            let color = Palette::of(ui.visuals()).problem;
            ui.colored_label(color, tf!("無法啟用：{e}", "Couldn't turn it on: {e}"));
        }
        changed
    }

    /// 快捷鍵頁的「固定的按鍵」要不要列出媒體鍵（這個系統支援時）
    pub(super) fn media_keys_listed(&self) -> bool {
        self.media.backend.available()
    }
}

/// 「設定 → 系統」說明各系統的媒體控制有哪些
fn media_platform_note() -> &'static str {
    if cfg!(windows) {
        tr!(
            "鍵盤的媒體鍵、耳機按鍵、Windows 的媒體控制（音量浮動視窗、快速設定、鎖定畫面）可以控制影戲，視窗不在最前面也可以",
            "Media keys, headset buttons and Windows' media controls (volume flyout, Quick Settings, lock screen) \
             control VitaScope, even when it isn't the front window"
        )
    } else if cfg!(target_os = "macos") {
        tr!(
            "鍵盤的媒體鍵（F7–F9）、AirPods 與耳機的按鍵、控制中心的「正在播放」可以控制影戲",
            "Media keys (F7–F9), AirPods and headset buttons, and Now Playing in Control Center control VitaScope"
        )
    } else {
        tr!(
            "透過 MPRIS 讓桌面環境（GNOME、KDE…）的媒體控制、媒體鍵與 playerctl 控制影戲",
            "Through MPRIS, your desktop's (GNOME, KDE…) media controls, media keys and playerctl control VitaScope"
        )
    }
}

/// 快捷鍵頁「固定的按鍵」的媒體鍵那一列（按鍵、做什麼）
pub(super) fn media_keys_row() -> (String, &'static str) {
    (
        tr!("媒體鍵", "Media keys").to_owned(),
        tr!(
            "播放／暫停、上一個／下一個檔案、停止（設定 → 系統）",
            "Play/pause, previous/next file, stop (Settings → System)"
        ),
    )
}
