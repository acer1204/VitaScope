//! 假的 `SystemPower`（「播完後 ▸」的睡眠、休眠、關機）：只記下要求，絕不呼叫真的系統。
//! 系統最後一次有鍵盤、滑鼠動作的時間由測試決定（[`FakePower::input_now`] = 使用者在別的程式裡打字）。

use std::sync::Mutex;
use std::time::{Duration, Instant};
use vitascope::system_power::{PowerAction, PowerCaps, PowerError, SystemPower};

pub struct FakePower {
    caps: Mutex<PowerCaps>,
    runs: Mutex<Vec<PowerAction>>,
    result: Mutex<Result<(), PowerError>>,
    /// 系統最後一次有輸入的時間（None = 查不到，像 Wayland）
    last_input: Mutex<Option<Instant>>,
}

impl FakePower {
    /// 系統支援 `caps`；最後一次輸入是現在（之後開始的倒數不會被它取消）
    pub fn new(caps: PowerCaps) -> Self {
        Self {
            caps: Mutex::new(caps),
            runs: Mutex::new(Vec::new()),
            result: Mutex::new(Ok(())),
            last_input: Mutex::new(Some(Instant::now())),
        }
    }

    /// 執行時回傳這個結果（例如沒有權限）
    pub fn fail_with(&self, e: PowerError) {
        *self.result.lock().unwrap() = Err(e);
    }

    /// 使用者現在在別的程式裡打字、移動滑鼠
    pub fn input_now(&self) {
        *self.last_input.lock().unwrap() = Some(Instant::now());
    }

    /// 查不到系統的輸入（Wayland）
    pub fn no_input_info(&self) {
        *self.last_input.lock().unwrap() = None;
    }

    /// 被要求執行過的動作
    pub fn runs(&self) -> Vec<PowerAction> {
        self.runs.lock().unwrap().clone()
    }
}

impl SystemPower for FakePower {
    fn caps(&self) -> PowerCaps {
        self.caps.lock().unwrap().clone()
    }

    fn run(&self, action: PowerAction) -> Result<(), PowerError> {
        self.runs.lock().unwrap().push(action);
        self.result.lock().unwrap().clone()
    }

    fn last_input_age(&self) -> Option<Duration> {
        self.last_input.lock().unwrap().map(|t| t.elapsed())
    }
}
