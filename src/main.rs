// 發佈版不要多開一個主控台視窗；開發版保留，方便看記錄
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use vitascope::app::{APP_NAME, Launch, VitascopeApp};
use vitascope::autoshot::AutoShot;
use vitascope::player::{Options, Player};
use vitascope::settings::Settings;

/// 命令列：`vitascope [影片] [--fullscreen] [--shot 輸出.png [--shot-delay 秒]]`
fn parse_args() -> Launch {
    let mut launch = Launch::default();
    let mut shot: Option<PathBuf> = None;
    let mut delay = 1.5;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
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
        Err(e) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title(APP_NAME)
                .set_description(format!("無法啟動播放引擎 libmpv：\n{e}"))
                .show();
            std::process::exit(1);
        }
    };

    let settings = Settings::load();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_inner_size([960.0, 600.0])
        .with_min_inner_size([480.0, 300.0])
        .with_drag_and_drop(true);
    if let Some(g) = settings.window {
        viewport = viewport
            .with_inner_size(g.size)
            .with_position(g.pos)
            .with_maximized(g.maximized);
    }
    let options = eframe::NativeOptions {
        viewport,
        // mpv 的 render API 只支援 OpenGL
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "VitaScope",
        options,
        Box::new(move |cc| {
            let _ = egui_ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(VitascopeApp::new(cc, player, settings, launch)))
        }),
    )
}
