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
//! - **A viewport owns its surface, so a second canvas is ordinary.** The
//!   canvas becomes a surface here, on the tab's one WebGPU device, and
//!   nothing about that device is bound to any element. Two viewports on one
//!   replica are two swapchains sharing one renderer and one cache; the
//!   editor's habit of keeping a single canvas mounted is now a convenience
//!   rather than the constraint it was on the WebGL2 tier (see [`Gpu`]).
//!
//! - **[`readback`] is the one asynchronous entry, and the loop cannot tell
//!   it happened.** It draws the current frame again, at the timestamp the
//!   loop last used so nothing is advanced, and copies the surface texture out
//!   in the same submission; only the buffer mapping is awaited. It exists
//!   because a headless Chromium holding a WebGPU device never composites the
//!   canvas — a screenshot of one is blank while the picture is correct — so
//!   the smoke test has no other way to see what was drawn. Nothing in the
//!   editor calls it, and it starts, stops and invalidates nothing.
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
//! [`readback`]: Viewport::readback
//! [`start`]: Viewport::start
//! [`stop`]: Viewport::stop
//! [`Gpu`]: crate::Gpu
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
    /// The animation-frame timestamp the last frame drew at, so a readback can
    /// redraw the same one rather than inventing a clock of its own.
    last_ms: f64,
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
            // A surface that will not hand over a texture is not an error: the
            // canvas was resized, or the tab was hidden and came back. `draw`
            // reconfigured it; stay stale so the next frame tries again.
            Drawn::NoTexture => {
                self.dirty = true;
                Ok(false)
            }
            Drawn::Rendered(_) => Ok(true),
        }
    }

    /// Draws the current frame again and copies it out of the surface.
    ///
    /// At the timestamp the loop last drew at, so nothing is advanced: this
    /// reads what is on screen rather than a frame of its own.
    fn capture(&mut self) -> Result<Capture, String> {
        match self.draw(self.last_ms, Copy::Yes)? {
            Drawn::Rendered(Some(capture)) => Ok(capture),
            _ => Err("the surface handed over no texture to read back; \
                 the canvas was resized, or the tab is hidden"
                .to_string()),
        }
    }

    /// One frame into the surface texture, and a copy of it when asked.
    fn draw(&mut self, now_ms: f64, copy: Copy) -> Result<Drawn, String> {
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

        // A surface that will not hand over a texture is not an error here: the
        // canvas was resized, or the tab was hidden and came back. Reconfigure
        // and let the caller decide what that means.
        let Some((frame, view)) = self.surface.acquire() else {
            if let Some(device) = replica.device() {
                self.surface.reconfigure(device);
            }
            return Ok(Drawn::NoTexture);
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
        Ok(Drawn::Rendered(capture))
    }
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
    /// A frame reached the canvas, with the copy of it that was asked for.
    Rendered(Option<Capture>),
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

        let frame = js_sys::Object::new();
        js_sys::Reflect::set(&frame, &"width".into(), &JsValue::from(self.width))?;
        js_sys::Reflect::set(&frame, &"height".into(), &JsValue::from(self.height))?;
        js_sys::Reflect::set(
            &frame,
            &"rgba".into(),
            &js_sys::Uint8Array::from(rgba.as_slice()),
        )?;
        Ok(frame.into())
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

        // One surface per viewport, on this tab's one device: a WebGPU device
        // presents to any canvas, so a second viewport on a second element is
        // an ordinary thing to build rather than an error.
        let surface = gpu.surface_for(canvas).map_err(|e| JsValue::from_str(&e))?;
        // Caught here because `configure_surface` would index an empty
        // capability list instead of saying so.
        if surface.get_capabilities(&gpu.adapter).formats.is_empty() {
            return Err(JsValue::from_str(
                "this canvas has no surface format in common with the adapter",
            ));
        }
        let mut surface = configure_surface(&gpu.adapter, &gpu.device, surface, width, height);
        // `configure_surface` asks for a render attachment and nothing else.
        // `COPY_SRC` beside it is what lets [`Viewport::readback`] copy the
        // frame it just drew, and it costs a surface that is never read back
        // nothing.
        surface.config.usage |= wgpu::TextureUsages::COPY_SRC;
        surface.reconfigure(&gpu.device);

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
                last_ms: 0.0,
            })),
            tick: Rc::new(RefCell::new(None)),
            pending: Rc::new(Cell::new(None)),
        })
    }

    /// The frame on the canvas right now, as `{ width, height, rgba }`.
    ///
    /// **The one asynchronous entry after [`Gpu::acquire`], and it exists for
    /// the browser smoke test.** Nothing in the editor calls it: a viewport's
    /// contract with the page is synchronous and stays that way, so this
    /// neither starts, stops nor invalidates anything, and the frame loop
    /// cannot tell it happened. What it does is draw the current frame once
    /// more — at the timestamp the loop last used, so the puppet is not
    /// advanced — and copy the surface texture into a buffer in that same
    /// submission.
    ///
    /// The rendering half is synchronous and happens before this returns; the
    /// promise is only the buffer being mapped. `rgba` is always RGBA in that
    /// order, whatever channel order the surface prefers, because the test
    /// asking has no way to find out and no business caring.
    ///
    /// It is what a headless Chromium has instead of a screenshot: that
    /// configuration never composites the canvas, so the compositor's copy is
    /// blank while the picture is fine.
    ///
    /// [`Gpu::acquire`]: crate::Gpu::acquire
    pub fn readback(&self) -> Result<js_sys::Promise, JsValue> {
        let capture = self
            .inner
            .try_borrow_mut()
            .map_err(|_| JsValue::from_str("this viewport is in the middle of a frame"))?
            .capture()
            .map_err(|e| JsValue::from_str(&e))?;
        Ok(wasm_bindgen_futures::future_to_promise(capture.finish()))
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
