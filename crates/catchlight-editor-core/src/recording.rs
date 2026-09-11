//! A gesture records only its delta onto one binding cell. Other bindings'
//! contributions belong to the preview, never to the key being authored.
//! Capture before scratch is applied and keep the destination for the gesture.

use catchlight_core::{
    BindingKey, BindingParams, BindingTarget, Model, ModelNodeKind, NodeId, NodeKind, Puppet,
    ScalarTarget,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordProperties {
    pub translate: Option<[f32; 3]>,
    pub rotate: Option<[f32; 3]>,
    pub scale: Option<[f32; 2]>,
    pub z_order: Option<f32>,
    pub opacity: Option<f32>,
    pub tint: Option<[f32; 3]>,
    pub screen_tint: Option<[f32; 3]>,
}

impl RecordProperties {
    fn scalars(&self) -> Vec<(ScalarTarget, f32)> {
        use ScalarTarget as T;
        let mut out = Vec::new();
        if let Some(v) = self.translate {
            out.extend([(T::Tx, v[0]), (T::Ty, v[1])]);
        }
        if let Some(v) = self.rotate {
            out.extend([(T::Rx, v[0]), (T::Ry, v[1]), (T::Rz, v[2])]);
        }
        if let Some(v) = self.scale {
            out.extend([(T::Sx, v[0]), (T::Sy, v[1])]);
        }
        if let Some(v) = self.z_order {
            out.push((T::ZOrder, v));
        }
        if let Some(v) = self.opacity {
            out.push((T::Opacity, v));
        }
        if let Some(v) = self.tint {
            out.extend([(T::TintR, v[0]), (T::TintG, v[1]), (T::TintB, v[2])]);
        }
        if let Some(v) = self.screen_tint {
            out.extend([
                (T::ScreenTintR, v[0]),
                (T::ScreenTintG, v[1]),
                (T::ScreenTintB, v[2]),
            ]);
        }
        out
    }
}

pub struct Recording {
    pub posed: RecordProperties,
    base: RecordProperties,
    keys: Vec<(ScalarTarget, f32)>,
    deform: Vec<f32>,
}

impl Recording {
    pub fn capture(
        model: &Model,
        puppet: &Puppet,
        node: &NodeId,
        params: BindingParams,
        cell: [u32; 2],
    ) -> Result<Self, String> {
        let base = model
            .node(node)
            .ok_or("The selected node no longer exists.")?;
        let posed = puppet
            .node_idx(node)
            .and_then(|i| puppet.get(i))
            .ok_or("The selected node has no pose.")?;
        for (axis, id) in params.iter().enumerate() {
            let p = model
                .param(id)
                .ok_or("The selected param no longer exists.")?;
            let at = p
                .key_positions
                .get(cell[axis] as usize)
                .ok_or("Choose an existing key position.")?;
            let wanted = p.min + at * (p.max - p.min);
            if (puppet.param_value(id).unwrap_or(p.default) - wanted).abs()
                > (p.max - p.min).abs().max(1.0) * 1e-5
            {
                return Err("The pose moved away from the recording keypoint.".into());
            }
        }
        if params.y().is_none() && cell[1] != 0 {
            return Err("A one-param binding has one row.".into());
        }
        let mut base_values = RecordProperties {
            translate: Some(base.transform.translation),
            rotate: Some(base.transform.rotation),
            scale: Some(base.transform.scale),
            z_order: Some(base.z_order),
            ..Default::default()
        };
        let mut posed_values = RecordProperties {
            translate: Some(posed.transform.translation.to_array()),
            rotate: Some(posed.transform.rotation.to_array()),
            scale: Some(posed.transform.scale.to_array()),
            z_order: Some(posed.z_order),
            ..Default::default()
        };
        let base_color = match &base.kind {
            ModelNodeKind::Part(p) => Some((p.opacity, p.tint, p.screen_tint)),
            ModelNodeKind::Composite(p) => Some((p.opacity, p.tint, p.screen_tint)),
            _ => None,
        };
        let pose_color = match &posed.kind {
            NodeKind::Part(p) => Some((p.opacity, p.tint.to_array(), p.screen_tint.to_array())),
            NodeKind::Composite(p) => {
                Some((p.opacity, p.tint.to_array(), p.screen_tint.to_array()))
            }
            _ => None,
        };
        if let Some((opacity, tint, screen)) = base_color {
            base_values.opacity = Some(opacity);
            base_values.tint = Some(tint);
            base_values.screen_tint = Some(screen);
        }
        if let Some((opacity, tint, screen)) = pose_color {
            posed_values.opacity = Some(opacity);
            posed_values.tint = Some(tint);
            posed_values.screen_tint = Some(screen);
        }
        let key = |target| BindingKey {
            params: params.clone(),
            node: node.clone(),
            target,
        };
        let keys = base_values
            .scalars()
            .into_iter()
            .map(|(target, _)| {
                let value = model
                    .scalar_value_at(&key(BindingTarget::Scalar(target)), cell)
                    .unwrap_or(target.identity());
                (target, value)
            })
            .collect();
        let deform = if model.node_mesh(node).is_some() {
            model
                .deform_value_at(&key(BindingTarget::Deform), cell)
                .map_err(|e| e.to_string())?
        } else {
            Vec::new()
        };
        Ok(Self {
            posed: posed_values,
            base: base_values,
            keys,
            deform,
        })
    }

    /// `authored_basis` adapts ordinary arrangement handles: they return an
    /// edited base value, while a recording inspector returns an edited pose.
    pub fn patch(
        &self,
        patch: &RecordProperties,
        authored_basis: bool,
    ) -> Result<Vec<(ScalarTarget, f32)>, String> {
        let basis = if authored_basis {
            &self.base
        } else {
            &self.posed
        }
        .scalars();
        let mut result = Vec::new();
        if let Some(v) = patch.translate {
            let before = if authored_basis {
                &self.base
            } else {
                &self.posed
            }
            .translate
            .unwrap_or_default();
            if v[2] != before[2] {
                return Err("Depth translation cannot be recorded. Use draw order instead.".into());
            }
        }
        for (target, after) in patch.scalars() {
            let before = basis
                .iter()
                .find(|(t, _)| *t == target)
                .ok_or("This property cannot be recorded on this node.")?
                .1;
            if !after.is_finite() {
                return Err("Enter a finite value.".into());
            }
            if (after - before).abs() <= 1e-6 {
                continue;
            }
            let current = self
                .keys
                .iter()
                .find(|(t, _)| *t == target)
                .map_or(target.identity(), |(_, v)| *v);
            result.push((target, record_value(target, current, before, after)?));
        }
        Ok(result)
    }

    pub fn deform(&self, deltas: &[f32]) -> Result<Vec<f32>, String> {
        if deltas.len() != self.deform.len() || deltas.iter().any(|v| !v.is_finite()) {
            return Err("The mesh changed during this gesture. Start the gesture again.".into());
        }
        Ok(self
            .deform
            .iter()
            .zip(deltas)
            .map(|(key, delta)| key + delta)
            .collect())
    }
}

fn record_value(target: ScalarTarget, key: f32, before: f32, after: f32) -> Result<f32, String> {
    let value = if target.identity() == 1.0 {
        if before.abs() < 1e-8 {
            return Err("This property is zero in the starting pose. Restore a nonzero value before recording it.".into());
        }
        key * (after / before)
    } else {
        key + (after - before)
    };
    if value.is_finite() {
        Ok(value)
    } else {
        Err("The recorded value is outside the supported range.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_only_the_gesture_delta_beside_other_bindings() {
        assert_eq!(
            record_value(ScalarTarget::Tx, 20., 135., 140.).unwrap(),
            25.
        );
        assert_eq!(record_value(ScalarTarget::Sx, 2., 6., 9.).unwrap(), 3.);
        assert_eq!(
            record_value(ScalarTarget::ScreenTintR, 0.1, 0.3, 0.4).unwrap(),
            0.19999999
        );
    }
    #[test]
    fn zero_multiplicative_pose_refuses_instead_of_authoring_a_false_key() {
        assert!(record_value(ScalarTarget::Opacity, 1., 0., 0.5).is_err());
    }
}
