//! 網站影片：執行 yt-dlp 的部分（不連網：用假的 yt-dlp）。
//!
//! 假的 yt-dlp 把收到的參數、環境記到檔案，照指定的方式回應（印出 JSON、錯誤、一直不結束、開子程序…）：
//! - Unix：一個 `#!/bin/sh` 腳本。
//! - Windows：一個 Python 腳本（`Located { program: python, prefix_args: [腳本] }`；PowerShell 會重新解析參數，
//!   沒辦法確認參數一字不差）。CI 有 Python（產生測試樣本用）；找不到 Python 時這些測試說明原因後略過。
//!
//! 真的 yt-dlp（連到 YouTube）的測試要另外設定 `VITASCOPE_ONLINE_TESTS=1` 才會跑（`--ignored`）。

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use vitascope::net::NetSettings;
use vitascope::ytdl::locate::{self, Located, Locator, SearchEnv, Source, Tools};
use vitascope::ytdl::run::{self, ChildEnv, Limits, Lock, RunError};
use vitascope::ytdl::{self, Hint, ProcessResolver, Request, Resolve, SitePrefs, YtdlError};

const YOUTUBE_JSON: &str = include_str!("fixtures/ytdl/youtube_like.json");

/// Unix 的假 yt-dlp：參數 = 模式、工作資料夾，後面才是 yt-dlp 的參數
#[cfg(unix)]
const FAKE_SH: &str = r#"#!/bin/sh
mode="$1"; dir="$2"; shift 2
: > "$dir/argv.bin"
for a in "$@"; do printf '%s\0' "$a" >> "$dir/argv.bin"; done
{
  printf 'PATH=%s\n' "$PATH"
  printf 'NO_COLOR=%s\n' "$NO_COLOR"
  printf 'PYTHONUTF8=%s\n' "$PYTHONUTF8"
  printf 'PYTHONIOENCODING=%s\n' "$PYTHONIOENCODING"
  printf 'CWD=%s\n' "$(pwd)"
} > "$dir/env.txt"
case "$mode" in
  out)
    [ -f "$dir/stdout.txt" ] && cat "$dir/stdout.txt"
    # 跟真的 yt-dlp 一樣：--no-warnings 時不印警告
    if [ -f "$dir/stderr.txt" ]; then
      case " $* " in
        *" --no-warnings "*) grep -v '^WARNING:' "$dir/stderr.txt" >&2;;
        *) cat "$dir/stderr.txt" >&2;;
      esac
    fi
    code=0; [ -f "$dir/exit.txt" ] && code=$(cat "$dir/exit.txt")
    exit "$code";;
  hang)
    ( while :; do echo x >> "$dir/hb"; sleep 0.1; done ) &
    sleep 60;;
  stubborn)
    trap '' TERM
    ( while :; do echo x >> "$dir/hb"; sleep 0.1; done ) &
    while :; do sleep 0.1; done;;
  big)
    head -c 3000000 /dev/zero | tr '\0' a;;
  leave)
    # 印完 JSON 就結束，但留下一個還開著輸出管線的子程序
    cat "$dir/stdout.txt"
    ( while :; do echo x >> "$dir/hb"; sleep 0.1; done ) &
    exit 0;;
  version)
    echo 2026.08.19;;
esac
"#;

/// Windows 的假 yt-dlp（同樣的模式）
#[cfg(windows)]
const FAKE_PY: &str = r#"import os, subprocess, sys, time
mode, d = sys.argv[1], sys.argv[2]
if mode == "beat":
    end = time.time() + 60
    while time.time() < end:
        with open(os.path.join(d, "hb"), "a") as f:
            f.write("x\n")
        time.sleep(0.1)
    sys.exit(0)
with open(os.path.join(d, "argv.bin"), "wb") as f:
    for a in sys.argv[3:]:
        f.write(a.encode("utf-8") + b"\0")
with open(os.path.join(d, "env.txt"), "w", encoding="utf-8") as f:
    for k in ["PATH", "NO_COLOR", "PYTHONUTF8", "PYTHONIOENCODING"]:
        f.write(k + "=" + os.environ.get(k, "") + "\n")
    f.write("CWD=" + os.getcwd() + "\n")
def cat(name, out):
    p = os.path.join(d, name)
    if os.path.exists(p):
        with open(p, "rb") as f:
            out.buffer.write(f.read())
        out.flush()
if mode == "out":
    cat("stdout.txt", sys.stdout)
    p = os.path.join(d, "stderr.txt")
    if os.path.exists(p):
        # 跟真的 yt-dlp 一樣：--no-warnings 時不印警告
        with open(p, "rb") as f:
            lines = f.read().splitlines(keepends=True)
        if "--no-warnings" in sys.argv[3:]:
            lines = [l for l in lines if not l.startswith(b"WARNING:")]
        sys.stderr.buffer.write(b"".join(lines))
        sys.stderr.flush()
    p = os.path.join(d, "exit.txt")
    sys.exit(int(open(p).read()) if os.path.exists(p) else 0)
elif mode == "hang":
    subprocess.Popen([sys.executable, __file__, "beat", d])
    time.sleep(60)
elif mode == "leave":
    # 印完 JSON 就結束，但留下一個還開著輸出管線的子程序（明確繼承 stdout、stderr）
    cat("stdout.txt", sys.stdout)
    subprocess.Popen([sys.executable, __file__, "beat", d], stdout=sys.stdout, stderr=sys.stderr)
    sys.exit(0)
elif mode == "big":
    sys.stdout.write("a" * 3000000)
    sys.stdout.flush()
elif mode == "version":
    print("2026.08.19")
"#;

/// 一個假 yt-dlp 的工作資料夾
struct Fake {
    dir: PathBuf,
    program: PathBuf,
    prefix: Vec<OsString>,
}

/// Windows 上執行假 yt-dlp 用的 Python（完整路徑）；`VITASCOPE_TEST_PYTHON` 可以指定
#[cfg(windows)]
fn python() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("VITASCOPE_TEST_PYTHON") {
        return Some(PathBuf::from(p));
    }
    ["python", "python3", "py"].into_iter().find_map(|name| {
        let out = std::process::Command::new(name)
            .args(["-c", "import sys; print(sys.executable)"])
            .output()
            .ok()?;
        let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        (out.status.success() && path.is_absolute()).then_some(path)
    })
}

impl Fake {
    /// 找不到 Python（只有 Windows 需要）時 None
    fn new(name: &str) -> Option<Fake> {
        let dir = std::env::temp_dir().join(format!("vitascope-fake-ytdl-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = dir.join("yt-dlp");
            std::fs::write(&script, FAKE_SH).unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            Some(Fake {
                dir,
                program: script,
                prefix: Vec::new(),
            })
        }
        #[cfg(windows)]
        {
            let Some(py) = python() else {
                // CI 一定有 Python（setup-python）：找不到時要失敗，不能全部「通過」卻什麼都沒測
                assert!(
                    std::env::var_os("CI").is_none(),
                    "CI 上找不到 Python：執行 yt-dlp 的測試需要它（可以用 VITASCOPE_TEST_PYTHON 指定）"
                );
                eprintln!("找不到 Python，略過這個測試（可以用 VITASCOPE_TEST_PYTHON 指定）");
                return None;
            };
            let script = dir.join("fake_ytdl.py");
            std::fs::write(&script, FAKE_PY).unwrap();
            Some(Fake {
                dir,
                program: py,
                prefix: vec![script.into()],
            })
        }
    }

    fn located(&self, mode: &str) -> Located {
        let mut prefix = self.prefix.clone();
        prefix.push(mode.into());
        prefix.push(self.dir.clone().into());
        Located {
            program: self.program.clone(),
            prefix_args: prefix,
            source: Source::System,
        }
    }

    fn write(&self, name: &str, data: &str) {
        std::fs::write(self.dir.join(name), data).unwrap();
    }

    /// 假 yt-dlp 收到的參數
    fn argv(&self) -> Vec<String> {
        let data = std::fs::read(self.dir.join("argv.bin")).expect("假 yt-dlp 沒有執行");
        data.split(|b| *b == 0)
            .map(|a| String::from_utf8(a.to_vec()).unwrap())
            .collect::<Vec<_>>()
            .split_last()
            .map(|(_, rest)| rest.to_vec())
            .unwrap_or_default()
    }

    /// 假 yt-dlp 看到的環境變數
    fn env(&self, key: &str) -> String {
        let text = std::fs::read_to_string(self.dir.join("env.txt")).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .unwrap_or_default()
            .to_owned()
    }

    fn heartbeat(&self) -> u64 {
        std::fs::metadata(self.dir.join("hb")).map(|m| m.len()).unwrap_or(0)
    }

    /// 等到子程序開始寫心跳
    fn wait_heartbeat(&self) {
        let until = Instant::now() + Duration::from_secs(30);
        while self.heartbeat() == 0 {
            assert!(Instant::now() < until, "假 yt-dlp 的子程序沒有開始");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// 心跳停了（1 秒內沒有變）：程序都結束了。等不到就失敗
    fn assert_tree_ends(&self, within: Duration) {
        let until = Instant::now() + within;
        loop {
            let before = self.heartbeat();
            std::thread::sleep(Duration::from_secs(1));
            if self.heartbeat() == before {
                return;
            }
            assert!(Instant::now() < until, "取消後子程序還在跑（心跳一直增加）");
        }
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn limits() -> Limits {
    Limits {
        deadline: Duration::from_secs(60),
        ..Limits::default()
    }
}

fn no_env() -> ChildEnv {
    ChildEnv::default()
}

/// 這個資料夾放在最前面，接著原本的 PATH（假的 yt-dlp 還要用 cat 之類的程式）
fn path_with(first: PathBuf) -> OsString {
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(first).chain(std::env::split_paths(&inherited))).unwrap()
}

fn request(url: &str) -> Request {
    Request::new(url, &NetSettings::default(), &SitePrefs::default()).unwrap()
}

#[test]
fn arguments_and_environment_reach_the_program_exactly() {
    let Some(fake) = Fake::new("argv") else { return };
    // 空白、引號、反斜線（結尾的也是）、shell 與 cmd 的特殊字元、中文：不經過 shell，原樣送到
    let tricky = [
        "--",
        "a b",
        "q\"uote",
        r"back\slash\",
        r#"\"x\\"#,
        "&|<>^%PATH%$HOME;`echo`",
        "影戲 ü 日本",
        "https://h.test/watch?v=a&list=b",
    ];
    let args: Vec<OsString> = tricky.iter().map(OsString::from).collect();
    let cwd = fake.dir.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let env = ChildEnv {
        path: Some(path_with(fake.dir.join("first"))),
        cwd: Some(cwd.clone()),
    };
    let never = AtomicBool::new(false);
    let out = run::run(&fake.located("out"), &args, &env, &limits(), Lock::Shared, &never).unwrap();
    assert!(out.success);
    assert_eq!(fake.argv(), tricky);
    assert_eq!(fake.env("NO_COLOR"), "1");
    assert_eq!(fake.env("PYTHONUTF8"), "1");
    assert_eq!(fake.env("PYTHONIOENCODING"), "utf-8");
    let first = std::env::split_paths(&OsString::from(fake.env("PATH"))).next().unwrap();
    assert_eq!(first, fake.dir.join("first"));
    let seen_cwd = PathBuf::from(fake.env("CWD"));
    assert_eq!(seen_cwd.canonicalize().unwrap(), cwd.canonicalize().unwrap());
}

#[test]
fn resolve_reads_json_and_warnings() {
    let Some(fake) = Fake::new("json") else { return };
    fake.write("stdout.txt", YOUTUBE_JSON);
    fake.write(
        "stderr.txt",
        "[youtube] Extracting URL\nWARNING: [youtube] No supported JavaScript runtime could be found. Only deno is \
         enabled by default; some formats may be missing\n",
    );
    let req = request("https://www.youtube.com/watch?v=BaW_jenozKc");
    let never = AtomicBool::new(false);
    let got = ytdl::resolve_with(&fake.located("out"), &no_env(), &req, &limits(), &never).unwrap();
    assert_eq!(got.info.id.as_deref(), Some("BaW_jenozKc"));
    assert_eq!(got.info.requested_formats.len(), 2);
    assert_eq!(got.hints, vec![Hint::NeedsJsRuntime]);
    // 收到的參數就是命令列產生的那一份，網址在 `--` 後面
    let argv = fake.argv();
    assert_eq!(argv, ytdl::args(&req));
    assert_eq!(
        &argv[argv.len() - 2..],
        ["--", "https://www.youtube.com/watch?v=BaW_jenozKc"]
    );
}

#[test]
fn failures_become_reasons() {
    let Some(fake) = Fake::new("fail") else { return };
    let req = request("https://www.youtube.com/watch?v=gone");
    let never = AtomicBool::new(false);
    let resolve = || ytdl::resolve_with(&fake.located("out"), &no_env(), &req, &limits(), &never).map(|_| ());

    fake.write(
        "stderr.txt",
        "ERROR: [youtube] gone: Video unavailable. This video has been removed\n",
    );
    fake.write("exit.txt", "1");
    assert_eq!(resolve(), Err(YtdlError::Unavailable.into()));

    // 失敗時警告裡的提醒也要留著：沒有 deno 時錯誤看起來像「yt-dlp 太舊」，警告才說出真正的原因
    fake.write(
        "stderr.txt",
        "WARNING: [youtube] No supported JavaScript runtime could be found. Only deno is enabled by default; \
         some formats may be missing\nERROR: [youtube] gone: Requested format is not available. Use --list-formats \
         for a list of available formats\n",
    );
    let failure = resolve().unwrap_err();
    assert_eq!(failure.error, YtdlError::Outdated);
    assert_eq!(failure.hints, vec![Hint::NeedsJsRuntime]);
    assert_eq!(failure.remedy(), Some(ytdl::Remedy::GetDeno));

    // 成功結束但輸出不是 JSON（設定檔加了 --print 之類）
    std::fs::remove_file(fake.dir.join("stderr.txt")).unwrap();
    fake.write("exit.txt", "0");
    fake.write("stdout.txt", "just text\n");
    assert_eq!(resolve(), Err(YtdlError::NotJson.into()));
    // 什麼都沒印
    fake.write("stdout.txt", "");
    assert_eq!(resolve(), Err(YtdlError::NoResponse.into()));

    // 找不到程式
    let missing = Located::new(fake.dir.join("no-such-yt-dlp"), Source::UserPath);
    assert_eq!(
        ytdl::resolve_with(&missing, &no_env(), &req, &limits(), &never).map(|_| ()),
        Err(YtdlError::Missing.into())
    );
}

#[test]
fn leftover_children_do_not_hold_the_output_open() {
    // yt-dlp 結束了，但它開的子程序還開著輸出管線：要結束那個子程序、馬上拿到輸出（不是等 5 秒後當成沒有回應）
    let Some(fake) = Fake::new("leave") else { return };
    fake.write("stdout.txt", YOUTUBE_JSON);
    let req = request("https://www.youtube.com/watch?v=BaW_jenozKc");
    let never = AtomicBool::new(false);
    let got = ytdl::resolve_with(&fake.located("leave"), &no_env(), &req, &limits(), &never);
    assert_eq!(got.map(|r| r.info.id).unwrap(), Some("BaW_jenozKc".to_owned()));
    fake.assert_tree_ends(Duration::from_secs(20));
}

#[test]
fn huge_output_is_capped_without_hanging() {
    let Some(fake) = Fake::new("big") else { return };
    let never = AtomicBool::new(false);
    let lim = Limits {
        stdout_cap: 1 << 20,
        ..limits()
    };
    let out = run::run(&fake.located("big"), &[], &no_env(), &lim, Lock::Shared, &never).unwrap();
    assert!(out.success);
    assert_eq!(out.stdout.len(), 1 << 20);
    assert!(out.stdout_truncated);
}

#[test]
fn cancel_returns_at_once_and_ends_the_whole_process_tree() {
    let Some(fake) = Fake::new("cancel") else { return };
    let cancel = Arc::new(AtomicBool::new(false));
    // Windows：取消是「放手」，3 秒後才整個 Job 結束；Unix：馬上 SIGTERM，1 秒後 SIGKILL
    let lim = Limits {
        abandon_grace: Duration::from_secs(3),
        term_grace: Duration::from_secs(1),
        ..limits()
    };
    let (located, c) = (fake.located("hang"), cancel.clone());
    let worker = std::thread::spawn(move || {
        let r = run::run(&located, &[], &ChildEnv::default(), &lim, Lock::Shared, &c);
        (r, Instant::now())
    });
    fake.wait_heartbeat();
    let cancelled_at = Instant::now();
    cancel.store(true, Ordering::Relaxed);
    let (result, returned_at) = worker.join().unwrap();
    assert!(matches!(result, Err(RunError::Cancelled)), "{result:?}");
    assert!(
        returned_at - cancelled_at < Duration::from_secs(2),
        "取消要馬上回來，不等程序結束"
    );
    if cfg!(windows) {
        // 放手：程序還在跑（不留下 PyInstaller 的暫存資料夾）
        let before = fake.heartbeat();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(fake.heartbeat() > before, "Windows 的取消是放手，不馬上結束");
    }
    // 寬限時間過後，連孫程序（心跳）一起結束
    fake.assert_tree_ends(Duration::from_secs(20));
}

#[test]
fn deadline_gives_up_and_ends_the_tree() {
    let Some(fake) = Fake::new("deadline") else { return };
    // 等待時間給長一點：慢的 CI 上孫程序（心跳）也要在逾時前開始，不然「整個結束」什麼都沒證明
    let lim = Limits {
        deadline: Duration::from_secs(8),
        abandon_grace: Duration::from_millis(500),
        term_grace: Duration::from_millis(500),
        ..Limits::default()
    };
    let never = AtomicBool::new(false);
    let started = Instant::now();
    let r = run::run(&fake.located("hang"), &[], &no_env(), &lim, Lock::Shared, &never);
    assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(15));
    assert!(fake.heartbeat() > 0, "逾時前孫程序沒有開始，這個測試沒有意義");
    fake.assert_tree_ends(Duration::from_secs(20));
    // 解析的時候逾時：yt-dlp 沒有回應
    std::fs::remove_file(fake.dir.join("hb")).unwrap();
    let req = request("https://www.youtube.com/watch?v=slow");
    assert_eq!(
        ytdl::resolve_with(&fake.located("hang"), &no_env(), &req, &lim, &never).map(|_| ()),
        Err(YtdlError::NoResponse.into())
    );
    assert!(fake.heartbeat() > 0, "逾時前孫程序沒有開始，這個測試沒有意義");
    fake.assert_tree_ends(Duration::from_secs(20));
}

#[test]
fn abandoned_processes_end_at_the_hard_limit() {
    // Windows：放手後的寬限時間很長，但從啟動算起的上限先到 → 到上限就整個 Job 結束
    // （Unix 是 SIGTERM 後 term_grace 就 SIGKILL，本來就不會跑到上限）
    let Some(fake) = Fake::new("hard") else { return };
    let lim = Limits {
        deadline: Duration::from_secs(8),
        abandon_grace: Duration::from_secs(300),
        hard_limit: Duration::from_secs(10),
        term_grace: Duration::from_millis(500),
        ..Limits::default()
    };
    let never = AtomicBool::new(false);
    let r = run::run(&fake.located("hang"), &[], &no_env(), &lim, Lock::Shared, &never);
    assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
    assert!(fake.heartbeat() > 0, "逾時前孫程序沒有開始，這個測試沒有意義");
    // 上限是啟動後 10 秒（逾時後約 2 秒）；300 秒的寬限時間不算
    fake.assert_tree_ends(Duration::from_secs(30));
}

#[cfg(unix)]
#[test]
fn processes_that_ignore_sigterm_get_sigkill() {
    let Some(fake) = Fake::new("stubborn") else { return };
    let cancel = Arc::new(AtomicBool::new(false));
    let lim = Limits {
        term_grace: Duration::from_millis(500),
        ..limits()
    };
    let (located, c) = (fake.located("stubborn"), cancel.clone());
    let worker = std::thread::spawn(move || run::run(&located, &[], &ChildEnv::default(), &lim, Lock::Shared, &c));
    fake.wait_heartbeat();
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(worker.join().unwrap(), Err(RunError::Cancelled)));
    fake.assert_tree_ends(Duration::from_secs(20));
}

/// Unix：每個測試的假 yt-dlp 是不同的檔案，各自一把工具鎖（Windows 的假 yt-dlp 都是同一個 python.exe，
/// 平行的測試共用它的鎖，沒辦法檢查計數）
#[cfg(unix)]
#[test]
fn runs_hold_the_tool_lock_and_abandoned_ones_keep_updates_waiting() {
    let Some(fake) = Fake::new("lock") else { return };
    let lock = run::tool_lock(&fake.program);
    let cancel = Arc::new(AtomicBool::new(false));
    // 不理會 SIGTERM 的程序：放手後 3 秒才 SIGKILL，這段時間它還算「放手的程序」
    let lim = Limits {
        term_grace: Duration::from_secs(3),
        ..limits()
    };
    let (located, c) = (fake.located("stubborn"), cancel.clone());
    let worker = std::thread::spawn(move || run::run(&located, &[], &ChildEnv::default(), &lim, Lock::Shared, &c));
    fake.wait_heartbeat();
    // 執行中：拿著共用鎖，不能更新
    assert_eq!(
        lock.state(),
        run::LockState {
            active: 1,
            abandoned: 0,
            exclusive: false
        }
    );
    assert!(lock.exclusive(Duration::from_millis(100)).is_none());
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(worker.join().unwrap(), Err(RunError::Cancelled)));
    // 取消後馬上回來，但程序還沒結束：算放手的程序，更新還要等它
    assert_eq!(
        lock.state(),
        run::LockState {
            active: 0,
            abandoned: 1,
            exclusive: false
        }
    );
    assert!(
        lock.exclusive(Duration::from_millis(100)).is_none(),
        "放手的程序還在，不能更新"
    );
    fake.assert_tree_ends(Duration::from_secs(20));
    let update = lock.exclusive(Duration::from_secs(10));
    assert!(update.is_some(), "放手的程序結束後就能更新");
    drop(update);
    assert_eq!(lock.state(), run::LockState::default());
    // 正常結束：鎖放開
    let never = AtomicBool::new(false);
    run::run(
        &fake.located("version"),
        &[],
        &no_env(),
        &limits(),
        Lock::Shared,
        &never,
    )
    .unwrap();
    assert_eq!(lock.state(), run::LockState::default());
}

#[test]
fn version_probe_runs_once_per_file() {
    let Some(fake) = Fake::new("version") else { return };
    let located = fake.located("version");
    let out = locate::probe_version(&located, &no_env()).unwrap();
    assert_eq!(ytdl::Version::parse(&out), ytdl::Version::parse("2026.08.19"));
    assert_eq!(fake.argv(), ["--version"]);
    // 檔案沒變：不再執行（記住的結果）
    std::fs::remove_file(fake.dir.join("argv.bin")).unwrap();
    assert_eq!(locate::probe_version(&located, &no_env()).unwrap(), out);
    assert!(!fake.dir.join("argv.bin").exists(), "版本要記住，不重複執行");
    // Unix：檔案換了（更新之後）→ 重新執行。Windows 的程式是 python.exe，換不了
    #[cfg(unix)]
    {
        let script = std::fs::read_to_string(&fake.program).unwrap();
        let updated = script.replace("echo 2026.08.19;;", "echo 2026.09.01.1;;");
        assert_ne!(script, updated);
        std::fs::write(&fake.program, updated).unwrap();
        let out = locate::probe_version(&located, &no_env()).unwrap();
        assert_eq!(out.trim(), "2026.09.01.1");
        assert!(fake.dir.join("argv.bin").exists(), "檔案換了要重新查版本");
    }
}

#[test]
fn process_resolver_uses_the_located_tools() {
    let Some(fake) = Fake::new("resolver") else { return };
    fake.write("stdout.txt", YOUTUBE_JSON);
    let located = fake.located("out");
    let env = ChildEnv {
        path: Some(path_with(fake.dir.join("deno-dir"))),
        cwd: Some(fake.dir.clone()),
    };
    let tools = Tools {
        ytdl: Some(located),
        ytdl_version: None,
        ytdl_error: None,
        deno: None,
        deno_too_old: None,
        env,
    };
    let finder_tools = tools.clone();
    let loc = Locator::with_finder(None, Arc::new(move |_: &SearchEnv| finder_tools.clone()));
    let resolver = ProcessResolver::new(loc.clone());
    // 還沒找過：當成可以用（真的沒有時解析會說找不到）
    assert!(resolver.available());
    let never = AtomicBool::new(false);
    let got = resolver
        .resolve(&request("https://youtu.be/BaW_jenozKc"), &never)
        .unwrap();
    assert_eq!(
        got.info
            .title
            .as_deref()
            .map(|t| t.starts_with("youtube-dl test video")),
        Some(true)
    );
    // 子程序的 PATH 是找的時候決定的（deno 的資料夾在最前面）
    let first = std::env::split_paths(&OsString::from(fake.env("PATH"))).next().unwrap();
    assert_eq!(first, fake.dir.join("deno-dir"));
    assert!(resolver.available());

    // 確定沒有 yt-dlp
    let none = Locator::with_finder(None, Arc::new(|env: &SearchEnv| Tools::none(env)));
    let resolver = ProcessResolver::new(none.clone());
    assert_eq!(
        resolver.resolve(&request("https://youtu.be/x"), &never).map(|_| ()),
        Err(YtdlError::Missing.into())
    );
    assert!(!resolver.available());
}

/// 沒找到 yt-dlp：剛找過時馬上說沒有（連續開好幾個網址不每次都找）；過了一陣子再開就重新找，
/// 使用者照起始畫面的說明裝好之後不用重開影戲。找到 yt-dlp、沒有 deno 時也一樣（deno 裝好了也找得到）
#[test]
fn missing_tools_are_searched_again_later() {
    let Some(fake) = Fake::new("install") else { return };
    fake.write("stdout.txt", YOUTUBE_JSON);
    let tools = Tools {
        ytdl: Some(fake.located("out")),
        ytdl_version: None,
        ytdl_error: None,
        deno: None,
        deno_too_old: None,
        env: ChildEnv {
            path: None,
            cwd: Some(fake.dir.clone()),
        },
    };
    let installed = Arc::new(AtomicBool::new(false));
    let searches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (i, n) = (installed.clone(), searches.clone());
    let loc = Locator::with_finder(
        None,
        Arc::new(move |env: &SearchEnv| {
            n.fetch_add(1, Ordering::SeqCst);
            if i.load(Ordering::SeqCst) {
                tools.clone()
            } else {
                Tools::none(env)
            }
        }),
    );
    let never = AtomicBool::new(false);
    let req = request("https://youtu.be/BaW_jenozKc");
    // 剛找過、沒有：馬上說沒有，不再找
    let recent = ProcessResolver::new(loc.clone()).with_recheck(Duration::from_secs(3600));
    assert_eq!(
        recent.resolve(&req, &never).map(|_| ()).map_err(|f| f.error),
        Err(YtdlError::Missing)
    );
    assert!(!recent.available());
    assert!(!recent.available());
    assert_eq!(searches.load(Ordering::SeqCst), 1);
    // 裝好了；過了一陣子（測試把間隔改成 0）再開：重新找，找到就播
    installed.store(true, Ordering::SeqCst);
    let later = ProcessResolver::new(loc.clone()).with_recheck(Duration::ZERO);
    assert!(
        later.available(),
        "沒找到的結果過了一陣子：當成可以，解析時等重新找的結果"
    );
    let got = later.resolve(&req, &never).expect("裝好之後找得到 yt-dlp");
    assert!(got.info.title.is_some());
    assert_eq!(searches.load(Ordering::SeqCst), 2);
    // 有 yt-dlp、沒有 deno：也會重新找
    assert!(later.available());
    later.resolve(&req, &never).unwrap();
    assert_eq!(searches.load(Ordering::SeqCst), 3);
    // 剛找過、有 yt-dlp：可以，不再找
    assert!(recent.available());
    assert!(loc.get().is_some_and(|t| t.ytdl.is_some()));
    assert!(!loc.searching());
    assert_eq!(searches.load(Ordering::SeqCst), 3);
}

#[test]
fn the_tool_search_counts_against_the_deadline() {
    // 第一次播網站影片時要先找 yt-dlp：找的時間也算在等待時間裡（hook 的看門狗只多給 10 秒）
    let Some(fake) = Fake::new("slowsearch") else { return };
    let tools = Tools {
        ytdl: Some(fake.located("hang")),
        ytdl_version: None,
        ytdl_error: None,
        deno: None,
        deno_too_old: None,
        env: ChildEnv::default(),
    };
    let never = AtomicBool::new(false);
    let req = request("https://youtu.be/BaW_jenozKc");

    // 找不完：等到等待時間就放棄（不是等 90 秒）
    let gate = Arc::new(std::sync::Mutex::new(()));
    let held = gate.lock().unwrap();
    let (g, t) = (gate.clone(), tools.clone());
    let stuck = Locator::with_finder(
        None,
        Arc::new(move |_: &SearchEnv| {
            let _g = g.lock().unwrap();
            t.clone()
        }),
    );
    let started = Instant::now();
    let r = ProcessResolver::new(stuck)
        .with_deadline(Duration::from_secs(2))
        .resolve(&req, &never);
    assert_eq!(r.map(|_| ()).map_err(|f| f.error), Err(YtdlError::NoResponse));
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    drop(held);

    // 找了 5 秒：yt-dlp 只剩 3 秒（不是再給完整的 8 秒）
    let t = tools.clone();
    let slow = Locator::with_finder(
        None,
        Arc::new(move |_: &SearchEnv| {
            std::thread::sleep(Duration::from_secs(5));
            t.clone()
        }),
    );
    let resolver = ProcessResolver::new(slow)
        .with_deadline(Duration::from_secs(8))
        .with_limits(Limits {
            abandon_grace: Duration::from_millis(500),
            term_grace: Duration::from_millis(500),
            ..Limits::default()
        });
    let started = Instant::now();
    let r = resolver.resolve(&req, &never);
    let took = started.elapsed();
    assert_eq!(r.map(|_| ()).map_err(|f| f.error), Err(YtdlError::NoResponse));
    assert!(took >= Duration::from_secs(5), "{took:?}");
    assert!(took < Duration::from_secs(11), "找的時間要算在等待時間裡：{took:?}");
    fake.assert_tree_ends(Duration::from_secs(20));
}

/// Unix：照真的尋找方式（工具資料夾裡的 yt-dlp、deno，執行 `--version`）
#[cfg(unix)]
#[test]
fn real_search_finds_managed_tools_and_their_versions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("vitascope-tools-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let script = |name: &str, body: &str| {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let ytdlp = script("yt-dlp", "echo 2026.08.19.1");
    let deno = script("deno", "echo 'deno 2.9.7 (stable, release)'");
    let env = SearchEnv {
        tools_dir: Some(dir.clone()),
        home: Some(dir.join("home")),
        ..SearchEnv::default()
    };
    let tools = locate::find_tools(&env, &locate::probe_version);
    let _ = std::fs::remove_dir_all(&dir);
    let y = tools.ytdl.expect("找到影戲下載的 yt-dlp");
    assert_eq!((y.program.as_path(), y.managed()), (ytdlp.as_path(), true));
    assert_eq!(
        tools.ytdl_version.map(|v| v.to_string()).as_deref(),
        Some("2026.08.19.1")
    );
    let d = tools.deno.expect("找到 deno");
    assert_eq!((d.path.as_path(), d.managed), (deno.as_path(), true));
    assert_eq!(
        std::env::split_paths(tools.env.path.as_ref().unwrap())
            .next()
            .as_deref(),
        Some(dir.as_path())
    );
    assert_eq!(tools.env.cwd.as_deref(), Some(dir.as_path()));
}

/// 真的 yt-dlp 連到 YouTube（yt-dlp 自己的測試影片）。要 `VITASCOPE_ONLINE_TESTS=1` 加上 `--ignored`
#[test]
#[ignore = "連網：設定 VITASCOPE_ONLINE_TESTS=1 再用 --ignored 執行"]
fn real_ytdlp_resolves_a_youtube_video() {
    if std::env::var_os("VITASCOPE_ONLINE_TESTS").is_none_or(|v| v != "1") {
        eprintln!("沒有設定 VITASCOPE_ONLINE_TESTS=1，不連網");
        return;
    }
    let loc = Locator::new(vitascope::paths::tools_dir());
    let tools = loc.wait(locate::SEARCH_WAIT).expect("找 yt-dlp 逾時");
    eprintln!(
        "yt-dlp：{:?}（{:?}）；deno：{:?}",
        tools.ytdl.as_ref().map(|l| &l.program),
        tools.ytdl_version,
        tools.deno.as_ref().map(|d| (&d.path, d.version))
    );
    assert!(tools.ytdl.is_some(), "這台電腦沒有 yt-dlp");
    let has_deno = tools.deno.is_some();
    let page = "https://www.youtube.com/watch?v=BaW_jenozKc";
    let resolver = ProcessResolver::new(loc);
    let never = AtomicBool::new(false);
    let media = |page: &str| {
        let got = resolver.resolve(&request(page), &never).expect("解析失敗");
        eprintln!("{page} 的提醒：{:?}", got.hints);
        let plan = ytdl::plan::plan(&got.info, page, &Default::default(), &Default::default()).expect("沒有計畫");
        let ytdl::plan::Plan::Media(m) = plan else {
            panic!("{page} 應該是一部影片");
        };
        assert!(m.info.title.as_deref().is_some_and(|t| !t.is_empty()));
        assert!(!m.info.chosen.is_empty());
        m
    };
    let m = media(page);
    assert_eq!(m.info.resume_key().as_deref(), Some("ytdl://youtube/BaW_jenozKc"));
    if has_deno {
        // 有 deno：影像、聲音分開的最高畫質 → 兩個串流合成一個 EDL（不是退回 360p 的合併格式）
        assert_eq!(m.info.chosen.len(), 2, "{:?}", m.info.chosen);
        assert!(m.open.starts_with("edl://"), "{}", m.open);
    }
    // 第二個網站（yt-dlp 自己的 Vimeo 測試影片）
    media("https://vimeo.com/56015672");
}
