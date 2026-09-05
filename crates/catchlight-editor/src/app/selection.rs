use super::*;

/// Selection, and the reads that need the puppet's evaluated frame.
///
/// **Every read of a transform goes through the session lock.** The puppet
/// owns the frame the viewport last drew; picking, the gizmo, the vertex
/// tools and the selection overlay all ask it inside a [`Editor::with_puppet`]
/// closure rather than against a copy taken at render time. A second copy on
/// the GUI side is one more thing to invalidate, and it was only ever a copy
/// of `puppet.transforms()`.
///
/// What makes reading it safe is the *rev gate*, not a copy: `App::rendered_rev`
/// says which model revision the last render evaluated, and the tools sit
/// out any frame where that is older than the session's. Right after an edit
/// the puppet has rebaked but not been ticked, so its frame describes a pose
/// nobody has recomputed yet.
impl App {
    pub(super) fn primary(&self) -> Option<NodeId> {
        self.selection.last().cloned()
    }

    /// The puppet slot a node occupies. Node Ids are what everything else
    /// names a node by; a slot exists only inside the puppet, and only the
    /// tools that read its evaluated frame use one.
    pub(super) fn core_of_ref(&self, id: &NodeId) -> Option<u32> {
        let session = self.session?;
        self.editor
            .with_puppet(session, |_model, p| p.node_idx(id).map(|idx| idx.0))
            .ok()
            .flatten()
    }

    pub(super) fn ref_of_core(&self, core: u32) -> Option<NodeId> {
        let session = self.session?;
        self.editor
            .with_puppet(session, |_model, p| {
                p.node_id(catchlight_core::NodeIdx(core)).cloned()
            })
            .ok()
            .flatten()
    }

    /// The node's world matrix in the frame the puppet last evaluated.
    pub(super) fn node_world(&self, core: u32) -> glam::Mat4 {
        let Some(session) = self.session else {
            return glam::Mat4::IDENTITY;
        };
        self.editor
            .with_puppet(session, |_model, p| {
                p.transforms().get(catchlight_core::NodeIdx(core))
            })
            .unwrap_or(glam::Mat4::IDENTITY)
    }

    /// Core ids of the isolated subtree (drawing filter), if isolation is on.
    pub(super) fn isolate_set(&self, root: &TreeNode) -> Option<HashSet<u32>> {
        let session = self.session?;
        let iso = self.isolated.as_ref()?;
        let sub = find_subtree(root, iso)?;
        let mut ids = Vec::new();
        collect_refs(sub, &mut |r| ids.push(r));
        self.editor
            .with_puppet(session, |_model, p| {
                ids.iter()
                    .filter_map(|id| p.node_idx(id).map(|idx| idx.0))
                    .collect()
            })
            .ok()
    }

    pub(super) fn pick(&mut self, session: SessionId, world: glam::Vec2) -> Vec<u32> {
        self.editor
            .with_puppet(session, |model, p| picking::pick_all(model, p, world))
            .unwrap_or_default()
    }

    pub(super) fn select(&mut self, node: NodeId, additive: bool, range: bool, visible: &[NodeId]) {
        if range {
            if let Some(anchor) = self.selection.last() {
                let a = visible.iter().position(|r| r == anchor);
                let b = visible.iter().position(|r| *r == node);
                if let (Some(a), Some(b)) = (a, b) {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    for r in &visible[lo..=hi] {
                        if !self.selection.contains(r) {
                            self.selection.push(r.clone());
                        }
                    }
                    return;
                }
            }
        }
        if additive {
            if let Some(pos) = self.selection.iter().position(|r| *r == node) {
                self.selection.remove(pos);
            } else {
                self.selection.push(node);
            }
        } else {
            self.selection = vec![node];
        }
    }

    /// World-space bounds of the whole selection, from the puppet's frame.
    fn selection_bounds(&self, session: SessionId) -> Option<(glam::Vec2, glam::Vec2)> {
        let selection = self.selection.clone();
        self.editor
            .with_puppet(session, |_model, p| {
                let cores: Vec<u32> = selection
                    .iter()
                    .filter_map(|r| p.node_idx(r).map(|idx| idx.0))
                    .collect();
                picking::world_bounds(p, &cores)
            })
            .ok()
            .flatten()
    }

    pub(super) fn focus_selected(&mut self) {
        let Some(session) = self.session else { return };
        if let Some((min, max)) = self.selection_bounds(session) {
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
        if let Some((min, max)) = self.selection_bounds(session) {
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
        // Recording edits the *posed* value, so the handle sits on the
        // puppet's frame; a model edit starts from the model's own.
        let armed = self.armed.is_some();
        self.editor
            .with_puppet(session, |model, p| {
                let core = p.node_idx(&primary)?;
                let m = p.transforms().get(core);
                let origin = m.transform_point3(glam::vec3(0.0, 0.0, 0.0)).truncate();
                let parent_world = model
                    .node(&primary)
                    .and_then(|n| n.parent())
                    .and_then(|parent| p.node_idx(parent))
                    .map(|parent| p.transforms().get(parent))
                    .unwrap_or(glam::Mat4::IDENTITY);
                let (translation, rotation, scale) = if armed {
                    let n = p.get(core)?;
                    (
                        n.transform.translation.to_array(),
                        n.transform.rotation.to_array(),
                        n.transform.scale.to_array(),
                    )
                } else {
                    let n = model.node(&primary)?;
                    (
                        n.transform.translation,
                        n.transform.rotation,
                        n.transform.scale,
                    )
                };
                Some(GizmoTarget {
                    origin_world: origin,
                    parent_world,
                    translation,
                    rotation,
                    scale,
                })
            })
            .ok()
            .flatten()
    }
}
