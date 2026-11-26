#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! What a played clip evaluates, pinned against a committed baseline.
//!
//! Regenerate after an intentional behaviour change:
//!   UPDATE_FRAME_BASELINE=1 cargo test -p catchlight-extension-animations --test evaluated_frame

use std::collections::BTreeMap;
use std::path::PathBuf;

use catchlight_core::components::BlendMode;
use catchlight_core::formats::clm::{
    ClmBinding, ClmBindingValues, ClmCell, ClmCells, ClmFile, ClmIndices, ClmMesh, ClmNode,
    ClmNodeKind, ClmParam, ClmPart, ClmStructure, ClmTransform,
};
use catchlight_core::{InterpolateMode, Model, NodeId, NodeIdx, NodeKind, ParamId, Puppet};
use catchlight_extension_animations::{AnimationPlayer, Clip, Keyframe, Lane, LaneTarget};

const TOL: f32 = 1e-5;
const DT: f32 = 1.0 / 60.0;
const SAMPLE_EVERY: usize = 10;

/// Per node: the sixteen transform elements, z order, enabled flag, then
/// colour and combined deform when present.
type Frame = Vec<Vec<f32>>;
type Baseline = BTreeMap<String, Frame>;

fn node(id: &str, parent: Option<&str>, name: &str, kind: ClmNodeKind) -> ClmNode {
    ClmNode {
        id: NodeId::new(id).unwrap(),
        parent: parent.map(|p| NodeId::new(p).unwrap()),
        name: name.into(),
        enabled: true,
        z_order: 0.0,
        transform: ClmTransform {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0, 1.0],
        },
        lock_to_root: false,
        kind,
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

/// One part bound to one param by a translation and a deform.
fn model() -> Model {
    let mesh = ClmMesh {
        verts: vec![-5.0, -5.0, 5.0, -5.0, 5.0, 5.0, -5.0, 5.0],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 2, 3, 0]),
        origin: [0.0, 0.0],
    };
    let body = ClmPart {
        mesh,
        albedo: None,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        tint: [1.0; 3],
        screen_tint: [0.0; 3],
        masks: Vec::new(),
        mask_threshold: 0.5,
        slots: Vec::new(),
    };
    let binding = |values| ClmBinding {
        key_positions: vec![vec![0.0, 0.5, 1.0]],
        params: vec![ParamId::new("param-0").unwrap()],
        node: NodeId::new("node-1").unwrap(),
        interpolate_mode: InterpolateMode::Linear,
        values,
    };
    let file = ClmFile {
        doc: ClmStructure {
            nodes: vec![
                node("root", None, "Root", ClmNodeKind::Group),
                node("node-1", Some("root"), "Body", ClmNodeKind::Part(body)),
            ],
            params: vec![ClmParam {
                id: ParamId::new("param-0").unwrap(),
                name: "Blink".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
            }],
            bindings: vec![
                binding(ClmBindingValues::TransformTY(cells(vec![
                    (0, 0, -3.0),
                    (2, 0, 9.0),
                ]))),
                binding(ClmBindingValues::Deform(cells(vec![(
                    2,
                    0,
                    vec![1.0, 2.0, -1.0, 0.0, 0.0, 3.0, 0.5, -0.5],
                )]))),
            ],
            ..ClmStructure::default()
        },
        extensions: Vec::new(),
        unknown_sections: Vec::new(),
        textures: Vec::new(),
    };
    Model::from_clm_file(&file).unwrap()
}

fn blink() -> Clip {
    Clip {
        name: "Blink".into(),
        timestep: 1.0 / 60.0,
        length: 31,
        lead_in: Some(6),
        lead_out: Some(28),
        lanes: vec![Lane::new(
            LaneTarget::Param(ParamId::new("param-0").unwrap()),
            InterpolateMode::Linear,
            vec![
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
        )],
    }
}

fn frame(puppet: &Puppet, nodes: &[NodeIdx]) -> Frame {
    nodes
        .iter()
        .map(|&idx| {
            let t = puppet.transforms().get(idx);
            let node = puppet.get(idx).unwrap();
            let mut row: Vec<f32> = t.to_cols_array().to_vec();
            row.push(node.z_order);
            row.push(if node.enabled { 1.0 } else { 0.0 });
            if let NodeKind::Part(p) = &node.kind {
                row.push(p.opacity);
                row.extend(p.tint.to_array());
                row.extend(p.screen_tint.to_array());
            }
            if let Some(deform) = puppet.combined_deform(idx) {
                row.extend(deform.iter().flat_map(|v| [v.x, v.y]));
            }
            row
        })
        .collect()
}

fn current() -> Baseline {
    let model = model();
    let mut puppet = Puppet::new(&model);
    let nodes: Vec<NodeIdx> = ["root", "node-1"]
        .iter()
        .map(|id| puppet.node_idx(&NodeId::new(*id).unwrap()).unwrap())
        .collect();
    let mut player = AnimationPlayer::new(vec![blink()]);
    assert!(player.play("Blink"));
    assert_eq!(player.playing(), Some("Blink"));

    let mut out = Baseline::new();
    let mut moved = false;
    for i in 0..120 {
        player.advance(&mut puppet, DT);
        puppet.tick(&model, DT);
        if i % SAMPLE_EVERY == 0 {
            out.insert(format!("animation frame {i}"), frame(&puppet, &nodes));
        }
        moved |= puppet.transforms().get(nodes[1]).w_axis.y.abs() > 1e-3;
    }
    assert!(moved, "the lane actually drove the binding");

    player.stop();
    assert!(!player.is_playing());
    assert!(!player.advance(&mut puppet, DT));
    puppet.tick(&model, DT);
    out.insert("animation stopped".into(), frame(&puppet, &nodes));
    out
}

#[test]
fn a_played_clip_evaluates_the_frames_it_always_has() -> Result<(), Box<dyn std::error::Error>> {
    let current = current();
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/evaluated_frame.json");
    if std::env::var_os("UPDATE_FRAME_BASELINE").is_some() {
        std::fs::write(&path, serde_json::to_string(&current)?)?;
        eprintln!("updated frame baseline at {}", path.display());
        return Ok(());
    }
    let expected: Baseline = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        current.keys().collect::<Vec<_>>(),
        "the label set moved; regenerate the baseline"
    );
    for (label, got) in &current {
        let want = &expected[label];
        assert_eq!(want.len(), got.len(), "{label}: node count");
        for (i, (e, g)) in want.iter().zip(got).enumerate() {
            assert_eq!(e.len(), g.len(), "{label}: node {i}: value count");
            for (k, (a, b)) in e.iter().zip(g).enumerate() {
                assert!(
                    (a - b).abs() <= TOL,
                    "{label}: node {i} value {k}: expected {a} got {b}"
                );
            }
        }
    }
    Ok(())
}
