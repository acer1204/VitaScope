//! Windows：`SetSuspendState` 睡眠 / 休眠、`ExitWindowsEx` 關機、`GetLastInputInfo` 查整個系統多久沒有輸入。
//!
//! - 睡眠、休眠、關機都要先打開 SeShutdownPrivilege（一般使用者有這個權限，只是預設關著）。
//! - 關機不加 `EWX_FORCE`：有沒存的資料的程式還是可以擋下來（Windows 會顯示「這個 App 正在阻止關機」）；
//!   加 `EWX_FORCEIFHUNG`：沒有回應的程式不會讓關機永遠卡住。

use super::{Avail, windows_sleep_avail, windows_tick_age};
use super::{PowerAction, PowerCaps, PowerError, os_error, windows_hibernate_avail, windows_privilege_result};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::System::Power::{
    GetPwrCapabilities, IsPwrHibernateAllowed, IsPwrSuspendAllowed, SYSTEM_POWER_CAPABILITIES, SetSuspendState,
};
use windows_sys::Win32::System::Shutdown::{
    EWX_FORCEIFHUNG, EWX_POWEROFF, ExitWindowsEx, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION,
    SHTDN_REASON_MINOR_OTHER,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};

/// 關機的方式：關掉電源；沒有回應的程式不會讓關機永遠卡住。**不加 `EWX_FORCE`**：有沒存的資料的程式還是可以擋下來
pub(super) const SHUTDOWN_FLAGS: u32 = EWX_POWEROFF | EWX_FORCEIFHUNG;
/// 關機的原因（事件記錄裡看得到）：程式要求、預先計畫的
pub(super) const SHUTDOWN_REASON_CODE: u32 =
    SHTDN_REASON_MAJOR_APPLICATION | SHTDN_REASON_MINOR_OTHER | SHTDN_REASON_FLAG_PLANNED;

pub struct Power;

impl Power {
    pub fn new() -> Self {
        Self
    }

    /// 整個工作階段（所有程式）最後一次鍵盤、滑鼠輸入到現在多久
    pub fn last_input_age(&self) -> Option<Duration> {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        // SAFETY: cbSize 已經設好的結構；先問最後的輸入、再拿現在的時間（反過來的話中間的輸入會比「現在」還晚）
        unsafe {
            if GetLastInputInfo(&mut info) == 0 {
                return None;
            }
            Some(windows_tick_age(GetTickCount(), info.dwTime))
        }
    }
}

pub fn caps() -> PowerCaps {
    // SAFETY: 單純的查詢；結構由函式填好
    let (aoac, suspend, hibernate) = unsafe {
        let mut pc: SYSTEM_POWER_CAPABILITIES = std::mem::zeroed();
        let aoac = GetPwrCapabilities(&mut pc) && pc.AoAc;
        (aoac, IsPwrSuspendAllowed(), IsPwrHibernateAllowed())
    };
    PowerCaps {
        sleep: windows_sleep_avail(aoac, suspend),
        hibernate: windows_hibernate_avail(hibernate),
        // 權限在執行時才打開（沒有的話那時說明）
        shutdown: Avail::Yes,
    }
}

pub fn prepare(_action: PowerAction) -> Result<(), PowerError> {
    Ok(())
}

pub fn run(action: PowerAction) -> Result<(), PowerError> {
    enable_shutdown_privilege()?;
    // SAFETY: 參數都是常數；失敗時馬上讀 GetLastError
    unsafe {
        let ok = match action {
            // 睡眠醒來後才回傳（呼叫的是背景執行緒）
            PowerAction::Sleep => SetSuspendState(false, false, false),
            PowerAction::Hibernate => SetSuspendState(true, false, false),
            // 馬上回傳，關機在背景進行（程式會收到 WM_QUERYENDSESSION、WM_ENDSESSION）
            PowerAction::Shutdown => ExitWindowsEx(SHUTDOWN_FLAGS, SHUTDOWN_REASON_CODE) != 0,
        };
        if ok {
            Ok(())
        } else {
            Err(PowerError::Failed(os_error(GetLastError())))
        }
    }
}

/// 打開這個程式的 SeShutdownPrivilege（`SetSuspendState`、`ExitWindowsEx` 都要）
fn enable_shutdown_privilege() -> Result<(), PowerError> {
    // SAFETY: 開自己的 token，用完關掉；LookupPrivilegeValueW 的名稱是系統定義的常數
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0 {
            return Err(PowerError::Failed(os_error(GetLastError())));
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(std::ptr::null(), SE_SHUTDOWN_NAME, &mut luid) == 0 {
            let e = GetLastError();
            CloseHandle(token);
            return Err(PowerError::Failed(os_error(e)));
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let adjusted = AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut()) != 0;
        // 成功時也要看：帳號沒有這個權限時回傳成功、錯誤碼是 ERROR_NOT_ALL_ASSIGNED
        let e = GetLastError();
        CloseHandle(token);
        windows_privilege_result(adjusted, e)
    }
}
