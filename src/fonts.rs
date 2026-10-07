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

/// 固定路徑都找不到時，問 fontconfig 哪些字型涵蓋繁體中文（Linux 各發行版的路徑差很多）。
/// 不用 fc-match：沒有中文字型時它會回傳最接近的西文字型，中文仍然顯示成方塊，也不會提示安裝字型
fn fontconfig_lookup() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        return None;
    }
    let out = crate::syscmd::command("fc-list")
        .args(["-f", "%{file}\n", ":lang=zh-tw"])
        .output()
        .ok()?;
    let list = String::from_utf8_lossy(&out.stdout);
    let path = PathBuf::from(pick_font(&list)?);
    path.is_file().then_some(path)
}

/// 從 fc-list 列出的字型檔裡挑一個：優先選常見的無襯線中文字型（同一套字型選 Regular），
/// 都沒有就依路徑排序取第一個
fn pick_font(list: &str) -> Option<&str> {
    let mut fonts: Vec<&str> = list.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    fonts.sort_unstable();
    const PREFERRED: [&str; 5] = [
        "NotoSansCJK",
        "NotoSansTC",
        "SourceHanSans",
        "wqy-",
        "DroidSansFallback",
    ];
    let family = |p: &str| {
        let matches: Vec<&str> = fonts.iter().copied().filter(|f| f.contains(p)).collect();
        matches
            .iter()
            .copied()
            .find(|f| f.contains("Regular"))
            .or(matches.first().copied())
    };
    PREFERRED.iter().find_map(|p| family(p)).or(fonts.first().copied())
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

#[cfg(test)]
mod tests {
    use super::pick_font;

    #[test]
    fn picks_a_chinese_font_from_fc_list() {
        let list = "/usr/share/fonts/truetype/arphic/uming.ttc\n\n\
                    /usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc\n\
                    /usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc\n";
        assert_eq!(
            pick_font(list),
            Some("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")
        );
        let bold_only = "/usr/share/fonts/truetype/arphic/uming.ttc\n/usr/share/fonts/noto/NotoSansCJK-Bold.ttc\n";
        assert_eq!(pick_font(bold_only), Some("/usr/share/fonts/noto/NotoSansCJK-Bold.ttc"));
        assert_eq!(pick_font("/b/x.ttf\n/a/y.ttf\n"), Some("/a/y.ttf"));
        // 沒有任何中文字型：不要硬選一個，讓呼叫端印出安裝字型的提示
        assert_eq!(pick_font(""), None);
        assert_eq!(pick_font("\n  \n"), None);
    }
}
