//! The GPU an editor draws on: [`Gpu`], one device shared by every canvas and
//! replica in the tab, since GPU resources do not cross devices.
//!
//! WebGL2 is tried from a *second* `Instance`: one built with `BROWSER_WEBGPU`
//! while `navigator.gpu` exists reaches no other backend, so a browser whose
//! `requestAdapter` answers null would never fall back. On GL the adapter *is*
//! the first canvas's context, so that canvas is held for the tab's life, and
//! a GL surface advertises only `COLOR_TARGET`, so nothing copies into or out
//! of it.

use std::rc::Rc;
use std::sync::Arc;

use catchlight_wgpu::configure_surface;
use wasm_bindgen::prelude::*;

use crate::viewport::GlStage;

/// Shown verbatim when a tab cannot draw; the browser smoke test asserts it.
pub(crate) const NEEDS_WEBGPU_OR_WEBGL2: &str =
    "the catchlight editor needs WebGPU or WebGL2 to draw and this browser \
     offered neither; use a current Chrome, Edge, Safari or Firefox";

#[wasm_bindgen]
pub struct Gpu {
    instance: wgpu::Instance,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    /// The device's canvas, surface and frame loop on WebGL2; `None` on WebGPU.
    /// Held for the tab's life: dropping the element would take the device.
    pub(crate) gl: Option<Rc<GlStage>>,
}

#[wasm_bindgen]
impl Gpu {
    /// Request an adapter and device: WebGPU first, then WebGL2 from a second
    /// instance. `canvas` is the first viewport's; WebGL2 needs it because its
    /// adapter is that canvas's context.
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

    /// `"webgpu"` or `"webgl2"`, for status lines and bug reports; nothing above
    /// this crate may branch on it.
    pub fn tier(&self) -> String {
        match self.gl {
            Some(_) => "webgl2".to_string(),
            None => "webgpu".to_string(),
        }
    }

    /// The largest surface this device will configure, in device pixels per
    /// axis. A canvas above it goes black with no other symptom.
    #[wasm_bindgen(js_name = maxSize)]
    pub fn max_size(&self) -> u32 {
        self.device.limits().max_texture_dimension_2d
    }
}

impl Gpu {
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

    /// Phase two: WebGL2, whose adapter is `canvas`'s context, so the surface
    /// exists before the adapter and is the only one this device presents to.
    async fn request_gl(canvas: web_sys::HtmlCanvasElement) -> Result<Gpu, String> {
        let instance = instance_for(wgpu::Backends::GL);
        let surface = canvas_surface(&instance, canvas.clone())?;
        let adapter = request_adapter(&instance, Some(&surface)).await?;
        let (device, queue) = request_device(&adapter).await?;
        let (width, height) = crate::viewport::fit_device(
            (canvas.width(), canvas.height()),
            device.limits().max_texture_dimension_2d,
        );
        let surface = configure_surface(&adapter, &device, surface, width, height)
            .map_err(|e| e.to_string())?;
        let stage = GlStage::new(canvas, surface, &device);
        Ok(Gpu {
            instance,
            adapter,
            device,
            queue,
            gl: Some(Rc::new(stage)),
        })
    }

    /// A surface for `canvas`. **WebGPU only**: the GL tier's one surface is made
    /// during acquisition.
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

/// The only place a canvas becomes a surface, so each gets the display handle.
fn canvas_surface(
    instance: &wgpu::Instance,
    canvas: web_sys::HtmlCanvasElement,
) -> Result<wgpu::Surface<'static>, String> {
    instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|e| format!("no surface for the canvas ({e})"))
}

/// An empty web display handle: `wgpu_core::Instance::create_surface` refuses
/// a canvas surface (`MissingDisplayHandle`) when the instance has none. Owned
/// because the descriptor wants `Send + Sync + 'static`.
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
        .map_err(|e| format!("adapter gave no device ({e})"))?;
    // Without a handler wgpu-core (the WebGL2 tier) panics on any validation
    // error, and a panic in wasm leaves every `RefCell` borrowed until reload.
    device.on_uncaptured_error(Arc::new(|error: wgpu::Error| {
        web_sys::console::error_1(&JsValue::from_str(&error.to_string()));
    }));
    Ok((device, queue))
}
