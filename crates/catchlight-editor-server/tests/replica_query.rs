#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A replica answers the model-only reads exactly as the editor does.
//!
//! A browser tab holds a copy of the session's model and serves
//! `CommandKind::ReplicaQuery` from it without a round trip. That is only
//! trustworthy if the two answers are the same bytes, so every one of them is
//! asked twice here — once of the editor, once of `replica_reply` against the
//! model the editor is holding — and the whole envelope is compared.

use std::collections::BTreeSet;
use std::io::Cursor;

use catchlight_core::formats::clm::TextureEncoding;
use catchlight_editor_protocol::{
    BindingInfo, Command, CommandKind, ErrorCode, NodeId, NodeKindArg, NodePatch, ParamId,
    Presence, Reply, Request, ResponseBody, SeamAddr, SeamId, SessionId, SlotId, COMMAND_KINDS,
};
use catchlight_editor_server::{replica_query, replica_reply, Editor};

fn ok(reply: Reply) -> ResponseBody {
    match reply {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn json(reply: &Reply) -> serde_json::Value {
    serde_json::to_value(reply).expect("a reply serializes")
}

/// A 2x3 PNG, so `texture_list` reports dimensions somebody had to decode.
fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(2, 3, image::Rgba([200, 40, 40, 255]))
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("png encodes");
    bytes
}

/// Everything the seven reads have to chew on: a tree with two parts, a param
/// with a binding, a texture, two welded seams, and one slot nothing fills.
struct Fixture {
    editor: Editor,
    session: SessionId,
    body_part: NodeId,
    group: NodeId,
    next: std::cell::Cell<u64>,
}

impl Fixture {
    fn build() -> Self {
        let editor = Editor::new();
        let next = std::cell::Cell::new(1);
        let session = match ok(editor.handle(Request {
            id: 0,
            command: Command::SessionNew {
                name: Some("replica".into()),
            },
        })) {
            ResponseBody::Session { session } => session,
            other => panic!("expected Session, got {other:?}"),
        };
        let fixture = Self {
            editor,
            session,
            body_part: NodeId::new("placeholder").unwrap(),
            group: NodeId::new("placeholder").unwrap(),
            next,
        };

        let root = match fixture.step(Command::NodeTree { session }) {
            ResponseBody::Tree { root } => root.id,
            other => panic!("expected Tree, got {other:?}"),
        };
        let group = fixture.node(Command::NodeAdd {
            session,
            parent: root.clone(),
            kind: NodeKindArg::Group,
            name: Some("Torso".into()),
            node: None,
        });
        let body_part = fixture.part(&group, "Body");
        let skirt = fixture.part(&group, "Skirt");

        // A param with a binding, so `param_list` reports a binding count.
        let param = match fixture.step(Command::ParamAdd {
            session,
            name: "Pull".into(),
            min: -1.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
            param: None,
        }) {
            ResponseBody::Param { param } => param,
            other => panic!("expected Param, got {other:?}"),
        };
        fixture.step(Command::BindingKey {
            session,
            params: catchlight_editor_protocol::BindingParams::one(param),
            node: body_part.clone(),
            target: "tx".into(),
            cell: [1, 0],
            value: 12.0,
        });

        // A texture on a part, from bytes: the browser's own upload path.
        fixture
            .editor
            .add_texture_bytes(session, &body_part, TextureEncoding::Png, png())
            .expect("the part takes a texture");

        // Two seams, welded. `left` is filled on both ends; `right` is left
        // empty on the skirt, so `unfilled_slots` has something to report.
        let (collar, hem) = (SeamId::new("collar").unwrap(), SeamId::new("hem").unwrap());
        let (left, right) = (SlotId::new("left").unwrap(), SlotId::new("right").unwrap());
        for (node, seam) in [(&body_part, &collar), (&skirt, &hem)] {
            fixture.step(Command::SeamAdd {
                session,
                node: node.clone(),
                seam: Some(seam.clone()),
            });
            for slot in [&left, &right] {
                fixture.step(Command::SlotAdd {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                    slot: Some(slot.clone()),
                });
            }
        }
        for (i, slot) in [&left, &right].into_iter().enumerate() {
            fixture.step(Command::SlotFill {
                session,
                node: body_part.clone(),
                seam: collar.clone(),
                slot: slot.clone(),
                vertex: i as u32,
            });
        }
        fixture.step(Command::SlotFill {
            session,
            node: skirt.clone(),
            seam: hem.clone(),
            slot: left.clone(),
            vertex: 2,
        });
        fixture.step(Command::WeldSet {
            session,
            a: SeamAddr {
                node: body_part.clone(),
                seam: collar,
            },
            b: SeamAddr {
                node: skirt,
                seam: hem,
            },
            weights: Vec::new(),
        });

        Self {
            body_part,
            group,
            ..fixture
        }
    }

    fn id(&self) -> u64 {
        let id = self.next.get();
        self.next.set(id + 1);
        id
    }

    fn step(&self, command: Command) -> ResponseBody {
        ok(self.editor.handle(Request {
            id: self.id(),
            command,
        }))
    }

    fn node(&self, command: Command) -> NodeId {
        match self.step(command) {
            ResponseBody::Node { node, .. } => node,
            other => panic!("expected Node, got {other:?}"),
        }
    }

    /// A part with a quad, so its seam slots have vertices to point at.
    fn part(&self, parent: &NodeId, name: &str) -> NodeId {
        let node = self.node(Command::NodeAdd {
            session: self.session,
            parent: parent.clone(),
            kind: NodeKindArg::Part,
            name: Some(name.into()),
            node: None,
        });
        self.step(Command::MeshSet {
            session: self.session,
            node: node.clone(),
            verts: vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0],
            uvs: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
            indices: vec![0, 1, 2, 0, 2, 3],
            origin: [0.0, 0.0],
        });
        node
    }

    fn params(&self) -> Vec<catchlight_editor_protocol::ParamInfo> {
        match self.step(Command::ParamList {
            session: self.session,
        }) {
            ResponseBody::Params { params } => params,
            other => panic!("expected Params, got {other:?}"),
        }
    }

    /// A node's bindings, agreed between the editor and a replica.
    fn bindings(&self, node: &NodeId) -> Vec<BindingInfo> {
        match ok(self.agree(Command::BindingList {
            session: self.session,
            node: node.clone(),
        })) {
            ResponseBody::Bindings { bindings } => bindings,
            other => panic!("expected Bindings, got {other:?}"),
        }
    }

    fn model(&self) -> catchlight_core::Model {
        self.editor
            .with_model(self.session, |m| m.clone())
            .expect("the session is open")
    }

    /// Ask `command` of the editor and of a replica of the same model, and
    /// hand back the editor's reply once the two agree in full.
    fn agree(&self, command: Command) -> Reply {
        let request = Request {
            id: self.id(),
            command,
        };
        let tag = request.command.tag();
        let server = self.editor.handle(request.clone());
        let rev = match &server {
            Reply::Ok { rev, .. } => rev.expect("a session read reports a revision"),
            // An error carries no revision; a replica knows its own either way.
            Reply::Err { .. } => self.rev(),
            other => panic!("expected Ok or Err, got {other:?}"),
        };
        let replica = replica_reply(&self.model(), rev, request);
        assert_eq!(json(&server), json(&replica), "{tag} disagreed");
        server
    }

    fn rev(&self) -> u64 {
        match self.editor.handle(Request {
            id: self.id(),
            command: Command::Status {
                session: self.session,
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Status { status },
                ..
            } => status.rev,
            other => panic!("expected Status, got {other:?}"),
        }
    }
}

/// Every command a replica is meant to answer, built against a real model so
/// each one has a real answer. Held equal to `COMMAND_KINDS` below.
fn replica_commands(session: SessionId, node: NodeId) -> Vec<Command> {
    vec![
        Command::Check { session },
        Command::NodeTree { session },
        Command::NodeInfo {
            session,
            node: node.clone(),
        },
        Command::TextureList { session },
        Command::ParamList { session },
        Command::BindingList {
            session,
            node: node.clone(),
        },
        Command::Seams { session, node },
        Command::Welds { session },
        Command::UnfilledSlots { session },
    ]
}

/// One command of every other kind, so "a replica refuses it" is checked
/// against Document, Presence, Scratch and ServerQuery alike.
fn other_commands(session: SessionId, node: NodeId) -> Vec<Command> {
    vec![
        Command::SessionList,
        Command::Status { session },
        Command::PresenceGet { session },
        Command::Preview {
            session,
            pose: Vec::new(),
            size: None,
            out: None,
        },
        Command::PresenceSet {
            session,
            presence: Presence::default(),
        },
        Command::ScratchDeform {
            session,
            node: node.clone(),
            offsets: Vec::new(),
        },
        Command::NodeDelete {
            session,
            node: node.clone(),
        },
        Command::Undo { session },
        Command::ParamDelete {
            session,
            param: ParamId::new("pull").unwrap(),
        },
        Command::Save {
            session,
            path: None,
        },
    ]
}

#[test]
fn a_replica_answers_every_model_only_read_exactly_as_the_editor_does() {
    let f = Fixture::build();
    let session = f.session;

    match ok(f.agree(Command::NodeTree { session })) {
        ResponseBody::Tree { root } => {
            assert_eq!(root.children.len(), 1, "the group hangs off the root");
            assert_eq!(root.children[0].children.len(), 2, "two parts under it");
        }
        other => panic!("expected Tree, got {other:?}"),
    }
    match ok(f.agree(Command::ParamList { session })) {
        ResponseBody::Params { params } => {
            assert_eq!(params.len(), 1);
            assert_eq!(params[0].bindings, 1, "the binding is counted");
        }
        other => panic!("expected Params, got {other:?}"),
    }
    match ok(f.agree(Command::BindingList {
        session,
        node: f.body_part.clone(),
    })) {
        ResponseBody::Bindings { bindings } => {
            assert_eq!(bindings.len(), 1, "the fixture's one binding");
            assert_eq!(bindings[0].target, "tx");
        }
        other => panic!("expected Bindings, got {other:?}"),
    }
    match ok(f.agree(Command::TextureList { session })) {
        ResponseBody::Textures { textures } => {
            assert_eq!(textures.len(), 1);
            assert_eq!((textures[0].width, textures[0].height), (2, 3));
        }
        other => panic!("expected Textures, got {other:?}"),
    }
    match ok(f.agree(Command::Seams {
        session,
        node: f.body_part.clone(),
    })) {
        ResponseBody::Seams { seams } => {
            assert_eq!(seams.len(), 1);
            assert_eq!(seams[0].slots.len(), 2);
        }
        other => panic!("expected Seams, got {other:?}"),
    }
    match ok(f.agree(Command::Welds { session })) {
        ResponseBody::Welds { welds } => assert_eq!(welds.len(), 1),
        other => panic!("expected Welds, got {other:?}"),
    }
    match ok(f.agree(Command::UnfilledSlots { session })) {
        ResponseBody::UnfilledSlots { slots } => {
            assert_eq!(slots.len(), 1, "the skirt's right slot is empty");
            assert_eq!(slots[0].slot.as_str(), "right");
        }
        other => panic!("expected UnfilledSlots, got {other:?}"),
    }
    match ok(f.agree(Command::NodeInfo {
        session,
        node: f.body_part.clone(),
    })) {
        ResponseBody::NodeInfo { node } => {
            assert_eq!(node.id, f.body_part);
            assert_eq!(node.kind, "part");
            assert_eq!(node.parent.as_ref(), Some(&f.group));
            assert_eq!(node.name, "Body");
            assert_eq!(node.vertex_count, Some(4), "the fixture's quad");
            assert_eq!(node.triangle_count, Some(2));
        }
        other => panic!("expected NodeInfo, got {other:?}"),
    }
    // `check` reads the same model, warnings and all.
    match ok(f.agree(Command::Check { session })) {
        ResponseBody::Warnings { .. } => {}
        other => panic!("expected Warnings, got {other:?}"),
    }
}

/// What an inspector shows: every value a `node_set` authored, read back under
/// the name that authored it, plus the kind and parent a patch cannot set.
#[test]
fn node_info_reads_back_what_a_node_set_wrote() {
    let f = Fixture::build();
    let session = f.session;
    let texture = match ok(f.agree(Command::TextureList { session })) {
        ResponseBody::Textures { textures } => textures[0].id.clone(),
        other => panic!("expected Textures, got {other:?}"),
    };

    f.step(Command::NodeSet {
        session,
        node: f.body_part.clone(),
        patch: NodePatch {
            name: Some("Torso skin".into()),
            translate: Some([1.0, -2.0, 3.0]),
            rotate: Some([0.0, 0.0, 0.5]),
            scale: Some([2.0, 0.5]),
            z_order: Some(4.0),
            opacity: Some(0.25),
            enabled: Some(false),
            lock_to_root: Some(true),
            blend_mode: Some("Multiply".into()),
            tint: Some([0.1, 0.2, 0.3]),
            screen_tint: Some([0.4, 0.5, 0.6]),
            mask_threshold: Some(0.75),
            ..NodePatch::default()
        },
    });

    let node = match ok(f.agree(Command::NodeInfo {
        session,
        node: f.body_part.clone(),
    })) {
        ResponseBody::NodeInfo { node } => node,
        other => panic!("expected NodeInfo, got {other:?}"),
    };

    assert_eq!(node.id, f.body_part);
    assert_eq!(node.kind, "part");
    assert_eq!(node.parent.as_ref(), Some(&f.group));
    assert_eq!(node.name, "Torso skin");
    assert_eq!(node.translate, [1.0, -2.0, 3.0]);
    assert_eq!(node.rotate, [0.0, 0.0, 0.5]);
    assert_eq!(node.scale, [2.0, 0.5]);
    assert_eq!(node.z_order, 4.0);
    assert!(!node.enabled);
    assert!(node.lock_to_root);
    assert_eq!(node.opacity, Some(0.25));
    assert_eq!(node.blend_mode.as_deref(), Some("Multiply"));
    assert_eq!(node.tint, Some([0.1, 0.2, 0.3]));
    assert_eq!(node.screen_tint, Some([0.4, 0.5, 0.6]));
    assert_eq!(node.mask_threshold, Some(0.75));
    // The part draws the fixture's texture, and carries no mesh-group or
    // composite field to report.
    assert_eq!(node.texture, Some(texture));
    assert_eq!(node.propagate_meshgroup, None);
    assert_eq!(node.mg_dynamic, None);
    assert_eq!(node.mg_translate_children, None);

    // A group is drawn by nothing, so the colour half is absent rather than
    // reported at a default a patch would then write back.
    let group = match ok(f.agree(Command::NodeInfo {
        session,
        node: f.group.clone(),
    })) {
        ResponseBody::NodeInfo { node } => node,
        other => panic!("expected NodeInfo, got {other:?}"),
    };
    assert_eq!(group.kind, "group");
    assert_eq!(group.opacity, None);
    assert_eq!(group.blend_mode, None);
    assert_eq!(group.tint, None);
    assert_eq!(group.texture, None);
    assert_eq!(group.mask_threshold, None);
    assert_eq!(group.vertex_count, None, "a group holds no mesh");
    assert_eq!(group.triangle_count, None);
}

/// The kind-specific halves the other kinds carry, so each one is read from
/// the node that actually has it.
#[test]
fn node_info_reports_the_fields_a_composite_and_a_mesh_group_carry() {
    let f = Fixture::build();
    let session = f.session;

    let composite = f.node(Command::NodeAdd {
        session,
        parent: f.group.clone(),
        kind: NodeKindArg::Composite,
        name: Some("Face".into()),
        node: None,
    });
    f.step(Command::NodeSet {
        session,
        node: composite.clone(),
        patch: NodePatch {
            propagate_meshgroup: Some(true),
            ..NodePatch::default()
        },
    });
    let mesh_group = f.node(Command::NodeAdd {
        session,
        parent: f.group.clone(),
        kind: NodeKindArg::MeshGroup,
        name: Some("Cloth".into()),
        node: None,
    });
    f.step(Command::NodeSet {
        session,
        node: mesh_group.clone(),
        patch: NodePatch {
            mg_dynamic: Some(true),
            mg_translate_children: Some(true),
            ..NodePatch::default()
        },
    });

    match ok(f.agree(Command::NodeInfo {
        session,
        node: composite,
    })) {
        ResponseBody::NodeInfo { node } => {
            assert_eq!(node.kind, "composite");
            assert_eq!(node.propagate_meshgroup, Some(true));
            assert_eq!(node.opacity, Some(1.0), "a composite is drawn");
            assert_eq!(node.texture, None, "and never carries one");
            assert_eq!(node.vertex_count, None, "and holds no mesh");
            assert_eq!(node.mg_dynamic, None);
        }
        other => panic!("expected NodeInfo, got {other:?}"),
    }
    match ok(f.agree(Command::NodeInfo {
        session,
        node: mesh_group,
    })) {
        ResponseBody::NodeInfo { node } => {
            assert_eq!(node.kind, "mesh_group");
            assert_eq!(node.mg_dynamic, Some(true));
            assert_eq!(node.mg_translate_children, Some(true));
            // A mesh group is never drawn, so it has no colour to show — but
            // it does hold a mesh, so it reports an empty one rather than
            // none at all.
            assert_eq!(node.opacity, None);
            assert_eq!(node.blend_mode, None);
            assert_eq!(node.propagate_meshgroup, None);
            assert_eq!(node.vertex_count, Some(0));
            assert_eq!(node.triangle_count, Some(0));
        }
        other => panic!("expected NodeInfo, got {other:?}"),
    }
}

/// What the "textured but its mesh has no triangles" lint stands in for. A
/// client asking whether a part has been meshed reads the counts off the node
/// rather than parsing a warning written for a person, so an unmeshed part has
/// to answer that read — as an empty mesh, which is what it has.
#[test]
fn node_info_counts_the_mesh_a_part_holds() {
    let f = Fixture::build();
    let session = f.session;
    let counts = |node: &NodeId| match ok(f.agree(Command::NodeInfo {
        session,
        node: node.clone(),
    })) {
        ResponseBody::NodeInfo { node } => (node.vertex_count, node.triangle_count),
        other => panic!("expected NodeInfo, got {other:?}"),
    };

    let bare = f.node(Command::NodeAdd {
        session,
        parent: f.group.clone(),
        kind: NodeKindArg::Part,
        name: Some("Bare".into()),
        node: None,
    });
    assert_eq!(counts(&bare), (Some(0), Some(0)));

    f.step(Command::MeshSet {
        session,
        node: bare.clone(),
        verts: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
        uvs: vec![0.0, 0.0, 1.0, 0.0, 1.0, 1.0],
        indices: vec![0, 1, 2],
        origin: [0.0, 0.0],
    });
    assert_eq!(counts(&bare), (Some(3), Some(1)));

    // And the counts follow the mesh, so a re-mesh is visible without a
    // second read of anything else.
    f.step(Command::MeshCopy {
        session,
        from: f.body_part.clone(),
        to: bare.clone(),
    });
    assert_eq!(counts(&bare), (Some(4), Some(2)), "the fixture's quad");
}

/// A refusal has to match too: a client that fell back to the editor after a
/// local error must not get a different answer.
#[test]
fn a_replica_refuses_a_bad_read_the_way_the_editor_does() {
    let f = Fixture::build();
    match f.agree(Command::Seams {
        session: f.session,
        node: f.group.clone(),
    }) {
        Reply::Err { code, .. } => assert_eq!(code, ErrorCode::Edit, "a group carries no seams"),
        other => panic!("expected Err, got {other:?}"),
    }
    match f.agree(Command::Seams {
        session: f.session,
        node: NodeId::new("nobody").unwrap(),
    }) {
        Reply::Err { code, .. } => assert_eq!(code, ErrorCode::NoNode),
        other => panic!("expected Err, got {other:?}"),
    }
    match f.agree(Command::NodeInfo {
        session: f.session,
        node: NodeId::new("nobody").unwrap(),
    }) {
        Reply::Err { code, .. } => assert_eq!(code, ErrorCode::NoNode),
        other => panic!("expected Err, got {other:?}"),
    }
    // A node that is gone reads as gone, not as a node with no bindings.
    match f.agree(Command::BindingList {
        session: f.session,
        node: NodeId::new("nobody").unwrap(),
    }) {
        Reply::Err { code, .. } => assert_eq!(code, ErrorCode::NoNode),
        other => panic!("expected Err, got {other:?}"),
    }
}

/// The kinds table is the one place a command's kind is written down, so what
/// a replica answers is held equal to it in both directions: a command
/// reclassified into `ReplicaQuery` and not implemented here fails, and one
/// reclassified out of it while still implemented fails too.
#[test]
fn what_a_replica_answers_is_exactly_the_replica_queries_of_the_kinds_table() {
    let f = Fixture::build();
    let model = f.model();
    let node = f.body_part.clone();

    let answered: BTreeSet<&str> = replica_commands(f.session, node.clone())
        .iter()
        .map(|c| c.tag())
        .collect();
    let table: BTreeSet<&str> = COMMAND_KINDS
        .iter()
        .filter(|(_, kind)| *kind == CommandKind::ReplicaQuery)
        .map(|(tag, _)| *tag)
        .collect();
    assert_eq!(answered, table);

    for command in replica_commands(f.session, node.clone()) {
        let tag = command.tag();
        assert_eq!(command.kind(), CommandKind::ReplicaQuery, "{tag}");
        // It may still fail on this particular model; what it may not do is
        // refuse to be a replica query at all.
        if let Err(e) = replica_query(&model, &command) {
            assert_ne!(e.code(), ErrorCode::BadRequest, "{tag} was not answered");
        }
    }
    for command in other_commands(f.session, node) {
        let tag = command.tag();
        assert_ne!(command.kind(), CommandKind::ReplicaQuery, "{tag}");
        let err = replica_query(&model, &command).expect_err(tag);
        assert_eq!(err.code(), ErrorCode::BadRequest, "{tag}");
        assert!(err.to_string().contains(tag), "{err} should name {tag}");
    }
}

/// The envelope: a refusal answers the request that asked, and a revision is
/// the replica's own rather than anything the model knows.
#[test]
fn a_replica_stamps_the_revision_it_was_given_on_the_request_that_asked() {
    let f = Fixture::build();
    let model = f.model();

    match replica_reply(
        &model,
        7,
        Request {
            id: 41,
            command: Command::Undo { session: f.session },
        },
    ) {
        Reply::Err { id, code, .. } => {
            assert_eq!(id, 41);
            assert_eq!(code, ErrorCode::BadRequest);
        }
        other => panic!("expected Err, got {other:?}"),
    }

    match replica_reply(
        &model,
        7,
        Request {
            id: 42,
            command: Command::ParamList { session: f.session },
        },
    ) {
        Reply::Ok { id, rev, .. } => {
            assert_eq!(id, 42);
            assert_eq!(rev, Some(7), "the caller's revision, not the model's");
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// What a binding panel is drawn from.
///
/// The grid is the product of the params' key positions and the model stores
/// only the cells somebody authored, so the read has to report both: the
/// numbers, and the holes between them. A hole is a state a rigger acts on —
/// reporting the target's identity there instead would hand a panel a number
/// to write back that nobody wrote.
#[test]
fn binding_list_reports_the_authored_grid_and_the_holes_in_it() {
    let f = Fixture::build();
    let session = f.session;
    let pull = f.params()[0].id.clone();

    // A third key position on the fixture's param. Authored cells shift and
    // the new column derives, so the grid now has a hole in the middle.
    f.step(Command::ParamKeyInsert {
        session,
        param: pull.clone(),
        value: 0.5,
    });

    // A second param, so one binding's grid spans two and is taller than one
    // row.
    let lean = match f.step(Command::ParamAdd {
        session,
        name: "Lean".into(),
        min: -1.0,
        max: 1.0,
        default: 0.0,
        key_positions: Vec::new(),
        param: None,
    }) {
        ResponseBody::Param { param } => param,
        other => panic!("expected Param, got {other:?}"),
    };
    f.step(Command::ParamKeyInsert {
        session,
        param: lean.clone(),
        value: 0.5,
    });
    let pair = catchlight_editor_protocol::BindingParams::two(pull.clone(), lean.clone());
    f.step(Command::BindingKey {
        session,
        params: pair.clone(),
        node: f.body_part.clone(),
        target: "ty".into(),
        cell: [0, 2],
        value: -3.0,
    });
    f.step(Command::BindingInterpolate {
        session,
        params: pair,
        node: f.body_part.clone(),
        target: "ty".into(),
        mode: "cubic".into(),
    });
    // A deform binding authors a vertex list rather than a number, and the
    // fixture's part is a quad. `[1, 0]` is also both params' rest cell, so
    // this authors exactly one.
    f.step(Command::DeformVertices {
        session,
        params: catchlight_editor_protocol::BindingParams::one(pull.clone()),
        node: f.body_part.clone(),
        cell: [1, 0],
        offsets: vec![1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    });

    let bindings = f.bindings(&f.body_part);
    assert_eq!(bindings.len(), 3, "tx, ty and the deform");

    // One param: one row. The 12 the fixture keyed, the identity the model
    // authors alongside a binding's first cell, and a hole between them.
    let tx = find(&bindings, "tx");
    assert_eq!(tx.param, pull);
    assert_eq!(tx.param_y, None);
    assert_eq!((tx.width, tx.height), (3, 1));
    assert_eq!(tx.interpolate, "linear", "what a new binding reads at");
    assert_eq!(tx.keys, vec![vec![Some(0.0), None, Some(12.0)]]);
    assert_eq!(tx.authored, vec![vec![true, false, true]]);

    // Two params: the grid is x's key positions by y's, indexed `[y][x]` — the
    // transpose of the `cell: [x, y]` that authored it.
    let ty = find(&bindings, "ty");
    assert_eq!(ty.param, pull);
    assert_eq!(ty.param_y.as_ref(), Some(&lean));
    assert_eq!((ty.width, ty.height), (3, 3));
    assert_eq!(
        ty.interpolate, "cubic",
        "the word `binding_interpolate` took"
    );
    assert_eq!(ty.keys[2][0], Some(-3.0), "cell [0, 2] is keys[2][0]");
    assert_eq!(ty.keys[1][1], Some(0.0), "the identity at the rest cell");
    assert_eq!(ty.keys[0], vec![None, None, None]);
    assert!(ty.authored[2][0]);
    assert!(!ty.authored[2][1]);

    // A deform cell is authored and has no scalar to report, so `keys` says
    // nothing about it and `authored` says everything.
    let deform = find(&bindings, "deform");
    assert_eq!(deform.keys, vec![vec![None, None, None]]);
    assert_eq!(deform.authored, vec![vec![false, true, false]]);

    // Every param a binding names is a param `param_list` reports, so a panel
    // reads the key positions its grid is sized by from there.
    let params: BTreeSet<String> = f
        .params()
        .iter()
        .map(|p| p.id.as_str().to_string())
        .collect();
    for b in &bindings {
        assert!(params.contains(b.param.as_str()), "{}", b.param);
        if let Some(y) = &b.param_y {
            assert!(params.contains(y.as_str()), "{y}");
        }
    }
}

/// Un-authoring a cell puts the hole back, and resetting one fills it with the
/// target's identity — so a panel that draws set and unset differently keeps
/// telling the truth across both edits.
#[test]
fn un_authoring_a_cell_reports_it_unset_again() {
    let f = Fixture::build();
    let session = f.session;
    let pull = f.params()[0].id.clone();
    let params = catchlight_editor_protocol::BindingParams::one(pull);

    // The fixture keyed one cell; the model authored the identity at the rest
    // cell alongside it, because one authored cell otherwise fills the grid.
    let bindings = f.bindings(&f.body_part);
    assert_eq!(
        find(&bindings, "tx").keys,
        vec![vec![Some(0.0), Some(12.0)]]
    );

    f.step(Command::BindingUnset {
        session,
        params: params.clone(),
        node: f.body_part.clone(),
        target: "tx".into(),
        cell: [0, 0],
    });
    let bindings = f.bindings(&f.body_part);
    let tx = find(&bindings, "tx");
    assert_eq!(tx.keys, vec![vec![None, Some(12.0)]]);
    assert_eq!(tx.authored, vec![vec![false, true]]);

    f.step(Command::BindingReset {
        session,
        params,
        node: f.body_part.clone(),
        target: "tx".into(),
        cell: [0, 0],
    });
    assert_eq!(
        find(&f.bindings(&f.body_part), "tx").keys,
        vec![vec![Some(0.0), Some(12.0)]],
        "a reset authors the target's identity"
    );
}

/// A node nothing drives.
#[test]
fn a_node_with_no_bindings_reports_an_empty_list() {
    let f = Fixture::build();
    assert!(f.bindings(&f.group).is_empty());
}

fn find<'a>(bindings: &'a [BindingInfo], target: &str) -> &'a BindingInfo {
    bindings
        .iter()
        .find(|b| b.target == target)
        .unwrap_or_else(|| panic!("no {target} binding in {bindings:?}"))
}
