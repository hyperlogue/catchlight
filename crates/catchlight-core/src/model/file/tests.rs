use super::*;
use crate::components::{BlendMode, MaskMode};
use crate::formats::clm::{ClmIndices, ClmMesh, TextureAlpha, TextureEncoding};
use crate::id::SeededHex;

pub(super) fn slot(id: &str) -> SlotId {
    SlotId::new(id).unwrap()
}

pub(super) fn quad(x: f32) -> ClmMesh {
    ClmMesh {
        verts: vec![x - 1.0, -1.0, x + 1.0, -1.0, x + 1.0, 1.0, x - 1.0, 1.0],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    }
}

pub(super) fn sample() -> Model {
    let mut hex = SeededHex::new(9);
    let mut m = Model::new();
    let root = m.root().unwrap().clone();

    let upper = m
        .add_node(
            &root,
            ModelNode::new("Upper", ModelNodeKind::Part(ModelPart::new(quad(0.0)))),
            &mut hex,
        )
        .unwrap();
    let lower = m
        .add_node(
            &root,
            ModelNode::new("Lower", ModelNodeKind::Part(ModelPart::new(quad(3.0)))),
            &mut hex,
        )
        .unwrap();
    m.add_texture(
        &upper,
        ModelTexture {
            encoding: TextureEncoding::Png,
            alpha: TextureAlpha::Straight,
            data: [0x89, b'P', b'N', b'G', 1, 2, 3, 4][..].into(),
        },
        &mut hex,
    )
    .unwrap();

    let composite = m
        .add_node(
            &root,
            ModelNode::new("Face", ModelNodeKind::Composite(ModelComposite::new())),
            &mut hex,
        )
        .unwrap();
    m.mask_add(&composite, &lower, MaskMode::DodgeMask).unwrap();
    m.update_node_kind(&composite, |kind| {
        if let ModelNodeKind::Composite(c) = kind {
            c.blend_mode = BlendMode::Multiply;
        }
    })
    .unwrap();

    m.add_node(
        &composite,
        ModelNode::new(
            "Bend",
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad(6.0))),
        ),
        &mut hex,
    )
    .unwrap();

    let x = m
        .add_param(
            ModelParam::new(Name::truncated("Head.x"), -1.0, 1.0, 0.0),
            &mut hex,
        )
        .unwrap();
    let y = m
        .add_param(
            ModelParam::new(Name::truncated("Head.y"), -1.0, 1.0, 0.0),
            &mut hex,
        )
        .unwrap();

    m.add_node(
        &root,
        ModelNode::new("Sway", ModelNodeKind::MotionResponse(Default::default())),
        &mut hex,
    )
    .unwrap();

    let key = BindingKey::pair(x, y, upper.clone(), BindingTarget::Deform);
    m.add_binding(&key).unwrap();
    m.set_deform_vertices(&key, [1, 1], vec![1.0; 8]).unwrap();

    let (left, right) = (slot("left"), slot("right"));
    for (node, verts) in [(&upper, [0, 3]), (&lower, [1, 2])] {
        for (slot, vertex) in [(&left, verts[0]), (&right, verts[1])] {
            m.slot_add(node, slot.clone()).unwrap();
            m.slot_fill(node, slot, vertex).unwrap();
        }
    }
    m.set_welds(vec![ModelWeld::new(
        upper,
        lower,
        vec![
            SlotPair {
                a: left.clone(),
                b: left,
                weight: 1.0,
            },
            SlotPair {
                a: right.clone(),
                b: right,
                weight: 0.25,
            },
        ],
    )])
    .unwrap();

    m
}

pub(super) fn named(m: &Model, name: &str) -> NodeId {
    m.nodes_in_order()
        .into_iter()
        .find(|id| {
            m.node(id)
                .is_some_and(|n| n.properties.name.as_str() == name)
        })
        .unwrap_or_else(|| panic!("no node named {name}"))
}

pub(super) fn node_named<'a>(file: &'a ClmFile, name: &str) -> &'a ClmNode {
    file.doc
        .nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node named {name}"))
}

pub(super) fn part_named<'a>(file: &'a mut ClmFile, name: &str) -> &'a mut ClmPart {
    let node = file
        .doc
        .nodes
        .iter_mut()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no node named {name}"));
    match &mut node.kind {
        ClmNodeKind::Part(p) => p,
        _ => panic!("{name} is not a part"),
    }
}

pub(super) fn load_err(file: &ClmFile) -> ClmLoadError {
    match Model::from_clm_file(file) {
        Err(LoadError::Invalid(e)) => e,
        other => panic!("expected a structured load error, got {other:?}"),
    }
}

pub(super) fn fragment_err(file: &ClmFile) -> ClmLoadError {
    match Model::from_clm_file_with(file, ReadOptions::fragment()) {
        Err(LoadError::Invalid(e)) => e,
        other => panic!("expected a structured load error, got {other:?}"),
    }
}

/// [`sample`] minus the root, the `Lower` part and the params: an addon
/// with every kind of dangling reference.
pub(super) fn sample_fragment_file() -> ClmFile {
    let mut file = sample().to_clm_file().unwrap();
    let lower = node_named(&file, "Lower").id.clone();
    file.doc
        .nodes
        .retain(|n| n.parent.is_some() && n.id != lower);
    file.doc.params.clear();
    file
}

pub(super) fn part<'a>(m: &'a Model, id: &NodeId) -> &'a ModelPart {
    match &m.node(id).unwrap().kind {
        ModelNodeKind::Part(p) => p,
        other => panic!("{id} is a {}", other.name()),
    }
}

pub(super) fn part_mask_source<'a>(m: &'a Model, id: &NodeId) -> &'a NodeId {
    match &m.node(id).unwrap().kind {
        ModelNodeKind::Composite(c) => c.masks()[0].source(),
        other => panic!("{id} is a {}", other.name()),
    }
}

#[test]
fn a_fragment_round_trips_with_its_dangling_references() {
    let file = sample_fragment_file();
    let bytes = clm::encode(&file.doc, &file.textures, &file.extensions).unwrap();
    let f = Model::from_clm_bytes_with(&bytes, ReadOptions::fragment()).unwrap();

    assert!(f.is_fragment());
    assert_eq!(f.root(), None);
    let upper = named(&f, "Upper");
    let face = named(&f, "Face");
    assert_eq!(f.roots(), [upper.clone(), face.clone(), named(&f, "Sway")]);
    for r in f.roots() {
        let parent = f.node(r).unwrap().parent().unwrap();
        assert_eq!(parent.as_str(), "root");
        assert!(f.node(parent).is_none());
    }
    assert!(f.texture(part(&f, &upper).albedo().unwrap()).is_some());
    assert!(f.node(part_mask_source(&f, &face)).is_none());
    assert!(f.node(f.welds()[0].b()).is_none());
    assert_eq!(f.welds()[0].pairs().len(), 2);
    let binding = f.bindings().next().unwrap();
    assert!(binding.params().iter().all(|p| f.param(p).is_none()));

    assert_eq!(f.to_clm_bytes().unwrap(), bytes);
    let again =
        Model::from_clm_bytes_with(&f.to_clm_bytes().unwrap(), ReadOptions::fragment()).unwrap();
    assert_eq!(again.to_clm_bytes().unwrap(), bytes);
}

#[test]
fn a_model_round_trips_through_clm() {
    let m = sample();
    let file = m.to_clm_file().unwrap();
    let reopened = Model::from_clm_file(&file).unwrap();
    assert_eq!(reopened.to_clm_file().unwrap(), file);
    assert_eq!(reopened.to_clm_bytes().unwrap(), m.to_clm_bytes().unwrap());
}

#[test]
fn ids_and_names_survive_the_round_trip() {
    let m = sample();
    let reopened = Model::from_clm_bytes(&m.to_clm_bytes().unwrap()).unwrap();

    assert_eq!(reopened.root(), m.root());
    assert_eq!(reopened.nodes_in_order(), m.nodes_in_order());
    assert_eq!(reopened.param_ids(), m.param_ids());
    assert_eq!(reopened.texture_ids(), m.texture_ids());
    for id in m.nodes_in_order() {
        assert_eq!(
            reopened.node(&id).unwrap().properties.name.as_str(),
            m.node(&id).unwrap().properties.name.as_str(),
            "name of {id}",
        );
    }
    for id in m.param_ids() {
        assert_eq!(
            reopened.param(id).unwrap().name.as_str(),
            m.param(id).unwrap().name.as_str(),
            "name of {id}",
        );
    }
    assert_eq!(
        reopened.bindings().next().unwrap().key(),
        m.bindings().next().unwrap().key(),
    );
}

#[test]
fn a_weld_travels_as_two_parts_and_its_pairs() {
    let m = sample();
    let file = m.to_clm_file().unwrap();

    let ClmNodeKind::Part(upper) = &node_named(&file, "Upper").kind else {
        panic!("Upper is a part")
    };
    assert_eq!(
        upper
            .slots
            .iter()
            .map(|s| (s.id.to_string(), s.vertex))
            .collect::<Vec<_>>(),
        vec![
            ("left".to_string(), Some(0)),
            ("right".to_string(), Some(3))
        ],
    );
    let [weld] = &file.doc.welds[..] else {
        panic!("one weld")
    };
    assert_eq!(weld.a, named(&m, "Upper"));
    assert_eq!(weld.b, named(&m, "Lower"));
    assert_eq!(
        weld.pairs
            .iter()
            .map(|p| (p.a.to_string(), p.b.to_string(), p.weight))
            .collect::<Vec<_>>(),
        vec![
            ("left".to_string(), "left".to_string(), 1.0),
            ("right".to_string(), "right".to_string(), 0.25)
        ],
    );

    let reopened = Model::from_clm_file(&file).unwrap();
    assert_eq!(
        reopened.welds()[0].resolve(&reopened),
        m.welds()[0].resolve(&m)
    );
}

#[test]
fn a_weld_over_an_unfilled_slot_loads_and_skips_the_pair() {
    let mut m = sample();
    let upper = named(&m, "Upper");
    m.slot_clear(&upper, &slot("right")).unwrap();

    let bytes = m.to_clm_bytes().unwrap();
    let reopened = Model::from_clm_bytes(&bytes).unwrap();

    let [weld] = reopened.welds() else {
        panic!("one weld")
    };
    assert_eq!(weld.pairs().len(), 2, "the slot is still paired");
    assert_eq!(
        weld.resolve(&reopened),
        vec![ModelWeldPair {
            a_vert: 0,
            b_vert: 1,
            weight: 1.0,
        }],
        "only the filled pair solves",
    );
    assert_eq!(reopened.unfilled_slots(), vec![(upper, slot("right"))],);
    assert_eq!(reopened.to_clm_bytes().unwrap(), bytes);
}
