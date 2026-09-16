//! Model-only execution shared by standalone commands and atomic batches.
//!
//! The caller owns a private model and publication. No operation reads session
//! state, allocates an ID, performs IO, or publishes a partial result. Selections
//! validate completely before writing. A batch stops at the first error and its
//! caller discards the candidate; final authored equality decides publication.

use std::collections::HashSet;

use catchlight_core::{
    BindingKey, BindingTarget as CoreTarget, Model, ModelError, ModelNode, ModelNodeKind,
};
use catchlight_editor_core::ModelMeshExt as _;
use catchlight_editor_protocol::*;

use super::{
    apply_patch, binding_key, build_mesh, build_weld, default_name, emptied_reply, joins,
    make_kind, pairs_the_same_parts, EditorError,
};

pub const MAX_EDIT_OPS: usize = 1024;
pub const MAX_BINDING_CELLS: usize = 65_536;

/// In-process and wire callers share count and encoded argument budgets.
pub fn check_batch(edits: &[EditOp]) -> Result<(), EditorError> {
    if edits.len() > MAX_EDIT_OPS {
        return Err(EditorError::Limit {
            resource: "edit_ops",
            limit: MAX_EDIT_OPS as u64,
            requested: edits.len() as u64,
        });
    }
    Ok(())
}

pub fn execute(model: &mut Model, edits: Vec<EditOp>) -> Result<Vec<ResponseBody>, EditorError> {
    check_batch(&edits)?;
    edits
        .into_iter()
        .enumerate()
        .map(|(index, edit)| {
            apply(model, edit).map_err(|source| EditorError::Operation {
                index: index as u32,
                source: Box::new(source),
            })
        })
        .collect()
}

pub fn apply(model: &mut Model, edit: EditOp) -> Result<ResponseBody, EditorError> {
    match edit {
        EditOp::NodeAdd {
            parent,
            kind,
            name,
            node,
        } => {
            model.add_node_with_id(
                node.clone(),
                &parent,
                ModelNode::new(name.unwrap_or_else(|| default_name(kind)), make_kind(kind)),
            )?;
            return Ok(ResponseBody::Node {
                node,
                dropped: Vec::new(),
            });
        }
        EditOp::NodeSet { node, patch } => {
            let mut dropped = Vec::new();
            if let Some(albedo) = &patch.texture {
                if let Some(tex) = albedo {
                    if model.texture(tex).is_none() {
                        return Err(EditorError::NoTexture(tex.clone()));
                    }
                }
                if matches!(
                    model.node(&node).map(|n| &n.kind),
                    Some(ModelNodeKind::Part(_))
                ) {
                    dropped.extend(model.texture_dropped_by_repointing(&node, albedo.as_ref()));
                    model.set_part_albedo(&node, albedo.clone())?;
                }
            }
            model.update_node(&node, |n| apply_patch(n, &patch))??;
            return Ok(ResponseBody::Node { node, dropped });
        }
        EditOp::NodeReparent { node, to } => model.reparent(&node, &to)?,
        EditOp::NodeReorder { node, index } => model.reorder(&node, index as usize)?,
        EditOp::MeshSet {
            node,
            verts,
            uvs,
            indices,
            origin,
            deform_mapping,
        } => {
            let mesh = build_mesh(verts, uvs, indices, origin)?;
            let emptied = if let Some(rows) = deform_mapping {
                let rows: Vec<Vec<catchlight_core::model::VertexWeight>> = rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|v| catchlight_core::model::VertexWeight {
                                vertex: v.vertex,
                                weight: v.weight,
                            })
                            .collect()
                    })
                    .collect();
                let requested = rows.len().max(
                    rows.iter()
                        .fold(0usize, |count, row| count.saturating_add(row.len())),
                );
                model
                    .set_node_mesh_mapped(&node, mesh, &rows)
                    .map_err(|error| match error {
                        catchlight_core::model::MeshMappingError::Model(error) => {
                            EditorError::Edit(error)
                        }
                        catchlight_core::model::MeshMappingError::Limit => EditorError::Limit {
                            resource: "deform_mapping_entries",
                            limit: catchlight_core::model::MAX_MAPPING_ENTRIES as u64,
                            requested: requested as u64,
                        },
                        error => EditorError::BadTarget(error.to_string()),
                    })?
            } else {
                model.set_mesh_with_refit(&node, mesh)?
            };
            return Ok(emptied_reply(node, emptied));
        }
        EditOp::BindingKeyInsert {
            node,
            params,
            target,
            axis,
            value,
        } => {
            model.key_insert(&binding_key(params, node, target)?, &axis, value)?;
        }
        EditOp::BindingKeyDelete {
            node,
            params,
            target,
            axis,
            index,
        } => {
            model.key_delete(&binding_key(params, node, target)?, &axis, index as usize)?;
        }
        EditOp::BindingKeyMove {
            node,
            params,
            target,
            axis,
            index,
            value,
        } => {
            model.key_move(
                &binding_key(params, node, target)?,
                &axis,
                index as usize,
                value,
            )?;
        }
        EditOp::BindingAdd {
            node,
            params,
            target,
            key_positions,
        } => {
            let key = binding_key(params, node, target)?;
            if let Some(positions) = key_positions {
                model.add_binding_with_positions(&key, positions)?;
            } else {
                model.add_binding(&key)?;
            }
        }
        EditOp::BindingCellsSet {
            node,
            params,
            target,
            cells,
        } => {
            write_cells(model, &binding_key(params, node, target)?, cells)?;
        }
        EditOp::BindingCellsUnset {
            node,
            params,
            target,
            cells,
        } => {
            let key = binding_key(params, node, target)?;
            if model.binding(&key).is_none() {
                return Err(ModelError::UnknownBinding.into());
            }
            validate_cells(model, &key, cells.iter().copied(), cells.len())?;
            for cell in cells {
                model.unset_binding_key(&key, cell)?;
            }
        }
        EditOp::BindingDelete {
            node,
            params,
            target,
        } => model.delete_binding(&binding_key(params, node, target)?)?,
        EditOp::BindingInterpolationSet {
            node,
            params,
            target,
            mode,
        } => model.set_binding_interpolate(&binding_key(params, node, target)?, mode.into())?,
        EditOp::MaskAdd { node, source, mode } => model.mask_add(&node, &source, mode.into())?,
        EditOp::MaskDelete { node, index } => model.mask_delete(&node, index as usize)?,
        EditOp::SlotAdd { node, slot } => {
            model.slot_add(&node, slot.clone())?;
            return Ok(ResponseBody::Slot {
                slot: SlotAddr { node, slot },
            });
        }
        EditOp::SlotFill { node, slot, vertex } => model.slot_fill(&node, &slot, vertex)?,
        EditOp::SlotClear { node, slot } => model.slot_clear(&node, &slot)?,
        EditOp::SlotDelete { node, slot } => model.slot_delete(&node, &slot)?,
        EditOp::WeldSet { a, b, pairs } => {
            let replacement = build_weld(a, b, pairs);
            let mut welds = model.welds().to_vec();
            if let Some(i) = welds
                .iter()
                .position(|w| pairs_the_same_parts(w, &replacement))
            {
                welds[i] = replacement;
            } else {
                welds.push(replacement);
            }
            model.set_welds(welds)?;
        }
        EditOp::WeldDelete { a, b } => {
            let mut welds = model.welds().to_vec();
            let before = welds.len();
            welds.retain(|w| !joins(w, &a, &b));
            if welds.len() == before {
                return Err(EditorError::UnknownWeld);
            }
            model.set_welds(welds)?;
        }
    }
    Ok(ResponseBody::Empty)
}

pub(super) fn validate_cells(
    model: &Model,
    key: &BindingKey,
    cells: impl Iterator<Item = [u32; 2]>,
    len: usize,
) -> Result<(), EditorError> {
    if len == 0 {
        return Err(EditorError::BadRequest("cells must not be empty".into()));
    }
    if len > MAX_BINDING_CELLS {
        return Err(EditorError::Limit {
            resource: "binding_cells",
            limit: MAX_BINDING_CELLS as u64,
            requested: len as u64,
        });
    }
    let (width, height) = model.binding_grid(key)?;
    let mut seen = HashSet::with_capacity(len);
    for cell in cells {
        if cell[0] >= width || cell[1] >= height {
            return Err(ModelError::CellOutOfRange.into());
        }
        if !seen.insert(cell) {
            return Err(EditorError::BadRequest("duplicate cell address".into()));
        }
    }
    Ok(())
}

fn write_cells(
    model: &mut Model,
    key: &BindingKey,
    cells: Vec<BindingCellWrite>,
) -> Result<(), EditorError> {
    validate_cells(model, key, cells.iter().map(|c| c.cell), cells.len())?;
    let count = if key.target == CoreTarget::Deform {
        Some(
            model
                .node_mesh(&key.node)
                .ok_or_else(|| {
                    if model.node(&key.node).is_some() {
                        ModelError::NotMeshed
                    } else {
                        ModelError::UnknownNode
                    }
                })?
                .verts
                .len()
                / 2,
        )
    } else {
        None
    };
    for cell in &cells {
        match (&cell.value, key.target) {
            (BindingCellValue::Scalar(v), CoreTarget::Scalar(_)) if v.is_finite() => {}
            (BindingCellValue::Offsets(v), CoreTarget::Deform)
                if Some(v.len()) == count && v.iter().flatten().all(|v| v.is_finite()) => {}
            _ => {
                return Err(EditorError::BadTarget(
                    "cell payload must match target and mesh size and contain only finite numbers"
                        .into(),
                ))
            }
        }
    }
    // add_binding validates node kind and all input IDs before any cell is set.
    model.add_binding(key)?;
    for cell in cells {
        match cell.value {
            BindingCellValue::Scalar(value) => model.set_binding_key(key, cell.cell, value)?,
            BindingCellValue::Offsets(offsets) => {
                model.set_deform_vertices(key, cell.cell, offsets.concat())?
            }
        }
    }
    Ok(())
}
