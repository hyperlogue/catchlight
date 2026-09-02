//! `catchlight-editor-cli` — a thin client for the editor server.
//!
//! Each invocation builds one [`Command`], connects to the canonical socket
//! exposed by an editor or standalone server, sends it, and prints the reply.
//! The "current session" is remembered in a small local file so most commands
//! need no `--session`. Unix-only by design.
//!
//! Nodes, params, textures, seams and slots are named by their Id — the same
//! string the `.clm` stores — so a command written against one session is
//! replayable against another that opened the same file.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use catchlight_editor_protocol::*;
use clap::{Parser, Subcommand};

/// The Id rules, repeated in `--help` because every command takes one.
const ID_HELP: &str = "\
Nodes, params, textures, seams and slots are named by their Id: one or more of
the characters [A-Za-z0-9_./-], starting with none of '.', '/' or '-'. An Id is
what the .clm file stores, so it means the same thing in every session that
opened that file, and `node tree` / `param list` / `texture list` print the ones
a model carries.";

#[derive(Parser)]
#[command(
    name = "catchlight-editor-cli",
    about = "Drive a catchlight editor session",
    after_long_help = ID_HELP
)]
struct Cli {
    /// Print the raw reply JSON instead of a friendly summary.
    #[arg(long, global = true)]
    json: bool,
    /// Target session id (defaults to the remembered current session).
    #[arg(long, global = true)]
    session: Option<u64>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Session lifecycle.
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
    /// Save the session to a `.clm` (defaults to its existing file).
    Save { path: Option<String> },
    /// Export the editable JSON manifest + textures next to `path`.
    ExportManifest { path: String },
    /// Print a one-line summary of the session.
    Status,
    /// List model problems (untextured parts, bad meshes, unfilled slots, …).
    Check,
    /// Node tree operations.
    Node {
        #[command(subcommand)]
        action: NodeCmd,
    },
    /// Change a node's, param's or texture's Id. Breaking for addons.
    Rename {
        #[command(subcommand)]
        action: RenameCmd,
    },
    /// Texture operations.
    Texture {
        #[command(subcommand)]
        action: TextureCmd,
    },
    /// Param operations.
    Param {
        #[command(subcommand)]
        action: ParamCmd,
    },
    /// Scalar binding operations (param drives a node's transform/opacity/…).
    Binding {
        #[command(subcommand)]
        action: BindingCmd,
    },
    /// Add a SimplePhysics node.
    Physics {
        #[command(subcommand)]
        action: PhysicsCmd,
    },
    /// Mask-source list operations on a Part/Composite.
    Mask {
        #[command(subcommand)]
        action: MaskCmd,
    },
    /// Mesh operations on a Part/MeshGroup.
    Mesh {
        #[command(subcommand)]
        action: MeshCmd,
    },
    /// Seams and their slots: the named vertices a weld pairs.
    Seam {
        #[command(subcommand)]
        action: SeamCmd,
    },
    /// Welds: pair two parts' seams slot by slot.
    Weld {
        #[command(subcommand)]
        action: WeldCmd,
    },
    /// Author a deform keypoint from an affine on the part's rest mesh.
    Deform {
        #[command(subcommand)]
        action: DeformCmd,
    },
    /// Undo the last edit on the session.
    Undo,
    /// Redo the last undone edit.
    Redo,
    /// Shared view state (pose / selection) — a separate path from the document.
    Presence {
        #[command(subcommand)]
        action: PresenceCmd,
    },
    /// Render a preview PNG and print its path.
    Preview {
        /// Pose a param: `--param <id>=0.5` (repeatable).
        #[arg(long = "param")]
        params: Vec<String>,
        /// Output size, e.g. `512x512`.
        #[arg(long)]
        size: Option<String>,
        /// Output path (defaults to a temp file).
        #[arg(long)]
        out: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Start a new empty model.
    New {
        #[arg(long)]
        name: Option<String>,
    },
    /// Open an existing `.clm`.
    Open { path: String },
    /// Build a model from a JSON manifest + its textures.
    Import { manifest: String },
    /// List open sessions.
    List,
    /// Remember a session as the default for later commands.
    Use { id: u64 },
    /// Close a session.
    Close,
}

#[derive(Subcommand)]
enum RenameCmd {
    /// Rename a node's Id, rewriting every reference in the model.
    Node { from: NodeId, to: NodeId },
    /// Rename a param's Id.
    Param { from: ParamId, to: ParamId },
    /// Rename a texture's Id.
    Texture { from: TexId, to: TexId },
    /// Rename a seam's Id on one part. Every weld that ended on it follows.
    Seam {
        #[arg(long)]
        node: NodeId,
        from: SeamId,
        to: SeamId,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Print the node tree (Ids and names).
    Tree,
    /// Print one node in full: everything `node set` can change, plus its
    /// kind and parent.
    Info { node: NodeId },
    /// Add a child node.
    Add {
        #[arg(long)]
        parent: NodeId,
        /// group | part | composite | meshgroup
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: Option<String>,
        /// The Id to create it under. Without one the editor draws a free
        /// one; an Id the model already carries is refused.
        #[arg(long)]
        id: Option<NodeId>,
    },
    /// Change fields on a node.
    Set {
        node: NodeId,
        /// The label a person reads; nothing is addressed by it.
        #[arg(long)]
        name: Option<String>,
        /// `x,y,z`
        #[arg(long, allow_hyphen_values = true)]
        translate: Option<String>,
        /// `x,y,z` (radians)
        #[arg(long, allow_hyphen_values = true)]
        rotate: Option<String>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        scale: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        z_order: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        opacity: Option<f32>,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        texture: Option<TexId>,
        /// Draw no texture at all. Wins over `--texture`; the last part
        /// drawing a texture takes it with it.
        #[arg(long = "clear-texture")]
        clear_texture: bool,
        #[arg(long = "lock-to-root")]
        lock_to_root: Option<bool>,
        /// Blend-mode name (Normal | Multiply | ColorDodge | …).
        #[arg(long = "blend-mode")]
        blend_mode: Option<String>,
        /// `r,g,b`
        #[arg(long, allow_hyphen_values = true)]
        tint: Option<String>,
        /// `r,g,b`
        #[arg(long = "screen-tint", allow_hyphen_values = true)]
        screen_tint: Option<String>,
        #[arg(long = "mask-threshold")]
        mask_threshold: Option<f32>,
        #[arg(long = "propagate-meshgroup")]
        propagate_meshgroup: Option<bool>,
        #[arg(long = "mg-dynamic")]
        mg_dynamic: Option<bool>,
        #[arg(long = "mg-translate-children")]
        mg_translate_children: Option<bool>,
    },
    /// Move a node under a new parent (its Id does not change).
    Reparent {
        node: NodeId,
        #[arg(long)]
        to: NodeId,
    },
    /// Move a node within its parent's children (index clamps to the end).
    Reorder {
        node: NodeId,
        #[arg(long)]
        index: u32,
    },
    /// Reparent + position in one step: become `--parent`'s child at `--index`.
    Move {
        node: NodeId,
        #[arg(long)]
        parent: NodeId,
        #[arg(long)]
        index: u32,
    },
    /// Deep-copy a node's subtree as its next sibling, with fresh Ids.
    Duplicate { node: NodeId },
    /// Delete a node and its subtree.
    Delete { node: NodeId },
}

#[derive(Subcommand)]
enum MeshCmd {
    /// Replace a node's mesh from a JSON file
    /// `{"verts": [...], "uvs": [...], "indices": [...], "origin": [x, y]}`.
    /// Deform bindings on the node are re-fitted in the same undo step, and
    /// every seam slot on the part is emptied — the reply lists them.
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
    /// Derive a part's mesh from its own texture's alpha and apply it (same
    /// re-fit, same emptying). Contour by default; every knob left off is the
    /// editor's own.
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

#[derive(Subcommand)]
enum SeamCmd {
    /// Add a seam to a part. Without `--seam` the editor draws one
    /// (`seam-<8 hex>`) and the reply says which.
    Add {
        node: NodeId,
        #[arg(long)]
        seam: Option<SeamId>,
    },
    /// Remove a seam, and every weld that named it.
    Delete {
        node: NodeId,
        #[arg(long)]
        seam: SeamId,
    },
    /// Print a part's seams and what fills each slot.
    List { node: NodeId },
    /// Add a slot. It lands unfilled, and reaches every welded seam. Without
    /// `--slot` the editor draws one (`slot-<8 hex>`).
    SlotAdd {
        node: NodeId,
        #[arg(long)]
        seam: SeamId,
        #[arg(long)]
        slot: Option<SlotId>,
    },
    /// Point a slot at one of the part's vertices.
    SlotFill {
        node: NodeId,
        #[arg(long)]
        seam: SeamId,
        #[arg(long)]
        slot: SlotId,
        #[arg(long)]
        vertex: u32,
    },
    /// Unfill a slot; welds skip it until it is filled again.
    SlotClear {
        node: NodeId,
        #[arg(long)]
        seam: SeamId,
        #[arg(long)]
        slot: SlotId,
    },
    /// Remove a slot — here and from every welded seam.
    SlotDelete {
        node: NodeId,
        #[arg(long)]
        seam: SeamId,
        #[arg(long)]
        slot: SlotId,
    },
    /// List every slot in the model that no vertex fills.
    Unfilled,
}

#[derive(Subcommand)]
enum WeldCmd {
    /// Pair two seams, replacing any weld already joining them. Without
    /// `--weight` every slot meets midway.
    Set {
        #[arg(long = "a-node")]
        a_node: NodeId,
        #[arg(long = "a-seam")]
        a_seam: SeamId,
        #[arg(long = "b-node")]
        b_node: NodeId,
        #[arg(long = "b-seam")]
        b_seam: SeamId,
        /// `--weight <slot>=0.5` (repeatable); the share the *first* seam's
        /// vertex is pulled by.
        #[arg(long = "weight")]
        weights: Vec<String>,
    },
    /// Move one slot's share of one weld's meeting point, leaving the rest
    /// alone. `--weight` is the share the *first* seam is pinned by, within
    /// 0..=1, whichever way round the weld is stored.
    Weight {
        #[arg(long = "a-node")]
        a_node: NodeId,
        #[arg(long = "a-seam")]
        a_seam: SeamId,
        #[arg(long = "b-node")]
        b_node: NodeId,
        #[arg(long = "b-seam")]
        b_seam: SeamId,
        #[arg(long)]
        slot: SlotId,
        #[arg(long)]
        weight: f32,
    },
    /// Unmake the weld joining two seams, named in either order. Both seams
    /// and their slots stay; only the pairing goes.
    Delete {
        #[arg(long = "a-node")]
        a_node: NodeId,
        #[arg(long = "a-seam")]
        a_seam: SeamId,
        #[arg(long = "b-node")]
        b_node: NodeId,
        #[arg(long = "b-seam")]
        b_seam: SeamId,
    },
    /// List the model's welds.
    List,
}

#[derive(Subcommand)]
enum TextureCmd {
    /// Give a PNG/TGA file to a part: the texture is added and the part draws
    /// it in one edit. A texture the part was the last to draw goes with it.
    Add {
        node: NodeId,
        path: String,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<TexId>,
    },
    /// List the session's textures.
    List,
}

#[derive(Subcommand)]
enum ParamCmd {
    /// Create a scalar param. `--keys a,b,c` sets key positions (defaults to
    /// the two endpoints).
    Add {
        name: String,
        #[arg(long, allow_hyphen_values = true)]
        min: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        max: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        default: Option<f32>,
        #[arg(long = "keys", allow_hyphen_values = true)]
        key_positions: Option<String>,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<ParamId>,
    },
    /// List params (with key positions + binding counts).
    List,
    /// Change param metadata (key positions are normalized, so a range change
    /// does not move them).
    Set {
        param: ParamId,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        min: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        max: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        default: Option<f32>,
    },
    /// Delete a param (and its bindings).
    Delete { param: ParamId },
    /// Insert a key position, strictly inside (0, 1).
    KeyInsert {
        param: ParamId,
        #[arg(long, allow_hyphen_values = true)]
        value: f32,
    },
    /// Remove an interior key position by index.
    KeyDelete {
        param: ParamId,
        #[arg(long)]
        index: u32,
    },
    /// Move an interior key position.
    KeyMove {
        param: ParamId,
        #[arg(long)]
        index: u32,
        #[arg(long, allow_hyphen_values = true)]
        value: f32,
    },
    /// Mirror the param (values untouched).
    Flip { param: ParamId },
}

/// The param, or pair of params, every binding command is keyed by.
#[derive(clap::Args)]
struct BindingParamsArg {
    #[arg(long)]
    param: ParamId,
    /// A second param: the binding's grid then spans both, and `--cell x,y`
    /// indexes both.
    #[arg(long = "param-y")]
    param_y: Option<ParamId>,
}

impl BindingParamsArg {
    fn wire(&self) -> BindingParams {
        BindingParams {
            param: self.param.clone(),
            param_y: self.param_y.clone(),
        }
    }
}

#[derive(Subcommand)]
enum BindingCmd {
    /// Create an identity binding: `--target tx|ty|sx|sy|rx|ry|rz|z_order|opacity|tint{r,g,b}|…`.
    Add {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
    },
    /// Set one keypoint of a binding (auto-creates it). `--cell x,y`.
    Key {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
        #[arg(long)]
        cell: String,
        #[arg(long, allow_hyphen_values = true)]
        value: f32,
    },
    /// Un-author a keypoint (back to derived). `--target` also accepts deform.
    Unset {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
        #[arg(long)]
        cell: String,
    },
    /// Author the identity value at a keypoint.
    Reset {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
        #[arg(long)]
        cell: String,
    },
    /// Delete a whole binding.
    Delete {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
    },
    /// Set interpolation: nearest | stepped | linear | cubic.
    Interpolate {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
        #[arg(long)]
        mode: String,
    },
    /// Negate every authored value.
    Invert {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
    },
    /// List a node's bindings, each with its authored key grid.
    List {
        #[arg(long)]
        node: NodeId,
    },
    /// Author the value evaluated at `--from` into `--to`.
    CopyKey {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        target: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
    },
}

#[derive(Subcommand)]
enum PhysicsCmd {
    /// Add a physics node under `--parent`. `--kind rigid|spring`.
    Add {
        #[arg(long)]
        parent: NodeId,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: Option<String>,
        /// A param the driver writes, in output order (angle, length).
        /// Repeatable, at most two; `-` leaves that output bound to nothing.
        #[arg(long = "target-param", allow_hyphen_values = true)]
        target_params: Vec<String>,
        #[arg(long, allow_hyphen_values = true)]
        length: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        gravity: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        frequency: Option<f32>,
        #[arg(long = "angle-damping", allow_hyphen_values = true)]
        angle_damping: Option<f32>,
        #[arg(long = "length-damping", allow_hyphen_values = true)]
        length_damping: Option<f32>,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<NodeId>,
    },
    /// Change fields on a physics node. `--kind rigid|spring`,
    /// `--map-mode xy|yx|angle_length|length_angle`.
    Set {
        node: NodeId,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long = "map-mode")]
        map_mode: Option<String>,
        #[arg(long = "local-only")]
        local_only: Option<bool>,
        /// Replace the driven params, in output order (angle, length).
        /// Repeatable, at most two; `-` leaves that output bound to nothing.
        #[arg(long = "target-param", allow_hyphen_values = true)]
        target_params: Vec<String>,
        #[arg(long = "clear-target-params")]
        clear_target_params: bool,
        #[arg(long, allow_hyphen_values = true)]
        gravity: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        length: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        frequency: Option<f32>,
        #[arg(long = "angle-damping", allow_hyphen_values = true)]
        angle_damping: Option<f32>,
        #[arg(long = "length-damping", allow_hyphen_values = true)]
        length_damping: Option<f32>,
        /// `x,y`
        #[arg(long = "output-scale", allow_hyphen_values = true)]
        output_scale: Option<String>,
    },
    /// Model-level physics constants.
    Globals {
        #[arg(long, allow_hyphen_values = true)]
        gravity: Option<f32>,
        #[arg(long = "pixels-per-meter", allow_hyphen_values = true)]
        pixels_per_meter: Option<f32>,
    },
}

#[derive(Subcommand)]
enum MaskCmd {
    /// Append a mask source (a Part) to a Part/Composite. `--mode mask|dodge`.
    Add {
        node: NodeId,
        #[arg(long)]
        source: NodeId,
        #[arg(long, default_value = "mask")]
        mode: String,
    },
    /// Change the mode of the mask at `--index`.
    Set {
        node: NodeId,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        mode: String,
    },
    /// Move the mask at `--index` to `--to`.
    Reorder {
        node: NodeId,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        to: u32,
    },
    /// Remove the mask at `--index`.
    Delete {
        node: NodeId,
        #[arg(long)]
        index: u32,
    },
}

#[derive(Subcommand)]
enum PresenceCmd {
    /// Publish pose / selection. `--param <id>=<value>` (repeatable).
    Set {
        #[arg(long = "param", allow_hyphen_values = true)]
        params: Vec<String>,
        #[arg(long)]
        select: Option<NodeId>,
    },
    /// Read the current shared presence.
    Get,
    /// Show a deform on the session's puppet without authoring it — the live
    /// half of a vertex drag. Never bumps the revision, never records undo.
    Scratch {
        node: NodeId,
        /// A JSON array of `[dx, dy, …]`, two per mesh vertex.
        #[arg(long)]
        file: Option<String>,
        /// Drop the scratch deform instead.
        #[arg(long)]
        clear: bool,
    },
}

#[derive(Subcommand)]
enum DeformCmd {
    /// Set a deform keypoint at `--cell x,y` from an affine.
    Set {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        cell: String,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        translate: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        rotate: Option<f32>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        scale: Option<String>,
    },
    /// Author per-vertex offsets from a JSON array — what commits a drag.
    Vertices {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        cell: String,
        #[arg(long)]
        file: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // `session use` is local-only — no server round trip.
    if let Cmd::Session {
        action: SessionCmd::Use { id },
    } = &cli.cmd
    {
        write_current(SessionId(*id))?;
        println!("using session {id}");
        return Ok(());
    }

    let command = build_command(&cli)?;
    let persist = matches!(
        cli.cmd,
        Cmd::Session {
            action: SessionCmd::New { .. } | SessionCmd::Open { .. } | SessionCmd::Import { .. }
        }
    );

    let mut stream = connect()?;
    let reply = call(&mut stream, Request { id: 1, command })?;

    if persist {
        if let Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } = &reply
        {
            write_current(*session)?;
        }
    }

    print_reply(&cli, &reply);
    if matches!(reply, Reply::Err { .. }) {
        std::process::exit(1);
    }
    Ok(())
}

fn build_command(cli: &Cli) -> Result<Command> {
    Ok(match &cli.cmd {
        Cmd::Session { action } => match action {
            SessionCmd::New { name } => Command::SessionNew { name: name.clone() },
            SessionCmd::Open { path } => Command::SessionOpen { path: path.clone() },
            SessionCmd::Import { manifest } => Command::SessionImport {
                manifest_path: manifest.clone(),
            },
            SessionCmd::List => Command::SessionList,
            SessionCmd::Close => Command::SessionClose {
                session: resolve_session(cli)?,
            },
            SessionCmd::Use { .. } => unreachable!("handled in main"),
        },
        Cmd::Save { path } => Command::Save {
            session: resolve_session(cli)?,
            path: path.clone(),
        },
        Cmd::ExportManifest { path } => Command::ExportManifest {
            session: resolve_session(cli)?,
            path: path.clone(),
        },
        Cmd::Status => Command::Status {
            session: resolve_session(cli)?,
        },
        Cmd::Check => Command::Check {
            session: resolve_session(cli)?,
        },
        Cmd::Node { action } => build_node_command(cli, action)?,
        Cmd::Rename { action } => Command::RenameId {
            session: resolve_session(cli)?,
            rename: match action {
                RenameCmd::Node { from, to } => Rename::Node {
                    from: from.clone(),
                    to: to.clone(),
                },
                RenameCmd::Param { from, to } => Rename::Param {
                    from: from.clone(),
                    to: to.clone(),
                },
                RenameCmd::Texture { from, to } => Rename::Texture {
                    from: from.clone(),
                    to: to.clone(),
                },
                RenameCmd::Seam { node, from, to } => Rename::Seam {
                    node: node.clone(),
                    from: from.clone(),
                    to: to.clone(),
                },
            },
        },
        Cmd::Param { action } => {
            let session = resolve_session(cli)?;
            match action {
                ParamCmd::Add {
                    name,
                    min,
                    max,
                    default,
                    key_positions,
                    id,
                } => Command::ParamAdd {
                    session,
                    name: name.clone(),
                    min: min.unwrap_or(0.0),
                    max: max.unwrap_or(1.0),
                    default: default.unwrap_or(0.0),
                    key_positions: key_positions
                        .as_deref()
                        .map(parse_f32_vec)
                        .transpose()?
                        .unwrap_or_default(),
                    param: id.clone(),
                },
                ParamCmd::List => Command::ParamList { session },
                ParamCmd::Set {
                    param,
                    name,
                    min,
                    max,
                    default,
                } => Command::ParamSet {
                    session,
                    param: param.clone(),
                    name: name.clone(),
                    min: *min,
                    max: *max,
                    default: *default,
                },
                ParamCmd::Delete { param } => Command::ParamDelete {
                    session,
                    param: param.clone(),
                },
                ParamCmd::KeyInsert { param, value } => Command::ParamKeyInsert {
                    session,
                    param: param.clone(),
                    value: *value,
                },
                ParamCmd::KeyDelete { param, index } => Command::ParamKeyDelete {
                    session,
                    param: param.clone(),
                    index: *index,
                },
                ParamCmd::KeyMove {
                    param,
                    index,
                    value,
                } => Command::ParamKeyMove {
                    session,
                    param: param.clone(),
                    index: *index,
                    value: *value,
                },
                ParamCmd::Flip { param } => Command::ParamFlip {
                    session,
                    param: param.clone(),
                },
            }
        }
        Cmd::Binding { action } => {
            let session = resolve_session(cli)?;
            match action {
                BindingCmd::Add {
                    params,
                    node,
                    target,
                } => Command::BindingAdd {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                },
                BindingCmd::Key {
                    params,
                    node,
                    target,
                    cell,
                    value,
                } => Command::BindingKey {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                    cell: parse_cell(cell)?,
                    value: *value,
                },
                BindingCmd::Unset {
                    params,
                    node,
                    target,
                    cell,
                } => Command::BindingUnset {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                    cell: parse_cell(cell)?,
                },
                BindingCmd::Reset {
                    params,
                    node,
                    target,
                    cell,
                } => Command::BindingReset {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                    cell: parse_cell(cell)?,
                },
                BindingCmd::Delete {
                    params,
                    node,
                    target,
                } => Command::BindingDelete {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                },
                BindingCmd::Interpolate {
                    params,
                    node,
                    target,
                    mode,
                } => Command::BindingInterpolate {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                    mode: mode.clone(),
                },
                BindingCmd::Invert {
                    params,
                    node,
                    target,
                } => Command::BindingInvert {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                },
                BindingCmd::List { node } => Command::BindingList {
                    session,
                    node: node.clone(),
                },
                BindingCmd::CopyKey {
                    params,
                    node,
                    target,
                    from,
                    to,
                } => Command::BindingCopyKey {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: target.clone(),
                    from: parse_cell(from)?,
                    to: parse_cell(to)?,
                },
            }
        }
        Cmd::Physics { action } => {
            let session = resolve_session(cli)?;
            match action {
                PhysicsCmd::Add {
                    parent,
                    kind,
                    name,
                    target_params,
                    length,
                    gravity,
                    frequency,
                    angle_damping,
                    length_damping,
                    id,
                } => Command::PhysicsAdd {
                    session,
                    parent: parent.clone(),
                    name: name.clone(),
                    kind: kind.clone(),
                    target_params: parse_target_params(target_params)?,
                    length: *length,
                    gravity: *gravity,
                    frequency: *frequency,
                    angle_damping: *angle_damping,
                    length_damping: *length_damping,
                    node: id.clone(),
                },
                PhysicsCmd::Set {
                    node,
                    kind,
                    map_mode,
                    local_only,
                    target_params,
                    clear_target_params,
                    gravity,
                    length,
                    frequency,
                    angle_damping,
                    length_damping,
                    output_scale,
                } => Command::PhysicsSet {
                    session,
                    node: node.clone(),
                    kind: kind.clone(),
                    map_mode: map_mode.clone(),
                    local_only: *local_only,
                    target_params: (!target_params.is_empty())
                        .then(|| parse_target_params(target_params))
                        .transpose()?,
                    clear_target_params: *clear_target_params,
                    gravity: *gravity,
                    length: *length,
                    frequency: *frequency,
                    angle_damping: *angle_damping,
                    length_damping: *length_damping,
                    output_scale: output_scale.as_deref().map(parse_vec2).transpose()?,
                },
                PhysicsCmd::Globals {
                    gravity,
                    pixels_per_meter,
                } => Command::PhysicsGlobals {
                    session,
                    gravity: *gravity,
                    pixels_per_meter: *pixels_per_meter,
                },
            }
        }
        Cmd::Mesh { action } => {
            let session = resolve_session(cli)?;
            match action {
                MeshCmd::Set { node, file } => {
                    #[derive(serde::Deserialize)]
                    struct MeshJson {
                        verts: Vec<f32>,
                        uvs: Vec<f32>,
                        indices: Vec<u32>,
                        #[serde(default)]
                        origin: [f32; 2],
                    }
                    let m: MeshJson = serde_json::from_str(&std::fs::read_to_string(file)?)?;
                    Command::MeshSet {
                        session,
                        node: node.clone(),
                        verts: m.verts,
                        uvs: m.uvs,
                        indices: m.indices,
                        origin: m.origin,
                    }
                }
                MeshCmd::Copy { node, from } => Command::MeshCopy {
                    session,
                    from: from.clone(),
                    to: node.clone(),
                },
                MeshCmd::Auto {
                    node,
                    grid,
                    threshold,
                    simplify,
                    margin,
                    spacing,
                    cols,
                    rows,
                } => Command::MeshAuto {
                    session,
                    node: node.clone(),
                    mode: if *grid {
                        AutoMesh::Grid {
                            threshold: *threshold,
                            cols: *cols,
                            rows: *rows,
                        }
                    } else {
                        AutoMesh::Contour {
                            threshold: *threshold,
                            simplify: *simplify,
                            margin: *margin,
                            spacing: *spacing,
                        }
                    },
                },
            }
        }
        Cmd::Seam { action } => {
            let session = resolve_session(cli)?;
            match action {
                SeamCmd::Add { node, seam } => Command::SeamAdd {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                },
                SeamCmd::Delete { node, seam } => Command::SeamDelete {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                },
                SeamCmd::List { node } => Command::Seams {
                    session,
                    node: node.clone(),
                },
                SeamCmd::SlotAdd { node, seam, slot } => Command::SlotAdd {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                    slot: slot.clone(),
                },
                SeamCmd::SlotFill {
                    node,
                    seam,
                    slot,
                    vertex,
                } => Command::SlotFill {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                    slot: slot.clone(),
                    vertex: *vertex,
                },
                SeamCmd::SlotClear { node, seam, slot } => Command::SlotClear {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                    slot: slot.clone(),
                },
                SeamCmd::SlotDelete { node, seam, slot } => Command::SlotDelete {
                    session,
                    node: node.clone(),
                    seam: seam.clone(),
                    slot: slot.clone(),
                },
                SeamCmd::Unfilled => Command::UnfilledSlots { session },
            }
        }
        Cmd::Weld { action } => {
            let session = resolve_session(cli)?;
            match action {
                WeldCmd::Set {
                    a_node,
                    a_seam,
                    b_node,
                    b_seam,
                    weights,
                } => Command::WeldSet {
                    session,
                    a: SeamAddr {
                        node: a_node.clone(),
                        seam: a_seam.clone(),
                    },
                    b: SeamAddr {
                        node: b_node.clone(),
                        seam: b_seam.clone(),
                    },
                    weights: weights
                        .iter()
                        .map(|w| parse_weight(w))
                        .collect::<Result<_>>()?,
                },
                WeldCmd::Weight {
                    a_node,
                    a_seam,
                    b_node,
                    b_seam,
                    slot,
                    weight,
                } => Command::WeldWeight {
                    session,
                    a: SeamAddr {
                        node: a_node.clone(),
                        seam: a_seam.clone(),
                    },
                    b: SeamAddr {
                        node: b_node.clone(),
                        seam: b_seam.clone(),
                    },
                    slot: slot.clone(),
                    weight: *weight,
                },
                WeldCmd::Delete {
                    a_node,
                    a_seam,
                    b_node,
                    b_seam,
                } => Command::WeldDelete {
                    session,
                    a: SeamAddr {
                        node: a_node.clone(),
                        seam: a_seam.clone(),
                    },
                    b: SeamAddr {
                        node: b_node.clone(),
                        seam: b_seam.clone(),
                    },
                },
                WeldCmd::List => Command::Welds { session },
            }
        }
        Cmd::Mask { action } => {
            let session = resolve_session(cli)?;
            match action {
                MaskCmd::Add { node, source, mode } => Command::MaskAdd {
                    session,
                    node: node.clone(),
                    source: source.clone(),
                    mode: mode.clone(),
                },
                MaskCmd::Set { node, index, mode } => Command::MaskSet {
                    session,
                    node: node.clone(),
                    index: *index,
                    mode: mode.clone(),
                },
                MaskCmd::Reorder { node, index, to } => Command::MaskReorder {
                    session,
                    node: node.clone(),
                    index: *index,
                    to: *to,
                },
                MaskCmd::Delete { node, index } => Command::MaskDelete {
                    session,
                    node: node.clone(),
                    index: *index,
                },
            }
        }
        Cmd::Deform { action } => {
            let session = resolve_session(cli)?;
            match action {
                DeformCmd::Set {
                    params,
                    node,
                    cell,
                    translate,
                    rotate,
                    scale,
                } => Command::DeformSet {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    cell: parse_cell(cell)?,
                    translate: translate.as_deref().map(parse_vec2).transpose()?,
                    rotate: *rotate,
                    scale: scale.as_deref().map(parse_vec2).transpose()?,
                },
                DeformCmd::Vertices {
                    params,
                    node,
                    cell,
                    file,
                } => Command::DeformVertices {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    cell: parse_cell(cell)?,
                    offsets: read_offsets(file)?,
                },
            }
        }
        Cmd::Presence { action } => {
            let session = resolve_session(cli)?;
            match action {
                PresenceCmd::Set { params, select } => Command::PresenceSet {
                    session,
                    presence: Presence {
                        pose: params
                            .iter()
                            .map(|p| parse_param(p))
                            .collect::<Result<_>>()?,
                        camera: None,
                        selection: select.clone(),
                    },
                },
                PresenceCmd::Get => Command::PresenceGet { session },
                PresenceCmd::Scratch { node, file, clear } => Command::ScratchDeform {
                    session,
                    node: node.clone(),
                    offsets: match (clear, file) {
                        (true, _) | (false, None) => Vec::new(),
                        (false, Some(file)) => read_offsets(file)?,
                    },
                },
            }
        }
        Cmd::Undo => Command::Undo {
            session: resolve_session(cli)?,
        },
        Cmd::Redo => Command::Redo {
            session: resolve_session(cli)?,
        },
        Cmd::Texture { action } => match action {
            TextureCmd::Add { node, path, id } => Command::TextureAdd {
                session: resolve_session(cli)?,
                node: node.clone(),
                path: path.clone(),
                texture: id.clone(),
            },
            TextureCmd::List => Command::TextureList {
                session: resolve_session(cli)?,
            },
        },
        Cmd::Preview { params, size, out } => Command::Preview {
            session: resolve_session(cli)?,
            pose: params
                .iter()
                .map(|p| parse_param(p))
                .collect::<Result<_>>()?,
            size: size.as_deref().map(parse_size).transpose()?,
            out: out.clone(),
        },
    })
}

fn build_node_command(cli: &Cli, action: &NodeCmd) -> Result<Command> {
    let session = resolve_session(cli)?;
    Ok(match action {
        NodeCmd::Tree => Command::NodeTree { session },
        NodeCmd::Info { node } => Command::NodeInfo {
            session,
            node: node.clone(),
        },
        NodeCmd::Add {
            parent,
            kind,
            name,
            id,
        } => Command::NodeAdd {
            session,
            parent: parent.clone(),
            kind: parse_kind(kind)?,
            name: name.clone(),
            node: id.clone(),
        },
        NodeCmd::Set {
            node,
            name,
            translate,
            rotate,
            scale,
            z_order,
            opacity,
            enabled,
            texture,
            clear_texture,
            lock_to_root,
            blend_mode,
            tint,
            screen_tint,
            mask_threshold,
            propagate_meshgroup,
            mg_dynamic,
            mg_translate_children,
        } => Command::NodeSet {
            session,
            node: node.clone(),
            patch: NodePatch {
                name: name.clone(),
                translate: translate.as_deref().map(parse_vec3).transpose()?,
                rotate: rotate.as_deref().map(parse_vec3).transpose()?,
                scale: scale.as_deref().map(parse_vec2).transpose()?,
                z_order: *z_order,
                opacity: *opacity,
                enabled: *enabled,
                texture: texture.clone(),
                clear_texture: *clear_texture,
                lock_to_root: *lock_to_root,
                blend_mode: blend_mode.clone(),
                tint: tint.as_deref().map(parse_vec3).transpose()?,
                screen_tint: screen_tint.as_deref().map(parse_vec3).transpose()?,
                mask_threshold: *mask_threshold,
                propagate_meshgroup: *propagate_meshgroup,
                mg_dynamic: *mg_dynamic,
                mg_translate_children: *mg_translate_children,
            },
        },
        NodeCmd::Reparent { node, to } => Command::NodeReparent {
            session,
            node: node.clone(),
            to: to.clone(),
        },
        NodeCmd::Reorder { node, index } => Command::NodeReorder {
            session,
            node: node.clone(),
            index: *index,
        },
        NodeCmd::Move {
            node,
            parent,
            index,
        } => Command::NodeMove {
            session,
            node: node.clone(),
            parent: parent.clone(),
            index: *index,
        },
        NodeCmd::Duplicate { node } => Command::NodeDuplicate {
            session,
            node: node.clone(),
        },
        NodeCmd::Delete { node } => Command::NodeDelete {
            session,
            node: node.clone(),
        },
    })
}

fn parse_kind(s: &str) -> Result<NodeKindArg> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "group" => NodeKindArg::Group,
        "part" => NodeKindArg::Part,
        "composite" => NodeKindArg::Composite,
        "meshgroup" | "mesh_group" => NodeKindArg::MeshGroup,
        other => bail!("unknown node kind {other:?} (group|part|composite|meshgroup)"),
    })
}

fn parse_floats<const N: usize>(s: &str) -> Result<[f32; N]> {
    let parts: Vec<f32> = s
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow!("bad number list {s:?}: {e}"))?;
    parts
        .try_into()
        .map_err(|_| anyhow!("expected {N} comma-separated numbers, got {s:?}"))
}

fn parse_vec3(s: &str) -> Result<[f32; 3]> {
    parse_floats::<3>(s)
}

fn parse_vec2(s: &str) -> Result<[f32; 2]> {
    parse_floats::<2>(s)
}

fn parse_f32_vec(s: &str) -> Result<Vec<f32>> {
    s.split(',')
        .map(|p| {
            p.trim()
                .parse::<f32>()
                .map_err(|e| anyhow!("bad number {p:?}: {e}"))
        })
        .collect()
}

fn parse_cell(s: &str) -> Result<[u32; 2]> {
    let (x, y) = s
        .split_once(',')
        .ok_or_else(|| anyhow!("cell must look like x,y, got {s:?}"))?;
    Ok([x.trim().parse()?, y.trim().parse()?])
}

fn parse_size(s: &str) -> Result<[u32; 2]> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| anyhow!("size must look like 512x512, got {s:?}"))?;
    Ok([w.trim().parse()?, h.trim().parse()?])
}

/// `<param id>=<value>`. Params are scalar, so there is one number.
fn parse_param(s: &str) -> Result<ParamPose> {
    let (id, value) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("pose must look like <param id>=<value>, got {s:?}"))?;
    Ok(ParamPose {
        param: ParamId::new(id.trim())?,
        value: value
            .trim()
            .parse::<f32>()
            .map_err(|e| anyhow!("bad param value {value:?}: {e}"))?,
    })
}

/// A driver's outputs in order, where `-` is an output bound to nothing.
///
/// Positional rather than a set: a driver whose length drives a param and
/// whose angle drives none has to be sayable, and `-` is safe as the hole
/// because no Id may start with one.
fn parse_target_params(args: &[String]) -> Result<Vec<Option<ParamId>>> {
    args.iter()
        .map(|arg| match arg.as_str() {
            "-" => Ok(None),
            id => Ok(Some(ParamId::new(id)?)),
        })
        .collect()
}

/// `<slot id>=<weight>`.
fn parse_weight(s: &str) -> Result<SlotWeight> {
    let (id, weight) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("weight must look like <slot id>=<weight>, got {s:?}"))?;
    Ok(SlotWeight {
        slot: SlotId::new(id.trim())?,
        weight: weight
            .trim()
            .parse::<f32>()
            .map_err(|e| anyhow!("bad weight {weight:?}: {e}"))?,
    })
}

/// A JSON array of per-vertex offsets, `[dx, dy, …]`.
fn read_offsets(path: &str) -> Result<Vec<f32>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

fn resolve_session(cli: &Cli) -> Result<SessionId> {
    if let Some(s) = cli.session {
        return Ok(SessionId(s));
    }
    read_current()?.ok_or_else(|| {
        anyhow!(
            "no session selected; pass --session <id> or run `session use <id>` / `session new`"
        )
    })
}

fn connect() -> Result<UnixStream> {
    let path = socket_path();
    UnixStream::connect(&path).map_err(|e| {
        anyhow!(
            "cannot connect to the editor server at {}; start catchlight-editor or catchlight-editor-server first: {e}",
            path.display()
        )
    })
}

fn call(stream: &mut UnixStream, req: Request) -> Result<Reply> {
    let want = req.id;
    let mut line = serde_json::to_string(&req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream.try_clone()?);
    loop {
        let mut buf = String::new();
        if reader.read_line(&mut buf)? == 0 {
            bail!("server closed the connection");
        }
        let buf = buf.trim();
        if buf.is_empty() {
            continue;
        }
        let reply: Reply = serde_json::from_str(buf)?;
        match &reply {
            Reply::Event(_) => continue,
            Reply::Ok { id, .. } | Reply::Err { id, .. } if *id == want => return Ok(reply),
            _ => continue,
        }
    }
}

fn print_reply(cli: &Cli, reply: &Reply) {
    if cli.json {
        if let Ok(s) = serde_json::to_string(reply) {
            println!("{s}");
        }
        return;
    }
    match reply {
        Reply::Err { code, message, .. } => eprintln!("error [{}]: {message}", code_name(*code)),
        Reply::Event(_) => {}
        Reply::Ok { body, .. } => print_body(body),
    }
}

/// The wire spelling of a code, so a person reading the terminal and a script
/// reading `--json` see the same word.
fn code_name(code: ErrorCode) -> String {
    serde_json::to_string(&code)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| "error".into())
}

fn print_body(body: &ResponseBody) {
    match body {
        ResponseBody::Empty => println!("ok"),
        ResponseBody::Session { session } => println!("session {}", session.0),
        ResponseBody::Sessions { sessions } => {
            if sessions.is_empty() {
                println!("(no open sessions)");
            }
            for s in sessions {
                println!(
                    "session {}  {}{}  nodes={} rev={}",
                    s.session.0,
                    s.title,
                    if s.dirty { " *" } else { "" },
                    s.node_count,
                    s.rev
                );
            }
        }
        ResponseBody::Node { node, dropped } => {
            println!("node {node}");
            print_dropped(dropped);
        }
        ResponseBody::Param { param } => println!("param {param}"),
        ResponseBody::Seam { seam } => println!("seam {} on {}", seam.seam, seam.node),
        ResponseBody::Slot { slot } => {
            println!("slot {} in {} on {}", slot.slot, slot.seam, slot.node)
        }
        ResponseBody::Params { params } => {
            if params.is_empty() {
                println!("(no params)");
            }
            for p in params {
                println!(
                    "param {}  {}  [{}, {}] default={} keys={} bindings={}",
                    p.id,
                    p.name,
                    p.min,
                    p.max,
                    p.default,
                    p.key_positions.len(),
                    p.bindings
                );
            }
        }
        ResponseBody::Bindings { bindings } => {
            if bindings.is_empty() {
                println!("(no bindings)");
            }
            for b in bindings {
                let params = match &b.param_y {
                    Some(y) => format!("{} x {}", b.param, y),
                    None => b.param.to_string(),
                };
                println!(
                    "binding {}  {}  {}  {}x{}",
                    b.target, params, b.interpolate, b.width, b.height
                );
                for (y, row) in b.keys.iter().enumerate() {
                    let cells: Vec<String> = row
                        .iter()
                        .enumerate()
                        .map(|(x, value)| match value {
                            Some(v) => format!("{v}"),
                            // Authored with no scalar of its own: a deform
                            // cell, which holds a vertex list.
                            None if authored(b, x, y) => "set".into(),
                            None => "-".into(),
                        })
                        .collect();
                    println!("  y={y}: [{}]", cells.join(" "));
                }
            }
        }
        ResponseBody::Texture { texture, dropped } => {
            println!("texture {texture}");
            print_dropped(dropped);
        }
        ResponseBody::Textures { textures } => {
            for t in textures {
                println!("texture {}  {}x{}", t.id, t.width, t.height);
            }
        }
        ResponseBody::Tree { root } => print_tree(root, 0),
        ResponseBody::NodeInfo { node } => print_node_info(node),
        ResponseBody::Status { status } => println!(
            "{}{}  nodes={} params={} textures={} rev={}",
            status.title,
            if status.dirty { " *" } else { "" },
            status.node_count,
            status.param_count,
            status.texture_count,
            status.rev
        ),
        ResponseBody::Warnings { warnings } => {
            if warnings.is_empty() {
                println!("no problems");
            }
            for w in warnings {
                println!("warning: {w}");
            }
        }
        ResponseBody::Preview { preview } => {
            println!(
                "preview -> {} ({}x{})",
                preview.path, preview.width, preview.height
            )
        }
        ResponseBody::Saved { path } => println!("saved -> {path}"),
        ResponseBody::ManifestRequirements { textures } => {
            if textures.is_empty() {
                println!("(no textures)");
            }
            for key in textures {
                println!("texture {key}");
            }
        }
        ResponseBody::Presence { presence } => match presence {
            None => println!("(no presence)"),
            Some(p) => {
                let pose: Vec<String> = p
                    .pose
                    .iter()
                    .map(|v| format!("{}={}", v.param, v.value))
                    .collect();
                println!(
                    "pose=[{}] selection={}",
                    pose.join(","),
                    p.selection
                        .as_ref()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
        },
        ResponseBody::Seams { seams } => {
            if seams.is_empty() {
                println!("(no seams)");
            }
            for seam in seams {
                let slots: Vec<String> = seam
                    .slots
                    .iter()
                    .map(|s| match s.vertex {
                        Some(v) => format!("{}=v{v}", s.id),
                        None => format!("{}=-", s.id),
                    })
                    .collect();
                println!("seam {}  [{}]", seam.id, slots.join(" "));
            }
        }
        ResponseBody::Welds { welds } => {
            if welds.is_empty() {
                println!("(no welds)");
            }
            for w in welds {
                let weights: Vec<String> = w
                    .weights
                    .iter()
                    .map(|s| format!("{}={}", s.slot, s.weight))
                    .collect();
                println!(
                    "weld {}:{} <-> {}:{}  [{}]",
                    w.a.node,
                    w.a.seam,
                    w.b.node,
                    w.b.seam,
                    weights.join(" ")
                );
            }
        }
        ResponseBody::UnfilledSlots { slots } => {
            if slots.is_empty() {
                println!("every slot is filled");
            }
            for s in slots {
                println!("unfilled {}:{}:{}", s.node, s.seam, s.slot);
            }
        }
        ResponseBody::Emptied { node, slots } => {
            if slots.is_empty() {
                println!("ok (no seam slots to refill)");
            } else {
                println!("ok; refill these slots on {node}:");
                for s in slots {
                    println!("  {}:{}", s.seam, s.slot);
                }
            }
        }
    }
}

/// Whether the author set that cell at all — the only thing that says so for
/// a deform binding, whose cells hold a vertex list rather than a number.
fn authored(binding: &BindingInfo, x: usize, y: usize) -> bool {
    binding
        .authored
        .get(y)
        .and_then(|row| row.get(x))
        .copied()
        .unwrap_or(false)
}

/// What an edit deleted on its way through. A texture with no part drawing it
/// is not a thing a model holds, so the edit that took the last user took the
/// texture — and an addon that named it by Id has no way back.
fn print_dropped(dropped: &[TexId]) {
    for t in dropped {
        println!("dropped texture {t}");
    }
}

/// One node, a field per line, each under the name `node set` takes — so a
/// value read here is a value that can be written straight back.
///
/// A field this node's kind does not carry is not printed: the reply leaves it
/// out rather than reporting a default nothing would accept.
fn print_node_info(node: &NodeInfo) {
    println!(
        "{} [{}] {} parent={}",
        node.id,
        node.kind,
        node.name,
        node.parent
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".into())
    );
    let [tx, ty, tz] = node.translate;
    let [rx, ry, rz] = node.rotate;
    let [sx, sy] = node.scale;
    println!("  translate {tx},{ty},{tz}");
    println!("  rotate {rx},{ry},{rz}");
    println!("  scale {sx},{sy}");
    println!("  z-order {}", node.z_order);
    println!("  enabled {}", node.enabled);
    println!("  lock-to-root {}", node.lock_to_root);
    if let Some(opacity) = node.opacity {
        println!("  opacity {opacity}");
    }
    if let Some(mode) = &node.blend_mode {
        println!("  blend-mode {mode}");
    }
    if let Some([r, g, b]) = node.tint {
        println!("  tint {r},{g},{b}");
    }
    if let Some([r, g, b]) = node.screen_tint {
        println!("  screen-tint {r},{g},{b}");
    }
    if let Some(threshold) = node.mask_threshold {
        println!("  mask-threshold {threshold}");
    }
    if let Some(texture) = &node.texture {
        println!("  texture {texture}");
    }
    if let Some(v) = node.propagate_meshgroup {
        println!("  propagate-meshgroup {v}");
    }
    if let Some(v) = node.mg_dynamic {
        println!("  mg-dynamic {v}");
    }
    if let Some(v) = node.mg_translate_children {
        println!("  mg-translate-children {v}");
    }
}

fn print_tree(node: &TreeNode, depth: usize) {
    println!(
        "{}{} [{}] {} (z={})",
        "  ".repeat(depth),
        node.id,
        node.kind,
        node.name,
        node.z_order
    );
    for child in &node.children {
        print_tree(child, depth + 1);
    }
}

use catchlight_editor_protocol::default_socket_path as socket_path;

fn current_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("catchlight-editor").join("current")
}

fn read_current() -> Result<Option<SessionId>> {
    match std::fs::read_to_string(current_path()) {
        Ok(s) => Ok(s.trim().parse::<u64>().ok().map(SessionId)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_current(session: SessionId) -> Result<()> {
    let path = current_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, session.0.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// The charset bans a leading `-` precisely so no Id ever has to be
    /// smuggled past clap's option parsing — `--help` says so, and the Id
    /// type refuses one before the socket.
    #[test]
    fn a_leading_dash_is_not_an_id() {
        assert!(ID_HELP.contains("[A-Za-z0-9_./-]"));
        assert!(ID_HELP.contains("starting with none of '.', '/' or '-'"));

        let Err(err) =
            Cli::try_parse_from(["cli", "--session", "1", "node", "delete", "--", "-hat"])
        else {
            panic!("a leading dash is not an id");
        };
        assert!(err.to_string().contains("'-'"), "{err}");
    }

    #[test]
    fn an_id_outside_the_charset_is_refused_before_the_socket() {
        let Err(err) = Cli::try_parse_from(["cli", "--session", "1", "node", "delete", "a b"])
        else {
            panic!("a space is not an id");
        };
        assert!(err.to_string().contains("invalid byte"), "{err}");
    }

    /// A binding names one param, or two — and the second is optional, so the
    /// common one-param form is unchanged.
    #[test]
    fn a_binding_can_name_two_params() {
        let one = Cli::try_parse_from([
            "cli",
            "--session",
            "1",
            "binding",
            "add",
            "--param",
            "pull",
            "--node",
            "body",
            "--target",
            "tx",
        ])
        .unwrap();
        match build_command(&one).unwrap() {
            Command::BindingAdd { params, .. } => assert!(params.param_y.is_none()),
            other => panic!("{other:?}"),
        }

        let two = Cli::try_parse_from([
            "cli",
            "--session",
            "1",
            "binding",
            "add",
            "--param",
            "head.x",
            "--param-y",
            "head.y",
            "--node",
            "body",
            "--target",
            "tx",
        ])
        .unwrap();
        match build_command(&two).unwrap() {
            Command::BindingAdd { params, .. } => {
                assert_eq!(params.param.as_str(), "head.x");
                assert_eq!(params.param_y.unwrap().as_str(), "head.y");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_pose_names_a_param_by_id() {
        let pose = parse_param("head.x=0.25").unwrap();
        assert_eq!(pose.param.as_str(), "head.x");
        assert!((pose.value - 0.25).abs() < 1e-6);
        assert!(parse_param("head.x").is_err(), "a pose needs a value");
        assert!(parse_param("head x=1").is_err(), "a pose needs a valid id");
    }
}
