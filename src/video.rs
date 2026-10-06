//! 影片畫面。
//!
//! mpv 透過 render API 畫進我們自己的 FBO，再 blit 到 egui 指定的矩形。
//! 只有 mpv 通知有新影格（或尺寸改變）時才重新渲染；
//! 介面重繪（例如滑鼠移過控制列）只做一次很便宜的 blit。

use crate::mpv::render::{GetProcAddress, RenderContext};
use crate::mpv::{self, Mpv};
use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub struct VideoView {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    render: Option<RenderContext>,
    target: Option<Target>,
    /// mpv 的 update callback 設定，表示要問 mpv 是否有新影格
    dirty: Arc<AtomicBool>,
}

struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    size: [i32; 2],
}

impl VideoView {
    /// 必須在 GL context 生效時呼叫（eframe 建立 App 的時候）。
    pub fn new(mpv: Arc<Mpv>, get_proc_address: GetProcAddress, egui_ctx: egui::Context) -> mpv::Result<Self> {
        // SAFETY: eframe 在呼叫 app creator 時，glow 的 GL context 是 current
        let mut render = unsafe { RenderContext::new_opengl(mpv, get_proc_address)? };
        let dirty = Arc::new(AtomicBool::new(true));
        let flag = dirty.clone();
        render.set_update_callback(move || {
            flag.store(true, Ordering::Release);
            egui_ctx.request_repaint();
        });
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                render: Some(render),
                target: None,
                dirty,
            })),
        })
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
            if let (Some(gl), Some(t)) = (gl, inner.target.take()) {
                unsafe {
                    gl.delete_framebuffer(t.fbo);
                    gl.delete_texture(t.tex);
                }
            }
        }
    }
}

impl Inner {
    /// `viewport` / `clip`：[左, 下, 寬, 高]，單位是實體像素，原點在左下（OpenGL 慣例）
    unsafe fn paint(&mut self, gl: &glow::Context, viewport: [i32; 4], clip: [i32; 4]) {
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
            let (Some(render), Some(target)) = (&self.render, &self.target) else {
                return;
            };

            let dirty = self.dirty.swap(false, Ordering::AcqRel);
            let has_new_frame = dirty && render.update();
            if resized || has_new_frame {
                // egui 開著 scissor（裁切到 clip rect，螢幕座標），會把畫進 FBO 的內容裁掉
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                if let Err(e) = render.render(target.fbo.0.get(), w, h, true) {
                    eprintln!("[vitascope] mpv 渲染失敗：{e}");
                }
            }

            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.fbo));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(clip[0], clip[1], clip[2], clip[3]);
            gl.blit_framebuffer(0, 0, w, h, x, y, x + w, y + h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }
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
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
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
            return Err(format!("framebuffer 不完整（0x{status:x}）"));
        }
        Ok(Target { fbo, tex, size: [w, h] })
    }
}
