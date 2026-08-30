//! Lints that surface rig problems an editor (or an agent) can self-correct on.
//! These are warnings, not errors, and most are cosmetic. Three are not:
//!
//! - A **colour binding on a mesh group** flattens to a file `catchlight_core`
//!   then refuses to load (a mesh group is never drawn, so it has no colour
//!   for the binding to fold into). [`Model::add_binding`] refuses to author
//!   one, so only a file written by an older tool can still carry it.
//! - A **weld naming a seam its part no longer carries**, or **two seams
//!   holding different slots**, cannot be written at all: [`Model::to_clm_file`]
//!   refuses. The seam methods keep both out — but [`Model::update_node`] can
//!   swap a welded part's whole kind, which takes its seams with it.
//! - A **weld over an unfilled slot** saves and loads; it just does not hold
//!   that slot pair together. This is the state a mesh edit leaves behind, and
//!   the one an editor should keep in front of the author until it is
//!   repaired.

use crate::formats::clm::{ClmBindingValues, ClmIndices};

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
                        ClmIndices::U16(v) => v.len() / 3,
                        ClmIndices::U32(v) => v.len() / 3,
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
                ModelNodeKind::SimplePhysics(ph)
                    if ph.target_params().iter().all(Option::is_none) =>
                {
                    out.push(warn(
                        id,
                        format!("physics node {:?} drives no target param", n.name.as_str()),
                    ));
                }
                _ => {}
            }
        }

        for b in self.bindings() {
            let Some(p) = self.param(b.params().x()) else {
                continue;
            };
            let Ok((w, h)) = self.binding_grid(b.key()) else {
                continue;
            };
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

        let fragment = self.is_fragment();
        for weld in self.welds() {
            let ends = [weld.a(), weld.b()];
            let (Some(a), Some(b)) = (
                self.seam(&ends[0].0, &ends[0].1),
                self.seam(&ends[1].0, &ends[1].1),
            ) else {
                for end in ends {
                    // In a fragment, an end on a node that is not here is a
                    // weld into the base: a requirement, not a broken weld.
                    if fragment && self.node(&end.0).is_none() {
                        continue;
                    }
                    if self.seam(&end.0, &end.1).is_none() {
                        out.push(CheckWarning {
                            node: Some(end.0.clone()),
                            message: format!(
                                "a weld names seam {:?} on node {:?}, which carries no such \
                                 seam (the model no longer saves)",
                                end.1.as_str(),
                                end.0.as_str(),
                            ),
                        });
                    }
                }
                continue;
            };
            if a.slots().len() != b.slots().len()
                || !a.slots().iter().all(|s| b.slot(s.id()).is_some())
            {
                out.push(CheckWarning {
                    node: Some(ends[0].0.clone()),
                    message: format!(
                        "the weld between seam {:?} and seam {:?} pairs two seams holding \
                         different slots (the model no longer saves)",
                        ends[0].1.as_str(),
                        ends[1].1.as_str(),
                    ),
                });
                continue;
            }
            for (end, seam) in ends.into_iter().zip([a, b]) {
                for slot in seam.slots().iter().filter(|s| s.vertex().is_none()) {
                    out.push(CheckWarning {
                        node: Some(end.0.clone()),
                        message: format!(
                            "slot {:?} of seam {:?} on node {:?} is unfilled, so the weld \
                             skips it — a mesh edit empties a part's slots and they have to \
                             be refilled",
                            slot.id().as_str(),
                            end.1.as_str(),
                            end.0.as_str(),
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

fn cells_outside(values: &ClmBindingValues, w: u32, h: u32) -> usize {
    use ClmBindingValues::*;
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
    use crate::formats::clm::{ClmIndices, ClmMesh};
    use crate::id::{Name, NodeId, SeamId, SeededHex, SlotId};
    use crate::model::binding::ScalarTarget;
    use crate::model::*;

    /// Two welded quads under the root, seams `collar` and `hem`, slots `l`
    /// and `r`, everything filled.
    fn welded() -> (Model, NodeId, NodeId) {
        let mut hex = SeededHex::new(4);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let mut part = |m: &mut Model, name: &str, seam: &str, verts: [u32; 2]| {
            let id = m
                .add_node(
                    &root,
                    ModelNode::new(name, ModelNodeKind::Part(ModelPart::new(quad()))),
                    &mut hex,
                )
                .unwrap();
            m.seam_add(&id, SeamId::new(seam).unwrap()).unwrap();
            for (slot, vertex) in ["l", "r"].into_iter().zip(verts) {
                let (seam, slot) = (SeamId::new(seam).unwrap(), SlotId::new(slot).unwrap());
                m.slot_add(&id, &seam, slot.clone()).unwrap();
                m.slot_fill(&id, &seam, &slot, vertex).unwrap();
            }
            id
        };
        let upper = part(&mut m, "upper", "collar", [0, 1]);
        let lower = part(&mut m, "lower", "hem", [2, 3]);
        m.set_welds(vec![ModelWeld::new(
            (upper.clone(), SeamId::new("collar").unwrap()),
            (lower.clone(), SeamId::new("hem").unwrap()),
            vec![
                (SlotId::new("l").unwrap(), 1.0),
                (SlotId::new("r").unwrap(), 0.5),
            ],
        )])
        .unwrap();
        (m, upper, lower)
    }

    fn quad() -> ClmMesh {
        ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0; 8],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    /// The state a mesh edit leaves behind: the weld still saves and loads, it
    /// just holds nothing, and the author has to be told.
    #[test]
    fn check_flags_a_weld_whose_slot_is_unfilled() {
        let (mut m, upper, _) = welded();
        assert!(!m.check().iter().any(|w| w.message.contains("unfilled")));

        m.set_node_mesh(&upper, quad()).unwrap();
        let unfilled: Vec<CheckWarning> = m
            .check()
            .into_iter()
            .filter(|w| w.message.contains("is unfilled"))
            .collect();
        assert_eq!(unfilled.len(), 2, "both of the re-meshed part's slots");
        assert!(unfilled.iter().all(|w| w.node.as_ref() == Some(&upper)));
        assert!(m.to_clm_bytes().is_ok(), "an unfilled slot still saves");
    }

    /// Swapping a welded part's kind is the one edit that can strand a weld,
    /// and a stranded weld stops the model from saving at all.
    #[test]
    fn check_flags_a_weld_whose_seam_is_gone() {
        let (mut m, upper, _) = welded();
        m.update_node(&upper, |n| n.kind = ModelNodeKind::Group)
            .unwrap();

        let w = m
            .check()
            .into_iter()
            .find(|w| w.message.contains("carries no such seam"))
            .expect("a weld naming a seam that is gone is flagged");
        assert_eq!(w.node.as_ref(), Some(&upper));
        assert!(w.message.contains("collar"), "{}", w.message);
        assert!(matches!(m.to_clm_file(), Err(ModelError::UnknownSeam)));
    }

    /// The editor refuses to author a colour binding on a mesh group, but a
    /// `.clm` written by an older tool can carry one — and the runtime will not
    /// load that model back, so `check` has to say so.
    #[test]
    fn check_flags_a_color_binding_on_a_mesh_group() {
        let mut hex = SeededHex::new(9);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let group = m
            .add_node(
                &root,
                ModelNode::new(
                    "lattice",
                    ModelNodeKind::MeshGroup(ModelMeshGroup::new(ClmMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        let param = m
            .add_param(
                ModelParam::new(Name::truncated("shade"), 0.0, 1.0, 0.0),
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

        // ...so only a file can bring one in. Round-trip through `.clm` with
        // the opacity binding spliced into the param's binding list.
        let mut file = m.to_legacy().unwrap();
        let group_index = file
            .doc
            .nodes
            .iter()
            .position(|n| n.name == "lattice")
            .expect("the mesh group is in the arena") as u32;
        file.doc.params[0]
            .bindings
            .push(crate::formats::legacy::LegacyBinding {
                node: group_index,
                interpolate_mode: crate::interpolate::InterpolateMode::Linear,
                values: crate::formats::clm::ClmBindingValues::Opacity(Default::default()),
            });
        let m = Model::from_legacy(&file).unwrap();

        let warnings = m.check();
        let w = warnings
            .iter()
            .find(|w| w.message.contains("has no colour"))
            .expect("a colour binding on a mesh group is flagged");
        assert!(w.message.contains("opacity"), "{}", w.message);
    }
}
