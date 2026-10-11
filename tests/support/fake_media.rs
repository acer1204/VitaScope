//! 假的系統媒體控制（`MediaBackend::Custom`）：記下影戲送給系統的更新，測試自己送指令（像使用者按了媒體鍵、
//! 在系統的媒體控制上按了按鈕）。絕不碰真的系統媒體控制。

use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use vitascope::media_keys::{Cover, MediaBackend, MediaCommand, MediaSession, PlaybackInfo, TrackInfo, Update, Wake};

#[derive(Default)]
struct Shared {
    updates: Mutex<Vec<Update>>,
    /// 目前的工作階段收指令的那一端（結束之後 None）
    sender: Mutex<Option<Sender<MediaCommand>>>,
    wake: Mutex<Option<Wake>>,
    /// 影戲要求建立的次數、成功的次數、結束的次數
    attempts: Mutex<usize>,
    created: Mutex<usize>,
    shutdowns: Mutex<usize>,
    /// 建立時回傳這個錯誤
    fail: Mutex<Option<String>>,
    /// 目前的工作階段斷線了（`MediaSession::alive` 回傳 false；新建立的又是好的）
    lost: Mutex<bool>,
}

#[derive(Clone, Default)]
pub struct FakeMedia(Arc<Shared>);

impl FakeMedia {
    pub fn new() -> Self {
        Self::default()
    }

    /// 給 `Launch.media` 的
    pub fn backend(&self) -> MediaBackend {
        let shared = self.0.clone();
        MediaBackend::Custom(Box::new(move |wake| {
            *shared.attempts.lock().unwrap() += 1;
            if let Some(e) = shared.fail.lock().unwrap().clone() {
                return Err(e);
            }
            let (tx, rx) = mpsc::channel();
            *shared.sender.lock().unwrap() = Some(tx);
            *shared.wake.lock().unwrap() = Some(wake);
            *shared.created.lock().unwrap() += 1;
            *shared.lost.lock().unwrap() = false;
            let session: Box<dyn MediaSession> = Box::new(FakeSession {
                shared: shared.clone(),
                ended: false,
            });
            Ok((session, rx))
        }))
    }

    /// 之後建立時失敗（None = 成功）
    pub fn fail_with(&self, e: Option<&str>) {
        *self.0.fail.lock().unwrap() = e.map(str::to_owned);
    }

    /// 像系統那一端斷線了（Windows 的檔案總管重開之類的）：目前的工作階段不能用了，叫醒介面
    pub fn lose(&self) {
        *self.0.lost.lock().unwrap() = true;
        if let Some(wake) = self.0.wake.lock().unwrap().as_ref() {
            wake();
        }
    }

    /// 像使用者按了媒體鍵：送給目前的工作階段，叫醒介面。沒有工作階段時回傳 false
    pub fn send(&self, cmd: MediaCommand) -> bool {
        let sent = self
            .0
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|tx| tx.send(cmd).is_ok());
        if let Some(wake) = self.0.wake.lock().unwrap().as_ref() {
            wake();
        }
        sent
    }

    pub fn updates(&self) -> Vec<Update> {
        self.0.updates.lock().unwrap().clone()
    }

    /// 拿走到目前為止的更新（之後的重新算）
    pub fn take_updates(&self) -> Vec<Update> {
        std::mem::take(&mut *self.0.updates.lock().unwrap())
    }

    pub fn attempts(&self) -> usize {
        *self.0.attempts.lock().unwrap()
    }

    pub fn created(&self) -> usize {
        *self.0.created.lock().unwrap()
    }

    pub fn shutdowns(&self) -> usize {
        *self.0.shutdowns.lock().unwrap()
    }

    /// 最後送出的檔案資訊
    pub fn last_track(&self) -> Option<TrackInfo> {
        self.updates().into_iter().rev().find_map(|u| match u {
            Update::Track(t) => Some(t),
            _ => None,
        })
    }

    /// 最後送出的播放狀態
    pub fn last_playback(&self) -> Option<PlaybackInfo> {
        self.updates().into_iter().rev().find_map(|u| match u {
            Update::Playback { info, .. } => Some(info),
            _ => None,
        })
    }

    pub fn covers(&self) -> Vec<Cover> {
        self.updates()
            .into_iter()
            .filter_map(|u| match u {
                Update::Cover(c) => Some(c),
                _ => None,
            })
            .collect()
    }
}

struct FakeSession {
    shared: Arc<Shared>,
    ended: bool,
}

impl MediaSession for FakeSession {
    fn apply(&mut self, update: &Update) {
        assert!(!self.ended, "工作階段結束之後不能再送更新：{update:?}");
        self.shared.updates.lock().unwrap().push(update.clone());
    }

    fn alive(&self) -> bool {
        !*self.shared.lost.lock().unwrap()
    }

    fn shutdown(&mut self) {
        assert!(!self.ended, "結束了兩次");
        self.ended = true;
        *self.shared.sender.lock().unwrap() = None;
        *self.shared.shutdowns.lock().unwrap() += 1;
    }
}
