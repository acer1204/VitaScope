//! 跟隨系統的深淺色：Linux 桌面的設定 portal（`org.freedesktop.portal.Settings`，F4）。
//!
//! - 只在測試自己開的 dbus-daemon 上（`support::private_bus`：自己的最小設定，不會啟動電腦上真的 portal），
//!   絕不是使用者的工作階段匯流排；CI 上沒有 dbus-daemon 時失敗（不跳過）。
//! - 假的 portal：新的（有 `ReadOne`）和舊的（只有 `Read`，值多包一層 variant）；`SettingChanged` 由測試送。
//! - 其他系統沒有 portal（winit 回報系統的深淺色）：這個檔案在那裡沒有測試。

#[cfg(all(unix, not(target_os = "macos")))]
mod support;

#[cfg(all(unix, not(target_os = "macos")))]
mod linux {
    use super::support::private_bus::PrivateBus;
    use eframe::egui::{self, Theme};
    use egui_kittest::Harness;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use vitascope::app::{Launch, VitascopeApp};
    use vitascope::instance::Wake;
    use vitascope::player::{Options, Player};
    use vitascope::settings::Settings;
    use vitascope::theme::portal::{self, Portal};
    use vitascope::theme::{ThemeChoice, ThemeSource};
    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedValue, Value};

    /// 慢的 CI 也要等得到（背景執行緒、匯流排都是非同步的）
    const TIMEOUT: Duration = Duration::from_secs(20);
    /// 測試縮短的「多久再試」（產品是 30 秒）：匯流排重開、portal 重新出現要在這之內（慢的 CI 也夠；再錯過一次還有第二次）
    const RETRY: Duration = Duration::from_secs(3);

    /// 假的 portal 共用的：現在的 color-scheme、收到的呼叫
    #[derive(Clone, Default)]
    struct Desktop {
        scheme: Arc<Mutex<u32>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Desktop {
        fn with(scheme: u32) -> Self {
            let d = Self::default();
            *d.scheme.lock().unwrap() = scheme;
            d
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        /// 回答 `namespace` / `key`：是深淺色就是現在的值，`nested` = 舊的 `Read` 多包一層
        fn answer(&self, method: &str, namespace: &str, key: &str, nested: bool) -> zbus::fdo::Result<OwnedValue> {
            self.calls.lock().unwrap().push(format!("{method} {namespace} {key}"));
            if namespace != portal::NAMESPACE || key != portal::KEY {
                return Err(zbus::fdo::Error::Failed("沒有這個設定".into()));
            }
            let n = Value::U32(*self.scheme.lock().unwrap());
            let value = if nested { Value::Value(Box::new(n)) } else { n };
            Ok(value.try_into_owned().unwrap())
        }
    }

    /// 新的 portal（設定介面第 2 版起有 `ReadOne`）
    struct NewPortal(Desktop);

    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl NewPortal {
        fn read_one(&self, namespace: String, key: String) -> zbus::fdo::Result<OwnedValue> {
            self.0.answer("ReadOne", &namespace, &key, false)
        }

        fn read(&self, namespace: String, key: String) -> zbus::fdo::Result<OwnedValue> {
            self.0.answer("Read", &namespace, &key, true)
        }
    }

    /// 舊的 portal（第 1 版：只有 `Read`，值是 `v(v)`）
    struct OldPortal(Desktop);

    #[zbus::interface(name = "org.freedesktop.portal.Settings")]
    impl OldPortal {
        fn read(&self, namespace: String, key: String) -> zbus::fdo::Result<OwnedValue> {
            self.0.answer("Read", &namespace, &key, true)
        }
    }

    /// 在測試的匯流排上提供 portal（跟桌面一樣的名稱、路徑）
    fn serve<I: zbus::object_server::Interface>(bus: &PrivateBus, portal: I) -> Connection {
        zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(portal::DESTINATION)
            .unwrap()
            .serve_at(portal::PATH, portal)
            .unwrap()
            .build()
            .unwrap()
    }

    /// 桌面改了設定：送 `SettingChanged`
    fn changed(conn: &Connection, namespace: &str, key: &str, scheme: u32) {
        conn.emit_signal(
            None::<&str>,
            portal::PATH,
            portal::INTERFACE,
            "SettingChanged",
            &(namespace, key, Value::U32(scheme)),
        )
        .unwrap();
    }

    /// 叫了幾次 wake
    fn counting_wake() -> (Wake, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let counted = n.clone();
        (
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
            n,
        )
    }

    fn wait_until(what: &str, cond: impl Fn() -> bool) {
        let start = Instant::now();
        while !cond() {
            assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn portal_answers_and_follows_setting_changed() {
        let Some(bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let desktop = Desktop::with(1);
        let conn = serve(&bus, NewPortal(desktop.clone()));
        let mut source = Portal::at(&bus.address);
        assert_eq!(source.get(), None);
        assert!(!source.running(), "還沒開始問");
        let (wake, wakes) = counting_wake();
        source.watch(wake.clone());
        // 背景執行緒先放上值、再數、再叫醒：三樣都等到（只等值的話另外兩樣可能還沒做）
        wait_until("portal 回答深色、數過、叫介面重畫", || {
            source.get() == Some(Theme::Dark) && source.reads() >= 1 && wakes.load(Ordering::SeqCst) >= 1
        });
        assert_eq!(source.reads(), 1);
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "回答時叫一次介面重畫");
        assert_eq!(
            desktop.calls(),
            ["ReadOne org.freedesktop.appearance color-scheme"],
            "新的 portal 用 ReadOne"
        );
        // 再叫一次 watch 不會再連一次
        source.watch(wake);
        // 桌面改成淺色、深色、沒有偏好（0 = 淺色）
        changed(&conn, portal::NAMESPACE, portal::KEY, 2);
        wait_until("改成淺色", || source.get() == Some(Theme::Light));
        changed(&conn, portal::NAMESPACE, portal::KEY, 1);
        wait_until("改成深色", || source.get() == Some(Theme::Dark));
        changed(&conn, portal::NAMESPACE, portal::KEY, 0);
        // 值先放上、才叫醒：等到這三次改變的叫醒都到了（回答 1 + 改變 3），才開始數下面的
        wait_until("沒有偏好 = 淺色、叫醒過", || {
            source.get() == Some(Theme::Light) && wakes.load(Ordering::SeqCst) >= 4
        });
        let before = wakes.load(Ordering::SeqCst);
        assert_eq!(before, 4, "每次改變叫醒一次");
        // 別的設定不理（匯流排上就濾掉）：之後送的深色是唯一叫醒的。
        // 同一個連線送的訊號依序到：看到深色時，前面兩個（不認得的值）如果沒被濾掉，早就放上值、叫醒過了
        changed(&conn, portal::NAMESPACE, "accent-color", 9);
        changed(&conn, "org.gnome.desktop.interface", portal::KEY, 9);
        changed(&conn, portal::NAMESPACE, portal::KEY, 1);
        wait_until("收到深淺色的改變", || {
            source.get() == Some(Theme::Dark) && wakes.load(Ordering::SeqCst) > before
        });
        assert_eq!(wakes.load(Ordering::SeqCst), before + 1, "別的設定沒有叫醒介面");
        // 不認得的值：當成不知道
        changed(&conn, portal::NAMESPACE, portal::KEY, 9);
        wait_until("不認得的值", || source.get().is_none());
        assert_eq!(source.reads(), 1, "只讀一次，之後跟著訊號");
        assert_eq!(desktop.calls().len(), 1, "watch 兩次也只連一次");
    }

    #[test]
    fn old_portal_without_read_one_falls_back_to_read() {
        let Some(bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let desktop = Desktop::with(1);
        let conn = serve(&bus, OldPortal(desktop.clone()));
        let mut source = Portal::at(&bus.address);
        let (wake, _) = counting_wake();
        source.watch(wake);
        wait_until("舊的 portal 回答深色（v(v)）", || {
            source.get() == Some(Theme::Dark)
        });
        assert_eq!(desktop.calls(), ["Read org.freedesktop.appearance color-scheme"]);
        changed(&conn, portal::NAMESPACE, portal::KEY, 2);
        wait_until("改成淺色", || source.get() == Some(Theme::Light));
    }

    #[test]
    fn no_portal_means_unknown_and_drop_ends_the_wait() {
        // 匯流排上沒有 portal（最小的視窗管理員、沒有 xdg-desktop-portal）：不知道，30 秒後再試一次
        let Some(bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let mut source = Portal::at(&bus.address);
        let running = source.running_flag();
        let (wake, wakes) = counting_wake();
        source.watch(wake);
        // 先數、再叫醒：兩樣都等到
        wait_until("問過一次、叫醒介面", || {
            source.reads() >= 1 && wakes.load(Ordering::SeqCst) >= 1
        });
        assert_eq!(source.reads(), 1);
        assert_eq!(source.get(), None);
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "問不到也叫介面（設定頁的提示）");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(source.reads(), 1, "不會一直重試");
        assert!(running(), "等著過一陣子再試");
        // 結束（關閉影戲）：等著重試的執行緒馬上結束
        drop(source);
        wait_until("背景執行緒結束", || !running());
    }

    #[test]
    fn dropping_the_portal_closes_the_connection() {
        // 等桌面改設定的執行緒（停在訊號上）也會結束
        let Some(bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let _conn = serve(&bus, NewPortal(Desktop::with(2)));
        let mut source = Portal::at(&bus.address);
        let running = source.running_flag();
        let (wake, _) = counting_wake();
        source.watch(wake);
        wait_until("portal 回答淺色", || source.get() == Some(Theme::Light));
        assert!(running());
        drop(source);
        wait_until("背景執行緒結束", || !running());
    }

    #[test]
    fn bus_gone_keeps_the_last_answer() {
        // 匯流排不見了（重新登入、dbus-daemon 當掉）：保留最後的值，不會一直重試
        let Some(mut bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let _conn = serve(&bus, NewPortal(Desktop::with(1)));
        let mut source = Portal::at(&bus.address);
        let running = source.running_flag();
        let (wake, _) = counting_wake();
        source.watch(wake);
        wait_until("portal 回答深色、數過", || {
            source.get() == Some(Theme::Dark) && source.reads() >= 1
        });
        bus.kill();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(source.get(), Some(Theme::Dark), "保留最後的值");
        assert_eq!(source.reads(), 1, "30 秒後才再試");
        assert!(running());
        drop(source);
        wait_until("背景執行緒結束", || !running());
    }

    #[test]
    fn reconnects_after_the_bus_comes_back() {
        // 重新登入：匯流排斷了又回來、portal 重新出現。過一陣子再連：讀新的值、重新訂閱 `SettingChanged`
        let Some(mut bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let first = serve(&bus, NewPortal(Desktop::with(1)));
        let mut source = Portal::at(&bus.address).retry_after(RETRY);
        let running = source.running_flag();
        let (wake, _) = counting_wake();
        source.watch(wake);
        wait_until("portal 回答深色、數過", || {
            source.get() == Some(Theme::Dark) && source.reads() >= 1
        });
        bus.restart();
        drop(first);
        let desktop = Desktop::with(2);
        let conn = serve(&bus, NewPortal(desktop.clone()));
        wait_until("再連一次、讀到新的值", || {
            source.get() == Some(Theme::Light) && source.reads() >= 2
        });
        assert_eq!(desktop.calls(), ["ReadOne org.freedesktop.appearance color-scheme"]);
        assert!(running(), "回答了就繼續跟著");
        changed(&conn, portal::NAMESPACE, portal::KEY, 1);
        wait_until("重新訂閱：桌面改成深色", || {
            source.get() == Some(Theme::Dark)
        });
        drop(source);
        wait_until("背景執行緒結束", || !running());
    }

    #[test]
    fn no_portal_twice_in_a_row_gives_up() {
        // 沒有 portal 的桌面：再試一次還是問不到就不再試（不會每 30 秒連一次）
        let Some(bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let mut source = Portal::at(&bus.address).retry_after(Duration::from_millis(100));
        let running = source.running_flag();
        let (wake, wakes) = counting_wake();
        source.watch(wake);
        // 先數、再叫醒、最後執行緒結束：等到結束，數字就不會再變
        wait_until("問兩次都問不到、放棄", || !running() && source.reads() >= 2);
        assert_eq!(source.reads(), 2, "連續兩次問不到就放棄");
        assert_eq!(wakes.load(Ordering::SeqCst), 2);
        assert_eq!(source.get(), None);
    }

    /// 整個接起來：App 選了跟隨系統，用真的 `Portal`（測試的匯流排），桌面改設定時介面跟著換
    #[test]
    fn app_follows_the_portal_on_a_private_bus() {
        let Some(bus) = PrivateBus::start("系統深淺色（portal）") else {
            return;
        };
        let desktop = Desktop::with(2);
        let conn = serve(&bus, NewPortal(desktop.clone()));
        let player = Player::new(Options::headless()).unwrap();
        let mut settings = Settings::default();
        settings.theme = ThemeChoice::System;
        let launch = Launch {
            theme_source: Some(Box::new(Portal::at(&bus.address))),
            ..Default::default()
        };
        let mut h = Harness::builder()
            .with_size([960.0, 600.0])
            .build_eframe(move |cc| VitascopeApp::new(cc, player, settings, launch));
        let step_until = |h: &mut Harness<'_, VitascopeApp>, what: &str, want: Theme| {
            // 從現在這一幀開始收（建立 Harness 時已經跑過一幀，portal 可能那時就回答了）
            let start = Instant::now();
            let mut sent = Vec::new();
            loop {
                if let Some(v) = h.output().viewport_output.get(&egui::ViewportId::ROOT) {
                    sent.extend(v.commands.iter().filter_map(|c| match c {
                        egui::ViewportCommand::SetTheme(t) => Some(*t),
                        _ => None,
                    }));
                }
                if h.ctx.theme() == want {
                    return sent;
                }
                assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}");
                std::thread::sleep(Duration::from_millis(10));
                h.step();
            }
        };
        // 建立 Harness 時 kittest 已經跑了好幾幀（回答可能在那時就到了）：只看結果
        step_until(&mut h, "portal 回答淺色", Theme::Light);
        assert!(!h.ctx.global_style().visuals.dark_mode);
        assert!(!h.ctx.options(|o| o.sync_window_theme), "標題列由 App 自己送");
        changed(&conn, portal::NAMESPACE, portal::KEY, 1);
        let sent = step_until(&mut h, "桌面改成深色", Theme::Dark);
        assert_eq!(
            sent.last(),
            Some(&egui::SystemTheme::Dark),
            "標題列也換成深色：{sent:?}"
        );
        changed(&conn, portal::NAMESPACE, portal::KEY, 2);
        let sent = step_until(&mut h, "桌面改成淺色", Theme::Light);
        assert_eq!(sent.last(), Some(&egui::SystemTheme::Light), "{sent:?}");
        assert_eq!(desktop.calls().len(), 1);
    }
}
