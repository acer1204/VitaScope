//! Linux：systemd-logind（D-Bus 的系統匯流排，`zbus::blocking`）睡眠、休眠、關機；X11 的 XScreenSaver 擴充查閒置時間。
//!
//! - logind 回答 "challenge"（要輸入系統管理員密碼）的項目停用：人不在時沒有人輸入密碼。
//! - 執行時 `interactive = false`：萬一系統改成要密碼，馬上失敗、說明原因，不會跳出一個沒有人回答、一直等下去的密碼視窗。
//! - Wayland 沒有整個系統的閒置時間（XWayland 的只算 X11 程式），查不到：只看影戲視窗裡的動作。

use super::{Avail, PowerAction, PowerCaps, PowerError, logind_avail, logind_error};
use std::sync::Mutex;
use std::time::Duration;
use x11_dl::xlib::{self, Xlib};
use x11_dl::xss::{XScreenSaverInfo, Xss};

const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const IFACE: &str = "org.freedesktop.login1.Manager";
/// logind 正常時馬上回答；卡住的系統匯流排不要讓背景執行緒一直等
const CALL_TIMEOUT: Duration = Duration::from_secs(25);

pub struct Power {
    /// Wayland 的工作階段：查不到整個系統的閒置時間
    wayland: bool,
    idle: Mutex<IdleState>,
}

enum IdleState {
    /// 還沒試著連 X
    Untried,
    /// 查不到（沒有 X、沒有 libXss、X 沒有 MIT-SCREEN-SAVER 擴充）：不再試
    Unavailable,
    Open(Box<XIdle>),
}

impl Power {
    pub fn new() -> Self {
        // 跟 winit 一樣：有 WAYLAND_DISPLAY 時用 Wayland
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
        Self {
            wayland,
            idle: Mutex::new(IdleState::Untried),
        }
    }

    /// 整個 X 桌面最後一次鍵盤、滑鼠輸入到現在多久（倒數期間每 0.25 秒在介面的執行緒上問一次：很快）
    pub fn last_input_age(&self) -> Option<Duration> {
        if self.wayland {
            return None;
        }
        let mut state = self.idle.lock().ok()?;
        if matches!(*state, IdleState::Untried) {
            *state = XIdle::open().map_or(IdleState::Unavailable, |x| IdleState::Open(Box::new(x)));
        }
        match &*state {
            IdleState::Open(x) => x.query(),
            _ => None,
        }
    }
}

fn connect() -> Result<zbus::blocking::Connection, PowerError> {
    // 連不上系統匯流排（容器、WSL）：跟沒有 logind 一樣
    zbus::blocking::connection::Builder::system()
        .and_then(|b| b.method_timeout(CALL_TIMEOUT).build())
        .map_err(|_| PowerError::NoLogind)
}

fn call<R>(conn: &zbus::blocking::Connection, method: &str, body: &R) -> Result<zbus::Message, PowerError>
where
    R: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(Some(DEST), PATH, Some(IFACE), method, body)
        .map_err(|e| match e {
            zbus::Error::MethodError(name, detail, _) => logind_error(name.as_str(), detail.as_deref()),
            zbus::Error::FDO(e) => match *e {
                zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_) => PowerError::NoLogind,
                zbus::fdo::Error::AccessDenied(_) => PowerError::NoPermission,
                zbus::fdo::Error::InteractiveAuthorizationRequired(_) => PowerError::NeedsPassword,
                e => PowerError::Failed(e.to_string()),
            },
            e => PowerError::Failed(e.to_string()),
        })
}

/// 問 logind 一項（"yes" / "no" / "challenge" / "na"）
fn can(conn: &zbus::blocking::Connection, method: &str) -> Avail {
    let answer = call(conn, method, &()).and_then(|reply| {
        reply
            .body()
            .deserialize::<String>()
            .map_err(|e| PowerError::Failed(e.to_string()))
    });
    match answer {
        Ok(answer) => logind_avail(&answer),
        Err(e) => Avail::No(e),
    }
}

pub fn caps() -> PowerCaps {
    let conn = match connect() {
        Ok(conn) => conn,
        Err(e) => {
            return PowerCaps {
                sleep: Avail::No(e.clone()),
                hibernate: Avail::No(e.clone()),
                shutdown: Avail::No(e),
            };
        }
    };
    PowerCaps {
        sleep: can(&conn, "CanSuspend"),
        hibernate: can(&conn, "CanHibernate"),
        shutdown: can(&conn, "CanPowerOff"),
    }
}

pub fn prepare(_action: PowerAction) -> Result<(), PowerError> {
    Ok(())
}

pub fn run(action: PowerAction) -> Result<(), PowerError> {
    let conn = connect()?;
    let method = match action {
        PowerAction::Sleep => "Suspend",
        PowerAction::Hibernate => "Hibernate",
        PowerAction::Shutdown => "PowerOff",
    };
    // interactive = false：見模組說明
    call(&conn, method, &(false,)).map(|_| ())
}

/// 自己的 X 連線（不跟 winit 共用）與 XScreenSaver 的查詢結構
struct XIdle {
    xlib: Xlib,
    xss: Xss,
    display: *mut xlib::Display,
    info: *mut XScreenSaverInfo,
}

// SAFETY: 只在 Mutex 裡用（一次一個執行緒），連線是自己開的
unsafe impl Send for XIdle {}

impl XIdle {
    fn open() -> Option<Self> {
        // 執行時才載入：沒有 X、沒有 libXss（有些精簡的系統沒裝）就是查不到
        let xlib = Xlib::open().ok()?;
        let xss = Xss::open().ok()?;
        // SAFETY: 開一條新連線，失敗或用不了時關掉；Drop 也會關
        unsafe {
            let display = (xlib.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return None;
            }
            // 先確認有這個擴充：沒有的話呼叫它的函式會產生 X 錯誤，Xlib 預設的處理是直接結束程式
            let (mut event_base, mut error_base) = (0, 0);
            if (xss.XScreenSaverQueryExtension)(display, &mut event_base, &mut error_base) == 0 {
                (xlib.XCloseDisplay)(display);
                return None;
            }
            let info = (xss.XScreenSaverAllocInfo)();
            if info.is_null() {
                (xlib.XCloseDisplay)(display);
                return None;
            }
            Some(Self {
                xlib,
                xss,
                display,
                info,
            })
        }
    }

    // c_ulong 在 64 位元 Linux 是 u64、32 位元是 u32：轉型在 64 位元上看起來多餘
    #[allow(clippy::unnecessary_cast)]
    fn query(&self) -> Option<Duration> {
        // SAFETY: open() 確認過擴充、配好 info；根視窗一定存在
        unsafe {
            let root = (self.xlib.XDefaultRootWindow)(self.display);
            if (self.xss.XScreenSaverQueryInfo)(self.display, root, self.info) == 0 {
                return None;
            }
            // idle 是毫秒（c_ulong）
            Some(Duration::from_millis((*self.info).idle as u64))
        }
    }
}

impl Drop for XIdle {
    fn drop(&mut self) {
        // SAFETY: open() 配的 info 與開的連線，各釋放一次
        unsafe {
            (self.xlib.XFree)(self.info.cast());
            (self.xlib.XCloseDisplay)(self.display);
        }
    }
}
