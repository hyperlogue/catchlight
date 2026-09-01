//! The renderer on a canvas, and the frame loop that drives it.
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
//! - **Nothing is drawn while nothing is stale.** A started viewport still
//!   wakes on every animation frame, but a clean one returns without touching
//!   the GPU or acquiring a surface texture. An idle editor costs a predicate.
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
//!   the same two numbers, passed in together by whoever resized it.
//!
//! [`invalidate`]: Viewport::invalidate
//! [`start`]: Viewport::start
//! [`stop`]: Viewport::stop

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use catchlight_editor_protocol::SessionId;
use catchlight_editor_server::Editor;
use catchlight_wgpu::{
    create_orthographic_camera_at, create_surface_context, CompositePool, PrepareOptions,
    RenderCache, RenderList, StencilTarget, SurfaceContext, WgpuRenderer,
};
use std::sync::Arc;
use wasm_bindgen::prelude::*;

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
    editor: Arc<Editor>,
    session: SessionId,
    renderer: WgpuRenderer,
    surface: SurfaceContext,
    cache: RenderCache,
    stencil: StencilTarget,
    composites: CompositePool,
    list: RenderList,
    camera: Camera,
    size: (u32, u32),
    dirty: bool,
    /// The timestamp of the last frame that drew, for the puppet's `dt`.
    last_ms: Option<f64>,
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

        let dt = self
            .last_ms
            .map(|last| ((now_ms - last) / 1000.0).clamp(0.0, 0.1) as f32)
            .unwrap_or(0.0);
        self.last_ms = Some(now_ms);

        let (width, height) = self.size;
        let aspect = width as f32 / height.max(1) as f32;
        let view_proj =
            create_orthographic_camera_at(self.camera.height, aspect, self.camera.center);

        let Self {
            editor,
            session,
            renderer,
            cache,
            list,
            ..
        } = self;
        editor
            .with_puppet(*session, |model, puppet| {
                puppet.tick(model, dt);
                cache.refresh(renderer, model, puppet)?;
                cache.collect_into(puppet, list);
                Ok(())
            })
            .map_err(|e| e.to_string())?
            .map_err(|e: catchlight_wgpu::RendererError| e.to_string())?;

        // A surface that will not hand over a texture is not an error: the
        // canvas was resized, or the tab was hidden and came back. Reconfigure
        // and stay stale so the next frame tries again.
        let Some((frame, view)) = self.surface.acquire() else {
            self.surface.reconfigure(&self.renderer.device);
            self.dirty = true;
            return Ok(false);
        };

        self.stencil.ensure_size_for_pipelines(
            &self.renderer.shared,
            &self.renderer.device,
            width,
            height,
        );
        self.composites.ensure_size(width, height);

        let mut encoder =
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("catchlight viewport"),
                });
        self.renderer.begin_camera_submit();
        self.renderer.update_camera(view_proj);
        let result = self.renderer.render_list(
            &self.list,
            &mut encoder,
            &view,
            &self.stencil,
            &mut self.composites,
            width,
            height,
            Some(CLEAR),
        );
        self.renderer.queue.submit(Some(encoder.finish()));
        frame.present();
        result.map_err(|e| e.to_string())?;
        Ok(true)
    }
}

/// The frame callback, shared between the viewport and the browser's schedule.
type FrameCallback = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;

/// One canvas, drawing one session.
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

impl Viewport {
    pub(crate) async fn attach(
        editor: Arc<Editor>,
        session: SessionId,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<Self, String> {
        let width = canvas.width().max(1);
        let height = canvas.height().max(1);

        let instance = wgpu::Instance::default();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| format!("creating a surface for the canvas: {e}"))?;
        let (device, queue, surface) = create_surface_context(&instance, surface, width, height)
            .await
            .map_err(|e| format!("no WebGPU device for this canvas: {e}"))?;

        let mut renderer = WgpuRenderer::new(device, queue, surface.render_format).await;
        let cache = editor
            .with_model(session, |model| {
                RenderCache::prepare(&mut renderer, model, PrepareOptions::default())
            })
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("preparing the render cache: {e}"))?;

        let stencil =
            StencilTarget::new_for_pipelines(&renderer.shared, &renderer.device, width, height);
        let composites = CompositePool::new(width, height);

        Ok(Self {
            inner: Rc::new(RefCell::new(Inner {
                editor,
                session,
                renderer,
                surface,
                cache,
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
                last_ms: None,
            })),
            tick: Rc::new(RefCell::new(None)),
            pending: Rc::new(Cell::new(None)),
        })
    }
}

#[wasm_bindgen]
impl Viewport {
    /// Marks the picture stale. At most one frame follows, however many times
    /// this is called before it runs.
    pub fn invalidate(&self) {
        self.inner.borrow_mut().dirty = true;
    }

    /// The largest backing store this adapter will configure a surface for,
    /// in device pixels on either axis.
    ///
    /// The page cannot work this out for itself: it is an adapter limit, and no
    /// adapter exists until [`attach`] has awaited one. A canvas above it fails
    /// surface configuration and the viewport goes black with no other symptom,
    /// so the size the page reports is clamped to this rather than to a guess.
    ///
    /// [`attach`]: crate::CatchlightEditor::attach
    #[wasm_bindgen(js_name = maxSize)]
    pub fn max_size(&self) -> u32 {
        self.inner
            .borrow()
            .renderer
            .device
            .limits()
            .max_texture_dimension_2d
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
        let device = inner.renderer.device.clone();
        inner.surface.resize(&device, width, height);
        inner.dirty = true;
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
