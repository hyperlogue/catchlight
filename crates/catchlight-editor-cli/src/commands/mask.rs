//! `mask`: the mask sources of a part or composite.

use anyhow::{Result, anyhow, bail};
use catchlight_editor_protocol::{
    Command, EditOp, MaskAddArgs, MaskDeleteArgs, MaskInfo, MaskMode, NodeId, ResponseBody,
};
use clap::Subcommand;

use crate::args::wire_enum;
use crate::compose::CapturedClient;
use crate::route::Route;

#[derive(Subcommand)]
pub(crate) enum MaskCmd {
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

pub(crate) fn route(action: &MaskCmd) -> Route<'_> {
    match action {
        MaskCmd::Add { node, source, mode } => Route::on_session(move |session| Command::MaskAdd {
            session,
            edit: MaskAddArgs {
                node: node.clone(),
                source: source.clone(),
                mode: *mode,
            },
        }),
        MaskCmd::Set { node, index, mode } => Route::composed(move |remote| {
            rearrange(remote, node, |masks| {
                let mask = masks
                    .get_mut(*index as usize)
                    .ok_or_else(|| anyhow!("mask index out of range"))?;
                mask.mode = *mode;
                Ok(())
            })
        }),
        MaskCmd::Reorder { node, index, to } => Route::composed(move |remote| {
            rearrange(remote, node, |masks| {
                if *index as usize >= masks.len() {
                    bail!("mask index out of range");
                }
                let mask = masks.remove(*index as usize);
                masks.insert((*to as usize).min(masks.len()), mask);
                Ok(())
            })
        }),
        MaskCmd::Delete { node, index } => Route::on_session(move |session| Command::MaskDelete {
            session,
            edit: MaskDeleteArgs {
                node: node.clone(),
                index: *index,
            },
        }),
    }
}

/// Replaces `node`'s masks with what `change` makes of them, in one batch.
fn rearrange(
    remote: &mut CapturedClient<'_>,
    node: &NodeId,
    change: impl FnOnce(&mut Vec<MaskInfo>) -> Result<()>,
) -> Result<Command> {
    let ResponseBody::NodeInfo { node: info } = remote.read(Command::NodeInfo {
        session: remote.session,
        node: node.clone(),
    })?
    else {
        bail!("node_get returned an unexpected body");
    };
    let mut masks = info.masks;
    let count = masks.len();
    change(&mut masks)?;
    remote.batch(replace_masks(node, count, masks))
}

fn replace_masks(node: &NodeId, count: usize, masks: Vec<MaskInfo>) -> Vec<EditOp> {
    (0..count)
        .rev()
        .map(|index| EditOp::MaskDelete {
            edit: MaskDeleteArgs {
                node: node.clone(),
                index: index as u32,
            },
        })
        .chain(masks.into_iter().map(|mask| EditOp::MaskAdd {
            edit: MaskAddArgs {
                node: node.clone(),
                source: mask.source,
                mode: mask.mode,
            },
        }))
        .collect()
}
