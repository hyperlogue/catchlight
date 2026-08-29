//! Lints that surface rig problems an editor (or an agent) can self-correct on.
//! These are warnings, not errors — the model still flattens to a `.clp`.
//! Most are cosmetic; the exception is a colour binding on a mesh group, which
//! flattens to a file `catchlight_core` then refuses to load (a mesh group is
//! never drawn, so it has no colour for the binding to fold into).
//! [`Model::add_binding`] refuses to author one, so only a file written by an
//! older tool can still carry it.

use crate::formats::clp::{ClpBindingValues, ClpIndices};

use super::*;
use crate::model::binding::BindingTarget;

#[derive(Debug, Clone)]
pub struct CheckWarning {
    /// The node the warning is about, if any.
    pub node: Option<NodeId>,
    pub message: String,
}

impl Model {
    /// Walk the model and report likely-unintended states: parts that render to
    /// nothing, malformed meshes, physics nodes that drive nothing, and bindings
    /// whose value matrix does not match their param's axis grid.
    pub fn check(&self) -> Vec<CheckWarning> {
        let mut out = Vec::new();
        for id in self.nodes_in_order() {
            let Some(n) = self.node(&id) else { continue };
            match &n.kind {
                ModelNodeKind::Part(p) => {
                    if p.albedo().is_none() {
                        out.push(warn(
                            id.clone(),
                            format!(
                                "part {:?} has no texture (the renderer culls it)",
                                n.name.as_str()
                            ),
                        ));
                    }
                    let mesh = p.mesh();
                    let verts = mesh.verts.len() / 2;
                    if mesh.uvs.len() / 2 != verts {
                        out.push(warn(
                            id.clone(),
                            format!(
                                "part {:?} has {} vertices but {} uvs",
                                n.name.as_str(),
                                verts,
                                mesh.uvs.len() / 2
                            ),
                        ));
                    }
                    let tris = match &mesh.indices {
                        ClpIndices::U16(v) => v.len() / 3,
                        ClpIndices::U32(v) => v.len() / 3,
                    };
                    if tris == 0 && p.albedo().is_some() {
                        out.push(warn(
                            id.clone(),
                            format!(
                                "part {:?} is textured but its mesh has no triangles",
                                n.name.as_str()
                            ),
                        ));
                    }
                }
                ModelNodeKind::SimplePhysics(ph) if ph.target_param().is_none() => {
                    out.push(warn(
                        id,
                        format!("physics node {:?} drives no target param", n.name.as_str()),
                    ));
                }
                _ => {}
            }
        }

        for b in self.bindings() {
            let Some(p) = self.param(b.param()) else {
                continue;
            };
            let w = p.axis_points_x.len().max(1) as u32;
            let h = p.axis_points_y.len().max(1) as u32;
            if let BindingTarget::Scalar(t) = b.target() {
                if t.is_color()
                    && matches!(
                        self.node(b.node()).map(|n| &n.kind),
                        Some(ModelNodeKind::MeshGroup(_))
                    )
                {
                    out.push(CheckWarning {
                        node: Some(b.node().clone()),
                        message: format!(
                            "param {:?}: {} binding targets a mesh group, which has no \
                             colour (the runtime refuses to load this model)",
                            p.name.as_str(),
                            t.name()
                        ),
                    });
                }
            }
            let stray = cells_outside(b.values(), w, h);
            if stray > 0 {
                out.push(CheckWarning {
                    node: Some(b.node().clone()),
                    message: format!(
                        "param {:?}: {} authored cell(s) outside the {}x{} key grid",
                        p.name.as_str(),
                        stray,
                        w,
                        h
                    ),
                });
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
    use crate::formats::clp::ClpMesh;
    use crate::id::{Name, SeededHex};
    use crate::model::binding::ScalarTarget;
    use crate::model::*;

    /// The editor refuses to author a colour binding on a mesh group, but a
    /// `.clp` written by an older tool can carry one — and the runtime will not
    /// load that model back, so `check` has to say so.
    #[test]
    fn check_flags_a_color_binding_on_a_mesh_group() {
        let mut hex = SeededHex::new(9);
        let mut m = Model::new();
        let root = m.root().clone();
        let group = m
            .add_node(
                &root,
                ModelNode::new(
                    "lattice",
                    ModelNodeKind::MeshGroup(ModelMeshGroup::new(ClpMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        let param = m
            .add_param(
                ModelParam {
                    name: Name::truncated("shade"),
                    is_vec2: false,
                    min: [0.0, 0.0],
                    max: [1.0, 0.0],
                    defaults: [0.0, 0.0],
                    axis_points_x: vec![0.0, 1.0],
                    axis_points_y: vec![0.0],
                },
                &mut hex,
            )
            .unwrap();

        m.add_binding(&BindingKey::new(
            param,
            group,
            BindingTarget::Scalar(ScalarTarget::Tx),
        ))
        .unwrap();
        assert!(!m
            .check()
            .iter()
            .any(|w| w.message.contains("has no colour")));

        // Authoring one is refused outright...
        let group = m
            .nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == "lattice"))
            .unwrap();
        assert!(matches!(
            m.add_binding(&BindingKey::new(
                m.param_ids()[0].clone(),
                group,
                BindingTarget::Scalar(ScalarTarget::Opacity)
            )),
            Err(ModelError::ColorOnMeshGroup)
        ));

        // ...so only a file can bring one in. Round-trip through `.clp` with
        // the opacity binding spliced into the param's binding list.
        let mut file = m.flatten().unwrap();
        let group_index = file
            .doc
            .nodes
            .iter()
            .position(|n| n.name == "lattice")
            .expect("the mesh group is in the arena") as u32;
        file.doc.params[0]
            .bindings
            .push(crate::formats::clp::ClpBinding {
                node: group_index,
                interpolate_mode: crate::params::InterpolateMode::Linear,
                values: crate::formats::clp::ClpBindingValues::Opacity(Default::default()),
            });
        let m = Model::from_clp_file(&file).unwrap();

        let warnings = m.check();
        let w = warnings
            .iter()
            .find(|w| w.message.contains("has no colour"))
            .expect("a colour binding on a mesh group is flagged");
        assert!(w.message.contains("opacity"), "{}", w.message);
    }
}
