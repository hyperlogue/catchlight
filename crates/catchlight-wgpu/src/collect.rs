//! Collects a posed runtime's drawables into the `RenderList` the renderer
//! draws.
//!
//! Higher accumulated `z_order` draws in front. A list holds dense render-cache
//! slots, never Ids, and is meaningful only against the cache it was collected
//! from.

use crate::renderer::DeformSet;
use catchlight_core::{BlendMode, CompositeData, MaskMode, Node, NodeIdx, NodeKind, NodeTree};
use smallvec::SmallVec;
use std::collections::HashMap;

/// Makes the renderer's dense-table probe miss, so the draw is skipped and
/// counted.
pub(crate) const NO_SLOT: u32 = u32::MAX;

pub(crate) trait DrawSource {
    fn tree(&self) -> &NodeTree;
    fn node(&self, idx: NodeIdx) -> Option<&Node>;
    fn node_count(&self) -> usize;
    fn transform(&self, idx: NodeIdx) -> glam::Mat4;
    /// Summed with every ancestor's; the rule lives on `Puppet` so the CLI and
    /// renderer agree on draw order.
    fn accumulated_z(&self, idx: NodeIdx) -> f32;
    fn structure_revision(&self) -> u64;
    fn mesh_slot(&self, idx: NodeIdx) -> u32;
    /// May name a slot that was never uploaded; the renderer skips those.
    fn texture_slot(&self, node: &Node) -> u32;
    fn has_texture(&self, slot: u32) -> bool;
}

pub type MaskSources = SmallVec<[MaskSourceData; 2]>;

#[derive(Debug, Clone)]
pub enum MaskSourceData {
    Part {
        mesh_id: u32,
        texture_id: u32,
        transform: glam::Mat4,
        mode: MaskMode,
        mask_threshold: f32,
    },
    Composite {
        node_id: u32,
        mode: MaskMode,
    },
}

impl MaskSourceData {
    pub fn mode(&self) -> MaskMode {
        match self {
            Self::Part { mode, .. } | Self::Composite { mode, .. } => *mode,
        }
    }

    pub fn is_part(&self) -> bool {
        matches!(self, Self::Part { .. })
    }
}

#[derive(Debug, Clone)]
pub struct CompositeMaskPartData {
    pub mesh_id: u32,
    pub texture_id: u32,
    pub transform: glam::Mat4,
    pub mask_threshold: f32,
}

#[derive(Debug, Clone)]
pub struct CompositeMaskSourceData {
    pub opacity: f32,
    pub mask_threshold: f32,
    pub parts: Vec<CompositeMaskPartData>,
}

#[derive(Debug, Clone)]
pub enum DrawableInfo {
    Part {
        mesh_id: u32,
        texture_id: u32,
        transform: glam::Mat4,
        z_order: f32,
        blend_mode: BlendMode,
        opacity: f32,
        tint: glam::Vec3,
        screen_tint: glam::Vec3,
        mask_sources: MaskSources,
        mask_threshold: f32,
    },
    Composite {
        node_id: u32,
        z_order: f32,
        blend_mode: BlendMode,
        opacity: f32,
        tint: glam::Vec3,
        screen_tint: glam::Vec3,
        mask_sources: MaskSources,
        mask_threshold: f32,
    },
}

impl DrawableInfo {
    pub fn z_order(&self) -> f32 {
        match self {
            DrawableInfo::Part { z_order, .. } => *z_order,
            DrawableInfo::Composite { z_order, .. } => *z_order,
        }
    }

    pub fn blend_mode(&self) -> BlendMode {
        match self {
            DrawableInfo::Part { blend_mode, .. } => *blend_mode,
            DrawableInfo::Composite { blend_mode, .. } => *blend_mode,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct RenderList {
    pub root_drawables: Vec<DrawableInfo>,
    /// Keyed by the composite's own `node_id` slot.
    pub composite_children: HashMap<u32, Vec<DrawableInfo>>,
    pub composite_mask_sources: HashMap<u32, CompositeMaskSourceData>,
    pub deform_set: DeformSet,
}

impl RenderList {
    /// Sized once at frame start; growing mid-frame would strand already
    /// recorded passes on the old buffer.
    pub fn total_instance_count(&self) -> usize {
        fn for_part(mask_sources_len: usize) -> usize {
            mask_sources_len + 1
        }

        fn for_drawable(d: &DrawableInfo) -> usize {
            match d {
                DrawableInfo::Part { mask_sources, .. } => {
                    for_part(mask_sources.iter().filter(|m| m.is_part()).count())
                }
                DrawableInfo::Composite { mask_sources, .. } => {
                    mask_sources.iter().filter(|m| m.is_part()).count()
                }
            }
        }

        let mut n = 0;
        for d in &self.root_drawables {
            n += for_drawable(d);
        }
        for children in self.composite_children.values() {
            for c in children {
                n += for_drawable(c);
            }
        }
        n += self
            .composite_mask_sources
            .values()
            .map(|source| source.parts.len())
            .sum::<usize>();
        n
    }

    pub fn total_mask_source_count(&self) -> usize {
        fn for_drawable(d: &DrawableInfo) -> usize {
            match d {
                DrawableInfo::Part { mask_sources, .. } => mask_sources.len(),
                DrawableInfo::Composite { mask_sources, .. } => mask_sources.len(),
            }
        }

        let mut n = 0;
        for d in &self.root_drawables {
            n += for_drawable(d);
        }
        for children in self.composite_children.values() {
            for c in children {
                n += for_drawable(c);
            }
        }
        n
    }

    /// Keeps composite-children buffers; consumers treat a stale empty entry
    /// like a missing one.
    pub fn clear(&mut self) {
        self.root_drawables.clear();
        for children in self.composite_children.values_mut() {
            children.clear();
        }
        self.composite_mask_sources.clear();
    }
}

fn composite_passthrough_static<S: DrawSource + ?Sized>(
    source: &S,
    node_id: NodeIdx,
    composite: &CompositeData,
) -> bool {
    if composite.blend_mode != BlendMode::Normal || !composite.masks.is_empty() {
        return false;
    }
    let mut stack = source.tree().get_children(node_id);
    while let Some(id) = stack.pop() {
        match source.node(id).map(|n| &n.kind) {
            Some(NodeKind::Composite(_)) => return false,
            Some(NodeKind::Part(part)) if part.blend_mode != BlendMode::Normal => return false,
            _ => {}
        }
        stack.extend(source.tree().get_children(id));
    }
    true
}

fn composite_passthrough_dynamic(composite: &CompositeData) -> bool {
    composite.opacity == 1.0
        && composite.tint == glam::Vec3::ONE
        && composite.screen_tint == glam::Vec3::ZERO
}

fn collect_mask_sources<S: DrawSource + ?Sized>(
    node_id: NodeIdx,
    source: &S,
    composite_sources: &mut HashMap<u32, CompositeMaskSourceData>,
) -> MaskSources {
    let Some(node) = source.node(node_id) else {
        return MaskSources::new();
    };

    let masks = match &node.kind {
        NodeKind::Part(part) => &part.masks,
        NodeKind::Composite(composite) => &composite.masks,
        _ => return MaskSources::new(),
    };

    let mut sources = MaskSources::new();
    for mask in masks {
        let mask_node_id = mask.source;
        let Some(mask_node) = source.node(mask_node_id) else {
            continue;
        };
        match &mask_node.kind {
            NodeKind::Part(part) => sources.push(MaskSourceData::Part {
                mesh_id: source.mesh_slot(mask_node_id),
                texture_id: source.texture_slot(mask_node),
                transform: source.transform(mask_node_id),
                mode: mask.mode,
                mask_threshold: part.mask_threshold,
            }),
            NodeKind::Composite(composite) => {
                composite_sources.entry(mask_node_id.0).or_insert_with(|| {
                    let mut parts = Vec::new();
                    let mut stack = source.tree().get_children(mask_node_id);
                    while let Some(descendant) = stack.pop() {
                        if let Some(node) = source.node(descendant) {
                            if let NodeKind::Part(part) = &node.kind {
                                let texture_id = source.texture_slot(node);
                                if source.has_texture(texture_id) {
                                    parts.push(CompositeMaskPartData {
                                        mesh_id: source.mesh_slot(descendant),
                                        texture_id,
                                        transform: source.transform(descendant),
                                        mask_threshold: part.mask_threshold,
                                    });
                                }
                            }
                        }
                        stack.extend(source.tree().get_children(descendant));
                    }
                    CompositeMaskSourceData {
                        opacity: composite.opacity,
                        mask_threshold: composite.mask_threshold,
                        parts,
                    }
                });
                sources.push(MaskSourceData::Composite {
                    node_id: mask_node_id.0,
                    mode: mask.mode,
                });
            }
            NodeKind::MeshGroup(_)
            | NodeKind::MotionResponse
            | NodeKind::Group
            | NodeKind::Spine(_) => {}
        }
    }
    sources
}

#[derive(Debug, Default)]
pub(crate) struct Collector {
    enabled_cum: Vec<bool>,
    composite_ancestor: Vec<Option<NodeIdx>>,
    composite_isolates: Vec<bool>,
    // Structural pass-through verdict per composite; rebuilt when the
    // structure revision or node count changes.
    static_passthrough: Vec<Option<bool>>,
    static_passthrough_len: usize,
    static_passthrough_revision: u64,
}

impl Collector {
    pub(crate) fn collect_into<S: DrawSource + ?Sized>(
        &mut self,
        source: &S,
        render_list: &mut RenderList,
    ) {
        render_list.clear();

        let n = source.node_count();
        self.enabled_cum.resize(n, true);
        self.enabled_cum.fill(true);
        self.composite_ancestor.resize(n, None);
        self.composite_ancestor.fill(None);
        self.composite_isolates.resize(n, false);
        self.composite_isolates.fill(false);

        let revision = source.structure_revision();
        if self.static_passthrough_len != n || self.static_passthrough_revision != revision {
            self.static_passthrough.clear();
            self.static_passthrough.resize(n, None);
            self.static_passthrough_len = n;
            self.static_passthrough_revision = revision;
        }

        source.tree().traverse_depth_first(|node_id| {
            let Some(node) = source.node(node_id) else {
                return;
            };
            let slot = node_id.0 as usize;

            let parent = source.tree().get_parent(node_id);

            let global_z = source.accumulated_z(node_id);
            let enabled = node.enabled
                && parent
                    .and_then(|p| self.enabled_cum.get(p.0 as usize).copied())
                    .unwrap_or(true);
            if slot < self.enabled_cum.len() {
                self.enabled_cum[slot] = enabled;
            }

            let nearest_composite = parent.and_then(|p| {
                let parent_node = source.node(p)?;
                let parent_isolates = self
                    .composite_isolates
                    .get(p.0 as usize)
                    .copied()
                    .unwrap_or(true);
                if matches!(parent_node.kind, NodeKind::Composite(_)) && parent_isolates {
                    Some(p)
                } else {
                    self.composite_ancestor.get(p.0 as usize).copied().flatten()
                }
            });
            if slot < self.composite_ancestor.len() {
                self.composite_ancestor[slot] = nearest_composite;
            }

            if !enabled {
                return;
            }

            // Opacity 0 culls every blend but Darken: `Min` ignores blend
            // factors, so a zero-alpha source still darkens.
            let culled = |opacity: f32, blend_mode: BlendMode| {
                opacity == 0.0 && blend_mode != BlendMode::Darken
            };

            match &node.kind {
                NodeKind::Composite(composite) => {
                    let isolates = nearest_composite.is_none()
                        || !self.composite_is_passthrough_group(source, node_id, composite);
                    if slot < self.composite_isolates.len() {
                        self.composite_isolates[slot] = isolates;
                    }
                    if !isolates {
                        return;
                    }
                    if culled(composite.opacity, composite.blend_mode) {
                        return;
                    }
                    let mask_sources = collect_mask_sources(
                        node_id,
                        source,
                        &mut render_list.composite_mask_sources,
                    );
                    let info = DrawableInfo::Composite {
                        node_id: node_id.0,
                        z_order: global_z,
                        blend_mode: composite.blend_mode,
                        opacity: composite.opacity,
                        tint: composite.tint,
                        screen_tint: composite.screen_tint,
                        mask_sources,
                        mask_threshold: composite.mask_threshold,
                    };

                    // An isolating composite is a drawable of its enclosing
                    // composite, so the outer's opacity/tint/blend/mask cover it.
                    match nearest_composite {
                        Some(c) => render_list
                            .composite_children
                            .entry(c.0)
                            .or_default()
                            .push(info),
                        None => render_list.root_drawables.push(info),
                    }
                }
                NodeKind::Part(part) => {
                    let texture_id = source.texture_slot(node);
                    if !source.has_texture(texture_id) {
                        return;
                    }
                    if culled(part.opacity, part.blend_mode) {
                        return;
                    }

                    let mask_sources = collect_mask_sources(
                        node_id,
                        source,
                        &mut render_list.composite_mask_sources,
                    );
                    let info = DrawableInfo::Part {
                        mesh_id: source.mesh_slot(node_id),
                        texture_id,
                        transform: source.transform(node_id),
                        z_order: global_z,
                        blend_mode: part.blend_mode,
                        opacity: part.opacity,
                        tint: part.tint,
                        screen_tint: part.screen_tint,
                        mask_sources,
                        mask_threshold: part.mask_threshold,
                    };

                    match nearest_composite {
                        Some(c) => render_list
                            .composite_children
                            .entry(c.0)
                            .or_default()
                            .push(info),
                        None => render_list.root_drawables.push(info),
                    }
                }
                NodeKind::MeshGroup(_)
                | NodeKind::MotionResponse
                | NodeKind::Group
                | NodeKind::Spine(_) => {}
            }
        });

        render_list
            .root_drawables
            .sort_by(|a, b| a.z_order().total_cmp(&b.z_order()));
        for children in render_list.composite_children.values_mut() {
            children.sort_by(|a, b| a.z_order().total_cmp(&b.z_order()));
        }
    }

    /// Only a composite whose slot would change nothing is flattened, so its
    /// Parts interleave by z with the enclosing composite's. OVER is
    /// associative, so dropping a no-op group boundary is exact.
    fn composite_is_passthrough_group<S: DrawSource + ?Sized>(
        &mut self,
        source: &S,
        node_id: NodeIdx,
        composite: &CompositeData,
    ) -> bool {
        let slot = node_id.0 as usize;
        let is_static = match self.static_passthrough.get(slot).copied().flatten() {
            Some(cached) => cached,
            None => {
                let computed = composite_passthrough_static(source, node_id, composite);
                if let Some(entry) = self.static_passthrough.get_mut(slot) {
                    *entry = Some(computed);
                }
                computed
            }
        };
        is_static && composite_passthrough_dynamic(composite)
    }
}
