#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `.clm` against the models that are actually committed, and against one big
//! enough that an accidental quadratic or a hash-order leak would show.
//! `tests/models/*.clm` are Git LFS objects.

use std::path::{Path, PathBuf};

use catchlight_core::formats::clm::{self, ClmAnimation, ClmKeyframe, ClmLane, ClmMesh};
use catchlight_core::id::{Name, ParamId, SeededHex};
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::{
    BindingKey, BindingTarget, Model, ModelNode, ModelNodeKind, ModelParam, ModelPart,
};

fn models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tests/models")
}

fn fixtures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(models_dir())
        .expect("tests/models")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "clm"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no .clm fixtures found");
    paths
}

/// Catches a reader that silently re-mints an Id.
fn identity(model: &Model) -> Vec<String> {
    let mut out = vec![format!("roots={:?}", model.roots())];
    for id in model.nodes_in_order() {
        let node = model.node(&id).expect("node in order");
        out.push(format!(
            "node {id} {:?} parent={:?}",
            node.name.as_str(),
            node.parent().map(ToString::to_string)
        ));
    }
    for id in model.param_ids() {
        let p = model.param(id).expect("listed param");
        out.push(format!("param {id} {:?}", p.name.as_str()));
    }
    for id in model.texture_ids() {
        out.push(format!("texture {id}"));
    }
    for b in model.bindings() {
        out.push(format!("binding {:?}", b.key()));
    }
    for w in model.welds() {
        out.push(format!(
            "weld {:?} {:?} {:?} {:?}",
            w.a(),
            w.b(),
            w.pairs(),
            w.resolve(model),
        ));
    }
    out
}

#[test]
fn every_committed_fixture_round_trips_byte_for_byte() {
    for path in fixtures() {
        let bytes = std::fs::read(&path).expect("read fixture");
        let model =
            Model::from_clm_bytes(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

        let encoded = model.to_clm_bytes().unwrap();
        assert_eq!(
            encoded,
            bytes,
            "{} is not what the writer would write",
            path.display()
        );
        let reopened = Model::from_clm_bytes(&encoded).unwrap();
        assert_eq!(reopened.to_clm_bytes().unwrap(), encoded);
        assert!(model.authored_eq(&reopened).unwrap());
        assert_eq!(
            identity(&reopened),
            identity(&model),
            "{} loses an Id or a Name on the way round",
            path.display()
        );
    }
}

#[test]
fn both_readers_and_both_writers_agree_on_every_fixture() {
    for path in fixtures() {
        let name = path.display();
        let bytes = std::fs::read(&path).expect("read fixture");

        let file = clm::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            clm::encode(&file.doc, &file.textures, &file.extensions).unwrap(),
            bytes,
            "{name}: decode then encode"
        );

        let direct = Model::from_clm_bytes(&bytes).unwrap();
        let through_file = Model::from_clm_file(&file).unwrap();
        assert!(direct.authored_eq(&through_file).unwrap(), "{name}");
        assert_eq!(identity(&direct), identity(&through_file), "{name}");
        for id in direct.texture_ids() {
            assert_eq!(
                direct.texture(id).unwrap().data,
                through_file.texture(id).unwrap().data,
                "{name}: texture {id}"
            );
        }

        assert_eq!(direct.to_clm_file().unwrap(), file, "{name}");
        assert_eq!(through_file.to_clm_bytes().unwrap(), bytes, "{name}");
    }
}

#[test]
fn the_welded_fixture_keeps_its_vertex_pairs() {
    let bytes = std::fs::read(models_dir().join("welded_seam.clm")).expect("welded_seam.clm");
    let model = Model::from_clm_bytes(&bytes).unwrap();

    let [weld] = model.welds() else {
        panic!("welded_seam carries exactly one weld");
    };
    assert_eq!(
        weld.resolve(&model)
            .iter()
            .map(|p| (p.a_vert, p.b_vert, p.weight))
            .collect::<Vec<_>>(),
        vec![(0, 0, 1.0), (1, 1, 0.5), (2, 2, 0.0)],
    );
    assert!(
        model.unfilled_slots().is_empty(),
        "every slot the fixture welds is filled",
    );
}

fn five_hundred_node_model() -> Model {
    let mut hex = SeededHex::new(21);
    let mut model = Model::new();
    let root = model.root().unwrap().clone();

    let mesh = |i: usize| ClmMesh {
        verts: vec![0.0, 0.0, 1.0, 0.0, i as f32, 1.0],
        uvs: vec![0.0, 0.0, 1.0, 0.0, 0.5, 1.0],
        indices: catchlight_core::formats::clm::ClmIndices::U16(vec![0, 1, 2]),
        origin: [0.0, 0.0],
    };

    let mut params: Vec<ParamId> = Vec::new();
    let mut parent = root;
    for i in 0..500usize {
        let node = model
            .add_node(
                &parent,
                ModelNode::new(
                    format!("node {i}"),
                    ModelNodeKind::Part(ModelPart::new(mesh(i))),
                ),
                &mut hex,
            )
            .unwrap();
        let param = model
            .add_param(
                ModelParam::new(Name::truncated(format!("p{i}")), 0.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        let key = BindingKey::new(param.clone(), node.clone(), BindingTarget::Deform);
        model.add_binding(&key).unwrap();
        model
            .set_deform_vertices(&key, [1, 0], vec![i as f32; 6])
            .unwrap();
        params.push(param);
        parent = if i % 8 == 7 {
            model.root().unwrap().clone()
        } else {
            node
        };
    }
    model
        .set_animations(vec![ClmAnimation {
            name: "sweep".into(),
            length: 500,
            lanes: params
                .iter()
                .map(|param| ClmLane {
                    param: param.clone(),
                    interpolation: InterpolateMode::Linear,
                    keyframes: vec![ClmKeyframe {
                        frame: 0,
                        value: 0.0,
                    }],
                })
                .collect(),
            ..ClmAnimation::default()
        }])
        .unwrap();
    model
}

/// Nodes, params and bindings live in `HashMap`s; a hash-order leak would
/// pass a three-node round trip and fail here.
#[test]
fn a_five_hundred_node_model_round_trips() {
    let model = five_hundred_node_model();
    assert_eq!(model.node_count(), 501);

    let bytes = model.to_clm_bytes().unwrap();
    let reopened = Model::from_clm_bytes(&bytes).unwrap();

    assert_eq!(reopened.to_clm_bytes().unwrap(), bytes);
    assert_eq!(identity(&reopened), identity(&model));
    assert_eq!(reopened.animations(), model.animations());
    assert_eq!(
        model.to_clm_bytes().unwrap(),
        bytes,
        "and writing is stable"
    );
}

#[test]
fn a_spine_round_trips_byte_for_byte() {
    use catchlight_core::model::ModelSpine;

    let mut hex = SeededHex::new(29);
    let mut model = Model::new();
    let params: Vec<ParamId> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            model
                .add_param(
                    ModelParam::new(Name::truncated(*name), -1.0, 1.0, 0.0),
                    &mut hex,
                )
                .unwrap()
        })
        .collect();
    let root = model.root().unwrap().clone();
    let joints = vec![[0.0, -60.0], [8.0, -110.0], [-4.0, -150.0]];
    let node = model
        .add_node(
            &root,
            ModelNode::new(
                "tail",
                ModelNodeKind::Spine(ModelSpine::new(joints.clone())),
            ),
            &mut hex,
        )
        .unwrap();
    model
        .set_spine_targets(
            &node,
            vec![Some(params[0].clone()), None, Some(params[2].clone())],
        )
        .unwrap();
    let bytes = model.to_clm_bytes().unwrap();
    let reopened = Model::from_clm_bytes(&bytes).unwrap();
    assert_eq!(reopened.to_clm_bytes().unwrap(), bytes, "writing is stable");
    assert_eq!(identity(&reopened), identity(&model));

    let ModelNodeKind::Spine(back) = &reopened.node(&node).unwrap().kind else {
        panic!("the spine came back as another kind");
    };
    assert_eq!(back.joints(), joints.as_slice());
    assert_eq!(
        back.targets(),
        [Some(params[0].clone()), None, Some(params[2].clone())]
    );
}
