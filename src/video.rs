//! 影片畫面。
//!
//! mpv 透過 render API 畫進我們自己的 FBO（貼圖），再把貼圖畫到 egui 指定的矩形。
//! 只有 mpv 通知有新影格（或尺寸改變）時才重新渲染；
//! 介面重繪（例如滑鼠移過控制列）只把貼圖再畫一次，很便宜。
//!
//! 把貼圖畫到畫面上用的是一個小 shader，不用 `glBlitFramebuffer`：
//! 視窗的 framebuffer 如果是多重取樣（MSAA，Linux 的 Mesa 常見），blit 過去會失敗，畫面一片黑。

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
    /// 把影片貼圖畫到畫面上的 shader；None = 還沒建立，Err = 建立失敗（改用 blit）
    quad: Option<Result<Quad, String>>,
    /// mpv 的 update callback 設定，表示要問 mpv 是否有新影格
    dirty: Arc<AtomicBool>,
    /// 第一次畫面輸出時記錄 GL 狀態（排查顯示問題用）
    logged: bool,
    /// VITASCOPE_DEBUG：已經印過幾次像素取樣
    probes: u32,
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
                quad: None,
                dirty,
                logged: false,
                probes: 0,
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

            let dirty = self.dirty.swap(false, Ordering::AcqRel);
            let has_new_frame = dirty && render.update();
            let rendered = resized || has_new_frame;
            // VITASCOPE_DEBUG：前幾次 mpv 渲染時，取樣貼圖與畫面中心的像素
            let probe = rendered && self.probes < 30 && std::env::var_os("VITASCOPE_DEBUG").is_some();
            let mut texture_px = None;
            if rendered {
                // egui 開著 scissor（裁切到 clip rect，螢幕座標），會把畫進 FBO 的內容裁掉
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                if let Err(e) = render.render(target.fbo.0.get(), w, h, true) {
                    eprintln!("[vitascope] mpv 渲染失敗：{e}");
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
            return Err(format!("framebuffer 不完整（0x{status:x}）"));
        }
        Ok(Target { fbo, tex, size: [w, h] })
    }
}

/// 一個蓋住整個 viewport 的三角形（用 gl_VertexID 算座標，不需要頂點資料）
unsafe fn create_quad(gl: &glow::Context) -> Result<Quad, String> {
    let version = egui_glow::ShaderVersion::get(gl);
    if !version.is_new_shader_interface() {
        return Err(format!("GLSL 版本太舊（{version:?}）"));
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
