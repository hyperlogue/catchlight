//! Lints that surface model problems an editor (or an agent) can self-correct on.
//! These are warnings, not errors, and most are cosmetic. Three are not:
//!
//! - A **colour binding on a mesh group** cannot be written and cannot be
//!   loaded (a mesh group is never drawn, so it has no colour for the binding
//!   to fold into). [`Model::add_binding`] refuses to author one and the
//!   `.clm` reader refuses a file carrying one, so the one way in is
//!   [`Model::update_node`] swapping a bound node's whole kind.
//! - A **weld whose end is no longer a part**, or one **pairing a slot its
//!   part no longer carries**, cannot be written at all:
//!   [`Model::to_clm_file`] refuses the first and the reader refuses the
//!   second. The slot and weld methods keep both out — but
//!   [`Model::update_node`] can swap a welded part's whole kind, which takes
//!   its slots with it.
//! - A **weld with no pairs** saves and loads and joins nothing: it records
//!   two parts and holds neither of them to the other.
//! - A **weld over an unfilled slot** saves and loads; it just does not hold
//!   that pair together. This is the state a mesh edit leaves behind, and the
//!   one an editor should keep in front of the author until it is repaired.

use crate::formats::clm::ClmBindingValues;

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
                    let tris = mesh.triangle_count();
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
            if weld.pairs().is_empty() {
                out.push(CheckWarning {
                    node: Some(ends[0].clone()),
                    message: format!(
                        "the weld between node {:?} and node {:?} pairs no slots, so it holds \
                         nothing together",
                        ends[0].as_str(),
                        ends[1].as_str(),
                    ),
                });
            }
            let slots = ends.map(|end| self.slots(end));
            for (end, carried) in ends.into_iter().zip(slots) {
                // In a fragment, an end on a node that is not here is a weld
                // into the base: a requirement, not a broken weld.
                if carried.is_none() && !(fragment && self.node(end).is_none()) {
                    out.push(CheckWarning {
                        node: Some(end.clone()),
                        message: format!(
                            "a weld names node {:?}, which is no longer a part (the model no \
                             longer saves)",
                            end.as_str(),
                        ),
                    });
                }
            }
            for pair in weld.pairs() {
                for ((end, carried), slot) in ends.into_iter().zip(slots).zip([&pair.a, &pair.b]) {
                    let Some(carried) = carried else { continue };
                    let Some(held) = carried.iter().find(|s| s.id() == slot) else {
                        out.push(CheckWarning {
                            node: Some(end.clone()),
                            message: format!(
                                "a weld pairs slot {:?} on node {:?}, which carries no such \
                                 slot (the model no longer saves)",
                                slot.as_str(),
                                end.as_str(),
                            ),
                        });
                        continue;
                    };
                    if held.vertex().is_none() {
                        out.push(CheckWarning {
                            node: Some(end.clone()),
                            message: format!(
                                "slot {:?} on node {:?} is unfilled, so the weld skips it — a \
                                 mesh edit empties a part's slots and they have to be refilled",
                                slot.as_str(),
                                end.as_str(),
                            ),
                        });
                    }
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
    use crate::id::{Name, NodeId, SeededHex, SlotId};
    use crate::model::binding::ScalarTarget;
    use crate::model::*;

    /// Two welded quads under the root, slots `l` and `r` on each, everything
    /// filled.
    fn welded() -> (Model, NodeId, NodeId) {
        let mut hex = SeededHex::new(4);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let mut part = |m: &mut Model, name: &str, verts: [u32; 2]| {
            let id = m
                .add_node(
                    &root,
                    ModelNode::new(name, ModelNodeKind::Part(ModelPart::new(quad()))),
                    &mut hex,
                )
                .unwrap();
            for (slot, vertex) in ["l", "r"].into_iter().zip(verts) {
                let slot = SlotId::new(slot).unwrap();
                m.slot_add(&id, slot.clone()).unwrap();
                m.slot_fill(&id, &slot, vertex).unwrap();
            }
            id
        };
        let upper = part(&mut m, "upper", [0, 1]);
        let lower = part(&mut m, "lower", [2, 3]);
        let pair = |id: &str, weight: f32| SlotPair {
            a: SlotId::new(id).unwrap(),
            b: SlotId::new(id).unwrap(),
            weight,
        };
        m.set_welds(vec![ModelWeld::new(
            upper.clone(),
            lower.clone(),
            vec![pair("l", 1.0), pair("r", 0.5)],
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
    fn check_flags_a_weld_whose_end_is_no_longer_a_part() {
        let (mut m, upper, _) = welded();
        m.update_node(&upper, |n| n.kind = ModelNodeKind::Group)
            .unwrap();

        let w = m
            .check()
            .into_iter()
            .find(|w| w.message.contains("no longer a part"))
            .expect("a weld naming a node that is no longer a part is flagged");
        assert_eq!(w.node.as_ref(), Some(&upper));
        assert!(matches!(m.to_clm_file(), Err(ModelError::WeldEndNotAPart)));
    }

    /// A slot delete on one part takes the pairs that named it, and a weld
    /// that runs out of pairs stays — holding nothing, which `check` says.
    #[test]
    fn check_flags_a_weld_that_pairs_nothing() {
        let (mut m, upper, _) = welded();
        for slot in ["l", "r"] {
            m.slot_delete(&upper, &SlotId::new(slot).unwrap()).unwrap();
        }
        assert_eq!(m.welds().len(), 1, "the weld stays");
        assert!(m.welds()[0].pairs().is_empty());
        assert!(
            m.check()
                .iter()
                .any(|w| w.message.contains("pairs no slots")),
            "a weld holding nothing is flagged"
        );
        assert!(m.to_clm_bytes().is_ok(), "and it still saves");
    }

    /// The editor refuses to author a colour binding on a mesh group and the
    /// `.clm` reader refuses a file carrying one — but `update_node` can turn
    /// a legitimately bound part *into* a mesh group, and the model then
    /// cannot be saved at all, so `check` has to say so.
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

        // ...so the way in is a part that legitimately carries one becoming a
        // mesh group afterwards.
        let part = m
            .add_node(
                &root,
                ModelNode::new("skin", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut hex,
            )
            .unwrap();
        m.add_binding(&BindingKey::new(
            m.param_ids()[0].clone(),
            part.clone(),
            BindingTarget::Scalar(ScalarTarget::Opacity),
        ))
        .unwrap();
        m.update_node(&part, |n| {
            n.kind = ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad()))
        })
        .unwrap();
        assert!(
            m.to_clm_file().is_err() || Model::from_clm_bytes(&m.to_clm_bytes().unwrap()).is_err(),
            "a model in this state cannot make the round trip"
        );

        let warnings = m.check();
        let w = warnings
            .iter()
            .find(|w| w.message.contains("has no colour"))
            .expect("a colour binding on a mesh group is flagged");
        assert!(w.message.contains("opacity"), "{}", w.message);
    }
}
