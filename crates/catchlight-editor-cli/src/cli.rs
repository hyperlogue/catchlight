//! The command line: global flags, the top-level commands and their help.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use catchlight_editor_protocol::{ParamPose, SessionId};
use clap::{Parser, Subcommand};

use crate::args::{parse_param, parse_size, parse_vec2};
use crate::commands::{
    binding::BindingCmd, deform::DeformCmd, mask::MaskCmd, mesh::MeshCmd, node::NodeCmd,
    param::ParamCmd, presence::PresenceCmd, rename::RenameCmd, session::SessionCmd, slot::SlotCmd,
    texture::TextureCmd, weld::WeldCmd,
};
use crate::{extension, official, review, version};

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
pub struct Cli {
    /// Print the raw reply JSON instead of a friendly summary.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// Target session id (defaults to the remembered current session).
    #[arg(long, global = true)]
    pub(crate) session: Option<u64>,
    /// Expected revision for guarded edits and composed read/write operations.
    #[arg(long, global = true)]
    pub(crate) if_rev: Option<u64>,
    /// The editor's HTTP door, e.g. <http://127.0.0.1:7323>.
    #[arg(long, global = true, env = "CATCHLIGHT_EDITOR_URL")]
    pub(crate) server: Option<String>,
    /// Bearer token for --server; read from the connection file when omitted.
    #[arg(
        long,
        global = true,
        env = "CATCHLIGHT_EDITOR_TOKEN",
        hide_env_values = true
    )]
    pub(crate) token: Option<String>,
    /// A Unix socket to use instead of the default one.
    #[arg(long, global = true)]
    pub(crate) socket: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Send any typed protocol command from JSON; use `-` for stdin.
    Request {
        file: String,
        /// Attach local bytes as NAME=PATH (repeatable).
        #[arg(long = "attachment")]
        attachments: Vec<String>,
        /// File for a binary reply, such as `model_export` or `preview_render`.
        #[arg(long)]
        out: Option<String>,
    },
    /// Apply an atomic JSON array of `EditOp` values, or validate it only.
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
        #[arg(long = "param", value_parser = parse_param)]
        params: Vec<ParamPose>,
        /// Output size, e.g. `512x512`.
        #[arg(long, value_parser = parse_size)]
        size: Option<[u32; 2]>,
        /// Orthographic height in world units (defaults to the editor's).
        #[arg(long = "camera-height")]
        camera_height: Option<f32>,
        /// Camera centre, e.g. `0,120`. Needs `--camera-height`.
        #[arg(long = "camera-center", value_parser = parse_vec2)]
        camera_center: Option<[f32; 2]>,
    },
}

/// `--session`, else the remembered session of the editor this reached.
pub fn resolve_session(cli: &Cli) -> Result<SessionId> {
    cli.session.map(SessionId).ok_or_else(|| {
        anyhow!(
            "no session selected; pass --session <id> or run `session use <id>` / `session new`"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

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
}
