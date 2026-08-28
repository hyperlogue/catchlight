//! Transform gizmo: screen-space handles over the viewport that drive a node's
//! local transform. World-aligned axes; the drag delta is mapped into the
//! node's parent space at drag start, so it lands in the same frame the
//! document stores. Preview while dragging, one commit on release; ctrl snaps.

use eframe::egui;
use glam::{Mat4, Vec2};

use crate::camera::EditorCamera;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GizmoMode {
    Translate,
    Rotate,
    Scale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    Free,
    AxisX,
    AxisY,
    Ring,
    Uniform,
}

pub(crate) struct GizmoDrag {
    /// The mode active when the drag began — a mid-drag hotkey must not
    /// reinterpret the gesture.
    mode: GizmoMode,
    handle: Handle,
    start_world: Vec2,
    origin_world: Vec2,
    parent_inv: Mat4,
    start_translation: [f32; 3],
    start_rotation: [f32; 3],
    start_scale: [f32; 2],
}

/// Current values of the node under the gizmo, world data resolved by the app.
pub(crate) struct GizmoTarget {
    pub origin_world: Vec2,
    pub parent_world: Mat4,
    pub translation: [f32; 3],
    pub rotation: [f32; 3],
    pub scale: [f32; 2],
}

pub(crate) enum GizmoEvent {
    Preview {
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 3]>,
        scale: Option<[f32; 2]>,
    },
    Commit {
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 3]>,
        scale: Option<[f32; 2]>,
    },
}

const ARM: f32 = 56.0;
const GRAB: f32 = 10.0;

/// (translation, rotation, scale) — only the component the handle drives is set.
type SolvedTransform = (Option<[f32; 3]>, Option<[f32; 3]>, Option<[f32; 2]>);

pub(crate) struct Gizmo {
    pub mode: GizmoMode,
    drag: Option<GizmoDrag>,
}

impl Default for Gizmo {
    fn default() -> Self {
        Self {
            mode: GizmoMode::Translate,
            drag: None,
        }
    }
}

impl Gizmo {
    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Returns true when the pointer at `pos` would grab a handle — the app
    /// uses this to give the gizmo priority over click-select.
    pub(crate) fn hit_test(
        &self,
        rect: egui::Rect,
        camera: &EditorCamera,
        target: &GizmoTarget,
        pos: egui::Pos2,
    ) -> bool {
        self.classify(rect, camera, target, pos).is_some()
    }

    fn classify(
        &self,
        rect: egui::Rect,
        camera: &EditorCamera,
        target: &GizmoTarget,
        pos: egui::Pos2,
    ) -> Option<Handle> {
        let c = camera.world_to_screen(rect, target.origin_world);
        let d = pos - c;
        let dist = d.length();
        match self.mode {
            GizmoMode::Translate => {
                if dist < GRAB {
                    Some(Handle::Free)
                } else if (pos - (c + egui::vec2(ARM, 0.0))).length() < GRAB {
                    Some(Handle::AxisX)
                } else if (pos - (c - egui::vec2(0.0, ARM))).length() < GRAB {
                    Some(Handle::AxisY)
                } else {
                    None
                }
            }
            GizmoMode::Rotate => ((dist - ARM).abs() < GRAB).then_some(Handle::Ring),
            GizmoMode::Scale => {
                if (pos - (c + egui::vec2(ARM, 0.0))).length() < GRAB {
                    Some(Handle::AxisX)
                } else if (pos - (c - egui::vec2(0.0, ARM))).length() < GRAB {
                    Some(Handle::AxisY)
                } else if (pos - (c + egui::vec2(ARM, -ARM) * 0.7)).length() < GRAB {
                    Some(Handle::Uniform)
                } else {
                    None
                }
            }
        }
    }

    pub(crate) fn draw(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        camera: &EditorCamera,
        target: &GizmoTarget,
    ) {
        let c = camera.world_to_screen(rect, target.origin_world);
        let stroke_x = egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(230, 80, 80));
        let stroke_y = egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(80, 200, 90));
        let stroke_ring = egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(90, 150, 240));
        match self.mode {
            GizmoMode::Translate => {
                painter.circle_stroke(c, GRAB * 0.7, stroke_ring);
                painter.arrow(c, egui::vec2(ARM, 0.0), stroke_x);
                painter.arrow(c, egui::vec2(0.0, -ARM), stroke_y);
            }
            GizmoMode::Rotate => {
                painter.circle_stroke(c, ARM, stroke_ring);
                painter.circle_filled(c, 2.5, stroke_ring.color);
            }
            GizmoMode::Scale => {
                painter.line_segment([c, c + egui::vec2(ARM, 0.0)], stroke_x);
                painter.rect_filled(
                    egui::Rect::from_center_size(c + egui::vec2(ARM, 0.0), egui::vec2(8.0, 8.0)),
                    0.0,
                    stroke_x.color,
                );
                painter.line_segment([c, c - egui::vec2(0.0, ARM)], stroke_y);
                painter.rect_filled(
                    egui::Rect::from_center_size(c - egui::vec2(0.0, ARM), egui::vec2(8.0, 8.0)),
                    0.0,
                    stroke_y.color,
                );
                painter.rect_filled(
                    egui::Rect::from_center_size(
                        c + egui::vec2(ARM, -ARM) * 0.7,
                        egui::vec2(8.0, 8.0),
                    ),
                    0.0,
                    stroke_ring.color,
                );
            }
        }
    }

    /// Feed pointer state; returns preview/commit events while a drag is live.
    pub(crate) fn update(
        &mut self,
        rect: egui::Rect,
        camera: &EditorCamera,
        target: &GizmoTarget,
        resp: &egui::Response,
        snap: bool,
    ) -> Option<GizmoEvent> {
        if resp.drag_started_by(egui::PointerButton::Primary) && self.drag.is_none() {
            // Classify at the press origin: egui's drag threshold means the
            // pointer has already traveled by this frame, and a fast flick
            // can leave the handle before the drag officially starts.
            let pos = resp
                .ctx
                .input(|i| i.pointer.press_origin())
                .or_else(|| resp.interact_pointer_pos())?;
            let handle = self.classify(rect, camera, target, pos)?;
            let parent_inv = catchlight_core::checked_affine_inverse(target.parent_world)?;
            self.drag = Some(GizmoDrag {
                mode: self.mode,
                handle,
                start_world: camera.screen_to_world(rect, pos),
                origin_world: target.origin_world,
                parent_inv,
                start_translation: target.translation,
                start_rotation: target.rotation,
                start_scale: target.scale,
            });
            return None;
        }

        let drag = self.drag.as_ref()?;
        let pos = resp
            .interact_pointer_pos()
            .or_else(|| resp.ctx.pointer_latest_pos())?;
        let world = camera.screen_to_world(rect, pos);
        let (translation, rotation, scale) = drag.solve(drag.mode, world, snap);

        if resp.drag_stopped_by(egui::PointerButton::Primary)
            || (!resp.dragged() && !resp.drag_started())
        {
            self.drag = None;
            return Some(GizmoEvent::Commit {
                translation,
                rotation,
                scale,
            });
        }
        Some(GizmoEvent::Preview {
            translation,
            rotation,
            scale,
        })
    }
}

impl GizmoDrag {
    fn solve(&self, mode: GizmoMode, world: Vec2, snap: bool) -> SolvedTransform {
        match mode {
            GizmoMode::Translate => {
                let mut delta = world - self.start_world;
                match self.handle {
                    Handle::AxisX => delta.y = 0.0,
                    Handle::AxisY => delta.x = 0.0,
                    _ => {}
                }
                // World delta -> parent-local delta (translation is stored in
                // parent space).
                let a = self.parent_inv.transform_point3(glam::vec3(0.0, 0.0, 0.0));
                let b = self
                    .parent_inv
                    .transform_point3(glam::vec3(delta.x, delta.y, 0.0));
                let mut local = (b - a).truncate();
                if snap {
                    local = (local / 10.0).round() * 10.0;
                }
                let t = [
                    self.start_translation[0] + local.x,
                    self.start_translation[1] + local.y,
                    self.start_translation[2],
                ];
                (Some(t), None, None)
            }
            GizmoMode::Rotate => {
                // Angles and ratios live in the parent frame — under a
                // mirrored or rotated parent, world-space math flips or
                // skews the result.
                let to_parent = |w: Vec2| -> Vec2 {
                    self.parent_inv
                        .transform_point3(glam::vec3(w.x, w.y, 0.0))
                        .truncate()
                };
                let o = to_parent(self.origin_world);
                let a0 = to_parent(self.start_world) - o;
                let a1 = to_parent(world) - o;
                if a0.length_squared() < 1e-6 || a1.length_squared() < 1e-6 {
                    return (None, None, None);
                }
                let mut delta = a1.y.atan2(a1.x) - a0.y.atan2(a0.x);
                if snap {
                    let step = 15f32.to_radians();
                    delta = (delta / step).round() * step;
                }
                let r = [
                    self.start_rotation[0],
                    self.start_rotation[1],
                    self.start_rotation[2] + delta,
                ];
                (None, Some(r), None)
            }
            GizmoMode::Scale => {
                let to_parent = |w: Vec2| -> Vec2 {
                    self.parent_inv
                        .transform_point3(glam::vec3(w.x, w.y, 0.0))
                        .truncate()
                };
                let o = to_parent(self.origin_world);
                let a0 = to_parent(self.start_world) - o;
                let a1 = to_parent(world) - o;
                let ratio_of = |v0: f32, v1: f32| if v0.abs() < 1e-3 { 1.0 } else { v1 / v0 };
                let (mut rx, mut ry) = match self.handle {
                    Handle::AxisX => (ratio_of(a0.x, a1.x), 1.0),
                    Handle::AxisY => (1.0, ratio_of(a0.y, a1.y)),
                    _ => {
                        let r = ratio_of(a0.length(), a1.length());
                        (r, r)
                    }
                };
                if snap {
                    rx = (rx / 0.1).round() * 0.1;
                    ry = (ry / 0.1).round() * 0.1;
                }
                let s = [self.start_scale[0] * rx, self.start_scale[1] * ry];
                (None, None, Some(s))
            }
        }
    }
}
