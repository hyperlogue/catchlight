//! `weld`: parts welded slot by slot.

use anyhow::{Result, anyhow, bail};
use catchlight_editor_protocol::{
    Command, EditOp, NodeId, ResponseBody, SlotId, SlotPair, WeldDeleteArgs, WeldInfo, WeldSetArgs,
};
use clap::Subcommand;

use crate::args::parse_pair;
use crate::route::Route;

#[derive(Subcommand)]
pub(crate) enum WeldCmd {
    /// Weld two parts, replacing any existing weld. Each repeatable `--pair` is
    /// `<slot on --a>:<slot on --b>:<weight>`, the weight being `--a`'s share
    /// in 0..=1.
    Set {
        #[arg(long)]
        a: NodeId,
        #[arg(long)]
        b: NodeId,
        #[arg(long = "pair", value_parser = parse_pair)]
        pairs: Vec<SlotPair>,
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

pub(crate) fn route(action: &WeldCmd) -> Route<'_> {
    match action {
        WeldCmd::Set { a, b, pairs } => Route::on_session(move |session| Command::WeldSet {
            session,
            edit: WeldSetArgs {
                a: a.clone(),
                b: b.clone(),
                pairs: pairs.clone(),
            },
        }),
        WeldCmd::Weight { a, b, slot, weight } => Route::composed(move |remote| {
            let ResponseBody::Welds { welds } = remote.read(Command::Welds {
                session: remote.session,
            })?
            else {
                bail!("weld_list returned an unexpected body");
            };
            let weld = welds
                .into_iter()
                .find(|w| (&w.a == a && &w.b == b) || (&w.a == b && &w.b == a))
                .ok_or_else(|| anyhow!("no weld joins {a} and {b}"))?;
            let pairs = reweight(weld, a, slot, *weight)?;
            remote.batch(vec![EditOp::WeldSet {
                edit: WeldSetArgs {
                    a: pairs.a,
                    b: pairs.b,
                    pairs: pairs.pairs,
                },
            }])
        }),
        WeldCmd::Delete { a, b } => Route::on_session(move |session| Command::WeldDelete {
            session,
            edit: WeldDeleteArgs {
                a: a.clone(),
                b: b.clone(),
            },
        }),
        WeldCmd::List => Route::on_session(|session| Command::Welds { session }),
    }
}

fn reweight(mut weld: WeldInfo, side: &NodeId, slot: &SlotId, weight: f32) -> Result<WeldInfo> {
    let forward = &weld.a == side;
    let pair = weld
        .pairs
        .iter_mut()
        .find(|pair| {
            if forward {
                &pair.a == slot
            } else {
                &pair.b == slot
            }
        })
        .ok_or_else(|| anyhow!("slot is not paired by this weld"))?;
    pair.weight = if forward { weight } else { 1.0 - weight };
    Ok(weld)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> NodeId {
        NodeId::new(value).unwrap()
    }

    #[test]
    fn reverse_weld_weight_keeps_storage_orientation_and_unrelated_pairs() {
        let weld = WeldInfo {
            a: id("a"),
            b: id("b"),
            pairs: vec![
                SlotPair {
                    a: SlotId::new("edge").unwrap(),
                    b: SlotId::new("join").unwrap(),
                    weight: 0.4,
                },
                SlotPair {
                    a: SlotId::new("tip").unwrap(),
                    b: SlotId::new("end").unwrap(),
                    weight: 0.6,
                },
            ],
        };
        let changed = reweight(weld, &id("b"), &SlotId::new("join").unwrap(), 0.25).unwrap();
        assert_eq!(changed.a, id("a"));
        assert_eq!(changed.pairs[0].weight, 0.75);
        assert_eq!(changed.pairs[1].weight, 0.6);
    }
}
