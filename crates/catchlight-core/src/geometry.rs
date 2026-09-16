//! Geometry observed from a frame the caller has already evaluated.
//!
//! This module never poses, ticks, synchronizes or mutates a Puppet. The caller
//! chooses the pose, physics, animation and scratch policy, then evaluates the
//! frame before borrowing it here. A model identity/generation check prevents
//! mixing authored geometry with a different or outdated bake; a frame without
//! computed transforms is rejected. Changes to pose or scratch still require
//! the caller's normal evaluation before observation.
//!
//! Vertex and triangle indices remain in authored mesh order. Local positions
//! are `rest - origin + combined_deform`; world positions apply the evaluated
//! node matrix, including the caller's root transform. The combined deform is
//! the runtime's final result, including inherited MeshGroup/spine deforms and
//! welds. World positions retain all three coordinates of the runtime matrix.
//!
//! Bounds enclose the XY projection of triangle-referenced world vertices.
//! They do not inspect texture alpha, enabled ancestry, opacity, masks or
//! occlusion. Selection and union of Part bounds belong to callers; a
//! MeshGroup's lattice is inspectable geometry but is not Part artwork.

use glam::{Mat4, Vec2, Vec3};

use crate::{Mesh, Model, NodeId, NodeKind, Puppet};

/// An observation cannot mix model versions or invent a missing frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
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
///
/// Construction and individual mesh lookup allocate nothing. The borrow keeps
/// both the model and the Puppet fixed for the lifetime of all returned views.
pub struct EvaluatedGeometry<'a> {
    puppet: &'a Puppet,
}

impl<'a> EvaluatedGeometry<'a> {
    pub fn new(model: &'a Model, puppet: &'a Puppet) -> Result<Self, GeometryError> {
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

    /// Select one Part or MeshGroup by authored Id. Empty meshes are valid.
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

/// Authored mesh data paired with its evaluated transform and deformation.
#[derive(Debug, Clone, Copy)]
pub struct EvaluatedMesh<'a> {
    node: &'a NodeId,
    mesh: &'a Mesh,
    combined: &'a [Vec2],
    local_to_world: Mat4,
    is_part: bool,
}

/// One vertex, retaining its authored index even when read independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvaluatedVertex {
    pub index: usize,
    pub rest: Vec2,
    pub local: Vec2,
    pub world: Vec3,
    /// `None` when the authored mesh has no UV array.
    pub uv: Option<Vec2>,
}

impl<'a> EvaluatedMesh<'a> {
    pub fn node(&self) -> &'a NodeId {
        self.node
    }

    /// Whether this mesh belongs to Part artwork, rather than a MeshGroup.
    pub fn is_part(&self) -> bool {
        self.is_part
    }

    /// Stored mesh coordinates, before subtracting the origin.
    pub fn rest(&self) -> &'a [Vec2] {
        &self.mesh.vertices
    }

    /// Authored UVs, empty when the mesh does not carry them.
    pub fn uvs(&self) -> &'a [Vec2] {
        &self.mesh.uvs
    }

    pub fn origin(&self) -> Vec2 {
        self.mesh.origin
    }

    pub fn local_to_world(&self) -> Mat4 {
        self.local_to_world
    }

    pub fn vertex_count(&self) -> usize {
        self.mesh.vertices.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.mesh.indices.len() / 3
    }

    /// Read a vertex without visiting earlier vertices, for bounded pages.
    /// Returns `None` exactly when the index is outside the authored mesh.
    pub fn vertex(&self, index: usize) -> Option<EvaluatedVertex> {
        (index < self.vertex_count()).then(|| self.vertex_at(index))
    }

    /// All vertices in authored order, including those unused by triangles.
    pub fn vertices(
        &self,
    ) -> impl DoubleEndedIterator<Item = EvaluatedVertex> + ExactSizeIterator + '_ {
        (0..self.vertex_count()).map(|index| self.vertex_at(index))
    }

    fn vertex_at(&self, index: usize) -> EvaluatedVertex {
        // Baking preserves the validated mesh's vertex order and sizes its
        // deform stack to that same count. No fallback may hide a broken bake.
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

    /// Read one triangle's original vertex indices without renumbering them.
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

    /// Complete triangles in authored order, paired with their original index.
    /// A trailing partial triangle contributes nothing, as in the renderer.
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

    /// Conservative XY bounds of the triangles, or `None` for no triangles.
    ///
    /// An unreferenced vertex never expands the bounds. Degenerate triangles
    /// still contribute their vertices. Non-finite positions return an error
    /// instead of silently producing an incomplete envelope.
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
                        })
                    }
                }
            }
        }
        Ok(bounds)
    }
}

/// An axis-aligned two-dimensional envelope. An absent envelope is `None`,
/// rather than an infinity sentinel that cannot be represented in JSON.
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

    /// Union already selected envelopes; no visibility policy is applied.
    pub fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}
