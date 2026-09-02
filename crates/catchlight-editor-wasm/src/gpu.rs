//! The GPU an editor draws on: [`Gpu`].
//!
//! Invariants this module carries:
//!
//! - **One device per tab on WebGPU, one canvas per device on WebGL2, which is
//!   why [`Gpu::acquire`] takes the first canvas.** Every [`Replica`] and every
//!   [`Viewport`] is built on one device, because GPU resources do not cross
//!   devices: two would mean a session's textures uploaded once per canvas
//!   showing it, and a second inspector view would double a model's video
//!   memory. WebGL2 cannot fully honour that. Its adapter *is* a canvas's
//!   `WebGL2RenderingContext` — `wgpu-hal`'s GLES backend enumerates no adapter
//!   at all without a surface to take one from, and its `present` blits into
//!   the default framebuffer of the context the adapter was made with, not of
//!   the surface being presented. So on that backend a device draws the one
//!   canvas it was born from, and a second viewport is an error rather than a
//!   black rectangle. The canvas is therefore an argument here: it is what the
//!   GL path needs to find an adapter at all, and the surface it produces is
//!   kept for the first viewport rather than being built twice.
//!
//! - **WebGL2 is a real fallback, not a message, and it takes two instances.**
//!   The crate builds `wgpu` with its `webgl` feature so the GL backend is
//!   compiled in, and [`Gpu::acquire`] tries WebGPU first and GL second — from
//!   a *second* `Instance`, not from one descriptor naming both backends.
//!   `Instance::new` hands back its WebGPU-only context whenever
//!   `BROWSER_WEBGPU` is requested and `navigator.gpu` exists, and that context
//!   can reach no other backend: it reports every one of them as "not compiled
//!   in" however the descriptor was written. So a browser that defines
//!   `navigator.gpu` but whose `requestAdapter` answers null — a headless
//!   Chromium, Chrome on Linux with no allowlisted GPU, Firefox with the pref
//!   on and no support — would fail at the first step and never try the
//!   second. Asking twice is the only way the fallback is reachable at all.
//!   The GL path then draws through the shader alpha-discard path
//!   `Pipelines::new_autodetect` picks for that backend, rather than the
//!   stencil one WebGL2 cannot always give.
//!
//! - **The GL instance carries a web display handle; the WebGPU one needs
//!   none.** `SurfaceTarget::Canvas` asks `wgpu-core` for a surface with no
//!   display, and `wgpu_core::Instance::create_surface` refuses that when the
//!   instance has none either — `MissingDisplayHandle`. Only the GL phase goes
//!   through `wgpu-core`, which is why the WebGPU path never noticed. So the
//!   GL instance is built with [`WebDisplay`] in its descriptor, and every
//!   canvas surface in this crate is created through [`Gpu::surface_for`] so
//!   the first one and a viewport's later one both come from that instance.
//!
//! - **The two promises are here and nowhere else.** Asking for an adapter and
//!   for a device are the only asynchronous steps WebGPU has. [`Gpu::acquire`]
//!   awaits both, once, and everything downstream — a replica, a viewport, a
//!   resize, a frame — is synchronous. That is what keeps the frame loop off
//!   the microtask queue.
//!
//! [`Replica`]: crate::Replica
//! [`Viewport`]: crate::Viewport

use std::cell::RefCell;

use wasm_bindgen::prelude::*;

/// The adapter, device and queue every canvas and every replica in this tab
/// share.
#[wasm_bindgen]
pub struct Gpu {
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    /// The surface the GL phase had to build to find an adapter, with the
    /// canvas element it belongs to. The first [`Viewport`] on that canvas
    /// takes it; on the WebGPU path there is nothing here, because that phase
    /// needs no surface to choose an adapter.
    ///
    /// [`Viewport`]: crate::Viewport
    held: RefCell<Option<(web_sys::HtmlCanvasElement, wgpu::Surface<'static>)>>,
}

#[wasm_bindgen]
impl Gpu {
    /// Request an adapter and a device: WebGPU where the browser has it, and
    /// WebGL2 where it does not.
    ///
    /// `canvas` is the one the first [`Viewport`] will draw. WebGPU ignores it
    /// — any adapter presents to any canvas — and WebGL2 cannot proceed
    /// without it; see the module doc.
    ///
    /// Two attempts against two instances, because one descriptor naming both
    /// backends only ever tries WebGPU. Rejects with a single message naming
    /// what each attempt said, so a page can tell "this browser has no GPU API
    /// at all" from "WebGPU is there but gave no adapter" and say something
    /// useful either way.
    ///
    /// [`Viewport`]: crate::Viewport
    pub async fn acquire(canvas: web_sys::HtmlCanvasElement) -> Result<Gpu, JsValue> {
        console_error_panic_hook::set_once();

        let webgpu = match Self::request_webgpu().await {
            Ok(gpu) => return Ok(gpu),
            Err(message) => message,
        };
        let gl = match Self::request_gl(canvas).await {
            Ok(gpu) => return Ok(gpu),
            Err(message) => message,
        };
        Err(JsValue::from_str(&format!(
            "no GPU for this page. WebGPU: {webgpu}. WebGL2: {gl}"
        )))
    }

    /// The largest backing store this device will configure a surface for, in
    /// device pixels on either axis.
    ///
    /// The page cannot work this out for itself: it is an adapter limit, and
    /// no adapter exists until [`Gpu::acquire`] has awaited one. A canvas
    /// above it fails surface configuration and the viewport goes black with
    /// no other symptom, so a page clamps the size it reports to this rather
    /// than to a guess.
    #[wasm_bindgen(js_name = maxSize)]
    pub fn max_size(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }

    /// Which phase won: `"webgpu"` or `"webgl2"`. Anything else is the
    /// backend's own name, which on a browser means something is wrong and the
    /// name is what says so.
    pub fn backend(&self) -> String {
        match self.adapter.get_info().backend {
            wgpu::Backend::BrowserWebGpu => "webgpu".to_string(),
            wgpu::Backend::Gl => "webgl2".to_string(),
            other => format!("{other:?}").to_lowercase(),
        }
    }
}

impl Gpu {
    /// Phase one: WebGPU, which chooses an adapter with no surface in hand.
    async fn request_webgpu() -> Result<Gpu, String> {
        let instance = instance_for(wgpu::Backends::BROWSER_WEBGPU);
        let adapter = request_adapter(&instance, None).await?;
        let (device, queue) = request_device(&adapter).await?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            held: RefCell::new(None),
        })
    }

    /// Phase two: WebGL2, whose adapter is a canvas's rendering context, so
    /// the surface has to exist before the adapter does. It is kept for the
    /// viewport that will draw this canvas.
    async fn request_gl(canvas: web_sys::HtmlCanvasElement) -> Result<Gpu, String> {
        let instance = instance_for(wgpu::Backends::GL);
        let surface = canvas_surface(&instance, canvas.clone())?;
        let adapter = request_adapter(&instance, Some(&surface)).await?;
        let (device, queue) = request_device(&adapter).await?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            held: RefCell::new(Some((canvas, surface))),
        })
    }

    /// The surface for `canvas`: the one the GL phase already built if this is
    /// that canvas, and otherwise a new one on this instance.
    ///
    /// The single door, because both halves have a condition attached. The
    /// held surface is *taken*, not cloned — a surface configured twice is a
    /// surface configured wrong — and it is matched by JavaScript identity,
    /// since two `HtmlCanvasElement` handles to one element are different Rust
    /// values and the same object. A new one has to come from this instance,
    /// the one carrying the display handle `wgpu-core` insists on.
    pub(crate) fn surface_for(
        &self,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<wgpu::Surface<'static>, String> {
        let held = {
            let mut held = self.held.borrow_mut();
            let same = held
                .as_ref()
                .is_some_and(|(held, _)| js_sys::Object::is(held.as_ref(), canvas.as_ref()));
            same.then(|| held.take().map(|(_, surface)| surface))
                .flatten()
        };
        match held {
            Some(surface) => Ok(surface),
            None => canvas_surface(&self.instance, canvas),
        }
    }

    /// Whether this device draws through WebGL2, which bounds it to the one
    /// canvas it was acquired with.
    pub(crate) fn is_webgl(&self) -> bool {
        self.adapter.get_info().backend == wgpu::Backend::Gl
    }
}

fn instance_for(backends: wgpu::Backends) -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: Default::default(),
        memory_budget_thresholds: Default::default(),
        backend_options: Default::default(),
        // Only the GL phase reaches `wgpu-core`, and only `wgpu-core` demands
        // this. The WebGPU context ignores the whole descriptor field.
        display: Some(Box::new(WebDisplay)),
    })
}

/// A canvas surface on `instance`. The one place this crate turns a canvas
/// into a surface, so the display handle above covers every one of them.
fn canvas_surface(
    instance: &wgpu::Instance,
    canvas: web_sys::HtmlCanvasElement,
) -> Result<wgpu::Surface<'static>, String> {
    instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| format!("no surface for the canvas ({e})"))
}

/// The browser as a display, for the one question `wgpu-core` asks before it
/// will build a surface.
///
/// A web display handle is **empty** — `WebDisplayHandle` is a struct with no
/// fields, because a page has no display connection, file descriptor or
/// pointer to name. `wgpu-core` only checks that the instance has one at all
/// (and that it matches the surface's, when both are given), and `wgpu-hal`'s
/// GLES web backend takes the parameter as `_display_handle` and never reads
/// it. So this is a token, and the whole reason it must exist is
/// `wgpu_core::Instance::create_surface`'s `(None, None) =>
/// MissingDisplayHandle` arm. `DisplayHandle::web` is the safe constructor
/// `raw-window-handle` ships for exactly this; the struct is still needed
/// because the descriptor wants `Send + Sync + 'static`, which a borrowed
/// handle is not.
#[derive(Debug)]
struct WebDisplay;

impl wgpu::rwh::HasDisplayHandle for WebDisplay {
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        Ok(wgpu::rwh::DisplayHandle::web())
    }
}

async fn request_adapter(
    instance: &wgpu::Instance,
    compatible_surface: Option<&wgpu::Surface<'static>>,
) -> Result<wgpu::Adapter, String> {
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface,
            force_fallback_adapter: false,
        })
        .await
        .map_err(|e| format!("no adapter ({e})"))
}

async fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), String> {
    let optional_features = wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER;
    let required_features = adapter.features() & optional_features;
    // Mobile Safari's WebGPU has no compute support and reports
    // `max_compute_workgroups_per_dimension = 0`; `Limits::default()` demands
    // 65535 and device creation fails. Catchlight is pure render, so take
    // whatever the adapter actually offers.
    let required_limits = adapter.limits();
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("catchlight editor"),
            required_features,
            required_limits,
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::default(),
        })
        .await
        .map_err(|e| format!("adapter gave no device ({e})"))
}
