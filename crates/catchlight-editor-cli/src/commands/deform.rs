//! `deform`: exact deform offsets.

use catchlight_editor_protocol::{
    BindingCellValue, BindingCellWrite, BindingTarget, MeshInfo, NodeId,
};
use clap::Subcommand;

use crate::args::{parse_cell, parse_vec2, read_offsets};
use crate::commands::binding::{BindingParamsArg, cell_write};
use crate::route::Route;

#[derive(Subcommand)]
pub enum DeformCmd {
    /// Write a raw deform contribution around the rest mesh origin. An absent
    /// binding gets default endpoint axes.
    Set {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = parse_cell)]
        cell: [u32; 2],
        /// `x,y`
        #[arg(long, allow_hyphen_values = true, value_parser = parse_vec2)]
        translate: Option<[f32; 2]>,
        #[arg(long, allow_hyphen_values = true)]
        rotate: Option<f32>,
        /// `x,y`
        #[arg(long, allow_hyphen_values = true, value_parser = parse_vec2)]
        scale: Option<[f32; 2]>,
    },
    /// Write exact per-vertex offsets from a JSON array; no pose conversion.
    Vertices {
        #[command(flatten)]
        params: BindingParamsArg,
        #[arg(long)]
        node: NodeId,
        #[arg(long, value_parser = parse_cell)]
        cell: [u32; 2],
        #[arg(long)]
        file: String,
    },
}

pub fn route(action: &DeformCmd) -> Route<'_> {
    Route::composed(move |remote| {
        let (params, node, cell, offsets) = match action {
            DeformCmd::Vertices {
                params,
                node,
                cell,
                file,
            } => (params, node, *cell, read_offsets(file)?),
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
                    translate.unwrap_or([0.0; 2]),
                    rotate.unwrap_or(0.0),
                    scale.unwrap_or([1.0; 2]),
                );
                (params, node, *cell, offsets)
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
        )])
    })
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
