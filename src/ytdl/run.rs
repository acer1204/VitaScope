//! 執行 yt-dlp（以及 deno 的版本查詢）：不經過 shell、用完整路徑、把輸出讀回來，取消與逾時時連它開的子程序一起結束。
//!
//! - 啟動用 `syscmd::command`（AppImage 的 `LD_LIBRARY_PATH` 還原成原本的值）。
//! - Windows：
//!   - 發佈版是 GUI 程式，主控台程式要加 `CREATE_NO_WINDOW`，不然每次都閃一個黑色視窗。
//!   - 子程序放進一個 Job Object（`KILL_ON_JOB_CLOSE`）。yt-dlp.exe 是 PyInstaller 打包的：外層程式解開後再開一個
//!     Python 子程序，只結束外層的話裡面那個照樣在跑，而且留下 `%TEMP%\_MEI*`。
//!   - 一般的取消、逾時是「放手」：馬上回傳，讓它自己跑完（最多 `--socket-timeout`），不留下暫存資料夾；
//!     放手後超過 [`Limits::abandon_grace`]（而且從啟動算起最多 [`Limits::hard_limit`]）還沒結束，
//!     才用 `TerminateJobObject` 整個結束。
//!     關閉 Job 的 handle 時（正常結束後、影戲結束時）裡面剩下的程序也會被結束。
//! - Unix：子程序自己一個 process group；取消、逾時先送 SIGTERM 給整個群組（PyInstaller 的外層會轉給裡面的程式、
//!   清掉暫存資料夾），[`Limits::term_grace`] 後再送 SIGKILL。
//! - 同一個 yt-dlp 的使用規則（[`ToolLock`]）：解析可以同時好幾個；換檔案（影戲的下載、更新、移除）要獨占，
//!   而且要等放手的程序真的結束（不然換掉執行檔時它還在用）。新的解析不等放手的程序。
//!   yt-dlp 在 YouTube 會開 deno：執行時也拿著子程序 PATH 裡第一個 deno 的共用鎖（[`deno_on_path`]，yt-dlp 自己也是
//!   這樣找），換掉、移除 deno 時一樣等它結束。

use super::Located;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 時間與大小的限制
#[derive(Debug, Clone)]
pub struct Limits {
    /// 最多等多久（之後回傳 [`RunError::TimedOut`]，程序照「取消」的方式處理）
    pub deadline: Duration,
    /// Windows：放手後再給它多久自己結束，之後整個 Job 結束
    pub abandon_grace: Duration,
    /// Windows：從啟動算起最多跑多久（放手的程序到了就整個 Job 結束，不管 `abandon_grace` 還剩多少）
    pub hard_limit: Duration,
    /// Unix：SIGTERM 之後多久送 SIGKILL
    pub term_grace: Duration,
    /// 標準輸出最多讀多少（超過的丟掉，[`Finished::stdout_truncated`]）
    pub stdout_cap: usize,
    /// 錯誤輸出最多讀多少
    pub stderr_cap: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            deadline: super::VIDEO_DEADLINE,
            // 放手後最多再跑 2 分鐘，而且從啟動算起不超過 3 分鐘（播放清單等 2 分鐘逾時後只剩 1 分鐘）
            abandon_grace: Duration::from_secs(120),
            hard_limit: Duration::from_secs(180),
            term_grace: Duration::from_secs(2),
            stdout_cap: 64 << 20,
            stderr_cap: 1 << 20,
        }
    }
}

/// 子程序的環境：PATH（先放找到的 deno、工具資料夾…）與目前資料夾；其他環境變數照常繼承
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChildEnv {
    pub path: Option<OsString>,
    pub cwd: Option<PathBuf>,
}

/// 程序結束了
#[derive(Debug, Clone)]
pub struct Finished {
    pub success: bool,
    /// 結束代碼（被訊號結束時 None）
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr: String,
}

#[derive(Debug)]
pub enum RunError {
    /// 啟動失敗（找不到、被防毒擋下…）
    Spawn(std::io::Error),
    /// 取消了（程序在背景結束）
    Cancelled,
    /// 超過等待時間（程序在背景結束）
    TimedOut,
}

/// 要不要拿 [`ToolLock`] 的共用鎖
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    /// 一般的執行（解析、查版本）：拿共用鎖，更新中時等它做完
    Shared,
    /// 呼叫的人已經拿了獨占鎖（更新）
    Held,
}

/// Windows 的 `CREATE_NO_WINDOW`：主控台程式不開視窗
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 等程序時多久看一次
const POLL: Duration = Duration::from_millis(10);
/// 程序結束後，最多等多久讀完它的輸出（還開著輸出的子程序已經被結束了，正常不用等）
const DRAIN_WAIT: Duration = Duration::from_secs(5);

/// 執行 `program`（加上它的 `prefix_args`）與 `args`，等它結束並讀回輸出。
/// `cancel` 變成 true 或超過 `limits.deadline` 時馬上回傳，程序在背景照平台的方式結束
pub fn run(
    program: &Located,
    args: &[OsString],
    env: &ChildEnv,
    limits: &Limits,
    lock: Lock,
    cancel: &AtomicBool,
) -> Result<Finished, RunError> {
    let started = Instant::now();
    // 這個程式，以及它可能開的 deno（同一個檔案只拿一次）
    let mut locks = vec![tool_lock(&program.program)];
    if let Some(deno) = deno_on_path(env).filter(|d| *d != program.program) {
        locks.push(tool_lock(&deno));
    }
    let guard = match lock {
        Lock::Shared => locks
            .iter()
            .map(|l| l.shared_until(cancel, started + limits.deadline))
            .collect::<Result<Vec<_>, _>>()?,
        Lock::Held => Vec::new(),
    };

    let mut cmd = crate::syscmd::command(&program.program);
    cmd.args(&program.prefix_args)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 沒有色彩控制碼、Python 的輸出一律 UTF-8（Windows 的主控台編碼可能是 cp950）
        .env("NO_COLOR", "1")
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8");
    if let Some(path) = &env.path {
        cmd.env("PATH", path);
    }
    if let Some(cwd) = &env.cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 自己一個 process group：取消時連它開的子程序一起結束
        cmd.process_group(0);
    }
    let mut child = spawn(&mut cmd)?;
    let group = Group::new(&child);

    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    spawn_reader(child.stdout.take(), limits.stdout_cap, out_tx);
    spawn_reader(child.stderr.take(), limits.stderr_cap, err_tx);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // 正常結束：還開著的子程序（留下來的）一起結束，輸出管線才會關上
                group.kill_rest();
                drop(guard);
                let (stdout, stdout_truncated) = out_rx.recv_timeout(DRAIN_WAIT).unwrap_or_default();
                let (stderr, _) = err_rx.recv_timeout(DRAIN_WAIT).unwrap_or_default();
                return Ok(Finished {
                    success: status.success(),
                    code: status.code(),
                    stdout,
                    stdout_truncated,
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                });
            }
            Ok(None) => {}
            Err(_) => {
                abandon(child, group, guard, limits, started);
                return Err(RunError::Cancelled);
            }
        }
        if cancel.load(Ordering::Relaxed) {
            abandon(child, group, guard, limits, started);
            return Err(RunError::Cancelled);
        }
        if started.elapsed() >= limits.deadline {
            abandon(child, group, guard, limits, started);
            return Err(RunError::TimedOut);
        }
        std::thread::sleep(POLL);
    }
}

/// 子程序會用的 deno：它的 PATH（[`ChildEnv::path`]）裡第一個 `deno`（Windows `deno.exe`）。
/// 沒有指定 PATH（繼承影戲的）時 None：那裡不會有影戲下載的 deno
pub fn deno_on_path(env: &ChildEnv) -> Option<PathBuf> {
    let name = if cfg!(windows) { "deno.exe" } else { "deno" };
    std::env::split_paths(env.path.as_ref()?)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// 啟動。Linux 的「Text file busy」：別的執行緒剛好在 fork 時，剛寫好的執行檔（下載、更新後）還被那個子程序開著，
/// 等一下就好，重試幾次
fn spawn(cmd: &mut std::process::Command) -> Result<Child, RunError> {
    let mut tries = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20 * tries));
            }
            r => return r.map_err(RunError::Spawn),
        }
    }
}

/// 讀完一個輸出管線（最多 `cap` 位元組，超過的讀掉丟棄，子程序才不會卡在寫入），讀完後送出
fn spawn_reader<R: Read + Send + 'static>(pipe: Option<R>, cap: usize, tx: mpsc::Sender<(Vec<u8>, bool)>) {
    let Some(mut pipe) = pipe else {
        let _ = tx.send((Vec::new(), false));
        return;
    };
    // 開不了執行緒（資源不足）時 tx 跟著丟掉：等輸出的那一邊馬上收到「沒有輸出」
    let _ = std::thread::Builder::new()
        .name("vitascope-ytdl-pipe".into())
        .spawn(move || {
            let mut data = Vec::new();
            let mut truncated = false;
            let mut buf = [0u8; 64 * 1024];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = cap.saturating_sub(data.len());
                        data.extend_from_slice(&buf[..n.min(room)]);
                        truncated |= n > room;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = tx.send((data, truncated));
        });
}

/// 不等了：程序交給背景執行緒結束。共用鎖換成「放手的程序」，更新要等它真的結束
fn abandon(child: Child, group: Group, guard: Vec<SharedGuard>, limits: &Limits, started: Instant) {
    let abandoned: Vec<AbandonedGuard> = guard.into_iter().map(SharedGuard::abandon).collect();
    group.terminate_softly();
    let limits = limits.clone();
    let spawned = std::thread::Builder::new()
        .name("vitascope-ytdl-reaper".into())
        .spawn(move || reap(child, group, abandoned, &limits, started));
    if let Err(e) = spawned {
        eprintln!("[vitascope] 無法開執行緒結束 yt-dlp：{e}");
    }
}

/// 背景：等放手的程序結束，超過寬限時間就強制結束
fn reap(mut child: Child, group: Group, abandoned: Vec<AbandonedGuard>, limits: &Limits, started: Instant) {
    let grace = if cfg!(windows) {
        limits
            .abandon_grace
            .min(limits.hard_limit.saturating_sub(started.elapsed()))
    } else {
        limits.term_grace
    };
    let until = Instant::now() + grace;
    while Instant::now() < until {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Windows：結束整個 Job 並關閉它（裡面剩下的也一起結束）
    group.kill_all(&mut child);
    let _ = child.wait();
    drop(abandoned);
}

// ───────────── 程序群組（Windows 的 Job Object、Unix 的 process group）─────────────

#[cfg(windows)]
struct Group {
    job: Option<job::Job>,
}

#[cfg(windows)]
impl Group {
    fn new(child: &Child) -> Self {
        let job = job::Job::new().filter(|j| j.assign(child));
        if job.is_none() {
            eprintln!("[vitascope] 無法把 yt-dlp 放進 Job Object：取消時可能留下子程序");
        }
        Self { job }
    }

    /// 正常結束後：Job 裡剩下的程序結束（之後關閉 Job）
    fn kill_rest(self) {
        if let Some(j) = &self.job {
            j.terminate();
        }
    }

    /// 放手：Windows 不先通知（PyInstaller 的程式被結束時會留下暫存資料夾），讓它自己跑完
    fn terminate_softly(&self) {}

    fn kill_all(self, child: &mut Child) {
        match &self.job {
            Some(j) => j.terminate(),
            None => {
                let _ = child.kill();
            }
        }
    }
}

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject,
    };

    /// 一個 Job Object；關閉 handle 時裡面的程序全部結束
    pub struct Job(HANDLE);

    // SAFETY: Job 的 handle 可以在任何執行緒使用、關閉
    unsafe impl Send for Job {}

    impl Job {
        pub fn new() -> Option<Job> {
            // SAFETY: 沒有名稱、預設的安全屬性；失敗時回傳 null
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if h.is_null() {
                return None;
            }
            let job = Job(h);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: info 是正確的結構與大小，handle 有效
            let ok = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const info).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            (ok != 0).then_some(job)
        }

        /// 把子程序放進來（之後它開的程序也在裡面）
        pub fn assign(&self, child: &Child) -> bool {
            // SAFETY: 兩個 handle 都有效（child 還沒被 wait 掉，handle 由 Child 持有）
            unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) != 0 }
        }

        pub fn terminate(&self) {
            // SAFETY: handle 有效
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: 只關一次
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(unix)]
struct Group {
    /// process group 的編號（= 子程序的 pid）
    pgid: libc::pid_t,
}

#[cfg(unix)]
impl Group {
    fn new(child: &Child) -> Self {
        Self {
            pgid: child.id() as libc::pid_t,
        }
    }

    fn signal(&self, sig: libc::c_int) {
        if self.pgid > 0 {
            // SAFETY: 送訊號給自己開的 process group（負數 = 整個群組）。群組的領頭程序結束、被收回之後，
            // 編號要等到 pid 繞一圈才可能被重新使用，這裡最多晚幾秒送出
            unsafe {
                libc::kill(-self.pgid, sig);
            }
        }
    }

    /// 正常結束後：群組裡剩下的程序結束
    fn kill_rest(self) {
        self.signal(libc::SIGKILL);
    }

    /// 取消：先請它們自己結束
    fn terminate_softly(&self) {
        self.signal(libc::SIGTERM);
    }

    fn kill_all(self, child: &mut Child) {
        self.signal(libc::SIGKILL);
        let _ = child.kill();
    }
}

// ───────────── 工具的使用規則 ─────────────

/// 同一個執行檔的共用／獨占鎖（像 RwLock，但放手的程序另外計算）
pub struct ToolLock {
    state: Mutex<LockState>,
    cv: Condvar,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LockState {
    /// 正在等結果的執行
    pub active: usize,
    /// 已經放手、還沒結束的程序
    pub abandoned: usize,
    /// 正在更新（獨占）
    pub exclusive: bool,
}

/// 這個執行檔的鎖（同一個路徑共用同一個）
pub fn tool_lock(program: &Path) -> Arc<ToolLock> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<ToolLock>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks
        .entry(program.to_path_buf())
        .or_insert_with(|| {
            Arc::new(ToolLock {
                state: Mutex::default(),
                cv: Condvar::new(),
            })
        })
        .clone()
}

impl ToolLock {
    fn lock(&self) -> std::sync::MutexGuard<'_, LockState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 目前的狀態（測試、狀態列用）
    pub fn state(&self) -> LockState {
        *self.lock()
    }

    /// 共用鎖：更新中時等它做完（不等放手的程序）。等的時候取消或超過 `until` 就放棄
    pub fn shared_until(self: &Arc<Self>, cancel: &AtomicBool, until: Instant) -> Result<SharedGuard, RunError> {
        let mut s = self.lock();
        while s.exclusive {
            if cancel.load(Ordering::Relaxed) {
                return Err(RunError::Cancelled);
            }
            let now = Instant::now();
            if now >= until {
                return Err(RunError::TimedOut);
            }
            s = self
                .cv
                .wait_timeout(s, (until - now).min(Duration::from_millis(50)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        s.active += 1;
        Ok(SharedGuard(self.clone()))
    }

    /// 獨占鎖（更新用）：等所有執行、放手的程序都結束；超過 `timeout` 時 None
    pub fn exclusive(self: &Arc<Self>, timeout: Duration) -> Option<ExclusiveGuard> {
        let until = Instant::now() + timeout;
        let mut s = self.lock();
        while s.exclusive || s.active > 0 || s.abandoned > 0 {
            let now = Instant::now();
            if now >= until {
                return None;
            }
            s = self
                .cv
                .wait_timeout(s, until - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        s.exclusive = true;
        Some(ExclusiveGuard(self.clone()))
    }
}

/// 共用鎖；放開時（執行結束）計數減一
pub struct SharedGuard(Arc<ToolLock>);

impl SharedGuard {
    /// 不等了：改算成放手的程序（程序真的結束時才放開）
    pub fn abandon(self) -> AbandonedGuard {
        let lock = self.0.clone();
        {
            let mut s = lock.lock();
            s.abandoned += 1;
            s.active -= 1;
        }
        std::mem::forget(self);
        lock.cv.notify_all();
        AbandonedGuard(lock)
    }
}

impl Drop for SharedGuard {
    fn drop(&mut self) {
        self.0.lock().active -= 1;
        self.0.cv.notify_all();
    }
}

/// 放手、還沒結束的程序
pub struct AbandonedGuard(Arc<ToolLock>);

impl Drop for AbandonedGuard {
    fn drop(&mut self) {
        self.0.lock().abandoned -= 1;
        self.0.cv.notify_all();
    }
}

/// 獨占鎖（更新中）
pub struct ExclusiveGuard(Arc<ToolLock>);

impl Drop for ExclusiveGuard {
    fn drop(&mut self) {
        self.0.lock().exclusive = false;
        self.0.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(name: &str) -> Arc<ToolLock> {
        tool_lock(&std::env::temp_dir().join(format!("vitascope-lock-test-{name}-{}", std::process::id())))
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn resolves_share_and_updates_wait_for_them() {
        let lock = fresh("share");
        let no = AtomicBool::new(false);
        let a = lock.shared_until(&no, far()).unwrap();
        let b = lock.shared_until(&no, far()).unwrap();
        assert_eq!(lock.state().active, 2);
        assert!(lock.exclusive(Duration::from_millis(50)).is_none(), "解析中不能更新");
        drop(a);
        drop(b);
        let x = lock.exclusive(Duration::from_millis(50)).expect("沒有人用時可以更新");
        // 更新中，新的解析要等；取消時放棄
        let cancel = AtomicBool::new(true);
        assert!(matches!(lock.shared_until(&cancel, far()), Err(RunError::Cancelled)));
        assert!(matches!(
            lock.shared_until(&no, Instant::now() + Duration::from_millis(30)),
            Err(RunError::TimedOut)
        ));
        drop(x);
        assert!(lock.shared_until(&no, far()).is_ok());
    }

    #[test]
    fn abandoned_processes_block_updates_but_not_new_resolves() {
        let lock = fresh("abandon");
        let no = AtomicBool::new(false);
        let g = lock.shared_until(&no, far()).unwrap();
        let abandoned = g.abandon();
        assert_eq!(
            lock.state(),
            LockState {
                active: 0,
                abandoned: 1,
                exclusive: false
            }
        );
        // 新的解析不等放手的程序
        let again = lock.shared_until(&no, Instant::now() + Duration::from_millis(30));
        assert!(again.is_ok());
        drop(again);
        // 更新要等它真的結束
        assert!(lock.exclusive(Duration::from_millis(50)).is_none());
        let l2 = lock.clone();
        let waiter = std::thread::spawn(move || l2.exclusive(Duration::from_secs(10)).is_some());
        std::thread::sleep(Duration::from_millis(50));
        drop(abandoned);
        assert!(waiter.join().unwrap(), "放手的程序結束後就能更新");
        assert_eq!(lock.state(), LockState::default());
    }

    #[test]
    fn a_waiting_resolve_continues_after_the_update() {
        let lock = fresh("after");
        let x = lock.exclusive(Duration::from_secs(1)).unwrap();
        let l2 = lock.clone();
        let waiter = std::thread::spawn(move || {
            let no = AtomicBool::new(false);
            l2.shared_until(&no, Instant::now() + Duration::from_secs(10)).is_ok()
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(x);
        assert!(waiter.join().unwrap());
    }
}
