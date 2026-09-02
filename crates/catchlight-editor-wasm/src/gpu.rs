//! One GPU device per editor: [`Gpu`].
//!
//! Invariants this module carries:
//!
//! - **One device for the whole tab, not one per canvas.** Every [`Replica`]
//!   and every [`Viewport`] is built on this one, because GPU resources do not
//!   cross devices: two devices would mean a session's textures uploaded once
//!   per canvas showing it, and a second inspector view would double a
//!   model's video memory. Texture memory is therefore per session, and a
//!   canvas costs a swapchain.
//!
//! - **WebGL2 is a real fallback, not a message.** The instance asks for
//!   `BROWSER_WEBGPU | GL` and the crate builds `wgpu` with its `webgl`
//!   feature, so a browser without WebGPU still draws — through the shader
//!   alpha-discard path `Pipelines::new_autodetect` picks for the GL backend
//!   rather than the stencil one WebGL2 cannot always give.
//!
//! - **The two promises are here and nowhere else.** Asking for an adapter and
//!   for a device are the only asynchronous steps WebGPU has. [`Gpu::acquire`]
//!   awaits both, once, and everything downstream — a replica, a viewport, a
//!   resize, a frame — is synchronous. That is what keeps the frame loop off
//!   the microtask queue.
//!
//! [`Replica`]: crate::Replica
//! [`Viewport`]: crate::Viewport

use wasm_bindgen::prelude::*;

/// The adapter, device and queue every canvas and every replica in this tab
/// share.
#[wasm_bindgen]
pub struct Gpu {
    pub(crate) instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

#[wasm_bindgen]
impl Gpu {
    /// Request an adapter and a device: WebGPU where the browser has it, and
    /// WebGL2 where it does not.
    ///
    /// Rejects with a message naming which of the two failed — the page can
    /// tell "this browser has no GPU API at all" from "the device was refused"
    /// and say something useful either way.
    pub async fn acquire() -> Result<Gpu, JsValue> {
        console_error_panic_hook::set_once();

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            flags: Default::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                // No surface yet: a canvas arrives with the first viewport,
                // and every browser adapter can present to any canvas.
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("no WebGPU or WebGL2 adapter: {e}")))?;

        let optional_features = wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER;
        let required_features = adapter.features() & optional_features;
        // Mobile Safari's WebGPU has no compute support and reports
        // `max_compute_workgroups_per_dimension = 0`; `Limits::default()`
        // demands 65535 and device creation fails. Catchlight is pure render,
        // so take whatever the adapter actually offers.
        let required_limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("catchlight editor"),
                required_features,
                required_limits,
                memory_hints: Default::default(),
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("adapter gave no device: {e}")))?;

        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
        })
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

    /// Which API this actually got: `"webgpu"` or `"webgl2"`. Anything else is
    /// the backend's own name, which on a browser means something is wrong and
    /// the name is what says so.
    pub fn backend(&self) -> String {
        match self.adapter.get_info().backend {
            wgpu::Backend::BrowserWebGpu => "webgpu".to_string(),
            wgpu::Backend::Gl => "webgl2".to_string(),
            other => format!("{other:?}").to_lowercase(),
        }
    }
}
