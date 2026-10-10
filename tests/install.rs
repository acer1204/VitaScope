//! 影戲下載 yt-dlp、deno（使用者按下時才連網，主人的決定 Q2 (a)）：從本機的測試伺服器上的假 GitHub 發佈下載，
//! 不連到真的 GitHub。確認：照最新的標籤下載、核對 SHA-256、解開 deno 的 zip、進度與取消、沒有進度時放棄、
//! 已經是最新版不下載、換檔案時等正在用它的 yt-dlp、失敗時不留下半個檔案也不動到原本的檔案。
//!
//! 真的從 GitHub 下載（約 60 MB）再用它解析 YouTube 的測試要設定 `VITASCOPE_ONLINE_TESTS=1` 加上 `--ignored`。

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};
use support::http::{Canned, Server};
use support::release::{self, asset, download_path, fake_ytdl, hex};
use vitascope::paths::Os;
use vitascope::web::{Progress, Web, WebError};
use vitascope::ytdl::install::{InstallError, Installer, Job, Op, Outcome, Stage, Status, Tool, sha256_of};

/// 測試用的工具資料夾（結束時刪掉）
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let p = std::env::temp_dir().join(format!("vitascope-install-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        TempDir(p)
    }

    /// 工具資料夾（還沒建立：下載時自己建）
    fn tools(&self) -> PathBuf {
        self.0.join("tools")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn installer(dir: &TempDir, server: &Server) -> Installer {
    Installer::with_base(dir.tools(), &server.url(""))
}

fn run(inst: &Installer, op: Op) -> Result<Outcome, InstallError> {
    inst.run(&op, &Status::default(), &AtomicBool::new(false))
}

/// 工具資料夾裡剩下的暫存檔
fn partials(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".vitascope-download"))
                .collect()
        })
        .unwrap_or_default()
}

fn requested(server: &Server, path: &str) -> bool {
    server.requests().iter().any(|r| r.path == path)
}

fn set_mtime(path: &Path, t: SystemTime) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(t).unwrap();
}

#[test]
fn downloads_ytdl_from_the_latest_release() {
    let server = Server::start();
    let dir = TempDir::new("ytdl");
    let content = fake_ytdl("2026.10.01", 300_000);
    release::publish_ytdl(&server, "2026.10.01", &content, None);
    let inst = installer(&dir, &server);
    assert!(inst.available(Tool::Ytdl));
    let status = Status::default();
    let out = inst.run(&Op::Install(Tool::Ytdl), &status, &AtomicBool::new(false));
    assert_eq!(
        out,
        Ok(Outcome::Installed {
            tool: Tool::Ytdl,
            version: "2026.10.01".into()
        })
    );
    let path = inst.path(Tool::Ytdl);
    assert_eq!(path, dir.tools().join(Tool::Ytdl.file_name(Os::current())));
    assert_eq!(std::fs::read(&path).unwrap(), content);
    // 進度：總共多大、收到多少
    assert_eq!(status.bytes.get(), (content.len() as u64, Some(content.len() as u64)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
    }
    assert!(partials(&dir.tools()).is_empty(), "{:?}", partials(&dir.tools()));
    // 從最新的標籤讀檢查碼與檔案（同一個標籤），User-Agent 是影戲
    for p in [
        format!("/{}/releases/latest", Tool::Ytdl.repo()),
        download_path(Tool::Ytdl, "2026.10.01", "SHA2-256SUMS"),
        download_path(Tool::Ytdl, "2026.10.01", asset(Tool::Ytdl)),
    ] {
        let r = server
            .requests()
            .into_iter()
            .find(|r| r.path == p)
            .unwrap_or_else(|| panic!("沒有讀 {p}"));
        assert!(
            r.header("User-Agent").is_some_and(|ua| ua.starts_with("VitaScope/")),
            "{r:?}"
        );
    }
}

#[test]
fn downloads_and_unpacks_deno() {
    let server = Server::start();
    let dir = TempDir::new("deno");
    let exe: Vec<u8> = (0..400_000u32).map(|i| (i % 253) as u8).collect();
    let zip = release::deno_zip(&exe);
    release::publish_deno(&server, "v2.9.7", &zip);
    let inst = installer(&dir, &server);
    let status = Status::default();
    let out = inst.run(&Op::Install(Tool::Deno), &status, &AtomicBool::new(false));
    assert_eq!(
        out,
        Ok(Outcome::Installed {
            tool: Tool::Deno,
            version: "2.9.7".into()
        })
    );
    let path = inst.path(Tool::Deno);
    assert_eq!(std::fs::read(&path).unwrap(), exe);
    // 下載的是 zip（進度是 zip 的大小），解開後只留下 deno
    assert_eq!(status.bytes.get().1, Some(zip.len() as u64));
    assert!(partials(&dir.tools()).is_empty());
    let names: Vec<String> = std::fs::read_dir(dir.tools())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [Tool::Deno.file_name(Os::current())]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
    }
}

/// 檢查碼對不上：刪掉下載的檔案，原本的那一份不動
#[test]
fn a_wrong_checksum_keeps_the_old_copy() {
    let server = Server::start();
    let dir = TempDir::new("mismatch");
    let inst = installer(&dir, &server);
    std::fs::create_dir_all(dir.tools()).unwrap();
    let old = fake_ytdl("2026.01.01", 100);
    std::fs::write(inst.path(Tool::Ytdl), &old).unwrap();
    let sums = format!("{}  {}\n", hex(&sha256_of(b"something else")), asset(Tool::Ytdl));
    release::publish_ytdl(&server, "2026.10.01", &fake_ytdl("2026.10.01", 5000), Some(sums));
    let out = run(
        &inst,
        Op::Update {
            tool: Tool::Ytdl,
            installed: Some("2026.01.01".into()),
        },
    );
    assert_eq!(out, Err(InstallError::HashMismatch));
    assert_eq!(std::fs::read(inst.path(Tool::Ytdl)).unwrap(), old);
    assert!(partials(&dir.tools()).is_empty(), "{:?}", partials(&dir.tools()));
    let msg = InstallError::HashMismatch.message(&Op::Install(Tool::Ytdl));
    assert_eq!(msg, "無法下載 yt-dlp：下載的檔案檢查失敗（雜湊不符），已刪除");

    // deno 的 zip 也一樣
    let zip = release::deno_zip(b"deno");
    release::publish_deno(&server, "v2.9.7", &zip);
    server.put(
        &download_path(Tool::Deno, "v2.9.7", asset(Tool::Deno)),
        Canned::ok(release::deno_zip(b"tampered")),
    );
    assert_eq!(run(&inst, Op::Install(Tool::Deno)), Err(InstallError::HashMismatch));
    assert!(!inst.path(Tool::Deno).exists());
    assert!(partials(&dir.tools()).is_empty());
}

/// 檢查碼裡沒有這個檔案：不下載
#[test]
fn no_checksum_means_no_download() {
    let server = Server::start();
    let dir = TempDir::new("nosum");
    let sums = format!("{}  yt-dlp_some_other_build\n", hex(&sha256_of(b"x")));
    release::publish_ytdl(&server, "2026.10.01", b"data", Some(sums));
    let inst = installer(&dir, &server);
    assert_eq!(run(&inst, Op::Install(Tool::Ytdl)), Err(InstallError::NoChecksum));
    assert!(!requested(
        &server,
        &download_path(Tool::Ytdl, "2026.10.01", asset(Tool::Ytdl))
    ));
    assert!(!inst.path(Tool::Ytdl).exists());
}

/// 找不到最新的發佈、GitHub 回應錯誤、限制次數：說明原因（中文介面）
#[test]
fn release_lookup_failures() {
    let server = Server::start();
    let dir = TempDir::new("lookup");
    let inst = installer(&dir, &server);
    let latest = format!("/{}/releases/latest", Tool::Ytdl.repo());
    // 沒有任何發佈：GitHub 轉到發佈的列表
    server.put(
        &latest,
        Canned::redirect(&server.url(&format!("/{}/releases", Tool::Ytdl.repo()))),
    );
    let e = run(&inst, Op::Install(Tool::Ytdl)).unwrap_err();
    assert_eq!(e, InstallError::NoRelease);
    assert_eq!(e.reason(Tool::Ytdl), "GitHub 上找不到最新的版本");
    // 不是轉址（看不出版本）
    server.put(&latest, Canned::ok("<html>"));
    assert_eq!(run(&inst, Op::Install(Tool::Ytdl)), Err(InstallError::NoRelease));
    server.put(&latest, Canned::status(404));
    let e = run(&inst, Op::Install(Tool::Ytdl)).unwrap_err();
    assert_eq!(e, InstallError::Web(WebError::Status(404)));
    assert_eq!(
        e.message(&Op::Install(Tool::Ytdl)),
        "無法下載 yt-dlp：伺服器回應錯誤（HTTP 404）"
    );
    server.put(&latest, Canned::status(429));
    let e = run(&inst, Op::Install(Tool::Ytdl)).unwrap_err();
    assert!(e.reason(Tool::Ytdl).contains("限制連線次數"), "{e:?}");
    // 連不上（伺服器關了）
    let url = server.url("");
    drop(server);
    let gone = Installer::with_base(dir.tools(), &url);
    let e = run(&gone, Op::Install(Tool::Ytdl)).unwrap_err();
    assert!(matches!(e, InstallError::Web(WebError::Network(_))), "{e:?}");
    assert!(e.reason(Tool::Ytdl).starts_with("無法連線："), "{e:?}");
    assert!(!dir.tools().join(Tool::Ytdl.file_name(Os::current())).exists());
}

/// 更新：先看最新的標籤，已經是最新版就不下載（檔案的時間更新，「超過 30 天」的提醒重新算）；有新版才下載
#[test]
fn update_checks_the_tag_first() {
    let server = Server::start();
    let dir = TempDir::new("update");
    let inst = installer(&dir, &server);
    std::fs::create_dir_all(dir.tools()).unwrap();
    let path = inst.path(Tool::Ytdl);
    let current = fake_ytdl("2026.10.01", 100);
    std::fs::write(&path, &current).unwrap();
    let long_ago = SystemTime::now() - Duration::from_secs(90 * 86_400);
    set_mtime(&path, long_ago);
    release::publish_ytdl(&server, "2026.10.01", &fake_ytdl("2026.10.01", 5000), None);
    let update = |installed: &str| Op::Update {
        tool: Tool::Ytdl,
        installed: Some(installed.into()),
    };
    assert_eq!(
        run(&inst, update("2026.10.01")),
        Ok(Outcome::UpToDate {
            tool: Tool::Ytdl,
            version: "2026.10.01".into()
        })
    );
    assert!(!requested(
        &server,
        &download_path(Tool::Ytdl, "2026.10.01", asset(Tool::Ytdl))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), current, "沒有換掉");
    let touched = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert!(
        SystemTime::now().duration_since(touched).unwrap() < Duration::from_secs(3600),
        "檢查過了：修改時間是現在"
    );
    // 每日建置比同一天的正式版新：也不下載
    assert!(matches!(
        run(&inst, update("2026.10.01.123456")),
        Ok(Outcome::UpToDate { .. })
    ));

    // 有新版：下載、換掉
    let newer = fake_ytdl("2026.10.09", 7000);
    release::publish_ytdl(&server, "2026.10.09", &newer, None);
    assert_eq!(
        run(&inst, update("2026.10.01")),
        Ok(Outcome::Updated {
            tool: Tool::Ytdl,
            version: "2026.10.09".into()
        })
    );
    assert_eq!(std::fs::read(&path).unwrap(), newer);
    let msg = Outcome::Updated {
        tool: Tool::Ytdl,
        version: "2026.10.09".into(),
    }
    .message();
    assert_eq!(msg, "yt-dlp 已更新到 2026.10.09");
}

fn wait_for(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let until = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < until, "等不到：{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 背景下載：有進度、可以取消（馬上停，不留下暫存檔，原本沒有的檔案還是沒有）
#[test]
fn background_download_reports_progress_and_cancels() {
    let server = Server::start();
    let dir = TempDir::new("cancel");
    let content = fake_ytdl("2026.10.01", 2_000_000);
    release::publish_ytdl(&server, "2026.10.01", &content, None);
    // 送一半就停住（連線開著）
    server.put(
        &download_path(Tool::Ytdl, "2026.10.01", asset(Tool::Ytdl)),
        Canned::ok(content.clone()).stall_after(1_000_000),
    );
    let inst = Arc::new(installer(&dir, &server));
    let woke = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let w = woke.clone();
    let job = Job::start(
        inst.clone(),
        Op::Install(Tool::Ytdl),
        Some(Arc::new(move || {
            w.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })),
    );
    wait_for("收到一半", Duration::from_secs(30), || {
        job.status.bytes.get() == (1_000_000, Some(content.len() as u64))
    });
    assert_eq!(job.status.stage(), Stage::Downloading);
    assert!(job.poll().is_none(), "還在下載");
    assert!(!partials(&dir.tools()).is_empty(), "下載中有暫存檔");
    let asked = Instant::now();
    job.cancel();
    let result = job.wait(Duration::from_secs(10)).expect("取消後很快結束");
    assert!(asked.elapsed() < Duration::from_secs(5), "{:?}", asked.elapsed());
    assert_eq!(result, Err(InstallError::Web(WebError::Cancelled)));
    assert!(!inst.path(Tool::Ytdl).exists());
    assert!(partials(&dir.tools()).is_empty(), "{:?}", partials(&dir.tools()));
    wait_for("叫醒介面", Duration::from_secs(10), || {
        woke.load(std::sync::atomic::Ordering::SeqCst) == 1
    });
}

/// 很久沒有收到資料：放棄（不是整個下載的時間限制）
#[test]
fn a_stalled_download_gives_up() {
    let server = Server::start();
    let dir = TempDir::new("stall");
    let content = fake_ytdl("2026.10.01", 50_000);
    release::publish_ytdl(&server, "2026.10.01", &content, None);
    server.put(
        &download_path(Tool::Ytdl, "2026.10.01", asset(Tool::Ytdl)),
        Canned::ok(content).stall_after(10_000),
    );
    let inst = installer(&dir, &server).with_web(Web::new(false, false).with_stall(Duration::from_millis(800)));
    let started = Instant::now();
    let e = run(&inst, Op::Install(Tool::Ytdl)).unwrap_err();
    assert_eq!(e, InstallError::Web(WebError::Stalled));
    assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    assert!(partials(&dir.tools()).is_empty());
    assert!(e.reason(Tool::Ytdl).contains("沒有進度"));
}

/// 換檔案、移除時等正在用它的 yt-dlp（解析中）結束；等太久就說正在使用中
#[test]
fn replacing_and_removing_wait_for_a_running_ytdl() {
    let server = Server::start();
    let dir = TempDir::new("lock");
    let content = fake_ytdl("2026.10.01", 1000);
    release::publish_ytdl(&server, "2026.10.01", &content, None);
    let inst = Arc::new(installer(&dir, &server).with_lock_wait(Duration::from_millis(300)));
    let path = inst.path(Tool::Ytdl);
    let lock = vitascope::ytdl::run::tool_lock(&path);
    let never = AtomicBool::new(false);
    let far = Instant::now() + Duration::from_secs(60);

    // 解析中：等不到就放棄，暫存檔刪掉
    let held = lock.shared_until(&never, far).unwrap();
    assert_eq!(run(&inst, Op::Install(Tool::Ytdl)), Err(InstallError::Busy));
    assert!(!path.exists());
    assert!(partials(&dir.tools()).is_empty());
    // 解析在等的時候結束：接著換上去
    let patient = Arc::new(installer(&dir, &server).with_lock_wait(Duration::from_secs(20)));
    let job = Job::start(patient, Op::Install(Tool::Ytdl), None);
    wait_for("等 yt-dlp 結束", Duration::from_secs(30), || {
        job.status.stage() == Stage::Waiting
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(job.poll().is_none(), "解析還沒結束，不能換");
    drop(held);
    let r = job.wait(Duration::from_secs(20)).expect("做完了");
    assert!(matches!(r, Ok(Outcome::Installed { .. })), "{r:?}");
    assert_eq!(std::fs::read(&path).unwrap(), content);

    // 移除也一樣
    let held = lock.shared_until(&never, far).unwrap();
    let e = run(&inst, Op::Remove(Tool::Ytdl)).unwrap_err();
    assert_eq!(e, InstallError::Busy);
    assert_eq!(
        e.message(&Op::Remove(Tool::Ytdl)),
        "無法移除 yt-dlp：yt-dlp 正在使用中，請稍後再試"
    );
    assert!(path.exists());
    drop(held);
    assert_eq!(run(&inst, Op::Remove(Tool::Ytdl)), Ok(Outcome::Removed(Tool::Ytdl)));
    assert!(!path.exists());
    // 已經沒有了：也算移除了
    assert_eq!(run(&inst, Op::Remove(Tool::Ytdl)), Ok(Outcome::Removed(Tool::Ytdl)));
}

/// 等正在用它的 yt-dlp 結束時按了取消：馬上停，不換上新的（原本的那一份不動，暫存檔刪掉）
#[test]
fn cancelling_while_waiting_for_ytdl_keeps_the_old_copy() {
    let server = Server::start();
    let dir = TempDir::new("lock-cancel");
    let newer = fake_ytdl("2026.10.01", 1000);
    release::publish_ytdl(&server, "2026.10.01", &newer, None);
    let inst = Arc::new(installer(&dir, &server).with_lock_wait(Duration::from_secs(60)));
    std::fs::create_dir_all(dir.tools()).unwrap();
    let path = inst.path(Tool::Ytdl);
    let old = fake_ytdl("2026.01.01", 100);
    std::fs::write(&path, &old).unwrap();
    let lock = vitascope::ytdl::run::tool_lock(&path);
    let never = AtomicBool::new(false);
    let held = lock
        .shared_until(&never, Instant::now() + Duration::from_secs(120))
        .unwrap();
    let job = Job::start(
        inst.clone(),
        Op::Update {
            tool: Tool::Ytdl,
            installed: Some("2026.01.01".into()),
        },
        None,
    );
    wait_for("等 yt-dlp 結束", Duration::from_secs(30), || {
        job.status.stage() == Stage::Waiting
    });
    let asked = Instant::now();
    job.cancel();
    let r = job.wait(Duration::from_secs(10)).expect("取消後很快結束");
    assert!(asked.elapsed() < Duration::from_secs(5), "{:?}", asked.elapsed());
    assert_eq!(r, Err(InstallError::Web(WebError::Cancelled)));
    drop(held);
    assert_eq!(std::fs::read(&path).unwrap(), old, "沒有換掉");
    assert!(partials(&dir.tools()).is_empty(), "{:?}", partials(&dir.tools()));
    assert_eq!(lock.state(), vitascope::ytdl::run::LockState::default(), "鎖放開了");
}

/// deno 也一樣：yt-dlp 執行時拿著它會用的 deno 的鎖（`run::run`），換掉、移除 deno 時等它結束
#[test]
fn replacing_and_removing_deno_wait_for_a_running_ytdl() {
    let server = Server::start();
    let dir = TempDir::new("deno-lock");
    let zip = release::deno_zip(b"new deno");
    release::publish_deno(&server, "v2.9.7", &zip);
    let inst = installer(&dir, &server).with_lock_wait(Duration::from_millis(300));
    std::fs::create_dir_all(dir.tools()).unwrap();
    let path = inst.path(Tool::Deno);
    std::fs::write(&path, b"old deno").unwrap();
    let lock = vitascope::ytdl::run::tool_lock(&path);
    let never = AtomicBool::new(false);
    let held = lock
        .shared_until(&never, Instant::now() + Duration::from_secs(60))
        .unwrap();
    assert_eq!(run(&inst, Op::Install(Tool::Deno)), Err(InstallError::Busy));
    assert_eq!(run(&inst, Op::Remove(Tool::Deno)), Err(InstallError::Busy));
    assert_eq!(std::fs::read(&path).unwrap(), b"old deno");
    assert!(partials(&dir.tools()).is_empty(), "{:?}", partials(&dir.tools()));
    drop(held);
    assert!(matches!(
        run(&inst, Op::Install(Tool::Deno)),
        Ok(Outcome::Installed { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"new deno");
    assert_eq!(run(&inst, Op::Remove(Tool::Deno)), Ok(Outcome::Removed(Tool::Deno)));
    assert!(!path.exists());
}

/// 這個系統沒有可以下載的版本：不連網
#[test]
fn unsupported_systems_never_connect() {
    let server = Server::start();
    let dir = TempDir::new("arch");
    let inst = installer(&dir, &server).with_arch("riscv64");
    release::publish_ytdl(&server, "2026.10.01", b"x", None);
    if cfg!(target_os = "macos") {
        // macOS 的 yt-dlp 是 universal：任何 CPU 都有
        assert!(inst.available(Tool::Ytdl));
    } else {
        assert!(!inst.available(Tool::Ytdl));
        assert_eq!(run(&inst, Op::Install(Tool::Ytdl)), Err(InstallError::Unsupported));
    }
    assert!(!inst.available(Tool::Deno));
    let before = server.requests().len();
    let e = run(&inst, Op::Install(Tool::Deno)).unwrap_err();
    assert_eq!(e, InstallError::Unsupported);
    assert_eq!(server.requests().len(), before, "沒有版本可以下載時不連網");
}

/// 下載的大小上限：伺服器說的大小超過就不下載；沒說大小時收到超過就停
#[test]
fn downloads_are_capped() {
    let server = Server::start();
    server.put("/big", Canned::ok(vec![b'x'; 5000]));
    let web = Web::new(false, false);
    let never = AtomicBool::new(false);
    let mut out = Vec::new();
    let r = web.download(&server.url("/big"), &mut out, 4999, &Progress::default(), &never);
    assert_eq!(r, Err(WebError::TooLarge));
    assert!(out.is_empty(), "看到大小就停，一個位元組都不寫");
    let mut out = Vec::new();
    assert_eq!(
        web.download(&server.url("/big"), &mut out, 5000, &Progress::default(), &never),
        Ok(5000)
    );
    assert_eq!(out.len(), 5000);
    // 轉址會跟著走
    server.put("/moved", Canned::redirect(&server.url("/big")));
    assert_eq!(
        web.fetch(&server.url("/moved"), 10_000, &never).map(|v| v.len()),
        Ok(5000)
    );
    assert_eq!(web.redirect_target(&server.url("/moved")), Ok(Some(server.url("/big"))));
    assert_eq!(web.redirect_target(&server.url("/big")), Ok(None));

    // 沒說大小（或說得比實際小）：收到超過上限就停，寫進去的不超過上限
    server.put("/unsized", Canned::ok(vec![b'y'; 300_000]).without_length());
    let progress = Progress::default();
    let mut out = Vec::new();
    let r = web.download(&server.url("/unsized"), &mut out, 100_000, &progress, &never);
    assert_eq!(r, Err(WebError::TooLarge));
    assert!(out.len() <= 100_000, "寫了 {} 位元組", out.len());
    assert_eq!(progress.get().1, None, "不知道總共多大");
    assert_eq!(
        web.fetch(&server.url("/unsized"), 300_000, &never).map(|v| v.len()),
        Ok(300_000)
    );
}

// ───────────── 真的從 GitHub 下載（要 VITASCOPE_ONLINE_TESTS=1） ─────────────

/// 真的從 GitHub 下載 yt-dlp 與 deno（約 60 MB）到暫存資料夾，確認能執行（版本），再用它們解析 YouTube。
/// 要 `VITASCOPE_ONLINE_TESTS=1` 加上 `--ignored`；不碰使用者的工具資料夾
#[test]
#[ignore = "連網：設定 VITASCOPE_ONLINE_TESTS=1 再用 --ignored 執行"]
fn real_github_download_and_youtube_resolve() {
    use vitascope::ytdl::locate::{self, SearchEnv};
    if std::env::var_os("VITASCOPE_ONLINE_TESTS").is_none_or(|v| v != "1") {
        eprintln!("沒有設定 VITASCOPE_ONLINE_TESTS=1，不連網");
        return;
    }
    let dir = TempDir::new("real");
    let inst = Installer::github(dir.tools());
    let never = AtomicBool::new(false);
    for tool in [Tool::Ytdl, Tool::Deno] {
        let started = Instant::now();
        let out = inst.run(&Op::Install(tool), &Status::default(), &never);
        eprintln!("{}：{out:?}（{:?}）", tool.name(), started.elapsed());
        assert!(matches!(out, Ok(Outcome::Installed { .. })), "{out:?}");
    }
    // 只找這個暫存的工具資料夾（不找使用者電腦上的）
    let env = SearchEnv {
        tools_dir: Some(dir.tools()),
        ..SearchEnv::current(None, None)
    };
    let env = SearchEnv {
        path_var: None,
        exe_dir: None,
        system_dirs: false,
        home: Some(dir.0.clone()),
        local_app_data: Some(dir.0.clone()),
        ..env
    };
    let tools = locate::find_tools(&env, &locate::probe_version);
    eprintln!("{tools:?}");
    assert!(tools.ytdl.as_ref().is_some_and(|y| y.managed()), "{tools:?}");
    assert!(tools.ytdl_version.is_some(), "{tools:?}");
    assert!(tools.deno.as_ref().is_some_and(|d| d.managed), "{tools:?}");
    // 已經是最新版
    let installed = tools.ytdl_version.map(|v| v.to_string());
    let out = inst.run(
        &Op::Update {
            tool: Tool::Ytdl,
            installed,
        },
        &Status::default(),
        &never,
    );
    assert!(matches!(out, Ok(Outcome::UpToDate { .. })), "{out:?}");
    // 用下載的 yt-dlp + deno 解析 YouTube：影像、聲音分開的最高畫質
    let req = vitascope::ytdl::Request::new(
        "https://www.youtube.com/watch?v=BaW_jenozKc",
        &vitascope::net::NetSettings::default(),
        &vitascope::ytdl::SitePrefs::default(),
    )
    .unwrap();
    let got = vitascope::ytdl::resolve_with(
        tools.ytdl.as_ref().unwrap(),
        &tools.child_env(),
        &req,
        &vitascope::ytdl::run::Limits::default(),
        &never,
    )
    .expect("解析失敗");
    eprintln!("提醒：{:?}", got.hints);
    assert!(got.hints.is_empty(), "有 deno 時不該有提醒：{:?}", got.hints);
    let plan = vitascope::ytdl::plan::plan(&got.info, &req.url, &Default::default(), &Default::default()).unwrap();
    let vitascope::ytdl::plan::Plan::Media(m) = plan else {
        panic!("應該是一部影片");
    };
    assert_eq!(m.info.chosen.len(), 2, "{:?}", m.info.chosen);
}
