//! Caller-side shortcuts over exact reads and atomic writes.
//!
//! Every read used by a shortcut must carry the same captured revision. The
//! final `edit_apply` uses that revision, so concurrent work is refused rather
//! than overwritten. Holes remain holes unless a command explicitly authors
//! identity or requests a derived copy. No shortcut writes recorded pose values.

use super::*;

/// Execute commands that need captured reads, or return control to the direct
/// one-command path. This context never retries a refused edit.
pub(super) enum Output {
    Reply(Reply),
    Unfilled { revision: u64, slots: Vec<SlotAddr> },
}

pub(super) fn execute(cli: &Cli, stream: &mut UnixStream) -> Result<Option<Output>> {
    let composed = matches!(
        &cli.cmd,
        Cmd::Edit { .. }
            | Cmd::History
            | Cmd::Goto { .. }
            | Cmd::Export { .. }
            | Cmd::Undo
            | Cmd::Redo
            | Cmd::Session {
                action: SessionCmd::Fork { .. }
            }
            | Cmd::Node {
                action: NodeCmd::Move { .. }
            }
            | Cmd::Mesh {
                action: MeshCmd::Copy { .. }
            }
            | Cmd::Slot {
                action: SlotCmd::Unfilled
            }
            | Cmd::Weld {
                action: WeldCmd::Weight { .. }
            }
            | Cmd::Mask {
                action: MaskCmd::Set { .. } | MaskCmd::Reorder { .. }
            }
            | Cmd::Deform { .. }
            | Cmd::Binding {
                action: BindingCmd::Key { .. }
                    | BindingCmd::Unset { .. }
                    | BindingCmd::Reset { .. }
                    | BindingCmd::Invert { .. }
                    | BindingCmd::CopyKey { .. }
                    | BindingCmd::KeyInsert { .. }
                    | BindingCmd::KeyDelete { .. }
                    | BindingCmd::KeyMove { .. }
                    | BindingCmd::Flip { .. }
            }
    );
    if !composed {
        return Ok(None);
    }
    let session = resolve_session(cli)?;
    let mut remote = CapturedClient {
        stream,
        session,
        revision: cli.if_rev,
        next_id: 1,
    };
    let command = match &cli.cmd {
        Cmd::History => Command::EditHistoryGet {
            session,
            if_rev: cli.if_rev,
        },
        Cmd::Undo => Command::Undo {
            session,
            if_rev: remote.guard()?,
        },
        Cmd::Redo => Command::Redo {
            session,
            if_rev: remote.guard()?,
        },
        Cmd::Goto { revision } => Command::EditGoto {
            session,
            if_rev: remote.guard()?,
            revision: *revision,
        },
        Cmd::Export { .. } => Command::ModelExport {
            session,
            if_rev: remote.guard()?,
        },
        Cmd::Session {
            action: SessionCmd::Fork { name },
        } => Command::SessionFork {
            session,
            if_rev: remote.guard()?,
            name: name.clone(),
        },
        Cmd::Edit { file, validate } => {
            let edits = serde_json::from_str(&read_json(file)?)?;
            let if_rev = remote.guard()?;
            if *validate {
                Command::EditValidate {
                    session,
                    if_rev,
                    edits,
                }
            } else {
                Command::EditApply {
                    session,
                    if_rev,
                    edits,
                }
            }
        }
        Cmd::Node {
            action:
                NodeCmd::Move {
                    node,
                    parent,
                    index,
                },
        } => remote.batch(vec![
            EditOp::NodeReparent {
                node: node.clone(),
                to: parent.clone(),
            },
            EditOp::NodeReorder {
                node: node.clone(),
                index: *index,
            },
        ])?,
        Cmd::Mesh {
            action: MeshCmd::Copy { node, from },
        } => {
            let mesh = remote.mesh(from)?;
            remote.batch(vec![EditOp::MeshSet {
                node: node.clone(),
                verts: mesh.verts,
                uvs: mesh.uvs,
                indices: mesh.indices,
                origin: mesh.origin,
                deform_mapping: None,
            }])?
        }
        Cmd::Slot {
            action: SlotCmd::Unfilled,
        } => {
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
            return Ok(Some(Output::Unfilled {
                revision: remote.guard()?,
                slots,
            }));
        }
        Cmd::Weld {
            action: WeldCmd::Weight { a, b, slot, weight },
        } => {
            let ResponseBody::Welds { welds } = remote.read(Command::Welds { session })? else {
                bail!("weld_list returned an unexpected body");
            };
            let weld = welds
                .into_iter()
                .find(|w| (&w.a == a && &w.b == b) || (&w.a == b && &w.b == a))
                .ok_or_else(|| anyhow!("no weld joins {a} and {b}"))?;
            let pairs = reweight(weld, a, slot, *weight)?;
            remote.batch(vec![EditOp::WeldSet {
                a: pairs.a,
                b: pairs.b,
                pairs: pairs.pairs,
            }])?
        }
        Cmd::Mask {
            action: MaskCmd::Set { node, .. } | MaskCmd::Reorder { node, .. },
        } => {
            let ResponseBody::NodeInfo { node: info } = remote.read(Command::NodeInfo {
                session,
                node: node.clone(),
            })?
            else {
                bail!("node_get returned an unexpected body");
            };
            let mut masks = info.masks;
            let count = masks.len();
            match &cli.cmd {
                Cmd::Mask {
                    action: MaskCmd::Set { index, mode, .. },
                } => {
                    let mask = masks
                        .get_mut(*index as usize)
                        .ok_or_else(|| anyhow!("mask index out of range"))?;
                    mask.mode = *mode;
                }
                Cmd::Mask {
                    action: MaskCmd::Reorder { index, to, .. },
                } => {
                    if *index as usize >= count {
                        bail!("mask index out of range");
                    }
                    let mask = masks.remove(*index as usize);
                    masks.insert((*to as usize).min(masks.len()), mask);
                }
                _ => unreachable!(),
            }
            remote.batch(replace_masks(node, count, masks))?
        }
        Cmd::Binding { action } => binding_command(&mut remote, action)?,
        Cmd::Deform { action } => {
            let (params, node, cell, offsets) = match action {
                DeformCmd::Vertices {
                    params,
                    node,
                    cell,
                    file,
                } => (params, node, parse_cell(cell)?, read_offsets(file)?),
                DeformCmd::Set {
                    params,
                    node,
                    cell,
                    translate,
                    rotate,
                    scale,
                } => {
                    let mesh = remote.mesh(node)?;
                    let offsets = affine_offsets(
                        &mesh,
                        translate
                            .as_deref()
                            .map(parse_vec2)
                            .transpose()?
                            .unwrap_or([0.0; 2]),
                        rotate.unwrap_or(0.0),
                        scale
                            .as_deref()
                            .map(parse_vec2)
                            .transpose()?
                            .unwrap_or([1.0; 2]),
                    );
                    (params, node, parse_cell(cell)?, offsets)
                }
            };
            remote.batch(vec![cell_write(
                node,
                params.wire(),
                BindingTarget::Deform,
                vec![BindingCellWrite {
                    cell,
                    value: BindingCellValue::Offsets(offsets),
                }],
            )])?
        }
        _ => unreachable!("composed command classified above"),
    };
    Ok(Some(Output::Reply(remote.send(command, siblings(cli)?)?)))
}

struct CapturedClient<'a> {
    stream: &'a mut UnixStream,
    session: SessionId,
    revision: Option<u64>,
    next_id: u64,
}

impl CapturedClient<'_> {
    fn send(
        &mut self,
        command: Command,
        siblings: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        call(self.stream, Request { id, command }, siblings)
    }

    fn read(&mut self, command: Command) -> Result<ResponseBody> {
        match self.send(command, serde_json::Map::new())? {
            Reply::Ok {
                rev: Some(revision),
                body,
                ..
            } => {
                capture_revision(&mut self.revision, revision)?;
                Ok(body)
            }
            Reply::Err { code, message, .. } => bail!("{}: {message}", code_name(code)),
            _ => bail!("captured model read omitted its revision"),
        }
    }

    fn guard(&mut self) -> Result<u64> {
        if self.revision.is_none() {
            self.read(Command::Status {
                session: self.session,
            })?;
        }
        self.revision
            .ok_or_else(|| anyhow!("captured model read omitted its revision"))
    }

    fn batch(&mut self, edits: Vec<EditOp>) -> Result<Command> {
        Ok(Command::EditApply {
            session: self.session,
            if_rev: self.guard()?,
            edits,
        })
    }

    fn mesh(&mut self, node: &NodeId) -> Result<MeshInfo> {
        match self.read(Command::MeshGet {
            session: self.session,
            node: node.clone(),
            if_rev: self.revision,
        })? {
            ResponseBody::MeshInfo { mesh, .. } => Ok(mesh),
            _ => bail!("mesh_get returned an unexpected body"),
        }
    }

    fn binding(
        &mut self,
        node: &NodeId,
        params: &BindingParams,
        target: BindingTarget,
    ) -> Result<BindingInfo> {
        let ResponseBody::Bindings { bindings } = self.read(Command::BindingList {
            session: self.session,
            node: node.clone(),
        })?
        else {
            bail!("binding_list returned an unexpected body");
        };
        bindings
            .into_iter()
            .find(|binding| {
                binding.param == params.param
                    && binding.param_y == params.param_y
                    && binding.target == target
            })
            .ok_or_else(|| anyhow!("binding does not exist; create it with binding add"))
    }

    fn cells(
        &mut self,
        node: &NodeId,
        params: BindingParams,
        target: BindingTarget,
        cells: Vec<[u32; 2]>,
        derived: bool,
    ) -> Result<Vec<BindingCellRead>> {
        if cells.is_empty() {
            return Ok(Vec::new());
        }
        let ResponseBody::BindingCells { cells, .. } = self.read(Command::BindingCellsGet {
            session: self.session,
            if_rev: self.revision,
            node: node.clone(),
            params,
            target,
            cells,
            include_derived: derived,
            vertices: None,
        })?
        else {
            bail!("binding_cells_get returned an unexpected body");
        };
        Ok(cells)
    }
}

fn capture_revision(captured: &mut Option<u64>, revision: u64) -> Result<()> {
    if captured.is_some_and(|held| held != revision) {
        bail!("revision_conflict: model changed between reads; no edit was sent");
    }
    *captured = Some(revision);
    Ok(())
}

fn binding_command(remote: &mut CapturedClient<'_>, action: &BindingCmd) -> Result<Command> {
    match action {
        BindingCmd::KeyInsert {
            selection,
            axis,
            value,
        } => remote.batch(vec![EditOp::BindingKeyInsert {
            node: selection.node.clone(),
            params: selection.params.wire(),
            target: selection.target,
            axis: axis.clone(),
            value: *value,
        }]),
        BindingCmd::KeyDelete {
            selection,
            axis,
            index,
        } => remote.batch(vec![EditOp::BindingKeyDelete {
            node: selection.node.clone(),
            params: selection.params.wire(),
            target: selection.target,
            axis: axis.clone(),
            index: *index,
        }]),
        BindingCmd::KeyMove {
            selection,
            axis,
            index,
            value,
        } => remote.batch(vec![EditOp::BindingKeyMove {
            node: selection.node.clone(),
            params: selection.params.wire(),
            target: selection.target,
            axis: axis.clone(),
            index: *index,
            value: *value,
        }]),
        BindingCmd::Key {
            params,
            node,
            target,
            cell,
            value,
        } => remote.batch(vec![cell_write(
            node,
            params.wire(),
            (*target).into(),
            vec![BindingCellWrite {
                cell: parse_cell(cell)?,
                value: BindingCellValue::Scalar(*value),
            }],
        )]),
        BindingCmd::Unset {
            params,
            node,
            target,
            cell,
        } => remote.batch(vec![EditOp::BindingCellsUnset {
            node: node.clone(),
            params: params.wire(),
            target: *target,
            cells: vec![parse_cell(cell)?],
        }]),
        BindingCmd::Reset {
            params,
            node,
            target,
            cell,
        } => {
            let binding = remote.binding(node, &params.wire(), *target)?;
            let value = match binding.identity {
                BindingIdentity::Scalar { scalar } => BindingCellValue::Scalar(scalar),
                BindingIdentity::Deform {
                    offset,
                    vertex_count,
                } => BindingCellValue::Offsets(vec![offset; vertex_count as usize]),
            };
            remote.batch(vec![cell_write(
                node,
                params.wire(),
                *target,
                vec![BindingCellWrite {
                    cell: parse_cell(cell)?,
                    value,
                }],
            )])
        }
        BindingCmd::CopyKey {
            params,
            node,
            target,
            from,
            to,
            derived,
        } => {
            let cells = remote.cells(
                node,
                params.wire(),
                *target,
                vec![parse_cell(from)?],
                *derived,
            )?;
            let read = cells
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("source cell not returned"))?;
            let value = copy_value(read, *derived)?;
            remote.batch(vec![cell_write(
                node,
                params.wire(),
                *target,
                vec![BindingCellWrite {
                    cell: parse_cell(to)?,
                    value,
                }],
            )])
        }
        BindingCmd::Invert {
            params,
            node,
            target,
        } => {
            let binding = remote.binding(node, &params.wire(), *target)?;
            let cells = remote.cells(
                node,
                params.wire(),
                *target,
                authored_cells(&binding),
                false,
            )?;
            let cells: Vec<_> = cells
                .into_iter()
                .filter_map(|cell| {
                    cell.value.map(|mut value| {
                        match &mut value {
                            BindingCellValue::Scalar(scalar) => *scalar = -*scalar,
                            BindingCellValue::Offsets(offsets) => {
                                for point in offsets {
                                    point[0] = -point[0];
                                    point[1] = -point[1];
                                }
                            }
                        }
                        BindingCellWrite {
                            cell: cell.cell,
                            value,
                        }
                    })
                })
                .collect();
            let edits = if cells.is_empty() {
                Vec::new()
            } else {
                vec![cell_write(node, params.wire(), *target, cells)]
            };
            remote.batch(edits)
        }
        BindingCmd::Flip { selection, axis } => {
            let params = selection.params.wire();
            let binding = remote.binding(&selection.node, &params, selection.target)?;
            let cells = remote.cells(
                &selection.node,
                params.clone(),
                selection.target,
                authored_cells(&binding),
                false,
            )?;
            remote.batch(flip_binding(selection, axis, binding, cells)?)
        }
        _ => unreachable!("only composed binding commands are routed here"),
    }
}

fn cell_write(
    node: &NodeId,
    params: BindingParams,
    target: BindingTarget,
    cells: Vec<BindingCellWrite>,
) -> EditOp {
    EditOp::BindingCellsSet {
        node: node.clone(),
        params,
        target,
        cells,
    }
}

fn copy_value(read: BindingCellRead, allow_derived: bool) -> Result<BindingCellValue> {
    read.value
        .or_else(|| allow_derived.then_some(read.derived).flatten())
        .ok_or_else(|| {
            anyhow!("source cell is un-authored; pass --derived to copy its evaluated contribution")
        })
}

fn authored_cells(binding: &BindingInfo) -> Vec<[u32; 2]> {
    binding
        .authored
        .iter()
        .enumerate()
        .flat_map(|(y, row)| {
            row.iter()
                .enumerate()
                .filter_map(move |(x, authored)| authored.then_some([x as u32, y as u32]))
        })
        .collect()
}

fn flip_binding(
    selection: &BindingSelection,
    axis: &ParamId,
    binding: BindingInfo,
    cells: Vec<BindingCellRead>,
) -> Result<Vec<EditOp>> {
    let params = selection.params.wire();
    let index = if axis == &params.param {
        0
    } else if Some(axis) == params.param_y.as_ref() {
        1
    } else {
        bail!("axis must name an input of this binding");
    };
    let positions = &binding.key_positions[index];
    let mirrored: Vec<f32> = positions
        .iter()
        .rev()
        .map(|position| 1.0 - position)
        .collect();
    if mirrored.is_empty()
        || mirrored
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        || mirrored.windows(2).any(|pair| pair[0] >= pair[1])
    {
        bail!("mirrored grid is not representable with strictly ordered normalized f32 positions");
    }
    let last = positions.len() as u32 - 1;
    // Decreases first in ascending order, increases in descending order, so
    // each individual move remains between its current neighbours. Preserve
    // the binding object itself: order and interpolation are authored state.
    let decreases = (0..positions.len()).filter(|&i| mirrored[i] < positions[i]);
    let increases = (0..positions.len())
        .rev()
        .filter(|&i| mirrored[i] > positions[i]);
    let mut edits: Vec<_> = decreases
        .chain(increases)
        .map(|i| EditOp::BindingKeyMove {
            node: selection.node.clone(),
            params: params.clone(),
            target: selection.target,
            axis: axis.clone(),
            index: i as u32,
            value: mirrored[i],
        })
        .collect();
    let authored: Vec<_> = cells
        .into_iter()
        .filter_map(|read| {
            read.value.map(|value| BindingCellWrite {
                cell: read.cell,
                value,
            })
        })
        .collect();
    if !authored.is_empty() {
        edits.push(EditOp::BindingCellsUnset {
            node: selection.node.clone(),
            params: params.clone(),
            target: selection.target,
            cells: authored.iter().map(|read| read.cell).collect(),
        });
        let mirrored = authored
            .into_iter()
            .map(|mut write| {
                write.cell[index] = last - write.cell[index];
                write
            })
            .collect();
        edits.push(cell_write(
            &selection.node,
            params,
            selection.target,
            mirrored,
        ));
    }
    Ok(edits)
}

fn replace_masks(node: &NodeId, count: usize, masks: Vec<MaskInfo>) -> Vec<EditOp> {
    (0..count)
        .rev()
        .map(|index| EditOp::MaskDelete {
            node: node.clone(),
            index: index as u32,
        })
        .chain(masks.into_iter().map(|mask| EditOp::MaskAdd {
            node: node.clone(),
            source: mask.source,
            mode: mask.mode,
        }))
        .collect()
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

fn affine_offsets(
    mesh: &MeshInfo,
    translate: [f32; 2],
    rotate: f32,
    scale: [f32; 2],
) -> Vec<[f32; 2]> {
    let (sin, cos) = rotate.sin_cos();
    mesh.verts
        .iter()
        .map(|&[x, y]| {
            let dx = (x - mesh.origin[0]) * scale[0];
            let dy = (y - mesh.origin[1]) * scale[1];
            [
                dx * cos - dy * sin + mesh.origin[0] + translate[0] - x,
                dx * sin + dy * cos + mesh.origin[1] + translate[1] - y,
            ]
        })
        .collect()
}

fn part_ids(root: &TreeNode, parts: &mut Vec<NodeId>) {
    if root.kind == NodeKind::Part {
        parts.push(root.id.clone());
    }
    for child in &root.children {
        part_ids(child, parts);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> NodeId {
        NodeId::new(value).unwrap()
    }
    fn param(value: &str) -> ParamId {
        ParamId::new(value).unwrap()
    }

    #[test]
    fn affine_offsets_respect_the_authored_pivot() {
        let mesh = MeshInfo {
            verts: vec![[12.0, 5.0], [10.0, 8.0]],
            uvs: vec![],
            indices: vec![],
            origin: [10.0, 5.0],
        };
        let offsets = affine_offsets(&mesh, [3.0, 4.0], std::f32::consts::FRAC_PI_2, [2.0, 1.0]);
        assert!((offsets[0][0] - 1.0).abs() < 1e-5);
        assert!((offsets[0][1] - 8.0).abs() < 1e-5);
        assert!(offsets[1][0].abs() < 1e-5);
        assert!((offsets[1][1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn flipped_grid_preserves_sparse_cells_and_interpolation() {
        let selection = BindingSelection {
            params: BindingParamsArg {
                param: param("x"),
                param_y: Some(param("y")),
            },
            node: id("panel"),
            target: BindingTarget::Deform,
        };
        let binding = BindingInfo {
            key_positions: vec![vec![0.0, 0.25, 1.0], vec![0.0, 1.0]],
            identity: BindingIdentity::Deform {
                offset: [0.0, 0.0],
                vertex_count: 1,
            },
            target: BindingTarget::Deform,
            param: param("x"),
            param_y: Some(param("y")),
            interpolate: Interpolate::Cubic,
            width: 3,
            height: 2,
            keys: vec![vec![None; 3]; 2],
            authored: vec![vec![false, true, false], vec![false, false, true]],
        };
        let cells = vec![
            BindingCellRead {
                cell: [1, 0],
                authored: true,
                value: Some(BindingCellValue::Offsets(vec![[2.0, 3.0]])),
                derived: None,
            },
            BindingCellRead {
                cell: [2, 1],
                authored: true,
                value: Some(BindingCellValue::Offsets(vec![[0.0, 0.0]])),
                derived: None,
            },
        ];
        let edits = flip_binding(&selection, &param("x"), binding, cells).unwrap();
        assert_eq!(edits.len(), 3);
        assert!(matches!(
            &edits[0],
            EditOp::BindingKeyMove {
                index: 1,
                value: 0.75,
                ..
            }
        ));
        assert!(
            matches!(&edits[1], EditOp::BindingCellsUnset { cells, .. } if cells == &vec![[1, 0], [2, 1]])
        );
        let EditOp::BindingCellsSet { cells, .. } = &edits[2] else {
            panic!()
        };
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].cell, [1, 0]);
        assert_eq!(cells[1].cell, [0, 1]);
        assert_eq!(cells[1].value, BindingCellValue::Offsets(vec![[0.0, 0.0]]));
    }

    #[test]
    fn flip_all_unset_moves_keys_without_recreating_or_writing_cells() {
        let selection = BindingSelection {
            params: BindingParamsArg {
                param: param("x"),
                param_y: None,
            },
            node: id("panel"),
            target: BindingTarget::Tx,
        };
        let positions = vec![0.0, 0.1, 0.6, 0.7, 0.8, 1.0];
        let binding = BindingInfo {
            key_positions: vec![positions.clone()],
            identity: BindingIdentity::Scalar { scalar: 0.0 },
            target: BindingTarget::Tx,
            param: param("x"),
            param_y: None,
            interpolate: Interpolate::Cubic,
            width: 6,
            height: 1,
            keys: vec![vec![None; 6]],
            authored: vec![vec![false; 6]],
        };
        let edits = flip_binding(&selection, &param("x"), binding.clone(), Vec::new()).unwrap();
        let mut applied = positions.clone();
        let mut visited = Vec::new();
        for edit in edits {
            let EditOp::BindingKeyMove { index, value, .. } = edit else {
                panic!("mirror must preserve binding order and interpolation")
            };
            let index = index as usize;
            assert!(applied[index - 1] < value && value < applied[index + 1]);
            applied[index] = value;
            visited.push(index);
        }
        assert_eq!(visited, [2, 3, 4, 1]);
        assert_eq!(
            applied,
            positions.iter().rev().map(|v| 1.0 - v).collect::<Vec<_>>()
        );
        for axis in [vec![0.2], vec![0.1, 0.4, 0.9]] {
            let mut scoped = binding.clone();
            scoped.key_positions = vec![axis.clone()];
            let mut moved = axis.clone();
            for edit in flip_binding(&selection, &param("x"), scoped, Vec::new()).unwrap() {
                let EditOp::BindingKeyMove { index, value, .. } = edit else {
                    panic!()
                };
                let index = index as usize;
                assert!(index == 0 || moved[index - 1] < value);
                assert!(index + 1 == moved.len() || value < moved[index + 1]);
                moved[index] = value;
            }
            assert_eq!(
                moved,
                axis.iter().rev().map(|v| 1.0 - v).collect::<Vec<_>>()
            );
        }
        let mut invalid = binding;
        invalid.key_positions = vec![vec![0.0, f32::MIN_POSITIVE, 1.0]];
        assert!(flip_binding(&selection, &param("x"), invalid, Vec::new()).is_err());
    }

    #[test]
    fn copying_a_hole_requires_explicit_derived_consent() {
        let read = BindingCellRead {
            cell: [1, 0],
            authored: false,
            value: None,
            derived: Some(BindingCellValue::Scalar(0.75)),
        };
        assert!(copy_value(read.clone(), false).is_err());
        assert_eq!(
            copy_value(read, true).unwrap(),
            BindingCellValue::Scalar(0.75)
        );
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

    #[test]
    fn coherent_reads_capture_once_and_reject_later_revisions() {
        let mut revision = None;
        capture_revision(&mut revision, 42).unwrap();
        capture_revision(&mut revision, 42).unwrap();
        assert!(capture_revision(&mut revision, 43).is_err());
        assert_eq!(revision, Some(42));
    }

    #[test]
    fn move_is_one_guarded_batch_after_one_captured_read() {
        let cli = Cli::try_parse_from([
            "cli",
            "--session",
            "7",
            "node",
            "move",
            "panel",
            "--parent",
            "head",
            "--index",
            "2",
        ])
        .unwrap();
        let (mut client, server) = UnixStream::pair().unwrap();
        let handler = std::thread::spawn(move || {
            let mut reader = BufReader::new(server);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            assert!(matches!(
                request.command,
                Command::Status {
                    session: SessionId(7)
                }
            ));
            writeln!(reader.get_mut(), "{}", serde_json::json!({"reply":"ok", "id":request.id, "rev":12, "body":{"result":"empty"}})).unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            let request: Request = serde_json::from_str(&line).unwrap();
            let Command::EditApply { if_rev, edits, .. } = request.command else {
                panic!()
            };
            assert_eq!(if_rev, 12);
            assert_eq!(edits.len(), 2);
            assert!(
                matches!(&edits[0], EditOp::NodeReparent { node, to } if node == &id("panel") && to == &id("head"))
            );
            assert!(matches!(&edits[1], EditOp::NodeReorder { index: 2, .. }));
            writeln!(reader.get_mut(), "{}", serde_json::json!({"reply":"ok", "id":request.id, "rev":13, "body":{"result":"edit_results", "changed":true,"results":[]}})).unwrap();
        });
        assert!(matches!(
            execute(&cli, &mut client).unwrap(),
            Some(Output::Reply(Reply::Ok { rev: Some(13), .. }))
        ));
        handler.join().unwrap();
    }
}
