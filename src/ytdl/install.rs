//! 由影戲下載、更新、移除 yt-dlp 與 deno（主人的決定 Q2 (a)）。
//!
//! - **只在使用者按下時才連網**：「下載 yt-dlp…」「下載 deno…」（先問過，說明大小與來源）、「立即更新」、
//!   「更新 yt-dlp 再試一次」。不在背景自動更新、不在啟動時檢查。
//! - 來源是官方的 GitHub 發佈：先讀 `https://github.com/<repo>/releases/latest` 轉去哪個標籤（不用 GitHub API，
//!   沒有每小時 60 次的限制），之後檢查碼與檔案都從**同一個標籤**下載（中途剛好有新版也不會拿到對不上的兩個檔案）。
//! - 核對發佈附的 SHA-256：yt-dlp 的 `SHA2-256SUMS`、deno 的 `<檔名>.sha256sum`（Windows 版是 PowerShell 的格式，
//!   取第一個 64 位的十六進位）。信任來自 TLS（github.com）與同一個發佈裡的檢查碼，跟 yt-dlp 自己的 `-U` 一樣；不驗 GPG 簽章。
//! - 先寫到工具資料夾裡的暫存檔（`.名稱.<pid>.vitascope-download`），核對（deno 再解開）之後才換成正式的檔名，
//!   Unix 設成可以執行（0755）。換檔案時拿那個執行檔的獨占鎖（[`super::run::ToolLock`]）：等正在解析的 yt-dlp、
//!   放手還沒結束的程序都結束，不會換掉正在執行的檔案。deno 也一樣：執行 yt-dlp 時也拿著它會用的 deno 的共用鎖
//!   （[`super::run::run`]），換掉、移除 deno 時等用到它的 yt-dlp 結束。等的時候也可以取消。
//! - deno 的 zip 自己讀（[`extract_single`]）：flate2 解壓、CRC 核對、有不安全檔名（`..`、絕對路徑、`\`）的壓縮檔整個拒絕、
//!   解開後最多 [`UNZIP_CAP`]。
//! - 「立即更新」也是用同一套下載（不是 `yt-dlp -U`）：先看最新的標籤，已經是最新版就不下載，只把檔案的修改時間更新
//!   （「影戲下載的版本超過 30 天」的提醒從檢查的時間重新算）。
//!
//! 背景執行緒只回傳 [`Outcome`] / [`InstallError`]，給使用者看的文字由介面執行緒產生。

use super::locate::{DenoVersion, Version};
use crate::instance::Wake;
use crate::paths::Os;
use crate::web::{Progress, Web, WebError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

/// 官方發佈的網站
pub const GITHUB: &str = "https://github.com";
/// yt-dlp 的下載上限（實際約 18～40 MB）
const YTDL_CAP: u64 = 200 << 20;
/// deno 的 zip 的下載上限（實際約 40 MB）
const DENO_ZIP_CAP: u64 = 200 << 20;
/// deno 解開後的上限（實際約 100 MB）
pub const UNZIP_CAP: u64 = 300 << 20;
/// 檢查碼檔案的上限
const SUMS_CAP: u64 = 1 << 20;
/// 換檔案、移除時，最多等正在用它的程序多久
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// 等正在用它的程序時，多久看一次取消
const LOCK_POLL: Duration = Duration::from_millis(100);
/// 暫存檔的副檔名（之前中斷留下來的、超過 [`PARTIAL_AGE`] 的會清掉）
const PARTIAL_EXT: &str = "vitascope-download";
const PARTIAL_AGE: Duration = Duration::from_secs(60 * 60);

/// 影戲可以下載的工具
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    Ytdl,
    /// YouTube 需要的 JavaScript 執行環境
    Deno,
}

impl Tool {
    /// 名稱（產品名稱，不翻譯）
    pub fn name(self) -> &'static str {
        match self {
            Tool::Ytdl => "yt-dlp",
            Tool::Deno => "deno",
        }
    }

    /// GitHub 上的專案
    pub fn repo(self) -> &'static str {
        match self {
            Tool::Ytdl => "yt-dlp/yt-dlp",
            Tool::Deno => "denoland/deno",
        }
    }

    /// 要下載的檔案（發佈裡的 asset）；`arch` 是 `std::env::consts::ARCH`。這個系統、CPU 沒有時 None
    pub fn asset(self, os: Os, arch: &str) -> Option<&'static str> {
        match (self, os, arch) {
            // 單一執行檔（PyInstaller）：只有這種能更新自己；`_win.zip` 之類的不行
            (Tool::Ytdl, Os::Windows, "x86_64") => Some("yt-dlp.exe"),
            (Tool::Ytdl, Os::Windows, "x86") => Some("yt-dlp_x86.exe"),
            // macOS 的是 universal（Intel、Apple 晶片都能跑）
            (Tool::Ytdl, Os::Macos, _) => Some("yt-dlp_macos"),
            (Tool::Ytdl, Os::Linux, "x86_64") => Some("yt-dlp_linux"),
            (Tool::Ytdl, Os::Linux, "aarch64") => Some("yt-dlp_linux_aarch64"),
            (Tool::Deno, Os::Windows, "x86_64") => Some("deno-x86_64-pc-windows-msvc.zip"),
            (Tool::Deno, Os::Macos, "aarch64") => Some("deno-aarch64-apple-darwin.zip"),
            (Tool::Deno, Os::Macos, "x86_64") => Some("deno-x86_64-apple-darwin.zip"),
            (Tool::Deno, Os::Linux, "x86_64") => Some("deno-x86_64-unknown-linux-gnu.zip"),
            (Tool::Deno, Os::Linux, "aarch64") => Some("deno-aarch64-unknown-linux-gnu.zip"),
            _ => None,
        }
    }

    /// 發佈裡的檢查碼檔案
    pub fn checksums(self, asset: &str) -> String {
        match self {
            Tool::Ytdl => "SHA2-256SUMS".to_owned(),
            Tool::Deno => format!("{asset}.sha256sum"),
        }
    }

    /// 放在工具資料夾裡的檔名（`locate` 找的就是這個）
    pub fn file_name(self, os: Os) -> &'static str {
        match (self, os) {
            (Tool::Ytdl, Os::Windows) => "yt-dlp.exe",
            (Tool::Ytdl, _) => "yt-dlp",
            (Tool::Deno, Os::Windows) => "deno.exe",
            (Tool::Deno, _) => "deno",
        }
    }

    /// 大約要下載幾 MB、放在電腦上幾 MB（同意下載的對話框；2026 年 10 月的大小）
    pub fn sizes_mb(self, os: Os) -> (u32, u32) {
        match (self, os) {
            (Tool::Ytdl, Os::Windows) => (18, 18),
            (Tool::Ytdl, Os::Macos) => (37, 37),
            (Tool::Ytdl, Os::Linux) => (40, 40),
            (Tool::Deno, Os::Windows) => (43, 100),
            (Tool::Deno, Os::Macos) => (39, 100),
            (Tool::Deno, Os::Linux) => (42, 100),
        }
    }

    /// 下載的上限
    fn cap(self) -> u64 {
        match self {
            Tool::Ytdl => YTDL_CAP,
            Tool::Deno => DENO_ZIP_CAP,
        }
    }

    /// 標籤 → 顯示的版本（deno 的標籤是 `v2.9.7`）
    pub fn version_of(self, tag: &str) -> String {
        tag.strip_prefix('v').unwrap_or(tag).to_owned()
    }

    /// 發佈的標籤比裝好的版本新（看不懂任一個時當成新的：寧可多下載一次）
    pub fn newer(self, tag: &str, installed: Option<&str>) -> bool {
        let Some(installed) = installed else {
            return true;
        };
        match self {
            Tool::Ytdl => match (Version::parse(tag), Version::parse(installed)) {
                (Some(t), Some(i)) => t > i,
                _ => true,
            },
            Tool::Deno => {
                let parse = |v: &str| DenoVersion::parse(&format!("deno {}", self.version_of(v)));
                match (parse(tag), parse(installed)) {
                    (Some(t), Some(i)) => t > i,
                    _ => true,
                }
            }
        }
    }
}

/// 要做的事
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// 下載（還沒有影戲下載的那一份）
    Install(Tool),
    /// 更新影戲下載的那一份：`installed` = 現在的版本（已經是最新版就不下載）
    Update { tool: Tool, installed: Option<String> },
    /// 移除影戲下載的那一份
    Remove(Tool),
}

impl Op {
    pub fn tool(&self) -> Tool {
        match self {
            Op::Install(t) | Op::Remove(t) | Op::Update { tool: t, .. } => *t,
        }
    }
}

/// 做完了
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 下載好了（版本來自發佈的標籤）
    Installed {
        tool: Tool,
        version: String,
    },
    /// 更新到新版了
    Updated {
        tool: Tool,
        version: String,
    },
    /// 已經是最新版（沒有下載）
    UpToDate {
        tool: Tool,
        version: String,
    },
    Removed(Tool),
}

impl Outcome {
    /// 給使用者看的結果（介面執行緒呼叫）
    pub fn message(&self) -> String {
        use crate::tf;
        match self {
            Outcome::Installed { tool, version } => {
                let name = tool.name();
                tf!("已下載 {name} {version}", "Downloaded {name} {version}")
            }
            Outcome::Updated { tool, version } => {
                let name = tool.name();
                tf!("{name} 已更新到 {version}", "{name} updated to {version}")
            }
            Outcome::UpToDate { tool, version } => {
                let name = tool.name();
                tf!("{name} 已經是最新版（{version}）", "{name} is up to date ({version})")
            }
            Outcome::Removed(tool) => {
                let name = tool.name();
                tf!("已移除影戲下載的 {name}", "Removed the {name} VitaScope downloaded")
            }
        }
    }
}

/// 壓縮檔的問題
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// 不是 zip 檔（找不到結尾的目錄）
    NotZip,
    /// 不支援的格式：加密、ZIP64、其他壓縮方式
    Unsupported,
    /// 裡面有不安全的檔名（`..`、絕對路徑、`\`、磁碟代號）：整個不用
    UnsafeName(String),
    /// 裡面沒有要的檔案
    Missing,
    /// 資料損毀（CRC、大小對不上、解壓失敗、超出檔案範圍）
    Corrupt,
    /// 解開後太大
    TooLarge,
    /// 寫不進去：原文
    Write(String),
}

impl ZipError {
    pub fn reason(&self) -> String {
        use crate::{tf, tr};
        match self {
            ZipError::NotZip => tr!("不是 zip 檔", "not a zip file").to_owned(),
            ZipError::Unsupported => tr!("不支援的壓縮格式", "unsupported compression").to_owned(),
            ZipError::UnsafeName(n) => tf!("裡面有不安全的檔名：{n}", "it contains an unsafe file name: {n}"),
            ZipError::Missing => tr!("裡面沒有要的檔案", "the expected file isn't in it").to_owned(),
            ZipError::Corrupt => tr!("檔案損毀", "the file is damaged").to_owned(),
            ZipError::TooLarge => tr!("解開後大得不合理", "it is unreasonably large when unpacked").to_owned(),
            ZipError::Write(e) => tf!("無法寫入：{e}", "couldn't write: {e}"),
        }
    }
}

/// 下載、更新、移除失敗的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// 連線、下載的問題
    Web(WebError),
    /// 這個系統、CPU 沒有可以下載的版本
    Unsupported,
    /// GitHub 上找不到最新的發佈（轉址不是預期的樣子）
    NoRelease,
    /// 發佈的檢查碼裡沒有這個檔案
    NoChecksum,
    /// 下載的檔案跟檢查碼不符（已刪除）
    HashMismatch,
    /// deno 的壓縮檔有問題
    Zip(ZipError),
    /// 無法寫入工具資料夾：(資料夾, 原文)
    Write(PathBuf, String),
    /// 正在使用中（解析中），等太久
    Busy,
}

impl From<WebError> for InstallError {
    fn from(e: WebError) -> Self {
        InstallError::Web(e)
    }
}

impl InstallError {
    /// 原因（介面執行緒呼叫；`tool` 是哪一個工具）
    pub fn reason(&self, tool: Tool) -> String {
        use crate::{tf, tr};
        let name = tool.name();
        match self {
            InstallError::Web(e) => e.message(),
            InstallError::Unsupported => {
                tr!("這個系統沒有可以下載的版本", "there is no download for this system").to_owned()
            }
            InstallError::NoRelease => tr!(
                "GitHub 上找不到最新的版本",
                "couldn't find the latest release on GitHub"
            )
            .to_owned(),
            InstallError::NoChecksum => tr!(
                "發佈裡找不到檢查碼，沒有下載",
                "the release has no checksum for it, so it wasn't downloaded"
            )
            .to_owned(),
            InstallError::HashMismatch => tr!(
                "下載的檔案檢查失敗（雜湊不符），已刪除",
                "the downloaded file failed the check (hash mismatch) and was deleted"
            )
            .to_owned(),
            InstallError::Zip(z) => {
                let why = z.reason();
                tf!(
                    "下載的壓縮檔有問題（{why}）",
                    "the downloaded archive has a problem ({why})"
                )
            }
            InstallError::Write(dir, e) => {
                let dir = dir.display();
                tf!("無法寫入 {dir}：{e}", "couldn't write to {dir}: {e}")
            }
            InstallError::Busy => tf!("{name} 正在使用中，請稍後再試", "{name} is in use; try again later"),
        }
    }

    /// 給使用者看的完整訊息：「無法下載 yt-dlp：原因」
    pub fn message(&self, op: &Op) -> String {
        use crate::tf;
        let tool = op.tool();
        let (name, reason) = (tool.name(), self.reason(tool));
        match op {
            Op::Install(_) => tf!("無法下載 {name}：{reason}", "Couldn't download {name}: {reason}"),
            Op::Update { .. } => tf!("無法更新 {name}：{reason}", "Couldn't update {name}: {reason}"),
            Op::Remove(_) => tf!("無法移除 {name}：{reason}", "Couldn't remove {name}: {reason}"),
        }
    }
}

/// 進行到哪一步
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 查最新的版本
    Checking,
    Downloading,
    /// 核對檢查碼
    Verifying,
    /// 解開壓縮檔
    Extracting,
    /// 等正在用它的 yt-dlp 結束
    Waiting,
}

impl Stage {
    const ALL: [Stage; 5] = [
        Stage::Checking,
        Stage::Downloading,
        Stage::Verifying,
        Stage::Extracting,
        Stage::Waiting,
    ];

    pub fn label(self) -> &'static str {
        use crate::tr;
        match self {
            Stage::Checking => tr!("正在查詢最新版本…", "Checking for the latest version…"),
            Stage::Downloading => tr!("下載中", "Downloading"),
            Stage::Verifying => tr!("正在核對檢查碼…", "Verifying the checksum…"),
            Stage::Extracting => tr!("正在解開…", "Unpacking…"),
            Stage::Waiting => tr!("等待 yt-dlp 結束…", "Waiting for yt-dlp to finish…"),
        }
    }
}

/// 進度（背景執行緒寫，介面讀）
#[derive(Debug, Default)]
pub struct Status {
    pub bytes: Progress,
    stage: AtomicU8,
}

impl Status {
    pub fn stage(&self) -> Stage {
        Stage::ALL[usize::from(self.stage.load(Ordering::Relaxed)).min(Stage::ALL.len() - 1)]
    }

    fn set(&self, stage: Stage) {
        self.stage.store(stage as u8, Ordering::Relaxed);
    }
}

/// 下載、更新、移除工具資料夾裡的 yt-dlp、deno
pub struct Installer {
    tools_dir: PathBuf,
    /// 發佈的網站（`https://github.com`；測試是本機的伺服器）
    base: String,
    os: Os,
    arch: String,
    web: Web,
    lock_wait: Duration,
}

impl Installer {
    /// 從 GitHub 下載到 `tools_dir`（`paths::tools_dir()`）。proxy 照環境變數，只用 https
    pub fn github(tools_dir: PathBuf) -> Installer {
        Installer {
            tools_dir,
            base: GITHUB.to_owned(),
            os: Os::current(),
            arch: std::env::consts::ARCH.to_owned(),
            web: Web::new(true, true),
            lock_wait: LOCK_WAIT,
        }
    }

    /// 測試用：從 `base`（本機的測試伺服器，http）下載；不用環境變數的 proxy
    #[doc(hidden)]
    pub fn with_base(tools_dir: PathBuf, base: &str) -> Installer {
        Installer {
            base: base.trim_end_matches('/').to_owned(),
            web: Web::new(false, false),
            ..Installer::github(tools_dir)
        }
    }

    /// 測試用：換掉連線設定（例如很短的「沒有進度」時間）
    #[doc(hidden)]
    pub fn with_web(mut self, web: Web) -> Self {
        self.web = web;
        self
    }

    /// 測試用：換檔案時最多等多久
    #[doc(hidden)]
    pub fn with_lock_wait(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    /// 測試用：當成別的 CPU（沒有可以下載的版本）
    #[doc(hidden)]
    pub fn with_arch(mut self, arch: &str) -> Self {
        self.arch = arch.to_owned();
        self
    }

    pub fn tools_dir(&self) -> &Path {
        &self.tools_dir
    }

    /// 這個系統、CPU 有沒有可以下載的版本
    pub fn available(&self, tool: Tool) -> bool {
        tool.asset(self.os, &self.arch).is_some()
    }

    /// 影戲下載的那一份放在哪裡
    pub fn path(&self, tool: Tool) -> PathBuf {
        self.tools_dir.join(tool.file_name(self.os))
    }

    fn release_url(&self, tool: Tool, tag: &str, file: &str) -> String {
        format!("{}/{}/releases/download/{tag}/{file}", self.base, tool.repo())
    }

    /// 最新的發佈的標籤：`<repo>/releases/latest` 轉去 `<repo>/releases/tag/<標籤>`
    pub fn latest_tag(&self, tool: Tool) -> Result<String, InstallError> {
        let url = format!("{}/{}/releases/latest", self.base, tool.repo());
        let target = self.web.redirect_target(&url)?.ok_or(InstallError::NoRelease)?;
        tag_from_url(&target).ok_or(InstallError::NoRelease)
    }

    /// 做 `op`（會等網路、檔案：在背景執行緒呼叫）。`cancel` 變成 true 時盡快停止（暫存檔刪掉，原本的檔案不動）
    pub fn run(&self, op: &Op, status: &Status, cancel: &AtomicBool) -> Result<Outcome, InstallError> {
        // 這個系統沒有可以下載的版本：連網之前就停
        if !matches!(op, Op::Remove(_)) && !self.available(op.tool()) {
            return Err(InstallError::Unsupported);
        }
        match op {
            Op::Install(tool) => {
                status.set(Stage::Checking);
                let tag = self.latest_tag(*tool)?;
                check_cancel(cancel)?;
                self.install_tag(*tool, &tag, status, cancel)?;
                Ok(Outcome::Installed {
                    tool: *tool,
                    version: tool.version_of(&tag),
                })
            }
            Op::Update { tool, installed } => {
                status.set(Stage::Checking);
                let tag = self.latest_tag(*tool)?;
                // 查的時候按了取消：不算檢查過（不更新檔案的時間）
                check_cancel(cancel)?;
                let version = tool.version_of(&tag);
                if !tool.newer(&tag, installed.as_deref()) {
                    // 檢查過了：「超過 30 天」的提醒從現在重新算（改不了時間只是之後還會提醒，不算失敗）
                    if let Err(e) = touch(&self.path(*tool)) {
                        eprintln!("[vitascope] 無法更新 {} 的修改時間：{e}", tool.name());
                    }
                    return Ok(Outcome::UpToDate { tool: *tool, version });
                }
                self.install_tag(*tool, &tag, status, cancel)?;
                Ok(Outcome::Updated { tool: *tool, version })
            }
            Op::Remove(tool) => {
                let path = self.path(*tool);
                status.set(Stage::Waiting);
                let _guard = self.wait_unused(&path, cancel)?;
                match std::fs::remove_file(&path) {
                    Ok(()) => Ok(Outcome::Removed(*tool)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Outcome::Removed(*tool)),
                    Err(e) => Err(InstallError::Write(self.tools_dir.clone(), e.to_string())),
                }
            }
        }
    }

    /// 下載標籤 `tag` 的版本，核對、（deno）解開，換掉工具資料夾裡的那一份
    fn install_tag(&self, tool: Tool, tag: &str, status: &Status, cancel: &AtomicBool) -> Result<(), InstallError> {
        let asset = tool.asset(self.os, &self.arch).ok_or(InstallError::Unsupported)?;
        let dir_error = |e: std::io::Error| InstallError::Write(self.tools_dir.clone(), e.to_string());
        std::fs::create_dir_all(&self.tools_dir).map_err(dir_error)?;
        sweep_partials(&self.tools_dir);

        let sums = self
            .web
            .fetch(&self.release_url(tool, tag, &tool.checksums(asset)), SUMS_CAP, cancel)?;
        let sums = String::from_utf8_lossy(&sums);
        let expected = match tool {
            Tool::Ytdl => sha256sums_entry(&sums, asset),
            Tool::Deno => first_sha256(&sums),
        }
        .ok_or(InstallError::NoChecksum)?;

        status.set(Stage::Downloading);
        let pid = std::process::id();
        let download = Partial::new(self.tools_dir.join(format!(".{}.{pid}.{PARTIAL_EXT}", tool.name())));
        let file = std::fs::File::create(&download.0).map_err(dir_error)?;
        let mut out = Hashing::new(std::io::BufWriter::new(file));
        self.web.download(
            &self.release_url(tool, tag, asset),
            &mut out,
            tool.cap(),
            &status.bytes,
            cancel,
        )?;
        status.set(Stage::Verifying);
        let (file, digest) = out.finish();
        drop(file);
        if digest != expected {
            return Err(InstallError::HashMismatch);
        }

        let ready = match tool {
            Tool::Ytdl => download,
            Tool::Deno => {
                status.set(Stage::Extracting);
                let unpacked = Partial::new(self.tools_dir.join(format!(".deno-bin.{pid}.{PARTIAL_EXT}")));
                extract_single(&download.0, tool.file_name(self.os), &unpacked.0, UNZIP_CAP)
                    .map_err(InstallError::Zip)?;
                drop(download);
                unpacked
            }
        };
        check_cancel(cancel)?;
        make_executable(&ready.0).map_err(dir_error)?;

        // 正在解析的 yt-dlp、放手還沒結束的程序都結束了才換（不換掉正在執行的檔案）。等的時候按了取消：不換
        status.set(Stage::Waiting);
        let path = self.path(tool);
        let _guard = self.wait_unused(&path, cancel)?;
        std::fs::rename(&ready.0, &path).map_err(dir_error)?;
        ready.keep();
        Ok(())
    }

    /// 拿 `path` 的獨占鎖：等正在用它的程序都結束，最多 `lock_wait`（太久時 [`InstallError::Busy`]）。
    /// 等的時候一直看取消；拿到之後也再看一次（取消了就放開，不換、不刪）
    fn wait_unused(&self, path: &Path, cancel: &AtomicBool) -> Result<super::run::ExclusiveGuard, InstallError> {
        let lock = super::run::tool_lock(path);
        let until = Instant::now() + self.lock_wait;
        loop {
            check_cancel(cancel)?;
            let left = until.saturating_duration_since(Instant::now());
            if let Some(guard) = lock.exclusive(left.min(LOCK_POLL)) {
                check_cancel(cancel)?;
                return Ok(guard);
            }
            if left.is_zero() {
                return Err(InstallError::Busy);
            }
        }
    }
}

/// 取消了：[`WebError::Cancelled`]
fn check_cancel(cancel: &AtomicBool) -> Result<(), InstallError> {
    if cancel.load(Ordering::Relaxed) {
        Err(WebError::Cancelled.into())
    } else {
        Ok(())
    }
}

/// 轉址的網址 `…/releases/tag/<標籤>` → 標籤（只接受英數字與 `.`、`_`、`-`、`+`）
pub fn tag_from_url(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_owned();
    let (_, rest) = path.split_once("/releases/tag/")?;
    let tag = rest.trim_end_matches('/');
    let ok = !tag.is_empty()
        && tag.len() <= 64
        && !tag.starts_with('.')
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'));
    ok.then(|| tag.to_owned())
}

/// 64 個十六進位字 → 32 位元組
fn hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks(2).enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// `sha256sum` 格式（`<雜湊>  <檔名>`，二進位模式是 `<雜湊> *<檔名>`）裡 `name` 的雜湊
pub fn sha256sums_entry(text: &str, name: &str) -> Option<[u8; 32]> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?;
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && parts.next().is_none()).then(|| hex32(hash)).flatten()
    })
}

/// 第一個剛好 64 個十六進位字的字（deno 的 `.sha256sum`：Linux、macOS 是 `sha256sum` 格式，
/// Windows 是 PowerShell `Get-FileHash` 的「Hash : ABCD…」）
pub fn first_sha256(text: &str) -> Option<[u8; 32]> {
    text.split(|c: char| c.is_whitespace() || c == ':').find_map(hex32)
}

/// 一邊寫一邊算 SHA-256
struct Hashing<W: Write> {
    inner: W,
    ctx: ring::digest::Context,
}

impl<W: Write> Hashing<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            ctx: ring::digest::Context::new(&ring::digest::SHA256),
        }
    }

    fn finish(self) -> (W, [u8; 32]) {
        let digest = self.ctx.finish();
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        (self.inner, out)
    }
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.ctx.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// 檔案的 SHA-256
pub fn sha256_of(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, data).as_ref());
    out
}

/// 暫存檔：沒有 `keep` 就刪掉（失敗、取消時不留下半個檔案）
struct Partial(PathBuf);

impl Partial {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    /// 已經換成正式的檔名了：不刪
    fn keep(mut self) {
        self.0 = PathBuf::new();
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.0.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// 之前中斷（影戲被關掉、當機）留下來的暫存檔：超過一小時的刪掉（別的影戲視窗正在下載的不會這麼舊）
fn sweep_partials(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let name = e.file_name();
        if !name.to_string_lossy().ends_with(&format!(".{PARTIAL_EXT}")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > PARTIAL_AGE);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 檔案的修改時間改成現在（檢查過更新、已經是最新版）。yt-dlp 可能正在執行（解析中）：不能用寫入模式開
/// （Windows 的共用違規、Linux 的「Text file busy」），只要改屬性的權限——Windows 用 `FILE_WRITE_ATTRIBUTES`，
/// Unix 用唯讀開（`futimens` 看的是檔案的擁有者，不是開檔的模式）
pub fn touch(path: &Path) -> std::io::Result<()> {
    let mut options = std::fs::File::options();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        /// `FILE_WRITE_ATTRIBUTES`：不算寫入內容，正在執行的程式也能開
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        options.access_mode(FILE_WRITE_ATTRIBUTES);
    }
    #[cfg(not(windows))]
    options.read(true);
    options.open(path)?.set_modified(SystemTime::now())
}

/// Unix：可以執行（0755）
fn make_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// ───────────── zip ─────────────

const EOCD_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// zip 裡的檔名可以用嗎：不是空的、不是絕對路徑、沒有 `..`、`\`、磁碟代號（`:`）、控制字元
pub fn safe_zip_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains(['\\', ':'])
        && !name.chars().any(char::is_control)
        && !name.split('/').any(|seg| seg == "..")
}

/// zip 裡的一個檔案（中央目錄的資料）
struct Entry {
    name: String,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    local_offset: u64,
}

/// 讀中央目錄
fn central_directory(f: &mut std::fs::File) -> Result<(Vec<Entry>, u64), ZipError> {
    let corrupt = |_| ZipError::Corrupt;
    let len = f.metadata().map_err(corrupt)?.len();
    // 結尾的目錄（22 位元組 + 最多 65535 的註解）
    let tail_len = len.min(22 + 65_535);
    f.seek(SeekFrom::Start(len - tail_len)).map_err(corrupt)?;
    let mut tail = vec![0u8; tail_len as usize];
    f.read_exact(&mut tail).map_err(corrupt)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(&tail, i) == Some(EOCD_SIG))
        .ok_or(ZipError::NotZip)?;
    let e = &tail[eocd..];
    let (disk, cd_disk) = (u16_at(e, 4).unwrap_or(1), u16_at(e, 6).unwrap_or(1));
    let count = u16_at(e, 10).ok_or(ZipError::Corrupt)?;
    let cd_size = u32_at(e, 12).ok_or(ZipError::Corrupt)?;
    let cd_offset = u32_at(e, 16).ok_or(ZipError::Corrupt)?;
    if disk != 0 || cd_disk != 0 || count == 0xFFFF || cd_size == u32::MAX || cd_offset == u32::MAX {
        // 分割的壓縮檔、ZIP64
        return Err(ZipError::Unsupported);
    }
    let eocd_pos = len - tail_len + eocd as u64;
    let (cd_offset, cd_size) = (u64::from(cd_offset), u64::from(cd_size));
    if cd_offset + cd_size > eocd_pos {
        return Err(ZipError::Corrupt);
    }
    f.seek(SeekFrom::Start(cd_offset)).map_err(corrupt)?;
    let mut cd = vec![0u8; cd_size as usize];
    f.read_exact(&mut cd).map_err(corrupt)?;
    let mut entries = Vec::new();
    let mut i = 0usize;
    for _ in 0..count {
        if u32_at(&cd, i) != Some(CENTRAL_SIG) {
            return Err(ZipError::Corrupt);
        }
        let field16 = |o: usize| u16_at(&cd, i + o).ok_or(ZipError::Corrupt);
        let field32 = |o: usize| u32_at(&cd, i + o).ok_or(ZipError::Corrupt);
        let (n, m, k) = (
            usize::from(field16(28)?),
            usize::from(field16(30)?),
            usize::from(field16(32)?),
        );
        let name = cd.get(i + 46..i + 46 + n).ok_or(ZipError::Corrupt)?;
        let (compressed, size, local) = (field32(20)?, field32(24)?, field32(42)?);
        if compressed == u32::MAX || size == u32::MAX || local == u32::MAX {
            return Err(ZipError::Unsupported);
        }
        entries.push(Entry {
            name: String::from_utf8_lossy(name).into_owned(),
            flags: field16(8)?,
            method: field16(10)?,
            crc: field32(16)?,
            compressed: u64::from(compressed),
            size: u64::from(size),
            local_offset: u64::from(local),
        });
        i += 46 + n + m + k;
    }
    Ok((entries, cd_offset))
}

/// 從 zip 檔 `zip` 解開最上層的檔案 `wanted` 到 `dest`（最多 `cap` 位元組），核對大小與 CRC。
/// 只要壓縮檔裡有任何不安全的檔名就整個拒絕（正常的 deno 壓縮檔只有一個 `deno` / `deno.exe`）
pub fn extract_single(zip: &Path, wanted: &str, dest: &Path, cap: u64) -> Result<u64, ZipError> {
    let mut f = std::fs::File::open(zip).map_err(|_| ZipError::NotZip)?;
    let (entries, cd_offset) = central_directory(&mut f)?;
    if let Some(bad) = entries.iter().find(|e| !safe_zip_name(&e.name)) {
        return Err(ZipError::UnsafeName(bad.name.clone()));
    }
    let entry = entries.iter().find(|e| e.name == wanted).ok_or(ZipError::Missing)?;
    // 加密
    if entry.flags & 1 != 0 || !matches!(entry.method, 0 | 8) {
        return Err(ZipError::Unsupported);
    }
    if entry.size > cap {
        return Err(ZipError::TooLarge);
    }
    // 本地標頭：資料在 30 + 檔名 + 額外欄位之後（額外欄位的長度可能跟中央目錄的不同）
    let corrupt = |_| ZipError::Corrupt;
    f.seek(SeekFrom::Start(entry.local_offset)).map_err(corrupt)?;
    let mut local = [0u8; 30];
    f.read_exact(&mut local).map_err(corrupt)?;
    if u32_at(&local, 0) != Some(LOCAL_SIG) {
        return Err(ZipError::Corrupt);
    }
    let skip = u64::from(u16_at(&local, 26).unwrap_or(0)) + u64::from(u16_at(&local, 28).unwrap_or(0));
    let data = entry.local_offset + 30 + skip;
    if data + entry.compressed > cd_offset {
        return Err(ZipError::Corrupt);
    }
    f.seek(SeekFrom::Start(data)).map_err(corrupt)?;
    let raw = (&mut f).take(entry.compressed);
    let mut reader: Box<dyn Read> = if entry.method == 8 {
        Box::new(flate2::read::DeflateDecoder::new(raw))
    } else {
        Box::new(raw)
    };
    let out = std::fs::File::create(dest).map_err(|e| ZipError::Write(e.to_string()))?;
    let mut out = std::io::BufWriter::new(out);
    let mut crc = flate2::Crc::new();
    let mut written: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ZipError::Corrupt),
        };
        written += n as u64;
        // 解開的比說的大：損毀，或是故意塞大檔案
        if written > entry.size {
            return Err(if written > cap {
                ZipError::TooLarge
            } else {
                ZipError::Corrupt
            });
        }
        crc.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| ZipError::Write(e.to_string()))?;
    }
    out.flush().map_err(|e| ZipError::Write(e.to_string()))?;
    if written != entry.size || crc.sum() != entry.crc {
        return Err(ZipError::Corrupt);
    }
    Ok(written)
}

/// 測試用的 zip 裡的一個檔案
#[doc(hidden)]
pub struct ZipSpec<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
    /// 用 deflate 壓縮（否則不壓縮）
    pub deflate: bool,
    /// 故意寫錯的 CRC
    pub bad_crc: bool,
    /// 故意寫錯的「解開後的大小」（壓縮炸彈：說得很小、解開很大）；None = 照實際的大小
    pub declared_size: Option<u32>,
}

/// 測試用：做一個 zip 檔（單元測試、整合測試的假 deno 都用它）
#[doc(hidden)]
pub fn build_zip(files: &[ZipSpec<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for f in files {
        let mut crc = flate2::Crc::new();
        crc.update(f.data);
        let crc = if f.bad_crc { crc.sum() ^ 1 } else { crc.sum() };
        let body = if f.deflate {
            let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(f.data).unwrap();
            enc.finish().unwrap()
        } else {
            f.data.to_vec()
        };
        let method: u16 = if f.deflate { 8 } else { 0 };
        let offset = out.len() as u32;
        let name = f.name.as_bytes();
        let common = |v: &mut Vec<u8>| {
            v.extend_from_slice(&20u16.to_le_bytes()); // 需要的版本
            v.extend_from_slice(&0u16.to_le_bytes()); // 旗標
            v.extend_from_slice(&method.to_le_bytes());
            v.extend_from_slice(&0u32.to_le_bytes()); // 時間、日期
            v.extend_from_slice(&crc.to_le_bytes());
            v.extend_from_slice(&(body.len() as u32).to_le_bytes());
            v.extend_from_slice(&f.declared_size.unwrap_or(f.data.len() as u32).to_le_bytes());
            v.extend_from_slice(&(name.len() as u16).to_le_bytes());
            v.extend_from_slice(&0u16.to_le_bytes()); // 額外欄位
        };
        out.extend_from_slice(&LOCAL_SIG.to_le_bytes());
        common(&mut out);
        out.extend_from_slice(name);
        out.extend_from_slice(&body);
        central.extend_from_slice(&CENTRAL_SIG.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // 建立的版本
        common(&mut central);
        central.extend_from_slice(&0u16.to_le_bytes()); // 註解
        central.extend_from_slice(&0u16.to_le_bytes()); // 磁碟
        central.extend_from_slice(&0u16.to_le_bytes()); // 內部屬性
        central.extend_from_slice(&0u32.to_le_bytes()); // 外部屬性
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&EOCD_SIG.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]); // 磁碟
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // 註解
    out
}

// ───────────── 背景工作 ─────────────

/// 在背景做的一件事（下載、更新、移除）：介面每一幀看進度、做完了沒
pub struct Job {
    pub op: Op,
    pub status: Arc<Status>,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Result<Outcome, InstallError>>,
}

impl Job {
    /// 在背景執行緒做 `op`，做完叫醒介面
    pub fn start(installer: Arc<Installer>, op: Op, wake: Option<Wake>) -> Job {
        let status = Arc::new(Status::default());
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (s, c, o) = (status.clone(), cancel.clone(), op.clone());
        let spawned = std::thread::Builder::new()
            .name("vitascope-install".into())
            .spawn(move || {
                let result = installer.run(&o, &s, &c);
                let _ = tx.send(result);
                if let Some(w) = wake {
                    w();
                }
            });
        if let Err(e) = spawned {
            // 開不了執行緒：馬上回報失敗（rx 收到「斷線」）
            eprintln!("[vitascope] 無法開執行緒下載：{e}");
        }
        Job { op, status, cancel, rx }
    }

    /// 取消（暫存檔刪掉，原本的檔案不動）
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// 做完了的結果（還沒做完時 None）
    pub fn poll(&self) -> Option<Result<Outcome, InstallError>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(InstallError::Web(WebError::Network("worker stopped".into()))))
            }
        }
    }

    /// 等做完（測試用；介面不要用）
    #[doc(hidden)]
    pub fn wait(&self, timeout: Duration) -> Option<Result<Outcome, InstallError>> {
        self.rx.recv_timeout(timeout).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8; 32]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn assets_per_system() {
        assert_eq!(Tool::Ytdl.asset(Os::Windows, "x86_64"), Some("yt-dlp.exe"));
        assert_eq!(Tool::Ytdl.asset(Os::Macos, "aarch64"), Some("yt-dlp_macos"));
        assert_eq!(Tool::Ytdl.asset(Os::Linux, "x86_64"), Some("yt-dlp_linux"));
        assert_eq!(Tool::Ytdl.asset(Os::Linux, "riscv64"), None);
        assert_eq!(
            Tool::Deno.asset(Os::Windows, "x86_64"),
            Some("deno-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(
            Tool::Deno.asset(Os::Macos, "aarch64"),
            Some("deno-aarch64-apple-darwin.zip")
        );
        assert_eq!(
            Tool::Deno.asset(Os::Linux, "x86_64"),
            Some("deno-x86_64-unknown-linux-gnu.zip")
        );
        assert_eq!(Tool::Deno.asset(Os::Windows, "aarch64"), None);
        assert_eq!(Tool::Ytdl.checksums("yt-dlp.exe"), "SHA2-256SUMS");
        assert_eq!(
            Tool::Deno.checksums("deno-x86_64-pc-windows-msvc.zip"),
            "deno-x86_64-pc-windows-msvc.zip.sha256sum"
        );
        // 放的檔名跟 locate 找的一樣
        for os in [Os::Windows, Os::Macos, Os::Linux] {
            let env = super::super::SearchEnv {
                os: Some(os),
                tools_dir: Some(std::env::temp_dir().join("t")),
                ..Default::default()
            };
            let dir = std::env::temp_dir().join("t");
            assert_eq!(env.managed_ytdl(), Some(dir.join(Tool::Ytdl.file_name(os))));
            assert_eq!(env.managed_deno(), Some(dir.join(Tool::Deno.file_name(os))));
        }
    }

    #[test]
    fn tags_from_the_latest_redirect() {
        assert_eq!(
            tag_from_url("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.08.19").as_deref(),
            Some("2026.08.19")
        );
        assert_eq!(
            tag_from_url("https://github.com/denoland/deno/releases/tag/v2.9.7").as_deref(),
            Some("v2.9.7")
        );
        // 沒有任何發佈時 GitHub 轉到 releases 的列表
        assert_eq!(tag_from_url("https://github.com/yt-dlp/yt-dlp/releases"), None);
        for bad in [
            "https://github.com/x/y/releases/tag/",
            "https://github.com/x/y/releases/tag/..",
            "https://github.com/x/y/releases/tag/a%2F..%2Fb",
            "https://github.com/x/y/releases/tag/a b",
            "not a url",
        ] {
            assert_eq!(tag_from_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn newer_versions() {
        assert!(Tool::Ytdl.newer("2026.09.01", Some("2026.08.19")));
        assert!(!Tool::Ytdl.newer("2026.08.19", Some("2026.08.19")));
        // 每日建置比同一天的正式版新
        assert!(!Tool::Ytdl.newer("2026.08.19", Some("2026.08.19.232934")));
        assert!(Tool::Ytdl.newer("2026.08.19", None));
        assert!(Tool::Ytdl.newer("weird", Some("2026.08.19")));
        assert!(Tool::Deno.newer("v2.9.8", Some("2.9.7")));
        assert!(!Tool::Deno.newer("v2.9.7", Some("2.9.7")));
        assert!(!Tool::Deno.newer("v2.9.7", Some("2.10.0")));
        assert_eq!(Tool::Deno.version_of("v2.9.7"), "2.9.7");
        assert_eq!(Tool::Ytdl.version_of("2026.08.19"), "2026.08.19");
    }

    #[test]
    fn checksum_files() {
        let a = sha256_of(b"yt-dlp.exe content");
        let b = sha256_of(b"other");
        let sums = format!(
            "{}  yt-dlp\n{}  yt-dlp.exe\n{} *yt-dlp_linux\n",
            hex(&b),
            hex(&a),
            hex(&b)
        );
        assert_eq!(sha256sums_entry(&sums, "yt-dlp.exe"), Some(a));
        assert_eq!(sha256sums_entry(&sums, "yt-dlp"), Some(b));
        assert_eq!(sha256sums_entry(&sums, "yt-dlp_linux"), Some(b));
        // 檔名要完全一樣（不是前綴）
        assert_eq!(sha256sums_entry(&sums, "yt-dlp.ex"), None);
        assert_eq!(sha256sums_entry(&sums, "yt-dlp_macos"), None);
        // 雜湊不是 64 位的不算
        assert_eq!(sha256sums_entry("abcd  yt-dlp.exe\n", "yt-dlp.exe"), None);
        // 大寫也可以
        let upper = format!("{}  yt-dlp.exe", hex(&a).to_uppercase());
        assert_eq!(sha256sums_entry(&upper, "yt-dlp.exe"), Some(a));

        // deno：sha256sum 格式與 PowerShell 的 Get-FileHash 格式
        assert_eq!(
            first_sha256(&format!("{}  deno-x86_64-unknown-linux-gnu.zip\n", hex(&a))),
            Some(a)
        );
        let powershell = format!(
            "\r\nAlgorithm       : SHA256\r\nHash            : {}\r\nPath            : D:\\a\\deno\\deno-x86_64-pc-windows-msvc.zip\r\n",
            hex(&b).to_uppercase()
        );
        assert_eq!(first_sha256(&powershell), Some(b));
        assert_eq!(first_sha256("no hash here"), None);
        // 65 位、63 位的不算
        assert_eq!(first_sha256(&format!("{}0", hex(&a))), None);
        assert_eq!(first_sha256(&hex(&a)[1..]), None);
    }

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("vitascope-install-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn unzip(t: &Tmp, files: &[ZipSpec<'_>], wanted: &str, cap: u64) -> (Result<u64, ZipError>, PathBuf) {
        let zip = t.0.join("a.zip");
        std::fs::write(&zip, build_zip(files)).unwrap();
        let dest = t.0.join("out.bin");
        let _ = std::fs::remove_file(&dest);
        (extract_single(&zip, wanted, &dest, cap), dest)
    }

    fn spec<'a>(name: &'a str, data: &'a [u8], deflate: bool) -> ZipSpec<'a> {
        ZipSpec {
            name,
            data,
            deflate,
            bad_crc: false,
            declared_size: None,
        }
    }

    #[test]
    fn zip_stored_and_deflated() {
        let t = Tmp::new("zip-ok");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        for deflate in [false, true] {
            let files = [spec("LICENSE.md", b"MIT", false), spec("deno", &data, deflate)];
            let (r, dest) = unzip(&t, &files, "deno", UNZIP_CAP);
            assert_eq!(r, Ok(data.len() as u64), "deflate={deflate}");
            assert_eq!(std::fs::read(dest).unwrap(), data);
        }
        // 要的檔案不在裡面
        let (r, _) = unzip(&t, &[spec("deno.exe", b"x", false)], "deno", UNZIP_CAP);
        assert_eq!(r, Err(ZipError::Missing));
    }

    #[test]
    fn zip_damage_and_limits_are_refused() {
        let t = Tmp::new("zip-bad");
        let data = vec![7u8; 10_000];
        for deflate in [false, true] {
            let bad = ZipSpec {
                bad_crc: true,
                ..spec("deno", &data, deflate)
            };
            assert_eq!(unzip(&t, &[bad], "deno", UNZIP_CAP).0, Err(ZipError::Corrupt));
        }
        // 解開後超過上限
        assert_eq!(
            unzip(&t, &[spec("deno", &data, true)], "deno", 9_999).0,
            Err(ZipError::TooLarge)
        );
        // 不是 zip、被截斷的 zip
        let not_zip = t.0.join("n.zip");
        std::fs::write(&not_zip, b"this is not a zip file at all, just some text").unwrap();
        assert_eq!(
            extract_single(&not_zip, "deno", &t.0.join("o"), UNZIP_CAP),
            Err(ZipError::NotZip)
        );
        let mut cut = build_zip(&[spec("deno", &data, false)]);
        cut.drain(100..5_000);
        let cut_path = t.0.join("cut.zip");
        std::fs::write(&cut_path, cut).unwrap();
        assert!(extract_single(&cut_path, "deno", &t.0.join("o"), UNZIP_CAP).is_err());
    }

    /// 壓縮炸彈：說解開後很小、實際很大。超過說的大小就停，不會先把整個寫到磁碟上才發現
    #[test]
    fn zip_with_an_understated_size_stops_early() {
        let t = Tmp::new("zip-bomb");
        let data = vec![0u8; 500_000];
        let bomb = |declared: u32| ZipSpec {
            declared_size: Some(declared),
            ..spec("deno", &data, true)
        };
        // 說 1000 位元組：上限之內，但跟說的不符（損毀）；寫出來的不超過說的大小
        let (r, dest) = unzip(&t, &[bomb(1000)], "deno", UNZIP_CAP);
        assert_eq!(r, Err(ZipError::Corrupt));
        let written = std::fs::metadata(&dest).map_or(0, |m| m.len());
        assert!(written <= 1000, "寫了 {written} 位元組");
        // 解開的超過上限：太大
        let (r, dest) = unzip(&t, &[bomb(1000)], "deno", 2000);
        assert_eq!(r, Err(ZipError::TooLarge));
        let written = std::fs::metadata(&dest).map_or(0, |m| m.len());
        assert!(written <= 1000, "寫了 {written} 位元組");
    }

    /// 「已經是最新版」時更新檔案的時間：yt-dlp 正在執行（不能用寫入模式開）也改得了
    #[test]
    fn touching_works_while_the_file_is_in_use() {
        let t = Tmp::new("touch");
        let path = t.0.join("yt-dlp");
        std::fs::write(&path, b"x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(90 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // Windows：正在執行的程式不讓別人寫入內容（跟只開放讀取的共用模式一樣）
        #[cfg(windows)]
        let _running = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 1;
            std::fs::File::options()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&path)
                .unwrap()
        };
        // Unix：沒有寫入權限（正在執行的檔案也不能用寫入模式開）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).unwrap();
        }
        touch(&path).unwrap();
        let now = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(now > old + Duration::from_secs(86_400), "{now:?}");
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn zip_with_unsafe_names_is_refused_whole() {
        let t = Tmp::new("zip-names");
        for bad in ["../deno", "a/../../deno", "/etc/deno", "..\\deno", "C:deno", "x\\y"] {
            let files = [spec("deno", b"ok", false), spec(bad, b"evil", false)];
            assert_eq!(
                unzip(&t, &files, "deno", UNZIP_CAP).0,
                Err(ZipError::UnsafeName(bad.to_owned())),
                "{bad}"
            );
        }
        // 子資料夾裡的普通檔名可以（只解開最上層要的那一個）
        let files = [spec("docs/README.md", b"x", false), spec("deno", b"ok", false)];
        assert_eq!(unzip(&t, &files, "deno", UNZIP_CAP).0, Ok(2));
        assert!(safe_zip_name("deno.exe") && safe_zip_name("a/b..c"));
        assert!(!safe_zip_name("") && !safe_zip_name("a\u{0}b"));
    }

    #[test]
    fn hashing_writer_matches_the_one_shot_digest() {
        let mut h = Hashing::new(Vec::new());
        h.write_all(b"hello ").unwrap();
        h.write_all(b"world").unwrap();
        let (data, digest) = h.finish();
        assert_eq!(data, b"hello world");
        assert_eq!(digest, sha256_of(b"hello world"));
        assert_eq!(
            hex(&sha256_of(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn stages_round_trip() {
        let s = Status::default();
        assert_eq!(s.stage(), Stage::Checking);
        for st in Stage::ALL {
            s.set(st);
            assert_eq!(s.stage(), st);
        }
    }

    #[test]
    fn old_partial_files_are_swept() {
        let t = Tmp::new("sweep");
        let old = t.0.join(".yt-dlp.1.vitascope-download");
        let fresh = t.0.join(".deno.2.vitascope-download");
        let other = t.0.join("yt-dlp.exe");
        for p in [&old, &fresh, &other] {
            std::fs::write(p, b"x").unwrap();
        }
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .unwrap();
        drop(f);
        sweep_partials(&t.0);
        assert!(!old.exists());
        assert!(fresh.exists() && other.exists());
    }
}
