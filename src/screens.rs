//! 螢幕的資訊，直接問作業系統：
//! - 目前接著哪些螢幕：上次的視窗位置在已經拔掉的螢幕上時，不要把視窗開在看不到的地方。
//!   egui / eframe 在建立視窗前拿不到螢幕清單（eframe 只替它自己存的位置做這個檢查）：
//!   Windows 用 EnumDisplayMonitors，macOS 用 NSScreen，Linux 的 X11 用 XRandR。
//!   Wayland 本來就不讓程式指定視窗位置，不用檢查。
//! - 視窗所在螢幕的精確更新率（流暢播放用，[`refresh_rate`]）：winit 在 Windows 用 EnumDisplaySettings
//!   的整數 Hz（×1000 當成 mHz），119.88 Hz 會變成 119 或 120

use raw_window_handle::RawWindowHandle;

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

// ───────────── 螢幕更新率 ─────────────

/// 合理的更新率範圍：超出的值當成讀錯（驅動回報 0、1 Hz 之類的）
const MIN_HZ: f64 = 20.0;
const MAX_HZ: f64 = 500.0;

/// 視窗所在螢幕的實際更新率
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Refresh {
    pub hz: f64,
    pub source: RefreshSource,
}

/// 更新率是從哪裡查到的（媒體資訊面板上顯示，回報問題時看得出準不準）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshSource {
    /// Windows QueryDisplayConfig：顯示卡實際送出的訊號，精確的分數（例如 120000/1001）
    DisplayConfig,
    /// Windows EnumDisplaySettings：只有整數，59、119 之類的當成 /1.001 的速率
    DevMode,
    /// macOS CVDisplayLink 的標稱更新週期
    DisplayLink,
    /// macOS CGDisplayMode
    DisplayMode,
    /// macOS NSScreen.maximumFramesPerSecond（整數）
    ScreenMaxFps,
    /// X11 XRandR 的 modeline
    Randr,
}

impl RefreshSource {
    /// 面板上的簡短說明
    pub fn label(self) -> &'static str {
        match self {
            Self::DisplayConfig => "QueryDisplayConfig",
            Self::DevMode => crate::tr!("EnumDisplaySettings，整數", "EnumDisplaySettings, whole number"),
            Self::DisplayLink => "CVDisplayLink",
            Self::DisplayMode => "CGDisplayMode",
            Self::ScreenMaxFps => crate::tr!("NSScreen 最高更新率", "NSScreen maximum rate"),
            Self::Randr => "XRandR",
        }
    }
}

/// 視窗所在螢幕的更新率；查不到（Wayland、視窗還沒顯示、驅動不給）時是 None。
/// X11 每次呼叫都開一條新連線，常常查的話用 [`X11Probe`]
pub fn refresh_rate(w: RawWindowHandle) -> Option<Refresh> {
    match w {
        #[cfg(windows)]
        RawWindowHandle::Win32(h) => rate::refresh_rate(h.hwnd.get() as _),
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(h) => rate::refresh_rate(h.ns_view),
        #[cfg(all(unix, not(target_os = "macos")))]
        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => X11Probe::default().refresh_rate(w),
        _ => None,
    }
}

/// 視窗在哪個螢幕上（只用來比較有沒有換螢幕；Windows 是 HMONITOR、macOS 是 CGDirectDisplayID、X11 是 CRTC）
pub fn monitor_key(w: RawWindowHandle) -> Option<u64> {
    match w {
        #[cfg(windows)]
        RawWindowHandle::Win32(h) => rate::monitor_key(h.hwnd.get() as _),
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(h) => rate::monitor_key(h.ns_view),
        #[cfg(all(unix, not(target_os = "macos")))]
        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => X11Probe::default().monitor_key(w),
        _ => None,
    }
}

/// 遠端桌面連線中：RDP 回報的更新率是假的，swap 也不等垂直同步，不能依螢幕同步
pub fn remote_session() -> bool {
    #[cfg(windows)]
    {
        rate::remote_session()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub use rate::X11Probe;

/// 在合理範圍內的更新率
fn plausible(hz: f64) -> Option<f64> {
    (hz.is_finite() && (MIN_HZ..=MAX_HZ).contains(&hz)).then_some(hz)
}

/// 分數形式的更新率（Windows 的 DISPLAYCONFIG_RATIONAL）
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn rational_hz(num: u32, den: u32) -> Option<f64> {
    if den == 0 {
        return None;
    }
    plausible(f64::from(num) / f64::from(den))
}

/// EnumDisplaySettings 的整數更新率：0、1 是「硬體預設」；59、119 之類的其實是 60/1.001、120/1.001
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn devmode_hz(n: u32) -> Option<f64> {
    match n {
        0 | 1 => None,
        23 | 29 | 47 | 59 | 71 | 119 | 143 | 239 => plausible(f64::from(n + 1) / 1.001),
        _ => plausible(f64::from(n)),
    }
}

/// XRandR modeline 的更新率：像素時脈 ÷（水平總長 × 垂直總長）。
/// 交錯式（RR_Interlace）一個畫面是兩個場，×2；倍掃描（RR_DoubleScan）每條線掃兩次，÷2
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub(crate) fn modeline_hz(dot: u64, ht: u32, vt: u32, flags: u64) -> Option<f64> {
    const RR_INTERLACE: u64 = 0x10;
    const RR_DOUBLE_SCAN: u64 = 0x20;
    if ht == 0 || vt == 0 {
        return None;
    }
    let mut hz = dot as f64 / (f64::from(ht) * f64::from(vt));
    if flags & RR_INTERLACE != 0 {
        hz *= 2.0;
    }
    if flags & RR_DOUBLE_SCAN != 0 {
        hz /= 2.0;
    }
    plausible(hz)
}

/// CVTime（一次更新的長度 = value / scale 秒）→ 更新率。flags 的第 0 位元（kCVTimeIsIndefinite）= 不知道
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn cvtime_hz(value: i64, scale: i32, flags: i32) -> Option<f64> {
    if flags & 1 != 0 || value <= 0 || scale <= 0 {
        return None;
    }
    plausible(f64::from(scale) / value as f64)
}

/// 同一個桌面複製到好幾個螢幕（clone）時，各螢幕查到的更新率
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Clones {
    /// 都一樣（或只有一個）
    Rate(f64),
    /// 一個都沒查到：改用其他方法
    Nothing,
    /// 不一樣：不知道 swap 跟哪個螢幕同步，當成查不到
    Disagree,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn clone_rate(rates: &[f64]) -> Clones {
    match rates.split_first() {
        None => Clones::Nothing,
        Some((first, rest)) if rest.iter().all(|r| (r - first).abs() < 0.005) => Clones::Rate(*first),
        Some(_) => Clones::Disagree,
    }
}

#[cfg(windows)]
mod rate {
    use super::{Clones, Refresh, RefreshSource, clone_rate, devmode_hz, rational_hz};
    use windows_sys::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
        DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
        GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST,
        MONITORINFO, MONITORINFOEXW, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_REMOTESESSION};

    fn monitor(hwnd: HWND) -> Option<HMONITOR> {
        // SAFETY: 單純的系統呼叫；視窗不存在時回傳最近的螢幕
        let m = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        (!m.is_null()).then_some(m)
    }

    pub fn monitor_key(hwnd: HWND) -> Option<u64> {
        monitor(hwnd).map(|m| m as usize as u64)
    }

    pub fn refresh_rate(hwnd: HWND) -> Option<Refresh> {
        monitor_refresh(monitor(hwnd)?)
    }

    pub fn remote_session() -> bool {
        // SAFETY: 單純的系統呼叫
        unsafe { GetSystemMetrics(SM_REMOTESESSION) != 0 }
    }

    /// 一個螢幕用各種方法查到的值（除錯記錄、實機確認用）
    #[derive(Debug, Default)]
    pub struct Methods {
        /// GDI 的裝置名稱（\\.\DISPLAY1）
        pub device: String,
        /// 這個桌面的每個輸出（clone 時有好幾個）：(targetVideoSignalInfo.vSyncFreq, targetInfo.refreshRate)
        pub paths: Vec<(Option<f64>, Option<f64>)>,
        /// EnumDisplaySettings 的 dmDisplayFrequency
        pub devmode: Option<u32>,
    }

    impl Methods {
        fn pick(&self) -> Option<Refresh> {
            // 優先用實際送出的訊號（vSyncFreq），沒有才用路徑上設定的更新率
            let rates: Vec<f64> = self.paths.iter().filter_map(|(v, t)| v.or(*t)).collect();
            match clone_rate(&rates) {
                Clones::Rate(hz) => Some(Refresh {
                    hz,
                    source: RefreshSource::DisplayConfig,
                }),
                Clones::Disagree => None,
                Clones::Nothing => self.devmode.and_then(devmode_hz).map(|hz| Refresh {
                    hz,
                    source: RefreshSource::DevMode,
                }),
            }
        }
    }

    pub fn monitor_refresh(hmon: HMONITOR) -> Option<Refresh> {
        let m = methods(hmon)?;
        let picked = m.pick();
        if crate::pacing::debug() {
            eprintln!("[vitascope] 螢幕 {}：{:?} → {picked:?}", m.device, m);
        }
        picked
    }

    /// 螢幕的 GDI 裝置名稱（以 0 結尾的 UTF-16）
    fn device_name(hmon: HMONITOR) -> Option<[u16; 32]> {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: MONITORINFOEXW 的開頭就是 MONITORINFO，cbSize 告訴系統實際的大小
        let ok = unsafe { GetMonitorInfoW(hmon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) };
        (ok != 0).then_some(info.szDevice)
    }

    /// 以 0 結尾的 UTF-16 字串相同
    fn same_name(a: &[u16], b: &[u16]) -> bool {
        let trim = |s: &[u16]| s.iter().position(|c| *c == 0).map_or(s.len(), |n| n);
        a[..trim(a)] == b[..trim(b)]
    }

    pub fn methods(hmon: HMONITOR) -> Option<Methods> {
        let device = device_name(hmon)?;
        let mut m = Methods {
            device: String::from_utf16_lossy(&device[..device.iter().position(|c| *c == 0).unwrap_or(32)]),
            ..Default::default()
        };
        if let Some((paths, modes)) = active_paths() {
            for path in &paths {
                let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
                source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
                source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
                source.header.adapterId = path.sourceInfo.adapterId;
                source.header.id = path.sourceInfo.id;
                // SAFETY: header 的 type、size 跟實際的結構一致
                if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0
                    || !same_name(&source.viewGdiDeviceName, &device)
                {
                    continue;
                }
                // 沒有 QDC_VIRTUAL_MODE_AWARE 時 modeInfoIdx 就是整個 u32（無效是 0xFFFFFFFF）
                // SAFETY: 讀 union 的 u32 欄位，任何位元組合都合法
                let idx = unsafe { path.targetInfo.Anonymous.modeInfoIdx } as usize;
                let vsync = modes
                    .get(idx)
                    .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET)
                    .and_then(|mode| {
                        // SAFETY: infoType 是 TARGET，union 裡放的是 targetMode
                        let f = unsafe { mode.Anonymous.targetMode.targetVideoSignalInfo.vSyncFreq };
                        rational_hz(f.Numerator, f.Denominator)
                    });
                let r = path.targetInfo.refreshRate;
                m.paths.push((vsync, rational_hz(r.Numerator, r.Denominator)));
            }
        }
        let mut dm = DEVMODEW {
            dmSize: size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: device 以 0 結尾；dmSize 已設好
        if unsafe { EnumDisplaySettingsW(device.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm) } != 0 {
            m.devmode = Some(dm.dmDisplayFrequency);
        }
        Some(m)
    }

    /// 目前使用中的輸出路徑與模式。查兩次之間螢幕設定變了會回 ERROR_INSUFFICIENT_BUFFER，重查（最多 3 次）。
    /// 不加 QDC_VIRTUAL_MODE_AWARE：加了 modeInfoIdx 的格式不一樣
    fn active_paths() -> Option<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
        for _ in 0..3 {
            let (mut np, mut nm) = (0u32, 0u32);
            // SAFETY: 單純的系統呼叫
            if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) } != ERROR_SUCCESS {
                return None;
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            // SAFETY: 兩個陣列的長度就是傳進去的數量；沒有 QDC_DATABASE_CURRENT 時 topology 要是 null
            let r = unsafe {
                QueryDisplayConfig(
                    QDC_ONLY_ACTIVE_PATHS,
                    &mut np,
                    paths.as_mut_ptr(),
                    &mut nm,
                    modes.as_mut_ptr(),
                    std::ptr::null_mut(),
                )
            };
            match r {
                ERROR_SUCCESS => {
                    paths.truncate(np as usize);
                    modes.truncate(nm as usize);
                    return Some((paths, modes));
                }
                ERROR_INSUFFICIENT_BUFFER => continue,
                _ => return None,
            }
        }
        None
    }

    /// 所有螢幕（實機確認用）
    #[cfg(test)]
    pub fn all_monitors() -> Vec<HMONITOR> {
        use windows_sys::Win32::Foundation::{LPARAM, RECT};
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC};
        unsafe extern "system" fn each(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> windows_sys::core::BOOL {
            // SAFETY: data 是下面傳進來的 &mut Vec，只在 EnumDisplayMonitors 執行期間使用
            unsafe { &mut *(data as *mut Vec<HMONITOR>) }.push(m);
            1
        }
        let mut list: Vec<HMONITOR> = Vec::new();
        // SAFETY: callback 只在 EnumDisplayMonitors 裡被呼叫
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(each),
                &mut list as *mut Vec<HMONITOR> as LPARAM,
            )
        };
        list
    }

    #[cfg(test)]
    mod tests {
        use super::{Methods, Refresh, RefreshSource};

        fn pick(paths: &[(Option<f64>, Option<f64>)], devmode: Option<u32>) -> Option<Refresh> {
            Methods {
                device: r"\\.\DISPLAY1".to_owned(),
                paths: paths.to_vec(),
                devmode,
            }
            .pick()
        }

        fn display_config(hz: f64) -> Option<Refresh> {
            Some(Refresh {
                hz,
                source: RefreshSource::DisplayConfig,
            })
        }

        #[test]
        fn picks_the_signal_rate_first() {
            // 實際送出的訊號（vSyncFreq）優先於路徑上設定的更新率
            assert_eq!(pick(&[(Some(119.88), Some(120.0))], Some(120)), display_config(119.88));
            // 沒有訊號的值才用路徑上的
            assert_eq!(pick(&[(None, Some(59.94))], Some(59)), display_config(59.94));
            // DisplayConfig 一個都沒查到：EnumDisplaySettings 的整數
            assert_eq!(
                pick(&[], Some(119)),
                Some(Refresh {
                    hz: 120.0 / 1.001,
                    source: RefreshSource::DevMode,
                })
            );
            assert_eq!(
                pick(&[(None, None)], Some(60)).map(|r| r.source),
                Some(RefreshSource::DevMode)
            );
            assert_eq!(pick(&[], Some(0)), None);
            assert_eq!(pick(&[], None), None);
        }

        #[test]
        fn cloned_outputs_that_disagree_give_nothing() {
            // 複製到兩個更新率不同的螢幕：不知道 swap 跟哪個同步，也不改用 EnumDisplaySettings
            assert_eq!(pick(&[(Some(59.94), None), (Some(60.0), None)], Some(60)), None);
            assert_eq!(
                pick(&[(Some(120.0), None), (None, Some(120.0))], Some(120)),
                display_config(120.0)
            );
        }
    }
}

#[cfg(target_os = "macos")]
mod rate {
    use super::{Refresh, RefreshSource, cvtime_hz, plausible};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::NSObjectProtocol;
    use objc2_app_kit::{NSScreen, NSView};
    use objc2_foundation::{NSNumber, NSString};
    use std::ffi::c_void;
    use std::ptr::NonNull;

    /// CoreVideo 的 CVTime
    #[repr(C)]
    struct CVTime {
        time_value: i64,
        time_scale: i32,
        flags: i32,
    }

    // CVDisplayLink 在 macOS 15 標成過時，但還能用，而且是唯一查得到精確分數的方法
    #[link(name = "CoreVideo", kind = "framework")]
    unsafe extern "C" {
        fn CVDisplayLinkCreateWithCGDisplay(display: u32, link: *mut *mut c_void) -> i32;
        fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: *mut c_void) -> CVTime;
        fn CVDisplayLinkRelease(link: *mut c_void);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayCopyDisplayMode(display: u32) -> *mut c_void;
        fn CGDisplayModeGetRefreshRate(mode: *mut c_void) -> f64;
        fn CGDisplayModeRelease(mode: *mut c_void);
    }

    /// 視窗所在的螢幕與它的 CGDirectDisplayID。視窗還沒顯示時 screen 是 nil（回傳 None，之後再查）
    fn screen_of(view: NonNull<c_void>) -> Option<(u32, Retained<NSScreen>)> {
        // AppKit 只能在主執行緒用；logic() 在主執行緒
        MainThreadMarker::new()?;
        // SAFETY: winit 給的 ns_view 是 NSView，主視窗在整個程式執行期間都存在
        let view: &NSView = unsafe { view.cast::<NSView>().as_ref() };
        let screen = view.window()?.screen()?;
        let number = screen
            .deviceDescription()
            .objectForKey(&NSString::from_str("NSScreenNumber"))?;
        let id = number.downcast::<NSNumber>().ok()?.unsignedIntValue();
        Some((id, screen))
    }

    pub fn monitor_key(view: NonNull<c_void>) -> Option<u64> {
        screen_of(view).map(|(id, _)| u64::from(id))
    }

    pub fn refresh_rate(view: NonNull<c_void>) -> Option<Refresh> {
        let (id, screen) = screen_of(view)?;
        let found = |hz: Option<f64>, source| hz.map(|hz| Refresh { hz, source });
        // 1. CVDisplayLink 的標稱週期（例如 1001/60000 秒）
        let mut link = std::ptr::null_mut();
        // SAFETY: link 是輸出參數；建立成功才釋放
        if unsafe { CVDisplayLinkCreateWithCGDisplay(id, &mut link) } == 0 && !link.is_null() {
            let t = unsafe { CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link) };
            unsafe { CVDisplayLinkRelease(link) };
            if let Some(r) = found(
                cvtime_hz(t.time_value, t.time_scale, t.flags),
                RefreshSource::DisplayLink,
            ) {
                return Some(r);
            }
        }
        // 2. 顯示模式的更新率；內建螢幕常常是 0（不知道），換下一個方法
        // SAFETY: Copy 回傳的模式要 Release；null = 查不到
        let mode = unsafe { CGDisplayCopyDisplayMode(id) };
        if !mode.is_null() {
            let hz = unsafe { CGDisplayModeGetRefreshRate(mode) };
            unsafe { CGDisplayModeRelease(mode) };
            if let Some(r) = found(plausible(hz), RefreshSource::DisplayMode) {
                return Some(r);
            }
        }
        // 3. NSScreen.maximumFramesPerSecond：macOS 12 起才有（最低支援 macOS 11，先確認有這個方法）
        if screen.respondsToSelector(objc2::sel!(maximumFramesPerSecond)) {
            return found(
                plausible(screen.maximumFramesPerSecond() as f64),
                RefreshSource::ScreenMaxFps,
            );
        }
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod rate {
    use super::{Refresh, RefreshSource, modeline_hz};
    use raw_window_handle::RawWindowHandle;
    use std::cell::OnceCell;
    use std::ffi::c_ulong;
    use x11_dl::xlib::{self, Xlib};
    use x11_dl::xrandr::Xrandr;

    /// 自己的 X 連線（不跟 winit 共用：winit 的連線在別的地方有自己的事件處理）
    struct Conn {
        xlib: Xlib,
        xrandr: Xrandr,
        display: *mut xlib::Display,
    }

    impl Conn {
        fn open() -> Option<Self> {
            // 執行時才載入（跟 winit 一樣），沒裝 libXrandr 的 Wayland 環境也開得起來
            let xlib = Xlib::open().ok()?;
            let xrandr = Xrandr::open().ok()?;
            // SAFETY: 開一條新連線；Drop 時關掉
            let display = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
            if display.is_null() {
                return None;
            }
            let conn = Self { xlib, xrandr, display };
            // XRRGetScreenResourcesCurrent 要 RandR 1.3；更舊的 server 會回錯誤，Xlib 預設的處理是直接結束程式，
            // 所以先確認版本，之後才呼叫其他 RandR 函式
            let (mut major, mut minor) = (0, 0);
            // SAFETY: display 是開好的連線
            let ok = unsafe { (conn.xrandr.XRRQueryVersion)(display, &mut major, &mut minor) } != 0;
            (ok && (major, minor) >= (1, 3)).then_some(conn)
        }

        /// 視窗所在（重疊面積最大）的 CRTC 與它的更新率
        // c_ulong 在 64 位元 Linux 是 u64、32 位元是 u32：轉型在 64 位元上看起來多餘
        #[allow(clippy::unnecessary_cast)]
        fn query(&self, window: c_ulong) -> Option<(u64, Option<f64>)> {
            let (xlib, xrandr, d) = (&self.xlib, &self.xrandr, self.display);
            // SAFETY: 都是單純的查詢；XRRGetScreenResourcesCurrent / XRRGetCrtcInfo 的結果用完就釋放
            unsafe {
                let mut attrs: xlib::XWindowAttributes = std::mem::zeroed();
                if (xlib.XGetWindowAttributes)(d, window, &mut attrs) == 0 {
                    return None;
                }
                // 視窗左上角在根視窗（整個桌面）上的位置
                let (mut x, mut y, mut child) = (0, 0, 0);
                if (xlib.XTranslateCoordinates)(d, window, attrs.root, 0, 0, &mut x, &mut y, &mut child) == 0 {
                    return None;
                }
                let res = (xrandr.XRRGetScreenResourcesCurrent)(d, attrs.root);
                if res.is_null() {
                    return None;
                }
                let r = &*res;
                let mut best: Option<(i64, c_ulong, c_ulong)> = None;
                for &crtc in slice(r.crtcs.cast_const(), r.ncrtc) {
                    let info = (xrandr.XRRGetCrtcInfo)(d, res, crtc);
                    if info.is_null() {
                        continue;
                    }
                    let c = &*info;
                    // mode 0 = 這個 CRTC 沒在用
                    if c.mode != 0 {
                        let area = overlap(
                            (x, y, attrs.width, attrs.height),
                            (c.x, c.y, c.width as i32, c.height as i32),
                        );
                        if area > 0 && best.is_none_or(|(a, ..)| area > a) {
                            best = Some((area, crtc, c.mode));
                        }
                    }
                    (xrandr.XRRFreeCrtcInfo)(info);
                }
                let found = best.map(|(_, crtc, mode)| {
                    let hz = slice(r.modes.cast_const(), r.nmode)
                        .iter()
                        .find(|m| m.id == mode)
                        .and_then(|m| modeline_hz(m.dotClock as u64, m.hTotal, m.vTotal, m.modeFlags as u64));
                    (crtc as u64, hz)
                });
                (xrandr.XRRFreeScreenResources)(res);
                found
            }
        }
    }

    impl Drop for Conn {
        fn drop(&mut self) {
            // SAFETY: open() 開的連線，只關一次
            unsafe { (self.xlib.XCloseDisplay)(self.display) };
        }
    }

    /// Xlib 回傳的陣列（指標 + 個數）
    ///
    /// # Safety
    /// `p` 指向至少 `n` 個元素，而且在回傳的 slice 用完之前不能釋放
    unsafe fn slice<'a, T>(p: *const T, n: std::ffi::c_int) -> &'a [T] {
        match usize::try_from(n) {
            // SAFETY: 見上面
            Ok(n) if n > 0 && !p.is_null() => unsafe { std::slice::from_raw_parts(p, n) },
            _ => &[],
        }
    }

    /// 兩個矩形（x, y, 寬, 高）重疊的面積
    fn overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> i64 {
        let w = i64::from((a.0 + a.2).min(b.0 + b.2)) - i64::from(a.0.max(b.0));
        let h = i64::from((a.1 + a.3).min(b.1 + b.3)) - i64::from(a.1.max(b.1));
        w.max(0) * h.max(0)
    }

    /// X11（含 XWayland）查視窗所在螢幕的更新率。連線第一次用到才開，留著重複用，Drop 時關掉。
    /// XWayland 的 RandR 模式是 Wayland 合成器給的近似值
    #[derive(Default)]
    pub struct X11Probe {
        conn: OnceCell<Option<Conn>>,
    }

    impl X11Probe {
        fn query(&self, w: RawWindowHandle) -> Option<(u64, Option<f64>)> {
            let window = match w {
                RawWindowHandle::Xlib(h) => h.window,
                RawWindowHandle::Xcb(h) => c_ulong::from(h.window.get()),
                _ => return None,
            };
            self.conn.get_or_init(Conn::open).as_ref()?.query(window)
        }

        pub fn refresh_rate(&self, w: RawWindowHandle) -> Option<Refresh> {
            let hz = self.query(w)?.1?;
            Some(Refresh {
                hz,
                source: RefreshSource::Randr,
            })
        }

        /// 視窗所在的 CRTC
        pub fn monitor_key(&self, w: RawWindowHandle) -> Option<u64> {
            self.query(w).map(|(crtc, _)| crtc)
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn overlap_area() {
            assert_eq!(super::overlap((0, 0, 100, 100), (50, 50, 100, 100)), 2500);
            assert_eq!(super::overlap((0, 0, 100, 100), (100, 0, 100, 100)), 0);
            assert_eq!(super::overlap((-50, 0, 100, 100), (0, 0, 1920, 1080)), 5000);
        }
    }
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

    fn near(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-3)
    }

    #[test]
    fn rational_refresh_rates() {
        assert!(near(rational_hz(120_000, 1001), 119.880));
        assert!(near(rational_hz(60, 1), 60.0));
        assert!(near(rational_hz(144_000, 1000), 144.0));
        assert_eq!(rational_hz(120, 0), None, "分母 0");
        assert_eq!(rational_hz(0, 1), None);
        // 超出 20–500 Hz：讀錯的值
        assert_eq!(rational_hz(1, 1), None);
        assert_eq!(rational_hz(1000, 1), None);
        assert!(near(rational_hz(20, 1), 20.0));
        assert!(near(rational_hz(500, 1), 500.0));
    }

    #[test]
    fn devmode_whole_numbers() {
        assert_eq!(devmode_hz(0), None, "硬體預設");
        assert_eq!(devmode_hz(1), None, "硬體預設");
        for (n, hz) in [
            (23, 23.976),
            (29, 29.970),
            (47, 47.952),
            (59, 59.940),
            (71, 71.928),
            (119, 119.880),
            (143, 143.856),
            (239, 239.760),
        ] {
            assert!(near(devmode_hz(n), hz), "{n} → {:?}", devmode_hz(n));
        }
        assert_eq!(devmode_hz(60), Some(60.0));
        assert_eq!(devmode_hz(120), Some(120.0));
        assert_eq!(devmode_hz(75), Some(75.0));
        assert_eq!(devmode_hz(5), None);
    }

    #[test]
    fn x11_modelines() {
        // 1080p59.94（CEA-861）
        assert!(near(modeline_hz(148_351_648, 2200, 1125, 0), 59.940));
        assert!(near(modeline_hz(148_500_000, 2200, 1125, 0), 60.0));
        // 1080i：一個畫面兩個場，場率 ×2
        assert!(near(modeline_hz(74_250_000, 2200, 1125, 0x10), 60.0));
        // 倍掃描：÷2
        assert!(near(modeline_hz(148_500_000, 2200, 1125, 0x20), 30.0));
        assert_eq!(modeline_hz(148_500_000, 0, 1125, 0), None);
        assert_eq!(modeline_hz(148_500_000, 2200, 0, 0), None);
        assert_eq!(modeline_hz(0, 2200, 1125, 0), None);
    }

    #[test]
    fn core_video_times() {
        assert!(near(cvtime_hz(1001, 60000, 0), 59.940));
        assert!(near(cvtime_hz(1, 120, 0), 120.0));
        assert_eq!(cvtime_hz(1001, 60000, 1), None, "kCVTimeIsIndefinite");
        assert_eq!(cvtime_hz(0, 60000, 0), None);
        assert_eq!(cvtime_hz(-1, 60000, 0), None);
        assert_eq!(cvtime_hz(1, 0, 0), None);
    }

    #[test]
    fn cloned_outputs_must_agree() {
        assert_eq!(clone_rate(&[]), Clones::Nothing);
        assert_eq!(clone_rate(&[119.88]), Clones::Rate(119.88));
        assert_eq!(clone_rate(&[59.94, 59.9401]), Clones::Rate(59.94));
        assert_eq!(clone_rate(&[59.94, 60.0]), Clones::Disagree);
    }

    #[test]
    fn source_labels_in_both_languages() {
        assert_eq!(RefreshSource::DisplayConfig.label(), "QueryDisplayConfig");
        assert_eq!(RefreshSource::DevMode.label(), "EnumDisplaySettings，整數");
        crate::i18n::set_lang(crate::i18n::Lang::En);
        assert_eq!(RefreshSource::DevMode.label(), "EnumDisplaySettings, whole number");
        crate::i18n::set_lang(crate::i18n::Lang::ZhTw);
    }

    /// 實機確認：印出每個螢幕各種方法查到的更新率
    /// （`cargo test --lib screens -- --ignored --nocapture`）
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn print_windows_refresh_methods() {
        let monitors = rate::all_monitors();
        assert!(!monitors.is_empty());
        for m in monitors {
            let methods = rate::methods(m).expect("GetMonitorInfoW");
            println!(
                "{}：QueryDisplayConfig (vSyncFreq, targetInfo.refreshRate) = {:?}；EnumDisplaySettings = {:?}（{:?}）→ {:?}",
                methods.device,
                methods.paths,
                methods.devmode,
                methods.devmode.and_then(devmode_hz),
                rate::monitor_refresh(m)
            );
        }
        println!("遠端桌面：{}", remote_session());
    }
}
