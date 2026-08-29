//! Per-node inspector. Reads a cloned [`InspectorData`] snapshot (built by the
//! app from the document under the session lock) and emits [`InspectorAction`]s.
//!
//! Continuous controls follow the gesture discipline: while dragging they emit
//! `Preview` (applied to the puppet's working state only); the app commits one
//! `NodeSet` when the gesture ends. Discrete controls commit immediately.

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_editor_protocol::{NodePatch, NodeRef, ParamRef, TexRef};
use eframe::egui;

pub(crate) struct InspectorData {
    pub name: String,
    pub enabled: bool,
    pub lock_to_root: bool,
    pub z_order: f32,
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 2],
    pub kind: InspectorKind,
}

pub(crate) struct DrawableProps {
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub tint: [f32; 3],
    pub screen_tint: [f32; 3],
    pub mask_threshold: f32,
    pub masks: Vec<MaskRow>,
}

pub(crate) struct MaskRow {
    pub source_name: String,
    pub mode: MaskMode,
}

pub(crate) enum InspectorKind {
    Empty,
    Part {
        props: DrawableProps,
        albedo: Option<TexRef>,
        vert_count: usize,
        tri_count: usize,
    },
    Composite {
        props: DrawableProps,
        propagate_meshgroup: bool,
    },
    MeshGroup {
        dynamic: bool,
        translate_children: bool,
        vert_count: usize,
    },
    Physics {
        kind: PendulumKind,
        map_mode: PhysicsParamMapMode,
        local_only: bool,
        target_param: Option<ParamRef>,
        gravity: f32,
        length: f32,
        frequency: f32,
        angle_damping: f32,
        length_damping: f32,
        output_scale: [f32; 2],
    },
}

/// `(value-fields set, emitted-on)` — Preview while a drag is live, Commit once
/// it ends. The app merges Previews into puppet working state and turns the
/// final Commit into one NodeSet.
pub(crate) enum InspectorAction {
    Preview(NodePatch),
    Commit(NodePatch),
    PhysicsCommit(PhysicsPatch),
    MaskAdd {
        source: NodeRef,
        mode: MaskMode,
    },
    MaskSetMode {
        index: u32,
        mode: MaskMode,
    },
    MaskReorder {
        index: u32,
        to: u32,
    },
    MaskDelete {
        index: u32,
    },
    /// Enter mesh edit mode on the inspected node.
    EditMesh,
}

#[derive(Default)]
pub(crate) struct PhysicsPatch {
    pub kind: Option<String>,
    pub map_mode: Option<String>,
    pub local_only: Option<bool>,
    pub target_param: Option<ParamRef>,
    pub clear_target_param: bool,
    pub gravity: Option<f32>,
    pub length: Option<f32>,
    pub frequency: Option<f32>,
    pub angle_damping: Option<f32>,
    pub length_damping: Option<f32>,
    pub output_scale: Option<[f32; 2]>,
}

/// Candidate lists the inspector needs: parts (mask sources) and params
/// (physics targets).
pub(crate) struct InspectorContext<'a> {
    pub parts: &'a [(NodeRef, String)],
    pub params: &'a [(ParamRef, String)],
}

pub(crate) fn inspector_ui(
    ui: &mut egui::Ui,
    data: &InspectorData,
    ctx: &InspectorContext<'_>,
    textures: &[(TexRef, String)],
) -> Vec<InspectorAction> {
    let mut out = Vec::new();

    // Name commits on focus loss / enter, not per keystroke.
    let mut name = data.name.clone();
    let name_resp = ui.text_edit_singleline(&mut name);
    if name_resp.lost_focus() && name != data.name {
        out.push(InspectorAction::Commit(NodePatch {
            name: Some(name),
            ..Default::default()
        }));
    }

    let mut enabled = data.enabled;
    if ui.checkbox(&mut enabled, "enabled").changed() {
        out.push(InspectorAction::Commit(NodePatch {
            enabled: Some(enabled),
            ..Default::default()
        }));
    }
    let mut lock = data.lock_to_root;
    if ui
        .checkbox(&mut lock, "lock to root")
        .on_hover_text("transform relative to the puppet root, ignoring parents")
        .changed()
    {
        out.push(InspectorAction::Commit(NodePatch {
            lock_to_root: Some(lock),
            ..Default::default()
        }));
    }

    ui.separator();
    ui.label("Transform");
    let mut t = data.translation;
    triple_drag(ui, "translate", &mut t, 1.0, &mut out, |v| NodePatch {
        translate: Some(v),
        ..Default::default()
    });
    let mut r = data.rotation;
    triple_drag(ui, "rotate (rad)", &mut r, 0.01, &mut out, |v| NodePatch {
        rotate: Some(v),
        ..Default::default()
    });
    let mut sc = data.scale;
    double_drag(ui, "scale", &mut sc, 0.01, &mut out, |v| NodePatch {
        scale: Some(v),
        ..Default::default()
    });
    let mut z = data.z_order;
    single_drag(ui, "z order", &mut z, 0.01, &mut out, |v| NodePatch {
        z_order: Some(v),
        ..Default::default()
    });

    match &data.kind {
        InspectorKind::Empty => {}
        InspectorKind::Part {
            props,
            albedo,
            vert_count,
            tri_count,
        } => {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("Part — {vert_count} verts, {tri_count} tris"));
                if ui.button("✎ Edit mesh").clicked() {
                    out.push(InspectorAction::EditMesh);
                }
            });
            texture_combo(ui, *albedo, textures, &mut out);
            drawable_props_ui(ui, props, ctx, &mut out);
        }
        InspectorKind::Composite {
            props,
            propagate_meshgroup,
        } => {
            ui.separator();
            ui.label("Composite");
            let mut prop = *propagate_meshgroup;
            if ui
                .checkbox(&mut prop, "propagate mesh-group deform")
                .changed()
            {
                out.push(InspectorAction::Commit(NodePatch {
                    propagate_meshgroup: Some(prop),
                    ..Default::default()
                }));
            }
            drawable_props_ui(ui, props, ctx, &mut out);
        }
        InspectorKind::MeshGroup {
            dynamic,
            translate_children,
            vert_count,
        } => {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("MeshGroup — {vert_count} lattice verts"));
                if ui.button("✎ Edit mesh").clicked() {
                    out.push(InspectorAction::EditMesh);
                }
            });
            let mut dy = *dynamic;
            if ui
                .checkbox(&mut dy, "dynamic (re-deform every frame)")
                .changed()
            {
                out.push(InspectorAction::Commit(NodePatch {
                    mg_dynamic: Some(dy),
                    ..Default::default()
                }));
            }
            let mut tc = *translate_children;
            if ui.checkbox(&mut tc, "translate children").changed() {
                out.push(InspectorAction::Commit(NodePatch {
                    mg_translate_children: Some(tc),
                    ..Default::default()
                }));
            }
            // No colour here: a mesh group is never drawn.
        }
        InspectorKind::Physics {
            kind,
            map_mode,
            local_only,
            target_param,
            gravity,
            length,
            frequency,
            angle_damping,
            length_damping,
            output_scale,
        } => {
            ui.separator();
            ui.label("SimplePhysics");
            physics_ui(
                ui,
                *kind,
                *map_mode,
                *local_only,
                *target_param,
                *gravity,
                *length,
                *frequency,
                *angle_damping,
                *length_damping,
                *output_scale,
                ctx,
                &mut out,
            );
        }
    }
    out
}

fn drawable_props_ui(
    ui: &mut egui::Ui,
    props: &DrawableProps,
    ctx: &InspectorContext<'_>,
    out: &mut Vec<InspectorAction>,
) {
    let mut op = props.opacity;
    single_opacity(ui, &mut op, out);
    blend_combo(ui, props.blend_mode, out);
    tint_edit(ui, "tint", props.tint, false, out);
    tint_edit(ui, "screen tint", props.screen_tint, true, out);

    let mut th = props.mask_threshold;
    let resp = ui.add(
        egui::Slider::new(&mut th, 0.0..=1.0)
            .text("mask threshold")
            .clamping(egui::SliderClamping::Always),
    );
    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
        out.push(InspectorAction::Commit(NodePatch {
            mask_threshold: Some(th),
            ..Default::default()
        }));
    }

    ui.separator();
    ui.label("Masked by");
    let mut swap: Option<(u32, u32)> = None;
    for (i, m) in props.masks.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(&m.source_name);
            let mut mode = m.mode;
            egui::ComboBox::from_id_salt(("mask-mode", i))
                .selected_text(mask_mode_label(mode))
                .show_ui(ui, |ui| {
                    for candidate in [MaskMode::Mask, MaskMode::DodgeMask] {
                        if ui
                            .selectable_value(&mut mode, candidate, mask_mode_label(candidate))
                            .clicked()
                            && mode != m.mode
                        {
                            out.push(InspectorAction::MaskSetMode {
                                index: i as u32,
                                mode,
                            });
                        }
                    }
                });
            if ui.small_button("↑").clicked() && i > 0 {
                swap = Some((i as u32, i as u32 - 1));
            }
            if ui.small_button("↓").clicked() && i + 1 < props.masks.len() {
                swap = Some((i as u32, i as u32 + 1));
            }
            if ui.small_button("✕").clicked() {
                out.push(InspectorAction::MaskDelete { index: i as u32 });
            }
        });
    }
    if let Some((index, to)) = swap {
        out.push(InspectorAction::MaskReorder { index, to });
    }
    ui.menu_button("＋ add mask source", |ui| {
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| {
                for (part, name) in ctx.parts {
                    if ui.button(name).clicked() {
                        out.push(InspectorAction::MaskAdd {
                            source: *part,
                            mode: MaskMode::Mask,
                        });
                        ui.close();
                    }
                }
            });
    });
}

#[allow(clippy::too_many_arguments)]
fn physics_ui(
    ui: &mut egui::Ui,
    kind: PendulumKind,
    map_mode: PhysicsParamMapMode,
    local_only: bool,
    target_param: Option<ParamRef>,
    gravity: f32,
    length: f32,
    frequency: f32,
    angle_damping: f32,
    length_damping: f32,
    output_scale: [f32; 2],
    ctx: &InspectorContext<'_>,
    out: &mut Vec<InspectorAction>,
) {
    let kind_name = |m: PendulumKind| match m {
        PendulumKind::RigidPendulum => "Pendulum",
        PendulumKind::SpringPendulum => "SpringPendulum",
    };
    let mut m = kind;
    egui::ComboBox::from_label("kind")
        .selected_text(kind_name(m))
        .show_ui(ui, |ui| {
            for candidate in [PendulumKind::RigidPendulum, PendulumKind::SpringPendulum] {
                if ui
                    .selectable_value(&mut m, candidate, kind_name(candidate))
                    .clicked()
                    && m != kind
                {
                    out.push(InspectorAction::PhysicsCommit(PhysicsPatch {
                        kind: Some(kind_name(m).to_string()),
                        ..Default::default()
                    }));
                }
            }
        });

    let map_name = |m: PhysicsParamMapMode| match m {
        PhysicsParamMapMode::XY => "XY",
        PhysicsParamMapMode::YX => "YX",
        PhysicsParamMapMode::AngleLength => "AngleLength",
        PhysicsParamMapMode::LengthAngle => "LengthAngle",
    };
    let mut mm = map_mode;
    egui::ComboBox::from_label("map mode")
        .selected_text(map_name(mm))
        .show_ui(ui, |ui| {
            for candidate in [
                PhysicsParamMapMode::XY,
                PhysicsParamMapMode::YX,
                PhysicsParamMapMode::AngleLength,
                PhysicsParamMapMode::LengthAngle,
            ] {
                if ui
                    .selectable_value(&mut mm, candidate, map_name(candidate))
                    .clicked()
                    && mm != map_mode
                {
                    out.push(InspectorAction::PhysicsCommit(PhysicsPatch {
                        map_mode: Some(map_name(mm).to_string()),
                        ..Default::default()
                    }));
                }
            }
        });

    let mut lo = local_only;
    if ui.checkbox(&mut lo, "local only").changed() {
        out.push(InspectorAction::PhysicsCommit(PhysicsPatch {
            local_only: Some(lo),
            ..Default::default()
        }));
    }

    let target_label = target_param
        .and_then(|t| ctx.params.iter().find(|(p, _)| *p == t))
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| "(none)".into());
    egui::ComboBox::from_label("target param")
        .selected_text(target_label)
        .show_ui(ui, |ui| {
            if ui.button("(none)").clicked() {
                out.push(InspectorAction::PhysicsCommit(PhysicsPatch {
                    clear_target_param: true,
                    ..Default::default()
                }));
                ui.close();
            }
            for (p, name) in ctx.params {
                if ui.button(name).clicked() {
                    out.push(InspectorAction::PhysicsCommit(PhysicsPatch {
                        target_param: Some(*p),
                        ..Default::default()
                    }));
                    ui.close();
                }
            }
        });

    let mut phys_drag = |ui: &mut egui::Ui, label: &str, v: f32, write: fn(f32) -> PhysicsPatch| {
        let mut val = v;
        let resp = ui.add(
            egui::DragValue::new(&mut val)
                .speed(0.05)
                .prefix(format!("{label}: ")),
        );
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            out.push(InspectorAction::PhysicsCommit(write(val)));
        }
    };
    phys_drag(ui, "gravity", gravity, |v| PhysicsPatch {
        gravity: Some(v),
        ..Default::default()
    });
    phys_drag(ui, "length", length, |v| PhysicsPatch {
        length: Some(v),
        ..Default::default()
    });
    phys_drag(ui, "frequency", frequency, |v| PhysicsPatch {
        frequency: Some(v),
        ..Default::default()
    });
    phys_drag(ui, "angle damping", angle_damping, |v| PhysicsPatch {
        angle_damping: Some(v),
        ..Default::default()
    });
    phys_drag(ui, "length damping", length_damping, |v| PhysicsPatch {
        length_damping: Some(v),
        ..Default::default()
    });

    let mut os = output_scale;
    ui.horizontal(|ui| {
        ui.label("output scale");
        let rx = ui.add(egui::DragValue::new(&mut os[0]).speed(0.01));
        let ry = ui.add(egui::DragValue::new(&mut os[1]).speed(0.01));
        let done = |r: &egui::Response| r.drag_stopped() || (r.changed() && !r.dragged());
        if done(&rx) || done(&ry) {
            out.push(InspectorAction::PhysicsCommit(PhysicsPatch {
                output_scale: Some(os),
                ..Default::default()
            }));
        }
    });
}

fn texture_combo(
    ui: &mut egui::Ui,
    current: Option<TexRef>,
    textures: &[(TexRef, String)],
    out: &mut Vec<InspectorAction>,
) {
    let label = current
        .and_then(|c| textures.iter().find(|(t, _)| *t == c))
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| "(no texture)".into());
    egui::ComboBox::from_label("albedo")
        .selected_text(label)
        .show_ui(ui, |ui| {
            for (t, name) in textures {
                if ui.button(name).clicked() {
                    out.push(InspectorAction::Commit(NodePatch {
                        texture: Some(*t),
                        ..Default::default()
                    }));
                    ui.close();
                }
            }
        });
}

fn blend_combo(ui: &mut egui::Ui, current: BlendMode, out: &mut Vec<InspectorAction>) {
    const MODES: [BlendMode; 15] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::ColorDodge,
        BlendMode::LinearDodge,
        BlendMode::Screen,
        BlendMode::ClipToLower,
        BlendMode::SliceFromLower,
        BlendMode::Overlay,
        BlendMode::ColorBurn,
        BlendMode::LinearBurn,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Add,
        BlendMode::Inverse,
        BlendMode::Subtract,
    ];
    let mut mode = current;
    egui::ComboBox::from_label("blend mode")
        .selected_text(mode.as_str())
        .show_ui(ui, |ui| {
            for candidate in MODES {
                if ui
                    .selectable_value(&mut mode, candidate, candidate.as_str())
                    .clicked()
                    && mode != current
                {
                    out.push(InspectorAction::Commit(NodePatch {
                        blend_mode: Some(mode.as_str().to_string()),
                        ..Default::default()
                    }));
                }
            }
        });
}

fn mask_mode_label(m: MaskMode) -> &'static str {
    catchlight_editor_core::mask_mode_name(m)
}

fn single_opacity(ui: &mut egui::Ui, op: &mut f32, out: &mut Vec<InspectorAction>) {
    let resp = ui.add(
        egui::Slider::new(op, 0.0..=1.0)
            .text("opacity")
            .clamping(egui::SliderClamping::Always),
    );
    if resp.changed() {
        out.push(InspectorAction::Preview(NodePatch {
            opacity: Some(*op),
            ..Default::default()
        }));
    }
    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
        out.push(InspectorAction::Commit(NodePatch {
            opacity: Some(*op),
            ..Default::default()
        }));
    }
}

fn tint_edit(
    ui: &mut egui::Ui,
    label: &str,
    value: [f32; 3],
    is_screen: bool,
    out: &mut Vec<InspectorAction>,
) {
    let mut rgb = value;
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.color_edit_button_rgb(&mut rgb).changed() {
            // Color pickers emit per-frame; each change is small and discrete
            // enough to commit directly (the picker closes between gestures).
            out.push(InspectorAction::Commit(if is_screen {
                NodePatch {
                    screen_tint: Some(rgb),
                    ..Default::default()
                }
            } else {
                NodePatch {
                    tint: Some(rgb),
                    ..Default::default()
                }
            }));
        }
    });
}

fn single_drag(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    speed: f64,
    out: &mut Vec<InspectorAction>,
    patch: fn(f32) -> NodePatch,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let resp = ui.add(egui::DragValue::new(v).speed(speed));
        if resp.changed() {
            out.push(InspectorAction::Preview(patch(*v)));
        }
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            out.push(InspectorAction::Commit(patch(*v)));
        }
    });
}

fn double_drag(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut [f32; 2],
    speed: f64,
    out: &mut Vec<InspectorAction>,
    patch: fn([f32; 2]) -> NodePatch,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let r0 = ui.add(egui::DragValue::new(&mut v[0]).speed(speed));
        let r1 = ui.add(egui::DragValue::new(&mut v[1]).speed(speed));
        if r0.changed() || r1.changed() {
            out.push(InspectorAction::Preview(patch(*v)));
        }
        let done = |r: &egui::Response| r.drag_stopped() || (r.changed() && !r.dragged());
        if done(&r0) || done(&r1) {
            out.push(InspectorAction::Commit(patch(*v)));
        }
    });
}

fn triple_drag(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut [f32; 3],
    speed: f64,
    out: &mut Vec<InspectorAction>,
    patch: fn([f32; 3]) -> NodePatch,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let r0 = ui.add(egui::DragValue::new(&mut v[0]).speed(speed));
        let r1 = ui.add(egui::DragValue::new(&mut v[1]).speed(speed));
        let r2 = ui.add(egui::DragValue::new(&mut v[2]).speed(speed));
        if r0.changed() || r1.changed() || r2.changed() {
            out.push(InspectorAction::Preview(patch(*v)));
        }
        let done = |r: &egui::Response| r.drag_stopped() || (r.changed() && !r.dragged());
        if done(&r0) || done(&r1) || done(&r2) {
            out.push(InspectorAction::Commit(patch(*v)));
        }
    });
}
