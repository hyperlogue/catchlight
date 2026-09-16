//! Native client composition over captured model facts. Every compound edit
//! carries the revision read with those facts; preview scratch stays local.
use super::*;
use catchlight_editor_core::{Recording, RecordingValue, RecordingWrite};
use catchlight_editor_protocol::{MaskMode, SlotId, SlotPair};

impl App {
    pub(super) fn compose(
        &mut self,
        session: SessionId,
        build: impl FnOnce(&Model) -> Result<Vec<EditOp>, String>,
    ) {
        self.compose_guarded(session, None, build);
    }

    /// A gesture keeps its original guard through both capture and publication.
    pub(super) fn compose_guarded(
        &mut self,
        session: SessionId,
        expected: Option<u64>,
        build: impl FnOnce(&Model) -> Result<Vec<EditOp>, String>,
    ) {
        match self.editor.with_model_revision(session, |model, rev| {
            let edits = if expected.is_some_and(|expected| expected != rev) {
                Err("edit cancelled: the model changed; start the gesture again".into())
            } else {
                build(model)
            };
            (expected.unwrap_or(rev), edits)
        }) {
            Ok((rev, Ok(edits))) if !edits.is_empty() => {
                self.send(Command::EditApply {
                    session,
                    if_rev: rev,
                    edits,
                });
            }
            Ok((_, Ok(_))) => {}
            Ok((_, Err(error))) => self.status = error,
            Err(error) => self.status = error.to_string(),
        }
    }

    /// The union is a controller discovery aid. It never defines ownership or
    /// supplies a binding's cell index; each row resolves its own grid.
    pub(super) fn discovered_positions(&self) -> HashMap<ParamId, Vec<f32>> {
        self.session
            .and_then(|s| {
                self.editor
                    .with_model(s, |model| {
                        model
                            .param_ids()
                            .iter()
                            .map(|id| (id.clone(), positions(model, id)))
                            .collect()
                    })
                    .ok()
            })
            .unwrap_or_default()
    }

    pub(super) fn edit_param_positions(
        &mut self,
        session: SessionId,
        param: &ParamId,
        insert: Option<f32>,
        delete: Option<u32>,
        flip: bool,
    ) {
        self.compose(session, |model| {
            let removed = delete.and_then(|i| positions(model, param).get(i as usize).copied());
            let mut edits = Vec::new();
            for binding in model.bindings_of_param(param) {
                let Some(axis) = binding.params().axis_of(param).map(usize::from) else {
                    continue;
                };
                let params = wire_params(binding.params());
                let node = binding.node().clone();
                let target = binding.target().into();
                let grid = &binding.key_positions()[axis];
                if let Some(value) = insert {
                    if !grid.contains(&value) {
                        edits.push(EditOp::BindingKeyInsert {
                            node,
                            params,
                            target,
                            axis: param.clone(),
                            value,
                        });
                    }
                } else if let Some(value) = removed {
                    if let Some(index) = grid.iter().position(|v| *v == value) {
                        edits.push(EditOp::BindingKeyDelete {
                            node,
                            params,
                            target,
                            axis: param.clone(),
                            index: index as u32,
                        });
                    }
                } else if flip {
                    let mirrored: Vec<_> = grid.iter().rev().map(|v| 1.0 - v).collect();
                    // Move toward the free space first, keeping every
                    // intermediate axis ordered without replacing the binding.
                    // Its list position and interpolation affect evaluation.
                    for index in (0..grid.len())
                        .filter(|i| mirrored[*i] < grid[*i])
                        .chain((0..grid.len()).rev().filter(|i| mirrored[*i] > grid[*i]))
                    {
                        edits.push(EditOp::BindingKeyMove {
                            node: node.clone(),
                            params: params.clone(),
                            target,
                            axis: param.clone(),
                            index: index as u32,
                            value: mirrored[index],
                        });
                    }
                    let mut cells = authored_cells(binding);
                    if !cells.is_empty() {
                        edits.push(EditOp::BindingCellsUnset {
                            node: node.clone(),
                            params: params.clone(),
                            target,
                            cells: cells.iter().map(|c| c.cell).collect(),
                        });
                    }
                    for c in &mut cells {
                        c.cell[axis] = grid.len() as u32 - 1 - c.cell[axis];
                    }
                    if !cells.is_empty() {
                        edits.push(EditOp::BindingCellsSet {
                            node: node.clone(),
                            params: params.clone(),
                            target,
                            cells,
                        });
                    }
                }
            }
            Ok(edits)
        });
    }
}

pub(super) fn positions(model: &Model, param: &ParamId) -> Vec<f32> {
    let mut positions: Vec<_> = model
        .bindings_of_param(param)
        .flat_map(|b| {
            b.params()
                .axis_of(param)
                .into_iter()
                .flat_map(|axis| b.key_positions()[usize::from(axis)].iter().copied())
        })
        .collect();
    if positions.is_empty() {
        positions.extend([0.0, 1.0]);
    }
    positions.sort_by(f32::total_cmp);
    positions.dedup();
    positions
}

pub(super) fn capture(
    model: &Model,
    node: &NodeId,
    params: &BindingParams,
    pose: &HashMap<ParamId, f32>,
) -> Result<Recording, String> {
    let core_params = binding_key(params, node, CoreBindingTarget::Deform).params;
    let mut position = [0.0; 2];
    for (axis, id) in core_params.iter().enumerate() {
        let p = model.param(id).ok_or("Recording param disappeared.")?;
        position[axis] = if p.max > p.min {
            ((pose.get(id).copied().unwrap_or(p.default) - p.min) / (p.max - p.min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }
    let mut puppet = catchlight_core::Puppet::new(model);
    puppet.set_physics_enabled(false);
    puppet.apply_pose(&pose.iter().map(|(id, v)| (id.clone(), *v)).collect());
    puppet.tick(model, 0.0);
    Recording::capture(model, &puppet, node, core_params, position)
}

pub(super) fn recording_edits(
    node: &NodeId,
    params: &BindingParams,
    writes: Vec<RecordingWrite>,
) -> Result<Vec<EditOp>, String> {
    let mut edits = Vec::new();
    for write in writes {
        let target = CoreBindingTarget::parse(&write.target)
            .ok_or("Unknown recording target.")?
            .into();
        edits.push(EditOp::BindingAdd {
            node: node.clone(),
            params: params.clone(),
            target,
            key_positions: Some(write.key_positions),
        });
        for insert in write.inserts {
            edits.push(EditOp::BindingKeyInsert {
                node: node.clone(),
                params: params.clone(),
                target,
                axis: insert.axis,
                value: insert.value,
            });
        }
        edits.push(EditOp::BindingCellsSet {
            node: node.clone(),
            params: params.clone(),
            target,
            cells: write
                .cells
                .into_iter()
                .map(|c| BindingCellWrite {
                    cell: c.cell,
                    value: match c.value {
                        RecordingValue::Scalar(v) => BindingCellValue::Scalar(v),
                        RecordingValue::Offsets(v) => BindingCellValue::Offsets(v),
                    },
                })
                .collect(),
        });
    }
    Ok(edits)
}

fn authored_cells(binding: &catchlight_core::ModelBinding) -> Vec<BindingCellWrite> {
    if let Some(cells) = catchlight_core::scalar_cells(binding.values()) {
        cells
            .iter()
            .map(|c| BindingCellWrite {
                cell: [c.x, c.y],
                value: BindingCellValue::Scalar(c.value),
            })
            .collect()
    } else {
        catchlight_core::deform_cells(binding.values())
            .into_iter()
            .flatten()
            .map(|c| BindingCellWrite {
                cell: [c.x, c.y],
                value: BindingCellValue::Offsets(pairs(&c.value)),
            })
            .collect()
    }
}

pub(super) fn binding_operation(
    model: &Model,
    params: &BindingParams,
    node: &NodeId,
    target: BindingTarget,
    cell: [u32; 2],
    op: BindingOp,
) -> Result<Vec<EditOp>, String> {
    let key = binding_key(params, node, target.into());
    let binding = model.binding(&key).ok_or("Binding disappeared.")?;
    let node = node.clone();
    let params = params.clone();
    Ok(vec![match op {
        BindingOp::Unset => EditOp::BindingCellsUnset {
            node,
            params,
            target,
            cells: vec![cell],
        },
        BindingOp::Reset => EditOp::BindingCellsSet {
            node,
            params,
            target,
            cells: vec![BindingCellWrite {
                cell,
                value: match key.target {
                    CoreBindingTarget::Scalar(t) => BindingCellValue::Scalar(t.identity()),
                    CoreBindingTarget::Deform => {
                        BindingCellValue::Offsets(vec![[0.0; 2]; model.deform_len(&key.node) / 2])
                    }
                },
            }],
        },
        BindingOp::Delete => EditOp::BindingDelete {
            node,
            params,
            target,
        },
        BindingOp::Interpolate(mode) => EditOp::BindingInterpolationSet {
            node,
            params,
            target,
            mode,
        },
        BindingOp::Invert => {
            let mut cells = authored_cells(binding);
            for c in &mut cells {
                match &mut c.value {
                    BindingCellValue::Scalar(value) => *value = -*value,
                    BindingCellValue::Offsets(values) => {
                        for value in values {
                            value[0] = -value[0];
                            value[1] = -value[1];
                        }
                    }
                }
            }
            if cells.is_empty() {
                return Ok(Vec::new());
            }
            EditOp::BindingCellsSet {
                node,
                params,
                target,
                cells,
            }
        }
        BindingOp::Copy | BindingOp::Paste => {
            return Err("Clipboard action needs its source.".into())
        }
    }])
}

pub(super) fn mask_edit(
    model: &Model,
    node: NodeId,
    index: u32,
    mode: Option<MaskMode>,
    to: Option<u32>,
) -> Result<Vec<EditOp>, String> {
    let masks = match model.node(&node).map(|n| &n.kind) {
        Some(ModelNodeKind::Part(p)) => p.masks(),
        Some(ModelNodeKind::Composite(p)) => p.masks(),
        _ => return Err("The node has no masks.".into()),
    };
    let mut values: Vec<_> = masks
        .iter()
        .map(|m| (m.source().clone(), m.mode().into()))
        .collect();
    let item = values
        .get_mut(index as usize)
        .ok_or("The mask disappeared.")?;
    if let Some(mode) = mode {
        item.1 = mode;
    }
    if let Some(to) = to {
        let item = values.remove(index as usize);
        values.insert((to as usize).min(values.len()), item);
    }
    let mut edits: Vec<_> = (0..masks.len())
        .rev()
        .map(|i| EditOp::MaskDelete {
            node: node.clone(),
            index: i as u32,
        })
        .collect();
    edits.extend(values.into_iter().map(|(source, mode)| EditOp::MaskAdd {
        node: node.clone(),
        source,
        mode,
    }));
    Ok(edits)
}

pub(super) fn weld_weight(
    model: &Model,
    a: NodeId,
    b: NodeId,
    slot: SlotId,
    weight: f32,
) -> Result<Vec<EditOp>, String> {
    let weld = model
        .welds()
        .iter()
        .find(|w| (w.a() == &a && w.b() == &b) || (w.a() == &b && w.b() == &a))
        .ok_or("Weld disappeared.")?;
    let reversed = weld.a() != &a;
    let mut pairs: Vec<_> = weld
        .pairs()
        .iter()
        .map(|p| SlotPair {
            a: p.a.clone(),
            b: p.b.clone(),
            weight: p.weight,
        })
        .collect();
    let pair = pairs
        .iter_mut()
        .find(|p| if reversed { p.b == slot } else { p.a == slot })
        .ok_or("Weld slot disappeared.")?;
    pair.weight = if reversed { 1.0 - weight } else { weight };
    Ok(vec![EditOp::WeldSet {
        a: weld.a().clone(),
        b: weld.b().clone(),
        pairs,
    }])
}
