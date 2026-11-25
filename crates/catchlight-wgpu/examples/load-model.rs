#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! winit viewer for a `.clm` model. `--control` (or `C`) opens per-param
//! sliders and stops animation playback.

use catchlight_core::{load_model, Model, ModelFormat, ParamId, Puppet};
use catchlight_extension_animations::{AnimationPlayer, Animations, Clip, Keyframe, Lane};
use catchlight_wgpu::{
    collect, create_orthographic_camera_at, create_surface_context, PrepareOptions, RenderCache,
    SurfaceContext, WgpuRenderer,
};
use std::path::Path;
use std::sync::Arc;

fn load_model_file(path: &Path) -> Result<Model, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let format = ModelFormat::from_path(path).ok_or_else(|| {
        format!(
            "{} is not a .clm; convert it first with `catchlight-importer --help`",
            path.display()
        )
    })?;
    Ok(load_model(&bytes, format)?)
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

/// Owned here so motion responses writing output params cannot move sliders.
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

struct ControlUi {
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    motion_responses_enabled: bool,
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
        // `chunk_by_mut` grouping in `prepare` relies on this sort.
        param_sliders.sort_by(|a, b| a.name.cmp(&b.name));

        Self {
            egui_ctx,
            egui_state,
            egui_renderer,
            motion_responses_enabled: true,
            param_sliders,
        }
    }

    fn prepare(&mut self, window: &Window) -> egui::FullOutput {
        let raw_input = self.egui_state.take_egui_input(window);
        let motion_responses_enabled = &mut self.motion_responses_enabled;
        let param_sliders = &mut self.param_sliders;
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            egui::Panel::right("catchlight-control-panel")
                .default_size(340.0)
                .show_inside(ui, |ui| {
                    ui.heading("Direct Control");
                    ui.checkbox(motion_responses_enabled, "Motion responses");
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
    targets: catchlight_wgpu::FrameTargets,
    model: Model,
    puppet: Puppet,
    player: AnimationPlayer,
    cache: Option<RenderCache>,
    start: std::time::Instant,
    last_frame: std::time::Instant,
    demo_param: Option<ParamId>,
    // World-space view center and ortho height in world units.
    camera_center: glam::Vec2,
    camera_height: f32,
    last_window_size: (u32, u32),
    dragging: bool,
    last_cursor: Option<(f64, f64)>,
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

            let mut renderer = WgpuRenderer::new(device, queue, surface_ctx.render_format);

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
                self.player.stop();
            } else {
                self.player.play("Blink");
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
            return;
        }

        // The panel gets input first so a slider drag doesn't also pan.
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
                        let world_per_pixel = self.camera_height / h.max(1) as f32;
                        let dx = (position.x - lx) as f32 * world_per_pixel;
                        let dy = (position.y - ly) as f32 * world_per_pixel;
                        // World is Y-up, screen Y grows down.
                        self.camera_center.x -= dx;
                        self.camera_center.y += dy;
                        self.update_camera();
                    }
                }
                self.last_cursor = Some((position.x, position.y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // ~30px per notch for pixel (trackpad) deltas.
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
                        // 0..0.5: a full 0..1 sweep over-deforms some models.
                        let t = self.start.elapsed().as_secs_f32();
                        let v = 0.25 + 0.25 * (t * std::f32::consts::TAU * 0.3).sin();
                        self.puppet.set_param_value(param, v);
                    }

                    let responses_enabled = !self.control_enabled
                        || self
                            .control
                            .as_ref()
                            .is_some_and(|c| c.motion_responses_enabled);
                    if self.puppet.motion_responses_enabled() != responses_enabled {
                        self.puppet.set_motion_responses_enabled(responses_enabled);
                    }
                    self.player.advance(&mut self.puppet, dt);
                    self.puppet.tick(&self.model, dt);
                    if let Err(e) = cache.refresh(renderer, &self.model, &self.puppet) {
                        eprintln!("Failed to refresh the render cache: {}", e);
                    }
                    let render_list = collect(cache, &self.puppet);

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
                    let (device, queue) = (renderer.device.clone(), renderer.queue.clone());
                    let mut done = renderer.frame().render(
                        &[&render_list],
                        catchlight_wgpu::RenderTarget {
                            view: &view,
                            texture: None,
                            width: w,
                            height: h,
                        },
                        &mut self.targets,
                        clear_color,
                    );
                    if let (Some(control), Some(output)) = (self.control.as_mut(), control_output) {
                        let screen_descriptor = egui_wgpu::ScreenDescriptor {
                            size_in_pixels: [w, h],
                            pixels_per_point: output.pixels_per_point,
                        };
                        control.paint(
                            &device,
                            &queue,
                            done.encoder(),
                            &view,
                            &screen_descriptor,
                            output,
                        );
                    }
                    done.submit();

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
    let path =
        std::path::PathBuf::from(path_arg.unwrap_or_else(|| "tests/models/lan.clm".to_string()));

    println!("Loading model: {}", path.display());
    let model = load_model_file(&path)?;
    println!(
        "Loaded {} textures, {} params",
        model.texture_ids().len(),
        model.param_ids().len()
    );
    let puppet = Puppet::new(&model);

    use catchlight_core::BindingTarget;
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

    // Synthesized blink for every param named "*Blink*": 0 = open, 1 = closed.
    let mut player = model
        .extension::<Animations>()
        .map(AnimationPlayer::from_animations)
        .unwrap_or_default();
    if player.clips().is_empty() {
        use catchlight_core::InterpolateMode;
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
                // Frame values are seconds * 60 (60 fps timestep below).
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
                .map(|param| Lane {
                    param: param.clone(),
                    keyframes: kfs(),
                    interpolation: InterpolateMode::Linear,
                })
                .collect();
            let anim = Clip {
                length: 102,
                lanes,
                ..Clip::new("Blink")
            };
            player.set_clips(vec![anim]);
            if !control_enabled && player.play("Blink") {
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
        targets: catchlight_wgpu::FrameTargets::new(),
        model,
        puppet,
        player,
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
