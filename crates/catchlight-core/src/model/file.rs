//! The file edge: a [`Model`] to and from `.clm` bytes.
//!
//! Writing is total and deterministic. Reading trusts nothing: every check is
//! the file-shaped half of a rule an edit already obeys, so a loaded Model
//! needs no repair pass. A complete model and an addon fragment are disjoint
//! on the wire (a fragment never has a parentless node), and the caller says
//! which it is reading.

use std::collections::{HashMap, HashSet};

use crate::extension::{decode_registered, Payload, Stored};
use crate::formats::clm::ClmPayload;
use crate::formats::clm::{
    self as clm, ClmBinding, ClmComposite, ClmExtension, ClmExtensionBlob, ClmExtensionMarker,
    ClmFile, ClmMask, ClmMeshGroup, ClmNode, ClmNodeKind, ClmParam, ClmPart, ClmSlot, ClmSlotPair,
    ClmSpine, ClmStructure, ClmTexture, ClmTextureRef, ClmWeld,
};
use crate::id::SlotId;
use crate::load_budget::{
    charge_clm_file, charge_clm_structure, charge_decoded_clm, charge_texture_payloads, LoadBudget,
};

use super::*;

/// Every variant names the Id that broke and the field that named it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
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
        "node {node:?} has no parent, so this file is a complete model; a fragment's roots name \
         the base node they hang from"
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
    #[error("binding on node {node:?} must carry one nonempty, finite, strictly increasing axis in 0..=1 per param, within the binding-grid budget")]
    BindingPositions { node: String },
    #[error("binding on node {node:?} has invalid authored cell {cell:?}: {reason}")]
    BindingCell {
        node: String,
        cell: [u32; 2],
        reason: &'static str,
    },
    #[error(
        "node {node:?} carries two {target} bindings over the same params; a binding is \
         addressed by its params, node and target, so a model holds one of each"
    )]
    DuplicateBinding { node: String, target: &'static str },
    #[error("part {node:?} carries two vertex slots with the Id {slot:?}")]
    DuplicateSlot { node: String, slot: String },
    #[error(
        "part {node:?} fills vertex slot {slot:?} with vertex {vertex}, past the mesh's {vertices}"
    )]
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
    #[error("a weld pairs vertex slot {slot:?} on node {node:?}, which carries no such slot")]
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
    #[error("param {param:?} has a default that is not finite")]
    ParamDefault { param: String },
    #[error("node {node:?} has a {field} that is not finite")]
    NodeNotFinite { node: String, field: &'static str },
    #[error("spine {node:?} carries no joints; a spine is at least one link")]
    SpineNoJoints { node: String },
    #[error("spine {node:?} joint {joint}: a coordinate is not finite")]
    SpineJointField { node: String, joint: usize },
    #[error(
        "spine {node:?} joint {joint} repeats the point above it; a link needs a length to turn \
         about"
    )]
    SpineJointCoincident { node: String, joint: usize },
    #[error(
        "spine {node:?} names {targets} targets for {joints} joints; a spine reads one param per \
         link"
    )]
    SpineTargetCount {
        node: String,
        targets: usize,
        joints: usize,
    },
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
    #[error("node {node:?} names itself as a mask source; a node cannot mask itself")]
    SelfMask { node: String },
    #[error("the mesh on node {node:?} is malformed: {reason}")]
    MalformedMesh { node: String, reason: &'static str },
    #[error("the weld between {a:?} and {b:?} pairs vertex slot {slot:?} twice")]
    WeldSlotPairedTwice { a: String, b: String, slot: String },
    #[error(
        "the weld between {a:?} and {b:?} weights slot {slot:?} outside 0..=1; a weight is a \
         share of a meeting point"
    )]
    WeldWeightOutOfRange { a: String, b: String, slot: String },
    #[error(
        "the structure names texture {id:?}, which the caller could not supply; fetch it before \
         applying the structure"
    )]
    MissingTexture { id: String },
}

type SlotTable = HashSet<SlotId>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Base,
    Fragment,
}

impl Shape {
    /// A binding's *node* never may dangle, in either shape.
    fn allows_dangling(self) -> bool {
        matches!(self, Self::Fragment)
    }
}

/// A byte payload that disagrees with its marker is refused, not trusted. A
/// maintained extension whose key is registered is decoded now; one that does
/// not decode is kept as stored, never refused. A value equal to the one
/// `previous` holds under its key reuses that slot, decoded data and all.
fn extensions_of(
    doc: &ClmStructure,
    bytes: impl Fn(&ExtensionKey) -> Option<Arc<[u8]>>,
    previous: Option<&Extensions>,
) -> Result<Extensions, ModelError> {
    let checked_bytes = |key: &ExtensionKey, marker: &ClmExtensionMarker| {
        let data = bytes(key).ok_or_else(|| clm::ClmError::ExtensionBytesMissing {
            key: key.to_string(),
        })?;
        if data.len() as u64 != marker.size {
            return Err(clm::ClmError::ExtensionSizeMismatch {
                key: key.to_string(),
                expected: marker.size,
                actual: data.len() as u64,
            });
        }
        if clm::extension_hash(&data) != marker.hash {
            return Err(clm::ClmError::ExtensionHashMismatch {
                key: key.to_string(),
            });
        }
        Ok(data)
    };
    let mut out = Extensions::default();
    for (key, value) in &doc.extensions {
        let value = match value {
            ClmExtension::Json(json) => ExtensionValue::Json(json.clone()),
            ClmExtension::Bytes(marker) => ExtensionValue::Bytes(checked_bytes(key, marker)?),
            ClmExtension::Maintained {
                version,
                refs,
                payload,
            } => ExtensionValue::Maintained(RawExtension {
                version: *version,
                refs: refs.clone(),
                payload: match payload {
                    ClmPayload::Json(json) => Payload::Json(json.clone()),
                    ClmPayload::Bytes(marker) => Payload::Bytes(checked_bytes(key, marker)?),
                },
            }),
        };
        if let Some(stored) = previous
            .and_then(|previous| previous.map.get(key))
            .filter(|stored| stored.as_value() == &value)
        {
            out.map.insert(key.clone(), stored.clone());
            continue;
        }
        let stored = match &value {
            ExtensionValue::Maintained(raw) => match decode_registered(key.as_ref(), raw) {
                Some(Ok(typed)) => Stored::Typed(typed),
                _ => Stored::value(value),
            },
            _ => Stored::value(value),
        };
        out.map.insert(key.clone(), stored);
    }
    Ok(out)
}

fn clm_extension(value: &ExtensionValue) -> ClmExtension {
    let marker = |data: &[u8]| ClmExtensionMarker {
        size: data.len() as u64,
        hash: clm::extension_hash(data),
    };
    match value {
        ExtensionValue::Json(json) => ClmExtension::Json(json.clone()),
        ExtensionValue::Bytes(data) => ClmExtension::Bytes(marker(data)),
        ExtensionValue::Maintained(raw) => ClmExtension::Maintained {
            version: raw.version,
            refs: raw.refs.clone(),
            payload: match &raw.payload {
                Payload::Json(json) => ClmPayload::Json(json.clone()),
                Payload::Bytes(data) => ClmPayload::Bytes(marker(data)),
            },
        },
    }
}

impl Model {
    /// Everything [`Self::to_clm_structure`] refuses, without building it.
    /// No edit produces either case; both stay checked so this writer's output
    /// is always readable.
    pub(super) fn check_writable(&self) -> Result<(), ModelError> {
        let fragment = self.is_fragment();
        for weld in &self.welds {
            // A fragment's weld into a base part has one end this model
            // cannot see, and only the end it can see is checked.
            for end in [weld.a(), weld.b()] {
                match validate::WeldEnd::of(self.node(end).map(|n| &n.kind), fragment) {
                    validate::WeldEnd::Part | validate::WeldEnd::Deferred => {}
                    validate::WeldEnd::NotAPart(_) => return Err(ModelError::WeldEndNotAPart),
                    validate::WeldEnd::Missing => return Err(ModelError::UnknownNode),
                }
            }
        }
        for b in &self.bindings {
            check_deform_cells(&b.key.node, self.deform_len(&b.key.node), &b.values)?;
        }
        Ok(())
    }

    /// Total for any Model whose deform cells are sized to their meshes.
    pub fn to_clm_structure(&self) -> Result<ClmStructure, ModelError> {
        self.check_writable()?;
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
            });
        }

        let mut bindings = Vec::with_capacity(self.bindings.len());
        for b in &self.bindings {
            bindings.push(ClmBinding {
                params: b.key.params.iter().cloned().collect(),
                key_positions: b.key_positions.clone(),
                node: b.key.node.clone(),
                interpolate_mode: b.interpolate_mode(),
                values: b.values.to_clm(),
            });
        }

        Ok(ClmStructure {
            nodes,
            params,
            bindings,
            welds,
            extensions: self
                .extensions
                .iter()
                .map(|(key, value)| (key.clone(), clm_extension(value)))
                .collect(),
        })
    }

    /// Copies payloads out of their `Arc`s; [`Self::to_clm_bytes`] copies once.
    pub fn to_clm_file(&self) -> Result<ClmFile, ModelError> {
        let doc = self.to_clm_structure()?;
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
        Ok(ClmFile {
            doc,
            textures,
            extensions: self.extension_blobs(),
            unknown_sections: self.unknown_sections.clone(),
        })
    }

    fn extension_blobs(&self) -> Vec<ClmExtensionBlob> {
        self.extensions
            .iter()
            .filter_map(|(key, value)| {
                Some(ClmExtensionBlob {
                    key: key.clone(),
                    data: value.bytes()?.to_vec(),
                })
            })
            .collect()
    }

    /// Serializes payloads from where the model holds them, copying each once.
    pub fn to_clm_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let doc = self.to_clm_structure()?;
        let mut textures = Vec::with_capacity(self.texture_order.len());
        for id in &self.texture_order {
            let t = self.texture(id).ok_or(ModelError::UnknownTexture)?;
            textures.push(clm::ClmTextureBytes {
                id,
                encoding: t.encoding,
                alpha: t.alpha,
                data: &t.data,
            });
        }
        Ok(clm::encode_borrowed(
            &doc,
            &textures,
            &self.extension_blobs(),
            &self.unknown_sections,
        )?)
    }

    /// A structure-only container: never touches a texture payload.
    pub fn to_structure_bytes(&self) -> Result<Vec<u8>, ModelError> {
        let doc = self.to_clm_structure()?;
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

    pub fn from_clm_file(file: &ClmFile) -> Result<Model, ModelError> {
        Self::from_clm_file_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clm_file_with_budget(
        file: &ClmFile,
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        Self::read(file, budget, Shape::Base)
    }

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
        charge_clm_file(file, budget)?;
        let textures = file.textures.iter().map(|t| {
            let texture = ModelTexture {
                encoding: t.encoding,
                alpha: t.alpha,
                data: t.data.as_slice().into(),
            };
            (t.id.clone(), texture)
        });
        let mut model = Self::assemble(&file.doc, textures, &file.extensions, shape)?;
        model.unknown_sections = file.unknown_sections.clone();
        Ok(model)
    }

    /// Decoding already charged the payloads, so only the texture count and
    /// the structure are charged here; charging again counts every byte twice.
    fn read_bytes(
        bytes: &[u8],
        budget: &mut LoadBudget,
        shape: Shape,
    ) -> Result<Model, ModelError> {
        let file = clm::decode_borrowed(bytes, budget)?;
        charge_decoded_clm(&file.doc, file.textures.len(), budget)?;
        let textures = file.textures.into_iter().map(|t| {
            let texture = ModelTexture {
                encoding: t.encoding,
                alpha: t.alpha,
                data: Arc::from(&*t.data),
            };
            (t.id, texture)
        });
        let mut model = Self::assemble(&file.doc, textures, &file.extensions, shape)?;
        model.unknown_sections = file.unknown_sections;
        Ok(model)
    }

    /// Charges nothing: its two callers charge different things.
    fn assemble(
        doc: &ClmStructure,
        table: impl ExactSizeIterator<Item = (TexId, ModelTexture)>,
        blobs: &[ClmExtensionBlob],
        shape: Shape,
    ) -> Result<Model, ModelError> {
        let mut textures = HashMap::with_capacity(table.len());
        let mut texture_order = Vec::with_capacity(table.len());
        for (id, texture) in table {
            if textures.insert(id.clone(), texture).is_some() {
                return Err(duplicate("texture", &id));
            }
            texture_order.push(id);
        }

        // A hand-built `ClmFile` skips decoding, so the pairing is checked here.
        clm::check_extensions(doc, blobs)?;
        let blobs: HashMap<&ExtensionKey, &[u8]> = blobs
            .iter()
            .map(|blob| (&blob.key, blob.data.as_slice()))
            .collect();
        let extensions = extensions_of(doc, |key| blobs.get(key).map(|d| Arc::from(*d)), None)?;
        Self::build(doc, textures, texture_order, extensions, shape)
    }

    /// The one path both a `.clm` and a structure push come through, so
    /// neither can accept what the other refuses. Charges nothing.
    fn build(
        doc: &ClmStructure,
        textures: HashMap<TexId, ModelTexture>,
        texture_order: Vec<TexId>,
        extensions: Extensions,
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
            if !p.default.is_finite() {
                return Err(ClmLoadError::ParamDefault {
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
                    },
                )
                .is_some()
            {
                return Err(duplicate("param", &p.id));
            }
            param_order.push(p.id.clone());
        }

        // Pre-scan so a parent written too late reads differently from one
        // that names nothing, and so a mask may name a later node.
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

            validate::node_placement(cn.z_order, &cn.transform).map_err(|fault| {
                ClmLoadError::NodeNotFinite {
                    node: cn.id.to_string(),
                    field: fault.field(),
                }
            })?;
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

        // Walked in tree order so a file with two bad masks names the first.
        // A source the file does not carry is checked by `Model::install`.
        for cn in &doc.nodes {
            for mask in clm_masks_of(&cn.kind) {
                if mask.source == cn.id {
                    return Err(ClmLoadError::SelfMask {
                        node: cn.id.to_string(),
                    }
                    .into());
                }
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

        // Spare textures are refused in both shapes. Walked in file order so
        // the first spare is named.
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

        for cn in &doc.nodes {
            if let Some(parent) = &cn.parent {
                if let Some(pn) = nodes.get_mut(parent) {
                    Arc::make_mut(&mut pn.children).push(cn.id.clone());
                }
            }
        }

        let mut bindings = Vec::with_capacity(doc.bindings.len());
        let mut bound_keys: HashSet<BindingKey> = HashSet::with_capacity(doc.bindings.len());
        for b in &doc.bindings {
            let Some(bound) = nodes.get(&b.node) else {
                return Err(ClmLoadError::DanglingBindingNode {
                    id: b.node.to_string(),
                }
                .into());
            };
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
            // Otherwise a meshless deform slips past `check_deform_cells`,
            // whose expected length for it is zero.
            match validate::binding_target(&bound.kind, target) {
                Ok(()) => {}
                Err(validate::BindingFault::ColorOnMeshGroup(t)) => {
                    return Err(ClmLoadError::ColorOnMeshGroup {
                        node: b.node.to_string(),
                        target: t.name(),
                    }
                    .into())
                }
                Err(validate::BindingFault::DeformOnUnmeshed) => {
                    return Err(ClmLoadError::DeformOnUnmeshed {
                        node: b.node.to_string(),
                        kind: bound.kind.name(),
                    }
                    .into())
                }
            }
            let vertices = bound.mesh().map_or(0, |m| m.verts.len());
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
            if !validate::binding_positions_are_valid(b.params.len(), &b.key_positions) {
                return Err(ClmLoadError::BindingPositions {
                    node: b.node.to_string(),
                }
                .into());
            }
            let width = b.key_positions[0].len();
            let height = b.key_positions.get(1).map_or(1, Vec::len);
            let mut occupied = HashSet::new();
            let mut check = |x: u32, y: u32, finite: bool| -> Result<(), ModelError> {
                let reason = if x as usize >= width || y as usize >= height {
                    Some("outside the binding grid")
                } else if !occupied.insert((x, y)) {
                    Some("duplicate coordinates")
                } else if !finite {
                    Some("non-finite value")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(ClmLoadError::BindingCell {
                        node: b.node.to_string(),
                        cell: [x, y],
                        reason,
                    }
                    .into());
                }
                Ok(())
            };
            match &b.values {
                ClmBindingValues::Deform(cells) => {
                    for cell in &cells.cells {
                        check(cell.x, cell.y, cell.value.iter().all(|v| v.is_finite()))?;
                    }
                }
                values => {
                    for cell in scalar_cells(values).unwrap_or(&[]) {
                        check(cell.x, cell.y, cell.value.is_finite())?;
                    }
                }
            }
            let key = BindingKey {
                params: key_params,
                node: b.node.clone(),
                target,
            };
            // Two under one key would both fold, but lookups and edits would
            // reach only one.
            if !bound_keys.insert(key.clone()) {
                return Err(ClmLoadError::DuplicateBinding {
                    node: b.node.to_string(),
                    target: target.name(),
                }
                .into());
            }
            bindings.push(ModelBinding {
                key_positions: b.key_positions.clone(),
                key,
                interpolate_mode: b.interpolate_mode,
                values: b.values.clone().into(),
                dense: OnceLock::new(),
            });
        }

        let mut welds = Vec::with_capacity(doc.welds.len());
        let mut welded = validate::Welded::with_capacity(doc.welds.len());
        for w in &doc.welds {
            welds.push(read_weld(w, &slots, &nodes, &mut welded, shape)?);
        }

        let model = Model {
            identity: super::next_identity(),
            generation: 0,
            welds,
            response_index: OnceLock::new(),
            response_count: nodes
                .values()
                .filter(|n| matches!(n.kind, ModelNodeKind::MotionResponse(_)))
                .count(),
            nodes,
            roots,
            params,
            param_order,
            textures,
            texture_order,
            bindings,
            extensions,
            // Set by whoever read a whole file; a structure push keeps them.
            unknown_sections: Vec::new(),
            binding_index: OnceLock::new(),
            bake: Default::default(),
        };
        model.validate_responses()?;
        Ok(model)
    }

    pub fn from_clm_bytes(bytes: &[u8]) -> Result<Model, ModelError> {
        Self::from_clm_bytes_with_budget(bytes, &mut LoadBudget::default())
    }

    pub fn from_clm_bytes_with_budget(
        bytes: &[u8],
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        Self::read_bytes(bytes, budget, Shape::Base)
    }

    pub fn from_clm_bytes_fragment(bytes: &[u8]) -> Result<Model, ModelError> {
        Self::from_clm_bytes_fragment_with_budget(bytes, &mut LoadBudget::default())
    }

    pub fn from_clm_bytes_fragment_with_budget(
        bytes: &[u8],
        budget: &mut LoadBudget,
    ) -> Result<Model, ModelError> {
        Self::read_bytes(bytes, budget, Shape::Fragment)
    }

    /// Rebuild from a structure-only container, taking payloads from
    /// `textures`. Returning an `Arc` the client already holds lets a render
    /// cache keep its GPU texture.
    ///
    /// Keeps [`Self::identity`] and bumps [`Self::generation`]. On error the
    /// model is untouched: the new state is built whole before the swap.
    pub fn replace_structure(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
    ) -> Result<(), ModelError> {
        self.replace_structure_with_budget(structure, textures, &mut LoadBudget::default())
    }

    /// [`Self::replace_structure`] with `extension_bytes` supplying byte
    /// extension payloads.
    pub fn replace_structure_with_extensions(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
        extension_bytes: impl Fn(&ExtensionKey) -> Option<Arc<[u8]>>,
    ) -> Result<(), ModelError> {
        self.replace_structure_with_budget_and_extensions(
            structure,
            textures,
            extension_bytes,
            &mut LoadBudget::default(),
        )
    }

    pub fn replace_structure_with_budget(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
        budget: &mut LoadBudget,
    ) -> Result<(), ModelError> {
        self.replace_structure_with_budget_and_extensions(structure, textures, |_| None, budget)
    }

    pub fn replace_structure_with_budget_and_extensions(
        &mut self,
        structure: &[u8],
        textures: impl Fn(&TexId) -> Option<ModelTexture>,
        extension_bytes: impl Fn(&ExtensionKey) -> Option<Arc<[u8]>>,
        budget: &mut LoadBudget,
    ) -> Result<(), ModelError> {
        let structure = clm::decode_structure_with_budget(structure, budget)?;

        let mut table = HashMap::with_capacity(structure.textures.len());
        let mut order = Vec::with_capacity(structure.textures.len());
        for want in &structure.textures {
            let found = textures(&want.id).ok_or_else(|| ClmLoadError::MissingTexture {
                id: want.id.to_string(),
            })?;
            // The manifest wins over the lookup, so replicas fed the same
            // structure hold the same model.
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
        charge_clm_structure(&structure.doc, budget)?;

        let extensions = extensions_of(&structure.doc, extension_bytes, Some(&self.extensions))?;
        let mut next = Self::build(&structure.doc, table, order, extensions, Shape::Base)?;
        next.unknown_sections = std::mem::take(&mut self.unknown_sections);
        self.become_model(next);
        Ok(())
    }

    /// [`Self::replace_structure`] from a `Model` in memory; payloads are
    /// `Arc`-shared.
    pub fn replace_from(&mut self, other: &Model) {
        self.become_model(other.clone());
    }

    fn become_model(&mut self, next: Model) {
        let identity = self.identity;
        *self = next;
        self.identity = identity;
        self.bump();
    }
}

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

fn clm_masks_of(kind: &ClmNodeKind) -> &[ClmMask] {
    match kind {
        ClmNodeKind::Part(p) => &p.masks,
        ClmNodeKind::Composite(c) => &c.masks,
        _ => &[],
    }
}

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
        ModelNodeKind::MotionResponse(r) => ClmNodeKind::MotionResponse(r.clone()),
        ModelNodeKind::Spine(spine) => ClmNodeKind::Spine(ClmSpine {
            joints: spine.joints().to_vec(),
            targets: spine.targets().to_vec(),
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
            part.masks = model_masks(id, &p.masks, declared, shape)?.into();
            part.mask_threshold = p.mask_threshold;
            part.slots = slots.into();
            ModelNodeKind::Part(part)
        }
        ClmNodeKind::Composite(c) => {
            let mut composite = ModelComposite::new();
            composite.opacity = c.opacity;
            composite.blend_mode = c.blend_mode;
            composite.tint = c.tint;
            composite.screen_tint = c.screen_tint;
            composite.masks = model_masks(id, &c.masks, declared, shape)?.into();
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
        ClmNodeKind::MotionResponse(r) => ModelNodeKind::MotionResponse(r.clone()),
        ClmNodeKind::Spine(sp) => ModelNodeKind::Spine(model_spine(id, sp, params, shape)?),
    })
}

/// A repeated joint would make the arc-length assignment divide by zero, and
/// a non-finite coordinate spreads `NaN` into every vertex the spine touches.
fn model_spine(
    id: &NodeId,
    sp: &ClmSpine,
    params: &HashMap<ParamId, ModelParam>,
    shape: Shape,
) -> Result<ModelSpine, ModelError> {
    validate::spine_joints(&sp.joints).map_err(|fault| {
        let node = id.to_string();
        match fault {
            SpineJointError::NoJoints => ClmLoadError::SpineNoJoints { node },
            SpineJointError::NonFinite(joint) => ClmLoadError::SpineJointField { node, joint },
            SpineJointError::Coincident(joint) => {
                ClmLoadError::SpineJointCoincident { node, joint }
            }
        }
    })?;
    let mut spine = ModelSpine::new(sp.joints.clone());
    // An absent key means every link is rigid; the model holds one per link.
    if !sp.targets.is_empty() {
        if sp.targets.len() != sp.joints.len() {
            return Err(ClmLoadError::SpineTargetCount {
                node: id.to_string(),
                targets: sp.targets.len(),
                joints: sp.joints.len(),
            }
            .into());
        }
        for target in sp.targets.iter().flatten() {
            if !params.contains_key(target) && !shape.allows_dangling() {
                return Err(ClmLoadError::DanglingParam {
                    owner: format!("spine {id}"),
                    id: target.to_string(),
                }
                .into());
            }
        }
        spine.targets = sp.targets.clone().into();
    }
    Ok(spine)
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

/// Whether a slot is filled is not checked: [`ModelWeld::resolve`] skips
/// unfilled pairs. A fragment's end into the base is finished by
/// [`Model::install`].
fn read_weld<'a>(
    weld: &'a ClmWeld,
    slots: &HashMap<&NodeId, SlotTable>,
    nodes: &HashMap<NodeId, ModelNode>,
    welded: &mut validate::Welded<'a>,
    shape: Shape,
) -> Result<ModelWeld, ModelError> {
    validate::weld(
        &weld.a,
        &weld.b,
        weld.pairs
            .iter()
            .map(|pair| (&pair.a, &pair.b, pair.weight)),
        welded,
        |end| validate::WeldEnd::of(nodes.get(end).map(|n| &n.kind), shape.allows_dangling()),
        |part, slot| slots.get(part).is_some_and(|known| known.contains(slot)),
    )
    .map_err(|fault| {
        let (a, b) = (weld.a.to_string(), weld.b.to_string());
        match fault {
            validate::WeldFault::SelfPaired => ClmLoadError::WeldSelfPaired { node: a },
            validate::WeldFault::EndNotAPart { end, kind } => ClmLoadError::WeldEndNotAPart {
                node: end.to_string(),
                kind,
            },
            validate::WeldFault::EndMissing { end } => ClmLoadError::DanglingWeldEnd {
                node: end.to_string(),
            },
            validate::WeldFault::Duplicate => ClmLoadError::DuplicateWeld { a, b },
            validate::WeldFault::WeightOutOfRange { slot } => ClmLoadError::WeldWeightOutOfRange {
                a,
                b,
                slot: slot.to_string(),
            },
            validate::WeldFault::UnknownSlot { end, slot } => ClmLoadError::WeldUnknownSlot {
                node: end.to_string(),
                slot: slot.to_string(),
            },
            validate::WeldFault::SlotPairedTwice { end, slot } => {
                ClmLoadError::WeldSlotPairedTwice {
                    a,
                    b,
                    slot: format!("{end}/{slot}"),
                }
            }
        }
    })?;
    let pairs = weld
        .pairs
        .iter()
        .map(|pair| SlotPair {
            a: pair.a.clone(),
            b: pair.b.clone(),
            weight: pair.weight,
        })
        .collect();
    Ok(ModelWeld::new(weld.a.clone(), weld.b.clone(), pairs))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::components::{BlendMode, MaskMode};
    use crate::formats::clm::{
        ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmMesh, TextureAlpha, TextureEncoding,
    };
    use crate::id::SeededHex;

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
        m.update_node_kind(&composite, |kind| {
            if let ModelNodeKind::Composite(c) = kind {
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

        m.add_node(
            &root,
            ModelNode::new("Sway", ModelNodeKind::MotionResponse(Default::default())),
            &mut hex,
        )
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

    /// [`sample`] minus the root, the `Lower` part and the params: an addon
    /// with every kind of dangling reference.
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

    #[test]
    fn a_fragment_round_trips_with_its_dangling_references() {
        let file = sample_fragment_file();
        let bytes = clm::encode(&file.doc, &file.textures, &file.extensions).unwrap();
        let f = Model::from_clm_bytes_fragment(&bytes).unwrap();

        assert!(f.is_fragment());
        assert_eq!(f.root(), None);
        let upper = named(&f, "Upper");
        let face = named(&f, "Face");
        assert_eq!(f.roots(), [upper.clone(), face.clone(), named(&f, "Sway")]);
        for r in f.roots() {
            let parent = f.node(r).unwrap().parent().unwrap();
            assert_eq!(parent.as_str(), "root");
            assert!(f.node(parent).is_none());
        }
        assert!(f.texture(part(&f, &upper).albedo().unwrap()).is_some());
        assert!(f.node(part_mask_source(&f, &face)).is_none());
        assert!(f.node(f.welds()[0].b()).is_none());
        assert_eq!(f.welds()[0].pairs().len(), 2);
        let binding = f.bindings().next().unwrap();
        assert!(binding.params().iter().all(|p| f.param(p).is_none()));

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

    /// Twice over, since `HashMap` order leaking into the writer would only
    /// show up on a second pass.
    #[test]
    fn saving_the_same_model_twice_writes_the_same_bytes() {
        let m = sample();
        let once = m.to_clm_bytes().unwrap();
        assert_eq!(m.to_clm_bytes().unwrap(), once);

        let reopened = Model::from_clm_bytes(&once).unwrap();
        assert_eq!(reopened.to_clm_bytes().unwrap(), once);
        assert_eq!(reopened.to_clm_bytes().unwrap(), once);
    }

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

    #[test]
    fn a_node_masking_itself_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let face = node_named(&file, "Face").id.clone();
        for node in &mut file.doc.nodes {
            if let ClmNodeKind::Composite(c) = &mut node.kind {
                c.masks[0].source = face.clone();
            }
        }
        assert_eq!(
            load_err(&file),
            ClmLoadError::SelfMask {
                node: face.to_string(),
            }
        );

        let mut fragment = sample_fragment_file();
        let upper = node_named(&fragment, "Upper").id.clone();
        part_named(&mut fragment, "Upper").masks.push(ClmMask {
            source: upper.clone(),
            mode: MaskMode::Mask,
        });
        assert_eq!(
            fragment_err(&fragment),
            ClmLoadError::SelfMask {
                node: upper.to_string(),
            }
        );
    }

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

    #[test]
    fn a_deform_binding_on_a_node_with_no_mesh_is_a_structured_error() {
        for (name, kind) in [("Face", "composite"), ("Sway", "motion_response")] {
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
    fn a_malformed_spine_is_a_structured_error() {
        fn spine_file(build: impl FnOnce(&mut ClmSpine)) -> (ClmFile, String) {
            let mut file = sample().to_clm_file().unwrap();
            let param = file.doc.params[0].id.clone();
            let mut spine = ClmSpine {
                joints: vec![[0.0, -50.0], [0.0, -100.0]],
                targets: vec![Some(param), None],
            };
            build(&mut spine);
            let root = file.doc.nodes[0].id.clone();
            let id = NodeId::new("root/spine-00000001").unwrap();
            file.doc.nodes.push(ClmNode {
                id: id.clone(),
                parent: Some(root),
                name: "tail".into(),
                enabled: true,
                z_order: 0.0,
                transform: ClmTransform::default(),
                lock_to_root: false,
                kind: ClmNodeKind::Spine(spine),
            });
            (file, id.to_string())
        }

        let (ok, _) = spine_file(|_| {});
        assert!(Model::from_clm_file(&ok).is_ok(), "the base case loads");

        let (file, node) = spine_file(|sp| sp.joints.clear());
        assert_eq!(load_err(&file), ClmLoadError::SpineNoJoints { node });

        let (file, node) = spine_file(|sp| sp.joints[1][0] = f32::NAN);
        assert_eq!(
            load_err(&file),
            ClmLoadError::SpineJointField { node, joint: 1 }
        );

        let (file, node) = spine_file(|sp| sp.joints[1][1] = f32::INFINITY);
        assert_eq!(
            load_err(&file),
            ClmLoadError::SpineJointField { node, joint: 1 }
        );

        let (file, node) = spine_file(|sp| sp.joints[1] = sp.joints[0]);
        assert_eq!(
            load_err(&file),
            ClmLoadError::SpineJointCoincident { node, joint: 1 }
        );

        // The first link starts at the node's own origin.
        let (file, node) = spine_file(|sp| sp.joints[0] = [0.0, 0.0]);
        assert_eq!(
            load_err(&file),
            ClmLoadError::SpineJointCoincident { node, joint: 0 }
        );

        let (file, node) = spine_file(|sp| sp.targets.pop().map(|_| ()).unwrap_or_default());
        assert_eq!(
            load_err(&file),
            ClmLoadError::SpineTargetCount {
                node,
                targets: 1,
                joints: 2,
            }
        );

        let (file, _) = spine_file(|sp| sp.targets[0] = Some(ParamId::new("gone").unwrap()));
        assert!(matches!(
            load_err(&file),
            ClmLoadError::DanglingParam { ref owner, ref id }
                if owner.starts_with("spine") && id == "gone"
        ));
    }

    #[test]
    fn a_spine_with_no_targets_key_loads_one_slot_per_link() {
        let mut file = sample().to_clm_file().unwrap();
        let root = file.doc.nodes[0].id.clone();
        let id = NodeId::new("root/spine-00000002").unwrap();
        file.doc.nodes.push(ClmNode {
            id: id.clone(),
            parent: Some(root),
            name: "tail".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: ClmNodeKind::Spine(ClmSpine {
                joints: vec![[0.0, -40.0], [0.0, -80.0], [10.0, -120.0]],
                // What `#[serde(default)]` hands the reader for an absent key.
                targets: Vec::new(),
            }),
        });

        let model = Model::from_clm_file(&file).unwrap();
        let ModelNodeKind::Spine(spine) = &model.node(&id).unwrap().kind else {
            panic!("the spine came back as another kind");
        };
        assert_eq!(spine.targets(), [None, None, None]);
        let round_tripped = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
        let ModelNodeKind::Spine(spine) = &round_tripped.node(&id).unwrap().kind else {
            panic!("the spine came back as another kind");
        };
        assert_eq!(spine.targets(), [None, None, None]);
        assert_eq!(spine.joints(), [[0.0, -40.0], [0.0, -80.0], [10.0, -120.0]]);
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
    fn two_bindings_under_one_key_are_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        let twin = file.doc.bindings[0].clone();
        let node = twin.node.to_string();
        file.doc.bindings.push(twin);

        assert_eq!(
            load_err(&file),
            ClmLoadError::DuplicateBinding {
                node: node.clone(),
                target: "deform",
            }
        );
        let mut fragment = sample_fragment_file();
        let twin = fragment.doc.bindings[0].clone();
        fragment.doc.bindings.push(twin);
        assert_eq!(
            fragment_err(&fragment),
            ClmLoadError::DuplicateBinding {
                node,
                target: "deform",
            }
        );

        let mut file = sample().to_clm_file().unwrap();
        let mut other = file.doc.bindings[0].clone();
        other.params.truncate(1);
        other.key_positions.truncate(1);
        if let ClmBindingValues::Deform(cells) = &mut other.values {
            cells.cells.clear();
        }
        file.doc.bindings.push(other);
        assert_eq!(Model::from_clm_file(&file).unwrap().bindings().count(), 2);
    }

    #[test]
    fn a_value_that_is_not_a_number_is_a_structured_error() {
        fn upper(file: &mut ClmFile) -> &mut ClmNode {
            file.doc
                .nodes
                .iter_mut()
                .find(|n| n.name == "Upper")
                .unwrap()
        }

        let mut file = sample().to_clm_file().unwrap();
        upper(&mut file).z_order = f32::NAN;
        let id = upper(&mut file).id.to_string();
        assert_eq!(
            load_err(&file),
            ClmLoadError::NodeNotFinite {
                node: id.clone(),
                field: "z-order",
            }
        );

        let mut file = sample().to_clm_file().unwrap();
        upper(&mut file).transform.rotation[2] = f32::INFINITY;
        assert_eq!(
            load_err(&file),
            ClmLoadError::NodeNotFinite {
                node: id.clone(),
                field: "transform",
            }
        );

        let mut file = sample().to_clm_file().unwrap();
        file.doc.params[0].default = f32::NAN;
        assert_eq!(
            load_err(&file),
            ClmLoadError::ParamDefault {
                param: file.doc.params[0].id.to_string(),
            }
        );

        let mut fragment = sample_fragment_file();
        upper(&mut fragment).transform.scale[0] = f32::NAN;
        assert_eq!(
            fragment_err(&fragment),
            ClmLoadError::NodeNotFinite {
                node: id,
                field: "transform",
            }
        );
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

    #[test]
    fn a_deform_binding_on_a_mesh_group_still_loads() {
        let mut file = sample().to_clm_file().unwrap();
        let bend = node_named(&file, "Bend").id.clone();
        file.doc.bindings[0].node = bend;
        file.doc.bindings[0].values = ClmBindingValues::Deform(ClmCells::default());

        assert!(Model::from_clm_file(&file).is_ok());
    }

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
                    cell: [1, 1],
                    got,
                    expected: 8,
                }
            );
        }
    }

    #[test]
    fn a_deform_cell_the_mesh_cannot_take_cannot_be_written() {
        let mut m = sample();
        let upper = m
            .nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == "Upper"))
            .unwrap();
        assert!(matches!(
            m.set_node_mesh_with(&upper, quad(0.0), |_, _, offsets| offsets[..2].to_vec()),
            Err(ModelError::DeformLength {
                expected: 8,
                got: 2
            })
        ));
        assert!(m.to_clm_file().is_ok(), "the refused refit changed nothing");
        for binding in &mut m.bindings {
            if let ClmBindingValues::Deform(cells) = binding.values_mut() {
                for cell in &mut cells.cells {
                    cell.value.truncate(2);
                }
            }
        }

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

    #[test]
    fn a_weld_pairing_one_slot_twice_is_a_structured_error() {
        let mut file = sample().to_clm_file().unwrap();
        file.doc.welds[0].pairs[1].a = file.doc.welds[0].pairs[0].a.clone();

        assert!(matches!(
            load_err(&file),
            ClmLoadError::WeldSlotPairedTwice { .. }
        ));
    }

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

    fn store(m: &Model) -> HashMap<TexId, ModelTexture> {
        m.texture_ids()
            .iter()
            .filter_map(|id| Some((id.clone(), m.texture(id)?.clone())))
            .collect()
    }

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

    #[test]
    fn a_structure_is_read_as_strictly_as_a_file() {
        let m = sample();
        let mut doc = m.to_clm_structure().unwrap();
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
    }
}
