//! libmpv 的安全包裝：只包播放器用得到的部分。
//!
//! 刻意不用 `libmpv2` crate：它對 Node 型別屬性會 `unimplemented!()`，
//! `get_info` 也會 panic。底層綁定用 `libmpv2-sys`，這一層自己掌控。

pub mod render;

use libmpv2_sys as sys;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::ptr::NonNull;

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
    Other(u32),
}

type Callback = Box<dyn Fn() + Send + Sync + 'static>;

/// 一個 mpv 播放核心。mpv 的 client API 是執行緒安全的，所以可以 Send + Sync。
pub struct Mpv {
    handle: NonNull<sys::mpv_handle>,
    wakeup: Option<Box<Callback>>,
}

// SAFETY: libmpv 的 client API 可以從任何執行緒呼叫（client.h「Thread safety」一節）
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    /// 建立並初始化。`options` 會在 `mpv_initialize` 之前設定，
    /// 只能在初始化前設定的選項（例如 `vo`、`config`）要放這裡。
    pub fn new(options: &[(&str, &str)]) -> Result<Self> {
        let api = unsafe { sys::mpv_client_api_version() } as u64;
        if api >> 16 != 2 {
            return Err(Error::new(
                sys::mpv_error_MPV_ERROR_UNSUPPORTED,
                format!("libmpv client API {}.{} 不相容（需要 2.x）", api >> 16, api & 0xffff),
            ));
        }

        let raw = unsafe { sys::mpv_create() };
        let handle = NonNull::new(raw).ok_or_else(|| Error::new(sys::mpv_error_MPV_ERROR_NOMEM, "mpv_create"))?;
        // 先包起來：之後任何一步失敗，Drop 都會正確釋放
        let mpv = Mpv { handle, wakeup: None };

        for (name, value) in options {
            let (n, v) = (cstring(name), cstring(value));
            let code = unsafe { sys::mpv_set_option_string(mpv.raw(), n.as_ptr(), v.as_ptr()) };
            // 不同建置的 libmpv 選項不完全相同（例如沒編 Lua 就沒有 osc、ytdl），
            // 沒有的選項略過即可，不影響播放
            if code == sys::mpv_error_MPV_ERROR_OPTION_NOT_FOUND {
                eprintln!("[vitascope] 這個 libmpv 沒有選項 {name}，略過");
                continue;
            }
            check(code, || format!("設定選項 {name}={value}"))?;
        }
        check(unsafe { sys::mpv_initialize(mpv.raw()) }, || "mpv_initialize".into())?;
        Ok(mpv)
    }

    pub(crate) fn raw(&self) -> *mut sys::mpv_handle {
        self.handle.as_ptr()
    }

    /// 執行指令，例如 `["loadfile", path]`、`["seek", "10", "relative"]`。
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let code = unsafe { sys::mpv_command(self.raw(), ptrs.as_mut_ptr()) };
        check(code, || format!("指令 {}", args.join(" ")))
    }

    /// 非同步指令，結果以 `Event::CommandReply { id }` 回報。
    pub fn command_async(&self, id: u64, args: &[&str]) -> Result<()> {
        let owned: Vec<CString> = args.iter().map(|a| cstring(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let code = unsafe { sys::mpv_command_async(self.raw(), id, ptrs.as_mut_ptr()) };
        check(code, || format!("非同步指令 {}", args.join(" ")))
    }

    pub fn set_property<T: PropertyValue>(&self, name: &str, value: T) -> Result<()> {
        let n = cstring(name);
        let code =
            value.with_raw(|format, data| unsafe { sys::mpv_set_property(self.raw(), n.as_ptr(), format, data) });
        check(code, || format!("設定屬性 {name}"))
    }

    pub fn get_property<T: PropertyValue>(&self, name: &str) -> Result<T> {
        let n = cstring(name);
        T::read(|format, data| unsafe { sys::mpv_get_property(self.raw(), n.as_ptr(), format, data) })
            .map_err(|code| Error::new(code, format!("讀取屬性 {name}")))
    }

    /// 以字串讀取屬性。Node 型別（`track-list`、`metadata`…）會拿到 JSON。
    pub fn get_string(&self, name: &str) -> Result<String> {
        self.get_property::<String>(name)
    }

    pub fn observe(&self, id: u64, name: &str, format: Format) -> Result<()> {
        let n = cstring(name);
        let code = unsafe { sys::mpv_observe_property(self.raw(), id, n.as_ptr(), format.raw()) };
        check(code, || format!("觀察屬性 {name}"))
    }

    /// 要求 mpv 把 `level` 以上的記錄訊息以 `Event::Log` 送來（"error"、"warn"、"info"…）。
    pub fn request_log_messages(&self, level: &str) -> Result<()> {
        let l = cstring(level);
        check(unsafe { sys::mpv_request_log_messages(self.raw(), l.as_ptr()) }, || {
            format!("request_log_messages {level}")
        })
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
                let error = (e.error < 0).then(|| Error::new(e.error, "播放失敗"));
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
                result: check(ev.error, || "非同步指令".into()),
            },
            sys::mpv_event_id_MPV_EVENT_QUEUE_OVERFLOW => Event::QueueOverflow,
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
