//! Direct-to-egui viewport.
//!
//! The puppet is rendered on eframe's own wgpu device into an offscreen texture
//! and handed to egui by `TextureId` via `register_native_texture` — catchlight
//! and egui-wgpu share the one wgpu device, so there is no GPU→CPU→GPU
//! readback. The same texture is re-rendered in place every time the pose,
//! camera, document revision or preview state changes; egui samples its current
//! GPU contents.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use catchlight_core::{GlobalTransforms, NodeIdx, Puppet, Vec2};
use catchlight_wgpu::{
    collect_drawables, create_orthographic_camera_at, CompositePool, DrawableInfo,
    FramebufferSnapshotPool, Pipelines, RenderList, StencilTarget, WgpuRenderer,
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

/// Isolate is a part filter. Composite slots stay only while they still
/// have something to blit — dropping them would leave their Parts in
/// `composite_children` with nothing walking that map.
fn retain_isolated(render_list: &mut RenderList, allowed: &HashSet<u32>) {
    let keep_part = |d: &DrawableInfo| match d {
        DrawableInfo::Part { mesh_id, .. } => allowed.contains(&mesh_id.0),
        DrawableInfo::Composite { .. } => true,
    };
    render_list.root_drawables.retain(keep_part);
    for children in render_list.composite_children.values_mut() {
        children.retain(keep_part);
    }
    loop {
        let empty: HashSet<NodeIdx> = render_list
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
    /// (session, rev) the puppet buffers were last uploaded for — mesh and
    /// texture uploads only happen when the document actually changed; pose,
    /// camera and preview changes reuse them.
    uploaded: Option<(u64, u64)>,
    pub transforms: GlobalTransforms,
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
    /// the GUI has a `TextureId` to show before the first render. Expensive
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
            uploaded: None,
            transforms: GlobalTransforms::new(),
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
    /// `deform_preview` is a live vertex drag: (core id, per-vertex local
    /// deltas), applied through the deform stack's scratch source so it
    /// composes with the pose like the committed value will.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        rs: &RenderState,
        puppet: &mut Puppet,
        upload_key: (u64, u64),
        pose: &[(String, Vec2)],
        previews: &[NodePreview],
        deform_preview: Option<&(u32, Vec<(usize, Vec2)>)>,
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

        if self.uploaded != Some(upload_key) {
            self.renderer
                .upload_puppet(puppet)
                .map_err(|e| anyhow!("upload: {e}"))?;
            self.uploaded = Some(upload_key);
        }
        puppet.apply_pose_overlay(pose);
        puppet.tick(&mut self.transforms, glam::Mat4::IDENTITY, 0.0);
        if !previews.is_empty() {
            // Re-fold the transform-dependent pipeline stages with the preview
            // in place (the manual tick tail from AGENTS.md): the tc filter
            // and MG deform propagation must see the previewed transforms, or
            // children of translate-children groups render at stale positions
            // until the commit rebuilds the puppet.
            puppet.reset_dynamic_state();
            puppet.reset_deforms();
            puppet.apply_params();
            apply_previews(puppet, previews);
            puppet.compute_transforms(&mut self.transforms);
            puppet.apply_translate_children_filter(&self.transforms);
            puppet.compute_transforms(&mut self.transforms);
            puppet.propagate_mesh_group_deforms(&self.transforms);
            puppet.apply_welds(&self.transforms);
            puppet.combine_deforms();
        }
        if let Some((core, deltas)) = deform_preview {
            apply_deform_preview(puppet, *core, deltas);
        }
        self.renderer.sync_deforms(puppet);
        let aspect = width as f32 / height as f32;
        self.renderer.begin_camera_submit();
        self.renderer.update_camera(create_orthographic_camera_at(
            camera.height,
            aspect,
            camera.center,
        ));
        let mut render_list = collect_drawables(puppet, &self.transforms);
        if let Some(allowed) = isolate {
            retain_isolated(&mut render_list, allowed);
        }

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

/// Write vertex-drag deltas into the part's scratch deform source and
/// re-combine, so the preview stacks on the posed deform exactly like the
/// committed keypoint will.
fn apply_deform_preview(puppet: &mut Puppet, core: u32, deltas: &[(usize, Vec2)]) {
    use catchlight_core::deform::DeformSource;
    let _ = puppet.update_deform_source(
        catchlight_core::NodeIdx(core),
        DeformSource::Preview,
        |buf| {
            buf.fill(Vec2::ZERO);
            for &(vertex, delta) in deltas {
                if let Some(slot) = buf.get_mut(vertex) {
                    *slot = delta;
                }
            }
        },
    );
    puppet.combine_deforms();
}

/// Write preview overrides into the puppet's post-`tick` working state.
/// `tick` starts from base state every frame, so previews never accumulate.
fn apply_previews(puppet: &mut Puppet, previews: &[NodePreview]) {
    for pv in previews {
        let id = catchlight_core::NodeIdx(pv.core_id);
        if pv.translation.is_some() || pv.rotation.is_some() || pv.scale.is_some() {
            let _ = puppet.update_node_transform(id, |transform| {
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
            puppet.set_node_z_order(id, z);
        }
        if let Some(op) = pv.opacity {
            puppet.set_node_opacity(id, op);
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
            "/../../tests/models/welded_seam.clp"
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

        let mut shot = |rev: u64, pose: &[(String, Vec2)]| -> Vec<u8> {
            editor
                .with_puppet(session, |puppet| {
                    viewport
                        .render(
                            &rs,
                            puppet,
                            (1, rev),
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

        let rest = shot(1, &[]);
        let posed = shot(1, &[("pull".into(), Vec2::new(1.0, 0.0))]);
        assert_ne!(rest, posed, "posing pull must change the render");
        let back = shot(1, &[("pull".into(), Vec2::new(0.0, 0.0))]);
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

    fn dummy_part(id: u32) -> DrawableInfo {
        use catchlight_core::{BlendMode, MeshId, TextureId};
        DrawableInfo::Part {
            mesh_id: MeshId(id),
            texture_id: TextureId(0),
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
            node_id: NodeIdx(id),
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
            .insert(NodeIdx(1), vec![dummy_part(2), dummy_part(3)]);

        retain_isolated(&mut list, &HashSet::from([2u32]));

        assert_eq!(list.root_drawables.len(), 1, "parent composite must stay");
        let kids = &list.composite_children[&NodeIdx(1)];
        assert_eq!(kids.len(), 1, "sibling part dropped");
        assert!(matches!(kids[0], DrawableInfo::Part { mesh_id, .. } if mesh_id.0 == 2));
    }

    #[test]
    fn isolate_keeps_nested_composite_chain() {
        let mut list = RenderList::default();
        list.root_drawables.push(dummy_composite(1));
        list.composite_children
            .insert(NodeIdx(1), vec![dummy_composite(2)]);
        list.composite_children
            .insert(NodeIdx(2), vec![dummy_part(3)]);

        retain_isolated(&mut list, &HashSet::from([3u32]));

        assert_eq!(list.root_drawables.len(), 1);
        assert_eq!(
            list.composite_children.get(&NodeIdx(1)).map(Vec::len),
            Some(1)
        );
        assert_eq!(
            list.composite_children.get(&NodeIdx(2)).map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn isolate_drops_empty_composite() {
        let mut list = RenderList::default();
        list.root_drawables.push(dummy_composite(1));
        list.composite_children
            .insert(NodeIdx(1), vec![dummy_part(2)]);

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
