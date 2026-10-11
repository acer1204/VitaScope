//! 播放順序：隨機播放、重複播放、播完時接下來做什麼、自動接下一個時開不起來的檔案跳過。
//!
//! - headless：mpv 的 `loop-file`（重複單一檔案）真的不會播完、改回 no 後馬上生效、換檔後照樣有效；
//!   `VITASCOPE_MPV_OPTS` 之類自己指定的 loop-file / loop 算在使用者指定的選項裡。
//! - 介面（kittest）：用 3 秒的樣本，每個檔案開始播就跳到快結束的地方（不整段播完，CI 才不會太慢）。
//! - 播完後的動作：倒數、各種取消、執行（關閉影戲；睡眠、關機只用假的 `SystemPower`，絕不呼叫真的系統）。
//!
//! 樣本來自 `python scripts/gen_samples.py`。

mod support;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::fake_power::FakePower;
use support::fake_ytdl::{FakeResolver, site_playlist_json};
use support::http::{Canned, Server};
use vitascope::app::{Launch, VitascopeApp};
use vitascope::player::{Options, Player, PlayerEvent};
use vitascope::playlist::{Playlist, Repeat};
use vitascope::settings::Settings;
use vitascope::system_power::{AfterPlayback, PowerAction, PowerCaps, PowerError};
use vitascope::window_mode::WindowMode;
use vitascope::ytdl::{Resolve, YtdlError};

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
        let dir = std::env::temp_dir().join(format!("vitascope-order-{}-{name}", std::process::id()));
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

    fn clips(&self, names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| self.clip(n)).collect()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ───────────── headless：mpv 的 loop-file ─────────────

fn headless(extra: &[(&str, &str)]) -> Player {
    let mut opts = Options {
        keep_open: true,
        ..Options::headless()
    };
    opts.extra
        .extend(extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    Player::new(opts).expect("建立 mpv 失敗")
}

fn open_and_wait(p: &mut Player, path: &Path) {
    p.open(&path.to_string_lossy()).unwrap();
    p.wait_for(TIMEOUT, |e| *e == PlayerEvent::FileLoaded)
        .unwrap_or_else(|e| panic!("打不開 {}：{e}", path.display()));
    p.wait_state(TIMEOUT, |s| s.duration.is_some()).unwrap();
}

/// 一直處理事件，直到條件成立
fn until(p: &mut Player, what: &str, mut cond: impl FnMut(&Player) -> bool) {
    let start = Instant::now();
    while !cond(p) {
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}（time-pos {:?}，eof {}）",
            p.get_f64("time-pos"),
            p.state.eof
        );
        p.wait(Duration::from_millis(20));
    }
}

#[test]
fn loop_file_inf_never_reaches_eof_and_no_applies_to_the_current_file() {
    let mut p = headless(&[]);
    p.mpv().set_property("loop-file", "inf").unwrap();
    open_and_wait(&mut p, &sample("common/mp4_h264_aac.mp4"));
    // 跳到快結束的地方，等它播到結尾：重複單一檔案時回到開頭，不會停在最後一格
    p.seek_to(2.6, true).unwrap();
    until(&mut p, "跳到快結束的地方", |p| {
        p.get_f64("time-pos").is_ok_and(|t| t > 2.4)
    });
    let mut eof_seen = false;
    until(&mut p, "繞回開頭", |p| {
        eof_seen |= p.state.eof;
        p.get_f64("time-pos").is_ok_and(|t| t < 1.0)
    });
    assert!(!eof_seen && !p.state.eof, "loop-file=inf 不會播到結尾");
    assert!(p.state.loaded);
    // 播放中改回 no：這個檔案馬上就不重複了（mpv 會重設這個檔案剩下的重複次數）
    p.mpv().set_property("loop-file", "no").unwrap();
    p.seek_to(2.6, true).unwrap();
    until(&mut p, "改回 no 後播到結尾停住", |p| p.state.eof);
}

#[test]
fn loop_file_survives_the_next_file() {
    let mut p = headless(&[]);
    p.mpv().set_property("loop-file", "inf").unwrap();
    open_and_wait(&mut p, &sample("common/mp4_h264_aac.mp4"));
    open_and_wait(&mut p, &sample("common/mkv_multitrack.mkv"));
    // 全域選項：換檔後照樣是 inf（不在 reset-on-next-file 裡）
    assert_eq!(p.get_string("loop-file").unwrap(), "inf");
}

#[test]
fn loop_and_loop_file_overrides_are_user_options() {
    assert!(
        !headless(&[]).user_overrides().contains("loop-file"),
        "沒指定時影戲可以設 loop-file"
    );
    // 直接指定，或用別名 loop 指定（即使跟預設值一樣是 no）
    for (name, value) in [("loop-file", "no"), ("loop", "no"), ("loop", "inf")] {
        let p = headless(&[(name, value)]);
        assert!(p.user_overrides().contains("loop-file"), "{name}={value}");
    }
    // include= 的設定檔改的也算（建立後跟預設值不一樣）
    let dir = TempDir::new("loop-include");
    let conf = dir.0.join("loop.conf");
    std::fs::write(&conf, "loop-file=inf\n").unwrap();
    let p = headless(&[("include", &conf.to_string_lossy())]);
    assert_eq!(p.get_string("loop-file").unwrap(), "inf");
    assert!(p.user_overrides().contains("loop-file"));
}

// ───────────── 介面 ─────────────

/// 播放順序測試的設定：自動接下一個、不續播（跳到快結束的地方會被記成續播位置）
fn order_settings(shuffle: bool, repeat: Repeat) -> Settings {
    let mut s = Settings::default();
    s.auto_next = true;
    s.resume = false;
    s.play_order.shuffle = shuffle;
    s.play_order.repeat = repeat;
    s
}

fn harness_opts(opts: Options, launch: Launch, settings: Settings) -> Harness<'static, VitascopeApp> {
    let player = Player::new(opts).unwrap();
    Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch))
}

fn harness(launch: Launch, settings: Settings) -> Harness<'static, VitascopeApp> {
    harness_opts(
        Options {
            keep_open: true,
            ..Options::headless()
        },
        launch,
        settings,
    )
}

fn opening(files: Vec<PathBuf>, settings: Settings) -> Harness<'static, VitascopeApp> {
    harness(
        Launch {
            files,
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
            "等待逾時：{what}\n清單：{:?}\n狀態：{:#?}",
            h.state().playlist(),
            h.state().player().state
        );
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

fn name_of(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

/// 清單上目前那一項的檔名
fn current_name(app: &VitascopeApp) -> Option<String> {
    app.playlist().and_then(Playlist::current).map(name_of)
}

/// mpv 正在播 `name`（載入完、還沒播到結尾）
fn playing(app: &VitascopeApp, name: &str) -> bool {
    let s = &app.player().state;
    s.loaded && !s.eof && s.duration.is_some() && s.path.as_deref().is_some_and(|p| p.ends_with(name))
}

/// 等清單上目前那一項真的開始播，回傳它的檔名
fn wait_current_playing(h: &mut Harness<'_, VitascopeApp>) -> String {
    step_until_app(h, "清單上目前那一項開始播放", |app| {
        current_name(app).is_some_and(|n| playing(app, &n))
    });
    current_name(h.state()).unwrap()
}

/// 跳到快結束的地方（剩 0.3 秒），等跳轉生效（之後 time-pos 才是新的）
fn seek_near_end(h: &mut Harness<'_, VitascopeApp>) {
    let d = h.state().player().state.duration.unwrap();
    h.state().player().seek_to(d - 0.3, true).unwrap();
    step_until_app(h, "跳到快結束的地方", |app| {
        app.player().state.time_pos > d - 1.0
    });
}

/// 按一下鍵，處理完這一幀（換檔是同步的：清單位置馬上就變了）
fn press(h: &mut Harness<'_, VitascopeApp>, key: egui::Key) {
    h.key_press(key);
    h.step();
}

fn loop_file(h: &Harness<'_, VitascopeApp>) -> String {
    h.state().player().get_string("loop-file").unwrap()
}

fn wait_scanned(h: &mut Harness<'_, VitascopeApp>, n: usize) {
    step_until_app(h, "同資料夾掃描完", |app| {
        app.playlist().is_some_and(|l| l.len() == n)
    });
}

#[test]
fn shuffle_plays_every_file_once_starting_with_the_opened_one_then_finishes() {
    let dir = TempDir::new("shuffle-once");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4", "d.mp4"]);
    let mut h = opening(vec![files[0].clone()], order_settings(true, Repeat::Off));
    wait_scanned(&mut h, 4);
    // 同資料夾掃描完換成完整的清單：隨機播放要重新套用，開的檔案排第一個
    let list = h.state().playlist().unwrap();
    assert!(list.is_shuffled(), "掃描完的清單也要隨機：{list:?}");
    let order: Vec<String> = list
        .shuffle_order()
        .unwrap()
        .0
        .iter()
        .map(|&i| name_of(&list.items()[i]))
        .collect();
    assert_eq!(order[0], "a.mp4", "開的檔案排第一個：{order:?}");
    let mut played = Vec::new();
    while played.len() < 4 {
        let now = wait_current_playing(&mut h);
        assert_eq!(h.state().playback_finished_count(), 0, "還沒播完：{played:?}");
        played.push(now.clone());
        seek_near_end(&mut h);
        if played.len() < 4 {
            step_until_app(&mut h, "播完換下一個", |app| {
                current_name(app).is_some_and(|n| n != now)
            });
        }
    }
    assert_eq!(played, order, "照洗好的順序播");
    let mut sorted = played.clone();
    sorted.sort();
    assert_eq!(sorted, ["a.mp4", "b.mp4", "c.mp4", "d.mp4"], "每個檔案播一次");
    step_until_app(&mut h, "最後一個播完：播放結束", |app| {
        app.playback_finished_count() == 1
    });
    wait_real(&mut h, 1.0);
    let app = h.state();
    assert_eq!(app.playback_finished_count(), 1, "只結束一次");
    assert_eq!(current_name(app).as_deref(), Some(played[3].as_str()), "停在最後一個");
    assert!(app.player().state.eof);
}

#[test]
fn pgup_walks_back_through_the_shuffled_history_and_pgdn_replays_it() {
    let dir = TempDir::new("shuffle-history");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4", "d.mp4"]);
    let mut h = opening(vec![files[0].clone()], order_settings(true, Repeat::Off));
    wait_scanned(&mut h, 4);
    let mut played = vec![wait_current_playing(&mut h)];
    for _ in 0..2 {
        press(&mut h, egui::Key::PageDown);
        // 提示是按下的那一幀設的（等檔案載入完可能已經過了顯示的時間）
        let osd = h.state().osd_text().unwrap_or_default().to_owned();
        played.push(wait_current_playing(&mut h));
        assert!(
            osd.contains("隨機") && osd.contains(&format!("{}/4", played.len())),
            "{osd}"
        );
    }
    assert_eq!(played.len(), 3);
    assert_ne!(played[1], played[2]);
    // PgUp 兩次沿著播過的往回走（不是在最後兩個之間來回）
    press(&mut h, egui::Key::PageUp);
    assert_eq!(wait_current_playing(&mut h), played[1]);
    press(&mut h, egui::Key::PageUp);
    assert_eq!(wait_current_playing(&mut h), played[0]);
    press(&mut h, egui::Key::PageUp);
    assert_eq!(h.state().osd_text(), Some("已經是第一個檔案"), "這一輪的開頭");
    assert_eq!(current_name(h.state()).as_deref(), Some(played[0].as_str()));
    // 再往前走一樣的順序
    press(&mut h, egui::Key::PageDown);
    assert_eq!(wait_current_playing(&mut h), played[1]);
    press(&mut h, egui::Key::PageDown);
    assert_eq!(wait_current_playing(&mut h), played[2]);
}

#[test]
fn turning_shuffle_on_and_off_keeps_the_current_file_and_is_saved() {
    let dir = TempDir::new("shuffle-toggle");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4"]);
    let mut h = opening(vec![files[1].clone()], order_settings(false, Repeat::Off));
    wait_scanned(&mut h, 3);
    assert_eq!(wait_current_playing(&mut h), "b.mp4");
    assert!(!h.state().playlist().unwrap().is_shuffled());
    h.state_mut().set_shuffle(true);
    let app = h.state();
    assert!(app.settings().play_order.shuffle, "隨機播放要存起來");
    assert_eq!(app.osd_text(), Some("隨機播放：開啟"));
    let list = app.playlist().unwrap();
    let (order, pos) = list.shuffle_order().unwrap();
    assert_eq!((order[0], pos), (1, 0), "目前的檔案排第一個");
    h.state_mut().set_shuffle(false);
    let app = h.state();
    assert!(!app.settings().play_order.shuffle && !app.playlist().unwrap().is_shuffled());
    assert_eq!(app.osd_text(), Some("隨機播放：關閉"));
    assert_eq!(current_name(app).as_deref(), Some("b.mp4"));
    // 回到清單的順序：下一個是 c
    press(&mut h, egui::Key::PageDown);
    assert_eq!(wait_current_playing(&mut h), "c.mp4");
}

#[test]
fn whole_list_opened_while_shuffled_starts_at_a_random_file() {
    let dir = TempDir::new("shuffle-whole");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4", "d.mp4"]);
    let mut h = harness(Launch::default(), order_settings(true, Repeat::Off));
    h.step();
    let mut firsts = Vec::new();
    // 每次都是 1/4 的機會剛好是 a：連續 8 次都是 a 的機率約十萬分之一.五
    for _ in 0..8 {
        for f in &files {
            h.input_mut()
                .dropped_files
                .push(std::sync::Arc::new(Dropped(f.clone())));
        }
        h.step();
        let first = wait_current_playing(&mut h);
        let list = h.state().playlist().unwrap();
        assert!(list.is_shuffled() && list.len() == 4);
        let (order, pos) = list.shuffle_order().unwrap();
        assert_eq!(pos, 0, "從新的一輪的第一個開始");
        assert_eq!(name_of(&list.items()[order[0]]), first);
        firsts.push(first);
    }
    assert!(
        firsts.iter().any(|f| f != "a.mp4"),
        "整份一起開時第一個也是隨機的：{firsts:?}"
    );
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

#[test]
fn restored_list_while_shuffled_starts_with_the_saved_file() {
    let dir = TempDir::new("shuffle-restored");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4", "d.mp4"]);
    let mut h = harness(
        Launch {
            playlist: Some(Playlist::restored(files.clone(), Some(2))),
            ..Default::default()
        },
        order_settings(true, Repeat::Off),
    );
    h.step();
    let list = h.state().playlist().unwrap();
    assert!(list.is_shuffled());
    assert_eq!(list.next_index(), Some(2), "下一個是上次播的那一個");
    // PgDn：從上次的檔案開始。提示的位置是「這一輪的第幾個」（1），不是清單上的位置（3）
    press(&mut h, egui::Key::PageDown);
    let osd = h.state().osd_text().unwrap_or_default().to_owned();
    assert!(osd.starts_with("下一個（隨機，1/4）"), "{osd}");
    assert_eq!(wait_current_playing(&mut h), "c.mp4");
    assert_eq!(h.state().playlist().unwrap().order_position(), 1);
}

#[test]
fn repeat_one_loops_the_file_and_pgdn_still_moves() {
    let dir = TempDir::new("repeat-one");
    let files = dir.clips(&["a.mp4", "b.mp4"]);
    let mut h = opening(vec![files[0].clone()], order_settings(false, Repeat::One));
    wait_scanned(&mut h, 2);
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    // 啟動時（開檔前）就同步設好了
    assert_eq!(loop_file(&h), "inf");
    seek_near_end(&mut h);
    step_until_app(&mut h, "播到結尾後回到開頭（mpv 自己重播）", |app| {
        playing(app, "a.mp4") && app.player().state.time_pos < 1.0
    });
    assert_eq!(current_name(h.state()).as_deref(), Some("a.mp4"));
    assert_eq!(h.state().playback_finished_count(), 0);
    // 手動的下一個照樣換檔
    press(&mut h, egui::Key::PageDown);
    assert_eq!(wait_current_playing(&mut h), "b.mp4");
    assert_eq!(loop_file(&h), "inf", "換檔後照樣重複");
    // 改成不重複：mpv 的 loop-file 跟著改回 no
    h.state_mut().set_repeat(Repeat::Off);
    assert_eq!(h.state().osd_text(), Some("重複播放：關閉"));
    step_until_app(&mut h, "loop-file 改回 no", |app| {
        app.player().get_string("loop-file").is_ok_and(|v| v == "no")
    });
    assert_eq!(h.state().settings().play_order.repeat, Repeat::Off);
}

#[test]
fn repeat_all_wraps_to_the_first_file_and_manual_steps_wrap_too() {
    let dir = TempDir::new("repeat-all");
    let files = dir.clips(&["a.mp4", "b.mp4"]);
    let mut h = opening(vec![files[0].clone()], order_settings(false, Repeat::All));
    wait_scanned(&mut h, 2);
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    assert_eq!(loop_file(&h), "no", "重複全部不是 mpv 的 loop-file");
    seek_near_end(&mut h);
    step_until_app(&mut h, "播完換 b", |app| {
        current_name(app).as_deref() == Some("b.mp4")
    });
    assert_eq!(wait_current_playing(&mut h), "b.mp4");
    // 最後一個：重複全部時控制列的「下一個」可以按（從頭），不重複時不能按
    let next_disabled = |h: &mut Harness<'_, VitascopeApp>| {
        h.step();
        h.get_by_label("⏭").accesskit_node().is_disabled()
    };
    assert!(!next_disabled(&mut h), "重複全部：最後一個也有下一個");
    h.state_mut().set_repeat(Repeat::Off);
    assert!(next_disabled(&mut h), "不重複：最後一個沒有下一個");
    h.state_mut().set_repeat(Repeat::All);
    assert!(!next_disabled(&mut h));
    seek_near_end(&mut h);
    // 換檔（清單位置）跟提示是同一幀做的
    step_until_app(&mut h, "清單播完從頭", |app| {
        current_name(app).as_deref() == Some("a.mp4")
    });
    let osd = h.state().osd_text().unwrap_or_default().to_owned();
    assert!(osd.starts_with("從頭重播清單（1/2）"), "{osd}");
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    assert_eq!(h.state().playback_finished_count(), 0, "重複全部不會結束");
    // 第一個：重複全部時控制列的「上一個」可以按（最後一個），不重複時不能按
    let prev_disabled = |h: &mut Harness<'_, VitascopeApp>| {
        h.step();
        h.get_by_label("⏮").accesskit_node().is_disabled()
    };
    assert!(!prev_disabled(&mut h), "重複全部：第一個也有上一個");
    h.state_mut().set_repeat(Repeat::Off);
    assert!(prev_disabled(&mut h), "不重複：第一個沒有上一個");
    h.state_mut().set_repeat(Repeat::All);
    assert!(!prev_disabled(&mut h));
    // 手動：第一個的上一個是最後一個，最後一個的下一個是第一個
    press(&mut h, egui::Key::PageUp);
    assert_eq!(wait_current_playing(&mut h), "b.mp4");
    press(&mut h, egui::Key::PageDown);
    let osd = h.state().osd_text().unwrap_or_default().to_owned();
    assert!(osd.starts_with("從頭重播清單（1/2）"), "{osd}");
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
}

#[test]
fn repeat_all_with_a_single_file_restarts_it() {
    let dir = TempDir::new("repeat-all-one");
    let a = dir.clip("a.mp4");
    let mut h = opening(vec![a], order_settings(false, Repeat::All));
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    seek_near_end(&mut h);
    step_until_app(&mut h, "播完從頭再播（不重開檔案）", |app| {
        playing(app, "a.mp4") && app.player().state.time_pos < 1.0 && !app.player().state.paused
    });
    assert_eq!(h.state().playback_finished_count(), 0);
    assert_eq!(loop_file(&h), "no");
}

#[test]
fn choosing_repeat_all_turns_on_auto_next() {
    let mut settings = order_settings(false, Repeat::Off);
    settings.auto_next = false;
    let mut h = harness(Launch::default(), settings);
    h.step();
    h.state_mut().set_repeat(Repeat::All);
    let app = h.state();
    assert!(app.settings().auto_next, "選「重複全部」時一併打開自動播放下一個");
    assert_eq!(app.settings().play_order.repeat, Repeat::All);
    let osd = app.osd_text().unwrap_or_default();
    assert!(
        osd.contains("重複播放：全部") && osd.contains("播完自動播放下一個"),
        "{osd}"
    );
    // 單一檔案：mpv 的 loop-file（非同步設定）
    h.state_mut().set_repeat(Repeat::One);
    assert_eq!(h.state().osd_text(), Some("重複播放：單一檔案"));
    step_until_app(&mut h, "loop-file 設成 inf", |app| {
        app.player().get_string("loop-file").is_ok_and(|v| v == "inf")
    });
}

#[test]
fn a_missing_file_during_auto_next_is_skipped() {
    let dir = TempDir::new("skip-missing");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4"]);
    let mut h = opening(files.clone(), order_settings(false, Repeat::Off));
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    assert_eq!(h.state().playlist().unwrap().len(), 3);
    // b 在播放中被刪掉了（網路磁碟斷線之類）
    std::fs::remove_file(&files[1]).unwrap();
    seek_near_end(&mut h);
    // 自動開 b → 開不起來 → 跳過，開 c（同一幀換清單位置、提示）
    step_until_app(&mut h, "跳過 b、換 c", |app| {
        current_name(app).as_deref() == Some("c.mp4")
    });
    let osd = h.state().osd_text().unwrap_or_default().to_owned();
    assert!(osd.contains("無法開啟") && osd.contains("b.mp4"), "{osd}");
    assert_eq!(wait_current_playing(&mut h), "c.mp4");
    assert_eq!(h.state().playback_finished_count(), 0);
    seek_near_end(&mut h);
    step_until_app(&mut h, "c 播完：播放結束", |app| {
        app.playback_finished_count() == 1
    });
}

#[test]
fn a_missing_last_file_during_auto_next_finishes_playback() {
    let dir = TempDir::new("skip-last");
    let files = dir.clips(&["a.mp4", "b.mp4"]);
    let mut h = opening(files.clone(), order_settings(false, Repeat::Off));
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    std::fs::remove_file(&files[1]).unwrap();
    seek_near_end(&mut h);
    step_until_app(&mut h, "b 開不起來、後面沒有了：播放結束", |app| {
        app.playback_finished_count() == 1
    });
    assert_eq!(current_name(h.state()).as_deref(), Some("b.mp4"));
    wait_real(&mut h, 0.5);
    assert_eq!(h.state().playback_finished_count(), 1);
}

#[test]
fn a_missing_file_opened_by_hand_is_not_skipped() {
    let dir = TempDir::new("skip-manual");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4"]);
    let mut h = opening(files.clone(), order_settings(false, Repeat::Off));
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    std::fs::remove_file(&files[1]).unwrap();
    // 使用者自己按 PgDn：開不起來就停在錯誤畫面，告訴他原因
    press(&mut h, egui::Key::PageDown);
    step_until_app(&mut h, "b 開不起來", |app| app.player().state.last_error.is_some());
    wait_real(&mut h, 1.0);
    let app = h.state();
    assert_eq!(current_name(app).as_deref(), Some("b.mp4"), "不自動跳過");
    assert!(!app.player().state.loaded);
    assert_eq!(app.playback_finished_count(), 0);
}

#[test]
fn user_loop_file_override_is_left_alone_and_the_file_restarts() {
    let dir = TempDir::new("loop-override");
    let files = dir.clips(&["a.mp4", "b.mp4"]);
    let mut opts = Options {
        keep_open: true,
        ..Options::headless()
    };
    opts.extra.push(("loop-file".into(), "no".into()));
    let mut h = harness_opts(
        opts,
        Launch {
            files: vec![files[0].clone()],
            ..Default::default()
        },
        order_settings(false, Repeat::One),
    );
    wait_scanned(&mut h, 2);
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    assert!(h.state().player().user_overrides().contains("loop-file"));
    // 改重複的設定也不動使用者指定的 loop-file
    h.state_mut().set_repeat(Repeat::One);
    wait_real(&mut h, 0.3);
    assert_eq!(loop_file(&h), "no");
    // mpv 不會自己重播：播完時影戲跳回開頭（不換到 b）
    seek_near_end(&mut h);
    step_until_app(&mut h, "播完從頭再播", |app| {
        playing(app, "a.mp4") && app.player().state.time_pos < 1.0
    });
    assert_eq!(current_name(h.state()).as_deref(), Some("a.mp4"));
    assert_eq!(loop_file(&h), "no");
}

/// 網路上的播放清單（IPTV 的 .m3u 網址）：清單本身換成裡面的項目，隨機播放時從新的一輪開始，
/// 整份一起開，第一個也是隨機的（不一定是清單上的第一項）
#[test]
fn remote_m3u_opened_while_shuffled_starts_a_shuffled_cycle() {
    let server = Server::start();
    let list = server.url("/m3u");
    let mut settings = order_settings(true, Repeat::Off);
    settings.auto_next = false;
    let mut h = harness(Launch::default(), settings);
    h.step();
    let mut firsts = Vec::new();
    // 兩項：每次都是 1/2 的機會剛好是第一項，連續 16 次的機率約六萬五千分之一
    for _ in 0..16 {
        h.event(egui::Event::Paste(list.clone()));
        h.step();
        // 先換成只有清單本身的網址，mpv 讀完清單才展開（不會在同一幀）
        step_until_app(&mut h, "開清單的網址", |app| {
            app.playlist().is_some_and(|l| l.len() == 1)
        });
        step_until_app(&mut h, "展開清單", |app| {
            app.playlist().is_some_and(|l| l.len() == 2)
        });
        let l = h.state().playlist().unwrap();
        assert!(l.is_shuffled(), "{l:?}");
        let (order, pos) = l.shuffle_order().unwrap();
        let current = l.current_index().unwrap();
        assert_eq!((pos, order[0]), (0, current), "從新的一輪的第一個開始：{l:?}");
        firsts.push(current);
        let url = l.items()[current].to_string_lossy().into_owned();
        step_until_app(&mut h, "播清單上目前那一項", |app| {
            let s = &app.player().state;
            s.loaded && s.path.as_deref() == Some(url.as_str())
        });
    }
    assert!(firsts.contains(&1), "整份一起開時第一個也是隨機的：{firsts:?}");
}

/// 「重複全部」但之後關掉了「播完自動播放下一個」：不繞回去（手動、播完都一樣），播完就是播放結束
#[test]
fn repeat_all_without_auto_next_does_not_wrap() {
    let dir = TempDir::new("repeat-all-off");
    let files = dir.clips(&["a.mp4", "b.mp4"]);
    let mut settings = order_settings(false, Repeat::All);
    settings.auto_next = false;
    let mut h = opening(files, settings);
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    press(&mut h, egui::Key::PageDown);
    assert_eq!(wait_current_playing(&mut h), "b.mp4");
    h.step();
    assert!(h.get_by_label("⏭").accesskit_node().is_disabled());
    press(&mut h, egui::Key::PageDown);
    assert_eq!(h.state().osd_text(), Some("已經是最後一個檔案"));
    seek_near_end(&mut h);
    step_until_app(&mut h, "播完：播放結束", |app| {
        app.playback_finished_count() == 1
    });
    assert_eq!(current_name(h.state()).as_deref(), Some("b.mp4"));
}

/// 播放清單開著時拖放進來的：加到空的播放器 = 整份一起開（第一個隨機）；播放中再加的打散到這一輪還沒播的部分
#[test]
fn files_added_to_the_open_playlist_while_shuffled_are_spread_over_the_unplayed_part() {
    let dir = TempDir::new("shuffle-add");
    let names: Vec<String> = (0..16).map(|k| format!("{k:02}.mp4")).collect();
    let files: Vec<PathBuf> = names.iter().map(|n| dir.clip(n)).collect();
    let mut settings = order_settings(true, Repeat::Off);
    settings.show_playlist = true;
    let mut h = harness(Launch::default(), settings);
    h.step();
    let drop_all = |h: &mut Harness<'_, VitascopeApp>, files: &[PathBuf]| {
        for f in files {
            h.input_mut()
                .dropped_files
                .push(std::sync::Arc::new(Dropped(f.clone())));
        }
        h.step();
    };
    drop_all(&mut h, &files[..8]);
    let first = wait_current_playing(&mut h);
    let l = h.state().playlist().unwrap();
    let (order, pos) = l.shuffle_order().unwrap();
    assert_eq!(pos, 0);
    assert_eq!(name_of(&l.items()[order[0]]), first, "從洗好的順序的第一個開始");
    drop_all(&mut h, &files[8..]);
    let l = h.state().playlist().unwrap();
    assert_eq!(l.len(), 16);
    assert_eq!(current_name(h.state()).as_deref(), Some(first.as_str()), "正在播的不換");
    let (order, pos) = l.shuffle_order().unwrap();
    assert_eq!(pos, 0);
    // 照加入的順序整串接在最後的機率是 1/(C(15,8)·8!)，約十億分之四
    let tail: Vec<usize> = order[8..].to_vec();
    assert_ne!(tail, (8..16).collect::<Vec<_>>(), "加進來的要打散：{order:?}");
}

// ───────────── 審查後補上的 ─────────────

/// 隨機、重複會記住：馬上寫進設定檔（不是只改在記憶體裡）
#[test]
fn shuffle_and_repeat_are_written_to_the_settings_file() {
    let dir = TempDir::new("order-saved");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    settings.resume = false;
    let mut h = harness(Launch::default(), settings);
    h.step();
    h.state_mut().set_shuffle(true);
    assert!(Settings::load_from(path.clone()).play_order.shuffle, "隨機播放馬上存檔");
    h.state_mut().set_repeat(Repeat::All);
    let saved = Settings::load_from(path.clone());
    assert_eq!(saved.play_order.repeat, Repeat::All, "重複播放馬上存檔");
    assert!(saved.auto_next, "一併打開的「播完自動播放下一個」也存檔");
    h.state_mut().set_repeat(Repeat::One);
    h.state_mut().set_shuffle(false);
    let saved = Settings::load_from(path);
    assert_eq!(
        (saved.play_order.shuffle, saved.play_order.repeat),
        (false, Repeat::One)
    );
}

/// 本機的 .m3u（整份一起開）：開著隨機播放時第一個也是隨機的
#[test]
fn local_m3u_opened_while_shuffled_starts_at_a_random_file() {
    let dir = TempDir::new("shuffle-m3u");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4", "d.mp4"]);
    let m3u = dir.0.join("list.m3u");
    let text: String = files.iter().map(|f| format!("{}\n", f.display())).collect();
    std::fs::write(&m3u, format!("#EXTM3U\n{text}")).unwrap();
    let mut h = harness(Launch::default(), order_settings(true, Repeat::Off));
    h.step();
    let mut firsts = Vec::new();
    // 每次都是 1/4 的機會剛好是 a：連續 8 次都是 a 的機率約十萬分之一.五
    for _ in 0..8 {
        h.input_mut().dropped_files.push(Arc::new(Dropped(m3u.clone())));
        h.step();
        let first = wait_current_playing(&mut h);
        let list = h.state().playlist().unwrap();
        assert!(list.is_shuffled() && list.len() == 4, "{list:?}");
        let (order, pos) = list.shuffle_order().unwrap();
        assert_eq!(pos, 0, "從新的一輪的第一個開始");
        assert_eq!(name_of(&list.items()[order[0]]), first);
        firsts.push(first);
    }
    assert!(
        firsts.iter().any(|f| f != "a.mp4"),
        "整份一起開時第一個也是隨機的：{firsts:?}"
    );
}

/// 播放清單開著、播放器是空的時拖放進來（「加入檔案」到空的播放器）= 整份一起開：
/// 開著隨機播放時第一個也是隨機的
#[test]
fn files_added_to_an_empty_player_while_shuffled_start_at_a_random_one() {
    let dir = TempDir::new("shuffle-add-empty");
    let files = dir.clips(&["a.mp4", "b.mp4", "c.mp4", "d.mp4"]);
    let mut firsts = Vec::new();
    // 每次都要是空的播放器：每次一個新的
    for _ in 0..8 {
        let mut settings = order_settings(true, Repeat::Off);
        settings.show_playlist = true;
        let mut h = harness(Launch::default(), settings);
        h.step();
        for f in &files {
            h.input_mut().dropped_files.push(Arc::new(Dropped(f.clone())));
        }
        h.step();
        let first = wait_current_playing(&mut h);
        let list = h.state().playlist().unwrap();
        assert!(list.is_manual() && list.is_shuffled() && list.len() == 4, "{list:?}");
        let (order, pos) = list.shuffle_order().unwrap();
        assert_eq!(pos, 0);
        assert_eq!(name_of(&list.items()[order[0]]), first);
        firsts.push(first);
    }
    assert!(
        firsts.iter().any(|f| f != "a.mp4"),
        "加到空的播放器時第一個也是隨機的：{firsts:?}"
    );
}

/// 重複全部，清單上每一項都連不上了（伺服器、網路磁碟斷線）：每一項都試過一次（包括繞回去再試第一項）就停，
/// 播放結束一次，不會一直繞
#[test]
fn every_file_unreachable_under_repeat_all_stops_after_trying_each_once() {
    let server = Server::start();
    let route = "/f/common/mp4_h264_aac.mp4";
    let urls: Vec<String> = (0..3).map(|k| server.url(&format!("{route}?n={k}"))).collect();
    let mut h = opening(
        urls.iter().map(PathBuf::from).collect(),
        order_settings(false, Repeat::All),
    );
    assert_eq!(wait_current_playing(&mut h), "mp4_h264_aac.mp4?n=0");
    // 先暫停、跳到快結束的地方、等讀完（之後不用再跟伺服器要資料），伺服器才開始回 404
    h.state().player().set_pause(true).unwrap();
    seek_near_end(&mut h);
    step_until_app(&mut h, "讀到結尾", |app| {
        app.player().get_string("demuxer-cache-idle").is_ok_and(|v| v == "yes")
    });
    server.put(route, Canned::status(404));
    let before = server.requests_to(route).len();
    h.state().player().set_pause(false).unwrap();
    step_until_app(&mut h, "每一項都開不起來：播放結束", |app| {
        app.playback_finished_count() == 1
    });
    let tried: Vec<String> = server.requests_to(route)[before..]
        .iter()
        .map(|r| r.path.clone())
        .collect();
    for k in [1, 2, 0] {
        assert!(
            tried.iter().any(|p| p.ends_with(&format!("?n={k}"))),
            "第 {k} 項要試過（0 是繞回去再試的）：{tried:?}"
        );
    }
    // 停了就不再試
    let after = server.requests_to(route).len();
    wait_real(&mut h, 1.0);
    assert_eq!(h.state().playback_finished_count(), 1, "只結束一次");
    assert_eq!(server.requests_to(route).len(), after, "停了之後不再開檔");
}

/// 自動接下一個開到網路上的播放清單，但清單裡沒有能播的網址：跟開不起來一樣跳過，後面沒有了就是播放結束
#[test]
fn an_empty_remote_playlist_during_auto_next_is_skipped() {
    let server = Server::start();
    server.put(
        "/empty.m3u",
        Canned::ok("#EXTM3U\n#EXTINF:-1,本機檔案\nfile:///nonexistent/vitascope/x.mp4\n"),
    );
    let dir = TempDir::new("skip-empty-m3u");
    let a = dir.clip("a.mp4");
    let list = server.url("/empty.m3u");
    let mut h = opening(vec![a, PathBuf::from(&list)], order_settings(false, Repeat::Off));
    assert_eq!(wait_current_playing(&mut h), "a.mp4");
    assert_eq!(h.state().playlist().unwrap().len(), 2);
    seek_near_end(&mut h);
    step_until_app(&mut h, "空的清單跳過、後面沒有了：播放結束", |app| {
        app.playback_finished_count() == 1
    });
    assert!(!server.requests_to("/empty.m3u").is_empty());
    wait_real(&mut h, 0.5);
    assert_eq!(h.state().playback_finished_count(), 1);
}

/// 網站的播放清單、網址的 v= 指的剛好是第一部（「載入整個播放清單」）：那是使用者選的影片，
/// 隨機播放時也從它開始（不是當成「沒指定」而隨機開一部）
#[test]
fn site_playlist_whose_chosen_video_is_the_first_starts_with_it_while_shuffled() {
    let server = Server::start();
    let base = server.url("");
    let fake = FakeResolver::new(move |req, _| {
        if !req.playlist {
            return Err(YtdlError::Unsupported.into());
        }
        // 第 k 次開的清單（網址的 list=PL{k}）：t{k}e0 … t{k}e3
        let k = req.url.rsplit("list=PL").next().unwrap_or_default().to_owned();
        let items: Vec<(String, String)> = (0..4)
            .map(|j| (format!("{base}/watch?v=t{k}e{j}"), format!("t{k}e{j}")))
            .collect();
        let refs: Vec<(String, &str)> = items.iter().map(|(u, id)| (u.clone(), id.as_str())).collect();
        Ok(support::fake_ytdl::resolved(
            &site_playlist_json(&req.url, &refs),
            Vec::new(),
        ))
    })
    .arc();
    let resolver: Arc<dyn Resolve> = fake.clone();
    let mut settings = order_settings(true, Repeat::Off);
    settings.auto_next = false;
    let mut h = harness_opts(
        Options {
            keep_open: true,
            net_hooks: true,
            net_resolver: Some(resolver),
            net_sites: vec!["127.0.0.1".into()],
            ..Options::headless()
        },
        Launch::default(),
        settings,
    );
    h.step();
    let whole = vitascope::ytdl::plan::Mode {
        yes_playlist: true,
        ..Default::default()
    };
    // 沒修好時每次只有 1/4 的機會剛好從第一部開始：6 次都是的機率約四千分之一
    for k in 0..6 {
        let first = server.url(&format!("/watch?v=t{k}e0"));
        let page = format!("{first}&list=PL{k}");
        h.state_mut().player_mut().open_with_mode(&page, whole.clone()).unwrap();
        step_until_app(&mut h, "變成播放清單", |app| {
            app.playlist()
                .is_some_and(|l| l.items().first().is_some_and(|p| p.to_string_lossy() == first))
        });
        let l = h.state().playlist().unwrap();
        assert!(l.is_shuffled() && l.len() == 4, "{l:?}");
        assert_eq!(l.current_index(), Some(0), "第 {k} 次：從網址 v= 的那一部開始：{l:?}");
        assert_eq!(l.order_position(), 1);
    }
}

/// 重複單一檔案，但檔案不能跳轉（伺服器不支援 Range，快取裡也跳不回開頭）：mpv 的 loop-file 跳不回開頭、
/// 會一直重試，所以這種檔案不用它（loop-file 是 no），播完時由影戲重新開；換到能跳轉的檔案後又用回 loop-file
#[test]
fn repeat_one_on_an_unseekable_stream_reopens_it() {
    let dir = TempDir::new("unseekable-then-local");
    let server = Server::start();
    let url = server.url("/norange/common/mkv_h264_aac_srt.mkv"); // 3 秒
    let mut opts = Options {
        keep_open: true,
        ..Options::headless()
    };
    // 快取也不能跳轉（不然整個檔案都在快取裡時，mpv 自己跳得回開頭）
    opts.extra.push(("demuxer-seekable-cache".into(), "no".into()));
    let mut h = harness_opts(
        opts,
        Launch {
            files: vec![PathBuf::from(&url)],
            ..Default::default()
        },
        order_settings(false, Repeat::One),
    );
    let name = "mkv_h264_aac_srt.mkv";
    step_until_app(&mut h, "播網址", |app| playing(app, name));
    assert!(!h.state().player().state.seekable, "伺服器不支援 Range：不能跳轉");
    step_until_app(&mut h, "這個檔案不用 loop-file", |app| {
        app.player().get_string("loop-file").is_ok_and(|v| v == "no")
    });
    let opens = server.requests_to("/norange/").len();
    step_until_app(&mut h, "播到快結束", |app| app.player().state.time_pos > 2.0);
    step_until_app(&mut h, "播完重新開、從頭播", |app| {
        playing(app, name) && app.player().state.time_pos < 1.0
    });
    assert!(server.requests_to("/norange/").len() > opens, "重新開了");
    assert_eq!(h.state().playback_finished_count(), 0);
    // 換到本機（能跳轉）的檔案：又用回 loop-file
    let a = dir.clip("a.mp4");
    h.input_mut().dropped_files.push(Arc::new(Dropped(a)));
    h.step();
    step_until_app(&mut h, "能跳轉的檔案用回 loop-file", |app| {
        playing(app, "a.mp4") && app.player().get_string("loop-file").is_ok_and(|v| v == "inf")
    });
}

// ───────────── 播完後的動作：倒數、取消、執行 ─────────────

/// 要等它被取消的倒數：很長（慢的 CI 也不會先時間到）
const LONG_COUNTDOWN: Duration = Duration::from_secs(300);
/// 要等它執行的倒數
const SHORT_COUNTDOWN: Duration = Duration::from_secs(2);

/// 一個 3 秒的片段（不自動接下一個）開始播放，設定檔在暫存資料夾；播完後的動作選 `a`。
/// 回傳（暫存資料夾、設定檔、介面）
fn after_case(name: &str, launch: Launch, a: AfterPlayback) -> (TempDir, PathBuf, Harness<'static, VitascopeApp>) {
    let dir = TempDir::new(name);
    let file = dir.clip("a.mp4");
    let path = dir.0.join("settings.json");
    let mut settings = Settings::load_from(path.clone());
    settings.auto_next = false;
    settings.resume = false;
    let mut h = harness(
        Launch {
            files: vec![file],
            ..launch
        },
        settings,
    );
    // 視窗配合影片調整完（之後才點得到控制列的按鈕）
    step_until_app(&mut h, "a.mp4 開始播放、視窗調整完", |app| {
        playing(app, "a.mp4") && app.player().state.video_size.is_some() && !app.window_fit_pending()
    });
    h.run_steps(5);
    arm(&mut h, a);
    (dir, path, h)
}

/// 選播完後的動作（睡眠、關機要等背景問好系統支援哪些）
fn arm(h: &mut Harness<'_, VitascopeApp>, a: AfterPlayback) {
    let start = Instant::now();
    loop {
        h.state_mut().set_after_playback(a);
        if h.state().after_playback() == a {
            return;
        }
        assert!(start.elapsed() < TIMEOUT, "選不了 {a:?}：{:?}", h.state().osd_text());
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 跳到快結束的地方，等播放結束、開始倒數；再跑一幀（倒數畫面畫出來之後，底下的東西才點不到）
fn finish_and_count_down(h: &mut Harness<'_, VitascopeApp>) {
    seek_near_end(h);
    step_until_app(h, "播完：開始倒數", |app| app.countdown_remaining().is_some());
    h.step();
}

/// 這一幀送給視窗的指令
fn viewport_commands(h: &Harness<'_, VitascopeApp>) -> Vec<egui::ViewportCommand> {
    h.output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|v| v.commands.clone())
        .unwrap_or_default()
}

/// 一直更新介面直到條件成立，收集這段期間送給視窗的指令
fn commands_until(
    h: &mut Harness<'_, VitascopeApp>,
    what: &str,
    cond: impl Fn(&VitascopeApp) -> bool,
) -> Vec<egui::ViewportCommand> {
    let start = Instant::now();
    let mut cmds = Vec::new();
    loop {
        h.step();
        cmds.extend(viewport_commands(h));
        if cond(h.state()) {
            return cmds;
        }
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// 實際經過一段時間，收集這段期間送給視窗的指令
fn commands_during(h: &mut Harness<'_, VitascopeApp>, seconds: f64) -> Vec<egui::ViewportCommand> {
    let start = Instant::now();
    let mut cmds = Vec::new();
    while start.elapsed() < Duration::from_secs_f64(seconds) {
        h.step();
        cmds.extend(viewport_commands(h));
        std::thread::sleep(Duration::from_millis(10));
    }
    cmds
}

/// mpv 現在是不是暫停（直接問 mpv：按鍵送出的暫停切換是同步的，不用等屬性通知）
fn mpv_paused(h: &Harness<'_, VitascopeApp>) -> bool {
    h.state().player().get_string("pause").unwrap() == "yes"
}

/// 等 mpv 在結尾停好：keep-open 要等聲音放完才暫停，比 eof-reached 晚（慢的機器上差很多），
/// 在那之前讀到的暫停狀態之後會自己變
fn wait_paused_at_end(h: &mut Harness<'_, VitascopeApp>) {
    let start = Instant::now();
    while !mpv_paused(h) {
        assert!(start.elapsed() < TIMEOUT, "播完後 mpv 沒有停在最後一格");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn left_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// 倒數被使用者的動作取消了：回到不做任何事、提示已取消、沒有關閉
fn assert_cancelled(h: &Harness<'_, VitascopeApp>, how: &str) {
    let app = h.state();
    assert_eq!(app.countdown_remaining(), None, "{how}：倒數取消");
    assert_eq!(app.after_playback(), AfterPlayback::Nothing, "{how}：回到不做任何事");
    assert_eq!(app.osd_text(), Some("已取消：播完後關閉影戲"), "{how}");
    assert!(!app.close_requested(), "{how}");
    assert!(h.query_by_label("播放完畢").is_none(), "{how}：倒數畫面關掉");
}

#[test]
fn close_after_playback_counts_down_then_closes_once_after_saving() {
    let (_dir, path, mut h) = after_case(
        "after-close",
        Launch {
            countdown_override: Some(SHORT_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    assert_eq!(h.state().osd_text(), Some("播完後：關閉影戲"));
    // 改了音量、還沒存檔：關閉之前要先存
    h.state().player().set_volume(37.0).unwrap();
    step_until_app(&mut h, "音量 37", |app| {
        (app.player().state.volume - 37.0).abs() < 0.5
    });
    seek_near_end(&mut h);
    let mut cmds = commands_until(&mut h, "播完：開始倒數", |app| {
        app.countdown_remaining().is_some()
    });
    assert_eq!(h.state().playback_finished_count(), 1);
    h.get_by_label("播放完畢");
    h.get_by_label_contains("秒後關閉影戲");
    h.get_by_label("取消（Esc）");
    h.get_by_label("按任何鍵、移動滑鼠或點一下就取消");
    assert_eq!(h.state().after_playback(), AfterPlayback::Close, "倒數中還是關閉影戲");
    assert!(!h.state().close_requested());
    assert!(
        (Settings::load_from(path.clone()).volume - 37.0).abs() > 0.5,
        "倒數之前沒有存（下面確認是執行前存的）"
    );
    cmds.extend(commands_until(&mut h, "時間到：關閉影戲", |app| {
        app.close_requested()
    }));
    let app = h.state();
    assert_eq!(app.after_playback(), AfterPlayback::Nothing, "只對這一次有效");
    assert_eq!(app.countdown_remaining(), None);
    assert!(
        (Settings::load_from(path).volume - 37.0).abs() < 0.5,
        "關閉之前先存設定（系統關機時可能來不及跑 on_exit）"
    );
    cmds.extend(commands_during(&mut h, 0.5));
    let closes = cmds
        .iter()
        .filter(|c| matches!(c, egui::ViewportCommand::Close))
        .count();
    assert_eq!(closes, 1, "只關一次：{cmds:?}");
    // 自動測試不動視窗（Launch.countdown_raise 預設關著）
    assert!(
        !cmds
            .iter()
            .any(|c| matches!(c, egui::ViewportCommand::WindowLevel(_) | egui::ViewportCommand::Focus)),
        "{cmds:?}"
    );
    assert!(h.query_by_label("播放完畢").is_none());
}

#[test]
fn shutdown_with_a_fake_system_counts_down_then_runs_once() {
    let fake = Arc::new(FakePower::new(PowerCaps::all_yes()));
    let (_dir, _path, mut h) = after_case(
        "after-shutdown",
        Launch {
            system: Some(fake.clone()),
            countdown_override: Some(SHORT_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Shutdown,
    );
    assert_eq!(h.state().osd_text(), Some("播完後：關機"));
    finish_and_count_down(&mut h);
    h.get_by_label_contains("秒後關機");
    assert!(fake.runs().is_empty(), "倒數中還沒執行");
    step_until_app(&mut h, "時間到", |app| app.countdown_remaining().is_none());
    let start = Instant::now();
    while fake.runs().is_empty() {
        assert!(start.elapsed() < TIMEOUT, "背景沒有執行");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    wait_real(&mut h, 1.0);
    let app = h.state();
    assert_eq!(fake.runs(), [PowerAction::Shutdown], "只執行一次");
    assert_eq!(app.after_playback(), AfterPlayback::Nothing, "只對這一次有效");
    assert!(!app.close_requested(), "關機不是關閉影戲");
}

#[test]
fn a_failed_power_action_says_why() {
    let fake = Arc::new(FakePower::new(PowerCaps::all_yes()));
    fake.fail_with(PowerError::NoPermission);
    let (_dir, _path, mut h) = after_case(
        "after-sleep-fails",
        Launch {
            system: Some(fake.clone()),
            countdown_override: Some(SHORT_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Sleep,
    );
    finish_and_count_down(&mut h);
    h.get_by_label_contains("秒後進入睡眠");
    step_until_app(&mut h, "執行失敗的提示", |app| {
        app.osd_text() == Some("無法進入睡眠：沒有權限")
    });
    assert_eq!(fake.runs(), [PowerAction::Sleep]);
    assert_eq!(h.state().after_playback(), AfterPlayback::Nothing);
}

/// 選了關機、系統的詢問（macOS 的「自動化」）還沒回答時改選了別的：之後才回來的「被拒」不影響新選的
#[test]
fn a_late_refusal_for_an_earlier_choice_keeps_the_new_choice() {
    let fake = Arc::new(FakePower::new(PowerCaps::all_yes()));
    fake.hold_prepares();
    let (_dir, _path, mut h) = after_case(
        "after-late-refusal",
        Launch {
            system: Some(fake.clone()),
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Shutdown,
    );
    step_until_app(&mut h, "選了關機：取得權限（等使用者回答）", |_| {
        !fake.prepares().is_empty()
    });
    assert!(h.state().power_prepare_pending());
    h.state_mut().set_after_playback(AfterPlayback::Close);
    h.step();
    fake.release_prepares(Err(PowerError::MacAutomationDenied));
    step_until_app(&mut h, "關機的權限被拒（處理完）", |app| {
        !app.power_prepare_pending()
    });
    h.run_steps(2);
    let app = h.state();
    assert_eq!(app.after_playback(), AfterPlayback::Close, "被拒的是之前選的關機");
    assert!(
        app.osd_text().is_none_or(|t| !t.contains("無法")),
        "{:?}",
        app.osd_text()
    );
    assert_eq!(fake.prepares(), [PowerAction::Shutdown]);
    assert!(fake.runs().is_empty());
}

/// 選了關機、還沒回答系統的詢問就播完了（開始倒數）：之後被拒的話倒數也停掉，不會時間到又去關機
#[test]
fn a_refusal_during_the_countdown_stops_it() {
    let fake = Arc::new(FakePower::new(PowerCaps::all_yes()));
    fake.hold_prepares();
    let (_dir, _path, mut h) = after_case(
        "after-refusal-countdown",
        Launch {
            system: Some(fake.clone()),
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Shutdown,
    );
    step_until_app(&mut h, "選了關機：取得權限（等使用者回答）", |_| {
        !fake.prepares().is_empty()
    });
    finish_and_count_down(&mut h);
    assert!(h.state().power_prepare_pending(), "還沒回答");
    fake.release_prepares(Err(PowerError::MacAutomationDenied));
    step_until_app(&mut h, "關機的權限被拒（處理完）", |app| {
        !app.power_prepare_pending()
    });
    // 處理的那一幀就說明（提示會自己消失：先看）
    let app = h.state();
    assert_eq!(app.countdown_remaining(), None, "倒數停掉");
    assert_eq!(app.after_playback(), AfterPlayback::Nothing);
    assert_eq!(
        app.osd_text(),
        Some("播完後無法關機：macOS 不允許影戲控制 loginwindow：請到「系統設定 → 隱私權與安全性 → 自動化」允許")
    );
    h.run_steps(2);
    assert!(h.query_by_label("播放完畢").is_none(), "倒數畫面關掉");
    assert!(fake.runs().is_empty());
}

#[test]
fn any_key_cancels_the_countdown_and_is_not_also_a_shortcut() {
    let (_dir, _path, mut h) = after_case(
        "after-space",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    finish_and_count_down(&mut h);
    wait_paused_at_end(&mut h);
    let paused = mpv_paused(&h);
    // 空白鍵被倒數吃掉：不會「從頭播放」（播完後按空白鍵是重新播放）
    press(&mut h, egui::Key::Space);
    assert_cancelled(&h, "空白鍵");
    assert_eq!(mpv_paused(&h), paused, "空白鍵只取消倒數，不切換暫停");
    wait_real(&mut h, 0.5);
    let st = &h.state().player().state;
    assert!(st.eof && st.time_pos > 2.0, "還停在結尾：{:?}", st.time_pos);
    assert!(!h.state().close_requested());
}

#[test]
fn typing_in_a_text_field_still_cancels_the_countdown_and_types_nothing() {
    let (_dir, _path, mut h) = after_case(
        "after-typing",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    // 字幕外觀視窗的字型欄位有焦點（打字時按鍵平常不當快捷鍵）
    h.get_by_label("字幕").click();
    h.run_steps(2);
    h.get_by_label("字幕外觀…").click();
    h.run_steps(2);
    h.get_by_role(egui::accesskit::Role::TextInput).focus();
    h.run_steps(2);
    assert!(h.ctx.text_edit_focused());
    let font = h.state().settings().subtitle.font.clone();
    seek_near_end(&mut h);
    step_until_app(&mut h, "播完：開始倒數", |app| {
        app.countdown_remaining().is_some()
    });
    // 倒數畫面出現的下一幀 egui 才把底下輸入框的焦點拿掉：這一幀按的鍵還是在輸入框有焦點時按的
    assert!(h.ctx.text_edit_focused(), "倒數開始時輸入框還有焦點");
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::X,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.input_mut().events.push(egui::Event::Text("x".into()));
    h.step();
    h.step();
    assert_cancelled(&h, "在輸入框打字");
    assert_eq!(h.state().settings().subtitle.font, font, "取消的那一下沒有打進輸入框");
}

#[test]
fn a_click_cancels_the_countdown_without_pausing() {
    let (_dir, _path, mut h) = after_case(
        "after-click",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    // 滑鼠先停在影片上（點的時候沒有移動）
    let pos = h.get_by_label("影片畫面").rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    finish_and_count_down(&mut h);
    wait_paused_at_end(&mut h);
    let paused = mpv_paused(&h);
    h.event(left_button(pos, true));
    h.event(left_button(pos, false));
    h.run_steps(2);
    assert_cancelled(&h, "點一下");
    assert_eq!(mpv_paused(&h), paused, "點一下只取消倒數，不切換暫停");
}

#[test]
fn moving_the_mouse_cancels_the_countdown_but_a_small_jitter_does_not() {
    let (_dir, _path, mut h) = after_case(
        "after-mouse",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    let pos = h.get_by_label("影片畫面").rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    finish_and_count_down(&mut h);
    // 剛開始的一下子不看滑鼠（視窗還原、移到最上層時，滑鼠在視窗裡的位置會變）
    wait_real(&mut h, 0.6);
    h.event(egui::Event::PointerMoved(pos + egui::vec2(3.0, 2.0)));
    h.run_steps(2);
    assert!(h.state().countdown_remaining().is_some(), "手碰到滑鼠的抖動不算");
    h.event(egui::Event::PointerMoved(pos + egui::vec2(40.0, 0.0)));
    h.run_steps(2);
    assert_cancelled(&h, "移動滑鼠");
}

#[test]
fn input_in_another_program_cancels_the_countdown() {
    let fake = Arc::new(FakePower::new(PowerCaps::all_yes()));
    let (_dir, _path, mut h) = after_case(
        "after-system-input",
        Launch {
            system: Some(fake.clone()),
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    finish_and_count_down(&mut h);
    // 倒數之前的輸入（例如剛才在別的程式打字）不算
    wait_real(&mut h, 0.5);
    assert!(h.state().countdown_remaining().is_some());
    // 使用者在別的程式裡打字
    fake.input_now();
    step_until_app(&mut h, "系統的輸入取消倒數", |app| {
        app.countdown_remaining().is_none()
    });
    assert_cancelled(&h, "別的程式裡的輸入");
    assert!(fake.runs().is_empty());
}

#[test]
fn opening_a_file_cancels_the_countdown_quietly() {
    let (dir, _path, mut h) = after_case(
        "after-open",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    finish_and_count_down(&mut h);
    let b = dir.clip("b.mp4");
    h.input_mut().dropped_files.push(Arc::new(Dropped(b)));
    h.step();
    let app = h.state();
    assert_eq!(app.countdown_remaining(), None);
    assert_eq!(
        app.after_playback(),
        AfterPlayback::Nothing,
        "開了別的檔案：回到不做任何事"
    );
    assert_ne!(
        app.osd_text(),
        Some("已取消：播完後關閉影戲"),
        "不提示（新檔案的提示比較有用）"
    );
    step_until_app(&mut h, "播 b.mp4", |app| playing(app, "b.mp4"));
    assert!(!h.state().close_requested());
}

/// 播完時開著檔案對話框（使用者在電腦前面；Windows 的主視窗這時點不到）：不倒數，取消
#[cfg(not(target_os = "macos"))]
#[test]
fn a_file_dialog_open_when_playback_ends_cancels_the_action() {
    let (_dir, _path, mut h) = after_case(
        "after-dialog",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    let requests = h.state_mut().stub_dialogs();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
    h.run_steps(2);
    let request = requests.recv_timeout(TIMEOUT).expect("沒有開對話框");
    seek_near_end(&mut h);
    step_until_app(&mut h, "播放結束", |app| app.playback_finished_count() == 1);
    let app = h.state();
    assert_eq!(app.countdown_remaining(), None, "對話框開著：不倒數");
    assert_eq!(app.after_playback(), AfterPlayback::Nothing);
    assert_eq!(app.osd_text(), Some("已取消：播完後關閉影戲"));
    let _ = request.reply.send(None);
}

#[test]
fn the_countdown_raises_the_window_and_restores_its_level() {
    let (_dir, _path, mut h) = after_case(
        "after-raise",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            countdown_raise: true,
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    // 視窗縮到最小（使用者在用別的程式）
    let minimized = |h: &mut Harness<'_, VitascopeApp>, on: bool| {
        h.input_mut()
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .minimized = Some(on);
    };
    minimized(&mut h, true);
    seek_near_end(&mut h);
    let cmds = commands_until(&mut h, "播完：開始倒數", |app| {
        app.countdown_remaining().is_some()
    });
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)),
        "倒數時置頂：{cmds:?}"
    );
    assert!(
        cmds.contains(&egui::ViewportCommand::Minimized(false)),
        "還原：{cmds:?}"
    );
    assert!(cmds.contains(&egui::ViewportCommand::Focus), "叫到前面：{cmds:?}");
    // 還沒還原好：不再叫；還原好了（macOS 的還原有動畫）：再叫一次到前面
    let cmds = commands_during(&mut h, 0.1);
    assert!(!cmds.contains(&egui::ViewportCommand::Focus), "{cmds:?}");
    minimized(&mut h, false);
    let mut cmds = commands_during(&mut h, 0.1);
    assert_eq!(
        cmds.iter().filter(|c| **c == egui::ViewportCommand::Focus).count(),
        1,
        "還原之後再叫一次：{cmds:?}"
    );
    // 系統不讓搶前景（視窗沒有焦點）時：之後閃工作列 / 跳 Dock
    cmds.extend(commands_during(&mut h, 0.6));
    assert!(
        cmds.iter()
            .any(|c| matches!(c, egui::ViewportCommand::RequestUserAttention(_))),
        "{cmds:?}"
    );
    assert!(h.state().countdown_remaining().is_some(), "叫視窗不會取消倒數");
    // 按下、放開 Esc 在同一幀（kittest 的 key_press 會分成好幾幀送，`output()` 只看得到最後一幀）
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.step();
    let cmds = viewport_commands(&h);
    assert_cancelled(&h, "Esc");
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal)),
        "取消後回到原本的層級（不置頂）：{cmds:?}"
    );
}

/// 迷你播放器裡的倒數：叫到前面、暫時置頂，但不離開迷你播放器（不改大小）；倒數畫面放得下；
/// 取消的 Esc 只取消倒數（不會順便回到一般視窗），層級回到迷你播放器自己的（S-B9）
#[test]
fn the_countdown_in_the_mini_player_keeps_the_mini_player() {
    let (_dir, _path, mut h) = after_case(
        "after-mini",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            countdown_raise: true,
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    // 迷你播放器不置頂（照「不置頂」）：才看得出倒數的置頂
    h.state_mut().settings_mut().compact.mini_on_top = false;
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
    assert_eq!(h.state().window_mode(), WindowMode::Mini);
    assert!(
        viewport_commands(&h).contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal)),
        "{:?}",
        viewport_commands(&h)
    );
    step_until_app(&mut h, "迷你播放器換好", |app| !app.window_mode_pending());
    let mini = h.ctx.content_rect().size();
    seek_near_end(&mut h);
    let cmds = commands_until(&mut h, "播完：開始倒數", |app| {
        app.countdown_remaining().is_some()
    });
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)),
        "倒數時置頂：{cmds:?}"
    );
    assert!(cmds.contains(&egui::ViewportCommand::Focus), "叫到前面：{cmds:?}");
    assert!(
        !cmds.iter().any(|c| matches!(
            c,
            egui::ViewportCommand::InnerSize(_) | egui::ViewportCommand::MinInnerSize(_)
        )),
        "不改大小：{cmds:?}"
    );
    assert_eq!(h.state().window_mode(), WindowMode::Mini);
    h.run_steps(2);
    let panel = h.get_by_label("播放完畢").rect();
    assert!(panel.width() <= mini.x, "倒數畫面放得下：{panel:?} 視窗 {mini:?}");
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.step();
    let cmds = viewport_commands(&h);
    assert_cancelled(&h, "Esc");
    assert!(
        cmds.contains(&egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal)),
        "取消後回到迷你播放器的層級：{cmds:?}"
    );
    h.run_steps(3);
    assert_eq!(h.state().window_mode(), WindowMode::Mini, "Esc 只取消倒數");
}

#[test]
fn after_playback_is_back_to_nothing_after_a_restart() {
    let dir = TempDir::new("after-restart");
    let path = dir.0.join("settings.json");
    let mut h = harness(Launch::default(), Settings::load_from(path.clone()));
    h.step();
    arm(&mut h, AfterPlayback::Close);
    // 存設定（改了隨機播放）：播完後的動作不會一起存
    h.state_mut().set_shuffle(true);
    let json = std::fs::read_to_string(&path).unwrap();
    assert!(json.contains("shuffle") && !json.contains("after"), "{json}");
    drop(h);
    let mut h = harness(Launch::default(), Settings::load_from(path));
    h.step();
    assert!(h.state().settings().play_order.shuffle);
    assert_eq!(
        h.state().after_playback(),
        AfterPlayback::Nothing,
        "每次啟動都是不做任何事"
    );
}

#[test]
fn choosing_an_action_while_repeating_says_playback_never_ends() {
    let mut h = harness(Launch::default(), order_settings(false, Repeat::All));
    h.step();
    arm(&mut h, AfterPlayback::Close);
    assert_eq!(
        h.state().osd_text(),
        Some("播完後：關閉影戲（重複播放開著時，播放不會結束）")
    );
    arm(&mut h, AfterPlayback::Nothing);
    assert_eq!(h.state().osd_text(), Some("播完後：不做任何事"));
}

/// 沒有 `Launch.system`（自動測試、`--shot`）：睡眠、休眠、關機選不了
#[test]
fn power_actions_need_a_system() {
    let mut h = harness(Launch::default(), Settings::default());
    h.step();
    for a in [AfterPlayback::Sleep, AfterPlayback::Hibernate, AfterPlayback::Shutdown] {
        h.state_mut().set_after_playback(a);
        assert_eq!(h.state().after_playback(), AfterPlayback::Nothing, "{a:?}");
    }
    assert_eq!(h.state().osd_text(), Some("播完後無法關機：這台電腦不支援"));
}

/// 已經播完（沒選動作，不倒數）；選「關閉影戲」，然後當成現在才播完：下一幀就是倒數開始的那一幀
/// （倒數畫面還沒畫過：底下的東西還點得到，`modal_open` 也還沒設，只剩倒數自己吃掉輸入）
fn count_down_from_this_frame(h: &mut Harness<'_, VitascopeApp>) {
    seek_near_end(h);
    step_until_app(h, "播放結束（沒選動作）", |app| {
        app.playback_finished_count() == 1
    });
    wait_real(h, 0.3);
    wait_paused_at_end(h);
    assert_eq!(h.state().countdown_remaining(), None);
    h.state_mut().set_after_playback(AfterPlayback::Close);
    h.state_mut().finish_playback_now();
    assert!(h.state().countdown_remaining().is_some(), "開始倒數");
}

#[test]
fn a_key_on_the_frame_the_countdown_starts_is_eaten_too() {
    let (_dir, _path, mut h) = after_case(
        "after-first-frame-key",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Nothing,
    );
    count_down_from_this_frame(&mut h);
    let paused = mpv_paused(&h);
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
    }
    h.step();
    assert_cancelled(&h, "倒數開始那一幀的空白鍵");
    assert_eq!(mpv_paused(&h), paused, "空白鍵只取消倒數，不切換暫停");
    wait_real(&mut h, 0.5);
    let st = &h.state().player().state;
    assert!(st.eof && st.time_pos > 2.0, "還停在結尾：{:?}", st.time_pos);
}

#[test]
fn a_click_on_the_frame_the_countdown_starts_does_not_reach_the_video() {
    let (_dir, _path, mut h) = after_case(
        "after-first-frame-click",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Nothing,
    );
    let pos = h.get_by_label("影片畫面").rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    count_down_from_this_frame(&mut h);
    let paused = mpv_paused(&h);
    let zoom = h.state().player().get_string("video-zoom").unwrap();
    // 按下（和縮放手勢）在倒數開始的那一幀，放開在之後的幀（egui 放開時才算點擊）。
    // `h.event` 一個事件一幀，同一幀的要直接放進輸入
    h.input_mut().events.push(left_button(pos, true));
    h.input_mut().events.push(egui::Event::Zoom(1.5));
    h.step();
    assert_cancelled(&h, "倒數開始那一幀按下");
    h.run_steps(2);
    h.event(left_button(pos, false));
    h.run_steps(3);
    assert_cancelled(&h, "放開");
    assert_eq!(mpv_paused(&h), paused, "取消倒數的那一下不切換暫停");
    assert_eq!(h.state().player().get_string("video-zoom").unwrap(), zoom, "也不縮放");
    wait_real(&mut h, 0.5);
    let st = &h.state().player().state;
    assert!(st.eof && st.time_pos > 2.0, "還停在結尾：{:?}", st.time_pos);
    // 之後的點擊照常
    h.event(left_button(pos, true));
    h.event(left_button(pos, false));
    h.step();
    assert_ne!(mpv_paused(&h), paused, "下一次點擊照常切換暫停");
}

/// 倒數中從快捷鍵以外的路徑（之後的系統媒體鍵）來的指令：停止不提示，其他的提示已取消
#[test]
fn stop_and_other_commands_cancel_the_countdown() {
    let (_dir, _path, mut h) = after_case(
        "after-commands",
        Launch {
            countdown_override: Some(LONG_COUNTDOWN),
            ..Default::default()
        },
        AfterPlayback::Close,
    );
    finish_and_count_down(&mut h);
    let muted = h.state().player().state.muted;
    h.state_mut().run_command(vitascope::keymap::Command::ToggleMute);
    let app = h.state();
    assert_eq!(app.countdown_remaining(), None, "其他指令：取消倒數");
    assert_eq!(app.after_playback(), AfterPlayback::Nothing);
    step_until_app(&mut h, "指令照樣執行（靜音）", |app| {
        app.player().state.muted != muted
    });
    assert!(!h.state().close_requested());
    // 再倒數一次，這次按停止：不提示已取消
    h.state_mut().set_after_playback(AfterPlayback::Close);
    h.state_mut().finish_playback_now();
    h.step();
    assert!(h.state().countdown_remaining().is_some());
    h.state_mut().run_command(vitascope::keymap::Command::Stop);
    h.step();
    let app = h.state();
    assert_eq!(app.countdown_remaining(), None, "停止：取消倒數");
    assert_eq!(app.after_playback(), AfterPlayback::Nothing);
    assert_ne!(app.osd_text(), Some("已取消：播完後關閉影戲"), "停止不提示");
    wait_real(&mut h, 0.3);
    assert!(!h.state().close_requested());
}

/// 倒數畫面在約 320 寬的小視窗（之後的迷你播放器）裡也放得下：每一行字、取消按鈕都在視窗裡（中英文）
#[test]
fn the_countdown_fits_a_window_about_320_wide() {
    for (lang, labels) in [
        (
            vitascope::i18n::Lang::ZhTw,
            ["播放完畢", "取消（Esc）", "按任何鍵、移動滑鼠或點一下就取消"],
        ),
        (
            vitascope::i18n::Lang::En,
            [
                "Playback finished",
                "Cancel (Esc)",
                "Press any key, move the mouse or click to cancel",
            ],
        ),
    ] {
        let dir = TempDir::new("after-narrow");
        let mut settings = Settings::load_from(dir.0.join("settings.json"));
        settings.auto_next = false;
        settings.resume = false;
        settings.language = lang;
        let mut h = harness(
            Launch {
                files: vec![dir.clip("a.mp4")],
                countdown_override: Some(LONG_COUNTDOWN),
                ..Default::default()
            },
            settings,
        );
        // 視窗配合影片調整完之後才改成 320 寬（不然會被調回影片的大小）
        step_until_app(&mut h, "a.mp4 開始播放、視窗調整完", |app| {
            playing(app, "a.mp4") && app.player().state.video_size.is_some() && !app.window_fit_pending()
        });
        h.run_steps(5);
        h.set_size(egui::vec2(320.0, 240.0));
        h.run_steps(3);
        arm(&mut h, AfterPlayback::Close);
        finish_and_count_down(&mut h);
        h.run_steps(2);
        let screen = h.ctx.content_rect();
        assert!(screen.width() <= 321.0, "{screen:?}");
        let line = if lang == vitascope::i18n::Lang::En {
            "Closing VitaScope in"
        } else {
            "秒後關閉影戲"
        };
        // 倒數畫面整塊（egui::Modal 的區域）和裡面的每一行字、按鈕
        let modal = h
            .ctx
            .memory(|m| m.area_rect(egui::Id::new("after_playback_countdown")))
            .expect("倒數畫面");
        let mut rects = vec![modal, h.get_by_label_contains(line).rect()];
        rects.extend(labels.iter().map(|l| h.get_by_label(l).rect()));
        for r in rects {
            assert!(
                screen.expand(0.5).contains_rect(r),
                "{lang:?}：倒數畫面超出 320 寬的視窗：{r:?} / {screen:?}"
            );
        }
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        assert_eq!(h.state().countdown_remaining(), None);
    }
}
