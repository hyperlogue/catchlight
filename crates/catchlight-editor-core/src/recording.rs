//! A gesture records only its delta onto one binding cell. Other bindings'
//! contributions belong to the preview, never to the key being authored.
//! Capture before scratch is applied and keep the normalized input position for
//! the gesture. Each destination binding owns its grid, so a recording resolves
//! or inserts keys independently. An empty binding receives an explicit identity
//! rest cell before the changed cell; raw writes retain exact caller data.

use catchlight_core::{
    deform_cells, scalar_cells, BindingKey, BindingParams, BindingTarget, Model, ModelNodeKind,
    NodeId, NodeKind, Pose, Puppet, ScalarTarget,
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

/// A recording plan, translated by a frontend into one atomic edit request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingWrite {
    pub target: String,
    pub key_positions: Vec<Vec<f32>>,
    pub inserts: Vec<RecordingKeyInsert>,
    pub cells: Vec<RecordingCell>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingKeyInsert {
    pub axis: catchlight_core::ParamId,
    pub value: f32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingCell {
    pub cell: [u32; 2],
    pub value: RecordingValue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingValue {
    Scalar(f32),
    Offsets(Vec<[f32; 2]>),
}

pub struct Recording {
    pub posed: RecordProperties,
    base: RecordProperties,
    keys: Vec<(ScalarTarget, f32)>,
    deform: Vec<f32>,
    model: Model,
    node: NodeId,
    params: BindingParams,
    position: [f32; 2],
}

impl Recording {
    pub fn capture(
        model: &Model,
        puppet: &Puppet,
        node: &NodeId,
        params: BindingParams,
        position: [f32; 2],
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
            let at = position[axis];
            if !at.is_finite() || !(0.0..=1.0).contains(&at) {
                return Err("Choose a normalized recording position within 0..1.".into());
            }
            let wanted = p.min + at * (p.max - p.min);
            if (puppet.param_value(id).unwrap_or(p.default) - wanted).abs()
                > (p.max - p.min).abs().max(1.0) * 1e-5
            {
                return Err("The pose moved away from the recording keypoint.".into());
            }
        }
        if params.y().is_none() && position[1] != 0.0 {
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
        let pose: Pose = params
            .iter()
            .enumerate()
            .filter_map(|(axis, id)| {
                model
                    .param(id)
                    .map(|p| (id.clone(), p.min + position[axis] * (p.max - p.min)))
            })
            .collect();
        let keys = base_values
            .scalars()
            .into_iter()
            .map(|(target, _)| {
                let value = model
                    .eval_scalar(&key(BindingTarget::Scalar(target)), &pose)
                    .unwrap_or(target.identity());
                (target, value)
            })
            .collect();
        let deform = if model.node_mesh(node).is_some() {
            model
                .eval_deform(&key(BindingTarget::Deform), &pose)
                .unwrap_or_else(|| vec![0.0; model.node_mesh(node).map_or(0, |m| m.verts.len())])
        } else {
            Vec::new()
        };
        Ok(Self {
            posed: posed_values,
            base: base_values,
            keys,
            deform,
            model: model.clone(),
            node: node.clone(),
            params,
            position,
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

    /// Plan exact writes on each property's own grid. The identity rest cell
    /// is an explicit recording policy, never a side effect of a model setter.
    pub fn writes(
        &self,
        patch: &RecordProperties,
        authored_basis: bool,
    ) -> Result<Vec<RecordingWrite>, String> {
        self.patch(patch, authored_basis)?
            .into_iter()
            .map(|(target, value)| {
                self.write(BindingTarget::Scalar(target), RecordingValue::Scalar(value))
            })
            .collect()
    }

    pub fn deform_write(&self, deltas: &[f32]) -> Result<RecordingWrite, String> {
        let offsets = self.deform(deltas)?.as_chunks::<2>().0.to_vec();
        self.write(BindingTarget::Deform, RecordingValue::Offsets(offsets))
    }

    fn write(
        &self,
        target: BindingTarget,
        value: RecordingValue,
    ) -> Result<RecordingWrite, String> {
        let key = BindingKey {
            params: self.params.clone(),
            node: self.node.clone(),
            target,
        };
        let unauthored = self.model.binding(&key).is_none_or(|b| {
            scalar_cells(b.values()).map_or_else(
                || deform_cells(b.values()).is_none_or(|c| c.is_empty()),
                |c| c.is_empty(),
            )
        });
        let mut model = self.model.clone();
        model.add_binding(&key).map_err(|e| e.to_string())?;
        let key_positions = model
            .binding(&key)
            .ok_or("Missing recording binding.")?
            .key_positions()
            .to_vec();
        let mut inserts = Vec::new();
        let mut rest = [0.0; 2];
        for (axis, param) in self.params.iter().enumerate() {
            let p = model.param(param).ok_or("Missing recording param.")?;
            rest[axis] = if p.max > p.min {
                ((p.default - p.min) / (p.max - p.min)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let positions = if unauthored {
                vec![rest[axis], self.position[axis]]
            } else {
                vec![self.position[axis]]
            };
            for value in positions {
                if !model
                    .binding(&key)
                    .ok_or("Missing recording binding.")?
                    .key_positions()[axis]
                    .contains(&value)
                {
                    model
                        .key_insert(&key, param, value)
                        .map_err(|e| e.to_string())?;
                    inserts.push(RecordingKeyInsert {
                        axis: param.clone(),
                        value,
                    });
                }
            }
        }
        let locate = |position: [f32; 2]| -> Result<[u32; 2], String> {
            let axes = model
                .binding(&key)
                .ok_or("Missing recording binding.")?
                .key_positions();
            let mut cell = [0; 2];
            for (axis, values) in axes.iter().enumerate() {
                cell[axis] = values
                    .iter()
                    .position(|v| *v == position[axis])
                    .ok_or("Missing recording position.")? as u32;
            }
            Ok(cell)
        };
        let destination = locate(self.position)?;
        let mut cells = Vec::new();
        if unauthored {
            let rest_cell = locate(rest)?;
            if rest_cell != destination {
                let identity = match target {
                    BindingTarget::Scalar(t) => RecordingValue::Scalar(t.identity()),
                    BindingTarget::Deform => {
                        RecordingValue::Offsets(vec![[0.0; 2]; self.deform.len() / 2])
                    }
                };
                cells.push(RecordingCell {
                    cell: rest_cell,
                    value: identity,
                });
            }
        }
        cells.push(RecordingCell {
            cell: destination,
            value,
        });
        Ok(RecordingWrite {
            target: target.name().to_owned(),
            key_positions,
            inserts,
            cells,
        })
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
    fn fixture(default: f32) -> (Model, catchlight_core::ParamId, NodeId) {
        use catchlight_core::{ModelNode, ModelParam, ModelPart, Name};
        let mut model = Model::new();
        let param = catchlight_core::ParamId::new("drive").unwrap();
        model
            .add_param_with_id(
                param.clone(),
                ModelParam::new(Name::new("Drive").unwrap(), 0.0, 1.0, default),
            )
            .unwrap();
        let node = NodeId::new("panel").unwrap();
        model
            .add_node_with_id(
                node.clone(),
                &model.root().unwrap().clone(),
                ModelNode::new(
                    "Panel",
                    ModelNodeKind::Part(ModelPart::new(
                        catchlight_core::formats::clm::ClmMesh::default(),
                    )),
                ),
            )
            .unwrap();
        (model, param, node)
    }

    fn at(
        model: &Model,
        param: &catchlight_core::ParamId,
        node: &NodeId,
        position: f32,
    ) -> Recording {
        let mut puppet = Puppet::new(model);
        puppet.set_physics_enabled(false);
        puppet.set_param_value(param, position);
        puppet.tick(model, 0.0);
        Recording::capture(
            model,
            &puppet,
            node,
            BindingParams::One(param.clone()),
            [position, 0.0],
        )
        .unwrap()
    }

    fn apply(
        model: &mut Model,
        param: &catchlight_core::ParamId,
        node: &NodeId,
        write: &RecordingWrite,
    ) {
        let target = BindingTarget::parse(&write.target).unwrap();
        let key = BindingKey::new(param.clone(), node.clone(), target);
        model
            .add_binding_with_positions(&key, write.key_positions.clone())
            .unwrap();
        for insert in &write.inserts {
            model.key_insert(&key, &insert.axis, insert.value).unwrap();
        }
        for cell in &write.cells {
            match &cell.value {
                RecordingValue::Scalar(value) => {
                    model.set_binding_key(&key, cell.cell, *value).unwrap()
                }
                RecordingValue::Offsets(value) => model
                    .set_deform_vertices(&key, cell.cell, value.concat())
                    .unwrap(),
            }
        }
    }

    #[test]
    fn first_recording_explicitly_seeds_the_default_and_the_between_key_pose() {
        let (mut model, param, node) = fixture(0.4);
        let captured = at(&model, &param, &node, 0.7);
        let writes = captured
            .writes(
                &RecordProperties {
                    translate: Some([10.0, 0.0, 0.0]),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0]
                .inserts
                .iter()
                .map(|i| i.value)
                .collect::<Vec<_>>(),
            vec![0.4, 0.7]
        );
        assert_eq!(
            writes[0].cells.iter().map(|c| c.cell).collect::<Vec<_>>(),
            vec![[1, 0], [2, 0]]
        );
        assert!(
            model.bindings().next().is_none(),
            "planning does not mutate the model"
        );
        apply(&mut model, &param, &node, &writes[0]);
        let key = BindingKey::new(
            param.clone(),
            node.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        for (position, expected) in [(0.4, 0.0), (0.7, 10.0)] {
            let pose = [(param.clone(), position)].into_iter().collect();
            assert_eq!(model.eval_scalar(&key, &pose), Some(expected));
        }
    }

    #[test]
    fn recording_resolves_each_property_on_its_own_grid_and_preserves_existing_cells() {
        let (mut model, param, node) = fixture(0.0);
        for (target, positions) in [
            (ScalarTarget::Tx, vec![0.0, 0.25, 1.0]),
            (ScalarTarget::Ty, vec![0.0, 0.75, 1.0]),
        ] {
            let key = BindingKey::new(param.clone(), node.clone(), BindingTarget::Scalar(target));
            model
                .add_binding_with_positions(&key, vec![positions])
                .unwrap();
            model.set_binding_key(&key, [0, 0], 0.0).unwrap();
            model.set_binding_key(&key, [2, 0], 20.0).unwrap();
        }
        let captured = at(&model, &param, &node, 0.5);
        let before = captured.posed.translate.unwrap();
        let writes = captured
            .writes(
                &RecordProperties {
                    translate: Some([before[0] + 3.0, before[1] + 7.0, before[2]]),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert_eq!(
            writes.iter().map(|w| w.cells[0].cell).collect::<Vec<_>>(),
            vec![[2, 0], [1, 0]]
        );
        assert!(writes.iter().all(|w| w.cells.len() == 1));
        for write in &writes {
            apply(&mut model, &param, &node, write);
        }
        for (target, value) in [(ScalarTarget::Tx, 13.0), (ScalarTarget::Ty, 17.0)] {
            let key = BindingKey::new(param.clone(), node.clone(), BindingTarget::Scalar(target));
            assert_eq!(model.scalar_value_at(&key, [3, 0]).unwrap(), 20.0);
            let pose = [(param.clone(), 0.5)].into_iter().collect();
            assert_eq!(model.eval_scalar(&key, &pose), Some(value));
        }
    }

    #[test]
    fn an_empty_mesh_records_authored_empty_deform_cells() {
        let (model, param, node) = fixture(0.0);
        let write = at(&model, &param, &node, 1.0).deform_write(&[]).unwrap();
        assert_eq!(write.cells.len(), 2);
        assert!(write
            .cells
            .iter()
            .all(|c| matches!(&c.value, RecordingValue::Offsets(v) if v.is_empty())));
    }

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
