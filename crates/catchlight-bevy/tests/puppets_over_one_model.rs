#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The main-world half of the split: one model asset, many puppets.
//!
//! These run without a render app — `CatchlightPlugin` leaves the render world
//! alone when there is none — so they pin the ownership rules (a model is
//! shared, a pose is not, a swap rebases) with no GPU in the picture. The
//! render half is `renders_headless.rs`.

use std::path::PathBuf;

use bevy::prelude::*;
use catchlight_bevy::{CatchlightModel, CatchlightPlugin, CatchlightPuppet};
use catchlight_core::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
use catchlight_core::{InterpolateMode, Model, ParamId, Pose, Puppet};

fn model_path(stem: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/models")
        .join(format!("{stem}.clm"))
}

fn fixture(stem: &str) -> Model {
    let bytes = std::fs::read(model_path(stem)).expect("read fixture");
    Model::from_clm_bytes(&bytes).expect("parse fixture")
}

/// A minimal app: no windows, no renderer, just the schedules
/// `update_puppets` needs.
fn headless_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin::default(),
        TransformPlugin,
        CatchlightPlugin,
    ));
    app
}

fn add_model(app: &mut App, model: Model) -> Handle<CatchlightModel> {
    app.world_mut()
        .resource_mut::<Assets<CatchlightModel>>()
        .add(CatchlightModel::new(model))
}

fn spawn(app: &mut App, model: &Handle<CatchlightModel>) -> Entity {
    app.world_mut()
        .spawn((CatchlightPuppet::new(model.clone()), Transform::default()))
        .id()
}

fn puppet(app: &App, entity: Entity) -> &Puppet {
    app.world()
        .entity(entity)
        .get::<CatchlightPuppet>()
        .expect("the entity still animates a model")
        .puppet()
        .expect("the model has been baked")
}

fn pose_param(app: &mut App, entity: Entity, param: &ParamId, value: f32) {
    app.world_mut()
        .entity_mut(entity)
        .get_mut::<CatchlightPuppet>()
        .expect("the entity still animates a model")
        .puppet_mut()
        .expect("the model has been baked")
        .set_param_value(param, value);
}

/// The first param any deform binding names — the one whose value visibly
/// moves a mesh.
fn deform_param(model: &Model) -> ParamId {
    model
        .param_ids()
        .iter()
        .find(|id| {
            model
                .bindings_of_param(id)
                .any(|b| b.target() == catchlight_core::BindingTarget::Deform)
        })
        .expect("the fixture has a deform param")
        .clone()
}

/// Every node's combined deform, flattened — what a render cache uploads, and
/// the honest answer to "are these two puppets posed the same".
fn deforms(puppet: &Puppet) -> Vec<(u32, Vec<[f32; 2]>)> {
    puppet
        .iter_deform_nodes()
        .filter_map(|(idx, _)| {
            let deform = puppet.combined_deform(idx)?;
            Some((idx.0, deform.iter().map(|v| [v.x, v.y]).collect()))
        })
        .collect()
}

#[test]
fn two_puppets_animate_one_model_at_two_poses() {
    let model = fixture("welded_seam");
    let param = deform_param(&model);
    let (min, max) = {
        let p = model.param(&param).unwrap();
        (p.min, p.max)
    };

    let mut app = headless_app();
    let handle = add_model(&mut app, model);
    let a = spawn(&mut app, &handle);
    let b = spawn(&mut app, &handle);

    // Frame one bakes both puppets against the one asset.
    app.update();
    assert_eq!(
        app.world().resource::<Assets<CatchlightModel>>().len(),
        1,
        "two entities must not have loaded two models",
    );
    assert_eq!(
        app.world()
            .entity(a)
            .get::<CatchlightPuppet>()
            .unwrap()
            .model()
            .id(),
        app.world()
            .entity(b)
            .get::<CatchlightPuppet>()
            .unwrap()
            .model()
            .id(),
        "both entities animate the same asset",
    );

    pose_param(&mut app, a, &param, min);
    pose_param(&mut app, b, &param, max);
    app.update();

    assert_eq!(puppet(&app, a).param_value(&param), Some(min));
    assert_eq!(puppet(&app, b).param_value(&param), Some(max));
    assert_ne!(
        deforms(puppet(&app, a)),
        deforms(puppet(&app, b)),
        "two poses of one model must evaluate to two different frames",
    );

    // Posing a puppet never reaches the model it animates: the asset is
    // still at the generation it loaded with, so nothing rebakes and nothing
    // else that shares it moves.
    let generation = app
        .world()
        .resource::<Assets<CatchlightModel>>()
        .get(&handle)
        .unwrap()
        .model()
        .generation();
    assert_eq!(puppet(&app, a).baked_generation(), generation);
    assert_eq!(puppet(&app, b).baked_generation(), generation);
}

#[test]
fn swapping_the_model_rebases_the_puppet_and_keeps_the_pose() {
    let model = fixture("welded_seam");
    let param = deform_param(&model);
    let posed = model.param(&param).unwrap().max;

    // The same rig, moved sideways: a different model with the same param
    // Ids, so the pose has somewhere to land and the rebase is observable.
    let mut moved = fixture("welded_seam");
    let root = moved
        .root()
        .expect("the fixture is a complete model")
        .clone();
    let root_child = moved
        .node(&root)
        .and_then(|root| root.children().first().cloned())
        .expect("the fixture's root has a child");
    moved
        .update_node(&root_child, |node| {
            node.transform.translation[0] += 1000.0;
        })
        .expect("move the child");

    let mut app = headless_app();
    let first = add_model(&mut app, model);
    let second = add_model(&mut app, moved);
    let entity = spawn(&mut app, &first);

    app.update();
    pose_param(&mut app, entity, &param, posed);
    app.update();
    let before = puppet(&app, entity).transforms().get(
        puppet(&app, entity)
            .node_idx(&root_child)
            .expect("the node is in the arena"),
    );

    app.world_mut()
        .entity_mut(entity)
        .get_mut::<CatchlightPuppet>()
        .unwrap()
        .set_model(second.clone());
    app.update();

    let after_puppet = puppet(&app, entity);
    assert_eq!(
        after_puppet.param_value(&param),
        Some(posed),
        "the pose survives the swap, by ParamId",
    );
    let after = after_puppet
        .transforms()
        .get(after_puppet.node_idx(&root_child).unwrap());
    assert_ne!(
        before.to_cols_array(),
        after.to_cols_array(),
        "the puppet is evaluating the new model, not the old arena",
    );
    assert_eq!(
        (after.to_cols_array()[12] - before.to_cols_array()[12]).round(),
        1000.0,
        "and it moved by exactly what the new model changed",
    );
}

#[test]
fn a_pose_set_before_the_model_loads_lands_on_the_bake() {
    let model = fixture("welded_seam");
    let param = deform_param(&model);
    let posed = model.param(&param).unwrap().max;

    let mut app = headless_app();
    // Spawn against a handle whose asset does not exist yet — what an
    // `AssetServer::load` looks like for the first frames.
    let handle: Handle<CatchlightModel> = Handle::default();
    let entity = app
        .world_mut()
        .spawn((
            CatchlightPuppet::new(handle).with_pose(Pose::from_iter([(param.clone(), posed)])),
            Transform::default(),
        ))
        .id();

    app.update();
    assert!(
        app.world()
            .entity(entity)
            .get::<CatchlightPuppet>()
            .unwrap()
            .puppet()
            .is_none(),
        "nothing is baked while the model is missing",
    );

    let loaded = add_model(&mut app, model);
    app.world_mut()
        .entity_mut(entity)
        .get_mut::<CatchlightPuppet>()
        .unwrap()
        .set_model(loaded);
    app.update();

    assert_eq!(puppet(&app, entity).param_value(&param), Some(posed));
}

#[test]
fn a_models_animation_is_baked_onto_every_puppet_that_shares_it() {
    // What `bevy_puppets` does with 50 entities: one model carrying one clip,
    // and each puppet playing it from its own phase.
    let mut model = fixture("welded_seam");
    let param = deform_param(&model);
    model
        .set_animations(vec![ClmAnimation {
            name: "Pull".into(),
            timestep: 1.0 / 60.0,
            length: 60,
            lanes: vec![ClmLane {
                param: param.clone(),
                interpolation: InterpolateMode::Linear,
                keyframes: vec![
                    ClmKeyframe {
                        frame: 0,
                        value: 0.0,
                    },
                    ClmKeyframe {
                        frame: 60,
                        value: 1.0,
                    },
                ],
            }],
            ..ClmAnimation::default()
        }])
        .expect("install the clip");

    let mut app = headless_app();
    let handle = add_model(&mut app, model);
    let entities: Vec<Entity> = (0..3).map(|_| spawn(&mut app, &handle)).collect();
    app.update();

    let mut values = Vec::new();
    for (i, entity) in entities.iter().enumerate() {
        let mut entity_mut = app.world_mut().entity_mut(*entity);
        let mut component = entity_mut.get_mut::<CatchlightPuppet>().unwrap();
        let puppet = component.puppet_mut().unwrap();
        assert_eq!(
            puppet.animations().len(),
            1,
            "the model's own clip is on the puppet, converted at bake",
        );
        assert!(puppet.play_animation("Pull"));
        puppet.tick_animations(i as f32 * 0.25);
        values.push(puppet.param_value(&param));
    }
    app.update();

    assert!(
        values[0] < values[1] && values[1] < values[2],
        "each puppet plays the shared clip from its own phase: {values:?}",
    );
}
