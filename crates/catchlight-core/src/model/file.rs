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
//!   node's mesh cannot take. (It also refuses a weld whose two seams no
//!   longer hold the same slots, which no edit can produce — see
//!   [`Model::slot_add`] — so that a file this writes is always one this
//!   reader takes.) The orders it
//!   writes — nodes pre-order from the root, params, textures and bindings in
//!   the Model's own order — are the Model's, so the same Model always writes
//!   the same bytes.
//! - **Reading trusts nothing.** Every Id in the file is resolved against what
//!   the file itself declares, and a failure names the field and the Id that
//!   dangled ([`ClmLoadError`]). What the reader accepts is exactly what the
//!   Model's own invariants allow — including the two a Model built from a
//!   file used to escape, a colour binding on a mesh group and a deform cell
//!   sized to something other than the node's mesh — so a loaded Model needs
//!   no repair pass.
//!
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::formats::clm::{
    self as clm, ClmBinding, ClmComposite, ClmDocument, ClmFile, ClmMask, ClmMeshGroup, ClmNode,
    ClmNodeKind, ClmParam, ClmPart, ClmSeam, ClmSimplePhysics, ClmSlot, ClmSlotWeight, ClmTexture,
    ClmWeld, ClmWeldEnd,
};
use crate::id::{SeamId, SlotId};
use crate::{charge_clm_structure, LoadBudget};

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
    #[error("node {node:?} names {field} {id:?}, which no node has")]
    DanglingNode {
        node: String,
        field: &'static str,
        id: String,
    },
    #[error("node {node:?} names texture {id:?}, which the file does not carry")]
    DanglingTexture { node: String, id: String },
    #[error("a binding names node {id:?}, which no node has")]
    DanglingBindingNode { id: String },
    #[error("{owner} names param {id:?}, which the file does not carry")]
    DanglingParam { owner: String, id: String },
    #[error(
        "the binding on node {node:?} names {got} params; a binding names one or two distinct ones"
    )]
    BindingParamCount { node: String, got: usize },
    #[error("part {node:?} carries two seams named {seam:?}")]
    DuplicateSeam { node: String, seam: String },
    #[error("seam {seam:?} on part {node:?} fills slot {slot:?} twice")]
    DuplicateSlot {
        node: String,
        seam: String,
        slot: String,
    },
    #[error(
        "seam {seam:?} on part {node:?} fills slot {slot:?} with vertex {vertex}, past the mesh's \
         {vertices}"
    )]
    SlotOutOfRange {
        node: String,
        seam: String,
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
        "the deform binding on node {node:?} holds {got} offsets at cell {cell:?}, but the node's \
         mesh takes {expected}"
    )]
    DeformCellShape {
        node: String,
        cell: [u32; 2],
        got: usize,
        expected: usize,
    },
    #[error("a weld names seam {seam:?} on node {node:?}, which carries no such seam")]
    UnknownSeam { node: String, seam: String },
    #[error(
        "the weld between {a:?} and {b:?} does not weight each of the two seams' slots exactly once"
    )]
    WeldSlotMismatch { a: String, b: String },
}

/// The slot Ids each of one part's seams holds — what a weld's two ends have
/// to agree about.
type SeamTable = HashMap<SeamId, HashSet<SlotId>>;

impl Model {
    /// Snapshot the model into a `.clm` document. Total for any Model whose
    /// deform cells are sized to the meshes they sit on.
    pub fn to_clm_file(&self) -> Result<ClmFile, ModelError> {
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

        let mut welds = Vec::with_capacity(self.welds.len());
        for weld in &self.welds {
            // Unreachable through the Model's own methods, which keep two
            // welded seams' slot sets equal; checked because the reader
            // refuses the file if they ever diverge.
            let (Some(a), Some(b)) = (
                self.seam(&weld.a().0, &weld.a().1),
                self.seam(&weld.b().0, &weld.b().1),
            ) else {
                return Err(ModelError::UnknownSeam);
            };
            if a.slots().len() != b.slots().len()
                || weld.weights().len() != a.slots().len()
                || !a.slots().iter().all(|s| b.slot(s.id()).is_some())
            {
                return Err(ModelError::WeldSlotMismatch);
            }
            welds.push(ClmWeld {
                a: ClmWeldEnd {
                    node: weld.a().0.clone(),
                    seam: weld.a().1.clone(),
                },
                b: ClmWeldEnd {
                    node: weld.b().0.clone(),
                    seam: weld.b().1.clone(),
                },
                weights: weld
                    .weights()
                    .iter()
                    .map(|(slot, weight)| ClmSlotWeight {
                        slot: slot.clone(),
                        weight: *weight,
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

        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(ClmTexture {
                id: id.clone(),
                encoding: t.encoding,
                alpha: t.alpha,
                data: (*t.data).clone(),
            });
        }

        Ok(ClmFile {
            doc: ClmDocument {
                physics: self.physics,
                nodes,
                params,
                bindings,
                welds,
                animations: self.animations.clone(),
            },
            textures,
        })
    }

    /// [`Self::to_clm_file`] then encode.
    pub fn to_clm_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let file = self.to_clm_file()?;
        Ok(clm::encode(&file.doc, &file.textures)?)
    }

    pub fn from_clm_file(file: &ClmFile) -> Result<Model, ModelError> {
        Self::from_clm_file_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clm_file_with_budget(
        file: &ClmFile,
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        charge_clm_structure(file, budget)?;
        let doc = &file.doc;

        let mut textures = HashMap::with_capacity(file.textures.len());
        let mut texture_order = Vec::with_capacity(file.textures.len());
        for t in &file.textures {
            if textures
                .insert(
                    t.id.clone(),
                    ModelTexture {
                        encoding: t.encoding,
                        alpha: t.alpha,
                        data: Arc::new(t.data.clone()),
                    },
                )
                .is_some()
            {
                return Err(duplicate("texture", &t.id));
            }
            texture_order.push(t.id.clone());
        }

        let mut params = HashMap::with_capacity(doc.params.len());
        let mut param_order = Vec::with_capacity(doc.params.len());
        for p in &doc.params {
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
        let mut seams: HashMap<&NodeId, SeamTable> = HashMap::new();
        let mut root: Option<&NodeId> = None;
        for cn in &doc.nodes {
            match &cn.parent {
                Some(parent) => {
                    if !declared.contains(parent) {
                        return Err(ClmLoadError::DanglingParent {
                            node: cn.id.to_string(),
                            parent: parent.to_string(),
                        }
                        .into());
                    }
                    if !nodes.contains_key(parent) {
                        return Err(ClmLoadError::NotTopological {
                            node: cn.id.to_string(),
                            parent: parent.to_string(),
                        }
                        .into());
                    }
                }
                None => match root {
                    Some(first) => {
                        return Err(ClmLoadError::MultipleRoots {
                            root: first.to_string(),
                            node: cn.id.to_string(),
                        }
                        .into())
                    }
                    None => root = Some(&cn.id),
                },
            }

            let node_seams = match &cn.kind {
                ClmNodeKind::Part(part) => {
                    let read = read_seams(&cn.id, part)?;
                    seams.insert(
                        &cn.id,
                        read.iter()
                            .map(|s| {
                                (
                                    s.id().clone(),
                                    s.slots().iter().map(|sl| sl.id().clone()).collect(),
                                )
                            })
                            .collect(),
                    );
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
            node.kind = model_kind(&cn.id, &cn.kind, node_seams, &declared, &textures, &params)?;
            nodes.insert(cn.id.clone(), node);
        }
        let root = root.ok_or(ClmLoadError::NoRoot)?.clone();

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
                if !params.contains_key(p) {
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
            let vertices = nodes
                .get(&b.node)
                .and_then(ModelNode::mesh)
                .map_or(0, |m| m.verts.len());
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
        for w in &doc.welds {
            welds.push(read_weld(w, &seams)?);
        }

        for (i, animation) in doc.animations.iter().enumerate() {
            for lane in &animation.lanes {
                if !params.contains_key(&lane.param) {
                    return Err(ClmLoadError::DanglingParam {
                        owner: format!("a lane of animation {i}"),
                        id: lane.param.to_string(),
                    }
                    .into());
                }
            }
        }

        Ok(Model {
            generation: 0,
            physics: doc.physics,
            welds,
            nodes,
            root,
            params,
            param_order,
            textures,
            texture_order,
            bindings,
            animations: doc.animations.clone(),
        })
    }

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
}

/// A deform cell holds one `[dx, dy]` per mesh vertex, so its length is the
/// node's flat vertex-array length and nothing else. The legacy runtime sized
/// its grid from the *longest* authored cell and a [`Model`] sizes it from the
/// mesh, so a file where the two disagree evaluates differently depending on
/// which runtime reads it — refuse it rather than pick a winner.
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
            seams: p
                .seams()
                .iter()
                .map(|s| ClmSeam {
                    id: s.id().clone(),
                    slots: s
                        .slots()
                        .iter()
                        .map(|slot| ClmSlot {
                            id: slot.id().clone(),
                            vertex: slot.vertex(),
                        })
                        .collect(),
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
            dynamic: mg.dynamic,
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
    seams: Vec<Seam>,
    declared: &HashSet<&NodeId>,
    textures: &HashMap<TexId, ModelTexture>,
    params: &HashMap<ParamId, ModelParam>,
) -> Result<ModelNodeKind, ModelError> {
    Ok(match kind {
        ClmNodeKind::Group => ModelNodeKind::Group,
        ClmNodeKind::Part(p) => {
            let mut part = ModelPart::new(p.mesh.clone());
            if let Some(albedo) = &p.albedo {
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
            part.masks = model_masks(id, &p.masks, declared)?;
            part.mask_threshold = p.mask_threshold;
            part.seams = seams;
            ModelNodeKind::Part(part)
        }
        ClmNodeKind::Composite(c) => {
            let mut composite = ModelComposite::new();
            composite.opacity = c.opacity;
            composite.blend_mode = c.blend_mode;
            composite.tint = c.tint;
            composite.screen_tint = c.screen_tint;
            composite.masks = model_masks(id, &c.masks, declared)?;
            composite.mask_threshold = c.mask_threshold;
            composite.propagate_meshgroup = c.propagate_meshgroup;
            ModelNodeKind::Composite(composite)
        }
        ClmNodeKind::MeshGroup(mg) => {
            let mut group = ModelMeshGroup::new(mg.mesh.clone());
            group.dynamic = mg.dynamic;
            group.translate_children = mg.translate_children;
            ModelNodeKind::MeshGroup(group)
        }
        ClmNodeKind::SimplePhysics(ph) => {
            let mut physics = ModelPhysics::new(ph.kind);
            physics.map_mode = ph.map_mode;
            physics.local_only = ph.local_only;
            for target in ph.target_params.iter().flatten() {
                if !params.contains_key(target) {
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
) -> Result<Vec<ModelMask>, ModelError> {
    masks
        .iter()
        .map(|m| {
            if !declared.contains(&m.source) {
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

/// One part's seams, checked: no repeated seam or slot Id, and every *filled*
/// slot naming a vertex the part's mesh actually has. An unfilled slot is a
/// slot whose part was re-meshed since it was filled; it carries no index to
/// check.
fn read_seams(id: &NodeId, part: &ClmPart) -> Result<Vec<Seam>, ModelError> {
    let vertices = part.mesh.vertex_count();
    let mut out: Vec<Seam> = Vec::with_capacity(part.seams.len());
    for seam in &part.seams {
        if out.iter().any(|s| s.id() == &seam.id) {
            return Err(ClmLoadError::DuplicateSeam {
                node: id.to_string(),
                seam: seam.id.to_string(),
            }
            .into());
        }
        let mut slots: Vec<Slot> = Vec::with_capacity(seam.slots.len());
        for slot in &seam.slots {
            if slot.vertex.is_some_and(|v| v as usize >= vertices) {
                return Err(ClmLoadError::SlotOutOfRange {
                    node: id.to_string(),
                    seam: seam.id.to_string(),
                    slot: slot.id.to_string(),
                    vertex: slot.vertex.unwrap_or_default(),
                    vertices,
                }
                .into());
            }
            if slots.iter().any(|s| s.id() == &slot.id) {
                return Err(ClmLoadError::DuplicateSlot {
                    node: id.to_string(),
                    seam: seam.id.to_string(),
                    slot: slot.id.to_string(),
                }
                .into());
            }
            slots.push(Slot {
                id: slot.id.clone(),
                vertex: slot.vertex,
            });
        }
        out.push(Seam {
            id: seam.id.clone(),
            slots,
        });
    }
    Ok(out)
}

/// A weld, checked against the seams the file declares: both ends have to
/// name a seam on a part, the two seams have to hold the same slots, and the
/// weights have to name each of those slots exactly once — anything else
/// leaves a slot pair with no weight, which the solve has no answer for.
/// Whether a slot is *filled* is not this check's business:
/// [`ModelWeld::resolve`] skips the ones that are not.
fn read_weld(weld: &ClmWeld, seams: &HashMap<&NodeId, SeamTable>) -> Result<ModelWeld, ModelError> {
    let slots_of = |end: &ClmWeldEnd| {
        seams
            .get(&end.node)
            .and_then(|t| t.get(&end.seam))
            .ok_or_else(|| ClmLoadError::UnknownSeam {
                node: end.node.to_string(),
                seam: end.seam.to_string(),
            })
    };
    let a_slots = slots_of(&weld.a)?;
    let b_slots = slots_of(&weld.b)?;
    let mismatch = || ClmLoadError::WeldSlotMismatch {
        a: weld.a.seam.to_string(),
        b: weld.b.seam.to_string(),
    };
    if a_slots.len() != b_slots.len() || weld.weights.len() != a_slots.len() {
        return Err(mismatch().into());
    }

    let mut seen = HashSet::with_capacity(weld.weights.len());
    let mut weights = Vec::with_capacity(weld.weights.len());
    for weight in &weld.weights {
        if !a_slots.contains(&weight.slot)
            || !b_slots.contains(&weight.slot)
            || !seen.insert(&weight.slot)
        {
            return Err(mismatch().into());
        }
        weights.push((weight.slot.clone(), weight.weight));
    }
    Ok(ModelWeld::new(
        (weld.a.node.clone(), weld.a.seam.clone()),
        (weld.b.node.clone(), weld.b.seam.clone()),
        weights,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{BlendMode, MaskMode};
    use crate::formats::clm::{
        ClmAnimation, ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmKeyframe, ClmLane,
        ClmMesh, TextureAlpha, TextureEncoding,
    };
    use crate::id::SeededHex;
    use crate::params::InterpolateMode;
    use crate::physics::PendulumKind;

    fn seam(id: &str) -> SeamId {
        SeamId::new(id).unwrap()
    }

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
        let root = m.root().clone();

        let tex = m
            .add_texture(
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: Arc::new(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4]),
                },
                &mut hex,
            )
            .unwrap();

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
        m.set_part_albedo(&upper, Some(tex)).unwrap();

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
                "Warp",
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

        let (collar, hem) = (seam("collar"), seam("hem"));
        let (left, right) = (slot("left"), slot("right"));
        for (node, s, verts) in [(&upper, &collar, [0, 3]), (&lower, &hem, [1, 2])] {
            m.seam_add(node, s.clone()).unwrap();
            for (slot, vertex) in [(&left, verts[0]), (&right, verts[1])] {
                m.slot_add(node, s, slot.clone()).unwrap();
                m.slot_fill(node, s, slot, vertex).unwrap();
            }
        }
        m.set_welds(vec![ModelWeld::new(
            (upper, collar),
            (lower, hem),
            vec![(left, 1.0), (right, 0.25)],
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

    /// A part's seams travel with the part and a weld travels as the two
    /// seams it names — the file is a copy of the model, not a translation of
    /// it.
    #[test]
    fn a_weld_travels_as_two_seams() {
        let m = sample();
        let file = m.to_clm_file().unwrap();

        let upper = match &node_named(&file, "Upper").kind {
            ClmNodeKind::Part(p) => p,
            _ => panic!("Upper is a part"),
        };
        assert_eq!(upper.seams.len(), 1);
        assert_eq!(upper.seams[0].id.as_str(), "collar");
        assert_eq!(
            upper.seams[0]
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
        assert_eq!(weld.a.seam.as_str(), "collar");
        assert_eq!(weld.b.seam.as_str(), "hem");
        assert_eq!(
            weld.weights
                .iter()
                .map(|w| (w.slot.to_string(), w.weight))
                .collect::<Vec<_>>(),
            vec![("left".to_string(), 1.0), ("right".to_string(), 0.25)],
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
        m.slot_clear(&upper, &seam("collar"), &slot("right"))
            .unwrap();

        let bytes = m.to_clm_bytes().unwrap();
        let reopened = Model::from_clm_bytes(&bytes).unwrap();

        let [weld] = reopened.welds() else {
            panic!("one weld")
        };
        assert_eq!(weld.weights().len(), 2, "the slot is still welded");
        assert_eq!(
            weld.resolve(&reopened),
            vec![ModelWeldPair {
                a_vert: 0,
                b_vert: 1,
                weight: 1.0,
            }],
            "only the filled pair solves",
        );
        assert_eq!(
            reopened.unfilled_slots(),
            vec![(upper, seam("collar"), slot("right"))],
        );
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
        let (face, warp) = (index("Face"), index("Warp"));
        file.doc.nodes.swap(face, warp);

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

    /// `Model::add_binding` refuses a colour target on a mesh group and the
    /// legacy loader refuses a file carrying one, but a Model read from a file
    /// used to accept it and the fold then dropped it silently. The reader is
    /// where that hole closes.
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
            let warp = node_named(&file, "Warp").id.clone();
            file.doc.bindings[0].node = warp.clone();
            file.doc.bindings[0].values = values;

            assert_eq!(
                load_err(&file),
                ClmLoadError::ColorOnMeshGroup {
                    node: warp.to_string(),
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
        let warp = node_named(&file, "Warp").id.clone();
        file.doc.bindings[0].node = warp;
        file.doc.bindings[0].values = ClmBindingValues::Deform(ClmCells::default());

        assert!(Model::from_clm_file(&file).is_ok());
    }

    /// The legacy runtime sized a deform grid from the longest authored cell
    /// and a Model sizes it from the mesh, so a file where a cell and the mesh
    /// disagree evaluates differently depending on who reads it.
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
    fn a_seam_slot_past_the_mesh_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        part_named(&mut file, "Upper").seams[0].slots[1].vertex = Some(99);

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
    fn two_seams_with_one_id_are_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let seams = &mut part_named(&mut file, "Upper").seams;
        let twin = seams[0].clone();
        seams.push(twin);

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DuplicateSeam { seam, .. } if seam == "collar"
        ));
    }

    #[test]
    fn a_seam_filling_one_slot_twice_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let slots = &mut part_named(&mut file, "Upper").seams[0].slots;
        slots[1].id = slots[0].id.clone();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::DuplicateSlot { slot, .. } if slot == "left"
        ));
    }

    #[test]
    fn a_weld_naming_a_seam_the_part_lacks_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].a.seam = SeamId::new("ghost").unwrap();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::UnknownSeam { seam, .. } if seam == "ghost"
        ));
    }

    /// The two ends have to agree slot for slot, or a pair of vertices would
    /// come back with no partner and no weight.
    #[test]
    fn a_weld_whose_ends_disagree_about_slots_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        part_named(&mut file, "Lower").seams[0].slots.pop();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldSlotMismatch { .. }
        ));

        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].weights[1].slot = file.doc.welds[0].weights[0].slot.clone();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldSlotMismatch { .. }
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
}
