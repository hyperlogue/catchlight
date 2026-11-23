#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! An add may name the Id it makes.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, NodeKind, NodeKindArg, ParamId, Reply, Request, ResponseBody,
    SessionId, TexId, TextureEncoding,
};
use catchlight_editor_server::{Attachments, Editor, Storage};

#[derive(Debug, Default)]
struct MemStorage(Mutex<HashMap<String, Vec<u8>>>);

impl Storage for MemStorage {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, key.to_string()))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }
}

const PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xff, 0xff, 0x3f,
    0x00, 0x05, 0xfe, 0x02, 0xfe, 0xdc, 0xcc, 0x59, 0xe7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

fn editor() -> (Editor, SessionId) {
    let ed = Editor::with_storage(Arc::new(MemStorage::default()));
    let session = match body(
        &ed,
        1,
        Command::SessionNew {
            source: None,
            name: None,
        },
    ) {
        ResponseBody::Session { session } => session,
        other => panic!("{other:?}"),
    };
    (ed, session)
}

fn reply(ed: &Editor, id: u64, command: Command) -> Reply {
    let mut attachments = Attachments::none();
    if matches!(command, Command::TextureAdd { .. }) {
        attachments.insert("texture", PIXEL_PNG.to_vec());
    }
    ed.handle_with(Request { id, command }, attachments).0
}

fn body(ed: &Editor, id: u64, command: Command) -> ResponseBody {
    match reply(ed, id, command) {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn node_of(b: ResponseBody) -> NodeId {
    match b {
        ResponseBody::Node { node, .. } => node,
        other => panic!("expected a node, got {other:?}"),
    }
}

#[test]
fn an_add_creates_under_the_id_it_was_given() {
    let (ed, session) = editor();
    let root = NodeId::new("root").unwrap();

    let hair = NodeId::new("hair").unwrap();
    assert_eq!(
        node_of(body(
            &ed,
            2,
            Command::NodeAdd {
                session,
                parent: root.clone(),
                kind: NodeKindArg::Part,
                name: Some("Hair".into()),
                node: Some(hair.clone()),
            }
        )),
        hair,
    );

    let pull = ParamId::new("head.pull").unwrap();
    assert!(matches!(
        body(
            &ed,
            3,
            Command::ParamAdd {
                session,
                name: "Pull".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                param: Some(pull.clone()),
            }
        ),
        ResponseBody::Param { param } if param == pull,
    ));

    let albedo = TexId::new("hair.albedo").unwrap();
    assert!(matches!(
        body(
            &ed,
            4,
            Command::TextureAdd {
                session,
                node: hair.clone(),
                encoding: TextureEncoding::default(),
                texture: Some(albedo.clone()),
            }
        ),
        ResponseBody::Texture { texture, .. } if texture == albedo,
    ));

    let response = NodeId::new("hair.swing").unwrap();
    assert_eq!(
        node_of(body(
            &ed,
            5,
            Command::NodeAdd {
                session,
                parent: hair.clone(),
                name: None,
                kind: NodeKindArg::MotionResponse,
                node: Some(response.clone()),
            }
        )),
        response,
    );

    assert!(matches!(
        body(
            &ed,
            6,
            Command::NodeInfo {
                session,
                node: response,
            }
        ),
        ResponseBody::NodeInfo { node } if node.kind == NodeKind::MotionResponse && node.parent == Some(hair),
    ));
    assert!(matches!(
        body(&ed, 7, Command::TextureList { session }),
        ResponseBody::Textures { textures } if textures.len() == 1 && textures[0].id == albedo,
    ));
}

#[test]
fn an_id_the_model_already_carries_is_refused_under_its_own_code() {
    let (ed, session) = editor();
    let hair = NodeId::new("hair").unwrap();
    let pull = ParamId::new("pull").unwrap();
    let albedo = TexId::new("albedo").unwrap();

    let part = || Command::NodeAdd {
        session,
        parent: NodeId::new("root").unwrap(),
        kind: NodeKindArg::Part,
        name: None,
        node: Some(hair.clone()),
    };
    let param = || Command::ParamAdd {
        session,
        name: "Pull".into(),
        min: 0.0,
        max: 1.0,
        default: 0.0,
        param: Some(pull.clone()),
    };
    let texture = || Command::TextureAdd {
        session,
        node: hair.clone(),
        encoding: TextureEncoding::default(),
        texture: Some(albedo.clone()),
    };

    body(&ed, 2, part());
    body(&ed, 3, param());
    body(&ed, 4, texture());

    for (id, command) in [(5, part()), (6, param()), (7, texture())] {
        match reply(&ed, id, command) {
            Reply::Err { code, .. } => assert_eq!(code, ErrorCode::DuplicateId, "request {id}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    assert!(matches!(
        body(&ed, 8, Command::Status { session }),
        ResponseBody::Status { status }
            if status.param_count == 1 && status.texture_count == 1 && status.node_count == 2,
    ));
}

/// A string outside the Id charset never reaches a command at all — it is
/// refused where the request is decoded, which is a different answer from an
/// Id that is merely taken.
#[test]
fn an_id_outside_the_charset_is_a_bad_request_not_a_duplicate() {
    let (_ed, session) = editor();
    let line = format!(
        r#"{{"id":9,"cmd":"node_add","session":{},"parent":"root","kind":"part","node":"has space"}}"#,
        session.0
    );
    let err = serde_json::from_str::<Request>(&line).unwrap_err();
    assert!(err.to_string().contains("invalid byte"), "{err}");
}

#[test]
fn an_add_with_no_id_still_draws_one() {
    let (ed, session) = editor();
    let node = node_of(body(
        &ed,
        2,
        Command::NodeAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        },
    ));
    assert!(node.as_str().starts_with("root/part-"), "{node}");
}
