#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Stress example: 50 puppets in a Bevy 2D scene.
//!
//! Usage: cargo run -p catchlight-bevy --example bevy_puppets --release
//!
//! Performance target: 50 puppets at 60fps. Frame-time diagnostics print
//! smoothed FPS / frame-time to stdout every 5 seconds. A final averaged
//! FPS line is logged on `AppExit`.

use std::path::Path;
use std::time::Duration;

use bevy::app::AppExit;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use catchlight_bevy::{CameraControlsPlugin, CatchlightCamera, CatchlightPlugin, CatchlightPuppet};
use catchlight_core::{
    load_model, Animation, AnimationLane, InterpolateMode, Keyframe, ModelFormat,
};

const GRID_COLS: usize = 10;
const GRID_ROWS: usize = 5;
const N: usize = GRID_COLS * GRID_ROWS;
const SPACING_X: f32 = 180.0;
const SPACING_Y: f32 = 260.0;
const PUPPET_SCALE: f32 = 0.022;

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
        .add_systems(Update, log_fps_on_exit)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((Camera2d, CatchlightCamera, Msaa::Off));

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example_models/reference/reference.clm".to_string());
    let bytes = std::fs::read(&path).expect("read model");
    let format = ModelFormat::from_path(Path::new(&path)).expect("recognized model extension");
    let base = load_model(&bytes, format, 0).expect("load model");

    let blink_anim = build_blink_animation(&base);

    for i in 0..N {
        let mut puppet = base.clone();

        if let Some(anim) = &blink_anim {
            puppet.set_animations(vec![anim.clone()]);
            puppet.play_animation("Blink");
            // Stagger each puppet into a distinct frame of the 1.7s blink
            // cycle so they don't all blink in lockstep. This exercises
            // the per-entity DynamicState path on frame 0.
            let phase = (i as f32) * 0.0531;
            puppet.tick_animations(phase);
        }

        let col = i % GRID_COLS;
        let row = i / GRID_COLS;
        let x = (col as f32 - (GRID_COLS - 1) as f32 / 2.0) * SPACING_X;
        let y = ((GRID_ROWS - 1) as f32 / 2.0 - row as f32) * SPACING_Y;

        commands.spawn((
            CatchlightPuppet::new(puppet),
            Transform::from_xyz(x, y, 0.0).with_scale(Vec3::splat(PUPPET_SCALE)),
            GlobalTransform::default(),
            Visibility::default(),
        ));
    }

    info!("{} puppets spawned", N);
}

fn build_blink_animation(puppet: &catchlight_core::LegacyPuppet) -> Option<Animation> {
    let blink_uuids: Vec<u32> = puppet
        .params()
        .iter()
        .filter(|p| p.name.contains("Blink"))
        .map(|p| p.id)
        .collect();
    if blink_uuids.is_empty() {
        return None;
    }
    let kfs = || -> Vec<Keyframe> {
        vec![
            Keyframe {
                frame: 0,
                value: 0.0,
            },
            Keyframe {
                frame: 60,
                value: 0.0,
            },
            Keyframe {
                frame: 72,
                value: 1.0,
            },
            Keyframe {
                frame: 90,
                value: 1.0,
            },
            Keyframe {
                frame: 102,
                value: 0.0,
            },
        ]
    };
    let lanes = blink_uuids
        .iter()
        .map(|&uuid| AnimationLane {
            param_id: uuid,
            axis: catchlight_core::ParamAxis::X,
            keyframes: kfs(),
            interpolation: InterpolateMode::Linear,
        })
        .collect();
    Some(Animation {
        name: "Blink".into(),
        timestep: 1.0 / 60.0,
        length: 102,
        lanes,
        ..Default::default()
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
