//! `binding`: what params drive on a node.

use anyhow::{Result, anyhow, bail};
use catchlight_editor_protocol::{
    BindingCellRead, BindingCellValue, BindingCellWrite, BindingIdentity, BindingInfo,
    BindingParams, BindingTarget, Command, EditOp, Interpolate, NodeId, ParamId, ScalarTarget,
};
use clap::Subcommand;

use crate::args::{parse_cell, wire_enum};
use crate::cli::resolve_session;
use crate::route::Route;

#[derive(clap::Args)]
pub(crate) struct BindingParamsArg {
    #[arg(long)]
    param: ParamId,
    /// A second param; `--cell x,y` then indexes both grid axes.
    #[arg(long = "param-y")]
    param_y: Option<ParamId>,
}

impl BindingParamsArg {
    pub(crate) fn wire(&self) -> BindingParams {
        BindingParams {
            param: self.param.clone(),
            param_y: self.param_y.clone(),
        }
    }
}

#[derive(clap::Args)]
pub(crate) struct BindingSelection {
    #[command(flatten)]
    params: BindingParamsArg,
    #[arg(long)]
    node: NodeId,
    #[arg(long, value_parser = wire_enum::<BindingTarget>)]
    target: BindingTarget,
}

#[derive(Subcommand)]
pub(crate) enum BindingCmd {
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
        #[arg(long, value_parser = parse_cell)]
        cell: [u32; 2],
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
        #[arg(long, value_parser = parse_cell)]
        cell: [u32; 2],
    },
    /// Author the identity value at a keypoint.
    Reset {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = wire_enum::<BindingTarget>)]
        target: BindingTarget,
        #[arg(long, value_parser = parse_cell)]
        cell: [u32; 2],
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
        #[arg(long, value_parser = parse_cell)]
        from: [u32; 2],
        #[arg(long, value_parser = parse_cell)]
        to: [u32; 2],
        #[arg(long)]
        derived: bool,
    },
}

pub(crate) fn route(action: &BindingCmd) -> Route<'_> {
    match action {
        BindingCmd::KeyInsert {
            selection,
            axis,
            value,
        } => Route::composed(move |remote| {
            remote.batch(vec![EditOp::BindingKeyInsert {
                node: selection.node.clone(),
                params: selection.params.wire(),
                target: selection.target,
                axis: axis.clone(),
                value: *value,
            }])
        }),
        BindingCmd::KeyDelete {
            selection,
            axis,
            index,
        } => Route::composed(move |remote| {
            remote.batch(vec![EditOp::BindingKeyDelete {
                node: selection.node.clone(),
                params: selection.params.wire(),
                target: selection.target,
                axis: axis.clone(),
                index: *index,
            }])
        }),
        BindingCmd::KeyMove {
            selection,
            axis,
            index,
            value,
        } => Route::composed(move |remote| {
            remote.batch(vec![EditOp::BindingKeyMove {
                node: selection.node.clone(),
                params: selection.params.wire(),
                target: selection.target,
                axis: axis.clone(),
                index: *index,
                value: *value,
            }])
        }),
        BindingCmd::Flip { selection, axis } => Route::composed(move |remote| {
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
        }),
        BindingCmd::Add {
            params,
            node,
            target,
            keys,
        } => Route::plain(move |cli| {
            let session = resolve_session(cli)?;
            Ok(Command::BindingAdd {
                key_positions: keys
                    .as_ref()
                    .map(|keys| serde_json::from_str(keys))
                    .transpose()?,
                session,
                params: params.wire(),
                node: node.clone(),
                target: *target,
            })
        }),
        BindingCmd::Key {
            params,
            node,
            target,
            cell,
            value,
        } => Route::composed(move |remote| {
            remote.batch(vec![cell_write(
                node,
                params.wire(),
                (*target).into(),
                vec![BindingCellWrite {
                    cell: *cell,
                    value: BindingCellValue::Scalar(*value),
                }],
            )])
        }),
        BindingCmd::Unset {
            params,
            node,
            target,
            cell,
        } => Route::composed(move |remote| {
            remote.batch(vec![EditOp::BindingCellsUnset {
                node: node.clone(),
                params: params.wire(),
                target: *target,
                cells: vec![*cell],
            }])
        }),
        BindingCmd::Reset {
            params,
            node,
            target,
            cell,
        } => Route::composed(move |remote| {
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
                vec![BindingCellWrite { cell: *cell, value }],
            )])
        }),
        BindingCmd::Delete {
            params,
            node,
            target,
        } => Route::on_session(move |session| Command::BindingDelete {
            session,
            params: params.wire(),
            node: node.clone(),
            target: *target,
        }),
        BindingCmd::Interpolate {
            params,
            node,
            target,
            mode,
        } => Route::on_session(move |session| Command::BindingInterpolate {
            session,
            params: params.wire(),
            node: node.clone(),
            target: *target,
            mode: *mode,
        }),
        BindingCmd::Invert {
            params,
            node,
            target,
        } => Route::composed(move |remote| {
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
        }),
        BindingCmd::List { node } => Route::on_session(move |session| Command::BindingList {
            session,
            node: node.clone(),
        }),
        BindingCmd::CopyKey {
            params,
            node,
            target,
            from,
            to,
            derived,
        } => Route::composed(move |remote| {
            let cells = remote.cells(node, params.wire(), *target, vec![*from], *derived)?;
            let read = cells
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("source cell not returned"))?;
            let value = copy_value(read, *derived)?;
            remote.batch(vec![cell_write(
                node,
                params.wire(),
                *target,
                vec![BindingCellWrite { cell: *to, value }],
            )])
        }),
    }
}

pub(crate) fn cell_write(
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
    // Decreases ascending, then increases descending, so each move stays
    // between its current neighbours.
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::build_command;
    use crate::cli::Cli;

    fn id(value: &str) -> NodeId {
        NodeId::new(value).unwrap()
    }
    fn param(value: &str) -> ParamId {
        ParamId::new(value).unwrap()
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
}
