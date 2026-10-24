//! iOS 外殼（Rust 那一半）：不用 eframe／winit（winit 還不支援 iOS 27 規定的 UIScene，見 bench/ios/README.md），
//! 改由 Swift 的 UIScene app 建好 CAMetalLayer，這裡用 egui 核心＋egui-wgpu（Metal）直接畫上去。
//!
//! Swift 每次 CADisplayLink 呼叫 `xq_egui_frame`；Rust 只在「有輸入事件、行情到了、egui 要求重畫的時間到了」才真的畫，
//! 其他幀直接回傳（不看焦點，所以不會有 Android 那次「失焦就不畫」的凍結問題）。
//! 觸控：每根手指送 egui 的 Touch 事件（雙指縮放用），第一根手指同時模擬成滑鼠（點選、拖曳、長按十字線用），跟 egui-winit 一樣。

use std::ffi::{c_char, c_void, CStr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Event, Modifiers, PointerButton, Pos2, Rect, TouchDeviceId, TouchId, TouchPhase};
use parking_lot::Mutex;

use crate::MobileApp;

pub struct Host {
    app: MobileApp,
    ctx: egui::Context,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: egui_wgpu::Renderer,
    scale: f32,
    events: Vec<Event>,
    start: Instant,
    /// egui（或行情執行緒）要求的下一次重畫時間；None＝不用畫
    repaint_at: Arc<Mutex<Option<Instant>>>,
    primary: Option<u64>,
    last_pos: Pos2,
    wants_kb: bool,
}

fn want_at(slot: &Mutex<Option<Instant>>, at: Instant) {
    let mut g = slot.lock();
    if g.is_none_or(|t| at < t) {
        *g = Some(at);
    }
}

impl Host {
    fn new(layer: *mut c_void, w: u32, h: u32, scale: f32) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::METAL, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let surface = unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer)) }.map_err(|e| format!("create_surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("request_adapter: {e}"))?;
        eprintln!("[egui-ios] adapter: {:?}", adapter.get_info().name);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("xq-egui"),
            // 用這張顯示卡實際的上限（模擬器 GPU 有些上限比 wgpu 預設值還低）
            required_limits: adapter.limits(),
            ..Default::default()
        })).map_err(|e| format!("request_device: {e}"))?;
        let mut config = surface.get_default_config(&adapter, w.max(1), h.max(1)).ok_or("surface 不支援這張顯示卡")?;
        let caps = surface.get_capabilities(&adapter);
        // egui 的著色器輸出的是 gamma 空間顏色：選非 sRGB 格式，顏色才跟桌面版一樣
        if let Some(f) = caps.formats.iter().find(|f| !f.is_srgb()) {
            config.format = *f;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        config.alpha_mode = wgpu::CompositeAlphaMode::Opaque;
        surface.configure(&device, &config);
        let renderer = egui_wgpu::Renderer::new(&device, config.format, egui_wgpu::RendererOptions::default());

        let ctx = egui::Context::default();
        let repaint_at = Arc::new(Mutex::new(Some(Instant::now())));
        {
            let slot = repaint_at.clone();
            ctx.set_request_repaint_callback(move |info| want_at(&slot, Instant::now() + info.delay));
        }
        let app = MobileApp::new(&ctx);
        Ok(Host { app, ctx, surface, device, queue, config, renderer, scale, events: Vec::new(), start: Instant::now(), repaint_at, primary: None, last_pos: Pos2::ZERO, wants_kb: false })
    }

    fn resize(&mut self, w: u32, h: u32, scale: f32) {
        self.scale = scale;
        if (w, h) != (self.config.width, self.config.height) && w > 0 && h > 0 {
            self.config.width = w;
            self.config.height = h;
            self.surface.configure(&self.device, &self.config);
        }
        want_at(&self.repaint_at, Instant::now());
    }

    fn touch(&mut self, id: u64, phase: u32, x: f32, y: f32, force: f32) {
        let pos = pos2(x, y);
        let phase = match phase {
            0 => TouchPhase::Start,
            1 => TouchPhase::Move,
            2 => TouchPhase::End,
            _ => TouchPhase::Cancel,
        };
        self.events.push(Event::Touch { device_id: TouchDeviceId(0), id: TouchId(id), phase, pos, force: (force > 0.0).then_some(force) });
        let m = Modifiers::NONE;
        match phase {
            TouchPhase::Start if self.primary.is_none() => {
                self.primary = Some(id);
                self.events.push(Event::PointerMoved(pos));
                self.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: m });
            }
            TouchPhase::Move if self.primary == Some(id) => self.events.push(Event::PointerMoved(pos)),
            TouchPhase::End | TouchPhase::Cancel if self.primary == Some(id) => {
                self.primary = None;
                self.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: m });
                self.events.push(Event::PointerGone);
            }
            _ => {}
        }
        self.last_pos = pos;
        want_at(&self.repaint_at, Instant::now());
    }

    fn key(&mut self, key: egui::Key) {
        for pressed in [true, false] {
            self.events.push(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
        }
        want_at(&self.repaint_at, Instant::now());
    }

    /// 回傳位元：1＝這次有畫，2＝要叫出鍵盤（搜尋框有焦點）
    fn frame(&mut self) -> u32 {
        let now = Instant::now();
        let due = { self.repaint_at.lock().is_some_and(|t| t <= now) };
        if !due && self.events.is_empty() {
            return if self.wants_kb { 2 } else { 0 };
        }
        *self.repaint_at.lock() = None;
        let size = vec2(self.config.width as f32, self.config.height as f32) / self.scale;
        let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(self.start.elapsed().as_secs_f64()), events: std::mem::take(&mut self.events), ..Default::default() };
        raw.max_texture_side = Some(self.device.limits().max_texture_dimension_2d as usize);
        if let Some(v) = raw.viewports.get_mut(&egui::ViewportId::ROOT) {
            v.native_pixels_per_point = Some(self.scale);
        }
        let app = &mut self.app;
        let out = self.ctx.run_ui(raw, |ui| app.frame_ui(ui));
        self.wants_kb = out.platform_output.ime.is_some() || self.ctx.egui_wants_keyboard_input();
        if let Some(vo) = out.viewport_output.get(&egui::ViewportId::ROOT) {
            if vo.repaint_delay < Duration::from_secs(3600) {
                want_at(&self.repaint_at, now + vo.repaint_delay);
            }
        }
        let ppp = out.pixels_per_point;
        let prims = self.ctx.tessellate(out.shapes, ppp);
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                want_at(&self.repaint_at, now);
                return 0;
            }
            _ => {
                want_at(&self.repaint_at, now + Duration::from_millis(16));
                return 0;
            }
        };
        for (id, deltas) in &out.textures_delta.set {
            for delta in deltas {
                self.renderer.update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let sd = egui_wgpu::ScreenDescriptor { size_in_pixels: [self.config.width, self.config.height], pixels_per_point: ppp };
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("xq-egui") });
        let mut cmds = self.renderer.update_buffers(&self.device, &self.queue, &mut enc, &prims, &sd);
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let bg = crate::theme::bg();
            let c = |v: u8| v as f64 / 255.0;
            let mut pass = enc
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("xq-egui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: c(bg.r()), g: c(bg.g()), b: c(bg.b()), a: 1.0 }), store: wgpu::StoreOp::Store },
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &prims, &sd);
        }
        cmds.push(enc.finish());
        self.queue.submit(cmds);
        self.queue.present(frame);
        for id in &out.textures_delta.free {
            self.renderer.free_texture(id);
        }
        1 | if self.wants_kb { 2 } else { 0 }
    }
}

// ───────────── C 介面（Swift 呼叫；全部在主執行緒） ─────────────

#[unsafe(no_mangle)]
pub extern "C" fn xq_egui_create(layer: *mut c_void, w_px: u32, h_px: u32, scale: f32) -> *mut Host {
    crate::ios_start();
    match Host::new(layer, w_px, h_px, scale) {
        Ok(h) => Box::into_raw(Box::new(h)),
        Err(e) => {
            eprintln!("[egui-ios] 初始化失敗：{e}");
            std::ptr::null_mut()
        }
    }
}

/// # Safety
/// `h` 必須是 `xq_egui_create` 回傳的指標
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xq_egui_resize(h: *mut Host, w_px: u32, h_px: u32, scale: f32) {
    if let Some(h) = unsafe { h.as_mut() } {
        h.resize(w_px, h_px, scale);
    }
}

/// phase：0 開始、1 移動、2 結束、3 取消；x/y 是 point（不是像素）
/// # Safety
/// 同上
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xq_egui_touch(h: *mut Host, id: u64, phase: u32, x: f32, y: f32, force: f32) {
    if let Some(h) = unsafe { h.as_mut() } {
        h.touch(id, phase, x, y, force);
    }
}

/// 鍵盤打的字（UTF-8，包含注音／拼音選好的字）
/// # Safety
/// 同上；`s` 是 NUL 結尾字串
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xq_egui_text(h: *mut Host, s: *const c_char) {
    let (Some(h), false) = (unsafe { h.as_mut() }, s.is_null()) else { return };
    let t = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    match t.as_str() {
        "\n" => h.key(egui::Key::Enter),
        _ => {
            h.events.push(Event::Text(t));
            want_at(&h.repaint_at, Instant::now());
        }
    }
}

/// # Safety
/// 同上
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xq_egui_backspace(h: *mut Host) {
    if let Some(h) = unsafe { h.as_mut() } {
        h.key(egui::Key::Backspace);
    }
}

/// # Safety
/// 同上
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xq_egui_frame(h: *mut Host) -> u32 {
    match unsafe { h.as_mut() } {
        Some(h) => h.frame(),
        None => 0,
    }
}

/// 回到前景：馬上重畫一次
/// # Safety
/// 同上
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xq_egui_wake(h: *mut Host) {
    if let Some(h) = unsafe { h.as_mut() } {
        want_at(&h.repaint_at, Instant::now());
    }
}

// ───────────── 字型：PingFang TC／Menlo，mmap 系統字型檔，不複製進記憶體 ─────────────

/// 用 CoreText 問系統字型檔的路徑，mmap 整個檔案，再用 ttf-parser 找出 .ttc 裡 PostScript 名稱相符的那一個字面
fn system_font(ps_name: &str) -> Result<egui::FontData, String> {
    let font = core_text::font::new_from_name(ps_name, 16.0).map_err(|_| format!("CoreText 找不到 {ps_name}"))?;
    let path = font.copy_descriptor().font_path().ok_or_else(|| format!("{ps_name} 沒有檔案路徑"))?;
    let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mm = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| format!("mmap {}: {e}", path.display()))?;
    let n = ttf_parser::fonts_in_collection(&mm).unwrap_or(1);
    let index = (0..n)
        .find(|&i| {
            ttf_parser::Face::parse(&mm, i).ok().is_some_and(|f| {
                f.names().into_iter().any(|nm| nm.name_id == ttf_parser::name_id::POST_SCRIPT_NAME && nm.to_string().is_some_and(|s| s == ps_name))
            })
        })
        .unwrap_or(0);
    eprintln!("[fonts] {ps_name} ← mmap {}（{:.1} MB 檔案，第 {index} 個字面；只有用到的頁面會進記憶體）", path.display(), mm.len() as f64 / 1048576.0);
    let blob: egui::epaint::text::Blob = Arc::new(mm);
    Ok(egui::FontData::from_blob(blob, index))
}

pub fn setup_fonts(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    let ui = std::env::var("XQ_CJK_FONT").unwrap_or_else(|_| "PingFangTC-Regular".into());
    match system_font(&ui) {
        Ok(fd) => {
            defs.font_data.insert("pingfang".into(), Arc::new(fd));
            defs.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "pingfang".into());
            defs.families.entry(egui::FontFamily::Monospace).or_default().push("pingfang".into());
        }
        Err(e) => eprintln!("[fonts] {e}（中文會變方塊）"),
    }
    match system_font("Menlo-Regular") {
        Ok(fd) => {
            defs.font_data.insert("menlo".into(), Arc::new(fd));
            defs.families.entry(egui::FontFamily::Monospace).or_default().insert(0, "menlo".into());
        }
        Err(e) => eprintln!("[fonts] {e}（等寬字改用內建字型）"),
    }
    ctx.set_fonts(defs);
}
