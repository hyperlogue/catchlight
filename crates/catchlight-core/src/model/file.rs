//! The file edge: a [`Model`] to and from `.clm` bytes.
//!
//! There is no translation here beyond framing — [`crate::formats::clm`] holds
//! the same value types the Model does, and both sides key everything by Id —
//! so this module orders, checks and copies, and nothing else. Its two
//! obligations:
//!
//! - **Writing is total and deterministic.** A Model's tree is always valid
//!   and its cross-references are always live, so the only thing
//!   [`Model::to_clm_file`] can refuse is a deform cell whose length its
//!   node's mesh cannot take. (It also refuses a weld whose end is no longer
//!   a part, which only [`Model::update_node`] swapping a welded node's kind
//!   can produce, so that a file this writes is always one this reader takes.)
//!   The orders it
//!   writes — nodes pre-order from the root, params, textures and bindings in
//!   the Model's own order — are the Model's, so the same Model always writes
//!   the same bytes.
//! - **Reading trusts nothing.** Every Id in the file is resolved against what
//!   the file itself declares, and a failure names the field and the Id that
//!   dangled ([`ClmLoadError`]). What the reader accepts is exactly what the
//!   Model's own invariants allow, so a loaded Model needs no repair pass.
//!   Beyond the Ids, that is eight checks, each the file-shaped half of a rule
//!   an edit already obeys: a param's range has to be finite and increasing
//!   ([`crate::param_range_is_valid`]); a mask source has to be a kind the
//!   renderer draws ([`Model::mask_add`]); a deform binding has to sit on a
//!   node that carries a mesh, and a colour binding may not sit on a mesh
//!   group ([`Model::add_binding`]); a mesh has to be `[x, y]` pairs with uvs
//!   to match and every index in range ([`Model::set_node_mesh`]); a deform
//!   cell has to be sized to its node's mesh; and every texture the file
//!   carries has to be one some part's albedo names
//!   ([`ClmLoadError::UnusedTexture`], the file-shaped half of
//!   [`Model::add_texture`] taking the part the texture is for); and an
//!   animation lane's keyframes have to be in frame order
//!   ([`Model::set_animations`], because playback reads a lane by binary
//!   search).
//! - **A file is read as one shape or the other, never guessed.**
//!   [`Model::from_clm_bytes`] reads a *complete model*: one node with no
//!   parent, and nothing dangling. [`Model::from_clm_bytes_fragment`] reads an
//!   *addon fragment*: every node names a parent, the ones the file does not
//!   carry are its roots, and every other reference into a base — mask
//!   source, physics target, binding param, weld end, animation lane — may
//!   dangle for [`Model::install`] to resolve. A part's albedo is the one
//!   that may not: an addon carries the textures its own parts draw, so that
//!   reference resolves inside the fragment in both shapes. The two shapes
//!   are disjoint on
//!   the wire (a complete model always has a parentless node and a fragment
//!   never does), so a caller that does not know which it holds decodes once
//!   and tries both readers against the same [`ClmFile`]; nothing else about
//!   the container or the document changes between them.
//! - **A structure carries no bytes, and reading one is the same reading.**
//!   [`Model::to_structure_bytes`] writes the `Structure` document and a
//!   manifest of the model's textures, no payloads;
//!   [`Model::replace_structure`] reads it back against payloads a caller
//!   supplies by Id. Both sides go through the same document builder as a
//!   `.clm`, so a structure is held to every rule a file is — including that
//!   every texture the manifest lists is one a part draws — and a payload the
//!   caller cannot supply is [`ClmLoadError::MissingTexture`] rather than a
//!   model with a hole in it. What the manifest says about a texture wins
//!   over what the lookup hands back, so two replicas given one structure
//!   hold one model.
//! - **Replacing keeps the model and moves the clock.**
//!   [`Model::replace_structure`] and [`Model::replace_from`] keep
//!   [`Model::identity`] and bump [`Model::generation`]. That is what makes a
//!   replica cheap: a puppet and a render cache built on the model read "the
//!   same model, new state", so the puppet rebakes carrying its pose and its
//!   drivers, and the cache rebuilds keeping every texture whose payload
//!   `Arc` came back unchanged. The new state is built whole before either
//!   field moves, so a refused structure leaves the model exactly as it was.
//!
use std::collections::{HashMap, HashSet};

use crate::formats::clm::{
    self as clm, ClmBinding, ClmComposite, ClmDocument, ClmFile, ClmMask, ClmMeshGroup, ClmNode,
    ClmNodeKind, ClmParam, ClmPart, ClmSimplePhysics, ClmSlot, ClmSlotPair, ClmTexture,
    ClmTextureRef, ClmWeld,
};
use crate::id::SlotId;
use crate::{charge_clm_document, charge_clm_structure, charge_texture_payloads, LoadBudget};

use super::*;

/// Why a `.clm` could not be turned into a [`Model`]. Every variant names the
/// Id that broke and the field that named it — a hostile or truncated file
/// reports itself, it never panics and never loads half a model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClmLoadError {
    #[error("two {kind}s share the id {id:?}")]
    DuplicateId { kind: &'static str, id: String },
    #[error("the file has no root node")]
    NoRoot,
    #[error("node {node:?} has no parent, but {root:?} is already the root")]
    MultipleRoots { root: String, node: String },
    #[error("node {node:?} names parent {parent:?}, which no node has")]
    DanglingParent { node: String, parent: String },
    #[error("node {node:?} names parent {parent:?}, which the file writes after it")]
    NotTopological { node: String, parent: String },
    #[error(
        "node {node:?} has no parent, so this file is a complete model; a fragment's roots name          the base node they hang from"
    )]
    FragmentHasNoParent { node: String },
    #[error("node {node:?} names {field} {id:?}, which no node has")]
    DanglingNode {
        node: String,
        field: &'static str,
        id: String,
    },
    #[error("node {node:?} names texture {id:?}, which the file does not carry")]
    DanglingTexture { node: String, id: String },
    #[error(
        "the file carries texture {id:?}, which no part's albedo names; every texture a model \
         holds is drawn by one of its parts, and an addon brings its own"
    )]
    UnusedTexture { id: String },
    #[error("a binding names node {id:?}, which no node has")]
    DanglingBindingNode { id: String },
    #[error("{owner} names param {id:?}, which the file does not carry")]
    DanglingParam { owner: String, id: String },
    #[error(
        "the binding on node {node:?} names {got} params; a binding names one or two distinct ones"
    )]
    BindingParamCount { node: String, got: usize },
    #[error("part {node:?} carries two slots named {slot:?}")]
    DuplicateSlot { node: String, slot: String },
    #[error("part {node:?} fills slot {slot:?} with vertex {vertex}, past the mesh's {vertices}")]
    SlotOutOfRange {
        node: String,
        slot: String,
        vertex: u32,
        vertices: usize,
    },
    #[error(
        "the {target} binding on node {node:?} targets a mesh group, which is never drawn and \
         has no colour to fold it into"
    )]
    ColorOnMeshGroup { node: String, target: &'static str },
    #[error(
        "the deform binding on node {node:?} targets a {kind}, which carries no mesh to deform"
    )]
    DeformOnUnmeshed { node: String, kind: &'static str },
    #[error(
        "the deform binding on node {node:?} holds {got} offsets at cell {cell:?}, but the node's \
         mesh takes {expected}"
    )]
    DeformCellShape {
        node: String,
        cell: [u32; 2],
        got: usize,
        expected: usize,
    },
    #[error("a weld pairs slot {slot:?} on node {node:?}, which carries no such slot")]
    WeldUnknownSlot { node: String, slot: String },
    #[error("a weld names node {node:?}, which the file does not carry")]
    DanglingWeldEnd { node: String },
    #[error("a weld names node {node:?} on both ends; a weld joins two different parts")]
    WeldSelfPaired { node: String },
    #[error("a weld names node {node:?}, which is a {kind}; a weld joins two parts")]
    WeldEndNotAPart { node: String, kind: &'static str },
    #[error("nodes {a:?} and {b:?} are welded twice; one weld records a pair of parts")]
    DuplicateWeld { a: String, b: String },
    #[error("param {param:?} has a range that is not finite and increasing")]
    ParamRange { param: String },
    // The field is `mask_source` rather than `source` because thiserror reads
    // a field of that name as the error's cause.
    #[error(
        "node {node:?} names mask source {mask_source:?}, which is a {kind}; only a part or a \
         composite is drawn, and only what is drawn can be a mask"
    )]
    MaskSourceKind {
        node: String,
        mask_source: String,
        kind: &'static str,
    },
    #[error("the mesh on node {node:?} is malformed: {reason}")]
    MalformedMesh { node: String, reason: &'static str },
    #[error("the weld between {a:?} and {b:?} pairs slot {slot:?} twice")]
    WeldSlotPairedTwice { a: String, b: String, slot: String },
    #[error(
        "the weld between {a:?} and {b:?} weights slot {slot:?} outside 0..=1; a weight is a \
         share of a meeting point"
    )]
    WeldWeightOutOfRange { a: String, b: String, slot: String },
    #[error(
        "a lane of animation {animation} on param {param:?} has keyframes out of frame order; a \
         player reads a lane by binary search"
    )]
    UnsortedLane { animation: usize, param: String },
    #[error(
        "the structure names texture {id:?}, which the caller could not supply; fetch it before \
         applying the structure"
    )]
    MissingTexture { id: String },
}

/// The slot Ids one part carries — what a weld's pairs are checked against.
type SlotTable = HashSet<SlotId>;

/// Which of the two shapes a `.clm` is being read as. The reader never
/// guesses: the caller says, and the file is refused if it is the other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A complete model. Exactly one node has no parent, and every Id the file
    /// names is one the file also carries.
    Base,
    /// An addon fragment. Every node names a parent; the ones the file does
    /// not carry are its roots, and every other reference into the base model
    /// is left dangling for [`Model::install`] to resolve.
    Fragment,
}

impl Shape {
    /// Whether a reference this shape allows to leave the file may dangle.
    /// A binding's *node* never may, in either shape: an addon binds its own
    /// nodes — see [`crate::model::addon`].
    fn allows_dangling(self) -> bool {
        matches!(self, Self::Fragment)
    }
}

impl Model {
    /// Snapshot the model's `Structure` document — everything a `.clm`
    /// carries except the texture payloads. Total for any Model whose deform
    /// cells are sized to the meshes they sit on.
    pub fn to_clm_document(&self) -> Result<ClmDocument, ModelError> {
        let order = self.nodes_in_order();
        let mut nodes = Vec::with_capacity(order.len());
        for id in &order {
            let node = self.node(id).ok_or(ModelError::UnknownNode)?;
            nodes.push(ClmNode {
                id: id.clone(),
                parent: node.parent().cloned(),
                name: node.name.to_string(),
                enabled: node.enabled,
                z_order: node.z_order,
                transform: node.transform,
                lock_to_root: node.lock_to_root,
                kind: clm_kind(&node.kind),
            });
        }

        let fragment = self.is_fragment();
        let mut welds = Vec::with_capacity(self.welds.len());
        for weld in &self.welds {
            // Unreachable through the Model's own methods, which refuse a weld
            // whose end is not a part; reachable through `update_node`
            // swapping a welded node's kind, and the reader refuses such a
            // file. A fragment's weld into a base part has one end this model
            // cannot see, and only the end it can see is checked.
            for end in [weld.a(), weld.b()] {
                match self.node(end).map(|n| &n.kind) {
                    Some(ModelNodeKind::Part(_)) => {}
                    Some(_) => return Err(ModelError::WeldEndNotAPart),
                    None if fragment => {}
                    None => return Err(ModelError::UnknownNode),
                }
            }
            welds.push(ClmWeld {
                a: weld.a().clone(),
                b: weld.b().clone(),
                pairs: weld
                    .pairs()
                    .iter()
                    .map(|pair| ClmSlotPair {
                        a: pair.a.clone(),
                        b: pair.b.clone(),
                        weight: pair.weight,
                    })
                    .collect(),
            });
        }

        let mut params = Vec::with_capacity(self.param_order.len());
        for id in &self.param_order {
            let p = self.param(id).ok_or(ModelError::UnknownParam)?;
            params.push(ClmParam {
                id: id.clone(),
                name: p.name.to_string(),
                min: p.min,
                max: p.max,
                default: p.default,
                key_positions: p.key_positions.clone(),
            });
        }

        let mut bindings = Vec::with_capacity(self.bindings.len());
        for b in &self.bindings {
            // Caught here as well as on the way in, so a refit that sized a
            // deform wrong is reported when the file is written rather than
            // when someone tries to open it.
            check_deform_cells(&b.key.node, self.deform_len(&b.key.node), &b.values)?;
            bindings.push(ClmBinding {
                params: b.key.params.iter().cloned().collect(),
                node: b.key.node.clone(),
                interpolate_mode: b.interpolate_mode(),
                values: b.values.to_clm(),
            });
        }

        Ok(ClmDocument {
            physics: self.physics,
            nodes,
            params,
            bindings,
            welds,
            animations: self.animations.clone(),
        })
    }

    /// Snapshot the model into a `.clm` document plus its texture table. The
    /// payloads are copied out of their `Arc`s here, which is what makes this
    /// the *file* path and [`Self::to_structure_bytes`] the push path.
    pub fn to_clm_file(&self) -> Result<ClmFile, ModelError> {
        let doc = self.to_clm_document()?;
        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClmTexture {
                id: id.clone(),
                encoding: t.encoding,
                alpha: t.alpha,
                data: t.data.to_vec(),
            });
        }
        Ok(ClmFile { doc, textures })
    }

    /// [`Self::to_clm_file`] then encode.
    pub fn to_clm_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let file = self.to_clm_file()?;
        Ok(clm::encode(&file.doc, &file.textures)?)
    }

    /// The document alone, as a **structure-only** container: the same
    /// `Structure` section [`Self::to_clm_bytes`] would write, beside a
    /// manifest naming the model's textures in order, and no payload bytes at
    /// all.
    ///
    /// This is what a server pushes a replica after every edit, so it never
    /// touches a texture: writing it costs the document and nothing else, and
    /// [`Self::replace_structure`] reads it back against payloads the replica
    /// already holds.
    pub fn to_structure_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let doc = self.to_clm_document()?;
        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClmTextureRef {
                id: id.clone(),
                encoding: t.encoding,
                alpha: t.alpha,
            });
        }
        Ok(clm::encode_structure(&doc, &textures)?)
    }

    /// Read a **complete model**: one root with no parent, nothing dangling.
    pub fn from_clm_file(file: &ClmFile) -> Result<Model, ModelError> {
        Self::from_clm_file_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clm_file_with_budget(
        file: &ClmFile,
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        Self::read(file, budget, Shape::Base)
    }

    /// Read an **addon fragment**: a forest whose roots name absent parents,
    /// with every other reference into the base model left to dangle. See
    /// [`crate::model::addon`] for what install then does with it.
    pub fn from_clm_file_fragment(file: &ClmFile) -> Result<Model, ModelError> {
        Self::from_clm_file_fragment_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clm_file_fragment_with_budget(
        file: &ClmFile,
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        Self::read(file, budget, Shape::Fragment)
    }

    fn read(file: &ClmFile, budget: &mut LoadBudget, shape: Shape) -> Result<Model, ModelError> {
        charge_clm_structure(file, budget)?;

        let mut textures = HashMap::with_capacity(file.textures.len());
        let mut texture_order = Vec::with_capacity(file.textures.len());
        for t in &file.textures {
            if textures
                .insert(
                    t.id.clone(),
                    ModelTexture {
                        encoding: t.encoding,
                        alpha: t.alpha,
                        data: t.data.as_slice().into(),
                    },
                )
                .is_some()
            {
                return Err(duplicate("texture", &t.id));
            }
            texture_order.push(t.id.clone());
        }

        Self::build(&file.doc, textures, texture_order, shape)
    }

    /// Every check the reader runs, over a document and a texture table that
    /// is already in the Model's own form. The one path both a `.clm` and a
    /// structure push come through, so neither can drift into accepting what
    /// the other refuses. Charges nothing: its callers charge what they read.
    fn build(
        doc: &ClmDocument,
        textures: HashMap<TexId, ModelTexture>,
        texture_order: Vec<TexId>,
        shape: Shape,
    ) -> Result<Model, ModelError> {
        let mut params = HashMap::with_capacity(doc.params.len());
        let mut param_order = Vec::with_capacity(doc.params.len());
        for p in &doc.params {
            if !param_range_is_valid(p.min, p.max) {
                return Err(ClmLoadError::ParamRange {
                    param: p.id.to_string(),
                }
                .into());
            }
            if params
                .insert(
                    p.id.clone(),
                    ModelParam {
                        name: Name::truncated(&p.name),
                        min: p.min,
                        max: p.max,
                        default: p.default,
                        key_positions: p.key_positions.clone(),
                    },
                )
                .is_some()
            {
                return Err(duplicate("param", &p.id));
            }
            param_order.push(p.id.clone());
        }

        // Pre-scan the Ids so a parent that names nothing reads differently
        // from one the file writes too late, and so a mask may name a node the
        // file has not reached yet.
        let mut declared: HashSet<&NodeId> = HashSet::with_capacity(doc.nodes.len());
        for n in &doc.nodes {
            if !declared.insert(&n.id) {
                return Err(duplicate("node", &n.id));
            }
        }

        let mut nodes: HashMap<NodeId, ModelNode> = HashMap::with_capacity(doc.nodes.len());
        let mut slots: HashMap<&NodeId, SlotTable> = HashMap::new();
        let mut roots: Vec<NodeId> = Vec::new();
        for cn in &doc.nodes {
            match &cn.parent {
                Some(parent) if declared.contains(parent) => {
                    if !nodes.contains_key(parent) {
                        return Err(ClmLoadError::NotTopological {
                            node: cn.id.to_string(),
                            parent: parent.to_string(),
                        }
                        .into());
                    }
                }
                // A parent the file does not carry: a fragment's root, or a
                // complete model that lost a node.
                Some(parent) => {
                    if !shape.allows_dangling() {
                        return Err(ClmLoadError::DanglingParent {
                            node: cn.id.to_string(),
                            parent: parent.to_string(),
                        }
                        .into());
                    }
                    roots.push(cn.id.clone());
                }
                None => {
                    if shape.allows_dangling() {
                        return Err(ClmLoadError::FragmentHasNoParent {
                            node: cn.id.to_string(),
                        }
                        .into());
                    }
                    if let Some(first) = roots.first() {
                        return Err(ClmLoadError::MultipleRoots {
                            root: first.to_string(),
                            node: cn.id.to_string(),
                        }
                        .into());
                    }
                    roots.push(cn.id.clone());
                }
            }

            let node_slots = match &cn.kind {
                ClmNodeKind::Part(part) => {
                    let read = read_slots(&cn.id, part)?;
                    slots.insert(&cn.id, read.iter().map(|s| s.id().clone()).collect());
                    read
                }
                _ => Vec::new(),
            };

            let mut node = ModelNode::new(&cn.name, ModelNodeKind::Group);
            node.parent = cn.parent.clone();
            node.enabled = cn.enabled;
            node.z_order = cn.z_order;
            node.transform = cn.transform;
            node.lock_to_root = cn.lock_to_root;
            node.kind = model_kind(
                &cn.id, &cn.kind, node_slots, &declared, &textures, &params, shape,
            )?;
            nodes.insert(cn.id.clone(), node);
        }
        if roots.is_empty() && !shape.allows_dangling() {
            return Err(ClmLoadError::NoRoot.into());
        }

        // Every mask source the file itself carries has to be a kind the
        // renderer draws — the reader's half of `Model::mask_add`. Walked in
        // document order so a file with two bad masks always names the first.
        // A source the file does *not* carry is a fragment's requirement, and
        // `Model::install` kind-checks it against the base.
        for cn in &doc.nodes {
            for mask in clm_masks_of(&cn.kind) {
                let Some(source) = nodes.get(&mask.source) else {
                    continue;
                };
                if !is_mask_source(&source.kind) {
                    return Err(ClmLoadError::MaskSourceKind {
                        node: cn.id.to_string(),
                        mask_source: mask.source.to_string(),
                        kind: source.kind.name(),
                    }
                    .into());
                }
            }
        }

        // The other half of the albedo check above: every texture the file
        // carries is drawn by a part the file carries. Both shapes, because
        // an addon brings the textures its own parts draw and nothing else —
        // a spare one in a fragment is as much a mistake as in a model.
        // Walked in the file's texture order, so a file with two spares
        // always names the first.
        let drawn: HashSet<&TexId> = doc
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                ClmNodeKind::Part(p) => p.albedo.as_ref(),
                _ => None,
            })
            .collect();
        if let Some(spare) = texture_order.iter().find(|id| !drawn.contains(id)) {
            return Err(ClmLoadError::UnusedTexture {
                id: spare.to_string(),
            }
            .into());
        }

        // Children in document order, which is the model's sibling order.
        for cn in &doc.nodes {
            if let Some(parent) = &cn.parent {
                if let Some(pn) = nodes.get_mut(parent) {
                    pn.children.push(cn.id.clone());
                }
            }
        }

        let mut bindings = Vec::with_capacity(doc.bindings.len());
        for b in &doc.bindings {
            if !nodes.contains_key(&b.node) {
                return Err(ClmLoadError::DanglingBindingNode {
                    id: b.node.to_string(),
                }
                .into());
            }
            let owner = || format!("the binding on node {}", b.node);
            for p in &b.params {
                if !params.contains_key(p) && !shape.allows_dangling() {
                    return Err(ClmLoadError::DanglingParam {
                        owner: owner(),
                        id: p.to_string(),
                    }
                    .into());
                }
            }
            let target = target_of(&b.values);
            let kind = nodes.get(&b.node).map(|n| &n.kind);
            if let BindingTarget::Scalar(t) = target {
                if t.is_color() && matches!(kind, Some(ModelNodeKind::MeshGroup(_))) {
                    return Err(ClmLoadError::ColorOnMeshGroup {
                        node: b.node.to_string(),
                        target: t.name(),
                    }
                    .into());
                }
            }
            let mesh = nodes.get(&b.node).and_then(ModelNode::mesh);
            // `Model::add_binding` refuses a deform on a node with no mesh to
            // deform; without this the file slipped past `check_deform_cells`,
            // whose expected length for such a node is zero.
            if mesh.is_none() && matches!(target, BindingTarget::Deform) {
                return Err(ClmLoadError::DeformOnUnmeshed {
                    node: b.node.to_string(),
                    kind: kind.map_or("node", ModelNodeKind::name),
                }
                .into());
            }
            let vertices = mesh.map_or(0, |m| m.verts.len());
            check_deform_cells(&b.node, vertices, &b.values)?;

            let key_params = match b.params.as_slice() {
                [x] => BindingParams::One(x.clone()),
                [x, y] if x != y => BindingParams::Two(x.clone(), y.clone()),
                other => {
                    return Err(ClmLoadError::BindingParamCount {
                        node: b.node.to_string(),
                        got: other.len(),
                    }
                    .into())
                }
            };
            bindings.push(ModelBinding {
                key: BindingKey {
                    params: key_params,
                    node: b.node.clone(),
                    target,
                },
                interpolate_mode: b.interpolate_mode,
                values: b.values.clone().into(),
                dense: OnceLock::new(),
            });
        }

        let mut welds = Vec::with_capacity(doc.welds.len());
        let mut welded: HashSet<(&NodeId, &NodeId)> = HashSet::with_capacity(doc.welds.len());
        for w in &doc.welds {
            let key = if w.a < w.b {
                (&w.a, &w.b)
            } else {
                (&w.b, &w.a)
            };
            if !welded.insert(key) {
                return Err(ClmLoadError::DuplicateWeld {
                    a: w.a.to_string(),
                    b: w.b.to_string(),
                }
                .into());
            }
            welds.push(read_weld(w, &slots, &nodes, &declared, shape)?);
        }

        for (i, animation) in doc.animations.iter().enumerate() {
            for lane in &animation.lanes {
                if !params.contains_key(&lane.param) && !shape.allows_dangling() {
                    return Err(ClmLoadError::DanglingParam {
                        owner: format!("a lane of animation {i}"),
                        id: lane.param.to_string(),
                    }
                    .into());
                }
                if lane.keyframes.windows(2).any(|w| w[0].frame > w[1].frame) {
                    return Err(ClmLoadError::UnsortedLane {
                        animation: i,
                        param: lane.param.to_string(),
                    }
                    .into());
                }
            }
        }

        Ok(Model {
            identity: super::next_identity(),
            generation: 0,
            physics: doc.physics,
            welds,
            nodes,
            roots,
            params,
            param_order,
            textures,
            texture_order,
            bindings,
            animations: doc.animations.clone(),
            binding_index: OnceLock::new(),
        })
    }

    /// Read a **complete model** from `.clm` bytes.
    pub fn from_clm_bytes(bytes: &[u8]) -> Result<Model, ModelError> {
        Self::from_clm_bytes_with_budget(bytes, &mut LoadBudget::default())
    }

    pub fn from_clm_bytes_with_budget(
        bytes: &[u8],
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        let file = clm::decode_with_budget(bytes, budget)?;
        Self::from_clm_file_with_budget(&file, budget)
    }

    /// Read an **addon fragment** from `.clm` bytes.
    pub fn from_clm_bytes_fragment(bytes: &[u8]) -> Result<Model, ModelError> {
        Self::from_clm_bytes_fragment_with_budget(bytes, &mut LoadBudget::default())
    }

    pub fn from_clm_bytes_fragment_with_budget(
        bytes: &[u8],
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        let file = clm::decode_with_budget(bytes, budget)?;
        Self::from_clm_file_fragment_with_budget(&file, budget)
    }

    /// Rebuild this model from a **structure-only** container
    /// ([`Self::to_structure_bytes`]), taking every texture payload from
    /// `textures` rather than from the bytes.
    ///
    /// This is the replica's one edit: a server pushes the document after
    /// each change and the client applies it over the payloads it already
    /// holds, so a per-commit sync costs no texture transfer, no decode and
    /// no re-upload. The lookup supplies the *bytes* and nothing else — a
    /// texture's Id, its place in the model's texture order, and how to read
    /// it are the structure's to say — and returning the `Arc` a client
    /// already holds is what lets a render cache keep the GPU texture it
    /// uploaded for it.
    ///
    /// A texture the lookup cannot supply is
    /// [`ClmLoadError::MissingTexture`], naming it; use
    /// [`clm::structure_texture_ids`] to fetch what is missing first. The
    /// structure is read as a **complete model**, and every check
    /// [`Self::from_clm_bytes`] runs is run here.
    ///
    /// The model **keeps its identity and moves its generation**, so a
    /// [`crate::Puppet`] and a render cache built on it see the same model in
    /// a new state: their gates rebake and rebuild rather than refusing. On
    /// any error the model is untouched — the new state is built whole before
    /// anything is swapped.
    pub fn replace_structure(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
    ) -> Result<(), ModelError> {
        self.replace_structure_with_budget(structure, textures, &mut LoadBudget::default())
    }

    pub fn replace_structure_with_budget(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
        budget: &mut LoadBudget,
    ) -> Result<(), ModelError> {
        let structure = clm::decode_structure_with_budget(structure, budget)?;

        let mut table = HashMap::with_capacity(structure.textures.len());
        let mut order = Vec::with_capacity(structure.textures.len());
        for want in &structure.textures {
            let found = textures(&want.id).ok_or_else(|| ClmLoadError::MissingTexture {
                id: want.id.to_string(),
            })?;
            // The structure decides everything but the bytes: two replicas
            // fed the same structure hold the same model whatever their
            // stores happen to remember about a payload.
            let texture = ModelTexture {
                encoding: want.encoding,
                alpha: want.alpha,
                data: found.data,
            };
            if table.insert(want.id.clone(), texture).is_some() {
                return Err(duplicate("texture", &want.id));
            }
            order.push(want.id.clone());
        }
        charge_texture_payloads(
            order
                .iter()
                .filter_map(|id| table.get(id))
                .map(|t| t.data.len() as u64),
            budget,
        )?;
        charge_clm_document(&structure.doc, budget)?;

        let next = Self::build(&structure.doc, table, order, Shape::Base)?;
        self.become_model(next);
        Ok(())
    }

    /// [`Self::replace_structure`] from another `Model` in memory: the
    /// in-tab handoff, where an in-page editor hands its session's model
    /// straight to the replica.
    ///
    /// The structure is cloned and every texture payload is `Arc`-shared with
    /// `other`, so this costs a shallow copy and no texture work. Identity
    /// and generation behave exactly as they do across a structure push.
    pub fn replace_from(&mut self, other: &Model) {
        self.become_model(other.clone());
    }

    /// Take `next`'s whole state while keeping this model's identity, then
    /// move the clock. The two halves are the point: derived objects tell
    /// models apart by identity and states apart by generation, so keeping
    /// one and moving the other is exactly "the same model moved".
    fn become_model(&mut self, next: Model) {
        let identity = self.identity;
        *self = next;
        self.identity = identity;
        self.bump();
    }
}

/// A deform cell holds one `[dx, dy]` per mesh vertex, so its length is the
/// node's flat vertex-array length and nothing else. A [`Model`] sizes the
/// grid from the mesh, so a cell that disagrees with it is a file saying two
/// things at once — refuse it rather than pick a winner.
fn check_deform_cells(
    node: &NodeId,
    expected: usize,
    values: &ClmBindingValues,
) -> Result<(), ModelError> {
    let Some(cells) = deform_cells(values) else {
        return Ok(());
    };
    for cell in cells {
        if cell.value.len() != expected {
            return Err(ClmLoadError::DeformCellShape {
                node: node.to_string(),
                cell: [cell.x, cell.y],
                got: cell.value.len(),
                expected,
            }
            .into());
        }
    }
    Ok(())
}

/// The masks a `.clm` node carries, or none for a kind that cannot hold any.
fn clm_masks_of(kind: &ClmNodeKind) -> &[ClmMask] {
    match kind {
        ClmNodeKind::Part(p) => &p.masks,
        ClmNodeKind::Composite(c) => &c.masks,
        _ => &[],
    }
}

/// A mesh off the wire, held to what [`Model::set_node_mesh`] holds an
/// authored one to — the reader is the only other way a mesh gets into a
/// Model.
fn check_mesh(id: &NodeId, mesh: &ClmMesh) -> Result<(), ModelError> {
    match super::validate_mesh(mesh) {
        Err(ModelError::MalformedMesh(reason)) => Err(ClmLoadError::MalformedMesh {
            node: id.to_string(),
            reason,
        }
        .into()),
        other => other,
    }
}

fn duplicate(kind: &'static str, id: &impl std::fmt::Display) -> ModelError {
    ClmLoadError::DuplicateId {
        kind,
        id: id.to_string(),
    }
    .into()
}

fn clm_kind(kind: &ModelNodeKind) -> ClmNodeKind {
    match kind {
        ModelNodeKind::Group => ClmNodeKind::Group,
        ModelNodeKind::Part(p) => ClmNodeKind::Part(ClmPart {
            mesh: p.mesh().clone(),
            albedo: p.albedo().cloned(),
            opacity: p.opacity,
            blend_mode: p.blend_mode,
            tint: p.tint,
            screen_tint: p.screen_tint,
            masks: clm_masks(p.masks()),
            mask_threshold: p.mask_threshold,
            slots: p
                .slots()
                .iter()
                .map(|slot| ClmSlot {
                    id: slot.id().clone(),
                    vertex: slot.vertex(),
                })
                .collect(),
        }),
        ModelNodeKind::Composite(c) => ClmNodeKind::Composite(ClmComposite {
            opacity: c.opacity,
            blend_mode: c.blend_mode,
            tint: c.tint,
            screen_tint: c.screen_tint,
            masks: clm_masks(c.masks()),
            mask_threshold: c.mask_threshold,
            propagate_meshgroup: c.propagate_meshgroup,
        }),
        ModelNodeKind::MeshGroup(mg) => ClmNodeKind::MeshGroup(ClmMeshGroup {
            mesh: mg.mesh().clone(),
            translate_children: mg.translate_children,
        }),
        ModelNodeKind::SimplePhysics(ph) => ClmNodeKind::SimplePhysics(ClmSimplePhysics {
            kind: ph.kind,
            map_mode: ph.map_mode,
            local_only: ph.local_only,
            target_params: ph.target_params().clone(),
            gravity: ph.gravity,
            length: ph.length,
            frequency: ph.frequency,
            angle_damping: ph.angle_damping,
            length_damping: ph.length_damping,
            output_scale: ph.output_scale,
        }),
    }
}

fn clm_masks(masks: &[ModelMask]) -> Vec<ClmMask> {
    masks
        .iter()
        .map(|m| ClmMask {
            source: m.source().clone(),
            mode: m.mode(),
        })
        .collect()
}

fn model_kind(
    id: &NodeId,
    kind: &ClmNodeKind,
    slots: Vec<Slot>,
    declared: &HashSet<&NodeId>,
    textures: &HashMap<TexId, ModelTexture>,
    params: &HashMap<ParamId, ModelParam>,
    shape: Shape,
) -> Result<ModelNodeKind, ModelError> {
    Ok(match kind {
        ClmNodeKind::Group => ModelNodeKind::Group,
        ClmNodeKind::Part(p) => {
            check_mesh(id, &p.mesh)?;
            let mut part = ModelPart::new(p.mesh.clone());
            if let Some(albedo) = &p.albedo {
                // Checked in either shape, unlike every other reference that
                // leaves a node: an addon carries the textures its own parts
                // draw, so this one never reaches into a base.
                if !textures.contains_key(albedo) {
                    return Err(ClmLoadError::DanglingTexture {
                        node: id.to_string(),
                        id: albedo.to_string(),
                    }
                    .into());
                }
                part.albedo = Some(albedo.clone());
            }
            part.opacity = p.opacity;
            part.blend_mode = p.blend_mode;
            part.tint = p.tint;
            part.screen_tint = p.screen_tint;
            part.masks = model_masks(id, &p.masks, declared, shape)?;
            part.mask_threshold = p.mask_threshold;
            part.slots = slots;
            ModelNodeKind::Part(part)
        }
        ClmNodeKind::Composite(c) => {
            let mut composite = ModelComposite::new();
            composite.opacity = c.opacity;
            composite.blend_mode = c.blend_mode;
            composite.tint = c.tint;
            composite.screen_tint = c.screen_tint;
            composite.masks = model_masks(id, &c.masks, declared, shape)?;
            composite.mask_threshold = c.mask_threshold;
            composite.propagate_meshgroup = c.propagate_meshgroup;
            ModelNodeKind::Composite(composite)
        }
        ClmNodeKind::MeshGroup(mg) => {
            check_mesh(id, &mg.mesh)?;
            let mut group = ModelMeshGroup::new(mg.mesh.clone());
            group.translate_children = mg.translate_children;
            ModelNodeKind::MeshGroup(group)
        }
        ClmNodeKind::SimplePhysics(ph) => {
            let mut physics = ModelPhysics::new(ph.kind);
            physics.map_mode = ph.map_mode;
            physics.local_only = ph.local_only;
            for target in ph.target_params.iter().flatten() {
                if !params.contains_key(target) && !shape.allows_dangling() {
                    return Err(ClmLoadError::DanglingParam {
                        owner: format!("physics node {id}"),
                        id: target.to_string(),
                    }
                    .into());
                }
            }
            physics.target_params = ph.target_params.clone();
            physics.gravity = ph.gravity;
            physics.length = ph.length;
            physics.frequency = ph.frequency;
            physics.angle_damping = ph.angle_damping;
            physics.length_damping = ph.length_damping;
            physics.output_scale = ph.output_scale;
            ModelNodeKind::SimplePhysics(physics)
        }
    })
}

fn model_masks(
    id: &NodeId,
    masks: &[ClmMask],
    declared: &HashSet<&NodeId>,
    shape: Shape,
) -> Result<Vec<ModelMask>, ModelError> {
    masks
        .iter()
        .map(|m| {
            if !declared.contains(&m.source) && !shape.allows_dangling() {
                return Err(ClmLoadError::DanglingNode {
                    node: id.to_string(),
                    field: "mask source",
                    id: m.source.to_string(),
                }
                .into());
            }
            Ok(ModelMask {
                source: m.source.clone(),
                mode: m.mode,
            })
        })
        .collect()
}

/// One part's slots, checked: no repeated slot Id, and every *filled* slot
/// naming a vertex the part's mesh actually has. An unfilled slot is a slot
/// whose part was re-meshed since it was filled; it carries no index to check.
fn read_slots(id: &NodeId, part: &ClmPart) -> Result<Vec<Slot>, ModelError> {
    let vertices = part.mesh.vertex_count();
    let mut out: Vec<Slot> = Vec::with_capacity(part.slots.len());
    for slot in &part.slots {
        if slot.vertex.is_some_and(|v| v as usize >= vertices) {
            return Err(ClmLoadError::SlotOutOfRange {
                node: id.to_string(),
                slot: slot.id.to_string(),
                vertex: slot.vertex.unwrap_or_default(),
                vertices,
            }
            .into());
        }
        if out.iter().any(|s| s.id() == &slot.id) {
            return Err(ClmLoadError::DuplicateSlot {
                node: id.to_string(),
                slot: slot.id.to_string(),
            }
            .into());
        }
        out.push(Slot {
            id: slot.id.clone(),
            vertex: slot.vertex,
        });
    }
    Ok(out)
}

/// A weld, checked against the slots the file declares: the two ends have to
/// be two different parts, each pair has to name a slot the part on its side
/// carries, no slot twice, and each weight has to be a share within `0..=1`.
/// Whether a slot is *filled* is not this check's business:
/// [`ModelWeld::resolve`] skips the pairs that are not.
///
/// A **fragment** may weld its own part to a base part, so an end naming a
/// node the file does not carry is checked as far as it can be — the pairs on
/// the resolvable side still have to name that part's slots — and
/// [`Model::install`] finishes the job against the base.
fn read_weld(
    weld: &ClmWeld,
    slots: &HashMap<&NodeId, SlotTable>,
    nodes: &HashMap<NodeId, ModelNode>,
    declared: &HashSet<&NodeId>,
    shape: Shape,
) -> Result<ModelWeld, ModelError> {
    if weld.a == weld.b {
        return Err(ClmLoadError::WeldSelfPaired {
            node: weld.a.to_string(),
        }
        .into());
    }
    let slots_of = |end: &NodeId| -> Result<Option<&SlotTable>, ModelError> {
        match slots.get(end) {
            Some(slots) => Ok(Some(slots)),
            // The part is missing because it is not a part, or because the
            // file does not carry it — in a fragment the latter is a
            // requirement, not a broken reference.
            None => match nodes.get(end) {
                Some(node) => Err(ClmLoadError::WeldEndNotAPart {
                    node: end.to_string(),
                    kind: node.kind.name(),
                }
                .into()),
                None if shape.allows_dangling() && !declared.contains(end) => Ok(None),
                None => Err(ClmLoadError::DanglingWeldEnd {
                    node: end.to_string(),
                }
                .into()),
            },
        }
    };
    let ends = [slots_of(&weld.a)?, slots_of(&weld.b)?];

    let mut seen_a = HashSet::with_capacity(weld.pairs.len());
    let mut seen_b = HashSet::with_capacity(weld.pairs.len());
    let mut pairs = Vec::with_capacity(weld.pairs.len());
    for pair in &weld.pairs {
        if !(0.0..=1.0).contains(&pair.weight) {
            return Err(ClmLoadError::WeldWeightOutOfRange {
                a: weld.a.to_string(),
                b: weld.b.to_string(),
                slot: pair.a.to_string(),
            }
            .into());
        }
        for (carried, (slot, node)) in ends
            .into_iter()
            .zip([(&pair.a, &weld.a), (&pair.b, &weld.b)])
        {
            if carried.is_some_and(|known| !known.contains(slot)) {
                return Err(ClmLoadError::WeldUnknownSlot {
                    node: node.to_string(),
                    slot: slot.to_string(),
                }
                .into());
            }
        }
        for (seen, (slot, node)) in [&mut seen_a, &mut seen_b]
            .into_iter()
            .zip([(&pair.a, &weld.a), (&pair.b, &weld.b)])
        {
            if !seen.insert(slot) {
                return Err(ClmLoadError::WeldSlotPairedTwice {
                    a: weld.a.to_string(),
                    b: weld.b.to_string(),
                    slot: format!("{node}/{slot}"),
                }
                .into());
            }
        }
        pairs.push(SlotPair {
            a: pair.a.clone(),
            b: pair.b.clone(),
            weight: pair.weight,
        });
    }
    Ok(ModelWeld::new(weld.a.clone(), weld.b.clone(), pairs))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::components::{BlendMode, MaskMode};
    use crate::formats::clm::{
        ClmAnimation, ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmKeyframe, ClmLane,
        ClmMesh, TextureAlpha, TextureEncoding,
    };
    use crate::id::SeededHex;
    use crate::interpolate::InterpolateMode;
    use crate::physics::PendulumKind;

    fn slot(id: &str) -> SlotId {
        SlotId::new(id).unwrap()
    }

    fn quad(x: f32) -> ClmMesh {
        ClmMesh {
            verts: vec![x - 1.0, -1.0, x + 1.0, -1.0, x + 1.0, 1.0, x - 1.0, 1.0],
            uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    /// One model touching every Id-bearing field the wire has: a textured part
    /// with a mask, a composite, a mesh group, a pendulum driving two params, a
    /// two-param deform binding, a weld, and an animation.
    fn sample() -> Model {
        let mut hex = SeededHex::new(9);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();

        let upper = m
            .add_node(
                &root,
                ModelNode::new("Upper", ModelNodeKind::Part(ModelPart::new(quad(0.0)))),
                &mut hex,
            )
            .unwrap();
        let lower = m
            .add_node(
                &root,
                ModelNode::new("Lower", ModelNodeKind::Part(ModelPart::new(quad(3.0)))),
                &mut hex,
            )
            .unwrap();
        m.add_texture(
            &upper,
            ModelTexture {
                encoding: TextureEncoding::Png,
                alpha: TextureAlpha::Straight,
                data: [0x89, b'P', b'N', b'G', 1, 2, 3, 4][..].into(),
            },
            &mut hex,
        )
        .unwrap();

        let composite = m
            .add_node(
                &root,
                ModelNode::new("Face", ModelNodeKind::Composite(ModelComposite::new())),
                &mut hex,
            )
            .unwrap();
        m.mask_add(&composite, &lower, MaskMode::DodgeMask).unwrap();
        m.update_node(&composite, |n| {
            if let ModelNodeKind::Composite(c) = &mut n.kind {
                c.blend_mode = BlendMode::Multiply;
            }
        })
        .unwrap();

        m.add_node(
            &composite,
            ModelNode::new(
                "Bend",
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad(6.0))),
            ),
            &mut hex,
        )
        .unwrap();

        let x = m
            .add_param(
                ModelParam::new(Name::truncated("Head.x"), -1.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        let y = m
            .add_param(
                ModelParam::new(Name::truncated("Head.y"), -1.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();

        let pendulum = m
            .add_node(
                &root,
                ModelNode::new(
                    "Sway",
                    ModelNodeKind::SimplePhysics(ModelPhysics::new(PendulumKind::RigidPendulum)),
                ),
                &mut hex,
            )
            .unwrap();
        m.set_physics_targets(&pendulum, [Some(x.clone()), Some(y.clone())])
            .unwrap();

        let key = BindingKey::pair(x.clone(), y.clone(), upper.clone(), BindingTarget::Deform);
        m.add_binding(&key).unwrap();
        m.set_deform_vertices(&key, [1, 1], vec![1.0; 8]).unwrap();

        let (left, right) = (slot("left"), slot("right"));
        for (node, verts) in [(&upper, [0, 3]), (&lower, [1, 2])] {
            for (slot, vertex) in [(&left, verts[0]), (&right, verts[1])] {
                m.slot_add(node, slot.clone()).unwrap();
                m.slot_fill(node, slot, vertex).unwrap();
            }
        }
        m.set_welds(vec![ModelWeld::new(
            upper,
            lower,
            vec![
                SlotPair {
                    a: left.clone(),
                    b: left,
                    weight: 1.0,
                },
                SlotPair {
                    a: right.clone(),
                    b: right,
                    weight: 0.25,
                },
            ],
        )])
        .unwrap();

        m.set_animations(vec![ClmAnimation {
            name: "nod".into(),
            length: 12,
            lanes: vec![ClmLane {
                param: x,
                interpolation: InterpolateMode::Linear,
                keyframes: vec![ClmKeyframe {
                    frame: 4,
                    value: 0.5,
                }],
            }],
            ..ClmAnimation::default()
        }])
        .unwrap();
        m
    }

    fn named(m: &Model, name: &str) -> NodeId {
        m.nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == name))
            .unwrap_or_else(|| panic!("no node named {name}"))
    }

    fn node_named<'a>(file: &'a ClmFile, name: &str) -> &'a ClmNode {
        file.doc
            .nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name}"))
    }

    fn part_named<'a>(file: &'a mut ClmFile, name: &str) -> &'a mut ClmPart {
        let node = file
            .doc
            .nodes
            .iter_mut()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name}"));
        match &mut node.kind {
            ClmNodeKind::Part(p) => p,
            _ => panic!("{name} is not a part"),
        }
    }

    fn load_err(file: &ClmFile) -> ClmLoadError {
        match Model::from_clm_file(file) {
            Err(ModelError::InvalidClm(e)) => e,
            other => panic!("expected a structured load error, got {other:?}"),
        }
    }

    fn fragment_err(file: &ClmFile) -> ClmLoadError {
        match Model::from_clm_file_fragment(file) {
            Err(ModelError::InvalidClm(e)) => e,
            other => panic!("expected a structured load error, got {other:?}"),
        }
    }

    /// [`sample`] with the root, the `Lower` part and the params taken out:
    /// what is left is an addon that hangs off `root`, is masked by a base
    /// part, welds into one, and is deformed and animated by base params.
    /// Every kind of dangling reference at once — the texture table stays,
    /// because the one reference an addon never makes is into the base's.
    fn sample_fragment_file() -> ClmFile {
        let mut file = sample().to_clm_file().unwrap();
        let lower = node_named(&file, "Lower").id.clone();
        file.doc
            .nodes
            .retain(|n| n.parent.is_some() && n.id != lower);
        file.doc.params.clear();
        file
    }

    #[test]
    fn a_fragment_is_refused_by_the_complete_model_reader() {
        assert!(matches!(
            load_err(&sample_fragment_file()),
            ClmLoadError::DanglingParent { .. }
        ));
    }

    #[test]
    fn a_complete_model_is_refused_by_the_fragment_reader() {
        let file = sample().to_clm_file().unwrap();
        assert!(matches!(
            fragment_err(&file),
            ClmLoadError::FragmentHasNoParent { node } if node == "root"
        ));
    }

    /// The point of the fragment shape: nothing that reaches into a base model
    /// is dropped, defaulted or rewritten on the way through a file.
    #[test]
    fn a_fragment_round_trips_with_its_dangling_references() {
        let file = sample_fragment_file();
        let bytes = clm::encode(&file.doc, &file.textures).unwrap();
        let f = Model::from_clm_bytes_fragment(&bytes).unwrap();

        assert!(f.is_fragment());
        assert_eq!(f.root(), None);
        let upper = named(&f, "Upper");
        let face = named(&f, "Face");
        assert_eq!(f.roots(), [upper.clone(), face.clone(), named(&f, "Sway")]);
        // Every root names the base node it hangs off.
        for r in f.roots() {
            let parent = f.node(r).unwrap().parent().unwrap();
            assert_eq!(parent.as_str(), "root");
            assert!(f.node(parent).is_none());
        }
        // Its own texture came with it; every other reference still points
        // at the base.
        assert!(f.texture(part(&f, &upper).albedo().unwrap()).is_some());
        assert!(f.node(part_mask_source(&f, &face)).is_none());
        assert!(f.node(f.welds()[0].b()).is_none());
        assert_eq!(f.welds()[0].pairs().len(), 2);
        let binding = f.bindings().next().unwrap();
        assert!(binding.params().iter().all(|p| f.param(p).is_none()));
        assert!(f.animations()[0]
            .lanes
            .iter()
            .all(|l| f.param(&l.param).is_none()));

        assert_eq!(f.to_clm_bytes().unwrap(), bytes);
        let again = Model::from_clm_bytes_fragment(&f.to_clm_bytes().unwrap()).unwrap();
        assert_eq!(again.to_clm_bytes().unwrap(), bytes);
    }

    fn part<'a>(m: &'a Model, id: &NodeId) -> &'a ModelPart {
        match &m.node(id).unwrap().kind {
            ModelNodeKind::Part(p) => p,
            other => panic!("{id} is a {}", other.name()),
        }
    }

    fn part_mask_source<'a>(m: &'a Model, id: &NodeId) -> &'a NodeId {
        match &m.node(id).unwrap().kind {
            ModelNodeKind::Composite(c) => c.masks()[0].source(),
            other => panic!("{id} is a {}", other.name()),
        }
    }

    #[test]
    fn a_model_round_trips_through_clm() {
        let m = sample();
        let file = m.to_clm_file().unwrap();
        let reopened = Model::from_clm_file(&file).unwrap();
        assert_eq!(reopened.to_clm_file().unwrap(), file);
        assert_eq!(reopened.to_clm_bytes().unwrap(), m.to_clm_bytes().unwrap());
    }

    /// Everything the model holds is keyed by Id, so a round trip that lost or
    /// rewrote one would still compare equal field for field. Pin the Ids and
    /// the labels themselves.
    #[test]
    fn ids_and_names_survive_the_round_trip() {
        let m = sample();
        let reopened = Model::from_clm_bytes(&m.to_clm_bytes().unwrap()).unwrap();

        assert_eq!(reopened.root(), m.root());
        assert_eq!(reopened.nodes_in_order(), m.nodes_in_order());
        assert_eq!(reopened.param_ids(), m.param_ids());
        assert_eq!(reopened.texture_ids(), m.texture_ids());
        for id in m.nodes_in_order() {
            assert_eq!(
                reopened.node(&id).unwrap().name.as_str(),
                m.node(&id).unwrap().name.as_str(),
                "name of {id}",
            );
        }
        for id in m.param_ids() {
            assert_eq!(
                reopened.param(id).unwrap().name.as_str(),
                m.param(id).unwrap().name.as_str(),
                "name of {id}",
            );
        }
        assert_eq!(
            reopened.bindings().next().unwrap().key(),
            m.bindings().next().unwrap().key(),
        );
    }

    /// The editor's dirty check is byte equality, so an open-and-save with no
    /// edit has to be a no-op — twice over, since a `HashMap` iteration order
    /// leaking into the writer would only show up on a second pass.
    #[test]
    fn saving_the_same_model_twice_writes_the_same_bytes() {
        let m = sample();
        let once = m.to_clm_bytes().unwrap();
        assert_eq!(m.to_clm_bytes().unwrap(), once);

        let reopened = Model::from_clm_bytes(&once).unwrap();
        assert_eq!(reopened.to_clm_bytes().unwrap(), once);
        assert_eq!(reopened.to_clm_bytes().unwrap(), once);
    }

    /// A part's slots travel with the part and a weld travels as the two
    /// parts it names plus its pairs — the file is a copy of the model, not a
    /// translation of it.
    #[test]
    fn a_weld_travels_as_two_parts_and_its_pairs() {
        let m = sample();
        let file = m.to_clm_file().unwrap();

        let upper = match &node_named(&file, "Upper").kind {
            ClmNodeKind::Part(p) => p,
            _ => panic!("Upper is a part"),
        };
        assert_eq!(
            upper
                .slots
                .iter()
                .map(|s| (s.id.to_string(), s.vertex))
                .collect::<Vec<_>>(),
            vec![
                ("left".to_string(), Some(0)),
                ("right".to_string(), Some(3))
            ],
        );
        let [weld] = &file.doc.welds[..] else {
            panic!("one weld")
        };
        assert_eq!(weld.a, named(&m, "Upper"));
        assert_eq!(weld.b, named(&m, "Lower"));
        assert_eq!(
            weld.pairs
                .iter()
                .map(|p| (p.a.to_string(), p.b.to_string(), p.weight))
                .collect::<Vec<_>>(),
            vec![
                ("left".to_string(), "left".to_string(), 1.0),
                ("right".to_string(), "right".to_string(), 0.25)
            ],
        );

        let reopened = Model::from_clm_file(&file).unwrap();
        assert_eq!(
            reopened.welds()[0].resolve(&reopened),
            m.welds()[0].resolve(&m)
        );
    }

    /// A mesh edit empties the slots on the part it re-meshed but leaves the
    /// weld alone, so the file carries an unfilled slot and reading it back
    /// gives a weld that solves the pairs it still resolves.
    #[test]
    fn a_weld_over_an_unfilled_slot_loads_and_skips_the_pair() {
        let mut m = sample();
        let upper = named(&m, "Upper");
        m.slot_clear(&upper, &slot("right")).unwrap();

        let bytes = m.to_clm_bytes().unwrap();
        let reopened = Model::from_clm_bytes(&bytes).unwrap();

        let [weld] = reopened.welds() else {
            panic!("one weld")
        };
        assert_eq!(weld.pairs().len(), 2, "the slot is still paired");
        assert_eq!(
            weld.resolve(&reopened),
            vec![ModelWeldPair {
                a_vert: 0,
                b_vert: 1,
                weight: 1.0,
            }],
            "only the filled pair solves",
        );
        assert_eq!(reopened.unfilled_slots(), vec![(upper, slot("right"))],);
        assert_eq!(reopened.to_clm_bytes().unwrap(), bytes);
    }

    #[test]
    fn a_dangling_parent_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.nodes[1].parent = Some(NodeId::new("ghost").unwrap());

        assert_eq!(
            load_err(&file),
            ClmLoadError::DanglingParent {
                node: file.doc.nodes[1].id.to_string(),
                parent: "ghost".into(),
            }
        );
    }

    /// A parent that exists but is written later is a different fault from one
    /// that does not exist: the file is readable, just not in one pass.
    #[test]
    fn a_parent_written_after_its_child_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let index = |name: &str| {
            file.doc
                .nodes
                .iter()
                .position(|n| n.name == name)
                .unwrap_or_else(|| panic!("no node named {name}"))
        };
        let (face, bend) = (index("Face"), index("Bend"));
        file.doc.nodes.swap(face, bend);

        assert!(matches!(
            load_err(&file),
            ClmLoadError::NotTopological { .. }
        ));
    }

    #[test]
    fn two_roots_are_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.nodes[1].parent = None;

        assert!(matches!(
            load_err(&file),
            ClmLoadError::MultipleRoots { .. }
        ));
    }

    #[test]
    fn a_dangling_albedo_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        part_named(&mut file, "Upper").albedo = Some(TexId::new("gone").unwrap());

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DanglingTexture { id, .. } if id == "gone"
        ));
    }

    /// An albedo is the one reference a fragment may not dangle: an addon
    /// carries the textures its own parts draw, so there is no base texture
    /// for it to be reaching at.
    #[test]
    fn a_fragments_albedo_may_not_dangle_either() {
        let mut file = sample_fragment_file();
        let drawn = part_named(&mut file, "Upper")
            .albedo
            .clone()
            .expect("Upper draws one");
        file.textures.clear();

        assert!(matches!(
            fragment_err(&file),
            ClmLoadError::DanglingTexture { id, .. } if id == drawn.as_str()
        ));
    }

    /// The other half of the same rule: a texture no part's albedo names is
    /// one an author cannot have meant, and it would survive every edit
    /// afterwards because nothing would ever collect it.
    #[test]
    fn a_texture_no_part_draws_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let spare = ClmTexture {
            id: TexId::new("spare").unwrap(),
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: vec![0x89, b'P', b'N', b'G'],
        };
        file.textures.push(spare);

        assert!(matches!(
            load_err(&file),
            ClmLoadError::UnusedTexture { id } if id == "spare"
        ));

        // And in a fragment, where a texture reaching into the base is
        // exactly what an addon may not do.
        let mut file = sample_fragment_file();
        file.textures.push(ClmTexture {
            id: TexId::new("spare").unwrap(),
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: vec![0x89, b'P', b'N', b'G'],
        });
        assert!(matches!(
            fragment_err(&file),
            ClmLoadError::UnusedTexture { id } if id == "spare"
        ));
    }

    #[test]
    fn a_dangling_mask_source_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        for node in &mut file.doc.nodes {
            if let ClmNodeKind::Composite(c) = &mut node.kind {
                c.masks[0].source = NodeId::new("ghost").unwrap();
            }
        }

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DanglingNode {
                field: "mask source",
                id,
                ..
            } if id == "ghost"
        ));
    }

    /// `Model::set_param_range` refuses a collapsed, inverted or non-finite
    /// range because the normalized key positions have nothing to map onto;
    /// a file carrying one has to be refused for the same reason.
    #[test]
    fn a_param_range_the_model_would_refuse_is_a_structured_error() {
        for (min, max) in [
            (1.0, 0.0),
            (0.0, 0.0),
            (f32::NAN, 1.0),
            (0.0, f32::INFINITY),
        ] {
            let mut file = sample().to_clm_file().unwrap();
            let id = file.doc.params[0].id.clone();
            file.doc.params[0].min = min;
            file.doc.params[0].max = max;

            assert_eq!(
                load_err(&file),
                ClmLoadError::ParamRange {
                    param: id.to_string(),
                },
                "range {min}..{max}",
            );
        }
    }

    /// A mask is the source's own drawing, so the source has to be one of the
    /// two kinds the renderer draws. `Model::mask_add` refuses the rest.
    #[test]
    fn a_mask_source_that_is_never_drawn_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let bend = node_named(&file, "Bend").id.clone();
        let face = node_named(&file, "Face").id.clone();
        for node in &mut file.doc.nodes {
            if let ClmNodeKind::Composite(c) = &mut node.kind {
                c.masks[0].source = bend.clone();
            }
        }

        assert_eq!(
            load_err(&file),
            ClmLoadError::MaskSourceKind {
                node: face.to_string(),
                mask_source: bend.to_string(),
                kind: "mesh_group",
            }
        );
    }

    /// A composite is drawn, so it is a source like a part is — which is what
    /// `tests/models/composite_masks.clm` has always been a baseline of.
    #[test]
    fn a_composite_mask_source_loads() {
        let mut file = sample().to_clm_file().unwrap();
        let face = node_named(&file, "Face").id.clone();
        part_named(&mut file, "Upper").masks.push(ClmMask {
            source: face.clone(),
            mode: MaskMode::Mask,
        });

        let m = Model::from_clm_file(&file).unwrap();

        let upper = named(&m, "Upper");
        assert_eq!(part(&m, &upper).masks()[0].source(), &face);
    }

    /// `Model::add_binding` refuses a deform on a node with no mesh to
    /// deform. The reader used to take one: `check_deform_cells` asks for
    /// cells of length zero, and a file that carries none agrees.
    #[test]
    fn a_deform_binding_on_a_node_with_no_mesh_is_a_structured_error() {
        for (name, kind) in [("Face", "composite"), ("Sway", "physics")] {
            let mut file = sample().to_clm_file().unwrap();
            let node = node_named(&file, name).id.clone();
            file.doc.bindings[0].node = node.clone();
            file.doc.bindings[0].values = ClmBindingValues::Deform(ClmCells::default());

            assert_eq!(
                load_err(&file),
                ClmLoadError::DeformOnUnmeshed {
                    node: node.to_string(),
                    kind,
                }
            );
        }
    }

    /// The mesh checks `Model::set_node_mesh` runs, run on the way in as
    /// well: the reader is the only other door a mesh comes through.
    #[test]
    fn a_malformed_mesh_is_a_structured_error() {
        let cases: [(BreakMesh, &str); 3] = [
            (
                |mesh| mesh.verts.push(0.5),
                "vertex array is not [x, y] pairs",
            ),
            (
                |mesh| mesh.uvs.truncate(6),
                "uv count does not match vertices",
            ),
            (
                |mesh| mesh.indices = ClmIndices::U16(vec![0, 1, 9]),
                "index names a missing vertex",
            ),
        ];

        // Both meshed kinds, so neither door is left open.
        for (name, take) in [("Upper", part_mesh as MeshOf), ("Bend", group_mesh)] {
            for (break_it, reason) in cases {
                let mut file = sample().to_clm_file().unwrap();
                let node = node_named(&file, name).id.clone();
                let target = file
                    .doc
                    .nodes
                    .iter_mut()
                    .find(|n| n.name == name)
                    .expect("the node");
                break_it(take(target));

                assert_eq!(
                    load_err(&file),
                    ClmLoadError::MalformedMesh {
                        node: node.to_string(),
                        reason,
                    },
                    "{name}: {reason}",
                );
            }
        }
    }

    /// One way to break a mesh, and the way to reach the mesh on one node
    /// kind: `a_malformed_mesh_is_a_structured_error` crosses the two.
    type BreakMesh = fn(&mut ClmMesh);
    type MeshOf = fn(&mut ClmNode) -> &mut ClmMesh;

    fn part_mesh(node: &mut ClmNode) -> &mut ClmMesh {
        match &mut node.kind {
            ClmNodeKind::Part(p) => &mut p.mesh,
            other => panic!("not a part: {other:?}"),
        }
    }

    fn group_mesh(node: &mut ClmNode) -> &mut ClmMesh {
        match &mut node.kind {
            ClmNodeKind::MeshGroup(g) => &mut g.mesh,
            other => panic!("not a mesh group: {other:?}"),
        }
    }

    #[test]
    fn a_dangling_physics_target_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        for node in &mut file.doc.nodes {
            if let ClmNodeKind::SimplePhysics(ph) = &mut node.kind {
                ph.target_params[1] = Some(ParamId::new("gone").unwrap());
            }
        }

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DanglingParam { id, .. } if id == "gone"
        ));
    }

    #[test]
    fn a_duplicate_node_id_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let twin = file.doc.nodes[1].id.clone();
        file.doc.nodes[2].id = twin.clone();

        assert_eq!(
            load_err(&file),
            ClmLoadError::DuplicateId {
                kind: "node",
                id: twin.to_string(),
            }
        );
    }

    #[test]
    fn a_duplicate_param_id_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let twin = file.doc.params[0].id.clone();
        file.doc.params[1].id = twin.clone();

        assert_eq!(
            load_err(&file),
            ClmLoadError::DuplicateId {
                kind: "param",
                id: twin.to_string(),
            }
        );
    }

    /// A binding's grid is one or two params wide and nothing else has a shape
    /// for a third, so a file that names one has to be refused rather than
    /// truncated.
    #[test]
    fn a_binding_over_three_params_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let third = file.doc.params[0].id.clone();
        file.doc.bindings[0].params.push(third);

        assert!(matches!(
            load_err(&file),
            ClmLoadError::BindingParamCount { got: 3, .. }
        ));
    }

    #[test]
    fn a_binding_over_one_param_twice_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let first = file.doc.bindings[0].params[0].clone();
        file.doc.bindings[0].params[1] = first;

        assert!(matches!(
            load_err(&file),
            ClmLoadError::BindingParamCount { got: 2, .. }
        ));
    }

    #[test]
    fn a_dangling_binding_node_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.bindings[0].node = NodeId::new("ghost").unwrap();

        assert_eq!(
            load_err(&file),
            ClmLoadError::DanglingBindingNode { id: "ghost".into() }
        );
    }

    #[test]
    fn a_dangling_binding_param_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.bindings[0].params[0] = ParamId::new("gone").unwrap();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DanglingParam { id, .. } if id == "gone"
        ));
    }

    /// `Model::add_binding` refuses a colour target on a mesh group, but a
    /// Model read from a file used to accept it and the fold then dropped it
    /// silently. The reader is where that hole closes.
    #[test]
    fn a_colour_binding_on_a_mesh_group_is_a_structured_error() {
        for (values, target) in [
            (ClmBindingValues::Opacity(ClmCells::default()), "opacity"),
            (ClmBindingValues::TintR(ClmCells::default()), "tintr"),
            (ClmBindingValues::TintG(ClmCells::default()), "tintg"),
            (ClmBindingValues::TintB(ClmCells::default()), "tintb"),
            (
                ClmBindingValues::ScreenTintR(ClmCells::default()),
                "screentintr",
            ),
            (
                ClmBindingValues::ScreenTintG(ClmCells::default()),
                "screentintg",
            ),
            (
                ClmBindingValues::ScreenTintB(ClmCells::default()),
                "screentintb",
            ),
        ] {
            let mut file = sample().to_clm_file().unwrap();
            let bend = node_named(&file, "Bend").id.clone();
            file.doc.bindings[0].node = bend.clone();
            file.doc.bindings[0].values = values;

            assert_eq!(
                load_err(&file),
                ClmLoadError::ColorOnMeshGroup {
                    node: bend.to_string(),
                    target,
                }
            );
        }
    }

    /// A non-colour target on a mesh group is exactly what a mesh group is
    /// for, so the check has to be about colour and not about mesh groups.
    #[test]
    fn a_deform_binding_on_a_mesh_group_still_loads() {
        let mut file = sample().to_clm_file().unwrap();
        let bend = node_named(&file, "Bend").id.clone();
        file.doc.bindings[0].node = bend;
        file.doc.bindings[0].values = ClmBindingValues::Deform(ClmCells::default());

        assert!(Model::from_clm_file(&file).is_ok());
    }

    /// A Model sizes a deform grid from the mesh, so a file whose cell and
    /// mesh disagree is saying two things at once about the same node.
    #[test]
    fn a_deform_cell_the_mesh_cannot_take_is_a_structured_error() {
        let node = {
            let file = sample().to_clm_file().unwrap();
            node_named(&file, "Upper").id.clone()
        };

        for got in [6usize, 10] {
            let mut file = sample().to_clm_file().unwrap();
            match &mut file.doc.bindings[0].values {
                ClmBindingValues::Deform(cells) => {
                    for cell in &mut cells.cells {
                        cell.value.resize(got, 0.0);
                    }
                }
                other => panic!("expected a deform binding, got {other:?}"),
            }

            assert_eq!(
                load_err(&file),
                ClmLoadError::DeformCellShape {
                    node: node.to_string(),
                    cell: [0, 0],
                    got,
                    expected: 8,
                }
            );
        }
    }

    /// And the writer refuses the same shape, so a bad refit is reported when
    /// the file is saved rather than when someone next opens it.
    #[test]
    fn a_deform_cell_the_mesh_cannot_take_cannot_be_written() {
        let mut m = sample();
        let upper = m
            .nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == "Upper"))
            .unwrap();
        // A refit that returns the wrong length is the one way a Model can
        // reach this state; `set_node_mesh` itself resizes correctly.
        m.set_node_mesh_with(&upper, quad(0.0), |_, _, offsets| {
            offsets.to_vec()[..2].to_vec()
        })
        .unwrap();

        assert!(matches!(
            m.to_clm_file(),
            Err(ModelError::InvalidClm(ClmLoadError::DeformCellShape {
                got: 2,
                expected: 8,
                ..
            }))
        ));
    }

    #[test]
    fn a_slot_past_the_mesh_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        part_named(&mut file, "Upper").slots[1].vertex = Some(99);

        assert!(matches!(
            load_err(&file),
            ClmLoadError::SlotOutOfRange {
                vertex: 99,
                vertices: 4,
                ..
            }
        ));
    }

    #[test]
    fn a_part_carrying_one_slot_id_twice_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let slots = &mut part_named(&mut file, "Upper").slots;
        slots[1].id = slots[0].id.clone();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DuplicateSlot { slot, .. } if slot == "left"
        ));
    }

    #[test]
    fn a_weld_pairing_a_slot_the_part_lacks_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].pairs[0].a = SlotId::new("ghost").unwrap();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldUnknownSlot { slot, .. } if slot == "ghost"
        ));
    }

    /// A slot may be paired once per weld: twice would pull one vertex to two
    /// meeting points and the solve has no answer for that.
    #[test]
    fn a_weld_pairing_one_slot_twice_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].pairs[1].a = file.doc.welds[0].pairs[0].a.clone();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldSlotPairedTwice { .. }
        ));
    }

    /// A weight is a share of a meeting point, so one outside `0..=1` has no
    /// meaning; and one weld records one pair of parts.
    #[test]
    fn the_other_things_a_weld_cannot_say_are_structured_errors() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].pairs[0].weight = 1.5;
        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldWeightOutOfRange { .. }
        ));

        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].b = file.doc.welds[0].a.clone();
        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldSelfPaired { .. }
        ));

        let mut file = sample().to_clm_file().unwrap();
        let mut twin = file.doc.welds[0].clone();
        std::mem::swap(&mut twin.a, &mut twin.b);
        file.doc.welds.push(twin);
        assert!(matches!(
            load_err(&file),
            ClmLoadError::DuplicateWeld { .. }
        ));

        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].a = named(&sample(), "Sway");
        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldEndNotAPart { .. }
        ));
    }

    #[test]
    fn a_dangling_animation_lane_param_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.animations[0].lanes[0].param = ParamId::new("gone").unwrap();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DanglingParam { id, .. } if id == "gone"
        ));
    }

    #[test]
    fn an_unsorted_animation_lane_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.animations[0].lanes[0].keyframes = vec![
            ClmKeyframe {
                frame: 8,
                value: 1.0,
            },
            ClmKeyframe {
                frame: 2,
                value: 0.0,
            },
        ];

        assert!(matches!(
            load_err(&file),
            ClmLoadError::UnsortedLane { animation: 0, .. }
        ));
    }

    /// A file with no `animations` key at all reads as a model with none, so
    /// an older writer's output still opens.
    #[test]
    fn an_absent_animations_section_reads_as_none() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.animations.clear();
        let bytes = clm::encode(&file.doc, &file.textures).unwrap();

        let m = Model::from_clm_bytes(&bytes).unwrap();

        assert!(m.animations().is_empty());
    }

    #[test]
    fn the_shared_aggregate_budget_applies() {
        let file = sample().to_clm_file().unwrap();
        let mut budget = LoadBudget::new(crate::LoadLimits {
            nodes: 1,
            ..crate::LoadLimits::default()
        });

        let err = Model::from_clm_file_with_budget(&file, &mut budget).unwrap_err();

        assert!(matches!(
            err,
            ModelError::LoadLimit(crate::LoadLimitError {
                resource: "nodes",
                ..
            })
        ));
    }

    /// Authored cells reach the wire through `ClmBindingValues`; pin one so the
    /// round-trip test is not the only thing that touches them.
    #[test]
    fn authored_cells_reach_the_wire() {
        let file = sample().to_clm_file().unwrap();
        match &file.doc.bindings[0].values {
            ClmBindingValues::Deform(ClmCells { cells }) => assert!(
                cells
                    .iter()
                    .any(|c: &ClmCell<Vec<f32>>| c.x == 1 && c.y == 1 && c.value == vec![1.0; 8]),
                "the authored cell is on the wire: {cells:?}",
            ),
            other => panic!("expected a deform binding, got {other:?}"),
        }
    }

    // ---- the structure path: a replica's whole edit ---------------------

    /// The textures a model holds, as a replica's store would: keyed by Id,
    /// payloads `Arc`-shared with the model they came from.
    fn store(m: &Model) -> HashMap<TexId, ModelTexture> {
        m.texture_ids()
            .iter()
            .filter_map(|id| Some((id.clone(), m.texture(id)?.clone())))
            .collect()
    }

    /// The one byte-level promise a structure makes: its `Structure` section
    /// is the same section the file would have written. A replica that
    /// applies a structure and then saves has to produce the file the server
    /// would have.
    #[test]
    fn a_structure_carries_the_files_own_structure_section() {
        let m = sample();
        let file = clm::decode(&m.to_clm_bytes().unwrap()).unwrap();
        let structure = clm::decode_structure(&m.to_structure_bytes().unwrap()).unwrap();

        assert_eq!(structure.doc, file.doc);
        assert_eq!(
            structure.textures,
            file.textures
                .iter()
                .map(ClmTextureRef::from)
                .collect::<Vec<_>>(),
            "the manifest is the texture table with the payloads taken out",
        );
        assert!(
            m.to_structure_bytes().unwrap().len() < m.to_clm_bytes().unwrap().len(),
            "a structure carries no payloads",
        );
        assert_eq!(
            m.to_structure_bytes().unwrap(),
            m.to_structure_bytes().unwrap(),
            "and it is byte-stable like the file it comes from",
        );
    }

    /// The two container shapes are disjoint and each reader says so by name,
    /// rather than reading a structure as a model whose textures all vanished.
    #[test]
    fn the_two_container_shapes_refuse_each_other() {
        let m = sample();
        assert!(matches!(
            clm::decode(&m.to_structure_bytes().unwrap()),
            Err(clm::ClmError::StructureOnly),
        ));
        assert!(matches!(
            clm::decode_structure(&m.to_clm_bytes().unwrap()),
            Err(clm::ClmError::NotAStructure),
        ));
    }

    /// What a client reads before it fetches: the Ids in the model's own
    /// order, each with the encoding its bytes will need.
    #[test]
    fn a_structures_textures_are_listed_without_building_a_model() {
        let m = sample();
        let listed = clm::structure_texture_ids(&m.to_structure_bytes().unwrap()).unwrap();

        assert_eq!(
            listed.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            m.texture_ids(),
        );
        let held = m.texture(&listed[0].id).unwrap();
        assert_eq!(listed[0].encoding, held.encoding);
        assert_eq!(listed[0].alpha, held.alpha);
    }

    /// The whole point: the document crosses, the payloads do not, and what
    /// lands is the model the server holds — same bytes, same texture order.
    #[test]
    fn applying_a_structure_rebuilds_the_model_over_the_payloads_it_has() {
        let server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let held = store(&replica);

        replica
            .replace_structure(&server.to_structure_bytes().unwrap(), |id| {
                held.get(id).cloned()
            })
            .unwrap();

        assert_eq!(
            replica.to_clm_bytes().unwrap(),
            server.to_clm_bytes().unwrap()
        );
        assert_eq!(replica.texture_ids(), server.texture_ids());
        for id in replica.texture_ids() {
            assert!(
                Arc::ptr_eq(&replica.texture(id).unwrap().data, &held[id].data),
                "the payload the store held is the payload the model now holds",
            );
        }
    }

    /// A structure is held to every rule a file is: the reader is the same
    /// reader.
    #[test]
    fn a_structure_is_read_as_strictly_as_a_file() {
        let m = sample();
        let mut doc = m.to_clm_document().unwrap();
        doc.nodes[1].parent = Some(NodeId::new("ghost").unwrap());
        let refs: Vec<ClmTextureRef> = m
            .texture_ids()
            .iter()
            .map(|id| ClmTextureRef {
                id: id.clone(),
                encoding: m.texture(id).unwrap().encoding,
                alpha: m.texture(id).unwrap().alpha,
            })
            .collect();
        let bytes = clm::encode_structure(&doc, &refs).unwrap();

        let mut replica = m.clone();
        let held = store(&m);
        let err = replica
            .replace_structure(&bytes, |id| held.get(id).cloned())
            .unwrap_err();

        assert!(matches!(
            err,
            ModelError::InvalidClm(ClmLoadError::DanglingParent { .. })
        ));
    }

    /// A payload the caller cannot supply names itself, and the model it was
    /// meant for is untouched — the new state is built whole before anything
    /// is swapped.
    #[test]
    fn a_payload_the_caller_lacks_names_itself_and_changes_nothing() {
        let server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let (was, generation) = (replica.to_clm_bytes().unwrap(), replica.generation());

        let err = replica
            .replace_structure(&server.to_structure_bytes().unwrap(), |_| None)
            .unwrap_err();

        let wanted = server.texture_ids()[0].to_string();
        assert!(
            matches!(&err, ModelError::InvalidClm(ClmLoadError::MissingTexture { id }) if *id == wanted),
            "got {err:?}",
        );
        assert_eq!(replica.to_clm_bytes().unwrap(), was);
        assert_eq!(
            replica.generation(),
            generation,
            "a refused push is not an edit"
        );
    }

    /// The two halves derived objects read: the identity says "the same
    /// model", the generation says "a new state of it".
    #[test]
    fn replacing_keeps_the_identity_and_moves_the_generation() {
        let server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let (identity, generation) = (replica.identity(), replica.generation());
        let held = store(&replica);

        replica
            .replace_structure(&server.to_structure_bytes().unwrap(), |id| {
                held.get(id).cloned()
            })
            .unwrap();
        assert_eq!(replica.identity(), identity);
        assert!(replica.generation() > generation);

        let before = replica.generation();
        replica.replace_from(&server);
        assert_eq!(replica.identity(), identity, "not even from another model");
        assert!(replica.generation() > before);
    }

    /// The in-tab handoff: an editor hands its model over and the replica
    /// shares the payloads rather than copying them.
    #[test]
    fn replace_from_shares_the_payloads_it_is_handed() {
        let server = sample();
        let mut replica = Model::new();
        replica.replace_from(&server);

        assert_eq!(
            replica.to_clm_bytes().unwrap(),
            server.to_clm_bytes().unwrap()
        );
        for id in server.texture_ids() {
            assert!(Arc::ptr_eq(
                &replica.texture(id).unwrap().data,
                &server.texture(id).unwrap().data,
            ));
        }
    }

    /// A puppet's gate reads a replaced model as "the same model moved", so
    /// its rebake carries what a rebake is documented to carry: the pose, and
    /// every `SimplePhysics` runtime field.
    #[test]
    fn a_puppet_carries_its_pose_and_its_drivers_across_a_replace() {
        let mut server = sample();
        let mut replica = Model::from_clm_bytes(&server.to_clm_bytes().unwrap()).unwrap();
        let held = store(&replica);

        let param = replica.param_ids()[0].clone();
        let mut puppet = crate::Puppet::new(&replica);
        puppet.set_param_value(&param, 0.75);
        for _ in 0..30 {
            puppet.tick(&replica, 1.0 / 60.0);
        }
        let posed = puppet.param_value(&param).unwrap();
        let swung = physics_state(&puppet);
        assert!(swung.anchor_initialized, "the pendulum has run");

        // The server edits, and only the structure crosses.
        let root = server.root().unwrap().clone();
        server
            .add_node(
                &root,
                ModelNode::new("Added", ModelNodeKind::Group),
                &mut SeededHex::new(77),
            )
            .unwrap();
        replica
            .replace_structure(&server.to_structure_bytes().unwrap(), |id| {
                held.get(id).cloned()
            })
            .unwrap();

        puppet.sync(&replica);
        assert_eq!(
            puppet.baked_generation(),
            replica.generation(),
            "it rebaked"
        );
        assert_eq!(puppet.param_value(&param), Some(posed), "the pose crossed");
        let after = physics_state(&puppet);
        assert_eq!(after.bob, swung.bob);
        assert_eq!(after.d_angle, swung.d_angle);
        assert_eq!(after.spring_vel, swung.spring_vel);
        assert_eq!(after.anchor, swung.anchor);
        assert!(after.anchor_initialized);
    }

    fn physics_state(puppet: &crate::Puppet) -> crate::physics::SimplePhysicsData {
        puppet
            .iter()
            .find_map(|(_, node)| match &node.kind {
                crate::components::NodeKind::SimplePhysics(p) => Some((**p).clone()),
                _ => None,
            })
            .expect("the sample carries a pendulum")
    }
}
