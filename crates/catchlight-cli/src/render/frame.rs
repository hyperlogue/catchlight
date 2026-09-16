//! One continuing Puppet per request. Bounds, sidecars, overlays and traces
//! observe the same evaluated frame; capture decimation never changes ticks.
use super::spec::{bad, Initial, Physics, ResolvedRequest};
use crate::Error;
use catchlight_core::{Model, NodeId, Pose, Puppet};
use std::collections::BTreeMap;

pub struct FrameRuntime {
    pub puppet: Puppet,
    timestep: f32,
    pub index: u32,
}
impl FrameRuntime {
    pub fn new(
        model: &Model,
        request: &ResolvedRequest,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        let mut puppet = Puppet::new(model);
        puppet.apply_pose(
            &request
                .pose
                .iter()
                .map(|(id, v)| (id.clone(), *v))
                .collect::<Pose>(),
        );
        let timestep = request
            .animation
            .as_ref()
            .map_or(0.0, |a| a.clip.0.timestep);
        // Native lanes supply frame-zero controls before initialization. Their
        // evaluator, including Stepped/Cubic behavior, remains the core method.
        if let Some(animation) = &request.animation {
            for lane in &animation.clip.0.lanes {
                puppet.set_param_value(&lane.param, lane.value_at(0.0));
            }
        }
        puppet.set_physics_enabled(!matches!(request.physics, Physics::Off {}));
        match request.physics {
            Physics::Settled {}
            | Physics::Simulate {
                initial: Initial::Settled,
                ..
            } => puppet.settle_physics(model),
            _ => {}
        }
        if let Physics::Simulate { warmup_frames, .. } = request.physics {
            for _ in 0..warmup_frames {
                if cancelled() {
                    return Err(bad("render cancelled"));
                }
                puppet.tick(model, timestep);
            }
        }
        if let Some(animation) = &request.animation {
            puppet.set_animations(vec![animation.clip.0.clone()]);
            if !puppet.play_animation(&animation.clip.0.name) {
                return Err(bad("selected animation could not start"));
            }
        }
        puppet.tick(model, 0.0);
        Ok(Self {
            puppet,
            timestep,
            index: 0,
        })
    }
    pub fn advance(&mut self, model: &Model) {
        self.puppet.tick(model, self.timestep);
        self.index += 1;
    }
    pub fn timestep(&self) -> f32 {
        self.timestep
    }
}

pub fn color_retained(request: &ResolvedRequest, id: &NodeId) -> bool {
    request
        .only_parts
        .as_ref()
        .is_none_or(|parts| parts.contains(id))
        && !request.hide_color.contains(id)
}

/// Mask edits affect only this command's private clone. Restoring through
/// ordinary edits keeps its generation monotonic, avoiding cache-key aliasing
/// between equally sized, differently stripped variants.
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
