#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The spine as a node the puppet ticks.
//!
//! The claim a spine exists to make is that its joints *compose*: a vertex
//! under two bent links lands where the two rigid rotations put it, and no
//! link changes length doing it. Summed per-link deform bindings do neither,
//! which is what these tests measure against. The rest is wiring — that the
//! bends come off the params, that a vertex above the root stays put, that a
//! turned part and a group above the spine both still work.
//!
//! Every model here is built through `Model`'s own API rather than through a
//! `.clm`, so a failure is the runtime's and not the reader's.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::id::SeededHex;
use catchlight_core::model::{
    ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelSpine,
};
use catchlight_core::{Mat4, Model, Name, NodeId, ParamId, Puppet, Vec2};

const DT: f32 = 1.0 / 60.0;
/// A sixth of a turn, in the half turns a bend is measured in.
const THIRTY_DEGREES: f32 = 1.0 / 6.0;

/// A strip two columns wide running down from the origin, `rows` rows deep
/// over `length` model pixels. Row 0 sits on the spine's root.
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

/// A spine hanging straight down with `links` links of `length` each, and one
/// bend param per link ready to be aimed at.
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

    /// Pose every bend and tick once, then report the part's deformed vertex
    /// positions in its own space.
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

/// The rigid position of a point under a chain of turns about moving hinges —
/// the answer a spine is claiming to produce, computed here the long way.
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
    // Carry each hinge through the turns above it, exactly as the pass does.
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

/// Two links bent a sixth of a turn each put the tip exactly where the two
/// rotations compose — and leave both links the length they were drawn.
///
/// This is the whole reason the kind exists. Two summed per-link deform
/// bindings authored at the same bends miss that point by a visible margin and
/// stretch the lower link along the way, because a sum of two rotations of the
/// rest shape is not the rotation of a rotation.
#[test]
fn two_bent_links_land_on_the_composed_position() {
    let f = Fixture::new(2, 50.0, 3);
    let bends = [THIRTY_DEGREES, THIRTY_DEGREES];
    let posed = f.bend(&bends);
    let joints = [Vec2::new(0.0, -50.0), Vec2::new(0.0, -100.0)];

    // Row 2 is the tip: 100 px down, at the far end of the second link.
    let tip = 0.5 * (posed[4] + posed[5]);
    let want = composed(&joints, &bends, 1, 1.0, Vec2::new(0.0, -100.0));
    assert!(
        (tip - want).length() < 1e-4,
        "tip at {tip:?}, composed rigid answer {want:?}"
    );

    // Neither link changed length: the turn is rigid on the spine's own axis.
    let root = 0.5 * (posed[0] + posed[1]);
    let knee = 0.5 * (posed[2] + posed[3]);
    assert!(
        ((knee - root).length() - 50.0).abs() < 1e-3,
        "upper link is {} long",
        (knee - root).length()
    );
    assert!(
        ((tip - knee).length() - 50.0).abs() < 1e-3,
        "lower link is {} long",
        (tip - knee).length()
    );
}

/// The same claim, one link deeper and at two different bends, so a test that
/// only happened to work for equal angles would fail.
#[test]
fn three_unequal_bends_compose_at_every_joint() {
    let f = Fixture::new(3, 40.0, 4);
    let bends = [0.2, -0.1, 0.35];
    let posed = f.bend(&bends);
    let joints = [
        Vec2::new(0.0, -40.0),
        Vec2::new(0.0, -80.0),
        Vec2::new(0.0, -120.0),
    ];
    for (row, (link, rest)) in [(0usize, 0.0f32), (1, -40.0), (2, -80.0), (3, -120.0)]
        .into_iter()
        .enumerate()
        .map(|(r, (l, y))| (r, (l.min(2), y)))
    {
        let ramp = if row == 0 { 0.0 } else { 1.0 };
        let got = 0.5 * (posed[row * 2] + posed[row * 2 + 1]);
        let want = composed(&joints, &bends, link, ramp, Vec2::new(0.0, rest));
        assert!(
            (got - want).length() < 1e-3,
            "row {row} at {got:?}, composed {want:?}"
        );
    }
}

/// Halfway into a link, a vertex takes half that link's turn — the ramp that
/// makes a crease a bend rather than a hinge. This pins the mapping; what the
/// mapping costs in length is measured by the test after it.
#[test]
fn a_vertex_halfway_into_a_link_takes_half_its_turn() {
    // Five rows over one 100 px link: row 2 sits at the middle.
    let f = Fixture::new(1, 100.0, 5);
    let posed = f.bend(&[0.5]);
    let middle = 0.5 * (posed[4] + posed[5]);
    // A bend of 0.5 half turns is a quarter turn; half of it is an eighth,
    // about the root, applied to the vertex at (0, -50).
    let (sin, cos) = std::f32::consts::FRAC_PI_4.sin_cos();
    let want = Vec2::new(50.0 * sin, -50.0 * cos);
    assert!(
        (middle - want).length() < 1e-3,
        "middle at {middle:?}, want {want:?}"
    );
}

/// **The joints are exact and the art between them stretches**, and this
/// pins how much: a 100 px link bent a quarter turn keeps its tip 100 px from
/// the root, and its three-row centre line measures 123.7 px, the middle row
/// having turned half as far about the same hinge. The number is the ramp's
/// trade, documented in the module doc, and a change that moves it is a
/// change to what a bend looks like.
#[test]
fn the_ramp_stretches_the_interior_between_exact_joints() {
    // Three rows over one 100 px link: root, middle and tip.
    let f = Fixture::new(1, 100.0, 3);
    let posed = f.bend(&[0.5]);
    let centre = |row: usize| 0.5 * (posed[2 * row] + posed[2 * row + 1]);
    let (root, middle, tip) = (centre(0), centre(1), centre(2));
    assert!(
        (tip - root).length() > 100.0 - 1e-3 && (tip - root).length() < 100.0 + 1e-3,
        "the joint kept its distance from the hinge: tip at {tip:?}",
    );
    let along = (middle - root).length() + (tip - middle).length();
    assert!(
        (along - 123.68).abs() < 0.05,
        "the centre line of a quarter-turn link measures {along}, want 123.68",
    );
}

/// A vertex above the spine's root does not move, however hard the spine
/// bends: it projects onto the root, and the transform there is the identity.
#[test]
fn art_above_the_root_stays_where_it_was_drawn() {
    let mut f = Fixture::new(2, 50.0, 3);
    // Push the art up so its first row sits 30 px above the spine's origin.
    f.model
        .update_node(&f.part, |n| {
            n.transform.translation = [0.0, 30.0, 0.0];
        })
        .expect("move the art");
    let posed = f.bend(&[0.4, -0.25]);
    // Rows 0 and 1 of a 3-row, 100 px strip shifted up by 30 sit at +30 and
    // -20 in spine space; only the first is above the root.
    let root_row = 0.5 * (posed[0] + posed[1]);
    assert!(
        (root_row - Vec2::new(0.0, 0.0)).length() < 1e-5,
        "the row above the root moved to {root_row:?}"
    );
}

/// Every bend at zero is the art as drawn, to the bit: the pass writes
/// nothing and the deform stack stays empty.
#[test]
fn an_unbent_spine_is_exactly_the_art() {
    let f = Fixture::new(3, 40.0, 4);
    let posed = f.bend(&[0.0, 0.0, 0.0]);
    for (i, p) in posed.iter().enumerate() {
        let rest = Vec2::new(if i % 2 == 0 { -5.0 } else { 5.0 }, -40.0 * (i / 2) as f32);
        assert_eq!(*p, rest, "vertex {i} moved on an unbent spine");
    }
}

/// A part turned under its spine still bends about the spine's own joints:
/// the mapping is rebuilt from the current globals every frame, exactly as a
/// mesh group's is.
#[test]
fn a_part_rotated_under_its_spine_still_turns_with_it() {
    let mut f = Fixture::new(1, 100.0, 3);
    // A quarter turn on the part sends its own -Y down the spine's +X, so the
    // spine is laid along +X to run down the art where the rotation puts it.
    f.model
        .update_node(&f.part, |n| {
            n.transform.rotation = [0.0, 0.0, std::f32::consts::FRAC_PI_2];
        })
        .expect("turn the art");
    f.model
        .set_spine_joints(&f.spine, vec![[100.0, 0.0]])
        .expect("lay the spine along the art");
    let posed = f.bend(&[0.5]);
    // The part's own (0, -100) is spine (100, 0); a quarter turn of the spine
    // about its root sends that to (0, 100), which is (100, 0) back in the
    // part's frame.
    let tip = 0.5 * (posed[4] + posed[5]);
    assert!(
        (tip - Vec2::new(100.0, 0.0)).length() < 1e-3,
        "tip at {tip:?} in the part's own frame"
    );
}

/// A mesh group above a spine warps the art the spine bent, rather than
/// stopping at the spine node.
///
/// The proof is that the two do not merely add: the spine runs first and the
/// group folds from the position it left, so bending and warping together is
/// not the same as bending plus warping. A lattice that shifts only its lower
/// edge is what makes the difference visible — where a vertex sits when the
/// group samples it decides how much of the shift it gets.
#[test]
fn a_mesh_group_above_a_spine_warps_what_the_spine_bent() {
    let mut hex = SeededHex::new(9);
    let mut model = Model::new();
    let root = model.root().expect("root").clone();
    // A lattice covering the whole strip, so every art vertex sits in a
    // triangle of it.
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
    // The lower edge of the lattice slides +X; the upper edge stays.
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

    // Each acts on its own.
    assert!(bent[4].length() > 1.0, "the spine moved nothing");
    assert!(warped[4].length() > 1.0, "the group moved nothing");
    // And together they are not a sum: the group sampled the bent position.
    let sum = bent[4] + warped[4];
    assert!(
        (both[4] - sum).length() > 1.0,
        "bend+warp {both:?} is the plain sum {sum:?}; the group did not see the bend",
        both = both[4],
        sum = sum
    );
}

/// A spine reads a param's value, not its normalized position along the
/// range: a bend of 0.5 is a quarter turn whatever `min` and `max` are. Read
/// as a position, 0.5 on a `-4..4` param would be 0.5625 of the way along and
/// the tip would land somewhere else entirely.
#[test]
fn a_bend_is_the_params_value_and_not_its_position() {
    let mut f = Fixture::new(1, 100.0, 2);
    f.model
        .set_param_range(&f.params[0], -4.0, 4.0)
        .expect("widen the range");
    let posed = f.bend(&[0.5]);
    let tip = 0.5 * (posed[2] + posed[3]);
    // A quarter turn about the root sends (0, -100) to (100, 0).
    assert!(
        (tip - Vec2::new(100.0, 0.0)).length() < 1e-3,
        "tip at {tip:?} on a param ranging -4..4"
    );
}

/// A rebake keeps the spine turning: the assignment is derived again from the
/// same rest geometry, so an edit elsewhere in the model does not disturb it.
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

/// The pass reads the world transform of the spine and the part, so a model
/// rendered under a root fold turns the same: the root cancels out of the
/// spine-to-child mapping.
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
