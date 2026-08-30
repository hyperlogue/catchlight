#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The new `Puppet` against the runtime it replaces, frame for frame.
//!
//! Both runtimes are driven from the *same* `.clp`: the legacy path builds a
//! [`LegacyPuppet`] from it, the new one reads it into a [`Model`] and bakes a
//! [`Puppet`]. For a grid of poses (and, where a model has drivers, a settle
//! plus a run of simulated frames) every node's evaluated frame has to agree —
//! global transform, z order, opacity, tint, screen tint and combined deform.
//!
//! The combined deform is what proves the passes downstream of the fold: a
//! mesh group's attachments, the `translate_children` filter and the weld solve
//! all land in a node's deform stack, so a difference in any of them shows up
//! here as a per-vertex difference. Nothing else observes them.

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clp::{
    self, ClpBinding, ClpBindingValues, ClpCell, ClpCells, ClpComposite, ClpDocument, ClpFile,
    ClpIndices, ClpMask, ClpMesh, ClpMeshGroup, ClpNode, ClpNodeKind, ClpParam, ClpPart,
    ClpPhysics, ClpSimplePhysics, ClpTransform, ClpWeld, ClpWeldPair, FORMAT_VERSION,
};
use catchlight_core::params::InterpolateMode;
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::puppet::Puppet;
use catchlight_core::{
    from_clp, GlobalTransforms, LegacyPuppet, Mat4, Model, NodeId, NodeIdx, NodeKind, ParamId, Vec2,
};

/// Absolute tolerance on every compared quantity. Both runtimes do the same
/// arithmetic but not always in the same association order (the fold walks
/// bindings, not params), so the difference is float reassociation, not method.
const TOL: f32 = 1e-5;

const DT: f32 = 1.0 / 60.0;

// ---------------------------------------------------------------------------
// Comparing one file, driven both ways
// ---------------------------------------------------------------------------

struct Pair {
    legacy: LegacyPuppet,
    transforms: GlobalTransforms,
    model: Model,
    puppet: Puppet,
    /// `.clp` node index -> (legacy slot, new slot).
    nodes: Vec<(NodeIdx, NodeIdx)>,
}

impl Pair {
    fn load(file: &ClpFile) -> Pair {
        let legacy = from_clp(file, 0).expect("legacy build");
        let model = Model::from_clp_file(file).expect("model build");
        let puppet = Puppet::new(&model);
        let nodes = (0..file.doc.nodes.len())
            .map(|i| {
                let id = if i == 0 {
                    NodeId::new("root").unwrap()
                } else {
                    NodeId::new(format!("node-{i}")).unwrap()
                };
                (
                    legacy.node_for_uuid(i as u32).expect("legacy node"),
                    puppet.node_idx(&id).expect("baked node"),
                )
            })
            .collect();
        Pair {
            legacy,
            transforms: GlobalTransforms::new(),
            model,
            puppet,
            nodes,
        }
    }

    fn pose(&mut self, file: &ClpFile, values: &[(f32, f32)]) {
        for (j, p) in file.doc.params.iter().enumerate() {
            let (x, y) = values[j];
            self.legacy.set_param_value(j as u32, Vec2::new(x, y));
            if p.is_vec2 {
                self.puppet
                    .set_param_value(&ParamId::new(format!("param-{j}.x")).unwrap(), x);
                self.puppet
                    .set_param_value(&ParamId::new(format!("param-{j}.y")).unwrap(), y);
            } else {
                self.puppet
                    .set_param_value(&ParamId::new(format!("param-{j}")).unwrap(), x);
            }
        }
    }

    fn settle(&mut self) {
        self.legacy.settle_physics();
        self.puppet.settle_physics(&self.model);
    }

    fn tick(&mut self, dt: f32) {
        self.legacy.tick(&mut self.transforms, Mat4::IDENTITY, dt);
        self.puppet.tick(&self.model, dt);
    }

    fn assert_agrees(&self, label: &str) {
        for (i, &(l, n)) in self.nodes.iter().enumerate() {
            let where_ = format!("{label}: node {i}");
            let (lt, nt) = (self.transforms.get(l), self.puppet.transforms().get(n));
            for (a, b) in lt.to_cols_array().iter().zip(nt.to_cols_array().iter()) {
                assert!(
                    (a - b).abs() <= TOL,
                    "{where_}: global transform\n legacy {lt:?}\n new    {nt:?}"
                );
            }
            let ln = self.legacy.get(l).expect("legacy node");
            let nn = self.puppet.get(n).expect("new node");
            close(&where_, "z_order", ln.z_order, nn.z_order);
            assert_eq!(
                ln.enabled, nn.enabled,
                "{where_}: enabled {} vs {}",
                ln.enabled, nn.enabled
            );
            let lc = colour(&ln.kind);
            let nc = colour(&nn.kind);
            match (lc, nc) {
                (Some(lc), Some(nc)) => {
                    close(&where_, "opacity", lc.0, nc.0);
                    for k in 0..3 {
                        close(&where_, "tint", lc.1[k], nc.1[k]);
                        close(&where_, "screen_tint", lc.2[k], nc.2[k]);
                    }
                }
                (None, None) => {}
                _ => panic!("{where_}: node kinds disagree"),
            }
            match (deform(&ln.kind), self.puppet.combined_deform(n)) {
                (Some(ld), Some(nd)) => {
                    assert_eq!(ld.len(), nd.len(), "{where_}: deform length");
                    for (v, (a, b)) in ld.iter().zip(nd.iter()).enumerate() {
                        assert!(
                            (a.x - b.x).abs() <= TOL && (a.y - b.y).abs() <= TOL,
                            "{where_}: deform vertex {v}: legacy {a:?} vs new {b:?}"
                        );
                    }
                }
                (None, None) => {}
                _ => panic!("{where_}: one runtime has a deform stack and the other does not"),
            }
        }
    }
}

fn colour(kind: &NodeKind) -> Option<(f32, [f32; 3], [f32; 3])> {
    match kind {
        NodeKind::Part(p) => Some((p.opacity, p.tint.to_array(), p.screen_tint.to_array())),
        NodeKind::Composite(c) => Some((c.opacity, c.tint.to_array(), c.screen_tint.to_array())),
        _ => None,
    }
}

fn deform(kind: &NodeKind) -> Option<&[Vec2]> {
    match kind {
        NodeKind::Part(p) => Some(p.deform_stack.combined()),
        NodeKind::MeshGroup(mg) => Some(mg.deform_stack.combined()),
        _ => None,
    }
}

fn close(where_: &str, what: &str, a: f32, b: f32) {
    assert!(
        (a - b).abs() <= TOL,
        "{where_}: {what}: legacy {a} vs new {b}"
    );
}

/// Poses to drive a file through: everything at rest, everything at each
/// extreme and at the middle, then each param swept on its own so a binding
/// that only one param reaches is still exercised.
fn pose_grid(file: &ClpFile) -> Vec<Vec<(f32, f32)>> {
    let params = &file.doc.params;
    let at = |k: usize| -> Vec<(f32, f32)> {
        params
            .iter()
            .map(|p| match k {
                0 => (p.defaults[0], p.defaults[1]),
                1 => (p.min[0], p.min[1]),
                2 => (p.max[0], p.max[1]),
                _ => ((p.min[0] + p.max[0]) * 0.5, (p.min[1] + p.max[1]) * 0.5),
            })
            .collect()
    };
    let mut poses: Vec<Vec<(f32, f32)>> = (0..4).map(at).collect();
    let rest = at(0);
    for (j, p) in params.iter().enumerate() {
        for &t in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut pose = rest.clone();
            pose[j] = (
                p.min[0] + (p.max[0] - p.min[0]) * t,
                p.min[1] + (p.max[1] - p.min[1]) * (1.0 - t),
            );
            poses.push(pose);
        }
    }
    poses
}

fn check(label: &str, file: &ClpFile) {
    let mut pair = Pair::load(file);
    let has_physics = file
        .doc
        .nodes
        .iter()
        .any(|n| matches!(n.kind, ClpNodeKind::SimplePhysics(_)));

    for (k, pose) in pose_grid(file).into_iter().enumerate() {
        pair.pose(file, &pose);
        // Two ticks: the first folds, the second exercises the memo that lets
        // an unchanged pose skip the fold.
        pair.tick(DT);
        pair.assert_agrees(&format!("{label} pose {k}"));
        pair.tick(DT);
        pair.assert_agrees(&format!("{label} pose {k} (second tick)"));
    }

    if has_physics {
        let mut pair = Pair::load(file);
        pair.settle();
        pair.tick(DT);
        pair.assert_agrees(&format!("{label} settled"));
        for frame in 0..90 {
            pair.tick(DT);
            pair.assert_agrees(&format!("{label} physics frame {frame}"));
        }
    }
}

// ---------------------------------------------------------------------------
// The committed fixtures
// ---------------------------------------------------------------------------

#[test]
fn every_committed_fixture_evaluates_the_same_both_ways() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/models");
    let mut seen = 0;
    for entry in std::fs::read_dir(dir).expect("read tests/models") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("clp") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("read fixture");
        let file = clp::decode(&bytes).expect("decode fixture");
        let label = path.file_name().unwrap().to_string_lossy().to_string();
        check(&label, &file);
        seen += 1;
    }
    assert!(seen > 0, "no .clp fixtures found in {dir}");
}

// ---------------------------------------------------------------------------
// Synthetic rigs: the features the committed fixtures do not cover
// ---------------------------------------------------------------------------

fn transform() -> ClpTransform {
    ClpTransform {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0, 1.0],
    }
}

fn quad(w: f32, h: f32) -> ClpMesh {
    ClpMesh {
        verts: vec![-w, -h, w, -h, w, h, -w, h],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClpIndices::U16(vec![0, 1, 2, 2, 3, 0]),
        origin: [0.0, 0.0],
    }
}

fn part(mesh: ClpMesh) -> ClpPart {
    ClpPart {
        mesh,
        albedo: u32::MAX,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0; 3],
        screen_tint: [0.0; 3],
        masks: Vec::new(),
        mask_threshold: 0.5,
    }
}

fn node(parent: Option<u32>, name: &str, kind: ClpNodeKind) -> ClpNode {
    ClpNode {
        parent,
        name: name.into(),
        enabled: true,
        z_order: 0.0,
        transform: transform(),
        lock_to_root: false,
        kind,
    }
}

fn at(parent: Option<u32>, name: &str, xy: [f32; 2], kind: ClpNodeKind) -> ClpNode {
    let mut n = node(parent, name, kind);
    n.transform.translation = [xy[0], xy[1], 0.0];
    n
}

fn cells<T>(entries: Vec<(u32, u32, T)>) -> ClpCells<T> {
    ClpCells {
        cells: entries
            .into_iter()
            .map(|(x, y, value)| ClpCell { x, y, value })
            .collect(),
    }
}

fn file(nodes: Vec<ClpNode>, params: Vec<ClpParam>, welds: Vec<ClpWeld>) -> ClpFile {
    ClpFile {
        version: FORMAT_VERSION,
        doc: ClpDocument {
            physics: ClpPhysics::default(),
            nodes,
            params,
            welds,
        },
        textures: Vec::new(),
    }
}

/// A part under a group, driven by one 2-D param whose deform binding is
/// authored at scattered cells of a 3x3 grid, plus scalar bindings on every
/// other target the runtime folds.
fn two_param_rig(mode: InterpolateMode) -> ClpFile {
    let nodes = vec![
        node(None, "Root", ClpNodeKind::Group),
        at(
            Some(0),
            "Body",
            [10.0, -5.0],
            ClpNodeKind::Part(part(quad(8.0, 6.0))),
        ),
        node(
            Some(0),
            "Layered",
            ClpNodeKind::Composite(ClpComposite {
                opacity: 0.9,
                blend_mode: BlendMode::Normal,
                tint: [1.0, 0.8, 0.7],
                screen_tint: [0.0; 3],
                masks: vec![ClpMask {
                    source: 1,
                    mode: MaskMode::Mask,
                }],
                mask_threshold: 0.4,
                propagate_meshgroup: true,
            }),
        ),
        at(
            Some(2),
            "Face",
            [0.0, 4.0],
            ClpNodeKind::Part(part(quad(3.0, 3.0))),
        ),
    ];
    let d = |x: u32, y: u32, v: Vec<f32>| (x, y, v);
    let params = vec![ClpParam {
        name: "Head".into(),
        is_vec2: true,
        min: [-1.0, -2.0],
        max: [1.0, 2.0],
        defaults: [0.0, 0.5],
        axis_points_x: vec![0.0, 0.25, 1.0],
        axis_points_y: vec![0.0, 0.5, 1.0],
        bindings: vec![
            ClpBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClpBindingValues::Deform(cells(vec![
                    d(0, 0, vec![-2.0, 0.5, 0.0, 0.0, 1.0, -1.0, 0.0, 3.0]),
                    d(2, 0, vec![4.0, 0.0, -1.0, 2.0, 0.0, 0.0, 0.5, 0.5]),
                    d(1, 2, vec![0.0, -3.0, 2.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
                    d(2, 2, vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
                ])),
            },
            ClpBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClpBindingValues::TransformTX(cells(vec![(0, 0, -6.0), (2, 2, 9.0)])),
            },
            ClpBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClpBindingValues::TransformRZ(cells(vec![(2, 0, 0.7)])),
            },
            ClpBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClpBindingValues::TransformSY(cells(vec![(0, 2, 1.8)])),
            },
            ClpBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClpBindingValues::ZOrder(cells(vec![(0, 0, -4.0), (2, 2, 6.0)])),
            },
            ClpBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClpBindingValues::Opacity(cells(vec![(0, 0, 0.25), (2, 2, 1.0)])),
            },
            ClpBinding {
                node: 3,
                interpolate_mode: mode,
                values: ClpBindingValues::TintG(cells(vec![(0, 0, 0.2), (2, 2, 1.0)])),
            },
            ClpBinding {
                node: 3,
                interpolate_mode: mode,
                values: ClpBindingValues::ScreenTintB(cells(vec![(1, 1, 0.6)])),
            },
            ClpBinding {
                node: 2,
                interpolate_mode: mode,
                values: ClpBindingValues::Opacity(cells(vec![(0, 2, 0.3)])),
            },
        ],
    }];
    file(nodes, params, Vec::new())
}

#[test]
fn a_two_param_rig_folds_the_same_in_every_interpolation_mode() {
    for mode in [
        InterpolateMode::Linear,
        InterpolateMode::Nearest,
        InterpolateMode::Stepped,
        InterpolateMode::Cubic,
    ] {
        check(&format!("two_param {mode:?}"), &two_param_rig(mode));
    }
}

/// Two one-param bindings on one node, from different params — the case the
/// legacy runtime gives one deform slot per param and the new one gives one
/// per binding.
#[test]
fn two_params_deforming_one_node_sum_the_same() {
    let nodes = vec![
        node(None, "Root", ClpNodeKind::Group),
        node(Some(0), "Body", ClpNodeKind::Part(part(quad(5.0, 5.0)))),
    ];
    let binding = |v: Vec<f32>| ClpBinding {
        node: 1,
        interpolate_mode: InterpolateMode::Linear,
        values: ClpBindingValues::Deform(cells(vec![(1, 0, v)])),
    };
    let param = |name: &str, values: Vec<f32>| ClpParam {
        name: name.into(),
        is_vec2: false,
        min: [0.0, 0.0],
        max: [1.0, 1.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![binding(values)],
    };
    let params = vec![
        param("A", vec![3.0, 0.0, 0.0, 0.0, -1.0, 2.0, 0.0, 0.0]),
        param("B", vec![0.0, -4.0, 1.0, 1.0, 0.0, 0.0, 2.0, 0.5]),
    ];
    check("two params one node", &file(nodes, params, Vec::new()));
}

/// A mesh group over two parts, keyed by a param: the descent, the attachment
/// bake and the `translate_children` filter all have to land the same.
fn mesh_group_rig(dynamic: bool, translate_children: bool) -> ClpFile {
    let nodes = vec![
        node(None, "Root", ClpNodeKind::Group),
        node(
            Some(0),
            "MG",
            ClpNodeKind::MeshGroup(ClpMeshGroup {
                mesh: quad(20.0, 20.0),
                dynamic,
                translate_children,
            }),
        ),
        at(
            Some(1),
            "Under",
            [2.0, 3.0],
            ClpNodeKind::Part(part(quad(6.0, 6.0))),
        ),
        at(Some(1), "Origin", [-4.0, 1.0], ClpNodeKind::Group),
        at(
            Some(3),
            "Deep",
            [1.0, -1.0],
            ClpNodeKind::Part(part(quad(2.0, 2.0))),
        ),
    ];
    let params = vec![ClpParam {
        name: "Warp".into(),
        is_vec2: false,
        min: [-1.0, 0.0],
        max: [1.0, 1.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 0.5, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![
            ClpBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(cells(vec![
                    (0, 0, vec![-5.0, 0.0, 3.0, 1.0, 0.0, -6.0, 2.0, 2.0]),
                    (2, 0, vec![4.0, 4.0, -2.0, 0.0, 1.0, 5.0, -3.0, 1.0]),
                ])),
            },
            ClpBinding {
                node: 2,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(cells(vec![(
                    2,
                    0,
                    vec![1.0, 1.0, -1.0, 0.0, 0.5, 0.5, 0.0, -1.0],
                )])),
            },
        ],
    }];
    file(nodes, params, Vec::new())
}

#[test]
fn mesh_groups_propagate_the_same() {
    for dynamic in [false, true] {
        for tc in [false, true] {
            check(
                &format!("mesh group dynamic={dynamic} tc={tc}"),
                &mesh_group_rig(dynamic, tc),
            );
        }
    }
}

/// Two parts welded seam to seam, each with its own deform binding, so the
/// weld pass has something to pull together.
#[test]
fn welds_solve_the_same() {
    let nodes = vec![
        node(None, "Root", ClpNodeKind::Group),
        at(
            Some(0),
            "A",
            [-4.0, 0.0],
            ClpNodeKind::Part(part(quad(4.0, 4.0))),
        ),
        at(
            Some(0),
            "B",
            [4.0, 0.0],
            ClpNodeKind::Part(part(quad(4.0, 4.0))),
        ),
    ];
    let params = vec![ClpParam {
        name: "Pull".into(),
        is_vec2: false,
        min: [0.0, 0.0],
        max: [1.0, 1.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![
            ClpBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![0.0, 0.0, 2.0, 1.0, 2.0, -1.0, 0.0, 0.0],
                )])),
            },
            ClpBinding {
                node: 2,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![-3.0, 0.5, 0.0, 0.0, 0.0, 0.0, -3.0, -0.5],
                )])),
            },
        ],
    }];
    let welds = vec![ClpWeld {
        a: 1,
        b: 2,
        pairs: vec![
            ClpWeldPair {
                a_vert: 1,
                b_vert: 0,
                weight: 0.5,
            },
            ClpWeldPair {
                a_vert: 2,
                b_vert: 3,
                weight: 0.25,
            },
        ],
    }];
    check("welds", &file(nodes, params, welds));
}

fn physics(target: Option<u32>, local_only: bool) -> ClpSimplePhysics {
    ClpSimplePhysics {
        kind: PendulumKind::RigidPendulum,
        map_mode: PhysicsParamMapMode::AngleLength,
        local_only,
        target_param: target,
        gravity: 1.0,
        length: 60.0,
        frequency: 1.0,
        angle_damping: 0.2,
        length_damping: 0.5,
        output_scale: [1.0, 1.0],
    }
}

/// Two drivers, the first's output translating the second's anchor: the
/// chained-physics case the contribution table exists for. The first writes a
/// 2-D param (two scalar params on the new side), the second a 1-D one.
#[test]
fn chained_physics_drivers_agree_frame_by_frame() {
    let nodes = vec![
        node(None, "Root", ClpNodeKind::Group),
        at(
            Some(0),
            "Upper",
            [0.0, 40.0],
            ClpNodeKind::SimplePhysics(physics(Some(0), false)),
        ),
        at(Some(0), "Mid", [0.0, 10.0], ClpNodeKind::Group),
        at(
            Some(2),
            "Lower",
            [5.0, -20.0],
            ClpNodeKind::SimplePhysics(physics(Some(1), true)),
        ),
        at(
            Some(0),
            "Hair",
            [0.0, 0.0],
            ClpNodeKind::Part(part(quad(6.0, 20.0))),
        ),
    ];
    let params = vec![
        ClpParam {
            name: "Swing".into(),
            is_vec2: true,
            min: [-1.0, 0.0],
            max: [1.0, 2.0],
            defaults: [0.0, 1.0],
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![
                // The upper driver's output moves the lower driver's anchor.
                ClpBinding {
                    node: 2,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClpBindingValues::TransformTX(cells(vec![(0, 0, -30.0), (2, 1, 30.0)])),
                },
                ClpBinding {
                    node: 4,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClpBindingValues::Deform(cells(vec![
                        (0, 0, vec![-4.0, 0.0, 4.0, 0.0, 0.0, 2.0, 0.0, -2.0]),
                        (2, 1, vec![4.0, 1.0, -4.0, 1.0, 0.0, -2.0, 0.0, 2.0]),
                    ])),
                },
                // An output-scale binding, so the pre-pass's second target
                // kind is exercised too.
                ClpBinding {
                    node: 3,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClpBindingValues::OutputScaleX(cells(vec![(0, 0, 0.5)])),
                },
            ],
        },
        ClpParam {
            name: "Tip".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![ClpBinding {
                node: 4,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::TransformTY(cells(vec![(1, 0, 12.0)])),
            }],
        },
    ];
    check("chained physics", &file(nodes, params, Vec::new()));
}

/// A mesh group whose `translate_children` filter targets a `local_only`
/// driver — the case that forces the anchor pre-pass every frame.
#[test]
fn a_translate_children_mesh_group_over_a_local_driver_agrees() {
    let nodes = vec![
        node(None, "Root", ClpNodeKind::Group),
        node(
            Some(0),
            "MG",
            ClpNodeKind::MeshGroup(ClpMeshGroup {
                mesh: quad(30.0, 30.0),
                dynamic: false,
                translate_children: true,
            }),
        ),
        at(
            Some(1),
            "Driver",
            [3.0, 6.0],
            ClpNodeKind::SimplePhysics(physics(Some(1), true)),
        ),
        at(
            Some(1),
            "Skin",
            [0.0, 0.0],
            ClpNodeKind::Part(part(quad(8.0, 8.0))),
        ),
    ];
    let params = vec![
        ClpParam {
            name: "Warp".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![ClpBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![6.0, -2.0, 6.0, -2.0, 6.0, -2.0, 6.0, -2.0],
                )])),
            }],
        },
        ClpParam {
            name: "Sway".into(),
            is_vec2: true,
            min: [-1.0, 0.0],
            max: [1.0, 2.0],
            defaults: [0.0, 1.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![ClpBinding {
                node: 3,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::TransformTX(cells(vec![(1, 1, 7.0)])),
            }],
        },
    ];
    check("tc over local driver", &file(nodes, params, Vec::new()));
}

// ---------------------------------------------------------------------------
// The generation gate
// ---------------------------------------------------------------------------

/// A model edited between two ticks must reach the puppet on the next one,
/// and the pose the caller set must survive the rebake.
#[test]
fn a_model_edit_between_ticks_rebakes_and_keeps_the_pose() {
    let f = two_param_rig(InterpolateMode::Linear);
    let mut model = Model::from_clp_file(&f).unwrap();
    let mut puppet = Puppet::new(&model);
    let (px, py) = (
        ParamId::new("param-0.x").unwrap(),
        ParamId::new("param-0.y").unwrap(),
    );
    puppet.set_param_value(&px, 0.6);
    puppet.set_param_value(&py, -1.0);
    puppet.tick(&model, DT);

    let body = NodeId::new("node-1").unwrap();
    let idx = puppet.node_idx(&body).unwrap();
    let before = puppet.transforms().get(idx);
    let deform_before = puppet.combined_deform(idx).unwrap().to_vec();
    let baked = puppet.baked_generation();

    model
        .update_node(&body, |n| n.transform.translation[1] += 25.0)
        .unwrap();
    assert_ne!(model.generation(), baked, "the edit bumped the generation");

    puppet.tick(&model, DT);
    assert_eq!(puppet.baked_generation(), model.generation(), "rebaked");
    let idx = puppet.node_idx(&body).unwrap();
    let after = puppet.transforms().get(idx);
    assert!(
        (after.w_axis.y - before.w_axis.y - 25.0).abs() < TOL,
        "the moved node moved: {} -> {}",
        before.w_axis.y,
        after.w_axis.y
    );
    assert_eq!(
        puppet.param_value(&px),
        Some(0.6),
        "the pose survived the rebake"
    );
    assert_eq!(puppet.param_value(&py), Some(-1.0));
    let deform_after = puppet.combined_deform(idx).unwrap();
    assert_eq!(
        deform_before.len(),
        deform_after.len(),
        "the deform is still folded at the same pose"
    );
    for (a, b) in deform_before.iter().zip(deform_after.iter()) {
        assert!(
            (a.x - b.x).abs() <= TOL && (a.y - b.y).abs() <= TOL,
            "a transform edit must not change the folded deform: {a:?} vs {b:?}"
        );
    }
}

/// A driver's state has to survive a rebake too, or an edit would visibly
/// restart every pendulum.
#[test]
fn a_model_edit_keeps_the_drivers_running() {
    let f = {
        let nodes = vec![
            node(None, "Root", ClpNodeKind::Group),
            at(
                Some(0),
                "Driver",
                [0.0, 40.0],
                ClpNodeKind::SimplePhysics(physics(Some(0), false)),
            ),
            at(
                Some(0),
                "Hair",
                [0.0, 0.0],
                ClpNodeKind::Part(part(quad(6.0, 20.0))),
            ),
        ];
        let params = vec![ClpParam {
            name: "Swing".into(),
            is_vec2: true,
            min: [-1.0, 0.0],
            max: [1.0, 2.0],
            defaults: [0.0, 1.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![ClpBinding {
                node: 2,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::TransformTX(cells(vec![(1, 1, 20.0)])),
            }],
        }];
        file(nodes, params, Vec::new())
    };
    let mut model = Model::from_clp_file(&f).unwrap();
    let mut puppet = Puppet::new(&model);
    puppet.settle_physics(&model);
    // Kick the pendulum away from rest and let it swing.
    let driver = NodeId::new("node-1").unwrap();
    model
        .update_node(&driver, |n| n.transform.translation[0] = 60.0)
        .unwrap();
    for _ in 0..10 {
        puppet.tick(&model, DT);
    }
    let swinging = puppet.param_value(&ParamId::new("param-0.x").unwrap());

    // An unrelated edit: the driver must keep its bob, not snap back to rest.
    model
        .update_node(&NodeId::new("node-2").unwrap(), |n| n.z_order += 1.0)
        .unwrap();
    puppet.tick(&model, DT);
    let after = puppet.param_value(&ParamId::new("param-0.x").unwrap());
    let (a, b) = (swinging.unwrap(), after.unwrap());
    assert!(
        (a - b).abs() < 0.05,
        "the driver kept swinging across the rebake: {a} -> {b}"
    );
}
