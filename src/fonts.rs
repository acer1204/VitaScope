//! 載入系統的中日韓字型。egui 內建字型沒有漢字，不載入的話中文檔名、選單都會變成方塊。

use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;

/// 常見的系統字型位置（依平台）
fn candidates() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &[
            "C:/Windows/Fonts/msjh.ttc",    // 微軟正黑體
            "C:/Windows/Fonts/msyh.ttc",    // 微軟雅黑
            "C:/Windows/Fonts/mingliu.ttc", // 細明體
            "C:/Windows/Fonts/simhei.ttf",
        ]
    } else if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
        ]
    } else {
        &[
            // Debian / Ubuntu
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            // Arch
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            // Fedora
            "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-sans-cjk-vf-fonts/NotoSansCJK-VF.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ]
    }
}

/// 固定路徑都找不到時，問 fontconfig 哪個字型支援繁體中文（Linux 各發行版的路徑差很多）
fn fontconfig_lookup() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return None;
    }
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", "sans-serif:lang=zh-tw"])
        .output()
        .ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    path.is_file().then_some(path)
}

pub fn install_cjk(ctx: &egui::Context) {
    // fontconfig 只在固定路徑都找不到時才呼叫
    let paths = candidates()
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once_with(fontconfig_lookup).flatten());
    for path in paths {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts
            .font_data
            .insert("cjk".to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        // 放在最後當備援：英數字仍用 egui 預設字型，缺字時才用 CJK 字型
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push("cjk".to_owned());
        }
        ctx.set_fonts(fonts);
        return;
    }
    eprintln!("[vitascope] 找不到中文字型，中文可能無法顯示（Linux 請安裝 Noto Sans CJK，例如 fonts-noto-cjk）");
}
