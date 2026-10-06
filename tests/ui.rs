//! 介面測試（ROADMAP 4.2）：用 egui_kittest 模擬按鍵與點擊，檢查播放器的反應。
//!
//! 播放器用 headless 模式（不出畫面、不出聲音），所以不需要 GPU，CI 也能跑。
//! 影片畫面本身的渲染另外用 `--shot` 自動截圖驗證。

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
    // 跟真正的播放器一樣播完停在最後一格，只是不出畫面、不出聲音
    let player = Player::new(Options {
        keep_open: true,
        ..Options::headless()
    })
    .unwrap();
    Harness::builder().with_size([960.0, 600.0]).build_eframe(move |cc| {
        VitascopeApp::new(
            cc,
            player,
            Settings::default(),
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
