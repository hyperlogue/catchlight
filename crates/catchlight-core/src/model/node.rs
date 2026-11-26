//! Nodes and their kinds, and the edits that change one node in place.

use super::*;

#[derive(Debug, Clone)]
pub struct NodeProperties {
    pub name: Name,
    pub enabled: bool,
    pub z_order: f32,
    pub transform: ClmTransform,
    pub lock_to_root: bool,
}

#[derive(Debug, Clone)]
pub struct ModelNode {
    pub(super) properties: NodeProperties,
    pub kind: ModelNodeKind,
    pub(super) parent: Option<NodeId>,
    // Like meshes, node-owned lists are shared across snapshots and edit rollback.
    pub(super) children: Arc<Vec<NodeId>>,
}

#[derive(Debug, Clone)]
pub enum ModelNodeKind {
    Group,
    Part(ModelPart),
    Composite(ModelComposite),
    MeshGroup(ModelMeshGroup),
    MotionResponse(crate::MotionResponse),
    Spine(ModelSpine),
}

impl ModelNodeKind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::Part(_) => "part",
            Self::Composite(_) => "composite",
            Self::MeshGroup(_) => "mesh_group",
            Self::MotionResponse(_) => "motion_response",
            Self::Spine(_) => "spine",
        }
    }

    pub(super) fn id_kind(&self) -> NodeIdKind {
        match self {
            Self::Group => NodeIdKind::Group,
            Self::Part(_) => NodeIdKind::Part,
            Self::Composite(_) => NodeIdKind::Composite,
            Self::MeshGroup(_) => NodeIdKind::MeshGroup,
            Self::MotionResponse(_) => NodeIdKind::MotionResponse,
            Self::Spine(_) => NodeIdKind::Spine,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMesh(pub(super) Arc<ClmMesh>);

impl ModelMesh {
    pub fn to_clm(&self) -> ClmMesh {
        (*self.0).clone()
    }
}

impl From<ClmMesh> for ModelMesh {
    fn from(mesh: ClmMesh) -> Self {
        Self(Arc::new(mesh))
    }
}

impl Deref for ModelMesh {
    type Target = ClmMesh;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Never drawn, so it has no colour fields.
#[derive(Debug, Clone)]
pub struct ModelMeshGroup {
    pub translate_children: bool,
    pub(super) mesh: ModelMesh,
}

impl ModelMeshGroup {
    pub fn new(mesh: impl Into<ModelMesh>) -> Self {
        Self {
            translate_children: true,
            mesh: mesh.into(),
        }
    }

    pub fn mesh(&self) -> &ClmMesh {
        &self.mesh
    }
}

#[derive(Debug, Clone)]
pub struct ModelPart {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub mask_threshold: f32,
    pub(super) mesh: ModelMesh,
    /// `None` for an unmapped part, which the renderer culls.
    pub(super) albedo: Option<TexId>,
    pub(super) masks: Arc<Vec<ModelMask>>,
    /// Built only by [`Model`]'s slot methods, so a slot is in mesh range or unfilled.
    pub(super) slots: Arc<Vec<Slot>>,
}

impl ModelPart {
    pub fn new(mesh: impl Into<ModelMesh>) -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            mask_threshold: 0.5,
            mesh: mesh.into(),
            albedo: None,
            masks: Arc::default(),
            slots: Arc::default(),
        }
    }

    pub fn mesh(&self) -> &ClmMesh {
        &self.mesh
    }

    /// A cheap immutable snapshot, sharing storage until the mesh is replaced.
    pub fn shared_mesh(&self) -> ModelMesh {
        self.mesh.clone()
    }

    pub fn albedo(&self) -> Option<&TexId> {
        self.albedo.as_ref()
    }

    pub fn masks(&self) -> &[ModelMask] {
        &self.masks
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }
}

#[derive(Debug, Clone)]
pub struct ModelComposite {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub mask_threshold: f32,
    pub propagate_meshgroup: bool,
    pub(super) masks: Arc<Vec<ModelMask>>,
}

impl ModelComposite {
    pub fn new() -> Self {
        Self {
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.0; 3],
            mask_threshold: 0.5,
            propagate_meshgroup: false,
            masks: Arc::default(),
        }
    }

    pub fn masks(&self) -> &[ModelMask] {
        &self.masks
    }
}

impl Default for ModelComposite {
    fn default() -> Self {
        Self::new()
    }
}

/// The source is a part or composite other than the node it clips; in a
/// fragment it may be a base node, which [`Model::install`] checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMask {
    pub(super) source: NodeId,
    pub(super) mode: MaskMode,
}

impl ModelMask {
    pub fn source(&self) -> &NodeId {
        &self.source
    }

    pub fn mode(&self) -> MaskMode {
        self.mode
    }
}

/// `targets` is always as long as `joints`; `None` marks a rigid link.
/// `joints[i]` is the far end of link `i`; `targets[i]` is link `i`'s bend,
/// the turn at the joint above it (the node's origin for link 0).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSpine {
    pub(super) joints: Arc<Vec<[f32; 2]>>,
    pub(super) targets: Arc<Vec<Option<ParamId>>>,
}

impl ModelSpine {
    /// Unchecked until it joins a model; see [`Self::check_joints`].
    pub fn new(joints: Vec<[f32; 2]>) -> Self {
        let targets = vec![None; joints.len()];
        Self {
            joints: joints.into(),
            targets: targets.into(),
        }
    }

    /// At least one joint, all finite, and no joint repeating the point above it
    /// (the node's origin for the first).
    pub fn check_joints(joints: &[[f32; 2]]) -> Result<(), SpineJointError> {
        validate::spine_joints(joints)
    }

    /// In the node's own space.
    pub fn joints(&self) -> &[[f32; 2]] {
        &self.joints
    }

    pub fn targets(&self) -> &[Option<ParamId>] {
        &self.targets
    }

    pub fn reads(&self, param: &ParamId) -> bool {
        self.targets.iter().flatten().any(|p| p == param)
    }

    fn set_joints(&mut self, joints: Vec<[f32; 2]>) {
        self.joints = joints.into();
        Arc::make_mut(&mut self.targets).resize(self.joints.len(), None);
    }
}

impl Deref for ModelNode {
    type Target = NodeProperties;
    fn deref(&self) -> &NodeProperties {
        &self.properties
    }
}

impl std::ops::DerefMut for ModelNode {
    fn deref_mut(&mut self) -> &mut NodeProperties {
        &mut self.properties
    }
}

impl ModelNode {
    /// The label is truncated rather than rejected.
    pub fn new(name: impl AsRef<str>, kind: ModelNodeKind) -> Self {
        Self {
            properties: NodeProperties {
                name: Name::truncated(name),
                enabled: true,
                z_order: 0.0,
                transform: ClmTransform {
                    translation: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0, 1.0],
                },
                lock_to_root: false,
            },
            kind,
            parent: None,
            children: Arc::default(),
        }
    }

    pub fn parent(&self) -> Option<&NodeId> {
        self.parent.as_ref()
    }

    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    pub fn mesh(&self) -> Option<&ClmMesh> {
        match &self.kind {
            ModelNodeKind::Part(p) => Some(p.mesh()),
            ModelNodeKind::MeshGroup(mg) => Some(mg.mesh()),
            _ => None,
        }
    }

    pub(super) fn masks(&self) -> Option<&[ModelMask]> {
        match &self.kind {
            ModelNodeKind::Part(p) => Some(&p.masks),
            ModelNodeKind::Composite(c) => Some(&c.masks),
            _ => None,
        }
    }

    pub(super) fn remove_masks(&mut self, removed: impl Fn(&NodeId) -> bool) {
        if self
            .masks()
            .is_some_and(|masks| masks.iter().any(|m| removed(&m.source)))
            && let Some(masks) = self.masks_mut()
        {
            masks.retain(|m| !removed(&m.source));
        }
    }

    pub(super) fn masks_mut(&mut self) -> Option<&mut Vec<ModelMask>> {
        match &mut self.kind {
            ModelNodeKind::Part(p) => Some(Arc::make_mut(&mut p.masks)),
            ModelNodeKind::Composite(c) => Some(Arc::make_mut(&mut c.masks)),
            _ => None,
        }
    }

    fn mesh_mut(&mut self) -> Option<&mut ModelMesh> {
        match &mut self.kind {
            ModelNodeKind::Part(p) => Some(&mut p.mesh),
            ModelNodeKind::MeshGroup(mg) => Some(&mut mg.mesh),
            _ => None,
        }
    }
}

fn kind_edit_fault(before: &ModelNodeKind, after: &ModelNodeKind) -> Option<&'static str> {
    use ModelNodeKind as Kind;
    match (before, after) {
        (Kind::Group, Kind::Group) | (Kind::MotionResponse(_), Kind::MotionResponse(_)) => None,
        (Kind::Part(a), Kind::Part(b)) => {
            if !same(&a.mesh.0, &b.mesh.0) {
                Some("a part's mesh")
            } else if a.albedo != b.albedo {
                Some("a part's albedo")
            } else if !same(&a.masks, &b.masks) {
                Some("a part's masks")
            } else if !same(&a.slots, &b.slots) {
                Some("a part's slots")
            } else {
                None
            }
        }
        (Kind::Composite(a), Kind::Composite(b)) => {
            (!same(&a.masks, &b.masks)).then_some("a composite's masks")
        }
        (Kind::MeshGroup(a), Kind::MeshGroup(b)) => {
            (!same(&a.mesh.0, &b.mesh.0)).then_some("a mesh group's mesh")
        }
        (Kind::Spine(a), Kind::Spine(b)) => (a != b).then_some("a spine's joints or targets"),
        _ => Some("a node's kind"),
    }
}

pub(crate) fn is_mask_source(kind: &ModelNodeKind) -> bool {
    matches!(kind, ModelNodeKind::Part(_) | ModelNodeKind::Composite(_))
}

/// A part's albedo is a texture's only user.
pub(crate) fn albedo_of(node: &ModelNode) -> Option<&TexId> {
    match &node.kind {
        ModelNodeKind::Part(p) => p.albedo.as_ref(),
        _ => None,
    }
}

/// Vertices come in pairs, uvs match them, every coordinate is finite, and
/// the indices are whole triangles over existing vertices.
pub(super) fn validate_mesh(mesh: &ClmMesh) -> Result<(), &'static str> {
    if !mesh.verts.len().is_multiple_of(2) {
        return Err("vertex array is not [x, y] pairs");
    }
    if !mesh.uvs.is_empty() && mesh.uvs.len() != mesh.verts.len() {
        return Err("uv count does not match vertices");
    }
    if mesh.verts.iter().any(|v| !v.is_finite()) {
        return Err("a vertex is not finite");
    }
    if mesh.uvs.iter().any(|v| !v.is_finite()) {
        return Err("a uv is not finite");
    }
    if mesh.origin.iter().any(|v| !v.is_finite()) {
        return Err("the origin is not finite");
    }
    let (count, max_index) = match &mesh.indices {
        clm::ClmIndices::U16(v) => (v.len(), v.iter().map(|&i| i as usize).max()),
        clm::ClmIndices::U32(v) => (v.len(), v.iter().map(|&i| i as usize).max()),
    };
    if !count.is_multiple_of(3) {
        return Err("index count is not whole triangles");
    }
    let vcount = mesh.verts.len() / 2;
    if max_index.is_some_and(|m| m >= vcount) {
        return Err("index names a missing vertex");
    }
    Ok(())
}

impl Model {
    /// Cannot change response ownership or inputs. Z-order and transform must stay
    /// finite, or the edit is undone with [`ModelError::NonFinite`].
    pub fn update_node<R>(
        &mut self,
        id: &NodeId,
        f: impl FnOnce(&mut NodeProperties) -> R,
    ) -> Result<R, ModelError> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| ModelError::UnknownNode(id.clone()))?;
        let previous = node.properties.clone();
        let out = f(&mut node.properties);
        if let Err(fault) = validate::node_placement(node.z_order, &node.transform) {
            node.properties = previous;
            return Err(fault.into());
        }
        self.bump();
        Ok(out)
    }

    /// Edits only the fields a kind exposes. Swapping the kind, or changing a
    /// mesh, albedo, mask list, slot list or spine, is undone with
    /// [`ModelError::KindEdit`]: each has its own method that keeps dependents in
    /// step.
    pub fn update_node_kind<R>(
        &mut self,
        id: &NodeId,
        f: impl FnOnce(&mut ModelNodeKind) -> R,
    ) -> Result<R, ModelError> {
        let node = self
            .nodes
            .get_mut(id)
            .ok_or_else(|| ModelError::UnknownNode(id.clone()))?;
        let previous = node.kind.clone();
        let out = f(&mut node.kind);
        if let Some(what) = kind_edit_fault(&previous, &node.kind) {
            node.kind = previous;
            return Err(ModelError::KindEdit(what));
        }
        if let Err(fault) = validate::appearance(&node.kind) {
            node.kind = previous;
            return Err(fault.into());
        }
        let response_changed = matches!(
            (&previous, &node.kind),
            (ModelNodeKind::MotionResponse(a), ModelNodeKind::MotionResponse(b)) if a != b
        );
        if response_changed {
            self.response_index.take();
            if let Err(error) = self.validate_responses() {
                self.nodes
                    .get_mut(id)
                    .ok_or_else(|| ModelError::UnknownNode(id.clone()))?
                    .kind = previous;
                self.response_index.take();
                return Err(error.into());
            }
        }
        self.bump();
        Ok(out)
    }

    /// `targets` must match the joint count; [`Model::set_spine_joints`] changes it.
    pub fn set_spine_targets(
        &mut self,
        node: &NodeId,
        targets: Vec<Option<ParamId>>,
    ) -> Result<(), ModelError> {
        if let Some(missing) = targets
            .iter()
            .flatten()
            .find(|p| !self.params.contains_key(*p))
        {
            return Err(ModelError::UnknownParam(missing.clone()));
        }
        match self.nodes.get_mut(node).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Spine(spine)) => {
                if targets.len() != spine.joints.len() {
                    return Err(ModelError::SpineTargetArity {
                        targets: targets.len(),
                        joints: spine.joints.len(),
                    });
                }
                spine.targets = targets.into();
            }
            Some(_) => return Err(ModelError::NotSpine),
            None => return Err(ModelError::UnknownNode(node.clone())),
        }
        self.bump();
        Ok(())
    }

    /// Links past the new end lose their param; new links arrive rigid.
    pub fn set_spine_joints(
        &mut self,
        node: &NodeId,
        joints: Vec<[f32; 2]>,
    ) -> Result<(), ModelError> {
        match self.nodes.get_mut(node).map(|n| &mut n.kind) {
            Some(ModelNodeKind::Spine(spine)) => {
                validate::spine_joints(&joints)?;
                spine.set_joints(joints);
            }
            Some(_) => return Err(ModelError::NotSpine),
            None => return Err(ModelError::UnknownNode(node.clone())),
        }
        self.bump();
        Ok(())
    }

    /// `refit` must return one finite `[dx, dy]` per new vertex, or the edit is
    /// refused untouched. Slots are not refitted: every slot on the part is
    /// emptied and returned for its author to refill.
    pub fn set_node_mesh_with(
        &mut self,
        id: &NodeId,
        mesh: ClmMesh,
        mut refit: impl FnMut(&ClmMesh, &ClmMesh, &[f32]) -> Vec<f32>,
    ) -> Result<Vec<SlotId>, ModelError> {
        validate_mesh(&mesh).map_err(ModelError::MalformedMesh)?;
        let old = match self.nodes.get(id).and_then(ModelNode::mesh) {
            Some(m) => m.clone(),
            None => {
                return Err(if self.nodes.contains_key(id) {
                    ModelError::NotMeshed
                } else {
                    ModelError::UnknownNode(id.clone())
                });
            }
        };
        let expected = mesh.verts.len();
        let mut refitted: Vec<(usize, Vec<Vec<f32>>)> = Vec::new();
        for (index, binding) in self
            .bindings
            .iter()
            .enumerate()
            .filter(|(_, b)| b.node() == id)
        {
            let mut values = Vec::new();
            if let ClmBindingValues::Deform(cells) = binding.values() {
                values.reserve(cells.cells.len());
                for cell in &cells.cells {
                    let value = refit(&old, &mesh, &cell.value);
                    validate::deform_offsets(&value, expected)?;
                    values.push(value);
                }
            }
            refitted.push((index, values));
        }
        let apply = |b: &mut ModelBinding, values: Vec<Vec<f32>>| {
            // The deform identity is sized to the mesh, so every grid goes stale.
            b.invalidate_dense();
            if let ClmBindingValues::Deform(cells) = b.values_mut() {
                for (cell, value) in cells.cells.iter_mut().zip(values) {
                    cell.value = value;
                }
            }
        };
        // A custom mesh refit also edits binding cells. Stage this node's
        // bindings together if one of them protects a response's rest pose.
        if self.bindings_of_node(id).any(|b| {
            b.target() == BindingTarget::Deform
                && b.params().iter().any(|p| self.response_owner(p).is_some())
        }) {
            let mut edits = Vec::with_capacity(refitted.len());
            for (index, values) in refitted {
                let mut candidate = self.bindings[index].clone();
                apply(&mut candidate, values);
                self.validate_response_binding(&candidate)?;
                edits.push((index, candidate));
            }
            for (index, binding) in edits {
                self.bindings[index] = binding;
            }
        } else {
            for (index, values) in refitted {
                apply(&mut self.bindings[index], values);
            }
        }
        if let Some(slot) = self.nodes.get_mut(id).and_then(ModelNode::mesh_mut) {
            *slot = mesh.into();
        }
        let mut emptied = Vec::new();
        if let Some(ModelNodeKind::Part(part)) = self.nodes.get_mut(id).map(|n| &mut n.kind) {
            for slot in Arc::make_mut(&mut part.slots) {
                slot.vertex = None;
                emptied.push(slot.id.clone());
            }
        }
        self.bump();
        Ok(emptied)
    }

    /// New vertices start at zero offset; slots are emptied as in
    /// [`Self::set_node_mesh_with`].
    pub fn set_node_mesh(&mut self, id: &NodeId, mesh: ClmMesh) -> Result<Vec<SlotId>, ModelError> {
        self.set_node_mesh_with(id, mesh, |_, new, offsets| {
            let mut out = offsets.to_vec();
            out.resize(new.verts.len(), 0.0);
            out
        })
    }

    pub(super) fn masks_mut(&mut self, id: &NodeId) -> Result<&mut Vec<ModelMask>, ModelError> {
        match self.nodes.get_mut(id) {
            Some(n) => n.masks_mut().ok_or(ModelError::NotMaskable),
            None => Err(ModelError::UnknownNode(id.clone())),
        }
    }

    pub fn mask_add(
        &mut self,
        id: &NodeId,
        source: &NodeId,
        mode: MaskMode,
    ) -> Result<(), ModelError> {
        if source == id {
            return Err(ModelError::SelfMask);
        }
        match self.nodes.get(source).map(|n| &n.kind) {
            Some(k) if is_mask_source(k) => {}
            Some(_) => return Err(ModelError::NotAMaskSource),
            None => return Err(ModelError::UnknownNode(source.clone())),
        }
        let source = source.clone();
        self.masks_mut(id)?.push(ModelMask { source, mode });
        self.bump();
        Ok(())
    }

    pub fn mask_set_mode(
        &mut self,
        id: &NodeId,
        index: usize,
        mode: MaskMode,
    ) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        let m = masks.get_mut(index).ok_or(ModelError::IndexOutOfRange)?;
        m.mode = mode;
        self.bump();
        Ok(())
    }

    /// `to` clamps. Mask order is evaluation order.
    pub fn mask_reorder(&mut self, id: &NodeId, index: usize, to: usize) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        let m = masks.remove(index);
        let to = to.min(masks.len());
        masks.insert(to, m);
        self.bump();
        Ok(())
    }

    pub fn mask_delete(&mut self, id: &NodeId, index: usize) -> Result<(), ModelError> {
        let masks = self.masks_mut(id)?;
        if index >= masks.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        masks.remove(index);
        self.bump();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::*;

    #[test]
    fn a_spine_no_reader_would_take_cannot_be_authored() {
        let mut r = fixture();
        let spine = r.spine();
        let root = r.root.clone();
        let before = r.model.generation();

        for (joints, expected) in [
            (Vec::new(), SpineJointError::NoJoints),
            (vec![[0.0, f32::NAN]], SpineJointError::NonFinite(0)),
            (vec![[0.0, 0.0]], SpineJointError::Coincident(0)),
            (
                vec![[0.0, -5.0], [0.0, -5.0]],
                SpineJointError::Coincident(1),
            ),
        ] {
            assert!(
                matches!(
                    r.model.set_spine_joints(&spine, joints.clone()),
                    Err(ModelError::SpineJoints(e)) if e == expected
                ),
                "set_spine_joints took {joints:?}"
            );
            let node = ModelNode::new("bad", ModelNodeKind::Spine(ModelSpine::new(joints.clone())));
            assert!(
                matches!(
                    r.model.add_node(&root, node, &mut r.hex),
                    Err(ModelError::SpineJoints(e)) if e == expected
                ),
                "add_node took {joints:?}"
            );
        }
        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        match &r.model.node(&spine).unwrap().kind {
            ModelNodeKind::Spine(sp) => {
                assert_eq!(sp.joints(), [[0.0, -50.0], [0.0, -100.0]]);
            }
            other => panic!("not a spine: {other:?}"),
        }

        r.model
            .set_spine_joints(&spine, vec![[0.0, -10.0]])
            .unwrap();
        Model::from_clm_bytes(&r.model.to_clm_bytes().unwrap()).unwrap();
    }

    #[test]
    fn a_refit_that_returns_a_bad_cell_refuses_the_whole_mesh_edit() {
        let mut r = fixture();
        let part = r.part.clone();
        let key = deform_key(&r);
        r.model
            .set_deform_vertices(&key, [0, 0], vec![0.5; 8])
            .unwrap();
        r.model
            .set_deform_vertices(&key, [1, 0], vec![0.25; 8])
            .unwrap();
        let mut bigger = quad();
        bigger.verts.extend([0.0, 0.0]);
        bigger.uvs.extend([0.0, 0.0]);
        let resized = |new: &ClmMesh, offsets: &[f32]| {
            let mut out = offsets.to_vec();
            out.resize(new.verts.len(), 0.0);
            out
        };
        let before = r.model.generation();
        let bytes = r.model.to_clm_bytes().unwrap();

        // The first cell refits fine; the second does not. Nothing may stick.
        let mut cell = 0;
        let got = r
            .model
            .set_node_mesh_with(&part, bigger.clone(), |_, new, offsets| {
                cell += 1;
                if cell == 2 {
                    offsets.to_vec()
                } else {
                    resized(new, offsets)
                }
            });
        assert!(
            matches!(
                got,
                Err(ModelError::DeformLength {
                    expected: 10,
                    got: 8
                })
            ),
            "{got:?}"
        );
        let got = r
            .model
            .set_node_mesh_with(&part, bigger.clone(), |_, new, _| {
                vec![f32::NAN; new.verts.len()]
            });
        assert!(
            matches!(got, Err(ModelError::NonFinite("a deform offset"))),
            "{got:?}"
        );
        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        assert_eq!(r.model.to_clm_bytes().unwrap(), bytes);

        r.model
            .set_node_mesh_with(&part, bigger, |_, new, offsets| resized(new, offsets))
            .unwrap();
        Model::from_clm_bytes(&r.model.to_clm_bytes().unwrap()).unwrap();
    }

    #[test]
    fn an_in_place_kind_edit_changes_only_the_fields_a_kind_exposes() {
        let mut r = fixture();
        let spine = r.spine();
        let group = r.node(
            "bend",
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad())),
        );
        let (part, other, composite) = (r.part.clone(), r.other.clone(), r.composite.clone());
        // `part` as it is before it gains a slot, and `other`, which draws a
        // texture `part` does not.
        let slotless = r.model.node(&part).unwrap().kind.clone();
        let textured = r.model.node(&other).unwrap().kind.clone();
        r.model.slot_add(&part, slot("s")).unwrap();
        let mut moved = quad();
        moved.verts[0] += 1.0;

        let before = r.model.generation();
        let bytes = r.model.to_clm_bytes().unwrap();
        let swaps: Vec<(&NodeId, ModelNodeKind, &str)> = vec![
            (&part, ModelNodeKind::Group, "a node's kind"),
            (
                &part,
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad())),
                "a node's kind",
            ),
            (
                &other,
                ModelNodeKind::Part(ModelPart::new(moved.clone())),
                "a part's mesh",
            ),
            (&part, textured, "a part's albedo"),
            (&part, slotless, "a part's slots"),
            (
                &composite,
                ModelNodeKind::Composite(ModelComposite::new()),
                "a composite's masks",
            ),
            (
                &group,
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(moved)),
                "a mesh group's mesh",
            ),
            (
                &spine,
                ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, 0.0]])),
                "a spine's joints or targets",
            ),
        ];
        for (id, replacement, what) in swaps {
            let got = r.model.update_node_kind(id, |kind| *kind = replacement);
            assert!(
                matches!(got, Err(ModelError::KindEdit(named)) if named == what),
                "expected a refusal naming {what}, got {got:?}"
            );
        }
        assert_eq!(r.model.generation(), before, "a refused edit is no edit");
        assert_eq!(
            r.model.to_clm_bytes().unwrap(),
            bytes,
            "every refused edit put the kind back"
        );

        let was = r
            .model
            .update_node_kind(&part, |kind| match kind {
                ModelNodeKind::Part(p) => std::mem::replace(&mut p.opacity, 0.25),
                _ => f32::NAN,
            })
            .unwrap();
        assert_eq!(was, 1.0);
        r.model
            .update_node_kind(&composite, |kind| {
                if let ModelNodeKind::Composite(c) = kind {
                    c.blend_mode = BlendMode::Multiply;
                }
            })
            .unwrap();
        r.model
            .update_node_kind(&group, |kind| {
                if let ModelNodeKind::MeshGroup(mg) = kind {
                    mg.translate_children = false;
                }
            })
            .unwrap();
        assert_ne!(r.model.generation(), before);
        match &r.model.node(&part).unwrap().kind {
            ModelNodeKind::Part(p) => {
                assert_eq!(p.opacity, 0.25);
                assert_eq!(p.slots().len(), 1, "the slot is still there");
            }
            other => panic!("not a part: {other:?}"),
        }
    }

    #[test]
    fn mask_ops_validate_and_reorder() {
        let mut r = fixture();
        let (a, b, target) = (r.part.clone(), r.other.clone(), r.composite.clone());
        let c = r.node("c", part_kind());
        let root = r.root.clone();
        let m = &mut r.model;

        m.mask_delete(&target, 0).unwrap();
        assert!(m.mask_add(&a, &a, MaskMode::Mask).is_err());
        assert!(
            m.mask_add(&a, &root, MaskMode::Mask)
                .is_err_and(|e| matches!(e, ModelError::NotAMaskSource))
        );
        m.mask_add(&a, &target, MaskMode::Mask).unwrap();
        m.mask_delete(&a, 0).unwrap();
        let bend = r.node(
            "bend",
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad())),
        );
        let m = &mut r.model;
        assert!(
            m.mask_add(&a, &bend, MaskMode::Mask)
                .is_err_and(|e| matches!(e, ModelError::NotAMaskSource))
        );

        m.mask_add(&target, &a, MaskMode::Mask).unwrap();
        m.mask_add(&target, &b, MaskMode::DodgeMask).unwrap();
        m.mask_add(&target, &c, MaskMode::Mask).unwrap();
        m.mask_reorder(&target, 2, 0).unwrap();
        m.mask_set_mode(&target, 1, MaskMode::DodgeMask).unwrap();
        m.mask_delete(&target, 2).unwrap();
        let masks = m.node(&target).unwrap().masks().unwrap();
        assert_eq!(masks.len(), 2);
        assert_eq!(masks[0].source(), &c);
        assert!(matches!(masks[1].mode(), MaskMode::DodgeMask));
        assert!(m.mask_delete(&target, 5).is_err());
    }

    #[test]
    fn a_new_mesh_group_translates_its_children() {
        assert!(ModelMeshGroup::new(quad()).translate_children);
    }
}
