//! 播放影片時螢幕不關閉（E10）。
//!
//! - headless：mpv 的 `stop-screensaver` 讀得到，`VITASCOPE_MPV_OPTS` / `include=` 改的算使用者指定的。
//! - 介面（kittest）：用假的 `Inhibitor`（`Launch.inhibitor`），絕不向系統要求：播放中的影片 → 螢幕不關，
//!   只有聲音 → 電腦不睡眠，暫停、播完、結束 → 放掉；`stop-screensaver=no` 時什麼都不要；
//!   `always` 時暫停也不關，但播完後的倒數、睡眠 / 關機之前一定放掉（用假的 `SystemPower`）。
//! - Linux：真的 ScreenSaver 介面，只在測試自己開的 dbus-daemon 上（絕不是使用者的工作階段匯流排）；
//!   CI 上沒有 dbus-daemon 時失敗（不跳過）。
//!
//! 樣本來自 `python scripts/gen_samples.py`。

mod support;

use eframe::egui;
use egui_kittest::Harness;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::fake_inhibit::FakeInhibitor;
use support::fake_power::FakePower;
use vitascope::app::{Launch, VitascopeApp};
use vitascope::inhibit::{StopScreensaver, Want};
use vitascope::mpv::{Event, Value};
use vitascope::player::{Options, Player, TrackKind};
use vitascope::settings::Settings;
use vitascope::system_power::{AfterPlayback, PowerAction, PowerCaps};

/// 慢的 CI 也要等得到（開檔、跳轉都是非同步的）
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

fn headless(extra: &[(&str, &str)]) -> Options {
    let mut opts = Options {
        keep_open: true,
        ..Options::headless()
    };
    opts.extra
        .extend(extra.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    opts
}

// ───────────── headless：mpv 的 stop-screensaver ─────────────

#[test]
fn stop_screensaver_is_read_from_mpv_and_user_values_are_overrides() {
    let p = Player::new(headless(&[])).unwrap();
    assert_eq!(p.stop_screensaver(), StopScreensaver::Yes, "mpv 的預設");
    assert!(!p.user_overrides().contains("stop-screensaver"));
    for (value, want) in [
        ("no", StopScreensaver::No),
        ("always", StopScreensaver::Always),
        // 跟預設一樣也算指定了
        ("yes", StopScreensaver::Yes),
    ] {
        let p = Player::new(headless(&[("stop-screensaver", value)])).unwrap();
        assert_eq!(p.stop_screensaver(), want, "{value}");
        assert!(p.user_overrides().contains("stop-screensaver"), "{value}");
    }
    // include= 的設定檔改的也算（建立後跟預設值不一樣）
    let dir = std::env::temp_dir().join(format!("vitascope-inhibit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("saver.conf");
    std::fs::write(&conf, "stop-screensaver=no\n").unwrap();
    let p = Player::new(headless(&[("include", &conf.to_string_lossy())])).unwrap();
    assert_eq!(p.stop_screensaver(), StopScreensaver::No);
    assert!(p.user_overrides().contains("stop-screensaver"));
    drop(p);
    let _ = std::fs::remove_dir_all(&dir);
}

// ───────────── 介面 ─────────────

fn settings() -> Settings {
    let mut s = Settings::default();
    s.auto_next = false;
    s.resume = false;
    s
}

fn harness(opts: Options, launch: Launch) -> Harness<'static, VitascopeApp> {
    let player = Player::new(opts).unwrap();
    let settings = settings();
    Harness::builder()
        .with_size([960.0, 600.0])
        .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch))
}

fn step_until(h: &mut Harness<'_, VitascopeApp>, what: &str, mut cond: impl FnMut(&VitascopeApp) -> bool) {
    let start = Instant::now();
    while !cond(h.state()) {
        assert!(
            start.elapsed() < TIMEOUT,
            "等待逾時：{what}\n狀態：{:#?}",
            h.state().player().state
        );
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 載入完、影像（或只有聲音）都知道了、開始播放
fn wait_playing(h: &mut Harness<'_, VitascopeApp>) {
    step_until(h, "開始播放", |app| {
        let s = &app.player().state;
        s.loaded && !s.paused && !s.eof && s.duration.is_some() && !s.tracks.is_empty()
    });
}

fn press(h: &mut Harness<'_, VitascopeApp>, key: egui::Key) {
    h.key_press(key);
    h.step();
}

/// 跳到快結束的地方，等播到結尾
fn play_to_the_end(h: &mut Harness<'_, VitascopeApp>) {
    let d = h.state().player().state.duration.unwrap();
    h.state().player().seek_to(d - 0.3, true).unwrap();
    step_until(h, "播到結尾", |app| app.player().state.eof);
}

/// 等 mpv 在結尾停好：keep-open 要等聲音放完才暫停，比 eof-reached 晚（慢的機器上差很多）。
/// 在那之前從頭播的話，mpv 之後的暫停會蓋過我們的「播放」（tests/play_order.rs 的 `wait_paused_at_end`）
fn wait_paused_at_end(h: &mut Harness<'_, VitascopeApp>) {
    step_until(h, "播完後 mpv 停在最後一格", |app| app.player().paused_now());
}

fn with(fake: &Arc<FakeInhibitor>, file: PathBuf) -> Launch {
    Launch {
        files: vec![file],
        inhibitor: Some(fake.clone()),
        ..Default::default()
    }
}

#[test]
fn a_playing_video_keeps_the_display_on_until_paused_finished_or_closed() {
    let fake = Arc::new(FakeInhibitor::new());
    let mut h = harness(headless(&[]), with(&fake, sample("common/mp4_h264_aac.mp4")));
    wait_playing(&mut h);
    step_until(&mut h, "螢幕不關", |app| app.keep_awake() == Want::Display);
    assert_eq!(fake.current(), Want::Display);
    // 只在變了的時候要求（不是每一幀）
    h.run_steps(5);
    let before = fake.history().len();
    h.run_steps(10);
    assert_eq!(fake.history().len(), before, "沒變就不再要求：{:?}", fake.history());
    // 暫停：放掉；再播：再要
    press(&mut h, egui::Key::Space);
    step_until(&mut h, "暫停時放掉", |_| fake.current() == Want::Nothing);
    press(&mut h, egui::Key::Space);
    step_until(&mut h, "再播時再要", |_| fake.current() == Want::Display);
    // 播完（keep-open 停在最後一格）：放掉
    play_to_the_end(&mut h);
    step_until(&mut h, "播完時放掉", |_| fake.current() == Want::Nothing);
    assert_eq!(h.state().keep_awake(), Want::Nothing);
    // 從頭再播，結束程式：放掉，並等它做完
    wait_paused_at_end(&mut h);
    h.state().player().seek_to(0.0, true).unwrap();
    h.state().player().set_pause(false).unwrap();
    step_until(&mut h, "從頭播：再要", |_| fake.current() == Want::Display);
    let settles = fake.settles();
    eframe::App::on_exit(h.state_mut(), None);
    assert_eq!(fake.current(), Want::Nothing, "結束時放掉");
    assert!(fake.settles() > settles, "結束時等它放掉");
    assert!(
        !fake.history().contains(&Want::System),
        "影片不是只有聲音：{:?}",
        fake.history()
    );
}

#[test]
fn audio_only_keeps_the_system_awake_but_lets_the_display_sleep() {
    let fake = Arc::new(FakeInhibitor::new());
    // 有專輯封面的 mp3：封面不算影片
    let mut h = harness(headless(&[]), with(&fake, sample("general/audio_mp3_cover.mp3")));
    wait_playing(&mut h);
    step_until(&mut h, "電腦不睡眠", |_| fake.current() == Want::System);
    h.run_steps(10);
    assert!(
        !fake.history().contains(&Want::Display),
        "只有聲音時螢幕照常關：{:?}",
        fake.history()
    );
    press(&mut h, egui::Key::Space);
    step_until(&mut h, "暫停時放掉", |_| fake.current() == Want::Nothing);
}

/// 影片關掉影像軌只聽聲音：跟只有聲音一樣（螢幕照常關、電腦不睡眠）；再打開影像：螢幕不關
#[test]
fn turning_the_video_track_off_lets_the_display_sleep() {
    let fake = Arc::new(FakeInhibitor::new());
    let mut h = harness(headless(&[]), with(&fake, sample("common/mp4_h264_aac.mp4")));
    wait_playing(&mut h);
    step_until(&mut h, "影像軌選好了", |app| {
        app.player().state.selected(TrackKind::Video).is_some()
    });
    step_until(&mut h, "螢幕不關", |_| fake.current() == Want::Display);
    let vid = h.state().player().state.selected(TrackKind::Video).unwrap().id;
    h.state().player().select_track(TrackKind::Video, None).unwrap();
    step_until(&mut h, "關掉影像：電腦不睡眠", |_| {
        fake.current() == Want::System
    });
    h.state().player().select_track(TrackKind::Video, Some(vid)).unwrap();
    step_until(&mut h, "打開影像：螢幕不關", |_| {
        fake.current() == Want::Display
    });
}

/// 剛載入、mpv 的軌道清單還沒到時，還不知道是影片還是只有聲音：先不要求（不會先要「電腦不睡眠」再改成螢幕）
#[test]
fn nothing_is_requested_before_the_tracks_are_known() {
    let fake = Arc::new(FakeInhibitor::new());
    let mut h = harness(headless(&[]), with(&fake, sample("common/mp4_h264_aac.mp4")));
    wait_playing(&mut h);
    step_until(&mut h, "螢幕不關", |_| fake.current() == Want::Display);
    // 模擬軌道清單到了、但還不知道選了哪些（mpv 先送清單、之後才送選了哪些）：影像軌還沒選不代表只有聲音。
    // 用 `set_fake_tracks` 固定住介面記下的清單：直接改 `state.tracks` 的話，mpv 之後再送的清單
    // （解碼器、音訊輸出開好時都會再送；慢的 CI 上就在下一幀）會把它蓋回選好的清單
    let mut unselected = h.state().player().state.tracks.clone();
    for t in &mut unselected {
        t.selected = false;
    }
    h.state_mut().player_mut().set_fake_tracks(Some(unselected));
    track_list_again(&mut h);
    h.step();
    assert_eq!(fake.current(), Want::Nothing, "{:?}", fake.history());
    // 模擬軌道清單還沒到（快的電腦上 mpv 在 FileLoaded 之前就送了，等不到這個時機）
    h.state_mut().player_mut().set_fake_tracks(Some(Vec::new()));
    track_list_again(&mut h);
    h.step();
    assert_eq!(fake.current(), Want::Nothing, "{:?}", fake.history());
    assert!(!fake.history().contains(&Want::System), "{:?}", fake.history());
    // 選了哪些也知道了：螢幕不關
    h.state_mut().player_mut().set_fake_tracks(None);
    step_until(&mut h, "知道選了影像軌：螢幕不關", |_| {
        fake.current() == Want::Display
    });
}

/// mpv 這一幀又送了一次軌道清單（選好的；CI 上就是這樣在測試改完之後蓋掉的）。用注入的，不靠 mpv 什麼時候送
fn track_list_again(h: &mut Harness<'_, VitascopeApp>) {
    let json = h.state().player().get_string("track-list").unwrap();
    assert!(json.contains("\"selected\":true"), "前提：mpv 的清單選了軌道 {json}");
    h.state_mut().player_mut().inject_event(Event::PropertyChange {
        id: 0,
        name: "track-list".into(),
        value: Value::String(json),
    });
}

#[test]
fn stop_screensaver_no_from_the_mpv_options_requests_nothing() {
    let fake = Arc::new(FakeInhibitor::new());
    let mut h = harness(
        headless(&[("stop-screensaver", "no")]),
        with(&fake, sample("common/mp4_h264_aac.mp4")),
    );
    wait_playing(&mut h);
    h.run_steps(20);
    assert_eq!(h.state().keep_awake(), Want::Nothing);
    eframe::App::on_exit(h.state_mut(), None);
    assert!(fake.history().is_empty(), "{:?}", fake.history());
}

/// `always`：暫停、停在最後一格也不關；但播完後的倒數、關機之前一定放掉（自己的要求不能擋住自己的關機）
#[test]
fn always_holds_while_paused_but_is_released_before_the_after_playback_action() {
    let fake = Arc::new(FakeInhibitor::new());
    let power = Arc::new(FakePower::new(PowerCaps::all_yes()));
    // 執行關機的那一刻：讓螢幕不關閉的要求是什麼、被等過幾次。之後先擋著（像 Windows 的睡眠，醒來才回傳），
    // 測試確認這段期間介面不會又要求螢幕不關
    let at_run: Arc<Mutex<Vec<(Want, usize)>>> = Arc::default();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    {
        let (fake, at_run, wait) = (fake.clone(), at_run.clone(), Mutex::new(wait));
        power.on_run(move || {
            at_run.lock().unwrap().push((fake.current(), fake.settles()));
            let _ = wait.lock().unwrap().recv_timeout(TIMEOUT);
        });
    }
    let mut h = harness(
        headless(&[("stop-screensaver", "always")]),
        Launch {
            system: Some(power.clone()),
            countdown_override: Some(Duration::from_secs(2)),
            ..with(&fake, sample("common/mp4_h264_aac.mp4"))
        },
    );
    wait_playing(&mut h);
    step_until(&mut h, "螢幕不關", |_| fake.current() == Want::Display);
    press(&mut h, egui::Key::Space);
    step_until(&mut h, "暫停了", |app| app.player().state.paused);
    h.run_steps(10);
    assert_eq!(fake.current(), Want::Display, "always：暫停時也不關");
    press(&mut h, egui::Key::Space);
    step_until(&mut h, "再播", |app| !app.player().state.paused);
    // 選「播完後關機」（要等背景問好系統支援）
    let start = Instant::now();
    while h.state().after_playback() != AfterPlayback::Shutdown {
        assert!(start.elapsed() < TIMEOUT, "選不了關機：{:?}", h.state().osd_text());
        h.state_mut().set_after_playback(AfterPlayback::Shutdown);
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    play_to_the_end(&mut h);
    // 倒數開始的那一幀就放掉（不多走一幀）
    step_until(&mut h, "開始倒數", |app| app.countdown_remaining().is_some());
    assert_eq!(fake.current(), Want::Nothing, "倒數時放掉（always 也一樣）");
    step_until(&mut h, "時間到", |app| app.countdown_remaining().is_none());
    step_until(&mut h, "背景開始執行關機", |_| {
        !at_run.lock().unwrap().is_empty()
    });
    let seen = at_run.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (want, settles) = seen[0];
    assert_eq!(want, Want::Nothing, "關機時已經放掉");
    assert!(settles >= 1, "關機之前等它放掉");
    // 執行中（Windows 的睡眠要醒來才回傳）：always 也不再要求
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.current(), Want::Nothing, "執行中不再要求：{:?}", fake.history());
    go.send(()).unwrap();
    step_until(&mut h, "執行完", |_| !power.runs().is_empty());
    assert_eq!(power.runs(), [PowerAction::Shutdown]);
    // 回來之後（睡眠醒來）：always 照樣不關
    step_until(&mut h, "醒來後 always 再要求", |_| {
        fake.current() == Want::Display
    });
}

/// `always` 時播完後「關閉影戲」：倒數、送出關閉之後都不再要求（結束時也放掉）
#[test]
fn always_is_released_when_closing_after_playback() {
    let fake = Arc::new(FakeInhibitor::new());
    let mut h = harness(
        headless(&[("stop-screensaver", "always")]),
        Launch {
            countdown_override: Some(Duration::from_secs(2)),
            ..with(&fake, sample("common/mp4_h264_aac.mp4"))
        },
    );
    wait_playing(&mut h);
    step_until(&mut h, "螢幕不關", |_| fake.current() == Want::Display);
    h.state_mut().set_after_playback(AfterPlayback::Close);
    assert_eq!(h.state().after_playback(), AfterPlayback::Close);
    play_to_the_end(&mut h);
    // 倒數開始的那一幀就放掉（不多走一幀）
    step_until(&mut h, "開始倒數", |app| app.countdown_remaining().is_some());
    assert_eq!(fake.current(), Want::Nothing, "倒數時放掉");
    step_until(&mut h, "送出關閉", |app| app.close_requested());
    for _ in 0..20 {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fake.current(), Want::Nothing, "關閉中不再要求：{:?}", fake.history());
    assert_eq!(h.state().keep_awake(), Want::Nothing);
}

// ───────────── Linux：真的 ScreenSaver 介面（測試自己的匯流排） ─────────────

#[cfg(all(unix, not(target_os = "macos")))]
mod screensaver {
    use super::*;
    use std::io::BufRead;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use vitascope::inhibit::{Failure, Inhibitor, RealInhibitor};
    use zbus::blocking::Connection;

    /// 測試自己的匯流排（Drop 時關掉）
    struct PrivateBus {
        child: Child,
        address: String,
        dir: PathBuf,
    }

    impl PrivateBus {
        /// None = 這台電腦沒有 dbus-daemon（不在 CI 上：跳過）
        fn start() -> Option<Self> {
            // 自己的最小設定：不用系統的 session.conf（裡面有標準的服務目錄，呼叫沒人提供的名稱時
            // 匯流排會去啟動這台電腦上真的螢幕保護程式）
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "vitascope-inhibit-bus-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let conf = dir.join("bus.conf");
            std::fs::write(
                &conf,
                format!(
                    r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                    dir.display()
                ),
            )
            .unwrap();
            let spawned = Command::new("dbus-daemon")
                .arg(format!("--config-file={}", conf.display()))
                .args(["--nofork", "--print-address=1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .spawn();
            let mut child = match spawned {
                Ok(child) => child,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    assert!(
                        std::env::var_os("CI").is_none(),
                        "CI 上一定要有 dbus-daemon（ci.yml 的 apt 要裝 dbus-daemon）：ScreenSaver 的測試不能跳過"
                    );
                    eprintln!("跳過：找不到 dbus-daemon（apt install dbus-daemon），ScreenSaver 的測試沒有跑");
                    let _ = std::fs::remove_dir_all(&dir);
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
            Some(Self { child, address, dir })
        }

        fn kill(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            self.kill();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// 假的桌面螢幕保護程式（像 KDE、gsd 的 org.freedesktop.ScreenSaver）：記下收到的呼叫
    #[derive(Clone, Default)]
    struct Saver {
        calls: Arc<Mutex<Vec<String>>>,
        next: Arc<Mutex<u32>>,
        /// 要求的那一端在匯流排上的名稱（最後一個 Inhibit 的）
        sender: Arc<Mutex<Option<String>>>,
        /// true = UnInhibit 回錯誤（桌面忙、逾時）
        fail_uninhibit: Arc<AtomicBool>,
    }

    #[zbus::interface(name = "org.freedesktop.ScreenSaver")]
    impl Saver {
        fn inhibit(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
            application_name: String,
            reason_for_inhibit: String,
        ) -> u32 {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            self.calls.lock().unwrap().push(format!(
                "Inhibit {application_name} {} → {next}",
                !reason_for_inhibit.is_empty()
            ));
            *self.sender.lock().unwrap() = header.sender().map(|s| s.to_string());
            *next
        }

        fn un_inhibit(&self, cookie: u32) -> zbus::fdo::Result<()> {
            if self.fail_uninhibit.load(Ordering::SeqCst) {
                self.calls.lock().unwrap().push(format!("UnInhibit {cookie} 失敗"));
                return Err(zbus::fdo::Error::Failed("忙".into()));
            }
            self.calls.lock().unwrap().push(format!("UnInhibit {cookie}"));
            Ok(())
        }
    }

    /// 系統顯示的程式名稱（跟視窗、媒體控制一樣；語言照這個執行緒的）
    fn inhibit_call(cookie: u32) -> String {
        format!("Inhibit {} true → {cookie}", vitascope::app::app_name())
    }

    fn serve(bus: &PrivateBus) -> (Connection, Saver) {
        let saver = Saver::default();
        let conn = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.freedesktop.ScreenSaver")
            .unwrap()
            .serve_at("/org/freedesktop/ScreenSaver", saver.clone())
            .unwrap()
            .build()
            .unwrap();
        (conn, saver)
    }

    fn settled(inh: &RealInhibitor) {
        assert!(inh.settle(TIMEOUT), "背景沒有做完");
    }

    #[test]
    fn screensaver_inhibit_round_trip() {
        let Some(bus) = PrivateBus::start() else { return };
        let (_service, saver) = serve(&bus);
        let calls = || saver.calls.lock().unwrap().clone();
        let inh = RealInhibitor::on_bus(&bus.address);
        inh.set(Want::Display);
        settled(&inh);
        assert_eq!(calls(), [inhibit_call(1)]);
        assert_eq!(inh.held(), Want::Display);
        // 同樣的再要一次：不重複要求
        inh.set(Want::Display);
        settled(&inh);
        assert_eq!(calls().len(), 1);
        // 只有聲音：這個介面只管螢幕，放掉
        inh.set(Want::System);
        settled(&inh);
        assert_eq!(calls(), [inhibit_call(1), "UnInhibit 1".into()]);
        inh.set(Want::Nothing);
        settled(&inh);
        assert_eq!(calls().len(), 2, "沒拿著就不用放");
        inh.set(Want::Display);
        settled(&inh);
        // 結束：放掉（用拿到的那個 cookie）
        drop(inh);
        assert_eq!(
            calls(),
            [
                inhibit_call(1),
                "UnInhibit 1".into(),
                inhibit_call(2),
                "UnInhibit 2".into()
            ]
        );
    }

    /// 放不掉（UnInhibit 回錯誤）：關掉連線，桌面看到我們離開匯流排就收回；下次要的時候重新連
    #[test]
    fn a_failed_uninhibit_drops_the_connection_so_the_desktop_releases_it() {
        let Some(bus) = PrivateBus::start() else { return };
        let (service, saver) = serve(&bus);
        let inh = RealInhibitor::on_bus(&bus.address);
        inh.set(Want::Display);
        settled(&inh);
        let first = saver.sender.lock().unwrap().clone().expect("Inhibit 的來源");
        saver.fail_uninhibit.store(true, Ordering::SeqCst);
        inh.set(Want::Nothing);
        settled(&inh);
        assert_eq!(inh.held(), Want::Nothing);
        assert!(
            inh.failures().iter().any(|f| matches!(f, Failure::Failed(_))),
            "{:?}",
            inh.failures()
        );
        // 我們那條連線離開匯流排了（桌面的 ScreenSaver 會收回那條連線的要求）
        let dbus = zbus::blocking::fdo::DBusProxy::new(&service).unwrap();
        let name = zbus::names::BusName::try_from(first.as_str()).unwrap();
        let start = Instant::now();
        while dbus.name_has_owner(name.clone()).unwrap() {
            assert!(start.elapsed() < TIMEOUT, "放不掉時沒有關掉連線：{first} 還在匯流排上");
            std::thread::sleep(Duration::from_millis(20));
        }
        // 下次要：重新連上、再要一次
        saver.fail_uninhibit.store(false, Ordering::SeqCst);
        inh.set(Want::Display);
        settled(&inh);
        assert_eq!(inh.held(), Want::Display);
        let second = saver.sender.lock().unwrap().clone().unwrap();
        assert_ne!(second, first, "新的連線");
        drop(inh);
        assert_eq!(
            saver.calls.lock().unwrap().clone(),
            [
                inhibit_call(1),
                "UnInhibit 1 失敗".into(),
                inhibit_call(2),
                "UnInhibit 2".into()
            ]
        );
    }

    #[test]
    fn a_missing_screensaver_is_logged_once_and_never_panics() {
        let Some(bus) = PrivateBus::start() else { return };
        let inh = RealInhibitor::on_bus(&bus.address);
        for want in [Want::Display, Want::Nothing, Want::Display, Want::System] {
            inh.set(want);
            settled(&inh);
        }
        assert_eq!(inh.held(), Want::System, "沒有拿著螢幕");
        assert_eq!(
            inh.failures(),
            [Failure::Missing("org.freedesktop.ScreenSaver".into())],
            "只記錄一次"
        );
        // 之後有了（桌面晚一點才啟動）：下一次要的時候就拿得到
        let (_service, saver) = serve(&bus);
        inh.set(Want::Display);
        settled(&inh);
        assert_eq!(inh.held(), Want::Display);
        assert_eq!(saver.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_lost_bus_does_not_panic_and_nothing_is_held() {
        let Some(mut bus) = PrivateBus::start() else { return };
        let (service, _saver) = serve(&bus);
        let inh = RealInhibitor::on_bus(&bus.address);
        inh.set(Want::Display);
        settled(&inh);
        assert_eq!(inh.held(), Want::Display);
        // 匯流排不見了（重新登入、dbus-daemon 當掉）：背景發現後重新連，連不上就記錄、什麼都不拿
        drop(service);
        bus.kill();
        let start = Instant::now();
        while inh.held() != Want::Nothing {
            assert!(start.elapsed() < TIMEOUT, "沒有發現匯流排不見了");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!inh.failures().is_empty());
        inh.set(Want::Nothing);
        settled(&inh);
        drop(inh);
    }

    /// 真的影戲、真的播放器：播放中的影片在測試的匯流排上要求、暫停時放掉、再播再要、結束程式時放掉（等它做完）
    #[test]
    fn the_app_inhibits_over_dbus_while_a_video_plays() {
        let Some(bus) = PrivateBus::start() else { return };
        let (_service, saver) = serve(&bus);
        let calls = || saver.calls.lock().unwrap().clone();
        let inh = Arc::new(RealInhibitor::on_bus(&bus.address));
        let mut h = harness(
            headless(&[]),
            Launch {
                files: vec![sample("common/mp4_h264_aac.mp4")],
                inhibitor: Some(inh.clone()),
                ..Default::default()
            },
        );
        wait_playing(&mut h);
        step_until(&mut h, "要求螢幕不關", |_| {
            calls().iter().any(|c| c.starts_with("Inhibit"))
        });
        press(&mut h, egui::Key::Space);
        step_until(&mut h, "暫停時放掉", |_| {
            calls().iter().any(|c| c.starts_with("UnInhibit"))
        });
        press(&mut h, egui::Key::Space);
        step_until(&mut h, "再播時再要", |_| calls().len() == 3);
        assert!(inh.settle(TIMEOUT));
        assert_eq!(inh.held(), Want::Display);
        // 結束程式：on_exit 自己放掉並等它做完（不是暫停時已經放掉的）
        eframe::App::on_exit(h.state_mut(), None);
        assert_eq!(inh.held(), Want::Nothing);
        assert_eq!(
            calls(),
            [
                inhibit_call(1),
                "UnInhibit 1".into(),
                inhibit_call(2),
                "UnInhibit 2".into()
            ]
        );
    }
}
