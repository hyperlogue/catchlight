//! The GPU an editor draws on: [`Gpu`].
//!
//! Invariants this module carries:
//!
//! - **One device per tab, and every canvas draws on it.** Every [`Replica`]
//!   and every [`Viewport`] is built on one device, because GPU resources do
//!   not cross devices: two would mean a session's textures uploaded once per
//!   canvas showing it, and a second inspector view would double a model's
//!   video memory. WebGPU imposes no tie between an adapter and a canvas — a
//!   device presents to any number of them — so [`Gpu::acquire`] takes no
//!   canvas at all and a [`Viewport`] makes its own surface for whichever
//!   element it was handed.
//!
//! - **WebGL2 was a tier and is not one any more.** The crate used to compile
//!   `wgpu`'s `webgl` feature and fall back to it, which cost the design above:
//!   a GLES adapter *is* a canvas's `WebGL2RenderingContext`, `present` blits
//!   into the framebuffer of the context the adapter was made with rather than
//!   of the surface being presented, and so one device could only ever draw
//!   the one canvas it was born from. A second viewport was an error on that
//!   tier, the editor shell had to keep a single canvas mounted for its whole
//!   life to work around it, and the shared-device design was a promise the
//!   fallback could not keep. WebGPU ships in current Chrome, Edge, Safari and
//!   Firefox on the desktop platforms this editor targets, so the tier is
//!   gone: a browser without it is told what it needs rather than shown a
//!   degraded editor. The runtime is not affected — `catchlight-wgpu` keeps
//!   its GL fallbacks for everything that is not the editor, and
//!   `Pipelines::new_autodetect` still picks the path a backend can draw.
//!
//! - **Failing to find WebGPU is one message.** `navigator.gpu` missing and
//!   `requestAdapter` answering null are the same problem to the person
//!   reading it, and neither is recoverable here, so both reject with
//!   [`NEEDS_WEBGPU`] and a parenthesised detail. The page shows the string; it
//!   does not have to classify it.
//!
//! - **The two promises are here and nowhere else.** Asking for an adapter and
//!   for a device are the only asynchronous steps WebGPU has. [`Gpu::acquire`]
//!   awaits both, once, and everything downstream — a replica, a viewport, a
//!   resize, a frame — is synchronous. That is what keeps the frame loop off
//!   the microtask queue. The one later exception is
//!   [`Viewport::readback`](crate::Viewport::readback), which exists for the
//!   smoke test and maps a buffer.
//!
//! [`Replica`]: crate::Replica
//! [`Viewport`]: crate::Viewport

use wasm_bindgen::prelude::*;

/// What a tab that cannot draw says, verbatim, to whoever mounted it.
///
/// User-facing and stable: the site prints it, and the browser smoke test
/// asserts that a Chromium launched without WebGPU produces exactly this
/// rather than a blank canvas nobody explains.
pub(crate) const NEEDS_WEBGPU: &str = "this browser has no WebGPU, which the catchlight editor \
     requires; use a current Chrome, Edge, Safari or Firefox";

/// The adapter, device and queue every canvas and every replica in this tab
/// share.
#[wasm_bindgen]
pub struct Gpu {
    instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

#[wasm_bindgen]
impl Gpu {
    /// Requests an adapter and a device.
    ///
    /// No canvas: a WebGPU adapter is chosen without a surface, and the device
    /// it hands back presents to any canvas a [`Viewport`] is later built on.
    ///
    /// Rejects with [`NEEDS_WEBGPU`] and the step that failed, which is the
    /// whole vocabulary a page needs: there is no second tier to fall to.
    ///
    /// [`Viewport`]: crate::Viewport
    pub async fn acquire() -> Result<Gpu, JsValue> {
        console_error_panic_hook::set_once();
        Self::request()
            .await
            .map_err(|detail| JsValue::from_str(&format!("{NEEDS_WEBGPU} ({detail})")))
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
    async fn request() -> Result<Gpu, String> {
        // No display handle: only `wgpu-core` demands one before it will build
        // a surface, and with the GL backend gone nothing here reaches it.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            flags: Default::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                // None: an adapter is picked before any canvas exists, which
                // is the whole reason `acquire` needs no element.
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| format!("no adapter: {e}"))?;
        let (device, queue) = request_device(&adapter).await?;
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// A surface for `canvas` on this tab's instance.
    ///
    /// One per viewport, built when the viewport is: a surface configured
    /// twice is a surface configured wrong, so nothing here is cached or
    /// handed out a second time.
    pub(crate) fn surface_for(
        &self,
        canvas: web_sys::HtmlCanvasElement,
    ) -> Result<wgpu::Surface<'static>, String> {
        self.instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .map_err(|e| format!("no surface for the canvas ({e})"))
    }
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
