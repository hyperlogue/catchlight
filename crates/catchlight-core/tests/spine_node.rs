#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a test aborts on a broken fixture"
)]
//! The spine as a node the puppet ticks.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::id::SeededHex;
use catchlight_core::model::{
    ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelSpine,
};
use catchlight_core::{Mat4, Model, Name, NodeId, ParamId, Puppet, Vec2};

const DT: f32 = 1.0 / 60.0;
/// In half turns.
const THIRTY_DEGREES: f32 = 1.0 / 6.0;

#[test]
fn a_strand_turning_vertical_does_not_bulge_past_its_joint_path() {
    let f = Fixture::new(3, 100.0, 301);
    let posed = f.bend(&[0.1, -0.1, 0.0]);
    let hanging_x = 100.0 * (0.1 * std::f32::consts::PI).sin();
    for row in 0..301 {
        let center = (posed[2 * row] + posed[2 * row + 1]) * 0.5;
        assert!(
            center.x <= hanging_x + 0.001,
            "row {row}: visible hair bulges to {}, beyond hanging path {hanging_x}",
            center.x
        );
    }
}

#[test]
fn a_bent_strip_has_no_tangent_jump_at_a_joint() {
    let f = Fixture::new(2, 100.0, 2001);
    let posed = f.bend(&[0.08, -0.03]);
    // One-sided tangents at each link join must approach the same direction.
    for row in [500, 1000, 1500] {
        for column in 0..2 {
            let index = 2 * row + column;
            let before = posed[index] - posed[index - 2];
            let after = posed[index + 2] - posed[index];
            let kink = before
                .perp_dot(after)
                .atan2(before.dot(after))
                .abs()
                .to_degrees();
            assert!(
                kink < 0.5,
                "edge {column}, row {row} has a {kink} degree corner"
            );
        }
    }
}

/// Row 0 sits on the spine's root.
fn strip(rows: usize, length: f32) -> ClmMesh {
    let mut verts = Vec::with_capacity(rows * 4);
    let mut uvs = Vec::with_capacity(rows * 4);
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        let y = -length * t;
        verts.extend_from_slice(&[-5.0, y, 5.0, y]);
        uvs.extend_from_slice(&[0.0, t, 1.0, t]);
    }
    let mut indices = Vec::with_capacity(6 * (rows - 1));
    for r in 0..rows - 1 {
        let v = (r * 2) as u16;
        indices.extend_from_slice(&[v, v + 1, v + 3, v, v + 3, v + 2]);
    }
    ClmMesh {
        verts,
        uvs,
        indices: ClmIndices::U16(indices),
        origin: [0.0, 0.0],
    }
}

struct Fixture {
    model: Model,
    hex: SeededHex,
    spine: NodeId,
    part: NodeId,
    params: Vec<ParamId>,
}

impl Fixture {
    fn new(links: usize, length: f32, rows: usize) -> Self {
        let mut hex = SeededHex::new(5);
        let mut model = Model::new();
        let params: Vec<ParamId> = (0..links)
            .map(|i| {
                model
                    .add_param(
                        ModelParam {
                            name: Name::truncated(format!("bend{i}")),
                            min: -1.0,
                            max: 1.0,
                            default: 0.0,
                        },
                        &mut hex,
                    )
                    .expect("add param")
            })
            .collect();
        let root = model.root().expect("a fresh model has a root").clone();
        let joints = (0..links)
            .map(|i| [0.0, -length * (i + 1) as f32])
            .collect();
        let spine = model
            .add_node(
                &root,
                ModelNode::new("spine", ModelNodeKind::Spine(ModelSpine::new(joints))),
                &mut hex,
            )
            .expect("add the spine");
        let part = model
            .add_node(
                &spine,
                ModelNode::new(
                    "art",
                    ModelNodeKind::Part(ModelPart::new(strip(rows, length * links as f32))),
                ),
                &mut hex,
            )
            .expect("add the art");
        let targets = params.iter().cloned().map(Some).collect();
        model
            .set_spine_targets(&spine, targets)
            .expect("aim the spine");
        Self {
            model,
            hex,
            spine,
            part,
            params,
        }
    }

    fn bend(&self, bends: &[f32]) -> Vec<Vec2> {
        let mut puppet = Puppet::new(&self.model);
        for (param, &b) in self.params.iter().zip(bends) {
            puppet.set_param_value(param, b);
        }
        puppet.tick(&self.model, DT);
        self.positions(&puppet)
    }

    fn positions(&self, puppet: &Puppet) -> Vec<Vec2> {
        let idx = puppet.node_idx(&self.part).expect("the part baked");
        let node = puppet.get(idx).expect("the node");
        let catchlight_core::NodeKind::Part(p) = &node.kind else {
            panic!("not a part");
        };
        let deform = puppet.combined_deform(idx).expect("a part has a stack");
        p.mesh
            .vertices
            .iter()
            .zip(deform)
            .map(|(v, d)| *v - p.mesh.origin + *d)
            .collect()
    }
}

fn composed(joints: &[Vec2], bends: &[f32], link: usize, ramp: f32, p: Vec2) -> Vec2 {
    let rotate = |pivot: Vec2, angle: f32, q: Vec2| {
        let (sin, cos) = angle.sin_cos();
        let d = q - pivot;
        pivot + Vec2::new(d.x * cos - d.y * sin, d.x * sin + d.y * cos)
    };
    let mut point = p;
    let hinges: Vec<Vec2> = (0..joints.len())
        .map(|k| if k == 0 { Vec2::ZERO } else { joints[k - 1] })
        .collect();
    let mut moved: Vec<Vec2> = Vec::new();
    for (k, &hinge) in hinges.iter().enumerate() {
        let mut h = hinge;
        for j in 0..k {
            h = rotate(moved[j], bends[j] * std::f32::consts::PI, h);
        }
        moved.push(h);
    }
    for j in 0..link {
        point = rotate(moved[j], bends[j] * std::f32::consts::PI, point);
    }
    rotate(
        moved[link],
        ramp * bends[link] * std::f32::consts::PI,
        point,
    )
}

/// Two summed per-link deform bindings miss this point: a sum of rotations
/// is not a composed rotation.
#[test]
fn two_bent_links_land_on_the_composed_position() {
    let f = Fixture::new(2, 50.0, 3);
    let bends = [THIRTY_DEGREES, THIRTY_DEGREES];
    let posed = f.bend(&bends);
    let joints = [Vec2::new(0.0, -50.0), Vec2::new(0.0, -100.0)];

    let tip = 0.5 * (posed[4] + posed[5]);
    let want = composed(&joints, &bends, 1, 1.0, Vec2::new(0.0, -100.0));
    assert!(
        (tip - want).length() < 1e-4,
        "tip at {tip:?}, composed rigid answer {want:?}"
    );

    let root = 0.5 * (posed[0] + posed[1]);
    let knee = 0.5 * (posed[2] + posed[3]);
    let joint = composed(&joints, &bends, 0, 1.0, joints[0]);
    let rounded = 0.125 * root + 0.75 * joint + 0.125 * tip;
    assert!(knee.distance(rounded) < 1e-4);
}

#[test]
fn three_unequal_bends_round_the_composed_joint_path() {
    let f = Fixture::new(3, 40.0, 4);
    let bends = [0.2, -0.1, 0.35];
    let posed = f.bend(&bends);
    let joints = [
        Vec2::new(0.0, -40.0),
        Vec2::new(0.0, -80.0),
        Vec2::new(0.0, -120.0),
    ];
    let mut controls = vec![Vec2::ZERO];
    for (k, joint) in joints.iter().enumerate() {
        controls.push(composed(&joints, &bends, k, 1.0, *joint));
    }
    for row in 0..4 {
        let got = 0.5 * (posed[row * 2] + posed[row * 2 + 1]);
        let want = if row == 0 || row == 3 {
            controls[row]
        } else {
            controls[row - 1] * 0.125 + controls[row] * 0.75 + controls[row + 1] * 0.125
        };
        assert!(got.distance(want) < 1e-3, "row {row}: {got:?} != {want:?}");
    }
}

#[test]
fn a_single_link_has_no_artificial_curl_or_length_inflation() {
    let f = Fixture::new(1, 100.0, 101);
    let posed = f.bend(&[0.5]);
    for row in 0..101 {
        let centre = (posed[2 * row] + posed[2 * row + 1]) * 0.5;
        let want = Vec2::new(row as f32, 0.0);
        assert!(
            centre.distance(want) < 1e-4,
            "row {row}: {centre:?} != {want:?}"
        );
    }
}

#[test]
fn art_above_the_root_stays_where_it_was_drawn() {
    let mut f = Fixture::new(2, 50.0, 3);
    f.model
        .update_node(&f.part, |n| {
            n.transform.translation = [0.0, 30.0, 0.0];
        })
        .expect("move the art");
    let posed = f.bend(&[0.4, -0.25]);
    let root_row = 0.5 * (posed[0] + posed[1]);
    assert!(
        (root_row - Vec2::new(0.0, 0.0)).length() < 1e-5,
        "the row above the root moved to {root_row:?}"
    );
}

#[test]
fn an_unbent_spine_is_exactly_the_art() {
    let f = Fixture::new(3, 40.0, 4);
    let posed = f.bend(&[0.0, 0.0, 0.0]);
    for (i, p) in posed.iter().enumerate() {
        let rest = Vec2::new(if i % 2 == 0 { -5.0 } else { 5.0 }, -40.0 * (i / 2) as f32);
        assert_eq!(*p, rest, "vertex {i} moved on an unbent spine");
    }
}

#[test]
fn art_and_its_posed_guide_are_visible_through_a_response_chain() {
    let mut f = Fixture::new(1, 100., 3);
    let observe = |f: &Fixture| {
        let mut p = Puppet::new(&f.model);
        let part = p.node_idx(&f.part).unwrap();
        p.set_scratch_transform(
            part,
            catchlight_core::ScratchTransform {
                translation: Some(catchlight_core::Vec3::new(5., 0., 0.)),
                ..Default::default()
            },
        );
        p.set_param_value(&f.params[0], 0.5);
        p.tick(&f.model, DT);
        let catchlight_core::NodeKind::Spine(spine) =
            &p.get(p.node_idx(&f.spine).unwrap()).unwrap().kind
        else {
            panic!("expected spine")
        };
        (f.positions(&p), spine.posed_guide().to_vec())
    };
    let expected = observe(&f);
    assert_ne!(expected.0, f.bend(&[0.]));
    assert_eq!(expected.1.len(), 2);
    let mut parent = f.spine.clone();
    for name in ["upper", "tip"] {
        parent = f
            .model
            .add_node(
                &parent,
                ModelNode::new(name, ModelNodeKind::MotionResponse(Default::default())),
                &mut f.hex,
            )
            .unwrap();
    }
    f.model.reparent(&f.part, &parent).unwrap();
    assert_eq!(observe(&f), expected);
}

#[test]
fn a_part_rotated_under_its_spine_still_turns_with_it() {
    let mut f = Fixture::new(1, 100.0, 3);
    f.model
        .update_node(&f.part, |n| {
            n.transform.rotation = [0.0, 0.0, std::f32::consts::FRAC_PI_2];
        })
        .expect("turn the art");
    f.model
        .set_spine_joints(&f.spine, vec![[100.0, 0.0]])
        .expect("lay the spine along the art");
    let posed = f.bend(&[0.5]);
    let tip = 0.5 * (posed[4] + posed[5]);
    assert!(
        (tip - Vec2::new(100.0, 0.0)).length() < 1e-3,
        "tip at {tip:?} in the part's own frame"
    );
}

/// Bending and warping together is not bending plus warping: the group folds
/// from the position the spine left.
#[test]
fn a_mesh_group_above_a_spine_warps_what_the_spine_bent() {
    let mut hex = SeededHex::new(9);
    let mut model = Model::new();
    let root = model.root().expect("root").clone();
    let lattice = ClmMesh {
        verts: vec![-160.0, 60.0, 160.0, 60.0, -160.0, -160.0, 160.0, -160.0],
        uvs: vec![0.0; 8],
        indices: ClmIndices::U16(vec![0, 1, 3, 0, 3, 2]),
        origin: [0.0, 0.0],
    };
    let group = model
        .add_node(
            &root,
            ModelNode::new(
                "lattice",
                ModelNodeKind::MeshGroup(ModelMeshGroup::new(lattice)),
            ),
            &mut hex,
        )
        .expect("add the group");
    let spine = model
        .add_node(
            &group,
            ModelNode::new(
                "spine",
                ModelNodeKind::Spine(ModelSpine::new(vec![[0.0, -100.0]])),
            ),
            &mut hex,
        )
        .expect("add the spine");
    let part = model
        .add_node(
            &spine,
            ModelNode::new("art", ModelNodeKind::Part(ModelPart::new(strip(3, 100.0)))),
            &mut hex,
        )
        .expect("add the art");
    let mut param = |name: &str, model: &mut Model| {
        model
            .add_param(
                ModelParam {
                    name: Name::truncated(name),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param")
    };
    let bend = param("bend", &mut model);
    let warp = param("warp", &mut model);
    model
        .set_spine_targets(&spine, vec![Some(bend.clone())])
        .expect("aim the spine");
    let key = catchlight_core::BindingKey::new(
        warp.clone(),
        group.clone(),
        catchlight_core::BindingTarget::Deform,
    );
    model
        .set_deform_vertices(&key, [1, 0], vec![0.0, 0.0, 0.0, 0.0, 60.0, 0.0, 60.0, 0.0])
        .expect("author the warp");

    let posed = |bend_v: f32, warp_v: f32| {
        let mut puppet = Puppet::new(&model);
        puppet.set_param_value(&bend, bend_v);
        puppet.set_param_value(&warp, warp_v);
        puppet.tick(&model, DT);
        let idx = puppet.node_idx(&part).expect("the part baked");
        puppet.combined_deform(idx).expect("a stack").to_vec()
    };
    let bent = posed(0.5, 0.0);
    let warped = posed(0.0, 1.0);
    let both = posed(0.5, 1.0);

    assert!(bent[4].length() > 1.0, "the spine moved nothing");
    assert!(warped[4].length() > 1.0, "the group moved nothing");
    let sum = bent[4] + warped[4];
    assert!(
        (both[4] - sum).length() > 1.0,
        "bend+warp {both:?} is the plain sum {sum:?}; the group did not see the bend",
        both = both[4],
        sum = sum
    );
}

/// A spine reads a param's value, not its normalized position along the
/// range: a bend of 0.5 is a quarter turn whatever `min` and `max` are.
#[test]
fn a_bend_is_the_params_value_and_not_its_position() {
    let mut f = Fixture::new(1, 100.0, 2);
    f.model
        .set_param_range(&f.params[0], -4.0, 4.0)
        .expect("widen the range");
    let posed = f.bend(&[0.5]);
    let tip = 0.5 * (posed[2] + posed[3]);
    assert!(
        (tip - Vec2::new(100.0, 0.0)).length() < 1e-3,
        "tip at {tip:?} on a param ranging -4..4"
    );
}

#[test]
fn an_edit_elsewhere_leaves_the_turn_alone() {
    let mut f = Fixture::new(2, 50.0, 3);
    let before = f.bend(&[0.3, 0.1]);
    let root = f.model.root().expect("root").clone();
    f.model
        .add_node(
            &root,
            ModelNode::new("bystander", ModelNodeKind::Group),
            &mut f.hex,
        )
        .expect("add a node");
    let after = f.bend(&[0.3, 0.1]);
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(
            (*a - *b).length() < 1e-4,
            "vertex {i} moved across a rebake"
        );
    }
}

/// The root transform cancels out of the spine-to-child mapping.
#[test]
fn the_render_root_does_not_change_the_turn() {
    let f = Fixture::new(2, 50.0, 3);
    let mut plain = Puppet::new(&f.model);
    let mut folded = Puppet::new(&f.model);
    for p in [&mut plain, &mut folded] {
        p.set_param_value(&f.params[0], 0.3);
        p.set_param_value(&f.params[1], -0.2);
    }
    plain.tick(&f.model, DT);
    folded.tick_with_root(
        &f.model,
        Mat4::from_scale(catchlight_core::Vec3::splat(3.0)),
        DT,
    );
    let idx = plain.node_idx(&f.part).expect("baked");
    let a = plain.combined_deform(idx).expect("stack").to_vec();
    let b = folded.combined_deform(idx).expect("stack").to_vec();
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert!((*x - *y).length() < 1e-4, "vertex {i} differs under a root");
    }
}
