//! 假的 `Inhibitor`（播放時螢幕不關閉）：只記下要求，絕不向系統要求。

use std::sync::Mutex;
use std::time::Duration;
use vitascope::inhibit::{Inhibitor, Want};

#[derive(Default)]
pub struct FakeInhibitor {
    /// 收到的每一個要求（照順序）
    history: Mutex<Vec<Want>>,
    /// 被等過幾次（睡眠、關機、結束之前等它放掉）
    settles: Mutex<usize>,
}

impl FakeInhibitor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 現在拿著的（最後一個要求；沒收過是 Nothing）
    pub fn current(&self) -> Want {
        self.history.lock().unwrap().last().copied().unwrap_or_default()
    }

    pub fn history(&self) -> Vec<Want> {
        self.history.lock().unwrap().clone()
    }

    pub fn settles(&self) -> usize {
        *self.settles.lock().unwrap()
    }
}

impl Inhibitor for FakeInhibitor {
    fn set(&self, want: Want) {
        self.history.lock().unwrap().push(want);
    }

    fn settle(&self, _timeout: Duration) -> bool {
        *self.settles.lock().unwrap() += 1;
        true
    }
}
