//! 產生應用程式圖示：`cargo run --example make_icon`
//!
//! 圖案：深藍色圓角方形、上下兩排膠卷孔、中間白色的播放三角形。用距離場畫（有反鋸齒），
//! 輸出到 `packaging/icons/`：
//! - `icon-{16..1024}.png`：Linux（hicolor）、視窗圖示
//! - `vitascope.ico`：Windows（內含 PNG 壓縮的各尺寸）
//! - `vitascope.icns`：macOS（內含 PNG 的各尺寸）
//!
//! 想換成自己設計的圖示：直接替換這些檔案即可（尺寸與檔名照舊）。

use std::path::Path;

const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

fn main() -> std::io::Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/icons");
    std::fs::create_dir_all(&out)?;
    let mut pngs = Vec::new();
    for size in SIZES.iter().copied().chain([1024]) {
        let png = encode_png(size, &render(size));
        std::fs::write(out.join(format!("icon-{size}.png")), &png)?;
        pngs.push((size, png));
    }
    std::fs::write(out.join("vitascope.ico"), ico(&pngs))?;
    std::fs::write(out.join("vitascope.icns"), icns(&pngs))?;
    println!("圖示已寫到 {}", out.display());
    Ok(())
}

/// 畫一張 `size`×`size` 的 RGBA 圖（每個像素 4×4 超取樣）
fn render(size: u32) -> Vec<u8> {
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let samples = 4;
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0f32; 4];
            for sy in 0..samples {
                for sx in 0..samples {
                    // 0..1 的座標
                    let u = (x as f32 + (sx as f32 + 0.5) / samples as f32) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / samples as f32) / size as f32;
                    let c = shade(u, v);
                    // 預乘透明度後累加，邊緣才不會有暗邊
                    acc[0] += c[0] * c[3];
                    acc[1] += c[1] * c[3];
                    acc[2] += c[2] * c[3];
                    acc[3] += c[3];
                }
            }
            let n = (samples * samples) as f32;
            let a = acc[3] / n;
            let i = ((y * size + x) * 4) as usize;
            if a > 0.0 {
                for k in 0..3 {
                    rgba[i + k] = ((acc[k] / n / a).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
            rgba[i + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    rgba
}

/// 一個點的顏色（RGBA，0..1）
fn shade(u: f32, v: f32) -> [f32; 4] {
    // 圓角方形（留一點邊，跟系統圖示的大小差不多）
    let margin = 0.06;
    let radius = 0.2;
    let inside = rounded_rect(u, v, margin, radius);
    if !inside {
        return [0.0; 4];
    }
    // 背景：由上到下的深藍漸層
    let t = (v - margin) / (1.0 - 2.0 * margin);
    let top = [0.13, 0.36, 0.78];
    let bottom = [0.05, 0.15, 0.42];
    let mut c = [
        top[0] + (bottom[0] - top[0]) * t,
        top[1] + (bottom[1] - top[1]) * t,
        top[2] + (bottom[2] - top[2]) * t,
        1.0,
    ];
    // 上下兩條膠卷帶，帶上有方形的孔
    let band = 0.13;
    let in_top_band = v < margin + band;
    let in_bottom_band = v > 1.0 - margin - band;
    if in_top_band || in_bottom_band {
        c = [0.03, 0.08, 0.22, 1.0];
        let holes = 6.0;
        let pitch = (1.0 - 2.0 * margin) / holes;
        let local = ((u - margin) / pitch).fract();
        let band_center = if in_top_band {
            margin + band / 2.0
        } else {
            1.0 - margin - band / 2.0
        };
        if (0.3..0.7).contains(&local) && (v - band_center).abs() < band * 0.22 {
            c = [0.92, 0.95, 1.0, 1.0];
        }
    }
    // 中間的播放三角形（白色）
    let (ax, ay) = (0.38, 0.32);
    let (bx, by) = (0.38, 0.68);
    let (cx, cy) = (0.70, 0.50);
    if in_triangle(u, v, (ax, ay), (bx, by), (cx, cy)) {
        c = [1.0, 1.0, 1.0, 1.0];
    }
    c
}

fn rounded_rect(u: f32, v: f32, margin: f32, radius: f32) -> bool {
    let (lo, hi) = (margin + radius, 1.0 - margin - radius);
    let dx = if u < lo {
        lo - u
    } else if u > hi {
        u - hi
    } else {
        0.0
    };
    let dy = if v < lo {
        lo - v
    } else if v > hi {
        v - hi
    } else {
        0.0
    };
    u >= margin && u <= 1.0 - margin && v >= margin && v <= 1.0 - margin && dx * dx + dy * dy <= radius * radius
}

fn in_triangle(px: f32, py: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1);
    let p = (px, py);
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn encode_png(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, size, size);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("png header");
        w.write_image_data(rgba).expect("png data");
    }
    out
}

/// Windows .ico：每個尺寸存一張 PNG（Vista 起支援）；只收 256 以下
fn ico(pngs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let entries: Vec<&(u32, Vec<u8>)> = pngs.iter().filter(|(s, _)| *s <= 256).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len() as u32;
    for (size, png) in &entries {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &entries {
        out.extend_from_slice(png);
    }
    out
}

/// macOS .icns：各尺寸的 PNG（ic07 = 128、ic08 = 256、ic09 = 512、ic10 = 1024、ic11 = 32、ic12 = 64、ic13 = 256、ic14 = 512）
fn icns(pngs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let find = |s: u32| pngs.iter().find(|(size, _)| *size == s).map(|(_, p)| p.as_slice());
    let types: [(&[u8; 4], u32); 8] = [
        (b"ic11", 32),
        (b"ic12", 64),
        (b"ic07", 128),
        (b"ic13", 256),
        (b"ic08", 256),
        (b"ic14", 512),
        (b"ic09", 512),
        (b"ic10", 1024),
    ];
    let mut body = Vec::new();
    for (tag, size) in types {
        if let Some(png) = find(size) {
            body.extend_from_slice(tag);
            body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
            body.extend_from_slice(png);
        }
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    out
}
