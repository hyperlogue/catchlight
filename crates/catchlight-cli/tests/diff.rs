//! `diff` against the committed fixtures, and encode byte-stability.

mod common;

use catchlight_cli::diff::{UNRENDERED, diff};
use catchlight_core::Model;
use catchlight_core::components::BlendMode;
use catchlight_core::formats::clm::{ClmFile, ClmNode, ClmNodeKind, ClmTransform};
use catchlight_core::id::NodeId;
use catchlight_extension_animations::{Animations, Clip, Keyframe, Lane, LaneTarget};

use common::{copy_fixture, decode, fixtures, read, tmp, write_clm};

#[test]
fn decoding_and_re_encoding_a_fixture_is_byte_stable() {
    for path in fixtures() {
        let file = decode(&path);
        let canonical =
            catchlight_core::formats::clm::encode(&file.doc, &file.textures, &file.extensions)
                .unwrap();
        assert!(
            canonical == read(&path),
            "noncanonical fixture {}",
            path.display()
        );
        let reloaded = catchlight_core::formats::clm::decode(&canonical).unwrap();
        assert!(diff(&file, &reloaded).is_empty());
        let again = catchlight_core::formats::clm::encode(
            &reloaded.doc,
            &reloaded.textures,
            &reloaded.extensions,
        )
        .unwrap();
        assert!(
            again == canonical,
            "{} is not byte-stable through a decode/encode round trip",
            path.display()
        );
    }
}

type Mutation = (&'static str, fn(&mut ClmFile));

#[test]
fn the_result_is_empty_exactly_when_the_files_are_equal() {
    let base = decode(&common::fixture("composite_masks"));

    let mutations: [Mutation; 5] = [
        ("nothing", |_| {}),
        ("a z order", |f| f.doc.nodes[1].z_order += 1.0),
        ("a texture byte", |f| f.textures[0].data[10] ^= 0xff),
        ("the texture order", |f| f.textures.swap(0, 1)),
        ("a dropped node", |f| {
            f.doc.nodes.pop();
        }),
    ];

    for (what, mutate) in mutations {
        let mut changed = base.clone();
        mutate(&mut changed);
        let lines = diff(&base, &changed);
        assert_eq!(
            lines.is_empty(),
            base == changed,
            "changing {what} gave {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == UNRENDERED),
            "changing {what} was reported only as {UNRENDERED}"
        );
    }
}

#[test]
fn a_changed_field_is_named_with_its_before_and_after() {
    let mut changed = decode(&common::fixture("composite_masks"));
    changed.doc.nodes[1].z_order = 2.5;
    changed.doc.nodes[1].name = "Backdrop".into();

    let base = decode(&common::fixture("composite_masks"));
    assert_eq!(
        diff(&base, &changed),
        vec![
            "~ node node-1 name: \"BG\" -> \"Backdrop\"".to_string(),
            "~ node node-1 z_order: -10 -> 2.5".to_string(),
        ]
    );
}

#[test]
fn an_added_and_a_removed_node_are_named_by_id() {
    let base = decode(&common::fixture("mip_checker"));

    let mut changed = base.clone();
    changed.doc.nodes.push(ClmNode {
        id: NodeId::new("root/group-newcomer").unwrap(),
        parent: Some(NodeId::new("root").unwrap()),
        name: "Newcomer".into(),
        enabled: true,
        z_order: 0.0,
        transform: ClmTransform::default(),
        lock_to_root: false,
        kind: ClmNodeKind::Group,
    });
    changed.doc.nodes.remove(1);

    assert_eq!(
        diff(&base, &changed),
        vec![
            "+ node root/group-newcomer".to_string(),
            "- node node-1".to_string(),
        ]
    );
}

#[test]
fn a_node_that_changes_kind_reports_the_fields_that_came_and_went() {
    let base = decode(&common::fixture("mip_checker"));
    let mut changed = base.clone();
    changed.doc.nodes[1].kind = ClmNodeKind::Group;

    let lines = diff(&base, &changed);
    assert!(lines.contains(&"~ node node-1 kind: Part -> Group".to_string()));
    assert!(lines.contains(&"~ node node-1 albedo: \"tex-0\" -> (none)".to_string()));
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("~ node node-1 mesh.verts: 8 values"))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("~ node node-1 opacity: 1 -> (none)"))
    );
}

#[test]
fn a_texture_is_compared_byte_for_byte() {
    let base = decode(&common::fixture("mip_checker"));
    let mut changed = base.clone();
    changed.textures[0].data.push(0);

    let lines = diff(&base, &changed);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("~ texture tex-0 data: 2421 bytes"),
        "{lines:?}"
    );
    assert!(lines[0].contains("-> 2422 bytes"), "{lines:?}");
}

#[test]
fn a_texture_of_the_same_length_with_different_bytes_still_differs() {
    let base = decode(&common::fixture("mip_checker"));
    let mut changed = base.clone();
    changed.textures[0].data[100] ^= 0xff;

    let lines = diff(&base, &changed);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("~ texture tex-0 data: 2421 bytes"),
        "{lines:?}"
    );
}

#[test]
fn a_binding_is_keyed_by_its_params_node_and_target() {
    let base = decode(&common::fixture("welded_seam"));
    let mut changed = base.clone();
    changed.doc.bindings[0].interpolate_mode =
        catchlight_core::interpolate::InterpolateMode::Stepped;

    assert_eq!(
        diff(&base, &changed),
        vec![
            "~ binding [param-0] \"node-1\" deform interpolate_mode: Linear -> Stepped".to_string()
        ]
    );
}

#[test]
fn a_weld_is_keyed_by_the_two_parts_it_joins() {
    let base = decode(&common::fixture("welded_seam"));
    let mut changed = base.clone();
    changed.doc.welds[0].pairs[0].weight = 0.25;

    let lines = diff(&base, &changed);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("~ weld node-1 <-> node-2 pairs: 3 pairs"),
        "{lines:?}"
    );

    let mut swapped = base.clone();
    let weld = &mut swapped.doc.welds[0];
    std::mem::swap(&mut weld.a, &mut weld.b);
    for pair in &mut weld.pairs {
        std::mem::swap(&mut pair.a, &mut pair.b);
    }
    let lines = diff(&base, &swapped);
    assert!(
        lines
            .iter()
            .all(|l| !l.starts_with('+') && !l.starts_with('-')),
        "an end swap is not an added and a removed weld: {lines:?}"
    );
}

#[test]
fn an_animation_is_keyed_by_name() {
    let base = decode(&common::fixture("welded_seam"));
    let mut model = Model::from_clm_file(&base).unwrap();
    let nod = |value: f32| Clip {
        length: 12,
        lanes: vec![Lane::new(
            LaneTarget::Param(base.doc.params[0].id.clone()),
            catchlight_core::interpolate::InterpolateMode::Linear,
            vec![Keyframe { frame: 0, value }],
        )],
        ..Clip::new("nod")
    };
    *model.extension_or_default::<Animations>().unwrap() = Animations::from_clips([nod(1.0)]);
    let with_one = model.to_clm_file().unwrap();
    model
        .extension_mut::<Animations>()
        .unwrap()
        .insert(nod(0.5));
    let changed = model.to_clm_file().unwrap();

    let added = diff(&base, &with_one);
    assert_eq!(added.len(), 2, "{added:?}");
    assert_eq!(added[0], "+ animation \"nod\"");
    assert!(added[1].starts_with("+ extension catchlight.animations"));
    let edited = diff(&with_one, &changed);
    assert!(
        edited[0].starts_with("~ animation \"nod\" lane[param \""),
        "{edited:?}"
    );
}

#[test]
fn a_pure_reorder_is_reported_as_one() {
    let base = decode(&common::fixture("composite_masks"));
    let mut changed = base.clone();
    changed.textures.swap(0, 1);

    assert_eq!(
        diff(&base, &changed),
        vec![
            "~ order textures: the textures both files carry are in a different order".to_string()
        ]
    );
}

#[test]
fn the_exit_status_says_whether_they_differ() {
    let dir = tmp("diff-exit-status");
    let same = copy_fixture("mip_checker", &dir);
    let (code, out, _) = common::run(&["diff", same.to_str().unwrap(), same.to_str().unwrap()]);
    assert_eq!(code, 0, "identical files exit 0");
    assert_eq!(out, "");

    let mut edited = decode(&same);
    edited.doc.nodes[1].kind = ClmNodeKind::Group;
    let other = write_clm(&dir, "edited", &edited);
    let (code, out, _) = common::run(&["diff", same.to_str().unwrap(), other.to_str().unwrap()]);
    assert_eq!(code, 1, "differing files exit 1");
    assert!(out.contains("~ node node-1 kind: Part -> Group"), "{out}");

    let (code, _, err) = common::run(&["diff", same.to_str().unwrap(), "no-such-file.clm"]);
    assert_eq!(code, 2, "an error exits 2");
    assert!(err.contains("no-such-file.clm"), "{err}");
}

#[test]
fn the_help_documents_the_id_charset_and_the_exit_statuses() {
    let (code, out, _) = common::run(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("[A-Za-z0-9_./-]"), "{out}");
    assert!(out.contains("starts with none of `.`, `/` or `-`"), "{out}");
    assert!(
        out.contains("1    `diff` only: the two files differ"),
        "{out}"
    );
    assert!(
        out.contains("141  a reader closed the output pipe"),
        "{out}"
    );
}

#[test]
fn an_enum_field_renders_as_its_wire_name() {
    let base = decode(&common::fixture("mip_checker"));
    let mut changed = base.clone();
    if let ClmNodeKind::Part(part) = &mut changed.doc.nodes[1].kind {
        part.blend_mode = BlendMode::Multiply;
    } else {
        panic!("node-1 should be a part");
    }

    assert_eq!(
        diff(&base, &changed),
        vec!["~ node node-1 blend_mode: Normal -> Multiply".to_string()]
    );
}
