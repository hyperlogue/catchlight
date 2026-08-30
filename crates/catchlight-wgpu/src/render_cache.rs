//! The renderer's derived copy of a model: [`RenderCache`].
//!
//! A cache is **prepared from a `Model`** and **refreshed from a `Puppet`**.
//! It is never authoritative — everything in it can be rebuilt from the model
//! it was prepared from, and nothing simulates here.
//!
//! Invariants this module enforces:
//!
//! - **`prepare` owns everything that survives a frame, `refresh` owns
//!   everything that does not.** `prepare` decodes and crops the model's
//!   textures, uploads them and every mesh, and assigns the slots the renderer
//!   keys its GPU state by. `refresh` uploads one frame's deforms and nothing
//!   else. A per-frame call that had to touch a texture or a vertex buffer
//!   would be a bug in that split, not a cost to optimize.
//! - **The generation gate is the only staleness check.** A cache records
//!   `Model::generation()` when it prepares; `refresh` compares it and rebuilds
//!   when it moved. A rebuild is currently the whole cache — per-node
//!   revisions are a later change, and only if profiling asks for one.
//! - **A cache, a puppet and a model handed to `refresh` together must
//!   agree.** All three carry the model's identity and its generation: the
//!   identity says they are the same model at all, the generation says they
//!   are the same *state* of it. A mismatch means the caller ticked against
//!   one model and refreshed against another. It is a programmer error and
//!   `debug_assert!`ed, not silently patched: the two would disagree about
//!   what a `NodeIdx` names.
//! - **The Idx arena is this crate's, and it is dense.** A `NodeIdx` is a
//!   node's position in the model's pre-order walk, which is exactly the
//!   puppet's arena order, so the two index each other with no lookup. A
//!   [`MeshIdx`] is handed out densely to meshed nodes as they upload, and a
//!   [`TexIdx`] is a position in `Model::texture_ids()`. All three are keyed
//!   by the model's Ids at prepare time and none of them leaves the crate:
//!   what reaches a caller is a `u32` slot in a [`RenderList`], meaningful
//!   only against the cache that produced it.
//! - **One cache, one renderer.** The GPU state a cache's slots name lives in
//!   the [`WgpuRenderer`] that prepared it. Two caches sharing a renderer
//!   would overwrite each other's mesh and texture slots, which is the same
//!   rule `renderer.rs` already states for two puppets.
//! - **UVs are remapped here, not in the model.** Textures are alpha-cropped
//!   before upload, and a model's meshes are authored against the *uncropped*
//!   images. The remap is applied to the vertex data this cache uploads, so
//!   the model keeps the UVs its author wrote.

use catchlight_core::components::{NodeIdx, NodeKind};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::{
    Mesh, MeshIndices, Model, ModelNodeKind, Node, NodeId, NodeTree, Puppet, TexturePrepCache,
    UvCrop, Vec2,
};

use crate::collect::{Collector, DrawSource, RenderList, NO_SLOT};
use crate::renderer::{RendererError, RendererResult, WgpuRenderer};

/// A slot in the renderer's mesh table — dense, handed out as meshes upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MeshIdx(pub(crate) u32);

/// A slot in the renderer's texture table — a position in
/// `Model::texture_ids()`, which is also what a baked part's albedo names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TexIdx(pub(crate) u32);

/// What a caller decides about how a model reaches the GPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrepareOptions {
    /// Halve every texture this many times before upload (0 = full
    /// resolution). A budget knob, not a model property: two caches over one
    /// model may choose differently.
    pub texture_halvings: u32,
    /// Keep a decode memo inside the cache, so rebuilding after a model edit
    /// skips re-decoding the textures that edit did not touch. It costs one
    /// decoded copy of every texture, so an editor wants it and a viewer that
    /// never edits does not.
    pub memoize_textures: bool,
}

/// The renderer's derived copy of a model.
///
/// Prepared from a [`Model`], refreshed from a [`Puppet`], collected into a
/// [`RenderList`]. Owns no simulation and no authored data.
pub struct RenderCache {
    /// The `Model::identity` of the model this cache is derived from.
    identity: u64,
    /// The `Model::generation` this cache was last built from.
    generation: u64,
    options: PrepareOptions,
    /// `NodeIdx.0` -> the model Id at that arena slot. The Id keying the task
    /// asks for, and what the debug-time agreement check compares against.
    node_ids: Vec<NodeId>,
    /// `NodeIdx.0` -> the mesh slot this node's geometry uploaded into.
    /// `None` for a node with no mesh, an empty mesh, or a mesh that failed
    /// to upload.
    mesh_of_node: Vec<Option<MeshIdx>>,
    /// How many texture slots are filled. A `TexIdx` at or above this names
    /// nothing, which is how an unmapped part is culled.
    texture_count: u32,
    /// The decode memo, when [`PrepareOptions::memoize_textures`] asked for
    /// one.
    texture_memo: Option<TexturePrepCache>,
    /// Collector scratch, kept here so a caller collecting every frame keeps
    /// its buffers and its pass-through memo.
    collector: Collector,
}

impl std::fmt::Debug for RenderCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderCache")
            .field("identity", &self.identity)
            .field("generation", &self.generation)
            .field("options", &self.options)
            .field("nodes", &self.node_ids.len())
            .field("meshes", &self.mesh_of_node.iter().flatten().count())
            .field("textures", &self.texture_count)
            .finish_non_exhaustive()
    }
}

impl RenderCache {
    /// Decode, crop and upload `model`'s textures, upload its meshes, and
    /// build the slot tables the renderer draws by.
    ///
    /// `renderer` holds the GPU state the returned cache's slots name; see
    /// "One cache, one renderer" above.
    pub fn prepare(
        renderer: &mut WgpuRenderer,
        model: &Model,
        options: PrepareOptions,
    ) -> RendererResult<Self> {
        let mut cache = Self {
            identity: model.identity(),
            generation: model.generation(),
            options,
            node_ids: Vec::new(),
            mesh_of_node: Vec::new(),
            texture_count: 0,
            texture_memo: options.memoize_textures.then(TexturePrepCache::default),
            collector: Collector::default(),
        };
        cache.rebuild(renderer, model)?;
        Ok(cache)
    }

    /// The `Model::generation` this cache was last built from.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The [`Model::identity`] of the model this cache is derived from.
    pub fn model_identity(&self) -> u64 {
        self.identity
    }

    /// How many textures this cache uploaded.
    pub fn texture_count(&self) -> usize {
        self.texture_count as usize
    }

    /// How many meshes this cache uploaded. Below the model's meshed-node
    /// count when a mesh was empty or failed to upload.
    pub fn mesh_count(&self) -> usize {
        self.mesh_of_node.iter().flatten().count()
    }

    /// Push one evaluated frame at the GPU: the puppet's combined deforms, for
    /// every node whose deform stack is active. A node whose stack has not
    /// moved since the last refresh is skipped by the renderer's
    /// generation memo, which is what makes a static frame free.
    ///
    /// Rebuilds the whole cache first if `model` has moved since it was
    /// prepared.
    pub fn refresh(
        &mut self,
        renderer: &mut WgpuRenderer,
        model: &Model,
        puppet: &Puppet,
    ) -> RendererResult<()> {
        debug_assert_eq!(
            self.identity,
            model.identity(),
            "refreshed a cache prepared from a different model; \
             a cache is derived from the model it was prepared from",
        );
        debug_assert_eq!(
            puppet.model_identity(),
            model.identity(),
            "refreshed a cache with a puppet built from a different model; \
             tick the puppet against the model you refresh with",
        );
        debug_assert_eq!(
            puppet.baked_generation(),
            model.generation(),
            "refreshed a cache with a puppet baked against a different model state; \
             tick the puppet against the model you refresh with",
        );
        if self.generation != model.generation() {
            self.rebuild(renderer, model)?;
        }
        debug_assert!(
            self.agrees_with(puppet),
            "the cache's node order and the puppet's arena order disagree",
        );

        let mesh_of_node = &self.mesh_of_node;
        let active = puppet.iter_deform_nodes().filter_map(|(idx, node)| {
            let stack = match &node.kind {
                NodeKind::Part(part) => &part.deform_stack,
                NodeKind::MeshGroup(group) => &group.deform_stack,
                _ => return None,
            };
            if !stack.is_active() {
                return None;
            }
            let slot = mesh_of_node.get(idx.0 as usize).copied().flatten()?;
            Some((slot.0, stack.generation(), stack.combined()))
        });
        renderer.upload_deforms(active);
        Ok(())
    }

    /// Collect `puppet`'s drawables into `render_list`, reusing this cache's
    /// collector scratch and the list's own allocations.
    pub fn collect_into(&mut self, puppet: &Puppet, render_list: &mut RenderList) {
        // Taken out and put back so the collector can borrow the cache's
        // tables while it runs; `Collector`'s buffers survive the round trip.
        let mut collector = std::mem::take(&mut self.collector);
        collector.collect_into(
            &CacheSource {
                cache: self,
                puppet,
            },
            render_list,
        );
        self.collector = collector;
    }

    /// The texture slot a baked part's albedo names, if this cache holds it.
    ///
    /// Identity, not a lookup: a baked albedo is already a position in
    /// `Model::texture_ids()`, and so is a [`TexIdx`]. An unmapped part
    /// carries `u32::MAX` and lands out of range here.
    fn tex_idx(&self, albedo: catchlight_core::TextureId) -> Option<TexIdx> {
        (albedo.0 < self.texture_count).then_some(TexIdx(albedo.0))
    }

    /// Whether the puppet's arena is the one this cache was built for. Only
    /// the debug assertions call it: it walks every node and compares Ids.
    fn agrees_with(&self, puppet: &Puppet) -> bool {
        self.node_ids.len() == puppet.len()
            && self
                .node_ids
                .iter()
                .enumerate()
                .all(|(slot, id)| puppet.node_id(NodeIdx(slot as u32)) == Some(id))
    }

    /// Rebuild every table and every GPU resource from `model`.
    ///
    /// Whole-cache on purpose: `Model::generation` is one counter, so nothing
    /// here can tell "a transform moved" from "the tree changed". A finer
    /// signal would let this patch instead, and is worth measuring before it
    /// is worth building.
    fn rebuild(&mut self, renderer: &mut WgpuRenderer, model: &Model) -> RendererResult<()> {
        let _span = tracing::trace_span!("render_cache::rebuild").entered();

        let textures: Vec<catchlight_core::formats::ModelTexture> = model
            .texture_ids()
            .iter()
            .filter_map(|id| model.texture(id))
            .map(Into::into)
            .collect();
        debug_assert_eq!(
            textures.len(),
            model.texture_ids().len(),
            "a model's texture order named a texture it does not hold",
        );
        let prepped = catchlight_core::prepare_textures(
            textures.as_slice(),
            self.options.texture_halvings,
            self.texture_memo.as_mut(),
        )
        .map_err(|error| RendererError::TexturePrep(error.to_string()))?;
        for (slot, prep) in prepped.iter().enumerate() {
            renderer.upload_texture(slot as u32, &prep.texture)?;
        }
        self.texture_count = prepped.len() as u32;

        // A texture slot is a position in `texture_ids()`, so a part's albedo
        // Id resolves to one by lookup here and by identity everywhere after.
        let tex_slot_of_id: std::collections::HashMap<_, u32> = model
            .texture_ids()
            .iter()
            .enumerate()
            .map(|(slot, id)| (id.clone(), slot as u32))
            .collect();

        renderer.clear_meshes();
        self.node_ids = model.nodes_in_order();
        self.mesh_of_node = vec![None; self.node_ids.len()];
        let mut next_mesh = 0u32;
        for (slot, id) in self.node_ids.iter().enumerate() {
            let Some(node) = model.node(id) else {
                continue;
            };
            let Some(authored) = model.node_mesh(id) else {
                continue;
            };
            if authored.verts.is_empty() {
                continue;
            }
            let is_part = matches!(node.kind, ModelNodeKind::Part(_));
            let uv_crop = match &node.kind {
                ModelNodeKind::Part(part) => part
                    .albedo()
                    .and_then(|tex| tex_slot_of_id.get(tex))
                    .and_then(|&s| prepped.get(s as usize))
                    .and_then(|prep| prep.uv_crop),
                _ => None,
            };
            let mesh = build_mesh(authored, uv_crop);
            // The format marks uvs optional; a MeshGroup without uvs is
            // normal (its mesh only drives deformation, never draws). A Part
            // without uvs *is* drawn — upload_mesh substitutes zeros, so flag
            // it.
            if is_part && mesh.uvs.is_empty() {
                tracing::warn!(
                    node = %id,
                    "part mesh has no uvs; substituting (0,0) for every vertex",
                );
            }
            // One quirky mesh must not blank every other part, so a failed
            // upload is a warning and no slot, exactly as the legacy path
            // treats it. Nothing draws it: the collector hands the renderer
            // a slot that misses.
            match renderer.upload_mesh(next_mesh, &mesh) {
                Ok(()) => {
                    self.mesh_of_node[slot] = Some(MeshIdx(next_mesh));
                    next_mesh += 1;
                }
                Err(error) => tracing::warn!(
                    node = %id,
                    "skipping mesh that failed upload: {error}",
                ),
            }
        }

        self.generation = model.generation();
        Ok(())
    }
}

/// Collect `puppet`'s drawables against `cache`.
///
/// Allocates a fresh list and a fresh collector; a caller drawing every frame
/// wants [`RenderCache::collect_into`], which keeps both.
pub fn collect(cache: &RenderCache, puppet: &Puppet) -> RenderList {
    let mut render_list = RenderList::default();
    Collector::default().collect_into(&CacheSource { cache, puppet }, &mut render_list);
    render_list
}

/// A model's authored mesh -> the vertex form the renderer uploads, with UVs
/// mapped onto the texture's alpha crop.
fn build_mesh(authored: &ClmMesh, uv_crop: Option<UvCrop>) -> Mesh {
    let vertices: Vec<Vec2> = authored
        .verts
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| Vec2::new(c[0], c[1]))
        .collect();
    let uvs: Vec<Vec2> = authored
        .uvs
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            let uv = Vec2::new(c[0], c[1]);
            match uv_crop {
                Some(crop) => crop.map(uv),
                None => uv,
            }
        })
        .collect();
    let indices = match &authored.indices {
        ClmIndices::U16(v) => MeshIndices::U16(v.clone()),
        ClmIndices::U32(v) => MeshIndices::U32(v.clone()),
    };
    Mesh::new(vertices, uvs, indices, Vec2::from_array(authored.origin))
}

/// A posed [`Puppet`] read through the cache that holds its GPU slots.
struct CacheSource<'a> {
    cache: &'a RenderCache,
    puppet: &'a Puppet,
}

impl DrawSource for CacheSource<'_> {
    fn tree(&self) -> &NodeTree {
        self.puppet.tree()
    }

    fn node(&self, idx: NodeIdx) -> Option<&Node> {
        self.puppet.get(idx)
    }

    fn node_count(&self) -> usize {
        self.puppet.len()
    }

    fn transform(&self, idx: NodeIdx) -> glam::Mat4 {
        self.puppet.transforms().get(idx)
    }

    fn structure_revision(&self) -> u64 {
        self.puppet.baked_generation()
    }

    fn mask_source(&self, source: u32) -> Option<NodeIdx> {
        // A baked `Mask` carries its source node's arena slot: the field is
        // the legacy uuid namespace and a model has none, so this cache
        // resolves it against its own node table rather than inventing one.
        // A mask naming a node the tree no longer has resolves to nothing.
        ((source as usize) < self.cache.node_ids.len()).then_some(NodeIdx(source))
    }

    fn mesh_slot(&self, idx: NodeIdx) -> u32 {
        self.cache
            .mesh_of_node
            .get(idx.0 as usize)
            .copied()
            .flatten()
            .map_or(NO_SLOT, |mesh| mesh.0)
    }

    fn texture_slot(&self, node: &Node) -> u32 {
        match &node.kind {
            NodeKind::Part(part) => self
                .cache
                .tex_idx(part.albedo_texture)
                .map_or(NO_SLOT, |tex| tex.0),
            _ => NO_SLOT,
        }
    }

    fn has_texture(&self, slot: u32) -> bool {
        slot < self.cache.texture_count
    }
}
