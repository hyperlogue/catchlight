//! One canvas drawing one replica, and the frame loops that drive them.
//!
//! Invariants this module carries:
//!
//! - **The renderer owns the loop; the page owns the element.** JavaScript
//!   hands over a canvas and says when it resized, and that is the whole
//!   contract. `requestAnimationFrame` is scheduled here, in Rust, so a frame
//!   is never one React render behind the state it draws. A UI layer that owned
//!   the loop would have to re-enter the wasm boundary every frame to ask
//!   whether anything changed.
//!
//! - **Who owns that loop differs by tier, and only by tier.** On WebGPU every
//!   viewport schedules its own callback: the canvases are independent
//!   swapchains and the order they draw in cannot be seen. On WebGL2 one loop
//!   belongs to the tier ([`GlStage`]) and drives every viewport on it in a
//!   fixed order, because there the canvases share one surface and the order
//!   they present in is the only thing keeping the picture right. Nothing above
//!   this file can tell: [`start`] and [`stop`] mean the same on both.
//!
//! - **How a frame reaches the canvas is the tier's business and nobody
//!   else's.** A canvas that is a wgpu surface has its frame presented into it
//!   and nothing is copied. That is every viewport on WebGPU, and on WebGL2 it
//!   is the viewport on the canvas the device was acquired from, which is the
//!   first one the editor attached (see [`Gpu`]). The editor mounts one canvas,
//!   so on both tiers the common case renders, presents, and stops there.
//!
//! - **On WebGL2 an extra canvas borrows the main one's surface, and is hidden
//!   in time.** A GL device presents into exactly one canvas, and no other
//!   canvas can have a surface at all — `wgpu-hal` reports no capabilities for
//!   a surface whose WebGL2 context is not the adapter's. So an extra viewport
//!   renders into a texture of its own size, blits that into the top-left of
//!   the *main* canvas's surface, presents, and copies that rectangle onto its
//!   own element with `drawImage`. The main view then renders and presents
//!   last, in the same animation-frame callback, so the browser composites the
//!   main view and never the borrowed frame. Nothing is enlarged and nothing is
//!   clipped: what hides the extra's pixels is that no composite happens
//!   between its present and the main view's.
//!
//! - **The main view redraws whenever an extra borrowed the surface.** The
//!   blit clobbered it, so a clean main view is redrawn anyway on such a frame.
//!   With no extra viewport, which is the editor today, that never fires and a
//!   settled canvas still costs one predicate per frame.
//!
//! - **An extra is capped at the main canvas's backing store.** It borrows that
//!   surface, so it cannot render larger than it; past the cap it renders at
//!   the cap and `drawImage` scales the result up on the way out, uniformly, so
//!   the picture is soft rather than stretched. An extra whose main canvas has
//!   no backing store at all skips the frame instead of drawing nothing.
//!
//! - **[`invalidate`] asks for a frame; it does not draw one.** Callers say
//!   "this is stale" as often as they like — once per pointer move, twice per
//!   command — and at most one frame follows. A `draw()` that rendered
//!   synchronously would render a gesture's worth of frames the compositor
//!   never shows, which is the shape of drag jank rather than a cure for it.
//!
//! - **Nothing is drawn while nothing is stale, and motion is what keeps it
//!   stale.** A started viewport still wakes on every animation frame, but a
//!   clean one returns without touching the GPU or acquiring a surface
//!   texture. A frame that drew stays dirty exactly while the replica's tick
//!   reports [`Motion`](catchlight_core::Motion): physics settling and an
//!   animation playing draw themselves, and everything else — a pose change, a
//!   scratch edit, a structure push — is someone calling [`invalidate`]. So a
//!   settled puppet costs a predicate per frame and a swinging one keeps
//!   drawing with nobody asking. Every viewport that frames hears that motion,
//!   including the ones whose call did not tick, or a second canvas would go
//!   clean and stop following a pendulum the first one is still swinging.
//!
//! - **[`readback`] is the one asynchronous entry, and the loop cannot tell
//!   it happened.** It draws the current frame again, at the timestamp the
//!   loop last used so nothing is advanced. It exists because a headless
//!   Chromium holding a WebGPU device never composites the canvas — a
//!   screenshot of one is blank while the picture is correct — so the smoke
//!   test has no other way to see what was drawn. Where the pixels come from
//!   differs by tier and is invisible to whoever asked. A WebGPU surface is
//!   copied into a buffer in the frame's own submission and the promise is
//!   that mapping. A GL surface takes no copy usage at all, so the main
//!   canvas is drawn onto a scratch 2D canvas and read from there, and an
//!   extra canvas is already a 2D context holding its own frame. All three
//!   answer RGBA8, row-major, width by height. Nothing in the editor calls it,
//!   and it starts, stops and invalidates nothing.
//!
//! - **[`start`] and [`stop`] are repeatable and idempotent.** React mounts,
//!   unmounts and remounts an effect — StrictMode does it deliberately on every
//!   mount — so the pair has to survive being run twice with nothing left
//!   running in between and no second loop started.
//!
//! - **Size arrives in device pixels, and only from above.** This never reads
//!   `clientWidth` or `devicePixelRatio`: CSS pixels are the page's business,
//!   and a renderer that measured the DOM itself would fight the layout it is
//!   embedded in. The canvas's backing store and the surface configuration are
//!   the same two numbers, passed in together by whoever resized it. What
//!   bounds them is the device's `max_texture_dimension_2d`, which the page
//!   reads once from [`Gpu::maxSize`](crate::Gpu::max_size) — a surface above
//!   it fails to configure and the canvas goes black with no other symptom.
//!
//! - **The GPU state belongs to the replica, not to the canvas, and so does
//!   the clock.** The renderer, its cache and its textures live on the
//!   [`Replica`] this draws; what a viewport owns is its stencil, its composite
//!   pool, and either the surface it presents into or the texture it renders
//!   to, all sized to this canvas alone. Two viewports on one session therefore
//!   cost two of those and not two copies of the model. The puppet is shared
//!   the same way, so a frame hands the replica the animation frame's
//!   *timestamp* rather than a `dt` of its own:
//!   [`ReplicaState::frame`](crate::ReplicaState::frame) ticks once per
//!   timestamp and the second canvas in the batch only collects and draws.
//!   Two viewports advancing the same puppet would run its physics and its
//!   animations at twice the rate.
//!
//! [`invalidate`]: Viewport::invalidate
//! [`readback`]: Viewport::readback
//! [`start`]: Viewport::start
//! [`stop`]: Viewport::stop
//! [`Gpu`]: crate::Gpu
//! [`Replica`]: crate::Replica

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use catchlight_wgpu::{
    configure_surface, create_orthographic_camera_at, CompositePool, RenderList, StencilTarget,
    SurfaceContext,
};
use wasm_bindgen::prelude::*;

use crate::replica::browser::ReplicaInner;
use crate::{Gpu, Replica};

/// The background a model is composited over. Deliberately opaque: the editor
/// draws its own checkerboard behind the canvas when it wants one, and an
/// alpha-blended canvas costs a composite pass the page never asked for.
const CLEAR: wgpu::Color = wgpu::Color {
    r: 0.12,
    g: 0.12,
    b: 0.14,
    a: 1.0,
};

/// What the camera looks at, in world units. Y-up, like the rest of catchlight.
#[derive(Debug, Clone, Copy)]
struct Camera {
    center: glam::Vec2,
    height: f32,
}

/// The animation-frame closure a loop owns, kept alive for as long as the loop
/// can run: dropping it while the browser still holds a handle to it would
/// free the closure out from under a scheduled call.
type FrameClosure = RefCell<Option<Closure<dyn FnMut(f64)>>>;

/// The same, shared between a viewport and the callback it schedules.
type FrameCallback = Rc<FrameClosure>;

/// The WebGL2 tier: one surface, one canvas, one frame loop, every viewport.
///
/// Made when the device is, because on that tier the device *is* this canvas's
/// rendering context. Held by the [`Gpu`](crate::Gpu) so the element outlives
/// any viewport on it: dropping it would take the context, and with it the
/// device, out from under every other canvas in the tab.
///
/// The loop is here rather than on a viewport because the order matters. Every
/// GL viewport presents into this one surface, so the last present of an
/// animation frame is the picture the browser composites, and that has to be
/// the main view's. Extras draw first and borrow the surface; the main view
/// draws last and takes it back.
pub(crate) struct GlStage {
    /// The device's canvas. Configuring the surface sizes this element, so it
    /// is the page's canvas and its backing store is the page's business.
    canvas: web_sys::HtmlCanvasElement,
    surface: RefCell<SurfaceContext>,
    /// The pass that puts an extra viewport's texture into this surface.
    blit: SurfaceBlit,
    /// Every started viewport on this tier, in the order they joined. The main
    /// one is drawn last whatever that order is.
    members: RefCell<Vec<Member>>,
    /// Whether an extra has drawn into the surface since the main view last
    /// had it. The main view redraws on the next frame if so, because what is
    /// on the surface is no longer its picture.
    clobbered: Cell<bool>,
    tick: FrameClosure,
    pending: Cell<Option<i32>>,
}

/// One viewport in the tier's loop.
struct Member {
    inner: Weak<RefCell<Inner>>,
    /// Whether this is the viewport on the device's own canvas.
    main: bool,
}

impl GlStage {
    /// The stage for a freshly acquired GL device.
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

    /// The element the device presents into, matched by JavaScript identity
    /// when a viewport asks whether it is the main one.
    pub(crate) fn is_device_canvas(&self, canvas: &web_sys::HtmlCanvasElement) -> bool {
        js_sys::Object::is(self.canvas.as_ref(), canvas.as_ref())
    }

    /// The format every viewport on this tier renders, and the one the blit
    /// pipeline was compiled for.
    pub(crate) fn render_format(&self) -> wgpu::TextureFormat {
        self.surface.borrow().render_format
    }

    /// The surface's current size, which is also the main canvas's backing
    /// store and therefore the ceiling on an extra viewport.
    fn size(&self) -> (u32, u32) {
        let surface = self.surface.borrow();
        (surface.config.width, surface.config.height)
    }

    /// Whether the main canvas has a backing store at all. An extra viewport
    /// has nowhere to render when it does not.
    fn drawable(&self) -> bool {
        self.canvas.width() > 0 && self.canvas.height() > 0
    }

    /// The surface, for a frame that is about to present through it.
    fn context(&self) -> std::cell::Ref<'_, SurfaceContext> {
        self.surface.borrow()
    }

    /// Configures the surface for a canvas that is now `size`, which is also
    /// what sets the element's backing store. Only the main view calls this.
    fn resize(&self, device: &wgpu::Device, (width, height): (u32, u32)) {
        let mut surface = self.surface.borrow_mut();
        if (surface.config.width, surface.config.height) == (width, height) {
            return;
        }
        surface.resize(device, width, height);
    }

    /// Notes that an extra has drawn into the surface, so the main view owes
    /// it a redraw whether or not anything else made it stale.
    fn clobber(&self) {
        self.clobbered.set(true);
    }

    /// The pixels on the device's canvas, for the main view's readback.
    ///
    /// Through a scratch 2D canvas because this element holds a WebGL2 context
    /// and can hold no other, and because a GL surface takes no copy usage, so
    /// wgpu cannot read it either.
    fn read_canvas(&self, width: u32, height: u32) -> Result<Vec<u8>, String> {
        let scratch = scratch_context(width, height)?;
        scratch
            .draw_image_with_html_canvas_element(&self.canvas, 0.0, 0.0)
            .map_err(|e| format!("copying the canvas for a readback failed ({e:?})"))?;
        pixels_of(&scratch, width, height)
    }

    /// Adds a viewport to the loop and starts it. Joining twice is joining
    /// once: `start` is idempotent and React calls it on every remount.
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

    /// Removes a viewport, and stops the loop when the last one goes.
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

    /// Schedules the loop if it is not already running.
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

    /// Cancels the loop. Starting again afterwards works.
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

    /// One animation frame for the whole tier: every extra, then the main view.
    ///
    /// The order is the design. An extra borrows the surface and presents it,
    /// which puts its picture on the main canvas; the main view then renders
    /// and presents over it before the browser composites anything.
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
        // Taken only when a main view is in the loop to be owed it. With none
        // — its canvas scrolled off screen, so it stopped, while an extra kept
        // drawing — the debt stands until one comes back, or the main view
        // would return to a surface holding an extra's picture with nothing
        // telling it to redraw.
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

    /// Draws one member, redrawing a clean one when the surface owes it.
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

/// The pass that puts an extra viewport's frame into the tier's surface.
///
/// A render pass and not a copy: `wgpu-hal` advertises `COLOR_TARGET` alone for
/// a GL surface, so it can be drawn into and never copied into. A full-screen
/// triangle sampling the extra's texture, with the pass's viewport set to the
/// rectangle the extra occupies, puts the picture in the top-left and leaves
/// the rest of the surface alone.
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
                    // Replace: the rectangle being written is the extra's whole
                    // picture, and it is opaque.
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

    /// The bind group for one extra viewport's texture, rebuilt whenever that
    /// texture is.
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

    /// Draws `bind`'s texture into the top-left `width` × `height` of `target`.
    ///
    /// `Load`, not a clear: everything outside that rectangle is the main
    /// view's last frame and is about to be redrawn anyway.
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

/// One triangle covering the pass's viewport, sampling the extra's frame.
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

/// Everything a frame touches. Held behind one `RefCell` because the animation
/// callback and the JS-facing methods are two paths into the same state, and
/// the browser runs them on one thread with no overlap.
struct Inner {
    /// The session being drawn. Shared: the renderer and the cache are its,
    /// and another viewport may be showing the same one.
    replica: Rc<RefCell<ReplicaInner>>,
    /// Where this frame is drawn and how it reaches the element: the whole of
    /// what the tier changes, decided once when the viewport is built.
    target: Target,
    stencil: StencilTarget,
    composites: CompositePool,
    list: RenderList,
    camera: Camera,
    size: (u32, u32),
    dirty: bool,
    /// The animation-frame timestamp the last frame drew at, so a readback can
    /// redraw the same one rather than inventing a clock of its own.
    last_ms: f64,
}

/// Where a viewport's frame goes, which is the whole of what a tier changes.
enum Target {
    /// This canvas is its own swapchain: every WebGPU viewport.
    Own(SurfaceContext),
    /// WebGL2, on the canvas the device was acquired from. The tier's surface
    /// is this element's, and a frame is presented into it and left there.
    Main(Rc<GlStage>),
    /// WebGL2, on any other canvas. See [`Extra`].
    Extra(Extra),
}

/// A WebGL2 viewport that is not the device's canvas.
///
/// It renders into a texture of its own, borrows the tier's surface to present
/// that texture, and copies the result onto its own element. The main view
/// takes the surface back in the same animation frame.
struct Extra {
    stage: Rc<GlStage>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The blit's binding for [`Extra::view`], rebuilt with the texture.
    bind: wgpu::BindGroup,
    /// The size the texture was built at: this canvas's, capped to the
    /// surface it borrows.
    rendered: (u32, u32),
    /// The element as a 2D context, which is what a frame is copied onto and
    /// what a readback reads.
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

    /// Rebuilds the colour target when the size it should render changed. A
    /// texture cannot be resized, so this is a new one and a new binding.
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

    /// Copies the rectangle just presented on the tier's canvas onto this one.
    ///
    /// Scaled when this viewport is larger than the surface it borrowed, which
    /// is the cap taking effect; the aspect is preserved by the cap itself, so
    /// this only ever makes the picture softer.
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
            // Neither is an error, and both stay stale so the next frame tries
            // again: a surface that refused a texture was reconfigured by
            // `draw`, and a skipped extra is waiting for a main canvas with a
            // backing store.
            Drawn::NoTexture | Drawn::Skipped => {
                self.dirty = true;
                Ok(false)
            }
            Drawn::Rendered(_) => Ok(true),
        }
    }

    /// Draws the current frame again and takes a copy of it.
    ///
    /// At the timestamp the loop last drew at, so nothing is advanced: this
    /// reads what is on screen rather than a frame of its own. Where the copy
    /// comes from is the tier's, and all three routes answer the same bytes.
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

    /// One frame into this viewport's target, and a copy of it when asked.
    fn draw(&mut self, now_ms: f64, copy: Copy) -> Result<Drawn, String> {
        let (width, height) = self.size;
        // What this frame renders at: its own size, except for an extra, which
        // is capped to the surface it borrows.
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
        // The timestamp, not a `dt`: the replica owns the tick and derives the
        // step from its own last one, so a second canvas in this same
        // animation frame redraws without advancing anything.
        //
        // Stale again while the puppet is still moving itself, so physics
        // settles and an animation plays with nobody calling `invalidate`.
        if replica.frame(now_ms, &mut self.list)?.any() {
            self.dirty = true;
        }

        // The surface this frame presents through: this viewport's own on
        // WebGPU, and the tier's on WebGL2 whether or not it is this canvas's.
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
        // A surface that will not hand over a texture is not an error here: the
        // canvas was resized, or the tab was hidden and came back. Reconfigure
        // and let the caller decide what that means.
        let Some((frame, view)) = surface.acquire() else {
            if let Some(device) = replica.device() {
                surface.reconfigure(device);
            }
            return Ok(Drawn::NoTexture);
        };
        // An extra draws into its own texture and blits that into the surface
        // afterwards; everything else draws into the surface directly.
        let target_view = match &self.target {
            Target::Extra(extra) => &extra.view,
            _ => &view,
        };

        let render = replica
            .render
            .as_mut()
            .ok_or("this viewport's replica lost its renderer")?;
        self.stencil.ensure_size_for_pipelines(
            &render.renderer.shared,
            &render.renderer.device,
            rendered.0,
            rendered.1,
        );
        self.composites.ensure_size(rendered.0, rendered.1);

        let mut encoder =
            render
                .renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("catchlight viewport"),
                });
        render.renderer.begin_camera_submit();
        render.renderer.update_camera(view_proj);
        let result = render.renderer.render_list(
            &self.list,
            &mut encoder,
            target_view,
            &self.stencil,
            &mut self.composites,
            rendered.0,
            rendered.1,
            Some(CLEAR),
        );
        // The borrowed surface, written in the same submission as the frame
        // that fills it, so what is presented is this frame and not the last.
        if let Target::Extra(extra) = &self.target {
            extra
                .stage
                .blit
                .draw(&mut encoder, &view, &extra.bind, rendered.0, rendered.1);
        }
        // Recorded into the same encoder as the pass above, so what is read
        // back is this frame and not the one before it.
        let capture = match copy {
            Copy::Yes => Some(Capture::record(
                &render.renderer.device,
                &mut encoder,
                &frame.texture,
                width,
                height,
            )),
            Copy::No => None,
        };
        render.renderer.queue.submit(Some(encoder.finish()));
        frame.present();
        result.map_err(|e| e.to_string())?;

        // The present above put an extra's picture on the tier's canvas. Take
        // it now, in this same task: the main view is about to draw over it,
        // and the browser must composite only that.
        if let Target::Extra(extra) = &self.target {
            // Told before the copy, not after: the present above clobbered the
            // surface whether or not the copy then succeeds, and a failure
            // that returned early here would leave the main view believing its
            // own picture is still on the canvas.
            extra.stage.clobber();
            extra.take_frame(rendered, (width, height))?;
        }
        Ok(Drawn::Rendered(capture))
    }
}

/// `size` scaled down to fit `cap`, keeping its aspect. Unchanged when it
/// already fits, which is every viewport but an oversized extra.
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

/// Whether a draw also copies the frame out. Two states rather than a `bool`
/// because the argument is read at the call site and `true` says nothing.
#[derive(Clone, Copy)]
enum Copy {
    Yes,
    No,
}

/// What a draw did.
enum Drawn {
    /// The surface refused a texture; it has been reconfigured.
    NoTexture,
    /// There was nowhere to draw: an extra whose main canvas has no backing
    /// store at all.
    Skipped,
    /// A frame reached the canvas, with the copy of it that was asked for.
    Rendered(Option<Capture>),
}

/// A colour texture a frame can be drawn into and then sampled from.
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

/// The 2D context of `canvas`.
///
/// Fails when the element already carries another kind of context — a canvas
/// answers `getContext` for one API and null for every other, which is exactly
/// why the device's own canvas can never be read this way.
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

/// A 2D context on a canvas nobody sees, for reading another canvas back.
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

/// `getImageData`, which answers RGBA8 row-major like every other route here.
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

/// A frame on its way back to JavaScript, however this tier got hold of it.
enum Readback {
    /// WebGPU: the copy is recorded and the buffer still has to be mapped.
    Mapping(Capture),
    /// WebGL2: the pixels, already read off a canvas, with their size.
    Pixels(Vec<u8>, u32, u32),
}

/// `{ width, height, rgba }`, the one shape a readback has on either tier.
fn frame_object(width: u32, height: u32, rgba: &[u8]) -> Result<JsValue, JsValue> {
    let frame = js_sys::Object::new();
    js_sys::Reflect::set(&frame, &"width".into(), &JsValue::from(width))?;
    js_sys::Reflect::set(&frame, &"height".into(), &JsValue::from(height))?;
    js_sys::Reflect::set(&frame, &"rgba".into(), &js_sys::Uint8Array::from(rgba))?;
    Ok(frame.into())
}

/// A frame's pixels on their way back from the GPU: a buffer with the copy
/// already recorded into the frame's own submission, and what it takes to read
/// it as RGBA.
struct Capture {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    /// The buffer's row stride, which `copyTextureToBuffer` rounds up to 256.
    bytes_per_row: u32,
    /// Whether the surface's channel order is BGRA. `bgra8unorm` is what a
    /// real GPU usually prefers and `rgba8unorm` is what llvmpipe reports, so
    /// which one this is says nothing about the picture and everything about
    /// the machine.
    swap_rb: bool,
}

impl Capture {
    /// Records the copy into `encoder`, to be submitted with the pass that
    /// filled `texture`.
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

    /// Waits for the copy, unpads it, and hands JavaScript
    /// `{ width, height, rgba }`.
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

/// `map_async` as a future, through a promise the browser resolves.
///
/// `wgpu`'s mapping callback is not a future and this crate carries no
/// executor to make one out of it; a promise is the one thing both sides
/// already speak. On the web nothing polls the device — the browser resolves
/// the mapping while the future is parked.
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

/// One canvas, drawing one replica.
#[wasm_bindgen]
pub struct Viewport {
    inner: Rc<RefCell<Inner>>,
    frames: Frames,
}

/// What wakes this viewport, which is the tier's choice and not the page's.
enum Frames {
    /// WebGPU: its own animation-frame callback. Independent swapchains draw
    /// in whatever order the browser calls them and nothing can tell.
    Own {
        tick: FrameCallback,
        /// The pending `requestAnimationFrame` handle, or none when stopped.
        pending: Rc<Cell<Option<i32>>>,
    },
    /// WebGL2: a member of the tier's one loop, which draws the extras and
    /// then the main view so the right picture is the one composited.
    Tier { stage: Rc<GlStage>, main: bool },
}

#[wasm_bindgen]
impl Viewport {
    /// Draws `replica` on `canvas`, from now until [`Viewport::stop`].
    ///
    /// Synchronous: the device already exists, and neither configuring a
    /// surface for a device in hand nor building a texture awaits anything.
    /// The canvas's current `width`/`height` are the initial backing store.
    ///
    /// Which of the three shapes this canvas gets is decided here and nowhere
    /// else: its own surface on WebGPU, the tier's surface when it is the
    /// canvas a WebGL2 device came from, and a borrowed one otherwise.
    #[wasm_bindgen(constructor)]
    pub fn new(
        gpu: &Gpu,
        replica: &Replica,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<Viewport, JsValue> {
        let size = (canvas.width().max(1), canvas.height().max(1));

        let (target, frames, rendered) = match &gpu.gl {
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
                    size,
                )
            }
            // WebGL2, on any other canvas: no surface is possible here, so it
            // renders into a texture and borrows the main canvas's surface to
            // present it. This arm is the whole of the extra-viewport
            // mechanism; a build that would rather refuse a second GL viewport
            // than borrow the surface returns an error here and deletes
            // [`Extra`], and nothing else in this file or above it changes.
            Some(stage) => {
                let extra = Extra::new(stage.clone(), &gpu.device, &canvas, size)
                    .map_err(|e| JsValue::from_str(&e))?;
                let rendered = extra.rendered;
                (
                    Target::Extra(extra),
                    Frames::Tier {
                        stage: stage.clone(),
                        main: false,
                    },
                    rendered,
                )
            }
            // WebGPU: one surface per viewport, on this tab's one device. A
            // device there presents to any canvas, so a second viewport on a
            // second element is an ordinary thing to build.
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
                // `configure_surface` asks for a render attachment and nothing
                // else. `COPY_SRC` beside it is what lets
                // [`Viewport::readback`] copy the frame it just drew, and it
                // costs a surface that is never read back nothing. WebGPU
                // only: a GL surface refuses to configure for any usage but
                // `RENDER_ATTACHMENT`, which is why that tier reads a canvas
                // through the DOM instead.
                surface.config.usage |= wgpu::TextureUsages::COPY_SRC;
                surface.reconfigure(&gpu.device);
                (
                    Target::Own(surface),
                    Frames::Own {
                        tick: Rc::new(RefCell::new(None)),
                        pending: Rc::new(Cell::new(None)),
                    },
                    size,
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
        let (stencil, composites) = {
            let mut inner = shared
                .try_borrow_mut()
                .map_err(|_| JsValue::from_str("this replica is busy drawing another canvas"))?;
            let render = inner
                .ensure_renderer(gpu, render_format)
                .map_err(|e| JsValue::from_str(&e))?;
            (
                StencilTarget::new_for_pipelines(
                    &render.renderer.shared,
                    &render.renderer.device,
                    rendered.0,
                    rendered.1,
                ),
                CompositePool::new(rendered.0, rendered.1),
            )
        };

        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                replica: shared,
                target,
                stencil,
                composites,
                list: RenderList::default(),
                // Until `set_camera`, frame the origin at a height that suits
                // a character rig. The editor sets a real one as soon as it
                // knows the model's bounds.
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

    /// The frame on the canvas right now, as `{ width, height, rgba }`.
    ///
    /// **The one asynchronous entry after [`Gpu::acquire`], and it exists for
    /// the browser smoke test.** Nothing in the editor calls it: a viewport's
    /// contract with the page is synchronous and stays that way, so this
    /// neither starts, stops nor invalidates anything, and the frame loop
    /// cannot tell it happened. What it does is draw the current frame once
    /// more, at the timestamp the loop last used so the puppet is not
    /// advanced, and take a copy.
    ///
    /// The rendering half is synchronous and happens before this returns. On
    /// WebGPU the promise is the surface copy being mapped; on WebGL2 there is
    /// nothing left to await, because the pixels are read off a canvas.
    /// `rgba` is always RGBA in that order, whatever channel order the surface
    /// prefers and whichever tier drew, because the test asking has no way to
    /// find out and no business caring.
    ///
    /// It is what a headless Chromium has instead of a screenshot: that
    /// configuration never composites the canvas, so the compositor's copy is
    /// blank while the picture is fine.
    ///
    /// [`Gpu::acquire`]: crate::Gpu::acquire
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

    /// Marks the picture stale. At most one frame follows, however many times
    /// this is called before it runs.
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
            // This canvas's own surface, so configuring it is also what sets
            // the element's backing store.
            Target::Own(surface) => surface.resize(&device, width, height),
            // The tier's surface, which is this canvas's too.
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

    /// Starts the frame loop. Running already is not an error.
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

    /// Stops the frame loop. Stopped already is not an error, and starting
    /// again afterwards works.
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

/// The same, for a callback the tier holds rather than a viewport.
fn request_frame_from(tick: &FrameClosure) -> Option<i32> {
    request_frame(tick.borrow().as_ref()?)
}
