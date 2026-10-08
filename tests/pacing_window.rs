//! 流暢播放的實機測試：真的開一個全螢幕視窗播 23.976 fps 的平移影片，從 mpv 的記錄算每格顯示幾次螢幕更新。
//! 會在螢幕上開視窗約 15 秒，所以預設不跑（#[ignore]），在有實體螢幕的開發機上手動跑：
//!
//! ```text
//! python scripts/gen_samples.py --tier pacing
//! cargo test --test pacing_window -- --ignored --nocapture --test-threads=1
//! ```
//!
//! 設定檔用暫存資料夾裡的（APPDATA 之類的環境變數換掉），不會動到使用者的設定。
//! 詳細的數字（每格的時間、swap 間隔）可以再用 `python scripts/pacing_stats.py <資料夾>` 看。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 一次只開一個視窗（同時開會搶螢幕）
static SCREEN: Mutex<()> = Mutex::new(());
/// 開始播放後幾秒截圖、關閉
const SHOT_DELAY: f64 = 12.0;

fn sample() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/generated/pacing/pan_23976.mkv");
    assert!(
        p.exists(),
        "找不到樣本 {}，請先執行：python scripts/gen_samples.py --tier pacing",
        p.display()
    );
    p
}

/// 一次執行的結果
struct Run {
    dir: PathBuf,
    /// mpv 的記錄（log-file）
    log: String,
    /// 影戲自己的記錄（stderr）
    stderr: String,
}

/// 用暫存的設定檔（`settings` 是 settings.json 的內容）開影戲播平移影片，等它截圖後自己關閉
fn run(name: &str, settings: &str, env: &[(&str, &str)]) -> Run {
    let _screen = SCREEN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("vitascope-pacing-window").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // VITASCOPE_MPV_OPTS 用空白分隔，路徑裡不能有空白
    assert!(
        !dir.to_string_lossy().contains(char::is_whitespace),
        "暫存資料夾的路徑有空白：{}",
        dir.display()
    );
    // 三個平台的設定檔位置都放一份（見 settings::config_dir）
    let home = dir.join("home");
    for config in [
        dir.join("appdata").join("Vitascope"),
        dir.join("xdg").join("vitascope"),
        home.join("Library/Application Support/Vitascope"),
    ] {
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("settings.json"), settings).unwrap();
    }
    let log = dir.join("mpv.log");
    let mpv_opts = format!(
        "dump-stats={} log-file={} msg-level=all=v,cplayer=trace",
        dir.join("stats.txt").display(),
        log.display()
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vitascope"));
    cmd.arg("--new-window")
        .arg("--fullscreen")
        .arg(sample())
        .arg("--shot")
        .arg(dir.join("shot.png"))
        .arg("--shot-delay")
        .arg(SHOT_DELAY.to_string())
        .env("APPDATA", dir.join("appdata"))
        .env("LOCALAPPDATA", dir.join("localappdata"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("HOME", &home)
        .env("VITASCOPE_MPV_OPTS", mpv_opts)
        .env("VITASCOPE_DEBUG", "pacing")
        .env_remove("VITASCOPE_PACING")
        .env_remove("VITASCOPE_TEST_MINIMIZE")
        .envs(env.iter().copied())
        .stdout(std::process::Stdio::null())
        .stderr(std::fs::File::create(dir.join("stderr.txt")).unwrap());
    let mut child = cmd.spawn().expect("無法啟動影戲");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(90) {
            let _ = child.kill();
            panic!("影戲 90 秒還沒結束（{}）", dir.display());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let read = |p: &Path| String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).into_owned();
    let run = Run {
        log: read(&log),
        stderr: read(&dir.join("stderr.txt")),
        dir,
    };
    assert!(status.success(), "影戲結束時出錯：{status}\n{}", run.stderr);
    assert!(
        run.stderr.contains("截圖已存到"),
        "沒有截到圖（{}）：\n{}",
        run.dir.display(),
        run.stderr
    );
    run
}

/// mpv 記錄裡的一行：`[   1.096][t][cplayer] s=1.001000 vsyncs=5 dur=…` → (時間, 內容)
fn log_lines(log: &str) -> impl Iterator<Item = (f64, &str)> {
    log.lines().filter_map(|l| {
        let rest = l.strip_prefix('[')?;
        let (t, rest) = rest.split_once(']')?;
        Some((t.trim().parse().ok()?, rest))
    })
}

/// 依螢幕同步時每一格顯示幾次更新（mpv 的 cplayer trace）：(時間, 次數)
fn vsyncs(log: &str) -> Vec<(f64, u32)> {
    log_lines(log)
        .filter_map(|(t, l)| {
            let v = l.split_once(" vsyncs=")?.1;
            let n = v.split_whitespace().next()?.parse().ok()?;
            Some((t, n))
        })
        .collect()
}

/// 依螢幕同步時每一格的速度修正（cplayer trace 的 `s=`）：(時間, 倍數)
fn speeds(log: &str) -> Vec<(f64, f64)> {
    log_lines(log)
        .filter_map(|(t, l)| {
            let (head, _) = l.split_once(" vsyncs=")?;
            let s = head.rsplit_once("s=")?.1;
            Some((t, s.trim().parse().ok()?))
        })
        .collect()
}

/// 影戲自己設定了 mpv 的同步方式或更新率（`Set property: video-sync=…`）
fn sets_sync_options(log: &str) -> bool {
    log.lines()
        .any(|l| l.contains("Set property: video-sync=") || l.contains("Set property: display-fps-override="))
}

/// mpv 等不到畫面（render() 沒被呼叫）的時間
fn stuck(log: &str) -> Vec<f64> {
    log_lines(log)
        .filter(|(_, l)| l.contains("not being called or stuck"))
        .map(|(t, _)| t)
        .collect()
}

/// 一段時間內的每格更新次數：(總格數, 剛好 5 次的格數, 分布)
fn cadence(samples: &[(f64, u32)], from: f64, to: f64) -> (usize, usize, Vec<(u32, usize)>) {
    let inside: Vec<u32> = samples
        .iter()
        .filter(|(t, _)| (from..to).contains(t))
        .map(|(_, n)| *n)
        .collect();
    let mut hist: Vec<(u32, usize)> = Vec::new();
    for n in &inside {
        match hist.iter_mut().find(|(k, _)| k == n) {
            Some((_, c)) => *c += 1,
            None => hist.push((*n, 1)),
        }
    }
    hist.sort();
    let fives = inside.iter().filter(|n| **n == 5).count();
    (inside.len(), fives, hist)
}

/// 「Assuming 120.000000 FPS for display sync.」的更新率
fn assumed_fps(log: &str) -> Option<f64> {
    let l = log
        .lines()
        .find(|l| l.contains("Assuming ") && l.contains("FPS for display sync"))?;
    l.split("Assuming ").nth(1)?.split_whitespace().next()?.parse().ok()
}

/// 截圖時的播放位置（影戲印在 stderr）
fn shot_position(stderr: &str) -> Option<f64> {
    let l = stderr.lines().find(|l| l.contains("截圖時的播放位置："))?;
    l.split("截圖時的播放位置：")
        .nth(1)?
        .trim_end_matches(" 秒")
        .trim()
        .parse()
        .ok()
}

const AUTO: &str = r#"{ "smooth": "auto" }"#;

#[test]
#[ignore = "會在螢幕上開全螢幕視窗（約 15 秒）；在開發機上手動跑"]
fn display_sync_gives_an_exact_cadence() {
    let r = run("display", AUTO, &[]);
    let fps = assumed_fps(&r.log).unwrap_or_else(|| panic!("mpv 沒有依螢幕同步（{}）\n{}", r.dir.display(), r.stderr));
    eprintln!("mpv 用的更新率：{fps} Hz");
    assert!(
        (fps - 120.0).abs() < 0.01 || (fps - 119.88).abs() < 0.01,
        "預期 120 或 119.88 Hz 的螢幕，mpv 用的是 {fps}"
    );
    let samples = vsyncs(&r.log);
    let t0 = samples.first().expect("沒有每格的同步記錄（cplayer trace）").0;
    // 開始播放後 1 秒到截圖前 1 秒
    let (from, to) = (t0 + 1.0, t0 + SHOT_DELAY - 1.0);
    let (total, fives, hist) = cadence(&samples, from, to);
    let ratio = fives as f64 / total.max(1) as f64;
    eprintln!(
        "每格更新次數：{fives}/{total} 格是 5 次（{:.2}%），分布 {hist:?}",
        ratio * 100.0
    );
    assert!(total >= 200, "格數太少：{total}");
    assert!(
        ratio >= 0.995,
        "每格 5 次的比例 {:.2}% 不到 99.5%：{hist:?}",
        ratio * 100.0
    );
    // 速度修正一直一樣（display-resample 穩定下來，不是一直在調）
    let s: Vec<f64> = speeds(&r.log)
        .into_iter()
        .filter(|(t, _)| (from..to).contains(t))
        .map(|(_, s)| s)
        .collect();
    let (lo, hi) = s
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    eprintln!("速度修正：{lo:.6} ~ {hi:.6}");
    assert!(s.len() == total && hi - lo < 1e-4, "速度修正在變：{lo} ~ {hi}");
    // 第 1 秒之後一直到截圖都不能等不到畫面。截圖之後關視窗時 mpv 等不到畫面是正常的：
    // mpv 要 200 ms 沒畫面才說，所以只多算到截圖後 0.05 秒（截圖前 0.15 秒以上開始的停頓都抓得到）
    let pos = shot_position(&r.stderr).expect("沒有截圖時的播放位置");
    let shot = t0 + pos + 0.05;
    let stuck: Vec<f64> = stuck(&r.log).into_iter().filter(|t| (from..shot).contains(t)).collect();
    assert!(stuck.is_empty(), "播放中 mpv 等不到畫面（截圖在 {shot:.3}）：{stuck:?}");
    assert!(
        r.stderr.contains("流暢播放（啟動）：Display"),
        "啟動時就決定依螢幕同步：\n{}",
        r.stderr
    );
}

#[test]
#[ignore = "會在螢幕上開全螢幕視窗（約 15 秒）；在開發機上手動跑"]
fn pacing_off_keeps_audio_sync() {
    // VITASCOPE_PACING=off：設定打開了也不動 mpv 的同步設定（跟以前一樣）
    let r = run("off", AUTO, &[("VITASCOPE_PACING", "off")]);
    assert_eq!(assumed_fps(&r.log), None, "不能依螢幕同步");
    assert!(vsyncs(&r.log).is_empty());
    assert!(!sets_sync_options(&r.log), "不能動 mpv 的同步設定");
    assert!(r.stderr.contains("Untouched"), "{}", r.stderr);
    // 預設設定（關）也一樣
    let r = run("default", "{}", &[]);
    assert_eq!(assumed_fps(&r.log), None, "不能依螢幕同步");
    assert!(vsyncs(&r.log).is_empty());
    assert!(!sets_sync_options(&r.log), "不能動 mpv 的同步設定");
    eprintln!("關閉時：沒有依螢幕同步");
}

#[test]
#[ignore = "會在螢幕上開全螢幕視窗（約 15 秒），還會縮到最小再還原；在開發機上手動跑"]
fn minimize_and_restore() {
    // 開始播放後第 3 秒縮到最小、第 6 秒還原：看不到時改用一般播放（聲音照常、位置照走），回來後恢復
    let r = run("minimize", AUTO, &[("VITASCOPE_TEST_MINIMIZE", "3,6")]);
    let hidden = r
        .stderr
        .find("流暢播放：Audio(Hidden)")
        .unwrap_or_else(|| panic!("縮小時沒有改用一般播放：\n{}", r.stderr));
    assert!(
        r.stderr[hidden..].contains("流暢播放：套用 [(\"display-fps-override\""),
        "還原後沒有恢復依螢幕同步：\n{}",
        r.stderr
    );
    let pos = shot_position(&r.stderr).expect("沒有截圖時的播放位置");
    eprintln!("截圖時的播放位置：{pos:.3} 秒（預期約 {SHOT_DELAY} 秒）");
    assert!((pos - SHOT_DELAY).abs() < 0.5, "播放位置 {pos} 跟經過的時間差太多");
    // 還原之後（等 0.5 秒再加 1 秒）的每格更新次數
    let samples = vsyncs(&r.log);
    let gap = samples
        .windows(2)
        .max_by(|a, b| (a[1].0 - a[0].0).total_cmp(&(b[1].0 - b[0].0)))
        .expect("沒有每格的同步記錄");
    assert!(
        gap[1].0 - gap[0].0 > 2.0,
        "縮小期間應該沒有依螢幕同步：最長的空檔 {gap:?}"
    );
    let t0 = samples[0].0;
    let (total, fives, hist) = cadence(&samples, gap[1].0 + 1.0, t0 + SHOT_DELAY - 1.0);
    eprintln!("還原後每格更新次數：{fives}/{total} 格是 5 次，分布 {hist:?}");
    assert!(total >= 50 && fives as f64 >= 0.995 * total as f64, "{hist:?}");
}

#[test]
fn log_parsing() {
    let log = "[   1.096][t][cplayer] s=1.001000 vsyncs=5 dur=0.041708 ratio=5.000000 err=0\n\
               [   0.722][v][vo/libmpv] Assuming 120.000000 FPS for display sync.\n\
               [   1.200][t][cplayer] s=1.001000 vsyncs=6 dur=0.041708\n\
               [   1.300][v][vo/libmpv] mpv_render_context_render() not being called or stuck.\n\
               not a log line\n";
    assert_eq!(vsyncs(log), vec![(1.096, 5), (1.2, 6)]);
    assert_eq!(assumed_fps(log), Some(120.0));
    assert_eq!(stuck(log), vec![1.3]);
    assert_eq!(speeds(log), vec![(1.096, 1.001), (1.2, 1.001)]);
    assert!(!sets_sync_options(log));
    assert!(sets_sync_options(
        "[   0.769][v][cplayer] Set property: video-sync=\"display-resample\" -> 1\n"
    ));
    assert_eq!(cadence(&vsyncs(log), 0.0, 2.0), (2, 1, vec![(5, 1), (6, 1)]));
    assert_eq!(shot_position("[vitascope] 截圖時的播放位置：11.982 秒\n"), Some(11.982));
}
