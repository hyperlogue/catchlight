//! Convex correspondence for replacing a mesh without guessing vertex identity.
//!
//! Each destination row names weighted source indices. Validation precedes all
//! model mutation; duplicate source entries accumulate in supplied order in f64,
//! then sort by source index. Zero weights disappear, and valid weights are never
//! renormalized. The same canonical map applies to every authored deform cell;
//! holes, interpolation and binding order are retained. Slots follow ordinary
//! mesh replacement: their old indices are cleared and returned for explicit refill.

use std::collections::BTreeMap;

use crate::formats::clm::{ClmBindingValues, ClmMesh};
use crate::{NodeId, SlotId};

use super::{Model, ModelError};

pub const MAX_MAPPING_ENTRIES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VertexWeight {
    pub vertex: u32,
    pub weight: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum MeshMappingError {
    #[error("mesh correspondence exceeds {MAX_MAPPING_ENTRIES} rows or entries")]
    Limit,
    #[error("mesh correspondence has {actual} rows; expected {expected}")]
    RowCount { expected: usize, actual: usize },
    #[error("mesh correspondence row {row} is empty")]
    EmptyRow { row: usize },
    #[error("mesh correspondence row {row} references missing vertex {vertex}")]
    UnknownVertex { row: usize, vertex: u32 },
    #[error("mesh correspondence row {row} has a nonfinite or negative weight")]
    InvalidWeight { row: usize },
    #[error("mesh correspondence row {row} weights sum to {sum}; expected 1 within 1e-6")]
    InvalidSum { row: usize, sum: f64 },
    #[error("source offsets have {actual} numbers; expected {expected}")]
    OffsetCount { expected: usize, actual: usize },
    #[error("mesh correspondence produces a nonfinite offset at vertex {vertex}")]
    NonFiniteOffset { vertex: usize },
    #[error(transparent)]
    Model(#[from] ModelError),
}

/// Validated correspondence reusable across authored cells or external fields.
#[derive(Debug, Clone)]
pub struct DeformMapping {
    source_vertices: usize,
    rows: Vec<Vec<(u32, f64)>>,
}

impl DeformMapping {
    pub fn new(
        source_vertices: usize,
        target_vertices: usize,
        rows: &[Vec<VertexWeight>],
    ) -> Result<Self, MeshMappingError> {
        if rows.len() > MAX_MAPPING_ENTRIES {
            return Err(MeshMappingError::Limit);
        }
        if rows.len() != target_vertices {
            return Err(MeshMappingError::RowCount {
                expected: target_vertices,
                actual: rows.len(),
            });
        }
        let mut count = 0usize;
        let mut canonical = Vec::with_capacity(rows.len());
        for (row, weights) in rows.iter().enumerate() {
            count = count.saturating_add(weights.len());
            if count > MAX_MAPPING_ENTRIES {
                return Err(MeshMappingError::Limit);
            }
            if weights.is_empty() {
                return Err(MeshMappingError::EmptyRow { row });
            }
            let mut merged = BTreeMap::<u32, f64>::new();
            let mut sum = 0.0;
            for &VertexWeight { vertex, weight } in weights {
                if vertex as usize >= source_vertices {
                    return Err(MeshMappingError::UnknownVertex { row, vertex });
                }
                if !weight.is_finite() || weight < 0.0 {
                    return Err(MeshMappingError::InvalidWeight { row });
                }
                sum += f64::from(weight);
                *merged.entry(vertex).or_default() += f64::from(weight);
            }
            if (sum - 1.0).abs() > 1e-6 {
                return Err(MeshMappingError::InvalidSum { row, sum });
            }
            canonical.push(merged.into_iter().filter(|(_, w)| *w != 0.0).collect());
        }
        Ok(Self {
            source_vertices,
            rows: canonical,
        })
    }

    pub fn map(&self, source: &[f32]) -> Result<Vec<f32>, MeshMappingError> {
        let expected = self.source_vertices.saturating_mul(2);
        if source.len() != expected {
            return Err(MeshMappingError::OffsetCount {
                expected,
                actual: source.len(),
            });
        }
        let mut result = Vec::with_capacity(self.rows.len() * 2);
        for (vertex, row) in self.rows.iter().enumerate() {
            let mut xy = [0.0f64; 2];
            for &(index, weight) in row {
                for axis in 0..2 {
                    xy[axis] += f64::from(source[index as usize * 2 + axis]) * weight;
                }
            }
            let xy = xy.map(|v| v as f32);
            if xy.iter().any(|v| !v.is_finite()) {
                return Err(MeshMappingError::NonFiniteOffset { vertex });
            }
            result.extend(xy);
        }
        Ok(result)
    }
}

impl Model {
    /// Replace rest geometry and map exactly the existing authored deforms.
    /// Invalid geometry, correspondence or mapped values leave this model intact.
    pub fn set_node_mesh_mapped(
        &mut self,
        node: &NodeId,
        mesh: ClmMesh,
        rows: &[Vec<VertexWeight>],
    ) -> Result<Vec<SlotId>, MeshMappingError> {
        super::validate_mesh(&mesh)?;
        let old = self.node_mesh(node).ok_or_else(|| {
            if self.node(node).is_some() {
                ModelError::NotMeshed
            } else {
                ModelError::UnknownNode
            }
        })?;
        let map = DeformMapping::new(old.verts.len() / 2, mesh.verts.len() / 2, rows)?;
        let mut mapped = Vec::new();
        for binding in self.bindings_of_node(node) {
            if let ClmBindingValues::Deform(cells) = binding.values() {
                for cell in &cells.cells {
                    mapped.push(map.map(&cell.value)?);
                }
            }
        }
        let mut at = 0;
        // set_node_mesh_with visits the same cells in the same order. All
        // fallible validation and arithmetic completed before it mutates anything.
        Ok(self.set_node_mesh_with(node, mesh, |_, _, _| {
            let offsets = std::mem::take(&mut mapped[at]);
            at += 1;
            offsets
        })?)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn row(values: &[(u32, f32)]) -> Vec<VertexWeight> {
        values
            .iter()
            .map(|&(vertex, weight)| VertexWeight { vertex, weight })
            .collect()
    }

    #[test]
    fn canonical_convex_map_retains_order_and_does_not_normalize() {
        let rows = vec![
            row(&[(2, 0.25), (0, 0.5), (2, 0.25), (1, 0.0)]),
            row(&[(1, 1.0)]),
        ];
        let map = DeformMapping::new(3, 2, &rows).unwrap();
        assert_eq!(map.rows[0], vec![(0, 0.5), (2, 0.5)]);
        assert_eq!(
            map.map(&[2., 4., 10., 20., 6., 8.]).unwrap(),
            vec![4., 6., 10., 20.]
        );
        let weight = 1.0 + 5e-7;
        let map = DeformMapping::new(1, 1, &[row(&[(0, weight)])]).unwrap();
        assert_eq!(map.map(&[1., 1.]).unwrap(), vec![weight, weight]);
    }

    #[test]
    fn invalid_maps_and_overflow_are_rejected_before_use() {
        for weights in [
            vec![],
            row(&[(3, 1.)]),
            row(&[(0, -1.), (1, 2.)]),
            row(&[(0, f32::NAN)]),
            row(&[(0, 0.9)]),
        ] {
            assert!(DeformMapping::new(2, 1, &[weights]).is_err());
        }
        assert!(DeformMapping::new(0, 0, &[])
            .unwrap()
            .map(&[])
            .unwrap()
            .is_empty());
        assert!(DeformMapping::new(0, 1, &[row(&[(0, 1.)])]).is_err());
        let map = DeformMapping::new(1, 1, &[row(&[(0, 1. + 5e-7)])]).unwrap();
        assert!(matches!(
            map.map(&[f32::MAX, 0.]),
            Err(MeshMappingError::NonFiniteOffset { .. })
        ));
        assert!(matches!(
            map.map(&[]),
            Err(MeshMappingError::OffsetCount { .. })
        ));
    }
    #[test]
    fn replacement_maps_authored_cells_preserves_holes_and_clears_slots_atomically() {
        use crate::formats::clm::ClmIndices;
        use crate::{
            BindingKey, BindingTarget, InterpolateMode, ModelNode, ModelNodeKind, ModelParam,
            ModelPart, Name, ParamId,
        };
        let mut model = Model::new();
        let node = NodeId::new("panel").unwrap();
        let root = model.root().unwrap().clone();
        let old = ClmMesh {
            verts: vec![0., 0., 2., 0., 0., 2.],
            uvs: vec![],
            indices: ClmIndices::U16(vec![0, 1, 2]),
            origin: [0.; 2],
        };
        model
            .add_node_with_id(
                node.clone(),
                &root,
                ModelNode::new("Panel", ModelNodeKind::Part(ModelPart::new(old))),
            )
            .unwrap();
        let param = ParamId::new("drive").unwrap();
        model
            .add_param_with_id(
                param.clone(),
                ModelParam::new(Name::truncated("Drive"), 0., 1., 0.),
            )
            .unwrap();
        let key = BindingKey::new(param, node.clone(), BindingTarget::Deform);
        model
            .add_binding_with_positions(&key, vec![vec![0., 0.5, 1.]])
            .unwrap();
        model
            .set_deform_vertices(&key, [2, 0], vec![0., 0., 2., 4., 4., 8.])
            .unwrap();
        model
            .set_binding_interpolate(&key, InterpolateMode::Stepped)
            .unwrap();
        let slot = SlotId::new("edge").unwrap();
        model.slot_add(&node, slot.clone()).unwrap();
        model.slot_fill(&node, &slot, 1).unwrap();
        let before = model.clone();
        let mesh = ClmMesh {
            verts: vec![1., 0., 2., 0., 1., 1.],
            uvs: vec![],
            indices: ClmIndices::U16(vec![0, 1, 2]),
            origin: [1., 1.],
        };
        let bad = vec![row(&[(0, 0.4)]), row(&[(1, 1.)]), row(&[(2, 1.)])];
        assert!(model
            .set_node_mesh_mapped(&node, mesh.clone(), &bad)
            .is_err());
        assert!(model.authored_eq(&before).unwrap());
        let rows = vec![
            row(&[(0, 0.5), (1, 0.5)]),
            row(&[(1, 1.)]),
            row(&[(1, 0.5), (2, 0.5)]),
        ];
        assert_eq!(
            model.set_node_mesh_mapped(&node, mesh, &rows).unwrap(),
            vec![slot]
        );
        let binding = model.binding(&key).unwrap();
        assert_eq!(binding.key_positions(), &[vec![0., 0.5, 1.]]);
        assert_eq!(binding.interpolate_mode(), InterpolateMode::Stepped);
        let cells = crate::deform_cells(binding.values()).unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!([cells[0].x, cells[0].y], [2, 0]);
        assert_eq!(cells[0].value, vec![1., 2., 2., 4., 3., 6.]);
        assert_eq!(model.slots(&node).unwrap()[0].vertex(), None);
    }
}
