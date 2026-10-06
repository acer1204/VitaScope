//! mpv render API（OpenGL）：讓 mpv 把影片畫到我們指定的 framebuffer。
//!
//! 所有方法（除了 update callback 本身）都必須在 GL context 生效的執行緒上呼叫。

use super::{Callback, Mpv, Result, check, trampoline};
use libmpv2_sys as sys;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;

/// 取得 OpenGL 函式位址的函式（eframe 的 `CreationContext::get_proc_address`）。
pub type GetProcAddress = Arc<dyn Fn(&CStr) -> *const c_void + Send + Sync>;

pub struct RenderContext {
    ctx: *mut sys::mpv_render_context,
    // mpv 可能在之後（例如第一次啟用硬體解碼時）才查詢 GL 函式，
    // 所以 get_proc_address 的資料要活得跟 render context 一樣久
    _gpa: Box<GetProcAddress>,
    update: Option<Box<Callback>>,
    // render context 必須在 mpv 核心之前釋放
    _mpv: Arc<Mpv>,
}

// SAFETY: render context 只會在 GL 執行緒使用；包在 Mutex 裡跨執行緒傳遞是安全的
unsafe impl Send for RenderContext {}

unsafe extern "C" fn gpa_trampoline(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    let gpa = unsafe { &*(ctx as *const GetProcAddress) };
    gpa(unsafe { CStr::from_ptr(name) }) as *mut c_void
}

impl RenderContext {
    /// 建立 OpenGL render context。
    ///
    /// # Safety
    /// 呼叫時 OpenGL context 必須是目前執行緒的 current context。
    pub unsafe fn new_opengl(mpv: Arc<Mpv>, get_proc_address: GetProcAddress) -> Result<Self> {
        let gpa: Box<GetProcAddress> = Box::new(get_proc_address);
        let mut init = sys::mpv_opengl_init_params {
            get_proc_address: Some(gpa_trampoline),
            get_proc_address_ctx: &*gpa as *const GetProcAddress as *mut c_void,
        };
        let mut advanced: c_int = 0;
        let mut params = [
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut init as *mut _ as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_ADVANCED_CONTROL,
                data: &mut advanced as *mut c_int as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut sys::mpv_render_context = std::ptr::null_mut();
        let code = unsafe { sys::mpv_render_context_create(&mut ctx, mpv.raw(), params.as_mut_ptr()) };
        check(code, || "建立 mpv OpenGL render context".into())?;
        Ok(Self {
            ctx,
            _gpa: gpa,
            update: None,
            _mpv: mpv,
        })
    }

    /// 有新影格要畫、或需要重繪時呼叫 `f`。跟 wakeup callback 一樣，
    /// `f` 在 mpv 的執行緒上執行，不能呼叫任何 mpv API。
    pub fn set_update_callback(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let boxed: Box<Callback> = Box::new(Box::new(f));
        let data = &*boxed as *const Callback as *mut c_void;
        unsafe { sys::mpv_render_context_set_update_callback(self.ctx, Some(trampoline), data) };
        self.update = Some(boxed);
    }

    /// update callback 之後呼叫；回傳 true 代表有新影格需要 `render`。
    pub fn update(&self) -> bool {
        let flags = unsafe { sys::mpv_render_context_update(self.ctx) };
        flags & u64::from(sys::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME) != 0
    }

    /// 把目前影格畫到 `fbo`（0 = 預設 framebuffer）。
    /// `flip_y`：畫到 OpenGL 慣例（原點在左下）的 framebuffer 時要設 true。
    ///
    /// # Safety
    /// GL context 必須是 current，`fbo` 必須是完整、可繪製的 framebuffer。
    pub unsafe fn render(&self, fbo: u32, width: i32, height: i32, flip_y: bool) -> Result<()> {
        let mut target = sys::mpv_opengl_fbo {
            fbo: fbo as c_int,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = flip_y.into();
        let mut params = [
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut target as *mut _ as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip as *mut c_int as *mut c_void,
            },
            sys::mpv_render_param {
                type_: sys::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let code = unsafe { sys::mpv_render_context_render(self.ctx, params.as_mut_ptr()) };
        check(code, || "mpv render".into())
    }

    /// 畫面送出（swap buffers）後呼叫，幫助 mpv 掌握顯示時機。
    pub fn report_swap(&self) {
        unsafe { sys::mpv_render_context_report_swap(self.ctx) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe {
            sys::mpv_render_context_set_update_callback(self.ctx, None, std::ptr::null_mut());
            sys::mpv_render_context_free(self.ctx);
        }
    }
}
