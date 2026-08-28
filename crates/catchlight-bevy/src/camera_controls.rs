use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::input::touch::Touches;
use bevy::prelude::*;

use crate::components::CatchlightCamera;

/// Plugin: scroll-wheel zoom + left-drag pan + single-touch pan + pinch zoom
/// for any `CatchlightCamera` entity with a `Projection` and `Transform`.
pub struct CameraControlsPlugin;

impl Plugin for CameraControlsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraControls>()
            .init_resource::<PinchState>()
            .add_systems(Update, (zoom_system, pan_system, touch_system));
    }
}

#[derive(Resource)]
pub struct CameraControls {
    /// Multiplicative zoom factor applied per scroll line (or per pixel
    /// scaled down). A line of scroll multiplies scale by
    /// `zoom_per_line^-delta.y`.
    pub zoom_per_line: f32,
    pub min_scale: f32,
    pub max_scale: f32,
    /// Mouse button that triggers pan when held.
    pub pan_button: MouseButton,
}

impl Default for CameraControls {
    fn default() -> Self {
        Self {
            zoom_per_line: 1.15,
            min_scale: 0.02,
            max_scale: 50.0,
            pan_button: MouseButton::Left,
        }
    }
}

#[derive(Resource, Default)]
struct PinchState {
    /// Previous distance between two active touches, if any.
    prev_distance: Option<f32>,
}

fn zoom_system(
    scroll: Res<AccumulatedMouseScroll>,
    controls: Res<CameraControls>,
    mut q: Query<&mut Projection, With<CatchlightCamera>>,
) {
    if scroll.delta.y == 0.0 {
        return;
    }
    // MouseScrollUnit::Line → delta is whole lines. Pixel unit (trackpad)
    // is raw pixels; divide by ~20 so a single scroll notch feels like
    // one "line".
    let lines = match scroll.unit {
        MouseScrollUnit::Line => scroll.delta.y,
        MouseScrollUnit::Pixel => scroll.delta.y / 20.0,
    };
    let factor = controls.zoom_per_line.powf(-lines);
    apply_zoom(&mut q, factor, controls.min_scale, controls.max_scale);
}

fn pan_system(
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    controls: Res<CameraControls>,
    mut q: Query<(&mut Transform, &Projection), With<CatchlightCamera>>,
) {
    if !buttons.pressed(controls.pan_button) {
        return;
    }
    if motion.delta == Vec2::ZERO {
        return;
    }
    for (mut tf, projection) in q.iter_mut() {
        let scale = ortho_scale(projection);
        // Screen Y is down, world Y is up → invert Y so content follows
        // the cursor.
        tf.translation.x -= motion.delta.x * scale;
        tf.translation.y += motion.delta.y * scale;
    }
}

fn touch_system(
    touches: Res<Touches>,
    controls: Res<CameraControls>,
    mut pinch: ResMut<PinchState>,
    mut q: Query<(&mut Transform, &mut Projection), With<CatchlightCamera>>,
) {
    let active: Vec<_> = touches.iter().collect();
    match active.len() {
        0 => {
            pinch.prev_distance = None;
        }
        1 => {
            pinch.prev_distance = None;
            let delta = active[0].delta();
            if delta == Vec2::ZERO {
                return;
            }
            for (mut tf, projection) in q.iter_mut() {
                let scale = ortho_scale(&projection);
                tf.translation.x -= delta.x * scale;
                tf.translation.y += delta.y * scale;
            }
        }
        _ => {
            let a = active[0].position();
            let b = active[1].position();
            let d = (a - b).length().max(1.0);
            if let Some(prev) = pinch.prev_distance {
                let factor = prev / d;
                for (_, mut projection) in q.iter_mut() {
                    if let Projection::Orthographic(ref mut ortho) = *projection {
                        ortho.scale =
                            (ortho.scale * factor).clamp(controls.min_scale, controls.max_scale);
                    }
                }
            }
            pinch.prev_distance = Some(d);
        }
    }
}

fn apply_zoom(
    q: &mut Query<&mut Projection, With<CatchlightCamera>>,
    factor: f32,
    min_scale: f32,
    max_scale: f32,
) {
    for mut projection in q.iter_mut() {
        if let Projection::Orthographic(ref mut ortho) = *projection {
            ortho.scale = (ortho.scale * factor).clamp(min_scale, max_scale);
        }
    }
}

fn ortho_scale(projection: &Projection) -> f32 {
    match projection {
        Projection::Orthographic(o) => o.scale,
        _ => 1.0,
    }
}
