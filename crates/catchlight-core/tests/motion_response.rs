#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Cross the model-file, live editing and puppet interfaces with the same
//! two-spring fixture. These invariants do not depend on a particular artwork.
use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelNode, ModelNodeKind, ModelParam, MotionResponse, Name,
    NodeId, ParamId, Pose, Puppet, ResponseInput, ResponseSource, ScalarTarget,
};

fn pid(s: &str) -> ParamId {
    ParamId::new(s).unwrap()
}
fn nid(s: &str) -> NodeId {
    NodeId::new(s).unwrap()
}
fn model(damping: f32) -> Model {
    let mut m = Model::new();
    let root = m.root().unwrap().clone();
    for p in ["roll", "broad", "bend"] {
        m.add_param_with_id(pid(p), ModelParam::new(Name::truncated(p), -1., 1., 0.))
            .unwrap();
    }
    for (name, parent, source, output) in [
        (
            "upper",
            root,
            ResponseSource::PoseParam(pid("roll")),
            "broad",
        ),
        ("tip", nid("upper"), ResponseSource::Parent, "bend"),
    ] {
        m.add_node_with_id(
            nid(name),
            &parent,
            ModelNode::new(
                name,
                ModelNodeKind::MotionResponse(MotionResponse {
                    inputs: vec![ResponseInput { source, gain: 1. }],
                    period_s: 0.4,
                    damping_ratio: damping,
                    output_param: Some(pid(output)),
                }),
            ),
        )
        .unwrap();
    }
    m
}

#[test]
fn child_reads_the_same_interval_unclipped_and_all_damping_regimes_settle() {
    for damping in [0.65, 1., 1.8] {
        let mut m = model(damping);
        m.set_param_range(&pid("broad"), -0.01, 0.01).unwrap();
        let mut p = Puppet::new(&m);
        p.settle_physics(&m);
        p.set_param_value(&pid("roll"), 1.);
        p.tick(&m, 1. / 60.);
        let upper = p.response_trace(&nid("upper")).unwrap();
        let tip = p.response_trace(&nid("tip")).unwrap();
        assert_eq!(upper.emitted, -0.01);
        assert!(upper.followed > 0. && upper.lag < -0.01);
        assert_eq!(tip.target, upper.followed);
        assert!(tip.followed > 0. && tip.followed < tip.target);
        let before = p.clone();
        let root = m.root().unwrap().clone();
        m.update_node(&root, |n| n.name = Name::truncated("new label"))
            .unwrap();
        p.sync(&m);
        assert_eq!(
            p.response_trace(&nid("tip")).unwrap().followed,
            before.response_trace(&nid("tip")).unwrap().followed
        );
        for _ in 0..400 {
            p.tick(&m, 1. / 60.);
        }
        assert_eq!(p.param_value(&pid("bend")), Some(0.));
        assert!(!p.tick(&m, 1. / 60.).physics);
        p.set_physics_enabled(false);
        p.set_param_value(&pid("bend"), 0.5);
        p.tick(&m, 0.);
        assert_eq!(p.param_value(&pid("bend")), Some(0.5));
        p.set_physics_enabled(true);
        p.tick(&m, 1. / 60.);
        assert_eq!(p.param_value(&pid("bend")), Some(0.));
    }
}

#[test]
fn writer_and_tree_errors_leave_the_model_unchanged() {
    let mut m = model(0.7);
    let before = m.to_clm_bytes().unwrap();
    let generation = m.generation();
    let root = m.root().unwrap().clone();
    assert!(m.reparent(&nid("tip"), &root).is_err());
    let ModelNodeKind::MotionResponse(mut r) = m.node(&nid("tip")).unwrap().kind.clone() else {
        panic!()
    };
    r.output_param = Some(pid("broad"));
    assert!(m.set_motion_response(&nid("tip"), r.clone()).is_err());
    r.inputs[0].source = ResponseSource::PoseParam(pid("broad"));
    r.output_param = None;
    assert!(m.set_motion_response(&nid("tip"), r).is_err());
    assert_eq!(m.generation(), generation);
    assert_eq!(m.to_clm_bytes().unwrap(), before);
    assert_eq!(
        Model::from_clm_bytes(&before)
            .unwrap()
            .to_clm_bytes()
            .unwrap(),
        before
    );
}

#[test]
fn one_positive_corrective_key_leaves_the_entire_zero_lag_slice_at_identity() {
    let mut m = model(0.7);
    let key = BindingKey::pair(
        pid("roll"),
        pid("bend"),
        m.root().unwrap().clone(),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    m.add_binding(&key).unwrap();
    m.set_binding_key(&key, [1, 2], 14.).unwrap();
    m.key_insert(&key, &pid("roll"), 0.3).unwrap();
    assert!(m.set_binding_key(&key, [0, 1], 4.).is_err());
    assert!(m.key_delete(&key, &pid("bend"), 1).is_err());
    m.unset_binding_key(&key, [1, 1]).unwrap();
    let mut pose = Pose::default();
    for value in [-1., -0.7, 0., 0.4, 1.] {
        pose.set(pid("roll"), value);
        assert_eq!(m.eval_scalar(&key, &pose), Some(0.));
    }
    pose.set(pid("roll"), 1.);
    pose.set(pid("bend"), 1.);
    assert_eq!(m.eval_scalar(&key, &pose), Some(14.));
    assert!(m.to_clm_bytes().is_ok());
}

#[test]
fn fixed_clock_uses_boundaries_and_interpolates_across_display_rates() {
    let m = model(0.75);
    let run = |rate: u32| {
        let mut p = Puppet::new(&m);
        p.set_response_clock(catchlight_core::ResponseClock::Fixed60);
        p.settle_physics(&m);
        let mut trace = Vec::new();
        for i in 1..=rate {
            p.set_param_value(&pid("roll"), (i as f32 / rate as f32 * 2.).min(1.));
            p.tick(&m, 1. / rate as f32);
            if i % (rate / 30) == 0 {
                trace.push(p.param_value(&pid("bend")).unwrap());
            }
        }
        trace
    };
    for (a, b) in run(30).iter().zip(run(120)) {
        assert!((a - b).abs() < 2e-6);
    }
}

#[test]
fn low_rate_fixed_clock_tracks_a_continuous_ramp() {
    let m = model(0.75);
    for fps in [1, 2, 4, 10, 12, 13, 14] {
        let mut display = Puppet::new(&m);
        display.set_response_clock(catchlight_core::ResponseClock::Fixed60);
        display.settle_physics(&m);
        let mut reference = display.clone();
        for frame in 1..=fps {
            display.set_param_value(&pid("roll"), frame as f32 / fps as f32);
            display.tick(&m, 1. / fps as f32);
            for sub in 1..=60 {
                reference.set_param_value(
                    &pid("roll"),
                    ((frame - 1) * 60 + sub) as f32 / (60 * fps) as f32,
                );
                reference.tick(&m, 1. / (60 * fps) as f32);
            }
            let output = |p: &Puppet| p.param_value(&pid("bend")).unwrap();
            assert!(
                (output(&display) - output(&reference)).abs() < 2e-5,
                "{fps} fps, frame {frame}"
            );
        }
    }
}

#[test]
fn loop_and_restart_initialize_at_the_new_clip_sample() {
    use catchlight_core::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
    let m = model(0.75);
    let mut p = Puppet::new(&m);
    let dt = 1. / 60.;
    p.set_animations(vec![ClmAnimation {
        name: "turn".into(),
        timestep: dt,
        length: 6,
        lead_in: 1,
        lead_out: 4,
        lanes: vec![ClmLane {
            param: pid("roll"),
            interpolation: catchlight_core::InterpolateMode::Linear,
            keyframes: vec![
                ClmKeyframe {
                    frame: 0,
                    value: -1.,
                },
                ClmKeyframe {
                    frame: 4,
                    value: 1.,
                },
            ],
        }],
    }]);
    assert!(p.play_animation("turn"));
    p.tick(&m, 0.);
    for _ in 0..3 {
        p.tick(&m, dt);
    }
    assert!(p.response_trace(&nid("tip")).unwrap().lag.abs() > 0.001);
    p.tick(&m, dt);
    assert_eq!(p.animation_sample().unwrap().frame, 1.);
    for name in ["upper", "tip"] {
        assert_eq!(p.response_trace(&nid(name)).unwrap().lag, 0.);
    }
    p.tick(&m, dt);
    assert!(p.play_animation("turn"));
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("tip")).unwrap().lag, 0.);
    p.tick(&m, dt);
    p.tick_animations(-dt);
    p.tick(&m, 0.);
    assert_eq!(p.response_trace(&nid("upper")).unwrap().lag, 0.);
}

#[test]
fn arbitrary_projection_reads_this_poses_slot_and_rebinding_resets_the_branch() {
    use catchlight_core::{
        formats::clm::{ClmIndices, ClmMesh},
        ModelPart, SlotId,
    };
    let mut m = model(0.7);
    let root = m.root().unwrap().clone();
    let mesh = ClmMesh {
        verts: vec![0., 0., 30., 40., 0., 20.],
        uvs: vec![],
        indices: ClmIndices::U16(vec![0, 1, 2]),
        origin: [0., 0.],
    };
    let lattice = ClmMesh {
        verts: vec![-100., -100., 200., -100., -100., 200.],
        ..mesh.clone()
    };
    m.add_node_with_id(
        nid("group"),
        &root,
        ModelNode::new(
            "Group",
            ModelNodeKind::MeshGroup(catchlight_core::ModelMeshGroup::new(lattice)),
        ),
    )
    .unwrap();
    m.add_node_with_id(
        nid("art"),
        &nid("group"),
        ModelNode::new("Art", ModelNodeKind::Part(ModelPart::new(mesh))),
    )
    .unwrap();
    let slot = SlotId::new("root").unwrap();
    m.slot_add(&nid("art"), slot.clone()).unwrap();
    m.slot_fill(&nid("art"), &slot, 1).unwrap();
    let config = MotionResponse {
        inputs: vec![ResponseInput {
            gain: 1.,
            source: ResponseSource::PartSlot {
                part: nid("art"),
                slot: slot.clone(),
                angle_deg: 4_f32.atan2(3.).to_degrees(),
                reference_length: 100.,
            },
        }],
        period_s: 0.4,
        damping_ratio: 0.7,
        output_param: Some(pid("broad")),
    };
    m.set_motion_response(&nid("upper"), config).unwrap();
    // A custom mesh refit is another binding writer. Reject a displaced
    // neutral cell before changing the mesh, slot, or any other binding.
    let corrective = BindingKey::new(pid("bend"), nid("art"), BindingTarget::Deform);
    m.add_binding(&corrective).unwrap();
    let before = m.to_clm_bytes().unwrap();
    let mesh = m.node_mesh(&nid("art")).unwrap().clone();
    assert!(matches!(
        m.set_node_mesh_with(&nid("art"), mesh, |_, _, offsets| vec![1.; offsets.len()]),
        Err(catchlight_core::ModelError::InvalidResponseBinding { .. })
    ));
    assert_eq!(m.to_clm_bytes().unwrap(), before);
    let mut p = Puppet::new(&m);
    p.settle_physics(&m);
    let read = |p: &Puppet| p.response_trace(&nid("upper")).unwrap();
    assert!((read(&p).target - 0.5).abs() < 1e-6);
    let input = p.response_inputs(&nid("upper")).unwrap();
    assert_eq!(input[0].position, Some([30., 40.]));
    let idx = p.node_idx(&nid("art")).unwrap();
    for node in [idx, p.node_idx(&nid("group")).unwrap()] {
        assert!(p.set_scratch_deform(node, &[catchlight_core::Vec2::new(3., 4.); 3]));
        p.combine_deforms();
        p.tick(&m, 1. / 60.);
        assert!((read(&p).target - 0.55).abs() < 1e-6, "scratch on {node:?}");
        p.clear_scratch_deform(node);
        p.tick(&m, 1. / 60.);
        assert!((read(&p).target - 0.5).abs() < 1e-6);
    }
    p.set_scratch_transform(
        idx,
        catchlight_core::ScratchTransform {
            translation: Some(catchlight_core::Vec3::new(30., 40., 0.)),
            ..Default::default()
        },
    );
    p.tick(&m, 1. / 60.);
    assert!((read(&p).target - 1.).abs() < 1e-6);
    assert!(read(&p).emitted < 0.);
    m.slot_clear(&nid("art"), &slot).unwrap();
    p.tick(&m, 1. / 60.);
    assert!(!read(&p).active);
    assert!(!p.response_trace(&nid("tip")).unwrap().active);
    assert_eq!(p.param_value(&pid("bend")), Some(0.));
    m.slot_fill(&nid("art"), &slot, 0).unwrap();
    p.tick(&m, 1. / 60.);
    assert!(read(&p).active);
    assert_eq!(p.response_trace(&nid("tip")).unwrap().lag, 0.);
}

#[test]
fn rejected_binding_edits_explain_the_zero_constraint_and_are_atomic() {
    let mut m = model(0.7);
    let key = BindingKey::new(
        pid("bend"),
        nid("upper"),
        BindingTarget::Scalar(ScalarTarget::Tx),
    );
    let require = |err: catchlight_core::ModelError, text: &str| {
        assert!(matches!(
            err,
            catchlight_core::ModelError::InvalidResponse { .. }
                | catchlight_core::ModelError::InvalidResponseBinding { .. }
        ));
        assert!(err.to_string().contains(text), "{err}");
    };
    let generation = m.generation();
    require(m.set_binding_key(&key, [1, 0], 5.).unwrap_err(), "zero-lag");
    assert_eq!(m.generation(), generation);
    assert!(m.binding(&key).is_none());
    m.add_binding(&key).unwrap();
    let generation = m.generation();
    let bytes = m.to_clm_bytes().unwrap();
    require(m.key_move(&key, &pid("bend"), 1, 0.4).unwrap_err(), "zero");
    require(m.key_delete(&key, &pid("bend"), 1).unwrap_err(), "zero");
    require(
        m.set_param_range(&pid("bend"), -1., 2.).unwrap_err(),
        "zero key",
    );
    assert_eq!(m.generation(), generation);
    assert_eq!(m.to_clm_bytes().unwrap(), bytes);
}

#[test]
fn addon_output_conflict_rolls_back_and_an_independent_reload_reports_the_same_error() {
    let mut base = model(0.7);
    let mut addon = base.extract(&[nid("tip")]);
    addon.rename_node_id(&nid("tip"), nid("other-tip")).unwrap();
    let before = base.to_clm_bytes().unwrap();
    let generation = base.generation();
    let mut replica = Model::from_clm_bytes(&before).unwrap();
    let error = base.install(&addon).unwrap_err().to_string();
    assert!(error.contains("already driven"), "{error}");
    assert_eq!(replica.install(&addon).unwrap_err().to_string(), error);
    assert_eq!(base.generation(), generation);
    assert_eq!(base.to_clm_bytes().unwrap(), before);
    base.delete_node(&nid("tip")).unwrap();
    base.install(&addon).unwrap();
    assert!(base.to_clm_bytes().is_ok());
}

#[test]
fn invalid_file_reports_the_same_response_first_on_every_load() {
    use catchlight_core::formats::clm::ClmNodeKind;
    let mut file = model(0.75).to_clm_file().unwrap();
    for n in &mut file.doc.nodes {
        if let ClmNodeKind::MotionResponse(r) = &mut n.kind {
            r.period_s = 0.;
        }
    }
    for _ in 0..32 {
        let err = Model::from_clm_file(&file).unwrap_err();
        assert!(
            matches!(err, catchlight_core::ModelError::InvalidResponse { node, .. } if node == nid("tip"))
        );
    }
}
