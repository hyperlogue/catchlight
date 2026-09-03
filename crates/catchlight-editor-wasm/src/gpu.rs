//! The GPU an editor draws on: [`Gpu`].
//!
//! Invariants this module carries:
//!
//! - **One device per tab, and every canvas draws on it.** Every [`Replica`]
//!   and every [`Viewport`] is built on one device, because GPU resources do
//!   not cross devices: two would mean a session's textures uploaded once per
//!   canvas showing it, and a second inspector view would double a model's
//!   video memory. Both tiers keep that; what differs is how a canvas that is
//!   not the device's own gets its picture, which is [`Viewport`]'s business
//!   and nothing above it.
//!
//! - **WebGL2 is a real fallback, not a message, and it takes two instances.**
//!   The crate builds `wgpu` with its `webgl` feature so the GL backend is
//!   compiled in, and [`Gpu::acquire`] tries WebGPU first and GL second — from
//!   a *second* `Instance`, not from one descriptor naming both backends.
//!   `Instance::new` hands back its WebGPU-only context whenever
//!   `BROWSER_WEBGPU` is requested and `navigator.gpu` exists, and that context
//!   can reach no other backend: it reports every one of them as "not compiled
//!   in" however the descriptor was written. So a browser that defines
//!   `navigator.gpu` but whose `requestAdapter` answers null — an iOS device
//!   below Safari 26, a headless Chromium, Chrome on Linux with no allowlisted
//!   GPU — would fail at the first step and never try the second. Asking twice
//!   is the only way the fallback is reachable at all. The GL path then draws
//!   through the shader alpha-discard path `Pipelines::new_autodetect` picks
//!   for that backend, rather than the stencil one WebGL2 cannot always give.
//!
//! - **On the GL tier the first canvas *is* the device, which is why
//!   [`Gpu::acquire`] takes one.** A GLES adapter is a canvas's
//!   `WebGL2RenderingContext`: `wgpu-hal` enumerates no adapter without a
//!   surface to take one from, and reports no capabilities at all for a surface
//!   whose context is not the adapter's — so a device there draws exactly the
//!   canvas it was born from, and a second canvas can never have a surface. The
//!   choice is therefore which canvas that is, and it is the first one the
//!   editor attaches: the common case, one canvas, then costs nothing at all.
//!   That viewport presents into its own element exactly as a WebGPU viewport
//!   does. WebGPU ignores the argument, where an adapter is chosen with no
//!   surface in hand and presents to any number of canvases.
//!
//! - **The device canvas outlives every viewport, including its own.**
//!   [`GlStage`] holds the element, so unmounting the first canvas neither
//!   loses the device nor asks for a second one: the context the adapter *is*
//!   stays alive because this holds the element it belongs to. It has to
//!   outlive the extras as well as its own viewport, because they present
//!   through its surface — see [`Viewport`]. Nothing re-acquires, and a
//!   viewport that mounts later on a fresh canvas is an ordinary extra.
//!
//! - **A GL surface is a render target and nothing else.** `wgpu-hal`
//!   advertises `COLOR_TARGET` alone in a GL surface's capabilities, and
//!   `wgpu-core` refuses to configure a surface for any usage outside what it
//!   advertises — `COPY_SRC` there fails with "not in the list of supported
//!   usages" before a single frame. So on this tier nothing copies out of the
//!   surface and nothing copies into it. It is why a second viewport cannot be
//!   stamped into the device canvas, and why reading the device canvas back
//!   goes through the DOM rather than through wgpu; see [`Viewport`].
//!
//! - **The GL instance carries a web display handle; the WebGPU one needs
//!   none.** `SurfaceTarget::Canvas` asks `wgpu-core` for a surface with no
//!   display, and `wgpu_core::Instance::create_surface` refuses that when the
//!   instance has none either — `MissingDisplayHandle`. Only the GL phase goes
//!   through `wgpu-core`, which is why the WebGPU path never noticed. So the
//!   GL instance is built with [`WebDisplay`] in its descriptor, and every
//!   canvas surface in this crate is created through [`canvas_surface`].
//!
//! - **Having neither tier is one message.** `navigator.gpu` missing, a
//!   `requestAdapter` that answers null, and a browser with no WebGL2 either
//!   are the same problem to the person reading it, and none of them is
//!   recoverable here, so the rejection is [`NEEDS_WEBGPU_OR_WEBGL2`] with
//!   what each attempt said in parentheses. The page shows the string; it does
//!   not have to classify it. Which tier *did* come up is [`Gpu::tier`], and
//!   that is a fact a status line reports rather than a failure.
//!
//! - **The two promises are here and nowhere else.** Asking for an adapter and
//!   for a device are the only asynchronous steps either tier has.
//!   [`Gpu::acquire`] awaits both, once, and everything downstream — a replica,
//!   a viewport, a resize, a frame — is synchronous. That is what keeps the
//!   frame loop off the microtask queue. The one later exception is
//!   [`Viewport::readback`](crate::Viewport::readback), which exists for the
//!   smoke test.
//!
//! [`Replica`]: crate::Replica
//! [`Viewport`]: crate::Viewport

use std::rc::Rc;

use catchlight_wgpu::configure_surface;
use wasm_bindgen::prelude::*;

use crate::viewport::GlStage;

/// What a tab that cannot draw at all says, verbatim, to whoever mounted it.
///
/// User-facing and stable: the site prints it, and the browser smoke test
/// asserts that a Chromium launched with both tiers switched off produces this
/// rather than a blank canvas nobody explains.
pub(crate) const NEEDS_WEBGPU_OR_WEBGL2: &str =
    "the catchlight editor needs WebGPU or WebGL2 to draw and this browser \
     offered neither; use a current Chrome, Edge, Safari or Firefox";

/// The adapter, device and queue every canvas and every replica in this tab
/// share.
#[wasm_bindgen]
pub struct Gpu {
    instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    /// The device's canvas, its surface and the tier's frame loop, on WebGL2.
    /// `None` is the whole of what "this is WebGPU" means downstream.
    ///
    /// Shared with every viewport on the tier and held here for the life of
    /// the tab: the adapter *is* that canvas's rendering context, so dropping
    /// the element would take the device with it, however many other canvases
    /// are drawing on it.
    pub(crate) gl: Option<Rc<GlStage>>,
}

#[wasm_bindgen]
impl Gpu {
    /// Request an adapter and a device: WebGPU where the browser has it, and
    /// WebGL2 where it does not.
    ///
    /// `canvas` is the one the first [`Viewport`] will draw. WebGPU ignores it
    /// — any adapter presents to any canvas — and WebGL2 cannot proceed
    /// without it, because there an adapter is a canvas's rendering context;
    /// see the module doc.
    ///
    /// Two attempts against two instances, because one descriptor naming both
    /// backends only ever tries WebGPU. Rejects with
    /// [`NEEDS_WEBGPU_OR_WEBGL2`] and what each attempt said, so a bug report
    /// carries which half failed and how.
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
            "{NEEDS_WEBGPU_OR_WEBGL2} (WebGPU: {webgpu}. WebGL2: {gl})"
        )))
    }

    /// Which tier came up: `"webgpu"` or `"webgl2"`.
    ///
    /// A fact about this tab that nothing on the screen otherwise shows, and
    /// the two fail differently enough that a bug report wants it. Nothing
    /// above this may branch on it — what the tier changes is handled here and
    /// in [`Viewport`](crate::Viewport), which is the point of there being one.
    pub fn tier(&self) -> String {
        match self.gl {
            Some(_) => "webgl2".to_string(),
            None => "webgpu".to_string(),
        }
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
}

impl Gpu {
    /// Phase one: WebGPU, which chooses an adapter with no surface in hand and
    /// presents into any canvas a viewport is later built on.
    async fn request_webgpu() -> Result<Gpu, String> {
        let instance = instance_for(wgpu::Backends::BROWSER_WEBGPU);
        let adapter = request_adapter(&instance, None).await?;
        let (device, queue) = request_device(&adapter).await?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            gl: None,
        })
    }

    /// Phase two: WebGL2, whose adapter is `canvas`'s rendering context — so
    /// the surface has to exist before the adapter does, and that canvas is
    /// the one this device can ever present into.
    ///
    /// The surface is configured to the canvas's current backing store, which
    /// the page set before attaching. The viewport built on this canvas
    /// resizes it from there like any other; nothing else ever does.
    async fn request_gl(canvas: web_sys::HtmlCanvasElement) -> Result<Gpu, String> {
        let instance = instance_for(wgpu::Backends::GL);
        let surface = canvas_surface(&instance, canvas.clone())?;
        let adapter = request_adapter(&instance, Some(&surface)).await?;
        let (device, queue) = request_device(&adapter).await?;
        let (width, height) = (canvas.width().max(1), canvas.height().max(1));
        let surface = configure_surface(&adapter, &device, surface, width, height);
        let stage = GlStage::new(canvas, surface, &device);
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            gl: Some(Rc::new(stage)),
        })
    }

    /// A surface for `canvas` on this tab's instance. **WebGPU only** — the GL
    /// tier's one surface is made during acquisition and no second canvas
    /// there can have one at all.
    ///
    /// One per viewport, built when the viewport is: a surface configured
    /// twice is a surface configured wrong, so nothing here is cached or
    /// handed out a second time.
    pub(crate) fn surface_for(
        &self,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<wgpu::Surface<'static>, String> {
        canvas_surface(&self.instance, canvas)
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
