//! Mesh edit mode: a tool-local editing session over a [`WorkingMesh`], with
//! its own snapshot undo stack. The document is untouched until Apply, which
//! flattens the CDT (alpha-culled) into one `MeshSet` command — the document
//! undo sees a single step, and the deform re-fit rides in it server-side.

use std::collections::HashSet;

use catchlight_editor_core::{
    contour_automesh, grid_automesh, AlphaMask, ContourKnobs, UvMap, WorkingMesh,
};
use catchlight_editor_protocol::NodeId;
use eframe::egui;

use crate::camera::EditorCamera;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MeshTool {
    /// Add / select / drag / delete vertices.
    Point,
    /// Pin / unpin constraint edges between two picked vertices.
    Connect,
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
    pub status: String,
}

struct VertDrag {
    vertex: u32,
    mirror: Option<u32>,
    start_pos: [f32; 2],
    start_world: glam::Vec2,
}

pub(crate) enum MeshEditOutcome {
    Continue,
    Apply,
    Cancel,
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
            status: String::new(),
        };
        s.retriangulate();
        s
    }

    fn retriangulate(&mut self) {
        self.tris = self.working.triangulate().unwrap_or_default();
    }

    fn snapshot(&mut self) {
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
        // Keyboard: delete + nudge.
        if !ui.ctx().egui_wants_keyboard_input() {
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
                MeshTool::Connect => {}
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

    /// Overlay: derived wireframe, pinned edges, vertices, marquee.
    pub(crate) fn draw(&self, ui: &egui::Ui, rect: egui::Rect, camera: &EditorCamera) {
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

    /// The mode's tool panel (drawn in place of the node tree). Returns
    /// Apply/Cancel when the user is done.
    pub(crate) fn panel_ui(
        &mut self,
        ui: &mut egui::Ui,
        copy_sources: &[(NodeId, String)],
    ) -> (MeshEditOutcome, Option<NodeId>) {
        let mut outcome = MeshEditOutcome::Continue;
        let mut copy_from = None;
        ui.horizontal(|ui| {
            let can_apply = !self.tris.is_empty();
            if ui
                .add_enabled(can_apply, egui::Button::new("✔ Apply"))
                .clicked()
            {
                outcome = MeshEditOutcome::Apply;
            }
            if ui.button("✖ Cancel").clicked() {
                outcome = MeshEditOutcome::Cancel;
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            for (label, tool) in [("Point", MeshTool::Point), ("Connect", MeshTool::Connect)] {
                if ui.selectable_label(self.tool == tool, label).clicked() {
                    self.tool = tool;
                    self.connect_from = None;
                }
            }
            ui.separator();
            ui.checkbox(&mut self.mirror_x, "mirror X");
            ui.checkbox(&mut self.mirror_y, "mirror Y");
        });
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
                        self.knobs.threshold,
                        self.grid_cols,
                        self.grid_rows,
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
        (outcome, copy_from)
    }
}
