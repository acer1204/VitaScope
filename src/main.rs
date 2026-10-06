// 發佈版不要多開一個主控台視窗；開發版保留，方便看記錄
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use vitascope::app::{APP_NAME, Launch, VitascopeApp};
use vitascope::autoshot::AutoShot;
use vitascope::history::History;
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

/// 命令列：`vitascope [影片] [--fullscreen] [--version] [--shot 輸出.png [--shot-delay 秒]]`
fn parse_args() -> Launch {
    let mut launch = Launch::default();
    let mut shot: Option<PathBuf> = None;
    let mut delay = 1.5;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--version" | "-V") => print_version(),
            Some("--fullscreen") => launch.fullscreen = true,
            Some("--shot") => shot = args.next().map(PathBuf::from),
            Some("--shot-delay") => {
                delay = args.next().and_then(|v| v.to_str()?.parse().ok()).unwrap_or(delay);
            }
            _ if launch.file.is_none() => launch.file = Some(PathBuf::from(arg)),
            _ => {}
        }
    }
    launch.autoshot = shot.map(|p| AutoShot::new(p, Duration::from_secs_f64(delay)));
    // 自動截圖（開發、CI 用）不讀也不寫播放紀錄、播放清單：畫面才固定，也不會混進使用者的最近開啟清單
    if launch.autoshot.is_none() {
        launch.history = History::load();
        launch.persist_playlist = true;
        // 沒有指定要開的檔案：還原上次手動整理的清單（不自動播）
        if launch.file.is_none() {
            launch.playlist = vitascope::m3u::load_session().map(|(items, current)| Playlist::restored(items, current));
        }
    }
    launch
}

fn main() -> eframe::Result {
    let launch = parse_args();

    // mpv 有新事件時要喚醒 egui；egui 的 Context 要等視窗建立後才有，先留一個位置
    let egui_ctx: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    let wake = egui_ctx.clone();
    let player = match Player::new(Options {
        wakeup: Some(Box::new(move || {
            if let Some(ctx) = wake.get() {
                ctx.request_repaint();
            }
        })),
        ..Options::default()
    }) {
        Ok(p) => p,
        Err(e) => fatal(&format!("無法啟動播放引擎 libmpv：\n{e}")),
    };

    // 自動截圖讀使用者的設定（字幕外觀之類的），但不寫回去
    let settings = if launch.autoshot.is_some() {
        Settings::load().detached()
    } else {
        Settings::load()
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
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

    let result = eframe::run_native(
        "VitaScope",
        options,
        Box::new(move |cc| {
            let _ = egui_ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(VitascopeApp::new(cc, player, settings, launch)))
        }),
    );
    // 發佈版沒有主控台，視窗開不起來（例如顯示卡不支援 OpenGL 3）時要用對話框告訴使用者
    if let Err(e) = &result {
        fatal(&format!(
            "無法開啟播放器視窗：\n{e}\n\n請確認顯示卡驅動程式已安裝，並支援 OpenGL 3.0 以上。"
        ));
    }
    result
}

/// 印出錯誤（給從終端機啟動的情況）並顯示對話框，然後結束
fn fatal(message: &str) -> ! {
    eprintln!("[vitascope] {message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(APP_NAME)
        .set_description(message)
        .show();
    std::process::exit(1)
}
