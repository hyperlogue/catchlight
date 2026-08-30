//! Parameters panel: a full-width track per param with a value handle, the
//! arm toggle, and the armed param's recording view with keypoint dots and
//! the bindings list.
//! Pure UI over snapshot data — emits [`ParamAction`]s the app applies.
//!
//! A param is a scalar, so every controller here is one track. Authoring two
//! params jointly is a *binding* that names both, and the pad that edits one
//! is a view over any two params rather than a property of either — which is
//! why there is no 2-D controller in this file.

use catchlight_editor_protocol::{NodeId, ParamId, ParamInfo};
use eframe::egui;

pub(crate) enum ParamAction {
    Pose {
        param: ParamId,
        value: f32,
    },
    Arm(Option<ParamId>),
    AddParam {
        name: String,
    },
    /// Relabel: free, repeatable, and it never touches the Id.
    Rename {
        param: ParamId,
        name: String,
    },
    /// Open the Id-rename prompt (a confirmation the app owns).
    RenameId(ParamId),
    Delete(ParamId),
    KeyInsert {
        param: ParamId,
        value: f32,
    },
    KeyDelete {
        param: ParamId,
        index: u32,
    },
    Flip {
        param: ParamId,
    },
    BindingUnset {
        node: NodeId,
        target: String,
    },
    BindingReset {
        node: NodeId,
        target: String,
    },
    BindingDelete {
        node: NodeId,
        target: String,
    },
    BindingInterpolate {
        node: NodeId,
        target: String,
        mode: String,
    },
    BindingInvert {
        node: NodeId,
        target: String,
    },
    CopyCell {
        node: NodeId,
        target: String,
    },
    PasteCell {
        node: NodeId,
        target: String,
    },
}

/// Authored-state of each grid cell for the armed param: 0 = none of the
/// bindings author it, 1 = some, 2 = all.
pub(crate) struct ArmedInfo {
    pub param: ParamId,
    pub cell: [u32; 2],
    pub cell_states: Vec<u8>,
    pub bindings: Vec<BindingRow>,
}

pub(crate) struct BindingRow {
    pub node: NodeId,
    pub node_name: String,
    pub target: String,
    pub interpolate: String,
    pub authored_at_cell: bool,
}

pub(crate) struct ParamsPanel<'a> {
    pub params: &'a [ParamInfo],
    pub pose: &'a dyn Fn(&ParamId) -> f32,
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
                if ui.button("add").clicked() {
                    self.actions.push(ParamAction::AddParam { name });
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
        let Some(p) = self.params.iter().find(|p| p.id == armed.param) else {
            return;
        };
        ui.label(format!(
            "{} @ ({}, {})",
            p.name, armed.cell[0], armed.cell[1]
        ));
        self.line_1d(ui, p, Some(armed));
        ui.separator();
        ui.label("Bindings");
        // Binding rows are wider than the panel; scroll them sideways without
        // dragging the controller into the scroll region.
        egui::ScrollArea::horizontal()
            .id_salt("bindings-scroll")
            .show(ui, |ui| self.bindings_list(ui, armed));
    }

    fn param_row(&mut self, ui: &mut egui::Ui, p: &ParamInfo) {
        let armed_here = self.armed.map(|a| &a.param) == Some(&p.id);
        let value = (self.pose)(&p.id);
        ui.horizontal(|ui| {
            let arm = ui
                .selectable_label(armed_here, "⏺")
                .on_hover_text("arm for recording");
            if arm.clicked() {
                self.actions.push(ParamAction::Arm(if armed_here {
                    None
                } else {
                    Some(p.id.clone())
                }));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("{value:.2}"));
                // Name fills the space left of the readout, left-aligned.
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let label = ui.add(
                        egui::Label::new(&p.name)
                            .truncate()
                            .sense(egui::Sense::click()),
                    );
                    // The Id is what everything else addresses the param by,
                    // and two params may share a name — so the hover says it.
                    let label = label.on_hover_text(format!("{}\n{}", p.name, p.id));
                    label.context_menu(|ui| self.param_menu(ui, p));
                });
            });
        });
        let armed = self.armed.filter(|a| a.param == p.id);
        self.line_1d(ui, p, armed);
        ui.add_space(6.0);
    }

    fn param_menu(&mut self, ui: &mut egui::Ui, p: &ParamInfo) {
        let id = ui.id().with(("rename", p.id.as_str()));
        let mut name: String = ui
            .ctx()
            .data_mut(|d| d.get_temp(id))
            .unwrap_or_else(|| p.name.clone());
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut name);
            if ui.button("rename").clicked() {
                self.actions.push(ParamAction::Rename {
                    param: p.id.clone(),
                    name: name.clone(),
                });
                ui.close();
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(id, name));
        ui.label(egui::RichText::new(format!("id: {}", p.id)).weak().small());
        if ui
            .button("rename Id…")
            .on_hover_text("what addons, bindings and the file name this param by")
            .clicked()
        {
            self.actions.push(ParamAction::RenameId(p.id.clone()));
            ui.close();
        }
        ui.separator();

        let value = (self.pose)(&p.id);
        // Menu labels speak param values; the wire speaks normalized 0..1.
        if ui
            .button(format!("insert key position at {value:.3}"))
            .clicked()
        {
            self.actions.push(ParamAction::KeyInsert {
                param: p.id.clone(),
                value: norm(value, p.min, p.max),
            });
            ui.close();
        }
        let denorm = |t: f32| p.min + t * (p.max - p.min);
        if let Some(i) = nearest_interior(&p.key_positions, norm(value, p.min, p.max)) {
            if ui
                .button(format!(
                    "delete key position {:.3}",
                    denorm(p.key_positions[i])
                ))
                .clicked()
            {
                self.actions.push(ParamAction::KeyDelete {
                    param: p.id.clone(),
                    index: i as u32,
                });
                ui.close();
            }
        }
        ui.separator();
        if ui.button("flip (mirror keypoints)").clicked() {
            self.actions.push(ParamAction::Flip {
                param: p.id.clone(),
            });
            ui.close();
        }
        ui.separator();
        if ui.button("delete param").clicked() {
            self.actions.push(ParamAction::Delete(p.id.clone()));
            ui.close();
        }
    }

    /// Full-width track: key-position ticks and a draggable value handle.
    /// When armed, ticks become authored-state dots.
    fn line_1d(&mut self, ui: &mut egui::Ui, p: &ParamInfo, armed: Option<&ArmedInfo>) {
        let value = (self.pose)(&p.id);
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
        for (xi, &ax) in p.key_positions.iter().enumerate() {
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
        handle(&paint, at(norm(value, p.min, p.max)));
        if let Some(pos) = drag_pos(&resp) {
            let t = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
            let t = self.maybe_snap(t, &p.key_positions);
            self.actions.push(ParamAction::Pose {
                param: p.id.clone(),
                value: p.min + t * (p.max - p.min),
            });
        }
    }

    fn bindings_list(&mut self, ui: &mut egui::Ui, armed: &ArmedInfo) {
        for row in &armed.bindings {
            ui.horizontal(|ui| {
                let mark = if row.authored_at_cell { "●" } else { "○" };
                ui.label(format!("{mark} {} · {}", row.node_name, row.target));
                egui::ComboBox::from_id_salt(("interp", row.node.as_str(), &row.target))
                    .selected_text(&row.interpolate)
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for mode in ["nearest", "stepped", "linear", "cubic"] {
                            if ui.button(mode).clicked() {
                                self.actions.push(ParamAction::BindingInterpolate {
                                    node: row.node.clone(),
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
                        node: row.node.clone(),
                        target: row.target.clone(),
                    });
                }
                if ui
                    .small_button("unset")
                    .on_hover_text("un-author this keypoint (back to derived)")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingUnset {
                        node: row.node.clone(),
                        target: row.target.clone(),
                    });
                }
                if ui.small_button("copy").clicked() {
                    self.actions.push(ParamAction::CopyCell {
                        node: row.node.clone(),
                        target: row.target.clone(),
                    });
                }
                if self.can_paste && ui.small_button("paste").clicked() {
                    self.actions.push(ParamAction::PasteCell {
                        node: row.node.clone(),
                        target: row.target.clone(),
                    });
                }
                if ui
                    .small_button("±")
                    .on_hover_text("negate every authored value")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingInvert {
                        node: row.node.clone(),
                        target: row.target.clone(),
                    });
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("delete binding")
                    .clicked()
                {
                    self.actions.push(ParamAction::BindingDelete {
                        node: row.node.clone(),
                        target: row.target.clone(),
                    });
                }
            });
        }
        if armed.bindings.is_empty() {
            ui.label("(no bindings yet — edit while armed to record)");
        }
    }

    /// Snap a normalized 0..1 coordinate to the nearest key position.
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

/// Nearest interior key-position index to `v` (endpoints excluded — they
/// define the range and can't be deleted).
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
