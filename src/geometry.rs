//! 畫面幾何：長寬比、裁切、填滿、縮放、平移、旋轉、翻轉（L2「畫面」）。
//!
//! 全部用 mpv 的選項做，不經過軟體濾鏡，硬體解碼（GPU 上的影格）也能用：
//! - 長寬比：`video-aspect-override`（整張畫面、旋轉前的顯示比例）。「原始比例」寫回 mpv 回報的預設值：
//!   0.37 是 -1、0.40 起是 -2，0.37 的 `no` 代表「像素當成正方形」，比例會錯
//! - 裁切：`video-crop`（0.37 起有），座標是送進畫面輸出的影格、旋轉之前，所以要先把旋轉換算回去
//! - 填滿視窗：`panscan`；縮放：`video-zoom`（log2）；平移：`video-pan-x/y`（影片大小的比例）
//! - 旋轉：`video-rotate`，只用 90° 的倍數（其他角度硬體解碼時不會真的轉）
//! - 翻轉：mpv 沒有翻轉的選項，軟體濾鏡又不能處理 GPU 上的影格；用一個 GLSL 著色器在畫面空間翻轉
//!   （字幕畫在之後，不會跟著鏡像）。軟體繪圖的簡化流程不跑著色器，改用 `vf` 濾鏡（影格在記憶體裡）
//!
//! 換檔時由 mpv 的 `reset-on-next-file` 全部還原（見 `player.rs`）。

use std::path::PathBuf;

/// 長寬比、裁切的選項：（顯示名稱, 寬 / 高）
pub const ASPECTS: [(&str, f64); 5] = [
    ("16:9", 16.0 / 9.0),
    ("4:3", 4.0 / 3.0),
    ("16:10", 16.0 / 10.0),
    ("1.85:1", 1.85),
    ("2.35:1", 2.35),
];

/// 裁切的選項（不含 16:10，跟 PotPlayer 一樣）
pub const CROPS: [(&str, f64); 4] = [
    ("16:9", 16.0 / 9.0),
    ("4:3", 4.0 / 3.0),
    ("1.85:1", 1.85),
    ("2.35:1", 2.35),
];

/// 縮放每一下的量（log2；0.1 ≈ 7%）
pub const ZOOM_STEP: f64 = 0.1;
/// 平移每一下的量（影片大小的比例）
pub const PAN_STEP: f64 = 0.05;

/// 使用者對畫面的調整。全部是預設值 = 原始畫面
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Geometry {
    /// `ASPECTS` 的索引；None = 原始比例
    pub aspect: Option<usize>,
    /// `CROPS` 的索引；None = 不裁切
    pub crop: Option<usize>,
    /// 填滿視窗（放大到沒有黑邊，超出的部分裁掉）
    pub fill: bool,
    /// 縮放（log2：0 = 100%、1 = 200%）
    pub zoom: f64,
    /// 平移（影片大小的比例，正數 = 往右 / 往下）
    pub pan: [f64; 2],
    /// 使用者加的旋轉（順時針，0 / 90 / 180 / 270），加在檔案本身的旋轉上
    pub rotate: u32,
    pub hflip: bool,
    pub vflip: bool,
}

impl Geometry {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// 縮放百分比（給 OSD、選單）
    pub fn zoom_percent(&self) -> f64 {
        (2f64.powf(self.zoom) * 100.0).round()
    }

    pub fn aspect_label(&self) -> &'static str {
        self.aspect.map_or(crate::tr!("原始比例", "Original"), |i| ASPECTS[i].0)
    }

    pub fn crop_label(&self) -> &'static str {
        self.crop.map_or(crate::tr!("不裁切", "No crop"), |i| CROPS[i].0)
    }
}

/// 下一個選項（依序切換，最後回到「沒有」）
pub fn cycle(current: Option<usize>, len: usize) -> Option<usize> {
    match current {
        None => Some(0),
        Some(i) if i + 1 < len => Some(i + 1),
        Some(_) => None,
    }
}

/// 送進畫面輸出的影格：寬、高、畫面輸出還要做的旋轉（順時針度數）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub w: f64,
    pub h: f64,
    pub rotate: u32,
}

/// 要給 mpv 的 `video-aspect-override`：使用者要的是「畫面上看起來」的比例，
/// mpv 要的是旋轉之前整張影格的比例；轉了 90° / 270° 時要倒過來
pub fn aspect_override(target: f64, total_rotate: u32) -> f64 {
    if total_rotate % 180 == 90 { 1.0 / target } else { target }
}

/// 把畫面裁成 `target` 比例要從四邊各裁掉多少（畫面上看到的方向；左、右、上、下，佔整個畫面的比例）。
/// `display` = 目前畫面上整張影片的比例
pub fn crop_edges(display: f64, target: f64) -> [f64; 4] {
    if display > target {
        let side = (1.0 - target / display) / 2.0;
        [side, side, 0.0, 0.0]
    } else {
        let side = (1.0 - display / target) / 2.0;
        [0.0, 0.0, side, side]
    }
}

/// 畫面上的四邊（左、右、上、下）→ 影格上的四邊：把畫面輸出的旋轉換算回去
pub fn to_frame_edges([l, r, t, b]: [f64; 4], rotate: u32) -> [f64; 4] {
    match rotate % 360 {
        // 順時針轉 90°：影格的上邊到了畫面右邊、左邊到了上面
        90 => [t, b, r, l],
        180 => [r, l, b, t],
        270 => [b, t, l, r],
        _ => [l, r, t, b],
    }
}

/// `video-crop` 的值：`寬x高+X+Y`（影格像素）
pub fn crop_value(frame: Frame, display_edges: [f64; 4]) -> String {
    let [l, r, t, b] = to_frame_edges(display_edges, frame.rotate);
    let w = (frame.w * (1.0 - l - r)).round().max(2.0);
    let h = (frame.h * (1.0 - t - b)).round().max(2.0);
    let x = (frame.w * l).round();
    let y = (frame.h * t).round();
    format!("{w}x{h}+{x}+{y}")
}

/// 翻轉用的 GLSL 著色器（mpv 的 user shader；在旋轉之後、字幕之前執行）
pub fn flip_shader(horizontal: bool) -> &'static str {
    if horizontal {
        "//!HOOK MAINPRESUB\n//!BIND HOOKED\n//!DESC vitascope hflip\n\
         vec4 hook() { return HOOKED_tex(vec2(1.0 - HOOKED_pos.x, HOOKED_pos.y)); }\n"
    } else {
        "//!HOOK MAINPRESUB\n//!BIND HOOKED\n//!DESC vitascope vflip\n\
         vec4 hook() { return HOOKED_tex(vec2(HOOKED_pos.x, 1.0 - HOOKED_pos.y)); }\n"
    }
}

/// 翻轉著色器的檔案（放在快取資料夾，第一次用到時寫出來）
pub fn flip_shader_path(horizontal: bool) -> std::io::Result<PathBuf> {
    let dir = crate::subs::cache_dir()
        .parent()
        .map(|p| p.join("shaders"))
        .unwrap_or_else(|| std::env::temp_dir().join("vitascope-shaders"));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(if horizontal { "hflip.glsl" } else { "vflip.glsl" });
    let content = flip_shader(horizontal);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(content) {
        std::fs::write(&path, content)?;
    }
    Ok(path)
}

/// 平移後的值：限制在 ±1（影片最多移出畫面一整個寬度），四捨五入到小數兩位
pub fn pan_after(pan: f64, delta: f64) -> f64 {
    (((pan + delta) * 100.0).round() / 100.0).clamp(-1.0, 1.0)
}

/// 縮放後的值：10% 到 800%（log2 -3.3 … 3；mpv 0.37 放太大會溢位）。
/// 不四捨五入：觸控板捏合、Ctrl + 精確捲動每一幀只有很小的量，四捨五入會被吃掉（顯示時才取整數百分比）
pub fn zoom_after(zoom: f64, delta: f64) -> f64 {
    (zoom + delta).clamp(-3.3, 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn cycles_through_options_and_back_to_none() {
        assert_eq!(cycle(None, 3), Some(0));
        assert_eq!(cycle(Some(1), 3), Some(2));
        assert_eq!(cycle(Some(2), 3), None);
    }

    #[test]
    fn aspect_is_inverted_for_quarter_turns() {
        assert!(close(aspect_override(16.0 / 9.0, 0), 16.0 / 9.0));
        assert!(close(aspect_override(16.0 / 9.0, 90), 9.0 / 16.0));
        assert!(close(aspect_override(16.0 / 9.0, 180), 16.0 / 9.0));
        assert!(close(aspect_override(16.0 / 9.0, 270), 9.0 / 16.0));
    }

    #[test]
    fn crop_wide_to_narrower_cuts_the_sides() {
        // 16:9 的畫面裁成 4:3：左右各裁掉 1/8
        let e = crop_edges(16.0 / 9.0, 4.0 / 3.0);
        assert!(
            close(e[0], 0.125) && close(e[1], 0.125) && e[2] == 0.0 && e[3] == 0.0,
            "{e:?}"
        );
        // 4:3 的畫面裁成 16:9：上下各裁掉 1/8
        let e = crop_edges(4.0 / 3.0, 16.0 / 9.0);
        assert!(e[0] == 0.0 && close(e[2], 0.125) && close(e[3], 0.125), "{e:?}");
    }

    #[test]
    fn crop_value_in_frame_pixels() {
        let frame = Frame {
            w: 640.0,
            h: 360.0,
            rotate: 0,
        };
        // 16:9 裁成 4:3：480x360，置中
        assert_eq!(crop_value(frame, crop_edges(16.0 / 9.0, 4.0 / 3.0)), "480x360+80+0");
    }

    #[test]
    fn rotation_is_undone_before_cropping() {
        // 直立顯示的 640x360 影格（畫面輸出轉 90°）：畫面上是 9:16，左右各裁 0.1 → 影格的上下各裁 0.1
        let frame = Frame {
            w: 640.0,
            h: 360.0,
            rotate: 90,
        };
        assert_eq!(crop_value(frame, [0.1, 0.1, 0.0, 0.0]), "640x288+0+36");
        // 只裁畫面左邊：轉 90° 時是影格的下邊、轉 270° 時是上邊、轉 180° 時是右邊
        assert_eq!(to_frame_edges([0.2, 0.0, 0.0, 0.0], 90), [0.0, 0.0, 0.0, 0.2]);
        assert_eq!(to_frame_edges([0.2, 0.0, 0.0, 0.0], 270), [0.0, 0.0, 0.2, 0.0]);
        assert_eq!(to_frame_edges([0.2, 0.0, 0.0, 0.0], 180), [0.0, 0.2, 0.0, 0.0]);
    }

    #[test]
    fn zoom_and_pan_are_bounded_and_rounded() {
        assert!(close(zoom_after(0.0, 0.1), 0.1));
        assert!(close(zoom_after(2.95, 0.1), 3.0));
        // 慢慢捏合：每一幀很小的量也要累積起來
        let mut z = 0.0;
        for _ in 0..100 {
            z = zoom_after(z, 0.002);
        }
        assert!(close(z, 0.2), "{z}");
        assert!(close(pan_after(0.95, 0.1), 1.0));
        assert!(
            close(pan_after(0.1, 0.05 + 0.05), 0.2),
            "不會累積出 0.20000000000000004"
        );
        let g = Geometry {
            zoom: 1.0,
            ..Geometry::default()
        };
        assert_eq!(g.zoom_percent(), 200.0);
    }
}
