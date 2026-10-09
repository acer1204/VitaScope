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
    // 流暢播放的實機測試：VITASCOPE_TEST_MINIMIZE=3,6 截圖前縮到最小再還原；
    // VITASCOPE_TEST_BUSY_UI=1 介面一直重畫（模擬滑鼠在視窗上移動）；VITASCOPE_TEST_STALL=3,10 第 3 秒讓介面停 10 秒
    //（像開著檔案對話框）。只跟 --shot 一起用
    let minimize = std::env::var("VITASCOPE_TEST_MINIMIZE").unwrap_or_default();
    let busy_ui = std::env::var_os("VITASCOPE_TEST_BUSY_UI").is_some_and(|v| v == "1");
    launch.autoshot = shot.map(|p| {
        AutoShot::new(p, Duration::from_secs_f64(delay))
            .minimize_at(vitascope::autoshot::parse_minimize(&minimize))
            .busy_ui(busy_ui)
            .stall_at(vitascope::autoshot::parse_stall(
                &std::env::var("VITASCOPE_TEST_STALL").unwrap_or_default(),
            ))
    });
    // 流暢播放出問題時回到以前的做法：VITASCOPE_PACING=off（只在啟動時讀一次）
    launch.pacing = vitascope::pacing::Overrides::from_env();
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

fn main() -> eframe::Result {
    let (mut launch, new_window) = parse_args();
    #[cfg(windows)]
    mark_running();
    // 相對路徑轉成完整路徑：送給已經開著的視窗時（兩個程式的目前資料夾不同）、自己開成清單時都要
    launch.files = launch.files.iter().map(|p| vitascope::playlist::absolute(p)).collect();

    // 自動截圖讀使用者的設定（字幕外觀之類的），但不寫回去
    let settings = if launch.autoshot.is_some() {
        Settings::load().detached()
    } else {
        Settings::load()
    };
    vitascope::i18n::set_lang(settings.language);
    // 自動截圖（開發、CI 用）不碰登錄檔
    #[cfg(windows)]
    let settings = if launch.autoshot.is_none() {
        refresh_file_associations(settings)
    } else {
        settings
    };

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
            paths: launch.files.clone(),
            fullscreen: launch.fullscreen,
        };
        // macOS 從 Finder 開檔時，檔名是之後才用 Apple Event 送來的（命令列沒有檔案）：
        // 不能先轉送一個空的要求就結束（檔案會不見）。已經開著的 .app 本來就由系統把檔案送過去
        let finder_launch = cfg!(target_os = "macos") && launch.files.is_empty();
        let forward = settings.single_instance && !new_window && !finder_launch;
        match instance::Endpoint::for_current_user().map(|ep| instance::start(&ep, &req, forward, wake)) {
            Ok(instance::Startup::Forwarded) => std::process::exit(0),
            Ok(instance::Startup::Primary(p)) => launch.instance = Some(p),
            Ok(instance::Startup::Standalone(why)) => eprintln!("[vitascope] 不使用單一執行個體：{why}"),
            Err(e) => eprintln!("[vitascope] 不使用單一執行個體：{e}"),
        }
    }
    let wake = egui_ctx.clone();
    let pace = !launch.pacing.block;
    let player = match Player::new(Options {
        wakeup: Some(Box::new(move || {
            if let Some(ctx) = wake.get() {
                if pace {
                    // 只要一輪（`request_repaint()` 每次畫兩輪）：一般播放時 mpv 每格都有事件（播放位置），
                    // 第二輪只是再看一次還沒到時間的影格。延遲不是 0 就只畫一輪，扣掉 predicted_dt 後還是馬上畫
                    ctx.request_repaint_after_for(std::time::Duration::from_nanos(1), egui::ViewportId::ROOT);
                } else {
                    ctx.request_repaint();
                }
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
        viewport = viewport.with_inner_size(g.size).with_maximized(g.maximized);
        // 上次的位置在已經拔掉的螢幕上（或解析度變小了）時不還原，交給系統擺：不然視窗會開在看不到的地方
        match vitascope::screens::areas() {
            Some(areas) if !vitascope::screens::title_bar_visible(g.pos, g.size[0], &areas) => {
                if std::env::var_os("VITASCOPE_DEBUG").is_some() {
                    eprintln!(
                        "[vitascope] 上次的視窗位置 {:?} 不在任何螢幕上，改由系統決定（螢幕：{areas:?}）",
                        g.pos
                    );
                }
            }
            _ => viewport = viewport.with_position(g.pos),
        }
    }
    // 只有「永遠置頂」一開始就置頂；「播放時置頂」由 App 在開始播放時才設（App 記的初始層級也用 `at_launch`）
    if settings.on_top.at_launch() {
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions {
        viewport,
        // mpv 的 render API 只支援 OpenGL
        renderer: eframe::Renderer::Glow,
        // 顯示器同步靠 swap 等垂直同步；不能關
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: true,
            ..Default::default()
        },
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
                let finder = vitascope::macos_open::take();
                if !finder.is_empty() {
                    // 跟命令列指定檔案一樣：不還原上次的清單（不然關閉時會把它清掉）
                    launch.playlist = None;
                    launch.files.extend(finder);
                }
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

/// 檔案關聯的設定跟登錄檔對齊（見 `assoc::on_startup`）：安裝程式勾了檔案關聯時設定跟著打開；
/// 免安裝版搬到別的資料夾時重新登錄。開發中的建置不動登錄檔（免得關聯指到建置資料夾）
#[cfg(windows)]
fn refresh_file_associations(mut settings: Settings) -> Settings {
    use vitascope::assoc::{self, OnStartup};
    if cfg!(debug_assertions) {
        return settings;
    }
    let Ok(exe) = std::env::current_exe() else {
        return settings;
    };
    let places = assoc::Places::default();
    let registered = assoc::registered_exe(&places);
    match assoc::on_startup(settings.file_associations, registered.as_deref(), &exe, |p| p.exists()) {
        OnStartup::Adopt if !settings.file_associations => {
            settings.file_associations = true;
            // 馬上存：之後解除安裝程式關掉它時，這個視窗關閉時才不會把舊的值寫回去
            let _ = settings.save();
        }
        OnStartup::Register => {
            if let Err(e) = assoc::register(&exe, &places) {
                eprintln!("[vitascope] 無法更新檔案關聯：{e}");
                // 不要每次啟動都重試一次（設定裡顯示為關閉，使用者可以自己再打開看錯誤訊息）
                settings.file_associations = false;
                let _ = settings.save();
            }
        }
        _ => {}
    }
    settings
}

/// 讓安裝程式、解除安裝程式知道影戲開著（Inno Setup 的 AppMutex），會先請使用者關掉。
/// 程式結束時系統自動釋放，不用保存控制代碼
#[cfg(windows)]
fn mark_running() {
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "VitaScope.Running\0".encode_utf16().collect();
    // SAFETY: 名稱以 0 結尾；不需要安全性屬性
    unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
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
