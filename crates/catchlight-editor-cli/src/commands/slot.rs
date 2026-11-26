//! `slot`: vertex slots on a part.

use anyhow::{Result, bail};
use catchlight_editor_protocol::{
    Command, NodeId, NodeKind, Rename, ResponseBody, SlotAddr, SlotId, TreeNode,
};
use clap::Subcommand;

use crate::cli::Cli;
use crate::compose::CapturedClient;
use crate::route::Route;
use crate::transport::Transport;

#[derive(Subcommand)]
pub(crate) enum SlotCmd {
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

pub(crate) fn route(action: &SlotCmd) -> Route<'_> {
    match action {
        SlotCmd::Add { node, slot } => Route::on_session(move |session| Command::SlotAdd {
            session,
            node: node.clone(),
            slot: slot.clone(),
        }),
        SlotCmd::Fill { node, slot, vertex } => {
            Route::on_session(move |session| Command::SlotFill {
                session,
                node: node.clone(),
                slot: slot.clone(),
                vertex: *vertex,
            })
        }
        SlotCmd::Clear { node, slot } => Route::on_session(move |session| Command::SlotClear {
            session,
            node: node.clone(),
            slot: slot.clone(),
        }),
        SlotCmd::Delete { node, slot } => Route::on_session(move |session| Command::SlotDelete {
            session,
            node: node.clone(),
            slot: slot.clone(),
        }),
        SlotCmd::Rename { node, slot, to } => Route::on_session(move |session| Command::RenameId {
            session,
            rename: Rename::Slot {
                node: node.clone(),
                from: slot.clone(),
                to: to.clone(),
            },
        }),
        SlotCmd::List { node } => Route::on_session(move |session| Command::Slots {
            session,
            node: node.clone(),
        }),
        SlotCmd::Unfilled => Route::typed(|cli, stream, _| unfilled(cli, stream)),
    }
}

/// Reads every part's slots at one revision.
fn unfilled(cli: &Cli, stream: &mut Transport) -> Result<i32> {
    let mut remote = CapturedClient::new(cli, stream)?;
    let session = remote.session;
    let ResponseBody::Tree { root } = remote.read(Command::NodeTree { session })? else {
        bail!("node_tree_get returned an unexpected body");
    };
    let mut parts = Vec::new();
    part_ids(&root, &mut parts);
    let mut slots = Vec::new();
    for node in parts {
        let ResponseBody::Slots { slots: held } = remote.read(Command::Slots {
            session,
            node: node.clone(),
        })?
        else {
            bail!("slot_list returned an unexpected body");
        };
        slots.extend(
            held.into_iter()
                .filter(|slot| slot.vertex.is_none())
                .map(|slot| SlotAddr {
                    node: node.clone(),
                    slot: slot.id,
                }),
        );
    }
    let revision = remote.guard()?;
    if cli.json {
        println!("{}", serde_json::json!({"rev": revision, "slots": slots}));
    } else if slots.is_empty() {
        println!("every vertex slot is filled");
    } else {
        for slot in slots {
            println!("unfilled {}:{}", slot.node, slot.slot);
        }
    }
    Ok(0)
}

fn part_ids(root: &TreeNode, parts: &mut Vec<NodeId>) {
    if root.kind == NodeKind::Part {
        parts.push(root.id.clone());
    }
    for child in &root.children {
        part_ids(child, parts);
    }
}
