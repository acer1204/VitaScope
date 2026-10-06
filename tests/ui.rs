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
    // 跟真正的播放器一樣播完停在最後一格，只是不出畫面、不出聲音。
    // 播放紀錄只放在記憶體（Launch 的預設），不會碰到使用者真正的紀錄
    let player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    Harness::builder().with_size([960.0, 600.0]).build_eframe(move |cc| {
        VitascopeApp::new(
            cc,
            player,
            settings,
            Launch {
                file,
                ..Default::default()
            },
        )
    })
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

#[test]
fn always_on_top_from_context_menu() {
    let mut h = playing_multitrack();
    h.get_by_label("影片畫面").click_secondary();
    h.run_steps(2);
    h.get_by_label_contains("視窗置頂").click();
    h.step();
    let cmds = viewport_commands(&h);
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)),
        "{cmds:?}"
    );
    assert!(h.state().settings().always_on_top);
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
    h.get_by_label_contains("畫面 ⏵").click();
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
    let view = h.get_by_label_contains("畫面 ⏵");
    assert!(view.accesskit_node().is_disabled(), "純音訊檔沒有畫面可以調整");
}
