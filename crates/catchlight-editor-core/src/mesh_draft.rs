//! An isolated mesh-edit transaction. The source texture mapping is captured
//! once, so moving topology cannot become a texture deform. Gesture history
//! is local to the draft; only `finish` produces a mesh for a model edit.

use catchlight_core::formats::clm::ClmMesh;

use crate::{AlphaMask, ContourKnobs, GridKnobs, MeshError, UvMap, WorkingMesh};

pub struct MeshDraft {
    original: WorkingMesh,
    mesh: WorkingMesh,
    uv: UvMap,
    alpha: AlphaMask,
    before: Option<WorkingMesh>,
    undo: Vec<WorkingMesh>,
    redo: Vec<WorkingMesh>,
}

fn same(a: &WorkingMesh, b: &WorkingMesh) -> bool {
    a.verts == b.verts && a.constraints == b.constraints && a.origin == b.origin
}

impl MeshDraft {
    pub fn new(mesh: &ClmMesh, alpha: AlphaMask) -> Result<Self, MeshError> {
        let uv = if mesh.verts.is_empty() {
            UvMap::from_texture_size(alpha.width as f32, alpha.height as f32)
        } else {
            UvMap::fit(&mesh.verts, &mesh.uvs).ok_or(MeshError::CustomTextureMapping)?
        };
        // A least-squares fit alone would silently flatten already-distorted
        // artwork. Refuse a mapping this view cannot preserve exactly.
        for (vertex, expected) in mesh
            .verts
            .as_chunks::<2>()
            .0
            .iter()
            .zip(mesh.uvs.as_chunks::<2>().0)
        {
            let mapped = uv.uv(*vertex);
            if mapped
                .iter()
                .zip(expected)
                .any(|(a, b)| !a.is_finite() || (a - b).abs() > 1e-4)
            {
                return Err(MeshError::CustomTextureMapping);
            }
        }
        let mesh = WorkingMesh::from_mesh(mesh);
        Ok(Self {
            original: mesh.clone(),
            mesh,
            uv,
            alpha,
            before: None,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn working(&self) -> &WorkingMesh {
        &self.mesh
    }
    pub fn uv_map(&self) -> UvMap {
        self.uv
    }
    pub fn dirty(&self) -> bool {
        !same(&self.mesh, &self.original)
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn begin_gesture(&mut self) {
        if self.before.is_none() {
            self.before = Some(self.mesh.clone());
        }
    }

    pub fn end_gesture(&mut self, commit: bool) {
        if let Some(before) = self.before.take() {
            if !commit {
                self.mesh = before;
            } else if !same(&before, &self.mesh) {
                self.undo.push(before);
                if self.undo.len() > 100 {
                    self.undo.remove(0);
                }
                self.redo.clear();
            }
        }
    }

    pub fn move_vertex(&mut self, index: u32, position: [f32; 2]) -> Result<(), MeshError> {
        self.mesh.move_vertex(index, position)
    }

    /// A delta from the start of the current gesture, applied to one selection.
    pub fn translate_vertices(
        &mut self,
        indices: &[u32],
        delta: [f32; 2],
    ) -> Result<(), MeshError> {
        let start = self.before.as_ref().unwrap_or(&self.mesh);
        let mut positions = Vec::with_capacity(indices.len());
        for &i in indices {
            if i as usize >= start.vertex_count() {
                return Err(MeshError::NoSuchVertex);
            }
            let p = start.pos(i);
            positions.push((i, [p[0] + delta[0], p[1] + delta[1]]));
        }
        self.mesh.move_vertices(&positions)
    }

    pub fn add_vertex(&mut self, position: [f32; 2]) -> Result<u32, MeshError> {
        self.begin_gesture();
        let result = self.mesh.add_vertex(position);
        self.end_gesture(result.is_ok());
        result
    }

    pub fn delete_vertices(&mut self, indices: &[u32]) -> Result<(), MeshError> {
        let remaining = (0..self.mesh.vertex_count() as u32)
            .filter(|i| !indices.contains(i))
            .count();
        if remaining < 3 {
            return Err(MeshError::Triangulation);
        }
        self.begin_gesture();
        self.mesh.delete_vertices(indices);
        let valid = self
            .mesh
            .triangulate()
            .is_ok_and(|triangles| !triangles.is_empty());
        self.end_gesture(valid);
        if valid {
            Ok(())
        } else {
            Err(MeshError::Triangulation)
        }
    }

    pub fn toggle_edge(&mut self, a: u32, b: u32) -> Result<(), MeshError> {
        self.begin_gesture();
        let result = if self.mesh.has_constraint(a, b) {
            self.mesh.remove_constraint(a, b);
            Ok(())
        } else {
            self.mesh.add_constraint(a, b)
        };
        self.end_gesture(result.is_ok());
        result
    }

    pub fn generate_contour(&mut self, spacing: u32, margin: u32) -> Result<(), MeshError> {
        let next = crate::contour_automesh(
            &self.alpha,
            &ContourKnobs {
                spacing: spacing.clamp(1, 4096),
                margin: margin.min(128),
                ..Default::default()
            },
            &self.uv,
            self.mesh.origin,
        )?;
        self.begin_gesture();
        self.mesh = next;
        self.end_gesture(true);
        Ok(())
    }

    pub fn generate_grid(&mut self, cols: u32, rows: u32) -> Result<(), MeshError> {
        let next = crate::grid_automesh(
            &self.alpha,
            &GridKnobs {
                cols: cols.clamp(2, 128),
                rows: rows.clamp(2, 128),
                ..Default::default()
            },
            &self.uv,
            self.mesh.origin,
        )?;
        self.begin_gesture();
        self.mesh = next;
        self.end_gesture(true);
        Ok(())
    }

    pub fn undo(&mut self) {
        self.end_gesture(false);
        if let Some(previous) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.mesh, previous));
        }
    }
    pub fn redo(&mut self) {
        self.end_gesture(false);
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.mesh, next));
        }
    }

    /// Alpha culling happens on Apply, not once per pointer event.
    pub fn finish(&self) -> Result<ClmMesh, MeshError> {
        self.mesh.to_mesh(&self.uv, Some(&self.alpha))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::formats::clm::ClmIndices;

    fn draft() -> MeshDraft {
        MeshDraft::new(
            &ClmMesh {
                verts: vec![-20., -20., 20., -20., 20., 20., -20., 20., 0., 0.],
                uvs: vec![0., 1., 1., 1., 1., 0., 0., 0., 0.5, 0.5],
                indices: ClmIndices::U16(vec![0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4]),
                origin: [0., 0.],
            },
            AlphaMask {
                width: 40,
                height: 40,
                alpha: vec![255; 1600],
            },
        )
        .unwrap()
    }

    #[test]
    fn moving_topology_preserves_the_texture_mapping_and_gesture_history() {
        let mut d = draft();
        d.begin_gesture();
        d.move_vertex(4, [2., 1.]).unwrap();
        d.move_vertex(4, [8., 4.]).unwrap();
        d.end_gesture(true);
        let result = d.finish().unwrap();
        assert_eq!(&result.verts[8..], &[8., 4.]);
        assert!((result.uvs[8] - 0.7).abs() < 1e-6);
        assert!((result.uvs[9] - 0.4).abs() < 1e-6);
        d.undo();
        assert!(!d.dirty());
        assert!(!d.can_undo());
        d.redo();
        assert_eq!(d.working().pos(4), [8., 4.]);
    }

    #[test]
    fn cancellation_and_invalid_placements_leave_the_previous_draft_intact() {
        let mut d = draft();
        d.begin_gesture();
        d.move_vertex(4, [5., 5.]).unwrap();
        assert!(d.move_vertex(4, [20., 20.]).is_err());
        assert_eq!(d.working().pos(4), [5., 5.]);
        d.end_gesture(false);
        assert!(!d.dirty());
        assert!(!d.can_undo());
        assert!(d.delete_vertices(&[0, 1, 2]).is_err());
    }
    #[test]
    fn a_selected_mesh_moves_as_one_and_cancellation_restores_it() {
        let mut d = draft();
        d.begin_gesture();
        let all = [0, 1, 2, 3, 4];
        d.translate_vertices(&all, [40.0, 0.0]).unwrap();
        d.translate_vertices(&all, [60.0, 5.0]).unwrap();
        assert_eq!(d.working().pos(0), [40.0, -15.0]);
        assert_eq!(d.working().pos(4), [60.0, 5.0]);
        d.end_gesture(false);
        assert!(!d.dirty());
    }

    #[test]
    fn custom_uvs_are_refused_without_flattening_the_original_artwork() {
        let d = draft();
        let mut mesh = d.finish().unwrap();
        mesh.uvs[8] += 0.1;
        assert!(matches!(
            MeshDraft::new(
                &mesh,
                AlphaMask {
                    width: 40,
                    height: 40,
                    alpha: vec![255; 1600]
                }
            ),
            Err(MeshError::CustomTextureMapping)
        ));
    }
}
