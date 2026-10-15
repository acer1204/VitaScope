//! 播完後的動作（右鍵選單「播完後 ▸」）：不做任何事、關閉影戲、睡眠、休眠、關機。
//!
//! - 只對這次有效：[`AfterPlayback`] 刻意不能存檔（沒有 serde），每次啟動都是「不做任何事」，
//!   執行、失敗或取消之後也回到「不做任何事」。
//! - 一定先倒數（[`Countdown`]）：關閉影戲 10 秒，睡眠 / 休眠 30 秒，關機 60 秒；倒數時任何動作都會取消。
//! - 真的讓電腦睡眠、關機的是注入的 [`SystemPower`]（`Launch.system`）：自動測試、`--shot` 沒有，
//!   選單也就不列出睡眠、休眠、關機，任何測試都不可能讓電腦睡眠或關機。
//! - 背景執行緒（查系統能不能睡眠、真的執行）只回傳 [`PowerError`]，文字由介面的執行緒產生（跟著目前的語言）。
//! - 各平台真正的做法是 [`RealPower`]（只有 main.rs 建立）：
//!   - Windows：`SetSuspendState` 睡眠 / 休眠、`ExitWindowsEx` 關機（先打開 SeShutdownPrivilege；不強制關閉程式），
//!     `GetLastInputInfo` 查整個系統多久沒有輸入；
//!   - macOS：`/usr/bin/pmset sleepnow` 睡眠、`/usr/bin/osascript` 請 loginwindow 關機（選「關機」時先問系統的
//!     「自動化」權限，詢問視窗才會在人還在的時候出現），`CGEventSourceSecondsSinceLastEventType` 查閒置；
//!   - Linux：systemd-logind（D-Bus 系統匯流排）；要密碼（polkit 的 challenge）的停用：人不在時不會有人輸入密碼。
//!     閒置時間用 X11 的 XScreenSaver 擴充（沒有 libXss 時查不到）；Wayland 沒有整個系統的閒置時間，只看影戲視窗裡的動作。
//! - 判斷規則（logind 的回答、權限錯誤、osascript 的參數與錯誤…）寫成純函式，每個平台都編譯、都測。

use std::time::{Duration, Instant};

/// 播完後要做什麼。**不存檔**（沒有 serde）：只對這次執行有效
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AfterPlayback {
    #[default]
    Nothing,
    /// 關閉影戲
    Close,
    Sleep,
    Hibernate,
    Shutdown,
}

impl AfterPlayback {
    /// 選單裡的順序
    pub const ALL: [AfterPlayback; 5] = [
        AfterPlayback::Nothing,
        AfterPlayback::Close,
        AfterPlayback::Sleep,
        AfterPlayback::Hibernate,
        AfterPlayback::Shutdown,
    ];

    /// 要不要系統幫忙（睡眠、休眠、關機）
    pub fn power(self) -> Option<PowerAction> {
        match self {
            AfterPlayback::Nothing | AfterPlayback::Close => None,
            AfterPlayback::Sleep => Some(PowerAction::Sleep),
            AfterPlayback::Hibernate => Some(PowerAction::Hibernate),
            AfterPlayback::Shutdown => Some(PowerAction::Shutdown),
        }
    }

    /// 選單、設定頁的選項（英文開頭大寫）
    pub fn label(self) -> &'static str {
        match self {
            AfterPlayback::Nothing => crate::tr!("不做任何事", "Do nothing"),
            AfterPlayback::Close => crate::tr!("關閉影戲", "Close VitaScope"),
            AfterPlayback::Sleep => crate::tr!("睡眠", "Sleep"),
            AfterPlayback::Hibernate => crate::tr!("休眠", "Hibernate"),
            AfterPlayback::Shutdown => crate::tr!("關機", "Shut down"),
        }
    }

    /// 放在句子裡的說法（「播完後：關閉影戲」"When playback ends: close VitaScope"）
    pub fn noun(self) -> &'static str {
        match self {
            AfterPlayback::Nothing => crate::tr!("不做任何事", "do nothing"),
            AfterPlayback::Close => crate::tr!("關閉影戲", "close VitaScope"),
            AfterPlayback::Sleep => crate::tr!("睡眠", "sleep"),
            AfterPlayback::Hibernate => crate::tr!("休眠", "hibernate"),
            AfterPlayback::Shutdown => crate::tr!("關機", "shut down"),
        }
    }

    /// 控制列的 ⏳（選了動作時一直顯示，點一下取消）
    pub fn indicator(self) -> &'static str {
        match self {
            AfterPlayback::Nothing => "",
            AfterPlayback::Close => crate::tr!("⏳ 播完後關閉影戲", "⏳ Close VitaScope when done"),
            AfterPlayback::Sleep => crate::tr!("⏳ 播完後睡眠", "⏳ Sleep when done"),
            AfterPlayback::Hibernate => crate::tr!("⏳ 播完後休眠", "⏳ Hibernate when done"),
            AfterPlayback::Shutdown => crate::tr!("⏳ 播完後關機", "⏳ Shut down when done"),
        }
    }

    /// 倒數畫面的大字
    pub fn countdown_line(self, secs: u64) -> String {
        match self {
            AfterPlayback::Nothing => String::new(),
            AfterPlayback::Close => crate::tf!("{secs} 秒後關閉影戲", "Closing VitaScope in {secs} s"),
            AfterPlayback::Sleep => crate::tf!("{secs} 秒後進入睡眠", "Sleeping in {secs} s"),
            AfterPlayback::Hibernate => crate::tf!("{secs} 秒後休眠", "Hibernating in {secs} s"),
            AfterPlayback::Shutdown => crate::tf!("{secs} 秒後關機", "Shutting down in {secs} s"),
        }
    }

    /// 播完時還在匯出：等匯出做完才開始倒數
    pub fn after_export(self) -> String {
        match self {
            AfterPlayback::Nothing => String::new(),
            AfterPlayback::Close => {
                crate::tr!("匯出完成後再關閉影戲", "Will close VitaScope after the export finishes").into()
            }
            AfterPlayback::Sleep => crate::tr!("匯出完成後再進入睡眠", "Will sleep after the export finishes").into(),
            AfterPlayback::Hibernate => {
                crate::tr!("匯出完成後再休眠", "Will hibernate after the export finishes").into()
            }
            AfterPlayback::Shutdown => {
                crate::tr!("匯出完成後再關機", "Will shut down after the export finishes").into()
            }
        }
    }

    /// 倒數被取消（按了鍵、動了滑鼠…）
    pub fn cancelled(self) -> String {
        crate::tf!("已取消：播完後{}", "Cancelled: {} when playback ends", self.noun())
    }
}

/// 要系統做的事
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Sleep,
    Hibernate,
    Shutdown,
}

impl PowerAction {
    /// 做不到時的提示
    pub fn failed(self, e: &PowerError) -> String {
        let e = e.message();
        match self {
            PowerAction::Sleep => crate::tf!("無法進入睡眠：{e}", "Couldn't sleep: {e}"),
            PowerAction::Hibernate => crate::tf!("無法休眠：{e}", "Couldn't hibernate: {e}"),
            PowerAction::Shutdown => crate::tf!("無法關機：{e}", "Couldn't shut down: {e}"),
        }
    }
}

/// 不能睡眠、關機的原因（背景執行緒回傳這個，文字在介面的執行緒產生）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerError {
    /// 這台電腦（或這個系統）不支援
    NotSupported,
    /// 沒有權限
    NoPermission,
    /// Windows：新式待命（Modern Standby）的電腦，程式要它睡眠不可靠
    ModernStandby,
    /// Windows：休眠沒有開啟
    HibernateOff,
    /// Windows：沒有關機的權限（SeShutdownPrivilege）
    NoShutdownPrivilege,
    /// Linux：沒有 systemd-logind（或連不上系統匯流排）
    NoLogind,
    /// Linux：要系統管理員的密碼（polkit 的 challenge）。人不在時沒有人輸入，所以不執行
    NeedsPassword,
    /// Linux：logind 不允許（它的回答，例如 "no"）
    LogindRefuses(String),
    /// macOS：系統不讓影戲控制 loginwindow（「隱私權與安全性 → 自動化」，錯誤 -1743）
    MacAutomationDenied,
    /// 系統回報的錯誤（原文，不翻譯）
    Failed(String),
}

impl PowerError {
    pub fn message(&self) -> String {
        match self {
            PowerError::NotSupported => crate::tr!("這台電腦不支援", "Not supported on this computer").into(),
            PowerError::NoPermission => crate::tr!("沒有權限", "Permission denied").into(),
            PowerError::ModernStandby => crate::tr!(
                "這台電腦使用新式待命（Modern Standby），不能由程式進入睡眠",
                "This computer uses Modern Standby, so programs can't put it to sleep"
            )
            .into(),
            PowerError::HibernateOff => crate::tr!(
                "休眠沒有開啟（系統管理員可以執行 powercfg /hibernate on）",
                "Hibernation is turned off (an administrator can run powercfg /hibernate on)"
            )
            .into(),
            PowerError::NoShutdownPrivilege => {
                crate::tr!("沒有關機的權限", "No permission to shut down this computer").into()
            }
            PowerError::NoLogind => crate::tr!(
                "找不到 systemd-logind，無法由程式睡眠或關機",
                "systemd-logind wasn't found, so programs can't sleep or shut down this computer"
            )
            .into(),
            PowerError::NeedsPassword => crate::tr!(
                "需要系統管理員密碼：離開電腦時不會執行",
                "Needs an administrator password, so it can't run while you're away"
            )
            .into(),
            PowerError::LogindRefuses(answer) => {
                crate::tf!("系統不允許（logind：{answer}）", "Not allowed by the system (logind: {answer})")
            }
            PowerError::MacAutomationDenied => crate::tr!(
                "macOS 不允許影戲控制 loginwindow：請到「系統設定 → 隱私權與安全性 → 自動化」允許",
                "macOS doesn't allow VitaScope to control loginwindow: allow it in System Settings → Privacy & Security → Automation"
            )
            .into(),
            PowerError::Failed(detail) => detail.clone(),
        }
    }
}

/// 能不能做
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Avail {
    Yes,
    /// 還在問系統（選單裡停用，提示「檢查中…」）
    #[default]
    Unknown,
    No(PowerError),
}

/// 系統能做哪些事（背景執行緒問一次）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PowerCaps {
    pub sleep: Avail,
    pub hibernate: Avail,
    pub shutdown: Avail,
}

impl PowerCaps {
    /// 每一項都可以（測試用的假系統、macOS 以外的一般情況）
    pub fn all_yes() -> Self {
        Self {
            sleep: Avail::Yes,
            hibernate: Avail::Yes,
            shutdown: Avail::Yes,
        }
    }

    pub fn of(&self, action: PowerAction) -> &Avail {
        match action {
            PowerAction::Sleep => &self.sleep,
            PowerAction::Hibernate => &self.hibernate,
            PowerAction::Shutdown => &self.shutdown,
        }
    }
}

/// 讓電腦睡眠、休眠、關機的方法（`Launch.system`）。自動測試只用假的；真的（[`RealPower`]）只由 main.rs 建立
pub trait SystemPower: Send + Sync {
    /// 能做哪些事。可能很慢（Linux 要問 logind）：在背景執行緒呼叫
    fn caps(&self) -> PowerCaps;
    /// 使用者選了這個動作（人在電腦前面）：先取得之後執行時要的權限。macOS 的關機在這裡跳出系統的詢問視窗，
    /// 不然要等倒數完、人不在了才問。可能等很久（等使用者回答）：在背景執行緒呼叫。預設不用做什麼
    fn prepare(&self, action: PowerAction) -> Result<(), PowerError> {
        let _ = action;
        Ok(())
    }
    /// 真的睡眠 / 休眠 / 關機。可能要等很久（Windows 的睡眠醒來後才回傳）：在背景執行緒呼叫
    fn run(&self, action: PowerAction) -> Result<(), PowerError>;
    /// 整個系統（不只影戲的視窗）多久沒有鍵盤、滑鼠的動作。None = 查不到（例如 Wayland）。
    /// 倒數期間別的程式裡有動作（使用者在電腦前面）就取消；很快，在介面的執行緒上呼叫
    fn last_input_age(&self) -> Option<Duration>;
}

/// 真的讓電腦睡眠、休眠、關機（各平台的做法見模組說明）。**只有 main.rs 建立**（不是 `--shot` 時）：
/// 自動測試、介面測試一律用假的，任何測試都不會呼叫到這裡的 [`SystemPower::run`]
pub struct RealPower {
    inner: platform::Power,
}

impl RealPower {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            inner: platform::Power::new(),
        }
    }
}

impl SystemPower for RealPower {
    fn caps(&self) -> PowerCaps {
        platform::caps()
    }

    fn prepare(&self, action: PowerAction) -> Result<(), PowerError> {
        platform::prepare(action)
    }

    fn run(&self, action: PowerAction) -> Result<(), PowerError> {
        // 單元測試絕不讓這台電腦睡眠或關機（介面測試、整合測試本來就只有假的）
        if cfg!(test) {
            panic!("自動測試不可以呼叫真的 {action:?}");
        }
        platform::run(action)
    }

    fn last_input_age(&self) -> Option<Duration> {
        self.inner.last_input_age()
    }
}

#[cfg(windows)]
#[path = "system_power/windows.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "system_power/macos.rs"]
mod platform;
#[cfg(all(unix, not(target_os = "macos")))]
#[path = "system_power/linux.rs"]
mod platform;

// ───────────── 判斷規則（純函式：每個平台都編譯、都測） ─────────────

/// Windows：睡眠能不能做（`aoac` = 新式待命，`allowed` = `IsPwrSuspendAllowed`）。
/// 新式待命的電腦 `SetSuspendState` 不可靠（可能什麼都不做或只關螢幕），當成不能做
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_sleep_avail(aoac: bool, allowed: bool) -> Avail {
    match (aoac, allowed) {
        (true, _) => Avail::No(PowerError::ModernStandby),
        (false, true) => Avail::Yes,
        (false, false) => Avail::No(PowerError::NotSupported),
    }
}

/// Windows：休眠能不能做（`IsPwrHibernateAllowed`：休眠關掉、沒有休眠檔時是 false）
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_hibernate_avail(allowed: bool) -> Avail {
    if allowed {
        Avail::Yes
    } else {
        Avail::No(PowerError::HibernateOff)
    }
}

/// `ERROR_NOT_ALL_ASSIGNED`：`AdjustTokenPrivileges` 成功了，但這個帳號沒有那個權限
#[cfg_attr(not(windows), allow(dead_code))]
const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;

/// Windows：打開 SeShutdownPrivilege 的結果（`adjusted` = `AdjustTokenPrivileges` 的回傳值，
/// `last_error` = 緊接著的 `GetLastError()`）。這個函式成功時還要看 `GetLastError`：帳號沒有這個權限時也回傳成功
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_privilege_result(adjusted: bool, last_error: u32) -> Result<(), PowerError> {
    match (adjusted, last_error) {
        (true, ERROR_NOT_ALL_ASSIGNED) => Err(PowerError::NoShutdownPrivilege),
        (true, _) => Ok(()),
        (false, e) => Err(PowerError::Failed(os_error(e))),
    }
}

/// 系統錯誤碼的說明（Windows 的 `FormatMessage`，跟著系統的語言）
#[cfg_attr(not(windows), allow(dead_code))]
fn os_error(code: u32) -> String {
    std::io::Error::from_raw_os_error(code as i32).to_string()
}

/// Windows：最後一次輸入（`GetLastInputInfo` 的 `dwTime`）到現在（`GetTickCount`）多久。
/// 兩個都是開機後的毫秒數、49.7 天繞回 0：用 wrapping 減。`last` 比 `now` 還晚（同一毫秒內的競爭）當成剛剛
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_tick_age(now: u32, last: u32) -> Duration {
    let d = now.wrapping_sub(last);
    Duration::from_millis(if d > i32::MAX as u32 { 0 } else { u64::from(d) })
}

/// Linux：logind 的 `CanSuspend` / `CanHibernate` / `CanPowerOff` 的回答。
/// "challenge" = 要輸入系統管理員密碼（polkit）：人不在時不會有人輸入，停用並說明
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn logind_avail(answer: &str) -> Avail {
    match answer {
        "yes" => Avail::Yes,
        "challenge" => Avail::No(PowerError::NeedsPassword),
        // na = 這台電腦沒有（沒有 swap 不能休眠之類）
        "na" => Avail::No(PowerError::NotSupported),
        other => Avail::No(PowerError::LogindRefuses(other.to_string())),
    }
}

/// Linux：D-Bus 的錯誤回覆（錯誤名稱、說明）
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn logind_error(name: &str, detail: Option<&str>) -> PowerError {
    match name {
        // 系統匯流排上沒有 logind（WSL、容器、沒有 systemd 也沒有 elogind 的發行版）
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner"
        | "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownInterface" => PowerError::NoLogind,
        "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => PowerError::NeedsPassword,
        "org.freedesktop.DBus.Error.AccessDenied" => PowerError::NoPermission,
        _ => PowerError::Failed(match detail {
            Some(d) if !d.trim().is_empty() => d.trim().to_string(),
            _ => name.to_string(),
        }),
    }
}

/// macOS：請 loginwindow 關機的 osascript 參數（Apple QA1134 的 kAEShutDown；用絕對路徑執行，不經過 shell）。
/// 這是「關機」，不是「顯示關機對話框」；開著的程式還是可以取消（例如有沒存的文件）
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const OSASCRIPT_ARGS: [&str; 2] = ["-e", "tell application \"loginwindow\" to «event aevtshut»"];

/// macOS：「自動化」權限被拒（errAEEventNotPermitted）
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const AE_NOT_PERMITTED: i32 = -1743;

/// macOS：osascript 跑完的結果（`ok` = 結束碼 0，`stderr` = 錯誤輸出）。
/// 沒有「自動化」權限時 osascript 的錯誤輸出含 (-1743)
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn osascript_result(ok: bool, stderr: &str) -> Result<(), PowerError> {
    if ok {
        return Ok(());
    }
    if stderr.contains(&AE_NOT_PERMITTED.to_string()) {
        return Err(PowerError::MacAutomationDenied);
    }
    let stderr = stderr.trim();
    Err(PowerError::Failed(if stderr.is_empty() {
        "osascript".into()
    } else {
        stderr.to_string()
    }))
}

/// macOS：`AEDeterminePermissionToAutomateTarget` 的結果。只有明確被拒（-1743）才算不行；
/// 其他（例如 loginwindow 一時找不到）留到執行時再說，不在選的時候擋掉
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn automation_permission(status: i32) -> Result<(), PowerError> {
    if status == AE_NOT_PERMITTED {
        Err(PowerError::MacAutomationDenied)
    } else {
        Ok(())
    }
}

/// 倒數多久：關閉影戲 10 秒，睡眠 / 休眠 30 秒，關機 60 秒
pub fn countdown_len(action: AfterPlayback) -> Duration {
    Duration::from_secs(match action {
        AfterPlayback::Nothing => 0,
        AfterPlayback::Close => 10,
        AfterPlayback::Sleep | AfterPlayback::Hibernate => 30,
        AfterPlayback::Shutdown => 60,
    })
}

/// 播完後的倒數（時間用參數傳進來，測試可以用假的時間）
#[derive(Debug, Clone)]
pub struct Countdown {
    action: AfterPlayback,
    started: Instant,
    deadline: Instant,
}

/// 倒數的進度
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tick {
    /// 還剩幾秒（無條件進位：剩 0.2 秒是「1 秒」），下次要重畫的時間（數字會變的時候，或時間到）
    Wait { remaining_secs: u64, repaint_in: Duration },
    /// 時間到：做這件事（之後再問還是 Fire，呼叫的人拿到第一次就把倒數丟掉）
    Fire(AfterPlayback),
}

impl Countdown {
    pub fn new(action: AfterPlayback, len: Duration, now: Instant) -> Self {
        Self {
            action,
            started: now,
            deadline: now + len,
        }
    }

    pub fn action(&self) -> AfterPlayback {
        self.action
    }

    /// 開始倒數的時間
    pub fn started(&self) -> Instant {
        self.started
    }

    pub fn tick(&self, now: Instant) -> Tick {
        let Some(left) = self.deadline.checked_duration_since(now).filter(|d| !d.is_zero()) else {
            return Tick::Fire(self.action);
        };
        let secs = left.as_nanos().div_ceil(1_000_000_000) as u64;
        // 數字在剩下 secs - 1 秒時變（剩 1 秒時就是時間到）
        let repaint_in = left.saturating_sub(Duration::from_secs(secs - 1));
        Tick::Wait {
            remaining_secs: secs,
            repaint_in,
        }
    }

    /// 開始倒數之後系統有沒有鍵盤、滑鼠的動作（`idle` = [`SystemPower::last_input_age`]）。
    /// 跟開始的時間剛好一樣也算（寧可誤取消：取消只是電腦不關，誤關會讓別的程式沒存的資料不見）
    pub fn input_since_start(&self, idle: Duration, now: Instant) -> bool {
        now.checked_sub(idle)
            .is_some_and(|last_input| last_input >= self.started)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, set_lang};

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn countdown_counts_whole_seconds_and_fires_once_time_is_up() {
        let t0 = Instant::now();
        let c = Countdown::new(AfterPlayback::Close, 10 * S, t0);
        assert_eq!(c.action(), AfterPlayback::Close);
        assert_eq!(c.started(), t0);
        assert_eq!(
            c.tick(t0),
            Tick::Wait {
                remaining_secs: 10,
                repaint_in: S
            }
        );
        // 剩 9.7 秒顯示 10，0.7 秒後變 9
        assert_eq!(
            c.tick(t0 + Duration::from_millis(300)),
            Tick::Wait {
                remaining_secs: 10,
                repaint_in: Duration::from_millis(700)
            }
        );
        assert_eq!(
            c.tick(t0 + S),
            Tick::Wait {
                remaining_secs: 9,
                repaint_in: S
            }
        );
        // 最後一秒：下次重畫就是時間到
        assert_eq!(
            c.tick(t0 + Duration::from_millis(9_800)),
            Tick::Wait {
                remaining_secs: 1,
                repaint_in: Duration::from_millis(200)
            }
        );
        assert_eq!(c.tick(t0 + 10 * S), Tick::Fire(AfterPlayback::Close));
        assert_eq!(c.tick(t0 + 11 * S), Tick::Fire(AfterPlayback::Close));
    }

    #[test]
    fn countdown_lengths_per_action() {
        assert_eq!(countdown_len(AfterPlayback::Close), 10 * S);
        assert_eq!(countdown_len(AfterPlayback::Sleep), 30 * S);
        assert_eq!(countdown_len(AfterPlayback::Hibernate), 30 * S);
        assert_eq!(countdown_len(AfterPlayback::Shutdown), 60 * S);
        // 很短的倒數（測試用）也從 1 秒開始顯示
        let t0 = Instant::now();
        let c = Countdown::new(AfterPlayback::Shutdown, Duration::from_millis(300), t0);
        assert!(matches!(c.tick(t0), Tick::Wait { remaining_secs: 1, .. }));
    }

    #[test]
    fn system_input_after_the_start_counts() {
        let t0 = Instant::now();
        let c = Countdown::new(AfterPlayback::Shutdown, 60 * S, t0);
        let now = t0 + 5 * S;
        // 最後一次輸入是 10 秒前（倒數之前）：不算
        assert!(!c.input_since_start(10 * S, now));
        // 2 秒前（倒數開始之後）、剛好在開始的時候：算
        assert!(c.input_since_start(2 * S, now));
        assert!(c.input_since_start(5 * S, now));
        // 系統很久沒有輸入（比 Instant 能表示的還久）：不算，也不會溢位
        assert!(!c.input_since_start(Duration::from_secs(u64::MAX / 4), now));
    }

    #[test]
    fn only_sleep_hibernate_and_shutdown_need_the_system() {
        assert_eq!(AfterPlayback::Nothing.power(), None);
        assert_eq!(AfterPlayback::Close.power(), None);
        assert_eq!(AfterPlayback::Sleep.power(), Some(PowerAction::Sleep));
        assert_eq!(AfterPlayback::Hibernate.power(), Some(PowerAction::Hibernate));
        assert_eq!(AfterPlayback::Shutdown.power(), Some(PowerAction::Shutdown));
        let caps = PowerCaps {
            hibernate: Avail::No(PowerError::NotSupported),
            ..PowerCaps::all_yes()
        };
        assert_eq!(caps.of(PowerAction::Sleep), &Avail::Yes);
        assert_eq!(caps.of(PowerAction::Hibernate), &Avail::No(PowerError::NotSupported));
        assert_eq!(PowerCaps::default().of(PowerAction::Shutdown), &Avail::Unknown);
    }

    #[test]
    fn texts_in_both_languages() {
        set_lang(Lang::ZhTw);
        assert_eq!(AfterPlayback::Close.countdown_line(7), "7 秒後關閉影戲");
        assert_eq!(AfterPlayback::Shutdown.countdown_line(60), "60 秒後關機");
        assert_eq!(AfterPlayback::Close.cancelled(), "已取消：播完後關閉影戲");
        assert_eq!(AfterPlayback::Close.after_export(), "匯出完成後再關閉影戲");
        assert_eq!(
            PowerAction::Shutdown.failed(&PowerError::NoPermission),
            "無法關機：沒有權限"
        );
        // 系統的原文照樣放
        assert_eq!(
            PowerAction::Sleep.failed(&PowerError::Failed("E_ACCESSDENIED".into())),
            "無法進入睡眠：E_ACCESSDENIED"
        );
        set_lang(Lang::En);
        assert_eq!(AfterPlayback::Close.countdown_line(7), "Closing VitaScope in 7 s");
        assert_eq!(AfterPlayback::Sleep.countdown_line(1), "Sleeping in 1 s");
        assert_eq!(
            AfterPlayback::Shutdown.cancelled(),
            "Cancelled: shut down when playback ends"
        );
        assert_eq!(
            PowerAction::Hibernate.failed(&PowerError::NotSupported),
            "Couldn't hibernate: Not supported on this computer"
        );
        set_lang(Lang::ZhTw);
        for a in AfterPlayback::ALL {
            assert!(!a.label().is_empty() && !a.noun().is_empty());
        }
    }

    /// 播完後的動作只對這次有效：型別本身不能存檔（編譯時檢查），設定檔裡也沒有它
    #[test]
    fn after_playback_is_not_serialisable() {
        // 型別有 Serialize / Deserialize 的話，下面的 `some_item` 有兩個實作可以選，編譯不過
        trait AmbiguousIfSerialize<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfSerialize<()> for T {}
        #[allow(dead_code)]
        struct Invalid;
        impl<T: ?Sized + serde::Serialize> AmbiguousIfSerialize<Invalid> for T {}
        let _ = <AfterPlayback as AmbiguousIfSerialize<_>>::some_item;
        trait AmbiguousIfDeserialize<A> {
            fn some_item() {}
        }
        impl<T: ?Sized> AmbiguousIfDeserialize<()> for T {}
        #[allow(dead_code)]
        struct Invalid2;
        impl<T: for<'de> serde::Deserialize<'de>> AmbiguousIfDeserialize<Invalid2> for T {}
        let _ = <AfterPlayback as AmbiguousIfDeserialize<_>>::some_item;

        let json = serde_json::to_string(&crate::settings::Settings::default()).unwrap();
        assert!(!json.contains("after"), "設定檔裡沒有播完後的動作：{json}");
        assert!(!json.contains("shutdown") && !json.contains("countdown"), "{json}");
    }

    // ───────────── E5：各平台的判斷規則（純函式）、查能做哪些事（不執行） ─────────────

    #[test]
    fn windows_caps_rules() {
        // 新式待命：程式要它睡眠不可靠，不管 IsPwrSuspendAllowed 怎麼說都當成不能
        assert_eq!(windows_sleep_avail(true, true), Avail::No(PowerError::ModernStandby));
        assert_eq!(windows_sleep_avail(true, false), Avail::No(PowerError::ModernStandby));
        assert_eq!(windows_sleep_avail(false, true), Avail::Yes);
        assert_eq!(windows_sleep_avail(false, false), Avail::No(PowerError::NotSupported));
        assert_eq!(windows_hibernate_avail(true), Avail::Yes);
        assert_eq!(windows_hibernate_avail(false), Avail::No(PowerError::HibernateOff));
    }

    #[test]
    fn privilege_error_maps_to_message() {
        set_lang(Lang::ZhTw);
        assert_eq!(windows_privilege_result(true, 0), Ok(()));
        // 成功但沒有全部打開：帳號沒有關機的權限
        assert_eq!(
            windows_privilege_result(true, ERROR_NOT_ALL_ASSIGNED),
            Err(PowerError::NoShutdownPrivilege)
        );
        assert_eq!(
            PowerAction::Shutdown.failed(&PowerError::NoShutdownPrivilege),
            "無法關機：沒有關機的權限"
        );
        // 呼叫本身失敗：系統的錯誤說明（原文）
        let Err(PowerError::Failed(text)) = windows_privilege_result(false, 5) else {
            panic!("失敗要回報系統的錯誤");
        };
        assert!(!text.is_empty());
        assert_eq!(text, os_error(5));
    }

    #[test]
    fn windows_input_age_survives_the_tick_wrap() {
        let ms = Duration::from_millis;
        assert_eq!(windows_tick_age(10_000, 9_000), ms(1_000));
        assert_eq!(windows_tick_age(5, 5), ms(0));
        // 開機 49.7 天後 GetTickCount 繞回 0
        assert_eq!(windows_tick_age(100, u32::MAX - 99), ms(200));
        // 最後的輸入比「現在」還晚（同一毫秒內的競爭）：當成剛剛，不能變成 49 天前（那樣就漏掉取消）
        assert_eq!(windows_tick_age(1_000, 1_003), ms(0));
    }

    #[test]
    fn logind_answers_map_to_avail() {
        assert_eq!(logind_avail("yes"), Avail::Yes);
        // 要密碼：人不在時沒有人輸入，停用
        assert_eq!(logind_avail("challenge"), Avail::No(PowerError::NeedsPassword));
        assert_eq!(logind_avail("na"), Avail::No(PowerError::NotSupported));
        assert_eq!(logind_avail("no"), Avail::No(PowerError::LogindRefuses("no".into())));
        assert_eq!(
            logind_avail("something-new"),
            Avail::No(PowerError::LogindRefuses("something-new".into()))
        );
        // D-Bus 的錯誤回覆
        assert_eq!(
            logind_error("org.freedesktop.DBus.Error.ServiceUnknown", Some("no logind")),
            PowerError::NoLogind
        );
        assert_eq!(
            logind_error("org.freedesktop.DBus.Error.NameHasNoOwner", None),
            PowerError::NoLogind
        );
        assert_eq!(
            logind_error("org.freedesktop.DBus.Error.InteractiveAuthorizationRequired", None),
            PowerError::NeedsPassword
        );
        assert_eq!(
            logind_error("org.freedesktop.DBus.Error.AccessDenied", Some("x")),
            PowerError::NoPermission
        );
        assert_eq!(
            logind_error(
                "org.freedesktop.login1.OperationInProgress",
                Some(" There's already a shutdown ")
            ),
            PowerError::Failed("There's already a shutdown".into())
        );
        assert_eq!(
            logind_error("org.freedesktop.login1.Whatever", Some("  ")),
            PowerError::Failed("org.freedesktop.login1.Whatever".into())
        );
        set_lang(Lang::ZhTw);
        assert_eq!(
            PowerError::NeedsPassword.message(),
            "需要系統管理員密碼：離開電腦時不會執行"
        );
        assert_eq!(
            PowerError::LogindRefuses("no".into()).message(),
            "系統不允許（logind：no）"
        );
    }

    #[test]
    fn macos_osascript_arguments_and_permission_errors() {
        // 用絕對路徑執行、不經過 shell：參數就是 -e 與 AppleScript 本身
        assert_eq!(OSASCRIPT_ARGS[0], "-e");
        assert_eq!(
            OSASCRIPT_ARGS[1],
            "tell application \"loginwindow\" to «event aevtshut»"
        );
        // 不是「顯示關機對話框」（rsdn）、也不是重新開機（rrst）
        assert!(!OSASCRIPT_ARGS[1].contains("rsdn") && !OSASCRIPT_ARGS[1].contains("rrst"));
        assert_eq!(osascript_result(true, ""), Ok(()));
        // 沒有「自動化」權限
        let denied = "36:73: execution error: Not authorized to send Apple events to loginwindow. (-1743)\n";
        assert_eq!(osascript_result(false, denied), Err(PowerError::MacAutomationDenied));
        assert_eq!(
            osascript_result(false, "execution error: User canceled. (-128)\n"),
            Err(PowerError::Failed("execution error: User canceled. (-128)".into()))
        );
        assert_eq!(osascript_result(false, ""), Err(PowerError::Failed("osascript".into())));
        // 選「關機」時先問權限：只有明確被拒才擋
        assert_eq!(automation_permission(0), Ok(()));
        assert_eq!(automation_permission(-1743), Err(PowerError::MacAutomationDenied));
        assert_eq!(
            automation_permission(-600),
            Ok(()),
            "loginwindow 一時找不到：執行時再說"
        );
        assert_eq!(automation_permission(-1744), Ok(()));
        set_lang(Lang::En);
        assert!(
            PowerError::MacAutomationDenied
                .message()
                .contains("Privacy & Security → Automation")
        );
        set_lang(Lang::ZhTw);
        assert_eq!(
            PowerAction::Shutdown.failed(&PowerError::MacAutomationDenied),
            "無法關機：macOS 不允許影戲控制 loginwindow：請到「系統設定 → 隱私權與安全性 → 自動化」允許"
        );
    }

    #[test]
    fn every_reason_has_text_in_both_languages() {
        let all = [
            PowerError::NotSupported,
            PowerError::NoPermission,
            PowerError::ModernStandby,
            PowerError::HibernateOff,
            PowerError::NoShutdownPrivilege,
            PowerError::NoLogind,
            PowerError::NeedsPassword,
            PowerError::LogindRefuses("no".into()),
            PowerError::MacAutomationDenied,
        ];
        for lang in [Lang::ZhTw, Lang::En] {
            set_lang(lang);
            let texts: Vec<String> = all.iter().map(PowerError::message).collect();
            for (e, t) in all.iter().zip(&texts) {
                assert!(!t.is_empty(), "{e:?}");
            }
            let mut unique = texts.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), texts.len(), "每個原因的說明都不一樣：{texts:?}");
        }
        set_lang(Lang::En);
        assert_eq!(
            PowerError::ModernStandby.message(),
            "This computer uses Modern Standby, so programs can't put it to sleep"
        );
        set_lang(Lang::ZhTw);
        assert_eq!(
            PowerError::HibernateOff.message(),
            "休眠沒有開啟（系統管理員可以執行 powercfg /hibernate on）"
        );
    }

    /// 這台電腦能做哪些事：真的去問系統，**只問、不執行**（不會睡眠或關機）
    #[cfg(windows)]
    #[test]
    fn windows_caps_probe_answers_without_acting() {
        let caps = platform::caps();
        // 關機一律列出（權限在執行時才打開）；睡眠、休眠看這台電腦，但一定有答案（不是「檢查中」）
        assert_eq!(caps.shutdown, Avail::Yes);
        for a in [&caps.sleep, &caps.hibernate] {
            assert!(
                matches!(
                    a,
                    Avail::Yes
                        | Avail::No(PowerError::ModernStandby | PowerError::NotSupported | PowerError::HibernateOff)
                ),
                "{caps:?}"
            );
        }
        // 整個系統多久沒有輸入查得到（只是讀，不會影響任何東西）
        let age = platform::Power::new().last_input_age();
        assert!(age.is_some(), "GetLastInputInfo");
        assert_eq!(platform::prepare(PowerAction::Shutdown), Ok(()));
    }

    /// Windows 關機不強制關掉別的程式（不加 `EWX_FORCE`）：有沒存的資料的程式還是可以擋下來
    #[cfg(windows)]
    #[test]
    fn windows_shutdown_never_forces_apps_closed() {
        use windows_sys::Win32::System::Shutdown::{
            EWX_FORCE, EWX_FORCEIFHUNG, EWX_POWEROFF, SHTDN_REASON_FLAG_PLANNED,
        };
        assert_eq!(platform::SHUTDOWN_FLAGS & EWX_FORCE, 0, "不加 EWX_FORCE");
        assert_eq!(platform::SHUTDOWN_FLAGS, EWX_POWEROFF | EWX_FORCEIFHUNG);
        assert_ne!(
            platform::SHUTDOWN_REASON_CODE & SHTDN_REASON_FLAG_PLANNED,
            0,
            "預先計畫的關機"
        );
    }

    /// Linux：沒有系統匯流排、沒有 logind（Docker、CI）也不會當掉，回答「找不到 logind」之類的原因。**只問、不執行**
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn logind_probe_does_not_panic() {
        let caps = platform::caps();
        for a in [&caps.sleep, &caps.hibernate, &caps.shutdown] {
            assert_ne!(a, &Avail::Unknown, "{caps:?}");
        }
        // 沒有 X、沒有 libXss、Wayland：查不到；有的話是一段時間。兩種都不會當掉，第二次也一樣
        let p = platform::Power::new();
        let first = p.last_input_age();
        let second = p.last_input_age();
        assert_eq!(first.is_some(), second.is_some());
    }

    /// macOS：能做哪些事、整個系統多久沒有輸入（CoreGraphics 的宣告對不對）。**只問、不執行**；
    /// 不呼叫 `prepare`（會跳出「自動化」的詢問、等人回答）
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_caps_and_idle_probe_answers_without_acting() {
        let caps = platform::caps();
        assert_eq!(caps.sleep, Avail::Yes);
        assert_eq!(caps.hibernate, Avail::No(PowerError::NotSupported));
        assert_eq!(caps.shutdown, Avail::Yes);
        // 宣告錯的話（參數、回傳的型別）常是 NaN 或負的：查不到
        let age = platform::Power::new().last_input_age();
        assert!(age.is_some(), "CGEventSourceSecondsSinceLastEventType");
    }

    /// 真的讓電腦睡眠、關機的 `RealPower` 只有 main.rs 建立：其他程式碼、任何測試都不能建立它
    #[test]
    fn real_power_is_only_created_by_main() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let created = ["RealPower", "::new"].concat();
        let (mut creators, mut in_tests) = (Vec::new(), Vec::new());
        let mut stack = vec![
            root.join("src"),
            root.join("tests"),
            root.join("benches"),
            root.join("examples"),
        ];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                let name = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if text.contains(&created) {
                    creators.push(name.clone());
                }
                // 測試（tests/ 底下，含 support）裡連名字都不會出現
                if name.starts_with("tests/") && text.contains("RealPower") {
                    in_tests.push(name);
                }
            }
        }
        assert_eq!(creators, ["src/main.rs"], "只有 main.rs 可以建立 RealPower");
        assert!(in_tests.is_empty(), "{in_tests:?}");
    }
}
