#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Bytes arrive inside the command that uses them.
//!
//! Every test here drives [`Editor::handle_with`] directly, with no transport
//! under it: what a socket or an HTTP body does with the blobs is glue, and
//! this is the thing the glue feeds. The store is a map with nothing in it, so
//! a command that reached for a staged key would fail rather than quietly
//! succeed.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{
    BindingParams, Camera, Command, ErrorCode, Event, NodeId, NodeKindArg, ParamId, Reply, Request,
    ResponseBody, ScalarTarget, SessionId, TexId, TextureEncoding,
};
use catchlight_editor_server::{Attachments, Editor, Payload, Storage};

// ------------------------------------------------------------------ harness

/// A store that is only a map, so nothing here needs a filesystem.
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

fn editor() -> Editor {
    Editor::with_storage(Arc::new(MemStorage::default()))
}

fn req(id: u64, command: Command) -> Request {
    Request { id, command }
}

fn with(pairs: Vec<(&str, Vec<u8>)>) -> Attachments {
    let mut attachments = Attachments::none();
    for (name, bytes) in pairs {
        attachments.insert(name, bytes);
    }
    attachments
}

/// Send one request with attachments and keep both halves of the answer.
fn send(ed: &Editor, id: u64, command: Command, at: Attachments) -> (Reply, Option<Payload>) {
    ed.handle_with(req(id, command), at)
}

fn ok(reply: Reply) -> ResponseBody {
    match reply {
        Reply::Ok { body, .. } => body,
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn rev_of(reply: &Reply) -> u64 {
    match reply {
        Reply::Ok { rev, .. } => rev.expect("a session command reports its revision"),
        other => panic!("expected Ok, got {other:?}"),
    }
}

fn code(reply: Reply) -> ErrorCode {
    match reply {
        Reply::Err { code, .. } => code,
        other => panic!("expected Err, got {other:?}"),
    }
}

fn new_session(ed: &Editor, id: u64) -> SessionId {
    match ok(ed.handle(req(id, Command::SessionNew { name: None }))) {
        ResponseBody::Session { session } => session,
        other => panic!("expected Session, got {other:?}"),
    }
}

fn root_of(ed: &Editor, id: u64, session: SessionId) -> NodeId {
    match ok(ed.handle(req(id, Command::NodeTree { session }))) {
        ResponseBody::Tree { root } => root.id,
        other => panic!("expected Tree, got {other:?}"),
    }
}

fn node_count(ed: &Editor, id: u64, session: SessionId) -> u32 {
    match ok(ed.handle(req(id, Command::Status { session }))) {
        ResponseBody::Status { status } => status.node_count,
        other => panic!("expected Status, got {other:?}"),
    }
}

fn add_part(ed: &Editor, id: u64, session: SessionId, parent: &NodeId, node: &str) -> NodeId {
    match ok(ed.handle(req(
        id,
        Command::NodeAdd {
            session,
            parent: parent.clone(),
            kind: NodeKindArg::Part,
            name: None,
            node: Some(NodeId::new(node).unwrap()),
        },
    ))) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("expected Node, got {other:?}"),
    }
}

fn png(rgba: [u8; 4]) -> Vec<u8> {
    png_of(1, rgba)
}

/// A solid `side`x`side` PNG. A quad automesh takes its size from the image,
/// so this is also how big the part is in world units.
fn png_of(side: u32, rgba: [u8; 4]) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(side, side, image::Rgba(rgba));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut out, image::ImageFormat::Png)
        .expect("encode png");
    out.into_inner()
}

/// A complete `.clm` holding a root and one part, as bytes.
fn a_model() -> Vec<u8> {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    add_part(&ed, 3, s, &root, "torso");
    ed.with_model(s, |m| m.to_clm_bytes().unwrap()).unwrap()
}

/// A fragment: the `hat` subtree cut out of a model that also carries `head`.
///
/// Built the way an addon is built anywhere else — [`Model::extract`] on a
/// real model — so the bytes are a fragment on the wire, roots naming a parent
/// the file does not carry.
fn a_fragment() -> Vec<u8> {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let head = add_part(&ed, 3, s, &root, "head");
    add_part(&ed, 4, s, &head, "hat");
    ed.with_model(s, |m| {
        m.extract(&[NodeId::new("hat").unwrap()])
            .to_clm_bytes()
            .unwrap()
    })
    .unwrap()
}

/// A fragment whose binding drives a param the base has to supply.
fn a_fragment_needing(param: &str) -> Vec<u8> {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let hat = add_part(&ed, 3, s, &root, "hat");
    ok(ed.handle(req(
        4,
        Command::ParamAdd {
            session: s,
            name: param.into(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            key_positions: Vec::new(),
            param: Some(ParamId::new(param).unwrap()),
        },
    )));
    ok(ed.handle(req(
        5,
        Command::BindingAdd {
            session: s,
            params: BindingParams::one(ParamId::new(param).unwrap()),
            node: hat.clone(),
            target: ScalarTarget::Tx,
        },
    )));
    ed.with_model(s, |m| m.extract(&[hat]).to_clm_bytes().unwrap())
        .unwrap()
}

// -------------------------------------------------------- the attachment gate

#[test]
fn a_texture_arrives_as_an_attachment_under_the_encoding_it_declares() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let part = add_part(&ed, 3, s, &root, "face");

    let (reply, payload) = send(
        &ed,
        4,
        Command::TextureAdd {
            session: s,
            node: part,
            encoding: TextureEncoding::Png,
            texture: Some(TexId::new("face").unwrap()),
        },
        with(vec![("texture", png([9, 8, 7, 255]))]),
    );
    assert!(payload.is_none(), "texture_add answers with no bytes");
    match ok(reply) {
        ResponseBody::Texture { texture, .. } => assert_eq!(texture.as_str(), "face"),
        other => panic!("expected Texture, got {other:?}"),
    }
    // The encoding is the field's, not a guess: the model stores what was
    // declared, and no key was ever named.
    ed.with_model(s, |m| {
        let stored = m
            .texture(&TexId::new("face").unwrap())
            .expect("the texture is in the model");
        assert_eq!(
            stored.encoding,
            catchlight_core::formats::clm::TextureEncoding::Png
        );
    })
    .unwrap();
}

/// There is one place the bytes can come from, so a command without them is
/// refused rather than reaching for a store.
#[test]
fn a_texture_add_with_no_image_attached_is_refused() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let part = add_part(&ed, 3, s, &root, "face");

    let (reply, _) = send(
        &ed,
        4,
        Command::TextureAdd {
            session: s,
            node: part,
            encoding: TextureEncoding::Png,
            texture: None,
        },
        Attachments::none(),
    );
    assert_eq!(code(reply), ErrorCode::BadRequest);
}

#[test]
fn an_attachment_the_command_did_not_declare_is_refused() {
    let ed = editor();
    let s = new_session(&ed, 1);

    // A command that carries no bytes at all.
    let (reply, _) = send(
        &ed,
        2,
        Command::Status { session: s },
        with(vec![("model", a_model())]),
    );
    assert_eq!(code(reply), ErrorCode::BadRequest);

    // And one that carries bytes, under a name it does not take.
    let (reply, _) = send(
        &ed,
        3,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        with(vec![("document", a_model())]),
    );
    assert_eq!(code(reply), ErrorCode::BadRequest);
}

#[test]
fn a_fixed_attachment_that_did_not_arrive_is_refused() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        Attachments::none(),
    );
    assert_eq!(code(reply), ErrorCode::BadRequest);
}

/// `handle` is `handle_with` with nothing attached, so a byte-bearing command
/// sent that way is refused for the reason it should be: its bytes are missing.
#[test]
fn handle_alone_cannot_carry_a_byte_bearing_command() {
    let ed = editor();
    let s = new_session(&ed, 1);
    assert_eq!(
        code(ed.handle(req(2, Command::ImportManifest { session: s }))),
        ErrorCode::BadRequest,
    );
}

// ------------------------------------------------------------- import_file

#[test]
fn an_import_replaces_a_pristine_session_keeping_its_identity() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let before = ed.with_model(s, |m| m.identity()).unwrap();

    let seen = Arc::new(Mutex::new(Vec::new()));
    let heard = seen.clone();
    ed.subscribe(Box::new(move |event: &Event| {
        if let Event::DocumentChanged { session, rev } = event {
            heard.lock().unwrap().push((*session, *rev));
        }
    }));

    let (reply, _) = send(
        &ed,
        2,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        with(vec![("model", a_model())]),
    );
    assert_eq!(rev_of(&reply), 1, "one import is one revision");
    ok(reply);
    assert_eq!(node_count(&ed, 3, s), 2, "the imported model landed");
    assert_eq!(
        ed.with_model(s, |m| m.identity()).unwrap(),
        before,
        "the session's model is the same model in a new state",
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(s, 1)],
        "an import is a document change like any other",
    );

    // And it leaves what an open leaves: a document nobody has edited.
    assert_clean_and_unundoable(&ed, 4, s);
}

/// A pristine replace is an open, so the session it leaves is clean with
/// nothing behind it. Anything else and a tab warns about unsaved changes the
/// moment a file is opened, and one undo empties the document.
fn assert_clean_and_unundoable(ed: &Editor, id: u64, session: SessionId) {
    match ok(ed.handle(req(id, Command::Status { session }))) {
        ResponseBody::Status { status } => {
            assert!(!status.dirty, "an import leaves the session clean")
        }
        other => panic!("expected Status, got {other:?}"),
    }
    assert_eq!(
        code(ed.handle(req(id + 1, Command::Undo { session }))),
        ErrorCode::NothingToUndo,
        "an import is not an edit to undo",
    );
}

#[test]
fn an_import_over_a_session_that_holds_a_model_is_refused() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    add_part(&ed, 3, s, &root, "torso");

    let (reply, _) = send(
        &ed,
        4,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        with(vec![("model", a_model())]),
    );
    assert_eq!(code(reply), ErrorCode::NotEmpty);
    assert_eq!(node_count(&ed, 5, s), 2, "a refusal changes nothing");
}

#[test]
fn a_fragment_cannot_replace_a_model() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        with(vec![("model", a_fragment())]),
    );
    // Whole-model replacement reads the bytes as a complete document, and a
    // fragment's roots name a parent, so the reader refuses it.
    assert_eq!(code(reply), ErrorCode::Edit);
    assert_eq!(node_count(&ed, 3, s), 1);
}

#[test]
fn a_fragment_installs_under_the_parent_the_command_names() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let head = add_part(&ed, 3, s, &root, "head");

    // The fragment's own root names `head` as its parent; so does the command.
    let (reply, _) = send(
        &ed,
        4,
        Command::ImportFile {
            session: s,
            parent: Some(head.clone()),
        },
        with(vec![("model", a_fragment())]),
    );
    ok(reply);

    // Ids travel verbatim, and the subtree hangs where the command said.
    match ok(ed.handle(req(
        5,
        Command::NodeInfo {
            session: s,
            node: NodeId::new("hat").unwrap(),
        },
    ))) {
        ResponseBody::NodeInfo { node } => {
            assert_eq!(node.parent.as_ref().map(|p| p.as_str()), Some("head"))
        }
        other => panic!("expected NodeInfo, got {other:?}"),
    }

    // A subtree install is an ordinary edit, unlike a pristine replace: it
    // dirties the session and one undo takes exactly it back.
    match ok(ed.handle(req(6, Command::Status { session: s }))) {
        ResponseBody::Status { status } => assert!(status.dirty),
        other => panic!("expected Status, got {other:?}"),
    }
    ok(ed.handle(req(7, Command::Undo { session: s })));
    assert_eq!(node_count(&ed, 8, s), 2);
}

/// The wire's `parent` wins over the parent a fragment's roots name.
#[test]
fn a_fragment_is_re_parented_onto_the_node_the_command_names() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let elsewhere = add_part(&ed, 3, s, &root, "belt");

    let (reply, _) = send(
        &ed,
        4,
        Command::ImportFile {
            session: s,
            // The fragment's root names `head`, which this model does not
            // have at all.
            parent: Some(elsewhere),
        },
        with(vec![("model", a_fragment())]),
    );
    ok(reply);
    match ok(ed.handle(req(
        5,
        Command::NodeInfo {
            session: s,
            node: NodeId::new("hat").unwrap(),
        },
    ))) {
        ResponseBody::NodeInfo { node } => {
            assert_eq!(node.parent.as_ref().map(|p| p.as_str()), Some("belt"))
        }
        other => panic!("expected NodeInfo, got {other:?}"),
    }
}

#[test]
fn an_id_the_session_already_carries_refuses_the_install() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    let head = add_part(&ed, 3, s, &root, "head");
    add_part(&ed, 4, s, &head, "hat");

    let (reply, _) = send(
        &ed,
        5,
        Command::ImportFile {
            session: s,
            parent: Some(head),
        },
        with(vec![("model", a_fragment())]),
    );
    assert_eq!(code(reply), ErrorCode::DuplicateId);
    assert_eq!(node_count(&ed, 6, s), 3, "a refused install moves nothing");
}

#[test]
fn a_requirement_the_session_does_not_have_refuses_the_install() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);

    let (reply, _) = send(
        &ed,
        3,
        Command::ImportFile {
            session: s,
            parent: Some(root),
        },
        with(vec![("model", a_fragment_needing("tilt"))]),
    );
    assert_eq!(code(reply), ErrorCode::NoParam);
    assert_eq!(node_count(&ed, 4, s), 1);
}

#[test]
fn a_parent_the_session_does_not_carry_refuses_the_install() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportFile {
            session: s,
            parent: Some(NodeId::new("nowhere").unwrap()),
        },
        with(vec![("model", a_fragment())]),
    );
    assert_eq!(code(reply), ErrorCode::NoNode);
}

// ----------------------------------------------------------- import_manifest

const MANIFEST: &str = r#"{"name":"akari",
    "textures":[{"id":"face","path":"images/face.png"}],
    "nodes":[{"id":"face","kind":"part","texture":"face","mesh":{"auto":"quad"}}]}"#;

#[test]
fn a_manifest_and_its_images_build_the_model_they_describe() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportManifest { session: s },
        with(vec![
            ("manifest", MANIFEST.as_bytes().to_vec()),
            ("texture:images/face.png", png([200, 30, 30, 255])),
        ]),
    );
    ok(reply);

    // The part the manifest names is there, drawing the image that came with
    // it, and the title came off the manifest.
    assert_eq!(node_count(&ed, 3, s), 2);
    match ok(ed.handle(req(4, Command::TextureList { session: s }))) {
        ResponseBody::Textures { textures } => assert_eq!(textures.len(), 1),
        other => panic!("expected Textures, got {other:?}"),
    }
    match ok(ed.handle(req(5, Command::Status { session: s }))) {
        ResponseBody::Status { status } => assert_eq!(status.title, "akari"),
        other => panic!("expected Status, got {other:?}"),
    }
    // A manifest import is an open too.
    assert_clean_and_unundoable(&ed, 6, s);

    // Two imports of one manifest are one model, whoever ran them.
    let again = editor();
    let t = new_session(&again, 1);
    let (reply, _) = send(
        &again,
        2,
        Command::ImportManifest { session: t },
        with(vec![
            ("manifest", MANIFEST.as_bytes().to_vec()),
            ("texture:images/face.png", png([200, 30, 30, 255])),
        ]),
    );
    ok(reply);
    assert_eq!(
        ed.with_model(s, |m| m.to_clm_bytes().unwrap()).unwrap(),
        again.with_model(t, |m| m.to_clm_bytes().unwrap()).unwrap(),
    );
}

#[test]
fn a_manifest_reference_with_no_attachment_is_refused_naming_it() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportManifest { session: s },
        with(vec![("manifest", MANIFEST.as_bytes().to_vec())]),
    );
    match reply {
        Reply::Err { code, message, .. } => {
            assert_eq!(code, ErrorCode::Manifest);
            assert!(
                message.contains("images/face.png"),
                "the refusal names the reference: {message:?}",
            );
        }
        other => panic!("expected Err, got {other:?}"),
    }
}

#[test]
fn a_manifest_import_needs_a_pristine_session_too() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let root = root_of(&ed, 2, s);
    add_part(&ed, 3, s, &root, "torso");

    let (reply, _) = send(
        &ed,
        4,
        Command::ImportManifest { session: s },
        with(vec![
            ("manifest", MANIFEST.as_bytes().to_vec()),
            ("texture:images/face.png", png([1, 1, 1, 255])),
        ]),
    );
    assert_eq!(code(reply), ErrorCode::NotEmpty);
}

/// An imported model is not a file on disk, so a bare save still has nowhere
/// to go — exactly as it did when the bytes came through a staged upload.
#[test]
fn an_import_leaves_the_session_with_no_file_to_save_to() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        with(vec![("model", a_model())]),
    );
    ok(reply);
    assert_eq!(
        code(ed.handle(req(
            3,
            Command::Save {
                session: s,
                path: None
            }
        ))),
        ErrorCode::NoSavePath,
    );
}

// ------------------------------------------------------------------ preview

#[test]
fn a_preview_answers_with_the_png_it_rendered() {
    let ed = editor();
    let s = new_session(&ed, 1);
    // 64 units across, so it is a shape a camera can be near or far from.
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportManifest { session: s },
        with(vec![
            ("manifest", MANIFEST.as_bytes().to_vec()),
            ("texture:images/face.png", png_of(64, [200, 30, 30, 255])),
        ]),
    );
    ok(reply);

    let shot = |id: u64, camera| -> Vec<u8> {
        let (reply, payload) = send(
            &ed,
            id,
            Command::Preview {
                session: s,
                pose: Vec::new(),
                size: Some([64, 48]),
                camera,
            },
            Attachments::none(),
        );
        match ok(reply) {
            ResponseBody::Preview { preview } => {
                assert_eq!([preview.width, preview.height], [64, 48]);
            }
            other => panic!("expected Preview, got {other:?}"),
        }
        let payload = payload.expect("the png is the payload");
        assert_eq!(payload.content_type, "image/png");
        payload.bytes
    };

    let framed = |height: f32| {
        Some(Camera {
            center: [0.0, 0.0],
            height,
        })
    };

    let default = shot(3, None);
    let decoded = image::load_from_memory(&default).expect("the payload is a png");
    assert_eq!((decoded.width(), decoded.height()), (64, 48));

    // The camera is the command's, so two heights are two pictures.
    let near = shot(4, framed(100.0));
    let far = shot(5, framed(400.0));
    assert_ne!(near, far, "a different camera height renders differently");
}

/// The model a session holds is untouched by rendering it.
#[test]
fn a_preview_is_still_a_read() {
    let ed = editor();
    let s = new_session(&ed, 1);
    let (reply, _) = send(
        &ed,
        2,
        Command::ImportFile {
            session: s,
            parent: None,
        },
        with(vec![("model", a_model())]),
    );
    let rev = rev_of(&reply);
    ok(reply);
    let (reply, _) = send(
        &ed,
        3,
        Command::Preview {
            session: s,
            pose: Vec::new(),
            size: Some([16, 16]),
            camera: None,
        },
        Attachments::none(),
    );
    assert_eq!(rev_of(&reply), rev, "a preview moves no revision");
    ok(reply);
}
