//! 找 yt-dlp 與 deno。
//!
//! 找的順序（找到第一個就用）：
//! 1. 使用者在設定裡指定的檔案。
//! 2. 影戲下載的（工具資料夾，[`crate::paths::tools_dir`]）。
//! 3. 影戲執行檔旁邊（免安裝版的使用者自己放的）。
//! 4. PATH，加上從圖形介面啟動時 PATH 沒有的常見位置（winget、scoop、Homebrew、~/.local/bin…）。
//!
//! 只看檔案在不在，之後用完整路徑執行（不靠系統的搜尋順序）。版本要執行 `--version` 才知道：
//! 尋找在背景執行緒做（第一次播網站影片、打開網路設定時），結果記住（[`Locator`]），介面執行緒從不等。
//! 版本依（路徑、修改時間、大小）記住，檔案沒變就不再執行。
//!
//! deno（YouTube 要的 JavaScript 執行環境）2.3 以上才算有：yt-dlp 預設只用 deno，而且要 2.3 以上。
//! 不傳 `--js-runtimes`，改成子程序的 PATH 最前面放找到的 deno：任何版本的 yt-dlp 都找得到。

use super::YtdlError;
use super::run::{self, ChildEnv, Limits, Lock};
use crate::instance::Wake;
use crate::paths::Os;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

/// 背景尋找最多等多久（執行兩次 `--version`；第一次執行 PyInstaller 的程式、防毒掃描時可能很慢）
pub const SEARCH_WAIT: Duration = Duration::from_secs(90);
/// 查版本最多等多久
const PROBE_DEADLINE: Duration = Duration::from_secs(30);
/// 影戲下載的 yt-dlp 超過幾天算舊（提醒更新；不自動連網）
pub const STALE_DAYS: i64 = 30;

/// 找到的程式從哪裡來
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 設定裡指定的
    UserPath,
    /// 影戲下載的（只有這個能由影戲更新、移除）
    Managed,
    /// 影戲執行檔旁邊
    NextToApp,
    /// PATH 或常見的安裝位置
    System,
}

/// 要執行的程式：完整路徑，加上放在參數最前面的固定參數（測試用 Python 執行假的 yt-dlp 時是腳本的路徑）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub program: PathBuf,
    pub prefix_args: Vec<OsString>,
    pub source: Source,
}

impl Located {
    pub fn new(program: impl Into<PathBuf>, source: Source) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
            source,
        }
    }

    /// 影戲下載的那一份（能更新、移除）
    pub fn managed(&self) -> bool {
        self.source == Source::Managed
    }
}

/// yt-dlp 的版本（`2026.08.19`，每日建置多一段 `.232934`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub rev: u32,
}

impl Version {
    /// `yt-dlp --version` 的輸出（第一個不是空白的行）
    pub fn parse(text: &str) -> Option<Version> {
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        let token = line.split_whitespace().next()?;
        let mut parts = token.split('.');
        let num = |s: Option<&str>| s.and_then(|s| s.parse::<u32>().ok());
        let year = num(parts.next())?;
        let month = num(parts.next())?;
        let day = num(parts.next())?;
        let rev = match parts.next() {
            Some(r) => r.parse().ok()?,
            None => 0,
        };
        if parts.next().is_some()
            || !(2000..=9999).contains(&year)
            || !(1..=12).contains(&month)
            || !(1..=31).contains(&day)
        {
            return None;
        }
        Some(Version {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            rev,
        })
    }

    /// 1970-01-01 起的第幾天
    fn days(self) -> i64 {
        days_from_civil(i64::from(self.year), i64::from(self.month), i64::from(self.day))
    }

    /// 發佈到 `now` 過了幾天
    pub fn age_days(self, now: SystemTime) -> i64 {
        let today = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| (d.as_secs() / 86_400) as i64);
        today - self.days()
    }

    /// 舊到該提醒更新了（[`STALE_DAYS`]）
    pub fn is_stale(self, now: SystemTime) -> bool {
        self.age_days(now) > STALE_DAYS
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}.{:02}.{:02}", self.year, self.month, self.day)?;
        if self.rev > 0 {
            write!(f, ".{}", self.rev)?;
        }
        Ok(())
    }
}

/// 公曆日期 → 1970-01-01 起的天數（Howard Hinnant 的算法）
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// deno 的版本（`deno 2.9.7 (stable, release, …)`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DenoVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl DenoVersion {
    /// yt-dlp 能用的最低版本
    pub const MIN: DenoVersion = DenoVersion {
        major: 2,
        minor: 3,
        patch: 0,
    };

    pub fn parse(text: &str) -> Option<DenoVersion> {
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
        let mut words = line.split_whitespace();
        if words.next()? != "deno" {
            return None;
        }
        let mut nums = words.next()?.split(['.', '-', '+']).map(|n| n.parse::<u32>().ok());
        Some(DenoVersion {
            major: nums.next()??,
            minor: nums.next()??,
            patch: nums.next().flatten().unwrap_or(0),
        })
    }

    pub fn supported(self) -> bool {
        self >= Self::MIN
    }
}

impl std::fmt::Display for DenoVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 找到的 deno
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deno {
    pub path: PathBuf,
    pub version: DenoVersion,
    /// 影戲下載的
    pub managed: bool,
}

/// 找的結果
#[derive(Debug, Clone, PartialEq)]
pub struct Tools {
    pub ytdl: Option<Located>,
    /// yt-dlp 的版本；None = 執行 `--version` 失敗（原因在 `ytdl_error`）或看不懂
    pub ytdl_version: Option<Version>,
    /// 執行 `--version` 失敗的原因（例如被 Windows 擋下）
    pub ytdl_error: Option<YtdlError>,
    /// 2.3 以上的 deno
    pub deno: Option<Deno>,
    /// 找到了但太舊的 deno（設定頁說明要更新）
    pub deno_too_old: Option<(PathBuf, DenoVersion)>,
    /// 執行 yt-dlp 的環境
    pub env: ChildEnv,
}

impl Tools {
    /// 什麼都沒找到
    pub fn none(env: &SearchEnv) -> Tools {
        Tools {
            ytdl: None,
            ytdl_version: None,
            ytdl_error: None,
            deno: None,
            deno_too_old: None,
            env: child_env(env, None),
        }
    }

    pub fn child_env(&self) -> ChildEnv {
        self.env.clone()
    }
}

/// 尋找用的環境（測試可以換成假的資料夾，不會找到使用者真的安裝的程式）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchEnv {
    pub os: Option<Os>,
    /// 設定裡指定的 yt-dlp
    pub user_path: Option<PathBuf>,
    /// 影戲的工具資料夾
    pub tools_dir: Option<PathBuf>,
    /// 影戲執行檔所在的資料夾
    pub exe_dir: Option<PathBuf>,
    /// 繼承的 PATH
    pub path_var: Option<OsString>,
    /// 家目錄（Windows 是 %USERPROFILE%）
    pub home: Option<PathBuf>,
    /// Windows 的 %LOCALAPPDATA%
    pub local_app_data: Option<PathBuf>,
    /// 也找系統的固定位置（Homebrew、/usr/local/bin…）；測試關掉，不會找到這台電腦真的安裝的程式
    pub system_dirs: bool,
}

impl SearchEnv {
    /// 這台電腦的環境
    pub fn current(user_path: Option<PathBuf>, tools_dir: Option<PathBuf>) -> SearchEnv {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let os = Os::current();
        SearchEnv {
            os: Some(os),
            user_path,
            tools_dir,
            exe_dir: std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf)),
            path_var: var("PATH"),
            home: var(if os == Os::Windows { "USERPROFILE" } else { "HOME" }).map(PathBuf::from),
            local_app_data: var("LOCALAPPDATA").map(PathBuf::from),
            system_dirs: true,
        }
    }

    fn os(&self) -> Os {
        self.os.unwrap_or_else(Os::current)
    }

    /// 工具資料夾（只要完整路徑：相對路徑會跟著目前資料夾變，不能從那裡執行程式）
    fn tools(&self) -> Option<&Path> {
        self.tools_dir.as_deref().filter(|d| d.is_absolute())
    }

    /// PATH 裡的資料夾（只要完整路徑：相對路徑會變成「目前資料夾」，不能從那裡執行程式）
    fn path_dirs(&self) -> Vec<PathBuf> {
        let Some(p) = &self.path_var else {
            return Vec::new();
        };
        std::env::split_paths(p)
            .map(|d| {
                // Windows 的 PATH 項目可能有引號
                let s = d.to_string_lossy();
                let t = s.trim().trim_matches('"');
                if t.len() == s.len() { d } else { PathBuf::from(t) }
            })
            .filter(|d| d.is_absolute())
            .collect()
    }

    /// 從圖形介面啟動時 PATH 常常沒有的位置（Finder 給的 PATH 只有 /usr/bin:/bin:/usr/sbin:/sbin）
    fn extra_dirs(&self) -> Vec<PathBuf> {
        let home = self.home.as_deref();
        let mut dirs = Vec::new();
        let fixed = |list: &[&str]| -> Vec<PathBuf> {
            if self.system_dirs {
                list.iter().map(PathBuf::from).collect()
            } else {
                Vec::new()
            }
        };
        match self.os() {
            Os::Windows => {
                dirs.extend(self.local_app_data.as_ref().map(|d| d.join(r"Microsoft\WinGet\Links")));
                dirs.extend(home.map(|h| h.join(r"scoop\shims")));
            }
            Os::Macos => {
                dirs.extend(fixed(&["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]));
                dirs.extend(home.map(|h| h.join(".local/bin")));
            }
            Os::Linux => {
                dirs.extend(home.map(|h| h.join(".local/bin")));
                dirs.extend(fixed(&["/usr/local/bin", "/usr/bin", "/snap/bin"]));
            }
        }
        // 家目錄是相對路徑（環境變數設錯）時不算
        dirs.retain(|d| d.has_root());
        dirs
    }

    /// yt-dlp 的檔名（找的時候依序試）
    fn ytdl_names(&self) -> &'static [&'static str] {
        match self.os() {
            Os::Windows => &["yt-dlp.exe"],
            Os::Macos => &["yt-dlp", "yt-dlp_macos"],
            Os::Linux => &["yt-dlp", "yt-dlp_linux"],
        }
    }

    fn deno_name(&self) -> &'static str {
        if self.os() == Os::Windows { "deno.exe" } else { "deno" }
    }

    /// 影戲下載的 yt-dlp 放在哪裡（工具資料夾裡）
    pub fn managed_ytdl(&self) -> Option<PathBuf> {
        let name = if self.os() == Os::Windows {
            "yt-dlp.exe"
        } else {
            "yt-dlp"
        };
        self.tools().map(|d| d.join(name))
    }

    /// 影戲下載的 deno 放在哪裡
    pub fn managed_deno(&self) -> Option<PathBuf> {
        self.tools().map(|d| d.join(self.deno_name()))
    }
}

/// 能執行的檔案（Unix 要有執行權限）
fn runnable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// 使用者選的 yt-dlp 為什麼不能用（只看路徑本身）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathProblem {
    /// 不是完整路徑（相對路徑會跟著目前資料夾變）
    NotAbsolute,
    /// Windows 上不是 .exe：.bat、.cmd、.ps1、.vbs、.js 之類的指令檔要經過 cmd、PowerShell、Windows Script Host 才能執行，
    /// 命令列的引號規則不同，網址裡的字元可能被當成指令
    NotExe,
}

impl PathProblem {
    pub fn message(self) -> &'static str {
        match self {
            PathProblem::NotAbsolute => crate::tr!("要選完整的路徑", "Choose a full path"),
            PathProblem::NotExe => crate::tr!(
                "請選 yt-dlp 的執行檔（.exe），不能用指令檔",
                "Choose the yt-dlp program (.exe), not a script"
            ),
        }
    }
}

/// 使用者選的 yt-dlp 能不能用：只看路徑（不碰檔案，介面執行緒可以用；檔案在不在、能不能執行由背景的尋找確認）
pub fn check_user_path(path: &Path, os: Os) -> Result<(), PathProblem> {
    let text = path.to_string_lossy();
    if !crate::paths::absolute(os, &text) {
        return Err(PathProblem::NotAbsolute);
    }
    if os == Os::Windows {
        let name = text.rsplit(['\\', '/']).next().unwrap_or_default();
        let exe = name
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && ext.eq_ignore_ascii_case("exe"));
        if !exe {
            return Err(PathProblem::NotExe);
        }
    }
    Ok(())
}

/// 找 yt-dlp（只看檔案，不執行）
pub fn locate_ytdl(env: &SearchEnv) -> Option<Located> {
    if let Some(p) = env.user_path.as_ref().filter(|p| p.is_absolute() && runnable(p)) {
        return Some(Located::new(p, Source::UserPath));
    }
    if let Some(p) = env.managed_ytdl().filter(|p| runnable(p)) {
        return Some(Located::new(p, Source::Managed));
    }
    let names = env.ytdl_names();
    let in_dir = |dir: &Path| names.iter().map(|n| dir.join(n)).find(|p| runnable(p));
    if let Some(p) = env.exe_dir.as_deref().and_then(in_dir) {
        return Some(Located::new(p, Source::NextToApp));
    }
    env.path_dirs()
        .into_iter()
        .chain(env.extra_dirs())
        .find_map(|d| in_dir(&d))
        .map(|p| Located::new(p, Source::System))
}

/// deno 可能在的位置，照優先順序（跟 yt-dlp 一樣：影戲下載的、影戲旁邊；接著 yt-dlp 旁邊、PATH、常見位置、
/// ~/.deno/bin）；不重複，只要完整路徑
pub fn deno_candidates(env: &SearchEnv, ytdl: Option<&Path>) -> Vec<PathBuf> {
    let name = env.deno_name();
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(env.tools().map(Path::to_path_buf));
    // 免安裝版的使用者放在影戲旁邊的
    dirs.extend(env.exe_dir.clone());
    // Windows 的 yt-dlp 也會找自己旁邊的 deno
    dirs.extend(ytdl.and_then(Path::parent).map(Path::to_path_buf));
    dirs.extend(env.path_dirs());
    dirs.extend(env.extra_dirs());
    dirs.extend(env.home.as_ref().map(|h| h.join(".deno").join("bin")));
    let mut out: Vec<PathBuf> = Vec::new();
    for d in dirs.into_iter().filter(|d| d.is_absolute()) {
        let p = d.join(name);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// 執行 yt-dlp 的環境：PATH = 找到的 deno 的資料夾、工具資料夾、常見位置、原本的 PATH（不重複）；
/// 目前資料夾 = 工具資料夾（還沒建立時用系統暫存資料夾）
pub fn child_env(env: &SearchEnv, deno_dir: Option<&Path>) -> ChildEnv {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut add = |d: PathBuf| {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Some(d) = deno_dir {
        add(d.to_path_buf());
    }
    if let Some(d) = env.tools() {
        add(d.to_path_buf());
    }
    for d in env.extra_dirs() {
        add(d);
    }
    let inherited: Vec<PathBuf> = env
        .path_var
        .as_ref()
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    for d in inherited {
        add(d);
    }
    let cwd = env
        .tools()
        .filter(|d| d.is_dir())
        .map_or_else(std::env::temp_dir, Path::to_path_buf);
    ChildEnv {
        path: std::env::join_paths(dirs).ok(),
        cwd: Some(cwd),
    }
}

/// 執行 `程式 --version` 拿輸出（測試換成假的）
pub type Probe = dyn Fn(&Located, &ChildEnv) -> Result<String, YtdlError> + Send + Sync;

/// 找 yt-dlp、deno，並查版本（會執行程式：只在背景執行緒呼叫）
pub fn find_tools(env: &SearchEnv, probe: &Probe) -> Tools {
    let mut tools = Tools::none(env);
    // 設定裡指定的 yt-dlp 只看路徑就不能用的（Windows 的指令檔之類：設定檔可能是手動改的、別的視窗存的）：
    // 不執行它，照常找（設定頁說明指定的檔案不能用）。跟「選擇檔案…」用同一個規則
    let usable = |p: &PathBuf| check_user_path(p, env.os()).is_ok();
    let checked;
    let ytdl_env = if env.user_path.as_ref().is_none_or(usable) {
        env
    } else {
        checked = SearchEnv {
            user_path: None,
            ..env.clone()
        };
        &checked
    };
    tools.ytdl = locate_ytdl(ytdl_env);
    for cand in deno_candidates(env, tools.ytdl.as_ref().map(|l| l.program.as_path())) {
        if !runnable(&cand) {
            continue;
        }
        let managed = env.managed_deno().as_deref() == Some(cand.as_path());
        let located = Located::new(&cand, if managed { Source::Managed } else { Source::System });
        let Some(version) = probe(&located, &tools.env)
            .ok()
            .and_then(|out| DenoVersion::parse(&out))
        else {
            continue;
        };
        if version.supported() {
            tools.deno = Some(Deno {
                path: cand,
                version,
                managed,
            });
            break;
        }
        if tools.deno_too_old.is_none() {
            tools.deno_too_old = Some((cand, version));
        }
    }
    tools.env = child_env(env, tools.deno.as_ref().and_then(|d| d.path.parent()));
    if let Some(ytdl) = &tools.ytdl {
        match probe(ytdl, &tools.env) {
            Ok(out) => tools.ytdl_version = Version::parse(&out),
            Err(e) => tools.ytdl_error = Some(e),
        }
    }
    tools
}

/// 真的執行 `程式 --version`；同一個檔案（路徑、修改時間、大小都一樣）只執行一次
pub fn probe_version(program: &Located, env: &ChildEnv) -> Result<String, YtdlError> {
    type Key = (PathBuf, Vec<OsString>);
    type Stamp = (Option<SystemTime>, u64);
    static CACHE: OnceLock<Mutex<HashMap<Key, (Stamp, String)>>> = OnceLock::new();
    let stamp = std::fs::metadata(&program.program)
        .map(|m| (m.modified().ok(), m.len()))
        .unwrap_or((None, 0));
    let key = (program.program.clone(), program.prefix_args.clone());
    let cache = CACHE.get_or_init(Default::default);
    if let Some((s, out)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key)
        && *s == stamp
    {
        return Ok(out.clone());
    }
    let limits = Limits {
        deadline: PROBE_DEADLINE,
        stdout_cap: 64 * 1024,
        stderr_cap: 64 * 1024,
        ..Limits::default()
    };
    let never = AtomicBool::new(false);
    let out = match run::run(
        program,
        &[OsString::from("--version")],
        env,
        &limits,
        Lock::Shared,
        &never,
    ) {
        Ok(out) => out,
        Err(run::RunError::Spawn(e)) => return Err(super::errors::spawn_error(&e)),
        Err(_) => return Err(YtdlError::NoResponse),
    };
    if !out.success {
        return Err(super::errors::describe(&out.stderr, out.code, None));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (stamp, text.clone()));
    Ok(text)
}

// ───────────── 背景尋找 ─────────────

/// 尋找的方法（測試換成假的）
pub type Finder = dyn Fn(&SearchEnv) -> Tools + Send + Sync;

/// 在背景找 yt-dlp、deno，找到後記住。複製出來的都是同一個（`Clone` 共用狀態）
#[derive(Clone)]
pub struct Locator {
    shared: Arc<Shared>,
}

struct Shared {
    tools_dir: Option<PathBuf>,
    finder: Arc<Finder>,
    state: Mutex<LocState>,
    cv: Condvar,
    wake: Mutex<Option<Wake>>,
}

#[derive(Default)]
struct LocState {
    /// 設定裡指定的 yt-dlp
    user_path: Option<PathBuf>,
    /// 每次要重新找（指定的檔案改了、下載或移除了）就加一
    generation: u64,
    /// 背景執行緒正在找
    running: bool,
    /// 找到的結果與當時的 `generation`
    done: Option<(u64, Arc<Tools>)>,
    /// `done` 是什麼時候找完的
    found_at: Option<std::time::Instant>,
}

impl Locator {
    /// `tools_dir` = 影戲的工具資料夾（自動測試是 None：不找影戲下載的）
    pub fn new(tools_dir: Option<PathBuf>) -> Locator {
        Self::with_finder(tools_dir, Arc::new(|env: &SearchEnv| find_tools(env, &probe_version)))
    }

    /// 測試用：換掉尋找的方法
    #[doc(hidden)]
    pub fn with_finder(tools_dir: Option<PathBuf>, finder: Arc<Finder>) -> Locator {
        Locator {
            shared: Arc::new(Shared {
                tools_dir,
                finder,
                state: Mutex::default(),
                cv: Condvar::new(),
                wake: Mutex::new(None),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LocState> {
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 找完時叫醒介面
    pub fn set_wake(&self, wake: Wake) {
        *self.shared.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
    }

    pub fn tools_dir(&self) -> Option<&Path> {
        self.shared.tools_dir.as_deref()
    }

    /// 設定裡指定的 yt-dlp（改了才重新找）
    pub fn set_user_path(&self, path: Option<PathBuf>) {
        let mut s = self.lock();
        if s.user_path != path {
            s.user_path = path;
            s.generation += 1;
        }
    }

    /// 重新找（下載、移除、更新之後）
    pub fn refresh(&self) {
        self.lock().generation += 1;
    }

    /// 找過了（或正在找）
    pub fn started(&self) -> bool {
        let s = self.lock();
        s.running || s.done.is_some()
    }

    /// 找到的結果；還沒找（或要重新找）時在背景開始找。不會等：
    /// 從來沒找完過時 None；重新找的時候先回傳上一次的結果（設定頁不會閃成「找不到」，要知道正在找用 [`Self::searching`]）
    pub fn get(&self) -> Option<Arc<Tools>> {
        let mut s = self.lock();
        let current = s.done.as_ref().is_some_and(|(g, _)| *g == s.generation);
        if !current && !s.running {
            s.running = true;
            let previous = s.done.as_ref().map(|(_, t)| t.clone());
            drop(s);
            self.spawn_search();
            return previous;
        }
        s.done.as_ref().map(|(_, t)| t.clone())
    }

    /// 正在找（第一次，或設定改了之後重新找）
    pub fn searching(&self) -> bool {
        self.lock().running
    }

    /// 目前的結果找完多久了（還沒找完、要重新找時 None）
    pub fn age(&self) -> Option<Duration> {
        let s = self.lock();
        let current = s.done.as_ref().is_some_and(|(g, _)| *g == s.generation);
        s.found_at.filter(|_| current).map(|t| t.elapsed())
    }

    /// 等找完目前要的結果（背景執行緒用；介面執行緒不要用）：不回傳設定改之前的舊結果。超過 `timeout` 時 None
    pub fn wait(&self, timeout: Duration) -> Option<Arc<Tools>> {
        let until = std::time::Instant::now() + timeout;
        let mut s = self.lock();
        loop {
            if let Some((g, t)) = &s.done
                && *g == s.generation
            {
                return Some(t.clone());
            }
            if !s.running {
                // 剛好被要求重新找：再開始一次
                s.running = true;
                drop(s);
                self.spawn_search();
                s = self.lock();
                continue;
            }
            let now = std::time::Instant::now();
            if now >= until {
                return None;
            }
            s = self
                .shared
                .cv
                .wait_timeout(s, until - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn spawn_search(&self) {
        let me = self.clone();
        let spawned = std::thread::Builder::new()
            .name("vitascope-ytdl-locate".into())
            .spawn(move || me.search_loop());
        if let Err(e) = spawned {
            eprintln!("[vitascope] 無法開執行緒尋找 yt-dlp：{e}");
            self.lock().running = false;
        }
    }

    /// 背景：找到目前要的（找的時候設定又改了就再找一次）
    fn search_loop(&self) {
        loop {
            let (generation, user_path) = {
                let s = self.lock();
                (s.generation, s.user_path.clone())
            };
            let env = SearchEnv::current(user_path, self.shared.tools_dir.clone());
            let tools = Arc::new((self.shared.finder)(&env));
            let mut s = self.lock();
            s.done = Some((generation, tools));
            s.found_at = Some(std::time::Instant::now());
            if s.generation == generation {
                s.running = false;
                drop(s);
                // 先叫醒介面再通知等的人：等到結果的人看得到「已經叫醒過了」
                if let Some(w) = self.shared.wake.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    w();
                }
                self.shared.cv.notify_all();
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        let v = Version::parse("2026.08.19\n").unwrap();
        assert_eq!((v.year, v.month, v.day, v.rev), (2026, 8, 19, 0));
        assert_eq!(v.to_string(), "2026.08.19");
        let nightly = Version::parse("  2026.08.19.232934 \n").unwrap();
        assert!(nightly > v);
        assert_eq!(nightly.to_string(), "2026.08.19.232934");
        for bad in [
            "",
            "yt-dlp",
            "2026.13.01",
            "26.08.19",
            "2026.08",
            "2026.08.19.x",
            "1.2.3.4.5",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn version_age() {
        let v = Version::parse("2026.08.19").unwrap();
        let day = |n: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(n * 86_400 + 3600);
        let release = v.days() as u64;
        assert_eq!(v.age_days(day(release)), 0);
        assert_eq!(v.age_days(day(release + 30)), 30);
        assert!(!v.is_stale(day(release + 30)));
        assert!(v.is_stale(day(release + 31)));
        // 已知的日期
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 10), 20_736);
    }

    #[test]
    fn deno_version() {
        let v = DenoVersion::parse("deno 2.9.7 (stable, release, x86_64-pc-windows-msvc)\nv8 14.0\ntypescript 5.9")
            .unwrap();
        assert_eq!(v.to_string(), "2.9.7");
        assert!(v.supported());
        assert!(!DenoVersion::parse("deno 2.2.12 (stable)").unwrap().supported());
        assert!(DenoVersion::parse("deno 2.3.0").unwrap().supported());
        assert!(!DenoVersion::parse("deno 1.46.3").unwrap().supported());
        assert!(DenoVersion::parse("deno 3.0.0-rc.1").unwrap().supported());
        assert_eq!(DenoVersion::parse("node v22"), None);
        assert_eq!(DenoVersion::parse(""), None);
    }

    /// 暫存資料夾裡的假程式（只有檔案，不能執行）
    struct Dirs {
        root: PathBuf,
    }

    impl Dirs {
        fn new(name: &str) -> Dirs {
            let root = std::env::temp_dir().join(format!("vitascope-locate-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Dirs { root }
        }

        fn dir(&self, name: &str) -> PathBuf {
            let d = self.root.join(name);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        fn file(&self, dir: &str, name: &str) -> PathBuf {
            let p = self.dir(dir).join(name);
            std::fs::write(&p, b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            p
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn env_in(d: &Dirs, path_dirs: &[&str]) -> SearchEnv {
        SearchEnv {
            os: Some(Os::current()),
            user_path: None,
            tools_dir: Some(d.root.join("tools")),
            exe_dir: Some(d.root.join("app")),
            path_var: std::env::join_paths(path_dirs.iter().map(|p| d.root.join(p))).ok(),
            home: Some(d.root.join("home")),
            local_app_data: Some(d.root.join("local")),
            system_dirs: false,
        }
    }

    fn exe(name: &str) -> String {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        }
    }

    #[test]
    fn chosen_ytdl_paths_are_checked_by_name_only() {
        // Windows：只能是 .exe（指令檔要經過 cmd、PowerShell 之類，引號規則不同）
        assert_eq!(check_user_path(Path::new(r"C:\Tools\yt-dlp.exe"), Os::Windows), Ok(()));
        assert_eq!(check_user_path(Path::new(r"D:\x\YT-DLP.EXE"), Os::Windows), Ok(()));
        assert_eq!(
            check_user_path(Path::new(r"\\nas\tools\yt-dlp.exe"), Os::Windows),
            Ok(())
        );
        for bad in [
            r"C:\Tools\yt-dlp.bat",
            r"C:\Tools\yt-dlp.cmd",
            r"C:\Tools\yt-dlp.ps1",
            r"C:\Tools\yt-dlp.vbs",
            r"C:\Tools\yt-dlp.js",
            r"C:\Tools\yt-dlp",
            r"C:\Tools\.exe",
            r"C:\Tools.exe\yt-dlp",
        ] {
            assert_eq!(
                check_user_path(Path::new(bad), Os::Windows),
                Err(PathProblem::NotExe),
                "{bad}"
            );
        }
        assert_eq!(
            check_user_path(Path::new(r"tools\yt-dlp.exe"), Os::Windows),
            Err(PathProblem::NotAbsolute)
        );
        // macOS、Linux：沒有副檔名也可以（能不能執行由背景的尋找確認）
        for os in [Os::Macos, Os::Linux] {
            assert_eq!(check_user_path(Path::new("/opt/homebrew/bin/yt-dlp"), os), Ok(()));
            assert_eq!(check_user_path(Path::new("/home/me/yt-dlp_linux"), os), Ok(()));
            assert_eq!(
                check_user_path(Path::new("bin/yt-dlp"), os),
                Err(PathProblem::NotAbsolute)
            );
        }
    }

    #[test]
    fn discovery_order() {
        let d = Dirs::new("order");
        let env = env_in(&d, &["p1", "p2"]);
        assert_eq!(locate_ytdl(&env), None);

        // PATH：前面的資料夾優先
        let p2 = d.file("p2", &exe("yt-dlp"));
        assert_eq!(locate_ytdl(&env), Some(Located::new(&p2, Source::System)));
        let p1 = d.file("p1", &exe("yt-dlp"));
        assert_eq!(locate_ytdl(&env).unwrap().program, p1);
        // 影戲旁邊的優先於 PATH
        let app = d.file("app", &exe("yt-dlp"));
        assert_eq!(locate_ytdl(&env), Some(Located::new(&app, Source::NextToApp)));
        // 影戲下載的優先於旁邊的
        let managed = d.file("tools", &exe("yt-dlp"));
        let found = locate_ytdl(&env).unwrap();
        assert_eq!(found, Located::new(&managed, Source::Managed));
        assert!(found.managed());
        // 設定裡指定的最優先；指定的檔案不見了就照常找
        let mine = d.file("mine", "my-ytdlp-build");
        let with_user = SearchEnv {
            user_path: Some(mine.clone()),
            ..env.clone()
        };
        assert_eq!(locate_ytdl(&with_user), Some(Located::new(&mine, Source::UserPath)));
        let missing = SearchEnv {
            user_path: Some(d.root.join("gone").join("yt-dlp")),
            ..env.clone()
        };
        assert_eq!(locate_ytdl(&missing).unwrap().program, managed);
        // 沒有工具資料夾（自動測試）：不找影戲下載的
        let no_tools = SearchEnv { tools_dir: None, ..env };
        assert_eq!(locate_ytdl(&no_tools).unwrap().program, app);
    }

    #[test]
    fn chosen_paths_that_fail_the_name_check_are_never_run() {
        let d = Dirs::new("chosen-check");
        let env = env_in(&d, &["p1"]);
        let found = d.file("p1", &exe("yt-dlp"));
        let probe = |_: &Located, _: &ChildEnv| -> Result<String, YtdlError> {
            Ok("2026.08.19
"
            .into())
        };
        // 能用的：用指定的那一個
        let mine = d.file("mine", &exe("my-yt-dlp"));
        let with = |p: &Path| SearchEnv {
            user_path: Some(p.to_path_buf()),
            ..env.clone()
        };
        let t = find_tools(&with(&mine), &probe);
        assert_eq!(t.ytdl, Some(Located::new(&mine, Source::UserPath)));
        // Windows 的指令檔（設定檔裡手動改的）：存在也不執行，照常找
        let script = d.file("mine", "yt-dlp.cmd");
        let windows = SearchEnv {
            os: Some(Os::Windows),
            ..with(&script)
        };
        let t = find_tools(&windows, &probe);
        assert_ne!(
            t.ytdl.as_ref().map(|l| l.source),
            Some(Source::UserPath),
            "{:?}",
            t.ytdl
        );
        if cfg!(windows) {
            assert_eq!(t.ytdl, Some(Located::new(&found, Source::System)));
        }
    }

    #[test]
    fn places_gui_launches_miss_are_searched() {
        let d = Dirs::new("extra");
        let env = env_in(&d, &[]);
        let found = match Os::current() {
            Os::Windows => d.file(r"local\Microsoft\WinGet\Links", "yt-dlp.exe"),
            _ => d.file("home/.local/bin", "yt-dlp"),
        };
        assert_eq!(locate_ytdl(&env), Some(Located::new(&found, Source::System)));
    }

    #[test]
    fn fixed_places_per_system() {
        let env = |os| SearchEnv {
            os: Some(os),
            home: Some(PathBuf::from("/h")),
            local_app_data: Some(PathBuf::from("/l")),
            system_dirs: true,
            ..SearchEnv::default()
        };
        let mac = env(Os::Macos).extra_dirs();
        assert_eq!(mac[0], PathBuf::from("/opt/homebrew/bin"));
        assert!(mac.contains(&PathBuf::from("/h").join(".local/bin")));
        let linux = env(Os::Linux).extra_dirs();
        assert!(linux.contains(&PathBuf::from("/usr/bin")) && linux.contains(&PathBuf::from("/snap/bin")));
        let win = env(Os::Windows).extra_dirs();
        assert_eq!(
            win,
            [
                PathBuf::from("/l").join(r"Microsoft\WinGet\Links"),
                PathBuf::from("/h").join(r"scoop\shims")
            ]
        );
        // 測試用的環境不找系統的固定位置
        let quiet = SearchEnv {
            system_dirs: false,
            ..env(Os::Linux)
        };
        assert_eq!(quiet.extra_dirs(), [PathBuf::from("/h").join(".local/bin")]);
        // 這台電腦的環境會找
        assert!(SearchEnv::current(None, None).system_dirs);
    }

    #[test]
    fn relative_and_quoted_path_entries() {
        let d = Dirs::new("rel");
        let sep = if cfg!(windows) { ";" } else { ":" };
        let path_var = format!("relative-dir{sep}\"{}\"", d.root.join("q").display());
        let env = SearchEnv {
            path_var: Some(path_var.into()),
            ..env_in(&d, &[])
        };
        // 相對路徑的項目不算（不從目前資料夾執行程式）
        assert_eq!(env.path_dirs(), vec![d.root.join("q")]);
        let q = d.file("q", &exe("yt-dlp"));
        assert_eq!(locate_ytdl(&env).unwrap().program, q);
    }

    #[cfg(unix)]
    #[test]
    fn files_without_execute_permission_are_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dirs::new("perm");
        let env = env_in(&d, &["p1", "p2"]);
        let p1 = d.file("p1", "yt-dlp");
        std::fs::set_permissions(&p1, std::fs::Permissions::from_mode(0o644)).unwrap();
        // 第二個檔名照這個系統的（Linux 是 yt-dlp_linux、macOS 是 yt-dlp_macos）
        let p2 = d.file("p2", env.ytdl_names()[1]);
        assert_eq!(locate_ytdl(&env).unwrap().program, p2);
        let p2_plain = d.file("p2", "yt-dlp");
        assert_eq!(locate_ytdl(&env).unwrap().program, p2_plain);
    }

    #[test]
    fn deno_search_and_child_path() {
        let d = Dirs::new("dn");
        let env = env_in(&d, &["p1"]);
        let ytdl = d.file("p1", &exe("yt-dlp"));
        let old = d.file("p1", &exe("deno"));
        let good = d.file("home/.deno/bin", &exe("deno"));
        let cands = deno_candidates(&env, Some(&ytdl));
        // 工具資料夾、影戲旁邊、yt-dlp 旁邊（這裡跟 PATH 同一個，不重複）、…、~/.deno/bin
        assert_eq!(cands[0], d.root.join("tools").join(exe("deno")));
        assert_eq!(cands[1], d.root.join("app").join(exe("deno")));
        assert_eq!(cands[2], old);
        assert_eq!(cands.iter().filter(|c| **c == old).count(), 1);
        assert_eq!(cands.last(), Some(&good));

        // 太舊的不算，繼續找下一個
        let probe = |l: &Located, _: &ChildEnv| -> Result<String, YtdlError> {
            let p = l.program.to_string_lossy();
            Ok(if p.contains(".deno") {
                "deno 2.9.7 (stable)".into()
            } else if p.contains("deno") {
                "deno 2.1.0 (stable)".into()
            } else {
                "2026.08.19\n".into()
            })
        };
        let tools = find_tools(&env, &probe);
        assert_eq!(tools.ytdl.as_ref().unwrap().program, ytdl);
        assert_eq!(tools.ytdl_version, Version::parse("2026.08.19"));
        let deno = tools.deno.as_ref().unwrap();
        assert_eq!((deno.path.clone(), deno.managed), (good.clone(), false));
        assert_eq!(tools.deno_too_old.as_ref().map(|(p, _)| p.clone()), Some(old.clone()));
        // 子程序的 PATH：找到的 deno 最前面，接著工具資料夾，原本的 PATH 也在
        let path: Vec<PathBuf> = std::env::split_paths(tools.env.path.as_ref().unwrap()).collect();
        assert_eq!(path[0], good.parent().unwrap());
        assert_eq!(path[1], d.root.join("tools"));
        assert!(path.contains(&d.root.join("p1")));
        // 工具資料夾還沒建立：目前資料夾用系統暫存資料夾
        assert_eq!(tools.env.cwd, Some(std::env::temp_dir()));

        // 免安裝版：影戲旁邊的 deno 也找得到（yt-dlp 在別的地方），而且排在 yt-dlp 旁邊、PATH 前面
        let beside = d.file("app", &exe("deno"));
        let cands = deno_candidates(&env, Some(&ytdl));
        assert_eq!(cands[1], beside);
        assert_eq!(cands[2], old);
        let tools = find_tools(&env, &|l: &Located, _: &ChildEnv| -> Result<String, YtdlError> {
            Ok(if l.program.to_string_lossy().contains("deno") {
                "deno 2.9.7 (stable)".into()
            } else {
                "2026.08.19\n".into()
            })
        });
        assert_eq!(tools.deno.map(|d| d.path), Some(beside));
    }

    #[test]
    fn relative_folders_are_never_searched() {
        // 環境變數設成相對路徑時，工具資料夾、家目錄底下的位置會跟著目前資料夾變：不找、不放進 PATH
        let d = Dirs::new("relroot");
        let env = SearchEnv {
            tools_dir: Some(PathBuf::from("rel-tools")),
            home: Some(PathBuf::from("rel-home")),
            local_app_data: None,
            exe_dir: None,
            ..env_in(&d, &["p1"])
        };
        assert_eq!(env.managed_ytdl(), None);
        assert_eq!(env.managed_deno(), None);
        assert_eq!(env.extra_dirs(), Vec::<PathBuf>::new());
        let cands = deno_candidates(&env, None);
        assert_eq!(cands, vec![d.root.join("p1").join(exe("deno"))]);
        let child = child_env(&env, None);
        let path: Vec<PathBuf> = std::env::split_paths(child.path.as_ref().unwrap()).collect();
        assert!(!path.contains(&PathBuf::from("rel-tools")), "{path:?}");
        assert_eq!(child.cwd, Some(std::env::temp_dir()));
    }

    #[test]
    fn a_failing_version_check_is_kept() {
        let d = Dirs::new("fail");
        let env = env_in(&d, &["p1"]);
        d.file("p1", &exe("yt-dlp"));
        let tools = find_tools(&env, &|_: &Located, _: &ChildEnv| Err(YtdlError::Blocked(225)));
        assert!(tools.ytdl.is_some());
        assert_eq!(tools.ytdl_version, None);
        assert_eq!(tools.ytdl_error, Some(YtdlError::Blocked(225)));
        assert_eq!(tools.deno, None);
    }

    fn counting_locator(count: Arc<std::sync::atomic::AtomicUsize>, gate: Arc<Mutex<()>>) -> Locator {
        Locator::with_finder(
            None,
            Arc::new(move |env: &SearchEnv| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _g = gate.lock().unwrap();
                let mut t = Tools::none(env);
                t.ytdl = env.user_path.clone().map(|p| Located::new(p, Source::UserPath));
                t
            }),
        )
    }

    #[test]
    fn locator_is_lazy_runs_in_the_background_and_remembers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Mutex::new(()));
        let loc = counting_locator(count.clone(), gate.clone());
        assert!(!loc.started());
        // 建立時不找；第一次 get 在背景開始找，不等
        let held = gate.lock().unwrap();
        assert!(loc.get().is_none());
        assert!(loc.started());
        assert!(loc.get().is_none(), "還在找");
        drop(held);
        let t = loc.wait(Duration::from_secs(10)).expect("找完了");
        assert!(t.ytdl.is_none());
        assert!(Arc::ptr_eq(&t, &loc.get().unwrap()));
        assert_eq!(count.load(Ordering::SeqCst), 1, "結果要記住");

        assert!(!loc.searching());

        // 指定的檔案改了：重新找；在背景找的時候先回傳上一次的結果（設定頁不會閃成「沒找到」），wait 等新的
        let p = std::env::temp_dir().join("my-yt-dlp");
        let held = gate.lock().unwrap();
        loc.set_user_path(Some(p.clone()));
        loc.set_user_path(Some(p.clone()));
        let during = loc.get().expect("先回傳上一次的結果");
        assert!(Arc::ptr_eq(&during, &t));
        assert!(loc.searching());
        assert!(loc.wait(Duration::from_millis(50)).is_none(), "wait 不回傳舊的結果");
        drop(held);
        let t2 = loc.wait(Duration::from_secs(10)).unwrap();
        assert_eq!(t2.ytdl.as_ref().map(|l| l.program.clone()), Some(p));
        assert!(Arc::ptr_eq(&t2, &loc.get().unwrap()));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        loc.refresh();
        loc.wait(Duration::from_secs(10)).unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_change_during_the_search_searches_again() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Mutex::new(()));
        let loc = counting_locator(count.clone(), gate.clone());
        let woke = Arc::new(AtomicUsize::new(0));
        let w = woke.clone();
        loc.set_wake(Arc::new(move || {
            w.fetch_add(1, Ordering::SeqCst);
        }));
        let held = gate.lock().unwrap();
        assert!(loc.get().is_none());
        // 等背景真的開始找（已經讀了當時的設定）
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while count.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        // 找的時候使用者改了指定的檔案：舊的結果不算，再找一次
        let p = std::env::temp_dir().join("changed-yt-dlp");
        loc.set_user_path(Some(p.clone()));
        drop(held);
        let t = loc.wait(Duration::from_secs(10)).unwrap();
        assert_eq!(t.ytdl.as_ref().map(|l| l.program.clone()), Some(p));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        // 叫醒介面在背景執行緒：等它發生（慢的電腦上可能比 wait 回來晚一點），之後不會再叫
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while woke.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(woke.load(Ordering::SeqCst), 1, "找完叫醒介面一次");
    }
}
