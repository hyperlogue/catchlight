#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A part can be re-meshed from its own texture, over the wire.
//!
//! `contour_automesh` and `grid_automesh` have always been in editor-core, and
//! only the desktop app could reach them: every other client would have had to
//! decode the image itself, agree with the editor about the UV mapping, and
//! send back a `mesh_set`. So these pin that the editor does the tracing, that
//! the result goes through the same re-fitting path a hand-authored mesh does,
//! and that every way it can refuse says which.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use catchlight_core::formats::clm::ClmMesh;
use catchlight_editor_protocol::{
    AutoMesh, Command, ErrorCode, NodeId, NodeKindArg, Reply, Request, ResponseBody, SessionId,
    TextureEncoding,
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

/// The texture every test here traces: 64×64, transparent but for an opaque
/// 32×32 square in the middle. Real alpha, and a shape whose outline and
/// bounding box are both known, so the mesh can be checked against it rather
/// than against itself.
const SIZE: u32 = 64;
const BLOB: std::ops::Range<u32> = 16..48;

fn blob_png() -> Vec<u8> {
    let mut image = image::RgbaImage::from_pixel(SIZE, SIZE, image::Rgba([0, 0, 0, 0]));
    for y in BLOB {
        for x in BLOB {
            image.put_pixel(x, y, image::Rgba([255, 128, 64, 255]));
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("the fixture encodes");
    bytes.into_inner()
}

/// A fully transparent texture of the same size: nothing to trace at any
/// threshold.
fn empty_png() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(SIZE, SIZE, image::Rgba([0, 0, 0, 0]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("the fixture encodes");
    bytes.into_inner()
}

struct Fixture {
    editor: Editor,
    session: SessionId,
    next: u64,
}

impl Fixture {
    fn new() -> Self {
        let store = MemStorage::default();
        store.write("blob.png", &blob_png()).unwrap();
        store.write("empty.png", &empty_png()).unwrap();
        let editor = Editor::with_storage(Arc::new(store));
        let mut f = Self {
            editor,
            session: SessionId(0),
            next: 0,
        };
        f.session = match f.body(Command::SessionNew { name: None }) {
            ResponseBody::Session { session } => session,
            other => panic!("{other:?}"),
        };
        f
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

    fn node(&mut self, kind: NodeKindArg) -> NodeId {
        let session = self.session;
        match self.body(Command::NodeAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            kind,
            name: None,
            node: None,
        }) {
            ResponseBody::Node { node, .. } => node,
            other => panic!("{other:?}"),
        }
    }

    /// A part drawing `key`.
    fn part(&mut self, key: &str) -> NodeId {
        let node = self.node(NodeKindArg::Part);
        let session = self.session;
        self.body(Command::TextureAdd {
            session,
            node: node.clone(),
            path: Some(key.to_string()),
            encoding: TextureEncoding::default(),
            texture: None,
        });
        node
    }

    fn auto(&mut self, node: &NodeId, mode: AutoMesh) -> Reply {
        let session = self.session;
        self.reply(Command::MeshAuto {
            session,
            node: node.clone(),
            mode,
        })
    }

    fn mesh(&self, node: &NodeId) -> ClmMesh {
        self.editor
            .with_model(self.session, |m| m.node_mesh(node).cloned())
            .unwrap()
            .expect("a part carries a mesh")
    }
}

/// `[min_x, min_y, max_x, max_y]` of a flat `[x, y, …]` list.
fn bounds(verts: &[f32]) -> [f32; 4] {
    verts
        .as_chunks::<2>()
        .0
        .iter()
        .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| {
            [
                b[0].min(p[0]),
                b[1].min(p[1]),
                b[2].max(p[0]),
                b[3].max(p[1]),
            ]
        })
}

fn triangles(mesh: &ClmMesh) -> usize {
    use catchlight_core::formats::clm::ClmIndices;
    match &mesh.indices {
        ClmIndices::U16(i) => i.len() / 3,
        ClmIndices::U32(i) => i.len() / 3,
    }
}

/// The default trace: a mesh that covers the art and nothing much else.
///
/// The square sits at texels 16..48 of 64, which is local −16..16 under the
/// centered convention; the default 4-texel margin puts the outline a little
/// outside that, and nothing should reach the edge of the texture.
#[test]
fn a_contour_trace_covers_the_art_and_not_the_transparent_border() {
    let mut f = Fixture::new();
    let part = f.part("blob.png");

    let emptied = match f.auto(&part, AutoMesh::default()) {
        Reply::Ok { body, .. } => body,
        other => panic!("{other:?}"),
    };
    // It answers the way `mesh_set` does, because it goes the same way.
    assert!(
        matches!(&emptied, ResponseBody::Emptied { node, slots } if node == &part && slots.is_empty()),
        "{emptied:?}",
    );

    let mesh = f.mesh(&part);
    assert!(mesh.verts.len() >= 6, "a traced outline has vertices");
    assert!(triangles(&mesh) > 0, "and triangles over them");
    assert_eq!(mesh.uvs.len(), mesh.verts.len());

    let [min_x, min_y, max_x, max_y] = bounds(&mesh.verts);
    assert!(
        min_x <= -16.0 && min_y <= -16.0 && max_x >= 16.0 && max_y >= 16.0,
        "the outline encloses the art: {:?}",
        bounds(&mesh.verts),
    );
    assert!(
        min_x >= -26.0 && min_y >= -26.0 && max_x <= 26.0 && max_y <= 26.0,
        "and does not sprawl to the texture's edge: {:?}",
        bounds(&mesh.verts),
    );

    // Every UV lands inside the texture, which is what says the mapping the
    // editor used is the one the texture is drawn with.
    for uv in mesh.uvs.as_chunks::<2>().0 {
        assert!(
            (-0.01..=1.01).contains(&uv[0]) && (-0.01..=1.01).contains(&uv[1]),
            "uv {uv:?} is off the texture",
        );
    }
}

/// A grid is exactly the lattice it was asked for, over the solid texels'
/// bounding box — so `cols` and `rows` reach the automesh unchanged, and the
/// mapping puts row 0 at the *top* of the texture: the rows come out with y
/// descending, which is what says the v axis was flipped on the way in.
#[test]
fn a_grid_lays_down_the_lattice_it_was_asked_for() {
    let mut f = Fixture::new();
    let part = f.part("blob.png");

    f.auto(
        &part,
        AutoMesh::Grid {
            threshold: None,
            cols: Some(3),
            rows: Some(2),
            axes_x: None,
            axes_y: None,
            margin: None,
        },
    );

    let mesh = f.mesh(&part);
    assert_eq!(mesh.verts.len() / 2, 4 * 3, "(cols + 1) × (rows + 1)");

    // The art spans texels 16..47 and the grid adds a texel of margin, so the
    // box is texels 15..49 — local ±17 under the centered convention, with v
    // increasing downward, so the first row is the largest y.
    let xs = [-17.0, -17.0 + 34.0 / 3.0, -17.0 + 68.0 / 3.0, 17.0];
    let ys = [17.0, 0.0, -17.0];
    let want: Vec<[f32; 2]> = ys
        .iter()
        .flat_map(|&y| xs.iter().map(move |&x| [x, y]))
        .collect();
    for (got, want) in mesh.verts.as_chunks::<2>().0.iter().zip(&want) {
        assert!(
            (got[0] - want[0]).abs() < 0.01 && (got[1] - want[1]).abs() < 0.01,
            "vertex {got:?} should be {want:?}",
        );
    }
}

/// A knob left off is the editor's own, so a client that wants "an automesh"
/// sends `{}` and gets what the desktop app's buttons give.
#[test]
fn an_omitted_knob_is_the_editors_default() {
    let mut f = Fixture::new();

    let spelled_out = {
        let part = f.part("blob.png");
        f.auto(
            &part,
            AutoMesh::Contour {
                threshold: Some(16),
                simplify: Some(6.0),
                margin: Some(4),
                spacing: Some(0),
                rings: Some(Vec::new()),
                min_distance: Some(0.0),
                mirror_x: None,
            },
        );
        f.mesh(&part)
    };
    let defaulted = {
        let part = f.part("blob.png");
        f.auto(&part, AutoMesh::default());
        f.mesh(&part)
    };

    assert_eq!(spelled_out.verts, defaulted.verts);
    assert_eq!(spelled_out.uvs, defaulted.uvs);

    // And a knob that *is* given changes the answer, so the defaults are not
    // simply being used for everything.
    let filled = {
        let part = f.part("blob.png");
        f.auto(
            &part,
            AutoMesh::Contour {
                threshold: None,
                simplify: None,
                margin: None,
                spacing: Some(8),
                rings: None,
                min_distance: None,
                mirror_x: None,
            },
        );
        f.mesh(&part)
    };
    assert!(
        filled.verts.len() > defaulted.verts.len(),
        "interior fill points add vertices",
    );
}

/// The mapping comes from the mesh being replaced when there is one to read,
/// so re-meshing a part built on the centered convention lands where the
/// texture already was rather than moving the art.
#[test]
fn a_part_that_already_has_a_mesh_keeps_its_mapping() {
    let mut f = Fixture::new();
    let fresh = {
        let part = f.part("blob.png");
        f.auto(&part, AutoMesh::default());
        f.mesh(&part)
    };

    let part = f.part("blob.png");
    let session = f.session;
    // A 64×64 quad on the centered convention, v increasing downward: exactly
    // what `from_texture_size` describes, so the fit has to recover it.
    f.body(Command::MeshSet {
        session,
        node: part.clone(),
        verts: vec![[-32.0, -32.0], [32.0, -32.0], [32.0, 32.0], [-32.0, 32.0]],
        uvs: vec![[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
        indices: vec![[0, 1, 2], [0, 2, 3]],
        origin: [0.0, 0.0],
    });
    f.auto(&part, AutoMesh::default());

    assert_eq!(f.mesh(&part).verts, fresh.verts);
}

/// A trace is a document edit like any other: one revision, one undo entry,
/// and the mesh it replaced comes back.
#[test]
fn a_trace_is_one_undoable_edit() {
    let mut f = Fixture::new();
    let part = f.part("blob.png");
    let session = f.session;
    f.body(Command::MeshSet {
        session,
        node: part.clone(),
        verts: vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]],
        uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        indices: vec![[0, 1, 2]],
        origin: [0.0, 0.0],
    });
    let before = f.mesh(&part);

    f.auto(&part, AutoMesh::default());
    assert_ne!(f.mesh(&part).verts, before.verts);

    f.body(Command::Undo { session });
    assert_eq!(f.mesh(&part).verts, before.verts);
}

/// Each way it can refuse says which, under a code a client branches on: a
/// node that is not a part, a part with nothing to trace, and a threshold that
/// finds no pixel.
#[test]
fn every_refusal_has_its_own_code() {
    let mut f = Fixture::new();

    let group = f.node(NodeKindArg::Group);
    assert!(matches!(
        f.auto(&group, AutoMesh::default()),
        Reply::Err {
            code: ErrorCode::BadTarget,
            ..
        }
    ));

    assert!(matches!(
        f.auto(&NodeId::new("root/part-nope").unwrap(), AutoMesh::default()),
        Reply::Err {
            code: ErrorCode::NoNode,
            ..
        }
    ));

    let bare = f.node(NodeKindArg::Part);
    assert!(matches!(
        f.auto(&bare, AutoMesh::default()),
        Reply::Err {
            code: ErrorCode::NoTexture,
            ..
        }
    ));

    // A texture with no opaque pixel at all, and one whose pixels are all
    // below the threshold asked for: the same answer, because a client fixes
    // both by offering a lower threshold.
    let blank = f.part("empty.png");
    assert!(matches!(
        f.auto(&blank, AutoMesh::default()),
        Reply::Err {
            code: ErrorCode::NothingToMesh,
            ..
        }
    ));

    let part = f.part("blob.png");
    for mode in [
        AutoMesh::Contour {
            threshold: Some(255),
            simplify: None,
            margin: None,
            spacing: None,
            rings: None,
            min_distance: None,
            mirror_x: None,
        },
        AutoMesh::Grid {
            threshold: Some(255),
            cols: None,
            rows: None,
            axes_x: None,
            axes_y: None,
            margin: None,
        },
    ] {
        assert!(
            matches!(
                f.auto(&part, mode.clone()),
                Reply::Err {
                    code: ErrorCode::NothingToMesh,
                    ..
                }
            ),
            "{mode:?} should have found nothing",
        );
    }

    // None of the refusals moved the document: the part still draws what it
    // drew and carries no mesh it did not have.
    assert!(f.mesh(&part).verts.is_empty());
}

/// The knobs the trace grew toward inochi-creator's automesh: rings and a
/// minimum spacing on a contour, named axes and a fractional margin on a
/// grid. They are fields on the mode a client already sends, and each one
/// moves the mesh in the direction it says.
#[test]
fn the_added_knobs_reach_the_trace() {
    fn contour(
        rings: Option<Vec<f32>>,
        spacing: Option<u32>,
        min_distance: Option<f32>,
    ) -> AutoMesh {
        AutoMesh::Contour {
            threshold: None,
            simplify: None,
            margin: None,
            spacing,
            rings,
            min_distance,
            mirror_x: None,
        }
    }
    let mut f = Fixture::new();
    let traced = |f: &mut Fixture, mode: AutoMesh| {
        let part = f.part("blob.png");
        f.auto(&part, mode);
        f.mesh(&part)
    };

    let outline = traced(&mut f, AutoMesh::default());
    let ringed = traced(&mut f, contour(Some(vec![0.6, 0.3]), None, None));
    assert!(
        ringed.verts.len() > outline.verts.len(),
        "rings fill the inside of the outline: {} vertices against {}",
        ringed.verts.len() / 2,
        outline.verts.len() / 2,
    );

    let dense = traced(&mut f, contour(None, Some(6), None));
    let thinned = traced(&mut f, contour(None, Some(6), Some(12.0)));
    assert!(
        thinned.verts.len() < dense.verts.len(),
        "12 texels apart thins a 6-texel fill: {} vertices against {}",
        thinned.verts.len() / 2,
        dense.verts.len() / 2,
    );

    // Named axes replace `cols`/`rows`, so the lattice is exactly as long as
    // the two lists rather than the 7 × 7 a default grid lays down.
    let grid = traced(
        &mut f,
        AutoMesh::Grid {
            threshold: None,
            cols: None,
            rows: None,
            axes_x: Some(vec![0.0, 0.5, 1.0]),
            axes_y: Some(vec![0.0, 1.0]),
            margin: None,
        },
    );
    assert_eq!(grid.verts.len() / 2, 3 * 2);

    // A margin is a fraction of the solid box, so half of it on each side of
    // a 32-texel square reaches the texture's own edges: local ±32 rather
    // than the ±17 one texel of margin gives.
    let wide = traced(
        &mut f,
        AutoMesh::Grid {
            threshold: None,
            cols: Some(1),
            rows: Some(1),
            axes_x: None,
            axes_y: None,
            margin: Some(0.5),
        },
    );
    let [min_x, min_y, max_x, max_y] = bounds(&wide.verts);
    assert!(
        (min_x + 32.0).abs() < 0.01
            && (min_y + 32.0).abs() < 0.01
            && (max_x - 32.0).abs() < 0.01
            && (max_y - 32.0).abs() < 0.01,
        "the box grew by half of itself: {:?}",
        bounds(&wide.verts),
    );
}
