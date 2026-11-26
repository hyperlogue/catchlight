//! Reading a model from `.clm`, trusting nothing.

use super::*;

type SlotTable = HashSet<SlotId>;

/// What a `.clm` holds. The two are disjoint on the wire: a fragment never
/// has a parentless node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Shape {
    /// One root, and every reference resolved within the file.
    #[default]
    Complete,
    /// An addon: its roots and some references name a base it is installed into.
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
) -> Result<Extensions, LoadError> {
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

/// How [`Model::from_clm_bytes_with`] and [`Model::from_clm_file_with`] read:
/// the shape expected and the budget charged. The default reads a complete
/// model under [`LoadBudget::default`].
#[derive(Debug, Default)]
pub struct ReadOptions<'a> {
    pub shape: Shape,
    /// Shared across reads, so several files can draw on one budget.
    pub budget: Option<&'a mut LoadBudget>,
}

impl<'a> ReadOptions<'a> {
    pub fn fragment() -> Self {
        Self {
            shape: Shape::Fragment,
            budget: None,
        }
    }

    #[must_use]
    pub fn budget(self, budget: &'a mut LoadBudget) -> Self {
        Self {
            budget: Some(budget),
            ..self
        }
    }

    fn run<T>(self, read: impl FnOnce(&mut LoadBudget, Shape) -> T) -> T {
        let mut default = LoadBudget::default();
        read(self.budget.unwrap_or(&mut default), self.shape)
    }
}

impl Model {
    /// A complete model under the default limits.
    pub fn from_clm_file(file: &ClmFile) -> Result<Self, LoadError> {
        Self::from_clm_file_with(file, ReadOptions::default())
    }

    pub fn from_clm_file_with(file: &ClmFile, options: ReadOptions<'_>) -> Result<Self, LoadError> {
        options.run(|budget, shape| Self::read(file, budget, shape))
    }

    /// A complete model under the default limits.
    pub fn from_clm_bytes(bytes: &[u8]) -> Result<Self, LoadError> {
        Self::from_clm_bytes_with(bytes, ReadOptions::default())
    }

    pub fn from_clm_bytes_with(bytes: &[u8], options: ReadOptions<'_>) -> Result<Self, LoadError> {
        options.run(|budget, shape| Self::read_bytes(bytes, budget, shape))
    }

    fn read(file: &ClmFile, budget: &mut LoadBudget, shape: Shape) -> Result<Self, LoadError> {
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
    fn read_bytes(bytes: &[u8], budget: &mut LoadBudget, shape: Shape) -> Result<Self, LoadError> {
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
    ) -> Result<Self, LoadError> {
        let mut textures = HashMap::with_capacity(table.len());
        let mut texture_order = Vec::with_capacity(table.len());
        for (id, texture) in table {
            if textures.insert(id.clone(), texture).is_some() {
                return Err(duplicate(id));
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
    ) -> Result<Self, LoadError> {
        let mut params = HashMap::with_capacity(doc.params.len());
        let mut param_order = Vec::with_capacity(doc.params.len());
        for p in &doc.params {
            if !param_range_is_valid(p.min, p.max) {
                return Err(ClmLoadError::ParamRange {
                    param: p.id.clone(),
                }
                .into());
            }
            if !p.default.is_finite() {
                return Err(ClmLoadError::ParamDefault {
                    param: p.id.clone(),
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
                return Err(duplicate(p.id.clone()));
            }
            param_order.push(p.id.clone());
        }

        // Pre-scan so a parent written too late reads differently from one
        // that names nothing, and so a mask may name a later node.
        let mut declared: HashSet<&NodeId> = HashSet::with_capacity(doc.nodes.len());
        for n in &doc.nodes {
            if !declared.insert(&n.id) {
                return Err(duplicate(n.id.clone()));
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
                            node: cn.id.clone(),
                            parent: parent.clone(),
                        }
                        .into());
                    }
                }
                Some(parent) => {
                    if !shape.allows_dangling() {
                        return Err(ClmLoadError::DanglingParent {
                            node: cn.id.clone(),
                            parent: parent.clone(),
                        }
                        .into());
                    }
                    roots.push(cn.id.clone());
                }
                None => {
                    if shape.allows_dangling() {
                        return Err(ClmLoadError::FragmentHasNoParent {
                            node: cn.id.clone(),
                        }
                        .into());
                    }
                    if let Some(first) = roots.first() {
                        return Err(ClmLoadError::MultipleRoots {
                            root: first.clone(),
                            node: cn.id.clone(),
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
                    node: cn.id.clone(),
                    field: fault.field(),
                }
            })?;
            let mut node = ModelNode::new(&cn.name, ModelNodeKind::Group);
            node.parent.clone_from(&cn.parent);
            node.properties.enabled = cn.enabled;
            node.properties.z_order = cn.z_order;
            node.properties.transform = cn.transform;
            node.properties.lock_to_root = cn.lock_to_root;
            node.kind = model_kind(
                &cn.id, &cn.kind, node_slots, &declared, &textures, &params, shape,
            )?;
            validate::appearance(&node.kind).map_err(|fault| ClmLoadError::NodeNotFinite {
                node: cn.id.clone(),
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
                        node: cn.id.clone(),
                    }
                    .into());
                }
                let Some(source) = nodes.get(&mask.source) else {
                    continue;
                };
                if !is_mask_source(&source.kind) {
                    return Err(ClmLoadError::MaskSourceKind {
                        node: cn.id.clone(),
                        mask_source: mask.source.clone(),
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
            return Err(ClmLoadError::UnusedTexture { id: spare.clone() }.into());
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
                return Err(ClmLoadError::DanglingBindingNode { id: b.node.clone() }.into());
            };
            for p in &b.params {
                if !params.contains_key(p) && !shape.allows_dangling() {
                    return Err(ClmLoadError::DanglingBindingParam {
                        node: b.node.clone(),
                        id: p.clone(),
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
                        node: b.node.clone(),
                        target: t.name(),
                    }
                    .into());
                }
                Err(validate::BindingFault::DeformOnUnmeshed) => {
                    return Err(ClmLoadError::DeformOnUnmeshed {
                        node: b.node.clone(),
                        kind: bound.kind.name(),
                    }
                    .into());
                }
            }
            let vertices = bound.mesh().map_or(0, |m| m.verts.len());
            if let Some((cell, got)) = misfit_deform_cell(vertices, &b.values) {
                return Err(ClmLoadError::DeformCellShape {
                    node: b.node.clone(),
                    cell,
                    got,
                    expected: vertices,
                }
                .into());
            }

            let key_params = match b.params.as_slice() {
                [x] => BindingParams::One(x.clone()),
                [x, y] if x != y => BindingParams::Two(x.clone(), y.clone()),
                other => {
                    return Err(ClmLoadError::BindingParamCount {
                        node: b.node.clone(),
                        got: other.len(),
                    }
                    .into());
                }
            };
            if !validate::binding_positions_are_valid(b.params.len(), &b.key_positions) {
                return Err(ClmLoadError::BindingPositions {
                    node: b.node.clone(),
                }
                .into());
            }
            let width = b.key_positions[0].len();
            let height = b.key_positions.get(1).map_or(1, Vec::len);
            let mut occupied = HashSet::new();
            let mut check = |x: u32, y: u32, finite: bool| -> Result<(), LoadError> {
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
                        node: b.node.clone(),
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
                    node: b.node.clone(),
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

fn check_mesh(id: &NodeId, mesh: &ClmMesh) -> Result<(), ClmLoadError> {
    validate_mesh(mesh).map_err(|reason| ClmLoadError::MalformedMesh {
        node: id.clone(),
        reason,
    })
}

pub(super) fn bytes_missing(key: &ExtensionKey) -> clm::ClmError {
    clm::ClmError::ExtensionBytesMissing { key: key.clone() }
}

pub(super) fn duplicate(id: impl Into<AnyId>) -> LoadError {
    ClmLoadError::DuplicateId { id: id.into() }.into()
}

fn model_kind(
    id: &NodeId,
    kind: &ClmNodeKind,
    slots: Vec<Slot>,
    declared: &HashSet<&NodeId>,
    textures: &HashMap<TexId, ModelTexture>,
    params: &HashMap<ParamId, ModelParam>,
    shape: Shape,
) -> Result<ModelNodeKind, LoadError> {
    Ok(match kind {
        ClmNodeKind::Group => ModelNodeKind::Group,
        ClmNodeKind::Part(p) => {
            check_mesh(id, &p.mesh)?;
            let mut part = ModelPart::new(p.mesh.clone());
            if let Some(albedo) = &p.albedo {
                if !textures.contains_key(albedo) {
                    return Err(ClmLoadError::DanglingTexture {
                        node: id.clone(),
                        id: albedo.clone(),
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
) -> Result<ModelSpine, LoadError> {
    validate::spine_joints(&sp.joints).map_err(|fault| {
        let node = id.clone();
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
                node: id.clone(),
                targets: sp.targets.len(),
                joints: sp.joints.len(),
            }
            .into());
        }
        for target in sp.targets.iter().flatten() {
            if !params.contains_key(target) && !shape.allows_dangling() {
                return Err(ClmLoadError::DanglingSpineTarget {
                    node: id.clone(),
                    id: target.clone(),
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
) -> Result<Vec<ModelMask>, LoadError> {
    masks
        .iter()
        .map(|m| {
            if !declared.contains(&m.source) && !shape.allows_dangling() {
                return Err(ClmLoadError::DanglingNode {
                    node: id.clone(),
                    field: "mask source",
                    id: m.source.clone(),
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

fn read_slots(id: &NodeId, part: &ClmPart) -> Result<(Vec<Slot>, SlotTable), LoadError> {
    let vertices = part.mesh.vertex_count();
    let mut out: Vec<Slot> = Vec::with_capacity(part.slots.len());
    let mut seen = SlotTable::with_capacity(part.slots.len());
    for slot in &part.slots {
        if slot.vertex.is_some_and(|v| v as usize >= vertices) {
            return Err(ClmLoadError::SlotOutOfRange {
                node: id.clone(),
                slot: slot.id.clone(),
                vertex: slot.vertex.unwrap_or_default(),
                vertices,
            }
            .into());
        }
        if !seen.insert(slot.id.clone()) {
            return Err(ClmLoadError::DuplicateSlot {
                node: id.clone(),
                slot: slot.id.clone(),
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
) -> Result<ModelWeld, LoadError> {
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
        let (a, b) = (weld.a.clone(), weld.b.clone());
        match fault {
            validate::WeldFault::SelfPaired => ClmLoadError::WeldSelfPaired { node: a },
            validate::WeldFault::EndNotAPart { end, kind } => ClmLoadError::WeldEndNotAPart {
                node: end.clone(),
                kind,
            },
            validate::WeldFault::EndMissing { end } => {
                ClmLoadError::DanglingWeldEnd { node: end.clone() }
            }
            validate::WeldFault::Duplicate => ClmLoadError::DuplicateWeld { a, b },
            validate::WeldFault::WeightOutOfRange { slot } => ClmLoadError::WeldWeightOutOfRange {
                a,
                b,
                slot: slot.clone(),
            },
            validate::WeldFault::UnknownSlot { end, slot } => ClmLoadError::WeldUnknownSlot {
                node: end.clone(),
                slot: slot.clone(),
            },
            validate::WeldFault::SlotPairedTwice { end, slot } => {
                ClmLoadError::WeldSlotPairedTwice {
                    a,
                    b,
                    end: end.clone(),
                    slot: slot.clone(),
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
