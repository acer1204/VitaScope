//! 系統的媒體控制（媒體鍵、Windows 的媒體控制、macOS 的「正在播放」、Linux 的 MPRIS）。
//!
//! 這裡用假的系統（`support::fake_media`）：影戲送出的更新記下來，測試自己送指令（像使用者按了媒體鍵）。
//! 絕不碰真的系統媒體控制（`Launch` 的預設沒有）。各系統真的實作的測試是 `#[ignore]` 的（會短暫出現在
//! 系統的媒體控制裡），在開發機上手動跑：Windows `smtc_session_round_trip`。macOS 的在 `tests/now_playing.rs`
//! （「正在播放」只能在主執行緒用，那個測試程式不用 libtest；只在 CI 跑）。
//!
//! 樣本來自 `python scripts/gen_samples.py`。

mod support;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::fake_media::FakeMedia;
use support::fake_ytdl::{FakeResolver, resolved};
use support::http::Server;
use vitascope::app::{Launch, VitascopeApp};
use vitascope::media_keys::{self, MediaCommand, MediaKind, Status, Update};
use vitascope::player::{ASYNC_BASE, AsyncKey, Options, Player, PlayerEvent};
use vitascope::playlist::{Playlist, Repeat};
use vitascope::settings::Settings;
use vitascope::system_power::AfterPlayback;
use vitascope::ytdl::Resolve;

/// 慢的 CI 也要等得到（開檔、跳轉、換檔都是非同步的）
const TIMEOUT: Duration = Duration::from_secs(30);

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

/// 每個測試自己的暫存資料夾（測試會平行執行）；要宣告在 Harness 之前，才會在播放器關掉檔案之後才刪
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vitascope-media-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 放一份樣本，取名為 `name`
    fn copy(&self, rel: &str, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::copy(sample(rel), &path).unwrap();
        path
    }

    /// 放一份 3 秒的影片
    fn clip(&self, name: &str) -> PathBuf {
        self.copy("common/mp4_h264_aac.mp4", name)
    }

    /// 測試自己的設定檔：不自動接下一個、不續播（除非測試自己改）；匯出、截圖的資料夾也在這裡
    /// （有快取資料夾時，啟動會清匯出資料夾裡留下的暫存檔，不能碰到使用者的資料夾）
    fn settings(&self) -> Settings {
        let mut s = Settings::load_from(self.0.join("settings.json"));
        s.auto_next = false;
        s.resume = false;
        s.export.clip_dir = Some(self.0.join("clips"));
        s.export.image_dir = Some(self.0.join("images"));
        s.screenshot_dir = Some(self.0.join("shots"));
        s
    }

    fn cache(&self) -> PathBuf {
        self.0.join("cache")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn harness(launch: Launch, settings: Settings) -> Harness<'static, VitascopeApp> {
    let player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch))
}

/// 開 `files`，系統的媒體控制用 `fake`
fn with_media(fake: &FakeMedia, files: Vec<PathBuf>, settings: Settings) -> Harness<'static, VitascopeApp> {
    harness(
        Launch {
            files,
            media: fake.backend(),
            ..Default::default()
        },
        settings,
    )
}

fn step_until_app(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn(&VitascopeApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}\n狀態：{:#?}",
            h.state().player().state
        );
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 一直更新介面直到條件成立（條件看假的系統）
fn step_until(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 實際經過一段時間（播放器在背景播放），期間一直更新介面
fn wait_real(h: &mut Harness<'_, VitascopeApp>, seconds: f64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f64(seconds) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// mpv 正在播 `name`（載入完、還沒播到結尾）
fn playing(app: &VitascopeApp, name: &str) -> bool {
    let s = &app.player().state;
    s.loaded && !s.eof && s.duration.is_some() && s.path.as_deref().is_some_and(|p| p.ends_with(name))
}

/// mpv 現在是不是暫停（直接問 mpv）
fn mpv_paused(h: &Harness<'_, VitascopeApp>) -> bool {
    h.state().player().get_string("pause").unwrap() == "yes"
}

/// 像使用者按了媒體鍵：送出、處理完這一幀
fn send(h: &mut Harness<'_, VitascopeApp>, fake: &FakeMedia, cmd: MediaCommand) {
    assert!(fake.send(cmd.clone()), "沒有工作階段可以送 {cmd:?}");
    h.step();
}

fn last_status(fake: &FakeMedia) -> Option<Status> {
    fake.last_playback().map(|p| p.status)
}

/// 這一幀送給視窗的指令
fn viewport_commands(h: &Harness<'_, VitascopeApp>) -> Vec<egui::ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// 打開設定視窗的 `page` 頁
fn open_settings_page(h: &mut Harness<'_, VitascopeApp>, page: &str) {
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label(page).click();
    h.run_steps(2);
}

fn click(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(2);
}

// ───────────── 建立工作階段、送出的內容 ─────────────

/// 沒有在播時不建立（系統的媒體控制不會多出影戲）；第一個檔案載入時建立，送出檔案與播放狀態
#[test]
fn the_session_starts_with_the_first_file_and_gets_track_and_status() {
    let dir = TempDir::new("track");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, Vec::new(), dir.settings());
    wait_real(&mut h, 0.3);
    assert_eq!(fake.attempts(), 0, "起始畫面：不建立");
    assert!(!h.state().media_session_active());
    h.input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(a.clone())));
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    assert_eq!(fake.created(), 1);
    assert!(h.state().media_session_active());
    let ups = fake.updates();
    let Update::Track(track) = &ups[0] else {
        panic!("第一個是檔案資訊：{ups:?}");
    };
    assert!(
        matches!(&ups[1], Update::Playback { seeked: true, .. }),
        "接著是播放狀態（重新對時間）：{ups:?}"
    );
    assert_eq!(track.kind, MediaKind::Video);
    assert_eq!(
        Some(&track.title),
        h.state().player().state.title.as_ref(),
        "影片：mpv 的標題"
    );
    assert!(track.details && track.artist.is_none() && track.album.is_none());
    let d = track.duration.unwrap();
    assert!((d - 3.0).abs() < 0.2, "{d}");
    let abs = std::path::absolute(&a).unwrap();
    assert_eq!(track.url, media_keys::file_url(&abs));
    let p = fake.last_playback().unwrap();
    assert!(p.seekable && !p.shuffle && p.repeat == Repeat::Off && !p.can_prev);
    assert!((p.volume - 1.0).abs() < 1e-9 && (p.rate - 1.0).abs() < 1e-9);
    // 按空白鍵暫停：系統跟著變
    h.key_press(egui::Key::Space);
    step_until(&mut h, "系統收到「暫停」", || {
        last_status(&fake) == Some(Status::Paused)
    });
    // 同一個檔案、標籤沒變：只送過一次檔案資訊
    let tracks = fake.updates().iter().filter(|u| matches!(u, Update::Track(_))).count();
    assert_eq!(tracks, 1);
}

/// 平穩播放時不會一直送（變了才送；播放中最多每 5 秒更新一次進度）；影片不存封面
#[test]
fn media_updates_are_not_spammed() {
    let dir = TempDir::new("spam");
    let long = dir.copy("common/mp4_long.mp4", "long.mp4");
    let fake = FakeMedia::new();
    let mut h = harness(
        Launch {
            files: vec![long],
            media: fake.backend(),
            cache_root: Some(dir.cache()),
            ..Default::default()
        },
        dir.settings(),
    );
    step_until_app(&mut h, "開始播放", |app| playing(app, "long.mp4"));
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    // 開檔時的標籤、軌道、影像設定都到齊
    wait_real(&mut h, 1.0);
    fake.take_updates();
    let before = h.state().media_updates_applied();
    let builds = h.state().media_track_builds();
    wait_real(&mut h, 2.5);
    let ups = fake.take_updates();
    assert!(ups.len() <= 2, "平穩播放 2.5 秒送了 {} 個更新：{ups:?}", ups.len());
    assert_eq!(h.state().media_updates_applied() - before, ups.len() as u64);
    assert_eq!(
        h.state().media_track_builds(),
        builds,
        "標籤沒變：檔案資訊不用每一幀重新整理"
    );
    assert!(fake.covers().is_empty(), "影片沒有封面");
    assert!(
        std::fs::read_dir(dir.cache().join("covers")).map_or(true, |d| d.count() == 0),
        "影片不存封面"
    );
}

/// 音訊檔：歌名、演出者、專輯；專輯封面存成 PNG 送給系統。關掉系統媒體控制時結束工作階段、刪掉封面，
/// 再打開時重新建立、重新送
#[test]
fn audio_cover_reaches_the_session_and_toggling_restarts_it() {
    let dir = TempDir::new("cover");
    let song = dir.copy("general/audio_mp3_cover.mp3", "song.mp3");
    let fake = FakeMedia::new();
    let mut h = harness(
        Launch {
            files: vec![song],
            media: fake.backend(),
            cache_root: Some(dir.cache()),
            ..Default::default()
        },
        dir.settings(),
    );
    step_until(&mut h, "封面送到系統", || !fake.covers().is_empty());
    let track = fake.last_track().unwrap();
    assert_eq!(track.kind, MediaKind::Audio);
    assert_eq!(track.title, "測試歌曲");
    assert_eq!(track.artist.as_deref(), Some("影戲樂團"));
    assert_eq!(track.album.as_deref(), Some("範例專輯"));
    let cover = fake.covers().remove(0);
    assert_eq!(cover.file_gen, track.file_gen, "是這個檔案的封面");
    let ups = fake.updates();
    let track_at = ups.iter().position(|u| matches!(u, Update::Track(_))).unwrap();
    let cover_at = ups.iter().position(|u| matches!(u, Update::Cover(_))).unwrap();
    assert!(track_at < cover_at, "先送檔案資訊才送封面");
    let png = png::Decoder::new(std::io::Cursor::new(cover.png.as_slice()))
        .read_info()
        .unwrap();
    assert_eq!((png.info().width, png.info().height), (300, 300));
    let expected = media_keys::cover_path(&dir.cache().join("covers"), std::process::id(), track.file_gen);
    assert_eq!(cover.path, expected);
    assert!(cover.path.exists(), "MPRIS 用的檔案在播放期間要在");
    // 關掉：結束工作階段，刪掉這次存的封面
    open_settings_page(&mut h, "系統");
    click(&mut h, "系統媒體控制");
    assert!(!h.state().settings().system_media.enabled);
    assert_eq!(fake.shutdowns(), 1);
    assert!(!h.state().media_session_active());
    assert!(!cover.path.exists(), "封面刪掉了");
    assert!(!fake.send(MediaCommand::Pause), "結束了就收不到指令");
    assert!(
        !Settings::load_from(dir.0.join("settings.json")).system_media.enabled,
        "存檔"
    );
    // 再打開：重新建立，檔案資訊、封面重新送
    let sent = fake.updates().len();
    click(&mut h, "系統媒體控制");
    step_until(&mut h, "重新建立、封面又送到系統", || {
        fake.created() == 2 && fake.covers().len() == 2
    });
    assert!(
        matches!(fake.updates()[sent], Update::Track(_)),
        "{:?}",
        &fake.updates()[sent..]
    );
    assert!(cover.path.exists());
}

/// mpv 存不了封面：下一次開始播放（跳轉完成）時再試一次；還是不行就不要封面（不提示，不再試）
#[test]
fn a_failed_cover_is_retried_once_at_the_next_playback_restart() {
    for second_try_works in [true, false] {
        let dir = TempDir::new(&format!("cover-retry-{second_try_works}"));
        let song = dir.copy("general/audio_mp3_cover.mp3", "song.mp3");
        // 封面的檔名被資料夾佔住：mpv 寫不進去（第一個開的檔案是第 1 個）
        let blocker = media_keys::cover_path(&dir.cache().join("covers"), std::process::id(), 1);
        std::fs::create_dir_all(&blocker).unwrap();
        let fake = FakeMedia::new();
        // 先不顯示片名與封面，播了一段（開檔時的「開始播放」已經處理完）才打開：
        // 不然第一次失敗的回覆可能比開檔時的「開始播放」早到，同一幀就重試了，測不到「等下一次」
        let mut settings = dir.settings();
        settings.system_media.details = false;
        let mut h = harness(
            Launch {
                files: vec![song],
                media: fake.backend(),
                cache_root: Some(dir.cache()),
                ..Default::default()
            },
            settings,
        );
        step_until_app(&mut h, "播了一段", |app| app.player().state.time_pos > 1.0);
        assert_eq!(h.state().media_cover_state(), "idle");
        open_settings_page(&mut h, "系統");
        click(&mut h, "在系統的媒體控制顯示片名與封面");
        step_until_app(&mut h, "第一次存封面失敗", |app| {
            app.media_cover_state() == "retry"
        });
        assert_eq!(fake.last_track().unwrap().file_gen, 1);
        assert_eq!(h.state().osd_text(), None, "失敗不提示");
        if second_try_works {
            std::fs::remove_dir(&blocker).unwrap();
        }
        // 跳轉：完成時（開始播放）再試一次
        h.state().player().seek_to(0.5, true).unwrap();
        if second_try_works {
            step_until(&mut h, "再試一次就送到系統", || !fake.covers().is_empty());
        } else {
            step_until_app(&mut h, "再試一次也失敗：不要封面", |app| {
                app.media_cover_state() == "done"
            });
            h.state().player().seek_to(0.2, true).unwrap();
            wait_real(&mut h, 0.5);
            assert_eq!(h.state().media_cover_state(), "done", "不再試");
            assert!(fake.covers().is_empty());
        }
    }
}

/// 關掉「顯示片名與封面」：系統只看到程式名稱，沒有演出者、專輯、網址、封面（也不存封面）；打開後馬上補上
#[test]
fn details_off_hides_the_title_and_cover() {
    let dir = TempDir::new("details");
    let song = dir.copy("general/audio_mp3_cover.mp3", "song.mp3");
    let mut settings = dir.settings();
    settings.system_media.details = false;
    let fake = FakeMedia::new();
    let mut h = harness(
        Launch {
            files: vec![song],
            media: fake.backend(),
            cache_root: Some(dir.cache()),
            ..Default::default()
        },
        settings,
    );
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    // 封面要等影像設定好：多等一下，確定真的沒有
    step_until_app(&mut h, "播了一段", |app| app.player().state.time_pos > 1.0);
    let track = fake.last_track().unwrap();
    assert_eq!(track.title, "影戲 VitaScope");
    assert!(!track.details);
    assert_eq!((track.artist, track.album, track.url), (None, None, None));
    assert!(fake.covers().is_empty());
    assert!(std::fs::read_dir(dir.cache().join("covers")).map_or(true, |d| d.count() == 0));
    // 打開：歌名、封面
    open_settings_page(&mut h, "系統");
    click(&mut h, "在系統的媒體控制顯示片名與封面");
    step_until(&mut h, "封面送到系統", || !fake.covers().is_empty());
    assert_eq!(fake.last_track().unwrap().title, "測試歌曲");
    assert_eq!(fake.created(), 1, "不用重新建立");
}

/// 封面只留目前這個檔案的：換檔時刪掉上一個的；換檔、關掉之前要的封面 mpv 之後才寫好的也刪；
/// 結束時結束工作階段、刪掉這次存的全部（換到沒有封面的檔案之後也一樣）
#[test]
fn covers_are_deleted_on_file_change_late_writes_and_exit() {
    let dir = TempDir::new("cover-cleanup");
    // 同資料夾只掃同一類的檔案：三個一起給（照給的順序）
    let files = vec![
        dir.copy("general/audio_mp3_cover.mp3", "a.mp3"),
        dir.copy("general/audio_mp3_cover.mp3", "b.mp3"),
        dir.clip("c.mp4"),
    ];
    let covers = dir.cache().join("covers");
    let pid = std::process::id();
    let fake = FakeMedia::new();
    let mut h = harness(
        Launch {
            files,
            media: fake.backend(),
            cache_root: Some(dir.cache()),
            ..Default::default()
        },
        dir.settings(),
    );
    step_until_app(&mut h, "a.mp3 開始播放", |app| {
        playing(app, "a.mp3") && app.playlist().is_some_and(|l| l.len() == 3)
    });
    step_until(&mut h, "a.mp3 的封面", || !fake.covers().is_empty());
    let first = fake.covers().remove(0);
    assert!(first.path.exists());
    // mpv 之後才寫好、回覆的（那時已經換檔或關掉了）：刪掉，目前的留著
    let late = media_keys::cover_path(&covers, pid, 900);
    std::fs::write(&late, b"png").unwrap();
    let stale = ASYNC_BASE | (AsyncKey::Cover as u64) << 24 | 0xABCDE;
    assert_eq!(vitascope::player::async_key(stale), Some(AsyncKey::Cover));
    h.state_mut()
        .deliver_player_event(PlayerEvent::CommandReply { id: stale, error: None });
    assert!(!late.exists(), "晚到的封面刪掉");
    assert!(first.path.exists(), "目前的封面留著");
    assert_eq!(h.state().osd_text(), None, "不提示");
    // 換到下一首（也有封面）：上一首的刪掉
    send(&mut h, &fake, MediaCommand::Next);
    step_until(&mut h, "b.mp3 的封面", || {
        fake.covers().last().is_some_and(|c| c.file_gen > first.file_gen)
    });
    let second = fake.covers().pop().unwrap();
    assert!(!first.path.exists(), "上一首的封面刪掉");
    assert!(second.path.exists());
    // 換到影片（沒有封面）：上一首的刪掉
    send(&mut h, &fake, MediaCommand::Next);
    step_until(&mut h, "系統收到影片", || {
        fake.last_track()
            .is_some_and(|t| t.kind == MediaKind::Video && t.file_gen > second.file_gen)
    });
    step_until(&mut h, "上一首的封面刪掉", || !second.path.exists());
    // mpv 之後才寫好、沒等到回覆就結束：結束時照樣刪
    let late = media_keys::cover_path(&covers, pid, 901);
    std::fs::write(&late, b"png").unwrap();
    eframe::App::on_exit(h.state_mut(), None);
    assert_eq!(fake.shutdowns(), 1, "結束時結束工作階段");
    assert!(!h.state().media_session_active());
    assert!(!late.exists(), "結束時刪掉這次存的封面");
    assert_eq!(std::fs::read_dir(&covers).unwrap().count(), 0);
}

/// 沒有快取資料夾（自動測試、`--shot`）：不存封面、也不清（S-B8：不能寫到、清到使用者真的快取）
#[test]
fn without_a_cache_folder_no_cover_is_made() {
    let dir = TempDir::new("no-cache");
    let song = dir.copy("general/audio_mp3_cover.mp3", "song.mp3");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![song], dir.settings());
    step_until(&mut h, "系統收到歌名", || {
        fake.last_track().is_some_and(|t| t.title == "測試歌曲")
    });
    // 封面要等影像設定好：多等一下，確定真的沒有
    step_until_app(&mut h, "播了一段", |app| app.player().state.time_pos > 1.0);
    h.run_steps(3);
    assert_eq!(h.state().media_cover_state(), "none", "不處理封面");
    assert!(fake.covers().is_empty());
}

/// 網路串流沒有標題：系統看到的是網址最後一段的檔名，不是 mpv 從網址來的標題（帶著 `?token=…`）
#[test]
fn a_stream_url_with_a_token_never_reaches_the_title() {
    let server = Server::start();
    let dir = TempDir::new("token-title");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, Vec::new(), dir.settings());
    h.step();
    let url = server.url("/f/common/mp4_h264_aac.mp4?token=SECRET123");
    h.event(egui::Event::Paste(url.clone()));
    step_until(&mut h, "系統收到網路串流", || fake.last_track().is_some());
    step_until_app(&mut h, "載入完成、有 mpv 的標題", |app| {
        app.player().state.loaded && app.player().state.title.is_some()
    });
    h.run_steps(3);
    let track = fake.last_track().unwrap();
    assert!(
        h.state()
            .player()
            .state
            .title
            .as_deref()
            .is_some_and(|t| t.contains("SECRET123")),
        "前提：mpv 的標題是從網址來的：{:?}",
        h.state().player().state.title
    );
    assert_eq!(track.title, "mp4_h264_aac.mp4");
    assert_eq!(track.url, None, "網址有權杖：不給系統");
    assert!(
        fake.updates().iter().all(|u| !format!("{u:?}").contains("SECRET123")),
        "{:?}",
        fake.updates()
    );
}

/// 網站影片：標題、上傳者來自 yt-dlp，網址是使用者開的網頁；網址裡有權杖之類的不給系統（跟最近開啟一樣）；不存封面
#[test]
fn site_video_title_and_page_come_from_yt_dlp() {
    let server = Server::start();
    let base = server.url("");
    let resolver = FakeResolver::new(move |req, _| {
        Ok(resolved(
            &format!(
                r#"{{"id": "long", "title": "很長的網站影片", "uploader": "上傳者", "extractor_key": "FakeSite",
                    "duration": 90, "webpage_url": "{}", "url": "{base}/f/common/mp4_long.mp4", "protocol": "http",
                    "vcodec": "avc1", "acodec": "mp4a", "format_id": "18"}}"#,
                req.url
            ),
            Vec::new(),
        ))
    })
    .arc();
    let resolver: Arc<dyn Resolve> = resolver;
    let dir = TempDir::new("site");
    let fake = FakeMedia::new();
    let player = Player::new(Options {
        keep_open: true,
        net_hooks: true,
        net_resolver: Some(resolver),
        net_sites: vec!["127.0.0.1".into()],
        ..Options::headless()
    })
    .unwrap();
    let settings = dir.settings();
    let launch = Launch {
        media: fake.backend(),
        cache_root: Some(dir.cache()),
        ..Default::default()
    };
    let mut h = Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch));
    h.step();
    let mut last_gen = 0;
    for (page, url_shown) in [
        (server.url("/watch?v=long"), true),
        (server.url("/watch?v=long&token=abc"), false),
    ] {
        h.event(egui::Event::Paste(page.clone()));
        step_until(&mut h, "系統收到網站影片", || {
            fake.last_track()
                .is_some_and(|t| t.file_gen > last_gen && t.title == "很長的網站影片")
        });
        let track = fake.last_track().unwrap();
        last_gen = track.file_gen;
        assert_eq!(track.artist.as_deref(), Some("上傳者"));
        assert_eq!(track.url, url_shown.then_some(page.clone()), "{page}");
        assert_eq!(track.kind, MediaKind::Video);
    }
    // 網路上的音訊檔（有專輯封面）：歌名照樣有，封面不存
    let song = server.url("/f/general/audio_mp3_cover.mp3");
    h.event(egui::Event::Paste(song.clone()));
    step_until(&mut h, "系統收到網路上的歌", || {
        fake.last_track()
            .is_some_and(|t| t.file_gen > last_gen && t.title == "測試歌曲")
    });
    assert_eq!(fake.last_track().unwrap().kind, MediaKind::Audio);
    step_until_app(&mut h, "播了一段（影像早就設定好了）", |app| {
        app.player().state.time_pos > 1.0
    });
    h.run_steps(5);
    assert!(fake.covers().is_empty());
    assert!(std::fs::read_dir(dir.cache().join("covers")).map_or(true, |d| d.count() == 0));
}

/// 建立失敗：記下原因（設定頁顯示），之後不再一直試；關掉再打開設定時再試
#[test]
fn a_failed_session_says_why_and_is_retried_after_toggling() {
    let dir = TempDir::new("fail");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    fake.fail_with(Some("測試用的錯誤"));
    let mut h = with_media(&fake, vec![a], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "試過建立", || fake.attempts() == 1);
    wait_real(&mut h, 0.5);
    assert_eq!(fake.attempts(), 1, "不會每一幀都再試");
    assert_eq!(h.state().media_error(), Some("測試用的錯誤"));
    open_settings_page(&mut h, "系統");
    h.get_by_label("無法啟用：測試用的錯誤");
    fake.fail_with(None);
    click(&mut h, "系統媒體控制");
    assert!(h.query_by_label("無法啟用：測試用的錯誤").is_none(), "關掉時不顯示");
    click(&mut h, "系統媒體控制");
    step_until(&mut h, "重新試、成功", || fake.created() == 1);
    assert_eq!(fake.attempts(), 2);
    h.run_steps(2);
    assert!(h.query_by_label("無法啟用：測試用的錯誤").is_none());
}

/// 「往前 / 往後跳」的秒數（macOS 的控制中心、Touch Bar 的按鈕）跟著設定的短跳轉：工作階段建立時送一次，
/// 改了設定再送一次，沒改不重送；關掉再打開時新的工作階段一建立就送
#[test]
fn the_skip_interval_follows_the_short_seek_setting() {
    let dir = TempDir::new("skip");
    let fake = FakeMedia::new();
    let mut settings = dir.settings();
    settings.seek_short = 7.0;
    let mut h = with_media(&fake, vec![dir.clip("a.mp4")], settings);
    step_until(&mut h, "建立工作階段", || fake.created() == 1);
    wait_real(&mut h, 0.3);
    assert_eq!(fake.skip_intervals(), [7.0], "建立時送一次，之後不重送");
    h.state_mut().settings_mut().seek_short = 12.0;
    wait_real(&mut h, 0.3);
    assert_eq!(fake.skip_intervals(), [7.0, 12.0]);
    h.state_mut().settings_mut().system_media.enabled = false;
    step_until(&mut h, "結束工作階段", || fake.shutdowns() == 1);
    h.state_mut().settings_mut().system_media.enabled = true;
    step_until(&mut h, "重新建立", || fake.created() == 2);
    wait_real(&mut h, 0.2);
    assert_eq!(fake.skip_intervals(), [7.0, 12.0, 12.0]);
}

/// 設定頁的選項、快捷鍵頁的「媒體鍵」只在能用時顯示（自動測試的預設、還不支援的系統：沒有）
#[test]
fn system_media_options_only_show_when_available() {
    for available in [false, true] {
        let fake = FakeMedia::new();
        let mut h = harness(
            Launch {
                media: if available { fake.backend() } else { Default::default() },
                ..Default::default()
            },
            Settings::default(),
        );
        h.step();
        open_settings_page(&mut h, "系統");
        assert_eq!(h.query_by_label("系統媒體控制").is_some(), available);
        assert_eq!(h.query_by_label("在系統的媒體控制顯示片名與封面").is_some(), available);
        h.get_by_label("只開一個視窗");
        click(&mut h, "快捷鍵");
        assert_eq!(
            h.query_by_label("播放／暫停、上一個／下一個檔案、停止（設定 → 系統）")
                .is_some(),
            available
        );
        assert_eq!(fake.attempts(), 0, "沒有在播：不建立");
    }
}

// ───────────── 系統的指令 ─────────────

/// 播放 / 暫停 / 播放／暫停、跳轉、上一個 / 下一個、隨機、重複、速度、音量都照系統的指令做
#[test]
fn media_commands_drive_the_player() {
    let dir = TempDir::new("commands");
    let files = [dir.clip("a.mp4"), dir.clip("b.mp4")];
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![files[0].clone()], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放、掃描完", |app| {
        playing(app, "a.mp4") && app.playlist().is_some_and(|l| l.len() == 2)
    });
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    step_until(&mut h, "系統知道有下一個", || {
        fake.last_playback().is_some_and(|p| p.can_next && !p.can_prev)
    });
    // 播放／暫停切換；播放、暫停是「變成那個狀態」
    send(&mut h, &fake, MediaCommand::PlayPause);
    assert!(mpv_paused(&h));
    assert_eq!(h.state().osd_text(), Some("⏸ 暫停"), "提示照常顯示");
    send(&mut h, &fake, MediaCommand::Pause);
    assert!(mpv_paused(&h), "已經暫停：暫停不會切換");
    send(&mut h, &fake, MediaCommand::Play);
    assert!(!mpv_paused(&h));
    send(&mut h, &fake, MediaCommand::Play);
    assert!(!mpv_paused(&h), "已經在播：播放不會切換");
    send(&mut h, &fake, MediaCommand::PlayPause);
    assert!(mpv_paused(&h));
    // 跳轉（暫停著，位置才不會跑）
    let file_gen = fake.last_track().unwrap().file_gen;
    send(
        &mut h,
        &fake,
        MediaCommand::SeekTo {
            secs: 1.5,
            track: Some(file_gen),
        },
    );
    step_until_app(&mut h, "跳到 1.5 秒", |app| {
        (app.player().state.time_pos - 1.5).abs() < 0.05
    });
    step_until(&mut h, "系統收到跳轉", || {
        fake.updates()
            .iter()
            .any(|u| matches!(u, Update::Playback { info, seeked: true } if (info.position - 1.5).abs() < 0.05))
    });
    // 別的檔案的跳轉（系統還以為是上一個檔案）：不理
    send(
        &mut h,
        &fake,
        MediaCommand::SeekTo {
            secs: 0.2,
            track: Some(file_gen + 100),
        },
    );
    wait_real(&mut h, 0.3);
    assert!((h.state().player().state.time_pos - 1.5).abs() < 0.05);
    send(&mut h, &fake, MediaCommand::SeekBy(-1.0));
    step_until_app(&mut h, "往回 1 秒", |app| {
        (app.player().state.time_pos - 0.5).abs() < 0.2
    });
    // 速度、音量
    send(&mut h, &fake, MediaCommand::SetRate(1.5));
    step_until_app(&mut h, "速度 1.5", |app| {
        (app.player().state.speed - 1.5).abs() < 1e-6
    });
    assert_eq!(h.state().osd_text(), Some("速度 1.5×"));
    // 靜音中改音量：跟按鍵、音量滑桿一樣取消靜音
    h.key_press(egui::Key::M);
    step_until_app(&mut h, "靜音", |app| app.player().state.muted);
    send(&mut h, &fake, MediaCommand::SetVolume(0.4));
    step_until_app(&mut h, "音量 40、取消靜音", |app| {
        (app.player().state.volume - 40.0).abs() < 1e-6 && !app.player().state.muted
    });
    step_until(&mut h, "系統收到音量、速度", || {
        fake.last_playback()
            .is_some_and(|p| (p.volume - 0.4).abs() < 1e-6 && (p.rate - 1.5).abs() < 1e-6)
    });
    // 隨機、重複：存檔；重複全部一併打開「播完自動播放下一個」
    send(&mut h, &fake, MediaCommand::SetShuffle(true));
    assert!(h.state().play_order().shuffle);
    send(&mut h, &fake, MediaCommand::SetRepeat(Repeat::All));
    assert_eq!(h.state().play_order().repeat, Repeat::All);
    assert!(h.state().settings().auto_next);
    let saved = Settings::load_from(dir.0.join("settings.json"));
    assert!(saved.play_order.shuffle && saved.play_order.repeat == Repeat::All && saved.auto_next);
    step_until(&mut h, "系統收到隨機、重複", || {
        fake.last_playback()
            .is_some_and(|p| p.shuffle && p.repeat == Repeat::All)
    });
    // 下一個、上一個（跟 PgDn / PgUp 一樣）
    fake.take_updates();
    send(&mut h, &fake, MediaCommand::Next);
    step_until_app(&mut h, "換到 b.mp4", |app| playing(app, "b.mp4"));
    step_until(&mut h, "系統收到新的檔案", || {
        fake.last_track().is_some_and(|t| t.file_gen != file_gen)
    });
    assert!(
        !fake.updates().contains(&Update::Clear),
        "換檔途中不送「停止」（系統的顯示不會閃一下）：{:?}",
        fake.updates()
    );
    // 指令不叫出控制列（不算使用者在操作視窗，不動 last_activity）、不把視窗叫到前面
    let activity = h.state().last_activity();
    send(&mut h, &fake, MediaCommand::Previous);
    assert!(
        !viewport_commands(&h).contains(&egui::ViewportCommand::Focus),
        "{:?}",
        viewport_commands(&h)
    );
    step_until_app(&mut h, "回到 a.mp4", |app| playing(app, "a.mp4"));
    assert_eq!(h.state().last_activity(), activity, "不算使用者的動作");
    send(&mut h, &fake, MediaCommand::Stop);
    step_until_app(&mut h, "停止", |app| !app.player().state.loaded);
    step_until(&mut h, "系統收到「停止」", || {
        fake.updates().last() == Some(&Update::Clear)
    });
}

/// 停止之後按播放：開停止的那個檔案（不是清單上的下一個，S-B10）；播放／暫停也一樣；暫停不做事
#[test]
fn play_after_stop_opens_the_stopped_file() {
    let dir = TempDir::new("play-stopped");
    let files = [dir.clip("a.mp4"), dir.clip("b.mp4"), dir.clip("c.mp4")];
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![files[1].clone()], dir.settings());
    step_until_app(&mut h, "b.mp4 開始播放、掃描完", |app| {
        playing(app, "b.mp4") && app.playlist().is_some_and(|l| l.len() == 3)
    });
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    for cmd in [MediaCommand::Play, MediaCommand::PlayPause] {
        h.key_press(egui::Key::Space);
        h.step();
        send(&mut h, &fake, MediaCommand::Stop);
        step_until_app(&mut h, "停止", |app| !app.player().state.loaded);
        assert_eq!(
            h.state()
                .playlist()
                .and_then(Playlist::current)
                .map(|p| p.ends_with("b.mp4")),
            Some(true),
            "停止不會改清單"
        );
        send(&mut h, &fake, MediaCommand::Pause);
        wait_real(&mut h, 0.3);
        assert!(!h.state().player().state.loaded, "暫停不會開檔");
        send(&mut h, &fake, cmd.clone());
        step_until_app(&mut h, "又開始播 b.mp4", |app| playing(app, "b.mp4"));
        assert!(!mpv_paused(&h), "{cmd:?}");
    }
    assert_eq!(fake.created(), 1, "停止時工作階段還在（按播放鍵才能再開）");
}

/// 停止的那個檔案已經移出清單（跟還原的清單一樣：還沒有「目前的」）：播放開接下來要播的那一個
#[test]
fn play_after_the_stopped_file_was_removed_opens_the_next_one() {
    let dir = TempDir::new("play-removed");
    let files = [dir.clip("a.mp4"), dir.clip("b.mp4"), dir.clip("c.mp4")];
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![files[1].clone()], dir.settings());
    step_until_app(&mut h, "b.mp4 開始播放、掃描完", |app| {
        playing(app, "b.mp4") && app.playlist().is_some_and(|l| l.len() == 3)
    });
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    send(&mut h, &fake, MediaCommand::Stop);
    step_until_app(&mut h, "停止", |app| !app.player().state.loaded);
    // 播放清單上刪掉 b
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    click(&mut h, "2. b.mp4");
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    let list = h.state().playlist().unwrap();
    assert_eq!((list.len(), list.current()), (2, None), "b 移出清單了");
    send(&mut h, &fake, MediaCommand::Play);
    step_until_app(&mut h, "播放接下來的 c.mp4", |app| playing(app, "c.mp4"));
}

/// 系統的「可以播放」（MPRIS 的 CanPlay）：有檔案載入、或停止之後按播放會開清單上的檔案時才是；
/// 清單上什麼都沒有了：不能播（按播放也不會開檔）
#[test]
fn can_play_follows_what_play_would_open() {
    let dir = TempDir::new("can-play");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![a], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放、掃描完", |app| {
        playing(app, "a.mp4") && app.playlist().is_some_and(|l| l.len() == 1)
    });
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    assert!(fake.last_playback().unwrap().can_play);
    send(&mut h, &fake, MediaCommand::Stop);
    step_until_app(&mut h, "停止", |app| !app.player().state.loaded);
    step_until(&mut h, "系統收到停止", || {
        fake.updates().last() == Some(&Update::Clear)
    });
    // 停止之後清單上還有 a.mp4：還是可以播（停止後跟停止前一樣，Clear 之外不另外送播放狀態）
    assert!(fake.last_playback().unwrap().can_play);
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    click(&mut h, "1. a.mp4");
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(h.state().playlist().is_none_or(Playlist::is_empty), "a 移出清單了");
    step_until(&mut h, "系統收到「不能播放」", || {
        fake.last_playback().is_some_and(|p| !p.can_play)
    });
    send(&mut h, &fake, MediaCommand::Play);
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.loaded && !h.state().player().state.loading);
}

/// 正在播的檔案移出清單（照樣播完）之後停止：按播放沒有東西可開，系統收到「不能播放」
/// （跟「停止」一起送；不然系統一直顯示停止前的「可以播放」）
#[test]
fn can_play_is_off_after_stopping_a_file_no_longer_on_the_list() {
    let dir = TempDir::new("can-play-removed");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![a], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放、掃描完", |app| {
        playing(app, "a.mp4") && app.playlist().is_some_and(|l| l.len() == 1)
    });
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    click(&mut h, "1. a.mp4");
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(h.state().playlist().is_none_or(Playlist::is_empty), "a 移出清單了");
    assert!(h.state().player().state.loaded, "正在播的照樣播");
    assert!(fake.last_playback().unwrap().can_play, "還在播：可以播放");
    send(&mut h, &fake, MediaCommand::Stop);
    step_until_app(&mut h, "停止", |app| !app.player().state.loaded);
    step_until(&mut h, "系統收到停止", || {
        fake.updates().last() == Some(&Update::Clear)
    });
    let p = fake.last_playback().unwrap();
    assert!(p.status == Status::Stopped && !p.can_play, "{p:?}");
    send(&mut h, &fake, MediaCommand::Play);
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.loaded && !h.state().player().state.loading);
}

/// 「下一個」接著馬上「停止」（同一批處理：新檔案可能還沒開始就被停掉，不會再有 StartFile）：
/// 系統照樣收到「停止」，之後按播放照常開檔
#[test]
fn next_then_stop_at_once_still_stops() {
    let dir = TempDir::new("next-stop");
    let files = [dir.clip("a.mp4"), dir.clip("b.mp4")];
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![files[0].clone()], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放、掃描完", |app| {
        playing(app, "a.mp4") && app.playlist().is_some_and(|l| l.len() == 2)
    });
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    assert!(fake.send(MediaCommand::Next));
    assert!(fake.send(MediaCommand::Stop));
    h.step();
    step_until_app(&mut h, "停止", |app| {
        !app.player().state.loaded && !app.player().loading_now()
    });
    step_until(&mut h, "系統收到「停止」", || {
        fake.updates().last() == Some(&Update::Clear)
    });
    send(&mut h, &fake, MediaCommand::Play);
    step_until_app(&mut h, "又開始播", |app| {
        app.player().state.loaded && !app.player().state.eof
    });
}

/// 系統的指令是從別的執行緒送來的：叫醒介面（視窗閒著、在背景、縮到最小時才會馬上處理），下一幀就處理
#[test]
fn a_media_command_wakes_the_window() {
    let dir = TempDir::new("wake");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![a], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    // 上一幀之後是誰要求重畫的（egui 記下呼叫的位置）：叫醒介面的是 src/app/media.rs 給系統的那個
    let woken = |h: &Harness<'_, VitascopeApp>| {
        h.ctx
            .repaint_causes()
            .iter()
            .any(|c| c.file.replace('\\', "/").ends_with("src/app/media.rs"))
    };
    h.run_steps(2);
    assert!(!woken(&h), "{:?}", h.ctx.repaint_causes());
    assert!(fake.send(MediaCommand::Pause));
    h.step();
    assert!(woken(&h), "收到指令時叫醒介面：{:?}", h.ctx.repaint_causes());
    assert!(mpv_paused(&h), "下一幀就處理");
}

/// 播完後的倒數中，任何媒體鍵都是取消（跟按鍵一樣）：照常做的、什麼都不用做的都一樣
#[test]
fn a_media_command_cancels_the_countdown() {
    let dir = TempDir::new("countdown");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = harness(
        Launch {
            files: vec![a],
            media: fake.backend(),
            countdown_override: Some(Duration::from_secs(30)),
            ..Default::default()
        },
        dir.settings(),
    );
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    let srt = dir.0.join("a.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:02,000\n字幕\n").unwrap();
    let srt_uri = media_keys::file_url(&std::path::absolute(&srt).unwrap()).unwrap();
    // 播放中：「播放」不用做什麼（走「只取消」那條路）；「暫停」是按鍵那條路；音量是直接的操作；
    // 開字幕（不會開檔，加到正在播的影片）
    for (cmd, pauses) in [
        (MediaCommand::Play, false),
        (MediaCommand::SetVolume(0.5), false),
        (MediaCommand::OpenUri(srt_uri), false),
        (MediaCommand::Pause, true),
    ] {
        if mpv_paused(&h) {
            h.key_press(egui::Key::Space);
            h.step();
        }
        h.state_mut().set_after_playback(AfterPlayback::Close);
        h.state_mut().finish_playback_now();
        h.run_steps(2);
        assert!(h.state().countdown_remaining().is_some(), "{cmd:?}：開始倒數");
        send(&mut h, &fake, cmd.clone());
        let app = h.state();
        assert_eq!(app.countdown_remaining(), None, "{cmd:?}：倒數取消");
        assert_eq!(app.after_playback(), AfterPlayback::Nothing, "{cmd:?}");
        assert!(!app.close_requested(), "{cmd:?}");
        assert_eq!(mpv_paused(&h), pauses, "{cmd:?}：取消之外照常做");
    }
    assert_eq!(
        h.state().osd_text(),
        Some("⏸ 暫停"),
        "取消的提示之後是暫停的提示（跟按空白鍵一樣）"
    );
}

/// 帶數值的指令（跳轉、速度、隨機、重複…）、上一個／下一個、把視窗叫到前面也都取消倒數（不只播放、暫停、音量）
#[test]
fn every_kind_of_media_command_cancels_the_countdown() {
    let dir = TempDir::new("countdown-all");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = harness(
        Launch {
            files: vec![a],
            media: fake.backend(),
            countdown_override: Some(Duration::from_secs(30)),
            ..Default::default()
        },
        dir.settings(),
    );
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    // 重複放最後（之後的倒數跟它無關，只是保險）
    for cmd in [
        MediaCommand::SeekBy(-0.5),
        MediaCommand::SeekTo { secs: 0.5, track: None },
        MediaCommand::SetRate(1.25),
        MediaCommand::SetShuffle(true),
        MediaCommand::Next,
        MediaCommand::Previous,
        MediaCommand::Raise,
        MediaCommand::SetRepeat(Repeat::One),
    ] {
        h.state_mut().set_after_playback(AfterPlayback::Close);
        h.state_mut().finish_playback_now();
        h.run_steps(2);
        assert!(h.state().countdown_remaining().is_some(), "{cmd:?}：開始倒數");
        send(&mut h, &fake, cmd.clone());
        let app = h.state();
        assert_eq!(app.countdown_remaining(), None, "{cmd:?}：倒數取消");
        assert_eq!(app.after_playback(), AfterPlayback::Nothing, "{cmd:?}");
        assert!(!app.close_requested(), "{cmd:?}");
    }
}

/// 系統送來的音量不超過「音量上限」（提示也是上限，不是系統要的值）；負的當成 0
#[test]
fn media_volume_is_capped_by_the_volume_limit() {
    let dir = TempDir::new("volume-cap");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut settings = dir.settings();
    settings.audio.volume_max = 150;
    let mut h = with_media(&fake, vec![a], settings);
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    send(&mut h, &fake, MediaCommand::SetVolume(5.0));
    assert_eq!(h.state().osd_text(), Some("音量 150%（放大）"));
    step_until_app(&mut h, "總音量 150", |app| {
        (app.player().volume_total() - 150.0).abs() < 1e-6
    });
    step_until(&mut h, "系統收到音量 1.5", || {
        fake.last_playback().is_some_and(|p| (p.volume - 1.5).abs() < 1e-6)
    });
    send(&mut h, &fake, MediaCommand::SetVolume(-1.0));
    assert_eq!(h.state().osd_text(), Some("音量 0%"));
    step_until_app(&mut h, "總音量 0", |app| app.player().volume_total().abs() < 1e-6);
}

/// 把視窗叫到前面、結束影戲、開啟系統送來的網址（`file://`；不支援的不開）
#[test]
fn raise_quit_and_open_uri() {
    let dir = TempDir::new("raise");
    let files = [dir.clip("a.mp4"), dir.clip("b b#1.mp4")];
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![files[0].clone()], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    fake.send(MediaCommand::Raise);
    h.step();
    assert!(
        viewport_commands(&h).contains(&egui::ViewportCommand::Focus),
        "{:?}",
        viewport_commands(&h)
    );
    send(&mut h, &fake, MediaCommand::OpenUri("ftp://example.com/x.mp4".into()));
    wait_real(&mut h, 0.3);
    assert!(playing(h.state(), "a.mp4"), "不支援的網址不開");
    let uri = media_keys::file_url(&std::path::absolute(&files[1]).unwrap()).unwrap();
    assert!(uri.contains("%20") && uri.contains("%23"), "{uri}");
    send(&mut h, &fake, MediaCommand::OpenUri(uri));
    step_until_app(&mut h, "開了 b b#1.mp4", |app| playing(app, "b b#1.mp4"));
    fake.send(MediaCommand::Quit);
    h.step();
    assert!(viewport_commands(&h).contains(&egui::ViewportCommand::Close));
}

/// 迷你播放器開著時，系統的媒體控制要叫視窗到前面（MPRIS 的 Raise 之類）：只叫到前面，不離開迷你播放器（S-B9）
#[test]
fn raise_keeps_the_mini_player() {
    let dir = TempDir::new("raise-mini");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![dir.clip("a.mp4")], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    step_until_app(&mut h, "視窗配合影片調整完", |app| !app.window_fit_pending());
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        });
    }
    h.step();
    assert_eq!(h.state().window_mode(), vitascope::window_mode::WindowMode::Mini);
    step_until_app(&mut h, "迷你播放器換好", |app| !app.window_mode_pending());
    // 縮到最小時也叫得回來（先還原，再叫到前面）
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .minimized = Some(true);
    fake.send(MediaCommand::Raise);
    h.step();
    let cmds = viewport_commands(&h);
    assert!(cmds.contains(&egui::ViewportCommand::Focus), "{cmds:?}");
    assert!(cmds.contains(&egui::ViewportCommand::Minimized(false)), "{cmds:?}");
    assert!(
        !cmds.iter().any(|c| matches!(
            c,
            egui::ViewportCommand::InnerSize(_)
                | egui::ViewportCommand::MinInnerSize(_)
                | egui::ViewportCommand::Fullscreen(_)
                | egui::ViewportCommand::WindowLevel(_)
        )),
        "大小、層級都不動：{cmds:?}"
    );
    assert_eq!(h.state().window_mode(), vitascope::window_mode::WindowMode::Mini);
    h.run_steps(3);
    assert_eq!(h.state().window_mode(), vitascope::window_mode::WindowMode::Mini);
}

/// 子母畫面開著時，系統的媒體控制叫視窗到前面：只叫到前面，不離開子母畫面、標題列不加回來（S-B9）
#[test]
fn raise_keeps_pip() {
    let dir = TempDir::new("raise-pip");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![dir.clip("a.mp4")], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "工作階段建立了", || fake.created() == 1);
    step_until_app(&mut h, "視窗配合影片調整完", |app| !app.window_fit_pending());
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::P,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers {
                shift: true,
                ..egui::Modifiers::COMMAND
            },
        });
    }
    h.step();
    assert_eq!(h.state().window_mode(), vitascope::window_mode::WindowMode::Pip);
    step_until_app(&mut h, "子母畫面換好", |app| !app.window_mode_pending());
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .minimized = Some(true);
    fake.send(MediaCommand::Raise);
    h.step();
    let cmds = viewport_commands(&h);
    assert!(cmds.contains(&egui::ViewportCommand::Focus), "{cmds:?}");
    assert!(cmds.contains(&egui::ViewportCommand::Minimized(false)), "{cmds:?}");
    assert!(
        !cmds.iter().any(|c| matches!(
            c,
            egui::ViewportCommand::InnerSize(_)
                | egui::ViewportCommand::MinInnerSize(_)
                | egui::ViewportCommand::Fullscreen(_)
                | egui::ViewportCommand::WindowLevel(_)
                | egui::ViewportCommand::Decorations(_)
        )),
        "大小、層級、標題列都不動：{cmds:?}"
    );
    h.run_steps(3);
    assert_eq!(h.state().window_mode(), vitascope::window_mode::WindowMode::Pip);
}

/// 網路串流在等快取：系統收到「在等」（位置不會前進，不會被當成一直在跳轉），等到了再收到一次
#[test]
fn a_stall_for_cache_reaches_the_session() {
    let dir = TempDir::new("buffering");
    let long = dir.copy("common/mp4_long.mp4", "long.mp4");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![long], dir.settings());
    step_until_app(&mut h, "開始播放", |app| playing(app, "long.mp4"));
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    assert!(!fake.last_playback().unwrap().buffering);
    // 本機檔案不會真的等快取：直接改介面記下的狀態（mpv 那邊沒變，不會再通知）
    h.state_mut().player_mut().state.paused_for_cache = true;
    step_until(&mut h, "系統收到「在等快取」", || {
        fake.last_playback().is_some_and(|p| p.buffering)
    });
    assert_eq!(last_status(&fake), Some(Status::Playing), "狀態還是播放中");
    h.state_mut().player_mut().state.paused_for_cache = false;
    step_until(&mut h, "系統收到「等到了」", || {
        fake.last_playback().is_some_and(|p| !p.buffering)
    });
}

/// 系統那一端斷線了（Windows 的檔案總管重開之類的）：結束工作階段；同一個檔案播放期間不一直重建，
/// 下一個檔案載入時重新建立、重新送檔案資訊
#[test]
fn a_lost_session_is_rebuilt_with_the_next_file() {
    let dir = TempDir::new("lost");
    let (a, b) = (dir.clip("a.mp4"), dir.clip("b.mp4"));
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![a, b], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    assert_eq!(fake.created(), 1);
    fake.lose();
    step_until(&mut h, "斷線的工作階段結束了", || fake.shutdowns() == 1);
    assert!(!h.state().media_session_active());
    assert!(h.state().media_session_lost());
    wait_real(&mut h, 0.5);
    assert_eq!(fake.attempts(), 1, "同一個檔案播放期間不重建");
    fake.take_updates();
    h.key_press(egui::Key::PageDown);
    step_until_app(&mut h, "b.mp4 開始播放", |app| playing(app, "b.mp4"));
    step_until(&mut h, "重新建立", || fake.created() == 2);
    step_until(&mut h, "新的工作階段收到 b.mp4", || {
        fake.last_track()
            .is_some_and(|t| t.url.is_some_and(|u| u.ends_with("b.mp4")))
    });
    assert!(h.state().media_session_active());
    assert!(!h.state().media_session_lost());
}

/// 換檔途中（StartFile 之後、FileLoaded 之前）才發現斷線：正在載入的檔案載入時就重新建立，
/// 不是要等到再下一個檔案（那可能是一整部電影）。檔案用 `/hold/` 停在載入中，斷線處理完才放行
#[test]
fn a_session_lost_while_the_next_file_loads_is_rebuilt_for_it() {
    let server = Server::start();
    let dir = TempDir::new("lost-switch");
    let a = dir.clip("a.mp4");
    let fake = FakeMedia::new();
    let mut h = with_media(&fake, vec![a], dir.settings());
    step_until_app(&mut h, "a.mp4 開始播放", |app| playing(app, "a.mp4"));
    step_until(&mut h, "系統收到「播放中」", || {
        last_status(&fake) == Some(Status::Playing)
    });
    assert_eq!(fake.created(), 1);
    h.event(egui::Event::Paste(server.url("/hold/common/mp4_h264_aac.mp4")));
    step_until(&mut h, "連到伺服器", || !server.requests_to("/hold/").is_empty());
    step_until_app(&mut h, "換檔途中（舊的卸載了、新的載入中）", |app| {
        let s = &app.player().state;
        s.loading && !s.loaded
    });
    fake.lose();
    step_until(&mut h, "斷線的工作階段結束了", || fake.shutdowns() == 1);
    assert!(!h.state().media_session_active());
    wait_real(&mut h, 0.3);
    assert_eq!(fake.attempts(), 1, "還在載入：先不重建");
    fake.take_updates();
    server.release();
    step_until_app(&mut h, "網址的檔案開始播放", |app| {
        playing(app, "mp4_h264_aac.mp4")
    });
    step_until(&mut h, "正在載入的檔案載入時重新建立", || {
        fake.created() == 2
    });
    step_until(&mut h, "新的工作階段收到這個檔案", || {
        fake.last_track().is_some_and(|t| t.title == "mp4_h264_aac.mp4")
    });
    assert!(h.state().media_session_active());
    assert!(!h.state().media_session_lost());
}

// ───────────── Windows：真的 SMTC ─────────────

/// 真的 SMTC 來回一趟：從系統那一端（`GlobalSystemMediaTransportControlsSessionManager`，跟音量浮動視窗看到的一樣）
/// 找到影戲的工作階段，看標題、封面、時間軸、重複，再按它的按鈕，影戲收到對應的指令；結束後系統不再列出。
/// 只操作這個測試自己的工作階段（不送全域的媒體鍵）；會短暫出現在使用者的媒體控制裡，所以 `#[ignore]`
#[cfg(windows)]
#[test]
#[ignore = "會短暫出現在 Windows 的媒體控制（音量浮動視窗）裡：在開發機上手動跑"]
fn smtc_session_round_trip() {
    use std::num::NonZeroIsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vitascope::media_keys::{Cover, PlaybackInfo, TrackInfo};
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as SessionStatus,
    };
    use windows::Media::{MediaPlaybackAutoRepeatMode, MediaPlaybackType};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, UnregisterClassW, WNDCLASSW,
        WS_OVERLAPPEDWINDOW,
    };

    const TICKS: f64 = 10_000_000.0;
    let pid = std::process::id();
    // 測試自己的隱藏視窗（SMTC 跟著視窗）
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let class = wide(&format!("VitaScopeSmtcTest{pid}"));
    // SAFETY: 這個測試執行緒自己的視窗類別與隱藏視窗（不顯示），測試結束前銷毀
    let hwnd = unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: Some(DefWindowProcW),
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        assert_ne!(RegisterClassW(&wc), 0);
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            100,
            100,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    assert!(!hwnd.is_null());
    let owner = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(
        NonZeroIsize::new(hwnd as isize).unwrap(),
    ));
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let (mut session, commands) = media_keys::create_system(
        Some(owner),
        "VitaScope",
        Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        }),
    )
    .expect("建立 SMTC");

    let title = format!("VitaScope 測試 {pid}");
    let mut track = TrackInfo {
        file_gen: 1,
        kind: MediaKind::Audio,
        title: format!("{title}（舊標題）"),
        artist: Some("影戲".into()),
        album: Some("測試專輯".into()),
        album_artist: None,
        track_no: Some(3),
        duration: Some(120.0),
        url: None,
        details: true,
    };
    // 4×4 的紅色 PNG
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, 4, 4);
        enc.set_color(png::ColorType::Rgb);
        enc.write_header()
            .unwrap()
            .write_image_data(&[255, 0, 0].repeat(16))
            .unwrap();
    }
    let info = PlaybackInfo {
        status: Status::Playing,
        position: 30.0,
        rate: 1.0,
        seekable: true,
        can_next: true,
        can_prev: true,
        shuffle: false,
        repeat: Repeat::All,
        volume: 1.0,
        buffering: false,
        can_play: true,
    };
    session.apply(&Update::Track(track.clone()));
    session.apply(&Update::Cover(Cover {
        file_gen: 1,
        png: Arc::new(png),
        path: PathBuf::from("cover.png"),
    }));
    session.apply(&Update::Playback {
        info: info.clone(),
        seeked: true,
    });
    // 同一個檔案換標題（ClearAll）：封面要放回去（S-B12）
    track.title = title.clone();
    session.apply(&Update::Track(track.clone()));
    session.apply(&Update::Playback { info, seeked: true });

    let manager = Manager::RequestAsync().unwrap().join().unwrap();
    let find = |want: &str| -> Option<Session> {
        manager.GetSessions().ok()?.into_iter().find(|s| {
            s.TryGetMediaPropertiesAsync()
                .and_then(|op| op.join())
                .and_then(|p| p.Title())
                .is_ok_and(|t| t == want)
        })
    };
    let start = Instant::now();
    let found = loop {
        if let Some(s) = find(&title) {
            break s;
        }
        assert!(start.elapsed() < TIMEOUT, "系統的媒體控制找不到「{title}」");
        std::thread::sleep(Duration::from_millis(200));
    };
    let props = found.TryGetMediaPropertiesAsync().unwrap().join().unwrap();
    assert_eq!(props.Artist().unwrap(), "影戲");
    assert_eq!(props.AlbumTitle().unwrap(), "測試專輯");
    assert_eq!(props.TrackNumber().unwrap(), 3);
    assert_eq!(props.PlaybackType().unwrap().Value().unwrap(), MediaPlaybackType::Music);
    assert!(props.Thumbnail().is_ok(), "換標題之後封面還在");
    let tl = found.GetTimelineProperties().unwrap();
    assert_eq!(tl.EndTime().unwrap().Duration as f64 / TICKS, 120.0);
    assert_eq!(tl.MaxSeekTime().unwrap().Duration as f64 / TICKS, 120.0);
    assert_eq!(tl.Position().unwrap().Duration as f64 / TICKS, 30.0);
    let pb = found.GetPlaybackInfo().unwrap();
    assert_eq!(pb.PlaybackStatus().unwrap(), SessionStatus::Playing);
    assert_eq!(
        pb.AutoRepeatMode().unwrap().Value().unwrap(),
        MediaPlaybackAutoRepeatMode::List
    );
    assert!(pb.Controls().unwrap().IsNextEnabled().unwrap());

    // 系統那一端按按鈕：影戲收到對應的指令（播放中按播放／暫停 = 暫停）
    let expect = |what: &str, want: MediaCommand| {
        let got = commands
            .recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("沒收到 {what}"));
        assert_eq!(got, want, "{what}");
    };
    assert!(found.TryTogglePlayPauseAsync().unwrap().join().unwrap());
    expect("播放／暫停", MediaCommand::Pause);
    assert!(found.TrySkipNextAsync().unwrap().join().unwrap());
    expect("下一個", MediaCommand::Next);
    assert!(found.TrySkipPreviousAsync().unwrap().join().unwrap());
    expect("上一個", MediaCommand::Previous);
    assert!(found.TryStopAsync().unwrap().join().unwrap());
    expect("停止", MediaCommand::Stop);
    assert!(
        found
            .TryChangePlaybackPositionAsync(45 * TICKS as i64)
            .unwrap()
            .join()
            .unwrap()
    );
    expect(
        "時間軸",
        MediaCommand::SeekTo {
            secs: 45.0,
            track: Some(1),
        },
    );
    assert!(
        found
            .TryChangeAutoRepeatModeAsync(MediaPlaybackAutoRepeatMode::Track)
            .unwrap()
            .join()
            .unwrap()
    );
    expect("重複", MediaCommand::SetRepeat(Repeat::One));
    assert!(found.TryChangeShuffleActiveAsync(true).unwrap().join().unwrap());
    expect("隨機", MediaCommand::SetShuffle(true));
    assert!(found.TryChangePlaybackRateAsync(1.5).unwrap().join().unwrap());
    expect("速度", MediaCommand::SetRate(1.5));
    // 先送進通道、才叫醒介面（系統的執行緒上）：收到最後一個指令時那一下可能還沒叫，等一下
    let start = Instant::now();
    while wakes.load(Ordering::Relaxed) < 8 {
        assert!(
            start.elapsed() < TIMEOUT,
            "每個指令都叫醒介面：{}",
            wakes.load(Ordering::Relaxed)
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // 結束：系統不再列出
    session.shutdown();
    let start = Instant::now();
    while find(&title).is_some() {
        assert!(start.elapsed() < TIMEOUT, "結束之後系統還列著「{title}」");
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(session);

    // 關掉再打開設定（同一個視窗再建立一次）：又出現；按鈕只送到新的工作階段一次（舊的事件拿掉了）
    let (mut again, commands2) =
        media_keys::create_system(Some(owner), "VitaScope", Arc::new(|| {})).expect("再建立 SMTC");
    let title2 = format!("{title}（再一次）");
    track.file_gen = 2;
    track.title = title2.clone();
    again.apply(&Update::Track(track));
    again.apply(&Update::Playback {
        info: PlaybackInfo {
            status: Status::Paused,
            position: 0.0,
            rate: 1.0,
            seekable: true,
            can_next: true,
            can_prev: false,
            shuffle: false,
            repeat: Repeat::Off,
            volume: 1.0,
            buffering: false,
            can_play: true,
        },
        seeked: true,
    });
    let start = Instant::now();
    let found = loop {
        if let Some(s) = find(&title2) {
            break s;
        }
        assert!(start.elapsed() < TIMEOUT, "再建立之後找不到「{title2}」");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(found.TryTogglePlayPauseAsync().unwrap().join().unwrap());
    assert_eq!(
        commands2.recv_timeout(TIMEOUT),
        Ok(MediaCommand::Play),
        "暫停中按播放／暫停 = 播放"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(commands2.try_recv().is_err(), "只送一次");
    assert!(commands.try_recv().is_err(), "舊的工作階段收不到");
    again.shutdown();
    // SAFETY: 上面建立的視窗與類別
    unsafe {
        DestroyWindow(hwnd);
        UnregisterClassW(class.as_ptr(), std::ptr::null_mut());
    }
}

// ───────────── mpv：專輯封面存成 PNG ─────────────

/// 封面是怎麼來的：mpv 把專輯封面當成影像，`screenshot-to-file … video` 存得下來；沒有封面的音訊檔存不了（回覆失敗、不會當掉）
#[test]
fn mpv_saves_album_art_as_png_and_refuses_files_without_it() {
    let dir = TempDir::new("headless-cover");
    let mut p = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    for (rel, has_art) in [("general/audio_mp3_cover.mp3", true), ("general/audio_mp3.mp3", false)] {
        p.open(&sample(rel).to_string_lossy()).unwrap();
        // 載入完成；有封面的還要等影像設定好（兩個事件的先後不一定）
        let (mut loaded, mut reconfigured) = (false, !has_art);
        p.wait_for(TIMEOUT, |e| {
            loaded |= *e == PlayerEvent::FileLoaded;
            reconfigured |= *e == PlayerEvent::VideoReconfig;
            loaded && reconfigured
        })
        .unwrap();
        let out = dir.0.join(format!("{has_art}.png"));
        p.screenshot_to_file(77, &out.to_string_lossy(), false).unwrap();
        let mut reply = None;
        p.wait_for(TIMEOUT, |e| match e {
            PlayerEvent::CommandReply { id: 77, error } => {
                reply = Some(error.clone());
                true
            }
            _ => false,
        })
        .unwrap();
        let error = reply.unwrap();
        assert_eq!(error.is_none(), has_art, "{rel}：{error:?}");
        if has_art {
            let png = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&out).unwrap()))
                .read_info()
                .unwrap();
            assert_eq!((png.info().width, png.info().height), (300, 300));
        } else {
            assert!(!out.exists());
        }
    }
}

// ───────────── Linux：MPRIS（測試自己開的 D-Bus 匯流排）─────────────

/// MPRIS 的測試：每個測試自己開一個 `dbus-daemon --session`（私人的匯流排，用完就關），只連到它的位址，
/// 絕不碰使用者真的工作階段匯流排（不讀 DBUS_SESSION_BUS_ADDRESS）。
/// 沒有 dbus-daemon 時：開發機上跳過並說明；CI（有 `CI` 變數）上失敗，不能悄悄跳過（S-C10）
#[cfg(all(unix, not(target_os = "macos")))]
mod mpris {
    use super::*;
    use std::collections::HashMap;
    use std::io::BufRead;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use vitascope::media_keys::mpris::{
        self as m, BUS_NAME, DESKTOP_ENTRY, NO_TRACK, OBJECT_PATH, PLAYER_IFACE, ROOT_IFACE,
    };
    use vitascope::media_keys::{Cover, MediaBackend, MediaSession, PlaybackInfo, TrackInfo, Wake};
    use zbus::blocking::{Connection, MessageIterator, Proxy};
    use zbus::zvariant::{ObjectPath, OwnedValue, Value};

    /// 測試自己的匯流排（Drop 時關掉）
    struct PrivateBus {
        child: Child,
        address: String,
    }

    impl PrivateBus {
        /// None = 這台電腦沒有 dbus-daemon（不在 CI 上：跳過）
        fn start() -> Option<Self> {
            let spawned = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "CI 上一定要有 dbus-daemon（ci.yml 的 apt 要裝 dbus-daemon）：MPRIS 的測試不能跳過"
                    );
                    eprintln!("跳過：找不到 dbus-daemon（apt install dbus-daemon），MPRIS 的測試沒有跑");
                    return None;
                }
                Err(e) => panic!("無法執行 dbus-daemon：{e}"),
            };
            let mut line = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let address = line.trim().to_owned();
            assert!(!address.is_empty(), "dbus-daemon 沒有印出位址");
            Some(Self { child, address })
        }

        /// 匯流排突然不見了（像重新登入、dbus-daemon 當掉）
        fn kill(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            self.kill();
        }
    }

    /// 測試這一端（像 playerctl、桌面的媒體控制）
    fn client(bus: &PrivateBus) -> Connection {
        zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .unwrap()
    }

    /// `name` 的 `iface` 介面；屬性每次都問（不用快取，才看得到真的值）
    fn proxy(conn: &Connection, name: &str, iface: &'static str) -> Proxy<'static> {
        zbus::blocking::proxy::Builder::<Proxy>::new(conn)
            .destination(name.to_owned())
            .unwrap()
            .path(OBJECT_PATH)
            .unwrap()
            .interface(iface)
            .unwrap()
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .unwrap()
    }

    /// 名稱現在的擁有者（唯一名稱）；沒有人時 None
    fn name_owner(conn: &Connection, name: &str) -> Option<String> {
        conn.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(name,),
        )
        .ok()?
        .body()
        .deserialize::<String>()
        .ok()
    }

    /// 收到的訊號
    #[derive(Debug)]
    enum Signal {
        /// PropertiesChanged 帶的屬性
        Changed(HashMap<String, OwnedValue>),
        Seeked(i64),
    }

    /// 聽 `sender`（影戲的連線）送出的訊號
    fn signals(conn: &Connection, sender: &str) -> Receiver<Signal> {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(sender.to_owned())
            .unwrap()
            .path(OBJECT_PATH)
            .unwrap()
            .build();
        let iter = MessageIterator::for_match_rule(rule, conn, None).unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for msg in iter.flatten() {
                let header = msg.header();
                let signal = match header.member().map(|m| m.as_str()) {
                    Some("PropertiesChanged") => {
                        let (iface, changed, _) = msg
                            .body()
                            .deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                            .unwrap();
                        assert_eq!(iface, PLAYER_IFACE);
                        Signal::Changed(changed)
                    }
                    Some("Seeked") => Signal::Seeked(msg.body().deserialize::<i64>().unwrap()),
                    _ => continue,
                };
                if tx.send(signal).is_err() {
                    break;
                }
            }
        });
        rx
    }

    fn next_signal(rx: &Receiver<Signal>) -> Signal {
        rx.recv_timeout(TIMEOUT).expect("等不到訊號")
    }

    /// 下一個 PropertiesChanged 帶的屬性名稱（排好）與內容
    fn next_changed(rx: &Receiver<Signal>) -> (Vec<String>, HashMap<String, OwnedValue>) {
        match next_signal(rx) {
            Signal::Changed(map) => {
                let mut names: Vec<String> = map.keys().cloned().collect();
                names.sort();
                (names, map)
            }
            other => panic!("要 PropertiesChanged，收到 {other:?}"),
        }
    }

    /// 換檔時的 PropertiesChanged（可能分成幾個：Track 與 Playback 不一定一起處理）合起來，直到 Seeked；
    /// 回傳變了的屬性（排好；同一個屬性只能送一次）、內容與 Seeked 的位置
    fn changes_until_seeked(rx: &Receiver<Signal>) -> (Vec<String>, HashMap<String, OwnedValue>, i64) {
        let mut all = HashMap::new();
        loop {
            match next_signal(rx) {
                Signal::Changed(map) => {
                    for (k, v) in map {
                        assert!(!all.contains_key(&k), "{k} 送了兩次");
                        all.insert(k, v);
                    }
                }
                Signal::Seeked(us) => {
                    let mut names: Vec<String> = all.keys().cloned().collect();
                    names.sort();
                    return (names, all, us);
                }
            }
        }
    }

    fn next_seeked(rx: &Receiver<Signal>) -> i64 {
        match next_signal(rx) {
            Signal::Seeked(us) => us,
            other => panic!("要 Seeked，收到 {other:?}"),
        }
    }

    fn next_command(rx: &Receiver<MediaCommand>) -> MediaCommand {
        rx.recv_timeout(TIMEOUT).expect("等不到指令")
    }

    fn string(v: &OwnedValue) -> String {
        String::try_from(v.try_clone().unwrap()).unwrap()
    }

    fn object_path(v: &OwnedValue) -> String {
        ObjectPath::try_from(v.try_clone().unwrap()).unwrap().to_string()
    }

    fn track(file_gen: u64) -> TrackInfo {
        TrackInfo {
            file_gen,
            kind: MediaKind::Audio,
            title: "測試的歌".into(),
            artist: Some("演出者".into()),
            album: Some("專輯".into()),
            album_artist: Some("專輯演出者".into()),
            track_no: Some(3),
            duration: Some(200.0),
            url: Some("file:///music/a.mp3".into()),
            details: true,
        }
    }

    fn info(status: Status, position: f64) -> PlaybackInfo {
        PlaybackInfo {
            status,
            position,
            rate: 1.0,
            seekable: true,
            can_next: true,
            can_prev: false,
            shuffle: false,
            repeat: Repeat::Off,
            volume: 0.8,
            buffering: false,
            can_play: true,
        }
    }

    fn counting_wake() -> (Arc<AtomicUsize>, Wake) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        let wake: Wake = Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        (n, wake)
    }

    /// 等到條件成立（D-Bus 的另一端、背景執行緒）
    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let start = Instant::now();
        while !cond() {
            assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 系統（像 playerctl）讀得到影戲的檔案與狀態、送的指令影戲收得到；PropertiesChanged 只帶變了的屬性，
    /// 跳轉時送 Seeked；第二個影戲拿 `.instance<pid>`，第一個的名稱不會被搶走（S-A1）
    #[test]
    fn mpris_round_trip() {
        let Some(bus) = PrivateBus::start() else { return };
        let (wakes, wake) = counting_wake();
        let (mut first, commands) = m::connect_to(&bus.address, 1111, "VitaScope 測試", wake).unwrap();
        assert_eq!(first.bus_name(), BUS_NAME);
        let conn = client(&bus);
        assert_eq!(name_owner(&conn, BUS_NAME).as_deref(), Some(first.unique_name()));
        let root = proxy(&conn, BUS_NAME, ROOT_IFACE);
        let player = proxy(&conn, BUS_NAME, PLAYER_IFACE);

        // 固定的屬性
        assert_eq!(root.get_property::<String>("Identity").unwrap(), "VitaScope 測試");
        assert_eq!(root.get_property::<String>("DesktopEntry").unwrap(), DESKTOP_ENTRY);
        assert!(root.get_property::<bool>("CanQuit").unwrap());
        assert!(root.get_property::<bool>("CanRaise").unwrap());
        assert!(!root.get_property::<bool>("HasTrackList").unwrap());
        assert_eq!(
            root.get_property::<Vec<String>>("SupportedUriSchemes").unwrap(),
            ["file", "http", "https"]
        );
        assert!(
            root.get_property::<Vec<String>>("SupportedMimeTypes")
                .unwrap()
                .contains(&"video/x-matroska".to_owned())
        );
        assert!(player.get_property::<bool>("CanControl").unwrap());
        assert_eq!(player.get_property::<f64>("MinimumRate").unwrap(), 0.25);
        assert_eq!(player.get_property::<f64>("MaximumRate").unwrap(), 4.0);
        // 還沒有檔案：停止、NoTrack
        assert_eq!(player.get_property::<String>("PlaybackStatus").unwrap(), "Stopped");
        let meta = player.get_property::<HashMap<String, OwnedValue>>("Metadata").unwrap();
        assert_eq!(object_path(&meta["mpris:trackid"]), NO_TRACK);
        assert!(!player.get_property::<bool>("CanPlay").unwrap());

        // 影戲開了檔案、從 30 秒開始：變了的屬性（排著的更新一起處理，通常合成一個 PropertiesChanged），接著 Seeked
        let rx = signals(&conn, first.unique_name());
        first.apply(&Update::Track(track(4)));
        first.apply(&Update::Playback {
            info: info(Status::Playing, 30.0),
            seeked: true,
        });
        let (names, changed, us) = changes_until_seeked(&rx);
        assert_eq!(
            names,
            [
                "CanGoNext",
                "CanPause",
                "CanPlay",
                "CanSeek",
                "Metadata",
                "PlaybackStatus",
                "Volume"
            ]
        );
        assert_eq!(string(&changed["PlaybackStatus"]), "Playing");
        assert!((30_000_000..33_000_000).contains(&us), "{us}");
        // Metadata 的型別（MPRIS 規格）：trackid 是物件路徑、長度是 x（微秒）、演出者是字串陣列
        let meta = player.get_property::<HashMap<String, OwnedValue>>("Metadata").unwrap();
        let get = |k: &str| meta[k].try_clone().unwrap();
        assert_eq!(
            object_path(&meta["mpris:trackid"]),
            "/io/github/acer1204/vitascope/track/4"
        );
        assert_eq!(i64::try_from(get("mpris:length")).unwrap(), 200_000_000);
        assert_eq!(String::try_from(get("xesam:title")).unwrap(), "測試的歌");
        assert!(matches!(&*meta["xesam:artist"], Value::Array(_)), "演出者是陣列");
        assert_eq!(Vec::<String>::try_from(get("xesam:artist")).unwrap(), ["演出者"]);
        assert_eq!(
            Vec::<String>::try_from(get("xesam:albumArtist")).unwrap(),
            ["專輯演出者"]
        );
        assert_eq!(String::try_from(get("xesam:album")).unwrap(), "專輯");
        assert_eq!(i32::try_from(get("xesam:trackNumber")).unwrap(), 3);
        assert_eq!(String::try_from(get("xesam:url")).unwrap(), "file:///music/a.mp3");
        assert!(!meta.contains_key("mpris:artUrl"), "還沒有封面");
        // 位置：系統要的時候才算（播放中往後推算）
        let pos = player.get_property::<i64>("Position").unwrap();
        assert!((30_000_000..60_000_000).contains(&pos), "{pos}");
        assert_eq!(player.get_property::<f64>("Volume").unwrap(), 0.8);
        assert_eq!(player.get_property::<String>("LoopStatus").unwrap(), "None");

        // 封面到了：artUrl 是快取資料夾裡的檔案（file://）
        first.apply(&Update::Cover(Cover {
            file_gen: 4,
            png: Arc::new(vec![1, 2, 3]),
            path: PathBuf::from("/tmp/vitascope 封面/cover-1-4.png"),
        }));
        let (names, changed) = next_changed(&rx);
        assert_eq!(names, ["Metadata"]);
        let art = HashMap::<String, OwnedValue>::try_from(changed["Metadata"].try_clone().unwrap()).unwrap();
        assert_eq!(
            string(&art["mpris:artUrl"]),
            "file:///tmp/vitascope%20%E5%B0%81%E9%9D%A2/cover-1-4.png"
        );

        // 只有位置變了（播放中的定期更新）：什麼都不送；接著暫停：只有 PlaybackStatus
        first.apply(&Update::Playback {
            info: info(Status::Playing, 35.0),
            seeked: false,
        });
        first.apply(&Update::Playback {
            info: info(Status::Paused, 35.5),
            seeked: false,
        });
        let (names, changed) = next_changed(&rx);
        assert_eq!(names, ["PlaybackStatus"], "沒有多送其他屬性");
        assert_eq!(string(&changed["PlaybackStatus"]), "Paused");
        // 隨機、重複：只有那兩個
        let mut shuffled = info(Status::Paused, 35.5);
        shuffled.shuffle = true;
        shuffled.repeat = Repeat::One;
        first.apply(&Update::Playback {
            info: shuffled.clone(),
            seeked: false,
        });
        assert_eq!(next_changed(&rx).0, ["LoopStatus", "Shuffle"]);
        // 跳轉（暫停中）：只有 Seeked（屬性沒變就不送 PropertiesChanged）
        shuffled.position = 50.0;
        first.apply(&Update::Playback {
            info: shuffled,
            seeked: true,
        });
        assert_eq!(next_seeked(&rx), 50_000_000);
        assert_eq!(
            player.get_property::<i64>("Position").unwrap(),
            50_000_000,
            "暫停中不動"
        );

        // 系統的指令 → 影戲
        for (method, cmd) in [
            ("PlayPause", MediaCommand::PlayPause),
            ("Play", MediaCommand::Play),
            ("Pause", MediaCommand::Pause),
            ("Stop", MediaCommand::Stop),
            ("Next", MediaCommand::Next),
            ("Previous", MediaCommand::Previous),
        ] {
            player.call_method(method, &()).unwrap();
            assert_eq!(next_command(&commands), cmd, "{method}");
        }
        player.call_method("Seek", &(-5_000_000_i64,)).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::SeekBy(-5.0));
        let current = ObjectPath::try_from("/io/github/acer1204/vitascope/track/4").unwrap();
        let stale = ObjectPath::try_from("/io/github/acer1204/vitascope/track/3").unwrap();
        // 別的檔案的 SetPosition（換檔之前拖的）不理：下一個收到的是後面這個
        player.call_method("SetPosition", &(&stale, 1_000_000_i64)).unwrap();
        player.call_method("SetPosition", &(&current, 12_500_000_i64)).unwrap();
        assert_eq!(
            next_command(&commands),
            MediaCommand::SeekTo {
                secs: 12.5,
                track: Some(4)
            }
        );
        // 設定屬性：送指令；zbus 設定完自己送的 PropertiesChanged 帶的是要的值（不是原來的值）
        player.set_property("LoopStatus", "Playlist").unwrap();
        assert_eq!(next_command(&commands), MediaCommand::SetRepeat(Repeat::All));
        let (names, changed) = next_changed(&rx);
        assert_eq!(
            (names, string(&changed["LoopStatus"])),
            (vec!["LoopStatus".to_owned()], "Playlist".to_owned())
        );
        assert!(player.set_property("LoopStatus", "Forever").is_err(), "不認得的值");
        player.set_property("Shuffle", false).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::SetShuffle(false));
        let (names, changed) = next_changed(&rx);
        assert_eq!(names, ["Shuffle"]);
        assert!(!bool::try_from(changed["Shuffle"].try_clone().unwrap()).unwrap());
        player.set_property("Rate", 1.5).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::SetRate(1.5));
        let (names, changed) = next_changed(&rx);
        assert_eq!(names, ["Rate"]);
        assert_eq!(f64::try_from(changed["Rate"].try_clone().unwrap()).unwrap(), 1.5);
        player.set_property("Volume", 0.25).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::SetVolume(0.25));
        let (names, changed) = next_changed(&rx);
        assert_eq!(names, ["Volume"]);
        assert_eq!(f64::try_from(changed["Volume"].try_clone().unwrap()).unwrap(), 0.25);
        assert_eq!(player.get_property::<f64>("Volume").unwrap(), 0.25);
        // 影戲照做了：同樣的值不再送一次（下一個 PropertiesChanged 只有後來真的變了的 PlaybackStatus）
        let mut applied = info(Status::Paused, 50.0);
        applied.repeat = Repeat::All;
        applied.rate = 1.5;
        applied.volume = 0.25;
        first.apply(&Update::Playback {
            info: applied.clone(),
            seeked: false,
        });
        applied.status = Status::Playing;
        first.apply(&Update::Playback {
            info: applied.clone(),
            seeked: false,
        });
        assert_eq!(next_changed(&rx).0, ["PlaybackStatus"], "沒有重送設定過的屬性");
        // 影戲沒照做（音量超過上限）：系統先顯示要的值，影戲送來真的值時改回來
        player.set_property("Volume", 5.0).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::SetVolume(5.0));
        assert_eq!(next_changed(&rx).0, ["Volume"]);
        applied.volume = 1.5;
        first.apply(&Update::Playback {
            info: applied.clone(),
            seeked: false,
        });
        let (names, changed) = next_changed(&rx);
        assert_eq!(names, ["Volume"]);
        assert_eq!(f64::try_from(changed["Volume"].try_clone().unwrap()).unwrap(), 1.5);
        assert_eq!(player.get_property::<f64>("Volume").unwrap(), 1.5);
        // OpenUri：網址也可以（N-M12）；不支援的網址回錯誤、不送
        assert!(player.call_method("OpenUri", &("ftp://example.com/a.mp4",)).is_err());
        player.call_method("OpenUri", &("https://example.com/a.m3u8",)).unwrap();
        assert_eq!(
            next_command(&commands),
            MediaCommand::OpenUri("https://example.com/a.m3u8".into())
        );
        root.call_method("Raise", &()).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::Raise);
        root.call_method("Quit", &()).unwrap();
        assert_eq!(next_command(&commands), MediaCommand::Quit);
        assert!(commands.try_recv().is_err(), "沒有多出來的指令");
        // 每個指令都叫醒介面（指令先送進通道才叫醒：數字可能晚一點才到）
        wait_until("叫醒介面", || wakes.load(Ordering::SeqCst) >= 15);

        // 第二個影戲：拿 `.instance<pid>`，第一個的名稱還是第一個的（不會被搶走，S-A1）
        let (mut second, second_commands) = m::connect_to(&bus.address, 2222, "VitaScope 2", Arc::new(|| {})).unwrap();
        assert_eq!(second.bus_name(), "org.mpris.MediaPlayer2.vitascope.instance2222");
        assert_eq!(name_owner(&conn, BUS_NAME).as_deref(), Some(first.unique_name()));
        assert_eq!(
            name_owner(&conn, second.bus_name()).as_deref(),
            Some(second.unique_name())
        );
        // 指令送到對的那一個
        proxy(&conn, second.bus_name(), PLAYER_IFACE)
            .call_method("Next", &())
            .unwrap();
        assert_eq!(next_command(&second_commands), MediaCommand::Next);
        assert!(commands.try_recv().is_err(), "第一個收不到第二個的指令");
        // 兩個名稱都有人用了：建立失敗、說明原因
        assert!(m::connect_to(&bus.address, 2222, "VitaScope 3", Arc::new(|| {})).is_err());

        // 結束：名稱放掉（系統的媒體控制不再顯示）；另一個不受影響
        first.shutdown();
        wait_until("第一個的名稱放掉了", || name_owner(&conn, BUS_NAME).is_none());
        assert_eq!(
            name_owner(&conn, "org.mpris.MediaPlayer2.vitascope.instance2222").as_deref(),
            Some(second.unique_name())
        );
        assert!(second.alive());
        second.shutdown();
        wait_until("第二個的名稱放掉了", || {
            name_owner(&conn, "org.mpris.MediaPlayer2.vitascope.instance2222").is_none()
        });
    }

    /// 停止（卸載）之後：NoTrack、Stopped、不能跳轉；清單上還有檔案時可以按播放，沒有時不能
    #[test]
    fn mpris_clear_shows_stopped() {
        let Some(bus) = PrivateBus::start() else { return };
        let (mut session, _commands) = m::connect_to(&bus.address, 1, "VitaScope", Arc::new(|| {})).unwrap();
        let conn = client(&bus);
        let player = proxy(&conn, session.bus_name(), PLAYER_IFACE);
        let rx = signals(&conn, session.unique_name());
        session.apply(&Update::Track(track(1)));
        session.apply(&Update::Playback {
            info: info(Status::Playing, 1.0),
            seeked: true,
        });
        changes_until_seeked(&rx);
        session.apply(&Update::Clear);
        let (names, _) = next_changed(&rx);
        assert_eq!(names, ["CanPause", "CanSeek", "Metadata", "PlaybackStatus"]);
        assert_eq!(player.get_property::<String>("PlaybackStatus").unwrap(), "Stopped");
        assert_eq!(player.get_property::<i64>("Position").unwrap(), 0);
        assert!(player.get_property::<bool>("CanPlay").unwrap());
        assert!(!player.get_property::<bool>("CanSeek").unwrap());
        let meta = player.get_property::<HashMap<String, OwnedValue>>("Metadata").unwrap();
        assert_eq!(meta.len(), 1, "{meta:?}");
        assert_eq!(object_path(&meta["mpris:trackid"]), NO_TRACK);

        // 停止時清單上沒有東西可播了（SessionSync 在 Clear 之前送停止後的狀態）：CanPlay 變成 false、有送出去
        session.apply(&Update::Track(track(2)));
        session.apply(&Update::Playback {
            info: info(Status::Playing, 1.0),
            seeked: true,
        });
        changes_until_seeked(&rx);
        let mut stopped = info(Status::Stopped, 0.0);
        stopped.seekable = false;
        stopped.can_play = false;
        session.apply(&Update::Playback {
            info: stopped,
            seeked: false,
        });
        session.apply(&Update::Clear);
        let mut changed = HashMap::new();
        while !changed.contains_key("Metadata") {
            changed.extend(next_changed(&rx).1);
        }
        assert_eq!(
            changed
                .get("CanPlay")
                .map(|v| bool::try_from(v.try_clone().unwrap()).unwrap()),
            Some(false)
        );
        assert!(!player.get_property::<bool>("CanPlay").unwrap());
    }

    /// 匯流排不見了（重新登入、dbus-daemon 當掉）：工作階段變成不能用、叫醒介面（介面在下一個檔案時重新建立）
    #[test]
    fn a_lost_bus_marks_the_session_dead() {
        let Some(mut bus) = PrivateBus::start() else { return };
        let (wakes, wake) = counting_wake();
        let (mut session, _commands) = m::connect_to(&bus.address, 1, "VitaScope", wake).unwrap();
        assert!(session.alive());
        bus.kill();
        wait_until("發現斷線", || !session.alive());
        wait_until("叫醒介面", || wakes.load(Ordering::SeqCst) >= 1);
        // 斷線之後的更新、結束都不會卡住
        let start = Instant::now();
        session.apply(&Update::Clear);
        session.shutdown();
        assert!(start.elapsed() < Duration::from_secs(1), "{:?}", start.elapsed());
    }

    /// 真的影戲（介面、播放器）透過 MPRIS 控制：系統讀得到片名、狀態，暫停、跳轉、下一個都照做
    #[test]
    fn the_app_is_controlled_over_mpris() {
        let Some(bus) = PrivateBus::start() else { return };
        let dir = TempDir::new("mpris-app");
        let files = [dir.clip("a.mp4"), dir.clip("b.mp4")];
        let address = bus.address.clone();
        let launch = Launch {
            files: vec![files[0].clone()],
            media: MediaBackend::Custom(Box::new(move |wake| {
                m::connect_to(&address, 4321, "VitaScope", wake)
                    .map(|(s, rx)| (Box::new(s) as Box<dyn MediaSession>, rx))
            })),
            ..Default::default()
        };
        let mut h = harness(launch, dir.settings());
        step_until_app(&mut h, "a.mp4 開始播放、掃描完", |app| {
            playing(app, "a.mp4") && app.playlist().is_some_and(|l| l.len() == 2)
        });
        step_until_app(&mut h, "工作階段建立了", |app| app.media_session_active());
        let conn = client(&bus);
        let player = proxy(&conn, BUS_NAME, PLAYER_IFACE);
        let status = || player.get_property::<String>("PlaybackStatus").unwrap();
        let meta = || player.get_property::<HashMap<String, OwnedValue>>("Metadata").unwrap();
        let url = || meta().get("xesam:url").map(string);
        step_until(&mut h, "系統看到 a.mp4 在播", || {
            status() == "Playing" && url().is_some_and(|u| u.ends_with("/a.mp4"))
        });
        assert!(player.get_property::<bool>("CanGoNext").unwrap());
        assert!(player.get_property::<bool>("CanSeek").unwrap());
        // 標題是 mpv 的標題（檔名），不是隱藏片名時的程式名稱
        step_until_app(&mut h, "mpv 的標題", |app| app.player().state.title.is_some());
        let title = h.state().player().state.title.clone().unwrap();
        assert_ne!(title, vitascope::app::app_name());
        step_until(&mut h, "系統看到標題", || {
            meta().get("xesam:title").map(string).as_ref() == Some(&title)
        });
        // 暫停（像按了媒體鍵）
        player.call_method("PlayPause", &()).unwrap();
        step_until_app(&mut h, "影戲暫停了", |app| app.player().state.paused);
        step_until(&mut h, "系統看到暫停", || status() == "Paused");
        // 拖系統的進度條（帶目前檔案的 track id）。暫停中只有跳超過 SEEK_JUMP 才會送給系統：
        // 目標離暫停的位置夠遠（慢的電腦暫停得晚，位置不一定）
        let paused_at = h.state().player().state.time_pos;
        let target = if paused_at < 1.5 { 2.6 } else { 0.3 };
        let target_us = (target * 1e6) as i64;
        let owner = name_owner(&conn, BUS_NAME).unwrap();
        let rx = signals(&conn, &owner);
        let id = ObjectPath::try_from(meta()["mpris:trackid"].try_clone().unwrap()).unwrap();
        player.call_method("SetPosition", &(&id, target_us)).unwrap();
        step_until_app(&mut h, "跳到目標", |app| {
            (app.player().state.time_pos - target).abs() < 0.2
        });
        step_until(&mut h, "系統的位置跟著變", || {
            (player.get_property::<i64>("Position").unwrap() - target_us).abs() < 200_000
        });
        // 系統收到 Seeked（新的位置）
        let seeked = std::cell::Cell::new(None);
        step_until(&mut h, "Seeked", || {
            while let Ok(sig) = rx.try_recv() {
                if let Signal::Seeked(us) = sig {
                    seeked.set(Some(us));
                }
            }
            seeked.get().is_some()
        });
        let us = seeked.get().unwrap();
        assert!(
            (us - target_us).abs() < 200_000,
            "{us} / {target_us}（暫停在 {paused_at}）"
        );
        // 下一個檔案
        player.call_method("Next", &()).unwrap();
        step_until_app(&mut h, "b.mp4", |app| {
            app.player().state.loaded && app.player().state.path.as_deref().is_some_and(|p| p.ends_with("b.mp4"))
        });
        step_until(&mut h, "系統看到 b.mp4", || {
            url().is_some_and(|u| u.ends_with("/b.mp4"))
        });
        let new_id = ObjectPath::try_from(meta()["mpris:trackid"].try_clone().unwrap()).unwrap();
        assert_ne!(new_id, id, "換檔：新的 track id");
        // 關掉「系統媒體控制」：名稱放掉
        open_settings_page(&mut h, "系統");
        click(&mut h, "系統媒體控制");
        assert!(!h.state().settings().system_media.enabled);
        step_until(&mut h, "名稱放掉了", || name_owner(&conn, BUS_NAME).is_none());
    }
}
