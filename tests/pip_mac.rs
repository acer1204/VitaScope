//! macOS 的子母畫面（`window_mode::native`）：真的建立一個 `NSWindow`（不顯示），設定內容的比例鎖、Spaces 行為、讀回來比對。
//!
//! - AppKit 只能在主執行緒用，libtest 卻在別的執行緒跑測試：這個測試程式不用 libtest（Cargo.toml 的 `harness = false`），
//!   自己的 `main` 就在主執行緒。
//! - 視窗不顯示（不 `orderFront`），畫面上看不到，所以 macOS 上一律跑；其他系統顯示略過。
//! - 使用者真的拉邊緣改大小、浮在別的程式的全螢幕上，測試做不到：要在 Mac 上手動確認（ROADMAP 的手動確認）。

const NAME: &str = "pip_window_aspect_and_spaces";

/// libtest 帶一個值（下一個參數）的選項：那個值不是名稱篩選
const VALUE_OPTIONS: [&str; 7] = [
    "--skip",
    "--test-threads",
    "--format",
    "--color",
    "--logfile",
    "--shuffle-seed",
    "-Z",
];

/// 照 libtest 的參數決定跑不跑：`--list`、名稱篩選（`--exact`）、`--skip <名稱>`、`--ignored`（這個測試不是 ignored 的）；
/// 帶值的選項（`--test-threads 1` 之類）的值不當成篩選。回傳 (只列出, 要跑)
fn selection(args: &[String]) -> (bool, bool) {
    let (mut list, mut ignored, mut exact) = (false, false, false);
    let (mut filters, mut skips) = (Vec::new(), Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--list" => list = true,
            "--ignored" => ignored = true,
            "--exact" => exact = true,
            "--skip" => skips.extend(it.next().map(String::as_str)),
            o if VALUE_OPTIONS.contains(&o) => {
                it.next();
            }
            o if o.starts_with('-') => {}
            f => filters.push(f),
        }
    }
    let matches = |p: &str| if exact { NAME == p } else { NAME.contains(p) };
    let selected = (filters.is_empty() || filters.iter().any(|f| matches(f))) && !skips.iter().any(|s| matches(s));
    (list, selected && !ignored)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (list, selected) = selection(&args);
    if list {
        if selected {
            println!("{NAME}: test");
        }
        return;
    }
    if !selected {
        println!("\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out\n");
        return;
    }
    println!("\nrunning 1 test");
    if cfg!(target_os = "macos") {
        run();
        println!("test {NAME} ... ok");
        println!("\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n");
    } else {
        // 其他系統：什麼都不做（沒有視窗可以設，回傳「沒有設」）
        off_mac();
        println!("test {NAME} ... ignored, macOS 才有");
        println!("\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out\n");
    }
}

/// macOS 以外：不是 AppKit 的視窗什麼都不做
fn off_mac() {
    use raw_window_handle::{RawWindowHandle, Win32WindowHandle};
    use vitascope::window_mode::native;
    let hwnd = std::num::NonZeroIsize::new(1).unwrap();
    let w = RawWindowHandle::Win32(Win32WindowHandle::new(hwnd));
    assert!(!native::set_aspect(w, Some([16.0, 9.0])));
    assert_eq!(native::float_on_spaces(w), None);
    native::restore_spaces(w, 0);
}

#[cfg(not(target_os = "macos"))]
fn run() {}

#[cfg(target_os = "macos")]
fn run() {
    mac::pip_window_aspect_and_spaces();
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::{AppKitWindowHandle, RawWindowHandle};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use vitascope::window_mode::{native, pip_spaces};

    fn ratio(s: NSSize) -> f64 {
        s.width / s.height
    }

    /// 子母畫面那樣的視窗（沒有標題列、可以改大小），不顯示
    fn pip_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
        let rect = NSRect::new(NSPoint::new(100.0, 100.0), NSSize::new(480.0, 270.0));
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::Resizable;
        // SAFETY: 主執行緒；關掉時不要自己釋放（Retained 管）
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window
    }

    fn handle(window: &NSWindow) -> RawWindowHandle {
        let view = window.contentView().expect("contentView");
        let ptr = NonNull::from(&*view).cast::<c_void>();
        RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr))
    }

    pub fn pip_window_aspect_and_spaces() {
        let mtm = MainThreadMarker::new().expect("main 在主執行緒");
        let _app = NSApplication::sharedApplication(mtm);
        let window = pip_window(mtm);
        let w = handle(&window);

        // 比例鎖：設成影片的寬高
        assert!(native::set_aspect(w, Some([640.0, 360.0])));
        let got = window.contentAspectRatio();
        assert!((ratio(got) - 16.0 / 9.0).abs() < 1e-6, "contentAspectRatio = {got:?}");

        // winit 在使用者開始 / 結束改大小時把 contentResizeIncrements 設回 1×1（會清掉比例）：再設一次就回來
        window.setContentResizeIncrements(NSSize::new(1.0, 1.0));
        eprintln!(
            "[pip_mac] 設 contentResizeIncrements 1×1 之後 contentAspectRatio = {:?}",
            window.contentAspectRatio()
        );
        assert!(native::set_aspect(w, Some([360.0, 640.0])));
        let got = window.contentAspectRatio();
        assert!((ratio(got) - 9.0 / 16.0).abs() < 1e-6, "旋轉後：{got:?}");

        // 解除：contentResizeIncrements 回到 1×1。鎖之前先設成跟 1×1 分得出來的 7×5（同時也清掉上面的比例），
        // 不然「解除後是 1×1」在什麼都沒做時也成立（上面 winit 那一步已經是 1×1）
        window.setContentResizeIncrements(NSSize::new(7.0, 5.0));
        assert!(native::set_aspect(w, Some([360.0, 640.0])));
        let got = window.contentAspectRatio();
        assert!((ratio(got) - 9.0 / 16.0).abs() < 1e-6, "7×5 之後再鎖：{got:?}");
        let locked = (window.contentAspectRatio(), window.contentResizeIncrements());
        assert!(native::set_aspect(w, None));
        let cleared = (window.contentAspectRatio(), window.contentResizeIncrements());
        eprintln!("[pip_mac] 鎖著時 (contentAspectRatio, contentResizeIncrements) = {locked:?}，解除後 = {cleared:?}");
        assert_eq!((cleared.1.width, cleared.1.height), (1.0, 1.0), "{cleared:?}");
        assert_ne!(locked, cleared, "解除要改到 AppKit 的狀態");

        // Spaces 行為：一般視窗常見的「可以全螢幕」「搬到目前的桌面」換成「每個桌面都有」「可以放在別的全螢幕旁邊」
        //（同時設互斥的 AppKit 會丟例外，程式直接結束：這裡沒結束就是沒有）；其他位元照留，還原成原本的
        let original = NSWindowCollectionBehavior::FullScreenPrimary
            | NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::Managed;
        window.setCollectionBehavior(original);
        let saved = native::float_on_spaces(w).expect("有視窗");
        assert_eq!(saved, original.0);
        let now = window.collectionBehavior();
        assert_eq!(now.0, pip_spaces(original.0));
        assert!(now.contains(NSWindowCollectionBehavior::CanJoinAllSpaces));
        assert!(now.contains(NSWindowCollectionBehavior::FullScreenAuxiliary));
        assert!(now.contains(NSWindowCollectionBehavior::Managed));
        assert!(!now.contains(NSWindowCollectionBehavior::FullScreenPrimary));
        assert!(!now.contains(NSWindowCollectionBehavior::MoveToActiveSpace));
        native::restore_spaces(w, saved);
        assert_eq!(window.collectionBehavior(), original, "還原");

        // winit 的預設（什麼都沒設）也一樣
        window.setCollectionBehavior(NSWindowCollectionBehavior::Default);
        let saved = native::float_on_spaces(w).expect("有視窗");
        assert_eq!(saved, 0);
        native::restore_spaces(w, saved);
        assert_eq!(window.collectionBehavior(), NSWindowCollectionBehavior::Default);

        // 不是主執行緒：什麼都不做（AppKit 只能在主執行緒用）；視窗的比例、改大小的單位、Spaces 行為都沒變
        let state = |window: &NSWindow| {
            (
                window.contentAspectRatio(),
                window.contentResizeIncrements(),
                window.collectionBehavior(),
            )
        };
        let before = state(&window);
        let view = match w {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as usize,
            _ => unreachable!(),
        };
        let off_main = std::thread::spawn(move || {
            let ptr = NonNull::new(view as *mut c_void).unwrap();
            let w = RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr));
            (native::set_aspect(w, Some([4.0, 3.0])), native::float_on_spaces(w))
        })
        .join()
        .unwrap();
        assert_eq!(off_main, (false, None), "別的執行緒不碰視窗");
        assert_eq!(state(&window), before, "別的執行緒沒有改視窗");
        window.close();
    }
}
