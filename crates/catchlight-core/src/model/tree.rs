//! The node tree: adding, removing, moving and copying nodes.

use super::*;

impl Model {
    fn siblings_of(&self, id: &NodeId) -> Option<&[NodeId]> {
        match self.nodes.get(id)?.parent.as_ref() {
            Some(p) if self.nodes.contains_key(p) => Some(&self.nodes.get(p)?.children),
            _ => Some(&self.roots),
        }
    }

    fn siblings_mut(&mut self, id: &NodeId) -> Result<&mut Vec<NodeId>, ModelError> {
        let parent = self
            .nodes
            .get(id)
            .ok_or_else(|| ModelError::UnknownNode(id.clone()))?
            .parent
            .clone();
        match parent {
            Some(p) if self.nodes.contains_key(&p) => self
                .nodes
                .get_mut(&p)
                .map(|n| Arc::make_mut(&mut n.children))
                .ok_or(ModelError::UnknownNode(p)),
            _ => Ok(&mut self.roots),
        }
    }

    pub(super) fn attach_child(&mut self, parent: &NodeId, child: NodeId) {
        match self.nodes.get_mut(parent) {
            Some(p) => Arc::make_mut(&mut p.children).push(child),
            None => self.roots.push(child),
        }
    }

    /// Pre-order, following sibling order; `.clm` and the arena bridge rely on it.
    pub fn nodes_in_order(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<NodeId> = self.roots.iter().rev().cloned().collect();
        while let Some(id) = stack.pop() {
            if let Some(n) = self.nodes.get(&id) {
                for child in n.children.iter().rev() {
                    stack.push(child.clone());
                }
            }
            out.push(id);
        }
        out
    }

    /// The Id is drawn by [`NodeId::generate`] until it is free.
    pub fn add_node(
        &mut self,
        parent: &NodeId,
        node: ModelNode,
        hex: &mut impl HexSource,
    ) -> Result<NodeId, ModelError> {
        let kind = node.kind.id_kind();
        let id = self.mint(
            |h| NodeId::generate(parent, kind, h),
            |m, id| m.nodes.contains_key(id),
            hex,
        )?;
        self.add_node_with_id(id.clone(), parent, node)?;
        Ok(id)
    }

    pub fn add_node_with_id(
        &mut self,
        id: NodeId,
        parent: &NodeId,
        mut node: ModelNode,
    ) -> Result<(), ModelError> {
        if !self.nodes.contains_key(parent) {
            return Err(ModelError::UnknownNode(parent.clone()));
        }
        if self.nodes.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.clone().into()));
        }
        crate::load_budget::tree_depth(self.depth_of(parent) + 1)?;
        validate::node_placement(node.z_order, &node.transform)?;
        validate::appearance(&node.kind)?;
        if let Some(mesh) = node.mesh() {
            validate_mesh(mesh).map_err(ModelError::MalformedMesh)?;
        }
        if let ModelNodeKind::Spine(spine) = &node.kind {
            validate::spine_joints(spine.joints())?;
        }
        // A node cloned out of another model carries that model's Ids.
        self.check_node_refs(&node)?;
        node.parent = Some(parent.clone());
        node.children = Arc::default();
        let added = usize::from(matches!(node.kind, ModelNodeKind::MotionResponse(_)));
        self.response_count += added;
        if added != 0 {
            self.response_index.take();
        }
        // In a fragment the new node can resolve a formerly external input.
        let check = added != 0 || self.resolves_response_node(&id);
        self.nodes.insert(id.clone(), node);
        if check && let Err(error) = self.validate_responses() {
            self.response_count -= added;
            self.response_index.take();
            self.nodes.remove(&id);
            return Err(error.into());
        }
        self.attach_child(parent, id);
        self.bump();
        Ok(())
    }

    /// Also drops the textures only the subtree drew.
    pub fn delete_node(&mut self, id: &NodeId) -> Result<(), ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("deleted"));
        }
        if !self.nodes.contains_key(id) {
            return Err(ModelError::UnknownNode(id.clone()));
        }
        let removed: HashSet<NodeId> = self.subtree(id).into_iter().collect();
        self.remove_nodes(&removed);
        self.bump();
        Ok(())
    }

    /// `removed` must be closed under descent. Does not bump; the caller does.
    pub(super) fn remove_nodes(&mut self, removed: &HashSet<NodeId>) {
        self.roots.retain(|r| !removed.contains(r));
        for id in removed {
            self.remove_counted_node(id);
        }
        for node in self.nodes.values_mut() {
            if node.children.iter().any(|c| removed.contains(c)) {
                Arc::make_mut(&mut node.children).retain(|c| !removed.contains(c));
            }
            node.remove_masks(|id| removed.contains(id));
            validate::drop_slot_inputs(&mut node.kind, |part, _| removed.contains(part));
        }
        self.bindings.retain(|b| !removed.contains(&b.key.node));
        self.welds
            .retain(|w| !removed.contains(&w.a) && !removed.contains(&w.b));
        self.gc_textures();
        self.remove_in_extensions(|r| match r {
            IdRef::Node(id) | IdRef::Slot(id, _) => removed.contains(id),
            _ => false,
        });
    }

    /// The node keeps its Id: the prefix records where it was created.
    pub fn reparent(&mut self, id: &NodeId, new_parent: &NodeId) -> Result<(), ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("reparented"));
        }
        for node in [id, new_parent] {
            if !self.nodes.contains_key(node) {
                return Err(ModelError::UnknownNode(node.clone()));
            }
        }
        if self.is_self_or_descendant(new_parent, id) {
            return Err(ModelError::Cycle);
        }
        crate::load_budget::tree_depth(self.depth_of(new_parent) + self.height_of(id))?;
        if matches!(&self.nodes[id].kind, ModelNodeKind::MotionResponse(r) if r.inputs.iter().any(|t| matches!(t.source,crate::ResponseSource::Parent)))
            && !matches!(
                self.nodes[new_parent].kind,
                ModelNodeKind::MotionResponse(_)
            )
        {
            return Err(ModelError::InvalidResponse {
                node: id.clone(),
                reason: "Parent needs a motion response parent".into(),
            });
        }
        let old_parent = self.nodes.get(id).and_then(|n| n.parent.clone());
        match old_parent.and_then(|op| self.nodes.get_mut(&op)) {
            Some(p) => Arc::make_mut(&mut p.children).retain(|c| c != id),
            // A fragment root: its parent is not here to detach it from.
            None => self.roots.retain(|r| r != id),
        }
        self.attach_child(new_parent, id.clone());
        if let Some(n) = self.nodes.get_mut(id) {
            n.parent = Some(new_parent.clone());
        }
        self.bump();
        Ok(())
    }

    /// `index` clamps to the end. Sibling order breaks z-order ties when drawing.
    pub fn reorder(&mut self, id: &NodeId, index: usize) -> Result<(), ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("reordered"));
        }
        let siblings = self.siblings_mut(id)?;
        let cur = siblings
            .iter()
            .position(|c| c == id)
            .ok_or_else(|| ModelError::UnknownNode(id.clone()))?;
        let moved = siblings.remove(cur);
        let index = index.min(siblings.len());
        siblings.insert(index, moved);
        self.bump();
        Ok(())
    }

    /// Copies bindings, masks into the subtree, and welds between copied parts;
    /// a weld crossing the subtree edge is not copied.
    pub fn duplicate_subtree(
        &mut self,
        id: &NodeId,
        hex: &mut impl HexSource,
    ) -> Result<NodeId, ModelError> {
        if self.root() == Some(id) {
            return Err(ModelError::Root("duplicated"));
        }
        let parent = self
            .nodes
            .get(id)
            .and_then(|n| n.parent.clone())
            .ok_or_else(|| ModelError::UnknownNode(id.clone()))?;

        let mut order = Vec::new();
        let mut stack = vec![id.clone()];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.get(&n) {
                for c in node.children.iter().rev() {
                    stack.push(c.clone());
                }
            }
            order.push(n);
        }

        // Reserve every Id first so exhaustion leaves no partial copy.
        let mut map = HashMap::new();
        let mut reserved = HashSet::new();
        for old in &order {
            let node = self
                .node(old)
                .ok_or_else(|| ModelError::UnknownNode(old.clone()))?;
            let parent_id = node.parent().and_then(|p| map.get(p)).unwrap_or(&parent);
            let kind = node.kind.id_kind();
            let new_id = self.mint(
                |h| NodeId::generate(parent_id, kind, h),
                |m, id| {
                    reserved.contains(id)
                        || m.nodes.contains_key(id)
                        || m.resolves_response_node(id)
                },
                hex,
            )?;
            reserved.insert(new_id.clone());
            map.insert(old.clone(), new_id);
        }
        let previous_response_count = self.response_count;
        for old in &order {
            let unknown = || ModelError::UnknownNode(old.clone());
            let mut copy = self.node(old).ok_or_else(unknown)?.clone();
            let new_id = map.get(old).ok_or_else(unknown)?;
            let new_parent = copy.parent().and_then(|p| map.get(p)).unwrap_or(&parent);
            if old == id {
                copy.name = Name::truncated(format!("{} copy", copy.name));
            }
            copy.parent = Some(new_parent.clone());
            copy.children = Arc::default();
            self.response_count +=
                usize::from(matches!(copy.kind, ModelNodeKind::MotionResponse(_)));
            self.nodes.insert(new_id.clone(), copy);
            self.attach_child(new_parent, new_id.clone());
        }
        if self.response_count != previous_response_count {
            self.response_index.take();
        }

        for new_id in map.values() {
            if let Some(ModelNodeKind::MotionResponse(r)) =
                self.nodes.get_mut(new_id).map(|n| &mut n.kind)
            {
                r.output_param = None;
                for t in &mut r.inputs {
                    if let crate::ResponseSource::PartSlot { part, .. } = &mut t.source
                        && let Some(mapped) = map.get(part)
                    {
                        *part = mapped.clone();
                    }
                }
            }
            if let Some(masks) = self.nodes.get_mut(new_id).and_then(ModelNode::masks_mut) {
                for m in masks.iter_mut() {
                    if let Some(mapped) = map.get(&m.source) {
                        m.source = mapped.clone();
                    }
                }
            }
        }

        let copied: Vec<ModelBinding> = self
            .bindings
            .iter()
            .filter_map(|b| {
                map.get(&b.key.node).map(|node| ModelBinding {
                    key_positions: b.key_positions.clone(),
                    key: BindingKey {
                        params: b.key.params.clone(),
                        node: node.clone(),
                        target: b.key.target,
                    },
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone(),
                    dense: b.dense.clone(),
                })
            })
            .collect();
        self.bindings.extend(copied);

        // A weld crossing the subtree edge stays with the original; a second weld
        // onto the same slots would pull it two ways.
        let welded: Vec<ModelWeld> = self
            .welds
            .iter()
            .filter_map(|w| {
                Some(ModelWeld {
                    a: map.get(&w.a)?.clone(),
                    b: map.get(&w.b)?.clone(),
                    pairs: w.pairs.clone(),
                })
            })
            .collect();
        self.welds.extend(welded);

        let unknown = || ModelError::UnknownNode(id.clone());
        let new_root = map.get(id).ok_or_else(unknown)?.clone();
        let pos = self
            .siblings_of(id)
            .and_then(|s| s.iter().position(|c| c == id))
            .ok_or_else(unknown)?;
        // reorder bumps the generation for the whole duplicate.
        self.reorder(&new_root, pos + 1)?;
        Ok(new_root)
    }

    fn check_node_refs(&self, node: &ModelNode) -> Result<(), ModelError> {
        // Checked in either shape: an addon carries its own textures, so an
        // albedo is the one cross-reference that never dangles into a base.
        if let ModelNodeKind::Part(p) = &node.kind
            && let Some(albedo) = &p.albedo
            && !self.textures.contains_key(albedo)
        {
            return Err(ModelError::UnknownTexture(albedo.clone()));
        }
        // A fragment's other cross-references are supposed to dangle into a
        // base; `Model::install` is where they are resolved.
        if self.is_fragment() {
            return Ok(());
        }
        if let ModelNodeKind::Spine(spine) = &node.kind
            && let Some(missing) = spine
                .targets
                .iter()
                .flatten()
                .find(|p| !self.params.contains_key(*p))
        {
            return Err(ModelError::UnknownParam(missing.clone()));
        }
        if let Some(masks) = node.masks() {
            for m in masks {
                match self.nodes.get(&m.source).map(|n| &n.kind) {
                    Some(k) if is_mask_source(k) => {}
                    Some(_) => return Err(ModelError::NotAMaskSource),
                    None => return Err(ModelError::UnknownNode(m.source.clone())),
                }
            }
        }
        Ok(())
    }

    pub(super) fn subtree(&self, root: &NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![root.clone()];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.nodes.get(&id) {
                stack.extend(n.children.iter().cloned());
            }
            out.push(id);
        }
        out
    }

    /// Levels from the top down to `id`, both counted; a fragment's top is
    /// its own roots.
    pub(super) fn depth_of(&self, id: &NodeId) -> usize {
        let mut levels = 0;
        let mut cur = self.nodes.get(id);
        while let Some(node) = cur {
            levels += 1;
            cur = node.parent.as_ref().and_then(|p| self.nodes.get(p));
        }
        levels
    }

    /// Levels in the subtree under `id`, `id` counted.
    pub(super) fn height_of(&self, id: &NodeId) -> usize {
        let mut height = 0;
        let mut stack = vec![(id, 1)];
        while let Some((id, level)) = stack.pop() {
            height = height.max(level);
            if let Some(n) = self.nodes.get(id) {
                stack.extend(n.children.iter().map(|c| (c, level + 1)));
            }
        }
        height
    }

    fn is_self_or_descendant(&self, candidate: &NodeId, ancestor: &NodeId) -> bool {
        let mut cur = Some(candidate);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.nodes.get(c).and_then(|n| n.parent.as_ref());
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SeededHex;
    use crate::model::tests::*;

    const TOO_DEEP: crate::LoadLimitError = crate::LoadLimitError {
        resource: crate::LoadResource::TreeDepth,
        limit: 256,
        got: 257,
    };

    fn too_deep(error: &ModelError) -> bool {
        matches!(error, ModelError::LoadLimit(limit) if *limit == TOO_DEEP)
    }

    /// The root and a chain `n2`..=`n{levels}` under it.
    fn chain(levels: usize) -> Model {
        let mut model = Model::new();
        let mut parent = model.root().unwrap().clone();
        for level in 2..=levels {
            let id = NodeId::new(format!("n{level}")).unwrap();
            let node = ModelNode::new("link", ModelNodeKind::Group);
            model.add_node_with_id(id.clone(), &parent, node).unwrap();
            parent = id;
        }
        model
    }

    #[test]
    fn a_tree_is_at_most_256_levels_deep() {
        let id = |s: &str| NodeId::new(s).unwrap();
        let group = || ModelNode::new("g", ModelNodeKind::Group);
        let mut model = chain(256);
        let root = model.root().unwrap().clone();
        let before = model.generation();
        let got = model.add_node_with_id(id("over"), &id("n256"), group());
        assert!(got.as_ref().is_err_and(too_deep), "{got:?}");
        assert_eq!(model.generation(), before);

        model.add_node_with_id(id("side"), &root, group()).unwrap();
        model
            .add_node_with_id(id("below"), &id("side"), group())
            .unwrap();
        let got = model.reparent(&id("side"), &id("n255"));
        assert!(got.as_ref().is_err_and(too_deep), "{got:?}");
        model.reparent(&id("side"), &id("n254")).unwrap();
        model.reparent(&id("side"), &root).unwrap();
        // A copy sits beside its original, so it is never deeper.
        model
            .duplicate_subtree(&id("n256"), &mut SeededHex::new(1))
            .unwrap();

        let mut file = model.to_clm_file().unwrap();
        let mut over = file.doc.nodes.last().unwrap().clone();
        over.id = id("over");
        over.parent = Some(id("n256"));
        file.doc.nodes.push(over);
        let got = Model::from_clm_file(&file);
        assert!(
            matches!(&got, Err(LoadError::Limit(limit)) if *limit == TOO_DEEP),
            "{:?}",
            got.err()
        );

        let mut addon = model.extract(&[id("side")]).to_clm_file().unwrap();
        addon.doc.nodes[0].parent = Some(id("n255"));
        let addon = Model::from_clm_file_fragment(&addon).unwrap();
        let before = model.generation();
        match model.install(&addon) {
            Err(InstallError::Model(error)) => assert!(too_deep(&error), "{error}"),
            other => panic!("installed too deep: {other:?}"),
        }
        assert_eq!(model.generation(), before);
    }

    #[test]
    fn reparenting_leaves_the_generated_id_alone() {
        let mut r = fixture();
        let part = r.part.clone();
        assert!(
            part.as_str().starts_with("root/part-"),
            "generated under the root: {part}"
        );
        r.model.reparent(&part, &r.composite.clone()).unwrap();
        assert_eq!(
            r.model.node(&part).unwrap().parent(),
            Some(&r.composite),
            "the node moved",
        );
        assert!(
            r.model.node(&part).is_some(),
            "and kept the Id it was created with: {part}",
        );
        assert!(part.as_str().starts_with("root/part-"));
    }

    #[test]
    fn a_colliding_generated_id_is_redrawn() {
        // Hands out 0xaaaaaaaa twice, then 0xbbbbbbbb forever.
        let mut draws = 0u32;
        let mut hex = move || {
            draws += 1;
            if draws <= 2 { 0xaaaa_aaaa } else { 0xbbbb_bbbb }
        };
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let a = model
            .add_node(&root, ModelNode::new("a", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let b = model
            .add_node(&root, ModelNode::new("b", ModelNodeKind::Group), &mut hex)
            .unwrap();
        assert_eq!(a.as_str(), "root/group-aaaaaaaa");
        assert_eq!(
            b.as_str(),
            "root/group-bbbbbbbb",
            "the collision was redrawn"
        );
        assert_eq!(model.node_count(), 3);

        // A source that never moves runs the attempts out rather than looping.
        let mut stuck = || 0xaaaa_aaaa_u32;
        assert!(matches!(
            model.add_node(&root, ModelNode::new("c", ModelNodeKind::Group), &mut stuck),
            Err(ModelError::IdExhausted)
        ));
    }

    #[test]
    fn an_author_chosen_id_cannot_collide() {
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let chosen = NodeId::new("head").unwrap();
        model
            .add_node_with_id(
                chosen.clone(),
                &root,
                ModelNode::new("Head", ModelNodeKind::Group),
            )
            .unwrap();
        assert!(matches!(
            model.add_node_with_id(
                chosen,
                &root,
                ModelNode::new("Head again", ModelNodeKind::Group)
            ),
            Err(ModelError::DuplicateId(_))
        ));
    }

    #[test]
    fn reorder_moves_within_siblings_and_clamps() {
        let mut hex = SeededHex::new(1);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let mut add = |m: &mut Model, name: &str| {
            m.add_node(&root, ModelNode::new(name, ModelNodeKind::Group), &mut hex)
                .unwrap()
        };
        let a = add(&mut m, "a");
        let b = add(&mut m, "b");
        let c = add(&mut m, "c");
        let root = m.root().unwrap().clone();

        m.reorder(&c, 0).unwrap();
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[c.clone(), a.clone(), b.clone()]
        );

        m.reorder(&c, 99).unwrap();
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[a.clone(), b.clone(), c.clone()]
        );

        m.reorder(&b, 1).unwrap();
        assert_eq!(m.node(&root).unwrap().children(), &[a, b, c]);

        assert!(m.reorder(&root, 0).is_err());
    }

    #[test]
    fn duplicate_copies_bindings_and_remaps_internal_masks() {
        let mut hex = SeededHex::new(3);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let group = m
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let mask_src = m
            .add_node(&group, ModelNode::new("mask", part_kind()), &mut hex)
            .unwrap();
        let masked = m
            .add_node(&group, ModelNode::new("masked", part_kind()), &mut hex)
            .unwrap();
        m.mask_add(&masked, &mask_src, MaskMode::Mask).unwrap();

        let param = m.add_param(param("x"), &mut hex).unwrap();
        let key = BindingKey::new(
            param.clone(),
            masked,
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        m.set_binding_key(&key, [1, 0], 5.0).unwrap();

        let copy = m.duplicate_subtree(&group, &mut hex).unwrap();
        assert_ne!(copy, group);
        assert_eq!(
            m.node(&root).unwrap().children(),
            &[group.clone(), copy.clone()]
        );
        assert_eq!(m.node(&copy).unwrap().name.as_str(), "g copy");

        let copy_children = m.node(&copy).unwrap().children().to_vec();
        assert_eq!(copy_children.len(), 2);
        let copy_masked = &copy_children[1];
        let masks = m.node(copy_masked).unwrap().masks().unwrap();
        assert_eq!(masks.len(), 1);
        assert_eq!(masks[0].source(), &copy_children[0]);
        assert_eq!(m.bindings_of_param(&param).count(), 2);
        assert!(m.bindings().any(|b| b.node() == copy_masked));
        assert!(m.to_clm_bytes().is_ok());
    }

    #[test]
    fn duplicate_copies_the_welds_between_the_parts_it_copies() {
        let mut hex = SeededHex::new(3);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let group = m
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let upper = m
            .add_node(&group, ModelNode::new("upper", part_kind()), &mut hex)
            .unwrap();
        let lower = m
            .add_node(&group, ModelNode::new("lower", part_kind()), &mut hex)
            .unwrap();
        let outside = m
            .add_node(&root, ModelNode::new("outside", part_kind()), &mut hex)
            .unwrap();
        for part in [&upper, &lower, &outside] {
            m.slot_add(part, slot("seam")).unwrap();
            m.slot_fill(part, &slot("seam"), 1).unwrap();
        }
        m.set_welds(vec![
            ModelWeld::new(upper.clone(), lower.clone(), vec![pair("seam", 0.25)]),
            ModelWeld::new(upper.clone(), outside.clone(), vec![pair("seam", 0.5)]),
        ])
        .unwrap();

        let copy = m.duplicate_subtree(&group, &mut hex).unwrap();
        let copies = m.node(&copy).unwrap().children().to_vec();
        let (upper_copy, lower_copy) = (&copies[0], &copies[1]);

        assert_eq!(m.welds().len(), 3, "one weld came along, one stayed");
        let weld = m
            .welds()
            .iter()
            .find(|w| w.a() == upper_copy && w.b() == lower_copy)
            .expect("the weld inside the subtree is copied between the copies");
        assert_eq!(weld.pairs(), [pair("seam", 0.25)]);
        assert_eq!(weld.resolve(&m).len(), 1, "and it resolves on their slots");
        assert!(
            !m.welds()
                .iter()
                .any(|w| copies.contains(w.a()) && w.b() == &outside),
            "a weld that leaves the subtree stays with the original"
        );
        Model::from_clm_bytes(&m.to_clm_bytes().unwrap()).expect("the copy saves and loads");
    }

    #[test]
    fn cross_references_are_checked_before_they_land() {
        let mut r = fixture();
        // A texture, param and node this model no longer has.
        let orphan_tex = r.model.add_texture(&r.part, texture(), &mut r.hex).unwrap();
        r.model.delete_texture(&orphan_tex).unwrap();
        let orphan_param = r.model.add_param(param("gone"), &mut r.hex).unwrap();
        r.model.delete_param(&orphan_param).unwrap();
        let orphan_node = r.node("gone", ModelNodeKind::Group);
        r.model.delete_node(&orphan_node).unwrap();

        assert!(matches!(
            r.model.set_part_albedo(&r.part, Some(orphan_tex.clone())),
            Err(ModelError::UnknownTexture(id)) if id == orphan_tex
        ));
        assert!(matches!(
            r.model.set_part_albedo(&r.composite, Some(r.tex.clone())),
            Err(ModelError::NotAPart)
        ));
        assert!(matches!(
            r.model.set_welds(vec![ModelWeld::new(
                r.part.clone(),
                orphan_node,
                Vec::new(),
            )]),
            Err(ModelError::UnknownNode(_))
        ));
        assert!(
            matches!(
                r.model.set_welds(vec![ModelWeld::new(
                    r.part.clone(),
                    r.composite.clone(),
                    Vec::new(),
                )]),
                Err(ModelError::WeldEndNotAPart)
            ),
            "a weld's end has to be a part",
        );
        assert!(
            matches!(
                r.model.set_welds(vec![ModelWeld::new(
                    r.part.clone(),
                    r.other.clone(),
                    vec![pair("ghost", 0.5)],
                )]),
                Err(ModelError::WeldUnknownSlot(_))
            ),
            "a weld's pair has to name a slot the part carries",
        );
        let mut foreign = ModelPart::new(quad());
        foreign.albedo = Some(orphan_tex);
        let root = r.root.clone();
        assert!(matches!(
            r.model.add_node(
                &root,
                ModelNode::new("smuggler", ModelNodeKind::Part(foreign)),
                &mut r.hex,
            ),
            Err(ModelError::UnknownTexture(_))
        ));
        assert!(r.model.to_clm_bytes().is_ok());
    }
}
