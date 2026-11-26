use super::*;
use crate::formats::clm::{ClmCells, ClmIndices};
use crate::model::file::tests::*;

#[test]
fn a_fragment_is_refused_by_the_complete_model_reader() {
    assert!(matches!(
        load_err(&sample_fragment_file()),
        ClmLoadError::DanglingParent { .. }
    ));
}

#[test]
fn a_complete_model_is_refused_by_the_fragment_reader() {
    let file = sample().to_clm_file().unwrap();
    assert!(matches!(
        fragment_err(&file),
        ClmLoadError::FragmentHasNoParent { node } if node == "root"
    ));
}

#[test]
fn a_dangling_parent_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.nodes[1].parent = Some(NodeId::new("ghost").unwrap());

    assert_eq!(
        load_err(&file),
        ClmLoadError::DanglingParent {
            node: file.doc.nodes[1].id.to_string(),
            parent: "ghost".into(),
        }
    );
}

#[test]
fn a_parent_written_after_its_child_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let index = |name: &str| {
        file.doc
            .nodes
            .iter()
            .position(|n| n.name == name)
            .unwrap_or_else(|| panic!("no node named {name}"))
    };
    let (face, bend) = (index("Face"), index("Bend"));
    file.doc.nodes.swap(face, bend);

    assert!(matches!(
        load_err(&file),
        ClmLoadError::NotTopological { .. }
    ));
}

#[test]
fn two_roots_are_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.nodes[1].parent = None;

    assert!(matches!(
        load_err(&file),
        ClmLoadError::MultipleRoots { .. }
    ));
}

#[test]
fn a_dangling_albedo_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    part_named(&mut file, "Upper").albedo = Some(TexId::new("gone").unwrap());

    assert!(matches!(
        load_err(&file),
        ClmLoadError::DanglingTexture { id, .. } if id == "gone"
    ));
}

#[test]
fn a_fragments_albedo_may_not_dangle_either() {
    let mut file = sample_fragment_file();
    let drawn = part_named(&mut file, "Upper")
        .albedo
        .clone()
        .expect("Upper draws one");
    file.textures.clear();

    assert!(matches!(
        fragment_err(&file),
        ClmLoadError::DanglingTexture { id, .. } if id == drawn.as_str()
    ));
}

#[test]
fn a_texture_no_part_draws_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let spare = ClmTexture {
        id: TexId::new("spare").unwrap(),
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: vec![0x89, b'P', b'N', b'G'],
    };
    file.textures.push(spare);

    assert!(matches!(
        load_err(&file),
        ClmLoadError::UnusedTexture { id } if id == "spare"
    ));

    let mut file = sample_fragment_file();
    file.textures.push(ClmTexture {
        id: TexId::new("spare").unwrap(),
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: vec![0x89, b'P', b'N', b'G'],
    });
    assert!(matches!(
        fragment_err(&file),
        ClmLoadError::UnusedTexture { id } if id == "spare"
    ));
}

#[test]
fn a_dangling_mask_source_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    for node in &mut file.doc.nodes {
        if let ClmNodeKind::Composite(c) = &mut node.kind {
            c.masks[0].source = NodeId::new("ghost").unwrap();
        }
    }

    assert!(matches!(
        load_err(&file),
        ClmLoadError::DanglingNode {
            field: "mask source",
            id,
            ..
        } if id == "ghost"
    ));
}

#[test]
fn a_param_range_the_model_would_refuse_is_a_structured_error() {
    for (min, max) in [
        (1.0, 0.0),
        (0.0, 0.0),
        (f32::NAN, 1.0),
        (0.0, f32::INFINITY),
    ] {
        let mut file = sample().to_clm_file().unwrap();
        let id = file.doc.params[0].id.clone();
        file.doc.params[0].min = min;
        file.doc.params[0].max = max;

        assert_eq!(
            load_err(&file),
            ClmLoadError::ParamRange {
                param: id.to_string(),
            },
            "range {min}..{max}",
        );
    }
}

#[test]
fn a_mask_source_that_is_never_drawn_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let bend = node_named(&file, "Bend").id.clone();
    let face = node_named(&file, "Face").id.clone();
    for node in &mut file.doc.nodes {
        if let ClmNodeKind::Composite(c) = &mut node.kind {
            c.masks[0].source = bend.clone();
        }
    }

    assert_eq!(
        load_err(&file),
        ClmLoadError::MaskSourceKind {
            node: face.to_string(),
            mask_source: bend.to_string(),
            kind: "mesh_group",
        }
    );
}

#[test]
fn a_node_masking_itself_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let face = node_named(&file, "Face").id.clone();
    for node in &mut file.doc.nodes {
        if let ClmNodeKind::Composite(c) = &mut node.kind {
            c.masks[0].source = face.clone();
        }
    }
    assert_eq!(
        load_err(&file),
        ClmLoadError::SelfMask {
            node: face.to_string(),
        }
    );

    let mut fragment = sample_fragment_file();
    let upper = node_named(&fragment, "Upper").id.clone();
    part_named(&mut fragment, "Upper").masks.push(ClmMask {
        source: upper.clone(),
        mode: MaskMode::Mask,
    });
    assert_eq!(
        fragment_err(&fragment),
        ClmLoadError::SelfMask {
            node: upper.to_string(),
        }
    );
}

#[test]
fn a_composite_mask_source_loads() {
    let mut file = sample().to_clm_file().unwrap();
    let face = node_named(&file, "Face").id.clone();
    part_named(&mut file, "Upper").masks.push(ClmMask {
        source: face.clone(),
        mode: MaskMode::Mask,
    });

    let m = Model::from_clm_file(&file).unwrap();

    let upper = named(&m, "Upper");
    assert_eq!(part(&m, &upper).masks()[0].source(), &face);
}

#[test]
fn a_deform_binding_on_a_node_with_no_mesh_is_a_structured_error() {
    for (name, kind) in [("Face", "composite"), ("Sway", "motion_response")] {
        let mut file = sample().to_clm_file().unwrap();
        let node = node_named(&file, name).id.clone();
        file.doc.bindings[0].node = node.clone();
        file.doc.bindings[0].values = ClmBindingValues::Deform(ClmCells::default());

        assert_eq!(
            load_err(&file),
            ClmLoadError::DeformOnUnmeshed {
                node: node.to_string(),
                kind,
            }
        );
    }
}

#[test]
fn a_malformed_mesh_is_a_structured_error() {
    let cases: [(BreakMesh, &str); 7] = [
        (
            |mesh| mesh.verts.push(0.5),
            "vertex array is not [x, y] pairs",
        ),
        (
            |mesh| mesh.uvs.truncate(6),
            "uv count does not match vertices",
        ),
        (
            |mesh| mesh.indices = ClmIndices::U16(vec![0, 1, 9]),
            "index names a missing vertex",
        ),
        (
            |mesh| mesh.indices = ClmIndices::U16(vec![0, 1, 2, 0]),
            "index count is not whole triangles",
        ),
        (|mesh| mesh.verts[3] = f32::NAN, "a vertex is not finite"),
        (|mesh| mesh.uvs[0] = f32::INFINITY, "a uv is not finite"),
        (
            |mesh| mesh.origin[1] = f32::NEG_INFINITY,
            "the origin is not finite",
        ),
    ];

    for (name, take) in [("Upper", part_mesh as MeshOf), ("Bend", group_mesh)] {
        for (break_it, reason) in cases {
            let mut file = sample().to_clm_file().unwrap();
            let node = node_named(&file, name).id.clone();
            let target = file
                .doc
                .nodes
                .iter_mut()
                .find(|n| n.name == name)
                .expect("the node");
            break_it(take(target));

            assert_eq!(
                load_err(&file),
                ClmLoadError::MalformedMesh {
                    node: node.to_string(),
                    reason,
                },
                "{name}: {reason}",
            );
        }
    }
}

type BreakMesh = fn(&mut ClmMesh);

type MeshOf = fn(&mut ClmNode) -> &mut ClmMesh;

fn part_mesh(node: &mut ClmNode) -> &mut ClmMesh {
    match &mut node.kind {
        ClmNodeKind::Part(p) => &mut p.mesh,
        other => panic!("not a part: {other:?}"),
    }
}

fn group_mesh(node: &mut ClmNode) -> &mut ClmMesh {
    match &mut node.kind {
        ClmNodeKind::MeshGroup(g) => &mut g.mesh,
        other => panic!("not a mesh group: {other:?}"),
    }
}

#[test]
fn a_malformed_spine_is_a_structured_error() {
    fn spine_file(build: impl FnOnce(&mut ClmSpine)) -> (ClmFile, String) {
        let mut file = sample().to_clm_file().unwrap();
        let param = file.doc.params[0].id.clone();
        let mut spine = ClmSpine {
            joints: vec![[0.0, -50.0], [0.0, -100.0]],
            targets: vec![Some(param), None],
        };
        build(&mut spine);
        let root = file.doc.nodes[0].id.clone();
        let id = NodeId::new("root/spine-00000001").unwrap();
        file.doc.nodes.push(ClmNode {
            id: id.clone(),
            parent: Some(root),
            name: "tail".into(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: ClmNodeKind::Spine(spine),
        });
        (file, id.to_string())
    }

    let (ok, _) = spine_file(|_| {});
    assert!(Model::from_clm_file(&ok).is_ok(), "the base case loads");

    let (file, node) = spine_file(|sp| sp.joints.clear());
    assert_eq!(load_err(&file), ClmLoadError::SpineNoJoints { node });

    let (file, node) = spine_file(|sp| sp.joints[1][0] = f32::NAN);
    assert_eq!(
        load_err(&file),
        ClmLoadError::SpineJointField { node, joint: 1 }
    );

    let (file, node) = spine_file(|sp| sp.joints[1][1] = f32::INFINITY);
    assert_eq!(
        load_err(&file),
        ClmLoadError::SpineJointField { node, joint: 1 }
    );

    let (file, node) = spine_file(|sp| sp.joints[1] = sp.joints[0]);
    assert_eq!(
        load_err(&file),
        ClmLoadError::SpineJointCoincident { node, joint: 1 }
    );

    // The first link starts at the node's own origin.
    let (file, node) = spine_file(|sp| sp.joints[0] = [0.0, 0.0]);
    assert_eq!(
        load_err(&file),
        ClmLoadError::SpineJointCoincident { node, joint: 0 }
    );

    let (file, node) = spine_file(|sp| sp.targets.pop().map(|_| ()).unwrap_or_default());
    assert_eq!(
        load_err(&file),
        ClmLoadError::SpineTargetCount {
            node,
            targets: 1,
            joints: 2,
        }
    );

    let (file, _) = spine_file(|sp| sp.targets[0] = Some(ParamId::new("gone").unwrap()));
    assert!(matches!(
        load_err(&file),
        ClmLoadError::DanglingParam { ref owner, ref id }
            if owner.starts_with("spine") && id == "gone"
    ));
}

#[test]
fn a_spine_with_no_targets_key_loads_one_slot_per_link() {
    let mut file = sample().to_clm_file().unwrap();
    let root = file.doc.nodes[0].id.clone();
    let id = NodeId::new("root/spine-00000002").unwrap();
    file.doc.nodes.push(ClmNode {
        id: id.clone(),
        parent: Some(root),
        name: "tail".into(),
        enabled: true,
        z_order: 0.0,
        transform: ClmTransform::default(),
        lock_to_root: false,
        kind: ClmNodeKind::Spine(ClmSpine {
            joints: vec![[0.0, -40.0], [0.0, -80.0], [10.0, -120.0]],
            // What `#[serde(default)]` hands the reader for an absent key.
            targets: Vec::new(),
        }),
    });

    let model = Model::from_clm_file(&file).unwrap();
    let ModelNodeKind::Spine(spine) = &model.node(&id).unwrap().kind else {
        panic!("the spine came back as another kind");
    };
    assert_eq!(spine.targets(), [None, None, None]);
    let round_tripped = Model::from_clm_bytes(&model.to_clm_bytes().unwrap()).unwrap();
    let ModelNodeKind::Spine(spine) = &round_tripped.node(&id).unwrap().kind else {
        panic!("the spine came back as another kind");
    };
    assert_eq!(spine.targets(), [None, None, None]);
    assert_eq!(spine.joints(), [[0.0, -40.0], [0.0, -80.0], [10.0, -120.0]]);
}

#[test]
fn a_duplicate_node_id_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let twin = file.doc.nodes[1].id.clone();
    file.doc.nodes[2].id = twin.clone();

    assert_eq!(
        load_err(&file),
        ClmLoadError::DuplicateId {
            kind: "node",
            id: twin.to_string(),
        }
    );
}

#[test]
fn a_duplicate_param_id_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let twin = file.doc.params[0].id.clone();
    file.doc.params[1].id = twin.clone();

    assert_eq!(
        load_err(&file),
        ClmLoadError::DuplicateId {
            kind: "param",
            id: twin.to_string(),
        }
    );
}

#[test]
fn a_binding_over_three_params_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let third = file.doc.params[0].id.clone();
    file.doc.bindings[0].params.push(third);

    assert!(matches!(
        load_err(&file),
        ClmLoadError::BindingParamCount { got: 3, .. }
    ));
}

#[test]
fn a_binding_over_one_param_twice_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let first = file.doc.bindings[0].params[0].clone();
    file.doc.bindings[0].params[1] = first;

    assert!(matches!(
        load_err(&file),
        ClmLoadError::BindingParamCount { got: 2, .. }
    ));
}

#[test]
fn two_bindings_under_one_key_are_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let twin = file.doc.bindings[0].clone();
    let node = twin.node.to_string();
    file.doc.bindings.push(twin);

    assert_eq!(
        load_err(&file),
        ClmLoadError::DuplicateBinding {
            node: node.clone(),
            target: "deform",
        }
    );
    let mut fragment = sample_fragment_file();
    let twin = fragment.doc.bindings[0].clone();
    fragment.doc.bindings.push(twin);
    assert_eq!(
        fragment_err(&fragment),
        ClmLoadError::DuplicateBinding {
            node,
            target: "deform",
        }
    );

    let mut file = sample().to_clm_file().unwrap();
    let mut other = file.doc.bindings[0].clone();
    other.params.truncate(1);
    other.key_positions.truncate(1);
    if let ClmBindingValues::Deform(cells) = &mut other.values {
        cells.cells.clear();
    }
    file.doc.bindings.push(other);
    assert_eq!(Model::from_clm_file(&file).unwrap().bindings().count(), 2);
}

#[test]
fn a_value_that_is_not_a_number_is_a_structured_error() {
    fn upper(file: &mut ClmFile) -> &mut ClmNode {
        file.doc
            .nodes
            .iter_mut()
            .find(|n| n.name == "Upper")
            .unwrap()
    }

    let mut file = sample().to_clm_file().unwrap();
    upper(&mut file).z_order = f32::NAN;
    let id = upper(&mut file).id.to_string();
    assert_eq!(
        load_err(&file),
        ClmLoadError::NodeNotFinite {
            node: id.clone(),
            field: "z-order",
        }
    );

    let mut file = sample().to_clm_file().unwrap();
    upper(&mut file).transform.rotation[2] = f32::INFINITY;
    assert_eq!(
        load_err(&file),
        ClmLoadError::NodeNotFinite {
            node: id.clone(),
            field: "transform",
        }
    );

    let mut file = sample().to_clm_file().unwrap();
    match &mut upper(&mut file).kind {
        ClmNodeKind::Part(p) => p.screen_tint[1] = f32::NAN,
        other => panic!("not a part: {other:?}"),
    }
    assert_eq!(
        load_err(&file),
        ClmLoadError::NodeNotFinite {
            node: id.clone(),
            field: "screen tint",
        }
    );

    let mut file = sample().to_clm_file().unwrap();
    file.doc.params[0].default = f32::NAN;
    assert_eq!(
        load_err(&file),
        ClmLoadError::ParamDefault {
            param: file.doc.params[0].id.to_string(),
        }
    );

    let mut fragment = sample_fragment_file();
    upper(&mut fragment).transform.scale[0] = f32::NAN;
    assert_eq!(
        fragment_err(&fragment),
        ClmLoadError::NodeNotFinite {
            node: id,
            field: "transform",
        }
    );
}

#[test]
fn a_dangling_binding_node_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.bindings[0].node = NodeId::new("ghost").unwrap();

    assert_eq!(
        load_err(&file),
        ClmLoadError::DanglingBindingNode { id: "ghost".into() }
    );
}

#[test]
fn a_dangling_binding_param_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.bindings[0].params[0] = ParamId::new("gone").unwrap();

    assert!(matches!(
        load_err(&file),
        ClmLoadError::DanglingParam { id, .. } if id == "gone"
    ));
}

#[test]
fn a_colour_binding_on_a_mesh_group_is_a_structured_error() {
    for (values, target) in [
        (ClmBindingValues::Opacity(ClmCells::default()), "opacity"),
        (ClmBindingValues::TintR(ClmCells::default()), "tintr"),
        (ClmBindingValues::TintG(ClmCells::default()), "tintg"),
        (ClmBindingValues::TintB(ClmCells::default()), "tintb"),
        (
            ClmBindingValues::ScreenTintR(ClmCells::default()),
            "screentintr",
        ),
        (
            ClmBindingValues::ScreenTintG(ClmCells::default()),
            "screentintg",
        ),
        (
            ClmBindingValues::ScreenTintB(ClmCells::default()),
            "screentintb",
        ),
    ] {
        let mut file = sample().to_clm_file().unwrap();
        let bend = node_named(&file, "Bend").id.clone();
        file.doc.bindings[0].node = bend.clone();
        file.doc.bindings[0].values = values;

        assert_eq!(
            load_err(&file),
            ClmLoadError::ColorOnMeshGroup {
                node: bend.to_string(),
                target,
            }
        );
    }
}

#[test]
fn a_deform_binding_on_a_mesh_group_still_loads() {
    let mut file = sample().to_clm_file().unwrap();
    let bend = node_named(&file, "Bend").id.clone();
    file.doc.bindings[0].node = bend;
    file.doc.bindings[0].values = ClmBindingValues::Deform(ClmCells::default());

    assert!(Model::from_clm_file(&file).is_ok());
}

#[test]
fn a_deform_cell_the_mesh_cannot_take_is_a_structured_error() {
    let node = {
        let file = sample().to_clm_file().unwrap();
        node_named(&file, "Upper").id.clone()
    };

    for got in [6usize, 10] {
        let mut file = sample().to_clm_file().unwrap();
        match &mut file.doc.bindings[0].values {
            ClmBindingValues::Deform(cells) => {
                for cell in &mut cells.cells {
                    cell.value.resize(got, 0.0);
                }
            }
            other => panic!("expected a deform binding, got {other:?}"),
        }

        assert_eq!(
            load_err(&file),
            ClmLoadError::DeformCellShape {
                node: node.to_string(),
                cell: [1, 1],
                got,
                expected: 8,
            }
        );
    }
}

#[test]
fn a_slot_past_the_mesh_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    part_named(&mut file, "Upper").slots[1].vertex = Some(99);

    assert!(matches!(
        load_err(&file),
        ClmLoadError::SlotOutOfRange {
            vertex: 99,
            vertices: 4,
            ..
        }
    ));
}

#[test]
fn a_part_carrying_one_slot_id_twice_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    let slots = &mut part_named(&mut file, "Upper").slots;
    slots[1].id = slots[0].id.clone();

    assert!(matches!(
        load_err(&file),
        ClmLoadError::DuplicateSlot { slot, .. } if slot == "left"
    ));
}

#[test]
fn a_weld_pairing_a_slot_the_part_lacks_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.welds[0].pairs[0].a = SlotId::new("ghost").unwrap();

    assert!(matches!(
        load_err(&file),
        ClmLoadError::WeldUnknownSlot { slot, .. } if slot == "ghost"
    ));
}

#[test]
fn a_weld_pairing_one_slot_twice_is_a_structured_error() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.welds[0].pairs[1].a = file.doc.welds[0].pairs[0].a.clone();

    assert!(matches!(
        load_err(&file),
        ClmLoadError::WeldSlotPairedTwice { .. }
    ));
}

#[test]
fn the_other_things_a_weld_cannot_say_are_structured_errors() {
    let mut file = sample().to_clm_file().unwrap();
    file.doc.welds[0].pairs[0].weight = 1.5;
    assert!(matches!(
        load_err(&file),
        ClmLoadError::WeldWeightOutOfRange { .. }
    ));

    let mut file = sample().to_clm_file().unwrap();
    file.doc.welds[0].b = file.doc.welds[0].a.clone();
    assert!(matches!(
        load_err(&file),
        ClmLoadError::WeldSelfPaired { .. }
    ));

    let mut file = sample().to_clm_file().unwrap();
    let mut twin = file.doc.welds[0].clone();
    std::mem::swap(&mut twin.a, &mut twin.b);
    file.doc.welds.push(twin);
    assert!(matches!(
        load_err(&file),
        ClmLoadError::DuplicateWeld { .. }
    ));

    let mut file = sample().to_clm_file().unwrap();
    file.doc.welds[0].a = named(&sample(), "Sway");
    assert!(matches!(
        load_err(&file),
        ClmLoadError::WeldEndNotAPart { .. }
    ));
}

#[test]
fn the_shared_aggregate_budget_applies() {
    let file = sample().to_clm_file().unwrap();
    let mut budget = LoadBudget::new(crate::LoadLimits {
        nodes: 1,
        ..crate::LoadLimits::default()
    });

    let err = Model::from_clm_file_with_budget(&file, &mut budget).unwrap_err();

    assert!(matches!(
        err,
        LoadError::Limit(crate::LoadLimitError {
            resource: LoadResource::Nodes,
            ..
        })
    ));
}
