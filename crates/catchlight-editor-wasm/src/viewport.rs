//! One canvas drawing one replica, and the frame loop that drives it.
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
//!   [`Replica`] this draws; what a viewport owns is its surface, its stencil
//!   and its composite pool, all three sized to this canvas alone. Two
//!   viewports on one session therefore cost two swapchains and not two copies
//!   of the model. The puppet is shared the same way, so a frame hands the
//!   replica the animation frame's *timestamp* rather than a `dt` of its own:
//!   [`ReplicaState::frame`](crate::ReplicaState::frame) ticks once per
//!   timestamp and the second canvas in the batch only collects and draws.
//!   Two viewports advancing the same puppet would run its physics and its
//!   animations at twice the rate.
//!
//! [`invalidate`]: Viewport::invalidate
//! [`start`]: Viewport::start
//! [`stop`]: Viewport::stop
//! [`Replica`]: crate::Replica

use std::cell::{Cell, RefCell};
use std::rc::Rc;

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

/// Everything a frame touches. Held behind one `RefCell` because the animation
/// callback and the JS-facing methods are two paths into the same state, and
/// the browser runs them on one thread with no overlap.
struct Inner {
    /// The session being drawn. Shared: the renderer and the cache are its,
    /// and another viewport may be showing the same one.
    replica: Rc<RefCell<ReplicaInner>>,
    surface: SurfaceContext,
    stencil: StencilTarget,
    composites: CompositePool,
    list: RenderList,
    camera: Camera,
    size: (u32, u32),
    dirty: bool,
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

        let (width, height) = self.size;
        let aspect = width as f32 / height.max(1) as f32;
        let view_proj =
            create_orthographic_camera_at(self.camera.height, aspect, self.camera.center);

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

        // A surface that will not hand over a texture is not an error: the
        // canvas was resized, or the tab was hidden and came back. Reconfigure
        // and stay stale so the next frame tries again.
        let Some((frame, view)) = self.surface.acquire() else {
            if let Some(device) = replica.device() {
                self.surface.reconfigure(device);
            }
            self.dirty = true;
            return Ok(false);
        };

        let render = replica
            .render
            .as_mut()
            .ok_or("this viewport's replica lost its renderer")?;
        self.stencil.ensure_size_for_pipelines(
            &render.renderer.shared,
            &render.renderer.device,
            width,
            height,
        );
        self.composites.ensure_size(width, height);

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
            &view,
            &self.stencil,
            &mut self.composites,
            width,
            height,
            Some(CLEAR),
        );
        render.renderer.queue.submit(Some(encoder.finish()));
        frame.present();
        result.map_err(|e| e.to_string())?;
        Ok(true)
    }
}

/// The frame callback, shared between the viewport and the browser's schedule.
type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

/// One canvas, drawing one replica.
#[wasm_bindgen]
pub struct Viewport {
    inner: Rc<RefCell<Inner>>,
    /// The frame callback, kept alive for as long as the loop can run. Dropping
    /// it while the browser still holds a handle would free the closure out
    /// from under a scheduled call.
    tick: FrameCallback,
    /// The pending `requestAnimationFrame` handle, or none when stopped.
    pending: Rc<Cell<Option<i32>>>,
}

#[wasm_bindgen]
impl Viewport {
    /// Draws `replica` on `canvas`, from now until [`Viewport::stop`].
    ///
    /// Synchronous: the device already exists, and configuring a surface for a
    /// device in hand does not await. The canvas's current `width`/`height`
    /// are the initial backing store.
    #[wasm_bindgen(constructor)]
    pub fn new(
        gpu: &Gpu,
        replica: &Replica,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<Viewport, JsValue> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);

        // The GL phase of `Gpu::acquire` already built a surface for the canvas
        // it was given, because that is where its adapter came from. Take it
        // rather than build a second one on the same element.
        let surface = gpu.surface_for(canvas).map_err(|e| JsValue::from_str(&e))?;
        // A WebGL2 device presents to the one canvas its adapter was made
        // from, so a surface on any other canvas has no format in common with
        // it. Caught here because `configure_surface` would index an empty
        // capability list instead of saying so.
        if surface.get_capabilities(&gpu.adapter).formats.is_empty() {
            return Err(JsValue::from_str(if gpu.is_webgl() {
                "WebGL2 draws one canvas per device; a second viewport needs WebGPU"
            } else {
                "this canvas has no surface format in common with the adapter"
            }));
        }
        let surface = configure_surface(&gpu.adapter, &gpu.device, surface, width, height);

        let shared = replica.inner();
        let (stencil, composites) = {
            let mut inner = shared
                .try_borrow_mut()
                .map_err(|_| JsValue::from_str("this replica is busy drawing another canvas"))?;
            let render = inner
                .ensure_renderer(gpu, surface.render_format)
                .map_err(|e| JsValue::from_str(&e))?;
            (
                StencilTarget::new_for_pipelines(
                    &render.renderer.shared,
                    &render.renderer.device,
                    width,
                    height,
                ),
                CompositePool::new(width, height),
            )
        };

        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                replica: shared,
                surface,
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
                size: (width, height),
                dirty: true,
            })),
            tick: Rc::new(RefCell::new(None)),
            pending: Rc::new(Cell::new(None)),
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
        // Borrowed only for the handle: the surface is this viewport's, the
        // device is the tab's, and the frame in flight may hold the replica.
        let Ok(replica) = inner.replica.try_borrow() else {
            return;
        };
        let device = replica.device().cloned();
        drop(replica);
        if let Some(device) = device {
            inner.surface.resize(&device, width, height);
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
        if self.pending.get().is_some() {
            return;
        }
        let inner = self.inner.clone();
        let tick = self.tick.clone();
        let pending = self.pending.clone();
        let closure = Closure::<dyn FnMut(f64)>::new(move |now_ms: f64| {
            // Cleared first: a `stop` from inside the frame (an error handler,
            // a React unmount racing the callback) has to win over the
            // reschedule below.
            pending.set(None);
            if let Ok(mut borrowed) = inner.try_borrow_mut() {
                if let Err(message) = borrowed.frame(now_ms) {
                    web_sys::console::error_1(&JsValue::from_str(&message));
                }
            }
            if let Some(closure) = tick.borrow().as_ref() {
                pending.set(request_frame(closure));
            }
        });
        self.pending.set(request_frame(&closure));
        *self.tick.borrow_mut() = Some(closure);
    }

    /// Stops the frame loop. Stopped already is not an error, and starting
    /// again afterwards works.
    pub fn stop(&self) {
        if let Some(handle) = self.pending.take() {
            if let Some(window) = web_sys::window() {
                window.cancel_animation_frame(handle).ok();
            }
        }
        // Taken out before it drops: the browser is no longer holding a call
        // to it, and the closure captures this same cell, so leaving it in
        // place would be a cycle that keeps the GPU resources alive forever.
        let callback = self.tick.borrow_mut().take();
        drop(callback);
    }
}

/// `free()` from JavaScript lands here. Stopping first is what makes it safe:
/// a scheduled frame holding a freed closure is a trap in wasm, not an error.
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
