//! macOS：從 Finder 開檔（雙擊、拖到 Dock 圖示、「打開檔案的應用程式」）時，檔名是用 Apple Event
//! （kAEOpenDocuments）送來的，不在命令列參數裡。
//!
//! winit 0.30 沒有這個事件，也不能換掉它的 app delegate（會 panic），所以在
//! `NSApplicationWillFinishLaunchingNotification` 的時候自己裝 Apple Event 的處理器：
//! 那時 AppKit 已經裝好它自己的預設處理器（太早裝會被蓋掉），啟動時的開檔事件還沒送出。
//!
//! 已經開著時，Finder 也是把事件送給開著的這一個（不會再開一個程式），所以 macOS 不需要單一執行個體的轉送。

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSString, NSURL,
};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const fn code(c: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*c)
}

/// 收到、還沒交給介面的檔案
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
/// 介面已經開著時，收到檔案要叫醒它
static CTX: OnceLock<egui::Context> = OnceLock::new();

define_class!(
    // SAFETY: NSObject 沒有子類別的限制；這個類別沒有實作 Drop
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "VitaScopeOpenDocuments"]
    struct OpenDocuments;

    impl OpenDocuments {
        // SAFETY: 簽章跟通知中心呼叫的一樣
        #[unsafe(method(willFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            let manager = NSAppleEventManager::sharedAppleEventManager();
            // SAFETY: handler 是自己（一直活著）；選擇器的簽章跟下面的方法一樣
            unsafe {
                let _: () = msg_send![
                    &*manager,
                    setEventHandler: self,
                    andSelector: sel!(openDocuments:withReplyEvent:),
                    forEventClass: code(b"aevt"),
                    andEventID: code(b"odoc")
                ];
            }
        }

        // SAFETY: Apple Event 處理器的簽章
        #[unsafe(method(openDocuments:withReplyEvent:))]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            let files = files_of(event);
            if files.is_empty() {
                return;
            }
            PENDING.lock().unwrap_or_else(|e| e.into_inner()).extend(files);
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
        }
    }
);

/// 事件的直接參數（'----'）是檔案網址的清單；Finder 選了好幾個檔案時是同一個事件
fn files_of(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
    // SAFETY: 都是 NSAppleEventDescriptor 的方法，回傳可能是 nil
    unsafe {
        let list: Option<Retained<NSAppleEventDescriptor>> = msg_send![event, paramDescriptorForKeyword: code(b"----")];
        let Some(list) = list else { return Vec::new() };
        let count: isize = msg_send![&*list, numberOfItems];
        let items: Vec<Retained<NSAppleEventDescriptor>> = if count <= 0 {
            vec![list]
        } else {
            (1..=count)
                .filter_map(|i| {
                    let item: Option<Retained<NSAppleEventDescriptor>> = msg_send![&*list, descriptorAtIndex: i];
                    item
                })
                .collect()
        };
        items
            .iter()
            .filter_map(|d| {
                let url: Option<Retained<NSURL>> = msg_send![&**d, fileURLValue];
                url?.path()
            })
            .map(|p| PathBuf::from(p.to_string()))
            .collect()
    }
}

/// 主執行緒上、`eframe::run_native` 之前呼叫一次
pub fn install() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let this = OpenDocuments::alloc(mtm).set_ivars(());
    // SAFETY: NSObject 的 init
    let observer: Retained<OpenDocuments> = unsafe { msg_send![super(this), init] };
    let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
    // SAFETY: observer 一直活著（下面 forget 掉）；選擇器的簽章跟上面的方法一樣
    unsafe {
        NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
            &observer,
            sel!(willFinishLaunching:),
            Some(&name),
            None,
        );
    }
    // 通知中心、Apple Event 管理員都不會保留它，程式結束前都要在
    std::mem::forget(observer);
}

/// 介面建立後設定，之後收到檔案時叫醒它
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// 取出收到的檔案
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()))
}
