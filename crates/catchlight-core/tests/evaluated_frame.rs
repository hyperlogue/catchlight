#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! What a tick evaluates, pinned against a committed baseline.
//!
//! Each rig here is a synthetic model exercising one part of the pipeline the
//! committed `.clm` fixtures do not reach: mesh groups in every
//! `dynamic` x `translate_children` combination, two-param bindings in all
//! four interpolation modes, two params deforming one node, welds, chained
//! physics drivers, a `translate_children` group over a `local_only` driver,
//! and a playing animation. For a grid of poses (plus, where a rig has
//! drivers, a settle and a run of simulated frames) the whole evaluated frame
//! — every node's global transform, z order, enabled flag, colour and
//! combined deform — is compared against
//! `tests/fixtures/evaluated_frame.json`.
//!
//! The combined deform is what proves the passes downstream of the fold: a
//! mesh group's attachments, the `translate_children` filter and the weld
//! solve all land in a node's deform stack, so a difference in any of them
//! shows up here as a per-vertex difference. Nothing else observes them.
//!
//! **Where the numbers came from.** They are the output of the runtime this
//! suite used to compare against the one it replaced, frame for frame, at the
//! same `1e-5`. That comparison retired with the old runtime; these are the
//! values it had proven equal, captured.
//!
//! Regenerate after an intentional behaviour change:
//!   UPDATE_FRAME_BASELINE=1 cargo test -p catchlight-core --test evaluated_frame

use catchlight_core::animation::{Animation, Lane};
use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clm::{
    ClmBindingValues, ClmCell, ClmCells, ClmIndices, ClmMesh, ClmPhysics, ClmTransform,
};
use catchlight_core::formats::legacy::{
    LegacyBinding, LegacyComposite, LegacyDocument, LegacyFile, LegacyMask, LegacyMeshGroup,
    LegacyNode, LegacyNodeKind, LegacyParam, LegacyPart, LegacySimplePhysics, LegacyWeld,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::model::ModelWeldPair;
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::puppet::Puppet;
use catchlight_core::{Keyframe, Model, NodeId, NodeIdx, NodeKind, ParamId, Vec2};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Absolute tolerance on every compared number, the one the retired
/// two-runtime comparison used.
const TOL: f32 = 1e-5;

const DT: f32 = 1.0 / 60.0;

/// Frames between samples of a simulated run. The drivers move slowly enough
/// at 60 Hz that every tenth frame still pins the curve.
const SAMPLE_EVERY: usize = 10;

/// One evaluated frame, flattened: per node, the global transform's sixteen
/// elements, its z order and enabled flag, then its colour if it has one,
/// then its combined deform if it has one.
type Frame = Vec<Vec<f32>>;

/// Every label this suite captures, in name order.
type Baseline = BTreeMap<String, Frame>;

// ---------------------------------------------------------------------------
// Driving one rig
// ---------------------------------------------------------------------------

struct Rig {
    file: LegacyFile,
    model: Model,
    puppet: Puppet,
    /// Arena index of each `.clm` node, in document order.
    nodes: Vec<NodeIdx>,
}

impl Rig {
    fn load(file: LegacyFile) -> Rig {
        let model = Model::from_legacy(&file).expect("model build");
        let puppet = Puppet::new(&model);
        let nodes = (0..file.doc.nodes.len())
            .map(|i| {
                let id = if i == 0 {
                    NodeId::new("root").unwrap()
                } else {
                    NodeId::new(format!("node-{i}")).unwrap()
                };
                puppet.node_idx(&id).expect("baked node")
            })
            .collect();
        Rig {
            file,
            model,
            puppet,
            nodes,
        }
    }

    fn pose(&mut self, values: &[(f32, f32)]) {
        for (j, p) in self.file.doc.params.iter().enumerate() {
            let (x, y) = values[j];
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

    fn tick(&mut self) {
        self.puppet.tick(&self.model, DT);
    }

    /// Displace every driver's pendulum so the run that follows is a swing
    /// rather than a fixed point.
    fn kick_drivers(&mut self) {
        for (i, node) in self.file.doc.nodes.iter().enumerate() {
            if matches!(node.kind, LegacyNodeKind::SimplePhysics(_)) {
                assert!(
                    self.puppet
                        .place_driver(self.nodes[i], Vec2::new(40.0, 40.0)),
                    "node {i} is a driver"
                );
            }
        }
    }

    fn has_drivers(&self) -> bool {
        self.file
            .doc
            .nodes
            .iter()
            .any(|n| matches!(n.kind, LegacyNodeKind::SimplePhysics(_)))
    }

    /// The evaluated frame, flattened for the baseline.
    fn frame(&self) -> Frame {
        self.nodes
            .iter()
            .map(|&idx| {
                let t = self.puppet.transforms().get(idx);
                let node = self.puppet.get(idx).expect("node");
                let mut row: Vec<f32> = t.to_cols_array().to_vec();
                row.push(node.z_order);
                row.push(if node.enabled { 1.0 } else { 0.0 });
                if let Some((opacity, tint, screen)) = colour(&node.kind) {
                    row.push(opacity);
                    row.extend(tint);
                    row.extend(screen);
                }
                if let Some(deform) = self.puppet.combined_deform(idx) {
                    row.extend(deform.iter().flat_map(|v| [v.x, v.y]));
                }
                row
            })
            .collect()
    }
}

fn colour(kind: &NodeKind) -> Option<(f32, [f32; 3], [f32; 3])> {
    match kind {
        NodeKind::Part(p) => Some((p.opacity, p.tint.to_array(), p.screen_tint.to_array())),
        NodeKind::Composite(c) => Some((c.opacity, c.tint.to_array(), c.screen_tint.to_array())),
        _ => None,
    }
}

/// Poses to drive a rig through: everything at rest, everything at each
/// extreme and at the middle, then each param swept on its own so a binding
/// that only one param reaches is still exercised.
fn pose_grid(file: &LegacyFile) -> Vec<Vec<(f32, f32)>> {
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
        for &t in &[0.0f32, 0.5, 1.0] {
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

/// Run one rig's whole schedule into `out`.
///
/// A rig with no drivers is ticked twice per pose and the second frame has to
/// equal the first — that is the memo that lets an unchanged pose skip the
/// fold, and it is a property rather than a captured number. A rig with
/// drivers moves between the two ticks by design, so both frames are captured
/// instead, and a settle plus a simulated run follows.
fn capture(label: &str, file: LegacyFile, out: &mut Baseline) {
    let mut rig = Rig::load(file);
    let drivers = rig.has_drivers();

    for (k, pose) in pose_grid(&rig.file).into_iter().enumerate() {
        rig.pose(&pose);
        rig.tick();
        let first = rig.frame();
        out.insert(format!("{label} pose {k}"), first.clone());
        rig.tick();
        if drivers {
            out.insert(format!("{label} pose {k} (second tick)"), rig.frame());
        } else {
            compare(
                &format!("{label} pose {k}: an unchanged pose re-evaluates the same"),
                &first,
                &rig.frame(),
            );
        }
    }

    if drivers {
        let mut rig = Rig::load(rig.file);
        rig.puppet.settle_physics(&rig.model);
        rig.tick();
        let settled = rig.frame();
        out.insert(format!("{label} settled"), settled.clone());

        // Settling leaves the pendulums at the fixed point of the rest pose,
        // where they would sit still forever — and posing cannot move them,
        // because a driver claims its target params at full authority and
        // overwrites whatever was posed. Displacing each bob is what makes
        // the frames below a swing decaying back to rest, which is the
        // transient nothing else in the suite pins.
        rig.kick_drivers();
        for frame in 0..90 {
            rig.tick();
            if frame % SAMPLE_EVERY == 0 {
                out.insert(format!("{label} frame {frame}"), rig.frame());
            }
        }
        assert_ne!(
            out[&format!("{label} frame 0")],
            out[&format!("{label} frame 80")],
            "{label}: the simulated run has to actually move"
        );
    }
}

fn compare(where_: &str, expected: &Frame, got: &Frame) {
    assert_eq!(expected.len(), got.len(), "{where_}: node count");
    for (i, (e, g)) in expected.iter().zip(got).enumerate() {
        assert_eq!(e.len(), g.len(), "{where_}: node {i}: value count");
        for (k, (a, b)) in e.iter().zip(g).enumerate() {
            assert!(
                (a - b).abs() <= TOL,
                "{where_}: node {i} value {k}: expected {a} got {b}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The whole matrix, against the committed baseline
// ---------------------------------------------------------------------------

fn current() -> Baseline {
    let mut out = Baseline::new();
    for mode in [
        InterpolateMode::Linear,
        InterpolateMode::Nearest,
        InterpolateMode::Stepped,
        InterpolateMode::Cubic,
    ] {
        capture(
            &format!("two_param {mode:?}"),
            two_param_rig(mode),
            &mut out,
        );
    }
    capture(
        "two params one node",
        two_params_deforming_one_node(),
        &mut out,
    );
    for dynamic in [false, true] {
        for tc in [false, true] {
            capture(
                &format!("mesh group dynamic={dynamic} tc={tc}"),
                mesh_group_rig(dynamic, tc),
                &mut out,
            );
        }
    }
    capture("welds", weld_rig(), &mut out);
    capture("chained physics", chained_physics_rig(), &mut out);
    capture("tc over local driver", tc_over_local_driver_rig(), &mut out);
    animation_frames(&mut out);
    out
}

/// A clip the puppet plays, sampled over 120 frames: the loop region, the
/// lead-in and the keyframe interpolator, all reaching bindings.
fn animation_frames(out: &mut Baseline) {
    let mut rig = Rig::load(animation_rig());
    rig.puppet.set_animations(vec![Animation {
        name: "Blink".into(),
        timestep: 1.0 / 60.0,
        length: 31,
        lead_in: 6,
        lead_out: 28,
        lanes: vec![Lane {
            param: ParamId::new("param-0").unwrap(),
            keyframes: vec![
                Keyframe {
                    frame: 0,
                    value: 0.0,
                },
                Keyframe {
                    frame: 12,
                    value: 1.0,
                },
                Keyframe {
                    frame: 30,
                    value: 0.25,
                },
            ],
            interpolation: InterpolateMode::Linear,
        }],
    }]);
    assert!(rig.puppet.play_animation("Blink"));
    assert_eq!(rig.puppet.playing_animation(), Some("Blink"));

    let mut moved = false;
    for frame in 0..120 {
        rig.tick();
        if frame % SAMPLE_EVERY == 0 {
            out.insert(format!("animation frame {frame}"), rig.frame());
        }
        if rig.puppet.transforms().get(rig.nodes[1]).w_axis.y.abs() > 1e-3 {
            moved = true;
        }
    }
    assert!(moved, "the lane actually drove the binding");

    rig.puppet.stop_animation();
    assert!(!rig.puppet.has_playing_animation());
    rig.tick();
    out.insert("animation stopped".into(), rig.frame());
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/evaluated_frame.json")
}

#[test]
fn every_rig_evaluates_the_frame_it_always_has() -> Result<(), Box<dyn std::error::Error>> {
    let current = current();
    let path = baseline_path();

    if std::env::var_os("UPDATE_FRAME_BASELINE").is_some() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string(&current)?)?;
        eprintln!("updated frame baseline at {}", path.display());
        return Ok(());
    }

    let raw = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "missing frame baseline {} ({e}); regenerate with \
             UPDATE_FRAME_BASELINE=1 cargo test -p catchlight-core --test evaluated_frame",
            path.display()
        )
    })?;
    let expected: Baseline = serde_json::from_str(&raw)?;

    let missing: Vec<&String> = current
        .keys()
        .filter(|k| !expected.contains_key(*k))
        .collect();
    let extra: Vec<&String> = expected
        .keys()
        .filter(|k| !current.contains_key(*k))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "the label set moved; regenerate the baseline. missing {missing:?}, stale {extra:?}"
    );
    for (label, got) in &current {
        compare(label, &expected[label], got);
    }
    Ok(())
}
fn transform() -> ClmTransform {
    ClmTransform {
        translation: [0.0; 3],
        rotation: [0.0; 3],
        scale: [1.0, 1.0],
    }
}

fn quad(w: f32, h: f32) -> ClmMesh {
    ClmMesh {
        verts: vec![-w, -h, w, -h, w, h, -w, h],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 2, 3, 0]),
        origin: [0.0, 0.0],
    }
}

fn part(mesh: ClmMesh) -> LegacyPart {
    LegacyPart {
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

fn node(parent: Option<u32>, name: &str, kind: LegacyNodeKind) -> LegacyNode {
    LegacyNode {
        parent,
        name: name.into(),
        enabled: true,
        z_order: 0.0,
        transform: transform(),
        lock_to_root: false,
        kind,
    }
}

fn at(parent: Option<u32>, name: &str, xy: [f32; 2], kind: LegacyNodeKind) -> LegacyNode {
    let mut n = node(parent, name, kind);
    n.transform.translation = [xy[0], xy[1], 0.0];
    n
}

fn cells<T>(entries: Vec<(u32, u32, T)>) -> ClmCells<T> {
    ClmCells {
        cells: entries
            .into_iter()
            .map(|(x, y, value)| ClmCell { x, y, value })
            .collect(),
    }
}

fn file(nodes: Vec<LegacyNode>, params: Vec<LegacyParam>, welds: Vec<LegacyWeld>) -> LegacyFile {
    LegacyFile {
        doc: LegacyDocument {
            physics: ClmPhysics::default(),
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
fn two_param_rig(mode: InterpolateMode) -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        at(
            Some(0),
            "Body",
            [10.0, -5.0],
            LegacyNodeKind::Part(part(quad(8.0, 6.0))),
        ),
        node(
            Some(0),
            "Layered",
            LegacyNodeKind::Composite(LegacyComposite {
                opacity: 0.9,
                blend_mode: BlendMode::Normal,
                tint: [1.0, 0.8, 0.7],
                screen_tint: [0.0; 3],
                masks: vec![LegacyMask {
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
            LegacyNodeKind::Part(part(quad(3.0, 3.0))),
        ),
    ];
    let d = |x: u32, y: u32, v: Vec<f32>| (x, y, v);
    let params = vec![LegacyParam {
        name: "Head".into(),
        is_vec2: true,
        min: [-1.0, -2.0],
        max: [1.0, 2.0],
        defaults: [0.0, 0.5],
        axis_points_x: vec![0.0, 0.25, 1.0],
        axis_points_y: vec![0.0, 0.5, 1.0],
        bindings: vec![
            LegacyBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClmBindingValues::Deform(cells(vec![
                    d(0, 0, vec![-2.0, 0.5, 0.0, 0.0, 1.0, -1.0, 0.0, 3.0]),
                    d(2, 0, vec![4.0, 0.0, -1.0, 2.0, 0.0, 0.0, 0.5, 0.5]),
                    d(1, 2, vec![0.0, -3.0, 2.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
                    d(2, 2, vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
                ])),
            },
            LegacyBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClmBindingValues::TransformTX(cells(vec![(0, 0, -6.0), (2, 2, 9.0)])),
            },
            LegacyBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClmBindingValues::TransformRZ(cells(vec![(2, 0, 0.7)])),
            },
            LegacyBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClmBindingValues::TransformSY(cells(vec![(0, 2, 1.8)])),
            },
            LegacyBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClmBindingValues::ZOrder(cells(vec![(0, 0, -4.0), (2, 2, 6.0)])),
            },
            LegacyBinding {
                node: 1,
                interpolate_mode: mode,
                values: ClmBindingValues::Opacity(cells(vec![(0, 0, 0.25), (2, 2, 1.0)])),
            },
            LegacyBinding {
                node: 3,
                interpolate_mode: mode,
                values: ClmBindingValues::TintG(cells(vec![(0, 0, 0.2), (2, 2, 1.0)])),
            },
            LegacyBinding {
                node: 3,
                interpolate_mode: mode,
                values: ClmBindingValues::ScreenTintB(cells(vec![(1, 1, 0.6)])),
            },
            LegacyBinding {
                node: 2,
                interpolate_mode: mode,
                values: ClmBindingValues::Opacity(cells(vec![(0, 2, 0.3)])),
            },
        ],
    }];
    file(nodes, params, Vec::new())
}

/// Two one-param bindings on one node, from different params — the case the
/// legacy runtime gives one deform slot per param and the new one gives one
/// per binding.
fn two_params_deforming_one_node() -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        node(Some(0), "Body", LegacyNodeKind::Part(part(quad(5.0, 5.0)))),
    ];
    let binding = |v: Vec<f32>| LegacyBinding {
        node: 1,
        interpolate_mode: InterpolateMode::Linear,
        values: ClmBindingValues::Deform(cells(vec![(1, 0, v)])),
    };
    let param = |name: &str, values: Vec<f32>| LegacyParam {
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
    file(nodes, params, Vec::new())
}

/// A mesh group over two parts, keyed by a param: the descent, the attachment
/// bake and the `translate_children` filter all have to land the same.
fn mesh_group_rig(dynamic: bool, translate_children: bool) -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        node(
            Some(0),
            "MG",
            LegacyNodeKind::MeshGroup(LegacyMeshGroup {
                mesh: quad(20.0, 20.0),
                dynamic,
                translate_children,
            }),
        ),
        at(
            Some(1),
            "Under",
            [2.0, 3.0],
            LegacyNodeKind::Part(part(quad(6.0, 6.0))),
        ),
        at(Some(1), "Origin", [-4.0, 1.0], LegacyNodeKind::Group),
        at(
            Some(3),
            "Deep",
            [1.0, -1.0],
            LegacyNodeKind::Part(part(quad(2.0, 2.0))),
        ),
    ];
    let params = vec![LegacyParam {
        name: "Warp".into(),
        is_vec2: false,
        min: [-1.0, 0.0],
        max: [1.0, 1.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 0.5, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![
            LegacyBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells(vec![
                    (0, 0, vec![-5.0, 0.0, 3.0, 1.0, 0.0, -6.0, 2.0, 2.0]),
                    (2, 0, vec![4.0, 4.0, -2.0, 0.0, 1.0, 5.0, -3.0, 1.0]),
                ])),
            },
            LegacyBinding {
                node: 2,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells(vec![(
                    2,
                    0,
                    vec![1.0, 1.0, -1.0, 0.0, 0.5, 0.5, 0.0, -1.0],
                )])),
            },
        ],
    }];
    file(nodes, params, Vec::new())
}

/// Two parts welded seam to seam, each with its own deform binding, so the
/// weld pass has something to pull together.
fn weld_rig() -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        at(
            Some(0),
            "A",
            [-4.0, 0.0],
            LegacyNodeKind::Part(part(quad(4.0, 4.0))),
        ),
        at(
            Some(0),
            "B",
            [4.0, 0.0],
            LegacyNodeKind::Part(part(quad(4.0, 4.0))),
        ),
    ];
    let params = vec![LegacyParam {
        name: "Pull".into(),
        is_vec2: false,
        min: [0.0, 0.0],
        max: [1.0, 1.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![
            LegacyBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![0.0, 0.0, 2.0, 1.0, 2.0, -1.0, 0.0, 0.0],
                )])),
            },
            LegacyBinding {
                node: 2,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![-3.0, 0.5, 0.0, 0.0, 0.0, 0.0, -3.0, -0.5],
                )])),
            },
        ],
    }];
    let welds = vec![LegacyWeld {
        a: 1,
        b: 2,
        pairs: vec![
            ModelWeldPair {
                a_vert: 1,
                b_vert: 0,
                weight: 0.5,
            },
            ModelWeldPair {
                a_vert: 2,
                b_vert: 3,
                weight: 0.25,
            },
        ],
    }];
    file(nodes, params, welds)
}

fn physics(target: Option<u32>, local_only: bool) -> LegacySimplePhysics {
    LegacySimplePhysics {
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
fn chained_physics_rig() -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        at(
            Some(0),
            "Upper",
            [0.0, 40.0],
            LegacyNodeKind::SimplePhysics(physics(Some(0), false)),
        ),
        at(Some(0), "Mid", [0.0, 10.0], LegacyNodeKind::Group),
        at(
            Some(2),
            "Lower",
            [5.0, -20.0],
            LegacyNodeKind::SimplePhysics(physics(Some(1), true)),
        ),
        at(
            Some(0),
            "Hair",
            [0.0, 0.0],
            LegacyNodeKind::Part(part(quad(6.0, 20.0))),
        ),
    ];
    let params = vec![
        LegacyParam {
            name: "Swing".into(),
            is_vec2: true,
            min: [-1.0, 0.0],
            max: [1.0, 2.0],
            defaults: [0.0, 1.0],
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![
                // The upper driver's output moves the lower driver's anchor.
                LegacyBinding {
                    node: 2,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClmBindingValues::TransformTX(cells(vec![(0, 0, -30.0), (2, 1, 30.0)])),
                },
                LegacyBinding {
                    node: 4,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClmBindingValues::Deform(cells(vec![
                        (0, 0, vec![-4.0, 0.0, 4.0, 0.0, 0.0, 2.0, 0.0, -2.0]),
                        (2, 1, vec![4.0, 1.0, -4.0, 1.0, 0.0, -2.0, 0.0, 2.0]),
                    ])),
                },
                // An output-scale binding, so the pre-pass's second target
                // kind is exercised too.
                LegacyBinding {
                    node: 3,
                    interpolate_mode: InterpolateMode::Linear,
                    values: ClmBindingValues::OutputScaleX(cells(vec![(0, 0, 0.5)])),
                },
            ],
        },
        LegacyParam {
            name: "Tip".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![LegacyBinding {
                node: 4,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::TransformTY(cells(vec![(1, 0, 12.0)])),
            }],
        },
    ];
    file(nodes, params, Vec::new())
}

/// A mesh group whose `translate_children` filter targets a `local_only`
/// driver — the case that forces the anchor pre-pass every frame.
fn tc_over_local_driver_rig() -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        node(
            Some(0),
            "MG",
            LegacyNodeKind::MeshGroup(LegacyMeshGroup {
                mesh: quad(30.0, 30.0),
                dynamic: false,
                translate_children: true,
            }),
        ),
        at(
            Some(1),
            "Driver",
            [3.0, 6.0],
            LegacyNodeKind::SimplePhysics(physics(Some(1), true)),
        ),
        at(
            Some(1),
            "Skin",
            [0.0, 0.0],
            LegacyNodeKind::Part(part(quad(8.0, 8.0))),
        ),
    ];
    let params = vec![
        LegacyParam {
            name: "Warp".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 1.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: vec![LegacyBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![6.0, -2.0, 6.0, -2.0, 6.0, -2.0, 6.0, -2.0],
                )])),
            }],
        },
        LegacyParam {
            name: "Sway".into(),
            is_vec2: true,
            min: [-1.0, 0.0],
            max: [1.0, 2.0],
            defaults: [0.0, 1.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![LegacyBinding {
                node: 3,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::TransformTX(cells(vec![(0, 0, -7.0), (1, 1, 7.0)])),
            }],
        },
    ];
    file(nodes, params, Vec::new())
}

// ---------------------------------------------------------------------------
// The generation gate
// ---------------------------------------------------------------------------

/// A model edited between two ticks must reach the puppet on the next one,
/// and the pose the caller set must survive the rebake.
#[test]
fn a_model_edit_between_ticks_rebakes_and_keeps_the_pose() {
    let f = two_param_rig(InterpolateMode::Linear);
    let mut model = Model::from_legacy(&f).unwrap();
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
            node(None, "Root", LegacyNodeKind::Group),
            at(
                Some(0),
                "Driver",
                [0.0, 40.0],
                LegacyNodeKind::SimplePhysics(physics(Some(0), false)),
            ),
            at(
                Some(0),
                "Hair",
                [0.0, 0.0],
                LegacyNodeKind::Part(part(quad(6.0, 20.0))),
            ),
        ];
        let params = vec![LegacyParam {
            name: "Swing".into(),
            is_vec2: true,
            min: [-1.0, 0.0],
            max: [1.0, 2.0],
            defaults: [0.0, 1.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0, 1.0],
            bindings: vec![LegacyBinding {
                node: 2,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::TransformTX(cells(vec![(1, 1, 20.0)])),
            }],
        }];
        file(nodes, params, Vec::new())
    };
    let mut model = Model::from_legacy(&f).unwrap();
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

/// A part whose transform and deform a single param drives, for a clip's lane
/// to reach.
fn animation_rig() -> LegacyFile {
    let nodes = vec![
        node(None, "Root", LegacyNodeKind::Group),
        node(Some(0), "Body", LegacyNodeKind::Part(part(quad(5.0, 5.0)))),
    ];
    let params = vec![LegacyParam {
        name: "Blink".into(),
        is_vec2: false,
        min: [0.0, 0.0],
        max: [1.0, 1.0],
        defaults: [0.0, 0.0],
        axis_points_x: vec![0.0, 0.5, 1.0],
        axis_points_y: vec![0.0],
        bindings: vec![
            LegacyBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::TransformTY(cells(vec![(0, 0, -3.0), (2, 0, 9.0)])),
            },
            LegacyBinding {
                node: 1,
                interpolate_mode: InterpolateMode::Linear,
                values: ClmBindingValues::Deform(cells(vec![(
                    2,
                    0,
                    vec![1.0, 2.0, -1.0, 0.0, 0.0, 3.0, 0.5, -0.5],
                )])),
            },
        ],
    }];
    file(nodes, params, Vec::new())
}
