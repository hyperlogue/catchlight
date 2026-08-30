#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Stress example: 50 puppets of **one** model in a Bevy 2D scene.
//!
//! Usage: `cargo run -p catchlight-bevy --example bevy_puppets --release [-- <model.clm>]`
//!
//! The 50 entities share a single `CatchlightModel` asset — one load, one
//! decode, one copy of every texture — and hold a `Puppet` each, posed
//! differently and blinking out of phase. That is the split the example is
//! here to show, on top of the performance target: 50 puppets at 60fps.
//! Frame-time diagnostics print smoothed FPS / frame-time to stdout every 5
//! seconds, and a final averaged FPS line is logged on `AppExit`.

use std::path::Path;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use catchlight_bevy::{
    model_from_bytes, CameraControlsPlugin, CatchlightCamera, CatchlightModel, CatchlightPlugin,
    CatchlightPuppet,
};
use catchlight_core::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use catchlight_core::{BindingTarget, InterpolateMode, Model, ModelFormat, ParamId};

const GRID_COLS: usize = 10;
const GRID_ROWS: usize = 5;
const N: usize = GRID_COLS * GRID_ROWS;
const SPACING_X: f32 = 180.0;
const SPACING_Y: f32 = 260.0;
const PUPPET_SCALE: f32 = 0.022;
const BLINK: &str = "Blink";

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(RenderPlugin::default()))
        .add_plugins(CatchlightPlugin)
        .add_plugins(CameraControlsPlugin)
        .add_plugins(FrameTimeDiagnosticsPlugin::default())
        .add_plugins(LogDiagnosticsPlugin {
            wait_duration: Duration::from_secs(5),
            ..Default::default()
        })
        .add_systems(Startup, setup)
        .add_systems(Update, (start_puppets, log_fps_on_exit))
        .run();
}

/// What this entity does once its puppet is baked: play the blink out of
/// phase, and hold one param at its own value so no two puppets are posed
/// alike.
#[derive(Component)]
struct StartWhenBaked {
    phase: f32,
    pose: Option<(ParamId, f32)>,
}

fn setup(mut commands: Commands, mut models: ResMut<Assets<CatchlightModel>>) {
    commands.spawn((Camera2d, CatchlightCamera, Msaa::Off));

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example_models/reference/reference.clm".to_string());
    let bytes = std::fs::read(&path).expect("read model");
    let format = ModelFormat::from_path(Path::new(&path)).expect("recognized model extension");
    let mut model = model_from_bytes(&bytes, format).expect("load model");

    // A model carries its own animations now, and every puppet baked from it
    // gets them. Synthesize a blink if the file has none of its own.
    if model.animations().is_empty() {
        if let Some(blink) = build_blink_animation(&model) {
            model.set_animations(vec![blink]).expect("install blink");
        }
    }
    // Resolved against the model, because a range is the model's; the
    // puppets below are posed by `ParamId` alone.
    let posed_param = first_deform_param(&model)
        .and_then(|id| model.param(&id).map(|p| (id.clone(), p.min, p.max)));

    // One asset. Every puppet below animates *this* model; nothing about
    // posing one reaches the model or any other puppet.
    let model = models.add(CatchlightModel::new(model));

    for i in 0..N {
        let col = i % GRID_COLS;
        let row = i / GRID_COLS;
        let x = (col as f32 - (GRID_COLS - 1) as f32 / 2.0) * SPACING_X;
        let y = ((GRID_ROWS - 1) as f32 / 2.0 - row as f32) * SPACING_Y;

        commands.spawn((
            CatchlightPuppet::new(model.clone()),
            StartWhenBaked {
                // Stagger each puppet into a distinct frame of the blink
                // cycle so they don't all blink in lockstep.
                phase: (i as f32) * 0.0531,
                pose: posed_param.as_ref().map(|(param, min, max)| {
                    let frac = i as f32 / (N - 1) as f32;
                    (param.clone(), min + (max - min) * frac)
                }),
            },
            Transform::from_xyz(x, y, 0.0).with_scale(Vec3::splat(PUPPET_SCALE)),
            Visibility::default(),
        ));
    }

    info!("{} puppets spawned, sharing one model asset", N);
}

/// Pose and start each puppet on the first frame its model is baked. The
/// component is removed afterwards, so this runs once per entity.
fn start_puppets(
    mut commands: Commands,
    mut query: Query<(Entity, &mut CatchlightPuppet, &StartWhenBaked)>,
) {
    for (entity, mut catchlight, start) in &mut query {
        let Some(puppet) = catchlight.puppet_mut() else {
            continue;
        };
        if let Some((param, value)) = &start.pose {
            // Posing is by `ParamId`; a name is only what a human reads.
            puppet.set_param_value(param, *value);
        }
        if puppet.play_animation(BLINK) {
            puppet.tick_animations(start.phase);
        }
        commands.entity(entity).remove::<StartWhenBaked>();
    }
}

/// The first param (in the model's own order) any deform binding names.
fn first_deform_param(model: &Model) -> Option<ParamId> {
    model
        .param_ids()
        .iter()
        .find(|id| {
            model
                .bindings_of_param(id)
                .any(|binding| binding.target() == BindingTarget::Deform)
        })
        .cloned()
}

/// A blink over every param whose *name* contains "Blink" (the reference rig:
/// "Left Eye - Blink" and "Right Eye - Blink"), resolved to `ParamId`s against
/// the model. value=0 -> open, value=1 -> closed.
fn build_blink_animation(model: &Model) -> Option<ClmAnimation> {
    let blink_params: Vec<ParamId> = model
        .param_ids()
        .iter()
        .filter(|id| {
            model
                .param(id)
                .is_some_and(|p| p.name.as_str().contains(BLINK))
        })
        .cloned()
        .collect();
    if blink_params.is_empty() {
        return None;
    }
    let keyframes = || -> Vec<ClmKeyframe> {
        vec![
            ClmKeyframe {
                frame: 0,
                value: 0.0,
            },
            ClmKeyframe {
                frame: 60,
                value: 0.0,
            },
            ClmKeyframe {
                frame: 72,
                value: 1.0,
            },
            ClmKeyframe {
                frame: 90,
                value: 1.0,
            },
            ClmKeyframe {
                frame: 102,
                value: 0.0,
            },
        ]
    };
    Some(ClmAnimation {
        name: BLINK.to_string(),
        timestep: 1.0 / 60.0,
        length: 102,
        lanes: blink_params
            .into_iter()
            .map(|param| ClmLane {
                param,
                interpolation: InterpolateMode::Linear,
                keyframes: keyframes(),
            })
            .collect(),
        ..ClmAnimation::default()
    })
}

fn log_fps_on_exit(mut exit_events: MessageReader<AppExit>, diagnostics: Res<DiagnosticsStore>) {
    if exit_events.read().next().is_none() {
        return;
    }
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.average())
        .unwrap_or(0.0);
    let frame_time = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(|d| d.average())
        .unwrap_or(0.0);
    info!(
        "exit: averaged fps={:.2}, frame_time={:.2}ms over {} puppets",
        fps, frame_time, N
    );
}
