//! Lints that surface rig problems an editor (or an agent) can self-correct on.
//! These are warnings, not errors — the model still flattens to a `.clp`.
//! Most are cosmetic; the exception is a colour binding on a mesh group, which
//! flattens to a file `catchlight_core` then refuses to load (a mesh group is
//! never drawn, so it has no colour for the binding to fold into).

use catchlight_core::formats::clp::{ClpBindingValues, ClpIndices};

use crate::binding::{target_of, BindingTarget};
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
                if let BindingTarget::Scalar(t) = target_of(&b.values) {
                    if t.is_color()
                        && matches!(
                            self.node(b.node).map(|n| &n.kind),
                            Some(EditNodeKind::MeshGroup(_))
                        )
                    {
                        out.push(CheckWarning {
                            node: Some(b.node),
                            message: format!(
                                "param {:?}: {} binding targets a mesh group, which has no \
                                 colour (the runtime refuses to load this model)",
                                p.name,
                                t.name()
                            ),
                        });
                    }
                }
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

#[cfg(test)]
mod tests {
    use catchlight_core::formats::clp::ClpMesh;

    use crate::binding::ScalarTarget;
    use crate::model::*;

    /// The editor can hold a colour binding on a mesh group — a `.clp` written
    /// by an older tool opens fine — but the runtime will not load the model
    /// back, so `check` has to say so.
    #[test]
    fn check_flags_a_color_binding_on_a_mesh_group() {
        let mut m = EditModel::new();
        let root = m.root();
        let group = m
            .add_node(
                root,
                EditNode::new(
                    "lattice",
                    EditNodeKind::MeshGroup(EditMeshGroup {
                        mesh: ClpMesh::default().into(),
                        dynamic: false,
                        translate_children: true,
                    }),
                ),
            )
            .unwrap();
        let param = m.add_param(EditParam {
            name: "shade".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: Vec::new(),
        });

        m.add_scalar_binding(param, group, ScalarTarget::Tx)
            .unwrap();
        assert!(!m
            .check()
            .iter()
            .any(|w| w.message.contains("has no colour")));

        m.add_scalar_binding(param, group, ScalarTarget::Opacity)
            .unwrap();
        let warnings = m.check();
        let w = warnings
            .iter()
            .find(|w| w.message.contains("has no colour"))
            .expect("a colour binding on a mesh group is flagged");
        assert_eq!(w.node, Some(group));
        assert!(w.message.contains("opacity"), "{}", w.message);
    }
}
