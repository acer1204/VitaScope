//! 播完後的動作（右鍵選單「播完後 ▸」）：不做任何事、關閉影戲、睡眠、休眠、關機。
//!
//! - 只對這次有效：[`AfterPlayback`] 刻意不能存檔（沒有 serde），每次啟動都是「不做任何事」，
//!   執行、失敗或取消之後也回到「不做任何事」。
//! - 一定先倒數（[`Countdown`]）：關閉影戲 10 秒，睡眠 / 休眠 30 秒，關機 60 秒；倒數時任何動作都會取消。
//! - 真的讓電腦睡眠、關機的是注入的 [`SystemPower`]（`Launch.system`）：自動測試、`--shot` 沒有，
//!   選單也就不列出睡眠、休眠、關機，任何測試都不可能讓電腦睡眠或關機。
//! - 背景執行緒（查系統能不能睡眠、真的執行）只回傳 [`PowerError`]，文字由介面的執行緒產生（跟著目前的語言）。

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

/// 不能睡眠、關機的原因（背景執行緒回傳這個，文字在介面的執行緒產生）。
/// 各平台更精確的原因（新式待命、休眠沒開、logind…）之後加在這裡
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerError {
    /// 這台電腦（或這個系統）不支援
    NotSupported,
    /// 沒有權限
    NoPermission,
    /// 系統回報的錯誤（原文，不翻譯）
    Failed(String),
}

impl PowerError {
    pub fn message(&self) -> String {
        match self {
            PowerError::NotSupported => crate::tr!("這台電腦不支援", "Not supported on this computer").into(),
            PowerError::NoPermission => crate::tr!("沒有權限", "Permission denied").into(),
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

/// 讓電腦睡眠、休眠、關機的方法（`Launch.system`）。自動測試只用假的；真的（各平台）只由 main.rs 建立
pub trait SystemPower: Send + Sync {
    /// 能做哪些事。可能很慢（Linux 要問 logind）：在背景執行緒呼叫
    fn caps(&self) -> PowerCaps;
    /// 真的睡眠 / 休眠 / 關機。可能要等很久（Windows 的睡眠醒來後才回傳）：在背景執行緒呼叫
    fn run(&self, action: PowerAction) -> Result<(), PowerError>;
    /// 整個系統（不只影戲的視窗）多久沒有鍵盤、滑鼠的動作。None = 查不到（例如 Wayland）。
    /// 倒數期間別的程式裡有動作（使用者在電腦前面）就取消；很快，在介面的執行緒上呼叫
    fn last_input_age(&self) -> Option<Duration>;
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
}
