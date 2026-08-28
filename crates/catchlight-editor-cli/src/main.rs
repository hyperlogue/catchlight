//! `catchlight-editor-cli` — a thin client for the editor server.
//!
//! Each invocation builds one [`Command`], connects to the canonical socket
//! exposed by an editor or standalone server, sends it, and prints the reply.
//! The "current session" is remembered in a small local file so most commands
//! need no `--session`. Unix-only by design.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use catchlight_editor_protocol::*;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "catchlight-editor-cli",
    about = "Drive a catchlight editor session"
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
    /// Save the session to a `.clp` (defaults to its existing file).
    Save { path: Option<String> },
    /// Export the editable JSON manifest + textures next to `path`.
    ExportManifest { path: String },
    /// Print a one-line summary of the session.
    Status,
    /// List rig problems (untextured parts, bad meshes, …).
    Check,
    /// Node tree operations.
    Node {
        #[command(subcommand)]
        action: NodeCmd,
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
        /// Pose a param: `--param name=0.5` or `--param name=0.5,0.3` (repeatable).
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
    /// Start a new empty puppet.
    New {
        #[arg(long)]
        name: Option<String>,
    },
    /// Open an existing `.clp`.
    Open { path: String },
    /// Build a puppet from a JSON manifest + its textures.
    Import { manifest: String },
    /// List open sessions.
    List,
    /// Remember a session as the default for later commands.
    Use { id: u64 },
    /// Close a session.
    Close,
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Print the node tree (with refs).
    Tree,
    /// Add a child node.
    Add {
        #[arg(long)]
        parent: u64,
        /// empty | part | composite | meshgroup
        #[arg(long)]
        kind: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// Change fields on a node.
    Set {
        node: u64,
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
        zsort: Option<f32>,
        #[arg(long, allow_hyphen_values = true)]
        opacity: Option<f32>,
        #[arg(long)]
        enabled: Option<bool>,
        #[arg(long)]
        texture: Option<u64>,
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
    /// Move a node under a new parent.
    Reparent {
        node: u64,
        #[arg(long)]
        to: u64,
    },
    /// Move a node within its parent's children (index clamps to the end).
    Reorder {
        node: u64,
        #[arg(long)]
        index: u32,
    },
    /// Reparent + position in one step: become `--parent`'s child at `--index`.
    Move {
        node: u64,
        #[arg(long)]
        parent: u64,
        #[arg(long)]
        index: u32,
    },
    /// Deep-copy a node's subtree as its next sibling.
    Duplicate { node: u64 },
    /// Delete a node and its subtree.
    Delete { node: u64 },
}

#[derive(Subcommand)]
enum MeshCmd {
    /// Replace a node's mesh from a JSON file
    /// `{"verts": [...], "uvs": [...], "indices": [...], "origin": [x, y]}`.
    /// Deform bindings on the node are re-fitted in the same undo step.
    Apply {
        node: u64,
        #[arg(long)]
        file: String,
    },
    /// Copy `--from`'s mesh onto `node` (same re-fit).
    Copy {
        node: u64,
        #[arg(long)]
        from: u64,
    },
}

#[derive(Subcommand)]
enum TextureCmd {
    /// Register a PNG/TGA texture from a file.
    Add { path: String },
    /// List the session's textures.
    List,
}

#[derive(Subcommand)]
enum ParamCmd {
    /// Create a param. `--axis-x a,b,c` sets keypoints (defaults to min,max).
    Add {
        name: String,
        #[arg(long)]
        vec2: bool,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        min: Option<String>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        max: Option<String>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        defaults: Option<String>,
        #[arg(long = "axis-x", allow_hyphen_values = true)]
        axis_x: Option<String>,
        #[arg(long = "axis-y", allow_hyphen_values = true)]
        axis_y: Option<String>,
    },
    /// List params (with axis grid + binding counts).
    List,
    /// Change param metadata (range changes rescale axis points).
    Set {
        param: u64,
        #[arg(long)]
        name: Option<String>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        min: Option<String>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        max: Option<String>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true)]
        defaults: Option<String>,
    },
    /// Delete a param (and its bindings).
    Delete { param: u64 },
    /// Insert an axis point. `--axis 0|1`.
    AxisInsert {
        param: u64,
        #[arg(long)]
        axis: u8,
        #[arg(long, allow_hyphen_values = true)]
        value: f32,
    },
    /// Remove an interior axis point by index.
    AxisDelete {
        param: u64,
        #[arg(long)]
        axis: u8,
        #[arg(long)]
        index: u32,
    },
    /// Move an interior axis point.
    AxisMove {
        param: u64,
        #[arg(long)]
        axis: u8,
        #[arg(long)]
        index: u32,
        #[arg(long, allow_hyphen_values = true)]
        value: f32,
    },
    /// Mirror the param along an axis (values untouched).
    Flip {
        param: u64,
        #[arg(long)]
        axis: u8,
    },
}

#[derive(Subcommand)]
enum BindingCmd {
    /// Create an identity binding: `--target tx|ty|sx|sy|rx|ry|rz|zsort|opacity|tint{r,g,b}|…`.
    Add {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
    },
    /// Set one keypoint of a binding (auto-creates it). `--cell x,y`.
    Key {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
        #[arg(long)]
        cell: String,
        #[arg(long, allow_hyphen_values = true)]
        value: f32,
    },
    /// Un-author a keypoint (back to derived). `--target` also accepts deform.
    Unset {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
        #[arg(long)]
        cell: String,
    },
    /// Author the identity value at a keypoint.
    Reset {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
        #[arg(long)]
        cell: String,
    },
    /// Delete a whole binding.
    Delete {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
    },
    /// Set interpolation: nearest | stepped | linear | cubic.
    Interpolate {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
        #[arg(long)]
        mode: String,
    },
    /// Negate every authored value.
    Invert {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
        #[arg(long)]
        target: String,
    },
    /// Author the value evaluated at `--from` into `--to`.
    CopyKey {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
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
    /// Add a physics node under `--parent`. `--model rigid|spring`.
    Add {
        #[arg(long)]
        parent: u64,
        #[arg(long)]
        model: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long = "target-param")]
        target_param: Option<u64>,
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
    },
    /// Change fields on a physics node. `--model Pendulum|SpringPendulum`,
    /// `--map-mode XY|YX|AngleLength|LengthAngle`.
    Set {
        node: u64,
        #[arg(long)]
        model: Option<String>,
        #[arg(long = "map-mode")]
        map_mode: Option<String>,
        #[arg(long = "local-only")]
        local_only: Option<bool>,
        #[arg(long = "target-param")]
        target_param: Option<u64>,
        #[arg(long = "clear-target-param")]
        clear_target_param: bool,
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
    /// Puppet-level physics constants.
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
        node: u64,
        #[arg(long)]
        source: u64,
        #[arg(long, default_value = "mask")]
        mode: String,
    },
    /// Change the mode of the mask at `--index`.
    Set {
        node: u64,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        mode: String,
    },
    /// Move the mask at `--index` to `--to`.
    Reorder {
        node: u64,
        #[arg(long)]
        index: u32,
        #[arg(long)]
        to: u32,
    },
    /// Remove the mask at `--index`.
    Delete {
        node: u64,
        #[arg(long)]
        index: u32,
    },
}

#[derive(Subcommand)]
enum PresenceCmd {
    /// Publish pose / selection. `--param name=x[,y]` (repeatable).
    Set {
        #[arg(long = "param", allow_hyphen_values = true)]
        params: Vec<String>,
        #[arg(long)]
        select: Option<u64>,
    },
    /// Read the current shared presence.
    Get,
}

#[derive(Subcommand)]
enum DeformCmd {
    /// Set a deform keypoint at `--cell x,y` from an affine.
    Set {
        #[arg(long)]
        param: u64,
        #[arg(long)]
        node: u64,
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
        Cmd::Param { action } => {
            let session = resolve_session(cli)?;
            match action {
                ParamCmd::Add {
                    name,
                    vec2,
                    min,
                    max,
                    defaults,
                    axis_x,
                    axis_y,
                } => Command::ParamAdd {
                    session,
                    name: name.clone(),
                    vec2: *vec2,
                    min: min
                        .as_deref()
                        .map(parse_vec2)
                        .transpose()?
                        .unwrap_or([0.0, 0.0]),
                    max: max
                        .as_deref()
                        .map(parse_vec2)
                        .transpose()?
                        .unwrap_or([1.0, 1.0]),
                    defaults: defaults
                        .as_deref()
                        .map(parse_vec2)
                        .transpose()?
                        .unwrap_or([0.0, 0.0]),
                    axis_x: axis_x
                        .as_deref()
                        .map(parse_f32_vec)
                        .transpose()?
                        .unwrap_or_default(),
                    axis_y: axis_y
                        .as_deref()
                        .map(parse_f32_vec)
                        .transpose()?
                        .unwrap_or_default(),
                },
                ParamCmd::List => Command::ParamList { session },
                ParamCmd::Set {
                    param,
                    name,
                    min,
                    max,
                    defaults,
                } => Command::ParamSet {
                    session,
                    param: ParamRef(*param),
                    name: name.clone(),
                    min: min.as_deref().map(parse_vec2).transpose()?,
                    max: max.as_deref().map(parse_vec2).transpose()?,
                    defaults: defaults.as_deref().map(parse_vec2).transpose()?,
                },
                ParamCmd::Delete { param } => Command::ParamDelete {
                    session,
                    param: ParamRef(*param),
                },
                ParamCmd::AxisInsert { param, axis, value } => Command::ParamAxisInsert {
                    session,
                    param: ParamRef(*param),
                    axis: *axis,
                    value: *value,
                },
                ParamCmd::AxisDelete { param, axis, index } => Command::ParamAxisDelete {
                    session,
                    param: ParamRef(*param),
                    axis: *axis,
                    index: *index,
                },
                ParamCmd::AxisMove {
                    param,
                    axis,
                    index,
                    value,
                } => Command::ParamAxisMove {
                    session,
                    param: ParamRef(*param),
                    axis: *axis,
                    index: *index,
                    value: *value,
                },
                ParamCmd::Flip { param, axis } => Command::ParamFlip {
                    session,
                    param: ParamRef(*param),
                    axis: *axis,
                },
            }
        }
        Cmd::Binding { action } => {
            let session = resolve_session(cli)?;
            match action {
                BindingCmd::Add {
                    param,
                    node,
                    target,
                } => Command::BindingAdd {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                },
                BindingCmd::Key {
                    param,
                    node,
                    target,
                    cell,
                    value,
                } => Command::BindingKey {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                    cell: parse_cell(cell)?,
                    value: *value,
                },
                BindingCmd::Unset {
                    param,
                    node,
                    target,
                    cell,
                } => Command::BindingUnset {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                    cell: parse_cell(cell)?,
                },
                BindingCmd::Reset {
                    param,
                    node,
                    target,
                    cell,
                } => Command::BindingReset {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                    cell: parse_cell(cell)?,
                },
                BindingCmd::Delete {
                    param,
                    node,
                    target,
                } => Command::BindingDelete {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                },
                BindingCmd::Interpolate {
                    param,
                    node,
                    target,
                    mode,
                } => Command::BindingInterpolate {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                    mode: mode.clone(),
                },
                BindingCmd::Invert {
                    param,
                    node,
                    target,
                } => Command::BindingInvert {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
                    target: target.clone(),
                },
                BindingCmd::CopyKey {
                    param,
                    node,
                    target,
                    from,
                    to,
                } => Command::BindingCopyKey {
                    session,
                    param: ParamRef(*param),
                    node: NodeRef(*node),
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
                    model,
                    name,
                    target_param,
                    length,
                    gravity,
                    frequency,
                    angle_damping,
                    length_damping,
                } => Command::PhysicsAdd {
                    session,
                    parent: NodeRef(*parent),
                    name: name.clone(),
                    model: model.clone(),
                    target_param: target_param.map(ParamRef),
                    length: *length,
                    gravity: *gravity,
                    frequency: *frequency,
                    angle_damping: *angle_damping,
                    length_damping: *length_damping,
                },
                PhysicsCmd::Set {
                    node,
                    model,
                    map_mode,
                    local_only,
                    target_param,
                    clear_target_param,
                    gravity,
                    length,
                    frequency,
                    angle_damping,
                    length_damping,
                    output_scale,
                } => Command::PhysicsSet {
                    session,
                    node: NodeRef(*node),
                    model: model.clone(),
                    map_mode: map_mode.clone(),
                    local_only: *local_only,
                    target_param: target_param.map(ParamRef),
                    clear_target_param: *clear_target_param,
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
                MeshCmd::Apply { node, file } => {
                    #[derive(serde::Deserialize)]
                    struct MeshJson {
                        verts: Vec<f32>,
                        uvs: Vec<f32>,
                        indices: Vec<u32>,
                        #[serde(default)]
                        origin: [f32; 2],
                    }
                    let m: MeshJson = serde_json::from_str(&std::fs::read_to_string(file)?)?;
                    Command::MeshApply {
                        session,
                        node: NodeRef(*node),
                        verts: m.verts,
                        uvs: m.uvs,
                        indices: m.indices,
                        origin: m.origin,
                    }
                }
                MeshCmd::Copy { node, from } => Command::MeshCopy {
                    session,
                    from: NodeRef(*from),
                    to: NodeRef(*node),
                },
            }
        }
        Cmd::Mask { action } => {
            let session = resolve_session(cli)?;
            match action {
                MaskCmd::Add { node, source, mode } => Command::MaskAdd {
                    session,
                    node: NodeRef(*node),
                    source: NodeRef(*source),
                    mode: mode.clone(),
                },
                MaskCmd::Set { node, index, mode } => Command::MaskSet {
                    session,
                    node: NodeRef(*node),
                    index: *index,
                    mode: mode.clone(),
                },
                MaskCmd::Reorder { node, index, to } => Command::MaskReorder {
                    session,
                    node: NodeRef(*node),
                    index: *index,
                    to: *to,
                },
                MaskCmd::Delete { node, index } => Command::MaskDelete {
                    session,
                    node: NodeRef(*node),
                    index: *index,
                },
            }
        }
        Cmd::Deform { action } => {
            let session = resolve_session(cli)?;
            let DeformCmd::Set {
                param,
                node,
                cell,
                translate,
                rotate,
                scale,
            } = action;
            Command::DeformSet {
                session,
                param: ParamRef(*param),
                node: NodeRef(*node),
                cell: parse_cell(cell)?,
                translate: translate.as_deref().map(parse_vec2).transpose()?,
                rotate: *rotate,
                scale: scale.as_deref().map(parse_vec2).transpose()?,
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
                        selection: select.map(NodeRef),
                    },
                },
                PresenceCmd::Get => Command::PresenceGet { session },
            }
        }
        Cmd::Undo => Command::Undo {
            session: resolve_session(cli)?,
        },
        Cmd::Redo => Command::Redo {
            session: resolve_session(cli)?,
        },
        Cmd::Texture { action } => match action {
            TextureCmd::Add { path } => Command::TextureAdd {
                session: resolve_session(cli)?,
                path: path.clone(),
            },
            TextureCmd::List => Command::TextureList {
                session: resolve_session(cli)?,
            },
        },
        Cmd::Preview { params, size, out } => Command::Preview {
            session: resolve_session(cli)?,
            params: params
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
        NodeCmd::Add { parent, kind, name } => Command::NodeAdd {
            session,
            parent: NodeRef(*parent),
            kind: parse_kind(kind)?,
            name: name.clone(),
        },
        NodeCmd::Set {
            node,
            name,
            translate,
            rotate,
            scale,
            zsort,
            opacity,
            enabled,
            texture,
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
            node: NodeRef(*node),
            patch: NodePatch {
                name: name.clone(),
                translate: translate.as_deref().map(parse_vec3).transpose()?,
                rotate: rotate.as_deref().map(parse_vec3).transpose()?,
                scale: scale.as_deref().map(parse_vec2).transpose()?,
                zsort: *zsort,
                opacity: *opacity,
                enabled: *enabled,
                texture: texture.map(TexRef),
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
            node: NodeRef(*node),
            to: NodeRef(*to),
        },
        NodeCmd::Reorder { node, index } => Command::NodeReorder {
            session,
            node: NodeRef(*node),
            index: *index,
        },
        NodeCmd::Move {
            node,
            parent,
            index,
        } => Command::NodeMove {
            session,
            node: NodeRef(*node),
            parent: NodeRef(*parent),
            index: *index,
        },
        NodeCmd::Duplicate { node } => Command::NodeDuplicate {
            session,
            node: NodeRef(*node),
        },
        NodeCmd::Delete { node } => Command::NodeDelete {
            session,
            node: NodeRef(*node),
        },
    })
}

fn parse_kind(s: &str) -> Result<NodeKindArg> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "empty" => NodeKindArg::Empty,
        "part" => NodeKindArg::Part,
        "composite" => NodeKindArg::Composite,
        "meshgroup" | "mesh_group" => NodeKindArg::MeshGroup,
        other => bail!("unknown node kind {other:?} (empty|part|composite|meshgroup)"),
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

fn parse_param(s: &str) -> Result<ParamValue> {
    let (name, rest) = s
        .split_once('=')
        .ok_or_else(|| anyhow!("param must look like name=x or name=x,y, got {s:?}"))?;
    let nums: Vec<f32> = rest
        .split(',')
        .map(|p| p.trim().parse::<f32>())
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow!("bad param value {rest:?}: {e}"))?;
    Ok(ParamValue {
        name: name.to_string(),
        x: nums.first().copied().unwrap_or(0.0),
        y: nums.get(1).copied().unwrap_or(0.0),
    })
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
        Reply::Err { message, .. } => eprintln!("error: {message}"),
        Reply::Event(_) => {}
        Reply::Ok { body, .. } => print_body(body),
    }
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
        ResponseBody::Node { node } => println!("node {}", node.0),
        ResponseBody::Param { param } => println!("param {}", param.0),
        ResponseBody::Params { params } => {
            if params.is_empty() {
                println!("(no params)");
            }
            for p in params {
                println!(
                    "param {}  {}  {}  grid={}x{} bindings={}",
                    p.param.0,
                    p.name,
                    if p.vec2 { "2d" } else { "1d" },
                    p.axis[0],
                    p.axis[1],
                    p.bindings
                );
            }
        }
        ResponseBody::Texture { texture } => println!("texture {}", texture.0),
        ResponseBody::Textures { textures } => {
            for t in textures {
                println!("texture {}  {}x{}", t.texture.0, t.width, t.height);
            }
        }
        ResponseBody::Tree { root } => print_tree(root, 0),
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
        ResponseBody::Presence { presence } => match presence {
            None => println!("(no presence)"),
            Some(p) => {
                let pose: Vec<String> = p
                    .pose
                    .iter()
                    .map(|v| format!("{}={}", v.name, v.x))
                    .collect();
                println!(
                    "pose=[{}] selection={}",
                    pose.join(","),
                    p.selection
                        .map(|r| r.0.to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
        },
    }
}

fn print_tree(node: &TreeNode, depth: usize) {
    println!(
        "{}{} [{}] {} (z={})",
        "  ".repeat(depth),
        node.node.0,
        node.kind,
        node.name,
        node.zsort
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
