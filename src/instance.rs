//! 單一執行個體：已經有播放器開著時，把要開的檔案交給它（它會跳到最前面），自己結束。
//!
//! - 誰是主視窗：搶到鎖定檔（`File::try_lock`）的那一個。程式結束或當掉時系統會放開鎖，不會留下錯的狀態
//! - 怎麼傳檔名：Windows 用具名管道（名稱含使用者 SID 與工作階段，只允許同一個使用者連線），
//!   其他系統用 Unix socket（放在只有自己能進的資料夾）
//! - 檔案總管選了 5 個檔案按 Enter 會同時啟動 5 個程式：只有一個搶到鎖，其他的把檔名送過去；
//!   主視窗把一小段時間內收到的合併成一批（`Batcher`），當成一次拖放多個檔案處理
//! - 自動截圖（`--shot`）、`--version` 不經過這裡；設定可以關掉（允許多個視窗），`--new-window` 只對這一次
//!
//! 訊息格式：`VTSC`、版本、旗標（全螢幕）、路徑數，每個路徑「長度 + 內容」（Windows 是 UTF-16，
//! 其他系統是原始位元組，非 UTF-8 的檔名也不會壞掉）。回覆一個位元組：收到 / 正在關閉 / 版本不合。

use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 4] = b"VTSC";
const VERSION: u8 = 1;
const ACK: u8 = 0x06;
const BUSY: u8 = 0x15;
const REJECT: u8 = 0x18;
/// 等主視窗回應最多多久（剛啟動的主視窗還沒開始聽、或正在關閉）
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_PATHS: usize = 4096;
const MAX_PATH_BYTES: usize = 64 * 1024;

/// 要開的檔案
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Request {
    pub paths: Vec<PathBuf>,
    pub fullscreen: bool,
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

// ───────────── 訊息格式 ─────────────

#[cfg(windows)]
fn encode_path(p: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(unix)]
fn encode_path(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(windows)]
fn decode_path(b: &[u8]) -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    if !b.len().is_multiple_of(2) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "路徑長度不對"));
    }
    let wide: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(unix)]
fn decode_path(b: &[u8]) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(b.to_vec())))
}

pub fn write_request(w: &mut impl Write, req: &Request) -> io::Result<()> {
    let mut buf = Vec::from(&MAGIC[..]);
    buf.extend([VERSION, u8::from(req.fullscreen)]);
    buf.extend((req.paths.len() as u32).to_le_bytes());
    for p in &req.paths {
        let b = encode_path(p);
        buf.extend((b.len() as u32).to_le_bytes());
        buf.extend(b);
    }
    w.write_all(&buf)?;
    w.flush()
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

/// 讀一個要求；格式不對（別的程式、不同版本）回傳 `InvalidData`
pub fn read_request(r: &mut impl Read) -> io::Result<Request> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
    let mut head = [0; 6];
    r.read_exact(&mut head)?;
    if &head[..4] != MAGIC {
        return Err(bad("不是影戲的訊息"));
    }
    if head[4] != VERSION {
        return Err(bad("版本不同"));
    }
    let count = read_u32(r)? as usize;
    if count > MAX_PATHS {
        return Err(bad("檔案太多"));
    }
    let mut paths = Vec::with_capacity(count);
    for _ in 0..count {
        let len = read_u32(r)? as usize;
        if len > MAX_PATH_BYTES {
            return Err(bad("路徑太長"));
        }
        let mut b = vec![0; len];
        r.read_exact(&mut b)?;
        paths.push(decode_path(&b)?);
    }
    Ok(Request {
        paths,
        fullscreen: head[5] & 1 != 0,
    })
}

// ───────────── 合併同時到達的要求 ─────────────

/// 把陸續到達的要求合併成一批（檔案總管多選按 Enter = 一個檔案一個程式，幾乎同時啟動）
#[derive(Default)]
pub struct Batcher {
    paths: Vec<PathBuf>,
    fullscreen: bool,
    first: Option<Instant>,
    last: Option<Instant>,
    pending: bool,
}

pub enum BatchPoll {
    Idle,
    /// 還在等（多久之後再看一次）
    Wait(Duration),
    Ready(Request),
}

impl Batcher {
    /// 上一個到了之後多久沒有新的就算一批
    pub const GAP: Duration = Duration::from_millis(400);
    /// 最多等多久
    pub const MAX_WAIT: Duration = Duration::from_millis(2500);

    /// 加進這一批；回傳 true = 新一批的第一個（可以先把視窗叫到前面）
    pub fn push(&mut self, req: Request, now: Instant) -> bool {
        let first = !self.pending;
        self.pending = true;
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.fullscreen |= req.fullscreen;
        for p in req.paths {
            if !self.paths.contains(&p) {
                self.paths.push(p);
            }
        }
        first
    }

    pub fn poll(&mut self, now: Instant) -> BatchPoll {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return BatchPoll::Idle;
        };
        let quiet = now.saturating_duration_since(last);
        let total = now.saturating_duration_since(first);
        if quiet >= Self::GAP || total >= Self::MAX_WAIT {
            self.first = None;
            self.last = None;
            self.pending = false;
            BatchPoll::Ready(Request {
                paths: std::mem::take(&mut self.paths),
                fullscreen: std::mem::take(&mut self.fullscreen),
            })
        } else {
            BatchPoll::Wait((Self::GAP - quiet).min(Self::MAX_WAIT - total))
        }
    }
}

// ───────────── 啟動 ─────────────

/// 鎖定檔與管道 / socket 的位置
pub struct Endpoint {
    lock: PathBuf,
    name: OsString,
}

impl Endpoint {
    /// 目前使用者的（Windows 另外分工作階段：遠端桌面與本機各自一個）
    pub fn for_current_user() -> io::Result<Self> {
        let dir = runtime_dir().ok_or_else(|| io::Error::other("找不到可以放鎖定檔的資料夾"))?;
        Self::in_dir(dir, "")
    }

    /// 指定資料夾與字尾（測試用：Windows 的管道名稱是整台電腦共用的）
    pub fn in_dir(dir: PathBuf, suffix: &str) -> io::Result<Self> {
        prepare_private_dir(&dir)?;
        #[cfg(windows)]
        {
            let sid = sys::current_user_sid().ok_or_else(|| io::Error::other("讀不到使用者 SID"))?;
            let session = sys::session_id();
            Ok(Self {
                lock: dir.join(format!("instance-{session}{suffix}.lock")),
                name: format!(r"\\.\pipe\vitascope-{session}-{sid}{suffix}").into(),
            })
        }
        #[cfg(unix)]
        {
            let sock = dir.join(format!("instance{suffix}.sock"));
            // socket 路徑有長度上限（Linux 108、macOS 104 位元組）
            if sock.as_os_str().len() >= 100 {
                return Err(io::Error::other("socket 路徑太長"));
            }
            Ok(Self {
                lock: dir.join(format!("instance{suffix}.lock")),
                name: sock.into_os_string(),
            })
        }
    }
}

/// 放鎖定檔的資料夾：本機磁碟、只有自己能用
fn runtime_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // 不用 %APPDATA%：可能是漫遊設定檔或網路磁碟
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Vitascope"))
    }
    #[cfg(unix)]
    {
        let var = if cfg!(target_os = "macos") {
            "TMPDIR"
        } else {
            "XDG_RUNTIME_DIR"
        };
        match std::env::var_os(var) {
            Some(b) if !b.is_empty() => Some(PathBuf::from(b).join("vitascope")),
            // 用登入名稱區分使用者（沒有 libc 拿不到 uid）；資料夾權限另外檢查
            _ => {
                let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
                Some(PathBuf::from(format!("/tmp/vitascope-{user}")))
            }
        }
    }
}

/// 建立資料夾；Unix 上只能是自己的（0700），不能是捷徑
fn prepare_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if let Err(e) = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
            && e.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(e);
        }
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(io::Error::other("不是資料夾"));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            // 是自己的就改回只有自己能用；別人的改不了，就不用
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| io::Error::other("資料夾別人也能用"))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        std::fs::create_dir_all(dir)
    }
}

pub enum Startup {
    /// 自己是主視窗：收別人送來的檔案
    Primary(Primary),
    /// 已經交給主視窗了，自己結束
    Forwarded,
    /// 照一般方式自己開（允許多個視窗、或出了問題）
    Standalone(String),
}

/// `forward` = false：允許多個視窗（設定或 --new-window）；鎖還空著的話照樣當主視窗
pub fn start(ep: &Endpoint, req: &Request, forward: bool, wake: Wake) -> Startup {
    let lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&ep.lock)
    {
        Ok(f) => f,
        Err(e) => return Startup::Standalone(format!("鎖定檔：{e}")),
    };
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match lock.try_lock() {
            Ok(()) => return become_primary(ep, lock, wake),
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(e)) => return Startup::Standalone(format!("鎖定：{e}")),
        }
        if !forward {
            return Startup::Standalone("允許多個視窗".to_owned());
        }
        match forward_once(ep, req) {
            Forward::Done => return Startup::Forwarded,
            Forward::Reject(why) => return Startup::Standalone(why),
            Forward::Retry(e) if Instant::now() >= deadline => return Startup::Standalone(e),
            // 主視窗剛搶到鎖還沒開始聽，或正在關閉：鎖放開了，下一圈就換自己當主視窗
            Forward::Retry(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

enum Forward {
    Done,
    Retry(String),
    Reject(String),
}

fn forward_once(ep: &Endpoint, req: &Request) -> Forward {
    let (name, req) = (ep.name.clone(), req.clone());
    let (tx, rx) = mpsc::channel();
    // Windows 的管道沒有讀寫逾時：另開執行緒，最多等 TIMEOUT
    std::thread::spawn(move || {
        let result = (|| -> io::Result<u8> {
            let mut conn = sys::connect(&name)?;
            write_request(&mut conn, &req)?;
            let mut reply = [0];
            conn.read_exact(&mut reply)?;
            Ok(reply[0])
        })();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(Ok(ACK)) => Forward::Done,
        Ok(Ok(REJECT)) => Forward::Reject("開著的影戲是不同的版本".to_owned()),
        Ok(Ok(_)) => Forward::Retry("開著的影戲正在關閉".to_owned()),
        Ok(Err(e)) if e.kind() == io::ErrorKind::PermissionDenied => Forward::Reject(e.to_string()),
        Ok(Err(e)) => Forward::Retry(e.to_string()),
        Err(_) => Forward::Reject("開著的影戲沒有回應".to_owned()),
    }
}

fn become_primary(ep: &Endpoint, lock: File, wake: Wake) -> Startup {
    // 舊的主視窗可能還沒完全結束（名稱還被佔著）：等一下下
    let deadline = Instant::now() + Duration::from_secs(2);
    let listener = loop {
        match sys::Listener::bind(&ep.name) {
            Ok(l) => break l,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            // 鎖跟著 lock 一起放掉
            Err(e) => return Startup::Standalone(format!("無法接收：{e}")),
        }
    };
    let closing = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let (stopped_tx, stopped) = mpsc::channel();
    let flag = closing.clone();
    let spawned = std::thread::Builder::new()
        .name("vitascope-instance".into())
        .spawn(move || {
            let mut listener = listener;
            loop {
                let conn = listener.accept();
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(conn) = conn else {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                let (tx, wake, flag) = (tx.clone(), wake.clone(), flag.clone());
                // 一個連線一個短命的執行緒：卡住的連線不會擋住別人
                std::thread::spawn(move || serve(conn, &tx, &*wake, &flag));
            }
            drop(listener);
            let _ = stopped_tx.send(());
        });
    if let Err(e) = spawned {
        return Startup::Standalone(format!("執行緒：{e}"));
    }
    Startup::Primary(Primary {
        rx,
        lock: Some(lock),
        closing,
        stopped,
        name: ep.name.clone(),
    })
}

fn serve(mut conn: sys::Conn, tx: &mpsc::Sender<Request>, wake: &dyn Fn(), closing: &AtomicBool) {
    if !conn.peer_is_same_user() {
        return;
    }
    let reply = match read_request(&mut conn) {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => REJECT,
        Err(_) => return,
        Ok(_) if closing.load(Ordering::SeqCst) => BUSY,
        Ok(req) => {
            if tx.send(req).is_ok() {
                wake();
                ACK
            } else {
                BUSY
            }
        }
    };
    let _ = conn.write_all(&[reply]);
    conn.finish();
}

/// 主視窗：收別人送來的檔案
pub struct Primary {
    pub rx: mpsc::Receiver<Request>,
    lock: Option<File>,
    closing: Arc<AtomicBool>,
    stopped: mpsc::Receiver<()>,
    name: OsString,
}

impl Primary {
    /// 關閉時呼叫：之後連進來的會收到「正在關閉」，等鎖放開後自己當主視窗
    pub fn shutdown(&mut self) {
        let Some(lock) = self.lock.take() else { return };
        self.closing.store(true, Ordering::SeqCst);
        // 自己連一次，把等待連線的執行緒叫醒
        let _ = sys::connect(&self.name);
        let _ = self.stopped.recv_timeout(Duration::from_millis(500));
        drop(lock);
    }
}

impl Drop for Primary {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ───────────── Windows：具名管道 ─────────────

#[cfg(windows)]
mod sys {
    use std::ffi::{OsStr, c_void};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
        PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
        WaitNamedPipeW,
    };
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// 某個程式的使用者 SID（字串）
    fn sid_of(process: HANDLE) -> Option<String> {
        // SAFETY: 標準的 Win32 呼叫；緩衝區用 u64 對齊（TOKEN_USER 裡有指標）
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return None;
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return None;
            }
            let n = (0..).take_while(|&i| *text.add(i) != 0).count();
            let s = String::from_utf16(std::slice::from_raw_parts(text, n)).ok();
            LocalFree(text.cast());
            s
        }
    }

    pub fn current_user_sid() -> Option<String> {
        sid_of(unsafe { GetCurrentProcess() })
    }

    pub fn session_id() -> u32 {
        let mut id = 0;
        unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) };
        id
    }

    /// 另一個程式是不是同一個使用者的
    fn same_user(pid: u32) -> bool {
        // SAFETY: OpenProcess 失敗時回傳 null
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let other = sid_of(h);
            CloseHandle(h);
            other.is_some() && other == current_user_sid()
        }
    }

    pub struct Conn {
        file: File,
        server: bool,
    }

    impl Conn {
        pub fn peer_is_same_user(&self) -> bool {
            let mut pid = 0;
            let ok = unsafe { GetNamedPipeClientProcessId(self.file.as_raw_handle(), &mut pid) };
            ok != 0 && same_user(pid)
        }

        /// 伺服器端：等對方讀完回覆再關（關掉管道會丟掉還沒讀的資料）
        pub fn finish(self) {
            if self.server {
                let _ = self.file.sync_all();
            }
        }
    }

    impl Read for Conn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.file.read(buf)
        }
    }

    impl Write for Conn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.file.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    /// 連到主視窗：確認對方是同一個使用者（防止別人先佔用名稱），並讓它可以跳到前面
    pub fn connect(name: &OsStr) -> io::Result<Conn> {
        let file = loop {
            match std::fs::OpenOptions::new().read(true).write(true).open(name) {
                Ok(f) => break f,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    if unsafe { WaitNamedPipeW(wide(name).as_ptr(), 1000) } == 0 {
                        return Err(e);
                    }
                }
                Err(e) => return Err(e),
            }
        };
        let mut pid = 0;
        let ok = unsafe { GetNamedPipeServerProcessId(file.as_raw_handle(), &mut pid) };
        if ok == 0 || !same_user(pid) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "管道是別的使用者的"));
        }
        // 自己是使用者點開的，有權把前景交給主視窗（之後它才能把視窗叫到最前面）
        unsafe { AllowSetForegroundWindow(pid) };
        Ok(Conn { file, server: false })
    }

    pub struct Listener {
        name: Vec<u16>,
        first: bool,
        /// 只有自己（和系統）能連線的安全描述元
        sd: *mut c_void,
    }

    // SAFETY: sd 只在建立管道時讀取，由 Listener 獨佔
    unsafe impl Send for Listener {}

    impl Listener {
        pub fn bind(name: &OsStr) -> io::Result<Self> {
            let sid = current_user_sid().ok_or_else(|| io::Error::other("讀不到使用者 SID"))?;
            let sddl = wide(OsStr::new(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})")));
            let mut sd: *mut c_void = std::ptr::null_mut();
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut())
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut l = Self {
                name: wide(name),
                first: true,
                sd,
            };
            // 先建立第一個（確定名稱是自己的），之後每次 accept 再建立下一個
            let h = l.create()?;
            unsafe { CloseHandle(h) };
            Ok(l)
        }

        fn create(&mut self) -> io::Result<HANDLE> {
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.sd,
                bInheritHandle: 0,
            };
            let first = if std::mem::take(&mut self.first) {
                FILE_FLAG_FIRST_PIPE_INSTANCE
            } else {
                0
            };
            let h = unsafe {
                CreateNamedPipeW(
                    self.name.as_ptr(),
                    PIPE_ACCESS_DUPLEX | first,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    4096,
                    4096,
                    0,
                    &sa,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(h)
        }

        /// 等下一個連線（會卡住，在自己的執行緒上呼叫）
        pub fn accept(&mut self) -> io::Result<Conn> {
            let h = self.create()?;
            let ok = unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
            if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                let e = io::Error::last_os_error();
                unsafe { CloseHandle(h) };
                return Err(e);
            }
            // SAFETY: h 是剛建立、由 File 接手的管道
            Ok(Conn {
                file: unsafe { File::from_raw_handle(h) },
                server: true,
            })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe { LocalFree(self.sd) };
        }
    }
}

// ───────────── 其他系統：Unix socket ─────────────

#[cfg(unix)]
mod sys {
    use std::ffi::OsStr;
    use std::io::{self, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    pub struct Conn(UnixStream);

    impl Conn {
        /// socket 放在只有自己能進的資料夾，連得進來的就是自己
        pub fn peer_is_same_user(&self) -> bool {
            true
        }
        pub fn finish(self) {}
    }

    impl Read for Conn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for Conn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    pub fn connect(name: &OsStr) -> io::Result<Conn> {
        let s = UnixStream::connect(Path::new(name))?;
        s.set_read_timeout(Some(Duration::from_secs(3)))?;
        Ok(Conn(s))
    }

    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
    }

    impl Listener {
        pub fn bind(name: &OsStr) -> io::Result<Self> {
            let path = PathBuf::from(name);
            // 當掉時留下的 socket 檔：鎖在自己手上，刪的不會是別人正在用的
            let _ = std::fs::remove_file(&path);
            Ok(Self {
                inner: UnixListener::bind(&path)?,
                path,
            })
        }

        pub fn accept(&mut self) -> io::Result<Conn> {
            let (s, _) = self.inner.accept()?;
            s.set_read_timeout(Some(Duration::from_secs(3)))?;
            Ok(Conn(s))
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let req = Request {
            paths: vec![PathBuf::from("C:/影片/第1集.mkv"), PathBuf::from("/tmp/a b.mp4")],
            fullscreen: true,
        };
        let mut buf = Vec::new();
        write_request(&mut buf, &req).unwrap();
        assert_eq!(read_request(&mut buf.as_slice()).unwrap(), req);
    }

    #[test]
    fn garbage_and_other_versions_are_rejected() {
        let e = read_request(&mut &b"HELLO WORLD"[..]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let mut v2 = Vec::from(&MAGIC[..]);
        v2.extend([2, 0, 0, 0, 0, 0]);
        assert_eq!(
            read_request(&mut v2.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // 謊報的長度不會讓它配置一大塊記憶體
        let mut huge = Vec::from(&MAGIC[..]);
        huge.extend([VERSION, 0]);
        huge.extend(1u32.to_le_bytes());
        huge.extend(u32::MAX.to_le_bytes());
        assert_eq!(
            read_request(&mut huge.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn batcher_merges_a_burst() {
        let t0 = Instant::now();
        let mut b = Batcher::default();
        let req = |p: &str| Request {
            paths: vec![PathBuf::from(p)],
            fullscreen: false,
        };
        assert!(b.push(req("a"), t0), "第一個");
        assert!(!b.push(req("b"), t0 + Duration::from_millis(100)));
        assert!(!b.push(req("a"), t0 + Duration::from_millis(200)), "重複的不算");
        assert!(matches!(b.poll(t0 + Duration::from_millis(300)), BatchPoll::Wait(_)));
        match b.poll(t0 + Duration::from_millis(700)) {
            BatchPoll::Ready(r) => assert_eq!(r.paths, [PathBuf::from("a"), PathBuf::from("b")]),
            _ => panic!("應該好了"),
        }
        assert!(matches!(b.poll(t0 + Duration::from_millis(800)), BatchPoll::Idle));
        // 一直有新的進來：最多等 MAX_WAIT
        let mut b = Batcher::default();
        for i in 0..30 {
            b.push(req(&i.to_string()), t0 + Duration::from_millis(i * 100));
        }
        assert!(matches!(b.poll(t0 + Duration::from_millis(2600)), BatchPoll::Ready(_)));
    }
}
