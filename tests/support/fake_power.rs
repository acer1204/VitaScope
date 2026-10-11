//! 假的 `SystemPower`（「播完後 ▸」的睡眠、休眠、關機）：只記下要求，絕不呼叫真的系統。
//! 系統最後一次有鍵盤、滑鼠動作的時間由測試決定（[`FakePower::input_now`] = 使用者在別的程式裡打字）。

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use vitascope::system_power::{PowerAction, PowerCaps, PowerError, SystemPower};

pub struct FakePower {
    caps: Mutex<PowerCaps>,
    /// 被問過幾次能做哪些事（啟動時、打開選單時、選了動作時）
    caps_calls: Mutex<usize>,
    runs: Mutex<Vec<PowerAction>>,
    result: Mutex<Result<(), PowerError>>,
    /// 選了動作時先取得權限（macOS 的關機）：被要求過的、回傳的結果
    prepares: Mutex<Vec<PowerAction>>,
    prepare_result: Mutex<Result<(), PowerError>>,
    /// true = 取得權限的要求先擋著（像 macOS 等使用者回答系統的詢問），[`FakePower::release_prepares`] 才回答
    prepare_held: Mutex<bool>,
    prepare_released: Condvar,
    /// 系統最後一次有輸入的時間（None = 查不到，像 Wayland）
    last_input: Mutex<Option<Instant>>,
}

impl FakePower {
    /// 系統支援 `caps`；最後一次輸入是現在（之後開始的倒數不會被它取消）
    pub fn new(caps: PowerCaps) -> Self {
        Self {
            caps: Mutex::new(caps),
            caps_calls: Mutex::new(0),
            runs: Mutex::new(Vec::new()),
            result: Mutex::new(Ok(())),
            prepares: Mutex::new(Vec::new()),
            prepare_result: Mutex::new(Ok(())),
            prepare_held: Mutex::new(false),
            prepare_released: Condvar::new(),
            last_input: Mutex::new(Some(Instant::now())),
        }
    }

    /// 執行時回傳這個結果（例如沒有權限）
    pub fn fail_with(&self, e: PowerError) {
        *self.result.lock().unwrap() = Err(e);
    }

    /// 之後再問時系統的回答變成這樣（休眠被關掉、改成要密碼…）
    pub fn set_caps(&self, caps: PowerCaps) {
        *self.caps.lock().unwrap() = caps;
    }

    /// 被問過幾次能做哪些事
    pub fn caps_calls(&self) -> usize {
        *self.caps_calls.lock().unwrap()
    }

    /// 選了動作時取得權限的結果（例如 macOS 的「自動化」被拒）
    pub fn prepare_fails_with(&self, e: PowerError) {
        *self.prepare_result.lock().unwrap() = Err(e);
    }

    /// 之後取得權限的要求先不回答（使用者還沒回答系統的詢問）
    pub fn hold_prepares(&self) {
        *self.prepare_held.lock().unwrap() = true;
    }

    /// 使用者回答了：擋著的、之後的要求都回傳 `result`
    pub fn release_prepares(&self, result: Result<(), PowerError>) {
        *self.prepare_result.lock().unwrap() = result;
        *self.prepare_held.lock().unwrap() = false;
        self.prepare_released.notify_all();
    }

    /// 被要求先取得權限的動作
    pub fn prepares(&self) -> Vec<PowerAction> {
        self.prepares.lock().unwrap().clone()
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
        // 先拿回答再算次數：次數加了就表示回答已經定了（之後的 `set_caps` 不會影響這一次）
        let caps = self.caps.lock().unwrap().clone();
        *self.caps_calls.lock().unwrap() += 1;
        caps
    }

    fn prepare(&self, action: PowerAction) -> Result<(), PowerError> {
        self.prepares.lock().unwrap().push(action);
        let mut held = self.prepare_held.lock().unwrap();
        while *held {
            held = self.prepare_released.wait(held).unwrap();
        }
        drop(held);
        self.prepare_result.lock().unwrap().clone()
    }

    fn run(&self, action: PowerAction) -> Result<(), PowerError> {
        self.runs.lock().unwrap().push(action);
        self.result.lock().unwrap().clone()
    }

    fn last_input_age(&self) -> Option<Duration> {
        self.last_input.lock().unwrap().map(|t| t.elapsed())
    }
}
