//! Mesh edit mode: a tool-local editing session over a [`WorkingMesh`], with
//! its own snapshot undo stack. The document is untouched until Apply, which
//! flattens the CDT (alpha-culled) into one `MeshSet` command — the document
//! undo sees a single step, and the deform re-fit rides in it server-side.
//!
//! Invariants this module carries:
//!
//! - **A seam names vertices of the mesh the document holds, so the seam tool
//!   is only reachable while the working mesh still is that mesh** — on entry,
//!   or straight after an Apply. Filling a slot from an edited working mesh
//!   would point it at an index the document does not have yet, which is
//!   exactly the mistake seams exist to prevent.
//!
//! - **Seam and weld edits are document edits.** Unlike everything else in
//!   this mode they bump the revision the moment they are made, so they land
//!   on the *document's* undo stack, not the mode's; the seam panel offers its
//!   own undo button for that reason.
//!
//! - **Apply does not leave the mode when it emptied a slot.** Re-meshing a
//!   part empties every seam slot on it (which vertex fills a slot is a claim
//!   about the mesh that just went away), and the author is the only one who
//!   can say where they go now — so Apply hands the mode straight to the seam
//!   tool with the list. [`crate::app::App`] holds the commit gate that keeps
//!   the model from being saved half-repaired.

use std::collections::HashSet;

use catchlight_editor_core::{
    contour_automesh, grid_automesh, AlphaMask, ContourKnobs, GridKnobs, UvMap, WorkingMesh,
};
use catchlight_editor_protocol::{NodeId, SeamAddr, SeamId, SeamInfo, SlotId, WeldInfo};
use eframe::egui;

use crate::camera::EditorCamera;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MeshTool {
    /// Add / select / drag / delete vertices.
    Point,
    /// Pin / unpin constraint edges between two picked vertices.
    Connect,
    /// Name vertices as seam slots, and weld a seam to another part's.
    /// Only reachable while the working mesh is the document's.
    Seam,
}

/// What the mesh editor asks the app to do. The app owns every command; this
/// module owns no session.
pub(crate) enum MeshEditAction {
    Apply,
    Cancel,
    /// Replace the working mesh with another node's topology.
    CopyFrom(NodeId),
    Seam(SeamAction),
}

/// A seam or weld edit. Each is one document command, so each is one undo
/// entry — unlike the working-mesh edits around them.
pub(crate) enum SeamAction {
    AddSeam(SeamId),
    DeleteSeam(SeamId),
    AddSlot {
        seam: SeamId,
        slot: SlotId,
    },
    DeleteSlot {
        seam: SeamId,
        slot: SlotId,
    },
    ClearSlot {
        seam: SeamId,
        slot: SlotId,
    },
    /// Point a slot at one of the part's vertices — the click in the viewport.
    FillSlot {
        seam: SeamId,
        slot: SlotId,
        vertex: u32,
    },
    /// Pair this part's seam with another part's, slot by slot.
    Weld {
        seam: SeamId,
        other: SeamAddr,
    },
    /// One slot's share of the point its two welded vertices meet at.
    SetWeight {
        seam: SeamId,
        other: SeamAddr,
        slot: SlotId,
        weight: f32,
    },
    /// The document's undo, not the mode's.
    Undo,
}

/// What the seam panel reads: this part's seams, the welds naming any of
/// them, and the seams elsewhere in the model a weld could reach.
#[derive(Default)]
pub(crate) struct SeamView {
    pub seams: Vec<SeamInfo>,
    pub welds: Vec<WeldInfo>,
    /// (node, node name, seam) for every seam on another part.
    pub others: Vec<(NodeId, String, SeamId)>,
}

pub(crate) struct MeshEditState {
    pub node: NodeId,
    pub core: u32,
    pub working: WorkingMesh,
    /// Cached triangulation of `working` (the live preview).
    pub tris: Vec<[u32; 3]>,
    undo: Vec<WorkingMesh>,
    redo: Vec<WorkingMesh>,
    pub selection: HashSet<u32>,
    pub tool: MeshTool,
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub uv_map: UvMap,
    pub alpha: Option<AlphaMask>,
    pub knobs: ContourKnobs,
    pub grid_cols: u32,
    pub grid_rows: u32,
    /// Node world matrix (rest verts × this = viewport space).
    pub node_world: glam::Mat4,
    drag: Option<VertDrag>,
    marquee: Option<egui::Pos2>,
    connect_from: Option<u32>,
    /// The working mesh has been edited since it last matched the document's.
    /// While it has, a vertex index here names nothing the document holds, so
    /// the seam tool is out.
    edited: bool,
    /// The slot the next viewport click fills.
    armed_slot: Option<(SeamId, SlotId)>,
    /// What the last Apply emptied and nobody has refilled — the prompt.
    pub emptied: Vec<(SeamId, SlotId)>,
    /// Drained by the app once per frame.
    pub actions: Vec<MeshEditAction>,
    pub status: String,
}

struct VertDrag {
    vertex: u32,
    mirror: Option<u32>,
    start_pos: [f32; 2],
    start_world: glam::Vec2,
}

const AXIS_SNAP: f32 = 4.0;

impl MeshEditState {
    pub(crate) fn new(
        node: NodeId,
        core: u32,
        working: WorkingMesh,
        uv_map: UvMap,
        alpha: Option<AlphaMask>,
        node_world: glam::Mat4,
    ) -> Self {
        let mut s = Self {
            node,
            core,
            working,
            tris: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            selection: HashSet::new(),
            tool: MeshTool::Point,
            mirror_x: false,
            mirror_y: false,
            uv_map,
            alpha,
            knobs: ContourKnobs::default(),
            grid_cols: 6,
            grid_rows: 6,
            node_world,
            drag: None,
            marquee: None,
            connect_from: None,
            edited: false,
            armed_slot: None,
            emptied: Vec::new(),
            actions: Vec::new(),
            status: String::new(),
        };
        s.retriangulate();
        s
    }

    /// Re-seat the mode on the document's mesh — what Apply does when it
    /// emptied slots the author has to refill before the model can be saved.
    pub(crate) fn reseat(&mut self, working: WorkingMesh, emptied: Vec<(SeamId, SlotId)>) {
        self.working = working;
        self.undo.clear();
        self.redo.clear();
        self.selection.clear();
        self.edited = false;
        self.armed_slot = None;
        self.emptied = emptied;
        self.tool = MeshTool::Seam;
        self.retriangulate();
    }

    /// Is the working mesh still the one the document holds?
    pub(crate) fn matches_document(&self) -> bool {
        !self.edited
    }

    fn retriangulate(&mut self) {
        self.tris = self.working.triangulate().unwrap_or_default();
    }

    fn snapshot(&mut self) {
        self.edited = true;
        self.undo.push(self.working.clone());
        if self.undo.len() > 64 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Swap in a new working mesh as one undoable in-mode step (copy-from-node,
    /// automesh).
    pub(crate) fn replace_working(&mut self, working: WorkingMesh) {
        self.snapshot();
        self.working = working;
        self.selection.clear();
        self.retriangulate();
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some() || self.marquee.is_some()
    }

    pub(crate) fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.working, prev));
            self.selection.clear();
            self.retriangulate();
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.working, next));
            self.selection.clear();
            self.retriangulate();
        }
    }

    fn local_of_world(&self, world: glam::Vec2) -> Option<[f32; 2]> {
        let inv = catchlight_core::checked_affine_inverse(self.node_world)?;
        let p = inv.transform_point3(glam::vec3(world.x, world.y, 0.0));
        let o = self.working.origin;
        Some([p.x + o[0], p.y + o[1]])
    }

    fn world_of_local(&self, local: [f32; 2]) -> glam::Vec2 {
        let o = self.working.origin;
        self.node_world
            .transform_point3(glam::vec3(local[0] - o[0], local[1] - o[1], 0.0))
            .truncate()
    }

    fn screen_of_vertex(&self, rect: egui::Rect, camera: &EditorCamera, i: u32) -> egui::Pos2 {
        camera.world_to_screen(rect, self.world_of_local(self.working.pos(i)))
    }

    fn vertex_at(&self, rect: egui::Rect, camera: &EditorCamera, pos: egui::Pos2) -> Option<u32> {
        let mut best = None;
        let mut best_d = 9.0f32;
        for i in 0..self.working.vertex_count() as u32 {
            let d = (self.screen_of_vertex(rect, camera, i) - pos).length();
            if d < best_d {
                best_d = d;
                best = Some(i);
            }
        }
        best
    }

    fn mirror_partner(&self, i: u32) -> Option<u32> {
        if !self.mirror_x && !self.mirror_y {
            return None;
        }
        let p = self.working.pos(i);
        let target = [
            if self.mirror_x { -p[0] } else { p[0] },
            if self.mirror_y { -p[1] } else { p[1] },
        ];
        if (target[0] - p[0]).abs() < 1e-3 && (target[1] - p[1]).abs() < 1e-3 {
            return None; // on-axis vertex mirrors onto itself
        }
        (0..self.working.vertex_count() as u32).find(|&j| {
            j != i && {
                let q = self.working.pos(j);
                (q[0] - target[0]).abs() < 1e-2 && (q[1] - target[1]).abs() < 1e-2
            }
        })
    }

    fn mirrored_local(&self, local: [f32; 2]) -> [f32; 2] {
        [
            if self.mirror_x { -local[0] } else { local[0] },
            if self.mirror_y { -local[1] } else { local[1] },
        ]
    }

    /// Snap near-axis coordinates onto the mirror axes (screen-scaled).
    fn axis_snap(&self, local: [f32; 2], px_per_unit: f32) -> [f32; 2] {
        let eps = AXIS_SNAP / px_per_unit.max(1e-3);
        [
            if self.mirror_x && local[0].abs() < eps {
                0.0
            } else {
                local[0]
            },
            if self.mirror_y && local[1].abs() < eps {
                0.0
            } else {
                local[1]
            },
        ]
    }

    /// Viewport interaction while mesh editing. Returns true when the pointer
    /// was consumed.
    pub(crate) fn interact(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        resp: &egui::Response,
        camera: &EditorCamera,
    ) -> bool {
        let mods = ui.input(|i| i.modifiers);
        // Keyboard: delete + nudge. Not while the seam tool is up — it is the
        // one tool that must leave the mesh exactly as the document has it.
        if !ui.ctx().egui_wants_keyboard_input() && self.tool != MeshTool::Seam {
            if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
                && !self.selection.is_empty()
            {
                self.snapshot();
                let doomed: Vec<u32> = self.selection.drain().collect();
                self.working.delete_vertices(&doomed);
                self.retriangulate();
            }
            let nudge = ui.input(|i| {
                let step = if i.modifiers.shift { 10.0 } else { 1.0 };
                let mut d = [0.0f32; 2];
                if i.key_pressed(egui::Key::ArrowLeft) {
                    d[0] -= step;
                }
                if i.key_pressed(egui::Key::ArrowRight) {
                    d[0] += step;
                }
                if i.key_pressed(egui::Key::ArrowUp) {
                    d[1] += step;
                }
                if i.key_pressed(egui::Key::ArrowDown) {
                    d[1] -= step;
                }
                d
            });
            if (nudge[0] != 0.0 || nudge[1] != 0.0) && !self.selection.is_empty() {
                self.snapshot();
                let sel: Vec<u32> = self.selection.iter().copied().collect();
                for i in sel {
                    let p = self.working.pos(i);
                    let _ = self
                        .working
                        .move_vertex(i, [p[0] + nudge[0], p[1] + nudge[1]]);
                }
                self.retriangulate();
            }
        }

        let pointer = resp.interact_pointer_pos();

        if resp.drag_started_by(egui::PointerButton::Primary) {
            let Some(pos) = pointer else { return true };
            match self.tool {
                MeshTool::Point => {
                    if let Some(v) = self.vertex_at(rect, camera, pos) {
                        if !self.selection.contains(&v) && !mods.ctrl {
                            self.selection = HashSet::from([v]);
                        }
                        self.selection.insert(v);
                        self.snapshot();
                        self.drag = Some(VertDrag {
                            vertex: v,
                            mirror: self.mirror_partner(v),
                            start_pos: self.working.pos(v),
                            start_world: camera.screen_to_world(rect, pos),
                        });
                    } else {
                        self.marquee = Some(pos);
                    }
                }
                MeshTool::Connect | MeshTool::Seam => {}
            }
            return true;
        }

        if let Some(drag) = &self.drag {
            let Some(pos) = pointer.or_else(|| resp.ctx.pointer_latest_pos()) else {
                return true;
            };
            let world = camera.screen_to_world(rect, pos);
            let delta = world - drag.start_world;
            let Some(inv) = catchlight_core::checked_affine_inverse(self.node_world) else {
                self.drag = None;
                self.status = "cannot edit a mesh under a singular transform".into();
                return true;
            };
            let a = inv.transform_point3(glam::Vec3::ZERO);
            let b = inv.transform_point3(glam::vec3(delta.x, delta.y, 0.0));
            let local_delta = (b - a).truncate();
            let mut target = [
                drag.start_pos[0] + local_delta.x,
                drag.start_pos[1] + local_delta.y,
            ];
            target = self.axis_snap(target, camera.scale(rect));
            let (vertex, mirror) = (drag.vertex, drag.mirror);
            if self.working.move_vertex(vertex, target).is_ok() {
                if let Some(m) = mirror {
                    let _ = self.working.move_vertex(m, self.mirrored_local(target));
                }
            }
            if resp.drag_stopped_by(egui::PointerButton::Primary) {
                self.drag = None;
                self.retriangulate();
            }
            return true;
        }

        if let Some(start) = self.marquee {
            if resp.drag_stopped_by(egui::PointerButton::Primary) {
                if let Some(pos) = pointer.or_else(|| resp.ctx.pointer_latest_pos()) {
                    let sel_rect = egui::Rect::from_two_pos(start, pos);
                    if !mods.ctrl {
                        self.selection.clear();
                    }
                    for i in 0..self.working.vertex_count() as u32 {
                        if sel_rect.contains(self.screen_of_vertex(rect, camera, i)) {
                            self.selection.insert(i);
                        }
                    }
                }
                self.marquee = None;
            }
            return true;
        }

        if resp.clicked() {
            let Some(pos) = pointer else { return true };
            match self.tool {
                MeshTool::Point => match self.vertex_at(rect, camera, pos) {
                    Some(v) => {
                        if mods.ctrl {
                            if !self.selection.remove(&v) {
                                self.selection.insert(v);
                            }
                        } else {
                            self.selection = HashSet::from([v]);
                        }
                    }
                    None => {
                        let world = camera.screen_to_world(rect, pos);
                        let Some(mut local) = self.local_of_world(world) else {
                            self.status = "cannot edit a mesh under a singular transform".into();
                            return true;
                        };
                        local = self.axis_snap(local, camera.scale(rect));
                        self.snapshot();
                        match self.working.add_vertex(local) {
                            Ok(v) => {
                                let mirrored = self.mirrored_local(local);
                                if (self.mirror_x || self.mirror_y) && mirrored != local {
                                    let _ = self.working.add_vertex(mirrored);
                                }
                                self.selection = HashSet::from([v]);
                                self.retriangulate();
                            }
                            Err(e) => self.status = format!("add: {e}"),
                        }
                    }
                },
                MeshTool::Seam => {
                    // The click that fills a slot. Vertex indices here are the
                    // document's, because the seam tool only runs while the
                    // working mesh is the document's mesh.
                    match (self.armed_slot.clone(), self.vertex_at(rect, camera, pos)) {
                        (Some((seam, slot)), Some(vertex)) => {
                            self.armed_slot = None;
                            self.actions
                                .push(MeshEditAction::Seam(SeamAction::FillSlot {
                                    seam,
                                    slot,
                                    vertex,
                                }));
                        }
                        (Some(_), None) => {
                            self.status = "click a vertex to fill the slot".into();
                        }
                        (None, _) => {}
                    }
                }
                MeshTool::Connect => {
                    if let Some(v) = self.vertex_at(rect, camera, pos) {
                        match self.connect_from.take() {
                            None => self.connect_from = Some(v),
                            Some(a) if a == v => {}
                            Some(a) => {
                                self.snapshot();
                                if self.working.has_constraint(a, v) {
                                    self.working.remove_constraint(a, v);
                                } else if let Err(e) = self.working.add_constraint(a, v) {
                                    self.status = format!("pin: {e}");
                                    self.undo.pop();
                                }
                                self.retriangulate();
                            }
                        }
                    } else {
                        self.connect_from = None;
                    }
                }
            }
            return true;
        }
        true
    }

    /// Overlay: derived wireframe, pinned edges, vertices, marquee — and,
    /// under the seam tool, which vertices a slot already names.
    pub(crate) fn draw(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        camera: &EditorCamera,
        view: &SeamView,
    ) {
        let paint = ui.painter_at(rect);
        let wire = egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(90, 150, 240, 110),
        );
        for t in &self.tris {
            let p = [
                self.screen_of_vertex(rect, camera, t[0]),
                self.screen_of_vertex(rect, camera, t[1]),
                self.screen_of_vertex(rect, camera, t[2]),
            ];
            paint.line_segment([p[0], p[1]], wire);
            paint.line_segment([p[1], p[2]], wire);
            paint.line_segment([p[2], p[0]], wire);
        }
        let pin = egui::Stroke::new(1.8_f32, egui::Color32::from_rgb(240, 170, 60));
        for &(a, b) in &self.working.constraints {
            paint.line_segment(
                [
                    self.screen_of_vertex(rect, camera, a),
                    self.screen_of_vertex(rect, camera, b),
                ],
                pin,
            );
        }
        for i in 0..self.working.vertex_count() as u32 {
            let pos = self.screen_of_vertex(rect, camera, i);
            let selected = self.selection.contains(&i);
            let color = if Some(i) == self.connect_from {
                egui::Color32::from_rgb(240, 90, 90)
            } else if selected {
                egui::Color32::from_rgb(255, 220, 90)
            } else {
                egui::Color32::from_rgb(90, 150, 240)
            };
            paint.circle_filled(pos, if selected { 4.0 } else { 3.0 }, color);
        }
        if self.tool == MeshTool::Seam {
            // A filled slot is a name for a vertex: say which, and say it
            // loudly while the author is picking one.
            let picking = self.armed_slot.is_some();
            for seam in &view.seams {
                for slot in &seam.slots {
                    let Some(v) = slot
                        .vertex
                        .filter(|v| *v < self.working.vertex_count() as u32)
                    else {
                        continue;
                    };
                    let pos = self.screen_of_vertex(rect, camera, v);
                    paint.circle_stroke(
                        pos,
                        6.0,
                        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(120, 220, 160)),
                    );
                    paint.text(
                        pos + egui::vec2(8.0, -8.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{}·{}", seam.id, slot.id),
                        egui::FontId::proportional(10.0),
                        egui::Color32::from_rgb(120, 220, 160),
                    );
                }
            }
            if picking {
                for i in 0..self.working.vertex_count() as u32 {
                    paint.circle_stroke(
                        self.screen_of_vertex(rect, camera, i),
                        5.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 220, 90)),
                    );
                }
            }
        }
        if let (Some(start), Some(cur)) = (self.marquee, ui.ctx().pointer_latest_pos()) {
            paint.rect_stroke(
                egui::Rect::from_two_pos(start, cur),
                0.0,
                egui::Stroke::new(1.0_f32, ui.visuals().selection.bg_fill),
                egui::StrokeKind::Inside,
            );
        }
        if self.mirror_x {
            let a = camera.world_to_screen(rect, self.world_of_local([0.0, -1e5]));
            let b = camera.world_to_screen(rect, self.world_of_local([0.0, 1e5]));
            paint.line_segment(
                [a, b],
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(240, 90, 90, 90),
                ),
            );
        }
        if self.mirror_y {
            let a = camera.world_to_screen(rect, self.world_of_local([-1e5, 0.0]));
            let b = camera.world_to_screen(rect, self.world_of_local([1e5, 0.0]));
            paint.line_segment(
                [a, b],
                egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_rgba_unmultiplied(90, 240, 90, 90),
                ),
            );
        }
    }

    /// The mode's tool panel (drawn in place of the node tree). Pushes what
    /// the author asked for into [`Self::actions`], which the app drains.
    pub(crate) fn panel_ui(
        &mut self,
        ui: &mut egui::Ui,
        copy_sources: &[(NodeId, String)],
        seams: &SeamView,
    ) {
        ui.horizontal(|ui| {
            let can_apply = !self.tris.is_empty();
            // With nothing edited there is nothing to apply, and re-applying
            // would empty the seam slots all over again — so the button says
            // what it does, which is leave.
            let label = if self.matches_document() {
                "✔ Done"
            } else {
                "✔ Apply"
            };
            if ui
                .add_enabled(can_apply, egui::Button::new(label))
                .clicked()
            {
                self.actions.push(MeshEditAction::Apply);
            }
            if ui.button("✖ Cancel").clicked() {
                self.actions.push(MeshEditAction::Cancel);
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            for (label, tool) in [("Point", MeshTool::Point), ("Connect", MeshTool::Connect)] {
                if ui.selectable_label(self.tool == tool, label).clicked() {
                    self.tool = tool;
                    self.connect_from = None;
                    self.armed_slot = None;
                }
            }
            // A slot names a vertex of the mesh the document holds, so the
            // seam tool waits for the edits to land.
            let seam_ok = self.matches_document();
            let seam = ui.add_enabled(
                seam_ok,
                egui::Button::new("Seams").selected(self.tool == MeshTool::Seam),
            );
            if seam.clicked() {
                self.tool = MeshTool::Seam;
                self.connect_from = None;
            }
            if !seam_ok {
                seam.on_hover_text(
                    "apply the mesh first — a seam names vertices of the mesh \
                     the document holds",
                );
            }
            ui.separator();
            ui.checkbox(&mut self.mirror_x, "mirror X");
            ui.checkbox(&mut self.mirror_y, "mirror Y");
        });
        if self.tool == MeshTool::Seam {
            self.seam_panel(ui, seams);
            if !self.status.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(240, 170, 60), &self.status);
            }
            return;
        }
        ui.horizontal(|ui| {
            if ui.button("⟲ undo").clicked() {
                self.undo();
            }
            if ui.button("⟳ redo").clicked() {
                self.redo();
            }
            ui.label(format!(
                "{} verts · {} tris · {} pins",
                self.working.vertex_count(),
                self.tris.len(),
                self.working.constraints.len()
            ));
        });
        ui.separator();
        ui.label("Automesh");
        ui.horizontal(|ui| {
            let mut th = self.knobs.threshold as u32;
            ui.add(
                egui::DragValue::new(&mut th)
                    .range(0..=255)
                    .prefix("alpha ≥ "),
            );
            self.knobs.threshold = th as u8;
            ui.add(
                egui::DragValue::new(&mut self.knobs.simplify)
                    .speed(0.2)
                    .range(0.5..=64.0)
                    .prefix("simplify "),
            );
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.knobs.margin)
                    .range(0..=64)
                    .prefix("margin "),
            );
            ui.add(
                egui::DragValue::new(&mut self.knobs.spacing)
                    .range(0..=512)
                    .prefix("fill "),
            );
            if ui.button("Contour").clicked() {
                if let Some(alpha) = &self.alpha {
                    match contour_automesh(alpha, &self.knobs, &self.uv_map, self.working.origin) {
                        Ok(mesh) => {
                            self.snapshot();
                            self.working = mesh;
                            self.selection.clear();
                            self.retriangulate();
                        }
                        Err(e) => self.status = format!("contour: {e}"),
                    }
                } else {
                    self.status = "no texture to trace".into();
                }
            }
        });
        ui.horizontal(|ui| {
            let mut cols = self.grid_cols;
            let mut rows = self.grid_rows;
            ui.add(
                egui::DragValue::new(&mut cols)
                    .range(1..=64)
                    .prefix("cols "),
            );
            ui.add(
                egui::DragValue::new(&mut rows)
                    .range(1..=64)
                    .prefix("rows "),
            );
            self.grid_cols = cols;
            self.grid_rows = rows;
            if ui.button("Grid").clicked() {
                if let Some(alpha) = &self.alpha {
                    match grid_automesh(
                        alpha,
                        &GridKnobs {
                            threshold: self.knobs.threshold,
                            cols: self.grid_cols,
                            rows: self.grid_rows,
                            ..GridKnobs::default()
                        },
                        &self.uv_map,
                        self.working.origin,
                    ) {
                        Ok(mesh) => {
                            self.snapshot();
                            self.working = mesh;
                            self.selection.clear();
                            self.retriangulate();
                        }
                        Err(e) => self.status = format!("grid: {e}"),
                    }
                } else {
                    self.status = "no texture for grid bounds".into();
                }
            }
        });
        let mut copy_from = None;
        if !copy_sources.is_empty() {
            ui.menu_button("Copy mesh from…", |ui| {
                egui::ScrollArea::vertical()
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for (node, name) in copy_sources {
                            if ui.button(name).clicked() {
                                copy_from = Some(node.clone());
                                ui.close();
                            }
                        }
                    });
            });
        }
        if !self.status.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(240, 170, 60), &self.status);
        }
        if let Some(node) = copy_from {
            self.actions.push(MeshEditAction::CopyFrom(node));
        }
    }

    /// The seam and weld tool. Every button here is one document command, so
    /// every one of them is its own undo entry.
    fn seam_panel(&mut self, ui: &mut egui::Ui, view: &SeamView) {
        if !self.emptied.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(240, 170, 60),
                format!(
                    "{} slot(s) emptied by the new mesh — refill them, or \
                     delete the seam. The model will not save until you do.",
                    self.emptied.len()
                ),
            );
            ui.separator();
        }
        ui.horizontal(|ui| {
            let id = ui.id().with("new-seam");
            let mut name: String = ui
                .ctx()
                .data_mut(|d| d.get_temp(id))
                .unwrap_or_else(|| "seam".to_string());
            ui.add(egui::TextEdit::singleline(&mut name).desired_width(90.0));
            ui.ctx().data_mut(|d| d.insert_temp(id, name.clone()));
            if ui.button("＋ seam").clicked() {
                match SeamId::new(&name) {
                    Ok(seam) => self
                        .actions
                        .push(MeshEditAction::Seam(SeamAction::AddSeam(seam))),
                    Err(e) => self.status = format!("seam id: {e}"),
                }
            }
            if ui
                .button("⟲ undo")
                .on_hover_text("seam edits are document edits — this is the document's undo")
                .clicked()
            {
                self.actions.push(MeshEditAction::Seam(SeamAction::Undo));
            }
        });
        if view.seams.is_empty() {
            ui.label("(no seams on this part)");
        }
        for seam in &view.seams {
            self.seam_row(ui, seam, view);
        }
    }

    fn seam_row(&mut self, ui: &mut egui::Ui, seam: &SeamInfo, view: &SeamView) {
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(seam.id.as_str());
            if ui
                .small_button("✕ seam")
                .on_hover_text("delete the seam, and every weld that named it")
                .clicked()
            {
                self.actions
                    .push(MeshEditAction::Seam(SeamAction::DeleteSeam(
                        seam.id.clone(),
                    )));
            }
        });
        for slot in &seam.slots {
            let armed = self
                .armed_slot
                .as_ref()
                .is_some_and(|(s, l)| *s == seam.id && *l == slot.id);
            ui.horizontal(|ui| {
                let filled = match slot.vertex {
                    Some(v) => format!("{} → v{v}", slot.id),
                    None => format!("{} → (unfilled)", slot.id),
                };
                if slot.vertex.is_none() {
                    ui.colored_label(egui::Color32::from_rgb(240, 170, 60), filled);
                } else {
                    ui.label(filled);
                }
                if ui
                    .selectable_label(armed, "pick")
                    .on_hover_text("then click a vertex in the viewport")
                    .clicked()
                {
                    self.armed_slot = if armed {
                        None
                    } else {
                        Some((seam.id.clone(), slot.id.clone()))
                    };
                }
                if ui.small_button("clear").clicked() {
                    self.actions
                        .push(MeshEditAction::Seam(SeamAction::ClearSlot {
                            seam: seam.id.clone(),
                            slot: slot.id.clone(),
                        }));
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("remove the slot — from this seam and every seam welded to it")
                    .clicked()
                {
                    self.actions
                        .push(MeshEditAction::Seam(SeamAction::DeleteSlot {
                            seam: seam.id.clone(),
                            slot: slot.id.clone(),
                        }));
                }
            });
        }
        ui.horizontal(|ui| {
            let id = ui.id().with(("new-slot", seam.id.as_str()));
            let mut name: String = ui
                .ctx()
                .data_mut(|d| d.get_temp(id))
                .unwrap_or_else(|| format!("s{}", seam.slots.len()));
            ui.add(egui::TextEdit::singleline(&mut name).desired_width(70.0));
            ui.ctx().data_mut(|d| d.insert_temp(id, name.clone()));
            if ui
                .button("＋ slot")
                .on_hover_text("reaches every seam welded to this one — a weld pairs slot by slot")
                .clicked()
            {
                match SlotId::new(&name) {
                    Ok(slot) => self.actions.push(MeshEditAction::Seam(SeamAction::AddSlot {
                        seam: seam.id.clone(),
                        slot,
                    })),
                    Err(e) => self.status = format!("slot id: {e}"),
                }
            }
        });
        self.weld_row(ui, seam, view);
    }

    /// The welds naming this seam, and the menu that makes one.
    fn weld_row(&mut self, ui: &mut egui::Ui, seam: &SeamInfo, view: &SeamView) {
        let here = |addr: &SeamAddr| addr.node == self.node && addr.seam == seam.id;
        for weld in view.welds.iter().filter(|w| here(&w.a) || here(&w.b)) {
            let other = if here(&weld.a) { &weld.b } else { &weld.a };
            let name = view
                .others
                .iter()
                .find(|(n, _, s)| *n == other.node && *s == other.seam)
                .map(|(_, name, _)| name.clone())
                .unwrap_or_else(|| other.node.to_string());
            ui.label(format!("welded to {name} · {}", other.seam));
            for w in &weld.weights {
                let mut weight = w.weight;
                let resp = ui.add(
                    egui::Slider::new(&mut weight, 0.0..=1.0)
                        .text(w.slot.as_str())
                        .clamping(egui::SliderClamping::Always),
                );
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                    self.actions
                        .push(MeshEditAction::Seam(SeamAction::SetWeight {
                            seam: seam.id.clone(),
                            other: other.clone(),
                            slot: w.slot.clone(),
                            weight,
                        }));
                }
            }
        }
        if view.others.is_empty() {
            return;
        }
        ui.menu_button("weld to…", |ui| {
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    for (node, name, other) in &view.others {
                        if ui.button(format!("{name} · {other}")).clicked() {
                            self.actions.push(MeshEditAction::Seam(SeamAction::Weld {
                                seam: seam.id.clone(),
                                other: SeamAddr {
                                    node: node.clone(),
                                    seam: other.clone(),
                                },
                            }));
                            ui.close();
                        }
                    }
                });
        });
    }
}
