//! The authored model: the character as a person made it.

pub mod addon;
mod binding;
mod check;
mod correspondence;
mod equality;
mod error;
mod eval;
mod extensions;
mod file;
mod node;
mod param;
mod rename;
mod response;
mod slots;
#[cfg(test)]
mod tests;
mod texture;
mod tree;
mod validate;

pub use addon::{InstallError, Installed, Required, Requirement, RequirementOwner, Requirements};
pub use binding::{
    BindingKey, BindingParams, BindingTarget, DenseGrid, ModelBinding, ModelBindingValues,
    ScalarTarget, param_range_is_valid,
};
pub use check::{CheckWarning, ExtensionWarning};
pub use correspondence::{DeformMapping, MAX_MAPPING_ENTRIES, MeshMappingError, VertexWeight};
pub use error::ModelError;
pub use eval::Pose;
pub use extensions::ExtensionValue;
pub use file::{ClmLoadError, LoadError, ReadOptions, Shape};
pub use node::{
    ModelComposite, ModelMask, ModelMesh, ModelMeshGroup, ModelNode, ModelNodeKind, ModelPart,
    ModelSpine, NodeProperties,
};
pub(crate) use node::{albedo_of, is_mask_source};
pub use param::ModelParam;
pub use slots::{DEFAULT_SLOT_WEIGHT, ModelWeld, ModelWeldPair, Slot, SlotPair};
pub use texture::ModelTexture;
pub use validate::SpineJointError;

use node::validate_mesh;
use response::ResponseFault;

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::components::{BlendMode, MaskMode};
use crate::extension::{Extensions, IdRef, RawExtension};
use crate::formats::clm::{
    self as clm, ClmBindingValues, ClmMesh, ClmTransform, TextureAlpha, TextureEncoding,
};
use crate::id::{
    AnyId, ExtensionKey, HexSource, IdError, Name, NodeId, NodeIdKind, ParamId, SlotId, TexId,
};

/// 32-bit Ids collide by birthday around 2^16 siblings; the cap only fires on
/// a wedged [`HexSource`].
const ID_MINT_ATTEMPTS: usize = 64;

#[derive(Debug, Clone)]
pub struct Model {
    identity: u64,
    generation: u64,
    welds: Vec<ModelWeld>,
    nodes: HashMap<NodeId, ModelNode>,
    /// Avoid scanning a model without responses for response ownership on every edit.
    response_count: usize,
    response_index: OnceLock<Arc<response::ResponseIndex>>,
    /// Nodes whose parent this model does not carry: exactly one (parentless) in
    /// a complete model, one or more in a fragment.
    roots: Vec<NodeId>,
    params: HashMap<ParamId, ModelParam>,
    param_order: Vec<ParamId>,
    textures: HashMap<TexId, ModelTexture>,
    texture_order: Vec<TexId>,
    bindings: Vec<ModelBinding>,
    extensions: Extensions,
    /// Unrecognised file sections, written back unchanged on save.
    unknown_sections: Vec<clm::ClmUnknownSection>,
    /// Derived; dropped by [`Model::bump`].
    binding_index: OnceLock<HashMap<BindingKey, usize>>,
    /// Derived; dropped by [`Model::bump`].
    bake: crate::puppet::BakeMemo,
}

const DEFAULT_ROOT_ID: &str = "root";

/// Starts at 1 so 0 is free to mean "no model".
static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);

fn next_identity() -> u64 {
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

/// Process-wide because undo restores older models: a per-model counter would
/// reissue a generation a derived object already baked against.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

impl Model {
    pub fn new() -> Self {
        let root = NodeId::from_generated(DEFAULT_ROOT_ID.to_string());
        let mut nodes = HashMap::new();
        nodes.insert(root.clone(), ModelNode::new("Root", ModelNodeKind::Group));
        Self {
            identity: next_identity(),
            generation: 0,
            welds: Vec::new(),
            nodes,
            response_count: 0,
            response_index: OnceLock::new(),
            roots: vec![root],
            params: HashMap::new(),
            param_order: Vec::new(),
            textures: HashMap::new(),
            texture_order: Vec::new(),
            bindings: Vec::new(),
            extensions: Extensions::default(),
            unknown_sections: Vec::new(),
            binding_index: OnceLock::new(),
            bake: Default::default(),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Compared beside [`Self::generation`], which every loader starts at 0.
    /// A clone keeps it: an undo snapshot is the same model.
    pub fn identity(&self) -> u64 {
        self.identity
    }

    /// Called only on success, so a rejected edit leaves derived objects alone.
    fn bump(&mut self) {
        self.generation = NEXT_GENERATION.fetch_add(1, Ordering::Relaxed);
        self.binding_index.take();
        self.bake.clear();
    }

    pub(crate) fn bake_memo(&self) -> &crate::puppet::BakeMemo {
        &self.bake
    }

    pub fn root(&self) -> Option<&NodeId> {
        match self.roots.as_slice() {
            [only] if self.nodes.get(only).is_some_and(|n| n.parent.is_none()) => Some(only),
            _ => None,
        }
    }

    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    pub fn is_fragment(&self) -> bool {
        self.root().is_none()
    }

    pub fn node(&self, id: &NodeId) -> Option<&ModelNode> {
        self.nodes.get(id)
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn node_ids(&self) -> impl Iterator<Item = &NodeId> {
        self.nodes.keys()
    }

    pub fn estimated_size_bytes(&self) -> usize {
        let mut bytes = size_of::<Self>()
            .saturating_add(
                self.nodes
                    .len()
                    .saturating_mul(size_of::<ModelNode>() + size_of::<NodeId>()),
            )
            .saturating_add(
                self.params
                    .len()
                    .saturating_mul(size_of::<ModelParam>() + size_of::<ParamId>()),
            )
            .saturating_add(
                self.textures
                    .len()
                    .saturating_mul(size_of::<ModelTexture>() + size_of::<TexId>()),
            )
            .saturating_add(
                self.param_order
                    .capacity()
                    .saturating_mul(size_of::<ParamId>()),
            )
            .saturating_add(
                self.texture_order
                    .capacity()
                    .saturating_mul(size_of::<TexId>()),
            )
            .saturating_add(self.welds.capacity().saturating_mul(size_of::<ModelWeld>()))
            .saturating_add(
                self.bindings
                    .capacity()
                    .saturating_mul(size_of::<ModelBinding>()),
            );
        for weld in &self.welds {
            bytes = bytes
                .saturating_add(weld.a.as_str().len())
                .saturating_add(weld.b.as_str().len())
                .saturating_add(weld.pairs.capacity().saturating_mul(size_of::<SlotPair>()));
            for pair in weld.pairs.iter() {
                bytes = bytes
                    .saturating_add(pair.a.as_str().len())
                    .saturating_add(pair.b.as_str().len());
            }
        }
        for (id, node) in &self.nodes {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(node.name.as_str().len())
                .saturating_add(node.children.capacity().saturating_mul(size_of::<NodeId>()));
            if let Some(mesh) = node.mesh() {
                bytes = bytes.saturating_add(mesh_size(mesh));
            }
            if let Some(masks) = node.masks() {
                bytes = bytes.saturating_add(masks.len().saturating_mul(size_of::<ModelMask>()));
            }
            if let ModelNodeKind::Part(part) = &node.kind {
                bytes =
                    bytes.saturating_add(part.slots.capacity().saturating_mul(size_of::<Slot>()));
                for slot in part.slots.iter() {
                    bytes = bytes.saturating_add(slot.id.as_str().len());
                }
            }
        }
        for (id, param) in &self.params {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(param.name.as_str().len());
        }
        for binding in &self.bindings {
            bytes = bytes.saturating_add(binding_values_size(&binding.values));
            bytes = bytes.saturating_add(binding.key_positions.capacity() * size_of::<Vec<f32>>());
            for axis in &binding.key_positions {
                bytes = bytes.saturating_add(axis.capacity() * size_of::<f32>());
            }
        }
        for (id, texture) in &self.textures {
            bytes = bytes
                .saturating_add(id.as_str().len())
                .saturating_add(texture.data.len());
        }
        bytes
    }

    pub fn node_mesh(&self, id: &NodeId) -> Option<&ClmMesh> {
        self.node(id)?.mesh()
    }

    fn mint<T, H: HexSource>(
        &self,
        mut generate: impl FnMut(&mut H) -> T,
        taken: impl Fn(&Self, &T) -> bool,
        hex: &mut H,
    ) -> Result<T, ModelError> {
        for _ in 0..ID_MINT_ATTEMPTS {
            let id = generate(hex);
            if !taken(self, &id) {
                return Ok(id);
            }
        }
        Err(ModelError::IdExhausted)
    }
}

/// Pointer-equal leaves are equal without being read.
fn same<T: PartialEq + ?Sized>(a: &Arc<T>, b: &Arc<T>) -> bool {
    Arc::ptr_eq(a, b) || a == b
}

fn mesh_size(mesh: &ClmMesh) -> usize {
    let indices = match &mesh.indices {
        clm::ClmIndices::U16(indices) => indices.capacity().saturating_mul(size_of::<u16>()),
        clm::ClmIndices::U32(indices) => indices.capacity().saturating_mul(size_of::<u32>()),
    };
    mesh.verts
        .capacity()
        .saturating_add(mesh.uvs.capacity())
        .saturating_mul(size_of::<f32>())
        .saturating_add(indices)
}

fn binding_values_size(values: &ClmBindingValues) -> usize {
    use ClmBindingValues as V;
    match values {
        V::Deform(cells) => cells
            .cells
            .capacity()
            .saturating_mul(size_of::<clm::ClmCell<Vec<f32>>>())
            .saturating_add(cells.cells.iter().fold(0usize, |bytes, cell| {
                bytes.saturating_add(cell.value.capacity().saturating_mul(size_of::<f32>()))
            })),
        V::ZOrder(cells)
        | V::TransformTX(cells)
        | V::TransformTY(cells)
        | V::TransformTZ(cells)
        | V::TransformSX(cells)
        | V::TransformSY(cells)
        | V::TransformRX(cells)
        | V::TransformRY(cells)
        | V::TransformRZ(cells)
        | V::Opacity(cells)
        | V::TintR(cells)
        | V::TintG(cells)
        | V::TintB(cells)
        | V::ScreenTintR(cells)
        | V::ScreenTintG(cells)
        | V::ScreenTintB(cells) => cells
            .cells
            .capacity()
            .saturating_mul(size_of::<clm::ClmCell<f32>>()),
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod bench {
    use super::tests_support::dense_model;
    use std::time::Instant;

    /// Undo-snapshot cost; run with
    /// `cargo test -p catchlight-core --lib -- --ignored --nocapture bench`.
    #[test]
    #[ignore = "a timing measurement, not a pass/fail check"]
    fn clone_and_edit_a_five_hundred_node_model() {
        let (model, key) = dense_model(500, 64, 3);
        let bytes = model.estimated_size_bytes();

        let runs = 200;
        let start = Instant::now();
        let mut sink = 0usize;
        for _ in 0..runs {
            let snapshot = model.clone();
            sink += snapshot.node_count();
            std::hint::black_box(snapshot);
        }
        let clone_ns = start.elapsed().as_nanos() / runs;

        let start = Instant::now();
        for _ in 0..runs {
            let mut snapshot = model.clone();
            snapshot
                .set_deform_vertices(&key, [1, 1], vec![1.0; 128])
                .unwrap();
            std::hint::black_box(snapshot);
        }
        let edit_ns = start.elapsed().as_nanos() / runs;

        println!(
            "500 nodes / {} bindings / {:.1} MiB: clone {:.1} us, clone + one deform cell {:.1} us ({sink})",
            model.bindings().count(),
            bytes as f64 / (1024.0 * 1024.0),
            clone_ns as f64 / 1000.0,
            edit_ns as f64 / 1000.0,
        );
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;
    use crate::formats::clm::ClmIndices;
    use crate::id::SeededHex;

    pub(super) fn dense_model(nodes: usize, verts: usize, keys: usize) -> (Model, BindingKey) {
        let positions: Vec<f32> = (0..keys).map(|i| i as f32 / (keys - 1) as f32).collect();
        let mut hex = SeededHex::new(11);
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let mut param = |name: &str, model: &mut Model| {
            model
                .add_param(
                    ModelParam {
                        name: Name::truncated(name),
                        min: -1.0,
                        max: 1.0,
                        default: 0.0,
                    },
                    &mut hex,
                )
                .unwrap()
        };
        let (param_x, param_y) = (param("sweep.x", &mut model), param("sweep.y", &mut model));
        let mesh = ClmMesh {
            verts: (0..verts * 2).map(|i| i as f32).collect(),
            uvs: vec![0.0; verts * 2],
            indices: ClmIndices::U16(Vec::new()),
            origin: [0.0, 0.0],
        };
        let mut last = None;
        for i in 0..nodes {
            let node = model
                .add_node(
                    &root,
                    ModelNode::new(
                        format!("part-{i}"),
                        ModelNodeKind::Part(ModelPart::new(mesh.clone())),
                    ),
                    &mut hex,
                )
                .unwrap();
            let key = BindingKey::pair(
                param_x.clone(),
                param_y.clone(),
                node,
                BindingTarget::Deform,
            );
            model
                .add_binding_with_positions(&key, vec![positions.clone(), positions.clone()])
                .unwrap();
            for y in 0..keys as u32 {
                for x in 0..keys as u32 {
                    model
                        .set_deform_vertices(&key, [x, y], vec![x as f32; verts * 2])
                        .unwrap();
                }
            }
            last = Some(key);
        }
        let key = last.expect("at least one node");
        (model, key)
    }
}
