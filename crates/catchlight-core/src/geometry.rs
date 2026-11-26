//! Geometry observed from a frame the caller has already evaluated.
//!
//! Local positions are `rest - origin + combined_deform`; world positions
//! apply the evaluated node matrix, including the caller's root transform.
//! Bounds enclose the XY projection of triangle-referenced world vertices.

use glam::{Mat4, Vec2, Vec3};

use crate::{Mesh, Model, NodeId, NodeKind, Puppet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GeometryError {
    #[error("the puppet belongs to a different model")]
    ModelMismatch,
    #[error("the puppet has not evaluated the current model generation")]
    StalePuppet,
    #[error("the puppet has no evaluated transforms")]
    UnevaluatedPuppet,
    #[error("unknown node: {0}")]
    UnknownNode(NodeId),
    #[error("node has no mesh: {0}")]
    NotMeshed(NodeId),
    #[error("node {node} has a non-finite world position at vertex {vertex}")]
    NonFiniteWorldVertex { node: NodeId, vertex: usize },
}

/// A read-only view of one evaluated frame, sharing its mesh and deform data.
pub struct EvaluatedGeometry<'a> {
    puppet: &'a Puppet,
}

impl<'a> EvaluatedGeometry<'a> {
    pub const fn new(model: &'a Model, puppet: &'a Puppet) -> Result<Self, GeometryError> {
        if model.identity() != puppet.model_identity() {
            return Err(GeometryError::ModelMismatch);
        }
        if model.generation() != puppet.baked_generation() {
            return Err(GeometryError::StalePuppet);
        }
        if puppet.transforms().is_empty() {
            return Err(GeometryError::UnevaluatedPuppet);
        }
        Ok(Self { puppet })
    }

    /// Empty meshes are valid.
    pub fn mesh(&self, node: &NodeId) -> Result<EvaluatedMesh<'a>, GeometryError> {
        let idx = self
            .puppet
            .node_idx(node)
            .ok_or_else(|| GeometryError::UnknownNode(node.clone()))?;
        let runtime = self
            .puppet
            .get(idx)
            .ok_or_else(|| GeometryError::UnknownNode(node.clone()))?;
        let (mesh, combined, is_part) = match &runtime.kind {
            NodeKind::Part(part) => (&part.mesh, part.deform_stack.combined(), true),
            NodeKind::MeshGroup(group) => (&group.mesh, group.deform_stack.combined(), false),
            _ => return Err(GeometryError::NotMeshed(node.clone())),
        };
        let node = self
            .puppet
            .node_id(idx)
            .ok_or_else(|| GeometryError::UnknownNode(node.clone()))?;
        Ok(EvaluatedMesh {
            node,
            mesh,
            combined,
            local_to_world: self.puppet.transforms().get(idx),
            is_part,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EvaluatedMesh<'a> {
    node: &'a NodeId,
    mesh: &'a Mesh,
    combined: &'a [Vec2],
    local_to_world: Mat4,
    is_part: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvaluatedVertex {
    pub index: usize,
    pub rest: Vec2,
    pub local: Vec2,
    pub world: Vec3,
    pub uv: Option<Vec2>,
}

impl<'a> EvaluatedMesh<'a> {
    #[must_use]
    pub const fn node(&self) -> &'a NodeId {
        self.node
    }

    #[must_use]
    pub const fn is_part(&self) -> bool {
        self.is_part
    }

    /// Before subtracting the origin.
    #[must_use]
    pub fn rest(&self) -> &'a [Vec2] {
        &self.mesh.vertices
    }

    #[must_use]
    pub fn uvs(&self) -> &'a [Vec2] {
        &self.mesh.uvs
    }

    #[must_use]
    pub const fn origin(&self) -> Vec2 {
        self.mesh.origin
    }

    #[must_use]
    pub const fn local_to_world(&self) -> Mat4 {
        self.local_to_world
    }

    #[must_use]
    pub const fn vertex_count(&self) -> usize {
        self.mesh.vertices.len()
    }

    #[must_use]
    pub const fn triangle_count(&self) -> usize {
        self.mesh.indices.len() / 3
    }

    #[must_use]
    pub fn vertex(&self, index: usize) -> Option<EvaluatedVertex> {
        (index < self.vertex_count()).then(|| self.vertex_at(index))
    }

    #[must_use]
    pub fn vertices(
        &self,
    ) -> impl DoubleEndedIterator<Item = EvaluatedVertex> + ExactSizeIterator + '_ {
        (0..self.vertex_count()).map(|index| self.vertex_at(index))
    }

    fn vertex_at(&self, index: usize) -> EvaluatedVertex {
        // Baking sizes the deform stack to the validated mesh's vertex count.
        let rest = self.mesh.vertices[index];
        let local = rest - self.mesh.origin + self.combined[index];
        EvaluatedVertex {
            index,
            rest,
            local,
            world: self.local_to_world.transform_point3(local.extend(0.0)),
            uv: self.mesh.uvs.get(index).copied(),
        }
    }

    #[must_use]
    pub fn triangle(&self, index: usize) -> Option<[u32; 3]> {
        if index >= self.triangle_count() {
            return None;
        }
        let offset = index * 3;
        Some([
            self.mesh.indices.get(offset)?,
            self.mesh.indices.get(offset + 1)?,
            self.mesh.indices.get(offset + 2)?,
        ])
    }

    /// A trailing partial triangle contributes nothing, as in the renderer.
    #[must_use]
    pub fn triangles(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, [u32; 3])> + ExactSizeIterator + '_ {
        (0..self.triangle_count()).map(|index| {
            // Every index in the range names three indices in a valid mesh.
            let offset = index * 3;
            let triangle = match &self.mesh.indices {
                crate::MeshIndices::U16(v) => {
                    [v[offset].into(), v[offset + 1].into(), v[offset + 2].into()]
                }
                crate::MeshIndices::U32(v) => [v[offset], v[offset + 1], v[offset + 2]],
            };
            (index, triangle)
        })
    }

    /// `None` for no triangles. Non-finite positions return an error.
    pub fn world_bounds(&self) -> Result<Option<Bounds2>, GeometryError> {
        let mut bounds: Option<Bounds2> = None;
        for (_, triangle) in self.triangles() {
            for vertex in triangle {
                let world = self.vertex_at(vertex as usize).world;
                if !world.is_finite() {
                    return Err(GeometryError::NonFiniteWorldVertex {
                        node: self.node.clone(),
                        vertex: vertex as usize,
                    });
                }
                let point = world.truncate();
                match &mut bounds {
                    Some(bounds) => bounds.include(point),
                    None => {
                        bounds = Some(Bounds2 {
                            min: point,
                            max: point,
                        });
                    }
                }
            }
        }
        Ok(bounds)
    }
}

/// `None` (not an infinity sentinel) for an absent envelope, so it fits JSON.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds2 {
    pub min: Vec2,
    pub max: Vec2,
}

impl Bounds2 {
    fn include(&mut self, point: Vec2) {
        self.min = self.min.min(point);
        self.max = self.max.max(point);
    }

    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}
