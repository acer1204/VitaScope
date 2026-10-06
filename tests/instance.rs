//! 單一執行個體：真的開好幾個程式（這個測試程式自己當子程式），檢查只有一個主視窗、其他的把檔名送過去。
//! 不需要視窗，三個平台都能跑。

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vitascope::instance::{self, Endpoint, Request, Startup};

/// 子程式的參數：「資料夾\n字尾\n檔名\n等多久」
const CHILD: &str = "VITASCOPE_INSTANCE_TEST_CHILD";

/// 平常是空的測試；被下面的測試當成子程式啟動時才做事
#[test]
fn child() {
    let Ok(spec) = std::env::var(CHILD) else { return };
    let mut it = spec.lines();
    let (dir, suffix, file, hold) = (
        it.next().unwrap(),
        it.next().unwrap(),
        it.next().unwrap(),
        it.next().unwrap().parse::<u64>().unwrap(),
    );
    let ep = Endpoint::in_dir(dir.into(), suffix).unwrap();
    let req = Request {
        paths: vec![file.into()],
        fullscreen: false,
    };
    match instance::start(&ep, &req, true, Arc::new(|| {})) {
        Startup::Primary(mut p) => {
            // 讓測試知道已經是主視窗了（不用猜要等多久）
            println!("READY");
            let mut got = vec![file.to_owned()];
            let end = Instant::now() + Duration::from_millis(hold);
            while let Ok(r) = p.rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                got.extend(r.paths.iter().map(|p| p.display().to_string()));
            }
            p.shutdown();
            println!("PRIMARY {}", got.join("|"));
        }
        Startup::Forwarded => println!("FORWARDED"),
        Startup::Standalone(why) => println!("STANDALONE {why}"),
    }
}

fn spawn_child(dir: &std::path::Path, suffix: &str, file: &str, hold_ms: u64) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(CHILD, format!("{}\n{suffix}\n{file}\n{hold_ms}", dir.display()))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn output(child: std::process::Child) -> String {
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
}

/// 等子程式說它已經是主視窗；回傳讀到的輸出（之後的輸出用 `rest` 讀）
fn wait_ready(child: &mut std::process::Child) -> BufReader<std::process::ChildStdout> {
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "子程式沒有成為主視窗就結束了");
        if line.contains("READY") {
            return out;
        }
    }
}

fn rest(mut out: BufReader<std::process::ChildStdout>, mut child: std::process::Child) -> String {
    let mut s = String::new();
    out.read_to_string(&mut s).unwrap();
    child.wait().unwrap();
    s
}

fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("vts-{name}-{}", std::process::id()))
}

#[test]
fn five_launches_at_once_end_up_in_one_instance() {
    let dir = temp_dir("five");
    let suffix = format!("-five{}", std::process::id());
    let kids: Vec<_> = (0..5)
        .map(|i| spawn_child(&dir, &suffix, &format!("影片 {i}.mkv"), 2000))
        .collect();
    let outs: Vec<String> = kids.into_iter().map(output).collect();
    let primary: Vec<&str> = outs
        .iter()
        // 輸出的那一行前面還有測試框架的「test child ... 」
        .filter_map(|o| o.lines().find_map(|l| l.split_once("PRIMARY ").map(|(_, rest)| rest)))
        .collect();
    assert_eq!(primary.len(), 1, "只有一個主視窗：{outs:#?}");
    let mut got: Vec<&str> = primary[0].split('|').collect();
    got.sort();
    assert_eq!(
        got,
        ["影片 0.mkv", "影片 1.mkv", "影片 2.mkv", "影片 3.mkv", "影片 4.mkv"],
        "{outs:#?}"
    );
    assert_eq!(outs.iter().filter(|o| o.contains("FORWARDED")).count(), 4, "{outs:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn after_the_primary_closes_the_next_launch_takes_over() {
    let dir = temp_dir("takeover");
    let suffix = format!("-takeover{}", std::process::id());
    let first = output(spawn_child(&dir, &suffix, "a.mkv", 200));
    assert!(first.contains("PRIMARY a.mkv"), "{first}");
    // 主視窗結束後（鎖放開了）下一個自己當主視窗，不會卡著等
    let start = Instant::now();
    let second = output(spawn_child(&dir, &suffix, "b.mkv", 200));
    assert!(second.contains("PRIMARY b.mkv"), "{second}");
    assert!(start.elapsed() < Duration::from_secs(10));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multiple_windows_allowed_opens_its_own() {
    let dir = temp_dir("multi");
    let suffix = format!("-multi{}", std::process::id());
    let mut primary = spawn_child(&dir, &suffix, "a.mkv", 1500);
    let out = wait_ready(&mut primary);
    let ep = Endpoint::in_dir(dir.clone(), &suffix).unwrap();
    let req = Request {
        paths: vec!["b.mkv".into()],
        fullscreen: false,
    };
    // 設定允許多個視窗：不送過去，自己開
    let started = instance::start(&ep, &req, false, Arc::new(|| {}));
    assert!(matches!(started, Startup::Standalone(_)));
    let out = rest(out, primary);
    assert!(out.contains("PRIMARY a.mkv") && !out.contains("b.mkv"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_crashed_primary_is_replaced_right_away() {
    let dir = temp_dir("crash");
    let suffix = format!("-crash{}", std::process::id());
    let mut primary = spawn_child(&dir, &suffix, "a.mkv", 30_000);
    let _out = wait_ready(&mut primary);
    // 當掉（沒有正常關閉，Unix 上會留下舊的 socket 檔）
    primary.kill().unwrap();
    primary.wait().unwrap();
    let ep = Endpoint::in_dir(dir.clone(), &suffix).unwrap();
    let start = Instant::now();
    let req = Request {
        paths: vec!["b.mkv".into()],
        fullscreen: false,
    };
    let started = instance::start(&ep, &req, true, Arc::new(|| {}));
    assert!(matches!(started, Startup::Primary(_)), "接手當主視窗");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "不用等逾時：{:?}",
        start.elapsed()
    );
    drop(started);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_launch_while_the_primary_is_closing_takes_over() {
    let dir = temp_dir("closing");
    let suffix = format!("-closing{}", std::process::id());
    let ep = Endpoint::in_dir(dir.clone(), &suffix).unwrap();
    let Startup::Primary(mut primary) = instance::start(&ep, &Request::default(), true, Arc::new(|| {})) else {
        panic!("應該是主視窗");
    };
    // 主視窗開始關閉（還在存設定、還沒放開鎖）：送來的檔案不收（收了也不會開）
    primary.stop_accepting();
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        primary.shutdown();
    });
    let req = Request {
        paths: vec!["b.mkv".into()],
        fullscreen: false,
    };
    let started = instance::start(&ep, &req, true, Arc::new(|| {}));
    closer.join().unwrap();
    assert!(
        matches!(started, Startup::Primary(_)),
        "等舊的放開鎖之後自己當主視窗，檔案自己開"
    );
    drop(started);
    let _ = std::fs::remove_dir_all(&dir);
}
