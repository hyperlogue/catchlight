//! Renaming a node, param or texture Id, and every reference to it.

use super::*;

impl Model {
    /// Rewrites every reference. No alias is kept, so addons naming the old Id break.
    pub fn rename_node_id(&mut self, old: &NodeId, new: NodeId) -> Result<(), ModelError> {
        if old == &new {
            return Ok(());
        }
        if !self.nodes.contains_key(old) {
            return Err(ModelError::UnknownNode);
        }
        if self.nodes.contains_key(&new) {
            return Err(ModelError::DuplicateId(new.to_string()));
        }
        if self.resolves_response_node(&new) {
            self.validate_response_rename(Some((old, &new)), None)?;
        }
        let node = self.nodes.remove(old).ok_or(ModelError::UnknownNode)?;
        let parent = node.parent.clone();
        let children = node.children.clone();
        self.nodes.insert(new.clone(), node);
        self.response_index.take();
        if let Some(p) = parent.and_then(|p| self.nodes.get_mut(&p)) {
            for c in Arc::make_mut(&mut p.children) {
                if c == old {
                    *c = new.clone();
                }
            }
        }
        for child in children.iter() {
            if let Some(c) = self.nodes.get_mut(child) {
                c.parent = Some(new.clone());
            }
        }
        for node in self.nodes.values_mut() {
            if !node
                .masks()
                .is_some_and(|masks| masks.iter().any(|m| &m.source == old))
            {
                continue;
            }
            if let Some(masks) = node.masks_mut() {
                for m in masks.iter_mut() {
                    if &m.source == old {
                        m.source = new.clone();
                    }
                }
            }
        }
        for b in &mut self.bindings {
            if &b.key.node == old {
                b.key.node = new.clone();
            }
        }
        for w in &mut self.welds {
            if &w.a == old {
                w.a = new.clone();
            }
            if &w.b == old {
                w.b = new.clone();
            }
        }
        for r in &mut self.roots {
            if r == old {
                *r = new.clone();
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::MotionResponse(r) = &mut node.kind {
                for t in &mut r.inputs {
                    if let crate::ResponseSource::PartSlot { part, .. } = &mut t.source
                        && part == old
                    {
                        *part = new.clone();
                    }
                }
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Node(id) if id == old => Some(IdRef::Node(new.clone())),
            IdRef::Slot(id, slot) if id == old => Some(IdRef::Slot(new.clone(), slot.clone())),
            _ => None,
        });
        self.bump();
        Ok(())
    }

    pub fn rename_param_id(&mut self, old: &ParamId, new: ParamId) -> Result<(), ModelError> {
        if old == &new {
            return Ok(());
        }
        if !self.params.contains_key(old) {
            return Err(ModelError::UnknownParam);
        }
        if self.params.contains_key(&new) {
            return Err(ModelError::DuplicateId(new.to_string()));
        }
        if self.is_fragment()
            && (self.response_owner(old).is_some() || self.response_owner(&new).is_some())
        {
            self.validate_response_rename(None, Some((old, &new)))?;
        }
        let param = self.params.remove(old).ok_or(ModelError::UnknownParam)?;
        self.params.insert(new.clone(), param);
        for p in &mut self.param_order {
            if p == old {
                *p = new.clone();
            }
        }
        for b in &mut self.bindings {
            match &mut b.key.params {
                BindingParams::One(p) => {
                    if p == old {
                        *p = new.clone();
                    }
                }
                BindingParams::Two(x, y) => {
                    if x == old {
                        *x = new.clone();
                    }
                    if y == old {
                        *y = new.clone();
                    }
                }
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Param(id) if id == old => Some(IdRef::Param(new.clone())),
            _ => None,
        });
        for node in self.nodes.values_mut() {
            match &mut node.kind {
                ModelNodeKind::Spine(spine) if spine.reads(old) => {
                    for t in Arc::make_mut(&mut spine.targets) {
                        if t.as_ref() == Some(old) {
                            *t = Some(new.clone());
                        }
                    }
                }
                ModelNodeKind::MotionResponse(r) => {
                    if r.output_param.as_ref() == Some(old) {
                        r.output_param = Some(new.clone());
                    }
                    for t in &mut r.inputs {
                        if let crate::ResponseSource::PoseParam { param: p } = &mut t.source
                            && p == old
                        {
                            *p = new.clone();
                        }
                    }
                }
                _ => {}
            }
        }
        self.response_index.take();
        self.bump();
        Ok(())
    }

    pub fn rename_tex_id(&mut self, old: &TexId, new: TexId) -> Result<(), ModelError> {
        if old == &new {
            return Ok(());
        }
        if !self.textures.contains_key(old) {
            return Err(ModelError::UnknownTexture);
        }
        if self.textures.contains_key(&new) {
            return Err(ModelError::DuplicateId(new.to_string()));
        }
        let texture = self
            .textures
            .remove(old)
            .ok_or(ModelError::UnknownTexture)?;
        self.textures.insert(new.clone(), texture);
        for t in &mut self.texture_order {
            if t == old {
                *t = new.clone();
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::Part(p) = &mut node.kind
                && p.albedo.as_ref() == Some(old)
            {
                p.albedo = Some(new.clone());
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Texture(id) if id == old => Some(IdRef::Texture(new.clone())),
            _ => None,
        });
        self.bump();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::*;

    #[test]
    fn renaming_rewrites_every_reference_to_the_id() {
        let mut r = fixture();
        let (part, other, composite, response) = (
            r.part.clone(),
            r.other.clone(),
            r.composite.clone(),
            r.response.clone(),
        );
        let (param, tex) = (r.param.clone(), r.tex.clone());
        let child = r
            .model
            .add_node(&part, ModelNode::new("child", part_kind()), &mut r.hex)
            .unwrap();
        r.model.set_part_albedo(&part, Some(tex.clone())).unwrap();
        r.model
            .set_motion_response(
                &response,
                crate::MotionResponse {
                    inputs: vec![crate::ResponseInput {
                        source: crate::ResponseSource::PoseParam {
                            param: param.clone(),
                        },
                        gain: 1.0,
                    }],
                    output_param: None,
                    ..Default::default()
                },
            )
            .unwrap();
        r.model
            .set_welds(vec![ModelWeld::new(part.clone(), other, Vec::new())])
            .unwrap();
        let key = BindingKey::new(param.clone(), part.clone(), BindingTarget::Deform);
        r.model
            .set_deform_vertices(&key, [1, 0], vec![1.0; 8])
            .unwrap();

        let new_node = NodeId::new("head/part-renamed").unwrap();
        r.model.rename_node_id(&part, new_node.clone()).unwrap();

        assert!(r.model.node(&part).is_none());
        assert!(r.model.node(&new_node).is_some());
        assert!(
            r.model
                .node(&r.root)
                .unwrap()
                .children()
                .contains(&new_node)
        );
        assert_eq!(r.model.node(&child).unwrap().parent(), Some(&new_node));
        assert_eq!(
            r.model.node(&composite).unwrap().masks().unwrap()[0].source(),
            &new_node
        );
        assert_eq!(r.model.bindings().next().unwrap().node(), &new_node);
        assert_eq!(r.model.welds()[0].a(), &new_node);
        assert!(r.model.to_clm_bytes().is_ok());

        let new_param = ParamId::new("head-turn").unwrap();
        r.model.rename_param_id(&param, new_param.clone()).unwrap();
        assert_eq!(r.model.param_ids(), std::slice::from_ref(&new_param));
        assert_eq!(
            r.model.bindings().next().unwrap().params(),
            &BindingParams::One(new_param.clone())
        );
        assert_eq!(
            response_input(&r.model, &response),
            Some(new_param),
            "the response node follows its param",
        );

        let new_tex = TexId::new("skin").unwrap();
        r.model.rename_tex_id(&tex, new_tex.clone()).unwrap();
        assert_eq!(r.model.texture_ids(), std::slice::from_ref(&new_tex));
        assert_eq!(albedo(&r.model, &new_node), Some(new_tex));
        assert!(r.model.to_clm_bytes().is_ok());

        let new_root = NodeId::new("body").unwrap();
        let old_root = r.root.clone();
        r.model.rename_node_id(&old_root, new_root.clone()).unwrap();
        assert_eq!(r.model.root(), Some(&new_root));
        assert!(r.model.to_clm_bytes().is_ok());
    }

    #[test]
    fn renaming_onto_a_taken_id_is_refused() {
        let mut r = fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        assert!(matches!(
            r.model.rename_node_id(&part, other),
            Err(ModelError::DuplicateId(_))
        ));
        // Renaming to the same Id is a no-op, not a conflict.
        r.model.rename_node_id(&part, part.clone()).unwrap();
        assert!(r.model.node(&part).is_some());
    }
}
