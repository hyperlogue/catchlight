//! A response reads the pose, named part slots and its immediate parent.
//! Parent-before-child tree order is therefore also evaluation order. Only
//! one driver may own an art output; an ordinary spine can still consume it.
use std::collections::HashMap;

use super::{Model, ModelError, ModelNodeKind};
use crate::{MotionResponse, NodeId, ParamId, ResponseSource};

impl Model {
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
        if self.response_count == 0 {
            return Ok(());
        }
        for (id, node) in &self.nodes {
            if matches!(&node.kind,ModelNodeKind::MotionResponse(r) if r.output_param.as_ref()==Some(param))
            {
                return Err(ModelError::InvalidResponse {
                    node: id.clone(),
                    reason: format!("reserved output {param}"),
                });
            }
        }
        Ok(())
    }

    pub(super) fn validate_responses(&self) -> Result<(), ModelError> {
        if self.response_count == 0 {
            return Ok(());
        }
        let mut owners = HashMap::new();
        let fragment = self.is_fragment();
        for (id, node) in &self.nodes {
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
                if let Some(other) = owners.insert(p, id) {
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
        if owners.is_empty() {
            return Ok(());
        }
        let check = |p: &ParamId, reason: &str| -> Result<(), ModelError> {
            if let Some(&id) = owners.get(p) {
                return Err(ModelError::InvalidResponse {
                    node: id.clone(),
                    reason: format!("{p} {reason}"),
                });
            }
            Ok(())
        };
        for node in self.nodes.values() {
            match &node.kind {
                ModelNodeKind::SimplePhysics(p) => {
                    for p in p.target_params().iter().flatten() {
                        check(p, "is also driven by a pendulum")?;
                    }
                }
                ModelNodeKind::Spine(s) if s.chain().is_some() => {
                    for p in s.targets().iter().flatten() {
                        check(p, "is also driven by a particle chain")?;
                    }
                }
                ModelNodeKind::MotionResponse(r) => {
                    for t in &r.inputs {
                        if let ResponseSource::PoseParam(p) = &t.source {
                            check(p, "is a response output and cannot be a pose input")?;
                        }
                    }
                }
                _ => {}
            }
        }
        for binding in &self.bindings {
            if binding.key.params.iter().any(|p| owners.contains_key(p)) {
                self.validate_response_binding(binding)?;
            }
        }
        for lane in self.animations.iter().flat_map(|a| &a.lanes) {
            check(&lane.param, "also has an animation lane")?;
        }
        Ok(())
    }
}
