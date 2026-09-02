#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The render half of the split, with a real render app and no window.
//!
//! Bevy's renderer is brought up against whatever adapter is available (on CI
//! that is mesa's lavapipe, the same CPU Vulkan driver the wgpu suites use) and
//! the puppets draw into an offscreen `Image` target. What is asserted is what
//! the split moved: puppets of one model share the one render cache prepared
//! from the shared asset, each holding its own deform set, and each is
//! collected into with *its* puppet's frame.

use std::path::PathBuf;

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::sync_world::RenderEntity;
use bevy::render::texture::GpuImage;
use bevy::render::RenderPlugin;
use bevy::window::{ExitCondition, WindowPlugin};
use catchlight_bevy::{
    CatchlightCamera, CatchlightModel, CatchlightPlugin, CatchlightPuppet, CatchlightRenderState,
};
use catchlight_core::Model;
use wgpu::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

const SIZE: u32 = 256;

fn fixture(stem: &str) -> Model {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/models")
        .join(format!("{stem}.clm"));
    Model::from_clm_bytes(&std::fs::read(path).expect("read fixture")).expect("parse fixture")
}

/// A render app with no window: the plugins `CatchlightPlugin` needs, plus an
/// offscreen camera target. Returns `None` when no adapter could be created,
/// which is how a machine with no GPU at all reports itself.
fn render_app() -> App {
    let mut app = App::new();
    // The order is DefaultPlugins' own: the renderer comes up before the
    // asset plugins that register render assets against it.
    app.add_plugins((
        MinimalPlugins,
        TransformPlugin,
        WindowPlugin {
            primary_window: None,
            exit_condition: ExitCondition::DontExit,
            close_when_requested: false,
            ..default()
        },
        AssetPlugin::default(),
        RenderPlugin::default(),
        ImagePlugin::default(),
        bevy::mesh::MeshPlugin,
        bevy::camera::CameraPlugin,
        bevy::core_pipeline::CorePipelinePlugin,
        CatchlightPlugin,
    ));
    app.finish();
    app.cleanup();
    app
}

fn offscreen_target(app: &mut App) -> Handle<Image> {
    let mut image = Image::new_fill(
        Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage |=
        TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    app.world_mut().resource_mut::<Assets<Image>>().add(image)
}

/// Read the camera's target image back off the GPU.
///
/// bevy has no copy-back for a render target, so the copy is done here, after
/// the frame's graph has already submitted. `SIZE * 4` is a multiple of 256,
/// so no row padding is needed.
fn readback(app: &App, target: &Handle<Image>) -> Vec<u8> {
    let world = app.sub_app(bevy::render::RenderApp).world();
    let images = world.resource::<RenderAssets<GpuImage>>();
    let image = images
        .get(target.id())
        .expect("the target image is resident on the GPU");
    let device = world.resource::<RenderDevice>().wgpu_device();
    let queue = world.resource::<RenderQueue>();

    let bytes_per_row = SIZE * 4;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("catchlight test readback"),
        size: u64::from(bytes_per_row * SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &image.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |result| {
        result.expect("map the readback buffer");
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("poll the readback");
    let pixels = slice.get_mapped_range().to_vec();
    buffer.unmap();
    pixels
}

/// How many pixels are not fully transparent.
fn drawn_pixels(pixels: &[u8]) -> usize {
    pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] != 0)
        .count()
}

/// Drawn pixels left and right of the target's centre column.
fn drawn_per_half(pixels: &[u8]) -> (usize, usize) {
    let mut left = 0;
    let mut right = 0;
    for (i, p) in pixels.as_chunks::<4>().0.iter().enumerate() {
        if p[3] == 0 {
            continue;
        }
        if (i as u32) % SIZE < SIZE / 2 {
            left += 1;
        } else {
            right += 1;
        }
    }
    (left, right)
}

fn render_entity(app: &App, entity: Entity) -> Entity {
    app.world()
        .entity(entity)
        .get::<RenderEntity>()
        .expect("the puppet is synced to the render world")
        .id()
}

#[test]
fn two_puppets_of_one_model_share_one_cache_and_both_reach_the_target() {
    let mut app = render_app();
    let target = offscreen_target(&mut app);
    app.world_mut().spawn((
        Camera2d,
        Camera {
            // Transparent, so a drawn pixel is an opaque one.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        CatchlightCamera,
    ));

    let model = app
        .world_mut()
        .resource_mut::<Assets<CatchlightModel>>()
        .add(CatchlightModel::new(fixture("welded_seam")));

    // Far enough apart that each lands in its own half of the target, so
    // "did both draw" is answerable per pixel column.
    let left = app
        .world_mut()
        .spawn((
            CatchlightPuppet::new(model.clone()),
            Transform::from_xyz(-60.0, 0.0, 0.0).with_scale(Vec3::splat(0.25)),
        ))
        .id();
    let right = app
        .world_mut()
        .spawn((
            CatchlightPuppet::new(model.clone()),
            Transform::from_xyz(60.0, 0.0, 1.0).with_scale(Vec3::splat(0.25)),
        ))
        .id();

    // Frame 1 bakes and extracts, frame 2 prepares nothing new but collects,
    // frame 3 draws. Run a few more so a one-frame lag anywhere shows up as a
    // pass rather than as a flake.
    for _ in 0..5 {
        app.update();
    }

    let state = app
        .sub_app(bevy::render::RenderApp)
        .world()
        .resource::<CatchlightRenderState>();
    assert_eq!(
        state.resident_caches(),
        1,
        "two puppets of one model hold one cache between them",
    );
    assert_eq!(
        state.resident_puppets(),
        2,
        "and a deform set plus a render list each",
    );

    let left_list = state
        .collected_drawables(render_entity(&app, left))
        .expect("the left puppet collected a render list");
    let right_list = state
        .collected_drawables(render_entity(&app, right))
        .expect("the right puppet collected a render list");
    assert!(
        !left_list.root_drawables.is_empty(),
        "the fixture draws something",
    );
    assert_eq!(
        left_list.root_drawables.len(),
        right_list.root_drawables.len(),
        "two puppets of one model draw the same set of parts",
    );
    assert_ne!(
        left_list.deform_set, right_list.deform_set,
        "each puppet uploads its frame into its own slice of the atlas",
    );

    let stats = state
        .frame_stats(render_entity(&app, left))
        .expect("the puppets have a renderer");
    assert!(
        stats.instance_slots_written >= 2,
        "one frame carried both puppets' draws: {stats:?}",
    );
    assert_eq!(
        stats.instance_buffer_writes, 1,
        "and staged them under one cursor, flushed once: {stats:?}",
    );

    let x_of = |list: &catchlight_wgpu::RenderList| match &list.root_drawables[0] {
        catchlight_wgpu::DrawableInfo::Part { transform, .. } => transform.to_cols_array()[12],
        other => panic!("expected a part, got {other:?}"),
    };
    assert!(
        (x_of(&right_list) - x_of(&left_list) - 120.0).abs() < 1e-3,
        "each puppet's own root transform reached its own list: {} vs {}",
        x_of(&left_list),
        x_of(&right_list),
    );

    // Pixels, not counts: two puppets whose deform sets collided still report
    // one cache and two lists, and still draw — at one pose.
    let pixels = readback(&app, &target);
    let (left_half, right_half) = drawn_per_half(&pixels);
    assert!(
        left_half > 200 && right_half > 200,
        "both puppets must reach the target: {left_half} left, {right_half} right of {} pixels",
        SIZE * SIZE,
    );
}

#[test]
fn a_despawned_puppet_releases_its_cache() {
    let mut app = render_app();
    let target = offscreen_target(&mut app);
    app.world_mut().spawn((
        Camera2d,
        RenderTarget::Image(target.into()),
        CatchlightCamera,
    ));
    let model = app
        .world_mut()
        .resource_mut::<Assets<CatchlightModel>>()
        .add(CatchlightModel::new(fixture("welded_seam")));
    let entity = app
        .world_mut()
        .spawn((CatchlightPuppet::new(model), Transform::default()))
        .id();

    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        app.sub_app(bevy::render::RenderApp)
            .world()
            .resource::<CatchlightRenderState>()
            .resident_caches(),
        1,
    );

    app.world_mut().entity_mut(entity).despawn();
    for _ in 0..3 {
        app.update();
    }
    let state = app
        .sub_app(bevy::render::RenderApp)
        .world()
        .resource::<CatchlightRenderState>();
    assert_eq!(state.resident_puppets(), 0, "the deform set goes first");
    assert_eq!(
        state.resident_caches(),
        0,
        "and the cache with the last puppet that drew through it",
    );
}

#[test]
fn the_puppet_actually_lands_on_the_camera_target() {
    let mut app = render_app();
    let target = offscreen_target(&mut app);
    app.world_mut().spawn((
        Camera2d,
        Camera {
            // Transparent, so "was anything drawn" is just "is any pixel
            // opaque" — the default clear colour is opaque and would answer
            // yes for an empty frame.
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        CatchlightCamera,
    ));
    let model = app
        .world_mut()
        .resource_mut::<Assets<CatchlightModel>>()
        .add(CatchlightModel::new(fixture("welded_seam")));
    app.world_mut().spawn((
        CatchlightPuppet::new(model),
        Transform::from_scale(Vec3::splat(0.4)),
    ));

    for _ in 0..4 {
        app.update();
    }

    let drawn = drawn_pixels(&readback(&app, &target));
    assert!(
        drawn > 1000,
        "the puppet drew {drawn} of {} pixels; a blank frame here is the \
         classic bevy integration failure (wrong view format, MSAA on the \
         view target, a render list whose slots name another cache)",
        SIZE * SIZE,
    );
}

#[test]
fn swapping_the_model_re_prepares_the_same_cache() {
    let mut app = render_app();
    let target = offscreen_target(&mut app);
    app.world_mut().spawn((
        Camera2d,
        Camera {
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        CatchlightCamera,
    ));
    let (first, second) = {
        let mut models = app.world_mut().resource_mut::<Assets<CatchlightModel>>();
        (
            models.add(CatchlightModel::new(fixture("welded_seam"))),
            models.add(CatchlightModel::new(fixture("composite_masks"))),
        )
    };
    let entity = app
        .world_mut()
        .spawn((
            CatchlightPuppet::new(first),
            Transform::from_scale(Vec3::splat(0.4)),
        ))
        .id();

    for _ in 0..4 {
        app.update();
    }
    let before = readback(&app, &target);

    app.world_mut()
        .entity_mut(entity)
        .get_mut::<CatchlightPuppet>()
        .unwrap()
        .set_model(second);
    for _ in 0..4 {
        app.update();
    }
    let after = readback(&app, &target);

    assert_eq!(
        app.sub_app(bevy::render::RenderApp)
            .world()
            .resource::<CatchlightRenderState>()
            .resident_caches(),
        1,
        "the swap re-prepares the entity's cache rather than adding one",
    );
    assert!(drawn_pixels(&after) > 1000, "the new model draws");
    assert_ne!(before, after, "and it is a different model on the target");
}

#[test]
fn replacing_the_asset_value_re_prepares_the_cache() {
    let mut app = render_app();
    let target = offscreen_target(&mut app);
    app.world_mut().spawn((
        Camera2d,
        Camera {
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        CatchlightCamera,
    ));
    let handle = app
        .world_mut()
        .resource_mut::<Assets<CatchlightModel>>()
        .add(CatchlightModel::new(fixture("welded_seam")));
    app.world_mut().spawn((
        CatchlightPuppet::new(handle.clone()),
        Transform::from_scale(Vec3::splat(0.4)),
    ));
    for _ in 0..4 {
        app.update();
    }
    let before = readback(&app, &target);

    // Same handle, a different model: the id is unchanged and the new model's
    // generation starts back at zero, so only the model's own identity tells
    // the cache it is stale.
    app.world_mut()
        .resource_mut::<Assets<CatchlightModel>>()
        .insert(&handle, CatchlightModel::new(fixture("composite_masks")))
        .expect("replace the model asset");
    for _ in 0..4 {
        app.update();
    }
    let after = readback(&app, &target);

    assert!(drawn_pixels(&after) > 1000, "the replacement draws");
    assert_ne!(before, after, "and it is what reaches the target");
}
