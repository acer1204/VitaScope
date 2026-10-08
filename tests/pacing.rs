//! 流暢播放在播放引擎這一層的行為（headless：vo=null 用 vo-null-fps 當假的螢幕更新率，不出畫面、不出聲音，
//! 三個平台的 CI 都跑）。介面怎麼決定、什麼時候送設定在 tests/ui.rs 與 src/app/pacing.rs 的測試。

use std::time::{Duration, Instant};
use vitascope::player::{AsyncKey, Options, Player};

/// 23.976 fps：在 120 Hz 上每格 5.005 次更新，對齊 119.88 Hz 剛好 5 次
const SRC: &str = "av://lavfi:testsrc2=size=320x240:rate=24000/1001:duration=30";
const TIMEOUT: Duration = Duration::from_secs(15);

fn player(extra: &[(&str, &str)]) -> Player {
    Player::new(Options {
        extra: extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Options::headless()
    })
    .expect("建立 mpv 失敗")
}

/// mpv 回報的顯示同步數字。不在顯示同步時 vsync-ratio 讀不到，速度修正是 1（0.37 讀不到）
#[derive(Debug, Clone, Copy, PartialEq)]
struct Numbers {
    active: bool,
    ratio: Option<f64>,
    speed: Option<f64>,
}

fn numbers(p: &Player) -> Numbers {
    Numbers {
        active: p.state.display_sync_active,
        ratio: p.get_f64("vsync-ratio").ok(),
        speed: p.get_f64("video-speed-correction").ok(),
    }
}

/// 一直處理事件，直到條件成立；逾時就把最後讀到的數字印出來
fn wait_numbers(p: &mut Player, what: &str, cond: impl Fn(&Numbers) -> bool) -> Numbers {
    let start = Instant::now();
    loop {
        p.poll();
        let n = numbers(p);
        if cond(&n) {
            return n;
        }
        assert!(
            p.state.loaded || start.elapsed() < Duration::from_secs(5),
            "檔案沒有開起來：{what}（{:?}）",
            p.recent_errors()
        );
        assert!(start.elapsed() < TIMEOUT, "等待逾時：{what}，最後讀到 {n:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 沒有修正播放速度
fn unchanged(speed: Option<f64>) -> bool {
    speed.is_none_or(|s| s == 1.0)
}

fn near(v: Option<f64>, want: f64, tolerance: f64) -> bool {
    v.is_some_and(|v| (v - want).abs() < tolerance)
}

#[test]
fn display_sync_engages_on_vo_null() {
    // 假的螢幕：120 Hz（實際的電視就是剛好 120.000 Hz）
    let mut p = player(&[("vo-null-fps", "120")]);
    // 流暢播放在開檔之前就設好（跟啟動時就打開的情況一樣）：先設更新率，再換同步方式
    let failed = p.apply_sync(&[
        ("display-fps-override", "119.880000".to_owned()),
        ("video-sync", "display-resample".to_owned()),
    ]);
    assert!(failed.is_empty(), "{failed:?}");
    p.open(SRC).unwrap();
    // 對齊 119.88 Hz：每格剛好 5 次更新，速度幾乎不用修正
    let n = wait_numbers(&mut p, "依 119.88 Hz 同步", |n| {
        n.active && near(n.ratio, 5.0, 0.01) && near(n.speed, 1.0, 2e-4)
    });
    eprintln!("119.88 Hz：{n:?}");
    // 播放中換成 120 Hz（拖到另一個螢幕）：影片快 0.1%，每格還是 5 次
    p.set_async(AsyncKey::DisplayFps, "display-fps-override", "120.000000")
        .unwrap();
    let n = wait_numbers(&mut p, "依 120 Hz 同步，影片快 0.1%", |n| {
        n.active && near(n.ratio, 5.0, 0.01) && near(n.speed, 1.001, 2e-4)
    });
    eprintln!("120 Hz：{n:?}");
    // 改回一般播放（先換同步方式）：不再依螢幕同步，速度不修正
    p.set_async(AsyncKey::VideoSync, "video-sync", "audio").unwrap();
    p.set_async(AsyncKey::DisplayFps, "display-fps-override", "0").unwrap();
    wait_numbers(&mut p, "改回音訊同步", |n| {
        !n.active && n.ratio.is_none() && unchanged(n.speed)
    });
    assert!(p.state.loaded, "還在播放");
}

#[test]
fn display_sync_inactive_by_default() {
    // 沒打開流暢播放：螢幕的更新率 mpv 知道也不會依螢幕同步（跟以前一樣）
    for extra in [&[][..], &[("vo-null-fps", "120")][..]] {
        let mut p = player(extra);
        p.open(SRC).unwrap();
        p.wait_state(TIMEOUT, |s| s.loaded && s.time_pos > 1.0)
            .unwrap_or_else(|e| panic!("{extra:?}：{e}"));
        let n = numbers(&p);
        assert!(!n.active && n.ratio.is_none() && unchanged(n.speed), "{extra:?}：{n:?}");
        assert_eq!(p.get_string("video-sync").unwrap(), "audio");
        assert_eq!(p.get_f64("display-fps-override").unwrap(), 0.0);
    }
}
