//! Lints that surface model problems an editor (or an agent) can self-correct on.
//! Warnings, not errors; some describe states no edit or file can produce
//! and are kept as a second line of defence.

use crate::formats::clm::ClmBindingValues;

use super::*;

#[derive(Debug, Clone)]
pub struct CheckWarning {
    pub node: Option<NodeId>,
    pub message: String,
}

impl Model {
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
                // Only an entirely rigid spine; a partly read one is an ordinary rig.
                ModelNodeKind::Spine(spine) if spine.targets().iter().all(Option::is_none) => {
                    out.push(warn(
                        id,
                        format!("spine {:?} reads no bend param", n.name.as_str()),
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
            if let Some(Err(validate::BindingFault::ColorOnMeshGroup(t))) = self
                .node(b.node())
                .map(|n| validate::binding_target(&n.kind, b.target()))
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
                        "the weld between node {:?} and node {:?} pairs no vertex slots, so it holds \
                         nothing together",
                        ends[0].as_str(),
                        ends[1].as_str(),
                    ),
                });
            }
            let slots = ends.map(|end| self.slots(end));
            for end in ends {
                // In a fragment, a missing end is a weld into the base.
                if !self.weld_end(end, fragment).is_sound() {
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
                                "a weld pairs vertex slot {:?} on node {:?}, which carries no such \
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
                                "vertex slot {:?} on node {:?} is unfilled, so the weld skips it — a \
                                 mesh edit empties a part's slots and they have to be refilled",
                                slot.as_str(),
                                end.as_str(),
                            ),
                        });
                    }
                }
            }
        }

        for found in self.extension_warnings() {
            out.push(CheckWarning {
                node: found.warning.subjects.iter().find_map(|r| match r {
                    IdRef::Node(id) | IdRef::Slot(id, _) => Some(id.clone()),
                    _ => None,
                }),
                message: format!(
                    "extension {:?}: {}",
                    found.key.as_str(),
                    found.warning.message
                ),
            });
        }
        out
    }
}

/// One finding about one extension, from [`Model::extension_warnings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionWarning {
    pub key: ExtensionKey,
    pub warning: crate::extension::Warning,
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
        ZOrder(c) | TransformTX(c) | TransformTY(c) | TransformTZ(c) | TransformSX(c)
        | TransformSY(c) | TransformRX(c) | TransformRY(c) | TransformRZ(c) | Opacity(c)
        | TintR(c) | TintG(c) | TintB(c) | ScreenTintR(c) | ScreenTintG(c) | ScreenTintB(c) => {
            c.cells.iter().filter(|c| c.x >= w || c.y >= h).count()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::formats::clm::{ClmIndices, ClmMesh};
    use crate::id::{Name, NodeId, SeededHex, SlotId};
    use crate::model::binding::ScalarTarget;
    use crate::model::*;

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

    #[test]
    fn check_flags_a_spine_that_reads_nothing() {
        let mut hex = SeededHex::new(6);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let param = m
            .add_param(
                ModelParam::new(Name::truncated("bend"), -1.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        let spine = m
            .add_node(
                &root,
                ModelNode::new(
                    "hair",
                    ModelNodeKind::Spine(crate::model::ModelSpine::new(vec![
                        [0.0, -50.0],
                        [0.0, -100.0],
                    ])),
                ),
                &mut hex,
            )
            .unwrap();

        let unhooked: Vec<CheckWarning> = m
            .check()
            .into_iter()
            .filter(|w| w.message.contains("reads no bend param"))
            .collect();
        assert_eq!(unhooked.len(), 1, "the spine names no param at all");
        assert_eq!(unhooked[0].node.as_ref(), Some(&spine));

        m.set_spine_targets(&spine, vec![Some(param), None])
            .unwrap();
        assert!(
            !m.check()
                .iter()
                .any(|w| w.message.contains("reads no bend param")),
            "a partly-read spine is not flagged"
        );
    }

    fn quad() -> ClmMesh {
        ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0; 8],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

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

    /// The swap that could strand a weld is refused, so the state is built by
    /// hand.
    #[test]
    fn check_flags_a_weld_whose_end_is_no_longer_a_part() {
        let (mut m, upper, _) = welded();
        assert!(matches!(
            m.update_node_kind(&upper, |kind| *kind = ModelNodeKind::Group),
            Err(ModelError::KindEdit("a node's kind"))
        ));
        assert!(
            m.to_clm_file().is_ok(),
            "a refused swap leaves the weld sound"
        );
        m.nodes.get_mut(&upper).unwrap().kind = ModelNodeKind::Group;

        let w = m
            .check()
            .into_iter()
            .find(|w| w.message.contains("no longer a part"))
            .expect("a weld naming a node that is no longer a part is flagged");
        assert_eq!(w.node.as_ref(), Some(&upper));
        assert!(matches!(m.to_clm_file(), Err(ModelError::WeldEndNotAPart)));
    }

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
                .any(|w| w.message.contains("pairs no vertex slots")),
            "a weld holding nothing is flagged"
        );
        assert!(m.to_clm_bytes().is_ok(), "and it still saves");
    }

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
        assert!(
            !m.check()
                .iter()
                .any(|w| w.message.contains("has no colour"))
        );

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
        assert!(matches!(
            m.update_node_kind(&part, |kind| {
                *kind = ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad()))
            }),
            Err(ModelError::KindEdit("a node's kind"))
        ));
        m.nodes.get_mut(&part).unwrap().kind =
            ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad()));
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
