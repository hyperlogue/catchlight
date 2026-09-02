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
    Command, CommandKind, ErrorCode, NodeId, NodeKindArg, ParamId, Presence, Reply, Request,
    ResponseBody, SeamAddr, SeamId, SessionId, SlotId, COMMAND_KINDS,
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
                seam: seam.clone(),
            });
            for slot in [&left, &right] {
                fixture.step(Command::SlotAdd {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                    slot: slot.clone(),
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
        Command::TextureList { session },
        Command::ParamList { session },
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
    // `check` reads the same model, warnings and all.
    match ok(f.agree(Command::Check { session })) {
        ResponseBody::Warnings { .. } => {}
        other => panic!("expected Warnings, got {other:?}"),
    }
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
