// 發佈版不要多開一個主控台視窗；開發版保留，方便看記錄
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use vitascope::app::{Launch, VitascopeApp, app_name};
use vitascope::autoshot::AutoShot;
use vitascope::history::History;
use vitascope::instance;
use vitascope::player::{Options, Player};
use vitascope::playlist::Playlist;
use vitascope::settings::Settings;

/// 印出版本後結束。會真的載入 libmpv，發佈流程用它確認打包出來的程式能執行
fn print_version() -> ! {
    let version = env!("CARGO_PKG_VERSION");
    match Player::new(Options::headless()) {
        Ok(p) => {
            let mpv = p.get_string("mpv-version").unwrap_or_default();
            let ffmpeg = p.get_string("ffmpeg-version").unwrap_or_default();
            println!("VitaScope {version}\n{mpv}\nFFmpeg {ffmpeg}");
            std::process::exit(0)
        }
        Err(e) => {
            eprintln!("VitaScope {version}\n無法載入 libmpv：{e}");
            std::process::exit(1)
        }
    }
}

/// 解除安裝程式呼叫：移除檔案關聯（包括安裝後才在設定裡打開的），設定裡的選項也關掉，然後結束
fn unregister_associations() -> ! {
    #[cfg(windows)]
    {
        vitascope::assoc::unregister(&vitascope::assoc::Places::default());
        let mut settings = Settings::load();
        if settings.file_associations {
            settings.file_associations = false;
            let _ = settings.save();
        }
    }
    std::process::exit(0)
}

/// 命令列：`vitascope [影片…] [--fullscreen] [--new-window] [--version] [--shot 輸出.png [--shot-delay 秒]]`
/// 回傳（啟動參數, 只對這一次開新視窗）
fn parse_args() -> (Launch, bool) {
    let mut launch = Launch::default();
    let mut shot: Option<PathBuf> = None;
    let mut delay = 1.5;
    let mut new_window = false;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--version" | "-V") => print_version(),
            Some("--unregister-associations") => unregister_associations(),
            Some("--fullscreen") => launch.fullscreen = true,
            Some("--new-window") => new_window = true,
            Some("--shot") => shot = args.next().map(PathBuf::from),
            Some("--shot-delay") => {
                delay = args.next().and_then(|v| v.to_str()?.parse().ok()).unwrap_or(delay);
            }
            // 舊版 macOS 從 Finder 開啟時會多一個 -psn_… 參數
            Some(a) if a.starts_with("-psn_") => {}
            // 檔案總管把好幾個檔案拖到程式圖示上、Linux 的 %F：可以有好幾個檔案
            _ => launch.files.push(PathBuf::from(arg)),
        }
    }
    launch.autoshot = shot.map(|p| AutoShot::new(p, Duration::from_secs_f64(delay)));
    // 自動截圖（開發、CI 用）不讀也不寫播放紀錄、播放清單：畫面才固定，也不會混進使用者的最近開啟清單
    if launch.autoshot.is_none() {
        launch.history = History::load();
        launch.persist_playlist = true;
        // 沒有指定要開的檔案：還原上次手動整理的清單（不自動播）
        if launch.files.is_empty() {
            launch.playlist = vitascope::m3u::load_session().map(|(items, current)| Playlist::restored(items, current));
        }
    }
    (launch, new_window)
}

/// 另一個程式（例如雙擊檔案）傳過來的相對路徑，要在這邊轉成完整路徑（兩個程式的目前資料夾不同）
fn absolute_paths(files: &[PathBuf]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|p| {
            if vitascope::m3u::is_url(&p.to_string_lossy()) {
                p.clone()
            } else {
                std::path::absolute(p).unwrap_or_else(|_| p.clone())
            }
        })
        .collect()
}

fn main() -> eframe::Result {
    let (mut launch, new_window) = parse_args();

    // 自動截圖讀使用者的設定（字幕外觀之類的），但不寫回去
    let settings = if launch.autoshot.is_some() {
        Settings::load().detached()
    } else {
        Settings::load()
    };
    vitascope::i18n::set_lang(settings.language);
    #[cfg(windows)]
    let settings = refresh_file_associations(settings);

    // mpv 有新事件時要喚醒 egui；egui 的 Context 要等視窗建立後才有，先留一個位置
    let egui_ctx: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());

    // 單一執行個體：已經有影戲開著的話，把檔案交給它，自己結束（在載入播放引擎之前）
    if launch.autoshot.is_none() {
        let ctx = egui_ctx.clone();
        let wake: instance::Wake = Arc::new(move || {
            if let Some(c) = ctx.get() {
                c.request_repaint();
            }
        });
        let req = instance::Request {
            paths: absolute_paths(&launch.files),
            fullscreen: launch.fullscreen,
        };
        let forward = settings.single_instance && !new_window;
        match instance::Endpoint::for_current_user().map(|ep| instance::start(&ep, &req, forward, wake)) {
            Ok(instance::Startup::Forwarded) => std::process::exit(0),
            Ok(instance::Startup::Primary(p)) => launch.instance = Some(p),
            Ok(instance::Startup::Standalone(why)) => eprintln!("[vitascope] 不使用單一執行個體：{why}"),
            Err(e) => eprintln!("[vitascope] 不使用單一執行個體：{e}"),
        }
    }
    let wake = egui_ctx.clone();
    let player = match Player::new(Options {
        wakeup: Some(Box::new(move || {
            if let Some(ctx) = wake.get() {
                ctx.request_repaint();
            }
        })),
        hwdec: if settings.hwdec { "auto-safe" } else { "no" }.into(),
        ..Options::default()
    }) {
        Ok(p) => p,
        Err(e) => fatal(&vitascope::tf!(
            "無法啟動播放引擎 libmpv：\n{e}",
            "Cannot start the libmpv playback engine:\n{e}"
        )),
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(app_name())
        // Linux：Wayland 用它找 .desktop 檔與圖示，X11 是 WM_CLASS
        .with_app_id("io.github.acer1204.vitascope")
        .with_inner_size([960.0, 600.0])
        .with_min_inner_size([vitascope::app::MIN_WINDOW_WIDTH, 300.0])
        .with_drag_and_drop(true);
    if let Some(icon) = vitascope::icon::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    if let Some(g) = settings.window {
        viewport = viewport
            .with_inner_size(g.size)
            .with_position(g.pos)
            .with_maximized(g.maximized);
    }
    if settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions {
        viewport,
        // mpv 的 render API 只支援 OpenGL
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    // macOS：Finder 開檔是用 Apple Event 送來的（要在視窗建立之前裝好處理器）
    #[cfg(target_os = "macos")]
    vitascope::macos_open::install();

    let result = eframe::run_native(
        "VitaScope",
        options,
        Box::new(move |cc| {
            let _ = egui_ctx.set(cc.egui_ctx.clone());
            // 啟動時從 Finder 開的檔案（這時已經收到了）
            #[cfg(target_os = "macos")]
            {
                launch.files.extend(vitascope::macos_open::take());
                vitascope::macos_open::set_context(&cc.egui_ctx);
            }
            Ok(Box::new(VitascopeApp::new(cc, player, settings, launch)))
        }),
    );
    // 發佈版沒有主控台，視窗開不起來（例如顯示卡不支援 OpenGL 3）時要用對話框告訴使用者
    if let Err(e) = &result {
        fatal(&vitascope::tf!(
            "無法開啟播放器視窗：\n{e}\n\n請確認顯示卡驅動程式已安裝，並支援 OpenGL 3.0 以上。",
            "Cannot open the player window:\n{e}\n\nMake sure a graphics driver with OpenGL 3.0 or later is installed."
        ));
    }
    result
}

/// 檔案關聯的設定跟登錄檔對齊：
/// - 安裝程式勾了檔案關聯：設定裡的選項跟著打開
/// - 免安裝版搬到別的資料夾：關聯指向舊的位置，重新登錄（開發中的建置不登錄，免得關聯指到建置資料夾）
#[cfg(windows)]
fn refresh_file_associations(mut settings: Settings) -> Settings {
    let Ok(exe) = std::env::current_exe() else {
        return settings;
    };
    let places = vitascope::assoc::Places::default();
    let registered = vitascope::assoc::is_registered(&exe, &places);
    if registered {
        settings.file_associations = true;
    } else if settings.file_associations
        && !cfg!(debug_assertions)
        && let Err(e) = vitascope::assoc::register(&exe, &places)
    {
        eprintln!("[vitascope] 無法更新檔案關聯：{e}");
    }
    settings
}

/// 印出錯誤（給從終端機啟動的情況）並顯示對話框，然後結束
fn fatal(message: &str) -> ! {
    eprintln!("[vitascope] {message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(app_name())
        .set_description(message)
        .show();
    std::process::exit(1)
}
