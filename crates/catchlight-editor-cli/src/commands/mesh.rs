//! `mesh`: a part's or mesh group's mesh.

use anyhow::Context as _;
use catchlight_editor_protocol::{AutoMesh, Command, EditOp, MeshSetArgs, NodeId};
use clap::Subcommand;

use crate::cli::resolve_session;
use crate::route::Route;

#[derive(Subcommand)]
pub(crate) enum MeshCmd {
    /// Replace a node's mesh from JSON `{"verts", "uvs", "indices", "origin"}`,
    /// re-fitting its deform bindings and emptying its slots.
    Set {
        node: NodeId,
        #[arg(long)]
        file: String,
    },
    /// Copy `--from`'s mesh onto `node` (same re-fit, same emptying).
    Copy {
        node: NodeId,
        #[arg(long)]
        from: NodeId,
    },
    /// Derive a part's mesh from its texture's alpha (same re-fit and emptying).
    Auto {
        node: NodeId,
        /// Lay a regular grid over the solid texels instead of tracing them.
        #[arg(long, conflicts_with_all = ["simplify", "margin", "spacing"])]
        grid: bool,
        /// Alpha strictly above this counts as solid.
        #[arg(long)]
        threshold: Option<u8>,
        /// Contour: Douglas-Peucker tolerance in texels; higher is coarser.
        #[arg(long)]
        simplify: Option<f32>,
        /// Contour: texels of outward dilation before tracing.
        #[arg(long)]
        margin: Option<u32>,
        /// Contour: interior fill-point spacing in texels; 0 is boundary only.
        #[arg(long)]
        spacing: Option<u32>,
        /// Grid: columns of cells.
        #[arg(long, requires = "grid")]
        cols: Option<u32>,
        /// Grid: rows of cells.
        #[arg(long, requires = "grid")]
        rows: Option<u32>,
    },
}

pub(crate) fn route(action: &MeshCmd) -> Route<'_> {
    match action {
        MeshCmd::Set { node, file } => Route::plain(move |cli| {
            #[derive(serde::Deserialize)]
            struct MeshJson {
                verts: Vec<[f32; 2]>,
                uvs: Vec<[f32; 2]>,
                indices: Vec<[u32; 3]>,
                #[serde(default)]
                origin: [f32; 2],
            }
            let session = resolve_session(cli)?;
            let text = std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
            let m: MeshJson =
                serde_json::from_str(&text).with_context(|| format!("parsing {file}"))?;
            Ok(Command::MeshSet {
                if_rev: cli.if_rev,
                session,
                edit: MeshSetArgs {
                    deform_mapping: None,
                    node: node.clone(),
                    verts: m.verts,
                    uvs: m.uvs,
                    indices: m.indices,
                    origin: m.origin,
                },
            })
        }),
        MeshCmd::Copy { node, from } => Route::composed(move |remote| {
            let mesh = remote.mesh(from)?;
            remote.batch(vec![EditOp::MeshSet {
                edit: MeshSetArgs {
                    node: node.clone(),
                    verts: mesh.verts,
                    uvs: mesh.uvs,
                    indices: mesh.indices,
                    origin: mesh.origin,
                    deform_mapping: None,
                },
            }])
        }),
        MeshCmd::Auto {
            node,
            grid,
            threshold,
            simplify,
            margin,
            spacing,
            cols,
            rows,
        } => Route::on_session(move |session| Command::MeshAuto {
            session,
            node: node.clone(),
            mode: if *grid {
                AutoMesh::Grid {
                    threshold: *threshold,
                    cols: *cols,
                    rows: *rows,
                    axes_x: None,
                    axes_y: None,
                    margin: None,
                }
            } else {
                AutoMesh::Contour {
                    threshold: *threshold,
                    simplify: *simplify,
                    margin: *margin,
                    spacing: *spacing,
                    rings: None,
                    min_distance: None,
                    mirror_x: None,
                }
            },
        }),
    }
}
