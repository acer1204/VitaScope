//! 介面測試（ROADMAP 4.2）：用 egui_kittest 模擬按鍵與點擊，檢查播放器的反應。
//!
//! 播放器用 headless 模式（不出畫面、不出聲音），所以不需要 GPU，CI 也能跑。
//! 影片畫面本身的渲染另外用 `--shot` 自動截圖驗證。

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use vitascope::app::{DialogKind, Launch, Pick, PlatformProbe, VitascopeApp};
use vitascope::keymap::Platform;
use vitascope::pacing::{Plan, Reason, SmoothMode};
use vitascope::player::{AsyncKey, Options, Player, State, TrackKind};
use vitascope::power::PowerSource;
use vitascope::screens::{Refresh, RefreshSource};
use vitascope::settings::{OnTop, Settings, SideTab};
use vitascope::theme::ThemeChoice;

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
    harness_launch_with(
        Options {
            keep_open: true,
            ..Options::headless()
        },
        launch,
        settings,
    )
}

/// 自己指定播放器的選項（例如用 `extra` 加 mpv 選項：vo-null-fps 之類的）
fn harness_launch_with(opts: Options, launch: Launch, settings: Settings) -> Harness<'static, VitascopeApp> {
    harness_with_player(Player::new(opts).unwrap(), launch, settings)
}

/// 用準備好的播放器（例如先放了假的音訊裝置清單）
fn harness_with_player(player: Player, launch: Launch, settings: Settings) -> Harness<'static, VitascopeApp> {
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
    let mut settings = Settings::default();
    settings.auto_next = false;
    playing_multitrack_with(settings)
}

/// 同上，用指定的設定
fn playing_multitrack_with(settings: Settings) -> Harness<'static, VitascopeApp> {
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
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
    // 跳轉途中 mpv 會短暫回報目標時間，要看最後停在哪裡：超過片尾的相對跳轉，mpv 會退回最後一個關鍵影格（這個檔案是 10.4 秒）
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    let t = h.state().player().state.time_pos;
    assert!(t >= 19.0, "Ctrl+→ 之後停在 {t:.2} 秒，不是結尾");
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

/// 按下、放開按鍵（同一幀內），回傳這一幀送給視窗的指令。
/// 一定要放開：沒放開的鍵再按一次，egui 會當成按住不放的自動重複（開關類的指令不重複）
fn press_and_get_commands(h: &mut Harness<'_, VitascopeApp>, key: egui::Key) -> Vec<egui::ViewportCommand> {
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
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
    hover_context_item(h, label);
    h.get_by_label_contains(label).click();
    h.step();
}

/// 在影片上按右鍵、捲到選單裡的項目、把滑鼠移到它上面（有子選單的話子選單會打開）
fn hover_context_item(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    hover_menu_item(h, label);
}

/// 選單已經打開：捲到項目、把滑鼠移到它上面
fn hover_menu_item(h: &mut Harness<'_, VitascopeApp>, label: &str) {
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
    // 捲動有動畫：等項目停下來再移過去（不然滑鼠停在動畫途中的位置，項目捲走後就不在它上面了）
    let mut last = h.get_by_label_contains(label).rect();
    for _ in 0..30 {
        h.step();
        let now = h.get_by_label_contains(label).rect();
        if now == last {
            break;
        }
        last = now;
    }
    // 捲動時滑鼠可能停在有子選單的項目上（子選單會打開）：先移到要點的項目上，等子選單關掉
    h.get_by_label_contains(label).hover();
    h.run_steps(3);
}

#[test]
fn always_on_top_from_context_menu_and_keys() {
    // 右鍵選單「視窗置頂 ▸」的三個選項：各自換模式、送對的視窗層級（播放中：播放時置頂 = 置頂）
    let dir = TempDir::new("on-top-menu");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = playing_multitrack_with(settings);
    for (item, mode, level) in [
        ("永遠置頂", OnTop::Always, egui::WindowLevel::AlwaysOnTop),
        ("不置頂", OnTop::Never, egui::WindowLevel::Normal),
        ("播放時置頂", OnTop::WhilePlaying, egui::WindowLevel::AlwaysOnTop),
        ("不置頂", OnTop::Never, egui::WindowLevel::Normal),
    ] {
        let levels = pick_on_top_from_menu(&mut h, item);
        assert_eq!(levels, [level], "{item}");
        assert_eq!(h.state().settings().on_top, mode, "{item}");
        assert_eq!(Settings::load_from(path.clone()).on_top, mode, "馬上存檔：{item}");
        assert_eq!(h.state().osd_text(), Some(mode_osd(mode)));
    }
    // Ctrl+T 依序切換：不置頂 → 永遠置頂（其他順序見 ctrl_t_cycles_the_on_top_modes）
    assert_eq!(press_ctrl_t(&mut h), [egui::WindowLevel::AlwaysOnTop]);
    assert_eq!(h.state().settings().on_top, OnTop::Always);
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
    // 播放時置頂：播放中離開全螢幕也再設一次（動畫結束後再一次）；暫停中（實際是一般視窗）不設成置頂
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(2);
    assert_eq!(h.state().settings().on_top, OnTop::WhilePlaying);
    let leave_fullscreen = |h: &mut Harness<'_, VitascopeApp>| {
        set_fullscreen(h, true);
        h.input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .fullscreen = Some(false);
        h.step();
        window_levels(h)
    };
    assert_eq!(leave_fullscreen(&mut h), [egui::WindowLevel::AlwaysOnTop]);
    assert_eq!(
        levels_during(&mut h, 1.3),
        [egui::WindowLevel::AlwaysOnTop],
        "動畫結束後再設一次"
    );
    h.key_press(egui::Key::Space);
    let levels = levels_until(&mut h, "暫停", |s| s.paused);
    assert_eq!(levels, [egui::WindowLevel::Normal]);
    assert_eq!(leave_fullscreen(&mut h), [], "暫停中離開全螢幕不置頂");
    assert_eq!(levels_during(&mut h, 1.3), []);
}

/// 這一幀送給視窗的層級指令
fn window_levels(h: &Harness<'_, VitascopeApp>) -> Vec<egui::WindowLevel> {
    viewport_commands(h)
        .into_iter()
        .filter_map(|c| match c {
            egui::ViewportCommand::WindowLevel(l) => Some(l),
            _ => None,
        })
        .collect()
}

/// 一直跑介面幀直到播放器狀態符合條件，回傳這段期間送給視窗的所有層級指令
fn levels_until(
    h: &mut Harness<'_, VitascopeApp>,
    what: &str,
    cond: impl Fn(&State) -> bool,
) -> Vec<egui::WindowLevel> {
    let start = Instant::now();
    let mut levels = Vec::new();
    loop {
        h.step();
        levels.extend(window_levels(h));
        if cond(&h.state().player().state) {
            return levels;
        }
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}\n目前狀態：{:#?}",
            h.state().player().state
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 實際經過一段時間、一直更新介面，回傳期間送給視窗的所有層級指令
fn levels_during(h: &mut Harness<'_, VitascopeApp>, seconds: f64) -> Vec<egui::WindowLevel> {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    let mut levels = Vec::new();
    while Instant::now() < end {
        h.step();
        levels.extend(window_levels(h));
        std::thread::sleep(Duration::from_millis(10));
    }
    levels
}

/// 按下、放開 Ctrl（macOS：Cmd）+ T（同一幀內），回傳這一幀送給視窗的層級指令。
/// 不用 `key_press_modifiers`：它把修飾鍵分成好幾幀送，`output()` 只看得到最後一幀
fn press_ctrl_t(h: &mut Harness<'_, VitascopeApp>) -> Vec<egui::WindowLevel> {
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::T,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
    }
    h.step();
    window_levels(h)
}

/// 右鍵選單「視窗置頂 ▸」裡選一個模式，回傳那一幀送給視窗的層級指令
fn pick_on_top_from_menu(h: &mut Harness<'_, VitascopeApp>, item: &str) -> Vec<egui::WindowLevel> {
    hover_context_item(h, "視窗置頂");
    h.get_by_label(item).click();
    h.step();
    let levels = window_levels(h);
    h.run_steps(2);
    levels
}

fn mode_osd(mode: OnTop) -> &'static str {
    match mode {
        OnTop::Never => "視窗置頂：關閉",
        OnTop::Always => "視窗置頂：永遠",
        OnTop::WhilePlaying => "視窗置頂：播放時",
    }
}

#[test]
fn on_top_while_playing_follows_playback() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.on_top = OnTop::WhilePlaying;
    let mut h = harness_with(None, settings);
    assert_eq!(levels_during(&mut h, 0.2), [], "沒有檔案：一般視窗");
    // 開始播放：置頂（只送一次；拖進檔案的那一幀也算）
    h.input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(sample("common/mp4_h264_aac.mp4"))));
    let levels = levels_until(&mut h, "開始播放", |s| s.loaded && !s.paused && s.time_pos > 0.0);
    assert_eq!(levels, [egui::WindowLevel::AlwaysOnTop]);
    h.run_steps(5);
    // 暫停：一般視窗；繼續：置頂
    h.key_press(egui::Key::Space);
    assert_eq!(levels_until(&mut h, "暫停", |s| s.paused), [egui::WindowLevel::Normal]);
    assert_eq!(levels_during(&mut h, 0.2), []);
    // 暫停中逐格（mpv 會短暫取消暫停、播一格再停）：算暫停，維持一般視窗
    for _ in 0..3 {
        let before = h.state().player().state.time_pos;
        h.key_press(egui::Key::Period);
        let mut levels = levels_until(&mut h, "逐格前進一格", |s| s.paused && s.time_pos > before);
        levels.extend(levels_during(&mut h, 0.2));
        assert_eq!(levels, [], "逐格");
    }
    h.key_press(egui::Key::Space);
    assert_eq!(
        levels_until(&mut h, "繼續播放", |s| !s.paused),
        [egui::WindowLevel::AlwaysOnTop]
    );
    // 跳到快結束、播完停在最後一格（keep-open 會暫停）：一般視窗
    let duration = h.state().player().state.duration.unwrap();
    let _ = h.state().player().seek_to(duration - 0.3, true);
    assert_eq!(
        levels_until(&mut h, "播完停在最後一格", |s| s.eof && s.paused),
        [egui::WindowLevel::Normal]
    );
    assert_eq!(levels_during(&mut h, 0.2), []);
    // 從頭播放：置頂；停止：一般視窗
    h.key_press(egui::Key::Home);
    assert_eq!(
        levels_until(&mut h, "從頭播放", |s| s.loaded && !s.paused && !s.eof),
        [egui::WindowLevel::AlwaysOnTop]
    );
    let _ = h.state().player().stop();
    assert_eq!(levels_until(&mut h, "停止", |s| !s.loaded), [egui::WindowLevel::Normal]);
    assert_eq!(levels_during(&mut h, 0.2), [], "沒有檔案：維持一般視窗");
}

#[test]
fn on_top_level_is_sent_only_when_it_changes() {
    let mut h = playing_multitrack();
    assert_eq!(levels_during(&mut h, 0.3), [], "不置頂：什麼都不送");
    assert_eq!(
        pick_on_top_from_menu(&mut h, "永遠置頂"),
        [egui::WindowLevel::AlwaysOnTop]
    );
    assert_eq!(levels_during(&mut h, 0.5), [], "播放中不會一直送");
    // 播放中從永遠置頂換成播放時置頂：實際一樣是置頂，不用再送
    assert_eq!(pick_on_top_from_menu(&mut h, "播放時置頂"), []);
    assert_eq!(h.state().settings().on_top, OnTop::WhilePlaying);
    assert_eq!(levels_during(&mut h, 0.5), []);
    // 暫停中換成永遠置頂：從一般換成置頂
    h.key_press(egui::Key::Space);
    assert_eq!(levels_until(&mut h, "暫停", |s| s.paused), [egui::WindowLevel::Normal]);
    assert_eq!(levels_during(&mut h, 0.3), []);
    assert_eq!(
        pick_on_top_from_menu(&mut h, "永遠置頂"),
        [egui::WindowLevel::AlwaysOnTop]
    );
    assert_eq!(levels_during(&mut h, 0.3), []);
}

#[test]
fn ctrl_t_cycles_the_on_top_modes() {
    let dir = TempDir::new("on-top-keys");
    let path = dir.0.join("settings.json");
    for lang in vitascope::i18n::Lang::ALL {
        let en = lang == vitascope::i18n::Lang::En;
        let mut settings = Settings::load_from(path.clone());
        settings.auto_next = false;
        settings.language = lang;
        settings.on_top = OnTop::Never;
        let mut h = playing_multitrack_with(settings);
        // 播放中：不置頂 → 永遠置頂（置頂）→ 播放時置頂（還是置頂，不用再送）→ 不置頂（一般）
        for (mode, zh, english, levels) in [
            (
                OnTop::Always,
                "視窗置頂：永遠",
                "Always on top: always",
                vec![egui::WindowLevel::AlwaysOnTop],
            ),
            (
                OnTop::WhilePlaying,
                "視窗置頂：播放時",
                "Always on top: while playing",
                vec![],
            ),
            (
                OnTop::Never,
                "視窗置頂：關閉",
                "Always on top: off",
                vec![egui::WindowLevel::Normal],
            ),
        ] {
            assert_eq!(press_ctrl_t(&mut h), levels, "{mode:?}");
            assert_eq!(h.state().settings().on_top, mode);
            assert_eq!(h.state().osd_text(), Some(if en { english } else { zh }));
            assert_eq!(Settings::load_from(path.clone()).on_top, mode, "馬上存檔");
            h.run_steps(2);
        }
    }
}

#[test]
fn general_page_on_top_combo_box() {
    let dir = TempDir::new("on-top-page");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = playing_multitrack_with(settings);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("視窗置頂");
    h.get_by_value("不置頂").click();
    h.run_steps(2);
    h.get_by_label("播放時置頂").click();
    h.step();
    // 跟右鍵選單一樣：馬上套用（播放中：置頂）、存檔、提示
    assert_eq!(window_levels(&h), [egui::WindowLevel::AlwaysOnTop]);
    assert_eq!(h.state().settings().on_top, OnTop::WhilePlaying);
    assert_eq!(Settings::load_from(path.clone()).on_top, OnTop::WhilePlaying);
    assert_eq!(h.state().osd_text(), Some("視窗置頂：播放時"));
    h.run_steps(2);
    h.get_by_value("播放時置頂").click();
    h.run_steps(2);
    h.get_by_label("不置頂").click();
    h.step();
    assert_eq!(window_levels(&h), [egui::WindowLevel::Normal]);
    assert_eq!(Settings::load_from(path).on_top, OnTop::Never);
    h.run_steps(2);
    h.get_by_value("不置頂");
}

#[test]
fn wayland_refuses_both_on_top_modes() {
    let refused = "這個桌面環境（Wayland）不支援讓程式自己設定視窗置頂";
    let wayland = |on_top: OnTop| {
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.on_top = on_top;
        let mut h = harness_launch(
            Launch {
                files: vec![sample("common/mkv_multitrack.mkv")],
                wayland: Some(true),
                ..Default::default()
            },
            settings,
        );
        settle(&mut h, "mkv_multitrack.mkv");
        h
    };
    let mut h = wayland(OnTop::Never);
    // 快捷鍵、右鍵選單、設定視窗：永遠置頂、播放時置頂都不行
    assert_eq!(press_ctrl_t(&mut h), []);
    assert_eq!(h.state().settings().on_top, OnTop::Never);
    assert_eq!(h.state().osd_text(), Some(refused));
    h.run_steps(2);
    for item in ["永遠置頂", "播放時置頂"] {
        assert_eq!(pick_on_top_from_menu(&mut h, item), [], "{item}");
        assert_eq!(h.state().settings().on_top, OnTop::Never, "{item}");
        assert_eq!(h.state().osd_text(), Some(refused));
    }
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_value("不置頂").click();
    h.run_steps(2);
    h.get_by_label("播放時置頂").click();
    h.step();
    assert_eq!(h.state().settings().on_top, OnTop::Never);
    assert_eq!(h.state().osd_text(), Some(refused));
    assert_eq!(levels_during(&mut h, 0.3), []);
    // 在別的桌面環境開的置頂（兩種都一樣）：照樣可以用 Ctrl+T 關掉
    for on_top in [OnTop::Always, OnTop::WhilePlaying] {
        let mut h = wayland(on_top);
        assert_eq!(press_ctrl_t(&mut h), [egui::WindowLevel::Normal], "{on_top:?}");
        assert_eq!(h.state().settings().on_top, OnTop::Never, "{on_top:?}");
        assert_eq!(h.state().osd_text(), Some("視窗置頂：關閉"));
    }
}

/// 一直跑介面幀直到整個播放器符合條件（例如背景掃描完的播放清單），回傳期間送給視窗的層級指令
fn levels_until_app(
    h: &mut Harness<'_, VitascopeApp>,
    what: &str,
    cond: impl Fn(&VitascopeApp) -> bool,
) -> Vec<egui::WindowLevel> {
    let start = Instant::now();
    let mut levels = Vec::new();
    loop {
        h.step();
        levels.extend(window_levels(h));
        if cond(h.state()) {
            return levels;
        }
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn on_top_while_playing_stays_on_top_across_file_changes() {
    // 播放時置頂：換下一個／上一個檔案、播完自動播下一個時，中間不會先變一般視窗再變回置頂
    let dir = TempDir::new("on-top-switch");
    for name in ["第1集.mp4", "第2集.mp4"] {
        dir.clip(name);
    }
    let mut settings = Settings::default();
    settings.auto_next = true;
    settings.on_top = OnTop::WhilePlaying;
    let mut h = harness_with(None, settings);
    let started = |name: &'static str| move |s: &State| playing(s, name) && !s.paused && !s.eof && s.time_pos > 0.0;
    // 拖進第1集（不在啟動時開：建立視窗那一幀送的指令看不到）
    h.input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(dir.0.join("第1集.mp4"))));
    let mut levels = levels_until(&mut h, "播放第1集", started("第1集.mp4"));
    levels.extend(levels_until_app(&mut h, "掃描到兩個影片", |app| {
        playlist_len(app) == 2
    }));
    assert_eq!(levels, [egui::WindowLevel::AlwaysOnTop]);
    h.key_press(egui::Key::PageDown);
    let mut levels = levels_until(&mut h, "PgDn → 第2集", started("第2集.mp4"));
    levels.extend(levels_during(&mut h, 0.2));
    assert_eq!(levels, [], "換下一個檔案");
    h.key_press(egui::Key::PageUp);
    let mut levels = levels_until(&mut h, "PgUp → 第1集", started("第1集.mp4"));
    levels.extend(levels_during(&mut h, 0.2));
    assert_eq!(levels, [], "換上一個檔案");
    // 第1集播完（keep-open 停在最後一格、暫停）馬上自動播第2集：不算播完
    let duration = h.state().player().state.duration.unwrap();
    let _ = h.state().player().seek_to(duration - 0.3, true);
    let mut levels = levels_until(&mut h, "自動播第2集", started("第2集.mp4"));
    levels.extend(levels_during(&mut h, 0.2));
    assert_eq!(levels, [], "自動播下一個");
    // 最後一個播完、沒有下一個：停在最後一格，一般視窗
    let duration = h.state().player().state.duration.unwrap();
    let _ = h.state().player().seek_to(duration - 0.3, true);
    let mut levels = levels_until(&mut h, "第2集播完", |s| {
        playing(s, "第2集.mp4") && s.eof && s.paused
    });
    levels.extend(levels_during(&mut h, 0.3));
    assert_eq!(levels, [egui::WindowLevel::Normal], "清單播完");
}

#[test]
fn resuming_right_after_leaving_fullscreen_stays_on_top() {
    // 播放時置頂：暫停中離開全螢幕、一秒內又繼續播放。macOS 動畫結束時會把層級改回一般，
    // 這時送的置頂會被蓋掉：動畫結束後要再設一次
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.on_top = OnTop::WhilePlaying;
    let mut h = playing_multitrack_with(settings);
    h.run_steps(2);
    h.key_press(egui::Key::Space);
    assert_eq!(levels_until(&mut h, "暫停", |s| s.paused), [egui::WindowLevel::Normal]);
    set_fullscreen(&mut h, true);
    h.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .fullscreen = Some(false);
    h.step();
    assert_eq!(window_levels(&h), [], "暫停中離開全螢幕：不置頂");
    h.key_press(egui::Key::Space);
    assert_eq!(
        levels_until(&mut h, "繼續播放", |s| !s.paused),
        [egui::WindowLevel::AlwaysOnTop]
    );
    assert_eq!(
        levels_during(&mut h, 1.3),
        [egui::WindowLevel::AlwaysOnTop],
        "動畫結束後再設一次"
    );
    assert_eq!(levels_during(&mut h, 0.3), []);
}

#[test]
fn on_top_submenu_header_shows_the_assigned_key() {
    // 「視窗置頂 ▸」標題上的按鍵從快捷鍵對照表來：改了就跟著改，沒有指定按鍵就不寫
    let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    for (keys, header) in [(vec!["F13".to_owned()], "視窗置頂 F13 ⏵"), (vec![], "視窗置頂 ⏵")] {
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.keys.custom.insert("on-top".into(), keys);
        let mut h = playing_multitrack_with(settings);
        h.get_by_label("影片畫面").click_secondary();
        h.run_steps(2);
        assert!(h.query_by_label(header).is_some(), "選單上沒有「{header}」");
        assert!(
            h.query_by_label(&format!("視窗置頂 {cmd}+T ⏵")).is_none(),
            "{cmd}+T 已經不是切換視窗置頂"
        );
    }
}

#[test]
fn theme_note_sits_under_the_appearance_row() {
    // 偵測不到系統深淺色的提示緊接在「外觀」下面，不會跑到「視窗置頂」底下
    let mut settings = Settings::default();
    settings.theme = ThemeChoice::System;
    let mut h = harness_like_eframe(settings, None);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    let theme = h.get_by_value("跟隨系統").rect();
    let note = h.get_by_label("偵測不到系統的深淺色設定，暫時用深色").rect();
    let on_top = h.get_by_label("視窗置頂").rect();
    assert!(theme.bottom() <= note.top(), "提示在外觀下面：{theme:?} {note:?}");
    assert!(note.bottom() <= on_top.top(), "提示在視窗置頂上面：{note:?} {on_top:?}");
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
    // 翻轉的著色器跟使用者的著色器同一個清單，非同步送出：等它生效
    let flips = |app: &VitascopeApp| {
        let list = app.player().shader_list().unwrap();
        let has = |name: &str| list.iter().any(|f| f.ends_with(name));
        (has("hflip.glsl"), has("vflip.glsl"))
    };
    let mut h = playing_multitrack();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    step_until_app(&mut h, "左右翻轉", |app| flips(app) == (true, false));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::P);
    step_until_app(&mut h, "上下翻轉", |app| flips(app) == (true, true));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    step_until_app(&mut h, "再按一次取消", |app| flips(app) == (false, true));
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
    // 先設好螢幕與視窗的位置再開檔（開檔後很快就會配合影片調整視窗）
    let mut h = harness_with(None, settings);
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
    h.step();
    h.input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(sample("common/mov_hevc_aac_rot90.mov"))));
    // 調整大小與移動可能在不同的畫面送出（開檔時配合影片、之後畫面形狀確定再調整一次）
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
            if let egui::ViewportCommand::OuterPosition(p) = c {
                moved = Some(p);
            }
        }
    }
    let mut moved = moved.expect("超出螢幕時要移回來");
    // 之後如果又調整一次，以最後一次為準
    for _ in 0..10 {
        h.step();
        for c in h
            .output()
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
        {
            if let egui::ViewportCommand::OuterPosition(p) = c {
                moved = p;
            }
        }
    }
    // 測試環境會照 InnerSize 改變畫面大小：用最後的大小算新的外框（加上標題列、邊框）
    let size = h.ctx.content_rect().size();
    let bottom = moved.y + size.y + (635.0 - 600.0);
    assert!(size.y > 600.0, "直式影片的視窗比原本高：{size:?}");
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

// ───────────── 外觀：深色／淺色 ─────────────

/// 「切換深色／淺色」預設沒有按鍵：測試在設定裡指定 F12（也順便測自己改的按鍵）
fn with_theme_key(mut settings: Settings) -> Settings {
    settings.keys.custom.insert("cycle-theme".into(), vec!["F12".into()]);
    settings
}

/// 按 F12（`with_theme_key`）切換深色／淺色
fn cycle_theme(h: &mut Harness<'_, VitascopeApp>) {
    h.key_press(egui::Key::F12);
    h.step();
}

/// 跟真正的播放器（eframe）一樣：建立 App 之前主題偏好是「跟隨系統」（egui 的預設），之後也沒有人改它；
/// 作業系統回報的深淺色是 `system`（Windows、macOS 有，Linux 是 None）。
/// kittest 建好 App 之後會自己把主題設成深色，這裡改回 App 設的，不然測不出系統是淺色時的問題
fn harness_like_eframe(settings: Settings, system: Option<egui::Theme>) -> Harness<'static, VitascopeApp> {
    harness_like_eframe_launch(Launch::default(), settings, system)
}

/// 同上，啟動時開 `launch` 的檔案
fn harness_like_eframe_launch(
    launch: Launch,
    settings: Settings,
    system: Option<egui::Theme>,
) -> Harness<'static, VitascopeApp> {
    let player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    let set_by_app = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = set_by_app.clone();
    let mut h = Harness::builder().with_size([960.0, 600.0]).build_eframe(move |cc| {
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        let app = VitascopeApp::new(cc, player, settings, launch);
        *seen.lock().unwrap() = Some(cc.egui_ctx.options(|o| o.theme_preference));
        app
    });
    let pref = set_by_app.lock().unwrap().expect("建立了 App");
    h.ctx.set_theme(pref);
    h.input_mut().system_theme = system;
    h.run_steps(2);
    h
}

/// 這一幀畫的、整個蓋住 `rect` 的長方形的底色（由下往上：最後畫的在前面）
fn fills_covering(h: &Harness<'_, VitascopeApp>, rect: egui::Rect) -> Vec<egui::Color32> {
    fn walk(shape: &egui::Shape, rect: egui::Rect, out: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Rect(r) if r.rect.expand(0.5).contains_rect(rect) => out.push(r.fill),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, rect, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &h.output().shapes {
        walk(&clipped.shape, rect, &mut out);
    }
    out.reverse();
    out
}

/// 這一幀畫的、內容包含 `text` 的字用的顏色（同樣的字可能畫在好幾個地方，例如 OSD）
fn text_colors(h: &Harness<'_, VitascopeApp>, text: &str) -> Vec<egui::Color32> {
    fn walk(shape: &egui::Shape, text: &str, out: &mut Vec<egui::Color32>) {
        match shape {
            egui::Shape::Text(t) if t.galley.text().contains(text) => {
                out.extend(t.galley.job.sections.iter().map(|s| s.format.color));
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, text, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for clipped in &h.output().shapes {
        walk(&clipped.shape, text, &mut out);
    }
    assert!(!out.is_empty(), "這一幀沒有畫「{text}」");
    out
}

/// 這一幀有沒有畫底色是 `fill` 的長方形
fn has_rect_filled(h: &Harness<'_, VitascopeApp>, fill: egui::Color32) -> bool {
    fn walk(shape: &egui::Shape, fill: egui::Color32) -> bool {
        match shape {
            egui::Shape::Rect(r) => r.fill == fill,
            egui::Shape::Vec(shapes) => shapes.iter().any(|s| walk(s, fill)),
            _ => false,
        }
    }
    h.output().shapes.iter().any(|c| walk(&c.shape, fill))
}

fn luma(c: egui::Color32) -> u32 {
    (u32::from(c.r()) * 299 + u32::from(c.g()) * 587 + u32::from(c.b()) * 114) / 1000
}

/// 控制列（影片畫面下面那一條）的底色
fn control_bar_fill(h: &Harness<'_, VitascopeApp>) -> egui::Color32 {
    let video = h.get_by_label("影片畫面").rect();
    let below = egui::Rect::from_min_size(
        egui::pos2(video.left() + 4.0, video.bottom() + 2.0),
        egui::vec2(4.0, 4.0),
    );
    *fills_covering(h, below).first().expect("控制列有底色")
}

/// 播放清單面板（影片畫面右邊）的底色：取面板左邊的留白，不會碰到按鈕、清單項目
fn playlist_fill(h: &Harness<'_, VitascopeApp>) -> egui::Color32 {
    let video = h.get_by_label("影片畫面").rect();
    let inside = egui::Rect::from_center_size(egui::pos2(video.right() + 5.0, video.center().y), egui::vec2(2.0, 2.0));
    *fills_covering(h, inside).first().expect("播放清單有底色")
}

#[test]
fn dark_choice_is_not_flipped_by_a_light_system() {
    // v0.3.0 的問題：系統是淺色的 Windows、macOS 上，第一幀就換成 egui 的淺色樣式，
    // 蓋在寫死的深色控制列上（控制列的時間、按鈕變成深灰字配深色底）
    let h = harness_like_eframe(Settings::default(), Some(egui::Theme::Light));
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    assert!(h.ctx.global_style().visuals.dark_mode, "介面是深色的樣式");
    assert_eq!(
        h.state().palette().panel,
        egui::Color32::from_gray(24),
        "深色跟以前一樣"
    );
    assert_eq!(control_bar_fill(&h), egui::Color32::from_gray(24));
}

#[test]
fn light_choice_from_the_settings_file_on_a_dark_system() {
    let mut settings = Settings::default();
    settings.theme = ThemeChoice::Light;
    let h = harness_like_eframe(settings, Some(egui::Theme::Dark));
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    assert!(!h.ctx.global_style().visuals.dark_mode);
    assert!(luma(control_bar_fill(&h)) > 200, "淺色的控制列");
}

#[test]
fn follow_system_uses_the_reported_theme() {
    let mut settings = Settings::default();
    settings.theme = ThemeChoice::System;
    let mut h = harness_like_eframe(settings, Some(egui::Theme::Light));
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    assert!(luma(control_bar_fill(&h)) > 200);
    // 系統換成深色（Windows 的設定、macOS 的外觀）：跟著換
    h.input_mut().system_theme = Some(egui::Theme::Dark);
    h.run_steps(2);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    assert_eq!(control_bar_fill(&h), egui::Color32::from_gray(24));
    // 設定頁不提示（系統有回報）
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_value("跟隨系統");
    assert!(h.query_by_label_contains("偵測不到系統的深淺色設定").is_none());
}

#[test]
fn follow_system_without_a_system_answer_is_dark_with_a_note() {
    // Linux：作業系統不回報深淺色，暫時用深色，設定頁說明
    let mut settings = Settings::default();
    settings.theme = ThemeChoice::System;
    let mut h = harness_like_eframe(settings, None);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("偵測不到系統的深淺色設定，暫時用深色");
}

#[test]
fn theme_setting_switches_and_saves() {
    let dir = TempDir::new("theme-setting");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    settings.show_playlist = true;
    let mut h = harness_with(Some(sample("common/mp4_h264_aac.mp4")), settings);
    settle(&mut h, "mp4_h264_aac.mp4");
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    assert_eq!(playlist_fill(&h), egui::Color32::from_gray(28), "深色的播放清單");
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("外觀");
    h.get_by_value("深色").click();
    h.run_steps(2);
    h.get_by_label("淺色").click();
    h.run_steps(3);
    // 馬上換、馬上存
    assert_eq!(h.state().settings().theme, ThemeChoice::Light);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    assert!(!h.ctx.global_style().visuals.dark_mode);
    assert_eq!(Settings::load_from(path.clone()).theme, ThemeChoice::Light);
    assert!(luma(control_bar_fill(&h)) > 200, "控制列跟著主題");
    assert!(luma(playlist_fill(&h)) > 200, "播放清單也是：{:?}", playlist_fill(&h));
    // 影片畫面還是黑的
    let video = h.get_by_label("影片畫面").rect();
    assert_eq!(
        fills_covering(&h, video.shrink(8.0)).first(),
        Some(&egui::Color32::BLACK)
    );
    assert_eq!(h.state().palette().video, egui::Color32::BLACK);
    // 跟隨系統
    h.get_by_value("淺色").click();
    h.run_steps(2);
    h.get_by_label("跟隨系統").click();
    h.run_steps(3);
    assert_eq!(Settings::load_from(path).theme, ThemeChoice::System);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark, "自動測試沒有系統的回報：深色");
}

#[test]
fn idle_screen_stays_dark_in_the_light_theme() {
    let mut settings = Settings::default();
    settings.theme = ThemeChoice::Light;
    settings.auto_next = false;
    // 開檔失敗：起始畫面上顯示錯誤訊息（錯誤色依樣式選深色或淺色的一組）
    let launch = Launch {
        files: vec![PathBuf::from("Z:/不存在/沒有這個檔案.mkv")],
        ..Default::default()
    };
    let mut h = harness_like_eframe_launch(launch, settings, None);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    step_until(&mut h, "開檔失敗", |s| s.last_error.is_some());
    h.run_steps(5);
    let video = h.get_by_label("影片畫面").rect();
    assert_eq!(
        fills_covering(&h, video.shrink(8.0)).first(),
        Some(&egui::Color32::BLACK)
    );
    h.get_by_label_contains("拖放到這裡");
    // 畫在黑色的影片畫面上：用深色樣式的錯誤色（淺色主題的深紅色在黑底上看不清楚）
    let dark = vitascope::theme::Palette::of(&egui::Visuals::dark());
    let light = vitascope::theme::Palette::of(&egui::Visuals::light());
    let colors = text_colors(&h, "無法載入檔案");
    assert!(colors.contains(&dark.problem), "{colors:?}");
    assert!(!colors.contains(&light.problem), "{colors:?}");
}

#[test]
fn media_info_stays_dark_in_the_light_theme() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mp4_h264_aac.mp4")), with_theme_key(settings));
    settle(&mut h, "mp4_h264_aac.mp4");
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    // 標題用深色樣式的強調色（半透明黑底上的亮藍色），不是淺色主題的深藍色
    let dark = vitascope::theme::Palette::of(&egui::Visuals::dark());
    let light = vitascope::theme::Palette::of(&egui::Visuals::light());
    let colors = text_colors(&h, "播放流暢度");
    assert!(colors.contains(&dark.accent), "{colors:?}");
    assert!(!colors.contains(&light.accent), "{colors:?}");
}

#[test]
fn ab_section_follows_the_theme() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = playing_multitrack_with(with_theme_key(settings));
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    h.key_press(egui::Key::L);
    step_until(&mut h, "L 設定起點", |s| s.ab_loop[0].is_some());
    let a = h.state().player().state.ab_loop[0].unwrap();
    step_until(&mut h, "播放半秒", |s| s.time_pos > a + 0.5);
    h.key_press(egui::Key::L);
    step_until(&mut h, "L 設定終點", |s| s.ab_loop[1].is_some());
    h.run_steps(2);
    // 淺色主題用深一點的琥珀色（原本的顏色跟淺灰的進度條底差不多亮）
    let dark = vitascope::theme::Palette::of(&egui::Visuals::dark());
    let light = vitascope::theme::Palette::of(&egui::Visuals::light());
    assert!(has_rect_filled(&h, light.ab.gamma_multiply(0.6)), "淺色的 A-B 區段");
    assert!(!has_rect_filled(&h, dark.ab.gamma_multiply(0.6)));
    cycle_theme(&mut h);
    h.run_steps(2);
    assert!(has_rect_filled(&h, dark.ab.gamma_multiply(0.6)), "深色跟以前一樣");
}

#[test]
fn fullscreen_controls_stay_dark_in_the_light_theme() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mp4_long.mp4")), with_theme_key(settings));
    settle(&mut h, "mp4_long.mp4");
    // 暫停：全螢幕播放中滑鼠不動 2 秒控制列會藏起來（CI 的機器慢，載入就可能超過 2 秒）
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    let button_fill = |h: &Harness<'_, VitascopeApp>| {
        let rect = h.get_by_label("⛶").rect();
        *fills_covering(h, egui::Rect::from_center_size(rect.center(), egui::vec2(2.0, 2.0)))
            .first()
            .expect("按鈕有底色")
    };
    // 視窗模式：控制列跟著主題，按鈕是淺色的
    assert!(luma(button_fill(&h)) > 180, "{:?}", button_fill(&h));
    // 全螢幕：控制列蓋在影片上，一律是深色的
    set_fullscreen(&mut h, true);
    assert!(luma(button_fill(&h)) < 100, "{:?}", button_fill(&h));
}

#[test]
fn cycle_theme_switches_dark_and_light() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(None, with_theme_key(settings));
    h.step();
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.state().settings().theme, ThemeChoice::Light);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);
    assert_eq!(h.state().osd_text(), Some("外觀：淺色"));
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.state().settings().theme, ThemeChoice::Dark);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    assert_eq!(h.state().osd_text(), Some("外觀：深色"));
    // 跟隨系統（系統是淺色）：換成跟現在看到的相反
    let mut settings = Settings::default();
    settings.theme = ThemeChoice::System;
    let mut h = harness_like_eframe(with_theme_key(settings), Some(egui::Theme::Light));
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.state().settings().theme, ThemeChoice::Dark);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
}

#[test]
fn theme_labels_in_english() {
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    settings.theme = ThemeChoice::System;
    let mut h = harness_like_eframe(with_theme_key(settings), None);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("Appearance");
    h.get_by_label("Couldn't detect the system's light/dark setting; using dark for now");
    h.get_by_value("Follow the system").click();
    h.run_steps(2);
    h.get_by_label("Dark");
    h.get_by_label("Light").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().theme, ThemeChoice::Light);
    cycle_theme(&mut h);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Appearance: dark"));
}

// ───────────── 快捷鍵對照表 ─────────────

/// 這一幀有沒有畫出內容剛好是 `text` 的文字
fn painted(h: &Harness<'_, VitascopeApp>, text: &str) -> bool {
    fn walk(shape: &egui::Shape, text: &str) -> bool {
        match shape {
            egui::Shape::Text(t) => t.galley.text() == text,
            egui::Shape::Vec(shapes) => shapes.iter().any(|s| walk(s, text)),
            _ => false,
        }
    }
    h.output().shapes.iter().any(|c| walk(&c.shape, text))
}

#[test]
fn custom_shortcut_moves_the_key_and_the_menu_hint() {
    let dir = TempDir::new("custom-keys");
    for name in ["第1集.mp4", "第2集.mp4"] {
        dir.clip(name);
    }
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.custom.insert("next-file".into(), vec!["N".into()]);
    let mut h = harness_with(Some(dir.0.join("第1集.mp4")), settings);
    step_until(&mut h, "播放第1集", |s| playing(s, "第1集.mp4"));
    step_until_app(&mut h, "掃描到兩個影片", |app| playlist_len(app) == 2);
    settle(&mut h, "第1集.mp4");
    assert_eq!(h.state().keymap().hint(vitascope::keymap::Command::NextFile), "N");
    // 右鍵選單上「下一個檔案」的按鍵跟著改；沒改的照舊
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    assert!(painted(&h, "N"), "選單上寫 N");
    assert!(!painted(&h, "PgDn"), "PgDn 已經不是下一個檔案");
    assert!(painted(&h, "PgUp"));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!egui::Popup::is_any_open(&h.ctx), "Esc 先關選單");
    // PgDn 不再換檔，N 才會
    h.key_press(egui::Key::PageDown);
    h.run_steps(5);
    wait_real(&mut h, 0.3);
    assert!(playing(&h.state().player().state, "第1集.mp4"));
    h.key_press(egui::Key::N);
    step_until(&mut h, "N → 第2集", |s| playing(s, "第2集.mp4"));
}

#[test]
fn unbound_restart_drops_the_resume_hint() {
    let long = sample("common/mp4_long.mp4"); // 90 秒
    let mut settings = Settings::default();
    settings.auto_next = false;
    // 空陣列 = 不指定按鍵
    settings.keys.custom.insert("restart".into(), vec![]);
    let mut h = harness_with(Some(long.clone()), settings);
    step_until(&mut h, "開始播放", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos > 0.0
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(&mut h, "前進 30 秒", |s| s.time_pos >= 29.0);
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "換檔", |s| playing(s, "mp4_h264_aac.mp4"));
    drop_file(&mut h, long);
    step_until_app(&mut h, "續播的提示", |app| {
        app.osd_text().is_some_and(|t| t.contains("繼續播放"))
    });
    let osd = h.state().osd_text().unwrap().to_owned();
    assert!(
        !osd.contains("從頭播放") && !osd.contains('（'),
        "沒有按鍵就不寫括號：{osd}"
    );
    step_until(&mut h, "從上次的位置繼續", |s| s.time_pos >= 28.0);
    // Home 不再從頭播放
    h.key_press(egui::Key::Home);
    h.run_steps(5);
    wait_real(&mut h, 0.3);
    assert!(h.state().player().state.time_pos >= 28.0);
}

#[test]
fn about_dialog_takes_the_keys_and_esc_closes_only_it() {
    let mut h = playing_multitrack();
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.key_press(egui::Key::F1);
    h.run_steps(2);
    h.get_by_label("acer1204/VitaScope");
    // 對話框開著：空白鍵不會暫停
    h.key_press(egui::Key::Space);
    h.run_steps(3);
    assert!(!h.state().player().state.paused);
    // Esc 只關對話框，設定視窗還開著
    h.key_press(egui::Key::Escape);
    h.step();
    assert!(h.query_by_label("acer1204/VitaScope").is_none(), "對話框關掉了");
    // 關掉對話框的下一幀，按鍵照常有作用
    h.key_press(egui::Key::Space);
    h.step();
    step_until(&mut h, "暫停", |s| s.paused);
    h.get_by_label("一般");
    // 下一個 Esc 才關設定視窗
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("一般").is_none(), "設定視窗關掉了");
}

#[test]
fn key_hints_in_english() {
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    settings.auto_next = false;
    let mut h = harness_with(None, settings.clone());
    h.step();
    let open = if cfg!(target_os = "macos") { "Cmd+O" } else { "Ctrl+O" };
    h.get_by_label(&format!("Drop a video here, or press {open} to open a file"));
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
    settle(&mut h, "mkv_multitrack.mkv");
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    for key in [open, "Space", "PgUp", "PgDn", "L"] {
        assert!(painted(&h, key), "{key}");
    }
}

#[test]
fn esc_closes_one_window_at_a_time_in_order() {
    let mut h = playing_multitrack();
    let panel = |h: &Harness<'_, VitascopeApp>| h.query_by_label("控制面板").is_some();
    let settings = |h: &Harness<'_, VitascopeApp>| h.query_by_label("一般").is_some();
    let info = |h: &Harness<'_, VitascopeApp>| h.query_by_label_contains("640×360（16:9）").is_some();
    assert!(!panel(&h) && !settings(&h) && !info(&h));
    // 媒體資訊、設定、控制面板都開著
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F1);
    h.run_steps(2);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    assert!(panel(&h) && settings(&h) && info(&h));
    // Esc 一次關一個：控制面板 → 設定 → 媒體資訊
    let mut open = Vec::new();
    for _ in 0..3 {
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        open.push([panel(&h), settings(&h), info(&h)]);
    }
    assert_eq!(
        open,
        [[false, true, true], [false, false, true], [false, false, false]],
        "[控制面板, 設定, 媒體資訊]"
    );
}

#[test]
fn a_key_counts_once_per_frame() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(None, settings);
    h.step();
    let press = |key, modifiers, repeat| egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers,
    };
    // 介面卡住時累積的 E（含自動重複、多按了 Shift）：跟以前的 consume_key 一樣，同一幀只加一次
    for (mods, repeat) in [
        (egui::Modifiers::NONE, false),
        (egui::Modifiers::NONE, true),
        (egui::Modifiers::SHIFT, true),
    ] {
        h.input_mut().events.push(press(egui::Key::E, mods, repeat));
    }
    // 不同的按鍵各算各的
    h.input_mut()
        .events
        .push(press(egui::Key::U, egui::Modifiers::NONE, false));
    h.step();
    h.run_steps(2);
    assert_eq!(h.state().adjust().brightness, 1);
    assert_eq!(h.state().adjust().saturation, 1);
}

#[test]
fn a_custom_delete_key_does_not_take_delete_from_the_playlist() {
    let dir = three_episodes("panel-delete-custom");
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings
        .keys
        .custom
        .insert("toggle-pause".into(), vec!["Delete".into()]);
    let mut h = harness_with(Some(dir.0.join("第1集.mp4")), settings);
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    h.get_by_label("1. 第1集.mp4").click();
    h.run_steps(2);
    // 清單開著：Delete 是移出清單（固定的按鍵），不是自己指定的暫停
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(playlist_names(h.state()), ["第2集.mp4", "第3集.mp4"]);
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.paused);
    // 清單關著：Delete 就是自己指定的指令
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    h.key_press(egui::Key::Delete);
    step_until(&mut h, "Delete 暫停", |s| s.paused);
    assert_eq!(playlist_names(h.state()), ["第2集.mp4", "第3集.mp4"]);
}

/// 選單已經打開：捲到標籤剛好是 `label` 的子選單、把滑鼠移上去打開它（`hover_menu_item` 比對的是部分文字）
fn open_exact_submenu(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    let screen = h.ctx.content_rect();
    for _ in 0..10 {
        let item = h.get_by_label(label).rect();
        if item.bottom() <= screen.bottom() {
            break;
        }
        h.event(egui::Event::PointerMoved(egui::pos2(
            item.center().x,
            screen.center().y,
        )));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -120.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        h.run_steps(2);
    }
    // 等捲動的動畫停下來（同 `hover_menu_item`）
    let mut last = h.get_by_label(label).rect();
    for _ in 0..30 {
        h.step();
        let now = h.get_by_label(label).rect();
        if now == last {
            break;
        }
        last = now;
    }
    h.get_by_label(label).hover();
    h.run_steps(3);
}

/// 右鍵選單、子選單上每個按鍵說明都跟 v0.3.0 一樣，而且接在對的項目上（標籤 = 「項目 按鍵」）
#[test]
fn menu_shortcut_hints_match_v030() {
    let mac = cfg!(target_os = "macos");
    let (cmd, alt) = if mac { ("Cmd", "Option") } else { ("Ctrl", "Alt") };
    let crop = if mac { "Control+Q" } else { "Ctrl+Q" };
    let info = if mac { "Cmd+I" } else { "Ctrl+F1" };
    let mut h = opened(sample("common/mkv_chapters.mkv"));
    step_until(&mut h, "讀到三個章節", |s| s.chapters.len() == 3);
    let expect = |h: &Harness<'_, VitascopeApp>, labels: &[String]| {
        for l in labels {
            assert!(h.query_by_label(l).is_some(), "選單上沒有「{l}」");
        }
    };
    // 每個子選單都重新開一次選單：視窗窄的時候，開著的子選單會蓋住主選單的其他項目
    let reopen = |h: &mut Harness<'_, VitascopeApp>| {
        if egui::Popup::is_any_open(&h.ctx) {
            h.key_press(egui::Key::Escape);
            h.run_steps(2);
        }
        h.get_by_label("影片畫面").click_secondary();
        h.run_steps(2);
    };
    reopen(&mut h);
    assert!(h.query_by_label("暫停 空白鍵").is_some() || h.query_by_label("播放 空白鍵").is_some());
    expect(
        &h,
        &[
            format!("開啟檔案… {cmd}+O"),
            "停止".into(),
            "上一個檔案 PgUp".into(),
            "下一個檔案 PgDn".into(),
            "逐格前進 .".into(),
            "逐格後退 ,".into(),
            "A-B 重播：設定起點 L".into(),
            "全螢幕 F".into(),
            "播放清單 F6".into(),
            format!("媒體資訊 {info}"),
            // 子選單：按鍵寫在標題上（依序切換三種模式）
            format!("視窗置頂 {cmd}+T ⏵"),
            "設定… F5".into(),
            "關於影戲 F1".into(),
        ],
    );
    open_exact_submenu(&mut h, "章節 ⏵");
    expect(&h, &[format!("上一章 / 下一章：{cmd}+PgUp / PgDn")]);
    reopen(&mut h);
    open_exact_submenu(&mut h, "畫質 ⏵");
    expect(&h, &[format!("影像調整… {alt}+G"), "還原影像調整 Q".into()]);
    reopen(&mut h);
    open_exact_submenu(&mut h, "擷取畫面 ⏵");
    expect(
        &h,
        &[format!("存到截圖資料夾 {cmd}+E"), format!("複製到剪貼簿 {cmd}+C")],
    );
    reopen(&mut h);
    open_exact_submenu(&mut h, "畫面 ⏵");
    expect(
        &h,
        &[
            "放大 9".into(),
            "縮小 1".into(),
            "重設縮放（100%） 5".into(),
            format!("左右翻轉（鏡像） {cmd}+Z"),
            format!("上下翻轉 {cmd}+P"),
            format!("重設畫面 {alt}+Backspace"),
        ],
    );
    open_exact_submenu(&mut h, "移動畫面 ⏵");
    expect(
        &h,
        &[
            format!("左移 {alt}+←"),
            format!("右移 {alt}+→"),
            format!("上移 {alt}+↑"),
            format!("下移 {alt}+↓"),
            format!("置中 {cmd}+5"),
        ],
    );
    hover_menu_item(&mut h, "畫面比例（");
    expect(&h, &[format!("A 或 {cmd}+F6 依序切換")]);
    hover_menu_item(&mut h, "裁切（");
    expect(&h, &[format!("{crop} 依序切換")]);
    hover_menu_item(&mut h, "旋轉（");
    expect(&h, &[format!("{alt}+K 依序旋轉")]);
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

// ───────────── L3 基礎：非同步設定、啟動時不改 mpv 選項 ─────────────

#[test]
fn status_quo_options() {
    // 預設設定啟動、開檔播放：使用者沒要求的 mpv 選項一個都不能變（之後的批次才開始套用畫質、音效）。
    // 例外是去交錯：擁有者決定預設「自動」（批次 7），引擎支援 auto 時從啟動就是 auto
    let mut h = harness(Some(sample("common/mp4_h264_aac.mp4")));
    step_until(&mut h, "開始播放", |s| s.loaded && s.time_pos > 0.0);
    h.run_steps(3);
    let caps = *h.state().engine_caps();
    for name in [
        "video-sync",
        "display-fps-override",
        "deinterlace",
        "scale",
        "dscale",
        "cscale",
        "af",
        "glsl-shaders",
        "volume-max",
        "audio-device",
        "audio-exclusive",
        "audio-channels",
        "audio-normalize-downmix",
        "audio-spdif",
    ] {
        let default = prop(&h, &format!("option-info/{name}/default-value"));
        let expected = if name == "deinterlace" && caps.deint_auto {
            "auto".to_owned()
        } else {
            default
        };
        assert_eq!(prop(&h, name), expected, "{name} 不能被改掉");
    }
    // 啟動時有偵測引擎的功能，結果記下來了（沒偵測的話全是預設值 false；每個 FFmpeg 都有 aformat）
    assert_eq!(caps.macos, cfg!(target_os = "macos"));
    assert!(caps.af.aformat, "{caps:?}");
    let mut fresh = Player::new(Options::headless()).unwrap();
    let probed = fresh.probe_caps();
    assert_eq!((caps.af, caps.deint_auto), (probed.af, probed.deint_auto));
    // 縮放演算法的預設值是從引擎讀的
    let d = h.state().picture_defaults();
    for (name, value) in [
        ("scale", &d.scale),
        ("dscale", &d.dscale),
        ("cscale", &d.cscale),
        ("scale-antiring", &d.scale_antiring),
    ] {
        assert_eq!(*value, prop(&h, &format!("option-info/{name}/default-value")), "{name}");
    }
}

#[test]
fn async_option_failures_show_a_message_except_af_command() {
    let mut h = harness(None);
    h.step();
    // af-command 失敗不提示（沒開檔時一定失敗；之後會改寫整條 af）。後面接一個會成功的設定，
    // 非同步指令照順序執行：它生效時 af-command 的回覆也已經回來了
    h.state_mut()
        .command_async_keyed(AsyncKey::AfCommand, &["af-command", "vs-eq", "g", "3", "equalizer@b1"]);
    h.state_mut().set_option_async(AsyncKey::Deband, "deband", "yes");
    step_until_app(&mut h, "deband=yes 生效", |app| {
        app.player().get_string("deband").is_ok_and(|v| v == "yes")
    });
    h.run_steps(3);
    assert_eq!(h.state().osd_text(), None, "af-command 失敗不能有提示");
    // 其他設定失敗：提示選項名稱與原因
    h.state_mut().set_option_async(AsyncKey::Deband, "deband", "bogus");
    step_until_app(&mut h, "提示無法套用", |app| {
        app.osd_text().is_some_and(|t| t.starts_with("無法套用 deband："))
    });
    h.state_mut().command_async_keyed(
        AsyncKey::Shaders,
        &["change-list", "glsl-shaders", "bogus-action", "x.glsl"],
    );
    step_until_app(&mut h, "提示無法套用 glsl-shaders", |app| {
        app.osd_text().is_some_and(|t| t.starts_with("無法套用 glsl-shaders："))
    });
    // 英文介面
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    let mut h = harness_with(None, settings);
    h.step();
    h.state_mut().set_option_async(AsyncKey::Sharpen, "sharpen", "bogus");
    step_until_app(&mut h, "English message", |app| {
        app.osd_text()
            .is_some_and(|t| t.starts_with("Couldn't apply sharpen: "))
    });
}

#[test]
fn extra_options_count_as_user_overrides() {
    let mut h = harness_launch_with(
        Options {
            extra: vec![("deband".into(), "yes".into())],
            ..Options::headless()
        },
        Launch::default(),
        Settings::default(),
    );
    h.step();
    assert!(h.state().player().user_overrides().contains("deband"));
    assert_eq!(prop(&h, "deband"), "yes");
}

// ───────────── 流暢播放：依螢幕更新率同步 ─────────────

/// 假的螢幕與電源（自動測試沒有真的視窗，查不到）。測試中可以改（拔掉電源、查不到更新率）
#[derive(Clone)]
struct FakePlatform(std::sync::Arc<std::sync::Mutex<FakeState>>);

struct FakeState {
    hz: Option<f64>,
    power: PowerSource,
}

impl FakePlatform {
    fn new(hz: Option<f64>, power: PowerSource) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(FakeState { hz, power })))
    }

    fn hz(&self) -> Option<f64> {
        self.0.lock().unwrap().hz
    }

    /// 改假的平台資訊，並要播放器下一幀就重查（不用等 10 秒一次的電源檢查）
    fn set(&self, h: &mut Harness<'_, VitascopeApp>, change: impl FnOnce(&mut FakeState)) {
        change(&mut self.0.lock().unwrap());
        h.state_mut().pacing_requery();
    }
}

impl PlatformProbe for FakePlatform {
    fn refresh_rate(&self) -> Option<Refresh> {
        self.hz().map(|hz| Refresh {
            hz,
            source: RefreshSource::DisplayConfig,
        })
    }
    fn monitor_key(&self) -> Option<u64> {
        Some(1)
    }
    fn power(&self) -> PowerSource {
        self.0.lock().unwrap().power
    }
    fn remote_session(&self) -> bool {
        false
    }
}

/// 開一個檔案，指定流暢播放的設定與假的螢幕（None = 跟真正的自動測試一樣沒有）；等到開始播放。
/// 假螢幕的更新率也給 vo=null（vo-null-fps）：不然依螢幕同步時 vo=null 用最快的速度跑，測試不穩
fn playing_smooth(
    platform: Option<&FakePlatform>,
    smooth: SmoothMode,
    file: &str,
    lang: vitascope::i18n::Lang,
) -> Harness<'static, VitascopeApp> {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.smooth = smooth;
    settings.language = lang;
    let fps = platform.and_then(FakePlatform::hz);
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: fps
                .map(|hz| ("vo-null-fps".to_owned(), hz.to_string()))
                .into_iter()
                .collect(),
            ..Options::headless()
        },
        Launch {
            files: vec![sample(file)],
            platform: platform.map(|p| Box::new(p.clone()) as Box<dyn PlatformProbe>),
            ..Default::default()
        },
        settings,
    );
    step_until(&mut h, "開始播放", |s| {
        s.loaded && !s.paused && s.time_pos > 0.0 && !s.tracks.is_empty()
    });
    h.run_steps(3);
    h
}

fn playing_with_platform(platform: Option<&FakePlatform>, smooth: SmoothMode) -> Harness<'static, VitascopeApp> {
    playing_smooth(
        platform,
        smooth,
        "common/mkv_multitrack.mkv",
        vitascope::i18n::Lang::ZhTw,
    )
}

/// mpv 的同步設定是預設值（一般播放）
fn sync_options_untouched(h: &Harness<'_, VitascopeApp>) {
    for name in ["video-sync", "display-fps-override"] {
        let default = prop(h, &format!("option-info/{name}/default-value"));
        assert_eq!(prop(h, name), default, "{name} 不能被改掉");
    }
    assert_eq!(prop(h, "video-sync"), "audio");
    assert!(!h.state().player().state.display_sync_active);
}

/// 等到 mpv 用這個更新率依螢幕同步
fn wait_display_sync(h: &mut Harness<'_, VitascopeApp>, override_prefix: &str) {
    step_until_app(h, &format!("依 {override_prefix} Hz 同步"), |app| {
        let p = app.player();
        p.get_string("video-sync").is_ok_and(|v| v == "display-resample")
            && p.get_string("display-fps-override")
                .is_ok_and(|v| v.starts_with(override_prefix))
            && p.state.display_sync_active
    });
}

/// 等到 mpv 改回一般播放（音訊同步）
fn wait_audio_sync(h: &mut Harness<'_, VitascopeApp>, what: &str) {
    step_until_app(h, what, |app| {
        let p = app.player();
        p.get_string("video-sync").is_ok_and(|v| v == "audio")
            && p.get_f64("display-fps-override").is_ok_and(|v| v == 0.0)
            && !p.state.display_sync_active
    });
}

fn open_playback_settings(h: &mut Harness<'_, VitascopeApp>, page: &str) {
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label(page).click();
    h.run_steps(2);
}

#[test]
fn pacing_status_with_fake_probe() {
    // 打開流暢播放、螢幕 119.88 Hz：啟動時（開檔之前）就同步設定好，第一個檔案一開始就依螢幕同步
    let fake = FakePlatform::new(Some(119.88), PowerSource::Ac);
    let mut h = playing_with_platform(Some(&fake), SmoothMode::Auto);
    let status = h.state().pacing_status().clone();
    assert_eq!(
        status.plan,
        Some(Plan::Display {
            hz: 119.88,
            vdrop: false
        }),
        "{status:?}"
    );
    assert_eq!(status.refresh.map(|r| r.hz), Some(119.88));
    assert_eq!(status.power, PowerSource::Ac);
    assert!(status.applied);
    assert_eq!(prop(&h, "display-fps-override"), "119.880000");
    assert_eq!(prop(&h, "video-sync"), "display-resample");
    wait_display_sync(&mut h, "119.88");
    // 啟動時送的兩個設定之外沒有再送
    assert_eq!(h.state().pacing_sets(), 2);
    // 開檔時通知了防呆（「跟不上」只算這個檔案）
    assert_eq!(h.state().pacing_guard_file(), 1);
    // 媒體資訊面板的「播放流暢度」：mpv 回報每格幾次更新、影片速度修正
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    h.get_by_label_contains("播放流暢度");
    h.get_by_label_contains("螢幕更新率：119.880 Hz（QueryDisplayConfig）");
    h.get_by_label_contains("電源：接上電源");
    h.get_by_label_contains("流暢播放：使用中：119.880 Hz（每格");
    h.get_by_label_contains("顯示同步：開 · 119.880 Hz");
    h.get_by_label_contains("錯時 ");
}

#[test]
fn fake_probe_battery() {
    let fake = FakePlatform::new(Some(119.88), PowerSource::Battery);
    let h = playing_with_platform(Some(&fake), SmoothMode::Auto);
    let status = h.state().pacing_status();
    assert_eq!(status.plan, Some(Plan::Audio(Reason::Battery)), "{status:?}");
    assert_eq!(status.describe(), "未使用：使用電池中");
    sync_options_untouched(&h);
    assert_eq!(h.state().pacing_sets(), 0);
    // 「一直開」：用電池也會用
    let mut h = playing_with_platform(Some(&fake), SmoothMode::Always);
    assert!(matches!(h.state().pacing_status().plan, Some(Plan::Display { .. })));
    wait_display_sync(&mut h, "119.88");
}

#[test]
fn no_probe_no_refresh() {
    // 預設設定（流暢播放關）
    let h = playing_with_platform(None, SmoothMode::default());
    let status = h.state().pacing_status();
    assert_eq!(status.plan, Some(Plan::Audio(Reason::Setting)), "{status:?}");
    assert_eq!(status.refresh, None);
    sync_options_untouched(&h);
    // 打開了，但自動測試的視窗查不到更新率
    let mut h = playing_with_platform(None, SmoothMode::Auto);
    let status = h.state().pacing_status();
    assert_eq!(status.plan, Some(Plan::Audio(Reason::NoRefresh)), "{status:?}");
    assert_eq!(status.describe(), "未使用：偵測不到這個螢幕的更新率");
    sync_options_untouched(&h);
    assert_eq!(h.state().pacing_sets(), 0);
    // 複製媒體資訊也有這幾行
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    click_context_item(&mut h, "複製媒體資訊");
    let copied = h
        .output()
        .platform_output
        .commands
        .iter()
        .find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
        .expect("有複製到剪貼簿");
    assert!(
        copied.contains("播放流暢度")
            && copied.contains("螢幕更新率：偵測不到")
            && copied.contains("流暢播放：未使用：偵測不到這個螢幕的更新率")
            && copied.contains("顯示同步：關"),
        "{copied}"
    );
}

#[test]
fn pacing_status_in_english() {
    let fake = FakePlatform::new(None, PowerSource::Unknown);
    let mut h = playing_smooth(
        Some(&fake),
        SmoothMode::Auto,
        "common/mkv_multitrack.mkv",
        vitascope::i18n::Lang::En,
    );
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(3);
    h.get_by_label_contains("Smoothness");
    h.get_by_label_contains("Refresh rate: not detected");
    h.get_by_label_contains("Power: Unknown (treated as plugged in)");
    h.get_by_label_contains("Smooth playback: Not in use: can't detect this screen's refresh rate");
    h.get_by_label_contains("Display sync: off");
}

#[test]
fn smooth_env_override_stands_down() {
    // VITASCOPE_MPV_OPTS（Options.extra）自己指定了同步方式或更新率：流暢播放完全不管，使用者的值留著
    for (name, value) in [("video-sync", "desync"), ("display-fps-override", "59.940000")] {
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.smooth = SmoothMode::Auto;
        let mut h = harness_launch_with(
            Options {
                keep_open: true,
                extra: vec![(name.into(), value.into()), ("vo-null-fps".into(), "119.88".into())],
                ..Options::headless()
            },
            Launch {
                files: vec![sample("common/mkv_multitrack.mkv")],
                platform: Some(Box::new(FakePlatform::new(Some(119.88), PowerSource::Ac))),
                ..Default::default()
            },
            settings,
        );
        step_until(&mut h, "開始播放", |s| {
            s.loaded && s.time_pos > 0.0 && !s.tracks.is_empty()
        });
        wait_real(&mut h, 0.8);
        let status = h.state().pacing_status();
        assert_eq!(status.plan, Some(Plan::Untouched), "{name}：{status:?}");
        assert_eq!(status.describe(), "已由 VITASCOPE_MPV_OPTS 指定");
        assert_eq!(prop(&h, name), value, "{name}");
        assert_eq!(h.state().pacing_sets(), 0, "{name}：一個設定都不送");
        // 選單、設定頁的選項停用
        h.get_by_label("影片畫面").click_secondary();
        h.run_steps(2);
        h.get_by_label("畫質 ⏵").hover();
        h.run_steps(3);
        let item = h.get_by_label("流暢播放（已由 VITASCOPE_MPV_OPTS 指定）");
        assert!(item.accesskit_node().is_disabled(), "{name}");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        open_playback_settings(&mut h, "播放");
        assert!(
            h.get_by_label("流暢播放（對齊螢幕更新率）")
                .accesskit_node()
                .is_disabled()
        );
        h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
        assert_eq!(prop(&h, name), value, "{name}");
    }
}

#[test]
fn smooth_pacing_off_stands_down() {
    // VITASCOPE_PACING=off（main.rs 啟動時讀進 Launch.pacing）：設定打開了也不動 mpv 的同步設定
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.smooth = SmoothMode::Auto;
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: vec![("vo-null-fps".into(), "119.88".into())],
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_multitrack.mkv")],
            platform: Some(Box::new(FakePlatform::new(Some(119.88), PowerSource::Ac))),
            pacing: vitascope::pacing::parse_overrides(Some("off")),
            ..Default::default()
        },
        settings,
    );
    step_until(&mut h, "開始播放", |s| {
        s.loaded && s.time_pos > 0.0 && !s.tracks.is_empty()
    });
    wait_real(&mut h, 0.8);
    let status = h.state().pacing_status();
    assert_eq!(status.plan, Some(Plan::Untouched), "{status:?}");
    assert_eq!(status.describe(), "未使用：已由 VITASCOPE_PACING=off 關閉");
    assert_eq!(h.state().pacing_sets(), 0);
    sync_options_untouched(&h);
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label("畫質 ⏵").hover();
    h.run_steps(3);
    assert!(
        h.get_by_label("流暢播放（VITASCOPE_PACING=off）")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn smooth_default_off_changes_nothing() {
    // 預設設定（關）：就算查得到 119.88 Hz、有影像，mpv 的同步設定一個都不送
    let fake = FakePlatform::new(Some(119.88), PowerSource::Ac);
    let mut h = playing_with_platform(Some(&fake), SmoothMode::default());
    assert_eq!(h.state().settings().smooth, SmoothMode::Off);
    wait_real(&mut h, 1.0);
    // 換螢幕更新率、拔掉電源也一樣
    fake.set(&mut h, |f| f.hz = Some(60.0));
    wait_real(&mut h, 0.6);
    fake.set(&mut h, |f| f.power = PowerSource::Battery);
    wait_real(&mut h, 0.6);
    assert_eq!(h.state().pacing_sets(), 0);
    assert_eq!(h.state().pacing_status().plan, Some(Plan::Audio(Reason::Setting)));
    sync_options_untouched(&h);
}

#[test]
fn smooth_follows_platform_probe() {
    let fake = FakePlatform::new(Some(119.88), PowerSource::Ac);
    let mut h = playing_with_platform(Some(&fake), SmoothMode::Auto);
    wait_display_sync(&mut h, "119.88");
    assert!(
        h.state().pacing_status().describe().starts_with("使用中：119.880 Hz"),
        "{}",
        h.state().pacing_status().describe()
    );

    // 拔掉電源（使用電池時暫停）：馬上改回一般播放，提示
    fake.set(&mut h, |f| f.power = PowerSource::Battery);
    step_until_app(&mut h, "提示使用電池", |app| {
        app.osd_text() == Some("使用電池：流暢播放暫停（省電）")
    });
    wait_audio_sync(&mut h, "使用電池：改回音訊同步");
    assert_eq!(h.state().pacing_status().plan, Some(Plan::Audio(Reason::Battery)));

    // 接上電源：0.5 秒後恢復，提示
    fake.set(&mut h, |f| f.power = PowerSource::Ac);
    let asked = Instant::now();
    h.run_steps(5);
    // 慢的電腦上這幾幀可能就超過 0.5 秒了，那時不檢查
    if asked.elapsed() < Duration::from_millis(400) {
        assert_eq!(prop(&h, "video-sync"), "audio", "要先維持 0.5 秒");
        assert!(h.state().pacing_status().describe().starts_with("準備中"));
    }
    step_until_app(&mut h, "提示接上電源", |app| {
        app.osd_text() == Some("接上電源：流暢播放恢復")
    });
    wait_display_sync(&mut h, "119.88");

    // 設定頁：用電池時也開（一直開）
    fake.set(&mut h, |f| f.power = PowerSource::Battery);
    wait_audio_sync(&mut h, "使用電池");
    open_playback_settings(&mut h, "播放");
    h.get_by_label_contains("未使用：使用電池中");
    h.get_by_label("使用電池時暫停（省電）").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().smooth, SmoothMode::Always);
    wait_display_sync(&mut h, "119.88");
    h.run_steps(2);
    h.get_by_label_contains("使用中：119.880 Hz");
    // 是設定改的、還在用電池：不能提示「接上電源」
    assert_ne!(h.state().osd_text(), Some("接上電源：流暢播放恢復"));

    // 查不到更新率
    fake.set(&mut h, |f| f.hz = None);
    wait_audio_sync(&mut h, "查不到更新率");
    h.run_steps(2);
    h.get_by_label("未使用：偵測不到這個螢幕的更新率");
    fake.set(&mut h, |f| f.hz = Some(119.88));
    wait_display_sync(&mut h, "119.88");

    // 設定頁關掉：馬上改回一般播放
    h.get_by_label("流暢播放（對齊螢幕更新率）").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().smooth, SmoothMode::Off);
    wait_audio_sync(&mut h, "關掉流暢播放");
    h.get_by_label("未使用：設定為關閉");
    // 「使用電池時暫停」跟著停用
    assert!(h.get_by_label("使用電池時暫停（省電）").accesskit_node().is_disabled());

    // 再打開（使用電池時暫停預設勾著）、播純音訊檔：沒有影像，一般播放
    h.get_by_label("流暢播放（對齊螢幕更新率）").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().smooth, SmoothMode::Auto);
    fake.set(&mut h, |f| f.power = PowerSource::Ac);
    wait_display_sync(&mut h, "119.88");
    drop_file(&mut h, sample("general/audio_flac.flac"));
    step_until(&mut h, "播放純音訊檔", |s| playing(s, "audio_flac.flac"));
    step_until_app(&mut h, "純音訊檔：一般播放", |app| {
        app.pacing_status().plan == Some(Plan::Audio(Reason::NoVideo))
            && app.player().get_string("video-sync").is_ok_and(|v| v == "audio")
    });
}

#[test]
fn picture_menu_has_smooth_checkbox() {
    // 沒開檔也能用（整個程式共用的設定）
    let fake = FakePlatform::new(Some(119.88), PowerSource::Ac);
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: vec![("vo-null-fps".into(), "119.88".into())],
            ..Options::headless()
        },
        Launch {
            platform: Some(Box::new(fake.clone())),
            ..Default::default()
        },
        Settings::default(),
    );
    h.run_steps(3);
    let open_menu = |h: &mut Harness<'_, VitascopeApp>| {
        // 起始畫面中間是提示文字：在左上角按右鍵
        let corner = egui::pos2(40.0, 40.0);
        h.event(egui::Event::PointerMoved(corner));
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos: corner,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.run_steps(2);
        // 「畫質」緊接在「畫面」後面
        let view = h.get_by_label("畫面 ⏵").rect();
        let picture = h.get_by_label("畫質 ⏵");
        assert!(picture.rect().top() >= view.bottom() - 1.0);
        assert!(!picture.accesskit_node().is_disabled(), "沒開檔也能用");
        picture.hover();
        h.run_steps(3);
    };
    open_menu(&mut h);
    h.get_by_label("流暢播放（119.88 Hz）").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().smooth, SmoothMode::Auto);
    assert_eq!(h.state().osd_text(), Some("流暢播放：開（119.88 Hz）"));
    // 沒有檔案時也先套用（0.5 秒後），開檔就是同步的
    step_until_app(&mut h, "套用", |app| {
        app.player()
            .get_string("video-sync")
            .is_ok_and(|v| v == "display-resample")
    });
    // 再點一次：關
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_menu(&mut h);
    h.get_by_label("流暢播放（119.88 Hz）").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().smooth, SmoothMode::Off);
    assert_eq!(h.state().osd_text(), Some("流暢播放：關"));
    // 關掉馬上生效（不等 0.5 秒）：已經送出改回音訊同步。mpv 那邊是非同步設定，等它做完再讀
    let status = h.state().pacing_status();
    assert!(
        status.applied && status.plan == Some(Plan::Audio(Reason::Setting)),
        "{status:?}"
    );
    wait_audio_sync(&mut h, "關掉流暢播放");
    // 使用電池：選單上寫「暫停」
    fake.set(&mut h, |f| f.power = PowerSource::Battery);
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_menu(&mut h);
    h.get_by_label("流暢播放（119.88 Hz）").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("流暢播放：開（使用電池，暫停）"));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_menu(&mut h);
    h.get_by_label("流暢播放（使用電池，暫停）");
}

#[test]
fn settings_playback_page_smooth_section() {
    for (lang, page, on, battery, status) in [
        (
            vitascope::i18n::Lang::ZhTw,
            "播放",
            "流暢播放（對齊螢幕更新率）",
            "使用電池時暫停（省電）",
            "未使用：設定為關閉",
        ),
        (
            vitascope::i18n::Lang::En,
            "Playback",
            "Smooth playback (match the screen's refresh rate)",
            "Pause on battery (saves power)",
            "Not in use: turned off in Settings",
        ),
    ] {
        let mut settings = Settings::default();
        settings.language = lang;
        let mut h = harness_with(None, settings);
        h.step();
        open_playback_settings(&mut h, page);
        assert!(!h.get_by_label(on).accesskit_node().is_disabled());
        assert!(h.get_by_label(battery).accesskit_node().is_disabled(), "流暢播放關著");
        h.get_by_label(status);
        // 打開：沒有螢幕資訊（自動測試）
        h.get_by_label(on).click();
        h.run_steps(2);
        assert_eq!(h.state().settings().smooth, SmoothMode::Auto);
        assert!(!h.get_by_label(battery).accesskit_node().is_disabled());
        h.get_by_label_contains(if lang == vitascope::i18n::Lang::En {
            "can't detect this screen's refresh rate"
        } else {
            "偵測不到這個螢幕的更新率"
        });
        assert_eq!(h.state().pacing_sets(), 0);
    }
}

#[test]
fn smooth_apply_failure_falls_back() {
    // mpv 不接受流暢播放的設定（這裡用不合理的更新率）：提示、這次執行改用一般播放，改了設定才再試
    let fake = FakePlatform::new(Some(119.88), PowerSource::Ac);
    let mut h = playing_with_platform(Some(&fake), SmoothMode::Auto);
    wait_display_sync(&mut h, "119.88");
    fake.set(&mut h, |f| f.hz = Some(-5.0));
    step_until_app(&mut h, "設定失敗，改回一般播放", |app| {
        app.pacing_status().plan == Some(Plan::Audio(Reason::ApplyFailed))
            && app.player().get_string("video-sync").is_ok_and(|v| v == "audio")
    });
    assert!(
        h.state()
            .osd_text()
            .is_some_and(|t| t.starts_with("無法套用 display-fps-override：")),
        "{:?}",
        h.state().osd_text()
    );
    // 恢復正常的更新率也不再試（這次執行）
    fake.set(&mut h, |f| f.hz = Some(119.88));
    wait_real(&mut h, 0.8);
    assert_eq!(prop(&h, "video-sync"), "audio");
    assert_eq!(h.state().pacing_status().plan, Some(Plan::Audio(Reason::ApplyFailed)));
    // 改了設定（關掉再打開）：重新來過
    open_playback_settings(&mut h, "播放");
    h.get_by_label("流暢播放（對齊螢幕更新率）").click();
    h.run_steps(2);
    h.get_by_label("流暢播放（對齊螢幕更新率）").click();
    h.run_steps(2);
    wait_display_sync(&mut h, "119.88");
}

#[test]
fn smooth_sets_options_in_order() {
    // 打開時先設更新率再換同步方式（不會有一瞬間用錯的更新率同步）；關掉時先換回音訊同步。
    // 送出的順序看 mpv 的紀錄檔（每個 Set property 一行）
    let dir = TempDir::new("smooth-order");
    let log = dir.0.join("mpv.log");
    let fake = FakePlatform::new(Some(119.88), PowerSource::Battery);
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.smooth = SmoothMode::Auto;
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: vec![
                ("vo-null-fps".into(), "119.88".into()),
                ("log-file".into(), log.to_string_lossy().into_owned()),
            ],
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_multitrack.mkv")],
            platform: Some(Box::new(fake.clone())),
            ..Default::default()
        },
        settings,
    );
    step_until(&mut h, "開始播放", |s| {
        s.loaded && !s.paused && s.time_pos > 0.0 && !s.tracks.is_empty()
    });
    // 用電池：啟動時是一般播放，什麼都沒送；接上電源後由每一幀的套用（非同步）打開、再拔掉關掉
    assert_eq!(h.state().pacing_sets(), 0);
    fake.set(&mut h, |f| f.power = PowerSource::Ac);
    wait_display_sync(&mut h, "119.88");
    fake.set(&mut h, |f| f.power = PowerSource::Battery);
    wait_audio_sync(&mut h, "使用電池：改回音訊同步");
    assert_eq!(h.state().pacing_sets(), 4);
    let sets = || -> Vec<String> {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once("Set property: ").map(|(_, rest)| rest))
            .filter(|rest| rest.starts_with("video-sync=") || rest.starts_with("display-fps-override="))
            .map(|rest| rest.split(" -> ").next().unwrap_or(rest).replace('"', ""))
            .collect()
    };
    // mpv 寫紀錄檔有延遲
    let start = Instant::now();
    while sets().len() < 4 && start.elapsed() < TIMEOUT {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        sets(),
        vec![
            "display-fps-override=119.880000",
            "video-sync=display-resample",
            "video-sync=audio",
            "display-fps-override=0",
        ]
    );
}

#[test]
fn smooth_passthrough_drops_frames() {
    // 音訊直通（ao=null 也接受 spdif）：聲音不能變速，改用略過或重複影格對齊螢幕
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.smooth = SmoothMode::Auto;
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: vec![
                ("vo-null-fps".into(), "119.88".into()),
                ("audio-spdif".into(), "ac3".into()),
            ],
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_hevc_ac3.mkv")],
            platform: Some(Box::new(FakePlatform::new(Some(119.88), PowerSource::Ac))),
            ..Default::default()
        },
        settings,
    );
    step_until(&mut h, "音訊直通播放中", |s| {
        s.loaded && s.time_pos > 0.0 && s.audio_spdif.as_deref() == Some("ac3")
    });
    step_until_app(&mut h, "改用 display-vdrop", |app| {
        let p = app.player();
        p.get_string("video-sync").is_ok_and(|v| v == "display-vdrop")
            && p.get_string("display-fps-override")
                .is_ok_and(|v| v.starts_with("119.88"))
    });
    let status = h.state().pacing_status();
    assert_eq!(
        status.plan,
        Some(Plan::Display {
            hz: 119.88,
            vdrop: true
        }),
        "{status:?}"
    );
    assert!(status.applied);
    assert_eq!(status.describe(), "音訊直通中：以略過或重複影格對齊螢幕");
}

// ───────────── 影像調整、控制面板 ─────────────

/// mpv 目前的影像調整值（brightness 之類的）
fn adjust_prop(app: &VitascopeApp, name: &str) -> f64 {
    app.player().get_f64(name).unwrap_or(f64::NAN)
}

/// 等 mpv 套用（影像調整是非同步設定的）
fn wait_adjust(h: &mut Harness<'_, VitascopeApp>, name: &str, value: i32) {
    step_until_app(h, &format!("mpv 的 {name} = {value}"), |app| {
        adjust_prop(app, name) == f64::from(value)
    });
}

const ADJUST_PROPS: [&str; 5] = ["brightness", "contrast", "saturation", "hue", "gamma"];

fn key_event(key: egui::Key, modifiers: egui::Modifiers, repeat: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat,
        modifiers,
    }
}

#[test]
fn picture_keys_adjust_and_reset() {
    use egui::Key;
    let dir = TempDir::new("adjust-keys");
    for lang in vitascope::i18n::Lang::ALL {
        let en = lang == vitascope::i18n::Lang::En;
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        settings.screenshot_dir = Some(dir.0.clone());
        let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
        settle(&mut h, "mkv_multitrack.mkv");
        for (key, name, value, zh_text, en_text) in [
            (Key::E, "brightness", 1, "亮度 +1", "Brightness +1"),
            (Key::W, "brightness", 0, "亮度 0", "Brightness 0"),
            (Key::W, "brightness", -1, "亮度 -1", "Brightness -1"),
            (Key::T, "contrast", 1, "對比 +1", "Contrast +1"),
            (Key::R, "contrast", 0, "對比 0", "Contrast 0"),
            (Key::R, "contrast", -1, "對比 -1", "Contrast -1"),
            (Key::U, "saturation", 1, "飽和度 +1", "Saturation +1"),
            (Key::Y, "saturation", 0, "飽和度 0", "Saturation 0"),
            (Key::Y, "saturation", -1, "飽和度 -1", "Saturation -1"),
            (Key::O, "hue", 1, "色相 +1", "Hue +1"),
            (Key::I, "hue", 0, "色相 0", "Hue 0"),
            (Key::I, "hue", -1, "色相 -1", "Hue -1"),
        ] {
            h.key_press(key);
            h.step();
            assert_eq!(
                h.state().osd_text(),
                Some(if en { en_text } else { zh_text }),
                "{key:?}"
            );
            wait_adjust(&mut h, name, value);
        }
        let a = h.state().adjust();
        assert_eq!(
            [a.brightness, a.contrast, a.saturation, a.hue, a.gamma],
            [-1, -1, -1, -1, 0]
        );
        // 按住不放（鍵盤自動重複）：每一下都算
        for repeat in [false, true, true] {
            h.event(key_event(Key::E, egui::Modifiers::NONE, repeat));
            h.step();
        }
        wait_adjust(&mut h, "brightness", 2);
        // Shift 不影響（egui 比對時忽略多按的 Shift）
        h.key_press_modifiers(egui::Modifiers::SHIFT, Key::E);
        h.step();
        wait_adjust(&mut h, "brightness", 3);
        // Q：全部還原
        h.key_press(Key::Q);
        h.step();
        assert_eq!(
            h.state().osd_text(),
            Some(if en {
                "Image adjustments reset"
            } else {
                "影像調整已還原"
            })
        );
        for name in ADJUST_PROPS {
            wait_adjust(&mut h, name, 0);
        }
        assert!(h.state().adjust().is_neutral());
        if en {
            continue;
        }
        // Ctrl（macOS：Cmd）+ E / T / I、Ctrl + Q 還是原本的功能，不會變成影像調整
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::E);
        wait_for_png(&mut h, &dir.0, 1);
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::T);
        h.run_steps(2);
        assert_eq!(h.state().settings().on_top, OnTop::Always, "Ctrl+T 視窗置頂：永遠置頂");
        h.key_press_modifiers(egui::Modifiers::COMMAND, Key::I);
        h.run_steps(2);
        h.get_by_label_contains("640×360（16:9）");
        h.key_press_modifiers(egui::Modifiers::CTRL, Key::Q);
        h.run_steps(2);
        assert!(h.state().geometry().crop.is_some(), "Ctrl+Q 裁切");
        assert!(h.state().adjust().is_neutral(), "{:?}", h.state().adjust());
        h.run_steps(5);
        for name in ADJUST_PROPS {
            assert_eq!(adjust_prop(h.state(), name), 0.0, "{name}");
        }
    }
}

#[test]
fn adjust_session_carry_and_keep() {
    let dir = TempDir::new("adjust-keep");
    let (a, b) = (dir.clip("a.mp4"), dir.clip("b.mp4"));
    let path = dir.0.join("settings.json");
    let saved = |key: &str| -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["video"][key].clone()
    };
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = harness_with(Some(a), settings);
    settle(&mut h, "a.mp4");
    for _ in 0..3 {
        h.key_press(egui::Key::E);
        h.step();
    }
    h.key_press(egui::Key::R);
    h.step();
    wait_adjust(&mut h, "brightness", 3);
    wait_adjust(&mut h, "contrast", -1);
    // 開另一個檔案：mpv 的影像調整不會還原（不在 reset-on-next-file 裡），開檔時提醒一下
    drop_file(&mut h, b.clone());
    step_until(&mut h, "開始播放 b.mp4", |s| playing(s, "b.mp4"));
    step_until_app(&mut h, "提醒影像調整", |app| {
        app.osd_text() == Some("影像調整中：亮度 +3、對比 -1（Q 還原）")
    });
    h.run_steps(3);
    assert_eq!(adjust_prop(h.state(), "brightness"), 3.0);
    assert_eq!(adjust_prop(h.state(), "contrast"), -1.0);
    // 沒勾「下次開啟時沿用」：存檔時（例如切換視窗置頂）不寫影像調整
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(2);
    assert_eq!(saved("keep_adjust"), false);
    assert_eq!(saved("adjust")["brightness"], 0);
    assert_eq!(saved("adjust")["contrast"], 0);
    // 在控制面板勾選：馬上存下這次的調整
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("下次開啟時沿用這些調整").click();
    h.run_steps(2);
    assert!(h.state().settings().video.keep_adjust);
    assert_eq!(saved("keep_adjust"), true);
    assert_eq!(saved("adjust")["brightness"], 3);
    assert_eq!(saved("adjust")["contrast"], -1);
    // 再調一下，存檔時跟著寫
    h.key_press(egui::Key::U);
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(2);
    assert_eq!(saved("adjust")["saturation"], 1);
    drop(h);

    // 下次開啟：用存下來的調整，啟動時（開檔前）就套用，開檔時提醒
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = harness_with(Some(b.clone()), settings);
    h.step();
    assert_eq!(adjust_prop(h.state(), "brightness"), 3.0, "啟動時同步套用");
    assert_eq!(adjust_prop(h.state(), "saturation"), 1.0);
    step_until_app(&mut h, "提醒影像調整", |app| {
        app.osd_text() == Some("影像調整中：亮度 +3、對比 -1、飽和度 +1（Q 還原）")
    });
    // 取消勾選：存的值清成 0，下次從 0 開始（這次執行照樣沿用）
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("下次開啟時沿用這些調整").click();
    h.run_steps(2);
    assert_eq!(saved("keep_adjust"), false);
    assert_eq!(saved("adjust")["brightness"], 0);
    assert_eq!(h.state().adjust().brightness, 3);
    assert_eq!(adjust_prop(h.state(), "brightness"), 3.0);
    drop(h);

    // 設定檔裡有值、但沒勾沿用（例如手動改的）：從 0 開始
    std::fs::write(
        &path,
        r#"{"auto_next": false, "video": {"keep_adjust": false, "adjust": {"brightness": 7}}}"#,
    )
    .unwrap();
    let mut h = harness_with(Some(b), Settings::load_from(path.clone()));
    settle(&mut h, "b.mp4");
    assert!(h.state().adjust().is_neutral());
    assert_eq!(adjust_prop(h.state(), "brightness"), 0.0);
    assert_ne!(
        h.state().osd_text().map(|t| t.starts_with("影像調整中")),
        Some(true),
        "沒有調整時不提醒"
    );
}

/// 用無障礙動作設定滑桿的值（像螢幕閱讀器那樣）
fn set_slider(h: &mut Harness<'_, VitascopeApp>, label: &str, value: f64) {
    let (target_node, target_tree) = h.get_by_label(label).accesskit_node().locate();
    h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::SetValue,
        target_tree,
        target_node,
        data: Some(egui::accesskit::ActionData::NumericValue(value)),
    }));
    h.run_steps(2);
}

/// 用滑鼠把滑桿從中間拖到最右邊。`holding`：拖到最右邊、還沒放開滑鼠時要檢查的事
fn drag_slider_to_max(
    h: &mut Harness<'_, VitascopeApp>,
    label: &str,
    holding: impl FnOnce(&mut Harness<'_, VitascopeApp>),
) {
    let rect = h.get_by_label(label).rect();
    let (from, to) = (rect.center(), egui::pos2(rect.right() + 20.0, rect.center().y));
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    for e in [
        egui::Event::PointerMoved(from),
        button(from, true),
        egui::Event::PointerMoved(egui::pos2(from.x + 10.0, from.y)),
        egui::Event::PointerMoved(to),
    ] {
        h.event(e);
        h.step();
    }
    holding(h);
    h.event(button(to, false));
    h.step();
    h.run_steps(2);
}

/// 開右鍵選單、把滑鼠移到「畫質」上（子選單打開）
fn open_picture_menu(h: &mut Harness<'_, VitascopeApp>) {
    hover_context_item(h, "畫質 ⏵");
}

/// 同上，但在影片畫面的左下角按右鍵（控制面板開著時，畫面中間會被面板蓋住）
fn open_picture_menu_from_corner(h: &mut Harness<'_, VitascopeApp>) {
    let video = h.get_by_label("影片畫面").rect();
    let pos = video.left_bottom() + egui::vec2(30.0, -30.0);
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(2);
    hover_menu_item(h, "畫質 ⏵");
}

/// 設定檔裡存的值（`video.adjust.名稱`）
fn saved_adjust(path: &std::path::Path, name: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["video"]["adjust"][name].clone()
}

#[test]
fn control_panel_alt_g_and_esc() {
    let dir = TempDir::new("control-panel");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    settings.video.keep_adjust = true;
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
    settle(&mut h, "mkv_multitrack.mkv");
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("控制面板");
    h.get_by_label("W/E 亮度・R/T 對比・Y/U 飽和度・I/O 色相");
    for name in ["亮度", "對比", "飽和度", "色相", "Gamma"] {
        assert_eq!(
            h.get_by_label(name).accesskit_node().role(),
            egui::accesskit::Role::Slider,
            "{name}"
        );
    }
    // 設定滑桿的值：馬上套用，存檔（勾了沿用）
    set_slider(&mut h, "亮度", 25.0);
    wait_adjust(&mut h, "brightness", 25);
    assert_eq!(h.state().settings().video.adjust.brightness, 25);
    assert_eq!(saved_adjust(&path, "brightness"), 25);
    // 用滑鼠拖：拖曳中就套用，放開才存檔
    drag_slider_to_max(&mut h, "Gamma", |h| {
        wait_adjust(h, "gamma", 100);
        assert_eq!(saved_adjust(&path, "gamma"), 0, "還沒放開滑鼠，不存檔");
    });
    assert_eq!(h.state().settings().video.adjust.gamma, 100);
    assert_eq!(saved_adjust(&path, "gamma"), 100, "放開滑鼠時存檔");
    // 每一列的 ↺：只還原那一項（值是 0 的那幾列不能按），也存檔
    let resets: Vec<bool> = h
        .query_all_by_label("↺")
        .map(|n| n.accesskit_node().is_disabled())
        .collect();
    assert_eq!(resets, [false, true, true, true, false], "亮度、Gamma 有調整");
    h.query_all_by_label("↺").last().unwrap().click();
    h.run_steps(2);
    wait_adjust(&mut h, "gamma", 0);
    assert_eq!(h.state().adjust().brightness, 25, "亮度不受影響");
    assert_eq!(adjust_prop(h.state(), "brightness"), 25.0);
    assert_eq!(saved_adjust(&path, "gamma"), 0, "按 ↺ 時存檔");
    // 按鍵改的值，滑桿跟著變
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.get_by_label("亮度").accesskit_node().numeric_value(), Some(26.0));
    // 全部還原
    h.get_by_label("全部還原（Q）").click();
    h.run_steps(2);
    for name in ADJUST_PROPS {
        wait_adjust(&mut h, name, 0);
    }
    assert!(h.get_by_label("全部還原（Q）").accesskit_node().is_disabled());
    // 全螢幕時 Esc 先關面板，再按一次才離開全螢幕
    set_fullscreen(&mut h, true);
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(
        !cmds.contains(&egui::ViewportCommand::Fullscreen(false)),
        "面板開著時 Esc 只關面板：{cmds:?}"
    );
    h.run_steps(2);
    assert!(h.query_by_label("控制面板").is_none(), "Esc 關閉控制面板");
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(
        cmds.contains(&egui::ViewportCommand::Fullscreen(false)),
        "面板關了，Esc 離開全螢幕：{cmds:?}"
    );
    set_fullscreen(&mut h, false);
    // Alt+G 開、再按一次關
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("控制面板");
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    assert!(h.query_by_label("控制面板").is_none());
    // 右鍵選單「畫質」：沒有調整時「還原影像調整」不能按；「影像調整…」打開面板
    open_picture_menu(&mut h);
    assert!(h.get_by_label_contains("還原影像調整").accesskit_node().is_disabled());
    h.get_by_label_contains("影像調整…").click();
    h.run_steps(2);
    h.get_by_label("控制面板");
    h.get_by_label("亮度");
    // 面板開著時再選一次「影像調整…」：還是開著（選單只負責打開，Alt+G 才是開關）
    open_picture_menu_from_corner(&mut h);
    h.get_by_label_contains("影像調整…").click();
    h.run_steps(2);
    h.get_by_label("控制面板");
    // 有調整時可以從選單還原
    h.key_press(egui::Key::T);
    h.run_steps(2);
    wait_adjust(&mut h, "contrast", 1);
    // 小影片的視窗裡，面板會蓋到畫面中間（右鍵會按在面板上）：先關掉
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_picture_menu(&mut h);
    let reset = h.get_by_label_contains("還原影像調整");
    assert!(!reset.accesskit_node().is_disabled());
    reset.click();
    h.run_steps(2);
    wait_adjust(&mut h, "contrast", 0);
    assert_eq!(h.state().osd_text(), Some("影像調整已還原"));
}

/// 勾了「下次開啟時沿用」、存著亮度 `brightness` 的設定（只在記憶體裡）
fn kept_brightness(brightness: i32) -> Settings {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.keep_adjust = true;
    settings.video.adjust.brightness = brightness;
    settings
}

fn shows_adjust_reminder(app: &VitascopeApp) -> bool {
    app.osd_text().is_some_and(|t| t.starts_with("影像調整中"))
}

/// 一直跑介面幀直到條件成立；期間都不能出現影像調整的提醒
fn step_without_reminder(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn(&State) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        assert!(
            !shows_adjust_reminder(h.state()),
            "{what}：不該提醒影像調整（{:?}）",
            h.state().osd_text()
        );
        if cond(&h.state().player().state) {
            return;
        }
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// 開檔時的影像調整提醒不能蓋掉開檔後才出現、比較要緊的提示
#[test]
fn adjust_reminder_gives_way_to_next_file_and_resume() {
    // 播放清單的下一個：「下一個（2/3）：第2集.mp4」
    let dir = three_episodes("adjust-next");
    let mut h = harness_with(Some(dir.0.join("第1集.mp4")), kept_brightness(3));
    step_until_app(&mut h, "開檔時提醒影像調整", |app| {
        app.osd_text() == Some("影像調整中：亮度 +3（Q 還原）")
    });
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    h.key_press(egui::Key::PageDown);
    h.step();
    assert_eq!(h.state().osd_text(), Some("下一個（2/3）：第2集.mp4"));
    step_without_reminder(&mut h, "換到第2集", |s| playing(s, "第2集.mp4"));
    for _ in 0..10 {
        step_without_reminder(&mut h, "第2集載入後", |_| true);
    }
    drop(h);

    // 續播：「從 00:30 繼續播放（Home 從頭播放）」
    let long = sample("common/mp4_long.mp4");
    let mut h = harness_with(Some(long.clone()), kept_brightness(3));
    step_until(&mut h, "開始播放", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos > 0.0
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(&mut h, "前進 30 秒", |s| s.time_pos >= 29.0);
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until_app(&mut h, "沒有續播時照樣提醒", |app| {
        playing(&app.player().state, "mp4_h264_aac.mp4") && shows_adjust_reminder(app)
    });
    // 等這個提醒消失，下面才分得出是不是又提醒了
    step_until_app(&mut h, "提醒消失", |app| app.osd_text().is_none());
    drop_file(&mut h, long);
    step_without_reminder(&mut h, "從上次的位置繼續", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos >= 28.0
    });
    let osd = h.state().osd_text().unwrap_or_default().to_owned();
    assert!(osd.contains("繼續播放（Home 從頭播放）"), "{osd}");
}

#[test]
fn adjust_reminder_skips_audio_only_files() {
    let mut h = harness_with(Some(sample("general/audio_flac.flac")), kept_brightness(3));
    step_without_reminder(&mut h, "播放純音訊檔", |s| {
        playing(s, "audio_flac.flac") && s.time_pos > 0.3
    });
    for _ in 0..10 {
        step_without_reminder(&mut h, "播放中", |_| true);
    }
    assert_eq!(h.state().adjust().brightness, 3, "調整照樣沿用，只是不提醒");
}

// VITASCOPE_MPV_OPTS（這裡用 `Options.extra`）指定的影像調整：以 mpv 的值為準，影戲不去改它，也不存檔
#[test]
fn adjust_set_by_mpv_opts_is_left_alone() {
    let dir = TempDir::new("adjust-locked");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    settings.video.keep_adjust = true;
    settings.video.adjust.brightness = 5;
    let mut h = harness_launch_with(
        Options {
            extra: vec![("brightness".into(), "20".into())],
            keep_open: true,
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_multitrack.mkv")],
            ..Default::default()
        },
        settings,
    );
    settle(&mut h, "mkv_multitrack.mkv");
    assert_eq!(h.state().adjust().brightness, 20, "面板的數字跟 mpv 一致");
    assert_eq!(adjust_prop(h.state(), "brightness"), 20.0);
    assert!(!shows_adjust_reminder(h.state()), "{:?}", h.state().osd_text());
    // 按鍵：說明原因，不改
    h.key_press(egui::Key::E);
    h.step();
    assert_eq!(h.state().osd_text(), Some("亮度：已由 VITASCOPE_MPV_OPTS 指定"));
    // 其他項目照常
    h.key_press(egui::Key::T);
    h.step();
    wait_adjust(&mut h, "contrast", 1);
    assert_eq!(adjust_prop(h.state(), "brightness"), 20.0);
    // Q 只還原其他項目
    h.key_press(egui::Key::Q);
    h.step();
    wait_adjust(&mut h, "contrast", 0);
    h.run_steps(3);
    assert_eq!(adjust_prop(h.state(), "brightness"), 20.0);
    assert_eq!(h.state().adjust().brightness, 20);
    // 控制面板：亮度的滑桿停用，其他可以調；只剩被指定的項目時「全部還原」不能按
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    assert!(h.get_by_label("亮度").accesskit_node().is_disabled());
    assert!(!h.get_by_label("對比").accesskit_node().is_disabled());
    assert!(h.get_by_label("全部還原（Q）").accesskit_node().is_disabled());
    // 存檔（切換視窗置頂）：設定檔裡的亮度還是使用者自己存的 5
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(2);
    assert_eq!(saved_adjust(&path, "brightness"), 5);
}

// 「設定 → 畫質」：目前調整的摘要、「下次開啟時沿用這些調整」勾選與取消都馬上存檔
#[test]
fn picture_page_keep_checkbox_saves() {
    let dir = TempDir::new("adjust-page");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = harness_with(None, settings);
    h.step();
    for _ in 0..2 {
        h.key_press(egui::Key::E);
        h.step();
    }
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    h.get_by_label("亮度、對比、飽和度、色相、Gamma：亮度 +2");
    h.get_by_label("下次開啟時沿用這些調整").click();
    h.run_steps(2);
    assert!(h.state().settings().video.keep_adjust);
    let saved = |key: &str| -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["video"][key].clone()
    };
    assert_eq!(saved("keep_adjust"), true);
    assert_eq!(saved("adjust")["brightness"], 2);
    h.get_by_label("下次開啟時沿用這些調整").click();
    h.run_steps(2);
    assert!(!h.state().settings().video.keep_adjust);
    assert_eq!(saved("keep_adjust"), false);
    assert_eq!(saved("adjust")["brightness"], 0);
    assert_eq!(h.state().adjust().brightness, 2, "這次執行照樣沿用");
}

#[test]
fn picture_page_renders_in_english() {
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    let mut h = harness_with(None, settings);
    h.step();
    h.key_press(egui::Key::E);
    h.step();
    assert_eq!(h.state().osd_text(), Some("Brightness +1"));
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("Video quality").click();
    h.run_steps(2);
    h.get_by_label("Image adjustments");
    h.get_by_label("Brightness, contrast, saturation, hue, gamma: Brightness +1");
    h.get_by_label("Keep these adjustments next time");
    h.get_by_label("Image adjustments…").click();
    h.run_steps(2);
    h.get_by_label("Control Panel");
    assert_eq!(
        h.get_by_label("Brightness").accesskit_node().role(),
        egui::accesskit::Role::Slider
    );
    h.get_by_label("Reset all (Q)");
    h.get_by_label("W/E brightness · R/T contrast · Y/U saturation · I/O hue");
    assert!(h.query_by_label_contains("亮度").is_none(), "沒有中文");
}

/// 設定頁改成可以編輯、一個指令一列（A3）：原本找「W / E」這種成對的說明，改成找每一列的名稱和按鍵按鈕
#[test]
fn shortcuts_page_lists_the_picture_keys() {
    let alt = if cfg!(target_os = "macos") { "Option" } else { "Alt" };
    for (lang, page, rows) in [
        (
            vitascope::i18n::Lang::ZhTw,
            "快捷鍵",
            [
                "亮度 −",
                "亮度 +",
                "對比 −",
                "對比 +",
                "飽和度 −",
                "飽和度 +",
                "色相 −",
                "色相 +",
                "影像調整還原",
                "控制面板",
            ],
        ),
        (
            vitascope::i18n::Lang::En,
            "Shortcuts",
            [
                "Brightness −",
                "Brightness +",
                "Contrast −",
                "Contrast +",
                "Saturation −",
                "Saturation +",
                "Hue −",
                "Hue +",
                "Reset image adjustments",
                "Control panel",
            ],
        ),
    ] {
        let mut settings = Settings::default();
        settings.language = lang;
        let mut h = harness_with(None, settings);
        h.step();
        h.key_press(egui::Key::F5);
        h.run_steps(2);
        h.get_by_label(page).click();
        h.run_steps(2);
        for key in ["W", "E", "R", "T", "Y", "U", "I", "O", "Q", &format!("{alt}+G")] {
            h.get_by_label(key);
        }
        for row in rows {
            h.get_by_label(row);
        }
    }
}

// ───────────── 快捷鍵設定頁、PotPlayer 預設組 ─────────────

const RECORDING: &str = "請按下新的按鍵…（Esc 取消）";

/// 打開「設定 → 快捷鍵」（`page` 是分頁的名稱，看介面語言）；`search` 不是空的就輸入搜尋，
/// 只留下符合的指令（按鈕才不會在捲動區外面、「+」這種按鈕才只有一個）
fn shortcuts_page(h: &mut Harness<'_, VitascopeApp>, page: &str, search: &str) {
    if h.query_by_label(page).is_none() {
        h.key_press(egui::Key::F5);
        h.run_steps(2);
    }
    h.get_by_label(page).click();
    h.run_steps(2);
    if !search.is_empty() {
        h.get_by_role(egui::accesskit::Role::TextInput).focus();
        h.run_steps(1);
        h.get_by_role(egui::accesskit::Role::TextInput).type_text(search);
        h.run_steps(2);
    }
}

/// 同一幀按下、放開（含修飾鍵），回傳這一幀送給視窗的指令
fn press_mods_and_get_commands(
    h: &mut Harness<'_, VitascopeApp>,
    modifiers: egui::Modifiers,
    key: egui::Key,
) -> Vec<egui::ViewportCommand> {
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        });
    }
    h.step();
    viewport_commands(h)
}

#[test]
fn shortcut_record_adds_a_chord() {
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "播放／暫停");
    h.get_by_label("+").click();
    h.run_steps(2);
    h.get_by_label(RECORDING);
    // 只按了修飾鍵還不算
    h.key_press(egui::Key::ShiftLeft);
    h.run_steps(2);
    h.get_by_label(RECORDING);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::K);
    h.run_steps(2);
    assert!(h.query_by_label(RECORDING).is_none(), "錄好了");
    assert_eq!(h.state().settings().keys.custom["toggle-pause"], ["Space", "Shift+K"]);
    h.get_by_label("Shift+K");
    h.get_by_label("•");
    // 馬上生效（設定視窗開著也一樣）
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::K);
    step_until(&mut h, "Shift+K 暫停", |s| s.paused);
    // 這一列還原：Shift+K 不再有作用，空白鍵照舊
    h.get_by_label("↺").click();
    h.run_steps(2);
    assert!(h.state().settings().keys.custom.is_empty());
    assert!(h.query_by_label("Shift+K").is_none());
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::K);
    h.run_steps(5);
    wait_real(&mut h, 0.3);
    assert!(h.state().player().state.paused, "Shift+K 不再切換");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "空白鍵繼續播放", |s| !s.paused);
    // 移除一組按鍵：空白鍵不再暫停，存成「不指定」
    h.get_by_label("×").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.custom["toggle-pause"], Vec::<String>::new());
    h.get_by_label("（未指定）");
    h.key_press(egui::Key::Space);
    h.run_steps(5);
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.paused, "空白鍵不再暫停");
}

#[test]
fn shortcut_conflict_moves_the_key() {
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "播放／暫停");
    // 取消：兩個指令都不變
    h.get_by_label("+").click();
    h.run_steps(2);
    h.key_press(egui::Key::M);
    h.run_steps(2);
    h.get_by_label("M 已經用在「靜音」。");
    h.get_by_label("取消").click();
    h.run_steps(2);
    assert!(h.query_by_label_contains("已經用在").is_none());
    assert!(h.state().settings().keys.custom.is_empty());
    // 改到這裡：M 變成播放／暫停，靜音沒有按鍵
    h.get_by_label("+").click();
    h.run_steps(2);
    h.key_press(egui::Key::M);
    h.run_steps(2);
    h.get_by_label("改到這裡").click();
    h.run_steps(2);
    let keys = &h.state().settings().keys;
    assert_eq!(keys.custom["toggle-pause"], ["Space", "M"]);
    assert_eq!(keys.custom["toggle-mute"], Vec::<String>::new());
    assert!(
        h.state()
            .keymap()
            .chords(vitascope::keymap::Command::ToggleMute)
            .is_empty()
    );
    h.key_press(egui::Key::M);
    step_until(&mut h, "M 暫停", |s| s.paused);
    assert!(!h.state().player().state.muted, "M 不再是靜音");
}

#[test]
fn reserved_chord_is_refused() {
    let mac = cfg!(target_os = "macos");
    let cmd = if mac { "Cmd" } else { "Ctrl" };
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "播放／暫停");
    h.get_by_label("+").click();
    h.run_steps(2);
    h.get_by_label(&format!("{cmd}+V、{cmd}+X 不能指定"));
    if cfg!(target_os = "windows") {
        h.get_by_label("Ctrl+Insert、Shift+Insert、Shift+Delete 會當成 Ctrl+C、Ctrl+V、Ctrl+X");
    }
    // 真正的程式裡 Ctrl+V、Ctrl+X 不是按鍵事件：egui-winit 送的是貼上（剪貼簿有文字時）、剪下
    for (event, key) in [(egui::Event::Paste("文字".into()), "V"), (egui::Event::Cut, "X")] {
        h.event(event);
        h.run_steps(2);
        h.get_by_label(&format!("{cmd}+{key} 是系統保留的按鍵，不能指定"));
        h.get_by_label(RECORDING);
    }
    // 原始的按鍵事件（介面測試、其他平台）也一樣
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::V);
    h.run_steps(2);
    h.get_by_label(&format!("{cmd}+V 是系統保留的按鍵，不能指定"));
    if mac {
        h.key_press_modifiers(egui::Modifiers::MAC_CMD | egui::Modifiers::COMMAND, egui::Key::Q);
        h.run_steps(2);
        h.get_by_label("Cmd+Q 是系統保留的按鍵，不能指定");
    }
    assert!(h.state().settings().keys.custom.is_empty());
    // Ctrl+C 從「複製」來，可以指定：它是擷取畫面（剪貼簿）的，所以先問
    h.event(egui::Event::Copy);
    h.run_steps(2);
    h.get_by_label(&format!("{cmd}+C 已經用在「擷取畫面（剪貼簿）」。"));
    h.get_by_label("取消").click();
    h.run_steps(2);
    assert!(h.state().settings().keys.custom.is_empty());
    assert!(!h.state().player().state.paused);
}

#[test]
fn recording_esc_cancels_and_keeps_the_window_open() {
    let mut h = playing_multitrack();
    set_fullscreen(&mut h, true);
    shortcuts_page(&mut h, "快捷鍵", "播放／暫停");
    h.get_by_label("+").click();
    h.run_steps(2);
    h.get_by_label(RECORDING);
    // Esc 只取消錄按鍵：設定視窗還開著、不離開全螢幕
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(!cmds.contains(&egui::ViewportCommand::Fullscreen(false)), "{cmds:?}");
    h.run_steps(2);
    assert!(h.query_by_label(RECORDING).is_none());
    h.get_by_label("一般");
    assert!(h.state().settings().keys.custom.is_empty());
    // 下一個 Esc 才關設定視窗
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("一般").is_none());
    // 錄的時候 F5 也是被錄的按鍵，不是開關設定視窗
    shortcuts_page(&mut h, "快捷鍵", "播放／暫停");
    h.get_by_label("+").click();
    h.run_steps(2);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    assert!(h.query_by_label("一般").is_some(), "設定視窗還開著");
    h.get_by_label("F5 已經用在「設定」。");
}

#[test]
fn potplayer_preset_rebinds_f_and_d() {
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "");
    h.get_by_label("PotPlayer 風格").click();
    h.run_steps(2);
    assert_eq!(
        h.state().settings().keys.preset,
        vitascope::keymap::KeyPreset::Potplayer
    );
    assert!(h.query_by_label_contains("你改過的").is_none(), "沒有改過的快捷鍵");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // F / D 逐格
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    let t0 = h.state().player().state.time_pos;
    h.key_press(egui::Key::F);
    step_until(&mut h, "F 逐格前進", |s| s.paused && s.time_pos > t0 + 0.01);
    let t1 = h.state().player().state.time_pos;
    assert!(t1 - t0 < 0.2, "只前進一格：{t0} → {t1}");
    h.key_press(egui::Key::D);
    step_until(&mut h, "D 逐格後退", |s| s.paused && s.time_pos < t1 - 0.01);
    // Enter、Alt+Enter 全螢幕
    for mods in [egui::Modifiers::NONE, egui::Modifiers::ALT] {
        let cmds = press_mods_and_get_commands(&mut h, mods, egui::Key::Enter);
        assert!(
            cmds.contains(&egui::ViewportCommand::Fullscreen(true)),
            "{mods:?} {cmds:?}"
        );
    }
    // Backspace 從頭播放（播放清單沒開）
    h.key_press(egui::Key::ArrowRight);
    step_until(&mut h, "前進 5 秒", |s| s.time_pos > 4.0);
    h.key_press(egui::Key::Backspace);
    step_until(&mut h, "Backspace 從頭播放", |s| !s.paused && s.time_pos < 2.0);
    // 選單上的按鍵跟著換
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    assert!(h.query_by_label("全螢幕 Enter").is_some());
    assert!(h.query_by_label("逐格前進 F").is_some());
}

#[test]
fn potplayer_ab_keys_set_points() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.preset = vitascope::keymap::KeyPreset::Potplayer;
    let mut h = playing_multitrack_with(settings);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    let t1 = h.state().player().state.time_pos;
    // [ 設定起點：就是按下時的時間（不是字幕提早）
    h.key_press(egui::Key::OpenBracket);
    step_until(&mut h, "[ 設定起點", |s| {
        s.ab_loop[0].is_some_and(|a| (a - t1).abs() < 0.05) && s.ab_loop[1].is_none()
    });
    assert!(h.state().osd_text().unwrap().starts_with("A-B 重播：起點"));
    assert_eq!(h.state().player().state.sub_delay, 0.0);
    h.key_press(egui::Key::ArrowRight);
    step_until(&mut h, "前進", |s| s.time_pos > t1 + 3.0);
    h.run_steps(5);
    let t2 = h.state().player().state.time_pos;
    h.key_press(egui::Key::CloseBracket);
    step_until(&mut h, "] 設定終點", |s| {
        s.ab_loop[0].is_some_and(|a| (a - t1).abs() < 0.05) && s.ab_loop[1].is_some_and(|b| (b - t2).abs() < 0.05)
    });
    assert!(h.state().osd_text().unwrap().contains('→'));
    // \ 取消
    h.key_press(egui::Key::Backslash);
    step_until(&mut h, "\\ 取消", |s| s.ab_loop == [None, None]);
    assert_eq!(h.state().osd_text(), Some("取消 A-B 重播"));
    // 只設終點；再設一個在它後面的起點：舊的終點拿掉（重新開始一段）
    h.key_press(egui::Key::CloseBracket);
    step_until(&mut h, "只有終點", |s| {
        s.ab_loop[0].is_none() && s.ab_loop[1].is_some()
    });
    assert!(h.state().osd_text().unwrap().starts_with("A-B 重播：終點"));
    h.key_press(egui::Key::ArrowRight);
    step_until(&mut h, "再前進", |s| s.time_pos > t2 + 3.0);
    h.run_steps(5);
    h.key_press(egui::Key::OpenBracket);
    step_until(&mut h, "新的起點、沒有終點", |s| {
        s.ab_loop[0].is_some_and(|a| a > t2 + 3.0) && s.ab_loop[1].is_none()
    });
    // 字幕同步：Shift+. 延後、Shift+, 提早、/ 歸零
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Period);
    }
    step_until(&mut h, "Shift+. 兩次", |s| close_to(s.sub_delay, 0.2));
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Comma);
    step_until(&mut h, "Shift+, 一次", |s| close_to(s.sub_delay, 0.1));
    h.key_press(egui::Key::Slash);
    step_until(&mut h, "/ 歸零", |s| close_to(s.sub_delay, 0.0));
}

#[test]
fn reset_to_preset_asks_first_and_the_dialog_takes_the_keys() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings
        .keys
        .custom
        .insert("toggle-pause".into(), vec!["Space".into(), "K".into()]);
    settings.keys.custom.insert("next-file".into(), vec!["N".into()]);
    let mut h = playing_multitrack_with(settings);
    shortcuts_page(&mut h, "快捷鍵", "");
    h.get_by_label("你改過的 2 個快捷鍵（標 •）換預設組時照樣保留");
    h.get_by_label("還原成預設組…").click();
    h.run_steps(2);
    h.get_by_label("把所有快捷鍵還原成「影戲」的預設值？");
    // 對話框開著：空白鍵不會暫停，Esc 只關對話框
    h.key_press(egui::Key::Space);
    h.run_steps(3);
    wait_real(&mut h, 0.3);
    assert!(!h.state().player().state.paused, "對話框開著時空白鍵不能暫停");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label_contains("預設值？").is_none(), "Esc 關掉對話框");
    h.get_by_label("一般");
    assert_eq!(h.state().settings().keys.custom.len(), 2, "取消：不變");
    // 再開一次，按「還原」
    h.get_by_label("還原成預設組…").click();
    h.run_steps(2);
    h.get_by_label("還原").click();
    h.run_steps(2);
    assert!(h.state().settings().keys.custom.is_empty());
    assert_eq!(h.state().keymap().hint(vitascope::keymap::Command::NextFile), "PgDn");
    assert!(h.query_by_label_contains("你改過的").is_none());
    // 對話框關掉之後按鍵照常
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
}

#[test]
fn non_repeatable_commands_ignore_auto_repeat() {
    let mut h = playing_multitrack();
    let key = |h: &mut Harness<'_, VitascopeApp>, key, pressed| {
        h.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    };
    // 按住空白鍵：第一下暫停，之後的自動重複（egui 看到沒放開的鍵又按下，標成重複）不再切換
    for _ in 0..2 {
        key(&mut h, egui::Key::Space, true);
    }
    key(&mut h, egui::Key::Space, false);
    wait_real(&mut h, 0.3);
    assert!(h.state().player().state.paused, "只暫停一次");
    // 音量這類照樣重複：按住 ↓ 三下
    let v0 = h.state().player().state.volume;
    for _ in 0..3 {
        key(&mut h, egui::Key::ArrowDown, true);
    }
    key(&mut h, egui::Key::ArrowDown, false);
    step_until(&mut h, "音量 −15", |s| close_to(s.volume, v0 - 15.0));
}

/// 按著鍵的時候切到別的視窗（例如 Ctrl+O 開了檔案對話框）就收不到放開：回來再按同一個鍵是新的一下，不是自動重複
#[test]
fn a_key_held_when_the_window_loses_focus_works_again_afterwards() {
    let mut h = playing_multitrack();
    let space = |h: &mut Harness<'_, VitascopeApp>| {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    };
    // 按下空白鍵（暫停），還沒放開視窗就失去焦點
    space(&mut h);
    step_until(&mut h, "暫停", |s| s.paused);
    h.event(egui::Event::WindowFocused(false));
    h.step();
    h.event(egui::Event::WindowFocused(true));
    h.step();
    // 回來再按：繼續播放
    space(&mut h);
    step_until(&mut h, "繼續播放", |s| !s.paused);
}

#[test]
fn menu_hints_follow_the_keymap() {
    let dir = TempDir::new("menu-hints");
    for name in ["第1集.mp4", "第2集.mp4"] {
        dir.clip(name);
    }
    let mut h = harness(Some(dir.0.join("第1集.mp4")));
    step_until_app(&mut h, "掃描到兩個影片", |app| playlist_len(app) == 2);
    settle(&mut h, "第1集.mp4");
    // 把下一個檔案的 PgDn 換成 N
    shortcuts_page(&mut h, "快捷鍵", "下一個檔案");
    h.get_by_label("PgDn").click();
    h.run_steps(2);
    h.get_by_label(RECORDING);
    h.key_press(egui::Key::N);
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.custom["next-file"], ["N"]);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 控制列按鈕的提示
    h.get_by_label("⏭").hover();
    h.run_steps(3);
    h.get_by_label("下一個檔案（N）");
    // 右鍵選單
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    assert!(h.query_by_label("下一個檔案 N").is_some());
    assert!(h.query_by_label("下一個檔案 PgDn").is_none());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::N);
    step_until(&mut h, "N → 第2集", |s| playing(s, "第2集.mp4"));
}

/// 新的指令（預設沒有按鍵）指定按鍵後都有作用
#[test]
fn new_commands_work_once_bound() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    for (id, key) in [
        ("sub-delay-reset", "F13"),
        ("audio-delay-reset", "F14"),
        ("next-audio-track", "F15"),
        ("next-subtitle", "F16"),
        ("gamma-up", "F17"),
        ("gamma-down", "F18"),
        ("fill-window", "F19"),
        ("toggle-eq", "F20"),
        ("toggle-smooth", "F21"),
        ("ab-set-start", "F22"),
        ("ab-clear", "F23"),
        ("load-subtitle", "F24"),
        ("screenshot-as", "F25"),
    ] {
        settings.keys.custom.insert(id.into(), vec![key.into()]);
    }
    let mut h = playing_multitrack_with(settings);
    // 延遲歸零
    h.key_press(egui::Key::CloseBracket);
    h.key_press(egui::Key::Equals);
    step_until(&mut h, "字幕、音訊延遲 +0.1", |s| {
        close_to(s.sub_delay, 0.1) && close_to(s.audio_delay, 0.1)
    });
    h.key_press(egui::Key::F13);
    step_until(&mut h, "字幕延遲歸零", |s| close_to(s.sub_delay, 0.0));
    h.key_press(egui::Key::F14);
    step_until(&mut h, "音訊延遲歸零", |s| close_to(s.audio_delay, 0.0));
    // 下一條音軌：日本語 → 國語 → 日本語
    let audio = |s: &State| s.selected(TrackKind::Audio).and_then(|t| t.title.clone());
    step_until(&mut h, "第一條音軌", |s| audio(s).as_deref() == Some("日本語"));
    h.key_press(egui::Key::F15);
    step_until(&mut h, "換成國語", |s| audio(s).as_deref() == Some("國語"));
    h.key_press(egui::Key::F15);
    step_until(&mut h, "繞回日本語", |s| audio(s).as_deref() == Some("日本語"));
    // 下一個字幕：第一個 → 第二個 → 關閉 → 第一個
    let subs: Vec<i64> = h
        .state()
        .player()
        .state
        .tracks_of(TrackKind::Sub)
        .map(|t| t.id)
        .collect();
    let (first, second) = (subs[0], subs[1]);
    step_until(&mut h, "第一個字幕", |s| s.sid == Some(first));
    h.key_press(egui::Key::F16);
    step_until(&mut h, "第二個字幕", |s| s.sid == Some(second));
    h.key_press(egui::Key::F16);
    step_until(&mut h, "關閉字幕", |s| s.sid.is_none());
    assert_eq!(h.state().osd_text(), Some("字幕：關閉"));
    h.key_press(egui::Key::F16);
    step_until(&mut h, "回到第一個", |s| s.sid == Some(first));
    // Gamma
    h.key_press(egui::Key::F17);
    h.key_press(egui::Key::F17);
    h.key_press(egui::Key::F18);
    h.run_steps(2);
    assert_eq!(h.state().adjust().gamma, 1);
    // 填滿視窗、等化器、流暢播放
    h.key_press(egui::Key::F19);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("填滿視窗：開啟"));
    let eq = h.state().settings().audio.eq.enabled;
    h.key_press(egui::Key::F20);
    h.run_steps(2);
    assert_eq!(h.state().settings().audio.eq.enabled, !eq);
    let smooth = h.state().settings().smooth;
    h.key_press(egui::Key::F21);
    h.run_steps(2);
    assert_ne!(h.state().settings().smooth, smooth);
    // A-B：只設起點，再取消
    h.key_press(egui::Key::F22);
    step_until(&mut h, "設定起點", |s| s.ab_loop[0].is_some());
    h.key_press(egui::Key::F23);
    step_until(&mut h, "取消", |s| s.ab_loop == [None, None]);
    // 載入字幕檔…、另存截圖…：開的是各自的對話框（取消）
    let seen = record_dialogs(&mut h, |_| None);
    // 先跑幾幀讓按鍵生效：對話框在背景執行緒開，沒開之前 dialogs_done 會馬上回傳
    h.key_press(egui::Key::F24);
    h.run_steps(2);
    assert_eq!(dialogs_done(&mut h, &seen), [(DialogKind::LoadSubtitle, Pick::File)]);
    h.key_press(egui::Key::F25);
    h.run_steps(2);
    assert_eq!(
        dialogs_done(&mut h, &seen),
        [(DialogKind::ScreenshotSaveAs, Pick::Save)]
    );
}

#[test]
fn shortcuts_page_in_english() {
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    settings.auto_next = false;
    let mut h = playing_multitrack_with(settings);
    shortcuts_page(&mut h, "Shortcuts", "");
    for label in [
        "Preset:",
        "VitaScope",
        "PotPlayer style",
        "Reset to the preset…",
        "Fixed keys (can't be changed)",
        "A-B loop: set start",
        "Reset subtitle delay",
        "Next audio track",
        "Save screenshot as…",
    ] {
        h.get_by_label(label);
    }
    shortcuts_page(&mut h, "Shortcuts", "Play / pause");
    let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    h.get_by_label("+").click();
    h.run_steps(2);
    h.get_by_label("Press the new key… (Esc to cancel)");
    h.get_by_label(&format!("{cmd}+V and {cmd}+X can't be assigned"));
    h.event(egui::Event::Cut);
    h.run_steps(2);
    h.get_by_label(&format!("{cmd}+X is reserved by the system and can't be assigned"));
    h.key_press(egui::Key::M);
    h.run_steps(2);
    h.get_by_label("M is already used for “Mute”.");
    h.get_by_label("Use it here").click();
    h.run_steps(2);
    h.get_by_label("Your 2 changed shortcuts (marked •) are kept when you switch presets");
    h.get_by_label("Reset to the preset…").click();
    h.run_steps(2);
    h.get_by_label("Reset all shortcuts to the “VitaScope” defaults?");
    h.get_by_label("Reset").click();
    h.run_steps(2);
    assert!(h.state().settings().keys.custom.is_empty());
    // 只改了一個：英文是單數
    h.get_by_label("+").click();
    h.run_steps(2);
    h.key_press(egui::Key::K);
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.override_count(), 1);
    h.get_by_label("Your 1 changed shortcut (marked •) is kept when you switch presets");
}

/// 固定的按鍵清單：macOS 的 Backspace 只在快捷鍵沒用到它時才是「從清單移除」（PotPlayer 風格是從頭播放）
#[test]
fn fixed_keys_list_follows_the_backspace_binding() {
    let mac = cfg!(target_os = "macos");
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "");
    h.get_by_label(if mac { "Delete / Backspace" } else { "Delete" });
    h.get_by_label("PotPlayer 風格").click();
    h.run_steps(2);
    h.get_by_label("Delete");
    assert!(h.query_by_label("Delete / Backspace").is_none());
}

/// 換預設組：自己改過的照樣保留，而且比預設組優先（PotPlayer 的 F 是逐格前進，但自己把逐格前進改成 G 了）
#[test]
fn switching_presets_keeps_changed_keys() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.custom.insert("next-file".into(), vec!["N".into()]);
    settings.keys.custom.insert("frame-next".into(), vec!["G".into()]);
    let custom = settings.keys.custom.clone();
    let mut h = playing_multitrack_with(settings);
    shortcuts_page(&mut h, "快捷鍵", "");
    h.get_by_label("你改過的 2 個快捷鍵（標 •）換預設組時照樣保留");
    h.get_by_label("PotPlayer 風格").click();
    h.run_steps(2);
    assert_eq!(
        h.state().settings().keys.preset,
        vitascope::keymap::KeyPreset::Potplayer
    );
    assert_eq!(h.state().settings().keys.custom, custom, "改過的不變");
    h.get_by_label("你改過的 2 個快捷鍵（標 •）換預設組時照樣保留");
    let keymap = h.state().keymap();
    assert_eq!(
        keymap.lookup(egui::Key::N, egui::Modifiers::NONE),
        Some(vitascope::keymap::Command::NextFile)
    );
    assert_eq!(keymap.hint(vitascope::keymap::Command::NextFile), "N");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    // G 逐格前進（自己改的）；F 沒有作用（預設組的 F 被自己改的取代）；D 是 PotPlayer 的逐格後退
    let t0 = h.state().player().state.time_pos;
    h.key_press(egui::Key::G);
    step_until(&mut h, "G 逐格前進", |s| s.paused && s.time_pos > t0 + 0.01);
    h.run_steps(3);
    let t1 = h.state().player().state.time_pos;
    let cmds = press_and_get_commands(&mut h, egui::Key::F);
    assert!(!cmds.contains(&egui::ViewportCommand::Fullscreen(true)), "{cmds:?}");
    wait_real(&mut h, 0.3);
    assert_eq!(h.state().player().state.time_pos, t1, "F 不再逐格");
    h.key_press(egui::Key::D);
    step_until(&mut h, "D 逐格後退", |s| s.paused && s.time_pos < t1 - 0.01);
}

/// 錄到一半換到別的分頁：不錄了（不然看不到在錄，下一個按鍵卻被錄走、其他快捷鍵也都沒反應）
#[test]
fn changing_settings_page_stops_recording() {
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "播放／暫停");
    h.get_by_label("+").click();
    h.run_steps(2);
    h.get_by_label(RECORDING);
    h.get_by_label("一般").click();
    h.run_steps(2);
    h.key_press(egui::Key::K);
    h.run_steps(2);
    assert!(h.state().settings().keys.custom.is_empty(), "K 沒有被錄走");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "空白鍵照常暫停", |s| s.paused);
    shortcuts_page(&mut h, "快捷鍵", "");
    assert!(h.query_by_label(RECORDING).is_none());
}

/// 搜尋也比對按鍵；錄到一半點搜尋框就不錄了（打的字不會被錄成按鍵）
#[test]
fn shortcut_search_matches_keys_and_stops_recording() {
    let mut h = playing_multitrack();
    shortcuts_page(&mut h, "快捷鍵", "PgDn");
    h.get_by_label("下一個檔案");
    h.get_by_label("下一章");
    assert!(h.query_by_label("播放／暫停").is_none());
    assert!(h.query_by_label("上一個檔案").is_none());
    // 開始錄「下一個檔案」，再點搜尋框、打字
    h.get_by_label("PgDn").click();
    h.run_steps(2);
    h.get_by_label(RECORDING);
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    assert!(h.query_by_label(RECORDING).is_none(), "點搜尋框就不錄了");
    h.key_press(egui::Key::K);
    h.event(egui::Event::Text("k".into()));
    h.run_steps(2);
    assert!(h.state().settings().keys.custom.is_empty(), "K 沒有被錄走");
    assert!(h.query_by_label_contains("已經用在").is_none());
    // 打的字進了搜尋框（「PgDnk」什麼都找不到）
    assert!(h.query_by_label("下一個檔案").is_none());
}

/// 先放開 Shift 再放開符號鍵：Windows 放開時收到的是沒有 Shift 的鍵（按下 `?`、放開 `/`）。
/// 按著的鍵要用實體按鍵記，不然 `?` 一直算「按著」，之後每一次按都被當成自動重複
#[test]
fn shifted_symbol_keys_work_again_after_shift_is_released_first() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.custom.insert("toggle-mute".into(), vec!["?".into()]);
    settings
        .keys
        .custom
        .insert("toggle-pause".into(), vec!["Space".into(), "Cmd+C".into()]);
    settings.keys.custom.insert("copy-frame".into(), vec![]);
    let mut h = playing_multitrack_with(settings);
    let key = |h: &mut Harness<'_, VitascopeApp>, key, physical, pressed, modifiers| {
        h.input_mut().events.push(egui::Event::Key {
            key,
            physical_key: Some(physical),
            pressed,
            repeat: false,
            modifiers,
        });
        h.step();
    };
    let shift = egui::Modifiers::SHIFT;
    let none = egui::Modifiers::NONE;
    key(&mut h, egui::Key::Questionmark, egui::Key::Slash, true, shift);
    key(&mut h, egui::Key::Slash, egui::Key::Slash, false, none);
    step_until(&mut h, "? 靜音", |s| s.muted);
    key(&mut h, egui::Key::Questionmark, egui::Key::Slash, true, shift);
    key(&mut h, egui::Key::Slash, egui::Key::Slash, false, none);
    step_until(&mut h, "再按一次 ? 取消靜音", |s| !s.muted);
    // 按住 Ctrl+C：自動重複時每一下都是一個「複製」，開關類的指令只算第一下
    let command = egui::Modifiers::COMMAND;
    for _ in 0..2 {
        h.event(egui::Event::Copy);
        h.step();
    }
    wait_real(&mut h, 0.3);
    assert!(h.state().player().state.paused, "只暫停一次");
    // 放開 C 再按：再切換一次
    key(&mut h, egui::Key::C, egui::Key::C, false, command);
    h.event(egui::Event::Copy);
    step_until(&mut h, "再按一次 Ctrl+C 繼續播放", |s| !s.paused);
}

/// 設定的 A-B 終點一定會繞回起點：終點用 mpv 自己的時間、不能四捨五入（24 fps 的 1.0833… 寫成「1.083333」
/// 就比那一格早，mpv 認定已經過了終點，永遠不繞回去）
#[test]
fn ab_end_point_set_on_a_frame_loops_back() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.preset = vitascope::keymap::KeyPreset::Potplayer;
    let mut h = harness_with(Some(sample("common/mp4_h264_aac.mp4")), settings);
    step_until(&mut h, "開始播放", |s| s.loaded && !s.paused && s.time_pos > 0.1);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.key_press(egui::Key::OpenBracket);
    step_until(&mut h, "[ 設定起點", |s| s.ab_loop[0].is_some());
    let a = h.state().player().ab_loop_points()[0].unwrap();
    // 播一秒再暫停（這個短片只有開頭一個關鍵影格，往前跳會跳到片尾）
    h.key_press(egui::Key::Space);
    step_until(&mut h, "播一秒", |s| !s.paused && s.time_pos > a + 1.0);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "再暫停", |s| s.paused);
    h.run_steps(5);
    // 逐格到一格：時間寫成小數 6 位會比它小（大約每兩格就有一格）
    let now = |h: &Harness<'_, VitascopeApp>| h.state().player().get_f64("time-pos").unwrap();
    let rounded_down = |t: f64| format!("{t:.6}").parse::<f64>().unwrap() < t;
    let mut b = now(&h);
    for _ in 0..8 {
        if rounded_down(b) {
            break;
        }
        h.key_press(egui::Key::F);
        step_until(&mut h, "F 逐格前進", |s| s.paused && s.time_pos > b + 0.01);
        h.run_steps(3);
        b = now(&h);
    }
    assert!(rounded_down(b), "找不到這樣的一格：{b}");
    h.key_press(egui::Key::CloseBracket);
    step_until(&mut h, "] 設定終點", |s| s.ab_loop[1].is_some());
    assert_eq!(
        h.state().player().ab_loop_points(),
        [Some(a), Some(b)],
        "就是那一格的時間"
    );
    // 繼續播放：馬上到終點、繞回起點
    h.key_press(egui::Key::Space);
    step_until(&mut h, "繞回起點，或是播過終點", |s| {
        !s.paused && (s.time_pos < b - 0.5 || s.time_pos > b + 0.8)
    });
    let t = h.state().player().state.time_pos;
    assert!(t < b - 0.5, "播到終點要繞回起點 {a}：現在 {t}，終點 {b}");
}

/// 新指令有按鍵時，選單、設定頁上也寫出來（沒有按鍵時跟以前一樣）
#[test]
fn menus_show_the_keys_of_new_commands() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    for (id, key) in [
        ("fill-window", "F19"),
        ("toggle-eq", "F20"),
        ("toggle-smooth", "F21"),
        ("sub-delay-reset", "F13"),
        ("load-subtitle", "F24"),
        ("screenshot-as", "F22"),
        ("seek-back", "J"),
        ("seek-forward", "L"),
    ] {
        settings.keys.custom.insert(id.into(), vec![key.into()]);
    }
    let mut h = playing_multitrack_with(settings);
    // 畫面 → 裁切 → 填滿視窗
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label("畫面 ⏵").click();
    h.run_steps(2);
    h.get_by_label_contains("裁切").click();
    h.run_steps(2);
    h.get_by_label("填滿視窗（裁掉黑邊） F19");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 字幕 → 字幕延遲的說明、載入字幕檔…
    hover_context_item(&mut h, "字幕 ⏵");
    h.get_by_label("載入字幕檔… F24");
    h.get_by_label_contains("字幕延遲").click();
    h.run_steps(2);
    h.get_by_label("快捷鍵 [ / ]，歸零 F13。正數 = 字幕晚一點出現");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 擷取畫面 → 另存新檔…
    hover_context_item(&mut h, "擷取畫面 ⏵");
    h.get_by_label("擷取畫面 ⏵").click();
    h.run_steps(2);
    h.get_by_label("另存新檔… F22");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 畫質 → 流暢播放、音效 → 等化器：勾選框後面寫按鍵
    open_picture_menu(&mut h);
    h.get_by_label("F21");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_sound_menu(&mut h);
    h.get_by_label("F20");
    h.get_by_label("等化器");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 設定 → 播放：跳轉秒數的標題
    open_settings_page(&mut h, "播放");
    h.get_by_label("J / L 跳轉");
    let cmd = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };
    h.get_by_label(&format!("{cmd}+← / → 跳轉"));
}

// ───────────── 滑鼠按鍵（設定 → 快捷鍵 → 滑鼠） ─────────────

/// 在影片畫面中間按一下滑鼠的 `button`（按下、放開在同一幀）
fn click_video_with(h: &mut Harness<'_, VitascopeApp>, button: egui::PointerButton) {
    h.get_by_label("影片畫面").click_button(button);
    h.step();
}

/// 在影片畫面上捲動滾輪 `lines` 格（往上為正）
fn wheel_video(h: &mut Harness<'_, VitascopeApp>, lines: f32) {
    h.get_by_label("影片畫面").hover();
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, lines),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

#[test]
fn mouse_middle_side_buttons_and_wheel_bindings() {
    use vitascope::keymap::WheelMode;
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.mouse.click = String::new();
    settings.keys.mouse.middle = "toggle-mute".into();
    settings.keys.mouse.back = "speed-down".into();
    settings.keys.mouse.forward = "speed-up".into();
    settings.keys.mouse.wheel = WheelMode::Seek;
    // 不用預設的 5 秒：確定每格跳的是「跳轉秒數」
    settings.seek_short = 3.0;
    let mut h = playing_multitrack_with(settings);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    // 單擊 = 不動作：不會繼續播放
    click_video_with(&mut h, egui::PointerButton::Primary);
    h.run_steps(3);
    wait_real(&mut h, 0.3);
    assert!(h.state().player().state.paused, "單擊不動作");
    // 中鍵 = 靜音
    click_video_with(&mut h, egui::PointerButton::Middle);
    step_until(&mut h, "中鍵靜音", |s| s.muted);
    // 側鍵：下一頁加快、上一頁減慢
    click_video_with(&mut h, egui::PointerButton::Extra2);
    step_until(&mut h, "側鍵（下一頁）加快", |s| close_to(s.speed, 1.1));
    click_video_with(&mut h, egui::PointerButton::Extra1);
    step_until(&mut h, "側鍵（上一頁）減慢", |s| close_to(s.speed, 1.0));
    // 滾輪 = 跳轉：往上一格前進 3 秒（跳轉秒數），音量不變。
    // 相對跳轉會落在關鍵影格上（這個檔案只有 0 秒、10.4 秒兩個），跳了幾秒看 OSD，位置只看方向
    let t0 = h.state().player().state.time_pos;
    wheel_video(&mut h, 2.0);
    let osd = h.state().osd_text().unwrap_or_default();
    assert!(osd.starts_with("▶▶ 前進 6 秒"), "往上兩格 × 3 秒：{osd}");
    step_until(&mut h, "滾輪往上：往前跳", |s| s.time_pos > t0 + 1.0);
    wait_real(&mut h, 0.3);
    let t1 = h.state().player().state.time_pos;
    wheel_video(&mut h, -1.0);
    let osd = h.state().osd_text().unwrap_or_default();
    assert!(osd.starts_with("◀◀ 後退 3 秒"), "往下一格 × 3 秒：{osd}");
    step_until(&mut h, "往下一格：往回跳", |s| s.time_pos < t1 - 1.0);
    assert_eq!(h.state().player().state.volume, 100.0, "滾輪不再調音量");
    // 滾輪 = 不動作
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.mouse.wheel = WheelMode::None;
    let mut h = playing_multitrack_with(settings);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    let t0 = h.state().player().state.time_pos;
    let osd0 = h.state().osd_text().map(str::to_owned);
    // 先往上（音量已經是 100%，跳轉會跳到 10.4 秒），再往下（音量會變 90%）
    for lines in [2.0, -2.0] {
        wheel_video(&mut h, lines);
        let osd = h.state().osd_text().map(str::to_owned);
        assert!(
            osd.is_none() || osd == osd0,
            "滾輪 {lines} 格不顯示跳轉 / 音量：{osd:?}"
        );
        h.run_steps(3);
        wait_real(&mut h, 0.3);
        let st = &h.state().player().state;
        assert_eq!(st.volume, 100.0, "滾輪 {lines} 格不調音量");
        assert!(
            (st.time_pos - t0).abs() < 0.05,
            "滾輪 {lines} 格不跳轉：{t0} → {}",
            st.time_pos
        );
    }
}

/// 預設：雙擊畫面只切換全螢幕（第一下單擊已經暫停了，雙擊時切回來）
#[test]
fn double_click_on_the_video_goes_fullscreen_without_pausing() {
    let mut h = playing_multitrack();
    let pos = h.get_by_label("影片畫面").rect().center();
    double_click(&mut h, pos);
    let cmds = viewport_commands(&h);
    assert!(cmds.contains(&egui::ViewportCommand::Fullscreen(true)), "{cmds:?}");
    assert_eq!(h.state().osd_text(), None, "不顯示暫停 / 播放");
    wait_real(&mut h, 0.4);
    assert!(!h.state().player().state.paused, "雙擊不暫停");
}

/// 單擊 = 靜音：雙擊時也要把第一下的靜音切回來，結果只有全螢幕改變
#[test]
fn double_click_undoes_a_mute_click() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.mouse.click = "toggle-mute".into();
    let mut h = playing_multitrack_with(settings);
    click_video_with(&mut h, egui::PointerButton::Primary);
    step_until(&mut h, "單擊靜音", |s| s.muted);
    h.run_steps(3);
    assert!(!h.state().player().state.paused, "單擊不暫停");
    // 靜音中雙擊：還是靜音，切換全螢幕
    let pos = h.get_by_label("影片畫面").rect().center();
    double_click(&mut h, pos);
    let cmds = viewport_commands(&h);
    assert!(cmds.contains(&egui::ViewportCommand::Fullscreen(true)), "{cmds:?}");
    assert_eq!(h.state().osd_text(), None, "不顯示靜音 / 取消靜音");
    wait_real(&mut h, 0.4);
    let st = &h.state().player().state;
    assert!(st.muted, "雙擊不改變靜音");
    assert!(!st.paused);
}

/// 單擊 = 靜音、雙擊 = 不動作：點兩下是靜音再取消靜音（第二下照第一下之前的值設回去）
#[test]
fn double_click_bound_to_nothing_with_a_mute_click_mutes_and_unmutes() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.mouse.click = "toggle-mute".into();
    settings.keys.mouse.double_click = String::new();
    let mut h = playing_multitrack_with(settings);
    let pos = h.get_by_label("影片畫面").rect().center();
    double_click(&mut h, pos);
    let cmds = viewport_commands(&h);
    assert!(
        !cmds.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))),
        "{cmds:?}"
    );
    assert_eq!(h.state().osd_text(), Some("取消靜音"), "第二下取消靜音");
    h.run_steps(3);
    wait_real(&mut h, 0.4);
    let st = &h.state().player().state;
    assert!(!st.muted, "靜音再取消靜音");
    assert!(!st.paused);
    // 再點一下：照常靜音
    click_video_with(&mut h, egui::PointerButton::Primary);
    step_until(&mut h, "第三下靜音", |s| s.muted);
}

/// 雙擊 = 不動作：點兩下就是單擊兩下（暫停再播放），不切換全螢幕
#[test]
fn double_click_bound_to_nothing_is_two_clicks() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.mouse.double_click = String::new();
    let mut h = playing_multitrack_with(settings);
    let pos = h.get_by_label("影片畫面").rect().center();
    double_click(&mut h, pos);
    let cmds = viewport_commands(&h);
    assert!(
        !cmds.iter().any(|c| matches!(c, egui::ViewportCommand::Fullscreen(_))),
        "{cmds:?}"
    );
    assert!(h.state().osd_text().is_some(), "第二下照樣是單擊（顯示播放 / 暫停）");
    wait_real(&mut h, 0.4);
    assert!(!h.state().player().state.paused, "暫停又播放");
}

/// 設定頁的滑鼠區：改中鍵、滾輪馬上生效、存檔；「還原成預設組…」連滑鼠一起還原
#[test]
fn mouse_settings_page_changes_and_resets() {
    use vitascope::keymap::{MouseSettings, WheelMode};
    let dir = TempDir::new("mouse-page");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
    settle(&mut h, "mkv_multitrack.mkv");
    shortcuts_page(&mut h, "快捷鍵", "滑鼠");
    // 只剩滑鼠的設定（搜尋「滑鼠」）；預設是單擊播放／暫停、雙擊全螢幕、滾輪音量
    assert!(h.query_by_label("播放／暫停").is_none(), "鍵盤的指令不在搜尋結果裡");
    for (label, value) in [
        ("單擊畫面", "播放／暫停"),
        ("雙擊畫面", "全螢幕"),
        ("中鍵", "不動作"),
        ("側鍵（上一頁）", "不動作"),
        ("側鍵（下一頁）", "不動作"),
        ("在畫面上捲動滾輪", "音量"),
    ] {
        let combo = combo_box(&h, label);
        assert_eq!(combo.accesskit_node().value().as_deref(), Some(value), "{label}");
    }
    // 中鍵改成靜音（任何指令都可以選，清單很長，要捲到看得到）
    combo_in_view(&mut h, "中鍵");
    h.get_by_label("靜音").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("靜音").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.mouse.middle, "toggle-mute");
    assert_eq!(combo_box(&h, "中鍵").accesskit_node().value().as_deref(), Some("靜音"));
    // 單擊只能選開關：清單裡沒有全螢幕
    combo_in_view(&mut h, "單擊畫面");
    assert!(h.query_by_label("全螢幕").is_none(), "單擊不能選全螢幕");
    h.get_by_label("不動作").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.mouse.click, "");
    // 滾輪改成跳轉
    combo_in_view(&mut h, "在畫面上捲動滾輪");
    h.get_by_label("跳轉（每格 5 秒）").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.mouse.wheel, WheelMode::Seek);
    // 馬上存檔
    let saved = Settings::load_from(path.clone()).keys.mouse;
    assert_eq!(saved.middle, "toggle-mute");
    assert_eq!(saved.click, "");
    assert_eq!(saved.wheel, WheelMode::Seek);
    // 關掉設定，馬上生效：中鍵靜音
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    click_video_with(&mut h, egui::PointerButton::Middle);
    step_until(&mut h, "中鍵靜音", |s| s.muted);
    // 還原成預設組：滑鼠也還原（對話框寫明）
    shortcuts_page(&mut h, "快捷鍵", "");
    h.get_by_label("還原成預設組…").scroll_to_me();
    h.run_steps(5);
    h.get_by_label("還原成預設組…").click();
    h.run_steps(2);
    h.get_by_label("把所有快捷鍵還原成「影戲」的預設值？");
    h.get_by_label("滑鼠的設定也一起還原。");
    h.get_by_label("還原").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().keys.mouse, MouseSettings::default());
    assert_eq!(Settings::load_from(path).keys.mouse, MouseSettings::default());
    // 固定的按鍵清單不再寫單擊、雙擊、滾輪（可以改了）
    assert!(h.query_by_label("切換全螢幕").is_none());
}

#[test]
fn mouse_settings_in_english() {
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    settings.auto_next = false;
    settings.keys.mouse.forward = "future-command-2030".into();
    let mut h = playing_multitrack_with(settings);
    shortcuts_page(&mut h, "Shortcuts", "mouse");
    h.get_by_label("Mouse");
    for (label, value) in [
        ("Click on the video", "Play / pause"),
        ("Double-click on the video", "Fullscreen"),
        ("Middle button", "Do nothing"),
        ("Back button", "Do nothing"),
        // 新版的指令：照原樣寫出來，不會被改掉
        ("Forward button", "future-command-2030"),
        ("Wheel over the video", "Volume"),
    ] {
        let combo = combo_box(&h, label);
        assert_eq!(combo.accesskit_node().value().as_deref(), Some(value), "{label}");
    }
    combo_in_view(&mut h, "Wheel over the video");
    h.get_by_label("Seek (5 s per notch)");
    h.get_by_label("Do nothing").click();
    h.run_steps(2);
    assert_eq!(
        h.state().settings().keys.mouse.wheel,
        vitascope::keymap::WheelMode::None
    );
    assert_eq!(h.state().settings().keys.mouse.forward, "future-command-2030");
    // 捲回最上面（選滾輪時捲到下面了）再按「還原成預設組…」
    h.get_by_label("Reset to the preset…").scroll_to_me();
    h.run_steps(5);
    h.get_by_label("Reset to the preset…").click();
    h.run_steps(2);
    h.get_by_label("Mouse settings are reset too.");
}

// ───────────── 畫質：去交錯、去色帶、銳化、縮放演算法、HDR ─────────────

/// 右鍵選單「畫質」→ 一層層的子選單（`path`，標籤的一部分）→ 點 `item`（完整標籤）
fn pick_picture_item(h: &mut Harness<'_, VitascopeApp>, path: &[&str], item: &str) {
    open_picture_menu(h);
    for sub in path {
        hover_menu_item(h, sub);
    }
    h.get_by_label(item).click();
    h.run_steps(2);
}

/// 等 mpv 的選項變成 `value`（畫質選項是非同步設定的）
fn wait_prop(h: &mut Harness<'_, VitascopeApp>, name: &str, value: &str) {
    step_until_app(h, &format!("mpv 的 {name} = {value}"), |app| {
        app.player().get_string(name).is_ok_and(|v| v == value)
    });
}

/// 設定檔裡存的 `video`
fn saved_video(path: &std::path::Path) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["video"].clone()
}

/// 用暫存資料夾的設定檔、播放 `file`（播完不接下一個）
fn video_settings_harness(name: &str, file: &str) -> (TempDir, PathBuf, Harness<'static, VitascopeApp>) {
    let dir = TempDir::new(name);
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = harness_with(Some(sample(file)), settings);
    let file_name = PathBuf::from(file).file_name().unwrap().to_string_lossy().into_owned();
    settle(&mut h, &file_name);
    (dir, path, h)
}

#[test]
fn video_menu_items() {
    let (_dir, path, mut h) = video_settings_harness("video-menu", "common/mkv_multitrack.mkv");
    // 縮放演算法 → 高品質：放大用 ewa_lanczossharp、抗振鈴 0.6（mpv 的 high-quality 設定檔）
    pick_picture_item(&mut h, &["縮放演算法"], "高品質");
    assert_eq!(h.state().osd_text(), Some("縮放：高品質"));
    wait_prop(&mut h, "scale", "ewa_lanczossharp");
    wait_prop(&mut h, "scale-antiring", "0.600000");
    assert_eq!(h.state().settings().video.quality, vitascope::picture::Quality::High);
    assert_eq!(saved_video(&path)["quality"], "high");
    // 個別指定放大的演算法：優先於畫質；抗振鈴照畫質
    pick_picture_item(&mut h, &["縮放演算法", "放大（跟隨畫質）"], "Spline36");
    assert_eq!(h.state().osd_text(), Some("放大：Spline36"));
    wait_prop(&mut h, "scale", "spline36");
    assert_eq!(prop(&h, "scale-antiring"), "0.600000");
    assert_eq!(saved_video(&path)["scale"], "spline36");
    pick_picture_item(&mut h, &["縮放演算法", "放大（Spline36）"], "跟隨畫質");
    wait_prop(&mut h, "scale", "ewa_lanczossharp");
    assert_eq!(saved_video(&path)["scale"], serde_json::Value::Null);
    // 去色帶 → 中等：開啟，參數是中等的那一組（= mpv 的預設值）
    pick_picture_item(&mut h, &["去色帶"], "中等");
    assert_eq!(h.state().osd_text(), Some("去色帶：中等"));
    wait_prop(&mut h, "deband", "yes");
    for (name, value) in [
        ("deband-iterations", "1"),
        ("deband-threshold", "48.000000"),
        ("deband-range", "16.000000"),
        ("deband-grain", "32.000000"),
    ] {
        assert_eq!(prop(&h, name), value, "{name}");
    }
    assert_eq!(saved_video(&path)["deband"], "medium");
    // 再改成強：參數跟著變
    pick_picture_item(&mut h, &["去色帶"], "強");
    // 非同步設定照順序生效：等最後一個
    wait_prop(&mut h, "deband-grain", "48.000000");
    assert_eq!(prop(&h, "deband-iterations"), "2");
    assert_eq!(prop(&h, "deband-threshold"), "64.000000");
    // 銳化 → 輕微
    pick_picture_item(&mut h, &["銳化"], "輕微");
    assert_eq!(h.state().osd_text(), Some("銳化：輕微"));
    wait_prop(&mut h, "sharpen", "0.250000");
    assert_eq!(saved_video(&path)["sharpen"], "light");
    // HDR 色調映射 → Hable；這個影片不是 HDR，選單上註明
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "HDR 色調映射");
    h.get_by_label("目前的影片不是 HDR");
    h.get_by_label("Hable").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("HDR 色調映射：Hable"));
    wait_prop(&mut h, "tone-mapping", "hable");
    assert_eq!(saved_video(&path)["tone"]["curve"], "hable");
    // 目標亮度：「自動」註明是 203 nits，只列 203 以下的值（超過 203 時 vo_gpu 把亮部裁成白色）
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "HDR 色調映射");
    hover_menu_item(&mut h, "目標亮度（自動）");
    for label in ["自動（203 nits）", "100 nits", "150 nits"] {
        h.get_by_label(label);
    }
    for label in ["203 nits", "400 nits", "1000 nits"] {
        assert!(h.query_by_label(label).is_none(), "{label} 不能列");
    }
    h.get_by_label("150 nits").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("HDR 目標亮度：150 nits"));
    assert_eq!(h.state().settings().video.tone.target_peak, Some(150));
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 150);
    // SDR 影片：mpv 的目標亮度維持 auto（不然 SDR 影片也會被色調映射）
    h.run_steps(10);
    assert_eq!(prop(&h, "target-peak"), "auto");
    // 依畫面動態調整亮度：介面測試沒有 GL context = 畫面輸出不支援，不顯示
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "HDR 色調映射");
    assert!(h.query_by_label("依畫面動態調整亮度").is_none());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 支援的 GL：顯示，可以關掉
    h.state_mut().set_gl_compute_peak(true);
    pick_picture_item(&mut h, &["HDR 色調映射"], "依畫面動態調整亮度");
    assert_eq!(h.state().osd_text(), Some("依畫面動態調整亮度：關"));
    wait_prop(&mut h, "hdr-compute-peak", "no");
    assert_eq!(saved_video(&path)["tone"]["compute_peak"], false);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 換檔之後照舊（這些選項不是每個檔案各自的）
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    settle(&mut h, "mp4_h264_aac.mp4");
    for (name, value) in [
        ("scale", "ewa_lanczossharp"),
        ("deband", "yes"),
        ("sharpen", "0.250000"),
        ("tone-mapping", "hable"),
        ("target-peak", "auto"),
    ] {
        assert_eq!(prop(&h, name), value, "{name}");
    }
}

// 目標亮度只對 HDR 影片送：啟動時（還沒開檔）auto、HDR10 影片是設定的值、換成 SDR 影片變回 auto、再換回 HDR 又是設定的值。
// 看的是 mpv 的 target-peak（不是播放器記下的值）
#[test]
fn target_peak_follows_the_video_dynamic_range() {
    let dir = TempDir::new("hdr-peak");
    let path = dir.0.join("settings.json");
    std::fs::write(
        &path,
        r#"{"auto_next": false, "video": {"tone": {"target_peak": 100}}}"#,
    )
    .unwrap();
    let settings = Settings::load_from(path);
    assert_eq!(settings.video.tone.target_peak, Some(100));
    let mut h = harness_with(None, settings);
    h.run_steps(3);
    assert_eq!(prop(&h, "target-peak"), "auto", "還沒有影片");
    for (file, hdr, peak) in [
        ("general/mkv_hevc10_hdr10.mkv", true, "100"),
        ("common/mp4_h264_aac.mp4", false, "auto"),
        ("general/mkv_hevc10_hlg.mkv", true, "100"),
        ("common/mkv_multitrack.mkv", false, "auto"),
        ("general/mkv_hevc10_hdr10.mkv", true, "100"),
    ] {
        let name = PathBuf::from(file).file_name().unwrap().to_string_lossy().into_owned();
        drop_file(&mut h, sample(file));
        step_until(&mut h, &format!("{name} 的 HDR = {hdr}"), |s| {
            playing(s, &name) && s.video_hdr == hdr
        });
        wait_prop(&mut h, "target-peak", peak);
    }
    // 關檔之後（沒有影片）回到 auto
    h.state().player().stop().unwrap();
    step_until(&mut h, "關檔", |s| !s.loaded && !s.video_hdr);
    wait_prop(&mut h, "target-peak", "auto");
}

// VITASCOPE_MPV_OPTS 指定了 target-peak：HDR、SDR 影片都不改它
#[test]
fn user_target_peak_is_left_alone() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.tone.target_peak = Some(100);
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: vec![("target-peak".into(), "180".into())],
            ..Options::headless()
        },
        Launch::default(),
        settings,
    );
    for (file, hdr) in [
        ("general/mkv_hevc10_hdr10.mkv", true),
        ("common/mp4_h264_aac.mp4", false),
    ] {
        let name = PathBuf::from(file).file_name().unwrap().to_string_lossy().into_owned();
        drop_file(&mut h, sample(file));
        step_until(&mut h, &format!("{name} 的 HDR = {hdr}"), |s| {
            playing(s, &name) && s.video_hdr == hdr
        });
        h.run_steps(10);
        assert_eq!(prop(&h, "target-peak"), "180", "{name}");
    }
}

// 動態峰值偵測只在畫面輸出的 OpenGL 支援時出現（GLSL 4.20 + compute shader）：選單、設定頁都一樣
#[test]
fn compute_peak_checkbox_follows_the_gl_capability() {
    let (_dir, path, mut h) = video_settings_harness("compute-peak", "general/mkv_hevc10_hdr10.mkv");
    assert!(!h.state().engine_caps().compute_peak, "介面測試沒有 GL context");
    let shown = |h: &mut Harness<'_, VitascopeApp>| {
        open_picture_menu(h);
        hover_menu_item(h, "HDR 色調映射");
        h.get_by_label("Hable");
        let menu = h.query_by_label("依畫面動態調整亮度").is_some();
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        h.key_press(egui::Key::F5);
        h.run_steps(2);
        h.get_by_label("畫質").click();
        h.run_steps(2);
        h.get_by_label("HDR → SDR");
        let page = h.query_by_label("動態峰值偵測").is_some();
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        (menu, page)
    };
    assert_eq!(shown(&mut h), (false, false), "不支援：不顯示");
    // 不顯示時設定照舊（預設開），mpv 照樣收到 auto：換到支援的 GL 上自動生效
    assert_eq!(prop(&h, "hdr-compute-peak"), "auto");
    h.state_mut().set_gl_compute_peak(true);
    assert_eq!(shown(&mut h), (true, true), "支援：選單、設定頁都有");
    // 設定頁也能改
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    click_in_view(&mut h, "動態峰值偵測");
    wait_prop(&mut h, "hdr-compute-peak", "no");
    assert_eq!(saved_video(&path)["tone"]["compute_peak"], false);
}

// 建立 App 時看到的 GL context 能力要進到引擎功能（啟動時 probe_caps 一律 false，之後才填上 GL 的結果）：
// 不用 set_gl_compute_peak，一啟動選單就有「依畫面動態調整亮度」
#[test]
fn gl_capability_at_startup_reaches_engine_caps() {
    for supported in [true, false] {
        let mut settings = Settings::default();
        settings.auto_next = false;
        let mut h = harness_launch(
            Launch {
                files: vec![sample("general/mkv_hevc10_hdr10.mkv")],
                gl_compute_peak: Some(supported),
                ..Default::default()
            },
            settings,
        );
        assert_eq!(
            h.state().engine_caps().compute_peak,
            supported,
            "啟動後馬上就是 GL 的結果"
        );
        settle(&mut h, "mkv_hevc10_hdr10.mkv");
        open_picture_menu(&mut h);
        hover_menu_item(&mut h, "HDR 色調映射");
        h.get_by_label("Hable");
        assert_eq!(
            h.query_by_label("依畫面動態調整亮度").is_some(),
            supported,
            "GL context {}",
            if supported { "支援" } else { "不支援" }
        );
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
    }
}

// 杜比視界 Profile 5（顏色無法正確顯示）：每個檔案提示一次，換檔之後再提示
#[test]
fn dolby_vision_profile_5_notice_once_per_file() {
    const NOTICE: &str = "杜比視界 Profile 5：目前無法正確顯示顏色";
    let mut player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    // 產生不了杜比視界的樣本：把影片軌當成 profile 5
    player.set_fake_dolby_vision(Some(5));
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with_player(player, Launch::default(), settings);
    h.step();
    assert_ne!(h.state().osd_text(), Some(NOTICE), "還沒有檔案");
    drop_file(&mut h, sample("general/mkv_hevc10_hdr10.mkv"));
    step_until_app(&mut h, "提示杜比視界 Profile 5", |app| {
        app.osd_text() == Some(NOTICE)
    });
    // 換成別的提示之後，同一個檔案不會再提示
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_ne!(h.state().osd_text(), Some(NOTICE));
    for _ in 0..30 {
        h.step();
        assert_ne!(h.state().osd_text(), Some(NOTICE), "同一個檔案只提示一次");
        std::thread::sleep(Duration::from_millis(10));
    }
    // 下一個檔案：再提示一次
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until_app(&mut h, "下一個檔案再提示", |app| {
        playing(&app.player().state, "mp4_h264_aac.mp4") && app.osd_text() == Some(NOTICE)
    });
}

// 真正的 HDR 影片（不放進專案：設定 VITASCOPE_HDR_SAMPLES 指到放影片的資料夾才跑）：媒體資訊的動態範圍、
// 杜比視界 Profile 5 的提示、目標亮度只對 HDR 送。沒有的檔案略過
#[test]
#[ignore = "要真正的 HDR 影片：VITASCOPE_HDR_SAMPLES=資料夾"]
fn real_hdr_clips() {
    let Some(dir) = std::env::var_os("VITASCOPE_HDR_SAMPLES").map(PathBuf::from) else {
        eprintln!("略過：沒有設定 VITASCOPE_HDR_SAMPLES");
        return;
    };
    const P5: &str = "杜比視界 Profile 5：目前無法正確顯示顏色";
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.tone.target_peak = Some(100);
    let mut h = harness_with(None, settings);
    h.step();
    let mut tested = 0;
    for (file, range, notice) in [
        ("hdr10_hevc_1080p.mp4", "HDR10", false),
        ("hdr10_av1_1080p.mp4", "HDR10", false),
        ("dv_p5_1080p.mp4", "Dolby Vision（Profile 5，顏色無法正確顯示）", true),
        ("dv_p81_1080p.mp4", "Dolby Vision（Profile 8）", false),
        ("dv_p84_1080p.mp4", "Dolby Vision（Profile 8）", false),
        ("hlg_av1_1080p50.mp4", "HLG", false),
        // 這個 HDR10+ 影片每一格的 average_maxrgb 都是 0（ffprobe -show_frames）：libplacebo 把平均是 0 的
        // HDR10+ 當成沒有（pl_hdr_metadata_contains），mpv 的 video-params 就沒有 scene-max-*，看起來是 HDR10。
        // 引擎本身拿得到 HDR10+（mpv 把 WebM 的 BlockAdditional 轉成封包的 side data），見下一個
        ("hdr10plus_vp9.webm", "HDR10", false),
        // 用 x265 的 dhdr10-info 自己產生的 HEVC HDR10+（average_maxrgb 不是 0）
        ("hdr10plus_hevc_x265.mkv", "HDR10+", false),
    ] {
        let path = dir.join(file);
        if !path.exists() {
            eprintln!("略過：沒有 {}", path.display());
            continue;
        }
        tested += 1;
        // 先換掉上一個檔案的提示（提示顯示 1.5 秒，開下一個檔案時可能還在）
        if h.state().player().state.loaded {
            h.key_press(egui::Key::Space);
            h.run_steps(2);
        }
        drop_file(&mut h, path);
        step_until(&mut h, &format!("{file} 是 HDR"), |s| playing(s, file) && s.video_hdr);
        // HDR 影片：送設定的目標亮度
        wait_prop(&mut h, "target-peak", "100");
        // 影像參數（HDR10+ 的動態中繼資料）要等解出影格。對了之後再看 2 秒都不變
        //（HDR10 的不能過一下又變成 HDR10+）
        let mut got = String::new();
        let mut since: Option<Instant> = None;
        let start = Instant::now();
        while start.elapsed() < TIMEOUT && since.is_none_or(|t| t.elapsed() < Duration::from_secs(2)) {
            let info = vitascope::mediainfo::read(h.state().player(), None);
            if let Some(vp) = &info.vparams {
                got = vitascope::mediainfo::dynamic_range(vp, info.video.as_ref());
                if since.is_some() {
                    assert_eq!(got, range, "{file}：動態範圍變了");
                } else if got == range {
                    since = Some(Instant::now());
                }
            }
            h.step();
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("{file}：{got}");
        assert_eq!(got, range, "{file}");
        if notice {
            step_until_app(&mut h, "提示杜比視界 Profile 5", |app| app.osd_text() == Some(P5));
        } else {
            assert_ne!(h.state().osd_text(), Some(P5), "{file}");
        }
    }
    assert!(tested > 0, "{} 裡沒有任何 HDR 影片", dir.display());
    // 換成 SDR 影片：目標亮度回到 auto
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "SDR 影片", |s| playing(s, "mp4_h264_aac.mp4") && !s.video_hdr);
    wait_prop(&mut h, "target-peak", "auto");
}

// 不是杜比視界 Profile 5（profile 8、一般 HDR10）：不提示
#[test]
fn other_dolby_vision_profiles_have_no_notice() {
    let mut player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    player.set_fake_dolby_vision(Some(8));
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with_player(player, Launch::default(), settings);
    // 每一格都看（提示只顯示 1.5 秒：等開始播放之後才看的話，慢的電腦上可能已經消失了）
    let no_notice = |h: &Harness<'_, VitascopeApp>| {
        assert!(
            !h.state().osd_text().is_some_and(|t| t.contains("杜比視界")),
            "{:?}",
            h.state().osd_text()
        );
    };
    drop_file(&mut h, sample("general/mkv_hevc10_hdr10.mkv"));
    let start = Instant::now();
    loop {
        no_notice(&h);
        let s = &h.state().player().state;
        if playing(s, "mkv_hevc10_hdr10.mkv") && s.video_size.is_some() && s.time_pos > 0.0 {
            break;
        }
        assert!(start.elapsed() < TIMEOUT, "等待逾時：開始播放");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    for _ in 0..30 {
        h.step();
        no_notice(&h);
        std::thread::sleep(Duration::from_millis(10));
    }
}

// 杜比視界 Profile 5 的提示不蓋掉開檔時比較要緊的提示（續播位置）：等那個提示消失才出現
#[test]
fn dolby_vision_notice_waits_for_the_resume_message() {
    const NOTICE: &str = "杜比視界 Profile 5：目前無法正確顯示顏色";
    let mut player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    player.set_fake_dolby_vision(Some(5));
    let mut settings = Settings::default();
    settings.auto_next = false;
    let long = sample("common/mp4_long.mp4");
    let launch = Launch {
        files: vec![long.clone()],
        ..Default::default()
    };
    let mut h = harness_with_player(player, launch, settings);
    step_until_app(&mut h, "沒有續播時直接提示", |app| {
        app.osd_text() == Some(NOTICE)
    });
    step_until(&mut h, "開始播放", |s| {
        playing(s, "mp4_long.mp4") && s.time_pos > 0.0
    });
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowRight);
    step_until(&mut h, "前進 30 秒", |s| s.time_pos >= 29.0);
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until_app(&mut h, "下一個檔案提示", |app| {
        playing(&app.player().state, "mp4_h264_aac.mp4") && app.osd_text() == Some(NOTICE)
    });
    step_until_app(&mut h, "提示消失", |app| app.osd_text().is_none());
    // 回到上次的位置：先看到續播的提示，消失之後才提示杜比視界
    drop_file(&mut h, long);
    step_until_app(&mut h, "續播的提示", |app| {
        app.osd_text().is_some_and(|t| t.contains("繼續播放（Home 從頭播放）"))
    });
    step_until_app(&mut h, "續播的提示消失後提示杜比視界", |app| {
        app.osd_text() == Some(NOTICE)
    });
}

// HDR 影片播放中改目標亮度：選單、設定頁（打字時馬上生效）、勾回自動都直接送到 mpv
#[test]
fn target_peak_changes_reach_mpv_during_hdr_playback() {
    let (_dir, path, mut h) = video_settings_harness("peak-hdr", "general/mkv_hevc10_hdr10.mkv");
    step_until(&mut h, "知道是 HDR", |s| s.video_hdr);
    wait_prop(&mut h, "target-peak", "auto");
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "HDR 色調映射");
    hover_menu_item(&mut h, "目標亮度（自動）");
    h.get_by_label("100 nits").click();
    h.run_steps(2);
    wait_prop(&mut h, "target-peak", "100");
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 100);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    type_peak(&mut h, "150");
    wait_prop(&mut h, "target-peak", "150");
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 100, "還在打字，先不存");
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 150);
    click_in_view(&mut h, "自動");
    wait_prop(&mut h, "target-peak", "auto");
    assert_eq!(h.state().settings().video.tone.target_peak, None);
    assert!(saved_video(&path)["tone"]["target_peak"].is_null());
}

// 「目前的影片不是 HDR」只在 SDR 影片時出現：HDR10 影片時選單、設定頁都沒有，換成 SDR 影片後設定頁跟著顯示
#[test]
fn not_hdr_note_follows_the_video() {
    let (_dir, _path, mut h) = video_settings_harness("hdr-note", "general/mkv_hevc10_hdr10.mkv");
    step_until(&mut h, "知道是 HDR 影片", |s| s.video_hdr);
    h.run_steps(2);
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "HDR 色調映射");
    h.get_by_label("Hable");
    assert!(h.query_by_label("目前的影片不是 HDR").is_none(), "HDR 影片的選單");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    h.get_by_label("HDR → SDR");
    assert!(h.query_by_label("目前的影片不是 HDR").is_none(), "HDR 影片的設定頁");
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    step_until(&mut h, "換成 SDR 影片", |s| {
        playing(s, "mp4_h264_aac.mp4") && !s.video_hdr
    });
    h.run_steps(2);
    h.get_by_label("目前的影片不是 HDR");
}

#[test]
fn deinterlace_menu_shows_the_current_state() {
    let (_dir, path, mut h) = video_settings_harness("deint-menu", "common/mkv_multitrack.mkv");
    let caps = *h.state().engine_caps();
    if !caps.deint_auto || !caps.deint_status {
        // 系統的 libmpv 0.37：沒有「自動」、看不到目前的狀態，只確認開關
        open_picture_menu(&mut h);
        hover_menu_item(&mut h, "去交錯");
        assert!(h.query_by_label("自動（建議）").is_none(), "引擎不支援自動時不列");
        h.get_by_label("開啟").click();
        h.run_steps(2);
        wait_prop(&mut h, "deinterlace", "yes");
        assert_eq!(h.state().osd_text(), Some("去交錯：開啟"));
        return;
    }
    assert_eq!(prop(&h, "deinterlace"), "auto", "預設自動");
    // 逐行的影片：自動不去交錯
    open_picture_menu(&mut h);
    h.get_by_label("去交錯（目前：逐行影片） ⏵");
    // 開啟：提示先寫「未去交錯」，mpv 換好濾鏡後跟著更新
    pick_picture_item(&mut h, &["去交錯"], "開啟");
    step_until(&mut h, "mpv 換好去交錯濾鏡", |s| s.deinterlace_active);
    h.run_steps(2);
    // 提示還在的話已經跟著更新（提示只顯示 1.5 秒，CI 很忙時可能已經消失）
    let osd = h.state().osd_text();
    assert!(
        osd.is_none() || osd == Some("去交錯：開啟（目前：已去交錯）"),
        "提示要跟著更新：{osd:?}"
    );
    assert_eq!(prop(&h, "deinterlace"), "yes");
    assert_eq!(saved_video(&path)["deinterlace"], "on");
    open_picture_menu(&mut h);
    h.get_by_label("去交錯（目前：已去交錯） ⏵");
    hover_menu_item(&mut h, "去交錯");
    h.get_by_label("自動（建議）").click();
    h.run_steps(2);
    step_until(&mut h, "逐行的影片不再去交錯", |s| !s.deinterlace_active);
    h.run_steps(2);
    let osd = h.state().osd_text();
    assert!(
        osd.is_none() || osd == Some("去交錯：自動（目前：逐行影片）"),
        "提示要跟著更新：{osd:?}"
    );
    assert_eq!(prop(&h, "deinterlace"), "auto");
    assert_eq!(saved_video(&path)["deinterlace"], "auto");
    // 交錯的影片：自動就會去交錯
    let Some(interlaced) =
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated/general/ts_mpeg2_interlaced.ts"))
            .filter(|p| p.exists())
    else {
        eprintln!("略過交錯的部分：沒有 general/ts_mpeg2_interlaced.ts（這個 FFmpeg 產生不了）");
        return;
    };
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    drop_file(&mut h, interlaced);
    step_until(&mut h, "交錯的影片去交錯", |s| {
        playing(s, "ts_mpeg2_interlaced.ts") && s.deinterlace_active
    });
    h.run_steps(5);
    open_picture_menu(&mut h);
    h.get_by_label("去交錯（目前：已去交錯） ⏵");
}

#[test]
fn dumb_mode_greys_out_gpu_only_items() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.quality = vitascope::picture::Quality::High;
    settings.video.deband = vitascope::picture::Strength::Strong;
    // 使用中的像素著色器組合（別的電腦存的設定）：簡化流程不跑著色器，不送給 mpv
    let dir = TempDir::new("dumb-shaders");
    let (invert, _, _) = write_shaders(&dir);
    settings.video.shaders.presets = vec![vitascope::picture::ShaderPreset {
        id: 3,
        name: "A".into(),
        files: vec![path_str(&invert)],
    }];
    settings.video.shaders.active = Some(3);
    let mut h = harness_launch_with(
        Options {
            extra: vec![("gpu-dumb-mode".into(), "yes".into())],
            keep_open: true,
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_multitrack.mkv")],
            // GL 有 compute shader（像 llvmpipe 的 4.5）：簡化流程照樣不做動態峰值偵測
            gl_compute_peak: Some(true),
            ..Default::default()
        },
        settings,
    );
    settle(&mut h, "mkv_multitrack.mkv");
    assert!(h.state().engine_caps().dumb);
    assert!(!h.state().engine_caps().compute_peak, "簡化流程不做動態峰值偵測");
    assert!(
        h.state().player().shader_list().unwrap().is_empty(),
        "簡化流程不送著色器"
    );
    assert_eq!(h.state().settings().video.shaders.active, Some(3), "設定照舊");
    // 選項照樣對應（簡化流程的畫面輸出會忽略它們；設定跟 mpv 的值一致）
    assert_eq!(prop(&h, "scale"), "ewa_lanczossharp");
    assert_eq!(prop(&h, "deband"), "yes");
    open_picture_menu(&mut h);
    // 使用者的像素著色器也不跑（翻轉改用濾鏡）
    for label in ["去色帶", "銳化", "縮放演算法", "像素著色器"] {
        assert!(h.get_by_label_contains(label).accesskit_node().is_disabled(), "{label}");
    }
    // 去交錯是解碼後的濾鏡、影像調整與 HDR 色調映射在輸出到螢幕時做，簡化流程也有：照常
    for label in ["去交錯", "影像調整…", "HDR 色調映射"] {
        assert!(
            !h.get_by_label_contains(label).accesskit_node().is_disabled(),
            "{label}"
        );
    }
    hover_menu_item(&mut h, "HDR 色調映射");
    assert!(!h.get_by_label("Hable").accesskit_node().is_disabled());
    assert!(!h.get_by_label_contains("目標亮度").accesskit_node().is_disabled());
    assert!(h.query_by_label("依畫面動態調整亮度").is_none());
    h.get_by_label_contains("去色帶").hover();
    h.run_steps(3);
    h.get_by_label("軟體繪圖模式不支援");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 控制面板：銳化、去色帶停用，去交錯照常
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    for (label, disabled) in [("銳化", true), ("去色帶", true), ("去交錯", false)] {
        let combo = combo_box(&h, label);
        assert_eq!(combo.accesskit_node().is_disabled(), disabled, "{label}");
    }
    // 設定頁：一樣停用去色帶、銳化、縮放演算法，並註明原因；去交錯、HDR 照常
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    for (label, disabled) in [("去色帶", true), ("銳化", true), ("曲線", false), ("色域對應", false)] {
        let combo = combo_box(&h, label);
        assert_eq!(combo.accesskit_node().is_disabled(), disabled, "{label}");
    }
    for (label, disabled) in [
        ("高品質", true),
        ("快速", true),
        ("不使用", true),
        ("開啟", false),
        ("自動", false),
    ] {
        assert_eq!(
            h.get_by_label(label).accesskit_node().is_disabled(),
            disabled,
            "{label}"
        );
    }
    h.get_by_label("軟體繪圖模式不支援");
    h.get_by_label("軟體繪圖模式不支援像素著色器");
}

/// 捲到看得到再點（設定頁比視窗長）。捲動有動畫，多跑幾幀等它停下來
fn click_in_view(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    h.run_steps(15);
    h.get_by_label(label).click();
    h.run_steps(2);
}

/// 設定頁「HDR → SDR」的目標亮度數值欄：點進去、打字（還沒按 Enter）
fn type_peak(h: &mut Harness<'_, VitascopeApp>, text: &str) {
    h.query_all_by_label("目標亮度")
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::SpinButton)
        .unwrap()
        .focus();
    h.run_steps(2);
    h.event(egui::Event::Text(text.into()));
    h.run_steps(2);
}

/// 捲到看得到、打開名稱是 `label` 的下拉選單
fn combo_in_view(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    combo_box(h, label).scroll_to_me();
    h.run_steps(15);
    combo_box(h, label).click();
    h.run_steps(2);
}

/// 名稱是 `label` 的下拉選單（左邊的名稱是它的無障礙標籤）
fn combo_box<'a>(h: &'a Harness<'_, VitascopeApp>, label: &'a str) -> egui_kittest::Node<'a> {
    h.query_all_by_label(label)
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::ComboBox)
        .unwrap_or_else(|| panic!("找不到下拉選單 {label}"))
}

// VITASCOPE_MPV_OPTS（這裡用 `Options.extra`）指定的畫質選項：影戲不去改它，介面上停用並說明
#[test]
fn video_options_set_by_mpv_opts_are_left_alone() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.deband = vitascope::picture::Strength::Off;
    let mut h = harness_launch_with(
        Options {
            extra: vec![
                ("deband".into(), "yes".into()),
                ("tone-mapping".into(), "clip".into()),
                ("scale".into(), "bicubic".into()),
                ("deinterlace".into(), "yes".into()),
            ],
            keep_open: true,
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_multitrack.mkv")],
            ..Default::default()
        },
        settings,
    );
    settle(&mut h, "mkv_multitrack.mkv");
    assert_eq!(prop(&h, "deband"), "yes", "啟動時不能蓋掉");
    assert_eq!(prop(&h, "tone-mapping"), "clip");
    assert_eq!(prop(&h, "deinterlace"), "yes");
    open_picture_menu(&mut h);
    assert!(h.get_by_label_contains("去色帶").accesskit_node().is_disabled());
    assert!(!h.get_by_label_contains("銳化").accesskit_node().is_disabled());
    h.get_by_label_contains("去色帶").hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    // 高品質：放大由使用者指定，其他照送
    pick_picture_item(&mut h, &["縮放演算法"], "高品質");
    wait_prop(&mut h, "scale-antiring", "0.600000");
    assert_eq!(prop(&h, "scale"), "bicubic");
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "縮放演算法");
    assert!(h.get_by_label_contains("放大（").accesskit_node().is_disabled());
    assert!(!h.get_by_label_contains("縮小（").accesskit_node().is_disabled());
    hover_menu_item(&mut h, "HDR 色調映射");
    assert!(
        h.get_by_label("Hable").accesskit_node().is_disabled(),
        "曲線由使用者指定"
    );
    assert!(!h.get_by_label_contains("目標亮度").accesskit_node().is_disabled());
    h.get_by_label("Hable").hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 設定頁：去交錯的選項停用，滑鼠移上去說明原因；顯示 mpv 實際的值
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    let on = h.get_by_label("開啟");
    assert!(on.accesskit_node().is_disabled());
    assert_eq!(on.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
    assert!(combo_box(&h, "曲線").accesskit_node().is_disabled());
    h.get_by_label("開啟").hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
}

// 控制面板「畫質」分頁的銳化、去色帶、去交錯
#[test]
fn control_panel_processing_combos() {
    let (_dir, path, mut h) = video_settings_harness("panel-combos", "common/mkv_multitrack.mkv");
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    combo_box(&h, "去色帶").click();
    h.run_steps(2);
    h.get_by_label("中等").click();
    h.run_steps(2);
    wait_prop(&mut h, "deband", "yes");
    assert_eq!(saved_video(&path)["deband"], "medium");
    assert_eq!(h.state().osd_text(), Some("去色帶：中等"));
    combo_box(&h, "銳化").click();
    h.run_steps(2);
    h.get_by_label("強").click();
    h.run_steps(2);
    wait_prop(&mut h, "sharpen", "1.000000");
    assert_eq!(saved_video(&path)["sharpen"], "strong");
    let caps = *h.state().engine_caps();
    if caps.deint_status {
        h.get_by_label(if caps.deint_auto {
            "（目前：逐行影片）"
        } else {
            "（目前：未去交錯）"
        });
    }
    combo_box(&h, "去交錯").click();
    h.run_steps(2);
    h.get_by_label("開啟").click();
    h.run_steps(2);
    wait_prop(&mut h, "deinterlace", "yes");
    assert_eq!(saved_video(&path)["deinterlace"], "on");
    if caps.deint_status {
        step_until(&mut h, "mpv 換好去交錯濾鏡", |s| s.deinterlace_active);
        h.run_steps(2);
        h.get_by_label("（目前：已去交錯）");
    }
}

// 「設定 → 畫質」的去交錯、去色帶／銳化、縮放演算法、HDR → SDR
#[test]
fn picture_page_processing_sections() {
    let (_dir, path, mut h) = video_settings_harness("page-processing", "common/mkv_multitrack.mkv");
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("畫質").click();
    h.run_steps(2);
    for title in ["去交錯", "去色帶／銳化", "縮放演算法", "HDR → SDR"] {
        h.get_by_label(title);
    }
    h.get_by_label("目前的影片不是 HDR");
    // 縮放演算法：三個按鈕；進階裡的放大 / 縮小 / 色度預設收起來
    h.get_by_label("快速").click();
    h.run_steps(2);
    wait_prop(&mut h, "cscale", "bilinear");
    assert_eq!(prop(&h, "scale"), "bilinear");
    assert_eq!(prop(&h, "dscale"), "bilinear");
    assert_eq!(saved_video(&path)["quality"], "fast");
    assert!(h.query_by_label("色度").is_none(), "進階預設收起來");
    click_in_view(&mut h, "進階");
    combo_in_view(&mut h, "縮小");
    h.run_steps(2);
    h.get_by_label("Catmull-Rom").click();
    h.run_steps(2);
    wait_prop(&mut h, "dscale", "catmull_rom");
    assert_eq!(saved_video(&path)["dscale"], "catmull_rom");
    // HDR：曲線、目標亮度（取消自動 → 203 nits，可以拖，最多 203）
    combo_in_view(&mut h, "曲線");
    h.run_steps(2);
    h.get_by_label("BT.2390").click();
    h.run_steps(2);
    wait_prop(&mut h, "tone-mapping", "bt.2390");
    assert_eq!(saved_video(&path)["tone"]["curve"], "bt2390");
    let peak = |h: &Harness<'_, VitascopeApp>| {
        h.query_all_by_label("目標亮度")
            .find(|n| n.accesskit_node().role() == egui::accesskit::Role::SpinButton)
            .map(|n| n.accesskit_node().is_disabled())
            .expect("找不到目標亮度的數值欄")
    };
    assert!(peak(&h), "自動時不能拖");
    click_in_view(&mut h, "自動");
    h.run_steps(2);
    assert_eq!(h.state().settings().video.tone.target_peak, Some(203));
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 203);
    assert!(!peak(&h), "取消自動之後可以拖");
    // 這是 SDR 影片：mpv 的目標亮度維持 auto
    h.run_steps(10);
    assert_eq!(prop(&h, "target-peak"), "auto");
    // 數值欄打字：馬上生效，按 Enter（離開欄位）才存檔
    type_peak(&mut h, "150");
    assert_eq!(h.state().settings().video.tone.target_peak, Some(150));
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 203, "還在打字，先不存");
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 150);
    // 超過 203 拉回 203
    type_peak(&mut h, "500");
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().settings().video.tone.target_peak, Some(203));
    assert_eq!(saved_video(&path)["tone"]["target_peak"], 203);
    // 色域對應：自動、裁切兩個（「降低飽和度」在 vo_gpu 跟自動是同一段程式，拿掉了）
    combo_in_view(&mut h, "色域對應");
    h.run_steps(2);
    assert!(h.query_by_label("降低飽和度").is_none());
    h.get_by_label("裁切").click();
    h.run_steps(2);
    wait_prop(&mut h, "gamut-mapping-mode", "clip");
    assert_eq!(saved_video(&path)["tone"]["gamut"], "clip");
    // 動態峰值偵測：介面測試沒有 GL context（畫面輸出不支援），不顯示
    assert!(h.query_by_label("動態峰值偵測").is_none());
    // 去交錯（引擎支援自動時三個都有；不支援時「自動」當成關閉，所以選開啟）
    let caps = *h.state().engine_caps();
    click_in_view(&mut h, "開啟");
    wait_prop(&mut h, "deinterlace", "yes");
    assert_eq!(saved_video(&path)["deinterlace"], "on");
    assert_eq!(h.query_by_label("自動（建議）").is_some(), caps.deint_auto);
}

#[test]
fn picture_page_processing_in_english() {
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mkv_multitrack.mkv")), settings);
    settle(&mut h, "mkv_multitrack.mkv");
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label("Video quality").click();
    h.run_steps(2);
    for label in [
        "Deinterlacing",
        "Debanding / sharpening",
        "Scaling",
        "HDR → SDR",
        "Advanced",
        "Fast",
        "Standard (mpv default)",
        "High quality",
        "Target brightness",
        "The current video isn't HDR",
    ] {
        h.get_by_label(label);
    }
    for combo in ["Debanding", "Sharpening", "Curve", "Gamut mapping"] {
        combo_box(&h, combo);
    }
    h.get_by_label("High quality").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Scaling: High quality"));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 右鍵選單
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Video quality ⏵");
    for label in ["Debanding", "Sharpening", "Scaling", "HDR tone mapping"] {
        h.get_by_label_contains(label);
    }
    hover_menu_item(&mut h, "Debanding");
    h.get_by_label("Strong").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Debanding: Strong"));
    assert!(h.query_by_label_contains("去色帶").is_none(), "沒有中文");
}

// ───────────── 像素著色器 ─────────────

/// 測試用的著色器檔案：反相（mpv 格式，有 DESC）、原樣輸出（mpv 格式）、PotPlayer / MPC 的 HLSL
fn write_shaders(dir: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let invert = dir.0.join("反相 測試.glsl");
    std::fs::write(
        &invert,
        "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC 反相\n\
         vec4 hook() { vec4 c = HOOKED_tex(HOOKED_pos); return vec4(1.0 - c.rgb, c.a); }\n",
    )
    .unwrap();
    let keep = dir.0.join("keep.glsl");
    std::fs::write(
        &keep,
        "//!HOOK MAIN\n//!BIND HOOKED\n//!DESC 原樣\nvec4 hook() { return HOOKED_tex(HOOKED_pos); }\n",
    )
    .unwrap();
    let hlsl = dir.0.join("bad.hlsl");
    std::fs::write(
        &hlsl,
        "sampler s0 : register(s0);\nfloat4 main(float2 tex : TEXCOORD0) : COLOR {\n  return 1 - tex2D(s0, tex);\n}\n",
    )
    .unwrap();
    (invert, keep, hlsl)
}

fn path_str(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 等到送出的 glsl-shaders 都生效、mpv 的清單符合條件（著色器是非同步設定的）
fn wait_shader_list(h: &mut Harness<'_, VitascopeApp>, what: &str, want: impl Fn(&[String]) -> bool) {
    step_until_app(h, what, |app| {
        app.player().shaders_settled() && app.player().shader_list().is_ok_and(|l| want(&l))
    });
}

/// 打開設定視窗的 `page` 頁
fn open_settings_page(h: &mut Harness<'_, VitascopeApp>, page: &str) {
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    h.get_by_label(page).click();
    h.run_steps(2);
}

#[test]
fn shader_preset_and_flip() {
    let (dir, path, mut h) = video_settings_harness("shader-preset", "common/mkv_multitrack.mkv");
    let (invert, keep, hlsl) = write_shaders(&dir);
    // 設定 → 畫質 → 新增組合
    open_settings_page(&mut h, "畫質");
    h.get_by_label("像素著色器");
    h.get_by_label("著色器檔案由你自己提供（例如 Anime4K、FSRCNNX 的 .glsl）");
    click_in_view(&mut h, "新增組合");
    let id = h.state().settings().video.shaders.presets[0].id;
    assert_ne!(id, 0);
    assert_eq!(saved_video(&path)["shaders"]["presets"][0]["name"], "組合 1");
    // 加入檔案（檔案對話框選好的）；.hlsl 被拒絕，設定頁上說明原因
    let rejected = h.state_mut().add_shader_files(id, &[invert.clone(), keep.clone()]);
    assert!(rejected.is_empty(), "{rejected:?}");
    let rejected = h.state_mut().add_shader_files(id, std::slice::from_ref(&hlsl));
    assert_eq!(rejected.len(), 1);
    h.run_steps(2);
    h.get_by_label("bad.hlsl：這不是 mpv 格式的 GLSL 著色器（需要 //!HOOK）");
    h.get_by_label("1. 反相 測試.glsl");
    h.get_by_label("2. keep.glsl");
    h.get_by_label("反相");
    let user = vec![path_str(&invert), path_str(&keep)];
    assert_eq!(h.state().settings().video.shaders.presets[0].files, user);
    assert_eq!(
        saved_video(&path)["shaders"]["presets"][0]["files"],
        serde_json::json!(user)
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 右鍵選單「畫質 ▸ 像素著色器」選這個組合
    pick_picture_item(&mut h, &["像素著色器"], "組合 1");
    assert_eq!(h.state().osd_text(), Some("像素著色器：組合 1（2 個檔案）"));
    wait_shader_list(&mut h, "使用組合", |l| l == user);
    assert_eq!(saved_video(&path)["shaders"]["active"], id);
    // Ctrl+Z：使用者的檔案之後接翻轉的著色器
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_shader_list(&mut h, "組合 + 左右翻轉", |l| {
        l.len() == 3 && l[..2] == user[..] && l[2].ends_with("hflip.glsl")
    });
    // 下一個檔案：組合照舊，翻轉拿掉
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    settle(&mut h, "mp4_h264_aac.mp4");
    assert!(h.state().geometry().is_default());
    wait_shader_list(&mut h, "換檔後只剩組合", |l| l == user);
    assert_eq!(h.state().settings().video.shaders.active, Some(id));
    // 不使用：清單是空的
    open_picture_menu(&mut h);
    hover_menu_item(&mut h, "像素著色器");
    assert_eq!(
        h.get_by_label("組合 1").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True),
        "選單上標出使用中的組合"
    );
    h.get_by_label("不使用").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("像素著色器：不使用"));
    wait_shader_list(&mut h, "不使用", |l| l.is_empty());
    assert_eq!(saved_video(&path)["shaders"]["active"], serde_json::Value::Null);
    // 「管理著色器…」打開設定的畫質頁（先在設定視窗換到別頁再關掉：不是剛好停在畫質頁）
    open_settings_page(&mut h, "一般");
    assert!(h.query_by_label("新增組合").is_none());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    pick_picture_item(&mut h, &["像素著色器"], "管理著色器…");
    h.get_by_label("新增組合");
}

#[test]
fn shader_editor_reorders_renames_and_deletes() {
    let dir = TempDir::new("shader-editor");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    let mut h = harness_with(None, settings);
    h.step();
    let (invert, keep, _) = write_shaders(&dir);
    let (invert, keep) = (path_str(&invert), path_str(&keep));
    open_settings_page(&mut h, "畫質");
    click_in_view(&mut h, "新增組合");
    let id = h.state().settings().video.shaders.presets[0].id;
    h.state_mut()
        .add_shader_files(id, &[PathBuf::from(&invert), PathBuf::from(&keep)]);
    h.run_steps(2);
    // 設定頁上選這個組合
    click_in_view(&mut h, "組合 1");
    wait_shader_list(&mut h, "使用組合", |l| l == [invert.clone(), keep.clone()]);
    // ↓：第一個移到後面；使用中的組合馬上重新套用
    h.query_all_by_label("↓").next().unwrap().click();
    h.run_steps(2);
    wait_shader_list(&mut h, "換了順序", |l| l == [keep.clone(), invert.clone()]);
    assert_eq!(
        saved_video(&path)["shaders"]["presets"][0]["files"],
        serde_json::json!([keep, invert])
    );
    // ✕：移除第二個
    h.query_all_by_label("✕").nth(1).unwrap().click();
    h.run_steps(2);
    wait_shader_list(&mut h, "移除", |l| l == [keep.clone()]);
    h.get_by_label("組合 1（1 個檔案）");
    // 改名：打字時標題跟著變，按 Enter 才存檔
    h.query_all_by_label("名稱")
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::TextInput)
        .unwrap()
        .focus();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Anime4K A".into()));
    h.run_steps(2);
    h.get_by_label("Anime4K A（1 個檔案）");
    assert_eq!(
        saved_video(&path)["shaders"]["presets"][0]["name"],
        "組合 1",
        "還在打字"
    );
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(saved_video(&path)["shaders"]["presets"][0]["name"], "Anime4K A");
    // 刪除使用中的組合：改成不使用
    click_in_view(&mut h, "刪除組合");
    wait_shader_list(&mut h, "刪除後不使用", |l| l.is_empty());
    assert!(h.state().settings().video.shaders.presets.is_empty());
    assert_eq!(h.state().settings().video.shaders.active, None);
    assert_eq!(saved_video(&path)["shaders"]["presets"], serde_json::json!([]));
}

#[test]
fn missing_shader_files_are_skipped_and_the_preset_kept() {
    let dir = TempDir::new("shader-missing");
    let (invert, _, _) = write_shaders(&dir);
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.shaders.presets = vec![vitascope::picture::ShaderPreset {
        id: 42,
        name: "A".into(),
        files: vec![path_str(&dir.0.join("不見了.glsl")), path_str(&invert)],
    }];
    settings.video.shaders.active = Some(42);
    let mut h = harness_with(None, settings);
    h.step();
    assert_eq!(h.state().osd_text(), Some("找不到著色器檔案，先略過：不見了.glsl"));
    wait_shader_list(&mut h, "略過找不到的", |l| l == [path_str(&invert)]);
    assert_eq!(h.state().settings().video.shaders.active, Some(42), "組合照舊");
    assert_eq!(h.state().settings().video.shaders.presets[0].files.len(), 2);
    // 啟動時使用中的組合也要看能不能用（自動測試沒有畫面，一直等著）
    assert!(h.state().shader_watch_pending());
    // 設定頁上標出找不到的檔案
    open_settings_page(&mut h, "畫質");
    click_in_view(&mut h, "A（2 個檔案）");
    h.get_by_label("⚠ 找不到檔案");
}

/// 編譯失敗的記錄（mpv 不會寫是哪個檔案，算組合的第一個）
const COMPILE_ERRORS: [&str; 2] = [
    "[libmpv_render] fragment shader compile log (status=0):",
    "[libmpv_render] 0(37) : error C1503: undefined variable \"no_such_variable\"",
];

// 換了組合之後畫不出來：還原到最後一個確定能用的組合、重新套用、存檔、提示、設定頁標出來
#[test]
fn shader_failure_reverts_to_the_last_working_preset() {
    let (dir, path, mut h) = video_settings_harness("shader-revert", "common/mkv_multitrack.mkv");
    let (invert, keep, _) = write_shaders(&dir);
    let third = dir.0.join("third.glsl");
    std::fs::copy(&keep, &third).unwrap();
    let (a, b, c) = (
        h.state_mut().add_shader_preset(),
        h.state_mut().add_shader_preset(),
        h.state_mut().add_shader_preset(),
    );
    h.state_mut().add_shader_files(a, std::slice::from_ref(&keep));
    h.state_mut().add_shader_files(b, std::slice::from_ref(&invert));
    h.state_mut().add_shader_files(c, std::slice::from_ref(&third));
    h.run_steps(2);
    // A：從畫出第一格開始算，30 格沒有錯誤就確定能用
    h.state_mut().simulate_video_frames(5);
    pick_picture_item(&mut h, &["像素著色器"], "組合 1");
    wait_shader_list(&mut h, "使用 A", |l| l == [path_str(&keep)]);
    assert!(h.state().shader_watch_pending(), "還沒畫");
    h.state_mut().simulate_video_frames(6);
    h.run_steps(2);
    assert!(h.state().shader_watch_pending(), "第一格才開始算");
    h.state_mut().simulate_video_frames(40);
    h.run_steps(2);
    assert!(!h.state().shader_watch_pending(), "30 格沒有錯誤");
    // B 編譯失敗：還原成 A
    pick_picture_item(&mut h, &["像素著色器"], "組合 2");
    assert!(h.state().shader_watch_pending());
    for e in COMPILE_ERRORS {
        h.state_mut().push_render_error(e);
    }
    h.run_steps(2);
    assert!(!h.state().shader_watch_pending());
    assert_eq!(h.state().settings().video.shaders.active, Some(a));
    assert_eq!(saved_video(&path)["shaders"]["active"], a);
    assert_eq!(
        h.state().osd_text(),
        Some(
            "像素著色器無法使用，已還原：反相 測試.glsl（0(37) : error C1503: undefined variable \"no_such_variable\"）"
        )
    );
    wait_shader_list(&mut h, "還原成 A", |l| l == [path_str(&keep)]);
    // 還沒看完 B 就換成 C、C 也失敗：還原成確定能用的 A，不是還沒確認的 B
    pick_picture_item(&mut h, &["像素著色器"], "組合 2");
    pick_picture_item(&mut h, &["像素著色器"], "組合 3");
    h.state_mut()
        .push_render_error("[libmpv_render] third.glsl: Unrecognized command 'HOKO'!");
    h.run_steps(2);
    assert_eq!(h.state().settings().video.shaders.active, Some(a));
    assert!(!h.state().shader_watch_pending());
    wait_shader_list(&mut h, "又還原成 A", |l| l == [path_str(&keep)]);
    // 設定頁上標出 B 的檔案畫不出來
    pick_picture_item(&mut h, &["像素著色器"], "組合 3");
    assert!(h.state().shader_watch_pending());
    open_settings_page(&mut h, "畫質");
    click_in_view(&mut h, "組合 2（1 個檔案）");
    h.get_by_label("⚠ 無法使用：0(37) : error C1503: undefined variable \"no_such_variable\"");
    // 刪除使用中、還在看的 C（最後新增的組合，設定頁上已展開，排在最後）：不再看，之後才到的錯誤不會把 A 換回來
    h.query_all_by_label("刪除組合").last().unwrap().scroll_to_me();
    h.run_steps(15);
    h.query_all_by_label("刪除組合").last().unwrap().click();
    h.run_steps(2);
    assert_eq!(h.state().settings().video.shaders.presets.len(), 2);
    assert_eq!(h.state().settings().video.shaders.active, None);
    assert!(!h.state().shader_watch_pending());
    h.state_mut()
        .push_render_error("[libmpv_render] third.glsl: Unrecognized command 'HOKO'!");
    h.run_steps(2);
    assert_eq!(h.state().settings().video.shaders.active, None);
    wait_shader_list(&mut h, "刪除後不使用", |l| l.is_empty());
}

// VITASCOPE_MPV_OPTS（這裡用 `Options.extra`）指定了 glsl-shaders：影戲不換它，選單停用並說明；翻轉照樣接在後面
#[test]
fn shaders_set_by_mpv_opts_are_left_alone() {
    let dir = TempDir::new("shader-opts");
    let (invert, keep, _) = write_shaders(&dir);
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.video.shaders.presets = vec![vitascope::picture::ShaderPreset {
        id: 7,
        name: "A".into(),
        files: vec![path_str(&invert), path_str(&dir.0.join("不見了.glsl"))],
    }];
    settings.video.shaders.active = Some(7);
    let mut h = harness_launch_with(
        Options {
            extra: vec![("glsl-shaders".into(), path_str(&keep))],
            keep_open: true,
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mkv_multitrack.mkv")],
            ..Default::default()
        },
        settings,
    );
    h.step();
    // 組合根本不用：不提示找不到檔案
    let osd = h.state().osd_text().unwrap_or_default().to_owned();
    assert!(!osd.contains("找不到著色器檔案"), "{osd}");
    settle(&mut h, "mkv_multitrack.mkv");
    assert_eq!(
        h.state().player().shader_list().unwrap(),
        [path_str(&keep)],
        "啟動時不能蓋掉"
    );
    assert!(!h.state().shader_watch_pending());
    open_picture_menu(&mut h);
    assert!(h.get_by_label_contains("像素著色器").accesskit_node().is_disabled());
    h.get_by_label_contains("像素著色器").hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_shader_list(&mut h, "使用者的 + 左右翻轉", |l| {
        l.len() == 2 && l[0] == path_str(&keep) && l[1].ends_with("hflip.glsl")
    });
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    settle(&mut h, "mp4_h264_aac.mp4");
    wait_shader_list(&mut h, "換檔後還是使用者的", |l| l == [path_str(&keep)]);
    open_settings_page(&mut h, "畫質");
    h.get_by_label("glsl-shaders 已由 VITASCOPE_MPV_OPTS 指定");
}

#[test]
fn shader_section_in_english() {
    let dir = TempDir::new("shader-en");
    let (_, _, hlsl) = write_shaders(&dir);
    let mut settings = Settings::default();
    settings.language = vitascope::i18n::Lang::En;
    let mut h = harness_with(None, settings);
    h.step();
    open_settings_page(&mut h, "Video quality");
    h.get_by_label("Pixel shaders");
    click_in_view(&mut h, "New preset");
    let id = h.state().settings().video.shaders.presets[0].id;
    h.state_mut().add_shader_files(id, &[hlsl]);
    h.run_steps(2);
    for label in [
        "Preset 1 (0 files)",
        "No files yet",
        "Add files…",
        "Delete preset",
        "bad.hlsl: This isn't an mpv GLSL shader (it needs //!HOOK)",
        "Bring your own shader files (for example the .glsl files of Anime4K or FSRCNNX)",
    ] {
        h.get_by_label(label);
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 沒開檔時畫面中間是起始畫面的按鈕：在左下角按右鍵
    let pos = h.get_by_label("Video").rect().left_bottom() + egui::vec2(30.0, -30.0);
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(2);
    hover_menu_item(&mut h, "Video quality ⏵");
    hover_menu_item(&mut h, "Pixel shaders");
    h.get_by_label("Preset 1").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Pixel shaders: Preset 1 (0 files)"));
    assert!(h.query_by_label_contains("著色器").is_none(), "沒有中文");
}

// ───────────── 音效：輸出裝置、獨佔模式、轉成立體聲、音訊直通 ─────────────

/// 開右鍵選單、把滑鼠移到「音效」上（子選單打開）。有檔案時在影片中間按，沒有時在左上角按（中間是起始畫面的按鈕）
fn open_sound_menu(h: &mut Harness<'_, VitascopeApp>) {
    if h.state().player().state.loaded {
        h.get_by_label("影片畫面").click_secondary();
    } else {
        let corner = egui::pos2(40.0, 40.0);
        h.event(egui::Event::PointerMoved(corner));
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos: corner,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
    }
    h.run_steps(2);
    hover_menu_item(h, "音效 ⏵");
}

/// 右鍵選單「音效」→ 子選單（`path`）→ 點 `item`（完整標籤）
fn pick_sound_item(h: &mut Harness<'_, VitascopeApp>, path: &[&str], item: &str) {
    open_sound_menu(h);
    for sub in path {
        hover_menu_item(h, sub);
    }
    h.get_by_label(item).click();
    h.run_steps(2);
}

/// 設定檔裡存的 `audio`
fn saved_audio(path: &std::path::Path) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    v["audio"].clone()
}

/// 假的裝置清單：auto + 這些（名稱, 說明）。用 coreaudio 的名稱：macOS 固定列 coreaudio，其他系統照清單上第一種，
/// 三個平台列出的都一樣；Linux 不是 PipeWire，不顯示獨佔模式
fn fake_devices(devices: &[(&str, &str)]) -> Vec<vitascope::sound::AudioDevice> {
    std::iter::once(("auto", "Autoselect device"))
        .chain(devices.iter().copied())
        .map(|(name, description)| vitascope::sound::AudioDevice {
            name: name.into(),
            description: description.into(),
        })
        .collect()
}

/// 控制列的音量滑桿停用了沒（控制列上另一個滑桿是進度條）
fn volume_slider_disabled(h: &Harness<'_, VitascopeApp>) -> bool {
    let volume: Vec<_> = h
        .query_all_by_role(egui::accesskit::Role::Slider)
        .filter(|s| s.accesskit_node().label().as_deref() != Some("進度"))
        .collect();
    assert_eq!(volume.len(), 1, "控制列只有一個音量滑桿");
    volume[0].accesskit_node().is_disabled()
}

/// 舊的引擎播放中改 audio-spdif 不會馬上生效：重新開檔（下一個檔案就照新的設定）
fn reopen_for_spdif(h: &mut Harness<'_, VitascopeApp>, file: &str) {
    drop_file(h, sample(file));
    let name = PathBuf::from(file).file_name().unwrap().to_string_lossy().into_owned();
    settle(h, &name);
}

const SPEAKERS: (&str, &str) = ("coreaudio/BuiltInSpeakerDevice", "內建喇叭");
const DAC: (&str, &str) = ("coreaudio/AppleUSBAudioEngine:DAC:1", "USB DAC");

/// 用暫存資料夾的設定檔（`change` 先改設定）、假的裝置清單啟動，播放 `file`（None = 不開檔）
fn sound_harness(
    name: &str,
    file: Option<&str>,
    devices: Option<&[(&str, &str)]>,
    change: impl FnOnce(&mut Settings),
) -> (TempDir, PathBuf, Harness<'static, VitascopeApp>) {
    sound_harness_with(name, file, devices, &[], change)
}

/// 重播同一個檔案（樣本只有 3 秒；步驟多的測試在 CI 上可能比樣本還久，播完就停在最後）
const LOOP_FILE: (&str, &str) = ("loop-file", "inf");

/// 同 `sound_harness`，另外指定 mpv 選項（`extra`，當成 VITASCOPE_MPV_OPTS）
fn sound_harness_with(
    name: &str,
    file: Option<&str>,
    devices: Option<&[(&str, &str)]>,
    extra: &[(&str, &str)],
    change: impl FnOnce(&mut Settings),
) -> (TempDir, PathBuf, Harness<'static, VitascopeApp>) {
    let dir = TempDir::new(name);
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    change(&mut settings);
    let mut player = Player::new(Options {
        keep_open: true,
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .unwrap();
    if let Some(list) = devices {
        player.set_fake_audio_devices(fake_devices(list));
    }
    let launch = Launch {
        files: file.map(sample).into_iter().collect(),
        ..Default::default()
    };
    let mut h = harness_with_player(player, launch, settings);
    match file {
        Some(f) => {
            let file_name = PathBuf::from(f).file_name().unwrap().to_string_lossy().into_owned();
            settle(&mut h, &file_name);
        }
        None => h.run_steps(3),
    }
    (dir, path, h)
}

#[test]
fn sound_menu_follows_picture_and_works_without_a_file() {
    let (_dir, _path, mut h) = sound_harness("sound-menu-place", None, None, |_| {});
    open_sound_menu(&mut h);
    // 「音效」緊接在「畫質」後面，沒開檔也能用；「音軌」照舊（每個檔案各自的）
    let picture = h.get_by_label("畫質 ⏵").rect();
    let sound = h.get_by_label("音效 ⏵");
    assert!(sound.rect().top() >= picture.bottom() - 1.0);
    assert!(sound.rect().top() <= picture.bottom() + 12.0, "緊接在後面");
    assert!(!sound.accesskit_node().is_disabled(), "沒開檔也能用");
    assert!(h.get_by_label("音軌 ⏵").accesskit_node().is_disabled());
    for item in ["多聲道轉成立體聲（5.1／7.1 → 2.0）", "輸出裝置 ⏵", "音訊直通"] {
        h.get_by_label(item);
    }
}

#[test]
fn audio_device_list_is_watched_after_the_first_frame() {
    // 沒存裝置、也沒打開選單或設定頁：第一個畫面出來之後就開始觀察裝置清單（mpv 同時開始偵測插拔），
    // 存下的裝置拔掉、插回來才接得到（真的清單；CI 沒有音訊裝置時只有 auto）
    let (_dir, _path, mut h) = sound_harness("sound-watch-devices", None, None, |_| {});
    step_until(&mut h, "開始觀察裝置清單", |s| s.audio_devices.is_some());
    let list = h.state().player().state.audio_devices.clone().unwrap();
    assert_eq!(list[0].name, vitascope::sound::AUTO_DEVICE, "{list:?}");
}

#[test]
fn downmix_toggle_sets_audio_channels() {
    let (_dir, path, mut h) = sound_harness("sound-downmix", Some("common/mp4_h264_aac.mp4"), None, |_| {});
    assert_eq!(prop(&h, "audio-channels"), "auto-safe");
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert_eq!(h.state().osd_text(), Some("轉成立體聲：開"));
    wait_prop(&mut h, "audio-channels", "stereo");
    // 混音時避免破音（預設開）跟著轉成立體聲生效
    wait_prop(&mut h, "audio-normalize-downmix", "yes");
    assert!(h.state().settings().audio.downmix);
    assert_eq!(saved_audio(&path)["downmix"], true);
    // 再點一次：關，回到 mpv 原本的值
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert_eq!(h.state().osd_text(), Some("轉成立體聲：關"));
    wait_prop(&mut h, "audio-channels", "auto-safe");
    wait_prop(&mut h, "audio-normalize-downmix", "no");
    assert_eq!(saved_audio(&path)["downmix"], false);
    // 播放沒有中斷
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
}

#[test]
fn passthrough_toggle_sets_audio_spdif() {
    let (_dir, path, mut h) = sound_harness("sound-passthrough", Some("common/mkv_hevc_ac3.mkv"), None, |_| {});
    assert_eq!(h.state().player().state.audio_spdif, None);
    let live = h.state().engine_caps().spdif_live;
    // 點了之後馬上看提示：新的引擎很快就開始直通，開始直通的提示會蓋掉它
    open_sound_menu(&mut h);
    h.get_by_label("音訊直通").click();
    h.run_steps(1);
    let engaged = "音訊直通：AC-3 → 擴大機（音量請用擴大機調整）";
    let osd = h.state().osd_text();
    if live {
        assert!(osd == Some("音訊直通：開") || osd == Some(engaged), "{osd:?}");
    } else {
        // 舊的引擎（系統的 libmpv 0.40 以前）播放中改了要到下一個檔案才生效：提示說明
        assert_eq!(osd, Some("音訊直通：開（下一個檔案開始生效）"));
    }
    wait_prop(&mut h, "audio-spdif", "ac3,eac3,dts");
    assert_eq!(saved_audio(&path)["passthrough"]["enabled"], true);
    if !live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    // ao=null 也接受直通：開始直通時提示
    step_until(&mut h, "開始直通", |s| s.audio_spdif.as_deref() == Some("ac3"));
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some(engaged));
    // 選單上註明使用中的格式
    open_sound_menu(&mut h);
    h.get_by_label("音訊直通（使用中：AC-3）").click();
    h.run_steps(2);
    let off = if live {
        "音訊直通：關"
    } else {
        "音訊直通：關（下一個檔案開始生效）"
    };
    assert_eq!(h.state().osd_text(), Some(off));
    wait_prop(&mut h, "audio-spdif", "");
    if !live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    step_until(&mut h, "改回一般輸出", |s| s.loaded && s.audio_spdif.is_none());
    assert_eq!(saved_audio(&path)["passthrough"]["enabled"], false);
}

#[test]
fn passthrough_blocks_volume_and_speed() {
    let (_dir, _path, mut h) = sound_harness("sound-spdif-block", Some("common/mkv_hevc_ac3.mkv"), None, |s| {
        s.audio.passthrough.enabled = true;
    });
    step_until(&mut h, "直通中", |s| {
        s.audio_spdif.as_deref() == Some("ac3") && s.volume == 100.0
    });
    let blocked = "音訊直通中：聲音由擴大機處理";
    // 控制列的音量滑桿停用，滑鼠移上去說明原因（還沒按音量鍵：說明不會跟 OSD 混在一起）
    assert!(volume_slider_disabled(&h));
    h.query_all_by_role(egui::accesskit::Role::Slider)
        .find(|s| s.accesskit_node().label().as_deref() != Some("進度"))
        .unwrap()
        .hover();
    h.run_steps(3);
    h.get_by_label(blocked);
    // 轉成立體聲也停用（直通的資料不經過混音）：右鍵選單、設定頁
    open_sound_menu(&mut h);
    let downmix = "多聲道轉成立體聲（5.1／7.1 → 2.0）";
    assert!(h.get_by_label(downmix).accesskit_node().is_disabled());
    h.get_by_label(downmix).hover();
    h.run_steps(3);
    h.get_by_label(blocked);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_settings_page(&mut h, "音效");
    assert!(h.get_by_label(downmix).accesskit_node().is_disabled());
    assert!(h.get_by_label("混音時避免破音").accesskit_node().is_disabled());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label(downmix).is_none(), "設定視窗關了");
    assert!(!h.state().settings().audio.downmix);
    // 音量鍵
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(3);
    assert_eq!(h.state().osd_text(), Some(blocked));
    assert_eq!(h.state().player().state.volume, 100.0);
    assert_eq!(h.state().player().get_f64("volume").unwrap(), 100.0);
    // 滾輪
    h.get_by_label("影片畫面").hover();
    h.run_steps(1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -2.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(3);
    assert_eq!(h.state().player().get_f64("volume").unwrap(), 100.0);
    // 控制列的音量滑桿停用（進度條照常）
    assert!(volume_slider_disabled(&h));
    // 變速鍵、右鍵選單的速度
    for key in [egui::Key::C, egui::Key::X] {
        h.key_press(key);
        h.run_steps(3);
        assert_eq!(h.state().osd_text(), Some("音訊直通中無法變速"));
        assert_eq!(h.state().player().get_f64("speed").unwrap(), 1.0);
    }
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("播放速度").click();
    h.run_steps(2);
    h.get_by_label("2×").click();
    h.run_steps(3);
    assert_eq!(h.state().osd_text(), Some("音訊直通中無法變速"));
    assert_eq!(h.state().player().get_f64("speed").unwrap(), 1.0);
    // 恢復正常速度（Z）不擋
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.key_press(egui::Key::Z);
    h.run_steps(3);
    assert_eq!(h.state().osd_text(), Some("速度 1×"));
    // 關掉直通之後照常
    pick_sound_item(&mut h, &[], "音訊直通（使用中：AC-3）");
    if !h.state().engine_caps().spdif_live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    step_until(&mut h, "改回一般輸出", |s| s.loaded && s.audio_spdif.is_none());
    h.key_press(egui::Key::ArrowDown);
    step_until(&mut h, "音量 95", |s| s.volume == 95.0);
    h.key_press(egui::Key::C);
    step_until(&mut h, "加快到 1.1×", |s| close_to(s.speed, 1.1));
    assert!(!volume_slider_disabled(&h));
}

#[test]
fn missing_audio_device_falls_back_to_auto() {
    // 存下的裝置不在這台電腦上（真的裝置清單；CI 沒有音訊裝置也一樣）
    let gone = "wasapi/{00000000-0000-0000-0000-00000000dead}";
    let set_gone = |s: &mut Settings| {
        s.audio.device = Some(gone.into());
        s.audio.device_label = Some("拔掉的 USB DAC".into());
    };
    // 啟動時同步讀裝置清單：建好視窗（第一幀，還沒開始觀察清單）時就已經是預設裝置，拔掉的裝置沒送給 mpv
    {
        let dir = TempDir::new("sound-missing-device-startup");
        let mut settings = Settings::load_from(dir.0.join("settings.json"));
        set_gone(&mut settings);
        let h = harness_launch(Launch::default(), settings);
        assert_eq!(prop(&h, "audio-device"), "auto", "啟動時就用預設裝置");
        assert_eq!(
            h.state().osd_text(),
            Some("找不到音訊裝置「拔掉的 USB DAC」，改用預設裝置")
        );
    }
    let (_dir, path, mut h) = sound_harness("sound-missing-device", None, None, set_gone);
    assert_eq!(prop(&h, "audio-device"), "auto", "暫時用預設裝置");
    assert_eq!(
        h.state().osd_text(),
        Some("找不到音訊裝置「拔掉的 USB DAC」，改用預設裝置")
    );
    // 存下的名稱留著（插回來時切回去）
    assert_eq!(h.state().settings().audio.device.as_deref(), Some(gone));
    // 開檔照樣播放（有聲音輸出的路徑）
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    settle(&mut h, "mp4_h264_aac.mp4");
    assert_eq!(prop(&h, "audio-device"), "auto");
    // 選單列出它（不能選），存檔時名稱照舊
    open_sound_menu(&mut h);
    hover_menu_item(&mut h, "輸出裝置");
    let item = h.get_by_label("拔掉的 USB DAC（找不到，暫用預設裝置）");
    assert!(item.accesskit_node().is_disabled());
    h.get_by_label("預設裝置（跟隨系統）");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert_eq!(saved_audio(&path)["device"], gone);
    assert_eq!(saved_audio(&path)["device_label"], "拔掉的 USB DAC");
    assert_eq!(prop(&h, "audio-device"), "auto");
}

#[test]
fn audio_device_switches_back_when_plugged_in() {
    let (_dir, path, mut h) = sound_harness(
        "sound-device-back",
        Some("common/mp4_h264_aac.mp4"),
        Some(&[SPEAKERS]),
        |s| {
            s.audio.device = Some(DAC.0.into());
            s.audio.device_label = Some(DAC.1.into());
        },
    );
    assert_eq!(prop(&h, "audio-device"), "auto");
    // 插回來：自動切回去
    h.state_mut().fake_audio_devices(fake_devices(&[SPEAKERS, DAC]));
    wait_prop(&mut h, "audio-device", DAC.0);
    assert_eq!(h.state().osd_text(), Some("已切換回 USB DAC"));
    // 播放中又拔掉：mpv 先用拔掉的裝置重開音訊輸出，開不起來就把音軌關掉（這裡直接關掉音軌模擬）。
    // 改用預設裝置，再提示一次，並把音軌選回來（不然整個檔案都沒有聲音）
    let audio = h.state().player().state.selected(TrackKind::Audio).map(|t| t.id);
    assert!(audio.is_some());
    h.state_mut().set_option_async(AsyncKey::AudioDevice, "aid", "no");
    step_until(&mut h, "mpv 關掉音軌", |s| s.selected(TrackKind::Audio).is_none());
    h.run_steps(2);
    h.state_mut().fake_audio_devices(fake_devices(&[SPEAKERS]));
    wait_prop(&mut h, "audio-device", "auto");
    assert_eq!(h.state().osd_text(), Some("找不到音訊裝置「USB DAC」，改用預設裝置"));
    step_until(&mut h, "選回原本的音軌", |s| {
        s.selected(TrackKind::Audio).map(|t| t.id) == audio
    });
    // 只提示一次：清單又變了（多了別的裝置）但 USB DAC 還是不在，不再提示
    h.key_press(egui::Key::ArrowDown);
    step_until(&mut h, "音量 95", |s| s.volume == 95.0);
    let volume_osd = h.state().osd_text().map(str::to_owned);
    assert!(
        volume_osd.as_deref().is_some_and(|t| t.contains("95")),
        "{volume_osd:?}"
    );
    h.state_mut()
        .fake_audio_devices(fake_devices(&[SPEAKERS, ("coreaudio/Headphones", "耳機")]));
    h.run_steps(5);
    assert_eq!(h.state().osd_text().map(str::to_owned), volume_osd);
    assert_eq!(prop(&h, "audio-device"), "auto");
    // 選了別的裝置：不再等 USB DAC
    pick_sound_item(&mut h, &["輸出裝置"], "內建喇叭");
    wait_prop(&mut h, "audio-device", SPEAKERS.0);
    h.state_mut().fake_audio_devices(fake_devices(&[SPEAKERS, DAC]));
    h.run_steps(5);
    assert_eq!(prop(&h, "audio-device"), SPEAKERS.0);
    assert_eq!(saved_audio(&path)["device"], SPEAKERS.0);
}

#[test]
fn sound_menu_device_items() {
    let (_dir, path, mut h) = sound_harness(
        "sound-devices",
        Some("common/mp4_h264_aac.mp4"),
        Some(&[SPEAKERS, DAC]),
        |_| {},
    );
    open_sound_menu(&mut h);
    hover_menu_item(&mut h, "輸出裝置");
    for item in ["預設裝置（跟隨系統）", "內建喇叭", "USB DAC"] {
        h.get_by_label(item);
    }
    // 獨佔模式：Windows、macOS 顯示；Linux 只有 PipeWire 才有
    let desktop = cfg!(any(windows, target_os = "macos"));
    assert_eq!(h.query_by_label("獨佔模式").is_some(), desktop);
    h.get_by_label("USB DAC").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("音訊輸出：USB DAC"));
    wait_prop(&mut h, "audio-device", DAC.0);
    assert_eq!(saved_audio(&path)["device"], DAC.0);
    assert_eq!(saved_audio(&path)["device_label"], "USB DAC");
    // 媒體資訊的輸出裝置：顯示裝置的說明；面板開著時換裝置也跟著換（每秒重讀）
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    h.get_by_label_contains("· USB DAC");
    pick_sound_item(&mut h, &["輸出裝置"], "內建喇叭");
    wait_prop(&mut h, "audio-device", SPEAKERS.0);
    let start = Instant::now();
    while h.query_by_label_contains("· 內建喇叭").is_none() {
        assert!(start.elapsed() < TIMEOUT, "媒體資訊沒有跟著換裝置");
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(h.query_by_label_contains("· USB DAC").is_none());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::I);
    h.run_steps(2);
    // 預設裝置 = auto，存檔是 null
    pick_sound_item(&mut h, &["輸出裝置"], "預設裝置（跟隨系統）");
    assert_eq!(h.state().osd_text(), Some("音訊輸出：預設裝置（跟隨系統）"));
    wait_prop(&mut h, "audio-device", "auto");
    assert_eq!(saved_audio(&path)["device"], serde_json::Value::Null);
    if desktop {
        pick_sound_item(&mut h, &["輸出裝置"], "獨佔模式");
        assert_eq!(h.state().osd_text(), Some("獨佔模式：開"));
        wait_prop(&mut h, "audio-exclusive", "yes");
        assert_eq!(saved_audio(&path)["exclusive"], true);
    }
}

#[test]
fn sound_settings_page() {
    let (_dir, path, mut h) = sound_harness("sound-page", None, Some(&[SPEAKERS, DAC]), |_| {});
    open_settings_page(&mut h, "音效");
    for title in ["輸出裝置", "聲道", "音訊直通"] {
        h.get_by_label(title);
    }
    let desktop = cfg!(any(windows, target_os = "macos"));
    assert_eq!(h.query_by_label("獨佔模式").is_some(), desktop);
    h.get_by_label(if desktop {
        "直通時會獨佔這個裝置，其他程式暫時沒有聲音；TrueHD／DTS-HD 需要 HDMI 支援 HBR"
    } else {
        "需要 HDMI／IEC958 裝置"
    });
    // 輸出裝置的下拉選單
    combo_box(&h, "裝置").click();
    h.run_steps(2);
    h.get_by_label("USB DAC").click();
    h.run_steps(2);
    wait_prop(&mut h, "audio-device", DAC.0);
    assert_eq!(saved_audio(&path)["device"], DAC.0);
    // 混音時避免破音：轉成立體聲之後才能改
    assert!(h.get_by_label("混音時避免破音").accesskit_node().is_disabled());
    h.get_by_label("多聲道轉成立體聲（5.1／7.1 → 2.0）").click();
    h.run_steps(2);
    wait_prop(&mut h, "audio-normalize-downmix", "yes");
    let normalize = h.get_by_label("混音時避免破音");
    assert!(!normalize.accesskit_node().is_disabled());
    normalize.click();
    h.run_steps(2);
    wait_prop(&mut h, "audio-normalize-downmix", "no");
    assert_eq!(saved_audio(&path)["normalize_downmix"], false);
    assert_eq!(prop(&h, "audio-channels"), "stereo");
    // 音訊直通：格式要先啟用才能勾
    assert!(h.get_by_label("TrueHD").accesskit_node().is_disabled());
    click_in_view(&mut h, "啟用");
    wait_prop(&mut h, "audio-spdif", "ac3,eac3,dts");
    for (codec, value) in [("TrueHD", "ac3,eac3,dts,truehd"), ("AC-3", "eac3,dts,truehd")] {
        click_in_view(&mut h, codec);
        wait_prop(&mut h, "audio-spdif", value);
    }
    let saved = saved_audio(&path)["passthrough"].clone();
    assert_eq!(saved["truehd"], true);
    assert_eq!(saved["ac3"], false);
    // 關掉時格式記著，mpv 的 audio-spdif 清空
    click_in_view(&mut h, "啟用");
    wait_prop(&mut h, "audio-spdif", "");
    assert_eq!(saved_audio(&path)["passthrough"]["truehd"], true);
}

#[test]
fn sound_ui_in_english() {
    let (_dir, _path, mut h) = sound_harness("sound-en", Some("common/mkv_hevc_ac3.mkv"), Some(&[DAC]), |s| {
        s.language = vitascope::i18n::Lang::En;
        s.audio.passthrough.enabled = true;
    });
    step_until(&mut h, "passthrough", |s| s.audio_spdif.is_some());
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some("Passthrough: AC-3 → amplifier (use the amplifier's volume)")
    );
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some("Passthrough is on: the amplifier handles the sound")
    );
    h.key_press(egui::Key::C);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Can't change the speed during passthrough"));
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Sound ⏵");
    for label in ["Downmix to stereo (5.1/7.1 → 2.0)", "Passthrough (active: AC-3)"] {
        h.get_by_label(label);
    }
    hover_menu_item(&mut h, "Output device");
    h.get_by_label("Default device (follow the system)");
    h.get_by_label("USB DAC").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Audio output: USB DAC"));
    open_settings_page(&mut h, "Sound");
    for label in [
        "Output device",
        "Channels",
        "Avoid clipping when downmixing",
        "Enable",
        "E-AC-3",
    ] {
        h.get_by_label(label);
    }
    h.get_by_label("Active: AC-3");
    assert!(h.query_by_label_contains("音").is_none(), "沒有中文");
}

// VITASCOPE_MPV_OPTS（這裡用 `Options.extra`）指定的音效選項：影戲不去改它，介面上停用並說明
#[test]
fn sound_options_set_by_mpv_opts_are_left_alone() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    // 存了指定的裝置也不管（使用者自己指定了 audio-device）
    settings.audio.device = Some("wasapi/{00000000-0000-0000-0000-00000000dead}".into());
    let mut h = harness_launch_with(
        Options {
            extra: vec![
                ("audio-channels".into(), "stereo".into()),
                ("audio-device".into(), "auto".into()),
            ],
            keep_open: true,
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mp4_h264_aac.mp4")],
            ..Default::default()
        },
        settings,
    );
    settle(&mut h, "mp4_h264_aac.mp4");
    assert_eq!(prop(&h, "audio-channels"), "stereo", "啟動時不能蓋掉");
    assert_eq!(h.state().osd_text(), None, "使用者指定了裝置：不檢查存下的裝置");
    open_sound_menu(&mut h);
    let downmix = h.get_by_label("多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert!(downmix.accesskit_node().is_disabled());
    assert!(!h.get_by_label("音訊直通").accesskit_node().is_disabled());
    h.get_by_label("多聲道轉成立體聲（5.1／7.1 → 2.0）").hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    // 指定了 audio-device：子選單照樣打得開，只停用裝置；獨佔模式（audio-exclusive）照常可以改
    assert!(!h.get_by_label("輸出裝置 ⏵").accesskit_node().is_disabled());
    hover_menu_item(&mut h, "輸出裝置");
    let auto = h.get_by_label("預設裝置（跟隨系統）");
    assert!(auto.accesskit_node().is_disabled());
    auto.hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    if cfg!(any(windows, target_os = "macos")) {
        assert!(!h.get_by_label("獨佔模式").accesskit_node().is_disabled());
    }
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 直通照常可以改（不是使用者指定的）；聲道還是使用者的
    pick_sound_item(&mut h, &[], "音訊直通");
    wait_prop(&mut h, "audio-spdif", "ac3,eac3,dts");
    assert_eq!(prop(&h, "audio-channels"), "stereo");
    open_settings_page(&mut h, "音效");
    assert!(combo_box(&h, "裝置").accesskit_node().is_disabled());
    assert!(
        h.get_by_label("多聲道轉成立體聲（5.1／7.1 → 2.0）")
            .accesskit_node()
            .is_disabled()
    );
}

#[test]
fn passthrough_codecs_set_by_mpv_opts_say_why() {
    // 使用者指定了 audio-spdif：啟用和每個格式都停用，滑鼠移上去說明原因
    let mut settings = Settings::default();
    settings.audio.passthrough.enabled = true;
    let mut h = harness_launch_with(
        Options {
            extra: vec![("audio-spdif".into(), "ac3".into())],
            ..Options::headless()
        },
        Launch::default(),
        settings,
    );
    h.run_steps(3);
    open_settings_page(&mut h, "音效");
    for item in ["啟用", "AC-3", "TrueHD"] {
        let node = h.get_by_label(item);
        assert!(node.accesskit_node().is_disabled(), "{item}");
        node.hover();
        h.run_steps(3);
        h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    }
}

// ───────────── 音效：等化器、音量平衡、音量上限（影戲自己的 af 濾鏡鏈） ─────────────

/// 引擎有沒有等化器、音量平衡、限幅器的濾鏡（本專案建置的引擎、CI 的系統 libmpv 都有；舊的引擎略過）
fn has_af_filters(h: &Harness<'_, VitascopeApp>, test: &str) -> bool {
    let caps = h.state().engine_caps().af;
    if !caps.all() {
        eprintln!("{test}：播放引擎缺少音訊濾鏡（{caps:?}），略過");
    }
    caps.all()
}

/// 等到 mpv 的 af 符合條件（af 是非同步設定的；讀回來的寫法是 lavfi=graph=%長度%…，比對裡面的濾鏡）
fn wait_af(h: &mut Harness<'_, VitascopeApp>, what: &str, cond: impl Fn(&str) -> bool) {
    let start = Instant::now();
    loop {
        h.step();
        let af = prop(h, "af");
        if cond(&af) {
            return;
        }
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}\naf = {af}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 有濾鏡被 mpv 停用（濾鏡失敗時 mpv 記一筆錯誤，播放照樣繼續）
fn disabled_filters(h: &Harness<'_, VitascopeApp>) -> Vec<String> {
    h.state()
        .player()
        .recent_errors()
        .iter()
        .filter(|e| e.contains("Disabling filter"))
        .cloned()
        .collect()
}

/// 濾鏡失敗的紀錄穩定下來（1 秒內沒有新的，最多等 5 秒）之後的內容
fn settled_disabled_filters(h: &mut Harness<'_, VitascopeApp>) -> Vec<String> {
    let mut last = disabled_filters(h);
    for _ in 0..5 {
        wait_real(h, 1.0);
        let now = disabled_filters(h);
        if now == last {
            break;
        }
        last = now;
    }
    last
}

#[test]
fn sound_menu_items() {
    let (_dir, path, mut h) = sound_harness_with(
        "sound-eq-menu",
        Some("common/mp4_h264_aac.mp4"),
        None,
        &[LOOP_FILE],
        |_| {},
    );
    if !has_af_filters(&h, "sound_menu_items") {
        return;
    }
    assert_eq!(prop(&h, "af"), "", "預設什麼都沒開");
    // 等化器預設 → 搖滾：順便打開等化器；最高 +6 dB，自動防止破音的前級 −6 dB 在限幅器
    pick_sound_item(&mut h, &["等化器預設"], "搖滾");
    assert_eq!(h.state().osd_text(), Some("等化器：搖滾"));
    wait_af(&mut h, "搖滾", |af| {
        af.contains("equalizer@b1=f=31:t=o:w=1:g=5")
            && af.contains("equalizer@b10=f=16000:t=o:w=1:g=6")
            && af.contains("alimiter@lim=level_in=0.501187")
    });
    let saved = saved_audio(&path);
    assert_eq!(saved["eq"]["enabled"], true);
    assert_eq!(saved["eq"]["preset"], "rock");
    // 音量平衡 → 夜間模式
    pick_sound_item(&mut h, &["音量平衡"], "夜間模式（小聲變大、大聲變小）");
    assert_eq!(h.state().osd_text(), Some("音量平衡：夜間模式"));
    wait_af(&mut h, "夜間模式", |af| {
        af.contains("acompressor@c=threshold=0.125:ratio=4") && af.contains("equalizer@b1=")
    });
    assert_eq!(saved_audio(&path)["leveling"], "night");
    // 取消勾選等化器：只剩音量平衡和限幅器
    pick_sound_item(&mut h, &[], "等化器");
    assert_eq!(h.state().osd_text(), Some("等化器：關"));
    wait_af(&mut h, "等化器關掉", |af| {
        !af.contains("equalizer") && af.contains("acompressor") && af.contains("level_in=1:")
    });
    // 人聲平衡、音量平均
    pick_sound_item(&mut h, &["音量平衡"], "人聲平衡");
    assert_eq!(h.state().osd_text(), Some("音量平衡：人聲平衡"));
    wait_af(&mut h, "人聲平衡", |af| {
        af.contains("speechnorm@s=") && !af.contains("acompressor")
    });
    pick_sound_item(&mut h, &["音量平衡"], "音量平均");
    wait_af(&mut h, "音量平均", |af| af.contains("dynaudnorm@d="));
    assert_eq!(saved_audio(&path)["leveling"], "normalize");
    // 轉成立體聲
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert_eq!(h.state().osd_text(), Some("轉成立體聲：開"));
    wait_prop(&mut h, "audio-channels", "stereo");
    // 音量上限 150%；音量平衡關掉之後濾鏡鏈只剩限幅器，再改回 100% 就清空
    pick_sound_item(&mut h, &["音量上限"], "150%");
    assert_eq!(h.state().osd_text(), Some("音量上限：150%"));
    assert_eq!(saved_audio(&path)["volume_max"], 150);
    pick_sound_item(&mut h, &["音量平衡"], "關閉");
    assert_eq!(h.state().osd_text(), Some("音量平衡：關閉"));
    wait_af(&mut h, "只剩限幅器", |af| {
        af.contains("alimiter@lim") && !af.contains("dynaudnorm")
    });
    pick_sound_item(&mut h, &["音量上限"], "100%");
    wait_af(&mut h, "全部關掉", str::is_empty);
    // 「等化器…」打開控制面板的音效分頁
    pick_sound_item(&mut h, &[], "等化器…");
    h.get_by_label("控制面板");
    h.get_by_label("自動防止破音");
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
}

#[test]
fn volume_up_past_100_when_max_raised() {
    let (_dir, path, mut h) = sound_harness_with(
        "sound-boost-keys",
        Some("common/mp4_h264_aac.mp4"),
        None,
        &[LOOP_FILE],
        |s| s.audio.volume_max = 150,
    );
    if !h.state().engine_caps().af.alimiter {
        eprintln!("volume_up_past_100_when_max_raised：播放引擎沒有 alimiter，略過");
        return;
    }
    step_until(&mut h, "音量 100", |s| s.volume == 100.0);
    // 音量上限超過 100%：一開始就有限幅器（放大 1 倍），之後調音量只改它的輸入增益
    wait_af(&mut h, "限幅器", |af| af.contains("alimiter@lim=level_in=1:"));
    let sent = h.state().af_commands_sent();
    for _ in 0..10 {
        h.key_press(egui::Key::ArrowUp);
        h.step();
    }
    // 按著音量鍵：先送 af-command 馬上聽得到，整條 af 等停下來 300 毫秒才改寫（不會每按一下就重建濾鏡）
    assert!(h.state().af_commands_sent() > sent, "按音量鍵先送 af-command");
    assert!(h.state().af_rewrite_pending(), "停下來才改寫");
    assert!(prop(&h, "af").contains("level_in=1:"), "還沒改寫：{}", prop(&h, "af"));
    h.run_steps(2);
    let player = h.state().player();
    assert_eq!(player.volume_total(), 150.0, "到音量上限");
    assert_eq!(player.get_f64("volume").unwrap(), 100.0, "mpv 的音量停在 100");
    assert_eq!(h.state().osd_text(), Some("音量 150%（放大）"));
    // 停下來之後改寫 af：放大 1.5³ = 3.375 倍
    wait_af(&mut h, "放大寫進字串", |af| af.contains("level_in=3.375:"));
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().player().volume_total(), 150.0, "不超過音量上限");
    h.key_press(egui::Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("音量 145%（放大）"));
    wait_af(&mut h, "145%", |af| af.contains("level_in=3.048625:"));
    // 存檔存總音量
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["volume"], 145.0);
    // 回到 100% 以下：mpv 自己的音量，放大歸零
    for _ in 0..10 {
        h.key_press(egui::Key::ArrowDown);
        h.step();
    }
    step_until(&mut h, "音量 95", |s| s.volume == 95.0);
    assert_eq!(h.state().player().volume_total(), 95.0);
    assert_eq!(h.state().osd_text(), Some("音量 95%"));
    wait_af(&mut h, "放大歸零", |af| af.contains("level_in=1:"));
    // 滾輪也可以超過 100%
    h.get_by_label("影片畫面").hover();
    h.run_steps(1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 3.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(3);
    assert_eq!(h.state().player().volume_total(), 110.0);
    assert_eq!(h.state().osd_text(), Some("音量 110%（放大）"));
    // 控制列的音量滑桿：範圍到音量上限；放開（或用無障礙操作設定）時下一幀就改寫 af，不用等 300 毫秒
    set_volume_slider(&mut h, 150.0);
    assert_eq!(h.state().player().volume_total(), 150.0);
    assert_eq!(h.state().player().get_f64("volume").unwrap(), 100.0);
    assert!(!h.state().af_rewrite_pending(), "放開滑桿就改寫");
    wait_af(&mut h, "滑桿 150%", |af| af.contains("level_in=3.375:"));
    // 調低音量上限：總音量也拉下來
    pick_sound_item(&mut h, &["音量上限"], "100%");
    assert_eq!(h.state().player().volume_total(), 100.0);
    wait_af(&mut h, "沒有限幅器", str::is_empty);
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
}

#[test]
fn af_rewritten_directly_without_a_file() {
    // 沒開檔時濾鏡鏈還沒建立，af-command 沒有對象：不送，直接改寫整條 af（不用等 300 毫秒）
    let (_dir, _path, mut h) = sound_harness("sound-af-no-file", None, None, |s| {
        s.audio.volume_max = 150;
        s.audio.eq.enabled = true;
    });
    if !has_af_filters(&h, "af_rewritten_directly_without_a_file") {
        return;
    }
    assert!(!h.state().player().state.loaded);
    wait_af(&mut h, "平坦 + 限幅器", |af| {
        af.contains("equalizer@b6=f=1000:t=o:w=1:g=0,") && af.contains("alimiter@lim=level_in=1:")
    });
    let sent = h.state().af_commands_sent();
    // 音量鍵超過 100%：放大 1.1³ = 1.331 倍
    for _ in 0..2 {
        h.key_press(egui::Key::ArrowUp);
        h.step();
    }
    assert_eq!(h.state().player().volume_total(), 110.0);
    assert_eq!(h.state().af_commands_sent(), sent, "沒開檔不送 af-command");
    assert!(!h.state().af_rewrite_pending(), "直接改寫，不用等");
    wait_af(&mut h, "放大寫進字串", |af| af.contains("level_in=1.331:"));
    // 控制面板的等化器滑桿也一樣
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("音效").click();
    h.run_steps(2);
    set_slider(&mut h, "1k", 6.0);
    assert_eq!(h.state().af_commands_sent(), sent, "沒開檔不送 af-command");
    assert!(!h.state().af_rewrite_pending());
    wait_af(&mut h, "1 kHz +6", |af| af.contains("equalizer@b6=f=1000:t=o:w=1:g=6,"));
}

#[test]
fn startup_volume_capped_at_100() {
    // 上次放大到 150%：啟動時從 100% 開始（開啟時不會突然很大聲）
    let (_dir, path, h) = sound_harness("sound-startup-volume", None, None, |s| {
        s.audio.volume_max = 150;
        s.volume = 150.0;
    });
    assert_eq!(h.state().player().get_f64("volume").unwrap(), 100.0);
    assert_eq!(h.state().player().volume_total(), 100.0);
    assert_eq!(h.state().player().boost_pct(), 0.0);
    assert_eq!(h.state().settings().audio.volume_max, 150, "音量上限照樣記得");
    drop(h);
    // 120% 也一樣（mpv 自己的 volume-max 預設 130，擋不住 120）
    let (_dir3, _path3, h) = sound_harness("sound-startup-volume-120", None, None, |s| {
        s.audio.volume_max = 150;
        s.volume = 120.0;
    });
    assert_eq!(h.state().player().get_f64("volume").unwrap(), 100.0);
    assert_eq!(h.state().player().volume_total(), 100.0);
    drop(h);
    // 100% 以下照存下的值
    let (_dir2, _path2, h) = sound_harness("sound-startup-volume-low", None, None, |s| s.volume = 40.0);
    assert_eq!(h.state().player().get_f64("volume").unwrap(), 40.0);
    let _ = path;
}

#[test]
fn startup_sets_the_af_chain_before_the_first_frame() {
    // 啟動時同步設定濾鏡鏈（排在開第一個檔案之前，第一個檔案從第一個聲音就有效）：
    // app 一建好、還沒跑任何一幀，af 就已經是整條
    let dir = TempDir::new("sound-startup-af");
    let mut settings = Settings::load_from(dir.0.join("settings.json"));
    settings.audio.eq.enabled = true;
    settings.audio.eq.preset = vitascope::sound::EqPreset::Rock;
    settings.audio.volume_max = 150;
    let player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new((String::new(), 0)));
    let seen_in_app = seen.clone();
    let h = Harness::builder().with_size([960.0, 600.0]).build_eframe(move |cc| {
        let app = VitascopeApp::new(cc, player, settings, Launch::default());
        *seen_in_app.lock().unwrap() = (
            app.player().get_string("af").unwrap_or_default(),
            app.af_sets_in_flight(),
        );
        app
    });
    if !has_af_filters(&h, "startup_sets_the_af_chain_before_the_first_frame") {
        return;
    }
    let (af, in_flight) = seen.lock().unwrap().clone();
    assert!(
        af.contains(ROCK_B1) && af.contains("alimiter@lim=level_in=0.501187:"),
        "建好 app 時：{af}"
    );
    assert_eq!(in_flight, 0, "同步設定，不是排在後面的非同步指令");
}

/// 把垂直的滑桿從中間拖到最上面（還沒放開時先跑 `holding`）
fn drag_vertical_slider_to_top(
    h: &mut Harness<'_, VitascopeApp>,
    label: &str,
    holding: impl FnOnce(&mut Harness<'_, VitascopeApp>),
) {
    let rect = h.get_by_label(label).rect();
    let (from, to) = (rect.center(), egui::pos2(rect.center().x, rect.top() - 20.0));
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    for e in [
        egui::Event::PointerMoved(from),
        button(from, true),
        egui::Event::PointerMoved(egui::pos2(from.x, from.y - 10.0)),
        egui::Event::PointerMoved(to),
    ] {
        h.event(e);
        h.step();
    }
    holding(h);
    h.event(button(to, false));
    h.step();
    h.run_steps(2);
}

#[test]
fn control_panel_sound_tab() {
    let (_dir, path, mut h) = sound_harness_with(
        "sound-panel",
        Some("common/mp4_h264_aac.mp4"),
        None,
        &[LOOP_FILE],
        |_| {},
    );
    if !has_af_filters(&h, "control_panel_sound_tab") {
        return;
    }
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("音效").click();
    h.run_steps(2);
    // 十段滑桿，各有無障礙標籤；等化器沒開時停用
    for band in ["31", "62", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"] {
        let node = h.get_by_label(band);
        assert_eq!(node.accesskit_node().role(), egui::accesskit::Role::Slider, "{band}");
        assert!(node.accesskit_node().is_disabled(), "{band}");
    }
    h.get_by_label("等化器").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("等化器：平坦"));
    wait_af(&mut h, "平坦", |af| af.contains("equalizer@b6=f=1000:t=o:w=1:g=0,"));
    assert!(!h.get_by_label("1k").accesskit_node().is_disabled());
    // 用滑鼠拖 1 kHz 那一段到最上面：拖曳中馬上變成「自訂」、先送 af-command，放開才存檔、改寫 af
    let sent = h.state().af_commands_sent();
    drag_vertical_slider_to_top(&mut h, "1k", |h| {
        let eq = h.state().settings().audio.eq;
        assert_eq!(eq.preset, vitascope::sound::EqPreset::Custom);
        assert_eq!(eq.gains[5], 12.0);
        assert_eq!(saved_audio(&path)["eq"]["preset"], "flat", "放開才存檔");
        assert!(h.state().af_commands_sent() > sent, "拖曳中先送 af-command");
        assert!(h.state().af_rewrite_pending(), "放開才改寫");
        let af = prop(h, "af");
        assert!(af.contains("equalizer@b6=f=1000:t=o:w=1:g=0,"), "放開才改寫：{af}");
    });
    wait_af(&mut h, "1 kHz +12", |af| {
        af.contains("equalizer@b6=f=1000:t=o:w=1:g=12,") && af.contains("level_in=0.251189:")
    });
    let saved = saved_audio(&path);
    assert_eq!(saved["eq"]["preset"], "custom");
    assert_eq!(saved["eq"]["gains"][5], 12.0);
    h.get_by_label("（前級 -12.0 dB）");
    assert_eq!(combo_box(&h, "預設").accesskit_node().value().as_deref(), Some("自訂"));
    // 用無障礙操作（鍵盤、螢幕閱讀器）改 31 Hz
    set_slider(&mut h, "31", -6.0);
    wait_af(&mut h, "31 Hz -6", |af| af.contains("equalizer@b1=f=31:t=o:w=1:g=-6,"));
    assert_eq!(saved_audio(&path)["eq"]["gains"][0], -6.0);
    // 不自動防止破音：前級回到 1
    h.get_by_label("自動防止破音").click();
    h.run_steps(2);
    wait_af(&mut h, "沒有前級", |af| af.contains("level_in=1:"));
    assert_eq!(saved_audio(&path)["eq"]["auto_preamp"], false);
    // 還原：全部 0
    h.get_by_label("還原").click();
    h.run_steps(2);
    wait_af(&mut h, "還原", |af| {
        af.contains("equalizer@b1=f=31:t=o:w=1:g=0,") && af.contains("equalizer@b6=f=1000:t=o:w=1:g=0,")
    });
    assert_eq!(saved_audio(&path)["eq"]["preset"], "flat");
    // 音量平衡、音量上限的下拉選單
    combo_box(&h, "音量平衡").click();
    h.run_steps(2);
    h.get_by_label("夜間模式（小聲變大、大聲變小）").click();
    h.run_steps(2);
    wait_af(&mut h, "夜間模式", |af| af.contains("acompressor@c="));
    combo_box(&h, "音量上限").click();
    h.run_steps(2);
    h.get_by_label("200%").click();
    h.run_steps(2);
    assert_eq!(h.state().settings().audio.volume_max, 200);
    // 轉成立體聲
    h.get_by_label("轉成立體聲").click();
    h.run_steps(2);
    wait_prop(&mut h, "audio-channels", "stereo");
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
}

#[test]
fn sound_settings_page_eq_and_volume() {
    let (_dir, path, mut h) = sound_harness("sound-page-eq", None, None, |_| {});
    if !has_af_filters(&h, "sound_settings_page_eq_and_volume") {
        return;
    }
    open_settings_page(&mut h, "音效");
    h.get_by_label("平坦（關閉）");
    // 音量上限：說明經過限幅器
    let max = combo_box(&h, "音量上限");
    max.hover();
    h.run_steps(3);
    h.get_by_label("超過 100% 會經過限幅器，不會爆音");
    combo_in_view(&mut h, "音量上限");
    h.get_by_label("130%").click();
    h.run_steps(2);
    assert_eq!(saved_audio(&path)["volume_max"], 130);
    wait_af(&mut h, "限幅器", |af| af.contains("alimiter@lim=level_in=1:"));
    combo_in_view(&mut h, "音量平衡");
    h.get_by_label("人聲平衡").click();
    h.run_steps(2);
    assert_eq!(saved_audio(&path)["leveling"], "dialogue");
    wait_af(&mut h, "人聲平衡", |af| af.contains("speechnorm@s="));
    // 「等化器…」打開控制面板的音效分頁
    click_in_view(&mut h, "等化器…");
    h.get_by_label("控制面板");
    h.get_by_label("自動防止破音");
}

/// 開了音訊直通，等化器用「搖滾」
fn spdif_with_rock(s: &mut Settings) {
    s.audio.passthrough.enabled = true;
    s.audio.eq.enabled = true;
    s.audio.eq.preset = vitascope::sound::EqPreset::Rock;
}

/// 「搖滾」的 31 Hz 那一段（af 裡有等化器）
const ROCK_B1: &str = "equalizer@b1=f=31:t=o:w=1:g=5";

/// 用無障礙操作設定控制列的音量滑桿（控制列上另一個滑桿是進度條）
fn set_volume_slider(h: &mut Harness<'_, VitascopeApp>, value: f64) {
    let (target_node, target_tree) = h
        .query_all_by_role(egui::accesskit::Role::Slider)
        .find(|s| s.accesskit_node().label().as_deref() != Some("進度"))
        .expect("控制列的音量滑桿")
        .accesskit_node()
        .locate();
    h.event(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::SetValue,
        target_tree,
        target_node,
        data: Some(egui::accesskit::ActionData::NumericValue(value)),
    }));
    h.run_steps(2);
}

#[test]
fn passthrough_track_switch_clears_the_eq_chain() {
    // 換音軌：換成會直通的（AC-3）之前先清空 af（同步，排在選音軌之前），濾鏡不會碰到直通的資料；
    // 換成不會直通的（外掛的 FLAC）之後等化器回來
    let dir = TempDir::new("sound-spdif-track");
    let video = dir.0.join("雙音軌.mkv");
    std::fs::copy(sample("common/mkv_hevc_ac3.mkv"), &video).unwrap();
    // 同名的外掛音軌（FLAC）會自動載入
    std::fs::copy(sample("common/mkv_extaudio.mka"), dir.0.join("雙音軌.mka")).unwrap();
    let mut settings = Settings::load_from(dir.0.join("settings.json"));
    settings.auto_next = false;
    spdif_with_rock(&mut settings);
    let mut h = harness_launch_with(
        Options {
            keep_open: true,
            extra: vec![(LOOP_FILE.0.into(), LOOP_FILE.1.into())],
            ..Options::headless()
        },
        Launch {
            files: vec![video],
            ..Default::default()
        },
        settings,
    );
    if !has_af_filters(&h, "passthrough_track_switch_clears_the_eq_chain") {
        return;
    }
    settle(&mut h, "雙音軌.mkv");
    step_until(&mut h, "直通中、載入外掛音軌", |s| {
        s.audio_spdif.as_deref() == Some("ac3") && s.tracks_of(TrackKind::Audio).count() == 2
    });
    assert_eq!(prop(&h, "af"), "", "直通中沒有濾鏡");
    let ac3 = h
        .state()
        .player()
        .state
        .tracks_of(TrackKind::Audio)
        .find(|t| !t.external)
        .unwrap()
        .label();
    // 換成外掛的 FLAC：不會直通，直通結束之後等化器回來
    h.get_by_label("音軌").click();
    h.run_steps(2);
    h.get_by_label_contains("雙音軌.mka").click();
    step_until(&mut h, "換成 FLAC、不再直通", |s| {
        s.selected(TrackKind::Audio).is_some_and(|t| t.external) && s.audio_spdif.is_none()
    });
    wait_af(&mut h, "FLAC 有等化器", |af| af.contains(ROCK_B1));
    // 換回 AC-3：選音軌的那一幀 af 就已經清空（還沒開始直通）
    h.get_by_label("音軌").click();
    h.run_steps(2);
    h.get_by_label_contains(&ac3).click();
    h.step();
    assert_eq!(prop(&h, "af"), "", "選會直通的音軌之前先清空");
    step_until(&mut h, "又開始直通", |s| s.audio_spdif.as_deref() == Some("ac3"));
    wait_real(&mut h, 0.5);
    assert_eq!(prop(&h, "af"), "");
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
}

/// 直通開不起來、mpv 改回 PCM 之後：沒在播的話往回跳一下（見 `passthrough_refused_restores_the_eq_chain`）
fn kick_after_spdif_fallback(h: &mut Harness<'_, VitascopeApp>) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        h.step();
        if prop(h, "core-idle") == "no" && !prop(h, "audio-out-params").is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(2);
}

#[test]
fn passthrough_refused_restores_the_eq_chain() {
    // 輸出（擴大機、輸出方式）不支援直通時 mpv 改回 PCM 播放：預測會直通而清空的濾鏡鏈，確定沒有直通之後設回來。
    // 之後重新開啟音訊輸出（轉成立體聲、換裝置）時 mpv 會再試一次直通：濾鏡鏈要先清空，不然濾鏡碰到直通的資料會失敗。
    // ao=null 的 format 選項模擬只接受 float 的輸出（直通的格式開不起來）
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-spdif-refused",
        Some("common/mkv_hevc_ac3.mkv"),
        None,
        &[LOOP_FILE, ("ao-null-format", "float")],
        spdif_with_rock,
    );
    if !has_af_filters(&h, "passthrough_refused_restores_the_eq_chain") {
        return;
    }
    if prop(&h, "ao-null-format") != "float" {
        eprintln!("passthrough_refused_restores_the_eq_chain：這個引擎的 ao=null 沒有 format 選項，略過");
        return;
    }
    // 這個引擎在檔案一開始就改回 PCM 時會停住（core-idle，沒有濾鏡鏈也一樣），跳轉一下才繼續播
    kick_after_spdif_fallback(&mut h);
    wait_af(&mut h, "沒有直通，等化器回來", |af| af.contains(ROCK_B1));
    assert_eq!(h.state().player().state.audio_spdif, None);
    wait_real(&mut h, 0.3);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
    // 轉成立體聲：mpv 重新開啟音訊輸出、再試一次直通。送出之前濾鏡鏈已經清空
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert_eq!(prop(&h, "af"), "", "重新開啟音訊輸出之前先清空");
    wait_prop(&mut h, "audio-channels", "stereo");
    kick_after_spdif_fallback(&mut h);
    wait_af(&mut h, "又沒有直通，等化器回來", |af| af.contains(ROCK_B1));
    wait_real(&mut h, 0.3);
    assert_eq!(h.state().player().state.audio_spdif, None);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
}

#[test]
fn passthrough_clears_and_restores_the_eq_chain() {
    // 直通的資料不能過濾鏡：開了直通又開了等化器，AC-3 檔案直通之前 af 已經清空（沒有濾鏡失敗），
    // 關掉直通之後等化器回來、照樣有作用
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-spdif-eq",
        Some("common/mkv_hevc_ac3.mkv"),
        None,
        &[LOOP_FILE],
        spdif_with_rock,
    );
    if !has_af_filters(&h, "passthrough_clears_and_restores_the_eq_chain") {
        return;
    }
    step_until(&mut h, "直通中", |s| s.audio_spdif.as_deref() == Some("ac3"));
    h.run_steps(5);
    assert_eq!(prop(&h, "af"), "", "直通中沒有濾鏡");
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
    // 等化器、音量平衡在直通中停用，說明原因
    open_sound_menu(&mut h);
    let eq = h.get_by_label("等化器");
    assert!(eq.accesskit_node().is_disabled());
    eq.hover();
    h.run_steps(3);
    h.get_by_label("音訊直通中：聲音由擴大機處理");
    assert!(h.get_by_label_contains("音量平衡").accesskit_node().is_disabled());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 關掉直通：等化器回來（舊的引擎要重新開檔）
    pick_sound_item(&mut h, &[], "音訊直通（使用中：AC-3）");
    let live = h.state().engine_caps().spdif_live;
    if !live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    step_until(&mut h, "改回一般輸出", |s| s.loaded && s.audio_spdif.is_none());
    wait_af(&mut h, "等化器回來", |af| {
        af.contains("equalizer@b1=f=31:t=o:w=1:g=5")
    });
    wait_real(&mut h, 0.5);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
    open_sound_menu(&mut h);
    assert!(
        !h.get_by_label("等化器").accesskit_node().is_disabled(),
        "直通結束：等化器又可以用"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 再打開直通：af 先清空才開始直通，濾鏡不會失敗
    pick_sound_item(&mut h, &[], "音訊直通");
    if !live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    step_until(&mut h, "又開始直通", |s| s.audio_spdif.as_deref() == Some("ac3"));
    assert_eq!(prop(&h, "af"), "");
    wait_real(&mut h, 0.5);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
    // 換成不會直通的檔案：開檔前先清空，載入後看了音軌（AAC）再把等化器放回去
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    settle(&mut h, "mp4_h264_aac.mp4");
    wait_af(&mut h, "AAC 檔案有等化器", |af| {
        af.contains("equalizer@b1=f=31:t=o:w=1:g=5")
    });
    assert_eq!(h.state().player().state.audio_spdif, None);
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
}

#[test]
fn unpredicted_spdif_clears_and_revives_the_eq_chain() {
    // 預測不到的直通（VITASCOPE_MPV_OPTS 指定了 audio-spdif，影戲的直通設定是關的）：真的開始直通之後清空 af，
    // 直通結束再把等化器設回來（先清空再設，濾鏡重新建立）
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-spdif-unpredicted",
        Some("common/mkv_hevc_ac3.mkv"),
        None,
        &[LOOP_FILE, ("audio-spdif", "ac3")],
        |s| {
            s.audio.eq.enabled = true;
            s.audio.eq.preset = vitascope::sound::EqPreset::Rock;
        },
    );
    if !has_af_filters(&h, "unpredicted_spdif_clears_and_revives_the_eq_chain") {
        return;
    }
    assert!(h.state().player().user_overrides().contains("audio-spdif"));
    assert!(!h.state().settings().audio.passthrough.enabled);
    step_until(&mut h, "直通中", |s| s.audio_spdif.as_deref() == Some("ac3"));
    wait_af(&mut h, "直通開始之後清空", str::is_empty);
    wait_real(&mut h, 0.3);
    assert_eq!(prop(&h, "af"), "", "直通中沒有濾鏡");
    // 濾鏡在清空之前可能已經碰到直通的資料、失敗了（預測不到，沒辦法事先清空）：記下來，之後比對內容。
    // af 屬性先變成空的，舊的濾鏡鏈要等音訊執行緒重建才拆掉，失敗的紀錄可能晚一點才到（CI 的 macOS 上超過 0.3 秒）：
    // 等紀錄一秒內沒有再變才記下
    let mut failed = settled_disabled_filters(&mut h);
    // 關掉直通（舊的引擎要重新開檔）：等化器回來，不再失敗
    h.state().player().mpv().set_property("audio-spdif", "").unwrap();
    if !h.state().engine_caps().spdif_live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
        // 重新開檔時錯誤紀錄清空了
        failed.clear();
    }
    step_until(&mut h, "改回一般輸出", |s| s.loaded && s.audio_spdif.is_none());
    wait_af(&mut h, "等化器回來", |af| af.contains(ROCK_B1));
    wait_real(&mut h, 0.5);
    assert_eq!(disabled_filters(&h), failed, "等化器重新建立，沒有再失敗");
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
}

#[test]
fn af_set_by_mpv_opts_is_left_alone() {
    // 使用者用 VITASCOPE_MPV_OPTS 指定了 af：影戲不改它；等化器、音量平衡停用並說明；
    // 音量超過 100% 改用 mpv 自己的音量（沒有限幅器）
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.audio.volume_max = 150;
    settings.audio.eq.enabled = true;
    let user_af = "lavfi=[anull]";
    let mut h = harness_launch_with(
        Options {
            extra: vec![("af".into(), user_af.into())],
            keep_open: true,
            ..Options::headless()
        },
        Launch {
            files: vec![sample("common/mp4_h264_aac.mp4")],
            ..Default::default()
        },
        settings,
    );
    settle(&mut h, "mp4_h264_aac.mp4");
    let before = prop(&h, "af");
    assert!(before.contains("anull"), "{before}");
    assert!(h.state().player().user_overrides().contains("af"));
    open_sound_menu(&mut h);
    let eq = h.get_by_label("等化器");
    assert!(eq.accesskit_node().is_disabled());
    eq.hover();
    h.run_steps(3);
    h.get_by_label("已由 VITASCOPE_MPV_OPTS 指定");
    assert!(h.get_by_label_contains("等化器預設").accesskit_node().is_disabled());
    assert!(h.get_by_label_contains("音量平衡").accesskit_node().is_disabled());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    for _ in 0..10 {
        h.key_press(egui::Key::ArrowUp);
        h.step();
    }
    step_until(&mut h, "mpv 自己的音量 150", |s| s.volume == 150.0);
    assert_eq!(h.state().player().boost_pct(), 0.0);
    assert_eq!(h.state().osd_text(), Some("音量 150%（放大）"));
    h.run_steps(30);
    assert_eq!(prop(&h, "af"), before, "使用者的 af 不能被改掉");
}

#[test]
fn sound_eq_ui_in_english() {
    let (_dir, _path, mut h) = sound_harness("sound-eq-en", Some("common/mp4_h264_aac.mp4"), None, |s| {
        s.language = vitascope::i18n::Lang::En;
        s.audio.volume_max = 130;
    });
    if !has_af_filters(&h, "sound_eq_ui_in_english") {
        return;
    }
    h.key_press(egui::Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Volume 105% (boosted)"));
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Sound ⏵");
    for label in ["Equalizer…", "Equalizer"] {
        h.get_by_label(label);
    }
    h.get_by_label("Volume leveling ⏵");
    h.get_by_label("Volume limit ⏵");
    hover_menu_item(&mut h, "Equalizer preset");
    h.get_by_label("Rock").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Equalizer: Rock"));
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Sound ⏵");
    hover_menu_item(&mut h, "Volume leveling");
    h.get_by_label("Night mode (quiet parts louder, loud parts quieter)")
        .click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Volume leveling: Night mode"));
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("Sound").click();
    h.run_steps(2);
    for label in ["Prevent clipping", "Reset", "Downmix to stereo", "1k", "16k"] {
        h.get_by_label(label);
    }
    combo_box(&h, "Preset");
    combo_box(&h, "Volume limit");
    // 關掉控制面板（它的「Sound」分頁跟設定的「Sound」頁同名）
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    open_settings_page(&mut h, "Sound");
    h.get_by_label("Rock (on)");
    assert!(h.query_by_label_contains("等化器").is_none(), "沒有中文");
    assert!(h.query_by_label_contains("音量").is_none(), "沒有中文");
}

/// 控制列的靜音按鈕（`icon` 是現在的圖示）
fn mute_button<'a>(h: &'a Harness<'_, VitascopeApp>, icon: &'a str) -> egui_kittest::Node<'a> {
    h.get_by_label(icon)
}

#[test]
fn passthrough_mute_is_left_to_the_amplifier() {
    // 上次存了靜音、開了直通：mpv 的靜音是軟體音量，直通的資料不經過它（擴大機照樣出聲）。
    // 提示裡說要用擴大機，圖示不顯示靜音、按鈕停用，M 跟音量鍵一樣不動作；存下的靜音不動，改回一般輸出時照樣靜音
    let (_dir, _path, mut h) = sound_harness("sound-spdif-mute", Some("common/mkv_hevc_ac3.mkv"), None, |s| {
        s.audio.passthrough.enabled = true;
        s.muted = true;
    });
    step_until(&mut h, "直通中", |s| {
        s.audio_spdif.as_deref() == Some("ac3") && s.muted
    });
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some("音訊直通：AC-3 → 擴大機（靜音、音量請用擴大機調整）")
    );
    assert!(h.query_by_label("🔇").is_none(), "直通中不顯示靜音的圖示");
    let button = mute_button(&h, "🔊");
    assert!(button.accesskit_node().is_disabled());
    button.hover();
    h.run_steps(3);
    h.get_by_label("音訊直通中：聲音由擴大機處理");
    // M：不動作，說明交給擴大機
    h.key_press(egui::Key::M);
    h.run_steps(3);
    assert_eq!(h.state().osd_text(), Some("音訊直通中：聲音由擴大機處理"));
    assert_eq!(prop(&h, "mute"), "yes", "存下的靜音不動");
    // 關掉直通：照樣靜音，M、按鈕照常
    pick_sound_item(&mut h, &[], "音訊直通（使用中：AC-3）");
    if !h.state().engine_caps().spdif_live {
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    step_until(&mut h, "改回一般輸出", |s| s.loaded && s.audio_spdif.is_none());
    h.run_steps(2);
    assert!(!mute_button(&h, "🔇").accesskit_node().is_disabled());
    h.key_press(egui::Key::M);
    step_until(&mut h, "取消靜音", |s| !s.muted);
    assert_eq!(h.state().osd_text(), Some("取消靜音"));

    // 音量 0% 也一樣（直通中照樣出聲）
    let (_dir, _path, mut h) = sound_harness("sound-spdif-zero", Some("common/mkv_hevc_ac3.mkv"), None, |s| {
        s.audio.passthrough.enabled = true;
        s.volume = 0.0;
    });
    step_until(&mut h, "直通中", |s| {
        s.audio_spdif.as_deref() == Some("ac3") && s.volume == 0.0
    });
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some("音訊直通：AC-3 → 擴大機（靜音、音量請用擴大機調整）")
    );
    assert!(h.query_by_label("🔇").is_none());
}

#[test]
fn passthrough_resets_the_speed() {
    // 1.5× 播放中開始直通：直通的資料不能變速（mpv 會丟掉、重複整個封包，擴大機的聲音斷斷續續），改回 1× 並說明
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-spdif-speed",
        Some("common/mkv_hevc_ac3.mkv"),
        None,
        &[LOOP_FILE],
        |_| {},
    );
    for _ in 0..5 {
        h.key_press(egui::Key::C);
        h.run_steps(1);
    }
    step_until(&mut h, "1.5×", |s| close_to(s.speed, 1.5));
    pick_sound_item(&mut h, &[], "音訊直通");
    if !h.state().engine_caps().spdif_live {
        // 舊的引擎要重開檔案才會直通（重開之後一開始直通就改回 1×，這裡不能再檢查速度：跟改回來的時機搶）
        reopen_for_spdif(&mut h, "common/mkv_hevc_ac3.mkv");
    }
    step_until(&mut h, "直通中", |s| s.audio_spdif.as_deref() == Some("ac3"));
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some("音訊直通：AC-3 → 擴大機（音量請用擴大機調整）（直通時不能變速，已改回 1×）")
    );
    step_until(&mut h, "改回 1×", |s| s.speed == 1.0);
    assert_eq!(h.state().player().get_f64("speed").unwrap(), 1.0);
}

#[test]
fn adjustments_item_opens_the_picture_tab() {
    // 「等化器…」打開音效分頁之後，「影像調整…」（右鍵選單、設定頁）要打開畫質分頁；面板開著時換過去。
    // Alt+G 只是開關，分頁照上次的
    let (_dir, _path, mut h) = sound_harness("panel-tabs", Some("common/mp4_h264_aac.mp4"), None, |_| {});
    pick_sound_item(&mut h, &[], "等化器…");
    h.get_by_label("自動防止破音");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("控制面板").is_none());
    h.key_press_modifiers(egui::Modifiers::ALT, egui::Key::G);
    h.run_steps(2);
    h.get_by_label("自動防止破音");
    // 面板開著（音效分頁）：右鍵選單「畫質 ▸ 影像調整…」沒有標成開著；按了換到畫質分頁
    open_picture_menu_from_corner(&mut h);
    assert_ne!(
        h.get_by_label_contains("影像調整…").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True),
        "開著的是音效分頁"
    );
    h.get_by_label_contains("影像調整…").click();
    h.run_steps(2);
    h.get_by_label("控制面板");
    h.get_by_label("亮度");
    assert!(h.query_by_label("自動防止破音").is_none(), "不是音效分頁");
    open_picture_menu_from_corner(&mut h);
    assert_eq!(
        h.get_by_label_contains("影像調整…").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True),
        "開著的是畫質分頁"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label("亮度");
    // 換回音效分頁、關掉面板：設定頁畫質的「影像調整…」也打開畫質分頁
    h.get_by_label("音效").click();
    h.run_steps(2);
    h.get_by_label("自動防止破音");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    open_settings_page(&mut h, "畫質");
    click_in_view(&mut h, "影像調整…");
    h.get_by_label("控制面板");
    h.get_by_label("亮度");
    assert!(h.query_by_label("自動防止破音").is_none(), "不是音效分頁");
}

#[test]
fn changing_the_output_reselects_a_dropped_audio_track() {
    // 音訊輸出開不起來時 mpv 把音軌關掉（這裡直接關掉模擬）：音訊輸出已經關了，光改轉成立體聲（裝置、獨佔模式也一樣）
    // mpv 不會重開，要把音軌選回來
    let (_dir, _path, mut h) = sound_harness("sound-reselect", Some("common/mp4_h264_aac.mp4"), None, |_| {});
    let audio = h.state().player().state.selected(TrackKind::Audio).map(|t| t.id);
    assert!(audio.is_some());
    h.state_mut().set_option_async(AsyncKey::AudioDevice, "aid", "no");
    step_until(&mut h, "mpv 關掉音軌", |s| s.selected(TrackKind::Audio).is_none());
    h.run_steps(2);
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    wait_prop(&mut h, "audio-channels", "stereo");
    step_until(&mut h, "選回原本的音軌", |s| {
        s.selected(TrackKind::Audio).map(|t| t.id) == audio
    });
    step_until(&mut h, "照樣播放", |s| s.loaded && !s.paused);
}

#[test]
fn reselected_passthrough_track_clears_the_eq_chain_first() {
    // 同上，被關掉的是會直通的 AC-3、等化器開著：選回來會直通，濾鏡鏈要在選回來之前清空
    //（跟選單換音軌一樣），濾鏡不會碰到直通的資料
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-reselect-spdif",
        Some("common/mkv_hevc_ac3.mkv"),
        None,
        &[LOOP_FILE],
        spdif_with_rock,
    );
    if !has_af_filters(&h, "reselected_passthrough_track_clears_the_eq_chain_first") {
        return;
    }
    // 啟動前就開了直通：舊的引擎（系統的 libmpv 0.37）也是第一個檔案就直通，不用重開檔案
    step_until(&mut h, "直通中", |s| s.audio_spdif.as_deref() == Some("ac3"));
    let audio = h.state().player().state.selected(TrackKind::Audio).map(|t| t.id);
    h.state_mut().set_option_async(AsyncKey::AudioDevice, "aid", "no");
    step_until(&mut h, "mpv 關掉音軌", |s| s.selected(TrackKind::Audio).is_none());
    wait_af(&mut h, "沒有直通：等化器設回來", |af| af.contains(ROCK_B1));
    h.run_steps(2);
    let errors_before = disabled_filters(&h).len();
    pick_sound_item(&mut h, &[], "多聲道轉成立體聲（5.1／7.1 → 2.0）");
    assert_eq!(prop(&h, "af"), "", "選回會直通的音軌之前先清空");
    step_until(&mut h, "選回原本的音軌、直通中", |s| {
        s.selected(TrackKind::Audio).map(|t| t.id) == audio && s.audio_spdif.as_deref() == Some("ac3")
    });
    wait_real(&mut h, 0.5);
    assert_eq!(prop(&h, "af"), "");
    assert_eq!(disabled_filters(&h).len(), errors_before, "{:?}", disabled_filters(&h));
}

#[test]
fn audio_output_failure_keeps_playing_with_a_notice() {
    // 音訊輸出開不起來（ao 自動選、強制用不存在的裝置模擬）：mpv 改用 null 繼續播放，提示一次。
    // 純音樂檔以前會直接結束（「無法開啟音訊裝置」）
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-ao-fallback",
        None,
        Some(&[SPEAKERS]),
        &[
            ("ao", ""),
            ("audio-device", "wasapi/{00000000-0000-0000-0000-00000000dead}"),
            LOOP_FILE,
        ],
        |_| {},
    );
    drop_file(&mut h, sample("general/audio_flac.flac"));
    step_until_app(&mut h, "提示沒有聲音", |app| {
        app.osd_text() == Some("無法開啟音訊裝置，暫時沒有聲音")
    });
    step_until(&mut h, "照樣播放", |s| {
        playing(s, "audio_flac.flac") && s.time_pos > 0.3
    });
    assert!(h.state().player().state.selected(TrackKind::Audio).is_some());
    // 只提示一次：還是 null 也不會一直提示
    step_until_app(&mut h, "提示消失", |app| app.osd_text().is_none());
    wait_real(&mut h, 1.5);
    assert_eq!(h.state().osd_text(), None);
    assert!(h.state().player().audio_fell_back());
    assert_eq!(h.state().audio_output_retries(), 0);
    // 下一個檔案：mpv 會沿用改用的 null（同樣格式的不重開），要重開一次再試真正的裝置；還是開不起來就再提示
    drop_file(&mut h, sample("general/audio_flac.flac"));
    step_until_app(&mut h, "換檔時重開音訊輸出", |app| {
        app.audio_output_retries() == 1
    });
    step_until_app(&mut h, "還是開不起來：再提示", |app| {
        app.osd_text() == Some("無法開啟音訊裝置，暫時沒有聲音")
    });
    step_until(&mut h, "照樣播放", |s| s.loaded && s.time_pos > 0.3);
    // 真的重開了：這個檔案又試了一次指定的裝置（沒重開的話沿用 null，這個檔案沒有音訊輸出的錯誤）
    let errors = h.state().player().recent_errors();
    assert!(errors.iter().any(|e| e.starts_with("[ao")), "{errors:?}");
    // 裝置清單變了（例如預設裝置的電視打開了）：也重開一次
    step_until_app(&mut h, "提示消失", |app| app.osd_text().is_none());
    h.state_mut().fake_audio_devices(fake_devices(&[SPEAKERS, DAC]));
    h.run_steps(2);
    assert_eq!(h.state().audio_output_retries(), 2);
    step_until_app(&mut h, "還是開不起來：再提示", |app| {
        app.osd_text() == Some("無法開啟音訊裝置，暫時沒有聲音")
    });
}

#[test]
fn audio_output_failure_in_exclusive_mode_mentions_it() {
    // 開著獨佔模式時開不起來：提示可能是獨佔模式不被允許
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-ao-fallback-exclusive",
        None,
        None,
        &[
            ("ao", ""),
            ("audio-device", "wasapi/{00000000-0000-0000-0000-00000000dead}"),
        ],
        |s| s.audio.exclusive = true,
    );
    drop_file(&mut h, sample("general/audio_flac.flac"));
    step_until_app(&mut h, "提示沒有聲音、可能是獨佔模式", |app| {
        app.osd_text() == Some("無法開啟音訊裝置（可能不允許獨佔模式），暫時沒有聲音")
    });
}

#[test]
fn loading_a_passthrough_audio_file_clears_the_eq_chain() {
    // 「載入音軌檔…」載入會直通的音軌（AC-3）：跟選單換音軌一樣，選上之前先清空 af，濾鏡不會碰到直通的資料
    let (_dir, _path, mut h) = sound_harness_with(
        "sound-spdif-load-audio",
        Some("common/mp4_h264_aac.mp4"),
        None,
        &[LOOP_FILE],
        spdif_with_rock,
    );
    if !has_af_filters(&h, "loading_a_passthrough_audio_file_clears_the_eq_chain") {
        return;
    }
    wait_af(&mut h, "AAC 有等化器", |af| af.contains(ROCK_B1));
    h.state_mut().load_extra_file(&sample("common/mkv_hevc_ac3.mkv"), false);
    assert_eq!(prop(&h, "af"), "", "選會直通的音軌之前先清空");
    assert_eq!(h.state().osd_text(), Some("載入音軌：mkv_hevc_ac3.mkv"));
    step_until(&mut h, "換成外掛的 AC-3、直通中", |s| {
        s.selected(TrackKind::Audio).is_some_and(|t| t.external) && s.audio_spdif.as_deref() == Some("ac3")
    });
    wait_real(&mut h, 0.5);
    assert_eq!(prop(&h, "af"), "");
    assert_eq!(disabled_filters(&h), Vec::<String>::new());
}

#[test]
fn dropping_a_file_after_the_end_plays_it() {
    // 播完停在最後一格（keep-open 會暫停）之後拖進新檔：新檔要直接播放，不能沿用暫停
    let mut settings = Settings::default();
    settings.auto_next = false;
    let mut h = harness_with(Some(sample("common/mp4_h264_aac.mp4")), settings);
    settle(&mut h, "mp4_h264_aac.mp4");
    let _ = h.state().player().seek_to(1e9, true);
    step_until(&mut h, "播完停在最後一格", |s| s.eof && s.paused);
    drop_file(&mut h, sample("common/mkv_multitrack.mkv"));
    step_until(&mut h, "新檔開始播放", |s| playing(s, "mkv_multitrack.mkv"));
    wait_real(&mut h, 0.5);
    assert!(!h.state().player().state.paused, "新檔不應該是暫停的");
}

// ───────────── 檔案對話框 ─────────────

#[test]
fn file_dialog_results_do_what_the_dialogs_did() {
    // 對話框改在背景開之後，選好的結果另外處理（`on_dialog_result`）：每一種的效果跟以前選完之後一樣
    let dir = TempDir::new("dialog-results");
    let sub = |name: &str| {
        let d = dir.0.join(name);
        std::fs::create_dir_all(&d).unwrap();
        d
    };
    let (play, add, folder) = (sub("play"), sub("add"), sub("folder"));
    let movie = play.join("m.mkv");
    std::fs::copy(sample("common/mkv_multitrack.mkv"), &movie).unwrap();
    for (d, name) in [(&add, "c.mp4"), (&add, "b.mp4"), (&folder, "y.mp4"), (&folder, "x.mp4")] {
        std::fs::copy(sample("common/mp4_h264_aac.mp4"), d.join(name)).unwrap();
    }
    let settings_path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(settings_path.clone());
    settings.auto_next = false;
    let mut h = harness_with(None, settings);
    h.step();
    let names = |app: &VitascopeApp| -> Vec<String> {
        app.playlist().map_or(Vec::new(), |l| {
            l.items()
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        })
    };

    // 開啟影片
    h.state_mut().on_dialog_result(DialogKind::Open, vec![movie.clone()]);
    settle(&mut h, "m.mkv");
    step_until_app(&mut h, "同資料夾的清單", |app| playlist_len(app) == 1);

    // 載入字幕檔、音軌檔：加進目前的影片並選上
    let subs = h.state().player().state.tracks_of(TrackKind::Sub).count();
    h.state_mut()
        .on_dialog_result(DialogKind::LoadSubtitle, vec![sample("common/extsub_srt_utf8.srt")]);
    assert_eq!(h.state().osd_text(), Some("載入字幕：extsub_srt_utf8.srt"));
    step_until(&mut h, "多一條外掛字幕並選上", |s| {
        s.tracks_of(TrackKind::Sub).count() == subs + 1 && s.selected(TrackKind::Sub).is_some_and(|t| t.external)
    });
    h.state_mut()
        .on_dialog_result(DialogKind::LoadAudio, vec![sample("common/mkv_extaudio.mka")]);
    assert_eq!(h.state().osd_text(), Some("載入音軌：mkv_extaudio.mka"));
    step_until(&mut h, "換成外掛的音軌", |s| {
        s.selected(TrackKind::Audio).is_some_and(|t| t.external)
    });

    // 播放清單：加入檔案（依檔名排序）、加入資料夾（在背景掃）
    h.state_mut()
        .on_dialog_result(DialogKind::PlaylistAddFiles, vec![add.join("c.mp4"), add.join("b.mp4")]);
    assert_eq!(h.state().osd_text(), Some("加入播放清單：2 個檔案"));
    assert_eq!(names(h.state()), ["m.mkv", "b.mp4", "c.mp4"]);
    h.state_mut()
        .on_dialog_result(DialogKind::PlaylistAddFolder, vec![folder.clone()]);
    step_until_app(&mut h, "加入資料夾", |app| playlist_len(app) == 5);
    assert_eq!(names(h.state()), ["m.mkv", "b.mp4", "c.mp4", "x.mp4", "y.mp4"]);
    assert!(playing(&h.state().player().state, "m.mkv"), "加進清單不換檔");

    // 儲存清單：沒有副檔名的補上 .m3u8
    h.state_mut()
        .on_dialog_result(DialogKind::PlaylistSave, vec![dir.0.join("清單")]);
    assert_eq!(h.state().osd_text(), Some("已儲存播放清單：清單.m3u8"));
    let saved = std::fs::read_to_string(dir.0.join("清單.m3u8")).unwrap();
    for name in ["m.mkv", "b.mp4", "c.mp4", "x.mp4", "y.mp4"] {
        assert!(saved.contains(name), "{name}：{saved}");
    }

    // 開啟清單：換成檔案裡的清單，從第一個開始播
    let m3u = dir.0.join("另一個.m3u8");
    std::fs::write(&m3u, "#EXTM3U\nadd/c.mp4\nadd/b.mp4\n").unwrap();
    h.state_mut().on_dialog_result(DialogKind::PlaylistOpen, vec![m3u]);
    step_until(&mut h, "播清單的第一個", |s| playing(s, "c.mp4"));
    assert_eq!(names(h.state()), ["c.mp4", "b.mp4"]);

    // 截圖資料夾：改設定並存檔
    let shots = dir.0.join("截圖");
    h.state_mut()
        .on_dialog_result(DialogKind::ScreenshotDir, vec![shots.clone()]);
    assert_eq!(h.state().settings().screenshot_dir.as_deref(), Some(shots.as_path()));
    assert_eq!(
        h.state().osd_text().map(str::to_owned),
        Some(format!("截圖資料夾：{}", shots.display()))
    );
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(saved["screenshot_dir"], shots.to_string_lossy().as_ref());

    // 像素著色器的組合：加入選好的檔案（不能用的不加）
    let (invert, _, hlsl) = write_shaders(&dir);
    let id = h.state_mut().add_shader_preset();
    h.state_mut()
        .on_dialog_result(DialogKind::ShaderFiles(id), vec![invert.clone(), hlsl]);
    assert_eq!(
        h.state().settings().video.shaders.presets[0].files,
        vec![path_str(&invert)]
    );
    assert_eq!(
        h.state().osd_text(),
        Some("無法加入 bad.hlsl：這不是 mpv 格式的 GLSL 著色器（需要 //!HOOK）")
    );
}

type DialogLog = std::sync::Arc<std::sync::Mutex<Vec<(DialogKind, Pick)>>>;

/// each_button_opens_its_file_dialog：「另存新檔」對話框開著多久（測試的替身在這段時間內不回覆）
const SAVE_AS_CHOOSING: Duration = Duration::from_millis(1000);

/// 對話框不真的打開：記下開了哪一種、怎麼選，`answer` 決定選了什麼（None = 取消）。
/// 跟真的對話框一樣在開對話框的執行緒上回覆（Windows、Linux 是背景執行緒；macOS 是介面的執行緒，開著時暫停）
fn record_dialogs(
    h: &mut Harness<'_, VitascopeApp>,
    answer: impl Fn(DialogKind) -> Option<Vec<PathBuf>> + Send + Sync + 'static,
) -> DialogLog {
    let seen = DialogLog::default();
    let log = seen.clone();
    h.state_mut().stub_dialogs_with(move |kind, pick| {
        log.lock().unwrap().push((kind, pick));
        answer(kind)
    });
    seen
}

/// 等對話框關掉、選好的結果處理完，回傳這段時間開過的對話框
fn dialogs_done(h: &mut Harness<'_, VitascopeApp>, seen: &DialogLog) -> Vec<(DialogKind, Pick)> {
    step_until_app(h, "對話框關掉", |app| app.dialog_pending().is_none());
    h.run_steps(2);
    std::mem::take(&mut *seen.lock().unwrap())
}

#[test]
fn each_button_opens_its_file_dialog() {
    // 每個按鈕開哪一種對話框、怎麼選（單選、多選、資料夾、存檔），選好之後照真的路處理（不是直接呼叫 on_dialog_result）
    let (dir, _path, mut h) = video_settings_harness("dialog-buttons", "common/mkv_multitrack.mkv");
    let (invert, keep, _) = write_shaders(&dir);
    let shot = dir.0.join("另存");
    let answers = (invert.clone(), keep.clone(), shot.clone());
    let seen = record_dialogs(&mut h, move |kind| match kind {
        DialogKind::ShaderFiles(_) => Some(vec![answers.0.clone(), answers.1.clone()]),
        DialogKind::ScreenshotSaveAs => {
            // 使用者花一點時間選：開著時暫停，影片不能往前（存的是選「另存新檔」時的畫面）
            std::thread::sleep(SAVE_AS_CHOOSING);
            Some(vec![answers.2.clone()])
        }
        _ => None,
    });
    // 使用者回報的路：設定 → 畫質 → 像素著色器 → 新增組合 → 加入檔案…
    open_settings_page(&mut h, "畫質");
    click_in_view(&mut h, "新增組合");
    let id = h.state().settings().video.shaders.presets[0].id;
    click_in_view(&mut h, "加入檔案…");
    assert_eq!(
        dialogs_done(&mut h, &seen),
        [(DialogKind::ShaderFiles(id), Pick::Files)]
    );
    assert_eq!(
        h.state().settings().video.shaders.presets[0].files,
        vec![path_str(&invert), path_str(&keep)]
    );
    assert!(!h.state().player().state.paused, "選完照樣在播");
    // 設定 → 截圖 → 變更…
    h.get_by_label("截圖").click();
    h.run_steps(2);
    click_in_view(&mut h, "變更…");
    assert_eq!(dialogs_done(&mut h, &seen), [(DialogKind::ScreenshotDir, Pick::Folder)]);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 控制列的「字幕」「音軌」選單
    for (menu, item, kind) in [
        ("字幕", "載入字幕檔…", DialogKind::LoadSubtitle),
        ("音軌", "載入音軌檔…", DialogKind::LoadAudio),
    ] {
        h.get_by_label(menu).click();
        h.run_steps(2);
        h.get_by_label(item).click();
        h.run_steps(2);
        assert_eq!(dialogs_done(&mut h, &seen), [(kind, Pick::File)], "{item}");
    }
    // 播放清單面板的「…」選單
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    for (item, kind, pick) in [
        ("加入檔案…", DialogKind::PlaylistAddFiles, Pick::Files),
        ("加入資料夾…", DialogKind::PlaylistAddFolder, Pick::Folder),
        ("開啟播放清單檔…", DialogKind::PlaylistOpen, Pick::File),
        ("儲存播放清單檔…", DialogKind::PlaylistSave, Pick::Save),
    ] {
        h.get_by_label("…").click();
        h.run_steps(2);
        h.get_by_label(item).click();
        h.run_steps(2);
        assert_eq!(dialogs_done(&mut h, &seen), [(kind, pick)], "{item}");
    }
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    // 開檔
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    h.run_steps(2);
    assert_eq!(dialogs_done(&mut h, &seen), [(DialogKind::Open, Pick::File)]);
    // 右鍵「擷取畫面 ▸ 另存新檔…」：在介面的執行緒上開（開著時暫停），補上 .png，存好之後繼續播
    // 這個子選單在選單的下方（kittest 的視窗小）：捲到看得到、移過去再點開
    hover_context_item(&mut h, "擷取畫面 ⏵");
    h.get_by_label("擷取畫面 ⏵").click();
    h.run_steps(2);
    let before = h.state().player().get_f64("time-pos").unwrap();
    h.get_by_label("另存新檔…").click();
    h.run_steps(2);
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "對話框已經開過、關了（在介面的執行緒上）"
    );
    let moved = h.state().player().get_f64("time-pos").unwrap() - before;
    assert!(
        moved < SAVE_AS_CHOOSING.as_secs_f64() / 2.0,
        "對話框開著時沒有暫停：影片往前了 {moved:.2} 秒"
    );
    assert_eq!(
        dialogs_done(&mut h, &seen),
        [(DialogKind::ScreenshotSaveAs, Pick::Save)]
    );
    wait_for_png(&mut h, &dir.0, 1);
    assert_eq!(pngs_in(&dir.0), [shot.with_extension("png")]);
    assert!(!h.state().player().state.paused, "存好之後繼續播");
}

/// 等開對話框的執行緒送來要求（背景執行緒，不一定已經跑到）
#[cfg(not(target_os = "macos"))]
fn next_dialog(requests: &std::sync::mpsc::Receiver<vitascope::app::DialogRequest>) -> vitascope::app::DialogRequest {
    requests.recv_timeout(Duration::from_secs(5)).expect("沒有開對話框")
}

/// 對話框在背景執行緒開：介面照樣跑（macOS 一定要在介面的執行緒上開，開著時暫停，見 each_button_opens_its_file_dialog）
#[cfg(not(target_os = "macos"))]
#[test]
fn one_file_dialog_at_a_time_while_playback_continues() {
    let mut h = playing_multitrack();
    // 對話框不真的打開：開對話框的執行緒把要求送到這裡、等測試回覆（在介面的執行緒上開的話，介面會卡住、測試失敗）
    let requests = h.state_mut().stub_dialogs();
    let open = |h: &mut Harness<'_, VitascopeApp>| {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        h.run_steps(2);
    };
    open(&mut h);
    let req = next_dialog(&requests);
    assert_eq!((req.kind, req.pick), (DialogKind::Open, Pick::File));
    assert_eq!(h.state().dialog_pending(), Some(DialogKind::Open));
    // 開著時照樣在播（以前介面整個停住，只剩聲音）
    let t0 = h.state().player().state.time_pos;
    wait_real(&mut h, 0.6);
    let st = &h.state().player().state;
    assert!(!st.paused && st.time_pos > t0 + 0.3, "{t0} → {}", st.time_pos);
    // 同時只開一個：再按一次不開第二個，提示已經開著（Linux 的對話框可能躲在主視窗後面）
    open(&mut h);
    assert!(
        requests.recv_timeout(Duration::from_millis(300)).is_err(),
        "已經開著，不能再開"
    );
    assert_eq!(h.state().osd_text(), Some("檔案對話框已經開著"));
    assert_eq!(h.state().dialog_pending(), Some(DialogKind::Open));
    // 選好了：開對話框的執行緒叫醒介面（暫停中介面不會自己重畫），開那個檔案，之後又可以開
    req.reply.send(Some(vec![sample("common/mp4_h264_aac.mp4")])).unwrap();
    let start = Instant::now();
    loop {
        h.step();
        let causes = h.ctx.repaint_causes();
        if causes.iter().any(|c| c.file.ends_with("dialogs.rs")) {
            break;
        }
        assert!(start.elapsed() < TIMEOUT, "選好之後沒有叫醒介面：{causes:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    step_until(&mut h, "開啟選好的檔案", |s| playing(s, "mp4_h264_aac.mp4"));
    assert_eq!(h.state().dialog_pending(), None);
    // 取消：什麼都不做
    open(&mut h);
    next_dialog(&requests).reply.send(None).unwrap();
    step_until_app(&mut h, "取消", |app| app.dialog_pending().is_none());
    assert!(playing(&h.state().player().state, "mp4_h264_aac.mp4"));
    // 開對話框的執行緒出錯結束（channel 斷了）：當成關了
    open(&mut h);
    drop(next_dialog(&requests));
    step_until_app(&mut h, "執行緒結束", |app| app.dialog_pending().is_none());
    open(&mut h);
    let req = next_dialog(&requests);
    assert_eq!(req.kind, DialogKind::Open);
    req.reply.send(None).unwrap();
    step_until_app(&mut h, "取消", |app| app.dialog_pending().is_none());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn subtitle_chosen_for_a_replaced_file_is_not_loaded() {
    // 對話框開著時影片照樣播：可能播完換到清單的下一個，或自己換了檔案。給原來那個影片選的字幕不能加到新的影片上
    let mut h = playing_multitrack();
    let requests = h.state_mut().stub_dialogs();
    let load_subtitle = |h: &mut Harness<'_, VitascopeApp>| {
        h.get_by_label("字幕").click();
        h.run_steps(2);
        h.get_by_label("載入字幕檔…").click();
        h.run_steps(2);
    };
    let srt = sample("common/extsub_srt_utf8.srt");
    load_subtitle(&mut h);
    let req = next_dialog(&requests);
    assert_eq!((req.kind, req.pick), (DialogKind::LoadSubtitle, Pick::File));
    drop_file(&mut h, sample("common/mp4_h264_aac.mp4"));
    settle(&mut h, "mp4_h264_aac.mp4");
    req.reply.send(Some(vec![srt.clone()])).unwrap();
    step_until_app(&mut h, "對話框關掉", |app| app.dialog_pending().is_none());
    assert_eq!(h.state().osd_text(), Some("影片已經換了，沒有載入 extsub_srt_utf8.srt"));
    wait_real(&mut h, 0.3);
    assert_eq!(h.state().player().state.tracks_of(TrackKind::Sub).count(), 0);
    // 同一個影片：照樣載入並選上
    load_subtitle(&mut h);
    next_dialog(&requests).reply.send(Some(vec![srt])).unwrap();
    step_until(&mut h, "載入外掛字幕並選上", |s| {
        s.selected(TrackKind::Sub).is_some_and(|t| t.external)
    });
    assert_eq!(h.state().osd_text(), Some("載入字幕：extsub_srt_utf8.srt"));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn save_as_screenshot_waits_for_the_open_dialog() {
    // 「另存截圖」在介面的執行緒上開：已經開著別的對話框（Linux 的對話框擋不住主視窗）時不開，也不暫停
    let mut h = playing_multitrack();
    let requests = h.state_mut().stub_dialogs();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    h.run_steps(2);
    let req = next_dialog(&requests);
    // 這個子選單在選單的下方（kittest 的視窗小）：捲到看得到、移過去再點開
    hover_context_item(&mut h, "擷取畫面 ⏵");
    h.get_by_label("擷取畫面 ⏵").click();
    h.run_steps(2);
    h.get_by_label("另存新檔…").click();
    h.run_steps(2);
    // 沒有這個檢查的話會在介面的執行緒上再開一個（測試的替身等不到回覆，20 秒後才放開）
    assert!(
        requests.recv_timeout(Duration::from_millis(300)).is_err(),
        "不能再開另存截圖"
    );
    assert_eq!(h.state().osd_text(), Some("檔案對話框已經開著"));
    assert!(!h.state().player().state.paused);
    assert_eq!(h.state().dialog_pending(), Some(DialogKind::Open));
    req.reply.send(None).unwrap();
    step_until_app(&mut h, "取消", |app| app.dialog_pending().is_none());
}

// ───────────── 書籤 ─────────────

/// 目前檔案的書籤時間
fn mark_times(h: &Harness<'_, VitascopeApp>) -> Vec<f64> {
    let app = h.state();
    let key = app.player().state.path.clone().unwrap_or_default();
    app.bookmarks().marks(&key).iter().map(|m| m.time).collect()
}

/// 暫停中精準跳到 `t`，等畫面跟上
fn paused_at(h: &mut Harness<'_, VitascopeApp>, t: f64) {
    h.state().player().seek_to(t, true).unwrap();
    step_until(h, "跳到指定的時間", |s| {
        s.paused && (s.time_pos - t).abs() < 0.05
    });
    h.run_steps(3);
}

/// 開 90 秒的樣本、暫停
fn paused_long() -> Harness<'static, VitascopeApp> {
    let mut h = opened(sample("common/mp4_long.mp4"));
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h
}

#[test]
fn bookmark_key_adds_and_says_why_not() {
    // 沒有開檔：提示先開影片
    let mut h = harness(None);
    h.run_steps(2);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("請先開啟影片"));
    let mut h = paused_long();
    // 還沒有書籤：上一個 / 下一個說明怎麼加（按鍵從對照表來）；不會換檔
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageDown);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("這個檔案還沒有書籤（按 P 新增）"));
    assert!(
        playing(&h.state().player().state, "mp4_long.mp4"),
        "Shift+PgDn 不是下一個檔案"
    );
    paused_at(&mut h, 12.0);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("新增書籤 00:12"));
    assert_eq!(mark_times(&h), [12.0]);
    // 同一個位置（0.5 秒內）不重複加
    paused_at(&mut h, 12.3);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("00:12 已經有書籤"));
    assert_eq!(mark_times(&h), [12.0]);
    assert!(h.state().player().state.paused, "加書籤不影響暫停");
}

/// 書籤的時間是按下 P 那一刻 mpv 的時間（不是介面看到的狀態：跳轉剛送出時還是跳轉前的），
/// 上一個 / 下一個書籤精準跳到那一格
#[test]
fn bookmark_jump_is_exact() {
    let mut h = paused_long();
    for target in [3.2, 7.9] {
        // 跳轉送出後馬上按 P，同一幀處理（不等介面看到新的位置）。兩次跳轉不到 0.3 秒：
        // mpv 可能還沒開始做這一次（等上一次的畫面），書籤照樣在這次的目標
        h.state().player().seek_to(target, true).unwrap();
        h.key_press(egui::Key::P);
        h.step();
        let times = mark_times(&h);
        assert!(
            times.iter().any(|t| (t - target).abs() < 0.05),
            "書籤要在 {target}：{times:?}"
        );
    }
    assert_eq!(mark_times(&h).len(), 2);
    paused_at(&mut h, 0.0);
    for (key, want, osd) in [
        (egui::Key::PageDown, 3.2, "書籤 1/2：00:03"),
        (egui::Key::PageDown, 7.9, "書籤 2/2：00:08"),
        (egui::Key::PageUp, 3.2, "書籤 1/2：00:03"),
    ] {
        h.key_press_modifiers(egui::Modifiers::SHIFT, key);
        step_until(&mut h, osd, |s| (s.time_pos - want).abs() < 0.05);
        assert_eq!(h.state().osd_text(), Some(osd));
        // 跳轉途中的畫面時間可能先到、之後才真的停在那一格：多跑幾幀再確認
        wait_real(&mut h, 0.3);
        let s = &h.state().player().state;
        assert!(
            (s.time_pos - want).abs() < 0.05 && s.paused,
            "停在 {want}：{}",
            s.time_pos
        );
    }
    // 前面、後面沒有了
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageUp);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("前面沒有書籤"));
    paused_at(&mut h, 7.9);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageDown);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("後面沒有書籤"));
    assert!((h.state().player().state.time_pos - 7.9).abs() < 0.05);
}

/// 進度條上這個顏色的小三角形（書籤標記）的中心 x
fn marker_xs(h: &Harness<'_, VitascopeApp>) -> Vec<f32> {
    fn walk(shape: &egui::Shape, color: egui::Color32, out: &mut Vec<f32>) {
        match shape {
            egui::Shape::Path(p) if p.fill == color && p.points.len() == 3 => {
                out.push(p.points.iter().map(|q| q.x).sum::<f32>() / 3.0);
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| walk(s, color, out)),
            _ => {}
        }
    }
    let color = h.state().palette().bookmark;
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, color, &mut out);
    }
    out
}

#[test]
fn bookmark_marker_shows_its_name_and_click_snaps() {
    let mut h = paused_long();
    assert!(marker_xs(&h).is_empty());
    paused_at(&mut h, 30.0);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    paused_at(&mut h, 60.0);
    let bar = h.get_by_label("進度").rect();
    let x = bar.left() + bar.width() * (30.0 / 90.0);
    let xs = marker_xs(&h);
    assert_eq!(xs.len(), 1, "畫一個標記");
    assert!((xs[0] - x).abs() < 1.0, "標記在 30 秒的位置：{} / {x}", xs[0]);
    // 停在標記附近：時間換成書籤
    let near = egui::pos2(x + 3.0, bar.center().y);
    h.event(egui::Event::PointerMoved(near));
    h.run_steps(2);
    assert!(painted(&h, "00:30 · 書籤"), "停在標記上顯示書籤");
    // 點標記旁邊 3 點：精準跳到書籤的時間（不是點的位置）
    h.event(left_button(near, true));
    h.event(left_button(near, false));
    h.step();
    step_until(&mut h, "跳到書籤", |s| (s.time_pos - 30.0).abs() < 0.05);
    wait_real(&mut h, 0.3);
    assert!((h.state().player().state.time_pos - 30.0).abs() < 0.05);
    // 離標記遠一點：照點的位置跳，時間照舊顯示
    let far = egui::pos2(x + 40.0, bar.center().y);
    h.event(egui::Event::PointerMoved(far));
    h.run_steps(2);
    assert!(!painted(&h, "00:30 · 書籤"));
    h.event(left_button(far, true));
    h.event(left_button(far, false));
    h.step();
    let want = 90.0 * f64::from((far.x - bar.left()) / bar.width());
    step_until(&mut h, "照點的位置跳", |s| (s.time_pos - want).abs() < 0.3);
    assert!((h.state().player().state.time_pos - 30.0).abs() > 1.0);
    // 拖曳經過標記：放開時跳到的是滑鼠的位置，時間也照滑鼠的位置顯示（不換成書籤）
    wait_real(&mut h, 0.4);
    let close = egui::pos2(x + 4.0, bar.center().y);
    h.event(left_button(far, true));
    h.step();
    for i in 1..=10 {
        h.event(egui::Event::PointerMoved(far.lerp(close, i as f32 / 10.0)));
        h.step();
    }
    h.run_steps(2);
    assert!(!painted(&h, "00:30 · 書籤"), "拖曳中不換成書籤");
    let pointer = 90.0 * f64::from((close.x - bar.left()) / bar.width());
    assert!(painted(&h, &vitascope::app::fmt_time(pointer)), "顯示滑鼠位置的時間");
    h.event(left_button(close, false));
    h.step();
    step_until(&mut h, "放開：跳到滑鼠的位置", |s| {
        (s.time_pos - pointer).abs() < 0.1
    });
    // 放開後滑鼠還停在標記附近：又顯示書籤
    h.run_steps(2);
    assert!(painted(&h, "00:30 · 書籤"));
}

/// 連按「下一個書籤」：每按一次前進一個（上一次的跳轉 mpv 還沒做時，從它的目標往後找）
#[test]
fn bookmark_steps_add_up_when_pressed_quickly() {
    let mut h = paused_long();
    for t in [10.0, 20.0, 30.0, 40.0] {
        paused_at(&mut h, t);
        h.key_press(egui::Key::P);
        h.run_steps(2);
    }
    assert_eq!(mark_times(&h), [10.0, 20.0, 30.0, 40.0]);
    paused_at(&mut h, 0.0);
    for _ in 0..3 {
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageDown);
        h.step();
    }
    assert_eq!(h.state().osd_text(), Some("書籤 3/4：00:30"));
    step_until(&mut h, "停在第三個書籤", |s| (s.time_pos - 30.0).abs() < 0.05);
    wait_real(&mut h, 0.4);
    let s = &h.state().player().state;
    assert!((s.time_pos - 30.0).abs() < 0.05 && s.paused, "{}", s.time_pos);
    // 往回連按兩次：回到第一個
    for _ in 0..2 {
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageUp);
        h.step();
    }
    assert_eq!(h.state().osd_text(), Some("書籤 1/4：00:10"));
    step_until(&mut h, "回到第一個書籤", |s| (s.time_pos - 10.0).abs() < 0.05);
}

/// 不是精準跳轉（沒有記下目標）時，P 也是問 mpv 按下那一刻的 time-pos，不是介面記下的狀態。
/// 介面的狀態故意改成錯的（暫停中 mpv 不會再送 time-pos，下一幀也不會被蓋掉）：書籤要在 mpv 真正的位置。
/// 不靠「跳轉做完了、介面還沒看到」這種時間差，慢的機器上結果一樣
#[test]
fn bookmark_time_comes_from_mpv_not_the_ui_state() {
    let mut h = paused_long();
    paused_at(&mut h, 10.0);
    // 離上一次精準跳轉超過 0.35 秒：不再用跳轉的目標，改問 mpv
    wait_real(&mut h, 0.5);
    h.state_mut().player_mut().state.time_pos = 123.0;
    h.key_press(egui::Key::P);
    h.step();
    let times = mark_times(&h);
    assert_eq!(times.len(), 1, "{times:?}");
    assert!((times[0] - 10.0).abs() < 0.05, "書籤要在 mpv 的位置：{times:?}");
}

#[test]
fn bookmarks_context_submenu_adds_and_jumps() {
    let mut h = paused_long();
    // 沒有書籤：子選單說還沒有，寫出上一個 / 下一個的按鍵
    hover_context_item(&mut h, "書籤 ⏵");
    h.get_by_label("這個檔案還沒有書籤");
    h.get_by_label("上一個 / 下一個書籤：Shift+PgUp / PgDn");
    let first = h.state().player().state.time_pos;
    h.get_by_label("新增書籤 P").click();
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some(format!("新增書籤 {}", vitascope::app::fmt_time(first)).as_str())
    );
    assert_eq!(mark_times(&h).len(), 1);
    let first = mark_times(&h)[0];
    paused_at(&mut h, 45.0);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    paused_at(&mut h, 70.0);
    // 子選單列出書籤（時間），點了跳過去
    hover_context_item(&mut h, "書籤 ⏵");
    h.get_by_label("00:45").click();
    h.run_steps(2);
    step_until(&mut h, "跳到 45 秒", |s| (s.time_pos - 45.0).abs() < 0.05);
    assert_eq!(h.state().osd_text(), Some("書籤 2/2：00:45"));
    hover_context_item(&mut h, "書籤 ⏵");
    h.get_by_label(&vitascope::app::fmt_time(first)).click();
    h.run_steps(2);
    step_until(&mut h, "跳到第一個", |s| (s.time_pos - first).abs() < 0.05);
    assert!(h.state().player().state.paused);
}

#[test]
fn bookmark_menu_without_a_file_is_disabled() {
    let mut h = harness(None);
    h.run_steps(2);
    // 起始畫面中間是提示文字：在影片畫面的角落按右鍵
    let corner = h.get_by_label("影片畫面").rect().left_top() + egui::vec2(20.0, 20.0);
    h.event(egui::Event::PointerMoved(corner));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: corner,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.run_steps(2);
    hover_menu_item(&mut h, "書籤 ⏵");
    assert!(
        h.get_by_label("新增書籤 P").accesskit_node().is_disabled(),
        "沒有開檔不能加"
    );
    assert!(h.query_by_label("這個檔案還沒有書籤").is_none());
}

#[test]
fn unseekable_sources_cannot_be_bookmarked() {
    // 經過 FFmpeg 的 file 協定、告訴它不能跳轉（像直播、不支援 Range 的伺服器）
    let url = format!("lavf://file:{}", sample("common/mp4_long.mp4").display());
    for (lang, cant_add, cant_jump, video, menu, add) in [
        (
            vitascope::i18n::Lang::ZhTw,
            "這個檔案不能跳轉，無法加書籤",
            "這個檔案不能跳轉",
            "影片畫面",
            "書籤 ⏵",
            "新增書籤 P",
        ),
        (
            vitascope::i18n::Lang::En,
            "This file isn't seekable, so it can't be bookmarked",
            "This file isn't seekable",
            "Video",
            "Bookmarks ⏵",
            "Add bookmark P",
        ),
    ] {
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        let opts = Options {
            keep_open: true,
            extra: vec![("stream-lavf-o".into(), "seekable=0".into())],
            ..Options::headless()
        };
        // 之前（例如能跳轉時）加的書籤：跳不過去
        let mut bookmarks = vitascope::bookmarks::Bookmarks::default();
        bookmarks.add(&url, 50.0).unwrap();
        let launch = Launch {
            files: vec![PathBuf::from(&url)],
            bookmarks,
            ..Default::default()
        };
        let mut h = harness_launch_with(opts, launch, settings);
        step_until(&mut h, "載入完成", |s| s.loaded && s.duration.is_some());
        h.run_steps(3);
        assert!(!h.state().player().state.seekable, "這個來源不能跳轉");
        assert_eq!(mark_times(&h), [50.0]);
        h.key_press(egui::Key::P);
        h.run_steps(2);
        assert_eq!(h.state().osd_text(), Some(cant_add));
        assert_eq!(mark_times(&h), [50.0]);
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageDown);
        h.run_steps(2);
        assert_eq!(h.state().osd_text(), Some(cant_jump));
        h.get_by_label(video).click_secondary();
        h.run_steps(2);
        hover_menu_item(&mut h, menu);
        let node = h.get_by_label(add);
        assert!(node.accesskit_node().is_disabled());
        node.hover();
        h.run_steps(3);
        assert!(h.query_by_label(cant_add).is_some(), "停在上面說明為什麼不能加");
    }
}

#[test]
fn bookmarks_are_saved_to_their_file() {
    let dir = TempDir::new("bookmarks");
    let store = dir.0.join("bookmarks.json");
    let video = dir.clip("影片.mp4");
    let mut settings = Settings::default();
    settings.auto_next = false;
    let launch = Launch {
        files: vec![video.clone()],
        bookmarks: vitascope::bookmarks::Bookmarks::load_from(store.clone()),
        ..Default::default()
    };
    let mut h = harness_launch(launch, settings);
    settle(&mut h, "影片.mp4");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    paused_at(&mut h, 1.5);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    let key = h.state().player().state.path.clone().unwrap();
    assert!(h.state().bookmarks().flush(TIMEOUT), "背景存檔");
    let saved = vitascope::bookmarks::Bookmarks::load_from(store);
    let marks = saved.marks(&key);
    assert_eq!(marks.len(), 1);
    assert!((marks[0].time - 1.5).abs() < 0.05);
    let file = saved.file(&key).unwrap();
    assert_eq!(file.path, key, "存的是完整路徑");
    assert_eq!(file.size, Some(std::fs::metadata(&video).unwrap().len()));
}

#[test]
fn bookmarks_in_english() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.language = vitascope::i18n::Lang::En;
    let mut h = harness_with(None, settings.clone());
    h.run_steps(2);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Open a video first"));
    let mut h = harness_with(Some(sample("common/mp4_long.mp4")), settings);
    settle(&mut h, "mp4_long.mp4");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "pause", |s| s.paused);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageUp);
    h.run_steps(2);
    assert_eq!(
        h.state().osd_text(),
        Some("No bookmarks in this file yet (press P to add one)")
    );
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Bookmarks ⏵");
    h.get_by_label("No bookmarks in this file yet");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    paused_at(&mut h, 20.0);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Bookmark added at 00:20"));
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("There's already a bookmark at 00:20"));
    paused_at(&mut h, 50.0);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageDown);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("No more bookmarks after this point"));
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageUp);
    step_until(&mut h, "back to 20 s", |s| (s.time_pos - 20.0).abs() < 0.05);
    assert_eq!(h.state().osd_text(), Some("Bookmark 1/1: 00:20"));
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::PageUp);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("No bookmarks before this point"));
    paused_at(&mut h, 50.0);
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Bookmarks ⏵");
    h.get_by_label("Add bookmark P");
    h.get_by_label("Previous / next bookmark: Shift+PgUp / PgDn");
    h.get_by_label("00:20");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // 進度條上的標記
    let bar = h.get_by_label("Progress").rect();
    let x = bar.left() + bar.width() * (20.0 / 90.0);
    h.event(egui::Event::PointerMoved(egui::pos2(x, bar.center().y)));
    h.run_steps(2);
    assert!(painted(&h, "00:20 · Bookmark"));
}

/// 存檔失敗：使用者加書籤時提示一次，記憶體裡照樣保留；之後開檔時的讀回（順便重試）失敗不再提示
#[test]
fn bookmark_save_failure_is_shown_for_the_users_own_change_only() {
    for (lang, failed) in [
        (vitascope::i18n::Lang::ZhTw, "無法儲存書籤："),
        (vitascope::i18n::Lang::En, "Couldn't save bookmarks: "),
    ] {
        let dir = TempDir::new(&format!("bookmark-save-fails-{}", failed.len()));
        // 存檔的資料夾其實是一個檔案：寫不進去
        let blocked = dir.0.join("blocked");
        std::fs::write(&blocked, "不是資料夾").unwrap();
        let a = dir.clip("a.mp4");
        dir.clip("b.mp4");
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        let launch = Launch {
            files: vec![a],
            bookmarks: vitascope::bookmarks::Bookmarks::load_from(blocked.join("bookmarks.json")),
            ..Default::default()
        };
        let mut h = harness_launch(launch, settings);
        settle(&mut h, "a.mp4");
        step_until_app(&mut h, "掃描到兩個檔案", |app| playlist_len(app) == 2);
        h.key_press(egui::Key::P);
        step_until_app(&mut h, "存檔失敗的提示", |app| {
            app.osd_text().is_some_and(|t| t.starts_with(failed))
        });
        assert_eq!(mark_times(&h).len(), 1, "記憶體裡照樣保留");
        step_until_app(&mut h, "提示消失", |app| app.osd_text().is_none());
        // 換到下一個檔案：讀回 b 的書籤時順便重試存檔（還是失敗），使用者這次沒有動書籤，不提示
        h.key_press(egui::Key::PageDown);
        step_until(&mut h, "換到 b", |s| playing(s, "b.mp4"));
        assert!(h.state().bookmarks().flush(TIMEOUT));
        h.run_steps(3);
        assert!(
            !h.state().osd_text().is_some_and(|t| t.starts_with(failed)),
            "{:?}",
            h.state().osd_text()
        );
    }
}

/// 選單只列出目前位置附近的 20 個書籤，標出目前的那一個，多的寫總數
#[test]
fn bookmark_submenu_lists_twenty_around_the_current_mark() {
    // 開檔時會換成絕對路徑（Windows 上斜線也會統一）：書籤的代號跟著用
    let long = vitascope::playlist::absolute(&sample("common/mp4_long.mp4"));
    let key = long.to_string_lossy().into_owned();
    for (lang, video, menu, total) in [
        (vitascope::i18n::Lang::ZhTw, "影片畫面", "書籤 ⏵", "共 25 個書籤"),
        (vitascope::i18n::Lang::En, "Video", "Bookmarks ⏵", "25 bookmarks in all"),
    ] {
        let mut bookmarks = vitascope::bookmarks::Bookmarks::default();
        for i in 1..=25 {
            bookmarks.add(&key, 3.0 * f64::from(i)).unwrap();
        }
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        let launch = Launch {
            files: vec![long.clone()],
            bookmarks,
            ..Default::default()
        };
        let mut h = harness_launch(launch, settings);
        settle(&mut h, "mp4_long.mp4");
        h.key_press(egui::Key::Space);
        step_until(&mut h, "暫停", |s| s.paused);
        assert_eq!(mark_times(&h).len(), 25);
        // 40 秒：目前的書籤是 39 秒那個（第 13 個），列出第 3 到第 22 個（9 秒到 66 秒）
        paused_at(&mut h, 40.0);
        h.get_by_label(video).click_secondary();
        h.run_steps(2);
        hover_menu_item(&mut h, menu);
        h.get_by_label(total);
        for (label, listed) in [("00:06", false), ("00:09", true), ("01:06", true), ("01:09", false)] {
            assert_eq!(h.query_by_label(label).is_some(), listed, "{label}");
        }
        assert_eq!(
            h.get_by_label("00:39").accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True),
            "目前的書籤"
        );
        assert_eq!(
            h.get_by_label("00:42").accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::False)
        );
    }
}

/// 一個檔案的書籤滿了（1000 個）：說滿了，不加
#[test]
fn full_bookmark_list_says_so() {
    let dir = TempDir::new("bookmarks-full");
    let clip = dir.clip("影片.mp4");
    let key = clip.to_string_lossy().into_owned();
    for (lang, full) in [
        (vitascope::i18n::Lang::ZhTw, "這個檔案的書籤已經滿了（1000 個）"),
        (vitascope::i18n::Lang::En, "This file already has 1000 bookmarks"),
    ] {
        let mut bookmarks = vitascope::bookmarks::Bookmarks::default();
        for i in 0..vitascope::bookmarks::MAX_MARKS {
            bookmarks.add(&key, 100.0 + i as f64).unwrap();
        }
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        let launch = Launch {
            files: vec![clip.clone()],
            bookmarks,
            ..Default::default()
        };
        let mut h = harness_launch(launch, settings);
        settle(&mut h, "影片.mp4");
        h.key_press(egui::Key::P);
        h.run_steps(2);
        assert_eq!(h.state().osd_text(), Some(full));
        assert_eq!(mark_times(&h).len(), vitascope::bookmarks::MAX_MARKS);
    }
}

/// 開檔時讀回磁碟上這個檔案的書籤：同時開著的另一個視窗加的也看得到
#[test]
fn bookmarks_from_another_window_show_up_when_the_file_opens() {
    let dir = TempDir::new("bookmarks-other-window");
    let store = dir.0.join("bookmarks.json");
    let video = dir.clip("影片.mp4");
    let ours = vitascope::bookmarks::Bookmarks::load_from(store.clone());
    // 這邊讀完書籤檔之後，另一個視窗加了一個
    let mut other = vitascope::bookmarks::Bookmarks::load_from(store);
    other.add(&video.to_string_lossy(), 2.0).unwrap();
    assert!(other.flush(TIMEOUT));
    assert!(ours.marks(&video.to_string_lossy()).is_empty());
    let mut settings = Settings::default();
    settings.auto_next = false;
    let launch = Launch {
        files: vec![video],
        bookmarks: ours,
        ..Default::default()
    };
    let mut h = harness_launch(launch, settings);
    settle(&mut h, "影片.mp4");
    step_until_app(&mut h, "讀回另一個視窗加的書籤", |app| {
        let key = app.player().state.path.clone().unwrap_or_default();
        app.bookmarks().marks(&key).len() == 1
    });
    assert_eq!(mark_times(&h), [2.0]);
}

// ───────────── 書籤分頁（側邊面板） ─────────────

/// 目前檔案的書籤名稱
fn mark_names(h: &Harness<'_, VitascopeApp>) -> Vec<String> {
    let app = h.state();
    let key = app.player().state.path.clone().unwrap_or_default();
    app.bookmarks().marks(&key).iter().map(|m| m.name.clone()).collect()
}

/// 側邊面板目前的分頁（面板關著是 None）
fn side_tab(h: &Harness<'_, VitascopeApp>) -> Option<SideTab> {
    let s = h.state().settings();
    s.show_playlist.then_some(s.side_tab)
}

/// 視窗的寬度指令（打開、關掉側邊面板時視窗跟著變寬、變窄）
fn inner_width(cmds: &[egui::ViewportCommand]) -> Option<f32> {
    cmds.iter().find_map(|c| match c {
        egui::ViewportCommand::InnerSize(s) => Some(s.x),
        _ => None,
    })
}

/// 在輸入框裡打字：每個字都像真的鍵盤一樣先送按鍵（按下、放開）再送文字。
/// 按鍵是快捷鍵的字（P、O、空白鍵…）不能變成快捷鍵
fn type_like_a_keyboard(h: &mut Harness<'_, VitascopeApp>, text: &str) {
    for c in text.chars() {
        let key = match c {
            ' ' => Some(egui::Key::Space),
            _ => egui::Key::from_name(&c.to_ascii_uppercase().to_string()),
        };
        if let Some(key) = key {
            for pressed in [true, false] {
                h.event(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
        h.event(egui::Event::Text(c.to_string()));
        h.step();
    }
    h.run_steps(2);
}

/// 雙擊書籤分頁的一列（改名）。`double_click` 把 egui 的時間停在雙擊的那一刻：之後交還給 egui 自己算，
/// 不然之後每一次點擊都在雙擊的時間內（又變成改名）
fn double_click_row(h: &mut Harness<'_, VitascopeApp>, label: &str) {
    let pos = h.get_by_label(label).rect().center();
    double_click(h, pos);
    h.input_mut().time = None;
    h.run_steps(2);
}

/// 開 90 秒的樣本、暫停，在 `times` 秒各加一個書籤，按 H 打開書籤分頁
fn bookmarks_tab_with(times: &[f64]) -> Harness<'static, VitascopeApp> {
    let mut h = paused_long();
    for &t in times {
        paused_at(&mut h, t);
        h.key_press(egui::Key::P);
        h.run_steps(2);
    }
    assert_eq!(mark_times(&h).len(), times.len());
    h.key_press(egui::Key::H);
    h.run_steps(3);
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks));
    h
}

#[test]
fn bookmark_key_adds_and_panel_lists() {
    let mut h = paused_long();
    let list_len = playlist_len(h.state());
    paused_at(&mut h, 12.0);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("新增書籤 00:12"));
    // H：打開側邊面板的書籤分頁（一般視窗跟播放清單一樣變寬）
    let before = h.ctx.content_rect().width();
    let cmds = press_and_get_commands(&mut h, egui::Key::H);
    assert_eq!(inner_width(&cmds), Some(before + 280.0), "{cmds:?}");
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks));
    h.get_by_label("書籤（1）");
    // 雙擊改名：打字時 P（新增書籤）、O（色相）、空白鍵（暫停）都只是文字
    double_click_row(&mut h, "00:12");
    assert!(h.ctx.text_edit_focused(), "改名的輸入框拿到焦點");
    type_like_a_keyboard(&mut h, "OP end");
    assert_eq!(mark_times(&h), [12.0], "P 沒有新增書籤");
    assert!(h.state().player().state.paused, "空白鍵沒有切換暫停");
    assert_eq!(h.state().adjust().hue, 0, "O 沒有調色相");
    // Enter 改好（不是全螢幕）
    let cmds = press_and_get_commands(&mut h, egui::Key::Enter);
    assert!(!cmds.contains(&egui::ViewportCommand::Fullscreen(true)), "{cmds:?}");
    h.run_steps(2);
    assert_eq!(mark_names(&h), ["OP end"]);
    assert!(!h.ctx.text_edit_focused());
    // 選取後 Delete 刪掉（播放清單不受影響）
    h.get_by_label("00:12  OP end").click();
    h.run_steps(2);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(mark_times(&h).is_empty());
    assert_eq!(playlist_len(h.state()), list_len, "Delete 刪的是書籤，不是清單裡的項目");
    h.get_by_label("書籤（0）");
    h.get_by_label("按 P 在目前的位置新增書籤");
}

#[test]
fn bookmark_panel_tab_and_f6_switch() {
    let dir = three_episodes("side-tabs");
    let mut h = harness(Some(dir.0.join("第1集.mp4")));
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    let before = h.ctx.content_rect().width();
    // F6：播放清單分頁（標題跟以前一樣）
    let cmds = press_and_get_commands(&mut h, egui::Key::F6);
    assert_eq!(inner_width(&cmds), Some(before + 280.0), "{cmds:?}");
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Playlist));
    h.get_by_label("播放清單（1/3）");
    h.get_by_label("1. 第1集.mp4");
    // H：換到書籤分頁，視窗大小不變
    let cmds = press_and_get_commands(&mut h, egui::Key::H);
    assert_eq!(inner_width(&cmds), None, "換分頁不改視窗大小：{cmds:?}");
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks));
    h.get_by_label("書籤（0）");
    h.get_by_label("播放清單（1/3）");
    assert!(h.query_by_label("1. 第1集.mp4").is_none(), "書籤分頁不列清單");
    // F6 換回播放清單；點分頁的標題也能換
    let cmds = press_and_get_commands(&mut h, egui::Key::F6);
    assert_eq!(inner_width(&cmds), None, "{cmds:?}");
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Playlist));
    h.get_by_label("書籤（0）").click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks));
    // 書籤分頁時，右鍵選單的「播放清單」、控制列的 ☰ 不是選取的樣子
    assert_eq!(
        h.get_by_label("☰").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    hover_context_item(&mut h, "播放清單 F6");
    assert_eq!(
        h.get_by_label("播放清單 F6").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::False)
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("播放清單 F6").is_none(), "Esc 關掉選單");
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks));
    // 已經在書籤分頁：H 關掉面板（視窗變回原來的寬度）
    let cmds = press_and_get_commands(&mut h, egui::Key::H);
    let narrowed = inner_width(&cmds).expect("關閉時視窗變窄");
    assert!((narrowed - before).abs() < 2.0, "{cmds:?}");
    h.run_steps(2);
    assert_eq!(side_tab(&h), None);
    assert!(h.query_by_label("書籤（0）").is_none());
    assert_eq!(h.state().settings().side_tab, SideTab::Bookmarks, "記得上次的分頁");
    // F6：打開到播放清單分頁；× 關閉
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    assert_eq!(side_tab(&h), Some(SideTab::Playlist));
    h.get_by_label("1. 第1集.mp4");
    h.get_by_label("×").click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), None);
}

/// 上次關閉時開著書籤分頁：啟動時還是書籤分頁
#[test]
fn bookmarks_tab_is_remembered() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.show_playlist = true;
    settings.side_tab = SideTab::Bookmarks;
    let mut h = harness_with(Some(sample("common/mp4_long.mp4")), settings);
    settle(&mut h, "mp4_long.mp4");
    h.get_by_label("書籤（0）");
    h.get_by_label("按 P 在目前的位置新增書籤");
    assert!(h.query_by_label_contains("1. ").is_none(), "不是播放清單分頁");
}

/// 改名：Esc 不改（也不離開全螢幕），點別的地方（輸入框沒有焦點了）就改好
#[test]
fn bookmark_rename_esc_cancels_and_blur_commits() {
    let mut h = bookmarks_tab_with(&[12.0, 30.0]);
    set_fullscreen(&mut h, true);
    double_click_row(&mut h, "00:12");
    type_like_a_keyboard(&mut h, "abc");
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(!cmds.contains(&egui::ViewportCommand::Fullscreen(false)), "{cmds:?}");
    h.run_steps(2);
    assert_eq!(mark_names(&h), ["", ""], "Esc 不改");
    assert!(!h.ctx.text_edit_focused());
    h.get_by_label("00:12");
    // 再按一次 Esc 才離開全螢幕
    let cmds = press_and_get_commands(&mut h, egui::Key::Escape);
    assert!(cmds.contains(&egui::ViewportCommand::Fullscreen(false)), "{cmds:?}");
    set_fullscreen(&mut h, false);
    // 改名到一半點分頁的標題：改好
    double_click_row(&mut h, "00:30");
    type_like_a_keyboard(&mut h, "ED");
    h.get_by_label("書籤（2）").click();
    h.run_steps(2);
    assert_eq!(mark_names(&h), ["", "ED"]);
    h.get_by_label("00:30  ED");
    // 改名到一半按控制列的 ☰（換到播放清單分頁，輸入框不見了）：也是改好
    double_click_row(&mut h, "00:12");
    type_like_a_keyboard(&mut h, "OP");
    h.get_by_label("☰").click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Playlist));
    assert_eq!(mark_names(&h), ["OP", "ED"]);
    h.key_press(egui::Key::H);
    h.run_steps(2);
    h.get_by_label("00:12  OP");
    assert!(!h.ctx.text_edit_focused(), "回到書籤分頁時不是改名的樣子");
}

#[test]
fn bookmark_row_context_menu() {
    let mut h = bookmarks_tab_with(&[10.0, 40.0]);
    paused_at(&mut h, 60.0);
    // 跳到這裡
    h.get_by_label("00:40").click_secondary();
    h.run_steps(2);
    h.get_by_label("跳到這裡").click();
    step_until(&mut h, "跳到 40 秒", |s| (s.time_pos - 40.0).abs() < 0.05);
    assert_eq!(h.state().osd_text(), Some("書籤 2/2：00:40"));
    // 複製時間
    h.get_by_label("00:10").click_secondary();
    h.run_steps(2);
    h.get_by_label("複製時間").click();
    h.step();
    let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::CopyText(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(copied.as_deref(), Some("00:10"));
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("已複製時間：00:10"));
    // 改名
    h.get_by_label("00:10").click_secondary();
    h.run_steps(2);
    h.get_by_label("改名").click();
    h.run_steps(3);
    assert!(h.ctx.text_edit_focused());
    type_like_a_keyboard(&mut h, "op");
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(mark_names(&h), ["op", ""]);
    // 刪除
    h.get_by_label("00:10  op").click_secondary();
    h.run_steps(2);
    h.get_by_label("刪除 Delete").click();
    h.run_steps(2);
    assert_eq!(mark_times(&h), [40.0]);
    h.get_by_label("書籤（1）");
}

/// 「全部刪除…」先問：對話框開著時空白鍵不暫停、Esc 只關對話框
#[test]
fn delete_all_bookmarks_asks_first() {
    let mut h = bookmarks_tab_with(&[10.0, 20.0, 30.0]);
    assert!(h.state().player().state.paused);
    h.get_by_label("…").click();
    h.run_steps(2);
    h.get_by_label("全部刪除…").click();
    h.run_steps(2);
    h.get_by_label("刪除這個檔案的 3 個書籤？");
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    wait_real(&mut h, 0.3);
    assert!(h.state().player().state.paused, "對話框開著時空白鍵不暫停");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.query_by_label("刪除這個檔案的 3 個書籤？").is_none(),
        "Esc 關掉對話框"
    );
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks), "面板還開著");
    assert_eq!(mark_times(&h).len(), 3, "取消：書籤都還在");
    // 這次真的刪
    h.get_by_label("…").click();
    h.run_steps(2);
    h.get_by_label("全部刪除…").click();
    h.run_steps(2);
    h.get_by_label("刪除").click();
    h.run_steps(2);
    assert!(mark_times(&h).is_empty());
    assert_eq!(h.state().osd_text(), Some("已刪除 3 個書籤"));
    h.get_by_label("書籤（0）");
    // 沒有書籤時「全部刪除…」不能按
    h.get_by_label("…").click();
    h.run_steps(2);
    assert!(h.get_by_label("全部刪除…").accesskit_node().is_disabled());
}

#[test]
fn bookmarks_tab_empty_states_and_add_button() {
    // 沒有開檔
    let mut h = harness(None);
    h.run_steps(2);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    h.get_by_label("沒有開啟檔案");
    assert!(h.get_by_label("+ 新增書籤（P）").accesskit_node().is_disabled());
    // 有檔案、還沒有書籤：說怎麼加；按下面的按鈕新增
    let mut h = paused_long();
    paused_at(&mut h, 25.0);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    h.get_by_label("按 P 在目前的位置新增書籤");
    h.get_by_label("+ 新增書籤（P）").click();
    h.run_steps(2);
    assert_eq!(mark_times(&h), [25.0]);
    assert_eq!(h.state().osd_text(), Some("新增書籤 00:25"));
    h.get_by_label("00:25");
    // 點一列：精準跳到那個書籤
    paused_at(&mut h, 70.0);
    h.get_by_label("00:25").click();
    step_until(&mut h, "跳到 25 秒", |s| (s.time_pos - 25.0).abs() < 0.05);
    assert!(h.state().player().state.paused, "跳過去照樣暫停");
}

/// 沒有名稱的書籤：在有名稱的章節裡時，淡淡地寫章節名稱
#[test]
fn unnamed_bookmark_shows_its_chapter() {
    let mut h = opened(sample("common/mkv_chapters.mkv"));
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    paused_at(&mut h, 5.0);
    h.key_press(egui::Key::P);
    h.run_steps(2);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    h.get_by_label("00:05");
    assert!(painted(&h, "本篇"), "第二章的名稱");
}

#[test]
fn bookmarks_context_submenu_opens_the_list() {
    let mut h = paused_long();
    hover_context_item(&mut h, "書籤 ⏵");
    h.get_by_label("書籤清單 H").click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Bookmarks));
    hover_context_item(&mut h, "書籤 ⏵");
    let item = h.get_by_label("書籤清單 H");
    assert_eq!(item.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True));
    item.click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), None, "再按一次關掉");
}

/// 側邊面板開著、但是在書籤分頁：拖放進來的影片直接播（看不到清單，不偷偷加到清單最後）
#[test]
fn dropping_onto_the_bookmarks_tab_plays_the_file() {
    let dir = three_episodes("bookmarks-drop");
    let extra = TempDir::new("bookmarks-drop-extra");
    let more = extra.clip("番外1.mp4");
    let mut h = harness(Some(dir.0.join("第1集.mp4")));
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到三個影片", |app| playlist_len(app) == 3);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    // 拖到視窗上（還沒放開）：說的是「放開以播放」
    h.input_mut().hovered_files.push(egui::HoveredFile {
        path: Some(more.clone()),
        ..Default::default()
    });
    h.run_steps(2);
    assert!(painted(&h, "放開以播放"), "看不到清單：放開是播放");
    assert!(!painted(&h, "放開以加入播放清單"));
    h.input_mut().hovered_files.clear();
    h.run_steps(2);
    drop_file(&mut h, more);
    step_until(&mut h, "播放番外1", |s| playing(s, "番外1.mp4"));
}

/// PotPlayer 風格：Backspace 是從頭播放。清單開著、選過一列、滑鼠在畫面上時按 Backspace：
/// 從頭播放，清單不變（macOS 的 Backspace 不會變成「從清單移除」，P-B6）
#[test]
fn backspace_restarts_with_the_potplayer_preset_and_keeps_the_list() {
    let dir = TempDir::new("backspace-potplayer");
    for name in ["第1集.mp4", "第2集.mp4"] {
        std::fs::copy(sample("common/mp4_long.mp4"), dir.0.join(name)).unwrap();
    }
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.keys.preset = vitascope::keymap::KeyPreset::Potplayer;
    let mut h = harness_with(Some(dir.0.join("第1集.mp4")), settings);
    settle(&mut h, "第1集.mp4");
    step_until_app(&mut h, "掃描到兩個影片", |app| playlist_len(app) == 2);
    h.key_press(egui::Key::F6);
    h.run_steps(3);
    // 選取清單的第 2 項（單擊只是選取）
    h.get_by_label("2. 第2集.mp4").click();
    h.run_steps(2);
    // 滑鼠移到畫面上，播到 30 秒
    let video = h.get_by_label("影片畫面").rect().center();
    h.event(egui::Event::PointerMoved(video));
    h.state().player().seek_to(30.0, true).unwrap();
    step_until(&mut h, "播到 30 秒", |s| s.time_pos > 29.0);
    assert!(
        !h.state().side_backspace_removes(Platform::Mac),
        "PotPlayer 風格用到 Backspace：macOS 也不是移除"
    );
    h.key_press(egui::Key::Backspace);
    step_until(&mut h, "Backspace 從頭播放", |s| !s.paused && s.time_pos < 2.0);
    assert_eq!(playlist_names(h.state()), ["第1集.mp4", "第2集.mp4"], "清單不變");
    assert!(playing(&h.state().player().state, "第1集.mp4"));
}

/// macOS 的 Backspace（影戲預設組沒用到它）：只在滑鼠在面板上、或最後點的是面板時才移除選取的項目（P-B6）。
/// 規則在每個平台都檢查（`side_backspace_removes(Platform::Mac)`）；真的按 Backspace 只有 macOS 會移除
#[test]
fn mac_backspace_removes_only_when_the_panel_is_active() {
    let mac = cfg!(target_os = "macos");
    let dir = three_episodes("backspace-gate");
    let mut h = playlist_panel_open(&dir);
    let removes = |h: &Harness<'_, VitascopeApp>| h.state().side_backspace_removes(Platform::Mac);
    assert!(!removes(&h), "還沒點過面板、滑鼠也不在面板上");
    // 點了清單的一列：最後點的是面板
    h.get_by_label("3. 第3集.mp4").click();
    h.run_steps(2);
    assert!(removes(&h));
    assert!(
        !h.state().side_backspace_removes(Platform::Windows),
        "Windows、Linux 用 Delete"
    );
    let video = h.get_by_label("影片畫面").rect().center();
    h.event(egui::Event::PointerMoved(video));
    h.run_steps(2);
    assert!(removes(&h), "滑鼠移開了，但最後點的還是面板");
    // 點了畫面（暫停）：不再是面板
    h.get_by_label("影片畫面").click();
    h.run_steps(2);
    assert!(!removes(&h), "最後點的是畫面、滑鼠也在畫面上");
    h.key_press(egui::Key::Backspace);
    h.run_steps(2);
    assert_eq!(playlist_len(h.state()), 3, "Backspace 沒有移除");
    // 滑鼠移到面板上（沒有點）：可以移除
    let row = h.get_by_label("2. 第2集.mp4").rect().center();
    h.event(egui::Event::PointerMoved(row));
    h.run_steps(2);
    assert!(removes(&h), "滑鼠在面板上");
    h.key_press(egui::Key::Backspace);
    h.run_steps(2);
    if mac {
        assert_eq!(
            playlist_names(h.state()),
            ["第1集.mp4", "第2集.mp4"],
            "macOS：移除選取的第3集"
        );
    } else {
        assert_eq!(playlist_len(h.state()), 3, "其他平台的 Backspace 不移除");
    }
    // 面板關著：不移除
    h.key_press(egui::Key::F6);
    h.run_steps(2);
    assert!(!removes(&h));
}

#[test]
fn bookmarks_tab_in_english() {
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.language = vitascope::i18n::Lang::En;
    let mut h = harness_with(None, settings.clone());
    h.run_steps(2);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    h.get_by_label("Bookmarks (0)");
    h.get_by_label("No file is open");
    h.get_by_label("+ Add bookmark (P)");
    let mut h = harness_with(Some(sample("common/mp4_long.mp4")), settings);
    settle(&mut h, "mp4_long.mp4");
    h.key_press(egui::Key::Space);
    step_until(&mut h, "pause", |s| s.paused);
    h.key_press(egui::Key::H);
    h.run_steps(3);
    h.get_by_label("Press P to bookmark the current position");
    paused_at(&mut h, 20.0);
    h.get_by_label("+ Add bookmark (P)").click();
    h.run_steps(2);
    h.get_by_label("Bookmarks (1)");
    h.get_by_label("00:20").click_secondary();
    h.run_steps(2);
    for item in ["Jump here", "Rename", "Copy time"] {
        h.get_by_label(item);
    }
    h.get_by_label("Delete Delete");
    h.get_by_label("Copy time").click();
    h.run_steps(2);
    assert_eq!(h.state().osd_text(), Some("Time copied: 00:20"));
    // 分頁、「…」停在上面的說明
    h.get_by_label("Bookmarks (1)").hover();
    h.run_steps(3);
    h.get_by_label("Bookmark list (H)");
    h.get_by_label("…").hover();
    h.run_steps(3);
    h.get_by_label("Add or delete bookmarks");
    h.get_by_label("…").click();
    h.run_steps(2);
    h.get_by_label("Add bookmark P");
    h.get_by_label("Delete selected Delete");
    h.get_by_label("Delete all…").click();
    h.run_steps(2);
    h.get_by_label("Delete the bookmark of this file?");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(mark_times(&h), [20.0]);
    // 右鍵選單「Bookmarks ▸」的書籤清單
    h.get_by_label("Video").click_secondary();
    h.run_steps(2);
    hover_menu_item(&mut h, "Bookmarks ⏵");
    assert_eq!(
        h.get_by_label("Bookmark list H").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True)
    );
}

/// Delete 刪掉選取的書籤之後，選取移到下一個（可以連按 Delete）；刪掉最後一個時選前一個
#[test]
fn bookmark_delete_moves_the_selection() {
    let mut h = bookmarks_tab_with(&[10.0, 20.0, 30.0]);
    h.get_by_label("00:20").click();
    h.run_steps(2);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(mark_times(&h), [10.0, 30.0]);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(mark_times(&h), [10.0], "選取移到下一個（30 秒）");
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(mark_times(&h).is_empty(), "刪掉最後一個時選前一個");
    // 沒有選取：Delete 什麼都不做
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    h.get_by_label("書籤（0）");
}

/// 改名到一半點了別的東西（上面的一列、新增、另一個分頁、×）：改名改好，點的那一下也照做
#[test]
fn bookmark_rename_blur_keeps_the_click_that_caused_it() {
    let mut h = bookmarks_tab_with(&[10.0, 20.0, 30.0]);
    // 點上面的一列：改好，也跳到那一列
    double_click_row(&mut h, "00:20");
    type_like_a_keyboard(&mut h, "mid");
    h.get_by_label("00:10").click();
    step_until(&mut h, "跳到 10 秒", |s| (s.time_pos - 10.0).abs() < 0.05);
    h.run_steps(2);
    assert_eq!(mark_names(&h), ["", "mid", ""]);
    assert!(!h.ctx.text_edit_focused());
    // 點下面的「+ 新增書籤」：改好，也加了書籤
    double_click_row(&mut h, "00:30");
    type_like_a_keyboard(&mut h, "end");
    // 雙擊的第一下跳到了 30 秒：改名中換到 45 秒再新增
    paused_at(&mut h, 45.0);
    assert!(h.ctx.text_edit_focused());
    h.get_by_label("+ 新增書籤（P）").click();
    h.run_steps(2);
    assert_eq!(mark_times(&h), [10.0, 20.0, 30.0, 45.0]);
    assert_eq!(mark_names(&h), ["", "mid", "end", ""]);
    // 點播放清單分頁：改好，也換了分頁
    double_click_row(&mut h, "00:10");
    type_like_a_keyboard(&mut h, "op");
    h.get_by_label_contains("播放清單（").click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), Some(SideTab::Playlist));
    assert_eq!(mark_names(&h), ["op", "mid", "end", ""]);
    // 點 ×：改好，也關了面板
    h.key_press(egui::Key::H);
    h.run_steps(3);
    double_click_row(&mut h, "00:45");
    type_like_a_keyboard(&mut h, "x");
    h.get_by_label("×").click();
    h.run_steps(2);
    assert_eq!(side_tab(&h), None);
    assert_eq!(mark_names(&h), ["op", "mid", "end", "x"]);
}

/// 開著書籤分頁播 `file`、暫停；`marks` 是事先放好的書籤（檔案、時間），不用一個一個跳過去按 P
fn bookmarks_tab_launch(file: PathBuf, marks: &[(&PathBuf, &[f64])]) -> Harness<'static, VitascopeApp> {
    let mut bookmarks = vitascope::bookmarks::Bookmarks::default();
    for (path, times) in marks {
        for &t in *times {
            bookmarks.add(&path.to_string_lossy(), t).unwrap();
        }
    }
    let mut settings = Settings::default();
    settings.auto_next = false;
    settings.show_playlist = true;
    settings.side_tab = SideTab::Bookmarks;
    let name = file.file_name().unwrap().to_string_lossy().into_owned();
    let launch = Launch {
        files: vec![file],
        bookmarks,
        ..Default::default()
    };
    let mut h = harness_launch(launch, settings);
    settle(&mut h, &name);
    h.key_press(egui::Key::Space);
    step_until(&mut h, "暫停", |s| s.paused);
    h.run_steps(2);
    h
}

/// 改名到一半把那一列捲到看不見（只畫看得到的列，輸入框也跟著不見）：跟點了別的地方一樣，改好，
/// 不會一直停在沒有焦點的改名樣子
#[test]
fn bookmark_rename_commits_when_scrolled_out_of_view() {
    // 放到暫存資料夾：書籤依播放器看到的路徑記（樣本的路徑混了 / 與 \）
    let dir = TempDir::new("bookmarks-rename-scroll");
    let video = dir.0.join("影片.mp4");
    std::fs::copy(sample("common/mp4_long.mp4"), &video).unwrap();
    let times: Vec<f64> = (1..=80).map(f64::from).collect();
    let mut h = bookmarks_tab_launch(video.clone(), &[(&video, &times)]);
    h.get_by_label("書籤（80）");
    double_click_row(&mut h, "00:01");
    type_like_a_keyboard(&mut h, "first");
    assert!(h.ctx.text_edit_focused());
    // 滑鼠在清單上，往下捲到底
    let over = h.get_by_label("00:03").rect().center();
    let wheel = |h: &mut Harness<'_, VitascopeApp>, dy: f32| {
        h.event(egui::Event::PointerMoved(over));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        h.run_steps(10);
    };
    wheel(&mut h, -5000.0);
    assert!(h.query_by_label("00:01").is_none(), "第一列捲到看不見了");
    h.get_by_label("01:20");
    assert_eq!(mark_names(&h)[0], "first", "捲走了就改好");
    assert!(!h.ctx.text_edit_focused());
    // 捲回來：不是改名的樣子
    wheel(&mut h, 5000.0);
    h.get_by_label("00:01  first");
    assert!(!h.ctx.text_edit_focused());
}

/// 改名到一半換了檔案（拖放進來的影片）：改好的是原來那個檔案的書籤
#[test]
fn bookmark_rename_commits_when_the_file_changes() {
    let dir = TempDir::new("bookmarks-rename-file");
    let (one, two) = (dir.clip("第1集.mp4"), dir.clip("第2集.mp4"));
    let mut h = bookmarks_tab_launch(one.clone(), &[(&one, &[1.0]), (&two, &[2.0])]);
    double_click_row(&mut h, "00:01");
    type_like_a_keyboard(&mut h, "x");
    drop_file(&mut h, two.clone());
    step_until(&mut h, "播放第2集", |s| playing(s, "第2集.mp4"));
    h.run_steps(3);
    let marks = |p: &PathBuf| h.state().bookmarks().marks(&p.to_string_lossy()).to_vec();
    assert_eq!(marks(&one)[0].name, "x", "第1集的書籤改好了");
    assert_eq!(marks(&two)[0].name, "", "第2集的書籤沒有被改");
    assert!(!h.ctx.text_edit_focused());
    h.get_by_label("00:02");
}

/// 「全部刪除…」開著時換了檔案（例如自動接下一個）：刪的是開對話框時那個檔案的書籤
#[test]
fn delete_all_bookmarks_deletes_the_file_it_was_opened_for() {
    let dir = TempDir::new("bookmarks-clear-file");
    let (one, two) = (dir.clip("第1集.mp4"), dir.clip("第2集.mp4"));
    let mut h = bookmarks_tab_launch(one.clone(), &[(&one, &[1.0, 3.0]), (&two, &[2.0])]);
    h.get_by_label("…").click();
    h.run_steps(2);
    h.get_by_label("全部刪除…").click();
    h.run_steps(2);
    h.get_by_label("刪除這個檔案的 2 個書籤？");
    drop_file(&mut h, two.clone());
    step_until(&mut h, "播放第2集", |s| playing(s, "第2集.mp4"));
    h.run_steps(3);
    h.get_by_label("刪除").click();
    h.run_steps(2);
    let count = |p: &PathBuf| h.state().bookmarks().marks(&p.to_string_lossy()).len();
    assert_eq!(count(&one), 0, "刪的是第1集的");
    assert_eq!(count(&two), 1, "第2集的還在");
    assert_eq!(h.state().osd_text(), Some("已刪除 2 個書籤"));
    h.get_by_label("書籤（1）");
}

/// 改名時 Delete、Backspace 是編輯文字，不是刪掉書籤。滑鼠在面板上，不打字時 macOS 的 Backspace 會移除：
/// 擋下來的是「正在打字」
#[test]
fn delete_and_backspace_in_the_rename_box_edit_the_text() {
    let mut h = bookmarks_tab_with(&[12.0, 30.0]);
    double_click_row(&mut h, "00:12");
    type_like_a_keyboard(&mut h, "abcd");
    assert!(h.state().side_backspace_removes(Platform::Mac));
    // Backspace 刪掉 d；Home 到最前面；Delete 刪掉 a
    for key in [egui::Key::Backspace, egui::Key::Home, egui::Key::Delete] {
        h.key_press(key);
        h.run_steps(2);
    }
    assert_eq!(mark_times(&h), [12.0, 30.0], "書籤都還在");
    assert!(h.ctx.text_edit_focused(), "還在改名");
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(mark_names(&h), ["bc", ""]);
    assert!(h.state().player().state.paused, "Home 沒有從頭播放");
}

/// 不能跳轉的來源：書籤分頁說為什麼不能加，「+ 新增書籤」不能按
#[test]
fn unseekable_source_in_the_bookmarks_tab() {
    let url = format!("lavf://file:{}", sample("common/mp4_long.mp4").display());
    for (lang, cant_add, add) in [
        (
            vitascope::i18n::Lang::ZhTw,
            "這個檔案不能跳轉，無法加書籤",
            "+ 新增書籤（P）",
        ),
        (
            vitascope::i18n::Lang::En,
            "This file isn't seekable, so it can't be bookmarked",
            "+ Add bookmark (P)",
        ),
    ] {
        let mut settings = Settings::default();
        settings.auto_next = false;
        settings.language = lang;
        settings.show_playlist = true;
        settings.side_tab = SideTab::Bookmarks;
        let opts = Options {
            keep_open: true,
            extra: vec![("stream-lavf-o".into(), "seekable=0".into())],
            ..Options::headless()
        };
        let launch = Launch {
            files: vec![PathBuf::from(&url)],
            ..Default::default()
        };
        let mut h = harness_launch_with(opts, launch, settings);
        step_until(&mut h, "載入完成", |s| s.loaded && s.duration.is_some());
        h.run_steps(3);
        assert!(!h.state().player().state.seekable, "這個來源不能跳轉");
        h.get_by_label(cant_add);
        assert!(h.get_by_label(add).accesskit_node().is_disabled());
    }
}
