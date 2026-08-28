use catchlight_core::{NodeKind, Puppet, Vec2};

/// Frozen copy of every active node's combined deform, taken while the
/// puppet is not being mutated. The renderer uploads from this
/// (`WgpuRenderer::sync_deforms_snapshot`) instead of reading the live
/// puppet, so a caller that pipelines CPU and GPU work can recompute the
/// next frame's deforms — which overwrite the live `DeformStack::combined`
/// buffers in place — while the renderer is still consuming this frame.
/// `generation` lets the renderer skip re-uploading a mesh whose deform
/// didn't change.
#[derive(Debug, Clone, Default)]
pub struct DeformSnapshot {
    pub entries: Vec<DeformEntry>,
}

#[derive(Debug, Clone)]
pub struct DeformEntry {
    pub node_id: u32,
    pub generation: u64,
    pub combined: Vec<Vec2>,
}

impl DeformSnapshot {
    /// Build a fresh snapshot of `puppet`'s active deforms. Call after
    /// `Puppet::combine_deforms()`.
    pub fn from_puppet(puppet: &Puppet) -> Self {
        let mut snap = Self::default();
        snap.refill_from_puppet(puppet);
        snap
    }

    /// Refill in place, reusing the entry buffer and each entry's
    /// `combined` allocation (clear + extend, no realloc). The active
    /// deform set is near-constant frame to frame, so a caller that keeps
    /// one snapshot per puppet and refills it each frame allocates nothing
    /// steady-state.
    pub fn refill_from_puppet(&mut self, puppet: &Puppet) {
        let mut count = 0;
        for (node_id, node) in puppet.iter_deform_nodes() {
            let stack = match &node.kind {
                NodeKind::Part(p) => &p.deform_stack,
                NodeKind::MeshGroup(mg) => &mg.deform_stack,
                _ => continue,
            };
            if !stack.is_active() {
                continue;
            }
            let src = stack.combined();
            let generation = stack.generation();
            match self.entries.get_mut(count) {
                Some(entry) => {
                    if entry.node_id == node_id.0 && entry.generation == generation {
                        count += 1;
                        continue;
                    }
                    entry.node_id = node_id.0;
                    entry.generation = generation;
                    entry.combined.clear();
                    entry.combined.extend_from_slice(src);
                }
                None => self.entries.push(DeformEntry {
                    node_id: node_id.0,
                    generation,
                    combined: src.to_vec(),
                }),
            }
            count += 1;
        }
        // Drop any slots a now-smaller active set left behind.
        self.entries.truncate(count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::{DeformSource, DeformStack, Mesh, MeshIndices, Node, PartData, Puppet};

    // A Part whose deform stack is pre-activated with a uniform shift, so
    // is_active() is true and combined() == the shift (no param machinery).
    fn shifted_part(shift_x: f32) -> Node {
        let mut deform_stack = DeformStack::new(3);
        deform_stack
            .set(DeformSource::Param(0), vec![Vec2::new(shift_x, 0.0); 3])
            .unwrap();
        deform_stack.combine();
        Node {
            kind: NodeKind::Part(Box::new(PartData {
                deform_stack,
                mesh: Mesh::new(
                    vec![Vec2::ZERO; 3],
                    vec![Vec2::ZERO; 3],
                    MeshIndices::U16(vec![0, 1, 2]),
                    Vec2::ZERO,
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn inactive_part() -> Node {
        Node {
            kind: NodeKind::Part(Box::new(PartData {
                deform_stack: DeformStack::new(3),
                mesh: Mesh::new(
                    vec![Vec2::ZERO; 3],
                    vec![Vec2::ZERO; 3],
                    MeshIndices::U16(vec![0, 1, 2]),
                    Vec2::ZERO,
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    #[test]
    fn captures_only_active_stacks() {
        let mut puppet = Puppet::new();
        let active_id = puppet.insert_child(puppet.root(), shifted_part(8.0), Some(10));
        let _inactive_id = puppet.insert_child(puppet.root(), inactive_part(), Some(11));

        let snap = DeformSnapshot::from_puppet(&puppet);
        // The untouched part has no active source, so it must be absent.
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.entries[0].node_id, active_id.0);
        assert!((snap.entries[0].combined[0].x - 8.0).abs() < 1e-5);
    }

    #[test]
    fn refill_reuses_and_truncates() {
        let mut puppet = Puppet::new();
        let id = puppet.insert_child(puppet.root(), shifted_part(8.0), Some(20));

        let mut snap = DeformSnapshot::default();
        snap.refill_from_puppet(&puppet);
        assert_eq!(snap.entries.len(), 1);
        assert!((snap.entries[0].combined[0].x - 8.0).abs() < 1e-5);

        // Re-pose into the SAME snapshot: the reused slot holds the new
        // value + a fresh generation, never stale data.
        puppet
            .update_deform_source(id, DeformSource::Param(0), |buf| {
                buf.fill(Vec2::new(4.0, 0.0));
            })
            .expect("part deform stack");
        puppet.combine_deforms();
        let gen1 = snap.entries[0].generation;
        snap.refill_from_puppet(&puppet);
        assert_eq!(snap.entries.len(), 1);
        assert!((snap.entries[0].combined[0].x - 4.0).abs() < 1e-5);
        assert_ne!(snap.entries[0].generation, gen1);

        // Deactivate: the active set shrinks to empty, so entries truncate.
        assert!(puppet.reset_node_deforms(id));
        snap.refill_from_puppet(&puppet);
        assert!(snap.entries.is_empty());
    }
}
