//! libmpv 的安全包裝：只包播放器用得到的部分。
//!
//! 刻意不用 `libmpv2` crate：它對 Node 型別屬性會 `unimplemented!()`，
//! `get_info` 也會 panic。底層綁定用 `libmpv2-sys`，這一層自己掌控。

pub mod render;

use libmpv2_sys as sys;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::ptr::NonNull;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: i32,
    pub context: String,
}

impl Error {
    fn new(code: c_int, context: impl Into<String>) -> Self {
        Self {
            code,
            context: context.into(),
        }
    }

    /// mpv 對錯誤碼的英文說明，例如 "loading failed"。
    pub fn description(&self) -> String {
        error_string(self.code)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.description())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub fn error_string(code: i32) -> String {
    // SAFETY: mpv_error_string 回傳靜態字串，未知錯誤碼也會回傳 "unknown error"
    unsafe { CStr::from_ptr(sys::mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned()
}

fn check(code: c_int, context: impl FnOnce() -> String) -> Result<()> {
    if code >= 0 {
        Ok(())
    } else {
        Err(Error::new(code, context()))
    }
}

fn cstring(s: &str) -> CString {
    // mpv 的名稱和參數都不會有 NUL；真的有就截斷，不要整個失敗
    CString::new(s).unwrap_or_else(|e| {
        let pos = e.nul_position();
        CString::new(&s[..pos]).unwrap()
    })
}

/// 觀察屬性時要求的資料格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Flag,
    Int64,
    Double,
    /// 字串。Node 型別（例如 `track-list`）用這個格式會拿到 JSON。
    String,
}

impl Format {
    fn raw(self) -> sys::mpv_format {
        match self {
            Format::Flag => sys::mpv_format_MPV_FORMAT_FLAG,
            Format::Int64 => sys::mpv_format_MPV_FORMAT_INT64,
            Format::Double => sys::mpv_format_MPV_FORMAT_DOUBLE,
            Format::String => sys::mpv_format_MPV_FORMAT_STRING,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 屬性目前不可用（例如沒有開檔時的 `duration`）
    None,
    Flag(bool),
    Int64(i64),
    Double(f64),
    String(String),
}

impl Value {
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Double(v) => Some(v),
            Value::Int64(v) => Some(v as f64),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::Int64(v) => Some(v),
            Value::Double(v) => Some(v as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Flag(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// SAFETY: `format` 和 `data` 必須來自同一個 `mpv_event_property`
    unsafe fn from_raw(format: sys::mpv_format, data: *mut c_void) -> Value {
        if data.is_null() {
            return Value::None;
        }
        unsafe {
            match format {
                sys::mpv_format_MPV_FORMAT_FLAG => Value::Flag(*(data as *const c_int) != 0),
                sys::mpv_format_MPV_FORMAT_INT64 => Value::Int64(*(data as *const i64)),
                sys::mpv_format_MPV_FORMAT_DOUBLE => Value::Double(*(data as *const f64)),
                sys::mpv_format_MPV_FORMAT_STRING | sys::mpv_format_MPV_FORMAT_OSD_STRING => {
                    let s = *(data as *const *const c_char);
                    if s.is_null() {
                        Value::None
                    } else {
                        Value::String(CStr::from_ptr(s).to_string_lossy().into_owned())
                    }
                }
                _ => Value::None,
            }
        }
    }
}

/// 要送給 mpv 的 node（只做設定用）：字串清單、鍵值清單、`chapter-list` 這類
/// 不能用逗號字串表示的值（字串清單的項目裡可能有逗號）。
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Str(String),
    Int(i64),
    Double(f64),
    Flag(bool),
    Array(Vec<Node>),
    /// 鍵值清單，照給的順序；鍵裡的 NUL 跟字串一樣截斷
    Map(Vec<(String, Node)>),
}

impl Node {
    /// 字串清單，例如 `http-header-fields`、`glsl-shaders`
    pub fn strings<S: Into<String>>(items: impl IntoIterator<Item = S>) -> Node {
        Node::Array(items.into_iter().map(|s| Node::Str(s.into())).collect())
    }
}

/// `Node` 轉成的 C `mpv_node` 樹。每一塊記憶體都用原始指標記在這裡（`into_raw`），
/// 活到 mpv 讀完（`mpv_set_property` 回傳前 mpv 會複製一份），Drop 時全部釋放。
/// 不用 `Vec<Vec<mpv_node>>` 之類直接持有：搬動 Box、Vec 可能讓之前取得的指標失效
struct NodeTree {
    root: sys::mpv_node,
    strings: Vec<*mut c_char>,
    values: Vec<*mut [sys::mpv_node]>,
    keys: Vec<*mut [*mut c_char]>,
    lists: Vec<*mut sys::mpv_node_list>,
}

impl NodeTree {
    /// 項目太多（超過 c_int）時回傳 None
    fn new(node: &Node) -> Option<NodeTree> {
        let mut tree = NodeTree {
            // SAFETY: mpv_node 是純資料，全 0 = MPV_FORMAT_NONE
            root: unsafe { std::mem::zeroed() },
            strings: Vec::new(),
            values: Vec::new(),
            keys: Vec::new(),
            lists: Vec::new(),
        };
        // 中途失敗：已經配置的都記在 tree 裡，Drop 會釋放
        tree.root = tree.build(node)?;
        Some(tree)
    }

    fn string(&mut self, s: &str) -> *mut c_char {
        let p = cstring(s).into_raw();
        self.strings.push(p);
        p
    }

    fn build(&mut self, node: &Node) -> Option<sys::mpv_node> {
        // SAFETY: 同上，之後只填 format 對應的 union 欄位
        let mut out: sys::mpv_node = unsafe { std::mem::zeroed() };
        match node {
            Node::Str(s) => {
                out.format = sys::mpv_format_MPV_FORMAT_STRING;
                out.u.string = self.string(s);
            }
            Node::Int(v) => {
                out.format = sys::mpv_format_MPV_FORMAT_INT64;
                out.u.int64 = *v;
            }
            Node::Double(v) => {
                out.format = sys::mpv_format_MPV_FORMAT_DOUBLE;
                out.u.double_ = *v;
            }
            Node::Flag(v) => {
                // MPV_FORMAT_FLAG 的資料是 int
                out.format = sys::mpv_format_MPV_FORMAT_FLAG;
                out.u.flag = (*v).into();
            }
            Node::Array(items) => {
                let values = items.iter().map(|n| self.build(n)).collect::<Option<Vec<_>>>()?;
                out.format = sys::mpv_format_MPV_FORMAT_NODE_ARRAY;
                out.u.list = self.list(values, None)?;
            }
            Node::Map(pairs) => {
                // 先建所有的值（遞迴），再建鍵；兩邊數量一定一樣
                let values = pairs.iter().map(|(_, n)| self.build(n)).collect::<Option<Vec<_>>>()?;
                let keys = pairs.iter().map(|(k, _)| self.string(k)).collect();
                out.format = sys::mpv_format_MPV_FORMAT_NODE_MAP;
                out.u.list = self.list(values, Some(keys))?;
            }
        }
        Some(out)
    }

    /// 陣列、鍵值清單一定要有 mpv_node_list（mpv 不檢查 u.list 是不是 NULL）；
    /// 空的清單 values、keys 用 NULL（client.h：num 為 0 時可以是 NULL）
    fn list(&mut self, values: Vec<sys::mpv_node>, keys: Option<Vec<*mut c_char>>) -> Option<*mut sys::mpv_node_list> {
        let num = c_int::try_from(values.len()).ok()?;
        let values = if values.is_empty() {
            std::ptr::null_mut()
        } else {
            let p = Box::into_raw(values.into_boxed_slice());
            self.values.push(p);
            p as *mut sys::mpv_node
        };
        let keys = match keys {
            Some(k) if !k.is_empty() => {
                let p = Box::into_raw(k.into_boxed_slice());
                self.keys.push(p);
                p as *mut *mut c_char
            }
            _ => std::ptr::null_mut(),
        };
        let list = Box::into_raw(Box::new(sys::mpv_node_list { num, values, keys }));
        self.lists.push(list);
        Some(list)
    }
}

impl Drop for NodeTree {
    fn drop(&mut self) {
        // SAFETY: 每個指標都是上面 into_raw 得到的，只在這裡釋放一次；mpv 不保留這些記憶體
        unsafe {
            for p in self.lists.drain(..) {
                drop(Box::from_raw(p));
            }
            for p in self.values.drain(..) {
                drop(Box::from_raw(p));
            }
            for p in self.keys.drain(..) {
                drop(Box::from_raw(p));
            }
            for p in self.strings.drain(..) {
                drop(CString::from_raw(p));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error,
    Redirect,
    Unknown(u32),
}

impl EndReason {
    fn from_raw(r: sys::mpv_end_file_reason) -> Self {
        match r {
            sys::mpv_end_file_reason_MPV_END_FILE_REASON_EOF => EndReason::Eof,
            sys::mpv_end_file_reason_MPV_END_FILE_REASON_STOP => EndReason::Stop,
            sys::mpv_end_file_reason_MPV_END_FILE_REASON_QUIT => EndReason::Quit,
            sys::mpv_end_file_reason_MPV_END_FILE_REASON_ERROR => EndReason::Error,
            sys::mpv_end_file_reason_MPV_END_FILE_REASON_REDIRECT => EndReason::Redirect,
            other => EndReason::Unknown(other),
        }
    }
}

/// mpv 事件。資料都已複製出來，不會參照 mpv 的記憶體。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Shutdown,
    Log {
        prefix: String,
        level: String,
        text: String,
    },
    StartFile,
    FileLoaded,
    EndFile {
        reason: EndReason,
        error: Option<Error>,
    },
    VideoReconfig,
    AudioReconfig,
    Seek,
    PlaybackRestart,
    PropertyChange {
        id: u64,
        name: String,
        value: Value,
    },
    CommandReply {
        id: u64,
        result: Result<()>,
    },
    QueueOverflow,
    /// 註冊過的 hook 觸發了（`hook_add`）。mpv 停在那裡等，處理完一定要 `hook_continue(id)` 剛好一次，
    /// 不然載入永遠卡住。`userdata` 是註冊時給的值，`id` 是 mpv 給的序號
    Hook {
        name: String,
        id: u64,
        userdata: u64,
    },
    Other(u32),
}

type Callback = Box<dyn Fn() + Send + Sync + 'static>;

/// 一個 mpv 播放核心。mpv 的 client API 是執行緒安全的，所以可以 Send + Sync。
/// 只在有 Lua 的 libmpv 才有的選項（影戲都是把它們關掉）
const SCRIPT_OPTIONS: &[&str] = &["osc", "ytdl", "load-scripts", "load-stats-overlay"];

/// `mpv_get_time_ns` 的型別（client.h）
#[cfg(all(unix, not(target_os = "macos")))]
type GetTimeNs = unsafe extern "C" fn(*mut sys::mpv_handle) -> i64;

/// Linux：執行時才找 `mpv_get_time_ns`（libmpv 0.37 起才有）。Linux 版用系統的 libmpv，
/// 直接連結的話舊的 libmpv 在程式開始之前就被系統的載入器擋掉（undefined symbol），
/// `Mpv::new` 的版本檢查沒機會說明要更新 mpv。Windows、macOS 附帶自己的引擎，照常連結
#[cfg(all(unix, not(target_os = "macos")))]
fn get_time_ns() -> Option<GetTimeNs> {
    static FOUND: std::sync::OnceLock<Option<GetTimeNs>> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        // SAFETY: RTLD_DEFAULT 在已經載入的程式庫（包括連結的 libmpv）裡找，名稱是 NUL 結尾的常數
        let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"mpv_get_time_ns".as_ptr()) };
        // SAFETY: 找到的就是 client.h 宣告的這個函式
        (!found.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, GetTimeNs>(found) })
    })
}

/// 找得到 `mpv_get_time_ns`（Linux 執行時才找；其他平台直接連結，一定有）。測試用
#[doc(hidden)]
pub fn has_time_ns() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        get_time_ns().is_some()
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        true
    }
}

pub struct Mpv {
    handle: NonNull<sys::mpv_handle>,
    wakeup: Option<Box<Callback>>,
    /// 已經以 `Event::Hook` 交出去、還沒 continue 的 hook 序號。對同一個 hook continue 兩次、
    /// 或給錯序號是未定義行為（client.h），所以 `hook_continue` 只放行這裡有的
    hooks_pending: Mutex<Vec<u64>>,
}

// SAFETY: libmpv 的 client API 可以從任何執行緒呼叫（client.h「Thread safety」一節）
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    /// 建立並初始化。`options` 會在 `mpv_initialize` 之前設定，
    /// 只能在初始化前設定的選項（例如 `vo`、`config`）要放這裡。
    pub fn new(options: &[(&str, &str)]) -> Result<Self> {
        let api = unsafe { sys::mpv_client_api_version() } as u64;
        // 2.2（mpv 0.37）起才有 mpv_get_time_ns（畫面輸出挑時間取影格要用）
        if api >> 16 != 2 || api & 0xffff < 2 {
            return Err(Error::new(
                sys::mpv_error_MPV_ERROR_UNSUPPORTED,
                crate::tf!(
                    "libmpv client API {}.{} 不相容（需要 2.2 以上的 2.x，也就是 mpv 0.37 以上）",
                    "libmpv client API {}.{} is not compatible (2.2 or a newer 2.x required, i.e. mpv 0.37 or newer)",
                    api >> 16,
                    api & 0xffff
                ),
            ));
        }

        let raw = unsafe { sys::mpv_create() };
        let handle = NonNull::new(raw).ok_or_else(|| Error::new(sys::mpv_error_MPV_ERROR_NOMEM, "mpv_create"))?;
        // 先包起來：之後任何一步失敗，Drop 都會正確釋放
        let mpv = Mpv {
            handle,
            wakeup: None,
            hooks_pending: Mutex::new(Vec::new()),
        };

        for (name, value) in options {
            let (n, v) = (cstring(name), cstring(value));
            let code = unsafe { sys::mpv_set_option_string(mpv.raw(), n.as_ptr(), v.as_ptr()) };
            // 不同建置的 libmpv 選項不完全相同（例如沒編 Lua 就沒有 osc、ytdl），
            // 沒有的選項略過即可，不影響播放。Lua 腳本相關的選項本來就是要關掉的，不用提示
            if code == sys::mpv_error_MPV_ERROR_OPTION_NOT_FOUND {
                if !SCRIPT_OPTIONS.contains(name) {
                    eprintln!("[vitascope] 這個 libmpv 沒有選項 {name}，略過");
                }
                continue;
            }
            check(code, || {
                crate::tf!("設定選項 {name}={value}", "setting option {name}={value}")
            })?;
        }
        check(unsafe { sys::mpv_initialize(mpv.raw()) }, || "mpv_initialize".into())?;
        Ok(mpv)
    }

    pub(crate) fn raw(&self) -> *mut sys::mpv_handle {
        self.handle.as_ptr()
    }

    /// mpv 內部的時鐘（奈秒，跟 render API 的影格預定時間同一個基準）。
    /// 任何時候、在畫面輸出的執行緒上都能呼叫（client.h：safe from render threads）；libmpv 0.37 起才有
    pub fn time_ns(&self) -> i64 {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            match get_time_ns() {
                // SAFETY: 跟 client.h 的宣告一樣的函式，handle 有效
                Some(f) => unsafe { f(self.raw()) },
                // 不會發生（`new` 已經擋掉 0.37 以前的 libmpv）：用微秒的時鐘，同一個基準
                None => self.time_us().saturating_mul(1000),
            }
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        {
            unsafe { sys::mpv_get_time_ns(self.raw()) }
        }
    }

    /// 同 `time_ns`，單位是微秒
    pub fn time_us(&self) -> i64 {
        unsafe { sys::mpv_get_time_us(self.raw()) }
    }

    /// 執行指令，例如 `["loadfile", path]`、`["seek", "10", "relative"]`。
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let code = unsafe { sys::mpv_command(self.raw(), ptrs.as_mut_ptr()) };
        check(code, || crate::tf!("指令 {}", "command {}", args.join(" ")))
    }

    /// 非同步指令，結果以 `Event::CommandReply { id }` 回報。
    pub fn command_async(&self, id: u64, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let code = unsafe { sys::mpv_command_async(self.raw(), id, ptrs.as_mut_ptr()) };
        check(code, || crate::tf!("非同步指令 {}", "async command {}", args.join(" ")))
    }

    pub fn set_property<T: PropertyValue>(&self, name: &str, value: T) -> Result<()> {
        let n = cstring(name);
        let code =
            value.with_raw(|format, data| unsafe { sys::mpv_set_property(self.raw(), n.as_ptr(), format, data) });
        check(code, || crate::tf!("設定屬性 {name}", "setting property {name}"))
    }

    pub fn get_property<T: PropertyValue>(&self, name: &str) -> Result<T> {
        let n = cstring(name);
        T::read(|format, data| unsafe { sys::mpv_get_property(self.raw(), n.as_ptr(), format, data) })
            .map_err(|code| Error::new(code, crate::tf!("讀取屬性 {name}", "reading property {name}")))
    }

    /// 以字串讀取屬性。Node 型別（`track-list`、`metadata`…）會拿到 JSON。
    pub fn get_string(&self, name: &str) -> Result<String> {
        self.get_property::<String>(name)
    }

    /// 讀字串清單（`glsl-shaders` 之類）的每一項。用 Node 讀：讀成字串時分隔字元各版本不同
    /// （0.37 是逗號、新版的路徑清單是平台的路徑分隔字元），路徑裡也可能有逗號
    pub fn get_string_list(&self, name: &str) -> Result<Vec<String>> {
        let n = cstring(name);
        // SAFETY: mpv_node 是純資料（全 0 = MPV_FORMAT_NONE）；成功時 mpv 填好內容，讀完用 mpv_free_node_contents 釋放
        let mut node: sys::mpv_node = unsafe { std::mem::zeroed() };
        let code = unsafe {
            sys::mpv_get_property(
                self.raw(),
                n.as_ptr(),
                sys::mpv_format_MPV_FORMAT_NODE,
                &mut node as *mut sys::mpv_node as *mut c_void,
            )
        };
        check(code, || crate::tf!("讀取屬性 {name}", "reading property {name}"))?;
        // SAFETY: format 決定 union 裡哪一個欄位有效；陣列的 values 有 num 個
        let list = unsafe {
            match node.format {
                sys::mpv_format_MPV_FORMAT_NODE_ARRAY if !node.u.list.is_null() => {
                    let l = &*node.u.list;
                    let values: &[sys::mpv_node] = if l.num > 0 && !l.values.is_null() {
                        std::slice::from_raw_parts(l.values, l.num as usize)
                    } else {
                        &[]
                    };
                    Ok(values
                        .iter()
                        .filter(|v| v.format == sys::mpv_format_MPV_FORMAT_STRING && !v.u.string.is_null())
                        .map(|v| CStr::from_ptr(v.u.string).to_string_lossy().into_owned())
                        .collect())
                }
                sys::mpv_format_MPV_FORMAT_NONE => Ok(Vec::new()),
                _ => Err(Error::new(
                    sys::mpv_error_MPV_ERROR_PROPERTY_FORMAT,
                    crate::tf!("{name} 不是字串清單", "{name} is not a string list"),
                )),
            }
        };
        unsafe { sys::mpv_free_node_contents(&mut node) };
        list
    }

    /// 用 mpv_node 設定屬性或選項（`http-header-fields`、`chapter-list`、`file-local-options/…`）。
    /// 字串清單用這個設定，項目裡的逗號不會被當成分隔
    pub fn set_node(&self, name: &str, value: &Node) -> Result<()> {
        let n = cstring(name);
        let Some(mut tree) = NodeTree::new(value) else {
            return Err(Error::new(
                sys::mpv_error_MPV_ERROR_INVALID_PARAMETER,
                crate::tf!("設定屬性 {name}：項目太多", "setting property {name}: too many items"),
            ));
        };
        // SAFETY: tree 擁有整棵樹的記憶體，活到 mpv_set_property 回傳之後；mpv 只讀、會自己複製一份
        let code = unsafe {
            sys::mpv_set_property(
                self.raw(),
                n.as_ptr(),
                sys::mpv_format_MPV_FORMAT_NODE,
                &mut tree.root as *mut sys::mpv_node as *mut c_void,
            )
        };
        drop(tree);
        check(code, || crate::tf!("設定屬性 {name}", "setting property {name}"))
    }

    pub fn observe(&self, id: u64, name: &str, format: Format) -> Result<()> {
        let n = cstring(name);
        let code = unsafe { sys::mpv_observe_property(self.raw(), id, n.as_ptr(), format.raw()) };
        check(code, || crate::tf!("觀察屬性 {name}", "observing property {name}"))
    }

    /// 要求 mpv 把 `level` 以上的記錄訊息以 `Event::Log` 送來（"error"、"warn"、"info"…）。
    pub fn request_log_messages(&self, level: &str) -> Result<()> {
        let l = cstring(level);
        check(unsafe { sys::mpv_request_log_messages(self.raw(), l.as_ptr()) }, || {
            format!("request_log_messages {level}")
        })
    }

    /// 註冊 hook（`on_load`、`on_load_fail`…，見 mpv 說明的 Hooks 一節）。觸發時這個 handle 收到
    /// `Event::Hook { userdata, .. }`，只能在這個 handle 上 continue。`priority` 越小越先執行，
    /// 同優先順序照註冊順序。不認得的名稱不會觸發。hook 不能取消註冊，handle 釋放時 mpv 會自己放行
    pub fn hook_add(&self, userdata: u64, name: &str, priority: i32) -> Result<()> {
        let n = cstring(name);
        let code = unsafe { sys::mpv_hook_add(self.raw(), userdata, n.as_ptr(), priority) };
        check(code, || crate::tf!("註冊 hook {name}", "adding hook {name}"))
    }

    /// 讓停在 hook 的 mpv 繼續。每個 `Event::Hook` 剛好一次；序號不是等待中的（已經 continue 過、
    /// 或根本沒收到）就回傳錯誤，不交給 mpv。任何執行緒都可以呼叫
    pub fn hook_continue(&self, id: u64) -> Result<()> {
        let pending = {
            let mut list = self.hooks_pending.lock().unwrap_or_else(|e| e.into_inner());
            list.iter().position(|&x| x == id).map(|i| list.swap_remove(i))
        };
        if pending.is_none() {
            return Err(Error::new(
                sys::mpv_error_MPV_ERROR_INVALID_PARAMETER,
                crate::tf!("hook {id} 不是等待中的 hook", "hook {id} is not pending"),
            ));
        }
        check(unsafe { sys::mpv_hook_continue(self.raw(), id) }, || {
            crate::tf!("繼續 hook {id}", "continuing hook {id}")
        })
    }

    /// 還沒 continue 的 hook 數（自動測試用）
    pub fn hooks_pending(&self) -> usize {
        self.hooks_pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 等待下一個事件；`timeout` 為 0 時只檢查不等待。沒有事件時回傳 `None`。
    pub fn wait_event(&self, timeout: f64) -> Option<Event> {
        // SAFETY: mpv_wait_event 回傳的指標在下一次呼叫前有效，這裡立刻把資料複製出來
        let ev = unsafe { &*sys::mpv_wait_event(self.raw(), timeout) };
        let str_of = |p: *const c_char| -> String {
            if p.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
            }
        };
        let event = match ev.event_id {
            sys::mpv_event_id_MPV_EVENT_NONE => return None,
            sys::mpv_event_id_MPV_EVENT_SHUTDOWN => Event::Shutdown,
            sys::mpv_event_id_MPV_EVENT_LOG_MESSAGE => {
                let m = unsafe { &*(ev.data as *const sys::mpv_event_log_message) };
                Event::Log {
                    prefix: str_of(m.prefix),
                    level: str_of(m.level),
                    text: str_of(m.text),
                }
            }
            sys::mpv_event_id_MPV_EVENT_START_FILE => Event::StartFile,
            sys::mpv_event_id_MPV_EVENT_FILE_LOADED => Event::FileLoaded,
            sys::mpv_event_id_MPV_EVENT_END_FILE => {
                let e = unsafe { &*(ev.data as *const sys::mpv_event_end_file) };
                let reason = EndReason::from_raw(e.reason);
                let error = (e.error < 0).then(|| Error::new(e.error, crate::tr!("播放失敗", "Playback failed")));
                Event::EndFile { reason, error }
            }
            sys::mpv_event_id_MPV_EVENT_VIDEO_RECONFIG => Event::VideoReconfig,
            sys::mpv_event_id_MPV_EVENT_AUDIO_RECONFIG => Event::AudioReconfig,
            sys::mpv_event_id_MPV_EVENT_SEEK => Event::Seek,
            sys::mpv_event_id_MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
            sys::mpv_event_id_MPV_EVENT_PROPERTY_CHANGE => {
                let p = unsafe { &*(ev.data as *const sys::mpv_event_property) };
                Event::PropertyChange {
                    id: ev.reply_userdata,
                    name: str_of(p.name),
                    value: unsafe { Value::from_raw(p.format, p.data) },
                }
            }
            sys::mpv_event_id_MPV_EVENT_COMMAND_REPLY => Event::CommandReply {
                id: ev.reply_userdata,
                result: check(ev.error, || crate::tr!("非同步指令", "async command").into()),
            },
            sys::mpv_event_id_MPV_EVENT_QUEUE_OVERFLOW => Event::QueueOverflow,
            sys::mpv_event_id_MPV_EVENT_HOOK => {
                let h = unsafe { &*(ev.data as *const sys::mpv_event_hook) };
                // 先記下來，hook_continue 才放行這個序號
                self.hooks_pending.lock().unwrap_or_else(|e| e.into_inner()).push(h.id);
                Event::Hook {
                    name: str_of(h.name),
                    id: h.id,
                    userdata: ev.reply_userdata,
                }
            }
            other => Event::Other(other),
        };
        Some(event)
    }

    /// 有新事件時呼叫 `f`。`f` 會在 mpv 的執行緒上執行，
    /// 不可以在裡面呼叫任何 mpv API，只能用來喚醒別的執行緒（例如要求 egui 重繪）。
    pub fn set_wakeup_callback(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let boxed: Box<Callback> = Box::new(Box::new(f));
        let data = &*boxed as *const Callback as *mut c_void;
        unsafe { sys::mpv_set_wakeup_callback(self.raw(), Some(trampoline), data) };
        // 先換上新的 callback，舊的才能安全釋放
        self.wakeup = Some(boxed);
    }

    /// 叫醒正在等的 `wait_event`（它回傳 `None`），也會呼叫 wakeup callback（在呼叫這個函式的執行緒上）。
    /// 任何執行緒都可以呼叫；背景工作做完時用它通知處理事件的執行緒
    pub fn wakeup(&self) {
        unsafe { sys::mpv_wakeup(self.raw()) }
    }
}

unsafe extern "C" fn trampoline(data: *mut c_void) {
    // SAFETY: data 指向 Mpv / RenderContext 持有的 Box<Callback>，生命週期涵蓋 callback 註冊期間
    let f = unsafe { &*(data as *const Callback) };
    f();
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe {
            sys::mpv_set_wakeup_callback(self.raw(), None, std::ptr::null_mut());
            sys::mpv_terminate_destroy(self.raw());
        }
    }
}

/// 可以直接讀寫的屬性型別。
pub trait PropertyValue: Sized {
    #[doc(hidden)]
    fn with_raw(&self, f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> c_int;
    #[doc(hidden)]
    fn read(f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> std::result::Result<Self, c_int>;
}

impl PropertyValue for f64 {
    fn with_raw(&self, f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> c_int {
        let mut v = *self;
        f(sys::mpv_format_MPV_FORMAT_DOUBLE, &mut v as *mut f64 as *mut c_void)
    }
    fn read(f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> std::result::Result<Self, c_int> {
        let mut v = 0f64;
        let code = f(sys::mpv_format_MPV_FORMAT_DOUBLE, &mut v as *mut f64 as *mut c_void);
        if code < 0 { Err(code) } else { Ok(v) }
    }
}

impl PropertyValue for i64 {
    fn with_raw(&self, f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> c_int {
        let mut v = *self;
        f(sys::mpv_format_MPV_FORMAT_INT64, &mut v as *mut i64 as *mut c_void)
    }
    fn read(f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> std::result::Result<Self, c_int> {
        let mut v = 0i64;
        let code = f(sys::mpv_format_MPV_FORMAT_INT64, &mut v as *mut i64 as *mut c_void);
        if code < 0 { Err(code) } else { Ok(v) }
    }
}

impl PropertyValue for bool {
    fn with_raw(&self, f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> c_int {
        // MPV_FORMAT_FLAG 的資料是 int，不是 C 的 bool
        let mut v: c_int = (*self).into();
        f(sys::mpv_format_MPV_FORMAT_FLAG, &mut v as *mut c_int as *mut c_void)
    }
    fn read(f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> std::result::Result<Self, c_int> {
        let mut v: c_int = 0;
        let code = f(sys::mpv_format_MPV_FORMAT_FLAG, &mut v as *mut c_int as *mut c_void);
        if code < 0 { Err(code) } else { Ok(v != 0) }
    }
}

impl PropertyValue for String {
    fn with_raw(&self, f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> c_int {
        let c = cstring(self);
        let mut p = c.as_ptr();
        f(
            sys::mpv_format_MPV_FORMAT_STRING,
            &mut p as *mut *const c_char as *mut c_void,
        )
    }
    fn read(f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> std::result::Result<Self, c_int> {
        let mut p: *mut c_char = std::ptr::null_mut();
        let code = f(
            sys::mpv_format_MPV_FORMAT_STRING,
            &mut p as *mut *mut c_char as *mut c_void,
        );
        if code < 0 {
            return Err(code);
        }
        if p.is_null() {
            return Ok(String::new());
        }
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { sys::mpv_free(p as *mut c_void) };
        Ok(s)
    }
}

impl PropertyValue for &str {
    fn with_raw(&self, f: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> c_int {
        let c = cstring(self);
        let mut p = c.as_ptr();
        f(
            sys::mpv_format_MPV_FORMAT_STRING,
            &mut p as *mut *const c_char as *mut c_void,
        )
    }
    fn read(_: impl FnOnce(sys::mpv_format, *mut c_void) -> c_int) -> std::result::Result<Self, c_int> {
        // 借用的字串無法擁有 mpv 配置的記憶體；讀取請用 String
        Err(sys::mpv_error_MPV_ERROR_PROPERTY_FORMAT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把 C 的 mpv_node 樹讀回 Node，同時檢查 mpv 會依賴的形狀
    /// SAFETY: `n` 必須是 NodeTree 建出來、還活著的樹
    unsafe fn read_back(n: &sys::mpv_node) -> Node {
        unsafe {
            let items = |l: *mut sys::mpv_node_list, map: bool| -> Vec<(Option<String>, Node)> {
                assert!(!l.is_null(), "陣列、鍵值清單一定要有 mpv_node_list");
                let l = &*l;
                assert!(l.num >= 0);
                if l.num == 0 {
                    assert!(l.values.is_null() && l.keys.is_null(), "空的清單用 NULL");
                    return Vec::new();
                }
                let values = std::slice::from_raw_parts(l.values, l.num as usize);
                if map {
                    let keys = std::slice::from_raw_parts(l.keys, l.num as usize);
                    keys.iter()
                        .zip(values)
                        .map(|(&k, v)| {
                            assert!(!k.is_null(), "鍵不可以是 NULL");
                            (Some(CStr::from_ptr(k).to_str().unwrap().to_owned()), read_back(v))
                        })
                        .collect()
                } else {
                    assert!(l.keys.is_null(), "陣列沒有鍵");
                    values.iter().map(|v| (None, read_back(v))).collect()
                }
            };
            match n.format {
                sys::mpv_format_MPV_FORMAT_STRING => {
                    assert!(!n.u.string.is_null());
                    Node::Str(CStr::from_ptr(n.u.string).to_str().unwrap().to_owned())
                }
                sys::mpv_format_MPV_FORMAT_INT64 => Node::Int(n.u.int64),
                sys::mpv_format_MPV_FORMAT_DOUBLE => Node::Double(n.u.double_),
                sys::mpv_format_MPV_FORMAT_FLAG => {
                    assert!(matches!(n.u.flag, 0 | 1), "旗標是 int 0 或 1");
                    Node::Flag(n.u.flag != 0)
                }
                sys::mpv_format_MPV_FORMAT_NODE_ARRAY => {
                    Node::Array(items(n.u.list, false).into_iter().map(|(_, v)| v).collect())
                }
                sys::mpv_format_MPV_FORMAT_NODE_MAP => Node::Map(
                    items(n.u.list, true)
                        .into_iter()
                        .map(|(k, v)| (k.unwrap(), v))
                        .collect(),
                ),
                other => panic!("不該出現的 format {other}"),
            }
        }
    }

    fn round_trip(node: &Node) -> Node {
        let tree = NodeTree::new(node).unwrap();
        // SAFETY: tree 還活著
        unsafe { read_back(&tree.root) }
    }

    #[test]
    fn node_tree_matches_the_rust_value() {
        let chapters = Node::Array(vec![
            Node::Map(vec![
                ("title".into(), Node::Str("開場".into())),
                ("time".into(), Node::Double(0.0)),
            ]),
            Node::Map(vec![
                ("title".into(), Node::Str("A, B".into())),
                ("time".into(), Node::Int(90)),
            ]),
        ]);
        assert_eq!(round_trip(&chapters), chapters);
        let mixed = Node::Map(vec![
            ("empty-array".into(), Node::Array(Vec::new())),
            ("empty-map".into(), Node::Map(Vec::new())),
            ("flags".into(), Node::Array(vec![Node::Flag(true), Node::Flag(false)])),
            (
                "nested".into(),
                Node::Array(vec![Node::Array(vec![Node::Map(vec![("深".into(), Node::Int(-1))])])]),
            ),
            ("".into(), Node::Str(String::new())),
            ("max".into(), Node::Int(i64::MAX)),
            ("pi".into(), Node::Double(std::f64::consts::PI)),
        ]);
        assert_eq!(round_trip(&mixed), mixed);
        for scalar in [
            Node::Str("x".into()),
            Node::Int(7),
            Node::Double(-0.5),
            Node::Flag(true),
        ] {
            assert_eq!(round_trip(&scalar), scalar);
        }
        assert_eq!(round_trip(&Node::Array(Vec::new())), Node::Array(Vec::new()));
        assert_eq!(round_trip(&Node::Map(Vec::new())), Node::Map(Vec::new()));
    }

    #[test]
    fn node_strings_keep_commas_and_truncate_at_nul() {
        let headers = Node::strings(["X-A: 1,2", "Cookie: a=b; c=d"]);
        assert_eq!(
            headers,
            Node::Array(vec![Node::Str("X-A: 1,2".into()), Node::Str("Cookie: a=b; c=d".into())])
        );
        assert_eq!(round_trip(&headers), headers);
        // 跟 cstring 一樣：NUL 之後的不送（字串、鍵都是）
        assert_eq!(round_trip(&Node::Str("ab\0cd".into())), Node::Str("ab".into()));
        assert_eq!(
            round_trip(&Node::Map(vec![("k\0x".into(), Node::Int(1))])),
            Node::Map(vec![("k".into(), Node::Int(1))])
        );
    }

    #[test]
    fn node_tree_owns_every_allocation() {
        let node = Node::Map(vec![
            ("a".into(), Node::strings(["1", "2", "3"])),
            ("b".into(), Node::Array(Vec::new())),
            ("c".into(), Node::Map(vec![("d".into(), Node::Str("e".into()))])),
        ]);
        let tree = NodeTree::new(&node).unwrap();
        // 字串：鍵 a b c d + 值 1 2 3 e；清單：外層、a、b、c；空的 b 沒有 values / keys
        assert_eq!(tree.strings.len(), 8);
        assert_eq!(tree.lists.len(), 4);
        assert_eq!(tree.values.len(), 3);
        assert_eq!(tree.keys.len(), 2);
    }
}
