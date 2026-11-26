//! One continuing Puppet per request. Bounds, sidecars, overlays and traces
//! observe the same evaluated frame; capture decimation never changes ticks.
use super::spec::{MotionResponseMode, ResolvedRequest, bad};
use crate::Error;
use catchlight_core::{Model, NodeId, Pose, Puppet};
use catchlight_extension_animations::{AnimationPlayer, LiveInputs, Rig};
use catchlight_extension_expressions::Weights;
use catchlight_extension_humanoid::ControlValues;
use std::collections::BTreeMap;

pub struct FrameRuntime {
    pub puppet: Puppet,
    pub player: AnimationPlayer,
    /// The request's controls, expressions and visemes, applied every frame
    /// under the clip's lanes. Validation already found every name.
    inputs: LiveInputs,
    timestep: f32,
    pub index: u32,
}
impl FrameRuntime {
    pub fn new(model: &Model, request: &ResolvedRequest) -> Result<Self, Error> {
        let mut puppet = Puppet::new(model);
        // Before the first frame, whose controls and expressions skip a param
        // a running motion response drives.
        if let MotionResponseMode::On { response_clock } = request.motion_responses {
            puppet.set_response_clock(response_clock);
        }
        puppet.set_motion_responses_enabled(!matches!(
            request.motion_responses,
            MotionResponseMode::Off {}
        ));
        puppet.apply_pose(
            &request
                .pose
                .iter()
                .map(|(id, v)| (id.clone(), *v))
                .collect::<Pose>(),
        );
        let weights = |named: &BTreeMap<String, f32>| -> Weights {
            named
                .iter()
                .map(|(name, weight)| (name.as_str(), *weight))
                .collect()
        };
        let inputs = LiveInputs {
            controls: request
                .controls
                .iter()
                .map(|(name, value)| (name.as_str(), *value))
                .collect::<ControlValues>(),
            expressions: weights(&request.expressions),
            visemes: weights(&request.visemes),
        };
        let timestep = request
            .animation
            .as_ref()
            .map_or(0.0, |a| a.clip.0.timestep);
        if let Some(animation) = &request.animation {
            for lane in &animation.clip.0.lanes {
                if let Some(param) = lane.param() {
                    puppet.set_param_value(param, lane.value_at(0.0));
                }
            }
        }
        let mut player = AnimationPlayer::default();
        if let Some(animation) = &request.animation {
            player.set_clips(vec![animation.clip.0.clone()]);
            if !player.play(&animation.clip.0.name) {
                return Err(bad("selected animation could not start"));
            }
        }
        player.advance_with(&mut puppet, 0.0, Rig::of(model), &inputs);
        puppet.tick(model, 0.0);
        Ok(Self {
            puppet,
            player,
            inputs,
            timestep,
            index: 0,
        })
    }
    pub fn advance(&mut self, model: &Model) {
        self.player.advance_with(
            &mut self.puppet,
            self.timestep,
            Rig::of(model),
            &self.inputs,
        );
        self.puppet.tick(model, self.timestep);
        self.index += 1;
    }
    #[must_use]
    pub fn timestep(&self) -> f32 {
        self.timestep
    }
}

#[must_use]
pub fn color_retained(request: &ResolvedRequest, id: &NodeId) -> bool {
    request
        .only_parts
        .as_ref()
        .is_none_or(|parts| parts.contains(id))
        && !request.hide_color.contains(id)
}

/// Restoring through ordinary edits keeps the generation monotonic, avoiding
/// cache-key aliasing between equally sized, differently stripped variants.
pub fn configure_masks(
    model: &mut Model,
    original: &Model,
    previous: &mut Vec<NodeId>,
    request: &ResolvedRequest,
) -> Result<(), Error> {
    for id in previous.drain(..) {
        let Some(catchlight_core::ModelNodeKind::Part(part)) = original.node(&id).map(|n| &n.kind)
        else {
            return Err(bad("mask owner disappeared"));
        };
        let wanted: Vec<_> = part
            .masks()
            .iter()
            .map(|m| (m.source().clone(), m.mode()))
            .collect();
        let Some(catchlight_core::ModelNodeKind::Part(current)) = model.node(&id).map(|n| &n.kind)
        else {
            return Err(bad("mask owner disappeared"));
        };
        for i in (0..current.masks().len()).rev() {
            model.mask_delete(&id, i)?;
        }
        for (source, mode) in wanted {
            model.mask_add(&id, &source, mode)?;
        }
    }
    let mut by_node = BTreeMap::<_, Vec<_>>::new();
    for edge in &request.strip_masks {
        by_node
            .entry(edge.node.clone())
            .or_default()
            .push(&edge.source);
    }
    for (id, sources) in by_node {
        let Some(catchlight_core::ModelNodeKind::Part(part)) = model.node(&id).map(|n| &n.kind)
        else {
            return Err(bad("mask owner disappeared"));
        };
        let remove: Vec<_> = part
            .masks()
            .iter()
            .enumerate()
            .filter(|(_, m)| sources.contains(&m.source()))
            .map(|(i, _)| i)
            .rev()
            .collect();
        for i in remove {
            model.mask_delete(&id, i)?;
        }
        previous.push(id);
    }
    Ok(())
}
