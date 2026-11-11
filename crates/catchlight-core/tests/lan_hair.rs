#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The bundled model's long locks must hang below a rolling head. Exercise
//! the actual model through Puppet so carry, output params and limits all
//! participate. Requires the Lan Git LFS object, like the visual suite.
//!
//! Long locks keep a sprung root that follows the scalp and limp lower
//! links. The second joint has room to counter head roll; narrow stops along
//! the lower lengths must not prevent them from hanging under gravity.

use catchlight_core::{Model, NodeKind, ParamId, Puppet, Vec2, Vec3};

const DT: f32 = 1.0 / 60.0;

fn lan() -> Model {
    Model::from_clm_bytes(
        &std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/models/lan.clm"
        ))
        .expect("fetch the Lan Git LFS object"),
    )
    .unwrap()
}

#[test]
fn long_hair_hangs_down_while_its_roots_follow_head_roll() {
    let model = lan();
    let mut puppet = Puppet::new(&model);
    let roll = ParamId::new("head_roll").unwrap();
    puppet.settle_physics(&model);
    let initial: Vec<_> = model
        .node_ids()
        .filter_map(|id| {
            let idx = puppet.node_idx(id)?;
            let NodeKind::Spine(spine) = &puppet.get(idx)?.kind else {
                return None;
            };
            let chain = spine.chain.as_ref()?;
            (chain.links.len() >= 6 && id.as_str().contains("Hair-Lock")).then(|| {
                let tip = chain.particles.last().unwrap().pos;
                (idx, tip, chain.carry.inverse() * (tip - chain.anchor))
            })
        })
        .collect();
    // On a small first movement the tip should lag the head, not be carried
    // rigidly and only start moving under gravity on later frames.
    puppet.set_param_value(&roll, 1.0);
    puppet.tick(&model, DT);
    for (idx, old_tip, local_tip) in initial {
        let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
            unreachable!()
        };
        let chain = spine.chain.as_ref().unwrap();
        let carried_tip = chain.anchor + chain.carry * local_tip;
        let tip = chain.particles.last().unwrap().pos;
        assert!(
            tip.distance(old_tip) < carried_tip.distance(old_tip) * 0.95,
            "the tip must lag the head immediately: moved {}, rigid carry {}",
            tip.distance(old_tip),
            carried_tip.distance(old_tip)
        );
    }
    for angle in [-30.0, 30.0] {
        puppet.set_param_value(&roll, angle);
        for _ in 0..300 {
            puppet.tick(&model, DT);
        }
        let mut locks = 0;
        for id in model.node_ids() {
            let idx = puppet.node_idx(id).unwrap();
            let NodeKind::Spine(spine) = &puppet.get(idx).unwrap().kind else {
                continue;
            };
            let Some(chain) = &spine.chain else { continue };
            if chain.links.len() < 6 || !id.as_str().contains("Hair-Lock") {
                continue;
            }
            locks += 1;
            // The scalp attachment is fixed in the parent's frame. Moving
            // the spine and counter-moving its art puts the simulation's
            // pivot somewhere other than the visible strand's root.
            let node = model.node(id).unwrap();
            let parent = puppet.node_idx(node.parent().unwrap()).unwrap();
            let expected = puppet
                .transforms()
                .get(parent)
                .transform_point3(Vec3::from_array(node.transform.translation));
            assert!(
                chain.anchor.distance(Vec2::new(expected.x, -expected.y)) < 0.01,
                "{id} at {angle}: simulation root must remain on the scalp"
            );
            let particles = &chain.particles;
            let root = particles[1].pos - particles[0].pos;
            let drawn = chain.carry * chain.links[0].drawn;
            let root_bend = root
                .normalize()
                .angle_to(drawn.normalize())
                .abs()
                .to_degrees();
            assert!(
                root_bend < 5.0,
                "{id} at {angle}: root detached from head direction by {root_bend} degrees"
            );
            let n = particles.len();
            let tail = particles[n - 1].pos - particles[n - 2].pos;
            let tilt = tail.angle_to(Vec2::Y).abs().to_degrees();
            assert!(
                tilt < 8.0,
                "{id} at {angle}: lower hair remains {tilt} degrees from gravity"
            );
        }
        assert!(
            locks >= 10,
            "must exercise the bundled long front and back locks"
        );
    }
}
