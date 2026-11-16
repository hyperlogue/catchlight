//! A response reads the pose, named part slots and its immediate parent.
//! Parent-before-child tree order is therefore also evaluation order. Only
//! one driver may own an art output; an ordinary spine can still consume it.
use std::collections::HashMap;

use super::{Model, ModelError, ModelNodeKind};
use crate::{MotionResponse, NodeId, ParamId, ResponseSource};

/// Derived once per model revision. Stable node order also fixes which
/// diagnostic a native server and a separately loaded replica return first.
#[derive(Clone, Debug, Default)]
pub(super) struct ResponseIndex {
    rows: Vec<NodeId>,
    owners: HashMap<ParamId, NodeId>,
    validated: std::sync::OnceLock<()>,
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

    pub(super) fn remove_counted_node(&mut self, id: &NodeId) {
        if self
            .nodes
            .remove(id)
            .is_some_and(|n| matches!(n.kind, ModelNodeKind::MotionResponse(_)))
        {
            self.response_count -= 1;
        }
    }

    pub fn set_motion_response(
        &mut self,
        id: &NodeId,
        value: MotionResponse,
    ) -> Result<(), ModelError> {
        if !matches!(
            self.node(id).map(|n| &n.kind),
            Some(ModelNodeKind::MotionResponse(_))
        ) {
            return Err(ModelError::InvalidResponse {
                node: id.clone(),
                reason: "expected a motion response node".into(),
            });
        }
        self.update_node(id, |node| node.kind = ModelNodeKind::MotionResponse(value))
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
        if self.response_count == 0 {
            return Ok(());
        }
        let index = self.response_index();
        if index.validated.get().is_some() {
            return Ok(());
        }
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
                        let p = node.parent().and_then(|p| self.node(p));
                        if !(matches!(p.map(|n| &n.kind), Some(ModelNodeKind::MotionResponse(_)))
                            || (p.is_none() && fragment && node.parent().is_some()))
                        {
                            return Err(invalid(
                                "Parent requires an immediate motion response parent".into(),
                            ));
                        }
                    }
                    ResponseSource::PoseParam(p) if self.param(p).is_none() && !fragment => {
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
                        if (self.node(part).is_some() || !fragment)
                            && self.slot(part, slot).is_none()
                        {
                            return Err(invalid(format!("unknown part slot {part}/{slot}")));
                        }
                    }
                    _ => {}
                }
            }
            if let Some(p) = &r.output_param {
                if let Some(other) = self.response_owner(p).filter(|other| *other != id) {
                    return Err(invalid(format!("{p} is already driven by {other}")));
                }
                match self.param(p) {
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
            if let Some(id) = self.response_owner(p) {
                return Err(ModelError::InvalidResponse {
                    node: id.clone(),
                    reason: format!("{p} {reason}"),
                });
            }
            Ok(())
        };
        for node in index.rows.iter().filter_map(|id| self.node(id)) {
            if let ModelNodeKind::MotionResponse(r) = &node.kind {
                for t in &r.inputs {
                    if let ResponseSource::PoseParam(p) = &t.source {
                        check(p, "is a response output and cannot be a pose input")?;
                    }
                }
            }
        }
        for binding in &self.bindings {
            if binding
                .key
                .params
                .iter()
                .any(|p| self.response_owner(p).is_some())
            {
                self.validate_response_binding(binding)?;
            }
        }
        for lane in self.animations.iter().flat_map(|a| &a.lanes) {
            check(&lane.param, "also has an animation lane")?;
        }
        let _ = index.validated.set(());
        Ok(())
    }
}
