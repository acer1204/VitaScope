//! macOS：`/usr/bin/pmset sleepnow` 睡眠、請 loginwindow 關機（`/usr/bin/osascript`）、
//! `CGEventSourceSecondsSinceLastEventType` 查整個系統多久沒有輸入。macOS 沒有給程式用的休眠，不列出。
//!
//! 關機要「自動化」權限（控制 loginwindow）：使用者選「關機」時先問（[`prepare`]），系統的詢問視窗在人還在的時候出現；
//! 等倒數完才問的話沒有人回答，osascript 等到逾時、什麼都沒發生。

use super::{Avail, OSASCRIPT_ARGS, PowerAction, PowerCaps, PowerError};
use super::{automation_permission, osascript_result};
use std::ffi::c_void;
use std::time::Duration;

const PMSET: &str = "/usr/bin/pmset";
const OSASCRIPT: &str = "/usr/bin/osascript";

pub struct Power;

impl Power {
    pub fn new() -> Self {
        Self
    }

    /// 整個系統最後一次鍵盤、滑鼠（含觸控板）輸入到現在多久（不需要特別的權限）
    pub fn last_input_age(&self) -> Option<Duration> {
        // SAFETY: 單純的查詢
        let secs = unsafe { CGEventSourceSecondsSinceLastEventType(HID_SYSTEM_STATE, ANY_INPUT_EVENT) };
        (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
    }
}

pub fn caps() -> PowerCaps {
    PowerCaps {
        sleep: Avail::Yes,
        hibernate: Avail::No(PowerError::NotSupported),
        // 「自動化」權限在選的時候問（prepare），被拒的話那時說明
        shutdown: Avail::Yes,
    }
}

pub fn prepare(action: PowerAction) -> Result<(), PowerError> {
    match action {
        PowerAction::Shutdown => automation_permission(ask_automation()),
        PowerAction::Sleep | PowerAction::Hibernate => Ok(()),
    }
}

pub fn run(action: PowerAction) -> Result<(), PowerError> {
    match action {
        PowerAction::Sleep => {
            let out = crate::syscmd::command(PMSET)
                .arg("sleepnow")
                .output()
                .map_err(|e| PowerError::Failed(e.to_string()))?;
            if out.status.success() {
                Ok(())
            } else {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                Err(PowerError::Failed(if err.is_empty() { "pmset".into() } else { err }))
            }
        }
        PowerAction::Hibernate => Err(PowerError::NotSupported),
        PowerAction::Shutdown => {
            let out = crate::syscmd::command(OSASCRIPT)
                .args(OSASCRIPT_ARGS)
                .output()
                .map_err(|e| PowerError::Failed(e.to_string()))?;
            osascript_result(out.status.success(), &String::from_utf8_lossy(&out.stderr))
        }
    }
}

/// 問系統能不能送關機的 Apple Event 給 loginwindow；還沒決定時跳出系統的詢問視窗，等使用者回答。
/// 回傳 OSStatus（0 = 可以，-1743 = 被拒）
fn ask_automation() -> i32 {
    const BUNDLE_ID: &[u8] = b"com.apple.loginwindow";
    let mut target = AEDesc {
        descriptor_type: 0,
        data_handle: std::ptr::null_mut(),
    };
    // SAFETY: AECreateDesc 複製 bundle id 的位元組；建好的描述用完用 AEDisposeDesc 釋放
    unsafe {
        let err = AECreateDesc(
            TYPE_APPLICATION_BUNDLE_ID,
            BUNDLE_ID.as_ptr().cast(),
            BUNDLE_ID.len() as isize,
            &mut target,
        );
        if err != 0 {
            return i32::from(err);
        }
        let status = AEDeterminePermissionToAutomateTarget(&target, CORE_EVENT_CLASS, AE_SHUT_DOWN, 1);
        AEDisposeDesc(&mut target);
        status
    }
}

/// Apple Event 的四字元代碼
const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const TYPE_APPLICATION_BUNDLE_ID: u32 = four_cc(b"bund");
const CORE_EVENT_CLASS: u32 = four_cc(b"aevt");
const AE_SHUT_DOWN: u32 = four_cc(b"shut");

/// kCGEventSourceStateHIDSystemState：硬體送進來的輸入（所有程式）
const HID_SYSTEM_STATE: i32 = 1;
/// kCGAnyInputEventType
const ANY_INPUT_EVENT: u32 = !0;

/// AEDesc（AEDataModel.h）：描述的型別與資料（不透明的 handle）。
/// 標頭檔用 `#pragma pack(push, 2)` 包住：`data_handle` 在第 4 個位元組、整個 12 位元組（不是 C 預設的 8、16）
#[repr(C, packed(2))]
struct AEDesc {
    descriptor_type: u32,
    data_handle: *mut c_void,
}

const _: () = assert!(std::mem::size_of::<AEDesc>() == 12 && std::mem::align_of::<AEDesc>() == 2);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn AECreateDesc(type_code: u32, data_ptr: *const c_void, data_size: isize, result: *mut AEDesc) -> i16;
    fn AEDisposeDesc(desc: *mut AEDesc) -> i16;
    /// macOS 10.14 起；askUserIfNeeded = true 時可能等使用者回答很久（呼叫的是背景執行緒）
    fn AEDeterminePermissionToAutomateTarget(
        target: *const AEDesc,
        event_class: u32,
        event_id: u32,
        ask_user_if_needed: u8,
    ) -> i32;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
}
