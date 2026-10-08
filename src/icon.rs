//! 應用程式圖示。圖檔在 `packaging/icons/`（`cargo run --example make_icon` 產生），
//! 視窗標題列、工作列用的是這裡的；Windows 執行檔、macOS 的 .app、Linux 的 .desktop 另外在打包時放進去。

use eframe::egui::IconData;

/// 視窗圖示（256×256，工作列和 Alt+Tab 會縮小）
const WINDOW_ICON: &[u8] = include_bytes!("../packaging/icons/icon-256.png");

/// 解開內建的 PNG；壞掉的話回傳 None（用系統預設圖示，不影響播放）
pub fn window_icon() -> Option<IconData> {
    decode_rgba(WINDOW_ICON)
}

fn decode_rgba(bytes: &[u8]) -> Option<IconData> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // 調色盤、16 位元之類的格式都轉成 8 位元
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        _ => return None,
    };
    Some(IconData {
        rgba,
        width: info.width,
        height: info.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_icon_decodes_to_rgba() {
        let icon = window_icon().expect("內建圖示要能解開");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        // 四個角是透明的（圓角），中間不透明
        assert_eq!(icon.rgba[3], 0);
        let center = ((128 * 256 + 128) * 4 + 3) as usize;
        assert_eq!(icon.rgba[center], 255);
    }

    #[test]
    fn broken_png_gives_none() {
        assert!(decode_rgba(b"not a png").is_none());
    }
}
