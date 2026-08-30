//! Flattens a posed `LegacyPuppet` into the `RenderList` the renderer draws.
//!
//! **Z order: higher `z_order` draws in front.** `collect_drawables`
//! accumulates `parent_z + node.z_order` down the tree and sorts ascending, so
//! the last draw is the frontmost. `.inx` is authored the other way round,
//! lower in front; the flip happens at import, never here.

use catchlight_core::{
    BlendMode, CompositeData, GlobalTransforms, LegacyPuppet, MaskMode, NodeIdx, NodeKind,
};
use smallvec::SmallVec;
use std::collections::HashMap;

// SmallVec inline cap = 2 — typical part has 0 masks; rigs rarely exceed 2.
pub type MaskSources = SmallVec<[MaskSourceData; 2]>;

/// A mask source, with the render cache's slots for whatever the renderer
/// has to rasterize.
///
/// `mesh_id` / `texture_id` / `node_id` are **render-cache slots, not Ids** —
/// dense `u32` positions in the cache's own tables, meaningful only against
/// the cache the drawables were collected from.
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

/// One thing to draw, with the render cache's slots for its resources.
///
/// `mesh_id` / `texture_id` / `node_id` are **render-cache slots, not Ids**:
/// dense `u32` positions in the cache's mesh, texture and node tables. The
/// renderer indexes its GPU state by them and nothing outside a cache and the
/// list collected from it can interpret them.
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

#[derive(Debug, Default)]
pub struct RenderList {
    pub root_drawables: Vec<DrawableInfo>,
    /// Keyed by the composite's own slot, the `node_id` of its
    /// `DrawableInfo::Composite`.
    pub composite_children: HashMap<u32, Vec<DrawableInfo>>,
    pub composite_mask_sources: HashMap<u32, CompositeMaskSourceData>,
}

impl Clone for RenderList {
    fn clone(&self) -> Self {
        Self {
            root_drawables: self.root_drawables.clone(),
            composite_children: self.composite_children.clone(),
            composite_mask_sources: self.composite_mask_sources.clone(),
        }
    }

    // Hand-written so the bevy extract refill (`clone_from` into a
    // retained snapshot every frame) reuses existing allocations: the
    // derived default would allocate a fresh Vec/HashMap each call.
    // HashMap::clone_from drops and re-clones its values, so the nested
    // child Vecs are clone_from'd per key to keep their buffers too.
    fn clone_from(&mut self, source: &Self) {
        self.root_drawables.clone_from(&source.root_drawables);
        self.composite_children
            .retain(|k, _| source.composite_children.contains_key(k));
        for (k, v) in &source.composite_children {
            self.composite_children.entry(*k).or_default().clone_from(v);
        }
        self.composite_mask_sources
            .retain(|key, _| source.composite_mask_sources.contains_key(key));
        for (key, value) in &source.composite_mask_sources {
            match self.composite_mask_sources.entry(*key) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let retained = entry.get_mut();
                    retained.opacity = value.opacity;
                    retained.mask_threshold = value.mask_threshold;
                    retained.parts.clone_from(&value.parts);
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(value.clone());
                }
            }
        }
    }
}

impl RenderList {
    /// Upper-bound on the instance count needed for one frame. The
    /// renderer uses this to size its instance buffer once at frame
    /// start, avoiding mid-frame growth (which would strand already
    /// recorded passes on the old buffer).
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

    /// Upper bound on mask-source draws in one frame. Each mask-source
    /// draw writes its own part-uniform slot (the source's threshold),
    /// and a drawable's sources rasterize at most once per frame, so
    /// the per-frame uniform buffer is sized with this at frame start.
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

    /// Clears in place: composite-children Vecs keep their buffers so
    /// the per-frame refill doesn't reallocate them. Consumers treat a
    /// stale empty entry exactly like a missing one.
    pub fn clear(&mut self) {
        self.root_drawables.clear();
        for children in self.composite_children.values_mut() {
            children.clear();
        }
        self.composite_mask_sources.clear();
    }
}

#[derive(Debug, Default)]
pub struct DrawableCollector {
    cumul_z: Vec<f32>,
    // enabled ANDed down the tree — a disabled node hides its whole subtree,
    // because a disabled ancestor hides its entire subtree.
    enabled_cum: Vec<bool>,
    composite_ancestor: Vec<Option<NodeIdx>>,
    // Per composite node: does it need its own offscreen slot, or is it a
    // pass-through group whose Parts flatten into the enclosing composite?
    // Indexed by NodeIdx; set when the composite is visited (pre-order), read
    // by its descendants to find the nearest *isolating* composite.
    composite_isolates: Vec<bool>,
    // Cached structural half of the pass-through predicate, per composite
    // NodeIdx slot (`None` until that composite is first visited). This half
    // depends only on the tree shape and static blend/mask state, which
    // survives across frames — only the param-driven half is re-checked each
    // frame. Rebuilt wholesale when the puppet's compiled-node revision or
    // node count changes.
    static_passthrough: Vec<Option<bool>>,
    static_passthrough_len: usize,
    static_passthrough_revision: u64,
}

/// The structural half of the pass-through test — Normal blend, no mask, no
/// nested Composite descendant, and every descendant Part on Normal blend.
/// These change only through the puppet's compiled-node mutation APIs, which
/// bump `node_revision`, so the collector caches this per composite between
/// revisions and only walks the subtree once. See
/// `DrawableCollector::composite_is_passthrough_group` for why pass-through
/// groups flatten at all.
fn composite_passthrough_static(
    puppet: &LegacyPuppet,
    node_id: NodeIdx,
    composite: &CompositeData,
) -> bool {
    if composite.blend_mode != BlendMode::Normal || !composite.masks.is_empty() {
        return false;
    }
    let mut stack = puppet.tree().get_children(node_id);
    while let Some(id) = stack.pop() {
        match puppet.get(id).map(|n| &n.kind) {
            Some(NodeKind::Composite(_)) => return false,
            Some(NodeKind::Part(part)) if part.blend_mode != BlendMode::Normal => return false,
            _ => {}
        }
        stack.extend(puppet.tree().get_children(id));
    }
    true
}

/// The param-driven half of the pass-through test: full opacity and identity
/// tint/screen-tint. These are bound to params and change per frame, so this
/// is re-checked every frame rather than cached.
fn composite_passthrough_dynamic(composite: &CompositeData) -> bool {
    composite.opacity == 1.0
        && composite.tint == glam::Vec3::ONE
        && composite.screen_tint == glam::Vec3::ZERO
}

fn collect_mask_sources(
    node_id: NodeIdx,
    puppet: &LegacyPuppet,
    transforms: &GlobalTransforms,
    composite_sources: &mut HashMap<u32, CompositeMaskSourceData>,
) -> MaskSources {
    let Some(node) = puppet.get(node_id) else {
        return MaskSources::new();
    };

    let masks = match &node.kind {
        NodeKind::Part(part) => &part.masks,
        NodeKind::Composite(composite) => &composite.masks,
        _ => return MaskSources::new(),
    };

    let mut sources = MaskSources::new();
    for binding in masks {
        let Some(mask_node_id) = puppet.node_for_uuid(binding.source_uuid) else {
            continue;
        };
        let Some(mask_node) = puppet.get(mask_node_id) else {
            continue;
        };
        match &mask_node.kind {
            NodeKind::Part(part) => sources.push(MaskSourceData::Part {
                mesh_id: mask_node_id.0,
                texture_id: part.albedo_texture.0,
                transform: transforms.get(mask_node_id),
                mode: binding.mode,
                mask_threshold: part.mask_threshold,
            }),
            NodeKind::Composite(composite) => {
                composite_sources.entry(mask_node_id.0).or_insert_with(|| {
                    let mut parts = Vec::new();
                    let mut stack = puppet.tree().get_children(mask_node_id);
                    while let Some(descendant) = stack.pop() {
                        if let Some(NodeKind::Part(part)) =
                            puppet.get(descendant).map(|node| &node.kind)
                        {
                            if (part.albedo_texture.0 as usize) < puppet.textures().len() {
                                parts.push(CompositeMaskPartData {
                                    mesh_id: descendant.0,
                                    texture_id: part.albedo_texture.0,
                                    transform: transforms.get(descendant),
                                    mask_threshold: part.mask_threshold,
                                });
                            }
                        }
                        stack.extend(puppet.tree().get_children(descendant));
                    }
                    CompositeMaskSourceData {
                        opacity: composite.opacity,
                        mask_threshold: composite.mask_threshold,
                        parts,
                    }
                });
                sources.push(MaskSourceData::Composite {
                    node_id: mask_node_id.0,
                    mode: binding.mode,
                });
            }
            NodeKind::MeshGroup(_) | NodeKind::Group | NodeKind::SimplePhysics(_) => {}
        }
    }
    sources
}

pub fn collect_drawables(puppet: &LegacyPuppet, transforms: &GlobalTransforms) -> RenderList {
    let mut collector = DrawableCollector::default();
    let mut render_list = RenderList::default();
    collector.collect_into(puppet, transforms, &mut render_list);
    render_list
}

impl DrawableCollector {
    pub fn collect_into(
        &mut self,
        puppet: &LegacyPuppet,
        transforms: &GlobalTransforms,
        render_list: &mut RenderList,
    ) {
        render_list.clear();

        let n = puppet.len();
        self.cumul_z.resize(n, 0.0);
        self.cumul_z.fill(0.0);
        self.enabled_cum.resize(n, true);
        self.enabled_cum.fill(true);
        self.composite_ancestor.resize(n, None);
        self.composite_ancestor.fill(None);
        self.composite_isolates.resize(n, false);
        self.composite_isolates.fill(false);

        // Unlike the per-frame buffers above, the structural pass-through
        // cache persists until a compiled-node mutation invalidates it.
        let revision = puppet.node_revision();
        if self.static_passthrough_len != n || self.static_passthrough_revision != revision {
            self.static_passthrough.clear();
            self.static_passthrough.resize(n, None);
            self.static_passthrough_len = n;
            self.static_passthrough_revision = revision;
        }

        puppet.tree().traverse_depth_first(|node_id| {
            let Some(node) = puppet.get(node_id) else {
                return;
            };
            let slot = node_id.0 as usize;

            let parent = puppet.tree().get_parent(node_id);

            let parent_z = parent
                .and_then(|p| self.cumul_z.get(p.0 as usize).copied())
                .unwrap_or(0.0);
            let global_z = parent_z + node.z_order;
            if slot < self.cumul_z.len() {
                self.cumul_z[slot] = global_z;
            }
            let enabled = node.enabled
                && parent
                    .and_then(|p| self.enabled_cum.get(p.0 as usize).copied())
                    .unwrap_or(true);
            if slot < self.enabled_cum.len() {
                self.enabled_cum[slot] = enabled;
            }

            // Nearest enclosing *isolating* composite: a pass-through group
            // doesn't isolate, so its descendants attach to whatever
            // composite encloses the group, not to the group itself.
            let nearest_composite = parent.and_then(|p| {
                let parent_node = puppet.get(p)?;
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

            // Opacity 0 contributes nothing for every blend mode except
            // Darken: BlendOperation::Min ignores blend factors, so a
            // zero-alpha src (rgb = 0 premultiplied) still darkens the
            // destination. Mask sources are unaffected — they're
            // resolved by UUID straight from the puppet, and the
            // reference rasterizes masks without opacity. A culled
            // Composite leaves its children in composite_children, but
            // nothing renders them without the Composite drawable.
            let culled = |opacity: f32, blend_mode: BlendMode| {
                opacity == 0.0 && blend_mode != BlendMode::Darken
            };

            match &node.kind {
                NodeKind::Composite(composite) => {
                    // A pass-through group nested inside another composite is
                    // flattened away: its Parts route to `nearest_composite`
                    // and interleave there by z. A composite at the root, or
                    // one that genuinely isolates, keeps its own slot.
                    let isolates = nearest_composite.is_none()
                        || !self.composite_is_passthrough_group(puppet, node_id, composite);
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
                        puppet,
                        transforms,
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
                    // composite, exactly like a Part: it renders into its own
                    // slot and that slot blits into the *enclosing* one, so
                    // the outer's opacity/tint/blend/mask cover it and it
                    // z-interleaves with the outer's other children. Pushing
                    // it to `root_drawables` instead would escape all of that
                    // and sort it against the root's drawables.
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
                    if part.albedo_texture.0 as usize >= puppet.textures().len() {
                        return;
                    }
                    if culled(part.opacity, part.blend_mode) {
                        return;
                    }

                    let mask_sources = collect_mask_sources(
                        node_id,
                        puppet,
                        transforms,
                        &mut render_list.composite_mask_sources,
                    );
                    let info = DrawableInfo::Part {
                        mesh_id: node_id.0,
                        texture_id: part.albedo_texture.0,
                        transform: transforms.get(node_id),
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
                NodeKind::MeshGroup(_) | NodeKind::Group | NodeKind::SimplePhysics(_) => {}
            }
        });

        render_list
            .root_drawables
            .sort_by(|a, b| a.z_order().total_cmp(&b.z_order()));
        for children in render_list.composite_children.values_mut() {
            children.sort_by(|a, b| a.z_order().total_cmp(&b.z_order()));
        }
    }

    /// A pass-through Composite — Normal blend, full opacity, identity
    /// tint/screen-tint, no mask, no nested Composite, and every descendant
    /// Part on Normal blend — isolates nothing, so its Parts render straight
    /// into the enclosing composite, interleaved by cumulative z-order. OVER
    /// composition is associative, so dropping the group boundary changes
    /// nothing *within* the group; the intended effect is the cross-group
    /// interleaving it enables — e.g. an eye group's lashes now sorting
    /// *behind* the hair bangs that occlude them, instead of the whole group
    /// painting on top.
    ///
    /// Only no-op group boundaries are flattened. An isolating group must keep
    /// its slot or its blend, opacity, tint, or mask would be silently dropped.
    ///
    /// The verdict splits into a cached structural half
    /// (`composite_passthrough_static`, walked once per composite) and a
    /// per-frame param-driven half (`composite_passthrough_dynamic`), so the
    /// hot path never re-walks the subtree.
    fn composite_is_passthrough_group(
        &mut self,
        puppet: &LegacyPuppet,
        node_id: NodeIdx,
        composite: &CompositeData,
    ) -> bool {
        let slot = node_id.0 as usize;
        let is_static = match self.static_passthrough.get(slot).copied().flatten() {
            Some(cached) => cached,
            None => {
                let computed = composite_passthrough_static(puppet, node_id, composite);
                if let Some(entry) = self.static_passthrough.get_mut(slot) {
                    *entry = Some(computed);
                }
                computed
            }
        };
        is_static && composite_passthrough_dynamic(composite)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::{
        CompositeData, LegacyPuppet, Mask, MaskMode, Node, PartData, PuppetTexture,
    };

    fn one_pixel_texture() -> PuppetTexture {
        PuppetTexture {
            width: 1,
            height: 1,
            rgba: vec![255, 255, 255, 255].into(),
        }
    }

    fn part_node() -> Node {
        Node {
            kind: NodeKind::Part(Box::<PartData>::default()),
            ..Default::default()
        }
    }

    #[test]
    fn composite_masks_are_collected_and_counted() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);

        let mask_id = puppet.insert_child(puppet.root(), part_node(), Some(7));
        let composite = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Composite(Box::new(CompositeData {
                    masks: vec![Mask {
                        source_uuid: 7,
                        mode: MaskMode::DodgeMask,
                    }],
                    ..Default::default()
                })),
                ..Default::default()
            },
            Some(8),
        );
        puppet.insert_child(composite, part_node(), Some(9));

        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);
        let render_list = collect_drawables(&puppet, &transforms);

        let mask_sources = render_list
            .root_drawables
            .iter()
            .find_map(|d| match d {
                DrawableInfo::Composite {
                    node_id,
                    mask_sources,
                    ..
                } if *node_id == composite.0 => Some(mask_sources),
                _ => None,
            })
            .expect("composite drawable");

        assert_eq!(mask_sources.len(), 1);
        assert!(matches!(
            mask_sources[0],
            MaskSourceData::Part {
                mesh_id,
                mode: MaskMode::DodgeMask,
                ..
            } if mesh_id == mask_id.0
        ));
        assert_eq!(render_list.total_instance_count(), 3);
    }

    #[test]
    fn composite_mask_sources_retain_descendant_part_shapes() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);

        let source = puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Composite(Box::new(CompositeData {
                    opacity: 0.75,
                    mask_threshold: 0.6,
                    ..Default::default()
                })),
                ..Default::default()
            },
            Some(20),
        );
        let source_part = puppet.insert_child(source, part_node(), Some(21));
        puppet.insert_child(
            puppet.root(),
            Node {
                kind: NodeKind::Part(Box::new(PartData {
                    masks: vec![Mask {
                        source_uuid: 20,
                        mode: MaskMode::Mask,
                    }],
                    ..Default::default()
                })),
                ..Default::default()
            },
            Some(22),
        );

        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);
        let render_list = collect_drawables(&puppet, &transforms);

        let source_data = render_list
            .composite_mask_sources
            .get(&source.0)
            .expect("composite mask source");
        assert_eq!(source_data.opacity, 0.75);
        assert_eq!(source_data.mask_threshold, 0.6);
        assert_eq!(source_data.parts.len(), 1);
        assert_eq!(source_data.parts[0].mesh_id, source_part.0);
        assert_eq!(render_list.total_instance_count(), 3);
    }

    /// A pass-through Composite (Normal/opaque/identity/no-mask, all-Normal
    /// children, no nested Composite) inside another Composite is flattened:
    /// its Parts join the enclosing composite's children, interleaved by
    /// cumulative z, and it emits no Composite drawable of its own. This lets
    /// a Part it holds sort *behind* a more-positive-z Part of the enclosing
    /// composite — the eyelash-behind-hair-bangs case. Without flattening the
    /// inner composite hoists to root and paints on top of the bangs.
    #[test]
    fn passthrough_nested_composite_flattens_into_enclosing() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);

        let composite_node = |data: CompositeData| Node {
            kind: NodeKind::Composite(Box::new(data)),
            ..Default::default()
        };
        let part_z = |z: f32| Node {
            z_order: z,
            ..part_node()
        };

        // Outer isolates (it is a root composite). A direct Part at z=10
        // draws in front; the nested pass-through composite holds a Part at
        // z=0 that must sort behind it.
        let outer = puppet.insert_child(
            puppet.root(),
            composite_node(CompositeData::default()),
            Some(10),
        );
        let front = puppet.insert_child(outer, part_z(10.0), Some(11));
        let inner = puppet.insert_child(outer, composite_node(CompositeData::default()), Some(12));
        let behind = puppet.insert_child(inner, part_z(0.0), Some(13));

        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);
        let render_list = collect_drawables(&puppet, &transforms);

        // Only the outer composite emits a drawable; inner is flattened away.
        let composites: Vec<u32> = render_list
            .root_drawables
            .iter()
            .filter_map(|d| match d {
                DrawableInfo::Composite { node_id, .. } => Some(*node_id),
                _ => None,
            })
            .collect();
        assert_eq!(composites, vec![outer.0]);

        // Both Parts are children of the outer composite, sorted ascending
        // by z (front-most last): z=0 behind, then z=10 front.
        let kids = render_list
            .composite_children
            .get(&outer.0)
            .expect("outer composite children");
        let kid_ids: Vec<u32> = kids
            .iter()
            .filter_map(|d| match d {
                DrawableInfo::Part { mesh_id, .. } => Some(*mesh_id),
                _ => None,
            })
            .collect();
        assert_eq!(kid_ids, vec![behind.0, front.0]);
    }

    #[test]
    fn drawable_order_is_low_to_high_and_stable_for_equal_z() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);
        let root = puppet.root();
        let first_front = puppet.insert_child(
            root,
            Node {
                z_order: 2.0,
                ..part_node()
            },
            None,
        );
        let behind = puppet.insert_child(
            root,
            Node {
                z_order: -1.0,
                ..part_node()
            },
            None,
        );
        let last_front = puppet.insert_child(
            root,
            Node {
                z_order: 2.0,
                ..part_node()
            },
            None,
        );
        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);

        let render_list = collect_drawables(&puppet, &transforms);
        let ids = render_list
            .root_drawables
            .iter()
            .filter_map(|drawable| match drawable {
                DrawableInfo::Part { mesh_id, .. } => Some(*mesh_id),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(ids, vec![behind.0, first_front.0, last_front.0]);
    }

    /// An inner Composite that isolates something — here a Multiply child,
    /// which reads the destination — is NOT flattened: it keeps its own
    /// drawable so the child blends against the isolated buffer, not the
    /// enclosing composite's accumulated content. That drawable belongs to
    /// the *enclosing* composite, not to the root: the renderer blits the
    /// inner's slot into the outer's, so the outer's opacity/tint/blend/mask
    /// cover it and it z-sorts among the outer's children. Escaping to
    /// `root_drawables` would render it straight to the framebuffer at root z.
    #[test]
    fn isolating_nested_composite_is_not_flattened() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);
        let composite_node = || Node {
            kind: NodeKind::Composite(Box::default()),
            ..Default::default()
        };

        let outer = puppet.insert_child(puppet.root(), composite_node(), Some(10));
        let inner = puppet.insert_child(outer, composite_node(), Some(12));
        let multiply = Node {
            kind: NodeKind::Part(Box::new(PartData {
                blend_mode: BlendMode::Multiply,
                ..Default::default()
            })),
            ..Default::default()
        };
        puppet.insert_child(inner, multiply, Some(13));

        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);
        let render_list = collect_drawables(&puppet, &transforms);

        let composites = |drawables: &[DrawableInfo]| -> Vec<u32> {
            drawables
                .iter()
                .filter_map(|d| match d {
                    DrawableInfo::Composite { node_id, .. } => Some(*node_id),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(
            composites(&render_list.root_drawables),
            vec![outer.0],
            "only the outer composite is a root drawable"
        );
        let outer_kids = render_list
            .composite_children
            .get(&outer.0)
            .expect("outer composite children");
        assert_eq!(
            composites(outer_kids),
            vec![inner.0],
            "the isolating nested composite is a child drawable of the outer"
        );
        assert!(
            render_list
                .composite_children
                .get(&inner.0)
                .is_some_and(|kids| kids.len() == 1),
            "the inner composite keeps its own child list"
        );
    }

    /// Opacity 0 is a no-op for every blend mode except Darken
    /// (BlendOperation::Min ignores blend factors, so a zero-alpha src
    /// still darkens), so the collector culls all but Darken.
    #[test]
    fn opacity_zero_drawables_are_culled_except_darken() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);

        let part_with = |opacity: f32, blend_mode| Node {
            kind: NodeKind::Part(Box::new(PartData {
                opacity,
                blend_mode,
                ..Default::default()
            })),
            ..Default::default()
        };
        let root = puppet.root();
        puppet.insert_child(root, part_with(0.0, BlendMode::Normal), Some(1));
        let darken = puppet.insert_child(root, part_with(0.0, BlendMode::Darken), Some(2));
        let visible = puppet.insert_child(root, part_with(0.5, BlendMode::Normal), Some(3));

        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);
        let render_list = collect_drawables(&puppet, &transforms);

        let drawn: Vec<u32> = render_list
            .root_drawables
            .iter()
            .filter_map(|d| match d {
                DrawableInfo::Part { mesh_id, .. } => Some(*mesh_id),
                _ => None,
            })
            .collect();
        assert_eq!(drawn.len(), 2);
        assert!(drawn.contains(&darken.0));
        assert!(drawn.contains(&visible.0));
    }

    /// The cached structural pass-through verdict must be re-evaluated when
    /// the puppet grows. A collector reused across frames caches "inner is a
    /// pass-through group" on the first frame; inserting a Multiply Part
    /// under `inner` makes it genuinely isolating, and the node-count change
    /// must invalidate the cache so the second frame stops flattening it.
    #[test]
    fn static_passthrough_cache_invalidates_when_puppet_grows() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);
        let composite_node = || Node {
            kind: NodeKind::Composite(Box::<CompositeData>::default()),
            ..Default::default()
        };

        let outer = puppet.insert_child(puppet.root(), composite_node(), Some(10));
        let inner = puppet.insert_child(outer, composite_node(), Some(12));
        puppet.insert_child(inner, part_node(), Some(13));

        // Every Composite drawable in the list, wherever it is routed: the
        // question here is whether `inner` emits one at all, not where it
        // lands (`isolating_nested_composite_is_not_flattened` pins that).
        let all_composites = |render_list: &RenderList| -> Vec<u32> {
            render_list
                .root_drawables
                .iter()
                .chain(render_list.composite_children.values().flatten())
                .filter_map(|d| match d {
                    DrawableInfo::Composite { node_id, .. } => Some(*node_id),
                    _ => None,
                })
                .collect()
        };

        let mut collector = DrawableCollector::default();
        let mut render_list = RenderList::default();

        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);
        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert_eq!(
            all_composites(&render_list),
            vec![outer.0],
            "inner starts as a pass-through group and flattens into outer"
        );

        // Grow the tree: a Multiply Part under inner makes it isolating.
        let multiply = Node {
            kind: NodeKind::Part(Box::new(PartData {
                blend_mode: BlendMode::Multiply,
                ..Default::default()
            })),
            ..Default::default()
        };
        puppet.insert_child(inner, multiply, Some(14));
        puppet.compute_transforms(&mut transforms);
        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert!(
            all_composites(&render_list).contains(&inner.0),
            "growth must invalidate the cache so inner isolates again, got {:?}",
            all_composites(&render_list)
        );
    }

    #[test]
    fn static_passthrough_cache_tracks_blend_and_mask_mutations() {
        let mut puppet = LegacyPuppet::new();
        puppet.set_textures(vec![one_pixel_texture()]);
        let composite_node = || Node {
            kind: NodeKind::Composite(Box::<CompositeData>::default()),
            ..Default::default()
        };
        let outer = puppet.insert_child(puppet.root(), composite_node(), Some(10));
        let inner = puppet.insert_child(outer, composite_node(), Some(11));
        puppet.insert_child(inner, part_node(), Some(12));

        let mut collector = DrawableCollector::default();
        let mut render_list = RenderList::default();
        let mut transforms = GlobalTransforms::default();
        puppet.compute_transforms(&mut transforms);

        let emits_inner = |list: &RenderList| {
            list.root_drawables
                .iter()
                .chain(list.composite_children.values().flatten())
                .any(|drawable| {
                    matches!(drawable, DrawableInfo::Composite { node_id, .. } if *node_id == inner.0)
                })
        };

        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert!(!emits_inner(&render_list));

        assert!(puppet.set_node_blend_mode(inner, BlendMode::Multiply));
        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert!(emits_inner(&render_list));

        assert!(puppet.set_node_blend_mode(inner, BlendMode::Normal));
        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert!(!emits_inner(&render_list));

        assert!(puppet.set_node_masks(
            inner,
            vec![Mask {
                source_uuid: 999,
                mode: MaskMode::Mask,
            }]
        ));
        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert!(emits_inner(&render_list));

        assert!(puppet.set_node_masks(inner, Vec::new()));
        collector.collect_into(&puppet, &transforms, &mut render_list);
        assert!(!emits_inner(&render_list));
    }
}
