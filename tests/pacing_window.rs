//! 流暢播放的實機測試：真的開一個全螢幕視窗播 23.976 fps 的平移影片，從 mpv 的記錄算每格顯示幾次螢幕更新。
//! 會在螢幕上開視窗約 15 秒，所以預設不跑（#[ignore]），在有實體螢幕的開發機上手動跑：
//!
//! ```text
//! python scripts/gen_samples.py --tier pacing      # 1080p 與 4K 10-bit 的平移影片
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
    pacing_sample("pan_23976.mkv")
}

/// 4K 10-bit H.264：顯示卡不能硬體解碼，render 要上傳大貼圖，GPU 畫一格比較久
fn heavy_sample() -> PathBuf {
    pacing_sample("pan_4k10.mkv")
}

fn pacing_sample(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("samples/generated/pacing")
        .join(name);
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
    run_media(name, settings, env, &sample())
}

/// 同上，播 `media`
fn run_media(name: &str, settings: &str, env: &[(&str, &str)], media: &Path) -> Run {
    run_full(name, settings, env, media, SHOT_DELAY)
}

/// 同上，開始播放後 `delay` 秒截圖
fn run_full(name: &str, settings: &str, env: &[(&str, &str)], media: &Path, delay: f64) -> Run {
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
        .arg(media)
        .arg("--shot")
        .arg(dir.join("shot.png"))
        .arg("--shot-delay")
        .arg(delay.to_string())
        .env("APPDATA", dir.join("appdata"))
        .env("LOCALAPPDATA", dir.join("localappdata"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("HOME", &home)
        .env("VITASCOPE_MPV_OPTS", mpv_opts)
        .env("VITASCOPE_DEBUG", "pacing")
        .env_remove("VITASCOPE_PACING")
        .env_remove("VITASCOPE_TEST_MINIMIZE")
        .env_remove("VITASCOPE_TEST_BUSY_UI")
        .env_remove("VITASCOPE_TEST_STALL")
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

/// VITASCOPE_DEBUG=pacing 每 10 秒印一次的「畫面輸出統計：key=value …」（第一段：第一格畫出來 1 秒之後的 10 秒）
fn render_summary(stderr: &str) -> Option<Vec<(String, String)>> {
    let rest = stderr.lines().find_map(|l| l.split_once("畫面輸出統計："))?.1;
    Some(
        rest.split_whitespace()
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
    )
}

/// 統計裡的一個數字
fn stat(summary: &[(String, String)], key: &str) -> f64 {
    let v = &summary
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("統計裡沒有 {key}：{summary:?}"))
        .1;
    v.parse().unwrap_or_else(|_| panic!("{key}={v} 不是數字"))
}

/// 相鄰兩格隔幾次螢幕更新的分布（`vsyncs=5:230,6:2`；只有 Windows 量得到）：(更新次數, 格數)
fn present_vsyncs(summary: &[(String, String)]) -> Vec<(u32, usize)> {
    let raw = &summary.iter().find(|(k, _)| k == "vsyncs").expect("沒有 vsyncs").1;
    raw.split(',')
        .filter(|s| !s.is_empty() && *s != "-")
        .map(|item| {
            let (k, n) = item.split_once(':').expect("vsyncs 的格式");
            (k.parse().unwrap(), n.parse().unwrap())
        })
        .collect()
}

const AUTO: &str = r#"{ "smooth": "auto" }"#;

/// VITASCOPE_DEBUG=pacing 每秒一筆的 avsync（ms），只取 `after` 那一行之後的
fn avsync_after(stderr: &str, after: &str) -> Vec<f64> {
    let Some(start) = stderr.find(after) else {
        return Vec::new();
    };
    stderr[start..]
        .lines()
        .filter_map(|l| {
            l.split_once("[vitascope] avsync：")?
                .1
                .trim_end_matches(" ms")
                .trim()
                .parse()
                .ok()
        })
        .collect()
}

/// 開始播放後第 3 秒介面停 8 秒（VITASCOPE_TEST_STALL，像以前開著檔案對話框），18 秒截圖（影片 20 秒）
const STALL: &str = "3,8";
const STALL_SHOT: f64 = 18.0;

/// 介面停住之後的檢查：停住結束後約 2 秒起 avsync 都在 50 ms 內。回傳 mpv 記錄裡停住結束的時間
fn check_stall_recovery(name: &str, r: &Run) -> f64 {
    assert!(
        r.stderr.contains("測試：介面恢復"),
        "{name}：介面沒有停住（{}）\n{}",
        r.dir.display(),
        r.stderr
    );
    // 第 0 筆是停住剛結束時讀的（mpv 還沒處理），之後每秒一筆
    let avsync = avsync_after(&r.stderr, "測試：介面恢復");
    eprintln!("{name}：停住之後每秒的 avsync（ms）：{avsync:?}");
    assert!(avsync.len() >= 4, "{name}：avsync 的紀錄太少：{avsync:?}");
    // 最後一筆可能是截圖、關視窗時讀的，不算
    let settled = &avsync[2..avsync.len() - 1];
    assert!(
        settled.iter().all(|a| a.abs() < 50.0),
        "{name}：停住 2 秒之後影像跟聲音還差 50 ms 以上：{avsync:?}"
    );
    // mpv 的記錄：停住時每 200 ms 說一次等不到畫面，最長的那一串的最後一次 + 0.2 秒約是停住結束的時間
    //（關視窗時也會說幾次）
    let (len, end) = longest_stuck(&stuck(&r.log));
    assert!(len >= 10, "{name}：mpv 沒有記錄到停住");
    end + 0.2
}

/// mpv「等不到畫面」最長的一串（相鄰兩次隔不到 0.5 秒）：(次數, 最後一次的時間)
fn longest_stuck(times: &[f64]) -> (usize, f64) {
    let (mut best, mut run) = ((0, 0.0), (0, f64::NEG_INFINITY));
    for &t in times {
        run = if t - run.1 < 0.5 { (run.0 + 1, t) } else { (1, t) };
        if run.0 > best.0 {
            best = run;
        }
    }
    best
}

#[test]
#[ignore = "會在螢幕上開全螢幕視窗（約 20 秒，兩次），介面會停 8 秒；在開發機上手動跑"]
fn display_sync_recovers_after_a_ui_stall() {
    // 依螢幕同步中介面停住（以前開著檔案對話框、拖曳視窗）：停住時聲音照播、影像停住；恢復之後要馬上追上
    // （mpv 自己略過晚了的影格，追不上的話影戲暫時改用一般播放），之後回到每格剛好 5 次更新
    for (name, forced) in [("stall-display", false), ("stall-display-resync", true)] {
        let mut env = vec![("VITASCOPE_TEST_STALL", STALL)];
        if forced {
            // 不等 mpv 自己追，一律走「暫時改用一般播放」那條路（較舊的 mpv 追不上時才會走到）
            env.push(("VITASCOPE_PACING", "resync"));
        }
        let r = run_full(name, r#"{ "smooth": "always" }"#, &env, &sample(), STALL_SHOT);
        let end = check_stall_recovery(name, &r);
        assert!(
            r.stderr.contains("流暢播放：介面停了"),
            "{name}：沒有發現介面停住\n{}",
            r.stderr
        );
        if forced {
            assert!(
                r.stderr.contains("流暢播放：Audio(Resync)") && r.stderr.contains("流暢播放：重新同步花了"),
                "{name}：沒有暫時改用一般播放\n{}",
                r.stderr
            );
        } else {
            // 等完之後真的讀了 avsync（讀不到是「-」，會被當成已經追上，重新同步那條路就永遠走不到）
            let checked = r
                .stderr
                .lines()
                .find_map(|l| l.split_once("流暢播放：介面停頓之後 avsync ")?.1.split_once(" ms，"))
                .map(|(v, _)| v.trim().to_owned());
            assert!(
                checked.as_deref().is_some_and(|v| v.parse::<f64>().is_ok()),
                "{name}：停住之後沒有讀 avsync 檢查有沒有追上（{checked:?}）\n{}",
                r.stderr
            );
        }
        // 防呆不能把停住當成「跟不上」「沒等垂直同步」
        assert!(
            !r.stderr.contains("NoVsync") && !r.stderr.contains("TooSlow"),
            "{name}：{}",
            r.stderr
        );
        // 回到依螢幕同步：停住結束 2 秒之後到截圖前 1 秒，mpv 用的是螢幕的更新率、每格 5 次更新
        //（截圖要在介面上存 4K 的 PNG，除錯版會停住一下，之後的不算）
        let samples = vsyncs(&r.log);
        let to = samples.first().expect("沒有每格的同步記錄").0 + STALL_SHOT - 1.0;
        let fps: Vec<(f64, f64)> = log_lines(&r.log)
            .filter(|(t, l)| *t <= to && l.contains("FPS for display sync"))
            .filter_map(|(t, l)| Some((t, l.split("Assuming ").nth(1)?.split_whitespace().next()?.parse().ok()?)))
            .collect();
        eprintln!("{name}：mpv 用過的更新率 {fps:?}");
        let last = fps.last().expect("沒有依螢幕同步").1;
        assert!(
            (last - 120.0).abs() < 0.01 || (last - 119.88).abs() < 0.01,
            "{name}：最後用的更新率 {last}"
        );
        if forced {
            // 改用一般播放（0）再回來（螢幕的更新率）
            assert!(fps.len() >= 3, "{name}：{fps:?}");
        }
        let (total, fives, hist) = cadence(&samples, end + 2.0, to);
        eprintln!("{name}：停住結束 2 秒後每格更新次數：{fives}/{total} 格是 5 次，分布 {hist:?}");
        assert!(total >= 50 && fives as f64 >= 0.995 * total as f64, "{name}：{hist:?}");
    }
}

#[test]
#[ignore = "會在螢幕上開全螢幕視窗（約 20 秒），介面會停 8 秒；在開發機上手動跑"]
fn audio_mode_stays_in_sync_after_a_ui_stall() {
    // 一般播放（流暢播放關閉）：mpv 本來就會丟掉晚了的影格，停住之後也馬上追上；影戲不動同步設定
    let r = run_full(
        "stall-audio",
        "{}",
        &[("VITASCOPE_TEST_STALL", STALL)],
        &sample(),
        STALL_SHOT,
    );
    check_stall_recovery("stall-audio", &r);
    assert_eq!(assumed_fps(&r.log), None, "不能依螢幕同步");
    assert!(!sets_sync_options(&r.log), "不能動 mpv 的同步設定");
    assert!(!r.stderr.contains("流暢播放：介面停了"), "{}", r.stderr);
    let pos = shot_position(&r.stderr).expect("沒有截圖時的播放位置");
    assert!((pos - STALL_SHOT).abs() < 0.5, "播放位置 {pos} 跟經過的時間差太多");
}

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
    // 依螢幕同步時 mpv 要我們馬上畫（不延後）：render 不等
    let s = render_summary(&r.stderr).unwrap_or_else(|| panic!("沒有畫面輸出的統計\n{}", r.stderr));
    eprintln!(
        "render p50 {} ms、max {} ms、介面 {} 次/秒、延後 {} 次",
        stat(&s, "p50_ms"),
        stat(&s, "max_ms"),
        stat(&s, "passes_per_s"),
        stat(&s, "deferred")
    );
    assert!(stat(&s, "p50_ms") < 2.0, "{s:?}");
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

/// 一般播放一次量測的數字（VITASCOPE_DEBUG=pacing 的第一段統計）
struct Audio {
    s: Vec<(String, String)>,
    /// 影戲的記錄（stderr）
    stderr: String,
    /// 相鄰兩格剛好隔 5 次更新的比例（只有 Windows 量得到；其他平台是 NaN）
    fives: f64,
    /// 量到幾格的更新次數
    total: usize,
}

impl Audio {
    fn get(&self, key: &str) -> f64 {
        stat(&self.s, key)
    }
}

/// 一般播放比較的情況
#[derive(Clone, Copy, PartialEq)]
enum Cond {
    /// 介面閒著
    Idle,
    /// 介面一直重畫（VITASCOPE_TEST_BUSY_UI=1，像滑鼠在視窗上移動）
    Busy,
    /// 介面一直重畫，播 4K 10-bit（軟體解碼、上傳大貼圖，GPU 畫一格比較久：介面一直重畫時取影格到預定時間
    /// 最少只剩 2 ms，GPU 來不來得及）
    Heavy,
}

impl Cond {
    fn name(self) -> &'static str {
        match self {
            Cond::Idle => "idle",
            Cond::Busy => "busy",
            Cond::Heavy => "heavy",
        }
    }

    fn busy(self) -> bool {
        self != Cond::Idle
    }
}

/// 流暢播放關閉（一般播放、音訊同步）跑一次：`block` = VITASCOPE_PACING=block（以前的做法）
fn audio_run(block: bool, cond: Cond, nth: usize) -> Audio {
    let name = format!("audio-{}-{}-{nth}", cond.name(), if block { "block" } else { "pace" });
    let mut env = Vec::new();
    if block {
        env.push(("VITASCOPE_PACING", "block"));
    }
    if cond.busy() {
        env.push(("VITASCOPE_TEST_BUSY_UI", "1"));
    }
    let media = if cond == Cond::Heavy { heavy_sample() } else { sample() };
    let r = run_media(&name, "{}", &env, &media);
    assert_eq!(assumed_fps(&r.log), None, "{name}：不能依螢幕同步");
    let s = render_summary(&r.stderr).unwrap_or_else(|| panic!("{name}：沒有畫面輸出的統計\n{}", r.stderr));
    // avsync 是 mpv 排程影格時算的（player/video.c 的 update_av_diff），看不到我們什麼時候真的交出影格：
    // 只抓得到整個卡住、掉格之類的大問題；交出影格的時間靠 done_* 比
    assert!(stat(&s, "avsync_ms").abs() < 20.0, "{name}：影像跟聲音差太多 {s:?}");
    // 播放中 mpv 一直等得到畫面（第一格顯示 1 秒後到截圖）。4K 10-bit 要先試完硬體解碼才軟體解碼，
    // 第一格比「Starting playback」晚 1 秒多，等第一格時 mpv 也會記「not being called or stuck」
    let pos = shot_position(&r.stderr).expect("沒有截圖時的播放位置");
    let t0 = log_lines(&r.log)
        .find(|(_, l)| l.contains("first video frame after restart shown"))
        .or_else(|| log_lines(&r.log).find(|(_, l)| l.contains("Starting playback")))
        .map_or(0.0, |(t, _)| t);
    let stuck: Vec<f64> = stuck(&r.log)
        .into_iter()
        .filter(|t| (t0 + 1.0..t0 + pos + 0.05).contains(t))
        .collect();
    assert!(stuck.is_empty(), "{name}：播放中 mpv 等不到畫面：{stuck:?}");
    let vs = present_vsyncs(&s);
    let total: usize = vs.iter().map(|(_, n)| n).sum();
    let fives = vs.iter().find(|(k, _)| *k == 5).map_or(0, |(_, n)| *n);
    if cfg!(windows) {
        assert!(total >= 200, "{name}：格數太少 {vs:?}");
    }
    let a = Audio {
        fives: if total > 0 {
            fives as f64 / total as f64
        } else {
            f64::NAN
        },
        total,
        s,
        stderr: r.stderr,
    };
    eprintln!(
        "{name:<22} 5 次 {:>5.1}%（{fives}/{total}）交出 {:>6.2}/{:>6.2}/{:>6.2} ms  render {:>5.2}/{:>5.2} ms  \
         等待 {} 次 平均 {} ms 最久 {:>5.2} ms  取 {:>5.2}/{:>5.2}/{:>5.2} ms 前  介面 {:>5.1} 次/秒  每格 {:.2} 輪  \
         顯示 {} ms  GPU 畫完 {}/{} ms 提早 {:.2} ms  avsync {:>5.2} ms  {vs:?}",
        a.fives * 100.0,
        a.get("done_p5_ms"),
        a.get("done_p50_ms"),
        a.get("done_p95_ms"),
        a.get("p50_ms"),
        a.get("max_ms"),
        a.get("blocking"),
        stat_text(&a.s, "blocking_avg_ms"),
        a.get("blocking_max_ms"),
        a.get("take_p5_ms"),
        a.get("take_p50_ms"),
        a.get("take_p95_ms"),
        a.get("passes_per_s"),
        a.get("passes_per_frame"),
        stat_text(&a.s, "late_p50_ms"),
        stat_text(&a.s, "gpu_p50_ms"),
        stat_text(&a.s, "gpu_p95_ms"),
        a.get("gpu_lead_ms"),
        a.get("avsync_ms"),
    );
    a
}

/// 螢幕更新一次的時間（ms），不從影戲問：Windows 問 DWM，其他平台用影戲啟動時印的更新率
/// （量不到時當成 120 Hz，跟 `display_sync_gives_an_exact_cadence` 一樣假設開發機是 120 Hz 的螢幕）
fn refresh_period_ms(stderr: &str) -> f64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
        use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;
        // SAFETY: DWM_TIMING_INFO 是純資料，全 0 是合法的值；Windows 8.1 起 hwnd 要給 NULL
        let mut info: DWM_TIMING_INFO = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<DWM_TIMING_INFO>() as u32;
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq) };
        if unsafe { DwmGetCompositionTimingInfo(std::ptr::null_mut(), &mut info) } >= 0 && freq > 0 {
            return info.qpcRefreshPeriod as f64 * 1000.0 / freq as f64;
        }
    }
    let hz = stderr
        .split("Refresh { hz: ")
        .nth(1)
        .and_then(|r| r.split([',', ' ']).next()?.parse::<f64>().ok())
        .unwrap_or(120.0);
    1000.0 / hz
}

/// 統計裡的一個值（原樣的文字，量不到是 -）
fn stat_text<'a>(summary: &'a [(String, String)], key: &str) -> &'a str {
    &summary
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("統計裡沒有 {key}：{summary:?}"))
        .1
}

/// 幾次量測的平均
fn mean(runs: &[Audio], f: impl Fn(&Audio) -> f64) -> f64 {
    runs.iter().map(f).sum::<f64>() / runs.len() as f64
}

/// 一般播放（流暢播放關閉）時畫面輸出不卡住介面，影格交出、顯示的時間跟以前（`VITASCOPE_PACING=block`）一樣。
/// 介面閒著、一直重畫（`VITASCOPE_TEST_BUSY_UI=1`，像滑鼠在視窗上移動）、一直重畫又播 4K 10-bit（GPU 畫一格比較久）三種情況都比。
/// 10 秒的量測每次差幾個百分點（block 自己就在 83%～94% 之間），每種情況 block、pace 輪流各跑三次，比平均。
/// 每格顯示幾次更新、估計的顯示時間要問 DWM 垂直同步的時間，只在 Windows 檢查
#[test]
#[ignore = "會在螢幕上開全螢幕視窗（約 15 秒，18 次）；在開發機上手動跑"]
fn audio_mode_does_not_block_the_interface() {
    const RUNS: usize = 3;
    /// 取影格時多容許的量（`pacing::BLOCK_MARGIN_NS`）
    const MARGIN_MS: f64 = 2.0;
    let vsync_grid = cfg!(windows);
    for cond in [Cond::Idle, Cond::Busy, Cond::Heavy] {
        let (mut blocks, mut paces) = (Vec::new(), Vec::new());
        for nth in 0..RUNS {
            blocks.push(audio_run(true, cond, nth));
            paces.push(audio_run(false, cond, nth));
        }
        let cond_name = match cond {
            Cond::Idle => "介面閒著",
            Cond::Busy => "介面一直重畫",
            Cond::Heavy => "介面一直重畫、4K 10-bit",
        };
        let avg = |runs: &[Audio], key: &str| mean(runs, |a| a.get(key));
        let (b_fives, p_fives) = (mean(&blocks, |a| a.fives), mean(&paces, |a| a.fives));
        eprintln!(
            "{cond_name}：每格 5 次更新 block {:.1}% / pace {:.1}%；交出 p5 {:.2}/{:.2}、p95 {:.2}/{:.2} ms；\
             GPU 做完 p50 {:.2}/{:.2}、p95 {:.2}/{:.2} ms；avsync {:.2}/{:.2} ms；介面 {:.1}/{:.1} 次/秒",
            b_fives * 100.0,
            p_fives * 100.0,
            avg(&blocks, "done_p5_ms"),
            avg(&paces, "done_p5_ms"),
            avg(&blocks, "done_p95_ms"),
            avg(&paces, "done_p95_ms"),
            avg(&blocks, "gpu_p50_ms"),
            avg(&paces, "gpu_p50_ms"),
            avg(&blocks, "gpu_p95_ms"),
            avg(&paces, "gpu_p95_ms"),
            avg(&blocks, "avsync_ms"),
            avg(&paces, "avsync_ms"),
            avg(&blocks, "passes_per_s"),
            avg(&paces, "passes_per_s"),
        );
        for b in &blocks {
            // 以前的做法：每格在介面的執行緒上等 40 ms 左右，每次 render 都等
            assert!(b.get("p50_ms") > 20.0, "{cond_name}：基準（block）應該會等：{:?}", b.s);
            assert_eq!(
                b.get("blocking"),
                b.get("renders"),
                "{cond_name}：基準（block）每次都要等"
            );
            assert_eq!(b.get("deferred"), 0.0);
            assert_eq!(b.get("gpu_lead_ms"), 0.0, "{cond_name}：基準（block）不提早");
        }
        // 讓 render 等的範圍是螢幕更新一次的時間（120 Hz 是 8.33 ms）：跟另外問到的更新率比
        // （影戲沒把偵測到的更新率交給影片畫面的話會當成 60 Hz，等的時間變兩倍，其他數字看不出來）
        let window = paces[0].get("window_ms");
        let period = refresh_period_ms(&paces[0].stderr);
        assert!(
            (window - period).abs() < 0.05,
            "{cond_name}：讓 render 等的範圍 {window} ms 不是螢幕更新一次的時間 {period:.3} ms"
        );
        let refresh = 1000.0 / period;
        for p in &paces {
            assert!(p.get("deferred") > 0.0, "{cond_name}：沒有延後取影格 {:?}", p.s);
            // 取的時候照樣讓 render 等到預定時間，但最多等一次更新加上容許的 2 ms（GPU 來不及時再加上提早的量），
            // 再留 3 ms 給 render 本身（除錯版）。max、gpu_lead_ms 是這一段（第一格之後 1 秒起的 10 秒）裡最大的
            let bound = window + MARGIN_MS + p.get("gpu_lead_ms") + 3.0;
            assert!(
                p.get("max_ms") <= bound && p.get("blocking_max_ms") <= bound,
                "{cond_name}：render 等太久（上限 {bound} ms）：{:?}",
                p.s
            );
            assert!(p.get("blocking") > 0.0, "{cond_name}：{:?}", p.s);
        }
        // 交出影格的時間跟以前一樣：都是 mpv 到了預定時間才放行。最晚的 5% 不比以前晚超過 1 ms，
        // 最早的 5% 不比以前早超過 2 ms（早取的影格會早一次垂直同步顯示，avsync 看不出來）
        let (b95, p95) = (avg(&blocks, "done_p95_ms"), avg(&paces, "done_p95_ms"));
        assert!(
            p95 <= b95 + 1.0,
            "{cond_name}：交出影格比以前晚：{p95} ms（以前 {b95} ms）"
        );
        let (b5, p5) = (avg(&blocks, "done_p5_ms"), avg(&paces, "done_p5_ms"));
        assert!(p5 >= b5 - 2.0, "{cond_name}：交出影格比以前早：{p5} ms（以前 {b5} ms）");
        // 偶爾晚交出的那幾格（取影格時已經過了預定時間、render 畫太久）：最晚的 1% 也不比以前晚超過 1 ms
        let (b99, p99) = (avg(&blocks, "done_p99_ms"), avg(&paces, "done_p99_ms"));
        assert!(
            p99 <= b99 + 1.0,
            "{cond_name}：有幾格交出得比以前晚：{p99} ms（以前 {b99} ms）"
        );
        let avsync = avg(&paces, "avsync_ms") - avg(&blocks, "avsync_ms");
        assert!(avsync.abs() < 4.0, "{cond_name}：avsync 跟以前差 {avsync} ms");
        // GPU 畫完影格的時間（量不到的平台沒有）：以前 GPU 有 40 ms 可以畫，現在取影格之後只剩幾毫秒；
        // 晚畫完會晚一次垂直同步顯示（上面交出的時間看不出來）。1080p 跟以前差不到 0.1 ms。
        // 4K 10-bit（軟體解碼）不提早的話中位數、最晚的 5% 比以前晚 2.2、7 ms（GpuLate 拿掉就會抓到），
        // 最多提早 10 ms 時晚約 0.9、1.0 ms。留 1.5／2 ms（GPU 的時間每次跑差不少）
        if stat_text(&paces[0].s, "gpu_p50_ms") != "-" {
            for (key, slack) in [("gpu_p50_ms", 1.5), ("gpu_p95_ms", 2.0)] {
                let (b, p) = (avg(&blocks, key), avg(&paces, key));
                assert!(
                    p <= b + slack,
                    "{cond_name}：GPU 做完影格比以前晚：{key} {p} ms（以前 {b} ms）"
                );
            }
        }
        if vsync_grid {
            // 每格顯示幾次更新：23.976 fps 的影格時間每 8.3 秒跨過一次垂直同步，跨過的地方前後有 10～17 格 4／6 次
            // （交出時間差一點點就落在前後一次）；10 秒裡跨過一次或兩次要看開始的時間，所以一次 86% 一次 100%。
            // 實測三次平均的差（pace − block）在 −3.4～+4.6 個百分點之間（7 組，平均 +0.6、標準差約 3），
            // 留 3 個百分點會一成以上誤判：留 6 個（約 1%）。交出時間真的變晚的話上面的 done_*、下面的 late 會先抓到
            assert!(
                p_fives >= b_fives - 0.06,
                "{cond_name}：每格 5 次更新的比例變差：{p_fives}（block {b_fives}）"
            );
            let late = avg(&paces, "late_p50_ms") - avg(&blocks, "late_p50_ms");
            assert!(late.abs() < 2.0, "{cond_name}：顯示時間跟以前差 {late} ms");
            assert!(paces.iter().chain(&blocks).all(|a| a.total >= 200));
        }
        let (b_passes, p_passes) = (avg(&blocks, "passes_per_s"), avg(&paces, "passes_per_s"));
        if cond.busy() {
            // 介面一直重畫：以前每格在 render 等 40 ms，介面只剩影片的格率；現在跟著螢幕更新率
            assert!(
                p_passes >= 0.85 * refresh,
                "{cond_name}：介面每秒只畫 {p_passes} 次（螢幕 {refresh:.1} Hz）"
            );
            assert!(
                b_passes < 0.5 * refresh,
                "{cond_name}：基準（block）的介面應該只剩影片的格率：{b_passes} 次/秒"
            );
        } else {
            // 介面閒著：等影格時沒有一輪接一輪地空轉到時間，發現新影格之後到畫出來大約只多一輪（critique A3）；
            // egui 扣掉 predicted_dt 卻沒加回去的話是每次垂直同步都重畫（實測 119 次/秒、每格 3.9～4.0 輪）。
            // 實測 1.00～1.32：mpv 的事件（播放位置）跟新影格的通知誰先到每次跑不一樣，
            // 事件晚到的話同一格多看一輪；所以上限放到 1.5（還是遠低於空轉的 4）
            for p in &paces {
                assert!(
                    p.get("passes_per_frame") <= 1.5,
                    "{cond_name}：每格重繪太多次：{:?}",
                    p.s
                );
                assert!(
                    p.get("passes_per_s") < 0.85 * refresh,
                    "{cond_name}：介面一直在重畫：{:?}",
                    p.s
                );
                // 計時器叫醒的那一輪落在範圍中間（預定時間前約半次更新）：扣掉了計時器平均晚的量，
                // 最早取的 5% 也還在預定時間前 2 ms 以上（沒扣的話只剩 1.2～1.6 ms，render 本身就要約 1 ms）
                assert!(
                    p.get("take_p5_ms") >= MARGIN_MS,
                    "{cond_name}：計時器叫醒的那一輪太晚取影格：{:?}",
                    p.s
                );
            }
        }
    }
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
    let s = render_summary(
        "[vitascope] 流暢播放：Audio(Setting)\n\
         [vitascope] 影片畫面輸出：shader，視窗 MSAA 0x\n\
         [vitascope] 畫面輸出統計：secs=10.0 renders=240 p50_ms=0.41 vsyncs=4:1,5:230,6:2 late_p50_ms=-1.20\n",
    )
    .unwrap();
    assert_eq!(stat(&s, "renders"), 240.0);
    assert_eq!(stat(&s, "late_p50_ms"), -1.2);
    assert_eq!(present_vsyncs(&s), vec![(4, 1), (5, 230), (6, 2)]);
    assert_eq!(render_summary("沒有統計\n"), None);
    let stderr = "[vitascope] avsync：-1.50 ms\n\
                  [vitascope] 測試：介面恢復\n\
                  [vitascope] avsync：690.29 ms\n\
                  [vitascope] 流暢播放：介面停了 8.00 秒\n\
                  [vitascope] avsync：8.19 ms\n";
    assert_eq!(avsync_after(stderr, "測試：介面恢復"), vec![690.29, 8.19]);
    assert!(avsync_after(stderr, "沒有這行").is_empty());
    assert_eq!(longest_stuck(&[1.0, 4.0, 4.2, 4.4, 4.6, 9.0, 9.2]), (4, 4.6));
    assert_eq!(longest_stuck(&[]), (0, 0.0));
}
