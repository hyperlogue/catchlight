//! Parameters panel: per-param pose controllers (full-width 1D track, 2D pad
//! with a value dot), the arm toggle, and the armed param's recording view
//! with keypoint dots and the bindings list.
//! Pure UI over snapshot data — emits [`ParamAction`]s the app applies.

use catchlight_editor_protocol::{NodeRef, ParamInfo, ParamRef};
use eframe::egui;

pub(crate) enum ParamAction {
    Pose {
        name: String,
        value: [f32; 2],
    },
    Arm(Option<ParamRef>),
    AddParam {
        name: String,
        vec2: bool,
    },
    Rename {
        param: ParamRef,
        name: String,
    },
    Delete(ParamRef),
    AxisInsert {
        param: ParamRef,
        axis: u8,
        value: f32,
    },
    AxisDelete {
        param: ParamRef,
        axis: u8,
        index: u32,
    },
    Flip {
        param: ParamRef,
        axis: u8,
    },
    BindingUnset {
        node: NodeRef,
        target: String,
    },
    BindingReset {
        node: NodeRef,
        target: String,
    },
    BindingDelete {
        node: NodeRef,
        target: String,
    },
    BindingInterpolate {
        node: NodeRef,
        target: String,
        mode: String,
    },
    BindingInvert {
        node: NodeRef,
        target: String,
    },
    CopyCell {
        node: NodeRef,
        target: String,
    },
    PasteCell {
        node: NodeRef,
        target: String,
    },
}

/// Authored-state of each grid cell for the armed param: 0 = none of the
/// bindings author it, 1 = some, 2 = all.
pub(crate) struct ArmedInfo {
    pub param: ParamRef,
    pub cell: [u32; 2],
    pub cell_states: Vec<u8>,
    pub bindings: Vec<BindingRow>,
}

pub(crate) struct BindingRow {
    pub node: NodeRef,
    pub node_name: String,
    pub target: String,
    pub interpolate: String,
    pub authored_at_cell: bool,
}

pub(crate) struct ParamsPanel<'a> {
    pub params: &'a [ParamInfo],
    pub pose: &'a dyn Fn(&str) -> [f32; 2],
    pub armed: Option<&'a ArmedInfo>,
    pub snap: &'a mut bool,
    pub can_paste: bool,
    pub actions: Vec<ParamAction>,
}

impl ParamsPanel<'_> {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.menu_button("＋ param", |ui| {
                let id = ui.id().with("new-param-name");
                let mut name: String = ui
                    .ctx()
                    .data_mut(|d| d.get_temp(id))
                    .unwrap_or_else(|| "param".to_string());
                ui.text_edit_singleline(&mut name);
                ui.ctx().data_mut(|d| d.insert_temp(id, name.clone()));
                if ui.button("add 1D").clicked() {
                    self.actions.push(ParamAction::AddParam {
                        name: name.clone(),
                        vec2: false,
                    });
                    ui.close();
                }
                if ui.button("add 2D").clicked() {
                    self.actions
                        .push(ParamAction::AddParam { name, vec2: true });
                    ui.close();
                }
            });
            ui.checkbox(self.snap, "snap");
        });

        for p in self.params {
            self.param_row(ui, p);
        }
    }

    /// The recording-mode panel: the armed param's keypoint controller and
    /// bindings list (the left panel replaces the node tree with this).
    pub(crate) fn show_recording(&mut self, ui: &mut egui::Ui) {
        let Some(armed) = self.armed else { return };
        let Some(p) = self.params.iter().find(|p| p.param == armed.param) else {
            return;
        };
        ui.label(format!(
            "{} @ ({}, {})",
            p.name, armed.cell[0], armed.cell[1]
        ));
        self.controller(ui, p, armed);
        ui.separator();
        ui.label("Bindings");
        // Binding rows are wider than the panel; scroll them sideways without
        // dragging the controller into the scroll region.
        egui::ScrollArea::horizontal()
            .id_salt("bindings-scroll")
            .show(ui, |ui| self.bindings_list(ui, armed));
    }

    fn param_row(&mut self, ui: &mut egui::Ui, p: &ParamInfo) {
        let armed_here = self.armed.map(|a| a.param) == Some(p.param);
        let value = (self.pose)(&p.name);
        ui.horizontal(|ui| {
            let arm = ui
                .selectable_label(armed_here, "⏺")
                .on_hover_text("arm for recording");
            if arm.clicked() {
                self.actions.push(ParamAction::Arm(if armed_here {
                    None
                } else {
                    Some(p.param)
                }));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(if p.vec2 {
                    format!("{:.2}, {:.2}", value[0], value[1])
                } else {
                    format!("{:.2}", value[0])
                });
                // Name fills the space left of the readout, left-aligned.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let label = ui.add(
                        egui::Label::new(&p.name)
                            .truncate()
                            .sense(egui::Sense::click()),
                    );
                    let label = label.on_hover_text(&p.name);
                    label.context_menu(|ui| self.param_menu(ui, p));
                });
            });
        });
        let armed = self.armed.filter(|a| a.param == p.param);
        if p.vec2 {
            self.pad_2d(ui, p, armed);
        } else {
            self.line_1d(ui, p, armed);
        }
        ui.add_space(6.0);
    }

    fn param_menu(&mut self, ui: &mut egui::Ui, p: &ParamInfo) {
        let id = ui.id().with(("rename", p.param.0));
        let mut name: String = ui
            .ctx()
            .data_mut(|d| d.get_temp(id))
            .unwrap_or_else(|| p.name.clone());
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut name);
            if ui.button("rename").clicked() {
                self.actions.push(ParamAction::Rename {
                    param: p.param,
                    name: name.clone(),
                });
                ui.close();
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(id, name));

        let value = (self.pose)(&p.name);
        // Menu labels speak param values; the wire speaks normalized 0..1.
        if ui
            .button(format!("insert X axis point at {:.3}", value[0]))
            .clicked()
        {
            self.actions.push(ParamAction::AxisInsert {
                param: p.param,
                axis: 0,
                value: norm(value[0], p.min[0], p.max[0]),
            });
            ui.close();
        }
        if p.vec2
            && ui
                .button(format!("insert Y axis point at {:.3}", value[1]))
                .clicked()
        {
            self.actions.push(ParamAction::AxisInsert {
                param: p.param,
                axis: 1,
                value: norm(value[1], p.min[1], p.max[1]),
            });
            ui.close();
        }
        let denorm = |t: f32, min: f32, max: f32| min + t * (max - min);
        if let Some(i) = nearest_interior(&p.axis_points_x, norm(value[0], p.min[0], p.max[0])) {
            if ui
                .button(format!(
                    "delete X axis point {:.3}",
                    denorm(p.axis_points_x[i], p.min[0], p.max[0])
                ))
                .clicked()
            {
                self.actions.push(ParamAction::AxisDelete {
                    param: p.param,
                    axis: 0,
                    index: i as u32,
                });
                ui.close();
            }
        }
        if p.vec2 {
            if let Some(i) = nearest_interior(&p.axis_points_y, norm(value[1], p.min[1], p.max[1]))
            {
                if ui
                    .button(format!(
                        "delete Y axis point {:.3}",
                        denorm(p.axis_points_y[i], p.min[1], p.max[1])
                    ))
                    .clicked()
                {
                    self.actions.push(ParamAction::AxisDelete {
                        param: p.param,
                        axis: 1,
                        index: i as u32,
                    });
                    ui.close();
                }
            }
        }
        ui.separator();
        if ui.button("flip X (mirror keypoints)").clicked() {
            self.actions.push(ParamAction::Flip {
                param: p.param,
                axis: 0,
            });
            ui.close();
        }
        if p.vec2 && ui.button("flip Y (mirror keypoints)").clicked() {
            self.actions.push(ParamAction::Flip {
                param: p.param,
                axis: 1,
            });
            ui.close();
        }
        ui.separator();
        if ui.button("delete param").clicked() {
            self.actions.push(ParamAction::Delete(p.param));
            ui.close();
        }
    }

    /// The armed param's controller: the same widgets as the list rows, with
    /// the authored-state dots.
    fn controller(&mut self, ui: &mut egui::Ui, p: &ParamInfo, armed: &ArmedInfo) {
        if p.vec2 {
            self.pad_2d(ui, p, Some(armed));
        } else {
            self.line_1d(ui, p, Some(armed));
        }
    }

    /// Full-width 1D track: axis-point ticks and a draggable value handle.
    /// When armed, ticks become authored-state dots.
    fn line_1d(&mut self, ui: &mut egui::Ui, p: &ParamInfo, armed: Option<&ArmedInfo>) {
        let value = (self.pose)(&p.name);
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(120.0), 24.0),
            egui::Sense::click_and_drag(),
        );
        let paint = ui.painter_at(rect);
        let vis = ui.visuals();
        // Inset so the handle circle stays inside the allocated rect.
        let track = rect.shrink2(egui::vec2(HANDLE_R + 2.0, 0.0));
        let cy = rect.center().y;
        paint.hline(
            track.x_range(),
            cy,
            egui::Stroke::new(2.0_f32, vis.widgets.inactive.bg_fill),
        );
        // `t` is normalized 0..1 — the space axis points live in.
        let at = |t: f32| egui::pos2(track.left() + t.clamp(0.0, 1.0) * track.width(), cy);
        for (xi, &ax) in p.axis_points_x.iter().enumerate() {
            let pos = at(ax);
            match armed {
                Some(a) => dot(
                    &paint,
                    pos,
                    a.cell_states.get(xi).copied().unwrap_or(0),
                    a.cell[0] == xi as u32,
                    vis,
                ),
                None => {
                    paint.vline(
                        pos.x,
                        (cy - 4.0)..=(cy + 4.0),
                        egui::Stroke::new(1.0_f32, vis.weak_text_color()),
                    );
                }
            }
        }
        handle(&paint, at(norm(value[0], p.min[0], p.max[0])));
        if let Some(pos) = drag_pos(&resp) {
            let t = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let t = self.maybe_snap(t, &p.axis_points_x);
            self.actions.push(ParamAction::Pose {
                name: p.name.clone(),
                value: [p.min[0] + t * (p.max[0] - p.min[0]), value[1]],
            });
        }
    }

    /// Full-width 2D pad: grid lines at the axis points and a circle pointer
    /// at the current value. When armed, intersections show authored-state
    /// dots.
    fn pad_2d(&mut self, ui: &mut egui::Ui, p: &ParamInfo, armed: Option<&ArmedInfo>) {
        let value = (self.pose)(&p.name);
        let w = ui.available_width().max(120.0);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(w, w.min(220.0)), egui::Sense::click_and_drag());
        let paint = ui.painter_at(rect);
        let vis = ui.visuals();
        paint.rect(
            rect,
            4.0,
            vis.extreme_bg_color,
            egui::Stroke::new(1.0_f32, vis.widgets.inactive.bg_fill),
            egui::StrokeKind::Inside,
        );
        // Inset so edge grid lines, dots, and the pointer stay inside the frame.
        let grid = rect.shrink(HANDLE_R + 4.0);
        // `t` is normalized 0..1 — the space axis points live in.
        let sx = |t: f32| grid.left() + t.clamp(0.0, 1.0) * grid.width();
        let sy = |t: f32| grid.bottom() - t.clamp(0.0, 1.0) * grid.height();
        let grid_stroke = egui::Stroke::new(1.0_f32, vis.widgets.inactive.bg_fill);
        for &ax in &p.axis_points_x {
            paint.vline(sx(ax), grid.y_range(), grid_stroke);
        }
        for &ay in &p.axis_points_y {
            paint.hline(grid.x_range(), sy(ay), grid_stroke);
        }
        if let Some(a) = armed {
            for (yi, &ay) in p.axis_points_y.iter().enumerate() {
                for (xi, &ax) in p.axis_points_x.iter().enumerate() {
                    let state = a
                        .cell_states
                        .get(yi * p.axis_points_x.len() + xi)
                        .copied()
                        .unwrap_or(0);
                    let is_cell = a.cell == [xi as u32, yi as u32];
                    dot(&paint, egui::pos2(sx(ax), sy(ay)), state, is_cell, vis);
                }
            }
        }
        handle(
            &paint,
            egui::pos2(
                sx(norm(value[0], p.min[0], p.max[0])),
                sy(norm(value[1], p.min[1], p.max[1])),
            ),
        );
        if let Some(pos) = drag_pos(&resp) {
            let tx = ((pos.x - grid.left()) / grid.width()).clamp(0.0, 1.0);
            let ty = ((grid.bottom() - pos.y) / grid.height()).clamp(0.0, 1.0);
            let tx = self.maybe_snap(tx, &p.axis_points_x);
            let ty = self.maybe_snap(ty, &p.axis_points_y);
            self.actions.push(ParamAction::Pose {
                name: p.name.clone(),
                value: [
                    p.min[0] + tx * (p.max[0] - p.min[0]),
                    p.min[1] + ty * (p.max[1] - p.min[1]),
                ],
            });
        }
    }

    fn bindings_list(&mut self, ui: &mut egui::Ui, armed: &ArmedInfo) {
        for row in &armed.bindings {
            ui.horizontal(|ui| {
                let mark = if row.authored_at_cell { "●" } else { "○" };
                ui.label(format!("{mark} {} · {}", row.node_name, row.target));
                egui::ComboBox::from_id_salt(("interp", row.node.0, &row.target))
                    .selected_text(&row.interpolate)
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for mode in ["nearest", "stepped", "linear", "cubic"] {
                            if ui.button(mode).clicked() {
                                self.actions.push(ParamAction::BindingInterpolate {
                                    node: row.node,
                                    target: row.target.clone(),
                                    mode: mode.into(),
                                });
                                ui.close();
                            }
                        }
                    });
                if ui
                    .small_button("reset")
                    .on_hover_text("author the identity value at this keypoint")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingReset {
                        node: row.node,
                        target: row.target.clone(),
                    });
                }
                if ui
                    .small_button("unset")
                    .on_hover_text("un-author this keypoint (back to derived)")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingUnset {
                        node: row.node,
                        target: row.target.clone(),
                    });
                }
                if ui.small_button("copy").clicked() {
                    self.actions.push(ParamAction::CopyCell {
                        node: row.node,
                        target: row.target.clone(),
                    });
                }
                if self.can_paste && ui.small_button("paste").clicked() {
                    self.actions.push(ParamAction::PasteCell {
                        node: row.node,
                        target: row.target.clone(),
                    });
                }
                if ui
                    .small_button("±")
                    .on_hover_text("negate every authored value")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingInvert {
                        node: row.node,
                        target: row.target.clone(),
                    });
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("delete binding")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingDelete {
                        node: row.node,
                        target: row.target.clone(),
                    });
                }
            });
        }
        if armed.bindings.is_empty() {
            ui.label("(no bindings yet — edit while armed to record)");
        }
    }

    /// Snap a normalized 0..1 coordinate to the nearest axis point.
    fn maybe_snap(&self, t: f32, points: &[f32]) -> f32 {
        if !*self.snap {
            return t;
        }
        let mut best = t;
        let mut best_d = f32::INFINITY;
        for &p in points {
            let d = (p - t).abs();
            if d < best_d {
                best_d = d;
                best = p;
            }
        }
        best
    }
}

const HANDLE_R: f32 = 5.0;

fn handle(paint: &egui::Painter, pos: egui::Pos2) {
    paint.circle_filled(pos, HANDLE_R, egui::Color32::from_rgb(90, 150, 240));
    paint.circle_stroke(
        pos,
        HANDLE_R,
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
}

fn drag_pos(resp: &egui::Response) -> Option<egui::Pos2> {
    (resp.dragged() || resp.clicked())
        .then(|| resp.interact_pointer_pos())
        .flatten()
}

fn norm(v: f32, min: f32, max: f32) -> f32 {
    if (max - min).abs() <= f32::EPSILON {
        0.0
    } else {
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    }
}

fn dot(
    paint: &egui::Painter,
    pos: egui::Pos2,
    state: u8,
    is_armed_cell: bool,
    vis: &egui::Visuals,
) {
    let color = match state {
        0 => vis.weak_text_color(),
        1 => egui::Color32::from_rgb(220, 180, 70),
        _ => egui::Color32::from_rgb(90, 200, 110),
    };
    if state == 0 {
        paint.circle_stroke(pos, 3.0, egui::Stroke::new(1.2_f32, color));
    } else {
        paint.circle_filled(pos, 3.0, color);
    }
    if is_armed_cell {
        paint.circle_stroke(pos, 5.5, egui::Stroke::new(1.5_f32, vis.selection.bg_fill));
    }
}

/// Nearest interior axis-point index to `v` (endpoints excluded — they define
/// the range and can't be deleted).
fn nearest_interior(points: &[f32], v: f32) -> Option<usize> {
    if points.len() <= 2 {
        return None;
    }
    points[1..points.len() - 1]
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - v)
                .abs()
                .partial_cmp(&(*b - v).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i + 1)
}
