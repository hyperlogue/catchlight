//! `catchlight-editor-cli` — a thin client for the editor server.

use std::io::Read;

mod compose;
mod extension;
#[cfg(unix)]
mod listen;
mod official;
mod review;
mod transport;
mod version;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use catchlight_editor_protocol::*;
use clap::{Parser, Subcommand};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;

const ID_HELP: &str = "\
Nodes, params, textures and slots are named by their Id: one or more of
the characters [A-Za-z0-9_./-], starting with none of '.', '/' or '-'. An Id is
what the .clm file stores, so it means the same thing in every session that
opened that file, and `node tree` / `param list` / `texture list` print the ones
a model carries.";

const AGENT_HELP: &str = "\
Working with a running editor (a person may be watching the same session):
  1. `session list` shows what is open, including models opened in the
     browser; `session use <id>` picks the one later commands target.
  2. Read before writing: `node tree`, `node info <id>`, `param list`, `check`.
  3. Add --json for machine-readable replies. A reply that changes the model
     carries the session's new `rev`; `--if-rev <rev>` refuses an edit if the
     model changed since that read, including edits made in the browser.
  4. `edit <file.json>` applies a JSON array of edits as one undo step;
     `request -` sends any protocol command from stdin.
  5. `preview --out view.png` renders the model as it is now; `undo` reverts.
  6. `review listen` returns at once; after it, each time the person sends
     comments from the browser, a notice arrives in your Claude Code, Codex
     or Antigravity session with the `review fetch` command to run. Do not
     wait or poll meanwhile. Exit 5: this harness cannot receive notices;
     run `review watch` in the background instead, which waits for them.
     `fetch` and `watch` print the comments, then acknowledge them and claim
     their threads for you. Exit 0: delivered, act on them; 3: `--timeout`
     passed; 4: they changed before the acknowledgment, fetch again; 1: they
     could not be saved, printed or acknowledged and stay pending. Change the
     model, answer each thread with `review reply <thread> -m <text>` (which
     ends your claim), then, unless you listen, watch again. Never resolve a
     thread: that is the person's call. `review worker status` shows who
     listens and any notice that failed.
  7. You are your harness session (`CATCHLIGHT_AGENT_SESSION` overrides it);
     `--name` is only what the person sees. `review claim <thread>...` holds
     threads for 60 minutes, renewed by claiming again, and refuses one
     another agent holds; `review release` lets go. After a failure, repeat a
     `reply` or `open` with the same `--key <k>`: it writes nothing twice.
  8. A comment may carry images. Pass `--attachments-dir <dir>` to `review
     watch`, `fetch` or `list`, and open every file it prints with a tool
     that shows you the image before you answer. Show your own evidence the
     same way: `preview --out after.png`, then
     `review reply <thread> -m <text> --attach after.png`.
  9. When a batch of review work is done, save a version and point the
     replies at it: `review reply <thread> -m <text> --save-version <label>`
     saves one and references it; `--version <n>` references one already
     saved. `version save -l <label>`, `version list`, `version export <n>
     --out <file>`, `version open <n>` (read-only) and `version restore <n>`
     (one undoable edit) do the rest. A version is not a revision: it is
     numbered per model and kept with it.
Edits appear in the browser as they land. The editor is found through the
connection file it writes; --server/--token name one explicitly.";

#[derive(Parser)]
#[command(
    name = "catchlight-editor-cli",
    about = "Drive a catchlight editor session",
    after_long_help = format!("{ID_HELP}\n\n{AGENT_HELP}")
)]
struct Cli {
    /// Print the raw reply JSON instead of a friendly summary.
    #[arg(long, global = true)]
    json: bool,
    /// Target session id (defaults to the remembered current session).
    #[arg(long, global = true)]
    session: Option<u64>,
    /// Expected revision for guarded edits and composed read/write operations.
    #[arg(long, global = true)]
    if_rev: Option<u64>,
    /// The editor's HTTP door, e.g. http://127.0.0.1:7323.
    #[arg(long, global = true, env = "CATCHLIGHT_EDITOR_URL")]
    server: Option<String>,
    /// Bearer token for --server; read from the connection file when omitted.
    #[arg(
        long,
        global = true,
        env = "CATCHLIGHT_EDITOR_TOKEN",
        hide_env_values = true
    )]
    token: Option<String>,
    /// A Unix socket to use instead of the default one.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send any typed protocol command from JSON; use `-` for stdin.
    Request {
        file: String,
        /// Attach local bytes as NAME=PATH (repeatable).
        #[arg(long = "attachment")]
        attachments: Vec<String>,
        /// File for a binary reply, such as model_export or preview_render.
        #[arg(long)]
        out: Option<String>,
    },
    /// Apply an atomic JSON array of EditOp values, or validate it only.
    Edit {
        file: String,
        #[arg(long)]
        validate: bool,
    },
    /// Inspect retained branches and activation revisions.
    History,
    /// Navigate to a retained creation or activation revision.
    Goto { revision: u64 },
    /// Export a guarded CLM snapshot without changing save state.
    Export { out: String },
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
    /// List model problems (untextured parts, bad meshes, unfilled vertex slots, …).
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
    /// Vertex slots: handles on a part's vertices, for welds and motion responses.
    Slot {
        #[command(subcommand)]
        action: SlotCmd,
    },
    /// Welds: pair two parts, vertex slot by vertex slot.
    Weld {
        #[command(subcommand)]
        action: WeldCmd,
    },
    /// Author exact deform offsets; affine input is evaluated around the rest
    /// mesh origin, independent of the posed model.
    Deform {
        #[command(subcommand)]
        action: DeformCmd,
    },
    /// Undo the last edit on the session.
    Undo,
    /// Redo the last undone edit.
    Redo,
    /// Shared view state (pose / selection) — a separate path from the model.
    Presence {
        #[command(subcommand)]
        action: PresenceCmd,
    },
    /// Review threads: comments the person leaves on the model for an agent.
    Review(review::ReviewArgs),
    /// Saved versions: numbered snapshots of the model to compare, restore or
    /// point a reply at.
    Version(version::VersionArgs),
    /// Vendor data the model carries under its own key, and every
    /// extension's warnings.
    Extension {
        #[command(subcommand)]
        action: extension::ExtensionCmd,
    },
    /// The control mappings of `catchlight.humanoid`.
    Humanoid {
        #[command(subcommand)]
        action: official::HumanoidCmd,
    },
    /// The named expressions of `catchlight.expressions`.
    Expression {
        #[command(subcommand)]
        action: official::ExpressionCmd,
    },
    /// The visemes of `catchlight.expressions`, kept beside its expressions.
    Viseme {
        #[command(subcommand)]
        action: official::VisemeCmd,
    },
    /// The clips of `catchlight.animations`.
    Clip {
        #[command(subcommand)]
        action: official::ClipCmd,
    },
    /// Render a preview to a PNG.
    Preview {
        /// Where the PNG is written.
        #[arg(long)]
        out: String,
        /// Pose a param: `--param <id>=0.5` (repeatable).
        #[arg(long = "param")]
        params: Vec<String>,
        /// Output size, e.g. `512x512`.
        #[arg(long)]
        size: Option<String>,
        /// Orthographic height in world units (defaults to the editor's).
        #[arg(long = "camera-height")]
        camera_height: Option<f32>,
        /// Camera centre, e.g. `0,120`. Needs `--camera-height`.
        #[arg(long = "camera-center")]
        camera_center: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Start a new empty model.
    New {
        #[arg(long)]
        name: Option<String>,
        /// Initialize from a complete local CLM in one request.
        #[arg(long)]
        clm: Option<String>,
    },
    /// Fork the captured model into a clean session with independent history.
    Fork {
        #[arg(long)]
        name: Option<String>,
    },
    /// Open an existing `.clm`.
    Open { path: String },
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
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Print the node tree (Ids and names).
    Tree,
    /// Print one node in full, with the field names `node set` takes.
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
        /// The Id to create it under; without one the editor draws a free one.
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
        /// Draw no texture; the last part drawing a texture takes it with it.
        #[arg(long = "clear-texture", conflicts_with = "texture")]
        clear_texture: bool,
        #[arg(long = "lock-to-root")]
        lock_to_root: Option<bool>,
        /// normal | multiply | color_dodge | …
        #[arg(long = "blend-mode", value_parser = wire_enum::<BlendMode>)]
        blend_mode: Option<BlendMode>,
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

#[derive(Subcommand)]
enum SlotCmd {
    /// Add an unfilled vertex slot to a part. Without `--slot` the editor draws one.
    Add {
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        slot: Option<SlotId>,
    },
    /// Point a slot at one of the part's vertices.
    Fill {
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        slot: SlotId,
        #[arg(long)]
        vertex: u32,
    },
    /// Unfill a slot; weld pairs naming it skip it until it is refilled.
    Clear {
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        slot: SlotId,
    },
    /// Remove a slot and the weld pairs that name it.
    Delete {
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        slot: SlotId,
    },
    /// Rename a slot's Id on one part. Every weld pair that named it follows.
    Rename {
        #[arg(long)]
        node: NodeId,
        #[arg(long)]
        slot: SlotId,
        #[arg(long)]
        to: SlotId,
    },
    /// Print a part's slots and what fills each.
    List {
        #[arg(long)]
        node: NodeId,
    },
    /// List every vertex slot in the model that no vertex fills.
    Unfilled,
}

#[derive(Subcommand)]
enum WeldCmd {
    /// Weld two parts, replacing any existing weld. Each repeatable `--pair` is
    /// `<slot on --a>:<slot on --b>:<weight>`, the weight being `--a`'s share
    /// in 0..=1.
    Set {
        #[arg(long)]
        a: NodeId,
        #[arg(long)]
        b: NodeId,
        #[arg(long = "pair")]
        pairs: Vec<String>,
    },
    /// Set one weld pair's weight: `--slot` is on `--a`, and `--weight` is
    /// `--a`'s share in 0..=1, whichever way round the weld is stored.
    Weight {
        #[arg(long)]
        a: NodeId,
        #[arg(long)]
        b: NodeId,
        #[arg(long)]
        slot: SlotId,
        #[arg(long)]
        weight: f32,
    },
    /// Unweld two parts, named in either order; their slots stay.
    Delete {
        #[arg(long)]
        a: NodeId,
        #[arg(long)]
        b: NodeId,
    },
    /// List the model's welds.
    List,
}

#[derive(Subcommand)]
enum TextureCmd {
    /// Add a PNG/TGA texture and make a part draw it, in one edit. A texture
    /// the part was the last to draw goes with it.
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
    /// Create a scalar input parameter. Key positions belong to bindings.
    Add {
        name: String,
        #[arg(long, allow_hyphen_values = true)]
        min: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        max: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        default: Option<f32>,
        /// The Id to create it under; without one the editor draws a free one.
        #[arg(long)]
        id: Option<ParamId>,
    },
    /// List inputs, ranges, defaults and binding counts.
    List,
    /// Change param metadata; normalized key positions survive a range change.
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
}

#[derive(clap::Args)]
struct BindingParamsArg {
    #[arg(long)]
    param: ParamId,
    /// A second param; `--cell x,y` then indexes both grid axes.
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

#[derive(clap::Args)]
struct BindingSelection {
    #[command(flatten)]
    params: BindingParamsArg,
    #[arg(long)]
    node: NodeId,
    #[arg(long, value_parser = wire_enum::<BindingTarget>)]
    target: BindingTarget,
}

#[derive(Subcommand)]
enum BindingCmd {
    /// Insert a normalized key on one binding-owned axis.
    KeyInsert {
        #[command(flatten)]
        selection: BindingSelection,
        #[arg(long)]
        axis: ParamId,
        #[arg(long)]
        value: f32,
    },
    /// Delete one key and its authored row or column; retain at least one key.
    KeyDelete {
        #[command(flatten)]
        selection: BindingSelection,
        #[arg(long)]
        axis: ParamId,
        #[arg(long)]
        index: u32,
    },
    /// Move one normalized key without changing authored cell values.
    KeyMove {
        #[command(flatten)]
        selection: BindingSelection,
        #[arg(long)]
        axis: ParamId,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        value: f32,
    },
    /// Mirror one binding grid axis and its authored cells.
    Flip {
        #[command(flatten)]
        selection: BindingSelection,
        #[arg(long)]
        axis: ParamId,
    },
    /// Create an un-authored binding, including deform. Axes default to endpoints.
    Add {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
        /// JSON array of normalized axes, for example [[0,0.5,1]].
        #[arg(long)]
        keys: Option<String>,
    },
    /// Write one exact scalar cell; an absent binding gets default endpoint axes.
    Key {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<ScalarTarget>)]
        target: ScalarTarget,
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
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
        #[arg(long)]
        cell: String,
    },
    /// Author the identity value at a keypoint.
    Reset {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
        #[arg(long)]
        cell: String,
    },
    /// Delete a whole binding.
    Delete {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
    },
    /// Set interpolation: nearest | stepped | linear | cubic.
    Interpolate {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
        #[arg(long, value_parser = wire_enum::<Interpolate>)]
        mode: Interpolate,
    },
    /// Negate every authored value.
    Invert {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
    },
    /// List a node's bindings, each with its authored key grid.
    List {
        #[arg(long)]
        node: NodeId,
    },
    /// Copy an authored cell verbatim; use --derived to accept a derived hole.
    CopyKey {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        derived: bool,
    },
}

#[derive(Subcommand)]
enum MaskCmd {
    /// Append a mask source (a Part) to a Part/Composite. `--mode mask|dodge`.
    Add {
        node: NodeId,
        #[arg(long)]
        source: NodeId,
        #[arg(long, default_value = "mask", value_parser = wire_enum::<MaskMode>)]
        mode: MaskMode,
    },
    /// Change the mode of the mask at `--index`.
    Set {
        node: NodeId,
        #[arg(long)]
        index: u32,
        #[arg(long, value_parser = wire_enum::<MaskMode>)]
        mode: MaskMode,
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
}

#[derive(Subcommand)]
enum DeformCmd {
    /// Write a raw deform contribution around the rest mesh origin. An absent
    /// binding gets default endpoint axes.
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
    /// Write exact per-vertex offsets from a JSON array; no pose conversion.
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

pub fn command_names() -> Vec<String> {
    use clap::CommandFactory;
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .collect()
}

/// Which running editor a command reaches when no flag names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    SocketFirst,
    ConnectionFirst,
}

/// `args` starts with the program name; `name` replaces it in help and errors.
pub fn run<I, T>(name: &'static str, discovery: Discovery, args: I) -> Result<ExitCode>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::{CommandFactory, FromArgMatches};
    let matches = Cli::command()
        .name(name)
        .bin_name(name)
        .get_matches_from(args);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    if let Cmd::Session {
        action: SessionCmd::Use { id },
    } = &cli.cmd
    {
        write_current(SessionId(*id))?;
        println!("using session {id}");
        return Ok(ExitCode::SUCCESS);
    }

    if let Cmd::Review(args) = &cli.cmd {
        if let Some(code) = review::run_local(&cli, args)? {
            return Ok(exit_code(code));
        }
    }

    let (mut transport, endpoint) = transport::resolve(&transport::Target {
        server: cli.server.clone(),
        token: cli.token.clone(),
        socket: cli.socket.clone(),
        connection_first: discovery == Discovery::ConnectionFirst,
    })?;
    if let Cmd::Review(args) = &cli.cmd {
        let reached = review::Reached {
            program: name,
            endpoint: &endpoint,
        };
        return Ok(exit_code(review::run(
            &cli,
            args,
            &mut transport,
            &reached,
        )?));
    }
    if let Cmd::Version(args) = &cli.cmd {
        return Ok(exit_code(version::run(&cli, args, &mut transport)?));
    }
    let set_matches = |group: &str| {
        matches
            .subcommand_matches(group)
            .and_then(|m| m.subcommand_matches("set"))
    };
    let typed = match &cli.cmd {
        Cmd::Extension { action } => Some(extension::run(&cli, action, &mut transport)?),
        Cmd::Humanoid { action } => Some(official::humanoid(&cli, action, &mut transport)?),
        Cmd::Expression { action } => Some(official::expression(
            &cli,
            ExpressionGroup::Expression,
            action,
            set_matches("expression"),
            &mut transport,
        )?),
        Cmd::Viseme { action } => Some(official::viseme(
            &cli,
            action,
            set_matches("viseme"),
            &mut transport,
        )?),
        Cmd::Clip { action } => Some(official::clip(&cli, action, &mut transport)?),
        _ => None,
    };
    if let Some(code) = typed {
        return Ok(exit_code(code));
    }
    let composed = match compose::execute(&cli, &mut transport)? {
        Some(compose::Output::Reply(reply)) => Some(reply),
        Some(compose::Output::Unfilled { revision, slots }) => {
            if cli.json {
                println!("{}", serde_json::json!({"rev": revision, "slots": slots}));
            } else if slots.is_empty() {
                println!("every vertex slot is filled");
            } else {
                for slot in slots {
                    println!("unfilled {}:{}", slot.node, slot.slot);
                }
            }
            return Ok(ExitCode::SUCCESS);
        }
        None => None,
    };
    let persist = matches!(
        cli.cmd,
        Cmd::Session {
            action: SessionCmd::New { .. } | SessionCmd::Open { .. } | SessionCmd::Fork { .. }
        }
    );

    let reply = match composed {
        Some(reply) => reply,
        None => transport.call(
            Request {
                id: 1,
                command: build_command(&cli)?,
            },
            siblings(&cli)?,
        )?,
    };

    if persist {
        if let Reply::Ok {
            body: ResponseBody::Session { session } | ResponseBody::SessionFork { session, .. },
            ..
        } = &reply
        {
            write_current(*session)?;
        }
    }

    print_reply(&cli, &reply);
    Ok(if matches!(reply, Reply::Err { .. }) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// A status the platform cannot represent fails rather than passing as 0.
fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

fn build_command(cli: &Cli) -> Result<Command> {
    Ok(match &cli.cmd {
        Cmd::Request { file, .. } => serde_json::from_str(&read_json(file)?)?,
        Cmd::Edit { .. } | Cmd::History | Cmd::Goto { .. } | Cmd::Export { .. } => {
            unreachable!("composed before building")
        }
        Cmd::Session { action } => match action {
            SessionCmd::New { name, clm } => Command::SessionNew {
                name: name.clone(),
                source: clm.as_ref().map(|_| SessionSource::Clm {}),
            },
            SessionCmd::Fork { .. } => unreachable!("composed before building"),
            SessionCmd::Open { path } => Command::SessionOpen { path: path.clone() },
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
                    id,
                } => Command::ParamAdd {
                    session,
                    name: name.clone(),
                    min: min.unwrap_or(0.0),
                    max: max.unwrap_or(1.0),
                    default: default.unwrap_or(0.0),
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
            }
        }
        Cmd::Binding { action } => {
            let session = resolve_session(cli)?;
            match action {
                BindingCmd::Add {
                    params,
                    node,
                    target,
                    keys,
                } => Command::BindingAdd {
                    key_positions: keys
                        .as_ref()
                        .map(|keys| serde_json::from_str(keys))
                        .transpose()?,
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: *target,
                },
                BindingCmd::Key { .. }
                | BindingCmd::Unset { .. }
                | BindingCmd::Reset { .. }
                | BindingCmd::KeyInsert { .. }
                | BindingCmd::KeyDelete { .. }
                | BindingCmd::KeyMove { .. }
                | BindingCmd::Flip { .. } => unreachable!("composed before building"),
                BindingCmd::Delete {
                    params,
                    node,
                    target,
                } => Command::BindingDelete {
                    session,
                    params: params.wire(),
                    node: node.clone(),
                    target: *target,
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
                    target: *target,
                    mode: *mode,
                },
                BindingCmd::Invert { .. } => unreachable!("composed before building"),
                BindingCmd::List { node } => Command::BindingList {
                    session,
                    node: node.clone(),
                },
                BindingCmd::CopyKey { .. } => unreachable!("composed before building"),
            }
        }
        Cmd::Mesh { action } => {
            let session = resolve_session(cli)?;
            match action {
                MeshCmd::Set { node, file } => {
                    #[derive(serde::Deserialize)]
                    struct MeshJson {
                        verts: Vec<[f32; 2]>,
                        uvs: Vec<[f32; 2]>,
                        indices: Vec<[u32; 3]>,
                        #[serde(default)]
                        origin: [f32; 2],
                    }
                    let text =
                        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
                    let m: MeshJson =
                        serde_json::from_str(&text).with_context(|| format!("parsing {file}"))?;
                    Command::MeshSet {
                        if_rev: cli.if_rev,
                        deform_mapping: None,
                        session,
                        node: node.clone(),
                        verts: m.verts,
                        uvs: m.uvs,
                        indices: m.indices,
                        origin: m.origin,
                    }
                }
                MeshCmd::Copy { .. } => unreachable!("composed before building"),
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
                },
            }
        }
        Cmd::Slot { action } => {
            let session = resolve_session(cli)?;
            match action {
                SlotCmd::Add { node, slot } => Command::SlotAdd {
                    session,
                    node: node.clone(),
                    slot: slot.clone(),
                },
                SlotCmd::Fill { node, slot, vertex } => Command::SlotFill {
                    session,
                    node: node.clone(),
                    slot: slot.clone(),
                    vertex: *vertex,
                },
                SlotCmd::Clear { node, slot } => Command::SlotClear {
                    session,
                    node: node.clone(),
                    slot: slot.clone(),
                },
                SlotCmd::Delete { node, slot } => Command::SlotDelete {
                    session,
                    node: node.clone(),
                    slot: slot.clone(),
                },
                SlotCmd::Rename { node, slot, to } => Command::RenameId {
                    session,
                    rename: Rename::Slot {
                        node: node.clone(),
                        from: slot.clone(),
                        to: to.clone(),
                    },
                },
                SlotCmd::List { node } => Command::Slots {
                    session,
                    node: node.clone(),
                },
                SlotCmd::Unfilled => unreachable!("composed before building"),
            }
        }
        Cmd::Weld { action } => {
            let session = resolve_session(cli)?;
            match action {
                WeldCmd::Set { a, b, pairs } => Command::WeldSet {
                    session,
                    a: a.clone(),
                    b: b.clone(),
                    pairs: pairs.iter().map(|p| parse_pair(p)).collect::<Result<_>>()?,
                },
                WeldCmd::Weight { .. } => unreachable!("composed before building"),
                WeldCmd::Delete { a, b } => Command::WeldDelete {
                    session,
                    a: a.clone(),
                    b: b.clone(),
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
                    mode: *mode,
                },
                MaskCmd::Set { .. } | MaskCmd::Reorder { .. } => {
                    unreachable!("composed before building")
                }
                MaskCmd::Delete { node, index } => Command::MaskDelete {
                    session,
                    node: node.clone(),
                    index: *index,
                },
            }
        }
        Cmd::Deform { .. } => unreachable!("composed before building"),
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
            }
        }
        Cmd::Undo | Cmd::Redo => unreachable!("composed before building"),
        Cmd::Review(_)
        | Cmd::Version(_)
        | Cmd::Extension { .. }
        | Cmd::Humanoid { .. }
        | Cmd::Expression { .. }
        | Cmd::Viseme { .. }
        | Cmd::Clip { .. } => unreachable!("handled in run"),
        Cmd::Texture { action } => match action {
            TextureCmd::Add { node, path, id } => Command::TextureAdd {
                session: resolve_session(cli)?,
                node: node.clone(),
                encoding: encoding_of(path),
                texture: id.clone(),
            },
            TextureCmd::List => Command::TextureList {
                session: resolve_session(cli)?,
            },
        },
        Cmd::Preview {
            out: _,
            params,
            size,
            camera_height,
            camera_center,
        } => Command::Preview {
            session: resolve_session(cli)?,
            pose: params
                .iter()
                .map(|p| parse_param(p))
                .collect::<Result<_>>()?,
            size: size.as_deref().map(parse_size).transpose()?,
            camera: camera_height
                .map(|height| -> Result<Camera> {
                    Ok(Camera {
                        center: camera_center
                            .as_deref()
                            .map(parse_center)
                            .transpose()?
                            .unwrap_or([0.0, 0.0]),
                        height,
                    })
                })
                .transpose()?,
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
                texture: match (clear_texture, texture) {
                    (true, _) => Some(None),
                    (false, Some(tex)) => Some(Some(tex.clone())),
                    (false, None) => None,
                },
                lock_to_root: *lock_to_root,
                blend_mode: *blend_mode,
                tint: tint.as_deref().map(parse_vec3).transpose()?,
                screen_tint: screen_tint.as_deref().map(parse_vec3).transpose()?,
                mask_threshold: *mask_threshold,
                propagate_meshgroup: *propagate_meshgroup,
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
        NodeCmd::Move { .. } => unreachable!("composed before building"),
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

/// Parses as the wire does, so serde's error names the alternatives.
fn wire_enum<T: serde::de::DeserializeOwned>(s: &str) -> std::result::Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|e| e.to_string())
}

fn parse_kind(s: &str) -> Result<NodeKindArg> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "group" => NodeKindArg::Group,
        "part" => NodeKindArg::Part,
        "composite" => NodeKindArg::Composite,
        "motion_response" => NodeKindArg::MotionResponse,
        "meshgroup" | "mesh_group" => NodeKindArg::MeshGroup,
        other => {
            bail!("unknown node kind {other:?} (group|part|composite|meshgroup|motion_response)")
        }
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

fn read_json(path: &str) -> Result<String> {
    if path == "-" {
        read_command_json(std::io::stdin().lock())
    } else {
        let file = std::fs::File::open(path).with_context(|| format!("reading {path}"))?;
        read_command_json(file).with_context(|| format!("reading {path}"))
    }
}

fn read_command_json(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_COMMAND_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_COMMAND_BYTES {
        bail!("JSON input exceeds the 1 MiB command limit; split independent edits into explicit batches");
    }
    Ok(String::from_utf8(bytes)?)
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

/// The server's working directory is not this process's.
fn absolute(path: &str) -> Result<String> {
    Ok(std::path::absolute(path)?.display().to_string())
}

fn describe_extension(value: &ExtensionValueInfo) -> String {
    match value {
        ExtensionValueInfo::Json { value } => {
            serde_json::to_string(value).unwrap_or_else(|_| "<unprintable json>".into())
        }
        ExtensionValueInfo::Bytes { size, hash } => format!("{size} bytes  {hash}"),
        ExtensionValueInfo::Maintained {
            version,
            refs,
            value,
            size,
            hash,
        } => {
            let payload = match (value, size, hash) {
                (Some(value), _, _) => {
                    serde_json::to_string(value).unwrap_or_else(|_| "<unprintable json>".into())
                }
                (None, Some(size), Some(hash)) => format!("{size} bytes  {hash}"),
                _ => String::new(),
            };
            let refs = serde_json::to_string(refs).unwrap_or_default();
            format!("version {version}  refs {refs}  {payload}")
        }
    }
}

/// `<x>,<y>` in world units.
fn parse_center(s: &str) -> Result<[f32; 2]> {
    let (x, y) = s
        .split_once(',')
        .ok_or_else(|| anyhow!("a camera centre must look like 0,120, got {s:?}"))?;
    Ok([x.trim().parse()?, y.trim().parse()?])
}

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

fn parse_pair(s: &str) -> Result<SlotPair> {
    let bad = || anyhow!("a pair must look like <a slot>:<b slot>:<weight>, got {s:?}");
    let [a, b, weight] = s.split(':').collect::<Vec<_>>()[..] else {
        return Err(bad());
    };
    Ok(SlotPair {
        a: SlotId::new(a.trim())?,
        b: SlotId::new(b.trim())?,
        weight: weight
            .trim()
            .parse::<f32>()
            .map_err(|e| anyhow!("bad weight {weight:?}: {e}"))?,
    })
}

fn read_offsets(path: &str) -> Result<Vec<[f32; 2]>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {path}"))
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

/// `files` and `out` paths are resolved here because the server shares this
/// filesystem but not this working directory.
fn siblings(cli: &Cli) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut out = serde_json::Map::new();
    match &cli.cmd {
        Cmd::Session {
            action: SessionCmd::New {
                clm: Some(path), ..
            },
        } => {
            out.insert(
                "files".into(),
                serde_json::json!({"model": absolute(path)?}),
            );
        }
        Cmd::Request {
            attachments,
            out: path,
            ..
        } => {
            let mut files = serde_json::Map::new();
            for attachment in attachments {
                let (name, path) = attachment
                    .split_once('=')
                    .ok_or_else(|| anyhow!("attachment must be NAME=PATH"))?;
                if name.is_empty() || files.insert(name.into(), absolute(path)?.into()).is_some() {
                    bail!("attachment names must be nonempty and unique");
                }
            }
            if !files.is_empty() {
                out.insert("files".into(), files.into());
            }
            if let Some(path) = path {
                out.insert("out".into(), absolute(path)?.into());
            }
        }
        Cmd::Preview { out: path, .. } | Cmd::Export { out: path } => {
            out.insert("out".into(), absolute(path)?.into());
        }
        Cmd::Texture {
            action: TextureCmd::Add { path, .. },
        } => {
            let files = serde_json::json!({ "texture": absolute(path)? });
            out.insert("files".into(), files);
        }
        _ => {}
    }
    Ok(out)
}

fn encoding_of(path: &str) -> TextureEncoding {
    if path.to_ascii_lowercase().ends_with(".tga") {
        TextureEncoding::Tga
    } else {
        TextureEncoding::Png
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

fn code_name(code: ErrorCode) -> String {
    serde_json::to_string(&code)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| "error".into())
}

fn print_body(body: &ResponseBody) {
    match body {
        ResponseBody::MeshInfo { .. }
        | ResponseBody::ModelStructure { .. }
        | ResponseBody::GeometrySample { .. }
        | ResponseBody::EditHistory { .. }
        | ResponseBody::EditResults { .. }
        | ResponseBody::BindingCells { .. }
        | ResponseBody::SessionFork { .. }
        | ResponseBody::ModelExport { .. }
        | ResponseBody::Version { .. }
        | ResponseBody::Versions { .. }
        | ResponseBody::HumanoidMapped { .. }
        | ResponseBody::HumanoidUnmapped { .. }
        | ResponseBody::Humanoid { .. }
        | ResponseBody::Expression { .. }
        | ResponseBody::ExpressionDeleted { .. }
        | ResponseBody::Expressions { .. }
        | ResponseBody::VisemesAdded { .. }
        | ResponseBody::Clips { .. } => {
            println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
        }
        ResponseBody::ExtensionWarnings { warnings } => official::print_warnings(warnings),
        ResponseBody::Empty => println!("ok"),
        ResponseBody::Session { session } => println!("session {}", session.0),
        ResponseBody::Sessions { sessions } => {
            if sessions.is_empty() {
                println!("(no open sessions)");
            }
            for s in sessions {
                let viewing = s.viewing.map_or(String::new(), |v| {
                    format!(
                        "  read-only: version {} of session {}",
                        v.version, v.session.0
                    )
                });
                println!(
                    "session {}  {}{}  nodes={} rev={}{viewing}",
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
        ResponseBody::Slot { slot } => println!("slot {} on {}", slot.slot, slot.node),
        ResponseBody::Extensions { extensions } => {
            if extensions.is_empty() {
                println!("(no extensions)");
            }
            for e in extensions {
                println!("extension {}  {}", e.key, describe_extension(&e.value));
            }
        }
        ResponseBody::Extension { key, value } => {
            println!("extension {key}  {}", describe_extension(value));
        }
        ResponseBody::Params { params } => {
            if params.is_empty() {
                println!("(no params)");
            }
            for p in params {
                let driven = match &p.driven_by {
                    Some(node) => format!(" driven_by={node}"),
                    None => String::new(),
                };
                println!(
                    "param {}  {}  [{}, {}] default={} bindings={}{driven}",
                    p.id, p.name, p.min, p.max, p.default, p.bindings
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
                println!("  axes={:?}", b.key_positions);
                for (y, row) in b.keys.iter().enumerate() {
                    let cells: Vec<String> = row
                        .iter()
                        .enumerate()
                        .map(|(x, value)| match value {
                            Some(v) => format!("{v}"),
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
            println!("preview {}x{}", preview.width, preview.height)
        }

        ResponseBody::Saved { path } => println!("saved -> {path}"),
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
        ResponseBody::Slots { slots } => {
            if slots.is_empty() {
                println!("(no slots)");
            }
            for s in slots {
                match s.vertex {
                    Some(v) => println!("slot {}  v{v}", s.id),
                    None => println!("slot {}  -", s.id),
                }
            }
        }
        ResponseBody::Welds { welds } => {
            if welds.is_empty() {
                println!("(no welds)");
            }
            for w in welds {
                let pairs: Vec<String> = w
                    .pairs
                    .iter()
                    .map(|p| format!("{}:{}={}", p.a, p.b, p.weight))
                    .collect();
                println!("weld {} <-> {}  [{}]", w.a, w.b, pairs.join(" "));
            }
        }
        ResponseBody::Emptied { node, slots } => {
            if slots.is_empty() {
                println!("ok (no slots to refill)");
            } else {
                println!("ok; refill these slots on {node}:");
                for s in slots {
                    println!("  {s}");
                }
            }
        }
        ResponseBody::Review { .. }
        | ResponseBody::ReviewThread { .. }
        | ResponseBody::ReviewPending { .. }
        | ResponseBody::ReviewImage { .. }
        | ResponseBody::ReviewFetched { .. }
        | ResponseBody::ReviewAcked { .. }
        | ResponseBody::ReviewClaimed { .. } => {
            print!("{}", review::describe(body, true, None).unwrap_or_default());
        }
        ResponseBody::SpineFit { node, params } => {
            println!("spine {node}");
            for (link, param) in params.iter().enumerate() {
                println!("  link {} <- {param}", link + 1);
            }
        }
    }
}

/// The only authored signal for deform cells, which hold no scalar.
fn authored(binding: &BindingInfo, x: usize, y: usize) -> bool {
    binding
        .authored
        .get(y)
        .and_then(|row| row.get(x))
        .copied()
        .unwrap_or(false)
}

fn print_dropped(dropped: &[TexId]) {
    for t in dropped {
        println!("dropped texture {t}");
    }
}

/// Field names match `node set` flags so values can be written straight back.
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
    if let (Some(verts), Some(tris)) = (node.vertex_count, node.triangle_count) {
        println!("  mesh {verts} verts, {tris} tris");
    }
    if let Some(v) = node.propagate_meshgroup {
        println!("  propagate-meshgroup {v}");
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

fn current_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .or_else(|| std::env::var_os("LOCALAPPDATA").filter(|_| cfg!(windows)))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("catchlight-editor").join("current")
}

fn read_current() -> Result<Option<SessionId>> {
    let path = current_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(s.trim().parse::<u64>().ok().map(SessionId)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_current(session: SessionId) -> Result<()> {
    let path = current_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, session.0.to_string())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_json_stops_reading_at_the_input_limit() {
        let mut source = std::io::repeat(b' ').take(u64::MAX);
        let error = read_command_json(&mut source).unwrap_err();
        assert!(error.to_string().contains("1 MiB command limit"));
        assert_eq!(source.limit(), u64::MAX - MAX_COMMAND_BYTES as u64 - 1);
        let exact = std::io::repeat(b' ').take(MAX_COMMAND_BYTES as u64);
        assert_eq!(read_command_json(exact).unwrap().len(), MAX_COMMAND_BYTES);
        assert!(read_command_json(&[0xff][..]).is_err());
    }

    #[test]
    fn a_missing_input_file_is_named_in_the_error() {
        let missing = "no-such-dir/edits.json";
        for error in [
            read_json(missing).unwrap_err(),
            read_offsets(missing).unwrap_err(),
        ] {
            assert!(format!("{error:#}").contains(missing), "{error:#}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_refused_command_returns_failure_to_the_caller() {
        use std::io::{BufRead, Write};
        let dir = std::env::temp_dir().join(format!("clirun{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("e.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let editor = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap();
            let reply = Reply::Err {
                id: 1,
                code: catchlight_editor_protocol::ErrorCode::NoNode,
                message: "no node".into(),
                op_index: None,
                limit: None,
            };
            writeln!(&stream, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
        });
        let socket = path.to_str().unwrap();
        let args = [
            "cli",
            "--socket",
            socket,
            "--session",
            "1",
            "node",
            "delete",
            "x",
        ];
        let code = run("cli", Discovery::SocketFirst, args).unwrap();
        editor.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(code, ExitCode::FAILURE);
    }

    #[test]
    fn the_cli_definition_is_well_formed() {
        Cli::command().debug_assert();
    }

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
