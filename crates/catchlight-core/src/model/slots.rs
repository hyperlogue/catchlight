//! Vertex slots on parts, and the welds that pair them across parts.

use super::*;

pub const DEFAULT_SLOT_WEIGHT: f32 = 0.5;

/// A named handle on a part, filled by one of its vertices or unfilled until
/// the part's author refills it after a re-mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub(super) id: SlotId,
    pub(super) vertex: Option<u32>,
}

impl Slot {
    pub fn id(&self) -> &SlotId {
        &self.id
    }

    pub fn vertex(&self) -> Option<u32> {
        self.vertex
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SlotPair {
    pub a: SlotId,
    pub b: SlotId,
    /// A's share of the meeting point, `0..=1`: 1.0 pins A, 0.5 meets midway.
    pub weight: f32,
}

/// Pairs are pulled to a weighted meeting point after every other deformation.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelWeld {
    pub(super) a: NodeId,
    pub(super) b: NodeId,
    pub(super) pairs: Arc<Vec<SlotPair>>,
}

impl ModelWeld {
    pub fn new(a: NodeId, b: NodeId, pairs: Vec<SlotPair>) -> Self {
        Self {
            a,
            b,
            pairs: Arc::new(pairs),
        }
    }

    pub fn a(&self) -> &NodeId {
        &self.a
    }

    pub fn b(&self) -> &NodeId {
        &self.b
    }

    pub fn pairs(&self) -> &[SlotPair] {
        &self.pairs
    }

    /// Pairs with an unfilled slot on either end are skipped.
    pub fn resolve(&self, model: &Model) -> Vec<ModelWeldPair> {
        let (Some(a), Some(b)) = (model.slots(&self.a), model.slots(&self.b)) else {
            return Vec::new();
        };
        fn table(slots: &[Slot]) -> HashMap<&SlotId, Option<u32>> {
            slots.iter().map(|s| (&s.id, s.vertex)).collect()
        }
        let (a, b) = (table(a), table(b));
        let find = |slots: &HashMap<&SlotId, Option<u32>>, id: &SlotId| *slots.get(id)?;
        self.pairs
            .iter()
            .filter_map(|pair| {
                Some(ModelWeldPair {
                    a_vert: find(&a, &pair.a)?,
                    b_vert: find(&b, &pair.b)?,
                    weight: pair.weight,
                })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelWeldPair {
    /// Vertex indices into each part's mesh.
    pub a_vert: u32,
    pub b_vert: u32,
    /// A's share of the meeting point, `0..=1`: 1.0 pins A, 0.5 meets midway.
    pub weight: f32,
}

impl Model {
    pub fn slots(&self, id: &NodeId) -> Option<&[Slot]> {
        match &self.node(id)?.kind {
            ModelNodeKind::Part(p) => Some(p.slots()),
            _ => None,
        }
    }

    pub fn slot(&self, id: &NodeId, slot: &SlotId) -> Option<&Slot> {
        self.slots(id)?.iter().find(|s| &s.id == slot)
    }

    pub fn unfilled_slots(&self) -> Vec<(NodeId, SlotId)> {
        let mut out = Vec::new();
        for id in self.nodes_in_order() {
            let Some(slots) = self.slots(&id) else {
                continue;
            };
            for slot in slots {
                if slot.vertex.is_none() {
                    out.push((id.clone(), slot.id.clone()));
                }
            }
        }
        out
    }

    pub fn slot_add(&mut self, id: &NodeId, slot: SlotId) -> Result<(), ModelError> {
        let part = self.part_mut(id)?;
        if part.slots.iter().any(|s| s.id == slot) {
            return Err(ModelError::DuplicateSlot(slot.to_string()));
        }
        Arc::make_mut(&mut part.slots).push(Slot {
            id: slot,
            vertex: None,
        });
        self.bump();
        Ok(())
    }

    /// Generated Id: `slot-<8 hex>`, re-drawn until free on this part.
    pub fn slot_add_generated(
        &mut self,
        id: &NodeId,
        hex: &mut impl HexSource,
    ) -> Result<SlotId, ModelError> {
        self.part_mut(id)?;
        let slot = self.mint(SlotId::generate, |m, s| m.slot(id, s).is_some(), hex)?;
        self.slot_add(id, slot.clone())?;
        Ok(slot)
    }

    /// Rewrites every weld pair naming the slot; no alias is kept.
    pub fn rename_slot(
        &mut self,
        id: &NodeId,
        from: &SlotId,
        to: SlotId,
    ) -> Result<(), ModelError> {
        let part = self.part_mut(id)?;
        let Some(at) = part.slots.iter().position(|s| &s.id == from) else {
            return Err(ModelError::UnknownSlot);
        };
        if &to != from && part.slots.iter().any(|s| s.id == to) {
            return Err(ModelError::DuplicateSlot(to.to_string()));
        }
        Arc::make_mut(&mut part.slots)[at].id = to.clone();
        for w in &mut self.welds {
            let (renames_a, renames_b) = (&w.a == id, &w.b == id);
            if !renames_a && !renames_b {
                continue;
            }
            for pair in Arc::make_mut(&mut w.pairs) {
                if renames_a && &pair.a == from {
                    pair.a = to.clone();
                }
                if renames_b && &pair.b == from {
                    pair.b = to.clone();
                }
            }
        }
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::MotionResponse(r) = &mut node.kind {
                for t in &mut r.inputs {
                    if let crate::ResponseSource::PartSlot { part, slot, .. } = &mut t.source
                        && part == id
                        && slot == from
                    {
                        *slot = to.clone();
                    }
                }
            }
        }
        self.extensions.rewrite(|r| match r {
            IdRef::Slot(part, slot) if part == id && slot == from => {
                Some(IdRef::Slot(part.clone(), to.clone()))
            }
            _ => None,
        });
        self.bump();
        Ok(())
    }

    pub fn slot_fill(&mut self, id: &NodeId, slot: &SlotId, vertex: u32) -> Result<(), ModelError> {
        let vertices = match self.node(id).map(|n| &n.kind) {
            Some(ModelNodeKind::Part(p)) => p.mesh().vertex_count(),
            Some(_) => return Err(ModelError::NotAPart),
            None => return Err(ModelError::UnknownNode),
        };
        if vertex as usize >= vertices {
            return Err(ModelError::IndexOutOfRange);
        }
        self.slot_mut(id, slot)?.vertex = Some(vertex);
        self.bump();
        Ok(())
    }

    pub fn slot_clear(&mut self, id: &NodeId, slot: &SlotId) -> Result<(), ModelError> {
        self.slot_mut(id, slot)?.vertex = None;
        self.bump();
        Ok(())
    }

    /// Drops the weld pairs naming it; a weld left with no pairs stays.
    pub fn slot_delete(&mut self, id: &NodeId, slot: &SlotId) -> Result<(), ModelError> {
        let part = self.part_mut(id)?;
        let Some(at) = part.slots.iter().position(|s| &s.id == slot) else {
            return Err(ModelError::UnknownSlot);
        };
        Arc::make_mut(&mut part.slots).remove(at);
        for w in &mut self.welds {
            let (drops_a, drops_b) = (&w.a == id, &w.b == id);
            if !drops_a && !drops_b {
                continue;
            }
            Arc::make_mut(&mut w.pairs)
                .retain(|p| !(drops_a && &p.a == slot) && !(drops_b && &p.b == slot));
        }
        for node in self.nodes.values_mut() {
            validate::drop_slot_inputs(&mut node.kind, |part, s| part == id && s == slot);
        }
        self.remove_in_extensions(|r| matches!(r, IdRef::Slot(part, s) if part == id && s == slot));
        self.bump();
        Ok(())
    }

    fn part_mut(&mut self, id: &NodeId) -> Result<&mut ModelPart, ModelError> {
        match self.nodes.get_mut(id) {
            Some(n) => match &mut n.kind {
                ModelNodeKind::Part(p) => Ok(p),
                _ => Err(ModelError::NotAPart),
            },
            None => Err(ModelError::UnknownNode),
        }
    }

    fn slot_mut(&mut self, id: &NodeId, slot: &SlotId) -> Result<&mut Slot, ModelError> {
        Arc::make_mut(&mut self.part_mut(id)?.slots)
            .iter_mut()
            .find(|s| &s.id == slot)
            .ok_or(ModelError::UnknownSlot)
    }

    pub fn welds(&self) -> &[ModelWeld] {
        &self.welds
    }

    /// `slot` is on `a`, and `weight` is `a`'s share whichever way the weld is
    /// stored (naming the parts reversed stores `1 - weight`), so it must be in
    /// `0..=1`.
    pub fn set_weld_slot_weight(
        &mut self,
        a: &NodeId,
        b: &NodeId,
        slot: &SlotId,
        weight: f32,
    ) -> Result<(), ModelError> {
        if !(0.0..=1.0).contains(&weight) {
            return Err(ModelError::WeldWeightOutOfRange);
        }
        let weld = self
            .welds
            .iter_mut()
            .find(|w| (&w.a == a && &w.b == b) || (&w.a == b && &w.b == a))
            .ok_or(ModelError::UnknownWeld)?;
        let forward = &weld.a == a;
        let share = if forward { weight } else { 1.0 - weight };
        let pairs = Arc::make_mut(&mut weld.pairs);
        let at = pairs
            .iter()
            .position(|p| if forward { &p.a } else { &p.b } == slot)
            .ok_or(ModelError::UnknownSlot)?;
        pairs[at].weight = share;
        self.bump();
        Ok(())
    }

    /// Unfilled slots are fine. In a fragment, an end the fragment does not carry
    /// is a weld into the base, checked by [`Model::install`].
    pub fn set_welds(&mut self, welds: Vec<ModelWeld>) -> Result<(), ModelError> {
        let fragment = self.is_fragment();
        let mut welded = validate::Welded::with_capacity(welds.len());
        for w in &welds {
            validate::weld(
                &w.a,
                &w.b,
                w.pairs.iter().map(|pair| (&pair.a, &pair.b, pair.weight)),
                &mut welded,
                |end| self.weld_end(end, fragment),
                |part, slot| self.slot(part, slot).is_some(),
            )
            .map_err(|fault| match fault {
                validate::WeldFault::SelfPaired => ModelError::WeldSelfPaired,
                validate::WeldFault::EndNotAPart { .. } => ModelError::WeldEndNotAPart,
                validate::WeldFault::EndMissing { .. } => ModelError::UnknownNode,
                validate::WeldFault::Duplicate => ModelError::DuplicateWeld,
                validate::WeldFault::WeightOutOfRange { .. } => ModelError::WeldWeightOutOfRange,
                validate::WeldFault::UnknownSlot { slot, .. } => {
                    ModelError::WeldUnknownSlot(slot.to_string())
                }
                validate::WeldFault::SlotPairedTwice { slot, .. } => {
                    ModelError::WeldSlotPairedTwice(slot.to_string())
                }
            })?;
        }
        self.welds = welds;
        self.bump();
        Ok(())
    }

    pub(super) fn weld_end(&self, end: &NodeId, fragment: bool) -> validate::WeldEnd {
        validate::WeldEnd::of(self.node(end).map(|n| &n.kind), fragment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SeededHex;
    use crate::model::tests::*;

    fn welded_fixture() -> Fixture {
        let mut r = fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        for (node, verts) in [(&part, [0u32, 1]), (&other, [2, 3])] {
            for (id, vertex) in [(slot("l"), verts[0]), (slot("r"), verts[1])] {
                r.model.slot_add(node, id.clone()).unwrap();
                r.model.slot_fill(node, &id, vertex).unwrap();
            }
        }
        r.model
            .set_welds(vec![ModelWeld::new(
                part,
                other,
                vec![pair("l", 1.0), pair("r", 0.25)],
            )])
            .unwrap();
        r
    }

    #[test]
    fn a_generated_slot_id_is_redrawn_until_it_is_free() {
        let mut r = fixture();
        let part = r.part.clone();
        // Pin the draw, then take its first two results by hand so the mint
        // has something to collide with.
        let taken = {
            let mut hex = SeededHex::new(11);
            [SlotId::generate(&mut hex), SlotId::generate(&mut hex)]
        };
        for id in &taken {
            r.model.slot_add(&part, id.clone()).unwrap();
        }

        let mut hex = SeededHex::new(11);
        let minted = r.model.slot_add_generated(&part, &mut hex).unwrap();

        assert!(!taken.contains(&minted), "{minted} collides");
        assert!(minted.as_str().starts_with("slot-"), "{minted}");
        assert!(r.model.slot(&part, &minted).is_some());
        // A node that cannot carry a slot is refused before anything is drawn.
        let root = r.root.clone();
        assert!(matches!(
            r.model.slot_add_generated(&root, &mut hex),
            Err(ModelError::NotAPart)
        ));
    }

    #[test]
    fn a_slot_id_is_free_on_every_other_part() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());

        r.model.slot_add(&part, slot("m")).unwrap();
        assert!(
            r.model.slot(&other, &slot("m")).is_none(),
            "an edit to one part's slots reaches no other part",
        );
        assert_eq!(
            r.model.welds()[0].pairs().len(),
            2,
            "and pairs nothing on its own",
        );
        r.model.slot_add(&other, slot("m")).unwrap();
        assert!(r.model.slot(&other, &slot("m")).is_some());
    }

    #[test]
    fn renaming_a_slot_carries_the_weld_pairs_that_named_it() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        let before = r.model.welds()[0].resolve(&r.model);

        r.model
            .rename_slot(&part, &slot("l"), slot("neck"))
            .unwrap();

        assert!(r.model.slot(&part, &slot("l")).is_none());
        assert!(r.model.slot(&part, &slot("neck")).is_some());
        assert_eq!(r.model.welds()[0].pairs()[0].a, slot("neck"));
        assert_eq!(
            r.model.welds()[0].pairs()[0].b,
            slot("l"),
            "the far part's slot is not this rename's business",
        );
        assert!(
            r.model.slot(&other, &slot("l")).is_some(),
            "and the far part keeps the slot it had",
        );
        assert_eq!(
            r.model.welds()[0].resolve(&r.model),
            before,
            "the weld solves exactly what it did before",
        );

        // The refusals: a slot that is not there, and a name that is.
        assert!(matches!(
            r.model.rename_slot(&part, &slot("l"), slot("x")),
            Err(ModelError::UnknownSlot)
        ));
        assert!(matches!(
            r.model.rename_slot(&part, &slot("neck"), slot("r")),
            Err(ModelError::DuplicateSlot(id)) if id == "r"
        ));
        r.model
            .rename_slot(&part, &slot("neck"), slot("neck"))
            .unwrap();
    }

    #[test]
    fn a_slot_weight_is_a_share_of_the_end_it_is_named_against() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());

        r.model
            .set_weld_slot_weight(&part, &other, &slot("l"), 0.25)
            .unwrap();
        assert_eq!(r.model.welds()[0].pairs()[0].weight, 0.25);
        assert_eq!(r.model.welds()[0].pairs()[1].weight, 0.25);

        // Named the other way round, 0.25 is B's share, so A's is 0.75 — and
        // `slot` is read on the part named first, which is now `other`.
        r.model
            .set_weld_slot_weight(&other, &part, &slot("l"), 0.25)
            .unwrap();
        assert_eq!(r.model.welds()[0].pairs()[0].weight, 0.75);

        assert!(matches!(
            r.model
                .set_weld_slot_weight(&part, &other, &slot("gone"), 0.5),
            Err(ModelError::UnknownSlot)
        ));
        let root = r.root.clone();
        assert!(matches!(
            r.model.set_weld_slot_weight(&part, &root, &slot("l"), 0.5),
            Err(ModelError::UnknownWeld)
        ));
        assert!(
            matches!(
                r.model.set_weld_slot_weight(&part, &other, &slot("l"), 1.5),
                Err(ModelError::WeldWeightOutOfRange)
            ),
            "a share outside 0..=1 has no meaning to flip",
        );
    }

    #[test]
    fn re_meshing_a_part_empties_its_slots_and_reports_them() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());
        assert_eq!(
            r.model.welds()[0].resolve(&r.model),
            vec![
                ModelWeldPair {
                    a_vert: 0,
                    b_vert: 2,
                    weight: 1.0
                },
                ModelWeldPair {
                    a_vert: 1,
                    b_vert: 3,
                    weight: 0.25
                },
            ]
        );

        let emptied = r.model.set_node_mesh(&part, quad()).unwrap();
        assert_eq!(emptied, vec![slot("l"), slot("r")]);
        assert_eq!(
            r.model.unfilled_slots(),
            vec![(part.clone(), slot("l")), (part.clone(), slot("r"))],
        );
        assert!(
            r.model
                .slots(&other)
                .unwrap()
                .iter()
                .all(|s| s.vertex().is_some()),
            "the far part's slots are none of this edit's business",
        );

        let weld = &r.model.welds()[0];
        assert_eq!(weld.pairs().len(), 2, "the weld itself is untouched");
        assert!(
            weld.resolve(&r.model).is_empty(),
            "an unfilled slot is skipped, not guessed at",
        );

        r.model.slot_fill(&part, &slot("r"), 3).unwrap();
        assert_eq!(
            r.model.welds()[0].resolve(&r.model),
            vec![ModelWeldPair {
                a_vert: 3,
                b_vert: 3,
                weight: 0.25
            }],
            "a half-repaired part solves the pairs it has",
        );
        assert!(r.model.to_clm_bytes().is_ok());
    }

    #[test]
    fn deleting_a_slot_drops_the_pairs_that_named_it() {
        let mut r = welded_fixture();
        let (part, other) = (r.part.clone(), r.other.clone());

        r.model.slot_delete(&part, &slot("l")).unwrap();

        assert!(r.model.slot(&part, &slot("l")).is_none());
        assert!(
            r.model.slot(&other, &slot("l")).is_some(),
            "the far part keeps its own slot",
        );
        let pairs = r.model.welds()[0].pairs();
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            (pairs[0].a.clone(), pairs[0].b.clone()),
            (slot("r"), slot("r"))
        );
        assert!(r.model.to_clm_bytes().is_ok());

        r.model.slot_delete(&other, &slot("r")).unwrap();
        assert_eq!(r.model.welds().len(), 1, "a weld with no pairs stays");
        assert!(r.model.welds()[0].pairs().is_empty());
    }

    #[test]
    fn slot_edits_are_checked_before_they_land() {
        let mut r = fixture();
        let (part, composite) = (r.part.clone(), r.composite.clone());
        let ghost = NodeId::new("ghost").unwrap();

        assert!(matches!(
            r.model.slot_add(&composite, slot("l")),
            Err(ModelError::NotAPart)
        ));
        assert!(matches!(
            r.model.slot_add(&ghost, slot("l")),
            Err(ModelError::UnknownNode)
        ));

        r.model.slot_add(&part, slot("l")).unwrap();
        assert!(matches!(
            r.model.slot_add(&part, slot("l")),
            Err(ModelError::DuplicateSlot(id)) if id == "l"
        ));
        assert!(matches!(
            r.model.slot_fill(&part, &slot("ghost"), 0),
            Err(ModelError::UnknownSlot)
        ));
        assert!(
            matches!(
                r.model.slot_fill(&part, &slot("l"), 4),
                Err(ModelError::IndexOutOfRange)
            ),
            "the fixture's part draws a quad, so vertex 4 is off the end",
        );

        r.model.slot_fill(&part, &slot("l"), 3).unwrap();
        let filled = |m: &Model| m.slot(&part, &slot("l")).unwrap().vertex();
        assert_eq!(filled(&r.model), Some(3));
        r.model.slot_clear(&part, &slot("l")).unwrap();
        assert_eq!(filled(&r.model), None);

        assert!(matches!(
            r.model.slot_delete(&part, &slot("ghost")),
            Err(ModelError::UnknownSlot)
        ));
        assert!(
            r.model.slots(&composite).is_none(),
            "only a part carries slots",
        );
    }

    #[test]
    fn a_weld_joins_two_parts_and_pairs_slots_they_carry() {
        let mut r = welded_fixture();
        let (part, other, composite) = (r.part.clone(), r.other.clone(), r.composite.clone());
        let weld = |pairs: Vec<SlotPair>| vec![ModelWeld::new(part.clone(), other.clone(), pairs)];

        assert!(
            matches!(
                r.model
                    .set_welds(vec![ModelWeld::new(part.clone(), part.clone(), Vec::new())]),
                Err(ModelError::WeldSelfPaired)
            ),
            "a part is not welded to itself",
        );
        assert!(
            matches!(
                r.model
                    .set_welds(vec![ModelWeld::new(part.clone(), composite, Vec::new())]),
                Err(ModelError::WeldEndNotAPart)
            ),
            "only a part carries slots to pair",
        );
        assert!(matches!(
            r.model.set_welds(vec![ModelWeld::new(
                part.clone(),
                NodeId::new("ghost").unwrap(),
                Vec::new()
            )]),
            Err(ModelError::UnknownNode)
        ));
        assert!(
            matches!(
                r.model.set_welds(vec![
                    ModelWeld::new(part.clone(), other.clone(), Vec::new()),
                    ModelWeld::new(other.clone(), part.clone(), Vec::new()),
                ]),
                Err(ModelError::DuplicateWeld)
            ),
            "one record per unordered part pair, found in either order",
        );
        assert!(
            matches!(
                r.model.set_welds(weld(vec![pair("ghost", 0.5)])),
                Err(ModelError::WeldUnknownSlot(id)) if id == "ghost"
            ),
            "a pair naming a slot its part does not carry",
        );
        assert!(
            matches!(
                r.model.set_welds(weld(vec![pair("l", 0.5), pair("l", 0.25)])),
                Err(ModelError::WeldSlotPairedTwice(id)) if id == "l"
            ),
            "one slot, two pairs",
        );
        assert!(
            matches!(
                r.model.set_welds(weld(vec![pair("l", 1.5)])),
                Err(ModelError::WeldWeightOutOfRange)
            ),
            "a weight is a share of a meeting point",
        );
        assert_eq!(
            r.model.welds().len(),
            1,
            "a refused weld list leaves the old one in place",
        );
        assert_eq!(r.model.welds()[0].pairs().len(), 2);

        // A weld may pair slots with different Ids on the two sides, and one
        // side's slot may be left out of the pairing altogether.
        r.model.slot_add(&part, slot("m")).unwrap();
        r.model
            .set_welds(weld(vec![SlotPair {
                a: slot("m"),
                b: slot("r"),
                weight: 0.5,
            }]))
            .unwrap();
        assert!(r.model.to_clm_bytes().is_ok());

        let other_clone = other.clone();
        r.model.delete_node(&other_clone).unwrap();
        assert!(
            r.model.welds().is_empty(),
            "deleting a node takes the welds that named it, the way it takes \
             the masks that named it",
        );
    }
}
