//! Lints that surface rig problems an editor (or an agent) can self-correct on.
//! These are warnings, not errors — the model still flattens to a valid `.clp`.

use catchlight_core::formats::clp::{ClpBindingValues, ClpIndices};

use crate::model::*;

#[derive(Debug, Clone)]
pub struct CheckWarning {
    /// The node the warning is about, if any.
    pub node: Option<NodeId>,
    pub message: String,
}

impl EditModel {
    /// Walk the model and report likely-unintended states: parts that render to
    /// nothing, malformed meshes, physics nodes that drive nothing, and bindings
    /// whose value matrix does not match their param's axis grid.
    pub fn check(&self) -> Vec<CheckWarning> {
        let mut out = Vec::new();
        for id in self.nodes_in_order() {
            let Some(n) = self.node(id) else { continue };
            match &n.kind {
                EditNodeKind::Part(p) => {
                    if p.albedo.is_none() {
                        out.push(warn(
                            id,
                            format!("part {:?} has no texture (the renderer culls it)", n.name),
                        ));
                    }
                    let verts = p.mesh.verts.len() / 2;
                    if p.mesh.uvs.len() / 2 != verts {
                        out.push(warn(
                            id,
                            format!(
                                "part {:?} has {} vertices but {} uvs",
                                n.name,
                                verts,
                                p.mesh.uvs.len() / 2
                            ),
                        ));
                    }
                    let tris = match &p.mesh.indices {
                        ClpIndices::U16(v) => v.len() / 3,
                        ClpIndices::U32(v) => v.len() / 3,
                    };
                    if tris == 0 && p.albedo.is_some() {
                        out.push(warn(
                            id,
                            format!(
                                "part {:?} is textured but its mesh has no triangles",
                                n.name
                            ),
                        ));
                    }
                }
                EditNodeKind::SimplePhysics(ph) if ph.target_param.is_none() => {
                    out.push(warn(
                        id,
                        format!("physics node {:?} drives no target param", n.name),
                    ));
                }
                _ => {}
            }
        }

        for &pid in self.param_ids() {
            let Some(p) = self.param(pid) else { continue };
            let w = p.axis_points_x.len().max(1) as u32;
            let h = p.axis_points_y.len().max(1) as u32;
            for b in &p.bindings {
                let stray = cells_outside(&b.values, w, h);
                if stray > 0 {
                    out.push(CheckWarning {
                        node: Some(b.node),
                        message: format!(
                            "param {:?}: {} authored cell(s) outside the {}x{} axis grid",
                            p.name, stray, w, h
                        ),
                    });
                }
            }
        }
        out
    }
}

fn warn(node: NodeId, message: String) -> CheckWarning {
    CheckWarning {
        node: Some(node),
        message,
    }
}

fn cells_outside(values: &ClpBindingValues, w: u32, h: u32) -> usize {
    use ClpBindingValues::*;
    match values {
        Deform(c) => c.cells.iter().filter(|c| c.x >= w || c.y >= h).count(),
        ZOrder(c) | TransformTX(c) | TransformTY(c) | TransformSX(c) | TransformSY(c)
        | TransformRX(c) | TransformRY(c) | TransformRZ(c) | Opacity(c) | TintR(c) | TintG(c)
        | TintB(c) | ScreenTintR(c) | ScreenTintG(c) | ScreenTintB(c) | OutputScaleX(c)
        | OutputScaleY(c) => c.cells.iter().filter(|c| c.x >= w || c.y >= h).count(),
    }
}
