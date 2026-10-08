//! 影片畫面。
//!
//! mpv 透過 render API 畫進我們自己的 FBO（貼圖），再把貼圖畫到 egui 指定的矩形。
//! 只有 mpv 通知有新影格（或尺寸改變）時才重新渲染；
//! 介面重繪（例如滑鼠移過控制列）只把貼圖再畫一次，很便宜。
//!
//! 把貼圖畫到畫面上用的是一個小 shader，不用 `glBlitFramebuffer`：
//! 視窗的 framebuffer 如果是多重取樣（MSAA，例如顯示卡驅動強制開啟反鋸齒），blit 過去會失敗，畫面一片黑。
//!
//! 取影格的時機（不卡住介面）：mpv 預設的 render 會一直等到影格的預定時間才回來，
//! 一般播放（音訊同步）時新影格一出來就取，每格要在介面的執行緒上等 40 ms 左右，整個介面只剩影片的格率。
//! 現在先問 mpv 下一格的預定時間，離預定時間還超過一次螢幕更新就這一輪先畫舊的貼圖，
//! 跟 egui 要求大約預定時間前半次更新再畫一次；不到一次更新的那一輪（不管是誰叫醒的）才取，
//! 照樣讓 render 等到預定時間：交出影格的時間跟以前完全一樣，介面每格只等大約一次更新（見 `pacing::take`）。
//! 顯示卡來不及在預定時間畫完影格時（GL 的 timestamp query 量得到）自動提早一點取（見 `pacing::GpuLate`）。
//! 依螢幕同步時 mpv 要我們馬上畫，swap 等垂直同步就是時鐘。
//! `VITASCOPE_PACING=block` 回到以前一直等的做法。

use crate::mpv::render::{GetProcAddress, RenderContext, RenderOpts};
use crate::mpv::{self, Mpv};
use crate::pacing::{
    self, ClockSync, GpuLate, Lead, PresentLog, Presents, RenderCounter, RenderStats, SegmentMax, Take, WakeLate,
};
use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct VideoView {
    inner: Arc<Mutex<Inner>>,
    pass: Arc<PassInfo>,
}

/// 這一輪的資訊：`begin_pass` 在 logic() 裡記下，paint callback（不在 egui 的一輪裡面）延後取影格時用
struct PassInfo {
    /// egui 的 `predicted_dt`（f32 的位元）
    predicted_dt: AtomicU32,
    /// 螢幕更新一次的時間（奈秒；0 = 不知道，當成 60 Hz）：取影格時最多讓 render 等這麼久
    period_ns: AtomicI64,
}

struct Inner {
    render: Option<RenderContext>,
    mpv: Arc<Mpv>,
    egui_ctx: egui::Context,
    /// 自己挑時間取影格（false = `VITASCOPE_PACING=block`：跟以前一樣讓 render 等到預定時間）
    pace: bool,
    pass: Arc<PassInfo>,
    /// 計時器叫醒的那一輪晚多少畫影片
    late: WakeLate,
    /// GPU 來不及在預定時間畫完影格時提早多少取
    gpu_late: GpuLate,
    /// 量 GPU 什麼時候畫完影格；None = 還沒試，Some(None) = 不支援
    gpu: Option<Option<GpuTimer>>,
    counter: RenderCounter,
    /// VITASCOPE_DEBUG=pacing：記錄每格大約什麼時候顯示
    trace: Option<PresentTrace>,
    target: Option<Target>,
    /// 把影片貼圖畫到畫面上的 shader；None = 還沒建立，Err = 建立失敗（改用 blit）
    quad: Option<Result<Quad, String>>,
    /// mpv 的 update callback 設定，表示要問 mpv 是否有新影格
    dirty: Arc<AtomicBool>,
    /// 第一次畫面輸出時記錄 GL 狀態（排查顯示問題用）
    logged: bool,
    /// VITASCOPE_DEBUG：已經印過幾次像素取樣
    probes: u32,
}

/// 量 GPU 什麼時候畫完影格：render 回來之後放一個 GL 的 timestamp query（GPU 做完前面所有工作時記下時間），
/// 之後的某一輪結果出來了再拿（不用等 GPU）。render 回來時已經到了預定時間，量到的最早也是那時候
struct GpuTimer {
    query: glow::Query,
    /// 等結果中的影格的預定時間（mpv 時鐘的奈秒）
    pending: Option<i64>,
    clock: ClockSync,
}

impl GpuTimer {
    /// 桌面 GL 3.3 起才有 timestamp query（glQueryCounter、glGetQueryObjectui64v、glGetInteger64v 都是核心功能）；
    /// OpenGL ES、更舊的版本不量（只有 ARB_timer_query 的舊驅動不一定有 glGetInteger64v，glow 呼叫沒載入的函式會 panic）
    unsafe fn new(gl: &glow::Context) -> Option<Self> {
        let v = gl.version();
        if v.is_embedded || (v.major, v.minor) < (3, 3) {
            return None;
        }
        let query = unsafe { gl.create_query() }.ok()?;
        Some(Self {
            query,
            pending: None,
            clock: ClockSync::default(),
        })
    }

    /// 剛畫好預定時間是 `due` 的影格：放 query，順便對一次時鐘。上一格的結果還沒出來就不量這一格
    unsafe fn start(&mut self, gl: &glow::Context, mpv: &Mpv, due: i64) {
        if self.pending.is_some() {
            return;
        }
        unsafe {
            gl.query_counter(self.query, glow::TIMESTAMP);
            // 馬上交給 GPU：不然要等這一輪畫完（介面的其他部分）才送出去，量到的時間會太晚
            gl.flush();
            let before = mpv.time_ns();
            let now = gl.get_parameter_i64(glow::TIMESTAMP);
            self.clock.sample(before, now, mpv.time_ns());
        }
        self.pending = Some(due);
    }

    /// 結果出來了：(預定時間, GPU 畫完的時間（mpv 時鐘）)
    unsafe fn poll(&mut self, gl: &glow::Context) -> Option<(i64, i64)> {
        let due = self.pending?;
        // 沒有綁 GL_QUERY_BUFFER 時「offset」就是放結果的位址（glGetQueryObjectui64v，GL 3.3 核心）。
        // 不用 get_query_parameter_u32 / u64：glow 在沒有 GL 4.5 時改呼叫 EXT 版本，macOS（GL 4.1）沒有，會 panic
        let mut available = 0u64;
        unsafe {
            gl.get_query_parameter_u64_with_offset(
                self.query,
                glow::QUERY_RESULT_AVAILABLE,
                &mut available as *mut u64 as usize,
            )
        };
        if available == 0 {
            return None;
        }
        self.pending = None;
        let mut stamp = 0u64;
        unsafe {
            gl.get_query_parameter_u64_with_offset(self.query, glow::QUERY_RESULT, &mut stamp as *mut u64 as usize)
        };
        Some((due, (stamp as i64).wrapping_add(self.clock.offset()?)))
    }
}

struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    size: [i32; 2],
}

/// 一個畫滿 viewport 的三角形，取樣影片貼圖
struct Quad {
    program: glow::Program,
    vao: glow::VertexArray,
}

impl VideoView {
    /// 必須在 GL context 生效時呼叫（eframe 建立 App 的時候）。
    /// `pace`：自己挑時間取影格（`VITASCOPE_PACING=block` 時是 false）
    pub fn new(
        mpv: Arc<Mpv>,
        get_proc_address: GetProcAddress,
        egui_ctx: egui::Context,
        pace: bool,
    ) -> mpv::Result<Self> {
        // SAFETY: eframe 在呼叫 app creator 時，glow 的 GL context 是 current
        let mut render = unsafe { RenderContext::new_opengl(mpv.clone(), get_proc_address)? };
        let dirty = Arc::new(AtomicBool::new(true));
        let flag = dirty.clone();
        let ctx = egui_ctx.clone();
        render.set_update_callback(move || {
            flag.store(true, Ordering::Release);
            if pace {
                // 只要一輪：`request_repaint()` 每次會畫兩輪（egui 讓版面穩定用），
                // 第二輪在影格還沒到時間時只是多看一次。延遲不是 0 就只畫一輪，扣掉 predicted_dt 後還是馬上畫
                ctx.request_repaint_after_for(Duration::from_nanos(1), egui::ViewportId::ROOT);
            } else {
                ctx.request_repaint();
            }
        });
        let pass = Arc::new(PassInfo {
            predicted_dt: AtomicU32::new((1.0f32 / 60.0).to_bits()),
            period_ns: AtomicI64::new(0),
        });
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                render: Some(render),
                mpv,
                egui_ctx,
                pace,
                pass: pass.clone(),
                late: WakeLate::default(),
                gpu_late: GpuLate::default(),
                gpu: None,
                counter: RenderCounter::default(),
                trace: pacing::debug().then(PresentTrace::default),
                target: None,
                quad: None,
                dirty,
                logged: false,
                probes: 0,
            })),
            pass,
        })
    }

    /// 每一輪開始時（logic()）呼叫：記下 egui 的 `predicted_dt` 與視窗所在螢幕的更新率（None = 不知道），
    /// paint callback 決定取影格的時機時要用
    pub fn begin_pass(&self, ctx: &egui::Context, refresh_hz: Option<f64>) {
        let pdt = ctx.input(|i| i.predicted_dt);
        self.pass.predicted_dt.store(pdt.to_bits(), Ordering::Relaxed);
        let period = refresh_hz
            .filter(|hz| hz.is_finite() && *hz > 0.0)
            .map_or(0, |hz| (1e9 / hz).round() as i64);
        self.pass.period_ns.store(period, Ordering::Relaxed);
    }

    /// 畫面輸出的統計
    pub fn stats(&self) -> RenderStats {
        let window = pacing::block_window(self.pass.period_ns.load(Ordering::Relaxed));
        let us = |ns: i64| (ns / 1000).clamp(0, u32::MAX.into()) as u32;
        let Ok(i) = self.inner.lock() else {
            return RenderStats::default();
        };
        RenderStats {
            window_us: us(window),
            wake_late_us: us(i.late.estimate()),
            gpu_lead_us: us(i.gpu_late.lead()),
            ..i.counter.stats()
        }
    }

    /// VITASCOPE_DEBUG=pacing：上次之後 render 最久、GPU 最多提早多少，拿了就重新算
    pub fn take_segment_max(&self) -> SegmentMax {
        match self.inner.lock() {
            Ok(mut i) => i.counter.take_segment(),
            Err(_) => SegmentMax::default(),
        }
    }

    /// VITASCOPE_DEBUG=pacing：上次之後每格交出、顯示的時間，拿了就清掉
    pub fn take_presents(&self) -> Presents {
        match self.inner.lock() {
            Ok(mut i) => i.trace.as_mut().map(PresentTrace::take).unwrap_or_default(),
            Err(_) => Presents::default(),
        }
    }

    /// 在 `rect` 畫出影片（mpv 會自己補黑邊、維持比例）。
    pub fn paint(&self, ui: &egui::Ui, rect: egui::Rect) {
        let inner = self.inner.clone();
        let callback = egui_glow::CallbackFn::new(move |info, painter| {
            let vp = info.viewport_in_pixels();
            let clip = info.clip_rect_in_pixels();
            if let Ok(mut inner) = inner.lock() {
                // SAFETY: egui_glow 在 GL context 生效時呼叫 paint callback，結束後會還原 GL 狀態
                unsafe {
                    inner.paint(
                        painter.gl(),
                        painter.intermediate_fbo(),
                        [vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px],
                        [clip.left_px, clip.from_bottom_px, clip.width_px, clip.height_px],
                    )
                };
            }
        });
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(callback),
        });
    }

    /// 程式結束時呼叫（GL context 還在）：render context 必須在 GL 還有效時釋放。
    pub fn destroy(&self, gl: Option<&glow::Context>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.render = None;
            let Some(gl) = gl else { return };
            unsafe {
                if let Some(Some(t)) = inner.gpu.take() {
                    gl.delete_query(t.query);
                }
                if let Some(t) = inner.target.take() {
                    gl.delete_framebuffer(t.fbo);
                    gl.delete_texture(t.tex);
                }
                if let Some(Ok(q)) = inner.quad.take() {
                    gl.delete_program(q.program);
                    gl.delete_vertex_array(q.vao);
                }
            }
        }
    }
}

impl Inner {
    /// `viewport` / `clip`：[左, 下, 寬, 高]，單位是實體像素，原點在左下（OpenGL 慣例）
    unsafe fn paint(
        &mut self,
        gl: &glow::Context,
        screen_fbo: Option<glow::Framebuffer>,
        viewport: [i32; 4],
        clip: [i32; 4],
    ) {
        let [x, y, w, h] = viewport;
        if w <= 0 || h <= 0 || self.render.is_none() {
            return;
        }
        unsafe {
            let resized = self.target.as_ref().is_none_or(|t| t.size != [w, h]);
            if resized {
                if let Some(old) = self.target.take() {
                    gl.delete_framebuffer(old.fbo);
                    gl.delete_texture(old.tex);
                }
                match create_target(gl, w, h) {
                    Ok(t) => self.target = Some(t),
                    Err(e) => {
                        eprintln!("[vitascope] 建立影片 framebuffer 失敗：{e}");
                        return;
                    }
                }
            }
            if self.quad.is_none() {
                let quad = create_quad(gl);
                if let Err(e) = &quad {
                    eprintln!("[vitascope] 影片 shader 建立失敗，改用 blit：{e}");
                }
                self.quad = Some(quad);
            }
            let (Some(render), Some(target)) = (&self.render, &self.target) else {
                return;
            };

            self.counter.pass();
            let dirty = self.dirty.swap(false, Ordering::AcqRel);
            let mut has_new_frame = dirty && render.update();
            // 新影格的預定時間（mpv 時鐘的奈秒；0 = 沒有指定，例如依螢幕同步）
            let mut due = 0;
            // 沒有新影格（只是視窗大小變了）時：自己挑時間就不等
            let mut block = !self.pace;
            // 現在（mpv 時鐘的奈秒）：決定取影格的時機、量計時器叫醒的那一輪晚多少
            let now = self.mpv.time_ns();
            // 讓 render 等的範圍：一次螢幕更新
            let window = pacing::block_window(self.pass.period_ns.load(Ordering::Relaxed));
            if self.pace {
                self.late.painted(now, window);
            }
            // GPU 什麼時候畫完影格（取影格之後到預定時間只剩幾毫秒）：自己挑時間時要用；
            // VITASCOPE_PACING=block 照以前不量，只在 VITASCOPE_DEBUG=pacing 時量來比較
            if self.pace || self.trace.is_some() {
                let gpu = self.gpu.get_or_insert_with(|| GpuTimer::new(gl));
                if let Some((done_due, done)) = gpu.as_mut().and_then(|t| t.poll(gl)) {
                    if self.pace {
                        self.gpu_late.record(done - done_due);
                    }
                    if let Some(t) = &mut self.trace {
                        t.log.gpu_done(done - done_due);
                    }
                }
            }
            // 有預定時間、讓 render 等的新影格（量 GPU 什麼時候畫完）
            let mut timed = false;
            if has_new_frame {
                let info = render.next_frame_info();
                if let Some(info) = &info {
                    due = pacing::target_ns(info.target_raw, now, self.mpv.time_us());
                }
                let lead = Lead {
                    window,
                    late: self.late.estimate(),
                    gpu: self.gpu_late.lead(),
                };
                if self.pace {
                    self.counter.gpu_lead(lead.gpu);
                }
                match pacing::take(self.pace, resized, info.as_ref(), due, now, lead) {
                    Take::Now { block: b } => {
                        block = b;
                        timed = b && due > 0 && info.as_ref().is_some_and(|i| !i.block_vsync && !i.redraw);
                        // 在叫醒的時間之前就取了：之後那一輪不是計時器叫醒的
                        self.late.cancel();
                    }
                    Take::Later { wake } => {
                        // 到時候再問一次 mpv；這一輪先畫舊的貼圖
                        self.dirty.store(true, Ordering::Release);
                        let pdt = f32::from_bits(self.pass.predicted_dt.load(Ordering::Relaxed));
                        // paint callback 不在 egui 的一輪裡面，要指定 viewport
                        self.egui_ctx
                            .request_repaint_after_for(pacing::repaint_after(wake, pdt), egui::ViewportId::ROOT);
                        self.late.asked(now + wake.as_nanos() as i64);
                        self.counter.defer();
                        has_new_frame = false;
                    }
                }
            }
            let rendered = resized || has_new_frame;
            // VITASCOPE_DEBUG：前幾次 mpv 渲染時，取樣貼圖與畫面中心的像素
            let probe = rendered && self.probes < 30 && std::env::var_os("VITASCOPE_DEBUG").is_some();
            let mut texture_px = None;
            if rendered {
                // egui 開著 scissor（裁切到 clip rect，螢幕座標），會把畫進 FBO 的內容裁掉
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                let opts = RenderOpts { block, skip: false };
                let start = Instant::now();
                if let Err(e) = render.render(target.fbo.0.get(), w, h, true, opts) {
                    eprintln!("[vitascope] mpv 渲染失敗：{e}");
                }
                self.counter.rendered(start.elapsed(), has_new_frame, opts);
                if timed && let Some(Some(t)) = &mut self.gpu {
                    t.start(gl, &self.mpv, due);
                }
                if has_new_frame && let Some(t) = &mut self.trace {
                    t.rendered(self.mpv.time_ns(), due);
                    if due > 0 {
                        t.log.taken(due - now);
                    }
                }
                if std::env::var_os("VITASCOPE_DEBUG").is_some() {
                    let error = gl.get_error();
                    if error != glow::NO_ERROR {
                        eprintln!("[vitascope] mpv 渲染後有 GL 錯誤 0x{error:x}");
                    }
                }
                if probe {
                    texture_px = Some(read_center(gl, Some(target.fbo), [0, 0, w, h]));
                }
            }

            // 回到 egui 的 framebuffer，把影片貼圖畫到指定的矩形
            gl.bind_framebuffer(glow::FRAMEBUFFER, screen_fbo);
            gl.viewport(x, y, w, h);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(clip[0], clip[1], clip[2], clip[3]);
            gl.disable(glow::BLEND);
            match &self.quad {
                Some(Ok(q)) => {
                    gl.use_program(Some(q.program));
                    gl.active_texture(glow::TEXTURE0);
                    gl.bind_texture(glow::TEXTURE_2D, Some(target.tex));
                    gl.bind_vertex_array(Some(q.vao));
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                    gl.bind_vertex_array(None);
                    gl.bind_texture(glow::TEXTURE_2D, None);
                    gl.use_program(None);
                }
                _ => {
                    // 很舊的 OpenGL（沒有 GLSL 1.40 / ES 3.0）才會走這裡
                    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.fbo));
                    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, screen_fbo);
                    gl.blit_framebuffer(0, 0, w, h, x, y, x + w, y + h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
                    gl.bind_framebuffer(glow::FRAMEBUFFER, screen_fbo);
                }
            }

            if let Some(texture_px) = texture_px {
                self.probes += 1;
                let screen_px = read_center(gl, screen_fbo, viewport);
                eprintln!("[vitascope] 像素取樣 {w}×{h}：貼圖 {texture_px:?}，畫面 {screen_px:?}");
            }

            if !self.logged {
                self.logged = true;
                let samples = gl.get_parameter_i32(glow::SAMPLES);
                let error = gl.get_error();
                let how = if matches!(self.quad, Some(Ok(_))) {
                    "shader"
                } else {
                    "blit"
                };
                if error != glow::NO_ERROR {
                    eprintln!("[vitascope] 影片畫面輸出（{how}、視窗 MSAA {samples}x）發生 GL 錯誤 0x{error:x}");
                } else if std::env::var_os("VITASCOPE_DEBUG").is_some() {
                    eprintln!("[vitascope] 影片畫面輸出：{how}，視窗 MSAA {samples}x");
                }
            }
        }
    }
}

/// VITASCOPE_DEBUG=pacing：每格大約什麼時候顯示。一般播放時 swap 不會等（顯示卡的佇列是空的），
/// 影格在 swap 之後的下一次垂直同步顯示；這裡用 render 結束的時間估計（之後只剩介面的其他部分要畫）。
/// 垂直同步的時間每一段只跟 DWM 要一次，之後自己往後數（DWM 每次回報的時間有一點誤差，
/// 每格都重新要的話，誤差會讓剛好在垂直同步附近的影格被算到前後一次）
#[derive(Default)]
struct PresentTrace {
    grid: Option<VsyncGrid>,
    log: PresentLog,
}

impl PresentTrace {
    /// 剛畫好一格新影格：`now` 是 mpv 時鐘的現在，`due` 是它的預定時間（0 = 沒有指定）
    fn rendered(&mut self, now: i64, due: i64) {
        if self.grid.is_none() {
            self.grid = VsyncGrid::query();
        }
        self.log.record(now, due, self.grid.map(|g| g.next()));
    }

    /// 拿走這一段的紀錄；下一段重新跟 DWM 要垂直同步的時間（更新間隔的小誤差不會一直累積）
    fn take(&mut self) -> Presents {
        self.grid = None;
        self.log.take()
    }
}

/// 垂直同步的時間表：某一次的時間 + 間隔（QueryPerformanceCounter 的單位）
#[derive(Clone, Copy)]
#[cfg_attr(not(windows), allow(dead_code))]
struct VsyncGrid {
    anchor: i64,
    period: i64,
    freq: i64,
}

impl VsyncGrid {
    #[cfg(windows)]
    fn query() -> Option<Self> {
        use windows_sys::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
        use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;
        // SAFETY: DWM_TIMING_INFO 是純資料，全 0 是合法的值；Windows 8.1 起 hwnd 要給 NULL
        let mut info: DWM_TIMING_INFO = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<DWM_TIMING_INFO>() as u32;
        if unsafe { DwmGetCompositionTimingInfo(std::ptr::null_mut(), &mut info) } < 0 {
            return None;
        }
        let mut freq = 0i64;
        unsafe { QueryPerformanceFrequency(&mut freq) };
        let (anchor, period) = (info.qpcVBlank as i64, info.qpcRefreshPeriod as i64);
        (period > 0 && freq > 0).then_some(Self { anchor, period, freq })
    }

    /// 其他平台沒有簡單的方法知道垂直同步的時間：不記錄
    #[cfg(not(windows))]
    fn query() -> Option<Self> {
        None
    }

    /// 現在之後的第一次垂直同步：(第幾次, 離現在幾奈秒)
    fn next(&self) -> (i64, i64) {
        let now = qpc_now();
        let (n, at) = pacing::next_vsync(self.anchor, self.period, now);
        (n, ((at - now) as i128 * 1_000_000_000 / self.freq as i128) as i64)
    }
}

#[cfg(windows)]
fn qpc_now() -> i64 {
    let mut now = 0i64;
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut now) };
    now
}

#[cfg(not(windows))]
fn qpc_now() -> i64 {
    0
}

/// 排查用：讀 `fbo` 裡 `rect`（[左, 下, 寬, 高]）中心的像素，讀完綁回 `fbo`
unsafe fn read_center(gl: &glow::Context, fbo: Option<glow::Framebuffer>, rect: [i32; 4]) -> [u8; 4] {
    let mut px = [0u8; 4];
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
        gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
        gl.read_pixels(
            rect[0] + rect[2] / 2,
            rect[1] + rect[3] / 2,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut px)),
        );
    }
    px
}

unsafe fn create_target(gl: &glow::Context, w: i32, h: i32) -> Result<Target, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        // 貼圖跟畫面一樣大，取樣是 1:1；用 NEAREST 確保不會有任何模糊
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.bind_texture(glow::TEXTURE_2D, None);

        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        if status != glow::FRAMEBUFFER_COMPLETE {
            gl.delete_framebuffer(fbo);
            gl.delete_texture(tex);
            return Err(crate::tf!(
                "framebuffer 不完整（0x{status:x}）",
                "incomplete framebuffer (0x{status:x})"
            ));
        }
        Ok(Target { fbo, tex, size: [w, h] })
    }
}

/// 一個蓋住整個 viewport 的三角形（用 gl_VertexID 算座標，不需要頂點資料）
unsafe fn create_quad(gl: &glow::Context) -> Result<Quad, String> {
    let version = egui_glow::ShaderVersion::get(gl);
    if !version.is_new_shader_interface() {
        return Err(crate::tf!(
            "GLSL 版本太舊（{version:?}）",
            "GLSL version too old ({version:?})"
        ));
    }
    let header = version.version_declaration();
    let precision = if header.contains(" es") {
        "precision mediump float;\n"
    } else {
        ""
    };
    let vertex = format!(
        "{header}{precision}\
         out vec2 v_uv;\n\
         void main() {{\n\
             vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));\n\
             v_uv = p;\n\
             gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);\n\
         }}\n"
    );
    let fragment = format!(
        "{header}{precision}\
         uniform sampler2D u_tex;\n\
         in vec2 v_uv;\n\
         out vec4 frag_color;\n\
         void main() {{ frag_color = vec4(texture(u_tex, v_uv).rgb, 1.0); }}\n"
    );
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, source) in [(glow::VERTEX_SHADER, vertex), (glow::FRAGMENT_SHADER, fragment)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &source);
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                gl.delete_program(program);
                return Err(format!("shader 編譯失敗：{log}"));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            return Err(format!("shader 連結失敗：{log}"));
        }
        gl.use_program(Some(program));
        let loc = gl.get_uniform_location(program, "u_tex");
        gl.uniform_1_i32(loc.as_ref(), 0);
        gl.use_program(None);
        // core profile 畫東西一定要綁一個 VAO，即使不用頂點資料
        let vao = gl.create_vertex_array()?;
        Ok(Quad { program, vao })
    }
}
