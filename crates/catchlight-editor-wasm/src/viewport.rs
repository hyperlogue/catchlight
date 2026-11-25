//! One canvas drawing one replica, and the frame loops that drive them.
//!
//! The loop is Rust's `requestAnimationFrame`, so a frame is never a React
//! render behind. On WebGPU each viewport schedules its own; on WebGL2 one
//! [`GlStage`] loop drives every viewport, because GL canvases share one
//! surface: an extra renders to a texture, borrows the main surface to
//! present, copies onto its own element, and the main view redraws last in
//! the same callback so only its picture is composited.
//!
//! [`Viewport::invalidate`] requests at most one frame; a clean viewport skips
//! the GPU, and a frame stays dirty while the replica reports its puppet
//! still moving. Sizes arrive in device pixels from the
//! page. Frames pass the animation timestamp, not a `dt`, so two viewports on
//! one replica do not double its tick.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use catchlight_wgpu::{
    configure_surface, create_orthographic_camera_at, FrameTargets, RenderList, RenderTarget,
    SurfaceContext,
};
use wasm_bindgen::prelude::*;

use crate::replica::browser::ReplicaInner;
use crate::{Gpu, Replica};

/// Opaque graphite (#171d22, linear RGB), matching the editor's `--cl-bg`.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.008568,
    g: 0.012286,
    b: 0.015996,
    a: 1.0,
};

/// What the camera looks at, in world units. Y-up, like the rest of catchlight.
#[derive(Debug, Clone, Copy)]
struct Camera {
    center: glam::Vec2,
    height: f32,
}

/// Kept alive while the loop can run: dropping it would free the closure out
/// from under a scheduled call.
type FrameClosure = RefCell<Option<Closure<dyn FnMut(f64)>>>;

type FrameCallback = Rc<FrameClosure>;

/// The WebGL2 tier: one surface, one canvas, one frame loop for every
/// viewport. Held by the [`Gpu`](crate::Gpu): dropping the element would take
/// the device.
pub(crate) struct GlStage {
    canvas: web_sys::HtmlCanvasElement,
    surface: RefCell<SurfaceContext>,
    blit: SurfaceBlit,
    /// Started viewports in join order; the main one is drawn last regardless.
    members: RefCell<Vec<Member>>,
    /// Whether an extra has drawn into the surface since the main view last had it.
    clobbered: Cell<bool>,
    tick: FrameClosure,
    pending: Cell<Option<i32>>,
}

struct Member {
    inner: Weak<RefCell<Inner>>,
    main: bool,
}

impl GlStage {
    pub(crate) fn new(
        canvas: web_sys::HtmlCanvasElement,
        surface: SurfaceContext,
        device: &wgpu::Device,
    ) -> GlStage {
        let blit = SurfaceBlit::new(device, surface.render_format);
        GlStage {
            canvas,
            surface: RefCell::new(surface),
            blit,
            members: RefCell::new(Vec::new()),
            clobbered: Cell::new(false),
            tick: RefCell::new(None),
            pending: Cell::new(None),
        }
    }

    /// Matched by JavaScript identity to find the main viewport.
    pub(crate) fn is_device_canvas(&self, canvas: &web_sys::HtmlCanvasElement) -> bool {
        js_sys::Object::is(self.canvas.as_ref(), canvas.as_ref())
    }

    pub(crate) fn render_format(&self) -> wgpu::TextureFormat {
        self.surface.borrow().render_format
    }

    /// Also the main canvas's backing store and the ceiling on an extra.
    fn size(&self) -> (u32, u32) {
        let surface = self.surface.borrow();
        (surface.config.width, surface.config.height)
    }

    fn drawable(&self) -> bool {
        self.canvas.width() > 0 && self.canvas.height() > 0
    }

    fn context(&self) -> std::cell::Ref<'_, SurfaceContext> {
        self.surface.borrow()
    }

    /// Also sets the element's backing store. Only the main view calls this.
    fn resize(&self, device: &wgpu::Device, (width, height): (u32, u32)) {
        let mut surface = self.surface.borrow_mut();
        if (surface.config.width, surface.config.height) == (width, height) {
            return;
        }
        surface.resize(device, width, height);
    }

    fn clobber(&self) {
        self.clobbered.set(true);
    }

    /// Through a scratch 2D canvas: this element holds a WebGL2 context, and a GL
    /// surface takes no copy usage.
    fn read_canvas(&self, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let scratch = scratch_context(width, height)?;
        scratch
            .draw_image_with_html_canvas_element(&self.canvas, 0.0, 0.0)
            .map_err(|e| format!("copying the canvas for a readback failed ({e:?})"))?;
        pixels_of(&scratch, width, height)
    }

    /// Joining twice is joining once: React calls `start` on every remount.
    fn join(self: &Rc<Self>, inner: &Rc<RefCell<Inner>>, main: bool) {
        {
            let mut members = self.members.borrow_mut();
            let already = members.iter().any(|member| {
                member
                    .inner
                    .upgrade()
                    .is_some_and(|held| Rc::ptr_eq(&held, inner))
            });
            if !already {
                members.push(Member {
                    inner: Rc::downgrade(inner),
                    main,
                });
            }
        }
        self.run();
    }

    fn leave(&self, inner: &Rc<RefCell<Inner>>) {
        self.members.borrow_mut().retain(|member| {
            member
                .inner
                .upgrade()
                .is_some_and(|held| !Rc::ptr_eq(&held, inner))
        });
        if self.members.borrow().is_empty() {
            self.halt();
        }
    }

    fn run(self: &Rc<Self>) {
        if self.pending.get().is_some() {
            return;
        }
        let stage = self.clone();
        let closure = Closure::<dyn FnMut(f64)>::new(move |now_ms: f64| {
            // Cleared first: a `stop` from inside the frame has to win over
            // the reschedule below.
            stage.pending.set(None);
            stage.frame(now_ms);
            if stage.tick.borrow().is_some() {
                stage.pending.set(request_frame_from(&stage.tick));
            }
        });
        self.pending.set(request_frame(&closure));
        *self.tick.borrow_mut() = Some(closure);
    }

    fn halt(&self) {
        if let Some(handle) = self.pending.take() {
            if let Some(window) = web_sys::window() {
                window.cancel_animation_frame(handle).ok();
            }
        }
        // Taken out before it drops: the closure holds an `Rc` to this stage,
        // so leaving it in place would be a cycle nothing ever breaks.
        let callback = self.tick.borrow_mut().take();
        drop(callback);
    }

    /// Every extra, then the main view, so the main view's present is the one the
    /// browser composites.
    fn frame(&self, now_ms: f64) {
        let members: Vec<(Rc<RefCell<Inner>>, bool)> = self
            .members
            .borrow()
            .iter()
            .filter_map(|member| Some((member.inner.upgrade()?, member.main)))
            .collect();

        for (inner, _) in members.iter().filter(|(_, main)| !main) {
            Self::frame_one(inner, now_ms, false);
        }
        // Taken only when a main view is in the loop, or a returning main view would
        // not know an extra's picture is on its surface.
        let owed = if members.iter().any(|(_, main)| *main) {
            self.clobbered.replace(false)
        } else {
            false
        };
        for (inner, _) in members.iter().filter(|(_, main)| *main) {
            Self::frame_one(inner, now_ms, owed);
        }

        // A viewport that was freed leaves a dead weak handle behind; drop it
        // rather than upgrading it again every frame for the life of the tab.
        self.members
            .borrow_mut()
            .retain(|member| member.inner.strong_count() > 0);
    }

    fn frame_one(inner: &Rc<RefCell<Inner>>, now_ms: f64, owed: bool) {
        let Ok(mut borrowed) = inner.try_borrow_mut() else {
            return;
        };
        if owed {
            borrowed.dirty = true;
        }
        if let Err(message) = borrowed.frame(now_ms) {
            web_sys::console::error_1(&JsValue::from_str(&message));
        }
    }
}

/// The pass that puts an extra viewport's frame into the tier's surface. A
/// render pass, not a copy: a GL surface advertises only `COLOR_TARGET`.
struct SurfaceBlit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl SurfaceBlit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> SurfaceBlit {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("catchlight surface blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("catchlight surface blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("catchlight surface blit"),
            immediate_size: 0,
            bind_group_layouts: &[Some(&layout)],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("catchlight surface blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("catchlight surface blit"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        SurfaceBlit {
            pipeline,
            layout,
            sampler,
        }
    }

    fn bind(&self, device: &wgpu::Device, view: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("catchlight surface blit"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }

    /// `Load`, not a clear: outside the rectangle is the main view's last frame,
    /// about to be redrawn.
    fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("catchlight surface blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_viewport(0.0, 0.0, width as f32, height as f32, 0.0, 1.0);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
    }
}

const BLIT_WGSL: &str = r#"
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let x = f32((vertex_index & 1u) << 1u);
    let y = f32(vertex_index & 2u);
    var out: VertexOutput;
    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv = vec2<f32>(x, y);
    return out;
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(source, source_sampler, in.uv);
}
"#;

/// Behind one `RefCell`: the animation callback and JS-facing methods share it
/// on one thread with no overlap.
struct Inner {
    replica: Rc<RefCell<ReplicaInner>>,
    target: Target,
    targets: FrameTargets,
    list: RenderList,
    camera: Camera,
    size: (u32, u32),
    dirty: bool,
    /// The last frame's timestamp, so a readback redraws without advancing.
    last_ms: f64,
}

enum Target {
    Own(SurfaceContext),
    Main(Rc<GlStage>),
    Extra(Extra),
}

/// A WebGL2 viewport that is not the device's canvas; see the module doc.
struct Extra {
    stage: Rc<GlStage>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    /// This canvas's size, capped to the surface it borrows.
    rendered: (u32, u32),
    context: web_sys::CanvasRenderingContext2d,
}

impl Extra {
    fn new(
        stage: Rc<GlStage>,
        device: &wgpu::Device,
        canvas: &web_sys::HtmlCanvasElement,
        size: (u32, u32),
    ) -> Result<Extra, String> {
        let context = context_2d(canvas)?;
        let rendered = capped(size, stage.size());
        let (texture, view) = colour_target(device, stage.render_format(), rendered);
        let bind = stage.blit.bind(device, &view);
        Ok(Extra {
            stage,
            texture,
            view,
            bind,
            rendered,
            context,
        })
    }

    /// A texture cannot be resized, so a size change rebuilds it and its binding.
    fn ensure(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.rendered == size {
            return;
        }
        let (texture, view) = colour_target(device, self.stage.render_format(), size);
        self.bind = self.stage.blit.bind(device, &view);
        self.texture = texture;
        self.view = view;
        self.rendered = size;
    }

    /// Copies the rectangle just presented onto this canvas, scaled up when the
    /// cap applied.
    fn take_frame(&self, from: (u32, u32), to: (u32, u32)) -> Result<(), String> {
        self.context
            .draw_image_with_html_canvas_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                &self.stage.canvas,
                0.0,
                0.0,
                f64::from(from.0),
                f64::from(from.1),
                0.0,
                0.0,
                f64::from(to.0),
                f64::from(to.1),
            )
            .map_err(|e| format!("copying the frame onto the canvas failed ({e:?})"))
    }
}

impl Inner {
    /// Renders one frame if anything is stale. Returns whether it drew.
    fn frame(&mut self, now_ms: f64) -> Result<bool, String> {
        if !self.dirty {
            return Ok(false);
        }
        // Cleared before the work, not after: a command that lands while this
        // frame is in flight must leave the viewport stale again.
        self.dirty = false;
        self.last_ms = now_ms;

        match self.draw(now_ms, Copy::No)? {
            // Both stay stale so the next frame retries.
            Drawn::NoTexture | Drawn::Skipped => {
                self.dirty = true;
                Ok(false)
            }
            Drawn::Rendered(_) => Ok(true),
        }
    }

    /// At the loop's last timestamp, so nothing is advanced.
    fn capture(&mut self) -> Result<Readback, String> {
        let copy = match &self.target {
            Target::Own(_) => Copy::Yes,
            _ => Copy::No,
        };
        let (width, height) = self.size;
        match (self.draw(self.last_ms, copy)?, &self.target) {
            (Drawn::Rendered(Some(capture)), _) => Ok(Readback::Mapping(capture)),
            (Drawn::Rendered(None), Target::Main(stage)) => Ok(Readback::Pixels(
                stage.read_canvas(width, height)?,
                width,
                height,
            )),
            (Drawn::Rendered(None), Target::Extra(extra)) => Ok(Readback::Pixels(
                pixels_of(&extra.context, width, height)?,
                width,
                height,
            )),
            _ => Err("the surface handed over no texture to read back; \
                 the canvas was resized, or the tab is hidden"
                .to_string()),
        }
    }

    fn draw(&mut self, now_ms: f64, copy: Copy) -> Result<Drawn, String> {
        let (width, height) = self.size;
        let rendered = match &self.target {
            Target::Extra(extra) => {
                if !extra.stage.drawable() {
                    return Ok(Drawn::Skipped);
                }
                capped(self.size, extra.stage.size())
            }
            _ => self.size,
        };
        // The element's aspect, not the rendered one: the cap scales both axes
        // by the same factor, so the picture is the same shape either way.
        let aspect = width as f32 / height.max(1) as f32;
        let view_proj =
            create_orthographic_camera_at(self.camera.height, aspect, self.camera.center);

        // Before the long borrow below, because rebuilding the target needs
        // the device and a `&mut` of its own.
        if matches!(self.target, Target::Extra(_)) {
            let device = self
                .replica
                .try_borrow()
                .ok()
                .and_then(|replica| replica.device().cloned());
            if let (Some(device), Target::Extra(extra)) = (device, &mut self.target) {
                extra.ensure(&device, rendered);
            }
        }

        let mut replica = self
            .replica
            .try_borrow_mut()
            .map_err(|_| "the replica is already borrowed on this frame".to_string())?;
        // The timestamp, not a `dt`: the replica derives the step, so a second canvas
        // in this animation frame advances nothing. Stale again while the puppet is
        // still moving.
        if replica.frame(now_ms, &mut self.list)? {
            self.dirty = true;
        }

        let borrowed = match &self.target {
            Target::Own(_) => None,
            Target::Main(stage) => Some(stage.context()),
            Target::Extra(extra) => Some(extra.stage.context()),
        };
        let surface: &SurfaceContext = match (&self.target, &borrowed) {
            (Target::Own(context), _) => context,
            (_, Some(context)) => context,
            (_, None) => return Err("this viewport has no surface to present".to_string()),
        };
        // A resized canvas or a hidden-then-shown tab refuses a texture; reconfigure.
        let Some((frame, view)) = surface.acquire() else {
            if let Some(device) = replica.device() {
                surface.reconfigure(device);
            }
            return Ok(Drawn::NoTexture);
        };
        let target_view = match &self.target {
            Target::Extra(extra) => &extra.view,
            _ => &view,
        };

        let render = replica
            .render
            .as_mut()
            .ok_or("this viewport's replica lost its renderer")?;

        // The device the capture below copies with: the frame holds the
        // renderer for as long as it owns the encoder.
        let device = render.renderer.device.clone();
        render.renderer.update_camera(view_proj);
        let mut done = render.renderer.frame().render(
            &[&self.list],
            RenderTarget {
                view: target_view,
                texture: None,
                width: rendered.0,
                height: rendered.1,
            },
            &mut self.targets,
            Some(CLEAR),
        );
        // The borrowed surface, written in the same submission as the frame
        // that fills it, so what is presented is this frame and not the last.
        if let Target::Extra(extra) = &self.target {
            extra
                .stage
                .blit
                .draw(done.encoder(), &view, &extra.bind, rendered.0, rendered.1);
        }
        // Recorded into the same encoder as the pass above, so what is read
        // back is this frame and not the one before it.
        let capture = match copy {
            Copy::Yes => Some(Capture::record(
                &device,
                done.encoder(),
                &frame.texture,
                width,
                height,
            )),
            Copy::No => None,
        };
        done.submit();
        frame.present();

        // The present above put an extra's picture on the tier's canvas. Take
        // it now, in this same task: the main view is about to draw over it,
        // and the browser must composite only that.
        if let Target::Extra(extra) = &self.target {
            // Told before the copy: the present clobbered the surface even if the copy
            // fails.
            extra.stage.clobber();
            extra.take_frame(rendered, (width, height))?;
        }
        Ok(Drawn::Rendered(capture))
    }
}

/// `size` scaled down to fit `cap`, keeping its aspect.
fn capped((width, height): (u32, u32), (max_width, max_height): (u32, u32)) -> (u32, u32) {
    let scale = (max_width as f32 / width.max(1) as f32)
        .min(max_height as f32 / height.max(1) as f32)
        .min(1.0);
    if scale >= 1.0 {
        return (width, height);
    }
    (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    )
}

#[derive(Clone, Copy)]
enum Copy {
    Yes,
    No,
}

enum Drawn {
    /// The surface refused a texture; it has been reconfigured.
    NoTexture,
    /// An extra whose main canvas has no backing store.
    Skipped,
    Rendered(Option<Capture>),
}

fn colour_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    (width, height): (u32, u32),
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("catchlight viewport target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// The 2D context of `canvas`; fails if it already holds another kind.
fn context_2d(
    canvas: &web_sys::HtmlCanvasElement,
) -> Result<web_sys::CanvasRenderingContext2d, String> {
    canvas
        .get_context("2d")
        .map_err(|e| format!("this canvas refused a 2d context ({e:?})"))?
        .ok_or("this canvas has no 2d context; something else is already drawing it")?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .map_err(|_| "the canvas's 2d context is not one".to_string())
}

fn scratch_context(width: u32, height: u32) -> Result<web_sys::CanvasRenderingContext2d, String> {
    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("no document to make a canvas in")?
        .create_element("canvas")
        .map_err(|e| format!("no canvas element ({e:?})"))?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| "the element made was not a canvas".to_string())?;
    canvas.set_width(width);
    canvas.set_height(height);
    context_2d(&canvas)
}

fn pixels_of(
    context: &web_sys::CanvasRenderingContext2d,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let data = context
        .get_image_data(0.0, 0.0, f64::from(width), f64::from(height))
        .map_err(|e| format!("reading the canvas back failed ({e:?})"))?;
    Ok(data.data().0)
}

enum Readback {
    Mapping(Capture),
    Pixels(Vec<u8>, u32, u32),
}

fn frame_object(width: u32, height: u32, rgba: &[u8]) -> Result<JsValue, JsValue> {
    let frame = js_sys::Object::new();
    js_sys::Reflect::set(&frame, &"width".into(), &JsValue::from(width))?;
    js_sys::Reflect::set(&frame, &"height".into(), &JsValue::from(height))?;
    js_sys::Reflect::set(&frame, &"rgba".into(), &js_sys::Uint8Array::from(rgba))?;
    Ok(frame.into())
}

struct Capture {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    /// The buffer's row stride, which `copyTextureToBuffer` rounds up to 256.
    bytes_per_row: u32,
    /// `bgra8unorm` on most GPUs, `rgba8unorm` on llvmpipe.
    swap_rb: bool,
}

impl Capture {
    fn record(
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Capture {
        let bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("catchlight viewport readback"),
            size: u64::from(bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Capture {
            buffer,
            width,
            height,
            bytes_per_row,
            swap_rb: matches!(
                texture.format(),
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
            ),
        }
    }

    async fn finish(self) -> Result<JsValue, JsValue> {
        map_read(&self.buffer).await?;
        let padded = self.buffer.slice(..).get_mapped_range();
        let row = self.width as usize * 4;
        let mut rgba = Vec::with_capacity(row * self.height as usize);
        for y in 0..self.height as usize {
            let at = y * self.bytes_per_row as usize;
            let Some(line) = padded.get(at..at + row) else {
                return Err(JsValue::from_str("the readback buffer is short of pixels"));
            };
            if self.swap_rb {
                for &[b, g, r, a] in line.as_chunks::<4>().0 {
                    rgba.extend_from_slice(&[r, g, b, a]);
                }
            } else {
                rgba.extend_from_slice(line);
            }
        }
        drop(padded);
        self.buffer.unmap();

        frame_object(self.width, self.height, &rgba)
    }
}

/// `map_async` as a future via a promise the browser resolves: this crate has
/// no executor, and nothing polls the device on the web.
async fn map_read(buffer: &wgpu::Buffer) -> Result<(), JsValue> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // The caller is the browser and a throw here has nowhere to
                // go, so the settle is dropped rather than unwrapped.
                let settled = match result {
                    Ok(()) => resolve.call0(&JsValue::NULL),
                    Err(e) => reject.call1(
                        &JsValue::NULL,
                        &JsValue::from_str(&format!("mapping the readback buffer failed ({e})")),
                    ),
                };
                drop(settled);
            });
    });
    wasm_bindgen_futures::JsFuture::from(promise).await?;
    Ok(())
}

#[wasm_bindgen]
pub struct Viewport {
    inner: Rc<RefCell<Inner>>,
    frames: Frames,
}

enum Frames {
    Own {
        tick: FrameCallback,
        pending: Rc<Cell<Option<i32>>>,
    },
    Tier {
        stage: Rc<GlStage>,
        main: bool,
    },
}

#[wasm_bindgen]
impl Viewport {
    /// Draws `replica` on `canvas` until [`Viewport::stop`]. The canvas's current
    /// `width`/`height` are the initial backing store.
    #[wasm_bindgen(constructor)]
    pub fn new(
        gpu: &Gpu,
        replica: &Replica,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<Viewport, JsValue> {
        let size = (canvas.width().max(1), canvas.height().max(1));

        let (target, frames) = match &gpu.gl {
            // WebGL2, on the device's own canvas. The surface was configured
            // when the device was acquired, to whatever this canvas measured
            // then; a viewport built on it later may find it another size, and
            // a stencil sized to the canvas cannot be attached beside a
            // surface sized to the past.
            Some(stage) if stage.is_device_canvas(&canvas) => {
                stage.resize(&gpu.device, size);
                (
                    Target::Main(stage.clone()),
                    Frames::Tier {
                        stage: stage.clone(),
                        main: true,
                    },
                )
            }
            // WebGL2, on any other canvas: no surface is possible, so render to a texture
            // and borrow the main canvas's surface to present it.
            Some(stage) => {
                let extra = Extra::new(stage.clone(), &gpu.device, &canvas, size)
                    .map_err(|e| JsValue::from_str(&e))?;
                (
                    Target::Extra(extra),
                    Frames::Tier {
                        stage: stage.clone(),
                        main: false,
                    },
                )
            }
            None => {
                let surface = gpu.surface_for(canvas).map_err(|e| JsValue::from_str(&e))?;
                // Caught here because `configure_surface` would index an empty
                // capability list instead of saying so.
                if surface.get_capabilities(&gpu.adapter).formats.is_empty() {
                    return Err(JsValue::from_str(
                        "this canvas has no surface format in common with the adapter",
                    ));
                }
                let mut surface =
                    configure_surface(&gpu.adapter, &gpu.device, surface, size.0, size.1);
                // `COPY_SRC` lets `readback` copy the frame. WebGPU only: a GL surface
                // refuses any usage but `RENDER_ATTACHMENT`.
                surface.config.usage |= wgpu::TextureUsages::COPY_SRC;
                surface.reconfigure(&gpu.device);
                (
                    Target::Own(surface),
                    Frames::Own {
                        tick: Rc::new(RefCell::new(None)),
                        pending: Rc::new(Cell::new(None)),
                    },
                )
            }
        };
        // One format for the tab, whichever tier chose it: a GL viewport takes
        // the tier's, and on WebGPU every canvas of one adapter agrees.
        let render_format = match &target {
            Target::Own(surface) => surface.render_format,
            Target::Main(stage) => stage.render_format(),
            Target::Extra(extra) => extra.stage.render_format(),
        };

        let shared = replica.inner();
        shared
            .try_borrow_mut()
            .map_err(|_| JsValue::from_str("this replica is busy drawing another canvas"))?
            .ensure_renderer(gpu, render_format)
            .map_err(|e| JsValue::from_str(&e))?;

        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                replica: shared,
                target,
                targets: FrameTargets::new(),
                list: RenderList::default(),
                // Until `set_camera`, frame the origin at a height that suits a rig.
                camera: Camera {
                    center: glam::Vec2::ZERO,
                    height: 2.0,
                },
                size,
                dirty: true,
                last_ms: 0.0,
            })),
            frames,
        })
    }

    /// Redraws the current frame at the last timestamp and answers it as
    /// `{ width, height, rgba }`, always in RGBA order. For the browser smoke test
    /// only; it verifies rendering, not presentation.
    pub fn readback(&self) -> Result<js_sys::Promise, JsValue> {
        let readback = self
            .inner
            .try_borrow_mut()
            .map_err(|_| JsValue::from_str("this viewport is in the middle of a frame"))?
            .capture()
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(match readback {
            Readback::Mapping(capture) => wasm_bindgen_futures::future_to_promise(capture.finish()),
            // Resolved rather than awaited: reading a canvas is synchronous,
            // and the caller is handed the same promise either way.
            Readback::Pixels(rgba, width, height) => {
                js_sys::Promise::resolve(&frame_object(width, height, &rgba)?)
            }
        })
    }

    /// Marks the picture stale; at most one frame follows.
    pub fn invalidate(&self) {
        self.inner.borrow_mut().dirty = true;
    }

    /// Reconfigures for a canvas whose backing store is now `width` × `height`
    /// **device** pixels. The caller sets `canvas.width`/`canvas.height` to the
    /// same two numbers.
    pub fn resize(&self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        let mut inner = self.inner.borrow_mut();
        if inner.size == (width, height) {
            return;
        }
        inner.size = (width, height);
        inner.dirty = true;
        // Borrowed only for the handle: the target is this viewport's, the
        // device is the tab's, and the frame in flight may hold the replica.
        let Ok(replica) = inner.replica.try_borrow() else {
            return;
        };
        let device = replica.device().cloned();
        drop(replica);
        let Some(device) = device else {
            return;
        };
        match &mut inner.target {
            Target::Own(surface) => surface.resize(&device, width, height),
            Target::Main(stage) => stage.resize(&device, (width, height)),
            // Nothing here: an extra's texture is rebuilt by the next frame,
            // which is also where the cap is applied.
            Target::Extra(_) => {}
        }
    }

    /// Points the camera at `(center_x, center_y)` in world units, framing
    /// `height` world units vertically.
    #[wasm_bindgen(js_name = setCamera)]
    pub fn set_camera(&self, center_x: f32, center_y: f32, height: f32) {
        let mut inner = self.inner.borrow_mut();
        inner.camera = Camera {
            center: glam::Vec2::new(center_x, center_y),
            height: height.max(f32::EPSILON),
        };
        inner.dirty = true;
    }

    /// Starts the frame loop; idempotent.
    pub fn start(&self) {
        match &self.frames {
            Frames::Own { tick, pending } => {
                if pending.get().is_some() {
                    return;
                }
                let inner = self.inner.clone();
                let tick_for_closure = tick.clone();
                let pending_for_closure = pending.clone();
                let closure = Closure::<dyn FnMut(f64)>::new(move |now_ms: f64| {
                    // Cleared first: a `stop` from inside the frame (an error
                    // handler, a React unmount racing the callback) has to win
                    // over the reschedule below.
                    pending_for_closure.set(None);
                    if let Ok(mut borrowed) = inner.try_borrow_mut() {
                        if let Err(message) = borrowed.frame(now_ms) {
                            web_sys::console::error_1(&JsValue::from_str(&message));
                        }
                    }
                    if let Some(closure) = tick_for_closure.borrow().as_ref() {
                        pending_for_closure.set(request_frame(closure));
                    }
                });
                pending.set(request_frame(&closure));
                *tick.borrow_mut() = Some(closure);
            }
            Frames::Tier { stage, main } => stage.join(&self.inner, *main),
        }
    }

    /// Stops the frame loop; idempotent.
    pub fn stop(&self) {
        match &self.frames {
            Frames::Own { tick, pending } => {
                if let Some(handle) = pending.take() {
                    if let Some(window) = web_sys::window() {
                        window.cancel_animation_frame(handle).ok();
                    }
                }
                // Taken out before it drops: the browser is no longer holding
                // a call to it, and the closure captures this same cell, so
                // leaving it in place would be a cycle that keeps the GPU
                // resources alive forever.
                let callback = tick.borrow_mut().take();
                drop(callback);
            }
            Frames::Tier { stage, .. } => stage.leave(&self.inner),
        }
    }
}

/// `free()` from JavaScript lands here. Stopping first is what makes it safe:
/// a scheduled frame holding a freed closure is a trap in wasm, not an error,
/// and on the GL tier a stopped viewport is also one the tier stops driving.
impl Drop for Viewport {
    fn drop(&mut self) {
        self.stop();
    }
}

fn request_frame(closure: &Closure<dyn FnMut(f64)>) -> Option<i32> {
    web_sys::window()?
        .request_animation_frame(closure.as_ref().unchecked_ref())
        .ok()
}

fn request_frame_from(tick: &FrameClosure) -> Option<i32> {
    request_frame(tick.borrow().as_ref()?)
}
