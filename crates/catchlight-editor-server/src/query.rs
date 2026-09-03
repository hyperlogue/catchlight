//! The reads a replica can answer: [`CommandKind::ReplicaQuery`].
//!
//! A browser tab holds a replica of a session's [`Model`] and never mutates
//! it. Everything in here is a pure function of that model, so the tab answers
//! these without a round trip and the editor answers them the same way, from
//! the same bytes. That is only true while there is *one* implementation:
//! `Editor::dispatch` routes its `ReplicaQuery` arms straight into
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

use catchlight_core::{
    deform_cells, scalar_cells, InterpolateMode, Model, ModelBinding, ModelError, ModelNode,
    ModelNodeKind, ModelWeld,
};
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
        Command::NodeInfo { node, .. } => {
            let n = model
                .node(node)
                .ok_or_else(|| EditorError::NoNode(node.clone()))?;
            Ok(ResponseBody::NodeInfo {
                node: Box::new(node_info(node, n)),
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
        Command::BindingList { node, .. } => {
            // A node that is gone is `no_node` rather than an empty list: a
            // selection outlives the node it names, and a panel showing "no
            // bindings" for a deleted node is a lie the client cannot see
            // through.
            if model.node(node).is_none() {
                return Err(EditorError::NoNode(node.clone()));
            }
            Ok(ResponseBody::Bindings {
                bindings: model
                    .bindings_of_node(node)
                    .map(|b| binding_info(model, b))
                    .collect::<Result<Vec<_>, _>>()?,
            })
        }
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

/// One node as an inspector reads it: every [`NodePatch`] field under its own
/// name, and the four things a patch cannot set — the Id, the kind, the parent
/// and the size of the mesh the node holds.
///
/// A field the node's kind does not carry stays `None` — the same rule
/// `apply_patch` applies on the way in, where a colour set on a mesh group is
/// ignored. So what comes back is exactly what a `node_set` on this node
/// would keep.
fn node_info(id: &NodeId, node: &ModelNode) -> NodeInfo {
    // The colour a drawable carries. A group, mesh group or physics node is
    // never drawn, so it reports none rather than a default a patch would
    // then write back into it.
    let (opacity, blend_mode, tint, screen_tint, mask_threshold) = match &node.kind {
        ModelNodeKind::Part(p) => (
            Some(p.opacity),
            Some(p.blend_mode.as_str().to_string()),
            Some(p.tint),
            Some(p.screen_tint),
            Some(p.mask_threshold),
        ),
        ModelNodeKind::Composite(c) => (
            Some(c.opacity),
            Some(c.blend_mode.as_str().to_string()),
            Some(c.tint),
            Some(c.screen_tint),
            Some(c.mask_threshold),
        ),
        _ => (None, None, None, None, None),
    };
    let (mg_dynamic, mg_translate_children) = match &node.kind {
        ModelNodeKind::MeshGroup(mg) => (Some(mg.dynamic), Some(mg.translate_children)),
        _ => (None, None),
    };
    // Only the two kinds that hold a mesh report its size, so an empty mesh
    // reads as 0 and a kind that could never have one reads as absent.
    let (vertex_count, triangle_count) = match node.mesh() {
        Some(mesh) => (
            Some(mesh.vertex_count() as u32),
            Some(mesh.triangle_count() as u32),
        ),
        None => (None, None),
    };
    NodeInfo {
        id: id.clone(),
        kind: node.kind.name().to_string(),
        parent: node.parent().cloned(),
        name: node.name.to_string(),
        translate: node.transform.translation,
        rotate: node.transform.rotation,
        scale: node.transform.scale,
        z_order: node.z_order,
        enabled: node.enabled,
        lock_to_root: node.lock_to_root,
        opacity,
        blend_mode,
        tint,
        screen_tint,
        mask_threshold,
        texture: match &node.kind {
            ModelNodeKind::Part(p) => p.albedo().cloned(),
            _ => None,
        },
        vertex_count,
        triangle_count,
        propagate_meshgroup: match &node.kind {
            ModelNodeKind::Composite(c) => Some(c.propagate_meshgroup),
            _ => None,
        },
        mg_dynamic,
        mg_translate_children,
    }
}

/// One binding as a panel reads it: the authored grid filled in `[y][x]`,
/// with every cell nobody set left `None`.
///
/// The model stores only the cells a rigger authored and derives the rest at
/// puppet build, so the hole is the answer — spelling an unset cell as the
/// target's identity would hand a client a number to write back that the
/// author never wrote. A deform binding's cells hold a vertex list rather than
/// a scalar, so they say only that they are authored.
fn binding_info(model: &Model, binding: &ModelBinding) -> Result<BindingInfo, EditorError> {
    let key = binding.key();
    let (width, height) = model.binding_grid(key)?;
    let (w, h) = (width as usize, height as usize);
    let mut keys = vec![vec![None; w]; h];
    let mut authored = vec![vec![false; w]; h];
    // A cell outside the grid is one the key positions shrank away from; it
    // cannot be addressed and it cannot be drawn, so it is not reported.
    let mut set = |x: u32, y: u32, value: Option<f32>| {
        let (x, y) = (x as usize, y as usize);
        if x < w && y < h {
            keys[y][x] = value;
            authored[y][x] = true;
        }
    };
    match scalar_cells(binding.values()) {
        Some(cells) => {
            for c in cells {
                set(c.x, c.y, Some(c.value));
            }
        }
        None => {
            for c in deform_cells(binding.values()).unwrap_or(&[]) {
                set(c.x, c.y, None);
            }
        }
    }
    Ok(BindingInfo {
        target: key.target.name().to_string(),
        param: key.params.x().clone(),
        param_y: key.params.y().cloned(),
        interpolate: interpolate_name(binding.interpolate_mode()).to_string(),
        width,
        height,
        keys,
        authored,
    })
}

/// The wire name of an interpolation mode: the inverse of the server's own
/// `parse_interpolate_mode`, so a mode read here is a mode
/// [`Command::BindingInterpolate`] takes back.
fn interpolate_name(mode: InterpolateMode) -> &'static str {
    match mode {
        InterpolateMode::Nearest => "nearest",
        InterpolateMode::Stepped => "stepped",
        InterpolateMode::Linear => "linear",
        InterpolateMode::Cubic => "cubic",
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

/// The wire spelling of one seam. Public because the egui editor's seam panel
/// reads seams straight off the model rather than over the protocol, and there
/// is one spelling of a seam or there are two.
pub fn seam_info(seam: &catchlight_core::Seam) -> SeamInfo {
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
