//! The reads a replica can answer: [`CommandKind::ReplicaQuery`].
//!
//! A browser tab holds a replica of a session's [`Model`] and never mutates
//! it. Everything in here is a pure function of that model, so the tab answers
//! these without a round trip and the editor answers them the same way, from
//! the same bytes. That is only true while there is *one* implementation:
//! `Editor::dispatch` routes its seven `ReplicaQuery` arms straight into
//! [`replica_query`], so a fix to an answer reaches both ends or neither.
//!
//! Invariants this module carries:
//!
//! - **The kinds table decides what belongs here.** [`replica_query`] answers
//!   exactly the tags [`COMMAND_KINDS`] marks [`CommandKind::ReplicaQuery`],
//!   and anything else is [`ErrorCode::BadRequest`] naming the tag rather than
//!   a panic — a replica is fed by an untrusted page. The two sets are held
//!   equal by a test, so reclassifying a command in the protocol crate breaks
//!   the build here rather than quietly making it unanswerable.
//!
//! - **A revision comes from the caller.** A model does not know which
//!   revision it is; the replica that holds it does. So [`replica_reply`]
//!   takes the `rev` it stamps on the envelope rather than inventing one.

use catchlight_core::{Model, ModelError, ModelWeld};
use catchlight_editor_protocol::*;

use crate::{image_dims, EditorError};

/// Answer one [`CommandKind::ReplicaQuery`] against `model`.
///
/// Any other command is [`ErrorCode::BadRequest`] naming the tag that was
/// sent, never a panic.
pub fn replica_query(model: &Model, command: &Command) -> Result<ResponseBody, EditorError> {
    match command {
        Command::Check { .. } => Ok(ResponseBody::Warnings {
            warnings: model.check().into_iter().map(|w| w.message).collect(),
        }),
        Command::NodeTree { .. } => {
            // A session holds a complete model, so this is unreachable — but
            // `Fragment` says so on the wire rather than panicking.
            let root = model.root().ok_or(ModelError::Fragment)?;
            Ok(ResponseBody::Tree {
                root: build_tree(model, root),
            })
        }
        Command::TextureList { .. } => {
            let mut textures = Vec::new();
            for tid in model.texture_ids() {
                if let Some(t) = model.texture(tid) {
                    let (width, height) = image_dims(&t.data).unwrap_or((0, 0));
                    textures.push(TexInfo {
                        id: tid.clone(),
                        width,
                        height,
                    });
                }
            }
            Ok(ResponseBody::Textures { textures })
        }
        Command::ParamList { .. } => Ok(ResponseBody::Params {
            params: param_infos(model),
        }),
        Command::Seams { node, .. } => {
            let seams = model.seams(node).ok_or_else(|| match model.node(node) {
                Some(_) => EditorError::Edit(ModelError::NotAPart),
                None => EditorError::NoNode(node.clone()),
            })?;
            Ok(ResponseBody::Seams {
                seams: seams.iter().map(seam_info).collect(),
            })
        }
        Command::Welds { .. } => Ok(ResponseBody::Welds {
            welds: model.welds().iter().map(weld_info).collect(),
        }),
        Command::UnfilledSlots { .. } => Ok(ResponseBody::UnfilledSlots {
            slots: model
                .unfilled_slots()
                .into_iter()
                .map(|(node, seam, slot)| SlotAddr { node, seam, slot })
                .collect(),
        }),
        other => Err(EditorError::BadRequest(format!(
            "{} is not a model-only query",
            other.tag()
        ))),
    }
}

/// The whole reply envelope, as `Editor::handle` would build it for the same
/// request against the same model.
///
/// `rev` is the replica's own revision: the model does not carry one, and the
/// client that holds it knows which one it last accepted.
pub fn replica_reply(model: &Model, rev: u64, request: Request) -> Reply {
    match replica_query(model, &request.command) {
        Ok(body) => Reply::Ok {
            id: request.id,
            rev: Some(rev),
            body,
        },
        Err(e) => Reply::Err {
            id: request.id,
            code: e.code(),
            message: e.to_string(),
        },
    }
}

/// The tree under `id`. A missing node reads as an empty group rather than
/// dropping the subtree, so a reply always has the shape a client expects.
pub(crate) fn build_tree(model: &Model, id: &NodeId) -> TreeNode {
    let (name, kind, z_order, enabled, children) = match model.node(id) {
        Some(n) => (
            n.name.to_string(),
            n.kind.name().to_string(),
            n.z_order,
            n.enabled,
            n.children(),
        ),
        None => (String::new(), "group".to_string(), 0.0, true, &[][..]),
    };
    TreeNode {
        id: id.clone(),
        name,
        kind,
        z_order,
        enabled,
        children: children.iter().map(|c| build_tree(model, c)).collect(),
    }
}

pub(crate) fn param_infos(model: &Model) -> Vec<ParamInfo> {
    let mut out = Vec::with_capacity(model.param_ids().len());
    for pid in model.param_ids() {
        let Some(p) = model.param(pid) else { continue };
        out.push(ParamInfo {
            id: pid.clone(),
            name: p.name.to_string(),
            min: p.min,
            max: p.max,
            default: p.default,
            key_positions: p.key_positions.clone(),
            bindings: model.bindings_of_param(pid).count() as u32,
        });
    }
    out
}

fn seam_info(seam: &catchlight_core::Seam) -> SeamInfo {
    SeamInfo {
        id: seam.id().clone(),
        slots: seam
            .slots()
            .iter()
            .map(|slot| SlotInfo {
                id: slot.id().clone(),
                vertex: slot.vertex(),
            })
            .collect(),
    }
}

fn weld_info(weld: &ModelWeld) -> WeldInfo {
    let end = |(node, seam): &(NodeId, SeamId)| SeamAddr {
        node: node.clone(),
        seam: seam.clone(),
    };
    WeldInfo {
        a: end(weld.a()),
        b: end(weld.b()),
        weights: weld
            .weights()
            .iter()
            .map(|(slot, weight)| SlotWeight {
                slot: slot.clone(),
                weight: *weight,
            })
            .collect(),
    }
}
