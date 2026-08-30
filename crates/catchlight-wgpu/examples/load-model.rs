#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! winit viewer for a `.clm` rig — the interactive way to look at one.
//!
//! `--control` (or the `C` key) opens the egui panel of per-param sliders and
//! stops animation playback.

use catchlight_core::{
    formats::InxModel, importer::from_inx_model_to_legacy, Model, ModelFormat, ParamId, Puppet,
};
use catchlight_wgpu::{
    collect, create_orthographic_camera_at, create_surface_context, PrepareOptions, RenderCache,
    SurfaceContext, WgpuRenderer,
};
use std::path::Path;
use std::sync::Arc;

/// Load a model file into a [`Model`]. `.inx` goes through the importer's
/// legacy-document conversion, which is the one path an `.inx` still has.
fn load_model_file(path: &Path) -> Result<Model, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    match ModelFormat::from_path(path) {
        Some(ModelFormat::Clm) => Ok(Model::from_clm_bytes(&bytes)?),
        Some(ModelFormat::Inx) => {
            let inx = InxModel::parse(std::io::Cursor::new(bytes))?;
            Ok(Model::from_legacy(&from_inx_model_to_legacy(&inx)?)?)
        }
        _ => Err(format!("unsupported model file: {}", path.display()).into()),
    }
}
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowId},
};

const INITIAL_CAMERA_HEIGHT: f32 = 8000.0;
const MIN_CAMERA_HEIGHT: f32 = 500.0;
const MAX_CAMERA_HEIGHT: f32 = 40000.0;

/// One puppet param's slider state. Values are owned here rather than read
/// back from the puppet, so they stay a stable, single source of truth even
/// when physics overwrites its own target params on the frames it runs.
struct ParamSlider {
    param: ParamId,
    name: String,
    min: f32,
    max: f32,
    default: f32,
    value: f32,
}

fn param_group(name: &str) -> &str {
    name.split(" - ").next().unwrap_or(name)
}

/// egui integration for the direct-control mode, wired into the example's
/// own winit + wgpu loop.
struct ControlUi {
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    physics_enabled: bool,
    param_sliders: Vec<ParamSlider>,
}

impl ControlUi {
    fn new(
        device: &wgpu::Device,
        output_format: wgpu::TextureFormat,
        window: &Window,
        model: &Model,
    ) -> Self {
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer =
            egui_wgpu::Renderer::new(device, output_format, egui_wgpu::RendererOptions::default());

        let mut param_sliders: Vec<ParamSlider> = model
            .param_ids()
            .iter()
            .filter_map(|id| {
                let p = model.param(id)?;
                Some(ParamSlider {
                    param: id.clone(),
                    name: p.name.to_string(),
                    min: p.min,
                    max: p.max,
                    default: p.default,
                    value: p.default,
                })
            })
            .collect();
        // Sorted so params sharing a "Group - Name" prefix land adjacent,
        // which is what the chunk_by_mut grouping in `prepare` below relies on.
        param_sliders.sort_by(|a, b| a.name.cmp(&b.name));

        Self {
            egui_ctx,
            egui_state,
            egui_renderer,
            physics_enabled: true,
            param_sliders,
        }
    }

    /// Builds this frame's panel; slider edits land directly in
    /// `param_sliders` for the caller to fold into the puppet's params.
    fn prepare(&mut self, window: &Window) -> egui::FullOutput {
        let raw_input = self.egui_state.take_egui_input(window);
        let physics_enabled = &mut self.physics_enabled;
        let param_sliders = &mut self.param_sliders;
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            egui::Panel::right("catchlight-control-panel")
                .default_size(340.0)
                .show_inside(ui, |ui| {
                    ui.heading("Direct Control");
                    ui.checkbox(physics_enabled, "Physics");
                    if ui.button("Reset all to defaults").clicked() {
                        for slider in param_sliders.iter_mut() {
                            slider.value = slider.default;
                        }
                    }
                    ui.separator();
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for group in param_sliders
                            .chunk_by_mut(|a, b| param_group(&a.name) == param_group(&b.name))
                        {
                            egui::CollapsingHeader::new(param_group(&group[0].name))
                                .default_open(true)
                                .show(ui, |ui| {
                                    for slider in group.iter_mut() {
                                        ui.label(&slider.name);
                                        ui.add(egui::Slider::new(
                                            &mut slider.value,
                                            slider.min..=slider.max,
                                        ));
                                    }
                                });
                        }
                    });
                });
        });
        self.egui_state
            .handle_platform_output(window, full_output.platform_output.clone());
        full_output
    }

    fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        full_output: egui::FullOutput,
    ) {
        for (id, delta) in &full_output.textures_delta.set {
            self.egui_renderer.update_texture(device, queue, *id, delta);
        }
        let primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.egui_renderer
            .update_buffers(device, queue, encoder, &primitives, screen_descriptor);

        let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("egui control panel"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
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
        let mut render_pass = render_pass.forget_lifetime();
        self.egui_renderer
            .render(&mut render_pass, &primitives, screen_descriptor);
        drop(render_pass);

        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
    }
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<WgpuRenderer>,
    surface: Option<SurfaceContext>,
    stencil: Option<catchlight_wgpu::StencilTarget>,
    composites: Option<catchlight_wgpu::CompositePool>,
    model: Model,
    puppet: Puppet,
    /// Built in `resumed`, alongside the renderer whose GPU state it names.
    cache: Option<RenderCache>,
    start: std::time::Instant,
    last_frame: std::time::Instant,
    /// The first param with at least one deform binding. If present we
    /// sine-animate its value so the deform path is visible.
    demo_param: Option<ParamId>,
    // Camera state: world-space center the view is looking at, and
    // ortho camera height. Left-drag pans; scroll zooms.
    camera_center: glam::Vec2,
    camera_height: f32,
    last_window_size: (u32, u32),
    dragging: bool,
    last_cursor: Option<(f64, f64)>,
    // Direct parameter-control mode: toggled by --control at startup or the
    // 'C' key at runtime. Built lazily in `resumed` alongside the renderer.
    control_enabled: bool,
    control: Option<ControlUi>,
}

impl App {
    fn update_camera(&mut self) {
        if let Some(renderer) = self.renderer.as_mut() {
            let (w, h) = self.last_window_size;
            let aspect = w.max(1) as f32 / h.max(1) as f32;
            renderer.update_camera(create_orthographic_camera_at(
                self.camera_height,
                aspect,
                self.camera_center,
            ));
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let window_attrs = Window::default_attributes()
            .with_title("Catchlight - Load Model")
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720));

        let window = Arc::new(event_loop.create_window(window_attrs).unwrap());
        self.window = Some(window.clone());

        pollster::block_on(async {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::PRIMARY,
                flags: Default::default(),
                memory_budget_thresholds: Default::default(),
                backend_options: Default::default(),
                display: None,
            });

            let surface = instance.create_surface(window.clone()).unwrap();
            let size = window.inner_size();
            let (device, queue, surface_ctx) =
                create_surface_context(&instance, surface, size.width, size.height)
                    .await
                    .expect("surface context");

            let mut renderer = WgpuRenderer::new(device, queue, surface_ctx.render_format).await;

            println!("Preparing the render cache...");
            match RenderCache::prepare(&mut renderer, &self.model, PrepareOptions::default()) {
                Ok(cache) => {
                    println!(
                        "Uploaded {} textures, {} meshes",
                        cache.texture_count(),
                        cache.mesh_count()
                    );
                    self.cache = Some(cache);
                }
                Err(e) => eprintln!("Failed to prepare the render cache: {}", e),
            }

            let aspect = size.width as f32 / size.height as f32;
            renderer.update_camera(create_orthographic_camera_at(
                self.camera_height,
                aspect,
                self.camera_center,
            ));
            self.last_window_size = (size.width, size.height);

            self.control = Some(ControlUi::new(
                &renderer.device,
                surface_ctx.render_format,
                &window,
                &self.model,
            ));

            self.renderer = Some(renderer);
            self.surface = Some(surface_ctx);
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key: PhysicalKey::Code(KeyCode::KeyC),
                    state: ElementState::Pressed,
                    repeat: false,
                    ..
                },
            ..
        } = &event
        {
            self.control_enabled = !self.control_enabled;
            if self.control_enabled {
                self.puppet.stop_animation();
            } else {
                self.puppet.play_animation("Blink");
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }

        // Route input to the panel first so dragging a slider doesn't also
        // pan the camera underneath it. Only wired in while the mode is on,
        // so a hidden panel never steals events from the viewer controls.
        if self.control_enabled {
            if let (Some(control), Some(window)) = (self.control.as_mut(), self.window.as_ref()) {
                let response = control.egui_state.on_window_event(window, &event);
                if response.repaint {
                    window.request_redraw();
                }
                if response.consumed {
                    return;
                }
            }
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let (Some(surface_ctx), Some(renderer)) =
                    (self.surface.as_mut(), self.renderer.as_ref())
                {
                    surface_ctx.resize(&renderer.device, new_size.width, new_size.height);
                    self.last_window_size = (new_size.width, new_size.height);
                }
                self.update_camera();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed => {
                    self.dragging = true;
                }
                ElementState::Released => {
                    self.dragging = false;
                    self.last_cursor = None;
                }
            },
            WindowEvent::CursorMoved { position, .. } => {
                if self.dragging {
                    if let Some((lx, ly)) = self.last_cursor {
                        let (_, h) = self.last_window_size;
                        // Map screen-pixel delta to world-unit delta:
                        // one screen height = self.camera_height world
                        // units, so world-per-pixel = camera_height / h.
                        let world_per_pixel = self.camera_height / h.max(1) as f32;
                        let dx = (position.x - lx) as f32 * world_per_pixel;
                        let dy = (position.y - ly) as f32 * world_per_pixel;
                        // World is Y-up but screen Y grows downward, so the pan
                        // follows the cursor only if the Y delta is added back.
                        self.camera_center.x -= dx;
                        self.camera_center.y += dy;
                        self.update_camera();
                    }
                }
                self.last_cursor = Some((position.x, position.y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Lines: 1 notch zooms by 10%. Pixels (trackpad):
                // normalize by an arbitrary ~30px per notch.
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 30.0) as f32,
                };
                let factor = (1.0_f32 - notches * 0.1).max(0.1);
                self.camera_height =
                    (self.camera_height * factor).clamp(MIN_CAMERA_HEIGHT, MAX_CAMERA_HEIGHT);
                self.update_camera();
            }
            WindowEvent::RedrawRequested => {
                if let (Some(surface_ctx), Some(renderer), Some(cache), Some(window)) = (
                    self.surface.as_ref(),
                    self.renderer.as_mut(),
                    self.cache.as_mut(),
                    self.window.as_ref(),
                ) {
                    let now = std::time::Instant::now();
                    let dt = now.duration_since(self.last_frame).as_secs_f32();
                    self.last_frame = now;

                    let mut control_output: Option<egui::FullOutput> = None;
                    if self.control_enabled {
                        if let Some(control) = self.control.as_mut() {
                            let output = control.prepare(window);
                            for slider in &control.param_sliders {
                                self.puppet.set_param_value(&slider.param, slider.value);
                            }
                            control_output = Some(output);
                        }
                    } else if let Some(param) = &self.demo_param {
                        // Animate the first deform param so the param -> DeformStack
                        // -> GPU deform_buffer path shows on screen. A shallow
                        // 0..0.5 sine looks more like breathing than 0..1.0,
                        // which visibly over-deforms the reference rig's shoulders/body.
                        let t = self.start.elapsed().as_secs_f32();
                        let v = 0.25 + 0.25 * (t * std::f32::consts::TAU * 0.3).sin();
                        self.puppet.set_param_value(param, v);
                    }

                    // Control mode with physics off freezes the drivers, so
                    // the pose is exactly what the sliders say. Every other
                    // case leaves them running, so SimplePhysics still
                    // responds to whatever the sliders (or the demo animation)
                    // set.
                    let physics = !self.control_enabled
                        || self.control.as_ref().is_some_and(|c| c.physics_enabled);
                    if self.puppet.physics_enabled() != physics {
                        self.puppet.set_physics_enabled(physics);
                    }
                    self.puppet.tick(&self.model, dt);
                    if let Err(e) = cache.refresh(renderer, &self.model, &self.puppet) {
                        eprintln!("Failed to refresh the render cache: {}", e);
                    }
                    let render_list = collect(cache, &self.puppet);

                    // wgpu 29: acquire returns None for any non-presentable
                    // state (lost/outdated/timeout/…); reconfigure and retry.
                    let Some((frame, view)) = surface_ctx.acquire() else {
                        surface_ctx.reconfigure(&renderer.device);
                        window.request_redraw();
                        return;
                    };

                    let clear_color = Some(wgpu::Color {
                        r: 0.25,
                        g: 0.25,
                        b: 0.25,
                        a: 1.0,
                    });
                    let (w, h) = (surface_ctx.config.width, surface_ctx.config.height);
                    let stencil = self.stencil.get_or_insert_with(|| {
                        catchlight_wgpu::StencilTarget::new_for_pipelines(
                            &renderer.shared,
                            &renderer.device,
                            w,
                            h,
                        )
                    });
                    stencil.ensure_size_for_pipelines(&renderer.shared, &renderer.device, w, h);
                    let composites = self
                        .composites
                        .get_or_insert_with(|| catchlight_wgpu::CompositePool::new(w, h));

                    let mut encoder =
                        renderer
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("load-model frame"),
                            });
                    renderer.begin_camera_submit();
                    if let Err(e) = renderer.render_list(
                        &render_list,
                        &mut encoder,
                        &view,
                        stencil,
                        composites,
                        w,
                        h,
                        clear_color,
                    ) {
                        eprintln!("Render error: {}", e);
                    }

                    if let (Some(control), Some(output)) = (self.control.as_mut(), control_output) {
                        let screen_descriptor = egui_wgpu::ScreenDescriptor {
                            size_in_pixels: [w, h],
                            pixels_per_point: output.pixels_per_point,
                        };
                        control.paint(
                            &renderer.device,
                            &renderer.queue,
                            &mut encoder,
                            &view,
                            &screen_descriptor,
                            output,
                        );
                    }

                    renderer.queue.submit(std::iter::once(encoder.finish()));

                    frame.present();
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut path_arg: Option<String> = None;
    let mut control_enabled = false;
    for arg in std::env::args().skip(1) {
        if arg == "--control" {
            control_enabled = true;
        } else {
            path_arg = Some(arg);
        }
    }
    let path = std::path::PathBuf::from(
        path_arg.unwrap_or_else(|| "example_models/reference/reference.clm".to_string()),
    );

    println!("Loading model: {}", path.display());
    let model = load_model_file(&path)?;
    println!(
        "Loaded {} textures, {} params",
        model.texture_ids().len(),
        model.param_ids().len()
    );
    let mut puppet = Puppet::new(&model);

    use catchlight_core::BindingTarget;
    // First param (in the model's own order) that any deform binding names.
    let demo_param: Option<ParamId> = model
        .param_ids()
        .iter()
        .find(|id| {
            model
                .bindings_of_param(id)
                .any(|b| b.target() == BindingTarget::Deform)
        })
        .cloned();
    if let Some(param) = &demo_param {
        let name = model
            .param(param)
            .map(|p| p.name.to_string())
            .unwrap_or_default();
        println!("Animating deform param: {name}");
    }

    // Synthesize a blink animation targeting every param whose name
    // contains "Blink" (the reference rig: "Left Eye - Blink" and
    // "Right Eye - Blink"). value=0 -> open, value=1 -> closed.
    // Sequence: 3s open -> quick close -> brief hold -> quick reopen.
    if puppet.animations().is_empty() {
        use catchlight_core::{InterpolateMode, Keyframe, PuppetAnimation, PuppetLane};
        let blink_params: Vec<ParamId> = model
            .param_ids()
            .iter()
            .filter(|id| {
                model
                    .param(id)
                    .is_some_and(|p| p.name.as_str().contains("Blink"))
            })
            .cloned()
            .collect();
        if !blink_params.is_empty() {
            let kfs = || -> Vec<Keyframe> {
                // 60 fps timestep below; frame values become seconds * 60.
                // Short cycle with noticeable closed phase so the blink
                // is obvious without having to watch for seconds.
                vec![
                    Keyframe {
                        frame: 0,
                        value: 0.0,
                    },
                    Keyframe {
                        frame: 60,
                        value: 0.0,
                    }, // 1.0s open
                    Keyframe {
                        frame: 72,
                        value: 1.0,
                    }, // 200ms close
                    Keyframe {
                        frame: 90,
                        value: 1.0,
                    }, // 300ms hold closed
                    Keyframe {
                        frame: 102,
                        value: 0.0,
                    }, // 200ms reopen
                ]
            };
            let lanes = blink_params
                .iter()
                .map(|param| PuppetLane {
                    param: param.clone(),
                    keyframes: kfs(),
                    interpolation: InterpolateMode::Linear,
                })
                .collect();
            let anim = PuppetAnimation {
                name: "Blink".into(),
                timestep: 1.0 / 60.0,
                length: 102,
                lanes,
                ..Default::default()
            };
            puppet.set_animations(vec![anim]);
            // Direct-control mode disables automation up front; skip the
            // initial play so it doesn't fight the sliders from frame one.
            if !control_enabled && puppet.play_animation("Blink") {
                println!(
                    "Playing built-in animation: Blink ({} eye(s))",
                    blink_params.len()
                );
            }
        }
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let now = std::time::Instant::now();
    let mut app = App {
        window: None,
        renderer: None,
        surface: None,
        stencil: None,
        composites: None,
        model,
        puppet,
        cache: None,
        start: now,
        last_frame: now,
        demo_param,
        camera_center: glam::Vec2::ZERO,
        camera_height: INITIAL_CAMERA_HEIGHT,
        last_window_size: (1280, 720),
        dragging: false,
        last_cursor: None,
        control_enabled,
        control: None,
    };

    event_loop.run_app(&mut app)?;

    Ok(())
}
