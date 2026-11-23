//! A response reads the pose, named part slots and its immediate parent.
//! Parent-before-child tree order is therefore also evaluation order. Only
//! one driver may own an art output; an ordinary spine can still consume it.
use std::collections::HashMap;

use super::{Model, ModelError, ModelNode, ModelNodeKind};
use crate::{MotionResponse, NodeId, ParamId, ResponseSource};

/// Stable order fixes which diagnostic a native server and a separately
/// loaded replica return first.
#[derive(Clone, Debug, Default)]
pub(super) struct ResponseIndex {
    rows: Vec<NodeId>,
    owners: HashMap<ParamId, NodeId>,
}

/// Before committing a rename, both IDs resolve to the old declaration.
fn declaration_id<'a, I: Eq>(id: &'a I, rename: Option<(&'a I, &I)>) -> &'a I {
    match rename {
        Some((old, new)) if id == new => old,
        _ => id,
    }
}

impl Model {
    fn response_index(&self) -> &ResponseIndex {
        self.response_index.get_or_init(|| {
            let mut index = ResponseIndex::default();
            if self.response_count != 0 {
                index.rows.extend(
                    self.nodes
                        .iter()
                        .filter(|(_, node)| matches!(node.kind, ModelNodeKind::MotionResponse(_)))
                        .map(|(id, _)| id.clone()),
                );
                index.rows.sort_unstable();
                for id in &index.rows {
                    if let Some(ModelNodeKind::MotionResponse(r)) = self.node(id).map(|n| &n.kind) {
                        if let Some(p) = &r.output_param {
                            index.owners.entry(p.clone()).or_insert_with(|| id.clone());
                        }
                    }
                }
            }
            std::sync::Arc::new(index)
        })
    }

    pub(super) fn response_owner(&self, param: &ParamId) -> Option<&NodeId> {
        self.response_index().owners.get(param)
    }

    fn responses(&self) -> impl Iterator<Item = (&ModelNode, &MotionResponse)> {
        self.response_index().rows.iter().filter_map(|id| {
            let node = self.node(id)?;
            match &node.kind {
                ModelNodeKind::MotionResponse(response) => Some((node, response)),
                _ => None,
            }
        })
    }

    pub(super) fn resolves_response_node(&self, id: &NodeId) -> bool {
        self.is_fragment()
            && self.responses().any(|(node, r)| {
                r.inputs.iter().any(|input| match &input.source {
                    ResponseSource::Parent => node.parent() == Some(id),
                    ResponseSource::PartSlot { part, .. } => part == id,
                    _ => false,
                })
            })
    }

    pub(super) fn remove_counted_node(&mut self, id: &NodeId) {
        if self
            .nodes
            .remove(id)
            .is_some_and(|n| matches!(n.kind, ModelNodeKind::MotionResponse(_)))
        {
            self.response_count -= 1;
            self.response_index.take();
        }
    }

    pub fn set_motion_response(
        &mut self,
        id: &NodeId,
        value: MotionResponse,
    ) -> Result<(), ModelError> {
        if !matches!(
            self.node(id).ok_or(ModelError::UnknownNode)?.kind,
            ModelNodeKind::MotionResponse(_)
        ) {
            return Err(ModelError::NotMotionResponse);
        }
        self.update_node_kind(id, |kind| *kind = ModelNodeKind::MotionResponse(value))
    }

    pub(super) fn check_response_writer(&self, param: &ParamId) -> Result<(), ModelError> {
        if let Some(id) = self.response_owner(param) {
            return Err(ModelError::InvalidResponse {
                node: id.clone(),
                reason: format!("reserved output {param}"),
            });
        }
        Ok(())
    }

    pub(super) fn validate_responses(&self) -> Result<(), ModelError> {
        self.validate_response_rename(None, None)
    }

    /// Check how existing fragment references will resolve before mutating.
    pub(super) fn validate_response_rename(
        &self,
        node_rename: Option<(&NodeId, &NodeId)>,
        param_rename: Option<(&ParamId, &ParamId)>,
    ) -> Result<(), ModelError> {
        if self.response_count == 0 {
            return Ok(());
        }
        let index = self.response_index();
        let declaration = |p: &ParamId| self.param(declaration_id(p, param_rename));
        let owner = |p: &ParamId| {
            if let Some((old, new)) = param_rename.filter(|(old, new)| p == *old || p == *new) {
                return index
                    .owners
                    .get(old)
                    .into_iter()
                    .chain(index.owners.get(new))
                    .min();
            }
            index.owners.get(p)
        };
        let fragment = self.is_fragment();
        for id in &index.rows {
            let Some(node) = self.node(id) else { continue };
            let ModelNodeKind::MotionResponse(r) = &node.kind else {
                continue;
            };
            let invalid = |reason: String| ModelError::InvalidResponse {
                node: id.clone(),
                reason,
            };
            if !r.period_s.is_finite()
                || r.period_s <= 0.0
                || !r.damping_ratio.is_finite()
                || r.damping_ratio <= 0.0
            {
                return Err(invalid(
                    "period and damping ratio must be finite and positive".into(),
                ));
            }
            for input in &r.inputs {
                if !input.gain.is_finite() {
                    return Err(invalid("input gain must be finite".into()));
                }
                match &input.source {
                    ResponseSource::Parent => {
                        let p = node
                            .parent()
                            .and_then(|p| self.node(declaration_id(p, node_rename)));
                        if !(matches!(p.map(|n| &n.kind), Some(ModelNodeKind::MotionResponse(_)))
                            || (p.is_none() && fragment && node.parent().is_some()))
                        {
                            return Err(invalid(
                                "Parent requires an immediate motion response parent".into(),
                            ));
                        }
                    }
                    ResponseSource::PoseParam { param: p }
                        if declaration(p).is_none() && !fragment =>
                    {
                        return Err(invalid(format!("unknown input param {p}")))
                    }
                    ResponseSource::PartSlot {
                        part,
                        slot,
                        angle_deg,
                        reference_length,
                    } => {
                        if !angle_deg.is_finite()
                            || !reference_length.is_finite()
                            || *reference_length <= 0.0
                        {
                            return Err(invalid("slot projection requires a finite angle and positive reference length".into()));
                        }
                        let declared = declaration_id(part, node_rename);
                        if (self.node(declared).is_some() || !fragment)
                            && self.slot(declared, slot).is_none()
                        {
                            return Err(invalid(format!("unknown part slot {part}/{slot}")));
                        }
                    }
                    _ => {}
                }
            }
            if let Some(p) = &r.output_param {
                if let Some(other) = owner(p).filter(|other| *other != id) {
                    return Err(invalid(format!("{p} is already driven by {other}")));
                }
                match declaration(p) {
                    Some(param) if param.default == 0.0 && param.min <= 0.0 && param.max >= 0.0 => {
                    }
                    None if fragment => {}
                    _ => {
                        return Err(invalid(format!(
                            "output {p} needs a declared range containing zero and a zero default"
                        )))
                    }
                }
            }
        }
        let check = |p: &ParamId, reason: &str| -> Result<(), ModelError> {
            if let Some(id) = owner(p) {
                return Err(ModelError::InvalidResponse {
                    node: id.clone(),
                    reason: format!("{p} {reason}"),
                });
            }
            Ok(())
        };
        for (_, response) in self.responses() {
            for term in &response.inputs {
                if let ResponseSource::PoseParam { param } = &term.source {
                    check(param, "is a response output and cannot be a pose input")?;
                }
            }
        }
        for binding in &self.bindings {
            if binding.key.params.iter().any(|p| owner(p).is_some()) {
                Self::validate_response_binding_with(binding, |p| {
                    owner(p)?;
                    let param = declaration(p)?;
                    Some(super::binding::normalized_zero(param.min, param.max))
                })?;
            }
        }
        for lane in self.animations.iter().flat_map(|a| &a.lanes) {
            check(&lane.param, "also has an animation lane")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SeededHex;

    #[test]
    fn duplicate_includes_all_copied_responses_in_the_index_immediately() {
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        let mut ids = SeededHex::new(17);
        let row = || {
            ModelNode::new(
                "response",
                ModelNodeKind::MotionResponse(MotionResponse::default()),
            )
        };
        let parent = model.add_node(&root, row(), &mut ids).unwrap();
        model.add_node(&parent, row(), &mut ids).unwrap();
        assert_eq!(model.responses().count(), 2);
        model.duplicate_subtree(&parent, &mut ids).unwrap();
        assert_eq!(model.responses().count(), 4);
    }
}
