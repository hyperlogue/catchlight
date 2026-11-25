//! One continuing Puppet per request. Bounds, sidecars, overlays and traces
//! observe the same evaluated frame; capture decimation never changes ticks.
use super::spec::{bad, MotionResponseMode, ResolvedRequest};
use crate::Error;
use catchlight_core::extension::ExtensionError;
use catchlight_core::{Model, NodeId, Pose, Puppet};
use catchlight_extension_animations::AnimationPlayer;
use catchlight_extension_expressions::{Expressions, Weights};
use catchlight_extension_humanoid::{ControlValues, Humanoid};
use std::collections::BTreeMap;

pub struct FrameRuntime {
    pub puppet: Puppet,
    pub player: AnimationPlayer,
    timestep: f32,
    pub index: u32,
}
impl FrameRuntime {
    pub fn new(model: &Model, request: &ResolvedRequest) -> Result<Self, Error> {
        let mut puppet = Puppet::new(model);
        // Before the controls and expressions, which skip a param a running
        // motion response drives.
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
        pose_controls(model, request, &mut puppet)?;
        let timestep = request
            .animation
            .as_ref()
            .map_or(0.0, |a| a.clip.0.timestep);
        if let Some(animation) = &request.animation {
            for lane in &animation.clip.0.lanes {
                puppet.set_param_value(&lane.param, lane.value_at(0.0));
            }
        }
        let mut player = AnimationPlayer::default();
        if let Some(animation) = &request.animation {
            player.set_clips(vec![animation.clip.0.clone()]);
            if !player.play(&animation.clip.0.name) {
                return Err(bad("selected animation could not start"));
            }
        }
        player.advance(&mut puppet, 0.0);
        puppet.tick(model, 0.0);
        Ok(Self {
            puppet,
            player,
            timestep,
            index: 0,
        })
    }
    pub fn advance(&mut self, model: &Model) {
        self.player.advance(&mut self.puppet, self.timestep);
        self.puppet.tick(model, self.timestep);
        self.index += 1;
    }
    pub fn timestep(&self) -> f32 {
        self.timestep
    }
}

/// The order the expressions crate gives a host: control values, the
/// control settings, the control mappings, then the param settings, visemes
/// over expressions each time. Validation already found every name.
fn pose_controls(
    model: &Model,
    request: &ResolvedRequest,
    puppet: &mut Puppet,
) -> Result<(), Error> {
    if request.controls.is_empty() && request.expressions.is_empty() && request.visemes.is_empty() {
        return Ok(());
    }
    let mut controls: ControlValues = request
        .controls
        .iter()
        .map(|(name, value)| (name.as_str(), *value))
        .collect();
    let weights = |named: &BTreeMap<String, f32>| -> Weights {
        named
            .iter()
            .map(|(name, weight)| (name.as_str(), *weight))
            .collect()
    };
    let (emotions, visemes) = (weights(&request.expressions), weights(&request.visemes));
    let expressions = if emotions.is_empty() && visemes.is_empty() {
        None
    } else {
        Some(
            model
                .extension::<Expressions>()
                .map_err(|e| bad(format!("the model's expressions cannot be read: {e}")))?,
        )
    };
    if let Some(expressions) = expressions {
        expressions.blend_controls(&emotions, &visemes, &mut controls);
    }
    match model.extension::<Humanoid>() {
        Ok(humanoid) => humanoid.pose(&controls, puppet),
        Err(ExtensionError::Missing) => {}
        Err(e) => {
            return Err(bad(format!(
                "the model's control mappings cannot be read: {e}"
            )))
        }
    }
    if let Some(expressions) = expressions {
        expressions.blend_params(&emotions, &visemes, puppet);
    }
    Ok(())
}

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
