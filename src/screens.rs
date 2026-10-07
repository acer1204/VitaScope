//! 目前接著哪些螢幕：上次的視窗位置在已經拔掉的螢幕上時，不要把視窗開在看不到的地方。
//!
//! egui / eframe 在建立視窗前拿不到螢幕清單（eframe 只替它自己存的位置做這個檢查），所以直接問作業系統：
//! Windows 用 EnumDisplayMonitors，macOS 用 NSScreen，Linux 的 X11 用 XRandR。
//! Wayland 本來就不讓程式指定視窗位置，不用檢查。

/// 一個螢幕的範圍。Windows：扣掉工作列的工作區，實體像素；macOS：整個螢幕，點（Dock 蓋在視窗上面，
/// 選單列 AppKit 自己會避開）；X11：整個螢幕，實體像素。原點在主螢幕左上角，y 往下
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// 這個螢幕的縮放比例：實體像素 / 點（macOS 的座標已經是點，是 1）
    pub scale: f64,
}

/// 標題列至少要有這麼寬（點）落在同一個螢幕裡，才算看得到
const MIN_VISIBLE: f64 = 100.0;
/// 判斷高度用的位置：離視窗頂端這麼多點（標題列中間）
const TITLE_Y: f64 = 12.0;

/// 目前的螢幕；拿不到（Wayland、沒有 X server、不支援的平台）時回傳 None，照原本的方式還原位置
pub fn areas() -> Option<Vec<Area>> {
    let areas = platform::areas()?;
    (!areas.is_empty()).then_some(areas)
}

/// 視窗外框左上角放在 `pos`、寬 `width`（都是 egui 的點）時，標題列看得到嗎？
///
/// 存的位置是「實體像素 ÷ 當時那個螢幕的縮放比例」；還原時 eframe 也用視窗落在的那個螢幕的比例換回實體像素，
/// 所以每個螢幕用自己的比例檢查。原本看得到的位置，螢幕沒變時一定還是看得到
pub fn title_bar_visible(pos: [f32; 2], width: f32, areas: &[Area]) -> bool {
    if areas.is_empty() {
        return true;
    }
    let (x, y) = (f64::from(pos[0]), f64::from(pos[1]));
    let w = f64::from(width).max(1.0);
    let need = MIN_VISIBLE.min(w);
    areas.iter().any(|a| {
        let s = a.scale;
        let title_y = (y + TITLE_Y) * s;
        let overlap = ((x + w) * s).min(a.x + a.w) - (x * s).max(a.x);
        title_y >= a.y && title_y < a.y + a.h && overlap >= need * s
    })
}

#[cfg(windows)]
mod platform {
    use super::Area;
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
        MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
    };
    use windows_sys::core::BOOL;

    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: data 是下面傳進來的 &mut Vec<Area>，只在 EnumDisplayMonitors 執行期間使用
        let areas = unsafe { &mut *(data as *mut Vec<Area>) };
        let empty = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            rcMonitor: empty,
            rcWork: empty,
            dwFlags: 0,
        };
        // SAFETY: monitor 是系統列舉給的；info 的 cbSize 已設好
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            // SAFETY: 同上；失敗時當成 100%
            if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } != 0 {
                dpi_x = 96;
            }
            let r = info.rcWork;
            areas.push(Area {
                x: f64::from(r.left),
                y: f64::from(r.top),
                w: f64::from(r.right - r.left),
                h: f64::from(r.bottom - r.top),
                scale: f64::from(dpi_x) / 96.0,
            });
        }
        1
    }

    pub fn areas() -> Option<Vec<Area>> {
        let mut areas: Vec<Area> = Vec::new();
        // 座標要是實體像素：暫時把這個執行緒設成 per-monitor DPI aware，查完還原（不改整個程式的設定：
        // winit 之後會自己設）。V2 要 Windows 10 1703 起，1607 退回 V1（跟 winit 一樣）；都不行就不檢查
        // SAFETY: 都是單純的系統呼叫；callback 只在 EnumDisplayMonitors 裡被呼叫
        unsafe {
            let mut previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if previous.is_null() {
                previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE);
            }
            if previous.is_null() {
                return None;
            }
            let ok = EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(each),
                &mut areas as *mut Vec<Area> as LPARAM,
            );
            SetThreadDpiAwarenessContext(previous);
            (ok != 0).then_some(areas)
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Area;
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    pub fn areas() -> Option<Vec<Area>> {
        // NSScreen 只能在主執行緒用；建立視窗之前就是主執行緒
        let mtm = MainThreadMarker::new()?;
        let screens = NSScreen::screens(mtm);
        // 第一個是有選單列的主螢幕。AppKit 的原點在主螢幕左下角、y 往上；winit 的原點在左上角、y 往下
        let main_height = screens.iter().next()?.frame().size.height;
        Some(
            screens
                .iter()
                .map(|screen| {
                    let r = screen.frame();
                    Area {
                        x: r.origin.x,
                        y: main_height - (r.origin.y + r.size.height),
                        w: r.size.width,
                        h: r.size.height,
                        scale: 1.0,
                    }
                })
                .collect(),
        )
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::Area;
    use std::ffi::CStr;
    use x11_dl::{xlib::Xlib, xrandr::Xrandr};

    pub fn areas() -> Option<Vec<Area>> {
        // Wayland（winit 優先用它）不讓程式指定視窗位置，不用檢查
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            return None;
        }
        let xlib = Xlib::open().ok()?;
        let xrandr = Xrandr::open().ok()?;
        // SAFETY: 開一條自己的連線，查完關掉；XRRGetMonitors 回傳的陣列用 XRRFreeMonitors 釋放
        unsafe {
            let display = (xlib.XOpenDisplay)(std::ptr::null());
            if display.is_null() {
                return None;
            }
            let areas = query(&xlib, &xrandr, display);
            (xlib.XCloseDisplay)(display);
            areas
        }
    }

    unsafe fn query(xlib: &Xlib, xrandr: &Xrandr, display: *mut x11_dl::xlib::Display) -> Option<Vec<Area>> {
        // XRRGetMonitors 要 RandR 1.5（Xorg 1.18 起）；更舊的 server 會回錯誤，Xlib 預設的處理是直接結束程式
        let (mut major, mut minor) = (0, 0);
        // SAFETY: display 是開好的連線
        if unsafe { (xrandr.XRRQueryVersion)(display, &mut major, &mut minor) } == 0 || (major, minor) < (1, 5) {
            return None;
        }
        // 縮放比例跟 winit 的算法一樣：WINIT_X11_SCALE_FACTOR，否則 Xft.dpi，否則依螢幕的實際尺寸
        let fixed = match std::env::var("WINIT_X11_SCALE_FACTOR") {
            Ok(v) if v.eq_ignore_ascii_case("randr") => None,
            Ok(v) if !v.is_empty() => Some(v.parse::<f64>().ok().filter(|s| s.is_finite() && *s > 0.0)?),
            _ => unsafe { xft_dpi(xlib, display) }.map(|dpi| dpi / 96.0),
        };
        // SAFETY: display 是開好的連線
        let root = unsafe { (xlib.XDefaultRootWindow)(display) };
        let mut count = 0;
        let monitors = unsafe { (xrandr.XRRGetMonitors)(display, root, 1, &mut count) };
        if monitors.is_null() {
            return None;
        }
        // SAFETY: XRRGetMonitors 回傳 count 個元素
        let list = unsafe { std::slice::from_raw_parts(monitors, usize::try_from(count).unwrap_or(0)) };
        let areas = list
            .iter()
            .map(|m| Area {
                x: f64::from(m.x),
                y: f64::from(m.y),
                w: f64::from(m.width),
                h: f64::from(m.height),
                scale: fixed.unwrap_or_else(|| super::dpi_factor(m.width, m.height, m.mwidth, m.mheight)),
            })
            .collect();
        unsafe { (xrandr.XRRFreeMonitors)(monitors) };
        Some(areas)
    }

    /// X 資源資料庫裡的 Xft.dpi（GNOME、KDE 都會設）
    unsafe fn xft_dpi(xlib: &Xlib, display: *mut x11_dl::xlib::Display) -> Option<f64> {
        // SAFETY: 回傳的字串屬於 display，不用釋放
        let db = unsafe { (xlib.XResourceManagerString)(display) };
        if db.is_null() {
            return None;
        }
        let db = unsafe { CStr::from_ptr(db) }.to_string_lossy();
        super::xft_dpi_from(&db)
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    pub fn areas() -> Option<Vec<super::Area>> {
        None
    }
}

/// X 資源資料庫字串裡的 `Xft.dpi: 144`
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn xft_dpi_from(db: &str) -> Option<f64> {
    db.lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim() == "Xft.dpi")
        .and_then(|(_, value)| value.trim().parse::<f64>().ok())
        .filter(|dpi| dpi.is_finite() && *dpi > 0.0)
}

/// 沒有 Xft.dpi 時 winit 依螢幕的實際尺寸算縮放比例（winit 的 calc_dpi_factor）：每 1/12 一級、至少 1
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn dpi_factor(width_px: i32, height_px: i32, width_mm: i32, height_mm: i32) -> f64 {
    if width_mm <= 0 || height_mm <= 0 {
        return 1.0;
    }
    let ppmm = ((f64::from(width_px) * f64::from(height_px)) / (f64::from(width_mm) * f64::from(height_mm))).sqrt();
    let factor = ((ppmm * (12.0 * 25.4 / 96.0)).round() / 12.0).max(1.0);
    if factor <= 20.0 { factor } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: f64, y: f64, w: f64, h: f64, scale: f64) -> Area {
        Area { x, y, w, h, scale }
    }

    #[test]
    fn window_on_a_monitor_that_was_unplugged_is_not_visible() {
        // 原本：主螢幕 1920×1080 ＋ 右邊一台 2560×1440；視窗（寬 960）在右邊那台
        let both = [
            area(0.0, 0.0, 1920.0, 1040.0, 1.0),
            area(1920.0, 0.0, 2560.0, 1400.0, 1.0),
        ];
        assert!(title_bar_visible([2400.0, 300.0], 960.0, &both));
        // 拔掉右邊那台之後
        let one = [both[0]];
        assert!(!title_bar_visible([2400.0, 300.0], 960.0, &one));
        // 只露出一小段標題列也不算
        assert!(!title_bar_visible([1900.0, 300.0], 960.0, &one));
        assert!(title_bar_visible([1500.0, 300.0], 960.0, &one));
        // 往左超出螢幕，但大半個標題列還在
        assert!(title_bar_visible([-200.0, 300.0], 960.0, &one));
        assert!(!title_bar_visible([-900.0, 300.0], 960.0, &one));
        // 標題列在螢幕上緣之外
        assert!(!title_bar_visible([100.0, -300.0], 960.0, &one));
    }

    #[test]
    fn monitors_left_of_or_above_the_primary_have_negative_coordinates() {
        let areas = [
            area(0.0, 0.0, 1920.0, 1040.0, 1.0),
            area(-2560.0, -200.0, 2560.0, 1400.0, 1.0),
        ];
        assert!(title_bar_visible([-2000.0, -100.0], 960.0, &areas));
        assert!(!title_bar_visible([-3600.0, 100.0], 960.0, &areas));
        // 跨在兩台之間：標題列有一段在左邊那台
        assert!(title_bar_visible([-500.0, -100.0], 960.0, &areas));
    }

    #[test]
    fn each_monitor_is_checked_with_its_own_scale() {
        // 筆電 4K 150% 是主螢幕，右邊接一台 1920×1080 100%：外接螢幕上的位置（存的是點 = 像素 ÷ 1）要留著
        let areas = [
            area(0.0, 0.0, 3840.0, 2100.0, 1.5),
            area(3840.0, 0.0, 1920.0, 1040.0, 1.0),
        ];
        assert!(title_bar_visible([4200.0, 200.0], 960.0, &areas));
        assert!(title_bar_visible([2400.0, 1300.0], 960.0, &areas));
        // 拔掉外接螢幕後，同一個位置在 150% 的主螢幕上換算成 6300 像素，看不到
        assert!(!title_bar_visible([4200.0, 200.0], 960.0, &areas[..1]));
        // 200% 的主螢幕＋左邊 100% 的一台
        let areas = [
            area(-1920.0, 0.0, 1920.0, 1040.0, 1.0),
            area(0.0, 0.0, 3840.0, 2100.0, 2.0),
        ];
        assert!(title_bar_visible([1800.0, 900.0], 960.0, &areas));
        assert!(!title_bar_visible([1950.0, 900.0], 960.0, &areas));
        assert!(title_bar_visible([-1500.0, 100.0], 960.0, &areas));
    }

    #[test]
    fn narrow_windows_need_less_overlap() {
        let one = [area(0.0, 0.0, 1920.0, 1040.0, 1.0)];
        assert!(title_bar_visible([1880.0, 300.0], 40.0, &one));
        assert!(!title_bar_visible([1890.0, 300.0], 40.0, &one));
    }

    #[test]
    fn without_monitor_information_the_position_is_kept() {
        assert!(title_bar_visible([99999.0, 99999.0], 960.0, &[]));
    }

    #[test]
    fn x11_scale_factor_like_winit() {
        assert_eq!(
            xft_dpi_from("Xft.antialias:\t1\nXft.dpi:\t144\nXcursor.size: 24\n"),
            Some(144.0)
        );
        assert_eq!(xft_dpi_from("Xcursor.size: 24\n"), None);
        assert_eq!(xft_dpi_from("Xft.dpi: abc\n"), None);
        // 27 吋 4K（597×336 mm）約 163 DPI → 20/12；24 吋 1080p（531×299 mm）→ 1；尺寸不明 → 1
        assert!((dpi_factor(3840, 2160, 597, 336) - 20.0 / 12.0).abs() < 1e-9);
        assert_eq!(dpi_factor(1920, 1080, 531, 299), 1.0);
        assert_eq!(dpi_factor(1920, 1080, 0, 0), 1.0);
    }
}
