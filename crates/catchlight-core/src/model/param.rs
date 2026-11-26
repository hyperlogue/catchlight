//! Params, the named values a pose sets.

use super::*;

#[derive(Debug, Clone)]
pub struct ModelParam {
    pub name: Name,
    pub min: f32,
    pub max: f32,
    /// In param-value space.
    pub default: f32,
}

impl ModelParam {
    #[must_use]
    pub fn new(name: Name, min: f32, max: f32, default: f32) -> Self {
        Self {
            name,
            min,
            max,
            default,
        }
    }
}

impl Model {
    pub fn param_ids(&self) -> &[ParamId] {
        &self.param_order
    }

    pub fn param(&self, id: &ParamId) -> Option<&ModelParam> {
        self.params.get(id)
    }

    pub fn add_param(
        &mut self,
        param: ModelParam,
        hex: &mut impl HexSource,
    ) -> Result<ParamId, ModelError> {
        let id = self.mint(ParamId::generate, |m, id| m.params.contains_key(id), hex)?;
        self.add_param_with_id(id.clone(), param)?;
        Ok(id)
    }

    pub fn add_param_with_id(&mut self, id: ParamId, param: ModelParam) -> Result<(), ModelError> {
        if !param_range_is_valid(param.min, param.max) {
            return Err(ModelError::InvalidParamRange);
        }
        if !param.default.is_finite() {
            return Err(ModelError::NonFinite("a param's default"));
        }
        if self.params.contains_key(&id) {
            return Err(ModelError::DuplicateId(id.clone().into()));
        }
        self.params.insert(id.clone(), param);
        if self.is_fragment()
            && self.response_owner(&id).is_some()
            && let Err(error) = self.validate_responses()
        {
            self.params.remove(&id);
            return Err(error.into());
        }
        self.param_order.push(id);
        self.bump();
        Ok(())
    }

    /// A two-param binding naming it is removed whole.
    pub fn delete_param(&mut self, id: &ParamId) -> Result<(), ModelError> {
        if self.params.remove(id).is_none() {
            return Err(ModelError::UnknownParam(id.clone()));
        }
        self.param_order.retain(|p| p != id);
        self.bindings.retain(|b| !b.key.params.contains(id));
        for node in self.nodes.values_mut() {
            match &mut node.kind {
                ModelNodeKind::MotionResponse(r) => {
                    r.inputs.retain(
                        |t| !matches!(&t.source,crate::ResponseSource::PoseParam { param: p } if p==id),
                    );
                    if r.output_param.as_ref() == Some(id) {
                        r.output_param = None;
                    }
                }
                ModelNodeKind::Spine(spine) if spine.reads(id) => {
                    for t in Arc::make_mut(&mut spine.targets) {
                        if t.as_ref() == Some(id) {
                            *t = None;
                        }
                    }
                }
                _ => {}
            }
        }
        self.response_index.take();
        self.remove_in_extensions(|r| matches!(r, IdRef::Param(p) if p == id));
        self.bump();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::*;

    fn spine_targets(m: &Model, node: &NodeId) -> Vec<Option<ParamId>> {
        match m.node(node).map(|n| &n.kind) {
            Some(ModelNodeKind::Spine(sp)) => sp.targets().to_vec(),
            other => panic!("not a spine: {other:?}"),
        }
    }

    #[test]
    fn deleting_a_param_leaves_a_rigid_link_behind() {
        let mut r = fixture();
        let spine = r.spine();
        let param = r.param.clone();
        r.model
            .set_spine_targets(&spine, vec![Some(param.clone()), None])
            .unwrap();

        r.model.delete_param(&param).unwrap();
        assert_eq!(spine_targets(&r.model, &spine), vec![None, None]);
    }
}
