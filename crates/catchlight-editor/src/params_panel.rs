//! Parameters panel: a full-width track per param with a value handle, the
//! arm toggle, and the armed param's recording view with keypoint dots and
//! the bindings list.
//! Pure UI over snapshot data — emits [`ParamAction`]s the app applies.
//!
//! Invariants this module carries:
//!
//! - **A param is a scalar, so a controller is a track.** The two-axis pad is
//!   a view over a *binding* that names two params ([`Armed::Two`]), not a
//!   property of either: any two params can be posed together on it, and what
//!   it records is one binding whose grid is the product of their key
//!   positions. Nothing here can make a param two-dimensional.
//!
//! - **A row addresses its own binding.** [`BindingRow`] carries the params
//!   and the cell of the binding it lists, because arming one param of a pair
//!   lists rows whose grid has a second axis the armed param does not.

use catchlight_editor_protocol::{
    BindingParams, BindingTarget, Interpolate, NodeId, ParamId, ParamInfo,
};
use eframe::egui;

/// What recording writes through: one param, or two params authored jointly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Armed {
    One(ParamId),
    /// Two params on one pad; x runs along the grid's first axis.
    Two(ParamId, ParamId),
}

impl Armed {
    pub(crate) fn x(&self) -> &ParamId {
        match self {
            Self::One(p) | Self::Two(p, _) => p,
        }
    }

    pub(crate) fn y(&self) -> Option<&ParamId> {
        match self {
            Self::One(_) => None,
            Self::Two(_, p) => Some(p),
        }
    }

    pub(crate) fn contains(&self, param: &ParamId) -> bool {
        self.x() == param || self.y() == Some(param)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ParamId> {
        std::iter::once(self.x()).chain(self.y())
    }
}

/// One binding of the armed param(s), and where the current pose sits in
/// *that binding's* grid.
pub(crate) struct BindingAddr {
    pub params: BindingParams,
    pub node: NodeId,
    pub target: BindingTarget,
    pub cell: [u32; 2],
}

pub(crate) enum BindingOp {
    /// Un-author the keypoint (back to derived).
    Unset,
    /// Author the identity value at the keypoint.
    Reset,
    Delete,
    Invert,
    Interpolate(Interpolate),
    Copy,
    Paste,
}

pub(crate) enum ParamAction {
    Pose {
        param: ParamId,
        value: f32,
    },
    Arm(Option<Armed>),
    AddParam {
        name: String,
    },
    /// Two scalar params, `<name>.x` and `<name>.y`, armed together on the
    /// pad — what authoring "left *and* up" as one shape takes now that a
    /// param has one axis.
    AddParamPair {
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
    Binding {
        row: BindingAddr,
        op: BindingOp,
    },
}

/// Authored-state of each cell of the armed grid: 0 = none of the bindings
/// author it, 1 = some, 2 = all.
pub(crate) struct ArmedInfo {
    pub armed: Armed,
    /// The cell the current pose lands on, in the armed grid.
    pub cell: [u32; 2],
    /// The armed grid: the x param's key positions by the y param's (or 1).
    pub grid: (usize, usize),
    pub cell_states: Vec<u8>,
    pub bindings: Vec<BindingRow>,
}

pub(crate) struct BindingRow {
    pub node: NodeId,
    pub node_name: String,
    pub target: BindingTarget,
    pub interpolate: Interpolate,
    pub authored_at_cell: bool,
    /// The binding's own params and cell — not the armed param's, which may
    /// be one half of them.
    pub params: BindingParams,
    pub cell: [u32; 2],
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
                    return;
                }
                if ui
                    .button("add two (x / y)")
                    .on_hover_text(
                        "two scalar params on one pad — a pad is a view over a \
                         binding that names both, not a two-axis param",
                    )
                    .clicked()
                {
                    let name: String = ui.ctx().data_mut(|d| d.get_temp(id)).unwrap_or_default();
                    self.actions.push(ParamAction::AddParamPair { name });
                    ui.close();
                }
            });
            self.pad_menu(ui);
            ui.checkbox(self.snap, "snap");
        });

        for p in self.params {
            self.param_row(ui, p);
        }
    }

    /// Arm any two params on the pad. The pair belongs to the binding it
    /// records, so it is chosen here rather than carried by a param.
    fn pad_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("⊹ pad", |ui| {
            ui.label("pose and record two params together");
            let id = ui.id().with("pad-pair");
            let mut pair: (String, String) =
                ui.ctx().data_mut(|d| d.get_temp(id)).unwrap_or_else(|| {
                    match self.params.first().zip(self.params.get(1)) {
                        Some((a, b)) => (a.id.to_string(), b.id.to_string()),
                        None => (String::new(), String::new()),
                    }
                });
            let label = |id: &str| {
                self.params
                    .iter()
                    .find(|p| p.id.as_str() == id)
                    .map_or("(none)".to_string(), |p| p.name.clone())
            };
            for (axis, slot) in [("x", &mut pair.0), ("y", &mut pair.1)] {
                egui::ComboBox::from_id_salt(("pad-axis", axis))
                    .selected_text(format!("{axis}: {}", label(slot)))
                    .show_ui(ui, |ui| {
                        for p in self.params {
                            if ui.button(&p.name).on_hover_text(p.id.as_str()).clicked() {
                                *slot = p.id.to_string();
                                ui.close();
                            }
                        }
                    });
            }
            let chosen = ParamId::new(&pair.0).ok().zip(ParamId::new(&pair.1).ok());
            let chosen = chosen.filter(|(a, b)| a != b);
            ui.ctx().data_mut(|d| d.insert_temp(id, pair));
            if ui
                .add_enabled(chosen.is_some(), egui::Button::new("open pad"))
                .clicked()
            {
                if let Some((x, y)) = chosen {
                    self.actions.push(ParamAction::Arm(Some(Armed::Two(x, y))));
                }
                ui.close();
            }
        });
    }

    /// The recording-mode panel: the armed controller (a track, or the pad
    /// when two params are armed) and the bindings list.
    pub(crate) fn show_recording(&mut self, ui: &mut egui::Ui) {
        let Some(armed) = self.armed else { return };
        let Some(px) = self.params.iter().find(|p| p.id == *armed.armed.x()) else {
            return;
        };
        match armed
            .armed
            .y()
            .and_then(|y| self.params.iter().find(|p| p.id == *y))
        {
            Some(py) => {
                ui.label(format!(
                    "{} × {} @ ({}, {})",
                    px.name, py.name, armed.cell[0], armed.cell[1]
                ));
                self.pad_2d(ui, px, py, armed);
            }
            None => {
                ui.label(format!(
                    "{} @ ({}, {})",
                    px.name, armed.cell[0], armed.cell[1]
                ));
                self.line_1d(ui, px, Some(armed));
            }
        }
        ui.separator();
        ui.label("Bindings");
        // Binding rows are wider than the panel; scroll them sideways without
        // dragging the controller into the scroll region.
        egui::ScrollArea::horizontal()
            .id_salt("bindings-scroll")
            .show(ui, |ui| self.bindings_list(ui, armed));
    }

    fn param_row(&mut self, ui: &mut egui::Ui, p: &ParamInfo) {
        let armed_here = self.armed.is_some_and(|a| a.armed.contains(&p.id));
        let value = (self.pose)(&p.id);
        ui.horizontal(|ui| {
            let arm = ui
                .selectable_label(armed_here, "⏺")
                .on_hover_text("arm for recording");
            if arm.clicked() {
                self.actions.push(ParamAction::Arm(if armed_here {
                    None
                } else {
                    Some(Armed::One(p.id.clone()))
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
        // A one-param track under the row, showing this param's own keys; the
        // dots come from the armed grid only when this is the armed param.
        let armed = self
            .armed
            .filter(|a| matches!(&a.armed, Armed::One(id) if *id == p.id));
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
        // `t` is normalized 0..1 — the space key positions live in.
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

    /// The pad: two params posed together, over the grid their key positions
    /// make. Dragging it poses both, so a gesture records into one cell of one
    /// two-param binding.
    fn pad_2d(&mut self, ui: &mut egui::Ui, px: &ParamInfo, py: &ParamInfo, armed: &ArmedInfo) {
        let side = ui.available_width().clamp(120.0, 320.0);
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click_and_drag());
        let paint = ui.painter_at(rect);
        let vis = ui.visuals();
        let field = rect.shrink(HANDLE_R + 2.0);
        paint.rect_stroke(
            field,
            2.0,
            egui::Stroke::new(1.0_f32, vis.widgets.inactive.bg_fill),
            egui::StrokeKind::Inside,
        );
        // Screen y grows downward; a param's y grows upward, as the pad reads.
        let at = |tx: f32, ty: f32| {
            egui::pos2(
                field.left() + tx.clamp(0.0, 1.0) * field.width(),
                field.bottom() - ty.clamp(0.0, 1.0) * field.height(),
            )
        };
        let (w, _) = armed.grid;
        for (yi, &ay) in py.key_positions.iter().enumerate() {
            for (xi, &ax) in px.key_positions.iter().enumerate() {
                dot(
                    &paint,
                    at(ax, ay),
                    armed.cell_states.get(yi * w + xi).copied().unwrap_or(0),
                    armed.cell == [xi as u32, yi as u32],
                    vis,
                );
            }
        }
        let vx = (self.pose)(&px.id);
        let vy = (self.pose)(&py.id);
        handle(
            &paint,
            at(norm(vx, px.min, px.max), norm(vy, py.min, py.max)),
        );
        if let Some(pos) = drag_pos(&resp) {
            let tx = ((pos.x - field.left()) / field.width()).clamp(0.0, 1.0);
            let ty = ((field.bottom() - pos.y) / field.height()).clamp(0.0, 1.0);
            let tx = self.maybe_snap(tx, &px.key_positions);
            let ty = self.maybe_snap(ty, &py.key_positions);
            self.actions.push(ParamAction::Pose {
                param: px.id.clone(),
                value: px.min + tx * (px.max - px.min),
            });
            self.actions.push(ParamAction::Pose {
                param: py.id.clone(),
                value: py.min + ty * (py.max - py.min),
            });
        }
    }

    fn bindings_list(&mut self, ui: &mut egui::Ui, armed: &ArmedInfo) {
        for row in &armed.bindings {
            let addr = || BindingAddr {
                params: row.params.clone(),
                node: row.node.clone(),
                target: row.target,
                cell: row.cell,
            };
            ui.horizontal(|ui| {
                let mark = if row.authored_at_cell { "●" } else { "○" };
                let pair = row
                    .params
                    .param_y
                    .as_ref()
                    .map(|y| format!(" · {} × {}", row.params.param, y))
                    .unwrap_or_default();
                ui.label(format!("{mark} {} · {}", row.node_name, row.target))
                    .on_hover_text(format!("{}{pair}", row.node));
                egui::ComboBox::from_id_salt(("interp", row.node.as_str(), row.target.wire_name()))
                    .selected_text(row.interpolate.wire_name())
                    .width(70.0)
                    .show_ui(ui, |ui| {
                        for mode in Interpolate::ALL {
                            if ui.button(mode.wire_name()).clicked() {
                                self.actions.push(ParamAction::Binding {
                                    row: addr(),
                                    op: BindingOp::Interpolate(mode),
                                });
                                ui.close();
                            }
                        }
                    });
                let mut op = None;
                if ui
                    .small_button("reset")
                    .on_hover_text("author the identity value at this keypoint")
                    .clicked()
                {
                    op = Some(BindingOp::Reset);
                }
                if ui
                    .small_button("unset")
                    .on_hover_text("un-author this keypoint (back to derived)")
                    .clicked()
                {
                    op = Some(BindingOp::Unset);
                }
                if ui.small_button("copy").clicked() {
                    op = Some(BindingOp::Copy);
                }
                if self.can_paste && ui.small_button("paste").clicked() {
                    op = Some(BindingOp::Paste);
                }
                if ui
                    .small_button("±")
                    .on_hover_text("negate every authored value")
                    .clicked()
                {
                    op = Some(BindingOp::Invert);
                }
                if ui
                    .small_button("✕")
                    .on_hover_text("delete binding")
                    .clicked()
                {
                    op = Some(BindingOp::Delete);
                }
                if let Some(op) = op {
                    self.actions.push(ParamAction::Binding { row: addr(), op });
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
