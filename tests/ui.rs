//! 介面測試（ROADMAP 4.2）：用 egui_kittest 模擬按鍵與點擊，檢查播放器的反應。
//!
//! 播放器用 headless 模式（不出畫面、不出聲音），所以不需要 GPU，CI 也能跑。
//! 影片畫面本身的渲染另外用 `--shot` 自動截圖驗證。

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use vitascope::app::{Launch, VitascopeApp};
use vitascope::player::{Options, Player, State, TrackKind};
use vitascope::settings::Settings;

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

fn harness(file: Option<PathBuf>) -> Harness<'static, VitascopeApp> {
    // 樣本資料夾裡有很多檔案，播完自動接下一個會讓測試換到別的檔案；需要的測試再自己打開
    let mut settings = Settings::default();
    settings.auto_next = false;
    harness_with(file, settings)
}

fn harness_with(file: Option<PathBuf>, settings: Settings) -> Harness<'static, VitascopeApp> {
    harness_launch(
        Launch {
            files: file.into_iter().collect(),
            ..Default::default()
        },
        settings,
    )
}

fn harness_launch(launch: Launch, settings: Settings) -> Harness<'static, VitascopeApp> {
    // 跟真正的播放器一樣播完停在最後一格，只是不出畫面、不出聲音。
    // 播放紀錄只放在記憶體（Launch 的預設），不會碰到使用者真正的紀錄
    let player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch))
}

/// 一直跑介面幀，直到播放器狀態符合條件
fn step_until(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn(&State) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        if cond(&h.state().player().state) {
            return;
        }
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}\n目前狀態：{:#?}",
            h.state().player().state
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 開啟多軌樣本並等到開始播放
fn playing_multitrack() -> Harness<'static, VitascopeApp> {
    let mut h = harness(Some(sample("common/mkv_multitrack.mkv")));
    step_until(&mut h, "開始播放", |s| {
        s.loaded && !s.paused && s.time_pos > 0.0 && !s.tracks.is_empty() && s.video_size.is_some()
    });
    // 知道影片尺寸後，視窗會調整成影片大小；等版面穩定再找按鈕，不然會點到舊位置
    h.run_steps(5);
    h
}

#[test]
fn idle_screen_shows_hint() {
    let mut h = harness(None);
    h.step();
    h.get_by_label_contains("拖放到這裡");
}

#[test]
fn missing_file_shows_chinese_error() {
    let mut h = harness(Some(PathBuf::from("Z:/不存在/沒有這個檔案.mkv")));
    step_until(&mut h, "開檔失敗", |s| s.last_error.is_some());
    // 詳細原因（mpv 的記錄訊息）會晚一點送達，多跑幾幀
    h.run_steps(5);
    let err = h.state().player().state.last_error.clone().unwrap();
    assert!(err.contains("無法載入檔案"), "{err}");
    h.get_by_label_contains("無法載入檔案");
}

#[test]
fn space_toggles_pause() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "繼續播放", |s| !s.paused);
}

#[test]
fn arrow_keys_seek() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::Space); // 先暫停，時間才不會自己往前走
    step_until(&mut h, "暫停", |s| s.paused);
    let t0 = h.state().player().state.time_pos;

    h.key_press(egui::Key::ArrowRight);
    step_until(&mut h, "前進 5 秒", |s| s.time_pos >= t0 + 4.0);
    let t1 = h.state().player().state.time_pos;
    h.key_press(egui::Key::ArrowLeft);
    step_until(&mut h, "後退 5 秒", |s| s.time_pos <= t1 - 4.0);

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(
        &mut h,
        "Ctrl+→ 前進 30 秒（片長 20 秒，停在結尾）",
        |s| s.time_pos >= 15.0,
    );
}

#[test]
fn keys_change_volume_and_mute() {
    let mut h = playing_multitrack();
    step_until(&mut h, "音量 100", |s| s.volume == 100.0);
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(3);
    assert_eq!(h.state().player().state.volume, 100.0, "音量上限是 100");
    h.key_press(egui::Key::ArrowDown);
    step_until(&mut h, "音量 95", |s| s.volume == 95.0);
    h.key_press(egui::Key::M);
    step_until(&mut h, "靜音", |s| s.muted);
    // 靜音時調整音量會自動取消靜音
    h.key_press(egui::Key::ArrowDown);
    step_until(&mut h, "音量 90、取消靜音", |s| s.volume == 90.0 && !s.muted);
}

#[test]
fn subtitle_menu_switches_and_turns_off() {
    let mut h = playing_multitrack();
    // 字幕語言偏好：繁中優先
    step_until(&mut h, "自動選上繁中字幕", |s| {
        s.selected(TrackKind::Sub).and_then(|t| t.lang.as_deref()) == Some("chi")
    });

    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label_contains("English").click();
    step_until(&mut h, "換成英文字幕", |s| {
        s.selected(TrackKind::Sub).and_then(|t| t.lang.as_deref()) == Some("eng")
    });

    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label("關閉字幕").click();
    step_until(&mut h, "關閉字幕", |s| s.selected(TrackKind::Sub).is_none());
}

#[test]
fn audio_menu_switches_track() {
    let mut h = playing_multitrack();
    step_until(&mut h, "預設第一條音軌", |s| {
        s.selected(TrackKind::Audio).and_then(|t| t.title.as_deref()) == Some("日本語")
    });
    h.get_by_label("音軌").click();
    h.run_steps(2);
    h.get_by_label_contains("國語").click();
    step_until(&mut h, "換成國語", |s| {
        s.selected(TrackKind::Audio).and_then(|t| t.title.as_deref()) == Some("國語")
    });
}

/// 模擬從檔案總管拖放進來的檔案
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn drop_file(h: &mut Harness<'_, VitascopeApp>, path: PathBuf) {
    h.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(path)));
    h.step();
}

#[test]
fn dropping_video_opens_it() {
    let mut h = harness(None);
    h.step();
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "開始播放拖進來的影片", |s| {
        s.loaded && s.path.as_deref().is_some_and(|p| p.ends_with("mp4_h264_aac.mp4"))
    });
}

#[test]
fn dropping_subtitle_adds_it_to_current_video() {
    let mut h = playing_multitrack();
    let before = h.state().player().state.tracks_of(TrackKind::Sub).count();
    // 用別的樣本的外掛字幕（Big5 編碼）
    drop_file(&mut h, sample("common/extsub_srt_big5.srt"));
    step_until(&mut h, "多一條外掛字幕並選上", |s| {
        s.tracks_of(TrackKind::Sub).count() == before + 1 && s.selected(TrackKind::Sub).is_some_and(|t| t.external)
    });
    // 拖字幕不應該換掉正在播的影片
    assert!(
        h.state()
            .player()
            .state
            .path
            .as_deref()
            .unwrap()
            .ends_with("mkv_multitrack.mkv")
    );
}

/// 按下按鍵（同一幀內），回傳這一幀送給視窗的指令
fn press_and_get_commands(h: &mut Harness<'_, VitascopeApp>, key: egui::Key) -> Vec<egui::ViewportCommand> {
    h.input_mut().events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

#[test]
fn fullscreen_keys() {
    let mut h = playing_multitrack();
    for key in [egui::Key::F, egui::Key::Enter] {
        let cmds = press_and_get_commands(&mut h, key);
        assert!(
            cmds.contains(&egui::ViewportCommand::Fullscreen(true)),
            "{key:?} 應該切換到全螢幕：{cmds:?}"
        );
    }
    // 不在全螢幕時按 Esc 不該有動作（避免誤觸其他視窗操作）
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(
        !cmds.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))),
        "{cmds:?}"
    );
}

fn set_fullscreen(h: &mut Harness<'_, VitascopeApp>, on: bool) {
    if let Some(v) = h.input_mut().viewports.get_mut(&egui::ViewportId::ROOT) {
        v.fullscreen = Some(on);
    }
    h.run_steps(2);
}

#[test]
fn cursor_stays_visible_over_the_settings_window_in_fullscreen() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mp4_long.mp4")), settings);
    settle(&mut h, "mp4_long.mp4");
    set_fullscreen(&mut h, true);
    let cursor = |h: &Harness<'_, VitascopeApp>| h.output().platform_output.cursor_icon;
    // 對照：沒有開任何視窗時，2 秒不動滑鼠游標就隱藏
    std::thread::sleep(Duration::from_millis(2300));
    h.run_steps(3);
    assert_eq!(cursor(&h), egui::CursorIcon::None, "全螢幕播放中，滑鼠不動就隱藏");
    // 設定視窗開著：一直看得到游標
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    std::thread::sleep(Duration::from_millis(2300));
    h.run_steps(3);
    assert_ne!(cursor(&h), egui::CursorIcon::None, "設定視窗開著時游標不能消失");
}

// 手動測試抓到的 bug：Esc 被快捷鍵吃掉，選單關不掉，全螢幕也離不開
#[test]
fn escape_closes_menu_first_then_leaves_fullscreen() {
    let mut h = playing_multitrack();
    set_fullscreen(&mut h, true);

    h.get_by_label("字幕").click();
    h.run_steps(2);
    assert!(h.query_by_label("關閉字幕").is_some(), "選單應該打開");
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(
        !cmds.contains(&egui::ViewportCommand::Fullscreen(false)),
        "選單開著時 Esc 只關選單"
    );
    h.run_steps(2);
    assert!(h.query_by_label("關閉字幕").is_none(), "Esc 應該關掉選單");

    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(
        cmds.contains(&egui::ViewportCommand::Fullscreen(false)),
        "沒有選單時 Esc 離開全螢幕：{cmds:?}"
    );
}

// 手動測試抓到的 bug：暫停中開新檔案，新檔案也是暫停狀態
#[test]
fn opening_new_file_while_paused_starts_playing() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "新檔案開始播放", |s| {
        s.loaded && !s.paused && s.path.as_deref().is_some_and(|p| p.ends_with("mp4_h264_aac.mp4"))
    });
}

// 手動測試抓到的 bug：換檔時沿用上一部影片的尺寸，視窗沒有配合新影片調整
#[test]
fn window_fits_each_new_video() {
    let mut h = playing_multitrack(); // 640×360
    let first = h.ctx.content_rect().size();
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4")); // 320×240，會放大到 640 寬 → 640×480
    step_until(&mut h, "載入 4:3 影片", |s| {
        s.loaded && s.video_size == Some([320, 240])
    });
    h.run_steps(5);
    let second = h.ctx.content_rect().size();
    assert!(
        (second.y - first.y - 120.0).abs() < 2.0,
        "視窗應該從 16:9 變成 4:3（高度多 120）：{first:?} → {second:?}"
    );
}

#[test]
fn about_dialog_shows_author_links_and_license() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.get_by_label("acer1204");
    h.get_by_label("acer1204/VitaScope");
    h.get_by_label_contains("GPL-3.0");
    h.get_by_label_contains(&format!("版本 {}", env!("CARGO_PKG_VERSION")));
    // 關閉
    h.get_by_label("關閉").click();
    h.run_steps(2);
    assert!(
        h.query_by_label("acer1204/VitaScope").is_none(),
        "按「關閉」後視窗要消失"
    );
}

#[test]
fn about_button_in_controls_opens_dialog() {
    let mut h = harness(None);
    h.step();
    h.get_by_label("ℹ").click();
    h.run_steps(2);
    h.get_by_label("acer1204/VitaScope");
}

#[test]
fn update_available_asks_then_opens_releases_page() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.state_mut()
        .set_update_status(vitascope::update::UpdateStatus::Available {
            latest: "v9.9.9".into(),
        });
    h.run_steps(2);
    h.get_by_label_contains("有新版本 v9.9.9");
    h.get_by_label("是").click();
    // step() 會把滑鼠移入、按下、放開分成幾幀處理，開網址的指令在最後一幀（放開）
    h.step();
    let opened = h
        .output()
        .platform_output
        .commands
        .iter()
        .any(|c| matches!(c, egui::OutputCommand::OpenUrl(u) if u.url == vitascope::update::RELEASES_URL));
    assert!(
        opened,
        "按「是」要開啟 Releases 頁面：{:?}",
        h.output().platform_output.commands
    );
}

#[test]
fn reopening_about_allows_checking_again() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.state_mut()
        .set_update_status(vitascope::update::UpdateStatus::UpToDate {
            latest: "v0.1.0".into(),
        });
    h.run_steps(2);
    h.get_by_label_contains("已經是最新版本");
    h.get_by_label("關閉").click();
    h.run_steps(2);
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.get_by_label("檢查更新");
}

#[test]
fn update_failure_offers_releases_page() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.state_mut()
        .set_update_status(vitascope::update::UpdateStatus::Failed("測試".into()));
    h.run_steps(2);
    h.get_by_label("再試一次");
    h.get_by_label("開啟發佈頁面");
}

#[test]
fn update_available_no_just_dismisses() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.state_mut()
        .set_update_status(vitascope::update::UpdateStatus::Available {
            latest: "v9.9.9".into(),
        });
    h.run_steps(2);
    h.get_by_label("否").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains("有新版本").is_none());
    h.get_by_label("檢查更新");
}

#[test]
fn clicking_progress_bar_seeks() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    // 點進度條正中間 = 跳到片長（20 秒）的一半
    h.get_by_label("進度").click();
    step_until(&mut h, "跳到 10 秒附近", |s| (s.time_pos - 10.0).abs() < 1.0);
}

#[test]
fn play_button_toggles_pause() {
    let mut h = playing_multitrack();
    h.get_by_label("⏸").click();
    step_until(&mut h, "按鈕暫停", |s| s.paused);
    h.get_by_label("▶").click();
    step_until(&mut h, "按鈕播放", |s| !s.paused);
}

// ───────────── L2：播放控制 ─────────────

/// 每個測試自己的暫存資料夾（測試會平行執行）。測試失敗時也會刪掉；
/// 要宣告在 Harness 之前，才會在播放器關掉檔案之後才刪
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("vitascope-ui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// 放一份 3 秒的樣本，取名為 `name`
    fn clip(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::copy(sample("common/mp4_h264_aac.mp4"), &path).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn playing(s: &State, name: &str) -> bool {
    s.loaded && s.path.as_deref().is_some_and(|p| p.ends_with(name))
}

/// 等到等到條件成立（看的是整個播放器，例如背景掃描完的播放清單）
fn step_until_app(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn(&VitascopeApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn playlist_len(app: &VitascopeApp) -> usize {
    app.playlist().map_or(0, |l| l.len())
}

/// 等到視窗配合影片尺寸調整完（之後才能用座標點按鈕、開右鍵選單）
fn settle(h: &mut Harness<'_, VitascopeApp>, name: &str) {
    step_until(h, "開始播放、知道影片尺寸", |s| {
        playing(s, name) && s.video_size.is_some()
    });
    h.run_steps(5);
}

fn opened(file: PathBuf) -> Harness<'static, VitascopeApp> {
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let mut h = harness(Some(file));
    settle(&mut h, &name);
    h
}

/// 實際經過一段時間（播放器在背景播放），期間一直更新介面
fn wait_real(h: &mut Harness<'_, VitascopeApp>, seconds: f64) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs_f64(seconds) {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn plays_next_file_in_folder_and_page_keys_switch() {
    let dir = TempDir::new("playlist");
    for name in ["第1集.mp4", "第2集.mp4", "第10集.mp4", "第1集.srt", ".第3集.mp4"] {
        dir.clip(name);
    }
    let mut h = harness_with(Some(dir.0.join("第1集.mp4")), Settings::default());
    step_until(&mut h, "播放第1集", |s| playing(s, "第1集.mp4"));
    // 同資料夾的檔案在背景掃描；字幕檔、隱藏檔不算
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    // 播完自動接下一個；檔名照數字排（第2集在第10集前面）
    step_until(&mut h, "播完自動播放第2集", |s| playing(s, "第2集.mp4"));
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "PgDn → 第10集", |s| playing(s, "第10集.mp4"));
    h.key_press(egui::Key::PageUp);
    step_until(&mut h, "PgUp → 第2集", |s| playing(s, "第2集.mp4"));
    let list = h.state().playlist().unwrap();
    assert_eq!((list.position(), list.len()), (2, 3));
    // 控制列的「上一個」按鈕
    settle(&mut h, "第2集.mp4");
    h.get_by_label("⏮").click();
    step_until(&mut h, "⏮ → 第1集", |s| playing(s, "第1集.mp4"));
}

#[test]
fn auto_next_can_be_turned_off() {
    let dir = TempDir::new("no-auto-next");
    let a = dir.clip("a.mp4");
    dir.clip("b.mp4");
    let mut h = harness(Some(a)); // auto_next = false
    step_until_app(&mut h, "掃描到兩個檔案", |app| playlist_len(app) == 2);
    step_until(&mut h, "播完停在最後一格", |s| playing(s, "a.mp4") && s.eof);
    // 換檔是同步的（open 會立刻更新清單位置），這裡馬上就看得出來有沒有自動換檔
    assert_eq!(h.state().playlist().unwrap().position(), 1);
    wait_real(&mut h, 1.0);
    assert_eq!(h.state().playlist().unwrap().position(), 1);
    assert!(
        playing(&h.state().player().state, "a.mp4"),
        "關掉自動播放就停在這個檔案"
    );
}

#[test]
fn dropping_several_files_makes_a_sorted_playlist() {
    let dir = TempDir::new("dropped-list");
    let files = [dir.clip("第10集.mp4"), dir.clip("第2集.mp4")];
    let mut h = harness(None);
    h.step();
    // Windows 拖放時，滑鼠抓著的檔案會排在最前面；清單還是照檔名排
    for f in &files {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(f.clone())));
    }
    h.step();
    step_until(&mut h, "從第2集開始播", |s| playing(s, "第2集.mp4"));
    let list = h.state().playlist().unwrap();
    assert_eq!((list.position(), list.len()), (1, 2));
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "下一個是第10集", |s| playing(s, "第10集.mp4"));
}

#[test]
fn dropping_video_with_subtitle_opens_the_video() {
    let dir = TempDir::new("mixed-drop");
    let srt = dir.0.join("a.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:02,000\n測試\n").unwrap();
    let video = dir.clip("a.mp4");
    let mut h = harness(None);
    h.step();
    for f in [&srt, &video] {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(f.clone())));
    }
    h.step();
    step_until(&mut h, "打開影片（不是字幕檔）", |s| playing(s, "a.mp4"));
}

#[test]
fn holding_a_drag_at_the_end_does_not_run_through_the_playlist() {
    let dir = TempDir::new("drag-end");
    let a = dir.clip("a.mp4");
    dir.clip("b.mp4");
    dir.clip("c.mp4");
    let mut h = harness_with(Some(a), Settings::default());
    settle(&mut h, "a.mp4");
    step_until_app(&mut h, "掃描到三個檔案", |app| playlist_len(app) == 3);

    // 按住進度條、拖到最右邊，不放開
    let bar = h.get_by_label("進度").rect();
    let start = egui::pos2(bar.center().x, bar.center().y);
    let end = egui::pos2(bar.right() - 1.0, bar.center().y);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(egui::Event::PointerMoved(start));
    h.event(button(start, true));
    h.step();
    for i in 1..=10 {
        h.event(egui::Event::PointerMoved(start.lerp(end, i as f32 / 10.0)));
        h.step();
    }
    // 樣本只有開頭一個關鍵影格，拖曳中的快速跳轉會落在開頭，再自己播到結尾（3 秒）
    let held = Instant::now();
    while held.elapsed() < Duration::from_secs(4) {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        playing(&h.state().player().state, "a.mp4"),
        "還按著進度條時不換檔：{:?}",
        h.state().player().state.path
    );
    // 放開：播到結尾，接下一個；下一個檔案從頭播，不會被剛才的拖曳拉到結尾
    h.event(button(end, false));
    step_until(&mut h, "放開後播下一個", |s| playing(s, "b.mp4"));
    wait_real(&mut h, 1.0);
    let st = &h.state().player().state;
    assert!(
        playing(st, "b.mp4") && st.time_pos < 2.5,
        "{:?} {}",
        st.path,
        st.time_pos
    );
}

#[test]
fn resumes_where_it_left_off() {
    let long = sample("common/mp4_long.mp4"); // 90 秒
    let mut h = harness(Some(long.clone()));
    step_until(&mut h, "開始播放", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos > 0.0
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(&mut h, "前進 30 秒", |s| s.time_pos >= 29.0);

    // 換到別的檔案時記下看到哪裡
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "換檔", |s| playing(s, "mp4_h264_aac.mp4"));
    let history = h.state().history();
    let saved = history.positions.first().expect("應該記下續播位置");
    assert!(saved.path.ends_with("mp4_long.mp4") && saved.time >= 29.0, "{saved:?}");
    assert!(history.recent[0].ends_with("mp4_h264_aac.mp4"), "{:?}", history.recent);
    assert!(history.recent[1].ends_with("mp4_long.mp4"), "{:?}", history.recent);

    // 再開回來：從上次的位置繼續
    drop_file(&mut h, long);
    step_until(&mut h, "從上次的位置繼續", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos >= 28.0
    });
    // Home 從頭播放
    h.key_press(egui::Key::Home);
    step_until(&mut h, "Home 回到開頭", |s| s.time_pos < 5.0 && !s.paused);
}

#[test]
fn resume_can_be_turned_off() {
    let long = sample("common/mp4_long.mp4");
    let mut settings = Settings::default();
    settings.resume = false;
    settings.auto_next = false;
    let mut h = harness_with(Some(long.clone()), settings);
    step_until(&mut h, "開始播放", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos > 0.0
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(&mut h, "前進 30 秒", |s| s.time_pos >= 29.0);
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "換檔", |s| playing(s, "mp4_h264_aac.mp4"));
    drop_file(&mut h, long);
    step_until(&mut h, "再開回來", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos > 0.0
    });
    wait_real(&mut h, 0.5);
    let t = h.state().player().state.time_pos;
    assert!(t < 5.0, "關掉續播就從頭開始：{t}");
}

#[test]
fn stop_remembers_position_and_start_screen_resumes() {
    let mut h = opened(sample("common/mp4_long.mp4"));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(&mut h, "前進 30 秒", |s| s.time_pos >= 29.0);
    h.get_by_label("⏹").click();
    step_until(&mut h, "停止", |s| !s.loaded && !s.loading);
    h.run_steps(2);
    h.get_by_label_contains("最近開啟");
    h.get_by_label("mp4_long.mp4").click();
    step_until(&mut h, "從最近開啟的清單再開，接著上次的位置", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos >= 28.0
    });
}

#[test]
fn double_clicking_a_recent_file_just_opens_it() {
    let mut h = opened(sample("common/mp4_h264_aac.mp4"));
    h.get_by_label("⏹").click();
    step_until(&mut h, "停止", |s| !s.loaded && !s.loading);
    h.run_steps(2);
    // 起始畫面上雙擊：第一下打開檔案，第二下落在已經開始播放的畫面上，不應該變成暫停 + 全螢幕
    let pos = h.get_by_label("mp4_h264_aac.mp4").rect().center();
    let click = |h: &mut Harness<'_, VitascopeApp>| {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
    };
    // 自己控制 egui 的時間：兩下要在雙擊的時間內（0.3 秒），每一幀只前進 0.02 秒
    let mut now = h.ctx.input(|i| i.time) + 1.0;
    let mut step = |h: &mut Harness<'_, VitascopeApp>| {
        h.input_mut().time = Some(now);
        h.step();
        now += 0.02;
    };
    h.event(egui::Event::PointerMoved(pos));
    click(&mut h);
    step(&mut h);
    // 等 mpv 開始載入（起始畫面消失）
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(50));
        step(&mut h);
        if h.state().player().state.loading || h.state().player().state.loaded {
            break;
        }
    }
    assert!(h.query_by_label("mp4_h264_aac.mp4").is_none(), "起始畫面應該已經消失");
    click(&mut h);
    step(&mut h);
    let cmds = h
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default();
    assert!(
        !cmds.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))),
        "{cmds:?}"
    );
    step_until(&mut h, "播放中", |s| {
        playing(s, "mp4_h264_aac.mp4") && s.time_pos > 0.0
    });
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.paused, "第二下不應該暫停");
}

#[test]
fn clicking_to_close_the_context_menu_does_not_pause() {
    let mut h = playing_multitrack();
    let video = h.get_by_label("影片畫面").rect();
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("播放速度");
    // 點選單外面（畫面左上角）關掉選單
    let outside = video.left_top() + egui::vec2(20.0, 20.0);
    h.event(egui::Event::PointerMoved(outside));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    assert!(h.query_by_label_contains("播放速度").is_none(), "選單應該關掉");
    wait_real(&mut h, 0.4);
    assert!(!h.state().player().state.paused, "關選單的那一下不算暫停");
}

fn close_to(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn speed_keys() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::C);
    step_until(&mut h, "C 加快到 1.1×", |s| close_to(s.speed, 1.1));
    h.key_press(egui::Key::C);
    step_until(&mut h, "C 加快到 1.2×", |s| close_to(s.speed, 1.2));
    for _ in 0..3 {
        h.key_press(egui::Key::X);
    }
    step_until(&mut h, "X 按三次減慢到 0.9×", |s| close_to(s.speed, 0.9));
    h.key_press(egui::Key::Z);
    step_until(&mut h, "Z 恢復正常速度", |s| close_to(s.speed, 1.0));
    for _ in 0..40 {
        h.key_press(egui::Key::X);
    }
    step_until(&mut h, "最慢 0.25×", |s| close_to(s.speed, 0.25));
}

#[test]
fn context_menu_sets_speed() {
    let mut h = playing_multitrack();
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("播放速度").click();
    h.run_steps(2);
    h.get_by_label("2×").click();
    step_until(&mut h, "右鍵選單選 2×", |s| close_to(s.speed, 2.0));
}

#[test]
fn frame_step_keys() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    let t0 = h.state().player().state.time_pos;
    h.key_press(egui::Key::Period);
    step_until(&mut h, ". 逐格前進", |s| s.paused && s.time_pos > t0 + 0.01);
    let t1 = h.state().player().state.time_pos;
    assert!(t1 - t0 < 0.2, "只前進一格（24 fps 約 0.04 秒）：{t0} → {t1}");
    h.key_press(egui::Key::Comma);
    step_until(&mut h, ", 逐格後退", |s| s.paused && s.time_pos < t1 - 0.01);
}

#[test]
fn ab_loop_key_cycles_and_new_file_clears_it() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::L);
    step_until(&mut h, "L 設定起點", |s| {
        s.ab_loop[0].is_some() && s.ab_loop[1].is_none()
    });
    let a = h.state().player().state.ab_loop[0].unwrap();
    step_until(&mut h, "播放一秒", |s| s.time_pos > a + 1.0);
    h.key_press(egui::Key::L);
    step_until(&mut h, "L 設定終點", |s| s.ab_loop[1].is_some());
    let b = h.state().player().state.ab_loop[1].unwrap();
    // 播到終點會回到起點：等一段比區段還長的時間，時間都不會超出區段
    let start = Instant::now();
    let mut looped = false;
    let mut last = h.state().player().state.time_pos;
    while start.elapsed() < Duration::from_secs_f64(b - a + 1.5) {
        h.step();
        let t = h.state().player().state.time_pos;
        assert!(t <= b + 0.5, "時間 {t} 超過終點 {b}");
        looped |= t < last - 0.3;
        last = t;
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(looped, "應該從終點 {b} 跳回起點 {a}");
    h.key_press(egui::Key::L);
    step_until(&mut h, "L 第三次取消", |s| s.ab_loop == [None, None]);

    // mpv 換檔時會沿用 A-B 設定；開新檔要清掉
    h.key_press(egui::Key::L);
    step_until(&mut h, "再設一次起點", |s| s.ab_loop[0].is_some());
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "開新檔後取消 A-B 重播", |s| {
        playing(s, "mp4_h264_aac.mp4") && s.ab_loop == [None, None]
    });
}

#[test]
fn chapters_keys_and_menu() {
    // 章節在 0、4.037、8.041 秒（不對齊影格）：片頭、本篇、片尾
    let mut h = opened(sample("common/mkv_chapters.mkv"));
    step_until(&mut h, "讀到三個章節", |s| s.chapters.len() == 3);
    let titles: Vec<_> = h
        .state()
        .player()
        .state
        .chapters
        .iter()
        .map(|c| c.title.clone())
        .collect();
    assert_eq!(titles, [Some("片頭".into()), Some("本篇".into()), Some("片尾".into())]);
    // 暫停：時間不會自己往前走，下面的檢查才真的是跳章節的結果
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);

    let chapter_key = |h: &mut Harness<'_, VitascopeApp>, key| {
        h.key_press_modifiers(egui::Modifiers::COMMAND, key);
    };
    chapter_key(&mut h, egui::Key::PageDown);
    step_until(&mut h, "下一章：本篇", |s| {
        s.chapter == Some(1) && (3.9..4.6).contains(&s.time_pos)
    });
    // 跳過去之後畫面的時間比章節時間早一點點，下一章還是要能繼續往後
    chapter_key(&mut h, egui::Key::PageDown);
    step_until(&mut h, "下一章：片尾", |s| {
        s.chapter == Some(2) && (7.9..8.6).contains(&s.time_pos)
    });
    chapter_key(&mut h, egui::Key::PageDown);
    h.run_steps(5);
    assert_eq!(
        h.state().player().state.chapter,
        Some(2),
        "最後一章再往後不動（不會跳到片尾、換檔）"
    );
    // 才進入片尾沒多久：往回跳到上一章
    chapter_key(&mut h, egui::Key::PageUp);
    step_until(&mut h, "上一章：本篇", |s| {
        s.chapter == Some(1) && (3.9..4.6).contains(&s.time_pos)
    });

    // 右鍵選單 → 章節 → 片頭
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("章節").click();
    h.run_steps(2);
    h.get_by_label_contains("片頭").click();
    step_until(&mut h, "選單跳到片頭", |s| {
        s.chapter == Some(0) && s.time_pos < 1.0
    });
}

#[test]
fn wheel_over_video_changes_volume() {
    let mut h = playing_multitrack();
    step_until(&mut h, "音量 100", |s| s.volume == 100.0);
    h.get_by_label("影片畫面").hover();
    h.run_steps(1);
    let wheel = |y: f32| egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, y),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(wheel(-1.0));
    step_until(&mut h, "滾輪往下一格：音量 95", |s| s.volume == 95.0);
    h.event(wheel(-2.0));
    step_until(&mut h, "再兩格：音量 85", |s| s.volume == 85.0);
    h.event(wheel(1.0));
    step_until(&mut h, "往上一格：音量 90", |s| s.volume == 90.0);
    // 觸控板：連續的小量捲動，累積滿一格才算
    for _ in 0..4 {
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 15.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    step_until(&mut h, "觸控板 60 點：音量 95", |s| s.volume == 95.0);
}

// ───────────── L2：字幕與音訊 ─────────────

#[test]
fn delay_keys_and_menu() {
    let mut h = playing_multitrack();
    for _ in 0..3 {
        h.key_press(egui::Key::CloseBracket);
        h.step();
    }
    step_until(&mut h, "] 三次：字幕延遲 +0.3 秒", |s| {
        close_to(s.sub_delay, 0.3)
    });
    h.key_press(egui::Key::OpenBracket);
    step_until(&mut h, "[：字幕延遲 +0.2 秒", |s| close_to(s.sub_delay, 0.2));
    h.key_press(egui::Key::Equals);
    step_until(&mut h, "=：音訊延遲 +0.1 秒", |s| close_to(s.audio_delay, 0.1));
    h.key_press(egui::Key::Minus);
    h.step();
    h.key_press(egui::Key::Minus);
    step_until(&mut h, "- 兩次：音訊延遲 -0.1 秒", |s| {
        close_to(s.audio_delay, -0.1)
    });

    // 字幕選單 → 字幕延遲 → 歸零（子選單點了不會關，可以連按）
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label_contains("字幕延遲").click();
    h.run_steps(2);
    h.get_by_label("+0.1 秒").click();
    h.run_steps(2);
    step_until(&mut h, "選單 +0.1：+0.3 秒", |s| close_to(s.sub_delay, 0.3));
    h.get_by_label("歸零").click();
    step_until(&mut h, "選單歸零", |s| close_to(s.sub_delay, 0.0));
}

#[test]
fn secondary_subtitle_from_menu() {
    let mut h = playing_multitrack(); // 繁體中文（chi）+ English（eng）
    step_until(&mut h, "主字幕是繁中", |s| {
        s.selected(TrackKind::Sub).and_then(|t| t.lang.as_deref()) == Some("chi")
    });
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label_contains("第二字幕").click();
    h.run_steps(2);
    // 主字幕清單和第二字幕的子選單都有 English；子選單是後畫的那個
    h.query_all_by_label_contains("English").last().unwrap().click();
    step_until(&mut h, "第二字幕是英文、主字幕還是繁中", |s| {
        let eng = s
            .tracks_of(TrackKind::Sub)
            .find(|t| t.lang.as_deref() == Some("eng"))
            .map(|t| t.id);
        s.secondary_sid.is_some()
            && s.secondary_sid == eng
            && s.selected(TrackKind::Sub).and_then(|t| t.lang.as_deref()) == Some("chi")
    });
    // 換檔時關掉第二字幕（每個檔案的軌道編號不一樣）
    drop_file(&mut h, sample("common/mkv_h264_aac_srt.mkv"));
    step_until(&mut h, "換檔後沒有第二字幕", |s| {
        playing(s, "mkv_h264_aac_srt.mkv") && s.secondary_sid.is_none()
    });
}

/// 讀出某段字幕（`from`–`to` 秒）顯示時的文字：跳到開頭、播放，在這段時間內讀 mpv 的 sub-text。
/// 不能暫停後再讀：沒有畫面的測試模式下，暫停中跳轉不會更新字幕
fn subtitle_text_between(h: &mut Harness<'_, VitascopeApp>, from: f64, to: f64) -> String {
    for _ in 0..5 {
        h.state().player().set_pause(false).unwrap();
        h.state().player().seek_to(from, true).unwrap();
        let start = Instant::now();
        while start.elapsed() < TIMEOUT {
            h.step();
            let t = h.state().player().state.time_pos;
            if t > from + 0.15 && t < to - 0.15 {
                let text = h.state().player().get_string("sub-text").unwrap_or_default();
                if !text.is_empty() {
                    return text;
                }
            }
            if t >= to {
                break; // 錯過了，再跳一次
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    String::new()
}

#[test]
fn subtitle_encoding_can_be_chosen_by_hand() {
    // Big5 編碼的外掛字幕：自動判斷正確；手動改成 GBK 會變亂碼，再改回自動判斷又正常
    let mut h = opened(sample("common/extsub_srt_big5.mp4"));
    step_until(&mut h, "選上外掛字幕", |s| {
        s.selected(TrackKind::Sub).is_some_and(|t| t.external)
    });
    assert_eq!(subtitle_text_between(&mut h, 1.5, 2.6), "影戲播放器測試");

    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label_contains("字幕編碼").click();
    h.run_steps(2);
    h.get_by_label_contains("自動判斷（Big5）");
    h.get_by_label_contains("GB18030").click();
    h.run_steps(5);
    let garbled = subtitle_text_between(&mut h, 1.5, 2.6);
    assert!(!garbled.is_empty() && garbled != "影戲播放器測試", "{garbled}");
    assert_eq!(
        h.state().player().state.tracks_of(TrackKind::Sub).count(),
        1,
        "換編碼是取代原本那條字幕，不是多一條"
    );

    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label_contains("字幕編碼").click();
    h.run_steps(2);
    h.get_by_label_contains("自動判斷").click();
    h.run_steps(5);
    assert_eq!(subtitle_text_between(&mut h, 1.5, 2.6), "影戲播放器測試");
}

#[test]
fn same_name_external_audio_is_loaded_but_not_selected() {
    let mut h = opened(sample("common/mkv_extaudio.mkv")); // 旁邊有 mkv_extaudio.mka
    step_until(&mut h, "載入外掛音軌", |s| {
        s.tracks_of(TrackKind::Audio).count() == 2
    });
    let st = &h.state().player().state;
    let ext = st.tracks_of(TrackKind::Audio).find(|t| t.external).expect("外掛音軌");
    // .mka 裡的音軌沒有名稱：選單顯示檔名
    assert!(ext.label().contains("mkv_extaudio.mka"), "{}", ext.label());
    // 字幕組的外掛音軌常是另一種語言的配音：預設還是用影片內建的音軌
    assert!(!st.selected(TrackKind::Audio).unwrap().external);

    h.get_by_label("音軌").click();
    h.run_steps(2);
    h.get_by_label_contains("mkv_extaudio.mka").click();
    step_until(&mut h, "選單切換到外掛音軌", |s| {
        s.selected(TrackKind::Audio).is_some_and(|t| t.external)
    });
}

#[test]
fn audio_file_shows_cover_and_tags() {
    let mut h = opened(sample("general/audio_mp3_cover.mp3"));
    step_until(&mut h, "讀到標籤", |s| s.tag("artist") == Some("影戲樂團"));
    assert!(
        h.state().player().state.tracks_of(TrackKind::Video).any(|t| t.albumart),
        "封面是專輯封面，不是影片"
    );
    h.run_steps(2);
    h.get_by_label("測試歌曲");
    h.get_by_label("影戲樂團");
    h.get_by_label("範例專輯");
}

#[test]
fn audio_file_without_cover_shows_title() {
    let mut h = harness(Some(sample("general/audio_flac.flac")));
    step_until(&mut h, "播放", |s| playing(s, "audio_flac.flac"));
    h.run_steps(2);
    h.get_by_label_contains("audio_flac.flac");
}

#[test]
fn subtitle_style_window_applies_changes_and_typing_is_not_a_shortcut() {
    let mut h = playing_multitrack();
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label("字幕外觀…").click();
    h.run_steps(2);
    h.get_by_label("粗體").click();
    h.run_steps(2);
    assert!(h.state().settings().subtitle.bold);
    assert_eq!(h.state().player().get_string("sub-bold").unwrap(), "yes");

    // 在字型欄位輸入字型名稱：C 不能變成「加快速度」
    let field = h.get_by_role(egui::accesskit::Role::TextInput);
    field.focus();
    h.run_steps(2);
    h.key_press(egui::Key::C);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("Cambria");
    h.run_steps(3);
    assert_eq!(h.state().settings().subtitle.font, "Cambria");
    assert_eq!(h.state().player().get_string("sub-font").unwrap(), "Cambria");
    assert!(close_to(h.state().player().state.speed, 1.0), "輸入文字時快捷鍵要停用");

    h.get_by_label("恢復預設").click();
    h.run_steps(2);
    assert!(!h.state().settings().subtitle.bold);
    assert_eq!(h.state().player().get_string("sub-bold").unwrap(), "no");
}

#[test]
fn frame_stepping_to_the_end_does_not_switch_files() {
    // 沒有音軌的影片：mpv 逐格時會短暫取消暫停，以前到結尾會被當成「播完」而換到下一個檔案
    let dir = TempDir::new("frame-step-end");
    let src = sample("common/mp4_h264_noaudio.mp4");
    for name in ["a.mp4", "b.mp4"] {
        std::fs::copy(&src, dir.0.join(name)).unwrap();
    }
    let mut h = harness_with(Some(dir.0.join("a.mp4")), Settings::default());
    step_until(&mut h, "播放 a", |s| playing(s, "a.mp4") && s.time_pos > 0.0);
    step_until_app(&mut h, "掃描到兩個檔案", |app| playlist_len(app) == 2);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.state().player().seek_to(2.6, true).unwrap();
    step_until(&mut h, "跳到接近結尾", |s| s.time_pos > 2.5);
    for _ in 0..30 {
        h.key_press(egui::Key::Period);
        wait_real(&mut h, 0.05);
        if h.state().player().state.eof {
            break;
        }
    }
    step_until(&mut h, "逐格到結尾", |s| s.eof);
    wait_real(&mut h, 0.5);
    assert!(playing(&h.state().player().state, "a.mp4"), "暫停中逐格到結尾，不換檔");
    // 按播放之後就是一般播放：從頭播到結尾會接下一個
    h.key_press(egui::Key::Space);
    step_until(&mut h, "播完接下一個", |s| playing(s, "b.mp4"));
}

#[test]
fn dropping_video_with_subtitle_while_playing_opens_both() {
    let dir = TempDir::new("drop-video-sub");
    let video = dir.clip("新影片.mp4");
    let srt = dir.0.join("另一個字幕.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:03,000\n拖放的字幕\n").unwrap();
    let mut h = playing_multitrack();
    for f in [&srt, &video] {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(f.clone())));
    }
    h.step();
    step_until(
        &mut h,
        "打開拖進來的影片，並加上一起拖進來的字幕",
        |s| {
            playing(s, "新影片.mp4")
                && s.tracks_of(TrackKind::Sub)
                    .any(|t| t.external && t.title.as_deref().is_some_and(|title| title.contains("另一個字幕")))
        },
    );
}

#[test]
fn clicking_the_video_to_close_a_control_bar_menu_does_not_pause() {
    let mut h = playing_multitrack();
    let video = h.get_by_label("影片畫面").rect();
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label("關閉字幕");
    let outside = video.left_top() + egui::vec2(20.0, 20.0);
    h.event(egui::Event::PointerMoved(outside));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(3);
    assert!(h.query_by_label("關閉字幕").is_none(), "選單應該關掉");
    wait_real(&mut h, 0.4);
    assert!(!h.state().player().state.paused, "關選單的那一下不算暫停");
}

#[test]
fn dropping_a_season_gives_each_video_its_own_subtitle() {
    let dir = TempDir::new("drop-season");
    let mut files = Vec::new();
    for ep in ["ep1", "ep2", "ep10"] {
        files.push(dir.clip(&format!("{ep}.mp4")));
        let srt = dir.0.join(format!("{ep}.srt"));
        std::fs::write(&srt, format!("1\n00:00:00,000 --> 00:00:03,000\n{ep} 的字幕\n")).unwrap();
        files.push(srt);
    }
    let mut h = harness(None);
    h.step();
    for f in &files {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(f.clone())));
    }
    h.step();
    step_until(&mut h, "從 ep1 開始，只有自己的字幕", |s| {
        playing(s, "ep1.mp4") && s.tracks_of(TrackKind::Sub).count() >= 1 && s.selected(TrackKind::Sub).is_some()
    });
    h.run_steps(5);
    let st = &h.state().player().state;
    let subs: Vec<String> = st
        .tracks_of(TrackKind::Sub)
        .map(|t| t.external_filename.clone().unwrap_or_default())
        .collect();
    assert_eq!(subs.len(), 1, "ep1 不應該拿到 ep2、ep10 的字幕，也不能重複：{subs:?}");
    assert_eq!(h.state().playlist().unwrap().len(), 3);
}

#[test]
fn dropping_a_video_with_its_own_subtitle_does_not_duplicate_it() {
    let dir = TempDir::new("drop-own-sub");
    let video = dir.clip("影片.mp4");
    let srt = dir.0.join("影片.srt");
    std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:03,000\n字幕\n").unwrap();
    let mut h = harness(None);
    h.step();
    for f in [&video, &srt] {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(f.clone())));
    }
    h.step();
    step_until(&mut h, "播放並選上字幕", |s| {
        playing(s, "影片.mp4") && s.selected(TrackKind::Sub).is_some()
    });
    h.run_steps(5);
    assert_eq!(h.state().player().state.tracks_of(TrackKind::Sub).count(), 1);
    // 播放中再拖一次同一個字幕：選上原本那條，不會多一條
    drop_file(&mut h, srt);
    h.run_steps(5);
    assert_eq!(h.state().player().state.tracks_of(TrackKind::Sub).count(), 1);
}

#[test]
fn choosing_the_secondary_subtitle_as_primary_swaps_them() {
    let mut h = playing_multitrack(); // 繁體中文（chi）+ English（eng）
    let id_of = |s: &State, lang: &str| {
        s.tracks_of(TrackKind::Sub)
            .find(|t| t.lang.as_deref() == Some(lang))
            .map(|t| t.id)
    };
    step_until(&mut h, "主字幕是繁中", |s| {
        s.sid.is_some() && s.sid == id_of(s, "chi")
    });
    let eng = id_of(&h.state().player().state, "eng").unwrap();
    h.state().player().set_secondary_sub(Some(eng)).unwrap();
    step_until(&mut h, "第二字幕是英文", |s| s.secondary_sid == Some(eng));
    // 在主字幕清單選英文：英文變主字幕、繁中變第二字幕
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.query_all_by_label_contains("English").next().unwrap().click();
    step_until(&mut h, "兩條對調", |s| {
        s.sid == Some(eng) && s.secondary_sid == id_of(s, "chi")
    });
}

#[test]
fn video_without_audio_selects_same_name_external_audio() {
    let dir = TempDir::new("noaudio-mka");
    let video = dir.0.join("無聲.mp4");
    std::fs::copy(sample("common/mp4_h264_noaudio.mp4"), &video).unwrap();
    std::fs::copy(sample("common/mkv_extaudio.mka"), dir.0.join("無聲.mka")).unwrap();
    let mut h = harness(Some(video));
    step_until(&mut h, "自動選上外掛音軌（不然沒有聲音）", |s| {
        playing(s, "無聲.mp4") && s.selected(TrackKind::Audio).is_some_and(|t| t.external)
    });
}

#[test]
fn audio_files_do_not_load_other_audio_files_as_tracks() {
    let dir = TempDir::new("audio-siblings");
    let song = dir.0.join("歌.mp3");
    std::fs::copy(sample("general/audio_mp3.mp3"), &song).unwrap();
    std::fs::copy(sample("general/audio_flac.flac"), dir.0.join("歌.flac")).unwrap();
    let mut h = harness(Some(song));
    step_until(&mut h, "播放", |s| playing(s, "歌.mp3"));
    h.run_steps(5);
    assert_eq!(h.state().player().state.tracks_of(TrackKind::Audio).count(), 1);
}

#[test]
fn frame_step_on_audio_only_file_keeps_playing() {
    let mut h = harness(Some(sample("general/audio_flac.flac")));
    step_until(&mut h, "播放", |s| playing(s, "audio_flac.flac") && s.time_pos > 0.0);
    h.key_press(egui::Key::Period);
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.paused, "純音訊檔不能逐格，照常播放");
}

#[test]
fn escape_closes_the_subtitle_style_window() {
    let mut h = playing_multitrack();
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label("字幕外觀…").click();
    h.run_steps(2);
    h.get_by_label("恢復預設");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("恢復預設").is_none(), "Esc 關掉字幕外觀視窗");
}

// ───────────── L2：畫面 ─────────────

/// 這一幀送給視窗的指令
fn viewport_commands(h: &Harness<'_, VitascopeApp>) -> Vec<egui::ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// 在影片上按右鍵、點選單裡的項目。選單比視窗長時（小影片的視窗只有 421 高）先用滾輪捲到看得到
fn click_context_item(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    let screen = h.ctx.content_rect();
    for _ in 0..10 {
        let item = h.get_by_label_contains(label).rect();
        if item.bottom() <= screen.bottom() {
            break;
        }
        let over_menu = egui::pos2(item.center().x, screen.center().y);
        h.event(egui::Event::PointerMoved(over_menu));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        h.run_steps(2);
    }
    // 捲動時滑鼠可能停在有子選單的項目上（子選單會打開）：先移到要點的項目上，等子選單關掉
    h.get_by_label_contains(label).hover();
    h.run_steps(3);
    h.get_by_label_contains(label).click();
    h.step();
}

#[test]
fn always_on_top_from_context_menu_and_keys() {
    let mut h = playing_multitrack();
    click_context_item(&mut h, "視窗置頂");
    let cmds = viewport_commands(&h);
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)),
        "{cmds:?}"
    );
    assert!(h.state().settings().always_on_top);
    // Ctrl+T 切回一般
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::T,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    });
    h.step();
    let cmds = viewport_commands(&h);
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal)),
        "{cmds:?}"
    );
    assert!(!h.state().settings().always_on_top);
}

#[test]
fn leaving_fullscreen_restores_always_on_top() {
    let mut h = playing_multitrack();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(2);
    set_fullscreen(&mut h, true);
    // 離開全螢幕（macOS 會把置頂拿掉）：再設一次
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .fullscreen = Some(false);
    h.step();
    let cmds = viewport_commands(&h);
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)),
        "{cmds:?}"
    );
}

fn ratio(s: &State) -> f64 {
    s.video_size.map_or(0.0, |[w, h]| w as f64 / h as f64)
}

fn prop(h: &Harness<'_, VitascopeApp>, name: &str) -> String {
    h.state().player().get_string(name).unwrap_or_default()
}

#[test]
fn aspect_key_cycles_and_reset_restores() {
    let mut h = opened(sample("common/mp4_h264_aac.mp4")); // 320x240（4:3）
    assert!((ratio(&h.state().player().state) - 4.0 / 3.0).abs() < 0.01);
    h.key_press(egui::Key::A);
    step_until(&mut h, "A：16:9", |s| (ratio(s) - 16.0 / 9.0).abs() < 0.02);
    assert_eq!(h.state().geometry().aspect, Some(0));
    h.key_press(egui::Key::A);
    step_until(&mut h, "再按 A：4:3", |s| (ratio(s) - 4.0 / 3.0).abs() < 0.02);
    // 換成跟原本不一樣的比例，才看得出還原有沒有作用
    h.key_press(egui::Key::A);
    step_until(&mut h, "再按 A：16:10", |s| (ratio(s) - 1.6).abs() < 0.02);
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::Backspace);
    step_until(&mut h, "Alt+Backspace：回到原始比例", |s| {
        (ratio(s) - 4.0 / 3.0).abs() < 0.01
    });
    assert!(h.state().geometry().is_default());
    // 原始比例寫回 mpv 回報的預設值（0.37 是 -1、新版是 -2），不能是 0（像素當成正方形）
    let aspect: f64 = prop(&h, "video-aspect-override").parse().unwrap();
    assert!(aspect < 0.0, "{aspect}");
}

#[test]
fn crop_key_cuts_to_the_chosen_shape() {
    let mut h = playing_multitrack(); // 640x360（16:9）
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Q);
    h.run_steps(2);
    assert_eq!(
        h.state().geometry().crop,
        Some(0),
        "第一下：裁成 16:9（本來就是，不變）"
    );
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Q);
    step_until(&mut h, "第二下：裁成 4:3", |s| s.video_size == Some([480, 360]));
    assert_eq!(prop(&h, "video-crop"), "480x360+80+0");
}

#[test]
fn rotate_key_turns_the_picture_and_keeps_the_crop_shape() {
    let mut h = playing_multitrack(); // 640x360
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::K);
    step_until(&mut h, "Alt+K：轉 90°，變直的", |s| {
        s.video_size == Some([360, 640])
    });
    assert_eq!(h.state().geometry().rotate, 90);
    // 轉了之後再裁成 4:3：畫面上看起來是 4:3
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label("畫面 ⏵").click();
    h.run_steps(2);
    h.get_by_label_contains("裁切").click();
    h.run_steps(2);
    h.get_by_label("裁成 4:3").click();
    step_until(&mut h, "旋轉後裁成 4:3", |s| (ratio(s) - 4.0 / 3.0).abs() < 0.02);
}

#[test]
fn zoom_and_pan_keys() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::Num9);
    h.run_steps(2);
    assert_eq!(prop(&h, "video-zoom"), "0.100000");
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(prop(&h, "video-pan-x"), "0.050000");
    // Alt+→ 是移動畫面，不能同時被當成「前進 5 秒」
    let t = h.state().player().state.time_pos;
    assert!(t < 4.0, "沒有跳轉：{t}");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num5);
    h.run_steps(2);
    assert_eq!(prop(&h, "video-pan-x"), "0.000000");
    h.key_press(egui::Key::Num5);
    h.run_steps(2);
    assert_eq!(prop(&h, "video-zoom"), "0.000000");
}

#[test]
fn flip_keys_toggle_the_flip_shader() {
    let mut h = playing_multitrack();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert!(
        prop(&h, "glsl-shaders").contains("hflip.glsl"),
        "{}",
        prop(&h, "glsl-shaders")
    );
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::P);
    h.run_steps(2);
    assert!(prop(&h, "glsl-shaders").contains("vflip.glsl"));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert!(!prop(&h, "glsl-shaders").contains("hflip.glsl"), "再按一次取消");
}

#[test]
fn opening_another_file_resets_the_view() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::A);
    h.key_press(egui::Key::Num9);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(3);
    assert!(!h.state().geometry().is_default());
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "換檔", |s| {
        playing(s, "mp4_h264_aac.mp4") && s.video_size.is_some()
    });
    h.run_steps(3);
    assert!(h.state().geometry().is_default());
    assert_eq!(prop(&h, "video-zoom"), "0.000000");
    assert!(prop(&h, "glsl-shaders").is_empty(), "{}", prop(&h, "glsl-shaders"));
    step_until(&mut h, "新檔案是原本的 4:3", |s| {
        (ratio(s) - 4.0 / 3.0).abs() < 0.01
    });
    let aspect: f64 = prop(&h, "video-aspect-override").parse().unwrap();
    assert!(aspect < 0.0, "長寬比回到原始比例：{aspect}");
}

#[test]
fn view_menu_is_disabled_for_audio_only_files() {
    let mut h = harness(Some(sample("general/audio_flac.flac")));
    step_until(&mut h, "播放", |s| playing(s, "audio_flac.flac"));
    h.run_steps(2);
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    let view = h.get_by_label("畫面 ⏵");
    assert!(view.accesskit_node().is_disabled(), "純音訊檔沒有畫面可以調整");
    // 快捷鍵也一樣（跟選單一致）
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::A);
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Q);
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::K);
    h.key_press(egui::Key::Num9);
    h.run_steps(2);
    assert!(h.state().geometry().is_default(), "{:?}", h.state().geometry());
}

/// 檔案一載入就按畫面鍵（放大、裁切兩下）：裁切只套用一次，不會一直變來變去。
/// 審查時用探針重現過「畫面設定好之前就按」的情況（記不到原本的形狀，裁切來回跳）；
/// headless 的 mpv 通常同一幀就把畫面設定好，這裡多半是測正常的順序
#[test]
fn shape_keys_right_after_loading_apply_once() {
    let mut h = harness(Some(sample("common/mp4_hevc10_4k.mp4"))); // 3840x2160
    let start = Instant::now();
    while !h.state().player().state.loaded {
        h.step();
        assert!(start.elapsed() < TIMEOUT, "等待逾時：載入");
    }
    h.key_press(egui::Key::Num9);
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Q);
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Q);
    step_until(&mut h, "裁成 4:3", |s| s.video_size == Some([2880, 2160]));
    let crop = prop(&h, "video-crop");
    wait_real(&mut h, 0.6);
    assert_eq!(prop(&h, "video-crop"), crop, "裁切不會一直變來變去");
    assert_eq!(h.state().player().state.video_size, Some([2880, 2160]));
}

#[test]
fn window_height_follows_rotation() {
    let mut h = playing_multitrack(); // 640x360
    let width = h.ctx.content_rect().width();
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::K);
    let mut sizes = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        h.step();
        sizes.extend(viewport_commands(&h).into_iter().filter_map(|c| match c {
            egui::ViewportCommand::InnerSize(s) => Some(s),
            _ => None,
        }));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(h.state().player().state.video_size, Some([360, 640]));
    // 最後一次調整：寬度不變，高度配合直的畫面
    let last = sizes.last().copied().expect("視窗有調整");
    assert_eq!(last.x, width, "{sizes:?}");
    assert!(last.y > width * 640.0 / 360.0, "{sizes:?}");
}

// ───────────── 播放清單面板 ─────────────

fn playlist_names(app: &VitascopeApp) -> Vec<String> {
    app.playlist()
        .map(|l| {
            l.items()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// 開啟資料夾裡的第一集、等清單掃描完、按 F6 打開面板
fn playlist_panel_open(dir: &TempDir) -> Harness<'static, VitascopeApp> {
    let mut h = harness(Some(dir.0.join("第1集.mp4")));
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    h
}

fn three_episodes(name: &str) -> TempDir {
    let dir = TempDir::new(name);
    for name in ["第1集.mp4", "第2集.mp4", "第3集.mp4"] {
        dir.clip(name);
    }
    dir
}

fn left_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// 在 `pos` 雙擊：自己控制 egui 的時間，兩下在雙擊的時間內（預設每一幀前進 0.25 秒，太久）
fn double_click(h: &mut Harness<'_, VitascopeApp>, pos: egui::Pos2) {
    let mut now = h.ctx.input(|i| i.time) + 1.0;
    h.event(egui::Event::PointerMoved(pos));
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(left_button(pos, pressed));
        }
        h.input_mut().time = Some(now);
        h.step();
        now += 0.05;
    }
}

#[test]
fn f6_shows_the_playlist_and_double_click_plays_an_item() {
    let dir = three_episodes("panel");
    let mut h = harness(Some(dir.0.join("第1集.mp4")));
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    let width_of = |cmds: &[egui::ViewportCommand]| {
        cmds.iter().find_map(|c| match c {
            egui::ViewportCommand::InnerSize(s) => Some(s.x),
            _ => None,
        })
    };
    // 一般視窗：打開清單時視窗變寬（預設 280），影片大小不變；關掉時變回來
    let before = h.ctx.content_rect().width();
    let cmds = press_and_get_commands(&mut h, egui::Key::F6);
    assert_eq!(width_of(&cmds), Some(before + 280.0), "{cmds:?}");
    h.run_steps(2);
    assert!(h.state().settings().show_playlist);
    h.get_by_label("播放清單（1/3）");
    let cmds = press_and_get_commands(&mut h, egui::Key::F6);
    let narrowed = width_of(&cmds).expect("關閉時視窗變窄");
    assert!((narrowed - before).abs() < 2.0, "{cmds:?}");
    h.run_steps(2);
    assert!(h.query_by_label("播放清單（1/3）").is_none(), "再按一次 F6 關閉");
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    // 雙擊第 2 項：播放它
    let pos = h.get_by_label("2. 第2集.mp4").rect().center();
    double_click(&mut h, pos);
    step_until(&mut h, "雙擊 → 播放第2集", |s| playing(s, "第2集.mp4"));
    assert_eq!(h.state().playlist().unwrap().position(), 2);
}

#[test]
fn closing_a_playlist_that_was_open_at_start_shrinks_the_window() {
    // 上次關閉時清單開著：開檔時視窗配合影片 + 清單；關掉清單要縮回只有影片的寬度
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.show_playlist = true;
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings); // 640×360
    settle(&mut h, "mkv_multitrack.mkv");
    h.run_steps(5);
    let with_list = h.ctx.content_rect().width();
    assert!(with_list > 700.0, "影片 640 + 清單：{with_list}");
    h.key_press(egui::Key::F6);
    h.run_steps(5);
    let without = h.ctx.content_rect().width();
    assert!(
        (without - 640.0).abs() < 2.0,
        "關掉清單後縮回影片的寬度：{with_list} → {without}"
    );
}

#[test]
fn relative_paths_on_the_command_line_keep_their_playlist() {
    // 在終端機的影片資料夾裡執行 `vitascope a.mkv b.webm`：清單就是這兩個（不是整個資料夾）
    let rel = |name: &str| PathBuf::from("samples/generated/common").join(name);
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_launch(
        Launch {
            files: vec![rel("webm_vp9p2_opus.webm"), rel("mkv_multitrack.mkv")],
            ..Default::default()
        },
        settings,
    );
    settle(&mut h, "mkv_multitrack.mkv");
    h.run_steps(5);
    assert_eq!(
        playlist_names(h.state()),
        ["mkv_multitrack.mkv", "webm_vp9p2_opus.webm"]
    );
    let items = h.state().playlist().unwrap().items().to_vec();
    assert!(items.iter().all(|p| p.is_absolute()), "{items:?}");
}

#[test]
fn delete_removes_the_selected_item_but_keeps_playing() {
    let dir = three_episodes("panel-delete");
    let mut h = playlist_panel_open(&dir);
    // 單擊只是選取，不換檔
    h.get_by_label("1. 第1集.mp4").click();
    h.run_steps(2);
    assert!(playing(&h.state().player().state, "第1集.mp4"));
    assert!(!h.state().owns_session(), "開檔時的清單是掃描資料夾來的，不存");
    // 刪掉正在播的第1集：繼續播，下一個是第2集
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(playlist_names(h.state()), ["第2集.mp4", "第3集.mp4"]);
    assert!(h.state().owns_session(), "用 Delete 刪過：關閉時要存起來");
    assert!(playing(&h.state().player().state, "第1集.mp4"), "正在播的不受影響");
    h.get_by_label("播放清單（2）");
    // 選取移到下一項，可以連按
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(playlist_names(h.state()), ["第3集.mp4"]);
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "下一個是第3集", |s| playing(s, "第3集.mp4"));
}

#[test]
fn dragging_an_item_reorders_the_playlist() {
    let dir = three_episodes("panel-drag");
    let mut h = playlist_panel_open(&dir);
    let from = h.get_by_label("3. 第3集.mp4").rect().center();
    let first = h.get_by_label("1. 第1集.mp4").rect();
    // 拖到第 1 項的上半部 = 放在最前面
    let to = egui::pos2(first.center().x, first.top() + 2.0);
    h.event(egui::Event::PointerMoved(from));
    h.event(left_button(from, true));
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
        h.step();
    }
    h.event(left_button(to, false));
    h.run_steps(2);
    assert_eq!(playlist_names(h.state()), ["第3集.mp4", "第1集.mp4", "第2集.mp4"]);
    // 正在播的還是第1集，下一個照新的順序
    let list = h.state().playlist().unwrap();
    assert_eq!(list.position(), 2);
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "下一個是第2集", |s| playing(s, "第2集.mp4"));
}

#[test]
fn opening_an_m3u8_plays_its_list_in_order() {
    let dir = three_episodes("m3u");
    let list = dir.0.join("清單.m3u8");
    // 相對路徑、自己（清單檔不展開）、不存在的註解行
    std::fs::write(
        &list,
        "#EXTM3U\n#EXTINF:3,第三集\n第3集.mp4\n# 註解\n清單.m3u8\n第1集.mp4\n",
    )
    .unwrap();
    let mut h = harness(None);
    h.step();
    drop_file(&mut h, list);
    step_until(&mut h, "從清單的第一個（第3集）開始", |s| {
        playing(s, "第3集.mp4")
    });
    assert_eq!(playlist_names(h.state()), ["第3集.mp4", "第1集.mp4"]);
    // 不會被背景掃描的同資料夾清單蓋掉
    wait_real(&mut h, 0.5);
    assert_eq!(playlist_len(h.state()), 2);
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "下一個是第1集", |s| playing(s, "第1集.mp4"));
}

// ───────────── 媒體資訊 ─────────────

#[test]
fn media_info_panel_toggles_and_copies() {
    let mut h = playing_multitrack();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    h.get_by_label_contains("H.264");
    h.get_by_label_contains("640×360（16:9）");
    // 面板不接收滑鼠：點畫面照樣暫停
    h.get_by_label("影片畫面").click();
    step_until(&mut h, "點畫面暫停", |s| s.paused);
    // Esc 先關面板（不是離開全螢幕之類的）
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.query_by_label_contains("640×360（16:9）").is_none(),
        "Esc 關閉媒體資訊"
    );
    // Ctrl+F1 也可以打開（macOS 是 Cmd+F1，跟 F1「關於」分開）
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F1);
    h.run_steps(2);
    h.get_by_label_contains("640×360（16:9）");
    assert!(
        h.query_by_label("acer1204/VitaScope").is_none(),
        "Ctrl+F1 不是 F1（不開「關於」）"
    );
    // 右鍵選單「複製媒體資訊」（選單比視窗長，要捲動才點得到）：純文字放到剪貼簿
    click_context_item(&mut h, "複製媒體資訊");
    let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    let copied = copied.expect("有複製到剪貼簿");
    assert!(
        copied.contains("影像") && copied.contains("H.264") && copied.contains("mkv_multitrack.mkv"),
        "{copied}"
    );
}

#[test]
fn dropping_files_while_the_playlist_is_open_appends_them() {
    let dir = three_episodes("panel-drop");
    let extra = TempDir::new("panel-drop-extra");
    let more = [extra.clip("番外1.mp4"), extra.clip("番外2.mp4")];
    let mut h = playlist_panel_open(&dir);
    for f in &more {
        h.input_mut()
            .dropped_files
            .push(std::sync::Arc::new(Dropped(f.clone())));
    }
    h.step();
    h.run_steps(2);
    assert_eq!(
        playlist_names(h.state()),
        ["第1集.mp4", "第2集.mp4", "第3集.mp4", "番外1.mp4", "番外2.mp4"]
    );
    assert!(playing(&h.state().player().state, "第1集.mp4"), "照樣播原本的");
}

#[test]
fn dragging_below_the_last_row_moves_to_the_end() {
    let dir = three_episodes("panel-drag-end");
    let mut h = playlist_panel_open(&dir);
    let from = h.get_by_label("1. 第1集.mp4").rect().center();
    let last = h.get_by_label("3. 第3集.mp4").rect();
    let to = egui::pos2(last.center().x, last.bottom() + 30.0);
    h.event(egui::Event::PointerMoved(from));
    h.event(left_button(from, true));
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
        h.step();
    }
    h.event(left_button(to, false));
    h.run_steps(2);
    assert_eq!(playlist_names(h.state()), ["第2集.mp4", "第3集.mp4", "第1集.mp4"]);
}

#[test]
fn hls_m3u8_is_played_as_one_stream() {
    let dir = three_episodes("hls");
    let hls = dir.0.join("live.m3u8");
    std::fs::write(
        &hls,
        "#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:3
#EXTINF:3.0,
第1集.mp4
#EXTINF:3.0,
第2集.mp4
#EXT-X-ENDLIST
",
    )
    .unwrap();
    let mut h = harness(None);
    h.step();
    drop_file(&mut h, hls);
    // 不展開成片段：清單只有串流本身
    step_until_app(&mut h, "清單是 live.m3u8", |app| {
        playlist_names(app) == ["live.m3u8"]
    });
    wait_real(&mut h, 0.3);
    assert_eq!(playlist_names(h.state()), ["live.m3u8"]);
}

#[test]
fn restored_playlist_waits_and_page_down_plays_the_saved_item() {
    let dir = three_episodes("restored");
    let items: Vec<PathBuf> = ["第1集.mp4", "第2集.mp4", "第3集.mp4"]
        .iter()
        .map(|n| dir.0.join(n))
        .collect();
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.show_playlist = true;
    let mut h = harness_launch(
        Launch {
            playlist: Some(vitascope::playlist::Playlist::restored(items, Some(1))),
            ..Default::default()
        },
        settings,
    );
    h.run_steps(3);
    // 清單還在，但不自動播
    h.get_by_label("播放清單（3）");
    h.get_by_label("2. 第2集.mp4");
    assert!(!h.state().player().state.loaded && !h.state().player().state.loading);
    // PgDn：從上次播的那一項開始
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "播放第2集", |s| playing(s, "第2集.mp4"));
    assert_eq!(playlist_names(h.state()).len(), 3, "清單不會被換掉");
}

// ───────────── 擷取畫面 ─────────────

fn pngs_in(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|e| {
            e.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "png"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// 等到截圖資料夾裡有 `n` 張圖、而且都寫完了，回傳第一張的大小
fn wait_for_png(h: &mut Harness<'_, VitascopeApp>, dir: &std::path::Path, n: usize) -> (usize, usize) {
    step_until_app(h, "截圖存好", |_| pngs_in(dir).len() >= n);
    // 檔案出現之後還要等寫完（先建立檔案再寫入）
    let start = Instant::now();
    loop {
        h.step();
        let decoded: Vec<_> = pngs_in(dir)
            .iter()
            .filter_map(|p| vitascope::screenshot::decode_png(p).ok())
            .collect();
        if decoded.len() >= n {
            return (decoded[0].w, decoded[0].h);
        }
        assert!(start.elapsed() < TIMEOUT, "截圖讀不出來");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn harness_with_shot_dir(file: PathBuf, dir: &TempDir) -> Harness<'static, VitascopeApp> {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.screenshot_dir = Some(dir.0.clone());
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let mut h = harness_with(Some(file), settings);
    settle(&mut h, &name);
    h
}

#[test]
fn ctrl_e_saves_a_screenshot_at_original_size() {
    let dir = TempDir::new("shots");
    let mut h = harness_with_shot_dir(sample("common/mkv_multitrack.mkv"), &dir); // 640x360
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    assert_eq!(wait_for_png(&mut h, &dir.0, 1), (640, 360));
    let name = pngs_in(&dir.0)[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("mkv_multitrack 00.00."), "{name}");
    // 同一個時間再截一次：不覆蓋，加上 (2)
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    wait_for_png(&mut h, &dir.0, 2);
    assert!(
        pngs_in(&dir.0)
            .iter()
            .any(|p| p.to_string_lossy().ends_with(" (2).png"))
    );
}

#[test]
fn rapid_screenshots_of_the_same_frame_get_different_names() {
    let dir = TempDir::new("shots-burst");
    let mut h = harness_with_shot_dir(sample("common/mkv_multitrack.mkv"), &dir);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    // 連按：第一張還沒寫好（檔案還不存在）時就要第二張
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    wait_for_png(&mut h, &dir.0, 3);
    assert_eq!(pngs_in(&dir.0).len(), 3);
}

#[test]
fn screenshots_follow_rotation() {
    let dir = TempDir::new("shots-rotated");
    let mut h = harness_with_shot_dir(sample("common/mkv_multitrack.mkv"), &dir);
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::K);
    step_until(&mut h, "轉 90°", |s| s.video_size == Some([360, 640]));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    // 視窗裡由畫面輸出旋轉，mpv 的截圖不含旋轉，要自己轉正；這裡（沒有畫面）是 mpv 用濾鏡先轉好的
    assert_eq!(wait_for_png(&mut h, &dir.0, 1), (360, 640));
}

#[test]
fn ctrl_c_copies_the_frame() {
    let mut h = playing_multitrack();
    h.event(egui::Event::Copy);
    let start = Instant::now();
    let size = loop {
        h.step();
        let found = h.output().platform_output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::CopyImage(img) => Some(img.size),
            _ => None,
        });
        if let Some(size) = found {
            break size;
        }
        assert!(start.elapsed() < TIMEOUT, "沒有複製到剪貼簿");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(size, [640, 360]);
}

#[test]
fn audio_only_files_have_nothing_to_capture() {
    let dir = TempDir::new("shots-audio");
    let mut settings = Settings::default();
    settings.screenshot_dir = Some(dir.0.clone());
    let mut h = harness_with(Some(sample("general/audio_flac.flac")), settings);
    step_until(&mut h, "播放", |s| playing(s, "audio_flac.flac"));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    wait_real(&mut h, 0.5);
    assert!(pngs_in(&dir.0).is_empty());
}

#[test]
fn screenshots_follow_the_flip() {
    let dir = TempDir::new("shots-flip");
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.screenshot_dir = Some(dir.0.clone());
    settings.screenshot_subtitles = false;
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
    settle(&mut h, "mkv_multitrack.mkv");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    wait_for_png(&mut h, &dir.0, 1);
    // 左右翻轉（著色器，mpv 的截圖不含）之後再截一張：要是第一張的鏡像
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::E);
    wait_for_png(&mut h, &dir.0, 2);
    // 同一個時間：第二張的檔名多了「 (2)」
    let shots = pngs_in(&dir.0);
    let second = shots
        .iter()
        .find(|p| p.to_string_lossy().ends_with(" (2).png"))
        .unwrap();
    let first = shots.iter().find(|p| *p != second).unwrap();
    let a = vitascope::screenshot::decode_png(first).unwrap();
    let b = vitascope::screenshot::decode_png(second).unwrap();
    assert_eq!((a.w, a.h), (b.w, b.h));
    let mirrored = a.clone().fixed(vitascope::screenshot::Fixup {
        hflip: true,
        ..Default::default()
    });
    assert!(mirrored == b, "第二張是第一張左右翻轉");
    assert!(a != b, "畫面不是左右對稱的（不然測不出來）");
}

// ───────────── 進度條預覽縮圖 ─────────────

#[test]
fn hovering_the_progress_bar_shows_a_thumbnail() {
    let mut h = opened(sample("common/mp4_long.mp4")); // 90 秒
    let bar = h.get_by_label("進度").rect();
    let at = |frac: f32| egui::pos2(bar.left() + bar.width() * frac, bar.center().y);
    h.event(egui::Event::PointerMoved(at(0.5)));
    step_until_app(&mut h, "顯示 45 秒附近的縮圖", |app| {
        app.preview_shown()
            .is_some_and(|(b, size)| b == 45 && size == [240, 135])
    });
    // 移到別的地方：換成那裡的縮圖
    h.event(egui::Event::PointerMoved(at(0.1)));
    step_until_app(&mut h, "顯示 9 秒附近的縮圖", |app| {
        app.preview_shown().is_some_and(|(b, _)| b == 9)
    });
}

#[test]
fn audio_files_have_no_thumbnails() {
    let mut h = opened(sample("general/audio_mp3_cover.mp3"));
    let bar = h.get_by_label("進度").rect();
    h.event(egui::Event::PointerMoved(bar.center()));
    wait_real(&mut h, 0.5);
    assert!(h.state().preview_shown().is_none());
}

#[test]
fn opening_the_playlist_scrolls_to_the_current_file() {
    // 樣本資料夾裡有 30 幾個影片，最後一個在清單的最下面（一開始看不到）
    let mut h = opened(sample("common/webm_vp9p2_opus.webm"));
    step_until_app(&mut h, "掃描完資料夾", |app| playlist_len(app) > 25);
    h.key_press(egui::Key::F6);
    h.run_steps(4);
    let len = playlist_len(h.state());
    h.get_by_label(&format!("{len}. webm_vp9p2_opus.webm"));
}

// ───────────── 設定視窗、介面語言 ─────────────

#[test]
fn f5_opens_settings_and_escape_closes_them() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("一般");
    h.get_by_label("快捷鍵").click();
    h.run_steps(2);
    h.get_by_label("擷取畫面（存檔）");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("快捷鍵").is_none(), "Esc 關閉設定視窗");
}

#[test]
fn switching_the_language_to_english_updates_the_whole_ui() {
    let mut h = harness(None);
    h.step();
    h.get_by_label_contains("拖放到這裡");
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    // 下拉選單的值是目前的語言
    h.get_by_value("繁體中文").click();
    h.run_steps(2);
    h.get_by_label("English").click();
    h.run_steps(3);
    assert_eq!(h.state().settings().language, vitascope::i18n::Lang::En);
    h.get_by_label("General");
    h.get_by_label("Playback");
    h.get_by_label_contains("Drop a video here");
    assert!(h.query_by_label_contains("拖放到這裡").is_none());
    // 換回中文
    h.get_by_value("English").click();
    h.run_steps(2);
    h.get_by_label("繁體中文").click();
    h.run_steps(3);
    h.get_by_label_contains("拖放到這裡");
}

#[test]
fn media_info_follows_a_language_switch() {
    let mut h = playing_multitrack();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    h.get_by_label_contains("預設裝置");
    // 面板開著時換成英文：馬上換（不用等下一次每秒更新，也不用關掉再打開）
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_value("繁體中文").click();
    h.run_steps(2);
    h.get_by_label("English").click();
    h.run_steps(2);
    h.get_by_label_contains("Default device");
    assert!(h.query_by_label_contains("預設裝置").is_none());
}

#[test]
fn english_menus_and_messages() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.language = vitascope::i18n::Lang::En;
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
    settle(&mut h, "mkv_multitrack.mkv");
    h.get_by_label("Subtitles");
    h.get_by_label("Audio");
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("Open file");
    h.get_by_label_contains("Speed (1×)");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 媒體資訊也是英文
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    h.get_by_label_contains("640×360 (16:9)");
    // 「Video」：影片畫面本身和媒體資訊的「影像」
    assert_eq!(h.query_all_by_label("Video").count(), 2);
}

#[test]
fn a_tall_window_is_moved_up_so_the_controls_stay_on_screen() {
    // 實際遇到的：高 DPI 筆電（1707×960 點），系統把新視窗放在 (171, 171)；直式影片的視窗配合影片之後
    // 下緣超出螢幕，控制列被工作列蓋住
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mov_hevc_aac_rot90.mov")), settings);
    if let Some(v) = h.input_mut().viewports.get_mut(&egui::ViewportId::ROOT) {
        v.monitor_size = Some(egui::vec2(1707.0, 960.0));
        v.outer_rect = Some(egui::Rect::from_min_size(
            egui::pos2(171.0, 171.0),
            egui::vec2(972.0, 635.0),
        ));
        v.inner_rect = Some(egui::Rect::from_min_size(
            egui::pos2(177.0, 206.0),
            egui::vec2(960.0, 600.0),
        ));
    }
    let mut size = None;
    let mut moved = None;
    let start = Instant::now();
    while moved.is_none() && start.elapsed() < TIMEOUT {
        h.step();
        for c in h
            .output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
        {
            match c {
                egui::ViewportCommand::InnerSize(s) => size = Some(s),
                egui::ViewportCommand::OuterPosition(p) => moved = Some(p),
                _ => {}
            }
        }
    }
    let (size, moved) = (size.expect("視窗配合影片"), moved.expect("超出螢幕時要移回來"));
    // 新的外框（加上標題列、邊框）要在螢幕裡，下面留給工作列
    let bottom = moved.y + size.y + (635.0 - 600.0);
    assert!(
        bottom <= 960.0 - 48.0 + 0.5,
        "下緣 {bottom} 不能被工作列蓋住（視窗高 {}）",
        size.y
    );
    assert_eq!(moved.x, 171.0, "左右放得下就不動");
}

#[test]
fn english_controls_fit_a_minimum_width_window() {
    // 直式、一小時以上的影片：視窗是最小寬度，英文的按鈕比較寬、時間比較長
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.language = vitascope::i18n::Lang::En;
    let long = PathBuf::from("av://lavfi:testsrc2=size=240x320:rate=5:duration=4000");
    let mut h = harness_with(Some(long), settings);
    step_until(&mut h, "開始播放", |s| s.loaded && s.video_size.is_some());
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.run_steps(5);
    let width = h.ctx.content_rect().width();
    assert!(
        width <= vitascope::app::MIN_WINDOW_WIDTH + 1.0,
        "視窗是最小寬度：{width}"
    );
    let mute = h.get_by_label("🔊").rect();
    let time = h.get_by_label_contains("00:0").rect();
    assert!(
        time.right() <= mute.left(),
        "時間 {time:?} 不能被右邊的按鈕 {mute:?} 蓋到"
    );
}

#[test]
fn seek_step_comes_from_the_settings() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    // 樣本的關鍵影格很稀疏，跳轉會落在附近的關鍵影格：用 40 秒才分得出跟預設的 5 秒不一樣
    settings.seek_short = 40.0;
    let mut h = harness_with(Some(sample("common/mp4_long.mp4")), settings); // 90 秒
    settle(&mut h, "mp4_long.mp4");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    let t0 = h.state().player().state.time_pos;
    h.key_press(egui::Key::ArrowRight);
    step_until(&mut h, "前進 40 秒", |s| s.time_pos >= t0 + 30.0);
}

#[test]
fn hardware_decoding_can_be_switched_in_the_settings() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("播放").click();
    h.run_steps(2);
    // 預設開著：關掉、再打開
    h.get_by_label("硬體解碼").click();
    h.run_steps(2);
    assert!(!h.state().settings().hwdec);
    assert_eq!(prop(&h, "hwdec"), "no");
    h.get_by_label("硬體解碼").click();
    h.run_steps(2);
    assert!(h.state().settings().hwdec);
    assert_eq!(prop(&h, "hwdec"), "auto-safe");
}

#[test]
fn single_window_can_be_switched_off_on_the_system_page() {
    let mut h = harness(None);
    h.step();
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("系統").click();
    h.run_steps(2);
    // 檔案關聯的選項會寫登錄檔，測試不按；只確認它在
    #[cfg(windows)]
    h.get_by_label_contains("開啟檔案」選單");
    assert!(h.state().settings().single_instance);
    h.get_by_label("只開一個視窗").click();
    h.run_steps(2);
    assert!(!h.state().settings().single_instance);
}

// ───────────── 單一執行個體 ─────────────

#[test]
fn files_sent_by_another_launch_open_in_this_window() {
    let dir = TempDir::new("instance");
    let a = dir.clip("第1集.mp4");
    let b = dir.clip("第2集.mp4");
    let c = dir.clip("第3集.mp4");
    let suffix = format!("-ui{}", std::process::id());
    // socket 路徑有長度上限（macOS 104 位元組），macOS 的暫存資料夾本身就很長：放在短名稱的資料夾
    let run = TempDir(std::env::temp_dir().join(format!("vts-ui-{}", std::process::id())));
    let ep = vitascope::instance::Endpoint::in_dir(run.0.clone(), &suffix).unwrap();
    let primary = match vitascope::instance::start(
        &ep,
        &vitascope::instance::Request::default(),
        true,
        std::sync::Arc::new(|| {}),
    ) {
        vitascope::instance::Startup::Primary(p) => p,
        _ => panic!("應該是主視窗"),
    };
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_launch(
        Launch {
            files: vec![a.clone()],
            instance: Some(primary),
            ..Default::default()
        },
        settings,
    );
    step_until(
        &mut h,
        "自己的檔案（等一小段合併期間後）開始播",
        |s| playing(s, "第1集.mp4"),
    );
    // 另一個程式（例如在檔案總管選了兩個檔案按 Enter）：送過來，這個視窗開成清單
    for f in [&c, &b] {
        let req = vitascope::instance::Request {
            paths: vec![f.clone()],
            fullscreen: false,
        };
        let sent = vitascope::instance::start(&ep, &req, true, std::sync::Arc::new(|| {}));
        assert!(matches!(sent, vitascope::instance::Startup::Forwarded));
    }
    step_until(&mut h, "播放送來的第一個（依檔名排序：第2集）", |s| {
        playing(s, "第2集.mp4")
    });
    assert_eq!(playlist_names(h.state()), ["第2集.mp4", "第3集.mp4"]);
}

#[test]
fn dragging_in_a_scrolled_long_list_lands_where_dropped() {
    let dir = TempDir::new("panel-long");
    for i in 1..=40 {
        dir.clip(&format!("第{i:02}集.mp4"));
    }
    let mut h = harness(Some(dir.0.join("第01集.mp4")));
    settle(&mut h, "第01集.mp4");
    step_until_app(&mut h, "掃描到 40 個影片", |app| playlist_len(app) == 40);
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    // 捲到中間
    let first = h.get_by_label("1. 第01集.mp4").rect();
    h.event(egui::Event::PointerMoved(first.center()));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -400.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    });
    h.run_steps(3);
    // 找一列看得到的（中間附近），拖到它下面第 3 列的上半部
    // 完全看得到的列（捲動區上緣被裁掉一半的列點不到）
    let header = h.get_by_label_contains("播放清單（").rect();
    let visible = |h: &Harness<'_, VitascopeApp>, n: usize| {
        h.query_by_label(&format!("{n}. 第{n:02}集.mp4")).is_some_and(|r| {
            r.rect().top() > header.bottom() + 30.0 && r.rect().bottom() < h.ctx.content_rect().bottom() - 90.0
        })
    };
    let k = (5..35)
        .find(|&k| visible(&h, k) && visible(&h, k + 3))
        .expect("捲動後中間的列看得到");
    assert!(k > 5, "清單有捲動：{k}");
    let from = h.get_by_label(&format!("{k}. 第{k:02}集.mp4")).rect().center();
    let target = h.get_by_label(&format!("{}. 第{:02}集.mp4", k + 3, k + 3)).rect();
    let to = egui::pos2(target.center().x, target.top() + 3.0);
    h.event(egui::Event::PointerMoved(from));
    h.event(left_button(from, true));
    h.step();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
        h.step();
    }
    h.event(left_button(to, false));
    h.run_steps(2);
    let names = playlist_names(h.state());
    // 第 k 集移到原本第 k+3 集的前面（清單上的位置 k+1，從 0 算）
    assert_eq!(names[k + 1], format!("第{k:02}集.mp4"), "{names:?}");
    assert_eq!(names[k + 2], format!("第{:02}集.mp4", k + 3), "{names:?}");
}

#[test]
fn repeated_entries_do_not_loop() {
    let dir = three_episodes("m3u-dup");
    let list = dir.0.join("重複.m3u8");
    std::fs::write(&list, "第1集.mp4\n第2集.mp4\n第1集.mp4\n第3集.mp4\n").unwrap();
    let mut h = harness(None);
    h.step();
    drop_file(&mut h, list);
    step_until(&mut h, "第1集", |s| playing(s, "第1集.mp4"));
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "第2集", |s| playing(s, "第2集.mp4"));
    h.key_press(egui::Key::PageDown);
    step_until_app(&mut h, "清單上第 3 項（第二次的第1集）", |app| {
        app.playlist().is_some_and(|l| l.position() == 3)
    });
    h.key_press(egui::Key::PageDown);
    step_until(&mut h, "第3集（不會繞回第2集）", |s| playing(s, "第3集.mp4"));
}

#[test]
fn rotated_mkv_crops_and_stretches_the_upright_picture() {
    // 直拍影片轉存的 MKV：旋轉在容器層（mpv 自己的 MKV 解析器讀），影格上沒有
    let mut h = opened(sample("rare/mkv_hevc_aac_rot90.mkv")); // 320x240，標示轉 90°
    if prop(&h, "video-params/rotate") != "90" {
        // 舊版 ffmpeg（6.1）產生的樣本沒有旋轉資訊
        eprintln!("樣本沒有旋轉資訊，略過");
        return;
    }
    let natural = h.state().player().natural_shape().unwrap();
    assert!(
        (natural.0 - 0.75).abs() < 0.01 && natural.1 == 90,
        "原本是直的 3:4：{natural:?}"
    );
    assert_eq!(h.state().player().state.video_size, Some([240, 320]));
    // 裁成 16:9（第一下）：直的畫面裁掉上下
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Q);
    step_until(&mut h, "裁成 16:9", |s| s.video_size == Some([240, 135]));
}
