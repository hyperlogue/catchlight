//! The renderer's derived copy of a model: [`RenderCache`].
//!
//! - A cache's slots name GPU state in the [`WgpuRenderer`] that prepared it;
//!   two caches sharing a renderer overwrite each other's slots. Every puppet
//!   of one model shares one cache, each with its own
//!   [`DeformSet`], which only that renderer accepts.
//! - A cache, puppet and model passed to `refresh` together must share
//!   identity and generation; a mismatch is a caller bug, `debug_assert!`ed.
//! - A rebuild keeps a GPU texture by Id when its payload `Arc` is
//!   pointer-equal (payloads are immutable), wherever its slot moved.
//! - UVs are remapped onto the texture's alpha crop in uploaded vertex data,
//!   not in the model.

use std::sync::Arc;

use catchlight_core::components::{NodeIdx, NodeKind};
use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::load_budget::{LoadBudget, LoadLimits};
use catchlight_core::{
    Mesh, MeshIndices, Model, ModelMesh, ModelNodeKind, ModelTexture, Node, NodeId, NodeTree,
    Puppet, TexId, UvCrop, Vec2,
};

use crate::collect::{Collector, DrawSource, NO_SLOT, RenderList};
use crate::renderer::{DeformSet, RendererResult, WgpuRenderer};

/// A slot in the renderer's mesh table — dense, handed out as meshes upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct MeshIdx(pub(crate) u32);

/// A slot in the renderer's texture table — a position in
/// `Model::texture_ids()`, which is also what a baked part's albedo names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct TexIdx(pub(crate) u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareOptions {
    /// Halve every texture this many times before upload (0 = full
    /// resolution).
    pub texture_halvings: u32,
    /// Upper bound on all textures decoded as RGBA8 at header size, kept or
    /// fresh, checked before any decode; a failing build leaves the previous
    /// build resident.
    pub max_decoded_texture_bytes: u64,
}

impl Default for PrepareOptions {
    fn default() -> Self {
        Self {
            texture_halvings: 0,
            max_decoded_texture_bytes: LoadLimits::default().decoded_texture_bytes,
        }
    }
}

/// Cumulative since prepare. `texture_uploads + textures_kept` is the model's
/// texture count summed over `rebuilds`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Includes the build [`RenderCache::prepare`] runs.
    pub rebuilds: u64,
    pub texture_uploads: u64,
    /// Same Id and pointer-equal payload `Arc` as the previous build.
    pub textures_kept: u64,
    pub mesh_uploads: u64,
    /// Same node, mesh contents, and UV crop as the previous build.
    pub meshes_kept: u64,
    /// Parts left undrawn because their mesh failed to upload.
    pub meshes_skipped: u64,
}

struct UploadedMesh {
    id: NodeId,
    slot: u32,
    mesh: ModelMesh,
    uv_crop: Option<UvCrop>,
}

#[derive(Debug, Clone)]
struct UploadedTexture {
    id: TexId,
    slot: u32,
    payload: Arc<[u8]>,
    /// Kept because a reused texture is never decoded again.
    uv_crop: Option<UvCrop>,
    source_size: [u32; 2],
}

impl UploadedTexture {
    fn holds(&self, texture: &ModelTexture) -> bool {
        Arc::ptr_eq(&self.payload, &texture.data)
    }
}

pub struct RenderCache {
    identity: u64,
    generation: u64,
    options: PrepareOptions,
    /// Indexed by `NodeIdx.0`.
    node_ids: Vec<NodeId>,
    /// Indexed by `NodeIdx.0`; `None` when the node has no uploaded mesh.
    mesh_of_node: Vec<Option<MeshIdx>>,
    /// A `TexIdx` at or above this names nothing, which culls an unmapped part.
    texture_count: u32,
    /// In slot order.
    uploaded: Vec<UploadedTexture>,
    meshes: Vec<UploadedMesh>,
    stats: CacheStats,
    collector: Collector,
    /// Indexed by mesh slot.
    retained_scratch: Vec<bool>,
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
            uploaded: Vec::new(),
            meshes: Vec::new(),
            stats: CacheStats::default(),
            collector: Collector::default(),
            retained_scratch: Vec::new(),
        };
        cache.rebuild(renderer, model)?;
        Ok(cache)
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn model_identity(&self) -> u64 {
        self.identity
    }

    #[must_use]
    pub const fn texture_count(&self) -> usize {
        self.texture_count as usize
    }

    #[must_use]
    pub const fn stats(&self) -> CacheStats {
        self.stats
    }

    #[must_use]
    pub fn mesh_count(&self) -> usize {
        self.mesh_of_node.iter().flatten().count()
    }

    /// The mesh slot a [`RenderList`]'s `mesh_id` carries for `node`'s part.
    #[must_use]
    pub fn mesh_slot_of_node(&self, node: u32) -> Option<u32> {
        self.mesh_of_node
            .get(node as usize)
            .copied()
            .flatten()
            .map(|mesh| mesh.0)
    }

    /// Uploads deforms into the renderer's first deform set, rebuilding first
    /// if `model` moved. For several puppets of one model, use
    /// [`Self::refresh_puppet`].
    pub fn refresh(
        &mut self,
        renderer: &mut WgpuRenderer,
        model: &Model,
        puppet: &Puppet,
    ) -> RendererResult<()> {
        let set = renderer.first_deform_set();
        self.refresh_deforms(renderer, model, puppet, set)
    }

    /// Take `set` from
    /// [`WgpuRenderer::acquire_deform_set`](crate::WgpuRenderer::acquire_deform_set)
    /// and hand it back when the puppet goes.
    pub fn refresh_puppet(
        &mut self,
        renderer: &mut WgpuRenderer,
        model: &Model,
        puppet: &Puppet,
        set: DeformSet,
        render_list: &mut RenderList,
    ) -> RendererResult<()> {
        self.refresh_deforms(renderer, model, puppet, set)?;
        self.collect_into(puppet, render_list);
        render_list.deform_set = Some(set);
        Ok(())
    }

    fn refresh_deforms(
        &mut self,
        renderer: &mut WgpuRenderer,
        model: &Model,
        puppet: &Puppet,
        set: DeformSet,
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
            let NodeKind::Part(part) = &node.kind else {
                return None;
            };
            let stack = &part.deform_stack;
            if !stack.is_active() {
                return None;
            }
            let slot = mesh_of_node.get(idx.0 as usize).copied().flatten()?;
            Some((slot.0, stack.generation(), stack.combined()))
        });
        renderer.upload_deforms(set, active)
    }

    pub fn collect_into(&mut self, puppet: &Puppet, render_list: &mut RenderList) {
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

    /// Apply after collection; the next collection restores the full list.
    pub fn retain_part_colors(
        &mut self,
        render_list: &mut RenderList,
        mut keep: impl FnMut(&NodeId) -> bool,
    ) {
        let retained = &mut self.retained_scratch;
        retained.clear();
        for (id, mesh) in self.node_ids.iter().zip(&self.mesh_of_node) {
            let Some(MeshIdx(slot)) = *mesh else {
                continue;
            };
            let slot = slot as usize;
            if retained.len() <= slot {
                retained.resize(slot + 1, false);
            }
            retained[slot] = keep(id);
        }
        let retained = &*retained;
        let retain = |drawable: &crate::DrawableInfo| match drawable {
            crate::DrawableInfo::Part { mesh_id, .. } => {
                retained.get(*mesh_id as usize).copied().unwrap_or(false)
            }
            crate::DrawableInfo::Composite { .. } => true,
        };
        render_list.root_drawables.retain(retain);
        for children in &mut render_list.composite_children {
            children.retain(retain);
        }
    }

    /// `None` for an unmapped part, or for an index past the textures this
    /// cache holds.
    fn tex_idx(&self, albedo: Option<catchlight_core::TextureIdx>) -> Option<TexIdx> {
        albedo
            .filter(|albedo| albedo.0 < self.texture_count)
            .map(|albedo| TexIdx(albedo.0))
    }

    /// Walks every node; for debug assertions only.
    fn agrees_with(&self, puppet: &Puppet) -> bool {
        self.node_ids.len() == puppet.len()
            && self
                .node_ids
                .iter()
                .enumerate()
                .all(|(slot, id)| puppet.node_id(NodeIdx(slot as u32)) == Some(id))
    }

    /// Whole-cache: `Model::generation` cannot tell a transform from a tree
    /// change.
    fn rebuild(&mut self, renderer: &mut WgpuRenderer, model: &Model) -> RendererResult<()> {
        let _span = tracing::trace_span!("render_cache::rebuild").entered();

        let wanted: Vec<(&TexId, &ModelTexture)> = model
            .texture_ids()
            .iter()
            .filter_map(|id| Some((id, model.texture(id)?)))
            .collect();
        debug_assert_eq!(
            wanted.len(),
            model.texture_ids().len(),
            "a model's texture order named a texture it does not hold",
        );

        // Matched by Id, never by slot: deleting a texture shifts later slots.
        // Read, not taken, so error paths leave `self.uploaded` accurate.
        let previous: std::collections::HashMap<&TexId, &UploadedTexture> =
            self.uploaded.iter().map(|up| (&up.id, up)).collect();
        let mut kept: Vec<Option<UploadedTexture>> = Vec::with_capacity(wanted.len());
        let mut moved: Vec<(u32, u32)> = Vec::new();
        let mut fresh_slots: Vec<usize> = Vec::new();
        let mut fresh: Vec<catchlight_core::EncodedTexture> = Vec::new();
        // One total over kept and fresh textures, so a model cannot pass the
        // limit one edit at a time.
        let mut limits = LoadLimits::default();
        limits.decoded_texture_bytes = self.options.max_decoded_texture_bytes;
        let mut budget = LoadBudget::new(limits);
        for (slot, (id, texture)) in wanted.iter().enumerate() {
            match previous.get(*id) {
                Some(up) if up.holds(texture) => {
                    let [width, height] = up.source_size;
                    budget.check_texture_dimensions(width, height)?;
                    moved.push((up.slot, slot as u32));
                    kept.push(Some(UploadedTexture {
                        slot: slot as u32,
                        ..(*up).clone()
                    }));
                }
                _ => {
                    kept.push(None);
                    fresh_slots.push(slot);
                    fresh.push((*texture).into());
                }
            }
        }
        drop(previous);

        // Everything fallible runs before anything is released, so a failed
        // rebuild leaves the previous build resident.
        let prepped = catchlight_core::prepare_textures(
            fresh.as_slice(),
            self.options.texture_halvings,
            renderer.max_texture_dimension(),
            &mut budget,
        )?;
        for prep in &prepped {
            renderer.validate_texture_upload(&prep.texture)?;
        }

        renderer.retain_textures(&moved);
        renderer.upload_textures(
            fresh_slots
                .iter()
                .zip(&prepped)
                .map(|(&slot, prep)| (slot as u32, &prep.texture)),
        )?;
        self.texture_count = wanted.len() as u32;
        self.stats.rebuilds += 1;
        self.stats.texture_uploads += fresh_slots.len() as u64;
        self.stats.textures_kept += (wanted.len() - fresh_slots.len()) as u64;

        for (&slot, prep) in fresh_slots.iter().zip(prepped.iter()) {
            kept[slot] = Some(UploadedTexture {
                id: wanted[slot].0.clone(),
                slot: slot as u32,
                payload: wanted[slot].1.data.clone(),
                uv_crop: prep.uv_crop,
                source_size: prep.source_size,
            });
        }
        let uv_crops: Vec<Option<UvCrop>> = kept
            .iter()
            .map(|up| up.as_ref().and_then(|up| up.uv_crop))
            .collect();
        self.uploaded = kept.into_iter().flatten().collect();
        debug_assert_eq!(self.uploaded.len(), wanted.len());

        let tex_slot_of_id: std::collections::HashMap<_, u32> = wanted
            .iter()
            .enumerate()
            .map(|(slot, (id, _))| ((*id).clone(), slot as u32))
            .collect();

        self.node_ids = model.nodes_in_order();
        self.mesh_of_node = vec![None; self.node_ids.len()];
        let previous: std::collections::HashMap<_, _> =
            self.meshes.iter().map(|up| (&up.id, up)).collect();
        let mut meshes = Vec::new();
        let mut kept_meshes = Vec::new();
        let mut fresh_meshes = Vec::new();
        for (slot, id) in self.node_ids.iter().enumerate() {
            let Some(node) = model.node(id) else {
                continue;
            };
            let ModelNodeKind::Part(part) = &node.kind else {
                continue;
            };
            let authored = part.mesh();
            if authored.verts.is_empty() {
                continue;
            }
            let uv_crop = part
                .albedo()
                .and_then(|tex| tex_slot_of_id.get(tex))
                .and_then(|&s| uv_crops.get(s as usize).copied())
                .flatten();
            let next_mesh = meshes.len() as u32;
            // Serialized structure pushes have fresh storage for unchanged meshes.
            if let Some(up) = previous.get(id).filter(|up| {
                up.uv_crop == uv_crop
                    && renderer.has_mesh(up.slot)
                    && (std::ptr::eq::<ClmMesh>(&*up.mesh, authored) || *up.mesh == *authored)
            }) {
                kept_meshes.push((up.slot, next_mesh));
            } else {
                fresh_meshes.push((slot, next_mesh));
            }
            self.mesh_of_node[slot] = Some(MeshIdx(next_mesh));
            meshes.push(UploadedMesh {
                id: id.clone(),
                slot: next_mesh,
                mesh: part.shared_mesh(),
                uv_crop,
            });
        }
        renderer.retain_meshes(&kept_meshes, meshes.len());
        self.stats.meshes_kept += kept_meshes.len() as u64;
        for (node_slot, mesh_slot) in fresh_meshes {
            let up = &meshes[mesh_slot as usize];
            let authored = &up.mesh;
            let uv_crop = up.uv_crop;
            let mesh = build_mesh(authored, uv_crop);
            if mesh.uvs.is_empty() {
                tracing::warn!(
                    node = %up.id,
                    "part mesh has no uvs; substituting (0,0) for every vertex",
                );
            }
            // One bad mesh must not blank every other part.
            match renderer.upload_mesh(mesh_slot, &mesh) {
                Ok(()) => self.stats.mesh_uploads += 1,
                Err(error) => {
                    self.mesh_of_node[node_slot] = None;
                    self.stats.meshes_skipped += 1;
                    tracing::warn!(node = %up.id, "skipping mesh that failed upload: {error}");
                }
            }
        }
        meshes.retain(|up| renderer.has_mesh(up.slot));
        self.meshes = meshes;

        self.generation = model.generation();
        Ok(())
    }
}

/// Allocates; a caller drawing every frame wants [`RenderCache::collect_into`].
#[must_use]
pub fn collect(cache: &RenderCache, puppet: &Puppet) -> RenderList {
    let mut render_list = RenderList::default();
    Collector::default().collect_into(&CacheSource { cache, puppet }, &mut render_list);
    render_list
}

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

    fn accumulated_z(&self, idx: NodeIdx) -> f32 {
        self.puppet.accumulated_z(idx)
    }

    fn structure_revision(&self) -> u64 {
        self.puppet.baked_generation()
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
