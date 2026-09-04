#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Which texture a part draws, including none of them.
//!
//! `texture` on a patch means "point at this one" and an absent field means
//! "unchanged", so the two of them leave no way to say "draw nothing".
//! `clear_texture` is that way, and these pin what it does to the model and to
//! the texture it was the last part holding.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::{
    Command, ErrorCode, NodeId, NodeKindArg, NodePatch, Reply, Request, ResponseBody, TexId,
};
use catchlight_editor_server::{Editor, Storage};

/// A store that is only a map, so a texture needs no filesystem.
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

/// A 1×1 opaque PNG, so `texture_add` has something that decodes.
const PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xff, 0xff, 0x3f,
    0x00, 0x05, 0xfe, 0x02, 0xfe, 0xdc, 0xcc, 0x59, 0xe7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

struct Fixture {
    editor: Editor,
    session: catchlight_editor_protocol::SessionId,
    next: u64,
}

impl Fixture {
    /// An editor whose store holds one image under each of the given keys.
    fn new(keys: &[&str]) -> Self {
        let store = MemStorage::default();
        for key in keys {
            store.write(key, PIXEL_PNG).unwrap();
        }
        let editor = Editor::with_storage(Arc::new(store));
        let mut fixture = Self {
            editor,
            session: catchlight_editor_protocol::SessionId(0),
            next: 1,
        };
        fixture.session = match fixture.body(Command::SessionNew { name: None }) {
            ResponseBody::Session { session } => session,
            other => panic!("{other:?}"),
        };
        fixture
    }

    fn reply(&mut self, command: Command) -> Reply {
        self.next += 1;
        self.editor.handle(Request {
            id: self.next,
            command,
        })
    }

    fn body(&mut self, command: Command) -> ResponseBody {
        match self.reply(command) {
            Reply::Ok { body, .. } => body,
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    /// A part under the root, with a texture of its own.
    fn part(&mut self, key: &str) -> (NodeId, TexId) {
        let session = self.session;
        let node = match self.body(Command::NodeAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            kind: NodeKindArg::Part,
            name: None,
            node: None,
        }) {
            ResponseBody::Node { node, .. } => node,
            other => panic!("{other:?}"),
        };
        let texture = match self.body(Command::TextureAdd {
            session,
            node: node.clone(),
            path: key.to_string(),
            texture: None,
        }) {
            ResponseBody::Texture { texture, .. } => texture,
            other => panic!("{other:?}"),
        };
        (node, texture)
    }

    fn set(&mut self, node: &NodeId, patch: NodePatch) -> ResponseBody {
        let session = self.session;
        self.body(Command::NodeSet {
            session,
            node: node.clone(),
            patch,
        })
    }

    fn textures(&mut self) -> Vec<TexId> {
        let session = self.session;
        match self.body(Command::TextureList { session }) {
            ResponseBody::Textures { textures } => textures.into_iter().map(|t| t.id).collect(),
            other => panic!("{other:?}"),
        }
    }

    fn drawn(&mut self, node: &NodeId) -> Option<TexId> {
        let session = self.session;
        match self.body(Command::NodeInfo {
            session,
            node: node.clone(),
        }) {
            ResponseBody::NodeInfo { node } => node.texture.clone(),
            other => panic!("{other:?}"),
        }
    }
}

/// The whole point: a part can be told to draw nothing, and saying so takes
/// the texture it was the last one holding — the same rule every other
/// unmapping obeys, reported the same way.
#[test]
fn clearing_a_parts_texture_drops_the_texture_nothing_else_draws() {
    let mut f = Fixture::new(&["hair.png"]);
    let (part, texture) = f.part("hair.png");
    assert_eq!(f.drawn(&part), Some(texture.clone()));

    let dropped = match f.set(
        &part,
        NodePatch {
            clear_texture: true,
            ..NodePatch::default()
        },
    ) {
        ResponseBody::Node { dropped, .. } => dropped,
        other => panic!("{other:?}"),
    };

    assert_eq!(dropped, vec![texture], "the reply names what the edit took");
    assert_eq!(f.drawn(&part), None, "the part draws nothing now");
    assert!(f.textures().is_empty(), "and the model carries no texture");
}

/// Undo puts it back, because clearing is an ordinary document edit and not a
/// second path around the history.
#[test]
fn clearing_a_texture_is_one_undoable_edit() {
    let mut f = Fixture::new(&["hair.png"]);
    let (part, texture) = f.part("hair.png");
    f.set(
        &part,
        NodePatch {
            clear_texture: true,
            ..NodePatch::default()
        },
    );

    let session = f.session;
    f.body(Command::Undo { session });

    assert_eq!(f.drawn(&part), Some(texture));
}

/// `clear_texture` wins over `texture`: one says "draw none" and the other
/// "draw this one", so a patch carrying both says "none".
#[test]
fn clearing_beats_pointing_when_a_patch_carries_both() {
    let mut f = Fixture::new(&["hair.png", "skin.png"]);
    let (hair, _) = f.part("hair.png");
    let (skin, skin_tex) = f.part("skin.png");

    f.set(
        &hair,
        NodePatch {
            texture: Some(skin_tex.clone()),
            clear_texture: true,
            ..NodePatch::default()
        },
    );

    assert_eq!(f.drawn(&hair), None);
    assert_eq!(
        f.drawn(&skin),
        Some(skin_tex),
        "the other part is untouched"
    );
}

/// A patch that carries neither is not an edit to the albedo at all: a name
/// change on a part leaves what it draws alone.
#[test]
fn a_patch_that_names_no_texture_leaves_the_one_the_part_draws() {
    let mut f = Fixture::new(&["hair.png"]);
    let (part, texture) = f.part("hair.png");

    f.set(
        &part,
        NodePatch {
            name: Some("Fringe".into()),
            ..NodePatch::default()
        },
    );

    assert_eq!(f.drawn(&part), Some(texture));
}

/// Clearing on a node that could never draw is ignored, the way every other
/// kind-specific field on a patch is — not an error.
#[test]
fn clearing_on_a_node_that_is_not_a_part_is_ignored() {
    let mut f = Fixture::new(&[]);
    let session = f.session;
    let group = match f.body(Command::NodeAdd {
        session,
        parent: NodeId::new("root").unwrap(),
        kind: NodeKindArg::Group,
        name: None,
        node: None,
    }) {
        ResponseBody::Node { node, .. } => node,
        other => panic!("{other:?}"),
    };

    let dropped = match f.set(
        &group,
        NodePatch {
            clear_texture: true,
            ..NodePatch::default()
        },
    ) {
        ResponseBody::Node { dropped, .. } => dropped,
        other => panic!("{other:?}"),
    };
    assert!(dropped.is_empty());
}

/// Pointing at a texture the model does not carry is still refused, and
/// `clear_texture` does not smuggle a bad Id past that check by winning.
#[test]
fn pointing_at_an_unknown_texture_is_still_an_error() {
    let mut f = Fixture::new(&["hair.png"]);
    let (part, _) = f.part("hair.png");
    let session = f.session;

    assert!(matches!(
        f.reply(Command::NodeSet {
            session,
            node: part,
            patch: NodePatch {
                texture: Some(TexId::new("tex-deadbeef").unwrap()),
                ..NodePatch::default()
            },
        }),
        Reply::Err {
            code: ErrorCode::NoTexture,
            ..
        }
    ));
}
