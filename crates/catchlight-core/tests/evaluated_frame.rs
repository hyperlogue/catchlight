#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! What a tick evaluates, pinned against a committed baseline.
//!
//! Each fixture here is a synthetic model exercising one part of the pipeline the
//! committed `.clm` fixtures do not reach: mesh groups in every
//! `dynamic` x `translate_children` combination, two-param bindings in all
//! four interpolation modes, two params deforming one node, welds, chained
//! physics drivers, a `translate_children` group over a `local_only` driver,
//! and a playing animation. For a grid of poses (plus, where a fixture has
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

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clm::{
    ClmAnimation, ClmBinding, ClmBindingValues, ClmCell, ClmCells, ClmComposite, ClmDocument,
    ClmFile, ClmIndices, ClmKeyframe, ClmLane, ClmMask, ClmMesh, ClmMeshGroup, ClmNode,
    ClmNodeKind, ClmParam, ClmPart, ClmSimplePhysics, ClmSlot, ClmSlotPair, ClmTransform, ClmWeld,
};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::physics::{PendulumKind, PhysicsParamMapMode};
use catchlight_core::puppet::Puppet;
use catchlight_core::{Model, NodeId, NodeIdx, NodeKind, ParamId, SlotId, Vec2};
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
// Driving one fixture
// ---------------------------------------------------------------------------

struct Fixture {
    file: FixtureFile,
    model: Model,
    puppet: Puppet,
    /// Baked index of each document node, in document order.
    nodes: Vec<NodeIdx>,
}

impl Fixture {
    fn load(file: FixtureFile) -> Fixture {
        let model = Model::from_clm_file(&file.file).expect("model build");
        let puppet = Puppet::new(&model);
        let nodes = file
            .file
            .doc
            .nodes
            .iter()
            .map(|n| puppet.node_idx(&n.id).expect("baked node"))
            .collect();
        Fixture {
            file,
            model,
            puppet,
            nodes,
        }
    }

    fn pose(&mut self, values: &[(f32, f32)]) {
        for (slot, &(x, y)) in self.file.slots.iter().zip(values) {
            self.puppet.set_param_value(&slot.ids[0], x);
            if let Some(id) = slot.ids.get(1) {
                self.puppet.set_param_value(id, y);
            }
        }
    }

    fn tick(&mut self) {
        self.puppet.tick(&self.model, DT);
    }

    /// Displace every driver's pendulum so the run that follows is a swing
    /// rather than a fixed point.
    fn kick_drivers(&mut self) {
        for (i, node) in self.file.file.doc.nodes.iter().enumerate() {
            if matches!(node.kind, ClmNodeKind::SimplePhysics(_)) {
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
            .file
            .doc
            .nodes
            .iter()
            .any(|n| matches!(n.kind, ClmNodeKind::SimplePhysics(_)))
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

/// Poses to drive a fixture through: everything at rest, everything at each
/// extreme and at the middle, then each param swept on its own so a binding
/// that only one param reaches is still exercised.
fn pose_grid(file: &FixtureFile) -> Vec<Vec<(f32, f32)>> {
    let params = &file.slots;
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

/// Run one fixture's whole schedule into `out`.
///
/// A fixture with no drivers is ticked twice per pose and the second frame has to
/// equal the first — that is the memo that lets an unchanged pose skip the
/// fold, and it is a property rather than a captured number. A fixture with
/// drivers moves between the two ticks by design, so both frames are captured
/// instead, and a settle plus a simulated run follows.
fn capture(label: &str, file: FixtureFile, out: &mut Baseline) {
    let mut fixture = Fixture::load(file);
    let drivers = fixture.has_drivers();

    for (k, pose) in pose_grid(&fixture.file).into_iter().enumerate() {
        fixture.pose(&pose);
        fixture.tick();
        let first = fixture.frame();
        out.insert(format!("{label} pose {k}"), first.clone());
        fixture.tick();
        if drivers {
            out.insert(format!("{label} pose {k} (second tick)"), fixture.frame());
        } else {
            compare(
                &format!("{label} pose {k}: an unchanged pose re-evaluates the same"),
                &first,
                &fixture.frame(),
            );
        }
    }

    if drivers {
        let mut fixture = Fixture::load(fixture.file);
        fixture.puppet.settle_physics(&fixture.model);
        fixture.tick();
        let settled = fixture.frame();
        out.insert(format!("{label} settled"), settled.clone());

        // Settling leaves the pendulums at the fixed point of the rest pose,
        // where they would sit still forever — and posing cannot move them,
        // because a driver claims its target params at full authority and
        // overwrites whatever was posed. Displacing each bob is what makes
        // the frames below a swing decaying back to rest, which is the
        // transient nothing else in the suite pins.
        fixture.kick_drivers();
        for frame in 0..90 {
            fixture.tick();
            if frame % SAMPLE_EVERY == 0 {
                out.insert(format!("{label} frame {frame}"), fixture.frame());
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
            two_param_fixture(mode),
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
                mesh_group_fixture(dynamic, tc),
                &mut out,
            );
        }
    }
    capture("welds", weld_fixture(), &mut out);
    capture("chained physics", chained_physics_fixture(), &mut out);
    capture(
        "tc over local driver",
        tc_over_local_driver_fixture(),
        &mut out,
    );
    animation_frames(&mut out);
    out
}

/// A clip the puppet plays, sampled over 120 frames: the loop region, the
/// lead-in and the keyframe interpolator, all reaching bindings.
fn animation_frames(out: &mut Baseline) {
    let mut fixture = Fixture::load(animation_fixture());
    fixture.puppet.set_animations(vec![ClmAnimation {
        name: "Blink".into(),
        timestep: 1.0 / 60.0,
        length: 31,
        lead_in: 6,
        lead_out: 28,
        lanes: vec![ClmLane {
            param: ParamId::new("param-0").unwrap(),
            keyframes: vec![
                ClmKeyframe {
                    frame: 0,
                    value: 0.0,
                },
                ClmKeyframe {
                    frame: 12,
                    value: 1.0,
                },
                ClmKeyframe {
                    frame: 30,
                    value: 0.25,
                },
            ],
            interpolation: InterpolateMode::Linear,
        }],
    }]);
    assert!(fixture.puppet.play_animation("Blink"));
    assert_eq!(fixture.puppet.playing_animation(), Some("Blink"));

    let mut moved = false;
    for frame in 0..120 {
        fixture.tick();
        if frame % SAMPLE_EVERY == 0 {
            out.insert(format!("animation frame {frame}"), fixture.frame());
        }
        if fixture
            .puppet
            .transforms()
            .get(fixture.nodes[1])
            .w_axis
            .y
            .abs()
            > 1e-3
        {
            moved = true;
        }
    }
    assert!(moved, "the lane actually drove the binding");

    fixture.puppet.stop_animation();
    assert!(!fixture.puppet.has_playing_animation());
    fixture.tick();
    out.insert("animation stopped".into(), fixture.frame());
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/evaluated_frame.json")
}

#[test]
fn every_fixture_evaluates_the_frame_it_always_has() -> Result<(), Box<dyn std::error::Error>> {
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

// ---------------------------------------------------------------------------
// Authoring a fixture
// ---------------------------------------------------------------------------
//
// The fixtures author the `.clm` document directly and mint the Ids an import
// would: `root` / `node-<i>` by position, `param-<i>` per source param, or
// `param-<i>.x` / `param-<i>.y` for one the pose schedule drives on two axes.
// That second shape is why [`Slot`] exists — a model has only scalar params,
// but this suite's poses are defined per *source* param, and they have to stay
// the ones the committed baseline was captured at.

/// One source param: the model params it became, and the range the pose
/// schedule sweeps it over.
struct Slot {
    ids: Vec<ParamId>,
    min: [f32; 2],
    max: [f32; 2],
    defaults: [f32; 2],
}

/// A fixture: the document a [`Model`] is read from, and the slots that pose it.
struct FixtureFile {
    file: ClmFile,
    slots: Vec<Slot>,
}

/// One fixture param before it is split: `vec2` means two scalar params, `.x` and
/// `.y`, the way an import splits a 2-D inochi2d param.
struct FixtureParam {
    name: &'static str,
    vec2: bool,
    min: [f32; 2],
    max: [f32; 2],
    defaults: [f32; 2],
    keys_x: Vec<f32>,
    keys_y: Vec<f32>,
    bindings: Vec<ClmBinding>,
}

impl FixtureParam {
    fn scalar(
        name: &'static str,
        min: f32,
        max: f32,
        default: f32,
        keys: Vec<f32>,
    ) -> FixtureParam {
        FixtureParam {
            name,
            vec2: false,
            min: [min, 0.0],
            max: [max, 1.0],
            defaults: [default, 0.0],
            keys_x: keys,
            keys_y: vec![0.0],
            bindings: Vec::new(),
        }
    }

    fn pair(
        name: &'static str,
        min: [f32; 2],
        max: [f32; 2],
        defaults: [f32; 2],
        keys_x: Vec<f32>,
        keys_y: Vec<f32>,
    ) -> FixtureParam {
        FixtureParam {
            name,
            vec2: true,
            min,
            max,
            defaults,
            keys_x,
            keys_y,
            bindings: Vec::new(),
        }
    }

    fn driving(mut self, bindings: Vec<ClmBinding>) -> FixtureParam {
        self.bindings = bindings;
        self
    }
}

fn nid(i: usize) -> NodeId {
    NodeId::new(if i == 0 {
        "root".to_string()
    } else {
        format!("node-{i}")
    })
    .unwrap()
}

/// The one param a scalar slot became.
fn one(i: usize) -> Vec<ParamId> {
    vec![ParamId::new(format!("param-{i}")).unwrap()]
}

/// The two params a 2-D slot split into, `x` first.
fn two(i: usize) -> Vec<ParamId> {
    vec![
        ParamId::new(format!("param-{i}.x")).unwrap(),
        ParamId::new(format!("param-{i}.y")).unwrap(),
    ]
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

fn part(mesh: ClmMesh) -> ClmPart {
    ClmPart {
        mesh,
        albedo: None,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0; 3],
        screen_tint: [0.0; 3],
        masks: Vec::new(),
        mask_threshold: 0.5,
        slots: Vec::new(),
    }
}

/// A part whose slots `s0..` name the vertices `verts` — what a weld pairs.
fn welded_part(mesh: ClmMesh, verts: &[u32]) -> ClmPart {
    ClmPart {
        slots: verts
            .iter()
            .enumerate()
            .map(|(i, &vertex)| ClmSlot {
                id: SlotId::new(format!("s{i}")).unwrap(),
                vertex: Some(vertex),
            })
            .collect(),
        ..part(mesh)
    }
}

fn node(parent: Option<usize>, name: &str, kind: ClmNodeKind) -> ClmNode {
    ClmNode {
        // Overwritten by `file`, which numbers the nodes by position.
        id: nid(0),
        parent: parent.map(nid),
        name: name.into(),
        enabled: true,
        z_order: 0.0,
        transform: transform(),
        lock_to_root: false,
        kind,
    }
}

fn at(parent: Option<usize>, name: &str, xy: [f32; 2], kind: ClmNodeKind) -> ClmNode {
    let mut n = node(parent, name, kind);
    n.transform.translation = [xy[0], xy[1], 0.0];
    n
}

fn binding(
    node: usize,
    mode: InterpolateMode,
    params: Vec<ParamId>,
    values: ClmBindingValues,
) -> ClmBinding {
    ClmBinding {
        params,
        node: nid(node),
        interpolate_mode: mode,
        values,
    }
}

fn cells<T>(entries: Vec<(u32, u32, T)>) -> ClmCells<T> {
    ClmCells {
        cells: entries
            .into_iter()
            .map(|(x, y, value)| ClmCell { x, y, value })
            .collect(),
    }
}

/// Number the nodes by position, split every 2-D param into its `.x` / `.y`
/// pair, and lift the bindings out from under their params.
fn file(
    nodes: Vec<ClmNode>,
    fixture_params: Vec<FixtureParam>,
    welds: Vec<ClmWeld>,
) -> FixtureFile {
    let nodes: Vec<ClmNode> = nodes
        .into_iter()
        .enumerate()
        .map(|(i, mut n)| {
            n.id = nid(i);
            n
        })
        .collect();

    let mut params = Vec::new();
    let mut bindings = Vec::new();
    let mut slots = Vec::new();
    for (i, p) in fixture_params.into_iter().enumerate() {
        let ids = if p.vec2 { two(i) } else { one(i) };
        params.push(ClmParam {
            id: ids[0].clone(),
            name: if p.vec2 {
                format!("{}.x", p.name)
            } else {
                p.name.to_string()
            },
            min: p.min[0],
            max: p.max[0],
            default: p.defaults[0],
            key_positions: p.keys_x,
        });
        if p.vec2 {
            params.push(ClmParam {
                id: ids[1].clone(),
                name: format!("{}.y", p.name),
                min: p.min[1],
                max: p.max[1],
                default: p.defaults[1],
                key_positions: p.keys_y,
            });
        }
        bindings.extend(p.bindings);
        slots.push(Slot {
            ids,
            min: p.min,
            max: p.max,
            defaults: p.defaults,
        });
    }

    FixtureFile {
        file: ClmFile {
            doc: ClmDocument {
                nodes,
                params,
                bindings,
                welds,
                ..ClmDocument::default()
            },
            textures: Vec::new(),
        },
        slots,
    }
}

/// A part under a group, driven by one 2-D param whose deform binding is
/// authored at scattered cells of a 3x3 grid, plus scalar bindings on every
/// other target the runtime folds.
fn two_param_fixture(mode: InterpolateMode) -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        at(
            Some(0),
            "Body",
            [10.0, -5.0],
            ClmNodeKind::Part(part(quad(8.0, 6.0))),
        ),
        node(
            Some(0),
            "Layered",
            ClmNodeKind::Composite(ClmComposite {
                opacity: 0.9,
                blend_mode: BlendMode::Normal,
                tint: [1.0, 0.8, 0.7],
                screen_tint: [0.0; 3],
                masks: vec![ClmMask {
                    source: nid(1),
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
            ClmNodeKind::Part(part(quad(3.0, 3.0))),
        ),
    ];
    let d = |x: u32, y: u32, v: Vec<f32>| (x, y, v);
    let b = |node: usize, values: ClmBindingValues| binding(node, mode, two(0), values);
    let params = vec![FixtureParam::pair(
        "Head",
        [-1.0, -2.0],
        [1.0, 2.0],
        [0.0, 0.5],
        vec![0.0, 0.25, 1.0],
        vec![0.0, 0.5, 1.0],
    )
    .driving(vec![
        b(
            1,
            ClmBindingValues::Deform(cells(vec![
                d(0, 0, vec![-2.0, 0.5, 0.0, 0.0, 1.0, -1.0, 0.0, 3.0]),
                d(2, 0, vec![4.0, 0.0, -1.0, 2.0, 0.0, 0.0, 0.5, 0.5]),
                d(1, 2, vec![0.0, -3.0, 2.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
                d(2, 2, vec![1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
            ])),
        ),
        b(
            1,
            ClmBindingValues::TransformTX(cells(vec![(0, 0, -6.0), (2, 2, 9.0)])),
        ),
        b(1, ClmBindingValues::TransformRZ(cells(vec![(2, 0, 0.7)]))),
        b(1, ClmBindingValues::TransformSY(cells(vec![(0, 2, 1.8)]))),
        b(
            1,
            ClmBindingValues::ZOrder(cells(vec![(0, 0, -4.0), (2, 2, 6.0)])),
        ),
        b(
            1,
            ClmBindingValues::Opacity(cells(vec![(0, 0, 0.25), (2, 2, 1.0)])),
        ),
        b(
            3,
            ClmBindingValues::TintG(cells(vec![(0, 0, 0.2), (2, 2, 1.0)])),
        ),
        b(3, ClmBindingValues::ScreenTintB(cells(vec![(1, 1, 0.6)]))),
        b(2, ClmBindingValues::Opacity(cells(vec![(0, 2, 0.3)]))),
    ])];
    file(nodes, params, Vec::new())
}

/// Two one-param bindings on one node, from different params — one deform
/// slot per binding.
fn two_params_deforming_one_node() -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        node(Some(0), "Body", ClmNodeKind::Part(part(quad(5.0, 5.0)))),
    ];
    let param = |i: usize, name: &'static str, values: Vec<f32>| {
        FixtureParam::scalar(name, 0.0, 1.0, 0.0, vec![0.0, 1.0]).driving(vec![binding(
            1,
            InterpolateMode::Linear,
            one(i),
            ClmBindingValues::Deform(cells(vec![(1, 0, values)])),
        )])
    };
    let params = vec![
        param(0, "A", vec![3.0, 0.0, 0.0, 0.0, -1.0, 2.0, 0.0, 0.0]),
        param(1, "B", vec![0.0, -4.0, 1.0, 1.0, 0.0, 0.0, 2.0, 0.5]),
    ];
    file(nodes, params, Vec::new())
}

/// A mesh group over two parts, keyed by a param: the descent, the attachment
/// bake and the `translate_children` filter all have to land the same.
fn mesh_group_fixture(dynamic: bool, translate_children: bool) -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        node(
            Some(0),
            "MG",
            ClmNodeKind::MeshGroup(ClmMeshGroup {
                mesh: quad(20.0, 20.0),
                dynamic,
                translate_children,
            }),
        ),
        at(
            Some(1),
            "Under",
            [2.0, 3.0],
            ClmNodeKind::Part(part(quad(6.0, 6.0))),
        ),
        at(Some(1), "Origin", [-4.0, 1.0], ClmNodeKind::Group),
        at(
            Some(3),
            "Deep",
            [1.0, -1.0],
            ClmNodeKind::Part(part(quad(2.0, 2.0))),
        ),
    ];
    let b = |node: usize, values: ClmBindingValues| {
        binding(node, InterpolateMode::Linear, one(0), values)
    };
    let params = vec![
        FixtureParam::scalar("Bend", -1.0, 1.0, 0.0, vec![0.0, 0.5, 1.0]).driving(vec![
            b(
                1,
                ClmBindingValues::Deform(cells(vec![
                    (0, 0, vec![-5.0, 0.0, 3.0, 1.0, 0.0, -6.0, 2.0, 2.0]),
                    (2, 0, vec![4.0, 4.0, -2.0, 0.0, 1.0, 5.0, -3.0, 1.0]),
                ])),
            ),
            b(
                2,
                ClmBindingValues::Deform(cells(vec![(
                    2,
                    0,
                    vec![1.0, 1.0, -1.0, 0.0, 0.5, 0.5, 0.0, -1.0],
                )])),
            ),
        ]),
    ];
    file(nodes, params, Vec::new())
}

/// Two parts welded slot to slot, each with its own deform binding, so the
/// weld pass has something to pull together.
fn weld_fixture() -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        at(
            Some(0),
            "A",
            [-4.0, 0.0],
            ClmNodeKind::Part(welded_part(quad(4.0, 4.0), &[1, 2])),
        ),
        at(
            Some(0),
            "B",
            [4.0, 0.0],
            ClmNodeKind::Part(welded_part(quad(4.0, 4.0), &[0, 3])),
        ),
    ];
    let b = |node: usize, values: ClmBindingValues| {
        binding(node, InterpolateMode::Linear, one(0), values)
    };
    let params = vec![
        FixtureParam::scalar("Pull", 0.0, 1.0, 0.0, vec![0.0, 1.0]).driving(vec![
            b(
                1,
                ClmBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![0.0, 0.0, 2.0, 1.0, 2.0, -1.0, 0.0, 0.0],
                )])),
            ),
            b(
                2,
                ClmBindingValues::Deform(cells(vec![(
                    1,
                    0,
                    vec![-3.0, 0.5, 0.0, 0.0, 0.0, 0.0, -3.0, -0.5],
                )])),
            ),
        ]),
    ];
    let welds = vec![ClmWeld {
        a: nid(1),
        b: nid(2),
        pairs: vec![
            ClmSlotPair {
                a: SlotId::new("s0").unwrap(),
                b: SlotId::new("s0").unwrap(),
                weight: 0.5,
            },
            ClmSlotPair {
                a: SlotId::new("s1").unwrap(),
                b: SlotId::new("s1").unwrap(),
                weight: 0.25,
            },
        ],
    }];
    file(nodes, params, welds)
}

fn physics(targets: Vec<ParamId>, local_only: bool) -> ClmSimplePhysics {
    let mut target_params: [Option<ParamId>; 2] = [None, None];
    for (slot, id) in target_params.iter_mut().zip(targets) {
        *slot = Some(id);
    }
    ClmSimplePhysics {
        kind: PendulumKind::RigidPendulum,
        map_mode: PhysicsParamMapMode::AngleLength,
        local_only,
        target_params,
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
/// pair of params, the second a single one.
fn chained_physics_fixture() -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        at(
            Some(0),
            "Upper",
            [0.0, 40.0],
            ClmNodeKind::SimplePhysics(physics(two(0), false)),
        ),
        at(Some(0), "Mid", [0.0, 10.0], ClmNodeKind::Group),
        at(
            Some(2),
            "Lower",
            [5.0, -20.0],
            ClmNodeKind::SimplePhysics(physics(one(1), true)),
        ),
        at(
            Some(0),
            "Hair",
            [0.0, 0.0],
            ClmNodeKind::Part(part(quad(6.0, 20.0))),
        ),
    ];
    let swing = |node: usize, values: ClmBindingValues| {
        binding(node, InterpolateMode::Linear, two(0), values)
    };
    let params = vec![
        FixtureParam::pair(
            "Swing",
            [-1.0, 0.0],
            [1.0, 2.0],
            [0.0, 1.0],
            vec![0.0, 0.5, 1.0],
            vec![0.0, 1.0],
        )
        .driving(vec![
            // The upper driver's output moves the lower driver's anchor.
            swing(
                2,
                ClmBindingValues::TransformTX(cells(vec![(0, 0, -30.0), (2, 1, 30.0)])),
            ),
            swing(
                4,
                ClmBindingValues::Deform(cells(vec![
                    (0, 0, vec![-4.0, 0.0, 4.0, 0.0, 0.0, 2.0, 0.0, -2.0]),
                    (2, 1, vec![4.0, 1.0, -4.0, 1.0, 0.0, -2.0, 0.0, 2.0]),
                ])),
            ),
            // An output-scale binding, so the pre-pass's second target
            // kind is exercised too.
            swing(3, ClmBindingValues::OutputScaleX(cells(vec![(0, 0, 0.5)]))),
        ]),
        FixtureParam::scalar("Tip", -1.0, 1.0, 0.0, vec![0.0, 1.0]).driving(vec![binding(
            4,
            InterpolateMode::Linear,
            one(1),
            ClmBindingValues::TransformTY(cells(vec![(1, 0, 12.0)])),
        )]),
    ];
    file(nodes, params, Vec::new())
}

/// A mesh group whose `translate_children` filter targets a `local_only`
/// driver — the case that forces the anchor pre-pass every frame.
fn tc_over_local_driver_fixture() -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        node(
            Some(0),
            "MG",
            ClmNodeKind::MeshGroup(ClmMeshGroup {
                mesh: quad(30.0, 30.0),
                dynamic: false,
                translate_children: true,
            }),
        ),
        at(
            Some(1),
            "Driver",
            [3.0, 6.0],
            ClmNodeKind::SimplePhysics(physics(two(1), true)),
        ),
        at(
            Some(1),
            "Skin",
            [0.0, 0.0],
            ClmNodeKind::Part(part(quad(8.0, 8.0))),
        ),
    ];
    let params = vec![
        FixtureParam::scalar("Bend", -1.0, 1.0, 0.0, vec![0.0, 1.0]).driving(vec![binding(
            1,
            InterpolateMode::Linear,
            one(0),
            ClmBindingValues::Deform(cells(vec![(
                1,
                0,
                vec![6.0, -2.0, 6.0, -2.0, 6.0, -2.0, 6.0, -2.0],
            )])),
        )]),
        FixtureParam::pair(
            "Sway",
            [-1.0, 0.0],
            [1.0, 2.0],
            [0.0, 1.0],
            vec![0.0, 1.0],
            vec![0.0, 1.0],
        )
        .driving(vec![binding(
            3,
            InterpolateMode::Linear,
            two(1),
            ClmBindingValues::TransformTX(cells(vec![(0, 0, -7.0), (1, 1, 7.0)])),
        )]),
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
    let f = two_param_fixture(InterpolateMode::Linear);
    let mut model = Model::from_clm_file(&f.file).unwrap();
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
            node(None, "Root", ClmNodeKind::Group),
            at(
                Some(0),
                "Driver",
                [0.0, 40.0],
                ClmNodeKind::SimplePhysics(physics(two(0), false)),
            ),
            at(
                Some(0),
                "Hair",
                [0.0, 0.0],
                ClmNodeKind::Part(part(quad(6.0, 20.0))),
            ),
        ];
        let params = vec![FixtureParam::pair(
            "Swing",
            [-1.0, 0.0],
            [1.0, 2.0],
            [0.0, 1.0],
            vec![0.0, 1.0],
            vec![0.0, 1.0],
        )
        .driving(vec![binding(
            2,
            InterpolateMode::Linear,
            two(0),
            ClmBindingValues::TransformTX(cells(vec![(1, 1, 20.0)])),
        )])];
        file(nodes, params, Vec::new())
    };
    let mut model = Model::from_clm_file(&f.file).unwrap();
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
fn animation_fixture() -> FixtureFile {
    let nodes = vec![
        node(None, "Root", ClmNodeKind::Group),
        node(Some(0), "Body", ClmNodeKind::Part(part(quad(5.0, 5.0)))),
    ];
    let b = |values: ClmBindingValues| binding(1, InterpolateMode::Linear, one(0), values);
    let params = vec![
        FixtureParam::scalar("Blink", 0.0, 1.0, 0.0, vec![0.0, 0.5, 1.0]).driving(vec![
            b(ClmBindingValues::TransformTY(cells(vec![
                (0, 0, -3.0),
                (2, 0, 9.0),
            ]))),
            b(ClmBindingValues::Deform(cells(vec![(
                2,
                0,
                vec![1.0, 2.0, -1.0, 0.0, 0.0, 3.0, 0.5, -0.5],
            )]))),
        ]),
    ];
    file(nodes, params, Vec::new())
}
