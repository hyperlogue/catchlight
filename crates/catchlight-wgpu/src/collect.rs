//! Collects a posed runtime's drawables into the `RenderList` the renderer
//! draws.
//!
//! Invariants this module enforces:
//!
//! - **Z order: higher `z_order` draws in front.** A node's z is its own
//!   plus every ancestor's, which is `Puppet::accumulated_z` and reaches this
//!   walk through `DrawSource::accumulated_z` — the rule is in core so the
//!   CLI's `poses` dump reports the same order the renderer draws. The sort
//!   is ascending, so the last draw is the frontmost. `.inx` is authored the
//!   other way round, lower in front; the flip happens at import, never
//!   here.
//! - **A disabled node hides its whole subtree**, so `enabled` is ANDed down
//!   the tree rather than read per node.
//! - **A pass-through composite is dropped, an isolating one is not.** See
//!   [`Collector::composite_is_passthrough_group`]: only a composite whose
//!   slot would change nothing is flattened away, and its parts then
//!   interleave by z with the enclosing composite's. An isolating composite is
//!   a drawable *of its enclosing composite*, never of the root, or the
//!   outer's opacity, tint, blend and mask would not cover it.
//! - **Opacity 0 culls everything but Darken**, whose `Min` blend ignores
//!   blend factors and darkens even at zero alpha.
//! - **The walk is behind [`DrawSource`].** It is what the walk needs from a
//!   posed runtime: the tree, the evaluated transforms, and the cache slots
//!   its resources landed in. The render cache is the one implementation; the
//!   indirection is what let a second runtime be swapped in underneath while
//!   the two paths were proved to agree on pixels, and what keeps this module
//!   free of GPU types.
//!
//! Everything the list names is a **slot** — a dense `u32` position in the
//! render cache's mesh, texture or node tables — never an Id. A list is only
//! meaningful against the cache it was collected from.

use crate::renderer::DeformSet;
use catchlight_core::{BlendMode, CompositeData, MaskMode, Node, NodeIdx, NodeKind, NodeTree};
use smallvec::SmallVec;
use std::collections::HashMap;

/// A slot that names nothing: used where a node has no mesh or no albedo, so
/// the renderer's dense-table probe misses and the draw is skipped and
/// counted, exactly as it is for a resource that failed to upload.
pub(crate) const NO_SLOT: u32 = u32::MAX;

/// What collecting needs from one posed runtime.
///
/// Deliberately narrow: the tree, the frame it evaluated, and where its
/// resources live in the render cache. Nothing here can pose, tick or upload.
pub(crate) trait DrawSource {
    fn tree(&self) -> &NodeTree;
    fn node(&self, idx: NodeIdx) -> Option<&Node>;
    fn node_count(&self) -> usize;
    /// The node's global transform from the last evaluated frame.
    fn transform(&self, idx: NodeIdx) -> glam::Mat4;
    /// The node's `z_order` summed with every ancestor's — the number this
    /// module sorts by. The rule lives on `Puppet`, so the CLI's `poses` dump
    /// and this walk cannot disagree about what is in front of what.
    fn accumulated_z(&self, idx: NodeIdx) -> f32;
    /// Bumped whenever the tree's shape changes. The collector caches the
    /// structural half of its pass-through verdict against it.
    fn structure_revision(&self) -> u64;
    /// The cache slot holding this node's uploaded mesh, or [`NO_SLOT`].
    fn mesh_slot(&self, idx: NodeIdx) -> u32;
    /// The cache slot holding this part's albedo, or [`NO_SLOT`]. May name a
    /// slot that was never uploaded; the renderer skips those.
    fn texture_slot(&self, node: &Node) -> u32;
    /// Whether `slot` names a texture this cache actually holds.
    fn has_texture(&self, slot: u32) -> bool;
}

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

#[derive(Debug, Default, Clone)]
pub struct RenderList {
    pub root_drawables: Vec<DrawableInfo>,
    /// Keyed by the composite's own slot, the `node_id` of its
    /// `DrawableInfo::Composite`.
    pub composite_children: HashMap<u32, Vec<DrawableInfo>>,
    pub composite_mask_sources: HashMap<u32, CompositeMaskSourceData>,
    /// Which puppet's deforms these draws read: the slice of the renderer's
    /// deform atlas that the same puppet's
    /// [`RenderCache::refresh_puppet`](crate::RenderCache::refresh_puppet)
    /// uploaded into. Left at [`DeformSet::FIRST`] by `collect_into`, which
    /// is the only set a cache serving one puppet ever uses.
    pub deform_set: DeformSet,
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

/// The structural half of the pass-through test — Normal blend, no mask, no
/// nested Composite descendant, and every descendant Part on Normal blend.
/// These change only with the tree's shape, which bumps
/// [`DrawSource::structure_revision`], so the collector caches this per
/// composite between revisions and only walks the subtree once. See
/// [`Collector::composite_is_passthrough_group`] for why pass-through groups
/// flatten at all.
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

/// The param-driven half of the pass-through test: full opacity and identity
/// tint/screen-tint. These are bound to params and change per frame, so this
/// is re-checked every frame rather than cached.
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
            NodeKind::MeshGroup(_) | NodeKind::Group | NodeKind::SimplePhysics(_) => {}
        }
    }
    sources
}

/// Per-frame scratch plus the cross-frame pass-through memo. One lives in the
/// render cache; a caller that collects repeatedly reuses it so the memo pays
/// off.
#[derive(Debug, Default)]
pub(crate) struct Collector {
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
    // frame. Rebuilt wholesale when the source's structure revision or node
    // count changes.
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

        // Unlike the per-frame buffers above, the structural pass-through
        // cache persists until the tree's shape changes.
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

            // Nearest enclosing *isolating* composite: a pass-through group
            // doesn't isolate, so its descendants attach to whatever
            // composite encloses the group, not to the group itself.
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

            // Opacity 0 contributes nothing for every blend mode except
            // Darken: BlendOperation::Min ignores blend factors, so a
            // zero-alpha src (rgb = 0 premultiplied) still darkens the
            // destination. Mask sources are unaffected — they're resolved
            // straight from the source runtime, and the reference
            // rasterizes masks without opacity. A culled Composite leaves
            // its children in composite_children, but nothing renders them
            // without the Composite drawable.
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
