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

mod geometry;

use catchlight_core::formats::clm::extension_hash;
use catchlight_core::{
    deform_cells, scalar_cells, ExtensionValue, Model, ModelBinding, ModelError, ModelNode,
    ModelNodeKind, ModelWeld,
};
use catchlight_editor_protocol::*;

use crate::{image_dims, EditorError};

/// One extension's value as a reply reports it: JSON whole, bytes as the size
/// and hash their marker carries.
///
/// The one conversion, so a listing and a get never describe the same value
/// two ways — and so what a client compares to decide whether to fetch is the
/// hash the structure feed will carry.
pub fn extension_value_info(value: &ExtensionValue) -> ExtensionValueInfo {
    match value {
        ExtensionValue::Json(json) => ExtensionValueInfo::Json {
            value: json.clone(),
        },
        ExtensionValue::Bytes(data) => ExtensionValueInfo::Bytes {
            size: data.len() as u32,
            hash: extension_hash(data),
        },
    }
}

/// Answer one [`CommandKind::ReplicaQuery`] against `model`.
///
/// Any other command is [`ErrorCode::BadRequest`] naming the tag that was
/// sent, never a panic.
pub fn replica_query(model: &Model, command: &Command) -> Result<ResponseBody, EditorError> {
    let body = match command {
        Command::MeshGet { node, .. } => {
            let mesh = model.node_mesh(node).ok_or_else(|| {
                if model.node(node).is_some() {
                    EditorError::BadTarget("node has no mesh".into())
                } else {
                    EditorError::NoNode(node.clone())
                }
            })?;
            check_count("mesh_vertices", mesh.vertex_count(), MAX_QUERY_ITEMS)?;
            check_count("mesh_triangles", mesh.triangle_count(), MAX_QUERY_ITEMS)?;
            Ok(ResponseBody::MeshInfo {
                node: node.clone(),
                mesh: mesh_info(mesh),
            })
        }
        Command::ModelGet { .. } => {
            let structure = model.to_clm_structure()?;
            crate::limits::json_size(&structure, "reply_bytes", MAX_QUERY_BYTES)?;
            let structure = serde_json::to_value(structure)
                .map_err(|e| EditorError::BadTarget(e.to_string()))?;
            let textures = model
                .texture_ids()
                .iter()
                .filter_map(|id| {
                    model.texture(id).map(|t| ModelTextureHeader {
                        id: id.clone(),
                        encoding: t.encoding.into(),
                        alpha: t.alpha.into(),
                    })
                })
                .collect();
            Ok(ResponseBody::ModelStructure {
                structure,
                textures,
            })
        }
        Command::GeometryGet { .. } => geometry::sample(model, command),
        Command::BindingCellsGet { .. } => binding_cells(model, command),
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
        Command::Extensions { .. } => Ok(ResponseBody::Extensions {
            extensions: model
                .extensions()
                .iter()
                .map(|(key, value)| ExtensionInfo {
                    key: key.clone(),
                    value: extension_value_info(value),
                })
                .collect(),
        }),
        Command::TextureList { .. } => {
            let mut textures = Vec::new();
            for tid in model.texture_ids() {
                if let Some(t) = model.texture(tid) {
                    let (width, height) = image_dims(&t.data, t.encoding).unwrap_or((0, 0));
                    textures.push(TexInfo {
                        id: tid.clone(),
                        encoding: t.encoding.into(),
                        alpha: t.alpha.into(),
                        sha256: super::lifecycle::sha256(&t.data),
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
        Command::Slots { node, .. } => {
            let slots = model.slots(node).ok_or_else(|| match model.node(node) {
                Some(_) => EditorError::Edit(ModelError::NotAPart),
                None => EditorError::NoNode(node.clone()),
            })?;
            Ok(ResponseBody::Slots {
                slots: slots.iter().map(slot_info).collect(),
            })
        }
        Command::Welds { .. } => Ok(ResponseBody::Welds {
            welds: model.welds().iter().map(weld_info).collect(),
        }),
        other => Err(EditorError::BadRequest(format!(
            "{} is not a model-only query",
            other.tag()
        ))),
    }?;
    check_reply_size(&body)?;
    Ok(body)
}

/// The whole reply envelope, as `Editor::handle` would build it for the same
/// request against the same model.
///
/// `rev` is the replica's own revision: the model does not carry one, and the
/// client that holds it knows which one it last accepted.
pub fn replica_reply(model: &Model, rev: u64, request: Request) -> Reply {
    let answer = crate::limits::json_size(
        &request,
        "request_bytes",
        crate::limits::MAX_REQUEST_JSON_BYTES,
    )
    .and_then(|()| check_revision(&request.command, rev))
    .and_then(|()| replica_query(model, &request.command));
    match answer {
        Ok(body) => Reply::Ok {
            id: request.id,
            rev: Some(rev),
            body,
        },
        Err(e) => Reply::Err {
            id: request.id,
            code: e.code(),
            message: e.to_string(),
            op_index: None,
            limit: e.limit_info(),
        },
    }
}

/// Reply payload budget shared by server and synchronous browser replica reads.
pub const MAX_QUERY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_QUERY_ITEMS: usize = 262_144;

pub(crate) fn check_revision(command: &Command, rev: u64) -> Result<(), EditorError> {
    let expected = match command {
        Command::MeshGet { if_rev, .. }
        | Command::ModelGet { if_rev, .. }
        | Command::GeometryGet { if_rev, .. }
        | Command::BindingCellsGet { if_rev, .. } => *if_rev,
        _ => None,
    };
    if expected.is_some_and(|wanted| wanted != rev) {
        Err(EditorError::RevisionConflict)
    } else {
        Ok(())
    }
}

fn check_count(
    resource: &'static str,
    requested: usize,
    maximum: usize,
) -> Result<(), EditorError> {
    if requested > maximum {
        Err(EditorError::Limit {
            resource,
            requested: requested as u64,
            limit: maximum as u64,
        })
    } else {
        Ok(())
    }
}

fn check_reply_size(body: &ResponseBody) -> Result<(), EditorError> {
    crate::limits::json_size(body, "reply_bytes", MAX_QUERY_BYTES)
}

fn page(range: Option<IndexRange>, len: usize) -> Result<IndexRange, EditorError> {
    let range = range.unwrap_or(IndexRange {
        start: 0,
        count: len as u32,
    });
    let end = u64::from(range.start) + u64::from(range.count);
    if end > len as u64 {
        return Err(EditorError::BadRequest(
            "range exceeds authored array".into(),
        ));
    }
    check_count("range_items", range.count as usize, MAX_QUERY_ITEMS)?;
    Ok(range)
}

fn binding_cells(model: &Model, command: &Command) -> Result<ResponseBody, EditorError> {
    let Command::BindingCellsGet {
        node,
        params,
        target,
        cells,
        include_derived,
        vertices,
        ..
    } = command
    else {
        unreachable!()
    };
    let key = crate::binding_key(params.clone(), node.clone(), *target)?;
    let binding = model.binding(&key).ok_or(ModelError::UnknownBinding)?;
    crate::edit::validate_cells(model, &key, cells.iter().copied(), cells.len())?;
    let (width, height) = model.binding_grid(&key)?;
    let vertex_count = if *target == BindingTarget::Deform {
        Some(
            model
                .node_mesh(node)
                .ok_or(ModelError::NotMeshed)?
                .vertex_count() as u32,
        )
    } else {
        None
    };
    if vertex_count.is_none() && vertices.is_some() {
        return Err(EditorError::BadRequest(
            "vertices only applies to deform cells".into(),
        ));
    }
    let range = vertex_count
        .map(|n| page(*vertices, n as usize))
        .transpose()?;
    // Bound the combined page before allocating per-cell arrays.
    if let Some(range) = range {
        check_count(
            "cell_vertices",
            cells.len().saturating_mul(range.count as usize),
            MAX_QUERY_ITEMS,
        )?;
    }
    let scalars: std::collections::HashMap<_, _> = scalar_cells(binding.values())
        .unwrap_or_default()
        .iter()
        .map(|entry| ([entry.x, entry.y], entry.value))
        .collect();
    let deforms: std::collections::HashMap<_, _> = deform_cells(binding.values())
        .unwrap_or_default()
        .iter()
        .map(|entry| ([entry.x, entry.y], entry.value.as_slice()))
        .collect();
    // A derived hole fills the grid over this vertex page. Bound cumulative
    // fill work as well as returned vertices; otherwise one tiny read could
    // materialize a complete mesh at every key. Authored cells and an entirely
    // unset binding need no fill and can be sliced/answered directly.
    if *include_derived && !deforms.is_empty() {
        if let Some(range) = range {
            let holes = cells
                .iter()
                .filter(|cell| !deforms.contains_key(*cell))
                .count();
            let work = (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(range.count as usize)
                .saturating_mul(holes);
            check_count("derived_cell_vertices", work, MAX_QUERY_ITEMS)?;
        }
    }
    let offsets = |flat: &[f32]| -> BindingCellValue {
        let range = range.unwrap_or(IndexRange { start: 0, count: 0 });
        BindingCellValue::Offsets(
            flat.as_chunks::<2>()
                .0
                .iter()
                .skip(range.start as usize)
                .take(range.count as usize)
                .copied()
                .collect(),
        )
    };
    let mut result = Vec::with_capacity(cells.len());
    for &cell in cells {
        let value = scalars
            .get(&cell)
            .copied()
            .map(BindingCellValue::Scalar)
            .or_else(|| deforms.get(&cell).map(|flat| offsets(flat)));
        let derived = if *include_derived {
            Some(if let Some(authored) = &value {
                authored.clone()
            } else if let Some(range) = range {
                let start = range.start as usize;
                let flat =
                    model.deform_value_at_range(&key, cell, start..start + range.count as usize)?;
                BindingCellValue::Offsets(flat.as_chunks::<2>().0.to_vec())
            } else {
                BindingCellValue::Scalar(model.scalar_value_at(&key, cell)?)
            })
        } else {
            None
        };
        if let Some(derived) = &derived {
            let finite = match derived {
                BindingCellValue::Scalar(value) => value.is_finite(),
                BindingCellValue::Offsets(values) => {
                    values.iter().flatten().all(|value| value.is_finite())
                }
            };
            if !finite {
                return Err(EditorError::BadTarget(
                    "nonfinite derived binding value".into(),
                ));
            }
        }
        result.push(BindingCellRead {
            cell,
            authored: value.is_some(),
            value,
            derived,
        });
    }
    Ok(ResponseBody::BindingCells {
        node: node.clone(),
        params: params.clone(),
        target: *target,
        width,
        height,
        interpolate: binding.interpolate_mode().into(),
        vertex_count,
        vertices: range,
        cells: result,
    })
}

/// The tree under `id`. A missing node reads as an empty group rather than
/// dropping the subtree, so a reply always has the shape a client expects.
pub(crate) fn build_tree(model: &Model, id: &NodeId) -> TreeNode {
    let (name, kind, z_order, enabled, children) = match model.node(id) {
        Some(n) => (
            n.name.to_string(),
            NodeKind::of(&n.kind),
            n.z_order,
            n.enabled,
            n.children(),
        ),
        None => (String::new(), NodeKind::Group, 0.0, true, &[][..]),
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
///
/// A pendulum's and a spine's own settings are nested rather than flattened,
/// in [`NodeInfo::physics`] and [`NodeInfo::spine`], because no `node_set`
/// writes them: the round-trip they answer is to `physics_set` and `spine_set`, whose
/// field names they carry.
fn node_info(id: &NodeId, node: &ModelNode) -> NodeInfo {
    // The colour a drawable carries. A group, mesh group or physics node is
    // never drawn, so it reports none rather than a default a patch would
    // then write back into it.
    let (opacity, blend_mode, tint, screen_tint, mask_threshold) = match &node.kind {
        ModelNodeKind::Part(p) => (
            Some(p.opacity),
            Some(p.blend_mode.into()),
            Some(p.tint),
            Some(p.screen_tint),
            Some(p.mask_threshold),
        ),
        ModelNodeKind::Composite(c) => (
            Some(c.opacity),
            Some(c.blend_mode.into()),
            Some(c.tint),
            Some(c.screen_tint),
            Some(c.mask_threshold),
        ),
        _ => (None, None, None, None, None),
    };
    let mg_translate_children = match &node.kind {
        ModelNodeKind::MeshGroup(mg) => Some(mg.translate_children),
        _ => None,
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
        masks: match &node.kind {
            ModelNodeKind::Part(p) => p.masks(),
            ModelNodeKind::Composite(c) => c.masks(),
            _ => &[],
        }
        .iter()
        .map(|mask| MaskInfo {
            source: mask.source().clone(),
            mode: mask.mode().into(),
        })
        .collect(),
        id: id.clone(),
        kind: NodeKind::of(&node.kind),
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
        mg_translate_children,
        physics: match &node.kind {
            ModelNodeKind::SimplePhysics(ph) => {
                let targets = ph.target_params();
                Some(PhysicsInfo {
                    kind: ph.kind.into(),
                    map_mode: ph.map_mode.into(),
                    local_only: ph.local_only,
                    gravity: ph.gravity,
                    length: ph.length,
                    frequency: ph.frequency,
                    angle_damping: ph.angle_damping,
                    length_damping: ph.length_damping,
                    output_scale: ph.output_scale,
                    target_params: PhysicsTargets {
                        angle: targets[0].clone(),
                        length: targets[1].clone(),
                    },
                })
            }
            _ => None,
        },
        spine: match &node.kind {
            ModelNodeKind::Spine(spine) => Some(SpineInfo {
                joints: spine.joints().to_vec(),
                targets: spine.targets().to_vec(),
                chain: spine.chain().map(ChainArg::of),
            }),
            _ => None,
        },
    }
}

fn mesh_info(mesh: &catchlight_core::formats::clm::ClmMesh) -> MeshInfo {
    MeshInfo {
        verts: mesh
            .verts
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| [v[0], v[1]])
            .collect(),
        uvs: mesh
            .uvs
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| [v[0], v[1]])
            .collect(),
        indices: match &mesh.indices {
            catchlight_core::formats::clm::ClmIndices::U16(v) => v
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| [u32::from(t[0]), u32::from(t[1]), u32::from(t[2])])
                .collect(),
            catchlight_core::formats::clm::ClmIndices::U32(v) => v
                .as_chunks::<3>()
                .0
                .iter()
                .map(|t| [t[0], t[1], t[2]])
                .collect(),
        },
        origin: mesh.origin,
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
        key_positions: binding.key_positions().to_vec(),
        identity: match key.target {
            catchlight_core::BindingTarget::Scalar(t) => BindingIdentity::Scalar {
                scalar: t.identity(),
            },
            catchlight_core::BindingTarget::Deform => BindingIdentity::Deform {
                offset: [0.0; 2],
                vertex_count: model.node_mesh(&key.node).map_or(0, |m| m.vertex_count()) as u32,
            },
        },
        target: key.target.into(),
        param: key.params.x().clone(),
        param_y: key.params.y().cloned(),
        interpolate: binding.interpolate_mode().into(),
        width,
        height,
        keys,
        authored,
    })
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
            bindings: model.bindings_of_param(pid).count() as u32,
        });
    }
    out
}

/// The wire spelling of one slot. Public because the egui editor's slot panel
/// reads a part's slots straight off the model rather than over the protocol,
/// and there is one spelling of a slot or there are two.
pub fn slot_info(slot: &catchlight_core::Slot) -> SlotInfo {
    SlotInfo {
        id: slot.id().clone(),
        vertex: slot.vertex(),
    }
}

/// The wire spelling of one weld, public for the same reason.
pub fn weld_info(weld: &ModelWeld) -> WeldInfo {
    WeldInfo {
        a: weld.a().clone(),
        b: weld.b().clone(),
        pairs: weld
            .pairs()
            .iter()
            .map(|pair| SlotPair {
                a: pair.a.clone(),
                b: pair.b.clone(),
                weight: pair.weight,
            })
            .collect(),
    }
}
