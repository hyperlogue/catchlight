//! Lints that surface model problems an editor (or an agent) can self-correct on.
//! These are warnings, not errors, and most are cosmetic. Four are not. The
//! first two describe states no edit and no file can produce any more; they
//! are kept as a second line, so that a model which somehow reaches one says
//! so here rather than only when a save fails.
//!
//! - A **colour binding on a mesh group** cannot be loaded (a mesh group is
//!   never drawn, so it has no colour for the binding to fold into).
//!   [`Model::add_binding`] refuses to author one, the `.clm` reader refuses
//!   a file carrying one, and [`Model::update_node_kind`] refuses the kind
//!   swap that used to turn a bound part into one.
//! - A **weld whose end is no longer a part**, or one **pairing a slot its
//!   part no longer carries**, cannot be written at all:
//!   [`Model::to_clm_file`] refuses the first and the reader refuses the
//!   second. The slot and weld methods keep both out, and
//!   [`Model::update_node_kind`] refuses to swap a welded part's kind or
//!   replace its slots.
//! - A **weld with no pairs** saves and loads and joins nothing: it records
//!   two parts and holds neither of them to the other.
//! - A **weld over an unfilled slot** saves and loads; it just does not hold
//!   that pair together. This is the state a mesh edit leaves behind, and the
//!   one an editor should keep in front of the author until it is repaired.

use crate::formats::clm::ClmBindingValues;

use super::*;

#[derive(Debug, Clone)]
pub struct CheckWarning {
    /// The node the warning is about, if any.
    pub node: Option<NodeId>,
    pub message: String,
}

impl Model {
    /// Walk the model and report likely-unintended states: parts that render to
    /// nothing, malformed meshes, spines with no bend targets, and bindings
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
                // Only an entirely rigid spine: one that reads some of its
                // links and not others is an ordinary rig, not a mistake.
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
                        "the weld between node {:?} and node {:?} pairs no slots, so it holds \
                         nothing together",
                        ends[0].as_str(),
                        ends[1].as_str(),
                    ),
                });
            }
            let slots = ends.map(|end| self.slots(end));
            for end in ends {
                // In a fragment, an end on a node that is not here is a weld
                // into the base: a requirement, not a broken weld.
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

    /// A spine that reads nothing at all is a rig the author has not
    /// finished; one link short of finished is not, so the lint has to tell
    /// the two apart.
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

        // One link hooked up is a rig, not a mistake.
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

    /// Swapping a welded part's kind was the one edit that could strand a
    /// weld, and a stranded weld stops the model from saving at all. The swap
    /// is refused now, so the state is built by hand: the lint and the writer
    /// still have to say so if a model is ever in it.
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

    /// The editor refuses to author a colour binding on a mesh group, the
    /// `.clm` reader refuses a file carrying one, and `update_node_kind`
    /// refuses to turn a legitimately bound part *into* a mesh group. A model
    /// that reached that state anyway could not be loaded again, so `check`
    /// still has to say so.
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

        // ...and so does `update_node_kind`, which used to be the way in: a
        // part that legitimately carries one becoming a mesh group
        // afterwards. The state is built by hand to keep the lint honest.
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
