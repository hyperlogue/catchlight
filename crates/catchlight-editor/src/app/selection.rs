use super::*;
use catchlight_core::id::NodeId;

/// Rev-gated three-way mapping: Model `NodeRef` ⇄ clp arena index ⇄ core
/// (puppet) node id. `from_clp` stamps the clp index as each node's uuid, which
/// is what makes the core↔clp direction recoverable.
pub(super) struct NodeMapping {
    rev: u64,
    refs: Vec<NodeRef>,
    core_of: Vec<u32>,
    parent_of: Vec<Option<usize>>,
    clp_of_ref: HashMap<u64, usize>,
    clp_of_core: HashMap<u32, usize>,
}

impl App {
    pub(super) fn primary(&self) -> Option<NodeRef> {
        self.selection.last().copied()
    }

    /// Rebuild the ref⇄clp⇄core mapping when the document revision moves.
    pub(super) fn ensure_mapping(&mut self, rev: u64) {
        if self.mapping.as_ref().is_some_and(|m| m.rev == rev) {
            return;
        }
        let Some(session) = self.session else { return };
        let editor = self.editor.clone();
        let base = editor.with_model(session, |m, ref_map| {
            let order = m.nodes_in_order();
            let pos: HashMap<&NodeId, usize> =
                order.iter().enumerate().map(|(i, id)| (id, i)).collect();
            let parent_of: Vec<Option<usize>> = order
                .iter()
                .map(|id| {
                    m.node(id)
                        .and_then(|n| n.parent())
                        .and_then(|p| pos.get(p).copied())
                })
                .collect();
            let refs: Vec<NodeRef> = order.iter().map(|id| ref_map.node(id)).collect();
            (refs, parent_of)
        });
        let Ok((refs, parent_of)) = base else { return };
        let core_of = editor.with_puppet(session, |p| {
            (0..refs.len())
                .map(|i| p.node_for_uuid(i as u32).map(|id| id.0).unwrap_or(u32::MAX))
                .collect::<Vec<u32>>()
        });
        let Ok(core_of) = core_of else { return };
        let clp_of_ref = refs.iter().enumerate().map(|(i, r)| (r.0, i)).collect();
        let clp_of_core = core_of
            .iter()
            .enumerate()
            .filter(|(_, &c)| c != u32::MAX)
            .map(|(i, &c)| (c, i))
            .collect();
        self.mapping = Some(NodeMapping {
            rev,
            refs,
            core_of,
            parent_of,
            clp_of_ref,
            clp_of_core,
        });
    }

    pub(super) fn core_of_ref(&self, r: NodeRef) -> Option<u32> {
        let m = self.mapping.as_ref()?;
        let i = *m.clp_of_ref.get(&r.0)?;
        let c = *m.core_of.get(i)?;
        (c != u32::MAX).then_some(c)
    }

    pub(super) fn ref_of_core(&self, core: u32) -> Option<NodeRef> {
        let m = self.mapping.as_ref()?;
        let i = *m.clp_of_core.get(&core)?;
        m.refs.get(i).copied()
    }

    /// Core ids of the isolated subtree (drawing filter), if isolation is on.
    pub(super) fn isolate_set(&self, root: &TreeNode) -> Option<HashSet<u32>> {
        let iso = self.isolated?;
        let sub = find_subtree(root, iso)?;
        let mut out = HashSet::new();
        collect_refs(sub, &mut |r| {
            if let Some(c) = self.core_of_ref(r) {
                out.insert(c);
            }
        });
        Some(out)
    }

    pub(super) fn pick(&mut self, session: SessionId, world: glam::Vec2) -> Vec<u32> {
        let Some(viewport) = self.viewport.as_ref() else {
            return Vec::new();
        };
        let transforms = &viewport.transforms;
        self.editor
            .with_puppet(session, |p| picking::pick_all(p, transforms, world))
            .unwrap_or_default()
    }

    pub(super) fn select(
        &mut self,
        node: NodeRef,
        additive: bool,
        range: bool,
        visible: &[NodeRef],
    ) {
        if range {
            if let Some(&anchor) = self.selection.last() {
                let a = visible.iter().position(|&r| r == anchor);
                let b = visible.iter().position(|&r| r == node);
                if let (Some(a), Some(b)) = (a, b) {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    for &r in &visible[lo..=hi] {
                        if !self.selection.contains(&r) {
                            self.selection.push(r);
                        }
                    }
                    return;
                }
            }
        }
        if additive {
            if let Some(pos) = self.selection.iter().position(|&r| r == node) {
                self.selection.remove(pos);
            } else {
                self.selection.push(node);
            }
        } else {
            self.selection = vec![node];
        }
    }

    pub(super) fn focus_selected(&mut self) {
        let Some(session) = self.session else { return };
        let cores: Vec<u32> = self
            .selection
            .iter()
            .filter_map(|&r| self.core_of_ref(r))
            .collect();
        if cores.is_empty() {
            return;
        }
        let Some(viewport) = self.viewport.as_ref() else {
            return;
        };
        let transforms = &viewport.transforms;
        let bounds = self
            .editor
            .with_puppet(session, |p| picking::world_bounds(p, transforms, &cores))
            .ok()
            .flatten();
        if let Some((min, max)) = bounds {
            let rect = self.last_viewport_rect.unwrap_or(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(16.0, 9.0),
            ));
            self.camera.focus(rect, min, max);
        }
    }

    pub(super) fn draw_selection_bounds(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        session: SessionId,
    ) {
        if self.selection.is_empty() {
            return;
        }
        let cores: Vec<u32> = self
            .selection
            .iter()
            .filter_map(|&r| self.core_of_ref(r))
            .collect();
        let Some(viewport) = self.viewport.as_ref() else {
            return;
        };
        let transforms = &viewport.transforms;
        let bounds = self
            .editor
            .with_puppet(session, |p| picking::world_bounds(p, transforms, &cores))
            .ok()
            .flatten();
        if let Some((min, max)) = bounds {
            let a = self.camera.world_to_screen(rect, min);
            let b = self.camera.world_to_screen(rect, max);
            let sel_rect = egui::Rect::from_two_pos(a, b);
            ui.painter().rect_stroke(
                sel_rect,
                0.0,
                egui::Stroke::new(1.5_f32, ui.visuals().selection.bg_fill),
                egui::StrokeKind::Outside,
            );
        }
    }

    /// Resolve the primary selection into gizmo-space data.
    pub(super) fn gizmo_target(&self) -> Option<GizmoTarget> {
        let session = self.session?;
        let primary = self.primary()?;
        let core = self.core_of_ref(primary)?;
        let mapping = self.mapping.as_ref()?;
        let clp = *mapping.clp_of_ref.get(&primary.0)?;
        let parent_core = mapping
            .parent_of
            .get(clp)
            .copied()
            .flatten()
            .and_then(|pi| mapping.core_of.get(pi).copied())
            .filter(|&c| c != u32::MAX);
        let viewport = self.viewport.as_ref()?;
        let m = viewport.transforms.get(catchlight_core::NodeIdx(core));
        let origin = m.transform_point3(glam::vec3(0.0, 0.0, 0.0)).truncate();
        let parent_world = parent_core
            .map(|c| viewport.transforms.get(catchlight_core::NodeIdx(c)))
            .unwrap_or(glam::Mat4::IDENTITY);

        // Recording edits the *posed* value; document edits start from base.
        let (translation, rotation, scale) = if self.armed.is_some() {
            self.editor
                .with_puppet(session, |p| {
                    p.get(catchlight_core::NodeIdx(core)).map(|n| {
                        (
                            n.transform.translation.to_array(),
                            n.transform.rotation.to_array(),
                            n.transform.scale.to_array(),
                        )
                    })
                })
                .ok()
                .flatten()?
        } else {
            self.editor
                .with_model(session, |model, refs| {
                    model.node(refs.node_id(primary)?).map(|n| {
                        (
                            n.transform.translation,
                            n.transform.rotation,
                            n.transform.scale,
                        )
                    })
                })
                .ok()
                .flatten()?
        };
        Some(GizmoTarget {
            origin_world: origin,
            parent_world,
            translation,
            rotation,
            scale,
        })
    }
}
