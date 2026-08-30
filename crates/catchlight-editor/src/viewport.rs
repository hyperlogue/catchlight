//! Direct-to-egui viewport.
//!
//! The puppet is rendered on eframe's own wgpu device into an offscreen texture
//! and handed to egui by `TextureIdx` via `register_native_texture` — catchlight
//! and egui-wgpu share the one wgpu device, so there is no GPU→CPU→GPU
//! readback. The same texture is re-rendered in place every time the pose,
//! camera, document revision or preview state changes; egui samples its current
//! GPU contents.
//!
//! **One renderer, one session's render cache.** A cache's slots name GPU
//! state inside the renderer that prepared it, and the GUI keeps one renderer
//! for every session it shows, so the cache is held beside the session it was
//! prepared for and re-prepared when the shown session changes. Within one
//! session a model edit is the cache's own business: `refresh` rebuilds when
//! the model's generation moved.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use catchlight_core::id::ParamId;
use catchlight_core::{Model, Pose, Puppet, Vec2};
use catchlight_wgpu::{
    create_orthographic_camera_at, CompositePool, DrawableInfo, FramebufferSnapshotPool, Pipelines,
    PrepareOptions, RenderCache, RenderList, StencilTarget, WgpuRenderer,
};
use eframe::egui;
use eframe::egui_wgpu::RenderState;

use crate::camera::EditorCamera;

/// The puppet renders into an sRGB target (catchlight shaders blend in linear
/// and rely on hardware encode). egui, however, expects sampled textures to
/// yield gamma-encoded values (its shader names the sample `tex_gamma` and
/// decodes it itself), so the view handed to egui must be the *non-sRGB*
/// reinterpretation of the same texture — registering the sRGB view instead
/// double-decodes and the viewport renders visibly too dark.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const EGUI_VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Transient per-node overrides applied after `tick`, before drawing — the
/// preview half of gesture-scoped editing (the document sees one command on
/// release). Working state only; the next document change rebuilds the puppet.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NodePreview {
    pub core_id: u32,
    pub translation: Option<[f32; 3]>,
    pub rotation: Option<[f32; 3]>,
    pub scale: Option<[f32; 2]>,
    pub z_order: Option<f32>,
    pub opacity: Option<f32>,
}

/// Isolate is a part filter, over the *mesh* slots a render list names its
/// parts by. Composite slots stay only while they still have something to
/// blit — dropping them would leave their Parts in `composite_children` with
/// nothing walking that map.
fn retain_isolated(render_list: &mut RenderList, allowed: &HashSet<u32>) {
    let keep_part = |d: &DrawableInfo| match d {
        DrawableInfo::Part { mesh_id, .. } => allowed.contains(mesh_id),
        DrawableInfo::Composite { .. } => true,
    };
    render_list.root_drawables.retain(keep_part);
    for children in render_list.composite_children.values_mut() {
        children.retain(keep_part);
    }
    loop {
        let empty: HashSet<u32> = render_list
            .composite_children
            .iter()
            .filter(|(_, ch)| ch.is_empty())
            .map(|(&id, _)| id)
            .collect();
        if empty.is_empty() {
            break;
        }
        render_list
            .composite_children
            .retain(|id, _| !empty.contains(id));
        let keep_slot = |d: &DrawableInfo| match d {
            DrawableInfo::Composite { node_id, .. } => !empty.contains(node_id),
            DrawableInfo::Part { .. } => true,
        };
        render_list.root_drawables.retain(keep_slot);
        for children in render_list.composite_children.values_mut() {
            children.retain(keep_slot);
        }
    }
}

pub(crate) struct ViewportRenderer {
    renderer: WgpuRenderer,
    /// The session this cache was prepared for, and the cache. A model edit
    /// within the session is the cache's own generation gate; a different
    /// session needs its own slots in this renderer, so it re-prepares.
    cache: Option<(u64, RenderCache)>,
    target: wgpu::Texture,
    view: wgpu::TextureView,
    stencil: StencilTarget,
    composites: CompositePool,
    snapshots: FramebufferSnapshotPool,
    width: u32,
    height: u32,
    texture_id: egui::TextureId,
}

impl ViewportRenderer {
    /// Build the renderer on eframe's device and register the (empty) target so
    /// the GUI has a `TextureIdx` to show before the first render. Expensive
    /// (~18 pipelines); done once and reused across every session preview.
    pub(crate) fn new(rs: &RenderState, width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        // Autodetect keeps the stencil-free masking tier on GL/WebGL2 adapters;
        // construction is synchronous, which the wasm build requires (no
        // block_on there).
        let pipelines = Arc::new(Pipelines::new_autodetect(&rs.adapter, &rs.device, FORMAT));
        let renderer = WgpuRenderer::from_pipelines(rs.device.clone(), rs.queue.clone(), pipelines);
        let (target, view, egui_view) = make_target(&renderer.device, width, height);
        let stencil =
            StencilTarget::new_for_pipelines(&renderer.shared, &renderer.device, width, height);
        let texture_id = rs.renderer.write().register_native_texture(
            &renderer.device,
            &egui_view,
            wgpu::FilterMode::Linear,
        );
        Self {
            renderer,
            cache: None,
            target,
            view,
            stencil,
            composites: CompositePool::new(width, height),
            snapshots: FramebufferSnapshotPool::new(width, height),
            width,
            height,
            texture_id,
        }
    }

    /// Render `puppet` into the offscreen target and return the egui texture id
    /// pointing at it. `isolate` limits drawing to the given core node ids
    /// (mask sources ride inside each drawable and keep working).
    /// `scratch_deform` is a live vertex drag: (core id, per-vertex local
    /// deltas), applied through the deform stack's scratch source so it
    /// composes with the pose like the committed value will.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        rs: &RenderState,
        session: u64,
        model: &Model,
        puppet: &mut Puppet,
        pose: &[(ParamId, f32)],
        previews: &[NodePreview],
        scratch_deform: Option<&(u32, Vec<(usize, Vec2)>)>,
        camera: &EditorCamera,
        width: u32,
        height: u32,
        isolate: Option<&HashSet<u32>>,
    ) -> Result<egui::TextureId> {
        let width = width.clamp(1, 4096);
        let height = height.clamp(1, 4096);
        if width != self.width || height != self.height {
            self.resize(rs, width, height);
        }

        if !matches!(&self.cache, Some((held, _)) if *held == session) {
            // The editor edits, so the decode memo earns its copy: a rebuild
            // after a keystroke re-uploads without re-decoding.
            let cache = RenderCache::prepare(
                &mut self.renderer,
                model,
                PrepareOptions {
                    texture_halvings: 0,
                    memoize_textures: true,
                },
            )
            .map_err(|e| anyhow!("prepare: {e}"))?;
            self.cache = Some((session, cache));
        }
        let Some((_, cache)) = self.cache.as_mut() else {
            return Err(anyhow!("render cache unavailable"));
        };

        // Params are scalar and the GUI holds their Ids, so the pose needs
        // no name resolution: it *is* the pose the model evaluates.
        puppet.apply_pose(&pose.iter().cloned().collect::<Pose>());
        puppet.tick(model, 0.0);
        if !previews.is_empty() {
            // Re-fold with the preview in place: the tc filter and MG deform
            // propagation must see the previewed transforms, or children of
            // translate-children groups render at stale positions until the
            // commit rebakes the puppet.
            puppet.refold_with_node_edits(|edits| apply_previews(edits, previews));
        }
        if let Some((core, deltas)) = scratch_deform {
            apply_scratch_deform(puppet, *core, deltas);
        }
        cache
            .refresh(&mut self.renderer, model, puppet)
            .map_err(|e| anyhow!("refresh: {e}"))?;
        let mut render_list = catchlight_wgpu::collect(cache, puppet);
        if let Some(allowed) = isolate {
            // `isolate` names nodes; a render list names its parts by mesh
            // slot, and the two numberings are not the same.
            let allowed: HashSet<u32> = allowed
                .iter()
                .filter_map(|&node| cache.mesh_slot_of_node(node))
                .collect();
            retain_isolated(&mut render_list, &allowed);
        }
        let aspect = width as f32 / height as f32;
        self.renderer.begin_camera_submit();
        self.renderer.update_camera(create_orthographic_camera_at(
            camera.height,
            aspect,
            camera.center,
        ));

        let mut encoder =
            self.renderer
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewport-encoder"),
                });
        self.renderer
            .render_list_ext(
                &render_list,
                &mut encoder,
                &self.view,
                &self.stencil,
                &mut self.composites,
                Some(&self.target),
                Some(&mut self.snapshots),
                width,
                height,
                Some(wgpu::Color {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                }),
            )
            .map_err(|e| anyhow!("render: {e}"))?;
        self.renderer
            .queue
            .submit(std::iter::once(encoder.finish()));
        Ok(self.texture_id)
    }

    /// Clones of the GPU handles behind the rendered image, for a PNG
    /// snapshot readback (wgpu resources are internally ref-counted).
    pub(crate) fn snapshot_source(&self) -> (wgpu::Device, wgpu::Queue, wgpu::Texture, u32, u32) {
        (
            self.renderer.device.clone(),
            self.renderer.queue.clone(),
            self.target.clone(),
            self.width,
            self.height,
        )
    }

    fn resize(&mut self, rs: &RenderState, width: u32, height: u32) {
        let (target, view, egui_view) = make_target(&self.renderer.device, width, height);
        self.target = target;
        self.view = view;
        self.stencil = StencilTarget::new_for_pipelines(
            &self.renderer.shared,
            &self.renderer.device,
            width,
            height,
        );
        self.composites = CompositePool::new(width, height);
        self.snapshots = FramebufferSnapshotPool::new(width, height);
        self.width = width;
        self.height = height;

        let mut egui_renderer = rs.renderer.write();
        egui_renderer.free_texture(&self.texture_id);
        self.texture_id = egui_renderer.register_native_texture(
            &self.renderer.device,
            &egui_view,
            wgpu::FilterMode::Linear,
        );
    }
}

/// Write vertex-drag deltas into the part's scratch deform and re-combine, so
/// the scratch deform stacks on the posed deform exactly like the committed
/// keypoint will.
fn apply_scratch_deform(puppet: &mut Puppet, core: u32, deltas: &[(usize, Vec2)]) {
    let id = catchlight_core::NodeIdx(core);
    let Some(len) = puppet.combined_deform(id).map(<[Vec2]>::len) else {
        return;
    };
    let mut offsets = vec![Vec2::ZERO; len];
    for &(vertex, delta) in deltas {
        if let Some(slot) = offsets.get_mut(vertex) {
            *slot = delta;
        }
    }
    puppet.set_scratch_deform(id, &offsets);
    puppet.combine_deforms();
}

/// Write preview overrides over the frame the pose fold produced. The next
/// fold starts from the model's authored values, so previews never accumulate.
fn apply_previews(edits: &mut catchlight_core::NodeEdits<'_>, previews: &[NodePreview]) {
    for pv in previews {
        let id = catchlight_core::NodeIdx(pv.core_id);
        if pv.translation.is_some() || pv.rotation.is_some() || pv.scale.is_some() {
            edits.transform(id, |transform| {
                if let Some(t) = pv.translation {
                    transform.translation = glam::Vec3::from_array(t);
                }
                if let Some(r) = pv.rotation {
                    transform.rotation = glam::Vec3::from_array(r);
                }
                if let Some(s) = pv.scale {
                    transform.scale = glam::Vec2::from_array(s);
                }
            });
        }
        if let Some(z) = pv.z_order {
            edits.set_z_order(id, z);
        }
        if let Some(op) = pv.opacity {
            edits.set_opacity(id, op);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[allow(clippy::unwrap_used)]
    fn render_state() -> RenderState {
        let (adapter, device, queue) = pollster::block_on(
            catchlight_wgpu::create_headless_context_ext(wgpu::Backends::PRIMARY),
        )
        .unwrap();
        let renderer = eframe::egui_wgpu::Renderer::new(
            &device,
            wgpu::TextureFormat::Rgba8Unorm,
            eframe::egui_wgpu::RendererOptions::default(),
        );
        RenderState {
            adapter,
            available_adapters: Vec::new(),
            device,
            queue,
            target_format: wgpu::TextureFormat::Rgba8Unorm,
            renderer: Arc::new(eframe::egui::mutex::RwLock::new(renderer)),
        }
    }

    /// The full GUI render path (pose → tick → render): posing a param must
    /// change the rendered pixels, and returning to rest must restore them.
    #[test]
    fn posed_param_changes_rendered_pixels() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/models/welded_seam.clm"
        ))
        .unwrap();
        let editor = catchlight_editor_server::Editor::new();
        let session = editor.open_bytes("welded_seam", &bytes).unwrap();
        let rs = render_state();
        let mut viewport = ViewportRenderer::new(&rs, 512, 512);
        // welded_seam spans 300x240 world units, so the GUI's default 2000-unit
        // camera height would render it as a speck; frame it instead.
        let camera = EditorCamera {
            center: Vec2::ZERO,
            height: 600.0,
        };

        let pull = editor
            .with_model(session, |model| {
                model
                    .param_ids()
                    .iter()
                    .find(|id| model.param(id).is_some_and(|p| p.name.as_str() == "pull"))
                    .cloned()
            })
            .unwrap()
            .expect("welded_seam has a `pull` param");

        let mut shot = |pose: &[(ParamId, f32)]| -> Vec<u8> {
            editor
                .with_puppet(session, |model, puppet| {
                    viewport
                        .render(
                            &rs,
                            session.0,
                            model,
                            puppet,
                            pose,
                            &[],
                            None,
                            &camera,
                            512,
                            512,
                            None,
                        )
                        .unwrap();
                })
                .unwrap();
            let (device, queue, texture, w, h) = viewport.snapshot_source();
            pollster::block_on(read_texture_to_rgba(&device, &queue, &texture, w, h)).unwrap()
        };

        let rest = shot(&[]);
        let posed = shot(&[(pull.clone(), 1.0)]);
        assert_ne!(rest, posed, "posing pull must change the render");
        let back = shot(&[(pull, 0.0)]);
        if let Ok(dir) = std::env::var("VIEWPORT_TEST_DUMP") {
            for (name, img) in [("rest", &rest), ("posed", &posed), ("back", &back)] {
                image::save_buffer(
                    format!("{dir}/{name}.png"),
                    img,
                    512,
                    512,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
        assert_eq!(rest, back, "returning to rest must restore the render");
    }

    /// Isolate names *nodes*, while a render list names its parts by mesh
    /// slot, and the two numberings are not the same: mesh slots are dense
    /// and skip every node without one. Isolating each part in turn has to
    /// draw that part — passing a node slot through as a mesh slot draws the
    /// wrong part for some and nothing at all for the last.
    #[test]
    fn isolating_each_part_in_turn_draws_it() {
        let bytes = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/models/welded_seam.clm"
        ))
        .unwrap();
        let editor = catchlight_editor_server::Editor::new();
        let session = editor.open_bytes("welded_seam", &bytes).unwrap();
        let rs = render_state();
        let mut viewport = ViewportRenderer::new(&rs, 256, 256);
        let camera = EditorCamera {
            center: Vec2::ZERO,
            height: 600.0,
        };

        let parts: Vec<u32> = editor
            .with_puppet(session, |_model, puppet| {
                puppet
                    .iter()
                    .filter(|(_, node)| matches!(node.kind, catchlight_core::NodeKind::Part(_)))
                    .map(|(idx, _)| idx.0)
                    .collect()
            })
            .unwrap();
        assert_eq!(parts.len(), 2, "welded_seam is two parts");

        let mut shot = |isolate: Option<&HashSet<u32>>| -> Vec<u8> {
            editor
                .with_puppet(session, |model, puppet| {
                    viewport
                        .render(
                            &rs,
                            session.0,
                            model,
                            puppet,
                            &[],
                            &[],
                            None,
                            &camera,
                            256,
                            256,
                            isolate,
                        )
                        .unwrap();
                })
                .unwrap();
            let (device, queue, texture, w, h) = viewport.snapshot_source();
            pollster::block_on(read_texture_to_rgba(&device, &queue, &texture, w, h)).unwrap()
        };

        let blank = shot(Some(&HashSet::new()));
        let both = shot(None);
        assert_ne!(both, blank, "the model draws something to begin with");
        for &part in &parts {
            let only = shot(Some(&HashSet::from([part])));
            assert_ne!(only, blank, "isolating node slot {part} drew nothing");
            assert_ne!(only, both, "isolating node slot {part} drew everything");
        }
    }

    fn dummy_part(id: u32) -> DrawableInfo {
        use catchlight_core::BlendMode;
        DrawableInfo::Part {
            mesh_id: id,
            texture_id: 0,
            transform: glam::Mat4::IDENTITY,
            z_order: 0.0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            tint: glam::Vec3::ONE,
            screen_tint: glam::Vec3::ZERO,
            mask_sources: Default::default(),
            mask_threshold: 0.5,
        }
    }

    fn dummy_composite(id: u32) -> DrawableInfo {
        use catchlight_core::BlendMode;
        DrawableInfo::Composite {
            node_id: id,
            z_order: 0.0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            tint: glam::Vec3::ONE,
            screen_tint: glam::Vec3::ZERO,
            mask_sources: Default::default(),
            mask_threshold: 0.5,
        }
    }

    #[test]
    fn isolate_keeps_enclosing_composite_when_only_a_child_part_is_allowed() {
        let mut list = RenderList::default();
        list.root_drawables.push(dummy_composite(1));
        list.composite_children
            .insert(1, vec![dummy_part(2), dummy_part(3)]);

        retain_isolated(&mut list, &HashSet::from([2u32]));

        assert_eq!(list.root_drawables.len(), 1, "parent composite must stay");
        let kids = &list.composite_children[&1];
        assert_eq!(kids.len(), 1, "sibling part dropped");
        assert!(matches!(kids[0], DrawableInfo::Part { mesh_id, .. } if mesh_id == 2));
    }

    #[test]
    fn isolate_keeps_nested_composite_chain() {
        let mut list = RenderList::default();
        list.root_drawables.push(dummy_composite(1));
        list.composite_children.insert(1, vec![dummy_composite(2)]);
        list.composite_children.insert(2, vec![dummy_part(3)]);

        retain_isolated(&mut list, &HashSet::from([3u32]));

        assert_eq!(list.root_drawables.len(), 1);
        assert_eq!(list.composite_children.get(&1).map(Vec::len), Some(1));
        assert_eq!(list.composite_children.get(&2).map(Vec::len), Some(1));
    }

    #[test]
    fn isolate_drops_empty_composite() {
        let mut list = RenderList::default();
        list.root_drawables.push(dummy_composite(1));
        list.composite_children.insert(1, vec![dummy_part(2)]);

        retain_isolated(&mut list, &HashSet::from([99u32]));

        assert!(list.root_drawables.is_empty());
        assert!(list.composite_children.is_empty());
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
use catchlight_wgpu::read_texture_to_rgba;

fn make_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        // TEXTURE_BINDING so egui samples it; COPY_SRC for the renderer's
        // framebuffer-snapshot blend passes.
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[EGUI_VIEW_FORMAT],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let egui_view = target.create_view(&wgpu::TextureViewDescriptor {
        format: Some(EGUI_VIEW_FORMAT),
        ..Default::default()
    });
    (target, view, egui_view)
}
