//! Reading a model from `.clm`, trusting nothing.

use super::*;

type SlotTable = HashSet<SlotId>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    Base,
    Fragment,
}

impl Shape {
    /// A binding's *node* never may dangle, in either shape.
    fn allows_dangling(self) -> bool {
        matches!(self, Self::Fragment)
    }
}

/// `bytes` returns the payload behind a byte marker, refusing one that
/// disagrees with its marker. A maintained extension whose key is registered
/// is decoded now; one that does not decode is kept as stored, never refused.
/// A value equal to the one `previous` holds under its key reuses that slot,
/// decoded data and all.
pub(super) fn extensions_of(
    doc: &ClmStructure,
    bytes: impl Fn(&ExtensionKey, &ClmExtensionMarker) -> Result<Arc<[u8]>, clm::ClmError>,
    previous: Option<&Extensions>,
) -> Result<Extensions, ModelError> {
    let mut out = Extensions::default();
    for (key, value) in &doc.extensions {
        let value = match value {
            ClmExtension::Json(json) => ExtensionValue::Json(json.clone()),
            ClmExtension::Bytes(marker) => ExtensionValue::Bytes(bytes(key, marker)?),
            ClmExtension::Maintained {
                version,
                refs,
                payload,
            } => ExtensionValue::Maintained(RawExtension {
                version: *version,
                refs: refs.clone(),
                payload: match payload {
                    ClmPayload::Json(json) => Payload::Json(json.clone()),
                    ClmPayload::Bytes(marker) => Payload::Bytes(bytes(key, marker)?),
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

impl Model {
    pub fn from_clm_file(file: &ClmFile) -> Result<Self, ModelError> {
        Self::from_clm_file_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clm_file_with_budget(
        file: &ClmFile,
        budget: &mut LoadBudget,
    ) -> Result<Self, ModelError> {
        Self::read(file, budget, Shape::Base)
    }

    pub fn from_clm_file_fragment(file: &ClmFile) -> Result<Self, ModelError> {
        Self::from_clm_file_fragment_with_budget(file, &mut LoadBudget::default())
    }

    pub fn from_clm_file_fragment_with_budget(
        file: &ClmFile,
        budget: &mut LoadBudget,
    ) -> Result<Self, ModelError> {
        Self::read(file, budget, Shape::Fragment)
    }

    pub fn from_clm_bytes(bytes: &[u8]) -> Result<Self, ModelError> {
        Self::from_clm_bytes_with_budget(bytes, &mut LoadBudget::default())
    }

    pub fn from_clm_bytes_with_budget(
        bytes: &[u8],
        budget: &mut LoadBudget,
    ) -> Result<Self, ModelError> {
        Self::read_bytes(bytes, budget, Shape::Base)
    }

    pub fn from_clm_bytes_fragment(bytes: &[u8]) -> Result<Self, ModelError> {
        Self::from_clm_bytes_fragment_with_budget(bytes, &mut LoadBudget::default())
    }

    pub fn from_clm_bytes_fragment_with_budget(
        bytes: &[u8],
        budget: &mut LoadBudget,
    ) -> Result<Self, ModelError> {
        Self::read_bytes(bytes, budget, Shape::Fragment)
    }

    fn read(file: &ClmFile, budget: &mut LoadBudget, shape: Shape) -> Result<Self, ModelError> {
        charge_clm_file(file, budget)?;
        let textures = file.textures.iter().map(|t| {
            let texture = ModelTexture {
                encoding: t.encoding,
                alpha: t.alpha,
                data: t.data.as_slice().into(),
            };
            (t.id.clone(), texture)
        });
        // A hand-built `ClmFile` skips decoding, so the pairing is checked here.
        let extensions = CheckedExtensions::new(&file.doc, file.extensions.as_slice())?;
        let mut model = Self::assemble(&file.doc, textures, &extensions, shape)?;
        model.unknown_sections.clone_from(&file.unknown_sections);
        Ok(model)
    }

    /// Decoding already charged the payloads, so only the texture count and
    /// the structure are charged here; charging again counts every byte twice.
    fn read_bytes(bytes: &[u8], budget: &mut LoadBudget, shape: Shape) -> Result<Self, ModelError> {
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
    fn assemble<B: std::borrow::Borrow<[ClmExtensionBlob]>>(
        doc: &ClmStructure,
        table: impl ExactSizeIterator<Item = (TexId, ModelTexture)>,
        blobs: &CheckedExtensions<B>,
        shape: Shape,
    ) -> Result<Self, ModelError> {
        let mut textures = HashMap::with_capacity(table.len());
        let mut texture_order = Vec::with_capacity(table.len());
        for (id, texture) in table {
            if textures.insert(id.clone(), texture).is_some() {
                return Err(duplicate("texture", &id));
            }
            texture_order.push(id);
        }

        let blobs: HashMap<&ExtensionKey, &[u8]> = blobs
            .blobs()
            .iter()
            .map(|blob| (&blob.key, blob.data.as_slice()))
            .collect();
        let held = |key: &ExtensionKey, _: &ClmExtensionMarker| {
            blobs
                .get(key)
                .map(|data| Arc::from(*data))
                .ok_or_else(|| bytes_missing(key))
        };
        let extensions = extensions_of(doc, held, None)?;
        Self::build(doc, textures, texture_order, extensions, shape)
    }

    /// The one path both a `.clm` and a structure push come through, so
    /// neither can accept what the other refuses. Charges nothing.
    pub(super) fn build(
        doc: &ClmStructure,
        textures: HashMap<TexId, ModelTexture>,
        texture_order: Vec<TexId>,
        extensions: Extensions,
        shape: Shape,
    ) -> Result<Self, ModelError> {
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
        // A fragment counts from its own roots; `Model::install` counts the rest.
        let mut depths: HashMap<&NodeId, usize> = HashMap::with_capacity(doc.nodes.len());
        for cn in &doc.nodes {
            let depth = cn
                .parent
                .as_ref()
                .and_then(|parent| depths.get(parent))
                .map_or(1, |above| above + 1);
            crate::load_budget::tree_depth(depth)?;
            depths.insert(&cn.id, depth);
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
                    let (read, table) = read_slots(&cn.id, part)?;
                    slots.insert(&cn.id, table);
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
            node.parent.clone_from(&cn.parent);
            node.enabled = cn.enabled;
            node.z_order = cn.z_order;
            node.transform = cn.transform;
            node.lock_to_root = cn.lock_to_root;
            node.kind = model_kind(
                &cn.id, &cn.kind, node_slots, &declared, &textures, &params, shape,
            )?;
            validate::appearance(&node.kind).map_err(|fault| ClmLoadError::NodeNotFinite {
                node: cn.id.to_string(),
                field: fault.field(),
            })?;
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
            if let Some(parent) = &cn.parent
                && let Some(pn) = nodes.get_mut(parent)
            {
                Arc::make_mut(&mut pn.children).push(cn.id.clone());
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
            let target = b.values.target();
            // Otherwise a meshless deform slips past `check_deform_cells`,
            // whose expected length for it is zero.
            match validate::binding_target(&bound.kind, target) {
                Ok(()) => {}
                Err(validate::BindingFault::ColorOnMeshGroup(t)) => {
                    return Err(ClmLoadError::ColorOnMeshGroup {
                        node: b.node.to_string(),
                        target: t.name(),
                    }
                    .into());
                }
                Err(validate::BindingFault::DeformOnUnmeshed) => {
                    return Err(ClmLoadError::DeformOnUnmeshed {
                        node: b.node.to_string(),
                        kind: bound.kind.name(),
                    }
                    .into());
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
                    .into());
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
                    for cell in values.scalar_cells().unwrap_or(&[]) {
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

        let model = Self {
            identity: next_identity(),
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
}

fn clm_masks_of(kind: &ClmNodeKind) -> &[ClmMask] {
    match kind {
        ClmNodeKind::Part(p) => &p.masks,
        ClmNodeKind::Composite(c) => &c.masks,
        _ => &[],
    }
}

fn check_mesh(id: &NodeId, mesh: &ClmMesh) -> Result<(), ModelError> {
    match validate_mesh(mesh) {
        Err(ModelError::MalformedMesh(reason)) => Err(ClmLoadError::MalformedMesh {
            node: id.to_string(),
            reason,
        }
        .into()),
        other => other,
    }
}

pub(super) fn bytes_missing(key: &ExtensionKey) -> clm::ClmError {
    clm::ClmError::ExtensionBytesMissing {
        key: key.to_string(),
    }
}

pub(super) fn duplicate(kind: &'static str, id: &impl std::fmt::Display) -> ModelError {
    ClmLoadError::DuplicateId {
        kind,
        id: id.to_string(),
    }
    .into()
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

fn read_slots(id: &NodeId, part: &ClmPart) -> Result<(Vec<Slot>, SlotTable), ModelError> {
    let vertices = part.mesh.vertex_count();
    let mut out: Vec<Slot> = Vec::with_capacity(part.slots.len());
    let mut seen = SlotTable::with_capacity(part.slots.len());
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
        if !seen.insert(slot.id.clone()) {
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
    Ok((out, seen))
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
mod tests;
