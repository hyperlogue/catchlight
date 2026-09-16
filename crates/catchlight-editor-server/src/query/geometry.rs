//! Isolated explicit-pose geometry: no preview scratch, animation or physics.

use super::{check_count, page, MAX_QUERY_ITEMS};
use crate::EditorError;
use catchlight_core::{geometry::EvaluatedGeometry, LoadLimits, Model, Puppet};
use catchlight_editor_protocol::*;
use std::collections::HashSet;

pub(super) fn sample(model: &Model, command: &Command) -> Result<ResponseBody, EditorError> {
    let Command::GeometryGet {
        nodes,
        pose,
        fields,
        vertices,
        triangles,
        ..
    } = command
    else {
        unreachable!()
    };
    if nodes.is_empty() || fields.is_empty() {
        return Err(EditorError::BadRequest(
            "geometry requires nodes and fields".into(),
        ));
    }
    check_count("geometry_nodes", nodes.len(), 1024)?;
    if nodes.iter().collect::<HashSet<_>>().len() != nodes.len()
        || fields.iter().collect::<HashSet<_>>().len() != fields.len()
    {
        return Err(EditorError::BadRequest(
            "duplicate geometry node or field".into(),
        ));
    }
    let vertex_fields = fields
        .iter()
        .filter(|field| **field != GeometryField::Triangles)
        .count();
    let triangle_field = fields.contains(&GeometryField::Triangles);
    let mut pages = Vec::with_capacity(nodes.len());
    let mut items = 0;
    for node in nodes {
        let authored = model
            .node(node)
            .ok_or_else(|| EditorError::NoNode(node.clone()))?;
        let mesh = authored
            .mesh()
            .ok_or_else(|| EditorError::BadTarget(format!("node has no mesh: {node}")))?;
        let vertices = if vertex_fields != 0 {
            page(*vertices, mesh.vertex_count())?
        } else {
            IndexRange { start: 0, count: 0 }
        };
        let triangles = if triangle_field {
            page(*triangles, mesh.triangle_count())?
        } else {
            IndexRange { start: 0, count: 0 }
        };
        items += vertices.count as usize * vertex_fields + triangles.count as usize;
        check_count("geometry_items", items, MAX_QUERY_ITEMS)?;
        pages.push((vertices, triangles));
    }
    let mut inputs = HashSet::new();
    for input in pose {
        if !inputs.insert(&input.param) || !input.value.is_finite() {
            return Err(EditorError::BadRequest(
                "duplicate or nonfinite pose input".into(),
            ));
        }
        if model.param(&input.param).is_none() {
            return Err(EditorError::NoParam(input.param.clone()));
        }
    }
    check_evaluation_work(model)?;
    let mut puppet = Puppet::new(model);
    puppet.set_physics_enabled(false);
    for input in pose {
        puppet.set_param_value(&input.param, input.value);
    }
    puppet.tick(model, 0.0);
    let pose: Vec<ParamPose> = model
        .param_ids()
        .iter()
        .filter_map(|param| {
            model.param(param).map(|p| ParamPose {
                param: param.clone(),
                value: puppet.param_value(param).unwrap_or(p.default),
            })
        })
        .collect();
    if pose.iter().any(|input| !input.value.is_finite()) {
        return Err(EditorError::BadTarget("nonfinite evaluated pose".into()));
    }
    let observed = EvaluatedGeometry::new(model, &puppet)
        .map_err(|e| EditorError::BadTarget(e.to_string()))?;
    let mut result = Vec::with_capacity(nodes.len());
    for (node, (vertices, triangles)) in nodes.iter().zip(pages) {
        let mesh = observed
            .mesh(node)
            .map_err(|e| EditorError::BadTarget(e.to_string()))?;
        if !mesh.local_to_world().is_finite() || !mesh.origin().is_finite() {
            return Err(EditorError::BadTarget(
                "nonfinite evaluated transform".into(),
            ));
        }
        let mut out = GeometryNode {
            node: node.clone(),
            origin: mesh.origin().to_array(),
            local_to_world: mesh.local_to_world().to_cols_array(),
            vertex_count: mesh.vertex_count() as u32,
            triangle_count: mesh.triangle_count() as u32,
            vertices,
            triangle_range: triangles,
            rest: fields.contains(&GeometryField::Rest).then(Vec::new),
            local: fields.contains(&GeometryField::Local).then(Vec::new),
            world: fields.contains(&GeometryField::World).then(Vec::new),
            uvs: fields.contains(&GeometryField::Uvs).then(Vec::new),
            triangles: fields.contains(&GeometryField::Triangles).then(Vec::new),
        };
        for v in mesh
            .vertices()
            .skip(vertices.start as usize)
            .take(vertices.count as usize)
        {
            if !v.world.is_finite() || !v.local.is_finite() {
                return Err(EditorError::BadTarget(
                    "nonfinite evaluated geometry".into(),
                ));
            }
            if let Some(values) = &mut out.rest {
                values.push(v.rest.to_array());
            }
            if let Some(values) = &mut out.local {
                values.push(v.local.to_array());
            }
            if let Some(values) = &mut out.world {
                values.push(v.world.to_array());
            }
            if let (Some(values), Some(uv)) = (&mut out.uvs, v.uv) {
                values.push(uv.to_array());
            }
        }
        if let Some(values) = &mut out.triangles {
            values.extend(
                mesh.triangles()
                    .skip(triangles.start as usize)
                    .take(triangles.count as usize)
                    .map(|(_, t)| t),
            );
        }
        result.push(out);
    }
    Ok(ResponseBody::GeometrySample {
        pose,
        nodes: result,
    })
}

/// A tiny output page still evaluates the whole model. Live edits can grow a
/// sparse binding beyond file-load budgets, so charge its dense runtime shape
/// before constructing a Puppet. Count every binding, including unselected
/// nodes, and count grid cells separately so zero-vertex grids remain bounded.
fn check_evaluation_work(model: &Model) -> Result<(), EditorError> {
    let limits = LoadLimits::default();
    let mut cells = 0_u64;
    let mut vertices = 0_u64;
    for binding in model.bindings() {
        let grid_cells = binding
            .key_positions()
            .iter()
            .fold(1_u64, |count, axis| count.saturating_mul(axis.len() as u64));
        cells = cells.saturating_add(grid_cells);
        if cells > limits.binding_cells {
            return Err(EditorError::Limit {
                resource: "geometry_dense_cells",
                limit: limits.binding_cells,
                requested: cells,
            });
        }
        if binding.key().target == catchlight_core::BindingTarget::Deform {
            let mesh = model.node_mesh(&binding.key().node).ok_or_else(|| {
                EditorError::BadTarget(format!("node has no mesh: {}", binding.key().node))
            })?;
            vertices =
                vertices.saturating_add(grid_cells.saturating_mul(mesh.vertex_count() as u64));
            if vertices > limits.deform_offsets {
                return Err(EditorError::Limit {
                    resource: "geometry_dense_vertices",
                    limit: limits.deform_offsets,
                    requested: vertices,
                });
            }
        }
    }
    Ok(())
}
