//! A puppet: a model being animated.
//!
//! A [`Puppet`] holds everything animating a [`Model`] produces and a model
//! never does — the pose, the drivers' state and the evaluated frame — so one
//! model can back many puppets and posing one never touches the model.
//!
//! Invariants this module enforces:
//!
//! - **A tick is one pipeline, in one order.** [`Puppet::tick`] is: fold
//!   animations → pose the physics anchors and step the drivers → fold the
//!   bindings → compute transforms → run spines → run mesh groups → solve welds →
//!   combine deforms. The mesh-group pass is itself ordered, outer group
//!   first: each group shifts its `translate_children` targets and recomputes
//!   the globals under them before pushing its deform down, so an inner group
//!   inherits the outer one's same-frame warp. The code is an optimized form
//!   of exactly that, cached in two places, and the caching is where a bug
//!   hides: the anchor pre-pass is skipped unless the pose moved
//!   (`last_anchor_pose_generation`) and the whole fold is skipped when
//!   neither the pose nor the pre-pass touched anything
//!   (`last_tick_folded_param_generation`). A pre-pass that ran **forces** the
//!   final fold, because it reset colour and deactivated every deform stack.
//! - **An anchor carries the previous frame's `translate_children` shift.**
//!   The anchor pose is built before the mesh groups run, so the shift a group
//!   applies to a driver or to one of its ancestors is not there yet. Rather
//!   than run the mesh-group pass twice a frame, the pre-pass replays the shift
//!   the last one recorded (`Arena::apply_previous_tc_shifts`), so an anchor
//!   follows one frame late. It is also
//!   what makes the pre-pass skippable: see that method for the argument.
//! - **The generation gate is the only staleness check.** A puppet records
//!   `model.generation()` when it bakes; every method that takes a `&Model`
//!   compares it first and rebakes when it moved. Nothing else may assume the
//!   arena still matches the model — which is also what makes installing an
//!   addon between two frames cost one rebake and no caller changes. The
//!   counter says nothing about *which* model it counts, so a puppet also
//!   records [`Model::identity`] and `debug_assert!`s it on every call that
//!   takes a `&Model`: a puppet animates the model it was built from, and two
//!   models sitting at the same generation are otherwise indistinguishable.
//! - **A rebake carries the pose, the drivers and the scratch transforms, by
//!   Id.** Param values, driver contributions, every `SimplePhysics` runtime
//!   field, a particle chain's particles when the edit left its link count
//!   alone, and every [`ScratchTransform`] are saved
//!   against `ParamId` / `NodeId`, the arena is rebuilt, and they are put back
//!   where those Ids now live. Anything keyed by slot — every generation memo
//!   — is dropped, because the slots moved. A param or node the edit removed
//!   is dropped with it; a value for a param the edit has not added yet is
//!   kept aside and lands if it appears.
//! - **Hot paths address nodes and params by index, never by Id.** Ids are
//!   `Arc<str>` and comparing them is a string compare; a tick touches every
//!   node and every binding, so both are resolved to a `NodeIdx` / param slot
//!   once at bake and the loops index dense `Vec`s. Ids reappear only at the
//!   API edge, through `node_idx` / `node_id` and the pose methods.
//! - **Scratch state is the writer's, and only the writer ends it.** A
//!   scratch deform ([`Puppet::set_scratch_deform`]) and a scratch transform
//!   ([`Puppet::set_scratch_transform`]) are the two halves of an edit in
//!   progress: the fold re-derives everything else from the model each frame
//!   and would swallow a drag, so these are re-applied instead — the scratch
//!   transform right after the bindings fold, before the transform walk, so
//!   the frame moves the node's dependents too. **A scratch transform is
//!   absolute**, per field, `None` meaning "keep what the fold produced": the
//!   numbers a client previews are the numbers it commits. The two halves part
//!   company at a rebake — a deform is sized by the mesh the bake resolved and
//!   dies with it, while a transform is five scalars keyed by `NodeId` and
//!   survives, because the commit that ends a drag *is* a model edit and the
//!   preview has to outlive it.
//! - **A tick reports self-driven motion only.** [`Puppet::tick`] returns
//!   [`Motion`]: a pendulum away from the rest pose `settle_physics` places,
//!   a chain still moving, or an animation lane that wrote a param. Both
//!   drivers answer `is_at_rest` at `SETTLE_EPS_SQ`, the same epsilon
//!   `settle_physics` converges on; they differ in what they measure,
//!   because a sprung chain's arithmetic settles a hair off the pose it
//!   settles toward and it is stillness a caller is really asking about.
//!   A pose, scratch or model change is deliberately not motion — the caller
//!   made it and already knows to redraw. `Motion` is not `#[must_use]`: most
//!   callers tick for the frame, not for the answer.
//! - **`settle_physics` before the first render.** It iterates to the fixed
//!   point of "anchor → param value → transforms → anchor" so a freshly loaded
//!   model renders settled instead of swinging into place, and it leaves the
//!   puppet *unposed* — `tick` is what folds a renderable pose.
//!
//! Three things a puppet does not take from the model. **Textures**: nothing
//! here decodes an image, so a part carries its albedo as an index into
//! [`Model::texture_ids`] and the render cache resolves it. **Masks**: a baked
//! [`crate::components::Mask`] carries the `NodeIdx` the bake resolved the
//! model's mask source Id to, and a mask whose source the model does not carry
//! is dropped rather than baked as a dangling slot.
//! **Animations**: the puppet owns only the play state, and the clips are
//! installed rather than baked — [`Puppet::set_animations_from`] takes the
//! model's own, [`Puppet::set_animations`] takes a caller's, and a rebake
//! keeps whichever is loaded.

mod arena;
mod bake;
mod fold;

pub use arena::GlobalTransforms;

pub(crate) use arena::Arena;

use std::collections::{HashMap, HashSet};

use glam::{Mat4, Vec2, Vec3};

use crate::animation::AnimationPlayState;
use crate::components::{checked_affine_inverse, Node, NodeIdx, NodeKind};
use crate::deform::DeformSource;
use crate::formats::clm::ClmAnimation;
use crate::id::{NodeId, ParamId};
use crate::model::{BindingTarget, Model, Pose, ScalarTarget};
use crate::node::NodeTree;
use crate::physics::{ParticleChainData, SimplePhysicsData};

use bake::{Baked, BakedBinding, BakedParam};

/// Squared anchor movement below which `settle_physics` calls a driver
/// converged. Anchors are in model units, so this is a sub-millipixel
/// displacement on models whose coordinates run to the thousands.
const SETTLE_EPS_SQ: f32 = 1e-6;

/// One driver's weighted claim on a param. `weight` is authority, at or above
/// zero: 0 asserts nothing, 1 fully specifies the value, and larger weights
/// only matter relative to whatever else claims the param.
#[derive(Debug, Clone, Copy)]
struct Contribution {
    slot: u32,
    source: NodeIdx,
    value: f32,
    weight: f32,
    /// A chain's bend limit constrains the final value, even at zero weight.
    limit: Option<f32>,
}

/// Fold weighted claims against the value the caller posed.
///
/// The rule is an **order-independent** weighted mean, deliberately not a
/// sequential source-over: there is no principled ordering between "the caller
/// posed this before the tick" and "a driver wrote it during the tick", so
/// inventing one would bury a semantic choice in call order. A caller that
/// wants ordered compositing resolves it on its own side and poses the result.
///
/// `claims` is one slot's row of [`Puppet::slot_claims`], so this reads the
/// few entries that target the slot rather than filtering every claim in the
/// puppet. Order-independent is the rule and not the arithmetic: the row is
/// in the order the claims were made, which is the order the filter used to
/// produce, so the sum lands on the same bits it always did.
///
/// **Bend limits constrain the combined value, after every claim and the
/// pose have been blended.** A per-driver clamp would let another claim or
/// the pose move the result past the wall. Where links share a param, its
/// value must satisfy every link's limit, so the smallest wins. A limit is
/// independent of authority: weight zero adds no motion but keeps the wall.
fn resolve_contributions(entries: &[Contribution], claims: &[u32], base: f32) -> f32 {
    let mut total = 0.0;
    let mut sum = 0.0;
    let mut limit = f32::INFINITY;
    for &i in claims {
        let Some(e) = entries.get(i as usize) else {
            continue;
        };
        total += e.weight;
        sum += e.value * e.weight;
        if let Some(cap) = e.limit.filter(|l| l.is_finite() && *l > 0.0) {
            limit = limit.min(cap);
        }
    }
    let value = if total <= 0.0 {
        base
    } else if total < 1.0 {
        sum + base * (1.0 - total)
    } else {
        sum / total
    };
    value.clamp(-limit, limit)
}

/// A live edit to node properties, handed to the closure
/// [`Puppet::refold_with_node_edits`] takes.
///
/// It reaches only the three properties a gesture drags — the transform, the
/// z order and the opacity — and it writes into the puppet's own evaluated
/// frame, never into the model. Anything more belongs in a model edit.
pub struct NodeEdits<'a> {
    arena: &'a mut Arena,
}

impl NodeEdits<'_> {
    /// Edit the node's transform in place. `false` when the arena has no such
    /// node.
    pub fn transform(&mut self, id: NodeIdx, edit: impl FnOnce(&mut crate::Transform)) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        edit(&mut node.transform);
        self.arena.mark_transform_dirty(id);
        true
    }

    /// `false` when the arena has no such node.
    pub fn set_z_order(&mut self, id: NodeIdx, z_order: f32) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        node.z_order = z_order;
        true
    }

    /// `false` when the arena has no such node, or when the node is not one
    /// that carries an opacity (only parts and composites do).
    pub fn set_opacity(&mut self, id: NodeIdx, opacity: f32) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        match &mut node.kind {
            NodeKind::Part(part) => part.opacity = opacity,
            NodeKind::Composite(composite) => composite.opacity = opacity,
            _ => return false,
        }
        true
    }
}

/// A live edit to one node's properties, held by the puppet and re-applied by
/// every fold — the transform half of an edit in progress, exactly as a
/// scratch deform is its per-vertex half.
///
/// It reaches the same three properties [`NodeEdits`] does, because they are
/// the three a gesture drags: the local transform, the z order and the
/// opacity. Every field is optional and `None` means *leave what the fold
/// produced*, so a drag that only translates does not also pin the rotation a
/// binding is animating.
///
/// **The values are absolute, not deltas.** A field that is `Some` replaces
/// the node's evaluated property outright, and the number a client previews is
/// therefore the number it commits — the `node_set` command's patch carries
/// `translate` / `rotate` / `scale` / `z_order` / `opacity`, optional and
/// absolute in exactly this shape, so a drag ends by sending back the fields
/// it has been showing. A delta would have to be added to an authored value
/// the client would then have to re-derive, and the two could disagree.
///
/// The cost of absolute is that a `Some` field also overrides what the
/// bindings folded into it. That is the right trade for a drag: while the
/// pointer is down the node goes exactly where the pointer says, and the
/// binding's contribution comes back the moment the field goes to `None`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ScratchTransform {
    /// Local translation, in the parent's frame.
    pub translation: Option<Vec3>,
    /// Local rotation as Euler XYZ radians, the form [`crate::Transform`]
    /// holds.
    pub rotation: Option<Vec3>,
    /// Local scale. Z is not scaled; a node's transform carries two axes.
    pub scale: Option<Vec2>,
    /// Draw order within the parent.
    pub z_order: Option<f32>,
    /// Ignored on nodes that carry no opacity — only parts and composites do.
    pub opacity: Option<f32>,
}

/// What is still moving after a [`Puppet::tick`], so a viewport can stay dirty
/// while the puppet animates itself and idle when it does not.
///
/// Only self-driven motion counts. A pose change, a scratch edit or a model
/// edit is not motion: the caller made it and already knows to redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Motion {
    /// A driver — a `SimplePhysics` pendulum or a particle chain — is away
    /// from its rest pose, so the next tick will move it even if nothing else
    /// changes.
    pub physics: bool,
    /// A playing animation wrote a param this tick.
    pub animation: bool,
}

impl Motion {
    /// Whether anything at all is moving.
    pub fn any(self) -> bool {
        self.physics || self.animation
    }
}

/// A normalized param input, with a bracket filled for each binding axis.
#[derive(Debug, Clone, Copy)]
struct Located {
    /// The resolved param value, before clamping — the deform fold's memo key.
    value: f32,
    normalized: f32,
    lo: usize,
    hi: usize,
    frac: f32,
}

impl Located {
    const REST: Self = Self {
        value: 0.0,
        normalized: 0.0,
        lo: 0,
        hi: 0,
        frac: 0.0,
    };
}

/// A model being animated: its pose, its drivers' state and the frame its last
/// tick produced.
///
/// Cloning one is how a caller forks an animation — the clone shares every
/// binding grid with the original through `Arc` and copies only the frame.
#[derive(Clone)]
pub struct Puppet {
    /// The `Model::identity` of the model this puppet animates.
    model_identity: u64,
    /// The `Model::generation` this puppet is baked against.
    baked_generation: u64,
    arena: Arena,
    transforms: GlobalTransforms,

    node_of_id: HashMap<NodeId, NodeIdx>,
    /// Indexed by `NodeIdx.0`, so the arena's DFS order is the Id order.
    id_of_node: Vec<NodeId>,

    params: Vec<BakedParam>,
    slot_of_param: HashMap<ParamId, u32>,
    /// Posed value per param slot; `None` reads the param's default, which is
    /// the state `clear_param_value` leaves.
    param_values: Vec<Option<f32>>,
    /// Values posed for params this model does not (yet) have. Kept off the
    /// dense path so it never costs the hot loops, and promoted by the next
    /// rebake that gives the Id a slot.
    param_values_overflow: HashMap<ParamId, f32>,
    /// One entry per (param, source), upserted by the source rather than
    /// cleared each frame, so a driver's output survives into the next frame's
    /// anchor pre-pass. That persistence is what couples chained physics.
    ///
    /// **Every entry is indexed by `slot_claims`, and nothing reaches this
    /// list except through the pair.** An entry is pushed only once its
    /// slot's row exists to record where it went, and the only thing that
    /// removes entries — retirement — rebuilds the index from this list
    /// afterwards. So a claim is never in one and not the other, and no
    /// reader has to filter the whole list to find a slot's claims.
    param_contributions: Vec<Contribution>,
    /// Parallel to `params`: where that slot's claims sit in
    /// `param_contributions`, in the order they were made.
    ///
    /// The claims on one param are a handful — one driver, occasionally two —
    /// where the puppet's claims all told are one per driver output, thousands
    /// on a rig of a few hundred strands. Every read and every upsert is over
    /// this row, so the cost of a claim is the row's length and not the rig's.
    slot_claims: Vec<smallvec::SmallVec<[u32; 2]>>,
    /// Parallel to `params`: does any entry target that slot. Keeps the
    /// twice-a-frame fold to one bool load for the overwhelming majority of
    /// params, which have no contributor at all.
    param_contributed: Vec<bool>,
    param_generation: u64,
    last_tick_folded_param_generation: Option<u64>,

    bindings: Vec<BakedBinding>,
    /// Param slots whose bindings reach a mesh group or something under one.
    param_mesh_group_relevant: HashSet<u32>,
    mesh_group_param_generation: u64,
    last_tick_mesh_group_generation: Option<u64>,

    physics_enabled: bool,
    /// Parallel to `arena.physics_node_ids`.
    physics_targets: Vec<[Option<u32>; 2]>,
    physics_update_scratch: Vec<([Option<u32>; 2], NodeIdx, Vec2)>,
    /// Parallel to `arena.spine_node_ids`: one param slot per link. One list
    /// serves both directions — the spine reads its bends from these params
    /// and the chain it may carry writes them — because they are the same
    /// bends.
    spine_targets: Vec<Vec<Option<u32>>>,
    /// This frame's raw chain bends, with their authored weights and limits.
    /// Flat because the contribution loop and the retirement scan read it
    /// that way; the blend and its clamp belong to `resolve_contributions`.
    chain_update_scratch: Vec<Contribution>,
    /// Per-slot index into `chain_update_scratch` while collecting one chain.
    /// Repeated targets keep the last bend but all their limits. Clearing only
    /// the slots that chain touched keeps collection linear in its outputs.
    chain_claim_scratch: Vec<Option<usize>>,
    /// Held so reading a chain out costs no allocation per frame.
    chain_bends_scratch: Vec<f32>,
    /// Retirement's two sets, held for the same reason: which nodes are
    /// drivers, and which `(slot, source)` claims this frame's drivers made.
    /// Rebuilt at the top of every retirement pass and meaningless outside it.
    retire_drivers_scratch: HashSet<NodeIdx>,
    retire_live_scratch: HashSet<(u32, NodeIdx)>,
    /// The same, for the posed bend a chain's springs pull toward.
    chain_posed_scratch: Vec<f32>,
    /// `Some(G)` means the cached physics transforms and the node-level anchor
    /// inputs hold the anchor pose a fresh pre-pass at
    /// `param_generation == G` would produce.
    last_anchor_pose_generation: Option<u64>,

    animations: Vec<ClmAnimation>,
    play_state: Option<AnimationPlayState>,

    /// Live node edits, re-applied by every fold. Sparse rather than one entry
    /// per node: a drag holds one or two, and every fold pays for a lookup per
    /// entry, not per node.
    scratch_transforms: HashMap<NodeIdx, ScratchTransform>,

    /// Reused by the fold: where each param slot sits on its key positions.
    located: Vec<Located>,
}

impl Puppet {
    /// Bake `model` and record the generation it was baked at.
    pub fn new(model: &Model) -> Self {
        let mut puppet = Self {
            model_identity: model.identity(),
            baked_generation: model.generation(),
            arena: Arena::new(),
            transforms: GlobalTransforms::new(),
            node_of_id: HashMap::new(),
            id_of_node: Vec::new(),
            params: Vec::new(),
            slot_of_param: HashMap::new(),
            param_values: Vec::new(),
            param_values_overflow: HashMap::new(),
            param_contributions: Vec::new(),
            slot_claims: Vec::new(),
            param_contributed: Vec::new(),
            param_generation: 0,
            last_tick_folded_param_generation: None,
            bindings: Vec::new(),
            param_mesh_group_relevant: HashSet::new(),
            mesh_group_param_generation: 0,
            last_tick_mesh_group_generation: None,
            physics_enabled: true,
            physics_targets: Vec::new(),
            physics_update_scratch: Vec::new(),
            spine_targets: Vec::new(),
            chain_update_scratch: Vec::new(),
            chain_claim_scratch: Vec::new(),
            chain_bends_scratch: Vec::new(),
            chain_posed_scratch: Vec::new(),
            retire_drivers_scratch: HashSet::new(),
            retire_live_scratch: HashSet::new(),
            last_anchor_pose_generation: None,
            animations: Vec::new(),
            play_state: None,
            scratch_transforms: HashMap::new(),
            located: Vec::new(),
        };
        puppet.install(bake::bake(model));
        puppet
    }

    /// The `Model::generation` this puppet last baked against.
    pub fn baked_generation(&self) -> u64 {
        self.baked_generation
    }

    /// The [`Model::identity`] of the model this puppet animates.
    pub fn model_identity(&self) -> u64 {
        self.model_identity
    }

    /// Rebake if `model` moved since the last bake, carrying the pose and the
    /// drivers across by Id.
    ///
    /// `model` must be the model this puppet was built from — a puppet
    /// animates one model, and handing it another is a programmer error the
    /// identity check catches.
    pub fn sync(&mut self, model: &Model) {
        debug_assert_eq!(
            self.model_identity,
            model.identity(),
            "a puppet was driven against a model it was not built from; \
             a puppet animates the model it was baked from",
        );
        if self.baked_generation == model.generation() {
            return;
        }
        let pose: Vec<(ParamId, f32)> = self
            .param_values
            .iter()
            .enumerate()
            .filter_map(|(slot, v)| Some((self.params.get(slot)?.id.clone(), (*v)?)))
            .chain(
                self.param_values_overflow
                    .iter()
                    .map(|(id, v)| (id.clone(), *v)),
            )
            .collect();
        let contributions: Vec<_> = self
            .param_contributions
            .iter()
            .filter_map(|c| {
                Some((
                    self.params.get(c.slot as usize)?.id.clone(),
                    self.id_of_node.get(c.source.0 as usize)?.clone(),
                    c.value,
                    c.weight,
                    c.limit,
                ))
            })
            .collect();
        let drivers: Vec<(NodeId, SimplePhysicsData)> = self
            .arena
            .physics_node_ids
            .iter()
            .filter_map(|&idx| {
                let id = self.id_of_node.get(idx.0 as usize)?.clone();
                match &self.arena.get(idx)?.kind {
                    NodeKind::SimplePhysics(p) => Some((id, (**p).clone())),
                    _ => None,
                }
            })
            .collect();
        let chains: Vec<(NodeId, ParticleChainData)> = self
            .arena
            .chain_node_ids
            .iter()
            .filter_map(|&idx| {
                let id = self.id_of_node.get(idx.0 as usize)?.clone();
                match &self.arena.get(idx)?.kind {
                    NodeKind::Spine(sp) => sp.chain.as_ref().map(|c| (id, c.clone())),
                    _ => None,
                }
            })
            .collect();
        let scratch: Vec<(NodeId, ScratchTransform)> = self
            .scratch_transforms
            .iter()
            .filter_map(|(idx, edit)| Some((self.id_of_node.get(idx.0 as usize)?.clone(), *edit)))
            .collect();

        self.install(bake::bake(model));
        self.baked_generation = model.generation();

        for (id, edit) in scratch {
            let Some(&idx) = self.node_of_id.get(&id) else {
                continue;
            };
            self.scratch_transforms.insert(idx, edit);
        }
        for (id, value) in pose {
            self.set_param_value(&id, value);
        }
        for (param, source, value, weight, limit) in contributions {
            let (Some(&slot), Some(&source)) =
                (self.slot_of_param.get(&param), self.node_of_id.get(&source))
            else {
                continue;
            };
            self.record_contribution(Contribution {
                slot,
                source,
                value,
                weight,
                limit,
            });
        }
        for (id, saved) in drivers {
            let Some(&idx) = self.node_of_id.get(&id) else {
                continue;
            };
            if let Some(NodeKind::SimplePhysics(p)) = self.arena.get_mut(idx).map(|n| &mut n.kind) {
                // The authored half comes from the model; only the runtime
                // half survives an edit.
                p.offset_output_scale = saved.offset_output_scale;
                p.bob = saved.bob;
                p.spring_vel = saved.spring_vel;
                p.d_angle = saved.d_angle;
                p.anchor = saved.anchor;
                p.anchor_initialized = saved.anchor_initialized;
            }
        }
        for (id, saved) in chains {
            let Some(&idx) = self.node_of_id.get(&id) else {
                continue;
            };
            let Some(NodeKind::Spine(spine)) = self.arena.get_mut(idx).map(|n| &mut n.kind) else {
                continue;
            };
            let Some(chain) = &mut spine.chain else {
                continue;
            };
            // Only a chain of the same shape can take the old particles: they
            // are positions on rods of particular lengths, and a chain whose
            // links the edit changed has no rod to put the old point back on.
            // That one leaves the fresh bake alone and re-hangs on its next
            // tick, which is a visible snap — and the honest one, since the
            // author just changed the chain the hair was hanging from.
            if saved.particles.len() != chain.links.len() + 1 {
                continue;
            }
            chain.particles = saved.particles;
            chain.anchor = saved.anchor;
            chain.carry = saved.carry;
            chain.moved_last_tick = saved.moved_last_tick;
            chain.anchor_initialized = saved.anchor_initialized;
        }
    }

    /// Replace everything derived from the model. Every slot-keyed memo is
    /// dropped: the slots themselves have moved.
    fn install(&mut self, baked: Baked) {
        let Baked {
            arena,
            node_of_id,
            id_of_node,
            params,
            slot_of_param,
            bindings,
            physics_targets,
            spine_targets,
        } = baked;
        self.arena = arena;
        self.node_of_id = node_of_id;
        self.id_of_node = id_of_node;
        self.param_values = vec![None; params.len()];
        self.slot_claims = vec![smallvec::SmallVec::new(); params.len()];
        self.param_contributed = vec![false; params.len()];
        self.located = vec![Located::REST; params.len()];
        self.params = params;
        self.slot_of_param = slot_of_param;
        self.bindings = bindings;
        self.physics_targets = physics_targets;
        self.spine_targets = spine_targets;
        self.param_values_overflow.clear();
        self.param_contributions.clear();
        // Keyed by slot, and the slots have moved. `sync` re-keys the entries
        // it saved by Id and puts them back; a bare `install` has none.
        self.scratch_transforms.clear();
        self.transforms = GlobalTransforms::new();
        self.param_generation = self.param_generation.wrapping_add(1);
        self.last_tick_folded_param_generation = None;
        self.last_tick_mesh_group_generation = None;
        self.last_anchor_pose_generation = None;
        self.rebuild_param_effect_cache();
    }

    /// Param slots whose bindings can move a mesh group or something under
    /// one, so a pose change that touches none of them can skip the
    /// mesh-group passes.
    fn rebuild_param_effect_cache(&mut self) {
        self.param_mesh_group_relevant.clear();
        if self.arena.mesh_group_node_ids.is_empty() {
            return;
        }
        let mut related = HashSet::new();
        for &mg_id in &self.arena.mesh_group_node_ids {
            related.insert(mg_id);
            related.extend(self.arena.tree.get_all_descendants(mg_id));
        }
        for b in &self.bindings {
            let moves_geometry = matches!(
                b.target,
                BindingTarget::Deform
                    | BindingTarget::Scalar(
                        ScalarTarget::Tx
                            | ScalarTarget::Ty
                            | ScalarTarget::Sx
                            | ScalarTarget::Sy
                            | ScalarTarget::Rx
                            | ScalarTarget::Ry
                            | ScalarTarget::Rz
                    )
            );
            if moves_geometry && related.contains(&b.node) {
                self.param_mesh_group_relevant.insert(b.x);
                if let Some(y) = b.y {
                    self.param_mesh_group_relevant.insert(y);
                }
            }
        }
    }

    // ---- the arena, at the edge ------------------------------------------

    pub fn root(&self) -> NodeIdx {
        self.arena.root()
    }

    pub fn tree(&self) -> &NodeTree {
        &self.arena.tree
    }

    pub fn len(&self) -> usize {
        self.arena.len()
    }

    pub fn is_empty(&self) -> bool {
        self.arena.is_empty()
    }

    pub fn get(&self, id: NodeIdx) -> Option<&Node> {
        self.arena.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.arena.iter()
    }

    pub fn iter_deform_nodes(&self) -> impl Iterator<Item = (NodeIdx, &Node)> {
        self.arena.iter_deform_nodes()
    }

    /// The arena slot a model node was baked into.
    pub fn node_idx(&self, id: &NodeId) -> Option<NodeIdx> {
        self.node_of_id.get(id).copied()
    }

    /// The model node an arena slot came from.
    pub fn node_id(&self, idx: NodeIdx) -> Option<&NodeId> {
        self.id_of_node.get(idx.0 as usize)
    }

    /// The transforms the last tick produced.
    pub fn transforms(&self) -> &GlobalTransforms {
        &self.transforms
    }

    /// The node's z as everything that orders drawables sorts by: its own
    /// `z_order` plus every ancestor's, up to and including the root.
    ///
    /// `z_order` is authored relative to the parent, so a single node's field
    /// says nothing about where it lands against a node in another subtree —
    /// only the sum does. Higher draws in front. Every ancestor counts, a
    /// disabled or fully transparent one included: hiding a node must not
    /// reorder its descendants against anything else.
    ///
    /// O(depth). The climb is up the parent chain, a handful of links even in
    /// a deep rig.
    pub fn accumulated_z(&self, idx: NodeIdx) -> f32 {
        let mut z = 0.0;
        let mut at = Some(idx);
        while let Some(id) = at {
            let Some(node) = self.get(id) else { break };
            z += node.z_order;
            at = self.tree().get_parent(id);
        }
        z
    }

    /// The node's opacity: a Part's or a Composite's, `None` for a kind that
    /// carries none.
    ///
    /// **Opacity is not inherited, unlike [`Self::accumulated_z`].** A
    /// composite that is not fully opaque renders into its own target and
    /// that target is blitted at the composite's opacity, so an ancestor's
    /// opacity is applied once, at the blit, and multiplying it into a
    /// descendant here would apply it twice.
    pub fn opacity(&self, idx: NodeIdx) -> Option<f32> {
        match &self.get(idx)?.kind {
            NodeKind::Part(part) => Some(part.opacity),
            NodeKind::Composite(composite) => Some(composite.opacity),
            _ => None,
        }
    }

    /// Recompute the evaluated transforms without folding anything — for a
    /// caller that moved a node between ticks and needs where it landed.
    pub fn compute_transforms(&mut self) {
        let mut out = std::mem::take(&mut self.transforms);
        self.arena.compute_transforms(&mut out);
        self.transforms = out;
    }

    /// Collapse each deform stack's active sources into its combined offset.
    /// A tick does this itself; a caller needs it after writing a scratch
    /// deform outside one.
    pub fn combine_deforms(&mut self) {
        self.arena.combine_deforms();
    }

    /// One node's combined per-vertex deform after the last tick.
    pub fn combined_deform(&self, id: NodeIdx) -> Option<&[Vec2]> {
        match &self.arena.get(id)?.kind {
            NodeKind::Part(p) => Some(p.deform_stack.combined()),
            NodeKind::MeshGroup(mg) => Some(mg.deform_stack.combined()),
            _ => None,
        }
    }

    /// Write the puppet's scratch deform — an edit in progress, shown live and
    /// never part of the model.
    ///
    /// It outlives the folds that follow it: a tick re-derives every other
    /// deform source from the model and drops what it does not produce, but
    /// nothing but the writer holds this one. So a client writes it once per
    /// pointer move rather than once per frame, and the drag survives a frame
    /// that folds for its own reasons (a driver stepping, a pose moving).
    /// It ends at [`Self::clear_scratch_deform`], or when the model changes
    /// and [`Self::sync`] rebakes the stack.
    pub fn set_scratch_deform(&mut self, id: NodeIdx, offsets: &[Vec2]) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        let stack = match &mut node.kind {
            NodeKind::Part(p) => &mut p.deform_stack,
            NodeKind::MeshGroup(mg) => &mut mg.deform_stack,
            _ => return false,
        };
        let buf = stack.source_buf_mut(DeformSource::Scratch);
        let n = buf.len().min(offsets.len());
        buf[..n].copy_from_slice(&offsets[..n]);
        for v in buf[n..].iter_mut() {
            *v = Vec2::ZERO;
        }
        true
    }

    pub fn clear_scratch_deform(&mut self, id: NodeIdx) -> bool {
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        match &mut node.kind {
            NodeKind::Part(p) => p.deform_stack.clear_source(DeformSource::Scratch),
            NodeKind::MeshGroup(mg) => mg.deform_stack.clear_source(DeformSource::Scratch),
            _ => return false,
        }
        true
    }

    // ---- scratch transforms ------------------------------------------------

    /// Write the puppet's scratch transform for one node — an edit in
    /// progress, shown live and never part of the model.
    ///
    /// It outlives the folds that follow it exactly as a scratch deform does:
    /// every [`Self::tick`] re-applies it, at the point in the frame where the
    /// bindings have folded and nothing downstream has run yet, so the
    /// transform walk, the mesh-group pass, welds and the deform combine all
    /// see the previewed pose and the node's dependents move with it. A client
    /// writes it once per pointer move rather than once per frame.
    ///
    /// See [`ScratchTransform`] for what the fields mean and why they are
    /// absolute. Replaces any scratch transform already on the node; `false`
    /// when the arena has no such node.
    ///
    /// It ends at [`Self::clear_scratch_transform`] or
    /// [`Self::clear_all_scratch`] — and **only** there. Unlike a scratch
    /// deform, it survives the rebake a model edit triggers, because the
    /// commit that ends a drag *is* a model edit and the preview has to
    /// outlive it or the node flickers back for one frame. A node the edit
    /// deleted takes its scratch with it.
    pub fn set_scratch_transform(&mut self, id: NodeIdx, edit: ScratchTransform) -> bool {
        if self.arena.get(id).is_none() {
            return false;
        }
        self.scratch_transforms.insert(id, edit);
        self.invalidate_fold();
        true
    }

    /// Drop one node's scratch transform. `false` when it had none.
    pub fn clear_scratch_transform(&mut self, id: NodeIdx) -> bool {
        if self.scratch_transforms.remove(&id).is_none() {
            return false;
        }
        self.invalidate_fold();
        true
    }

    /// End every edit in progress: every scratch transform and every scratch
    /// deform. What a client calls once when a gesture is committed or
    /// abandoned, instead of remembering which nodes it touched.
    pub fn clear_all_scratch(&mut self) {
        self.scratch_transforms.clear();
        for i in 0..self.arena.deform_node_ids.len() {
            let id = self.arena.deform_node_ids[i];
            self.clear_scratch_deform(id);
        }
        self.invalidate_fold();
    }

    /// The scratch transform standing on a node, if any.
    pub fn scratch_transform(&self, id: NodeIdx) -> Option<&ScratchTransform> {
        self.scratch_transforms.get(&id)
    }

    /// Force the next tick to fold. The fold is memoized on the pose's
    /// generation, and a scratch edit moves neither the pose nor the model, so
    /// without this the next tick would skip the fold that applies it.
    fn invalidate_fold(&mut self) {
        self.last_tick_folded_param_generation = None;
        self.last_tick_mesh_group_generation = None;
    }

    /// Write every scratch transform over what the fold produced. Runs after
    /// the bindings and before anything that reads a transform.
    fn apply_scratch_transforms(&mut self) {
        if self.scratch_transforms.is_empty() {
            return;
        }
        // Moved out so the writes below can take `&mut Arena`; put back before
        // returning, and nothing here can early-return.
        let scratch = std::mem::take(&mut self.scratch_transforms);
        for (&id, edit) in &scratch {
            let Some(node) = self.arena.get_mut(id) else {
                continue;
            };
            let mut moved = false;
            if let Some(t) = edit.translation {
                node.transform.translation = t;
                moved = true;
            }
            if let Some(r) = edit.rotation {
                node.transform.rotation = r;
                moved = true;
            }
            if let Some(s) = edit.scale {
                node.transform.scale = s;
                moved = true;
            }
            if let Some(z) = edit.z_order {
                node.z_order = z;
            }
            if let Some(o) = edit.opacity {
                match &mut node.kind {
                    NodeKind::Part(part) => part.opacity = o,
                    NodeKind::Composite(composite) => composite.opacity = o,
                    _ => {}
                }
            }
            if moved {
                self.arena.mark_transform_dirty(id);
            }
        }
        self.scratch_transforms = scratch;
    }

    /// Re-evaluate the current frame with a live edit to node properties on
    /// top of the pose — the transform half of an edit in progress, shown
    /// live and never part of the model, exactly as a scratch deform is its
    /// per-vertex half.
    ///
    /// This is not a tick: no driver steps and no animation advances. The
    /// bindings are folded again from the model's authored values, `edit`
    /// writes over what they produced, and then everything that reads a
    /// transform is redone — the transform walk, the mesh-group pass, welds
    /// and the deform combine. Doing them again is the point: a previewed
    /// transform has to move the children of a translate-children group and
    /// the mesh groups above them, or the frame shows a node in its new place
    /// and its dependents in their old one.
    ///
    /// The edit lives until the next fold. [`Self::tick`] starts from the
    /// model's authored values, so a preview never accumulates and nothing has
    /// to undo it — which is also why a caller whose puppet ticks every frame
    /// wants [`Self::set_scratch_transform`] instead. The closure runs *after*
    /// the scratch transforms, so it wins over them on any field they share.
    pub fn refold_with_node_edits(&mut self, edit: impl FnOnce(&mut NodeEdits<'_>)) {
        let _span = tracing::trace_span!("refold_with_node_edits").entered();
        self.reset_frame();
        self.apply_params();
        self.apply_scratch_transforms();
        edit(&mut NodeEdits {
            arena: &mut self.arena,
        });
        let mut out = std::mem::take(&mut self.transforms);
        self.arena.compute_transforms(&mut out);
        self.arena
            .propagate_mesh_group_deforms(&mut out, Mat4::IDENTITY);
        self.arena.apply_welds(&out);
        self.arena.combine_deforms();
        self.transforms = out;
    }

    // ---- the pose ---------------------------------------------------------

    /// The params this puppet was baked with, in the model's order.
    pub fn param_ids(&self) -> impl Iterator<Item = &ParamId> {
        self.params.iter().map(|p| &p.id)
    }

    /// The param's effective value: what was posed, folded with any driver
    /// claims. `None` only when nothing posed it and nothing claims it, so the
    /// caller falls back to the param's own default exactly as the fold does.
    pub fn param_value(&self, param: &ParamId) -> Option<f32> {
        let slot = self.slot_of_param.get(param).copied();
        let base = match slot {
            Some(slot) => self.param_values.get(slot as usize).copied().flatten(),
            None => self.param_values_overflow.get(param).copied(),
        };
        let slot = match slot {
            Some(slot) => slot,
            None => return base,
        };
        let claims = self.slot_claims.get(slot as usize).map_or(&[][..], |c| c);
        if claims.is_empty() {
            return base;
        }
        let default = self
            .params
            .get(slot as usize)
            .map(|p| p.default)
            .unwrap_or(0.0);
        Some(resolve_contributions(
            &self.param_contributions,
            claims,
            base.unwrap_or(default),
        ))
    }

    /// Pose one param. Equivalent to a full-authority claim wherever nothing
    /// else writes it, which is every param on a model with no driver aimed at
    /// it; where a driver *does* claim it, this is what the driver's weight
    /// blends against.
    pub fn set_param_value(&mut self, param: &ParamId, value: f32) {
        match self.slot_of_param.get(param).copied() {
            Some(slot) => {
                if self.param_values.get(slot as usize).copied().flatten() == Some(value) {
                    return;
                }
                if let Some(v) = self.param_values.get_mut(slot as usize) {
                    *v = Some(value);
                }
                self.bump_param_generation_for_slot(slot);
            }
            None => {
                if self.param_values_overflow.get(param).copied() == Some(value) {
                    return;
                }
                self.param_values_overflow.insert(param.clone(), value);
                self.param_generation = self.param_generation.wrapping_add(1);
            }
        }
    }

    /// Drop a posed value, restoring the param to its default.
    pub fn clear_param_value(&mut self, param: &ParamId) {
        match self.slot_of_param.get(param).copied() {
            Some(slot) => {
                if self
                    .param_values
                    .get_mut(slot as usize)
                    .and_then(|v| v.take())
                    .is_some()
                {
                    self.bump_param_generation_for_slot(slot);
                }
            }
            None => {
                if self.param_values_overflow.remove(param).is_some() {
                    self.param_generation = self.param_generation.wrapping_add(1);
                }
            }
        }
    }

    /// Restore every param to its default, then apply `pose` as an overlay.
    /// A caller that poses only the params it controls needs the reset:
    /// without it a previous overlay's value would stick.
    pub fn apply_pose(&mut self, pose: &Pose) {
        for slot in 0..self.params.len() {
            let (id, default) = {
                let p = &self.params[slot];
                (p.id.clone(), p.default)
            };
            let value = pose.get(&id).unwrap_or(default);
            self.set_param_value(&id, value);
        }
        for (id, value) in pose.iter() {
            if !self.slot_of_param.contains_key(id) {
                self.set_param_value(id, value);
            }
        }
    }

    /// The pose the puppet currently holds, driver claims folded in.
    pub fn pose(&self) -> Pose {
        self.params
            .iter()
            .map(|p| (p.id.clone(), self.param_value(&p.id).unwrap_or(p.default)))
            .collect()
    }

    /// Record `source`'s weighted claim on `param`, replacing that source's
    /// previous one. Returns whether the resolved value moved.
    pub fn contribute_param_value(
        &mut self,
        param: &ParamId,
        source: NodeIdx,
        value: f32,
        weight: f32,
    ) -> bool {
        let Some(slot) = self.slot_of_param.get(param).copied() else {
            return false;
        };
        self.contribute(slot, source, value, weight)
    }

    fn contribute(&mut self, slot: u32, source: NodeIdx, value: f32, weight: f32) -> bool {
        self.record_contribution(Contribution {
            slot,
            source,
            value,
            weight,
            limit: None,
        })
    }

    fn record_contribution(&mut self, contribution: Contribution) -> bool {
        let slot = contribution.slot;
        // A slot with no row is a slot this puppet does not have; there is
        // nowhere to record the claim, and recording it only in the storage
        // would put the two out of step.
        let Some(claims) = self.slot_claims.get(slot as usize) else {
            return false;
        };
        let before = self.resolved(slot);
        let held = claims.iter().find(|&&i| {
            self.param_contributions
                .get(i as usize)
                .is_some_and(|e| e.source == contribution.source)
        });
        match held.copied() {
            Some(i) => {
                if let Some(e) = self.param_contributions.get_mut(i as usize) {
                    *e = contribution;
                }
            }
            None => {
                let at = self.param_contributions.len() as u32;
                self.param_contributions.push(contribution);
                if let Some(row) = self.slot_claims.get_mut(slot as usize) {
                    row.push(at);
                }
                if let Some(flag) = self.param_contributed.get_mut(slot as usize) {
                    *flag = true;
                }
            }
        }
        if before == self.resolved(slot) {
            return false;
        }
        self.bump_param_generation_for_slot(slot);
        true
    }

    /// Put `slot_claims` back in step with `param_contributions`, which is
    /// what the one thing that removes entries owes the pair. Every other
    /// writer keeps the two together as it goes.
    fn rebuild_claim_index(&mut self) {
        for row in self.slot_claims.iter_mut() {
            row.clear();
        }
        for (i, e) in self.param_contributions.iter().enumerate() {
            if let Some(row) = self.slot_claims.get_mut(e.slot as usize) {
                row.push(i as u32);
            }
        }
    }

    /// Drop every driver claim, restoring each param to what was posed.
    pub fn clear_param_contributions(&mut self) {
        if self.param_contributions.is_empty() {
            return;
        }
        for e in std::mem::take(&mut self.param_contributions) {
            if let Some(row) = self.slot_claims.get_mut(e.slot as usize) {
                row.clear();
            }
            self.bump_param_generation_for_slot(e.slot);
        }
        for flag in self.param_contributed.iter_mut() {
            *flag = false;
        }
    }

    /// What a slot was posed at before any driver claimed it: the value the
    /// caller set, or the param's own default.
    fn param_base(&self, slot: u32) -> Option<f32> {
        let p = self.params.get(slot as usize)?;
        Some(
            self.param_values
                .get(slot as usize)
                .copied()
                .flatten()
                .unwrap_or(p.default),
        )
    }

    /// The value the fold uses for a slot: what was posed (or the default),
    /// with driver claims folded in.
    fn resolved(&self, slot: u32) -> f32 {
        let Some(base) = self.param_base(slot) else {
            return 0.0;
        };
        if self
            .param_contributed
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
        {
            let claims = self.slot_claims.get(slot as usize).map_or(&[][..], |c| c);
            resolve_contributions(&self.param_contributions, claims, base)
        } else {
            base
        }
    }

    fn bump_param_generation_for_slot(&mut self, slot: u32) {
        self.param_generation = self.param_generation.wrapping_add(1);
        if self.param_mesh_group_relevant.contains(&slot) {
            self.mesh_group_param_generation = self.mesh_group_param_generation.wrapping_add(1);
        }
    }

    // ---- animations -------------------------------------------------------

    pub fn animations(&self) -> &[ClmAnimation] {
        &self.animations
    }

    /// Replace the animations this puppet can play with clips a caller built
    /// by hand. [`Self::set_animations_from`] is the one that takes the
    /// model's own.
    pub fn set_animations(&mut self, animations: Vec<ClmAnimation>) {
        self.animations = animations;
        // Any play state indexes into the old list.
        self.play_state = None;
    }

    /// Take the model's own animations. A rebake does not do this: the clips
    /// are the caller's to install, because a caller may be playing ones the
    /// model does not carry.
    pub fn set_animations_from(&mut self, model: &Model) {
        self.set_animations(model.animations().to_vec());
    }

    /// Start playing the animation with this name, looping. False when no
    /// animation has it.
    pub fn play_animation(&mut self, name: &str) -> bool {
        if let Some(index) = self.animations.iter().position(|a| a.name == name) {
            self.play_state = Some(AnimationPlayState {
                index,
                time: 0.0,
                looping: true,
            });
            true
        } else {
            false
        }
    }

    pub fn stop_animation(&mut self) {
        self.play_state = None;
    }

    pub fn has_playing_animation(&self) -> bool {
        self.play_state.is_some()
    }

    /// The name of the animation playing right now, so a caller swapping one
    /// puppet for another can carry the play state over.
    pub fn playing_animation(&self) -> Option<&str> {
        let state = self.play_state?;
        Some(self.animations.get(state.index)?.name.as_str())
    }

    /// Advance playback by `dt` seconds and pose every lane's param at the
    /// value it holds there. Returns whether any param moved.
    pub fn tick_animations(&mut self, dt: f32) -> bool {
        let _span = tracing::trace_span!("tick_animations").entered();
        if !dt.is_finite() {
            return false;
        }
        let Some(mut state) = self.play_state else {
            return false;
        };
        let Some(anim) = self.animations.get(state.index) else {
            self.play_state = None;
            return false;
        };
        state.time += dt;
        // A looping animation snaps back to the loop region's start (the
        // lead-in plays once), and playback always clamps to the last frame.
        if anim.length > 0 && anim.timestep > 0.0 {
            let (loop_begin, loop_end) = anim.loop_region();
            let frame = (state.time / anim.timestep).round() as i64;
            if state.looping && frame >= loop_end as i64 {
                state.time = loop_begin as f32 * anim.timestep;
            }
            let frame = (state.time / anim.timestep).round() as i64;
            if frame + 1 >= anim.length as i64 {
                state.time = (anim.length - 1) as f32 * anim.timestep;
            }
        }
        let frame = if anim.timestep > 0.0 {
            state.time / anim.timestep
        } else {
            0.0
        };
        // Move the list out so the writes below can take `&mut self`; nothing
        // here can early-return before it is put back.
        let animations = std::mem::take(&mut self.animations);
        let mut changed = false;
        if let Some(anim) = animations.get(state.index) {
            for lane in &anim.lanes {
                let value = lane.value_at(frame);
                if self.param_value_posed(&lane.param) != Some(value) {
                    self.set_param_value(&lane.param, value);
                    changed = true;
                }
            }
        }
        self.animations = animations;
        self.play_state = Some(state);
        changed
    }

    /// The value posed for a param, ignoring driver claims — what a lane
    /// compares against before writing.
    fn param_value_posed(&self, param: &ParamId) -> Option<f32> {
        match self.slot_of_param.get(param) {
            Some(&slot) => self.param_values.get(slot as usize).copied().flatten(),
            None => self.param_values_overflow.get(param).copied(),
        }
    }

    // ---- drivers ----------------------------------------------------------

    pub fn has_simple_physics(&self) -> bool {
        !self.arena.physics_node_ids.is_empty()
    }

    pub fn has_particle_chains(&self) -> bool {
        !self.arena.chain_node_ids.is_empty()
    }

    /// Whether the puppet carries any driver at all — a pendulum or a
    /// particle chain. This, not either half, is what gates the anchor
    /// pre-pass and the settle: both kinds hang from an anchor the pre-pass
    /// poses, so a model whose only driver is a chain needs it just as much.
    pub fn has_drivers(&self) -> bool {
        self.has_simple_physics() || self.has_particle_chains()
    }

    /// When false, a tick skips physics entirely: drivers never overwrite
    /// their target params, so the same pose always yields the same frame.
    /// The editor freezes physics this way — its dt=0 preview cannot
    /// integrate, and chained drivers would otherwise leave
    /// pose-history-dependent residue in the authoring view.
    pub fn set_physics_enabled(&mut self, enabled: bool) {
        self.physics_enabled = enabled;
        if !enabled {
            // Otherwise each driver's last output would keep overriding the
            // pose for as long as physics stays frozen.
            self.clear_param_contributions();
        }
    }

    pub fn physics_enabled(&self) -> bool {
        self.physics_enabled
    }

    /// Put a driver's pendulum at `bob` and stop it there, instead of leaving
    /// it hanging under its anchor.
    ///
    /// `bob` is a position in the frame the drivers integrate in — model
    /// units, **Y down** — and both the angular and the spring velocity are
    /// zeroed, so this is a displacement, not a throw. The next tick releases
    /// the pendulum from there; an untouched driver instead snaps straight
    /// down under its world anchor on its first tick, which is the state a
    /// freshly baked puppet is in.
    ///
    /// `false` when `node` is not a `SimplePhysics` node.
    pub fn place_driver(&mut self, node: NodeIdx, bob: Vec2) -> bool {
        let Some(NodeKind::SimplePhysics(p)) = self.arena.get_mut(node).map(|n| &mut n.kind) else {
            return false;
        };
        p.bob = bob;
        p.spring_vel = Vec2::ZERO;
        p.d_angle = 0.0;
        // The anchor snap is a *first tick* behaviour; a caller that placed
        // the bob deliberately must not have it undone.
        p.anchor_initialized = true;
        true
    }

    /// The param slots the links of chain `c` write and its spine reads: the
    /// spine's row of `spine_targets`, reached through the index the bake
    /// left in `arena.chain_spine`. `c` indexes `arena.chain_node_ids`.
    fn chain_slot_row(&self, c: usize) -> Option<&[Option<u32>]> {
        let spine = *self.arena.chain_spine.get(c)?;
        self.spine_targets.get(spine).map(|row| row.as_slice())
    }

    /// The bend each of a chain's links is posed at, in half turns, written
    /// into `out` (cleared first). `c` indexes `arena.chain_node_ids`.
    ///
    /// **The pose, not the resolved value.** A link's spring pulls toward what
    /// the caller posed or the param defaults to, before any driver claim is
    /// folded in — including the chain's own from last frame. Reading the
    /// resolved value here would feed the chain its own output and let a
    /// stiff strand walk its target away one frame at a time.
    ///
    /// A link no param drives is posed at 0, which is the strand as drawn.
    fn chain_posed_bends(&self, c: usize, out: &mut Vec<f32>) {
        out.clear();
        let Some(targets) = self.chain_slot_row(c) else {
            return;
        };
        out.extend(
            targets
                .iter()
                .map(|slot| slot.and_then(|slot| self.param_base(slot)).unwrap_or(0.0)),
        );
    }

    /// Displace every particle of a chain by `offset` and stop it there, so
    /// the ticks that follow are a swing rather than a fixed point — the
    /// chain's [`Self::place_driver`].
    ///
    /// `offset` is in the frame the drivers integrate in: model units, **Y
    /// down**. The anchor particle does not move, because it is pinned to the
    /// node; that is what makes this a bend rather than a translation, and it
    /// is what the rods then pull back.
    ///
    /// A chain that has not hung yet is hung at its stored anchor first, so
    /// the kick lands on a real shape rather than on the origin a fresh bake
    /// leaves. Velocities are zeroed: this is a displacement, not a throw.
    ///
    /// `false` when `node` is not a spine carrying a chain.
    pub fn kick_chain(&mut self, node: NodeIdx, offset: Vec2) -> bool {
        let posed = match self.arena.chain_node_ids.iter().position(|&id| id == node) {
            Some(c) => {
                let mut posed = std::mem::take(&mut self.chain_posed_scratch);
                self.chain_posed_bends(c, &mut posed);
                posed
            }
            None => Vec::new(),
        };
        let Some(NodeKind::Spine(spine)) = self.arena.get_mut(node).map(|n| &mut n.kind) else {
            self.chain_posed_scratch = posed;
            return false;
        };
        let Some(chain) = &mut spine.chain else {
            self.chain_posed_scratch = posed;
            return false;
        };
        if !chain.anchor_initialized || chain.particles.len() != chain.links.len() + 1 {
            let (anchor, carry) = (chain.anchor, chain.carry);
            chain.settle_to_rest(anchor, carry, &posed);
        }
        for particle in chain.particles.iter_mut().skip(1) {
            particle.pos += offset;
            particle.vel = Vec2::ZERO;
        }
        // The velocities are zero but the chain is off its rest pose, so it
        // is about to move: a kick is a move, and `is_at_rest` has to say so
        // before the next tick has run.
        chain.moved_last_tick = offset != Vec2::ZERO;
        self.chain_posed_scratch = posed;
        true
    }

    /// Advance every driver by `dt` seconds against `transforms`, then write
    /// each one's state into its target params.
    ///
    /// Pendulums first, then chains, then one pass that writes both. The two
    /// kinds never read each other's state — only the anchor pose, which this
    /// frame's pre-pass already built — so the split is bookkeeping, not an
    /// order that means anything.
    fn tick_physics(
        &mut self,
        transforms: &GlobalTransforms,
        dt: f32,
        substeps: std::num::NonZeroU8,
    ) -> bool {
        let _span = tracing::trace_span!("tick_physics").entered();
        for i in 0..self.arena.physics_node_ids.len() {
            let id = self.arena.physics_node_ids[i];
            let Some(anchor) = self.arena.physics_anchor(transforms, id) else {
                continue;
            };
            if let Some(NodeKind::SimplePhysics(p)) = self.arena.get_mut(id).map(|n| &mut n.kind) {
                p.tick(anchor, dt);
            }
        }
        let mut posed = std::mem::take(&mut self.chain_posed_scratch);
        // Sample every anchor and target before borrowing the chains.
        // Splitting the ordered arena slots gives disjoint mutable
        // references without unsafe code or moving chains out of nodes.
        type ChainInput = (NodeIdx, Vec2, crate::Mat2, smallvec::SmallVec<[f32; 8]>);
        let mut inputs: smallvec::SmallVec<[ChainInput; 32]> = smallvec::SmallVec::new();
        for i in 0..self.arena.chain_node_ids.len() {
            let id = self.arena.chain_node_ids[i];
            if let (Some(anchor), Some(carry)) = (
                self.arena.physics_anchor(transforms, id),
                self.arena.chain_carry(transforms, id),
            ) {
                self.chain_posed_bends(i, &mut posed);
                inputs.push((id, anchor, carry, posed.iter().copied().collect()));
            }
        }
        let mut jobs: smallvec::SmallVec<[crate::physics::ChainTick<'_>; 32]> =
            smallvec::SmallVec::new();
        let mut tail = self.arena.nodes.as_mut_slice();
        let mut base = 0;
        for (id, anchor, carry, posed) in inputs {
            let index = id.0 as usize;
            let (before, after) = tail.split_at_mut(index - base + 1);
            tail = after;
            base = index + 1;
            if let Some(NodeKind::Spine(spine)) = before.last_mut().map(|n| &mut n.kind) {
                if let Some(chain) = &mut spine.chain {
                    jobs.push(crate::physics::ChainTick {
                        chain,
                        anchor,
                        carry,
                        posed,
                    });
                }
            }
        }
        crate::physics::tick_chains(&mut jobs, dt, substeps);
        drop(jobs);
        self.chain_posed_scratch = posed;
        self.write_driver_param_outputs(transforms)
    }

    /// Map every driver's state into params and claim them with the result:
    /// a pendulum through its map mode, a chain one bend per link. Returns
    /// whether any resolved value moved.
    ///
    /// Pendulums claim at full authority; chains claim their raw bends at
    /// their authored weights. Each link's limit travels with its claim so
    /// `resolve_contributions` can clamp after every driver and the pose have
    /// been blended. Pre-blending each chain and claiming at full authority
    /// would let a zero-weight chain dilute another driver's output.
    fn write_driver_param_outputs(&mut self, transforms: &GlobalTransforms) -> bool {
        self.physics_update_scratch.clear();
        for i in 0..self.arena.physics_node_ids.len() {
            let id = self.arena.physics_node_ids[i];
            let targets = self.physics_targets.get(i).copied().unwrap_or([None, None]);
            if targets == [None, None] {
                continue;
            }
            let Some(NodeKind::SimplePhysics(p)) = self.arena.get(id).map(|n| &n.kind) else {
                continue;
            };
            // local_only: the bob was integrated in the parent's frame already
            // (the anchor is local), so no inverse is needed. Otherwise invert
            // the node's model-local world matrix to rotate the displacement
            // back into the node's own frame.
            let world_inverse = if p.local_only {
                Some(Mat4::IDENTITY)
            } else {
                checked_affine_inverse(transforms.get(id))
            };
            let Some(world_inverse) = world_inverse else {
                continue;
            };
            // Conjugate by the Y-flip so the Y-down integrator's displacement
            // rotates back through the Y-up node frame — the matching half of
            // the flip `Arena::physics_anchor` applies going in.
            let flip = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
            let value = p.param_value(flip * world_inverse * flip);
            self.physics_update_scratch.push((targets, id, value));
        }
        self.chain_update_scratch.clear();
        self.chain_claim_scratch.resize(self.params.len(), None);
        // Moved out so `link_bends` can fill it while the arena is borrowed;
        // it goes straight back, so the allocation is the puppet's for life.
        let mut bends = std::mem::take(&mut self.chain_bends_scratch);
        for c in 0..self.arena.chain_node_ids.len() {
            let id = self.arena.chain_node_ids[c];
            // Copied out rather than borrowed: the claim push below needs the
            // puppet back.
            let targets: smallvec::SmallVec<[Option<u32>; 8]> = match self.chain_slot_row(c) {
                Some(row) if row.iter().any(Option::is_some) => row.iter().copied().collect(),
                _ => continue,
            };
            let Some(NodeKind::Spine(spine)) = self.arena.get(id).map(|n| &n.kind) else {
                continue;
            };
            let Some(chain) = &spine.chain else {
                continue;
            };
            // Same two branches as a pendulum's, and for the same reason: a
            // `local_only` chain already integrated in the parent's frame.
            let world_inverse = if chain.local_only {
                Some(Mat4::IDENTITY)
            } else {
                checked_affine_inverse(transforms.get(id))
            };
            let Some(world_inverse) = world_inverse else {
                continue;
            };
            let flip = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
            chain.link_bends(flip * world_inverse * flip, &mut bends);
            let weight = chain.weight;
            // Copied out beside the bends, for the reason `targets` is: the
            // claim below needs the puppet back, and a link's limit has to
            // travel with the bend it bounds.
            let limits: smallvec::SmallVec<[Option<f32>; 8]> =
                chain.links.iter().map(|l| l.limit).collect();
            let start = self.chain_update_scratch.len();
            for (i, slot) in targets.iter().enumerate() {
                let (Some(slot), Some(&bend)) = (*slot, bends.get(i)) else {
                    continue;
                };
                let Some(at) = self.chain_claim_scratch.get_mut(slot as usize) else {
                    continue;
                };
                let limit = limits
                    .get(i)
                    .copied()
                    .flatten()
                    .filter(|l| l.is_finite() && *l > 0.0);
                if let Some(claim) = at.and_then(|i| self.chain_update_scratch.get_mut(i)) {
                    // A source has one claim per param. Preserve the existing
                    // last-link-wins bend without discarding an earlier wall.
                    claim.value = bend;
                    claim.limit = match (claim.limit, limit) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                    continue;
                }
                *at = Some(self.chain_update_scratch.len());
                self.chain_update_scratch.push(Contribution {
                    source: id,
                    slot,
                    value: bend,
                    weight,
                    limit,
                });
            }
            for claim in &self.chain_update_scratch[start..] {
                if let Some(at) = self.chain_claim_scratch.get_mut(claim.slot as usize) {
                    *at = None;
                }
            }
        }
        self.chain_bends_scratch = bends;
        let mut changed = self.retire_stale_driver_contributions();
        for i in 0..self.physics_update_scratch.len() {
            let (targets, source, value) = self.physics_update_scratch[i];
            // A driver writes two params, in the order its map mode produces
            // them: the pendulum's angle then its length, or whichever pair
            // the mode names.
            for (axis, slot) in targets.into_iter().enumerate() {
                let Some(slot) = slot else { continue };
                let v = if axis == 0 { value.x } else { value.y };
                if self.contribute(slot, source, v, 1.0) {
                    changed = true;
                }
            }
        }
        for i in 0..self.chain_update_scratch.len() {
            if self.record_contribution(self.chain_update_scratch[i]) {
                changed = true;
            }
        }
        changed
    }

    /// Drop claims from drivers that no longer produce one — a driver whose
    /// target was retargeted or cleared by an edit.
    ///
    /// A stale entry keeps full authority rather than merely lingering:
    /// resolution is a mean, so a frozen claim alongside a live one pulls the
    /// param to the midpoint of the two instead of tracking the driver that is
    /// still running.
    ///
    /// **The two questions each entry is asked are answered from sets built
    /// once, not from scans run per entry.** Which nodes are drivers, and
    /// which `(slot, source)` pairs this frame's drivers actually produced:
    /// both are the same for every entry, so the pass costs one walk of the
    /// drivers plus one walk of the claims rather than their product. The
    /// sets are kept between frames so a settled rig allocates nothing.
    fn retire_stale_driver_contributions(&mut self) -> bool {
        let mut drivers = std::mem::take(&mut self.retire_drivers_scratch);
        let mut live = std::mem::take(&mut self.retire_live_scratch);
        drivers.clear();
        live.clear();
        drivers.extend(self.arena.physics_node_ids.iter().copied());
        drivers.extend(self.arena.chain_node_ids.iter().copied());
        for (targets, source, _) in &self.physics_update_scratch {
            for &slot in targets.iter().flatten() {
                live.insert((slot, *source));
            }
        }
        for claim in &self.chain_update_scratch {
            live.insert((claim.slot, claim.source));
        }

        let mut entries = std::mem::take(&mut self.param_contributions);
        let before = entries.len();
        let mut retired: smallvec::SmallVec<[u32; 4]> = smallvec::SmallVec::new();
        entries.retain(|e| {
            if drivers.contains(&e.source) && !live.contains(&(e.slot, e.source)) {
                retired.push(e.slot);
                return false;
            }
            true
        });
        self.param_contributions = entries;
        self.retire_drivers_scratch = drivers;
        self.retire_live_scratch = live;
        if before == self.param_contributions.len() {
            return false;
        }
        // Removing shifts every entry after the first hole, so the index is
        // rebuilt whole. This is the one path that removes anything, and it
        // runs only when an edit retargets a driver.
        self.rebuild_claim_index();
        for slot in retired {
            if let Some(flag) = self.param_contributed.get_mut(slot as usize) {
                *flag = self
                    .slot_claims
                    .get(slot as usize)
                    .is_some_and(|row| !row.is_empty());
            }
            self.bump_param_generation_for_slot(slot);
        }
        true
    }

    /// Bring every driver to its analytic rest pose without simulating, so a
    /// freshly loaded or re-posed model renders settled on its first frame
    /// instead of visibly swinging into place.
    ///
    /// One driver's output can position another's anchor, so a single pass is
    /// not enough: each pass settles every driver against the current anchor
    /// pose, then re-folds the anchor bindings so those outputs propagate.
    /// Each driver's rest state is a fixed point of "anchor → param value →
    /// transforms → anchor", so an acyclic driver graph converges in at most
    /// one pass per driver; the extra pass is what observes the fixed point.
    ///
    /// Leaves the puppet in the anchor pose, with deform stacks cleared and
    /// colour at base — `tick` is what folds a renderable frame, and the
    /// resets here force it to. Render only after ticking.
    pub fn settle_physics(&mut self, model: &Model) {
        let _span = tracing::trace_span!("settle_physics").entered();
        self.sync(model);
        // Every driver of either kind gets a pass, plus the one that observes
        // the fixed point: a chain's anchor can hang off a pendulum's output
        // and vice versa, so the two kinds count into the same budget.
        let n = self.arena.physics_node_ids.len() + self.arena.chain_node_ids.len();
        // Settling writes driver outputs, and a frozen puppet never ticks
        // physics again to refresh or retire them — they would sit on the
        // pose forever, which is the override `set_physics_enabled` removes.
        if n == 0 || !self.physics_enabled {
            return;
        }
        self.arena.ensure_physics_ancestor_mask();
        let mut transforms = std::mem::take(&mut self.arena.physics_transforms);
        let mut posed = std::mem::take(&mut self.chain_posed_scratch);
        let mut settled = false;

        for _ in 0..=n {
            self.reset_frame();
            self.apply_anchor_transform_bindings();
            self.arena.apply_previous_tc_shifts();
            self.arena
                .compute_physics_ancestor_transforms(&mut transforms);

            let mut moved = false;
            for i in 0..self.arena.physics_node_ids.len() {
                let id = self.arena.physics_node_ids[i];
                let Some(anchor) = self.arena.physics_anchor(&transforms, id) else {
                    continue;
                };
                if let Some(NodeKind::SimplePhysics(p)) =
                    self.arena.get_mut(id).map(|n| &mut n.kind)
                {
                    if !p.anchor_initialized || (p.anchor - anchor).length_squared() > SETTLE_EPS_SQ
                    {
                        moved = true;
                    }
                    p.settle_to_rest(anchor);
                }
            }
            for i in 0..self.arena.chain_node_ids.len() {
                let id = self.arena.chain_node_ids[i];
                let Some(anchor) = self.arena.physics_anchor(&transforms, id) else {
                    continue;
                };
                let Some(carry) = self.arena.chain_carry(&transforms, id) else {
                    continue;
                };
                self.chain_posed_bends(i, &mut posed);
                if let Some(NodeKind::Spine(sp)) = self.arena.get_mut(id).map(|n| &mut n.kind) {
                    let Some(c) = &mut sp.chain else { continue };
                    // A turned node is as much a move as a shifted one: it
                    // carries the drawn shape somewhere else, so the rest pose
                    // this pass computes is a different one.
                    if !c.anchor_initialized
                        || (c.anchor - anchor).length_squared() > SETTLE_EPS_SQ
                        // A node that turned, scaled or mirrored is as much
                        // a move as a shifted one: it puts the drawing
                        // somewhere else, so the rest pose this pass computes
                        // is a different one. Both columns are asked, because
                        // either axis alone can carry the change.
                        || (c.carry.x_axis - carry.x_axis).length_squared() > SETTLE_EPS_SQ
                        || (c.carry.y_axis - carry.y_axis).length_squared() > SETTLE_EPS_SQ
                    {
                        moved = true;
                    }
                    c.settle_to_rest(anchor, carry, &posed);
                }
            }
            self.write_driver_param_outputs(&transforms);

            if !moved {
                settled = true;
                break;
            }
        }
        self.arena.physics_transforms = transforms;
        self.chain_posed_scratch = posed;

        if !settled {
            tracing::warn!(
                "physics failed to settle in {} passes; drivers likely form a \
                 dependency cycle. Leaving the last iterate in place.",
                n + 1
            );
        }
    }

    // ---- the tick ---------------------------------------------------------

    /// Evaluate the next frame: drivers step, the pose is folded through the
    /// bindings, and transforms and deforms are resolved. Rebakes first when
    /// `model` has moved since the last one.
    ///
    /// Returns what is still [`Motion`] afterwards, so a viewport knows
    /// whether the next frame would differ from this one.
    pub fn tick(&mut self, model: &Model, dt: f32) -> Motion {
        self.tick_with_root(model, Mat4::IDENTITY, dt)
    }

    /// [`Self::tick`] with `root` folded into the top-level transform, so the
    /// puppet evaluates at an arbitrary world placement. Drivers still sample
    /// the model-local pose, independent of `root`.
    pub fn tick_with_root(&mut self, model: &Model, root: Mat4, dt: f32) -> Motion {
        let _span = tracing::trace_span!("tick").entered();
        self.sync(model);
        let mut out = std::mem::take(&mut self.transforms);
        let mut motion = Motion {
            animation: self.tick_animations(dt),
            physics: false,
        };

        let has_physics = self.physics_enabled && self.has_drivers();
        let mut pre_pass_ran = false;
        let mut anchor_generation = 0;
        if has_physics {
            // Rebuild the anchor pose only when the pose or a driver output
            // moved since the cached one. A skipped frame reads the pose the
            // last final fold left, shift included, and a rebuild replays the
            // stored shift onto the same bindings, so the two agree whenever
            // the pose stood still — see `Arena::apply_previous_tc_shifts`.
            let stale = self.last_anchor_pose_generation != Some(self.param_generation);
            if stale {
                // Capture the generation BEFORE the drivers bump it: if one
                // moves this frame, next frame's staleness check then forces
                // the rebuild that feeds chained physics.
                anchor_generation = self.param_generation;
                pre_pass_ran = true;
                self.reset_frame();
                self.apply_anchor_transform_bindings();
                self.arena.apply_previous_tc_shifts();
                self.arena.ensure_physics_ancestor_mask();
                let mut local = std::mem::take(&mut self.arena.physics_transforms);
                self.arena.compute_physics_ancestor_transforms(&mut local);
                self.arena.physics_transforms = local;
            }
            let local = std::mem::take(&mut self.arena.physics_transforms);
            self.tick_physics(&local, dt, model.physics().chain_substeps);
            self.arena.physics_transforms = local;
            motion.physics = self.drivers_moving();
        }

        let params_changed = self.last_tick_folded_param_generation != Some(self.param_generation);
        let mesh_group_generation_changed =
            self.last_tick_mesh_group_generation != Some(self.mesh_group_param_generation);
        // A pre-pass reset colour and deactivated every deform stack, so the
        // frame that ran it must run the final fold too — otherwise it would
        // render the unposed puppet.
        let needs_final_apply = params_changed || pre_pass_ran;
        let has_mesh_group_work = !self.arena.mesh_group_node_ids.is_empty()
            // A scratch transform moves a node no param names, so the
            // param-relevance cache cannot see it; while one stands, the
            // mesh-group passes run on their own account.
            && (!self.param_mesh_group_relevant.is_empty() || !self.scratch_transforms.is_empty())
            && (needs_final_apply || mesh_group_generation_changed);
        if needs_final_apply {
            self.reset_frame();
            self.apply_params();
            self.apply_scratch_transforms();
            // Transforms BEFORE the propagation, so a mesh group and its
            // children sit where this frame's pose put them.
            self.arena.compute_transforms_with_root(&mut out, root);
            // Spines before mesh groups: a group above a spine reads its
            // children's current deform, so it warps art the spine has bent.
            // Unconditional on having any spine, because `reset_frame` just
            // dropped every spine source and only this rebuilds it.
            if !self.arena.spine_node_ids.is_empty() {
                self.apply_spine_bends();
                self.arena.propagate_spine_deforms(&out);
            }
            if has_mesh_group_work {
                self.arena.propagate_mesh_group_deforms(&mut out, root);
                self.last_tick_mesh_group_generation = Some(self.mesh_group_param_generation);
            }
            self.arena.apply_welds(&out);
            self.arena.combine_deforms();
            self.last_tick_folded_param_generation = Some(self.param_generation);
        } else {
            self.arena.compute_transforms_with_root(&mut out, root);
        }
        // Always consumed, so a stale flag never survives into a later frame.
        let tc_shift_moved = self.arena.take_tc_shift_changed();
        if pre_pass_ran && !tc_shift_moved {
            // Set at the very end: the final fold's reset cleared this, and
            // using the pre-tick generation is what lets a moved driver force
            // next frame's anchor rebuild. Left cleared when the mesh-group
            // pass recorded a shift the pre-pass had not seen, so the next
            // frame's anchors pick it up even if the pose then stands still.
            self.last_anchor_pose_generation = Some(anchor_generation);
        }
        self.transforms = out;
        motion
    }

    /// Whether any driver is away from the rest pose `settle_physics` puts it
    /// in, and so will move again on the next tick.
    fn drivers_moving(&self) -> bool {
        self.arena.physics_node_ids.iter().any(|&id| {
            matches!(
                self.arena.get(id).map(|n| &n.kind),
                Some(NodeKind::SimplePhysics(p)) if !p.is_at_rest(SETTLE_EPS_SQ)
            )
        }) || self.arena.chain_node_ids.iter().any(|&id| {
            matches!(
                self.arena.get(id).map(|n| &n.kind),
                Some(NodeKind::Spine(sp))
                    if sp.chain.as_ref().is_some_and(|c| !c.is_at_rest(SETTLE_EPS_SQ))
            )
        })
    }

    /// Copy this frame's resolved param values onto every spine, one bend per
    /// link, so `crate::spine` can turn the art without reaching for a param.
    ///
    /// A bend is the param's value read straight off — not its normalized
    /// position along the range, which is what a binding's grid is read over.
    /// A link with no target reads zero, which is the art as drawn.
    fn apply_spine_bends(&mut self) {
        let _span = tracing::trace_span!("apply_spine_bends").entered();
        for i in 0..self.arena.spine_node_ids.len() {
            let id = self.arena.spine_node_ids[i];
            let Some(targets) = self.spine_targets.get(i) else {
                continue;
            };
            let bends: smallvec::SmallVec<[f32; 8]> = targets
                .iter()
                .map(|slot| slot.map_or(0.0, |slot| self.resolved(slot)))
                .collect();
            if let Some(NodeKind::Spine(spine)) = self.arena.get_mut(id).map(|n| &mut n.kind) {
                spine.bends.clear();
                spine.bends.extend_from_slice(&bends);
                // A spine whose targets went missing still has joints to turn;
                // the missing ones read as the art.
                spine.bends.resize(spine.joints.len(), 0.0);
            }
        }
    }

    /// Restore the evaluated frame to the model's authored values and clear
    /// every deform stack — the start of a fold.
    fn reset_frame(&mut self) {
        self.last_tick_folded_param_generation = None;
        self.last_anchor_pose_generation = None;
        self.arena.reset_dynamic_state();
        self.arena.reset_deforms();
    }

    /// Fold every binding at the current pose.
    pub fn apply_params(&mut self) {
        let _span = tracing::trace_span!("apply_params").entered();
        self.apply_params_where(|_| true);
    }

    /// Physics pre-pass: build the anchor pose. Stepping a driver reads only
    /// node transforms (the anchors) and each driver's output scale, so
    /// restrict the fold to the targets feeding those. Deform and colour
    /// contributions would be wiped by the reset before the final fold anyway.
    ///
    /// Driver-output params are included at their *last-frame* values, so a
    /// driver whose output transform-binds another driver's anchor moves it —
    /// which is what makes chained physics work. The two-phase pipeline
    /// applies every driver's last-frame output uniformly, so chained drivers
    /// couple with a one-frame delay.
    fn apply_anchor_transform_bindings(&mut self) {
        let _span = tracing::trace_span!("apply_anchor_transform_bindings").entered();
        self.apply_params_where(|t| {
            matches!(
                t,
                BindingTarget::Scalar(
                    ScalarTarget::Tx
                        | ScalarTarget::Ty
                        | ScalarTarget::Sx
                        | ScalarTarget::Sy
                        | ScalarTarget::Rx
                        | ScalarTarget::Ry
                        | ScalarTarget::Rz
                        | ScalarTarget::OutputScaleX
                        | ScalarTarget::OutputScaleY
                )
            )
        });
    }

    fn apply_params_where(&mut self, include: impl Fn(BindingTarget) -> bool) {
        // Normalize each input once; each binding locates it on its own axes.
        for slot in 0..self.params.len() {
            self.located[slot] = self.locate(slot as u32);
        }
        // Move the bindings out so the fold can take `&mut Arena`; they are
        // put back before returning, and nothing here can early-return.
        let bindings = std::mem::take(&mut self.bindings);
        for b in &bindings {
            if !include(b.target) {
                continue;
            }
            self.fold_binding(b);
        }
        self.bindings = bindings;
    }

    /// The current resolved input and its normalized param-range position.
    fn locate(&self, slot: u32) -> Located {
        let Some(p) = self.params.get(slot as usize) else {
            return Located::REST;
        };
        let value = self.resolved(slot);
        let span = p.max - p.min;
        let normed = if span.abs() > 1e-9 {
            ((value.clamp(p.min, p.max) - p.min) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Located {
            value,
            normalized: normed,
            ..Located::REST
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame at 60 Hz.
    const DT: f32 = 1.0 / 60.0;

    /// Two models that have not been edited since they were built sit at the
    /// same generation — every loader hands one back at 0 — so the generation
    /// gate alone would let a puppet tick against the wrong one and read a
    /// tree that is not the one it baked. The identity is what catches it.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "a puppet was driven against a model it was not built from")]
    fn ticking_against_another_model_at_the_same_generation_is_a_programmer_error() {
        let a = Model::new();
        let b = Model::new();
        assert_eq!(a.generation(), b.generation());
        let mut puppet = Puppet::new(&a);
        puppet.tick(&b, 0.0);
    }

    /// A live node edit has to reach everything downstream of the node, not
    /// just the node: an editor dragging a group must show its children
    /// following. And it has to be *only* live — the next tick folds from the
    /// model's authored values, so nothing has to undo it.
    #[test]
    fn a_live_node_edit_moves_the_subtree_and_the_next_tick_folds_it_away() {
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind};

        let mut model = Model::new();
        let mut hex = SeededHex::new(3);
        let root = model.root().expect("a fresh model has one root").clone();
        let parent = model
            .add_node(
                &root,
                ModelNode::new("parent", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add parent");
        let child = model
            .add_node(
                &parent,
                ModelNode::new("child", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add child");

        let mut puppet = Puppet::new(&model);
        puppet.tick(&model, 0.0);
        let parent_idx = puppet.node_idx(&parent).expect("parent baked");
        let child_idx = puppet.node_idx(&child).expect("child baked");
        let at_rest = puppet.transforms().get(child_idx);

        puppet.refold_with_node_edits(|edits| {
            assert!(edits.transform(parent_idx, |t| t.translation.x += 25.0));
        });
        let previewed = puppet.transforms().get(child_idx);
        assert!(
            (previewed.w_axis.x - at_rest.w_axis.x - 25.0).abs() < 1e-4,
            "the child must follow its previewed parent",
        );

        puppet.tick(&model, 0.0);
        assert_eq!(
            puppet.transforms().get(child_idx),
            at_rest,
            "a tick folds from the model, so the preview is gone",
        );
    }

    /// A scratch deform is an edit in progress that only its writer holds, so
    /// the folds that follow it must leave it alone — an editor writes it once
    /// per pointer move, not once per frame, and a frame that folds for its
    /// own reasons must not swallow the drag. It ends when the writer clears
    /// it, or when the model moves and the puppet rebakes.
    #[test]
    fn a_scratch_deform_outlives_a_tick_and_dies_with_the_bake() {
        use crate::formats::clm::{ClmIndices, ClmMesh};
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind, ModelPart};

        let mut model = Model::new();
        let mut hex = SeededHex::new(11);
        let root = model.root().expect("a fresh model has one root").clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh {
                        verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0],
                        uvs: vec![0.0; 6],
                        indices: ClmIndices::U16(vec![0, 1, 2]),
                        origin: [0.0, 0.0],
                    })),
                ),
                &mut hex,
            )
            .expect("add part");

        let mut puppet = Puppet::new(&model);
        puppet.tick(&model, 0.0);
        let idx = puppet.node_idx(&part).expect("part baked");

        let drag = [Vec2::new(3.0, -4.0); 3];
        assert!(puppet.set_scratch_deform(idx, &drag));
        puppet.combine_deforms();
        assert_eq!(puppet.combined_deform(idx), Some(&drag[..]));

        puppet.tick(&model, 0.016);
        assert_eq!(
            puppet.combined_deform(idx),
            Some(&drag[..]),
            "a tick must not swallow an edit in progress",
        );

        // The writer's own clear ends it...
        assert!(puppet.clear_scratch_deform(idx));
        puppet.combine_deforms();
        assert_eq!(puppet.combined_deform(idx), Some(&[Vec2::ZERO; 3][..]));

        // ...and so does the model moving, which rebuilds the stack.
        assert!(puppet.set_scratch_deform(idx, &drag));
        puppet.combine_deforms();
        model
            .update_node(&part, |n| n.z_order = 1.0)
            .expect("edit the part");
        puppet.tick(&model, 0.0);
        assert_eq!(
            puppet.combined_deform(idx),
            Some(&[Vec2::ZERO; 3][..]),
            "a rebake starts from the model, so the drag is gone",
        );
    }

    /// The negative: a clone is the same model, because that is what an undo
    /// snapshot is. Restoring one must not invalidate a puppet.
    #[test]
    fn a_clone_of_the_model_still_drives_the_puppet() {
        let model = Model::new();
        let snapshot = model.clone();
        let mut puppet = Puppet::new(&model);
        puppet.tick(&snapshot, 0.0);
        assert_eq!(puppet.model_identity(), snapshot.identity());
    }

    /// A model's own clips reach the puppet whole, and the puppet says which
    /// one is playing — the two halves a caller needs to hand play state from
    /// one puppet to another.
    #[test]
    fn a_models_clips_reach_the_puppet_and_name_themselves_while_playing() {
        use crate::formats::clm::{ClmAnimation, ClmKeyframe, ClmLane};
        use crate::id::Name;
        use crate::id::SeededHex;
        use crate::interpolate::InterpolateMode;
        use crate::model::ModelParam;

        let mut model = Model::new();
        let mut hex = SeededHex::new(9);
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("blink"),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        model
            .set_animations(vec![ClmAnimation {
                name: "Blink".into(),
                timestep: 1.0 / 60.0,
                length: 4,
                lead_in: -1,
                lead_out: -1,
                lanes: vec![ClmLane {
                    param: param.clone(),
                    interpolation: InterpolateMode::Linear,
                    keyframes: vec![
                        ClmKeyframe {
                            frame: 0,
                            value: 0.0,
                        },
                        ClmKeyframe {
                            frame: 3,
                            value: 1.0,
                        },
                    ],
                }],
            }])
            .expect("install the clip");

        let mut puppet = Puppet::new(&model);
        assert!(
            puppet.animations().is_empty(),
            "clips are installed, not baked"
        );
        puppet.set_animations_from(&model);
        assert_eq!(puppet.animations().len(), 1);
        assert_eq!(puppet.playing_animation(), None, "nothing plays yet");

        assert!(puppet.play_animation("Blink"));
        assert_eq!(puppet.playing_animation(), Some("Blink"));
        let motion = puppet.tick(&model, 3.0 / 60.0);
        assert_eq!(
            puppet.param_value(&param),
            Some(1.0),
            "the lane drove the param it names"
        );
        assert!(motion.animation, "a lane that wrote a param is motion");
        assert!(!motion.physics, "this model has no drivers");

        puppet.stop_animation();
        assert_eq!(puppet.playing_animation(), None);
        assert!(
            !puppet.tick(&model, DT).any(),
            "a stopped clip leaves nothing moving",
        );
    }

    /// A scratch transform is the other half of an edit in progress: the fold
    /// re-derives every node transform from the model, so a drag that is only
    /// applied once is gone by the next frame. It has to outlive the fold,
    /// move everything downstream of the node, and outlive the rebake the
    /// drag's own commit triggers — and then end when, and only when, the
    /// writer says so.
    #[test]
    fn a_scratch_transform_outlives_ticks_and_rebakes_and_dies_with_its_writer() {
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind};

        let mut model = Model::new();
        let mut hex = SeededHex::new(5);
        let root = model.root().expect("a fresh model has one root").clone();
        let parent = model
            .add_node(
                &root,
                ModelNode::new("parent", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add parent");
        let child = model
            .add_node(
                &parent,
                ModelNode::new("child", ModelNodeKind::Group),
                &mut hex,
            )
            .expect("add child");

        let mut puppet = Puppet::new(&model);
        puppet.tick(&model, 0.0);
        let parent_idx = puppet.node_idx(&parent).expect("parent baked");
        let child_idx = puppet.node_idx(&child).expect("child baked");
        let at_rest = puppet.transforms().get(child_idx).w_axis.x;

        let drag = ScratchTransform {
            translation: Some(Vec3::new(25.0, 0.0, 0.0)),
            ..ScratchTransform::default()
        };
        assert!(puppet.set_scratch_transform(parent_idx, drag));
        assert_eq!(puppet.scratch_transform(parent_idx), Some(&drag));

        // Two ticks: the second is the one a memoized fold would skip.
        for _ in 0..2 {
            puppet.tick(&model, 0.016);
            assert!(
                (puppet.transforms().get(parent_idx).w_axis.x - 25.0).abs() < 1e-4,
                "a tick must not swallow an edit in progress",
            );
            assert!(
                (puppet.transforms().get(child_idx).w_axis.x - at_rest - 25.0).abs() < 1e-4,
                "the child must follow its previewed parent",
            );
        }

        // The commit that ends a drag is a model edit; the preview must not
        // flicker away for the frame the rebake lands on.
        model
            .update_node(&child, |n| n.z_order = 1.0)
            .expect("edit the model");
        puppet.tick(&model, 0.0);
        let parent_idx = puppet.node_idx(&parent).expect("parent still baked");
        let child_idx = puppet.node_idx(&child).expect("child still baked");
        assert!(
            (puppet.transforms().get(child_idx).w_axis.x - at_rest - 25.0).abs() < 1e-4,
            "a rebake re-keys the scratch by Id rather than dropping it",
        );

        // The writer's own clear ends it...
        assert!(puppet.clear_scratch_transform(parent_idx));
        assert!(!puppet.clear_scratch_transform(parent_idx), "already gone");
        assert_eq!(puppet.scratch_transform(parent_idx), None);
        puppet.tick(&model, 0.0);
        assert!(
            (puppet.transforms().get(child_idx).w_axis.x - at_rest).abs() < 1e-4,
            "a cleared preview folds away",
        );

        // ...and so does clearing the gesture wholesale.
        assert!(puppet.set_scratch_transform(parent_idx, drag));
        puppet.clear_all_scratch();
        assert_eq!(puppet.scratch_transform(parent_idx), None);
        puppet.tick(&model, 0.0);
        assert!(
            (puppet.transforms().get(child_idx).w_axis.x - at_rest).abs() < 1e-4,
            "clear_all_scratch ends every edit in progress",
        );
    }

    /// A viewport redraws while the puppet moves itself and idles when it does
    /// not, so a tick has to say which it is. A swinging driver is motion; the
    /// rest pose `settle_physics` leaves behind is not.
    #[test]
    fn a_tick_reports_a_swinging_driver_and_stops_once_it_settles() {
        use crate::formats::clm::ClmPhysics;
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind, ModelPhysics};
        use crate::physics::PendulumKind;

        let mut model = Model::new();
        // The bake folds `pixels_per_meter * gravity` into every driver; 1 * 1
        // leaves the node's own `gravity` as the effective one.
        model.set_physics(ClmPhysics {
            pixels_per_meter: 1.0,
            gravity: 1.0,
            ..ClmPhysics::default()
        });
        let mut hex = SeededHex::new(7);
        let root = model.root().expect("a fresh model has one root").clone();
        let mut driver = ModelPhysics::new(PendulumKind::RigidPendulum);
        driver.gravity = 981.0;
        driver.length = 100.0;
        driver.angle_damping = 0.5;
        let node = model
            .add_node(
                &root,
                ModelNode::new("driver", ModelNodeKind::SimplePhysics(driver)),
                &mut hex,
            )
            .expect("add driver");

        let mut puppet = Puppet::new(&model);
        let idx = puppet.node_idx(&node).expect("the driver baked");

        puppet.settle_physics(&model);
        assert!(
            !puppet.tick(&model, DT).any(),
            "a driver at rest under a still anchor moves nothing",
        );

        assert!(puppet.place_driver(idx, Vec2::new(80.0, 60.0)), "displaced");
        let swinging = puppet.tick(&model, DT);
        assert!(swinging.physics, "a displaced pendulum is still swinging");
        assert!(!swinging.animation, "nothing is playing");

        // It keeps saying so while it swings, rather than only on the frame
        // the displacement was written.
        assert!(puppet.tick(&model, DT).physics);

        puppet.settle_physics(&model);
        assert!(
            !puppet.tick(&model, DT).any(),
            "settling ends the motion the displacement started",
        );
    }

    // ---- driver claims ----------------------------------------------------

    /// What `param_contributions` and `slot_claims` promise each other: every
    /// claim is indexed exactly once, under the slot it is a claim on, and
    /// `param_contributed` is the row's emptiness.
    fn claim_index_agrees(puppet: &Puppet) {
        let mut counted = 0;
        for (slot, row) in puppet.slot_claims.iter().enumerate() {
            for &i in row {
                let e = puppet
                    .param_contributions
                    .get(i as usize)
                    .expect("a row points at a claim that exists");
                assert_eq!(
                    e.slot as usize, slot,
                    "slot {slot}'s row points at a claim on slot {}",
                    e.slot,
                );
                counted += 1;
            }
            assert_eq!(
                !row.is_empty(),
                puppet.param_contributed[slot],
                "slot {slot}'s fast-path flag disagrees with its row",
            );
        }
        assert_eq!(
            counted,
            puppet.param_contributions.len(),
            "every claim is indexed exactly once",
        );
    }

    /// One param and two nodes to claim it with. The nodes are plain groups:
    /// a claim is keyed by the node that made it and asks nothing else of it.
    fn two_claimants() -> (Model, Puppet, ParamId, NodeIdx, NodeIdx) {
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind, ModelParam};
        use crate::Name;

        let mut hex = SeededHex::new(3);
        let mut model = Model::new();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("aim"),
                    min: -10.0,
                    max: 10.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let nodes: Vec<NodeId> = ["a", "b"]
            .into_iter()
            .map(|name| {
                model
                    .add_node(&root, ModelNode::new(name, ModelNodeKind::Group), &mut hex)
                    .expect("add group")
            })
            .collect();
        let puppet = Puppet::new(&model);
        let a = puppet.node_idx(&nodes[0]).expect("a baked");
        let b = puppet.node_idx(&nodes[1]).expect("b baked");
        (model, puppet, param, a, b)
    }

    /// Two claims on one param are a weighted mean and not a last-writer-wins,
    /// whichever order they arrive in — the rule the fold is built on, now
    /// that a slot's claims are read off its own row rather than filtered out
    /// of every claim in the puppet.
    #[test]
    fn two_claims_on_one_slot_average() {
        let (_model, mut puppet, param, a, b) = two_claimants();
        puppet.set_param_value(&param, 4.0);

        assert!(puppet.contribute_param_value(&param, a, 0.0, 1.0));
        assert_eq!(puppet.param_value(&param), Some(0.0), "one full claim wins");

        assert!(puppet.contribute_param_value(&param, b, 1.0, 1.0));
        assert_eq!(
            puppet.param_value(&param),
            Some(0.5),
            "two full claims meet in the middle rather than the later winning",
        );
        claim_index_agrees(&puppet);

        // Half authority leaves half the pose showing, and the two rows are
        // still one slot's.
        assert!(puppet.contribute_param_value(&param, b, 1.0, 0.0));
        assert_eq!(puppet.param_value(&param), Some(0.0));
        assert_eq!(puppet.param_contributions.len(), 2, "an upsert, not a push");
        claim_index_agrees(&puppet);
    }

    /// A source that claims twice in one tick has one claim, not two: the
    /// second lands on the first rather than beside it, so it never averages
    /// against itself.
    #[test]
    fn a_claim_made_twice_counts_once() {
        let (_model, mut puppet, param, a, _b) = two_claimants();

        assert!(puppet.contribute_param_value(&param, a, 2.0, 1.0));
        assert!(puppet.contribute_param_value(&param, a, 6.0, 1.0));
        assert_eq!(
            puppet.param_value(&param),
            Some(6.0),
            "the second claim replaced the first instead of averaging with it",
        );
        assert_eq!(puppet.param_contributions.len(), 1);
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");
        assert_eq!(puppet.slot_claims[slot as usize].len(), 1);
        claim_index_agrees(&puppet);

        // A claim that changes nothing says so, and still leaves one entry.
        assert!(!puppet.contribute_param_value(&param, a, 6.0, 1.0));
        assert_eq!(puppet.param_contributions.len(), 1);
        claim_index_agrees(&puppet);
    }

    /// Clearing the claims empties the index with them; a row left behind
    /// would point at claims that are gone.
    #[test]
    fn clearing_the_claims_empties_the_index() {
        let (_model, mut puppet, param, a, b) = two_claimants();
        puppet.set_param_value(&param, 3.0);
        puppet.contribute_param_value(&param, a, 0.0, 1.0);
        puppet.contribute_param_value(&param, b, 1.0, 1.0);
        claim_index_agrees(&puppet);

        puppet.clear_param_contributions();
        assert!(puppet.param_contributions.is_empty());
        assert!(
            puppet.slot_claims.iter().all(|row| row.is_empty()),
            "a cleared claim leaves no row behind",
        );
        claim_index_agrees(&puppet);
        assert_eq!(
            puppet.param_value(&param),
            Some(3.0),
            "and the param is back to what was posed",
        );
    }

    /// A driver that stops naming a param has its claim retired on the next
    /// tick, and the param falls back to the pose. The stale claim would
    /// otherwise keep full authority for ever — see
    /// `retire_stale_driver_contributions`.
    #[test]
    fn a_retargeted_drivers_claim_retires_and_the_pose_comes_back() {
        use crate::formats::clm::ClmPhysics;
        use crate::id::SeededHex;
        use crate::model::{ModelNode, ModelNodeKind, ModelParam, ModelPhysics};
        use crate::physics::PendulumKind;
        use crate::Name;

        let mut model = Model::new();
        model.set_physics(ClmPhysics {
            pixels_per_meter: 1.0,
            gravity: 1.0,
            ..ClmPhysics::default()
        });
        let mut hex = SeededHex::new(13);
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("swing"),
                    min: -10.0,
                    max: 10.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let mut driver = ModelPhysics::new(PendulumKind::RigidPendulum);
        driver.gravity = 981.0;
        driver.length = 100.0;
        driver.angle_damping = 0.5;
        let node = model
            .add_node(
                &root,
                ModelNode::new("driver", ModelNodeKind::SimplePhysics(driver)),
                &mut hex,
            )
            .expect("add driver");
        model
            .set_physics_targets(&node, [Some(param.clone()), None])
            .expect("aim the driver");

        let mut puppet = Puppet::new(&model);
        puppet.set_param_value(&param, 5.0);
        let idx = puppet.node_idx(&node).expect("the driver baked");
        assert!(puppet.place_driver(idx, Vec2::new(80.0, 60.0)), "displaced");
        puppet.tick(&model, DT);
        assert_eq!(puppet.param_contributions.len(), 1, "the driver claimed");
        claim_index_agrees(&puppet);
        assert_ne!(
            puppet.param_value(&param),
            Some(5.0),
            "and the claim is what the param reads, not the pose",
        );

        // Aim it at nothing: the claim it left behind is stale the moment the
        // next tick finds the driver producing none.
        model
            .set_physics_targets(&node, [None, None])
            .expect("unaim the driver");
        puppet.tick(&model, DT);
        assert!(
            puppet.param_contributions.is_empty(),
            "the stale claim retired: {:?}",
            puppet.param_contributions,
        );
        claim_index_agrees(&puppet);
        assert_eq!(
            puppet.param_value(&param),
            Some(5.0),
            "and the param is back to what was posed",
        );
    }

    // ---- the anchor and the translate-children shift ----------------------

    /// A `translate_children` mesh group over a driver, built so the group's
    /// deform shifts the driver's anchor by exactly `+SHIFT` in x.
    ///
    /// `local_only` picks which of the two anchor sources is under test: with
    /// it the driver hangs straight off the group and is itself the shift
    /// target, reading its own `transform.translation`; without it the driver
    /// hangs off a plain group that is the target, and reads its world
    /// position out of the pre-pass transforms.
    fn tc_over_driver(local_only: bool) -> (Model, NodeId, ParamId) {
        use crate::formats::clm::{ClmIndices, ClmMesh, ClmPhysics};
        use crate::id::SeededHex;
        use crate::model::{
            BindingKey, ModelMeshGroup, ModelNode, ModelNodeKind, ModelParam, ModelPhysics,
        };
        use crate::physics::PendulumKind;

        let quad = ClmMesh {
            verts: vec![-50.0, -50.0, 50.0, -50.0, 50.0, 50.0, -50.0, 50.0],
            uvs: vec![0.0; 8],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        };

        let mut model = Model::new();
        model.set_physics(ClmPhysics {
            pixels_per_meter: 1.0,
            gravity: 1.0,
            ..ClmPhysics::default()
        });
        let mut hex = SeededHex::new(23);
        let root = model.root().expect("a fresh model has one root").clone();

        let mg = model
            .add_node(
                &root,
                ModelNode::new(
                    "mg",
                    ModelNodeKind::MeshGroup(ModelMeshGroup::new(quad.clone())),
                ),
                &mut hex,
            )
            .expect("add the mesh group");

        // Without `local_only` the driver sits one group below the target, so
        // the shift reaches it through its parent's global rather than through
        // its own transform.
        let driver_parent = if local_only {
            mg.clone()
        } else {
            model
                .add_node(
                    &mg,
                    ModelNode::new("carrier", ModelNodeKind::Group),
                    &mut hex,
                )
                .expect("add the carrier")
        };
        let mut physics = ModelPhysics::new(PendulumKind::RigidPendulum);
        physics.local_only = local_only;
        physics.gravity = 981.0;
        physics.length = 100.0;
        let driver = model
            .add_node(
                &driver_parent,
                ModelNode::new("driver", ModelNodeKind::SimplePhysics(physics)),
                &mut hex,
            )
            .expect("add the driver");

        let bend = ParamId::new("bend").expect("a valid param Id");
        model
            .add_param_with_id(
                bend.clone(),
                ModelParam::new(crate::id::Name::truncated("Bend"), 0.0, 1.0, 0.0),
            )
            .expect("add the param");
        let rest_key = BindingKey::new(bend.clone(), mg.clone(), BindingTarget::Deform);
        model.add_binding(&rest_key).unwrap();
        model.reset_binding_key(&rest_key, [0, 0]).unwrap();
        // Every lattice vertex moves the same way, so the warp is a pure
        // translation and the shift is `SHIFT` wherever the target sits.
        model
            .set_deform_vertices(
                &BindingKey::new(bend.clone(), mg.clone(), BindingTarget::Deform),
                [1, 0],
                vec![SHIFT, 0.0, SHIFT, 0.0, SHIFT, 0.0, SHIFT, 0.0],
            )
            .expect("author the deform");

        (model, driver, bend)
    }

    /// How far the mesh group's deform moves its targets, in x.
    const SHIFT: f32 = 20.0;

    /// The x of the anchor the driver last sampled. `physics_anchor` flips y,
    /// so x is the axis that reads straight.
    fn anchor_x(puppet: &Puppet, driver: NodeIdx) -> f32 {
        match puppet.arena.get(driver).map(|n| &n.kind) {
            Some(NodeKind::SimplePhysics(p)) => p.anchor.x,
            _ => panic!("the driver is a SimplePhysics node"),
        }
    }

    /// The anchor pre-pass runs before the mesh groups, so the shift a
    /// `translate_children` group applies cannot be in the pose it samples.
    /// Rather than run the mesh-group pass twice a frame, the pre-pass replays
    /// the shift the last one recorded — so the anchor follows one frame late,
    /// and then holds still.
    ///
    /// This is the world-anchored half: the driver reads its own global out of
    /// the pre-pass transforms, and the node the group shifts is its parent.
    #[test]
    fn a_world_anchor_follows_the_shift_one_frame_late() {
        let (model, driver, bend) = tc_over_driver(false);
        let mut puppet = Puppet::new(&model);
        let idx = puppet.node_idx(&driver).expect("the driver baked");

        puppet.tick(&model, DT);
        assert!(
            anchor_x(&puppet, idx).abs() < 1e-4,
            "at rest the anchor is the authored spot, not {}",
            anchor_x(&puppet, idx),
        );

        puppet.set_param_value(&bend, 1.0);
        puppet.tick(&model, DT);
        assert!(
            anchor_x(&puppet, idx).abs() < 1e-4,
            "the frame that first shifts must still anchor at the old spot, not {}",
            anchor_x(&puppet, idx),
        );

        puppet.tick(&model, DT);
        assert!(
            (anchor_x(&puppet, idx) - SHIFT).abs() < 1e-4,
            "the next frame anchors at the shifted spot, not {}",
            anchor_x(&puppet, idx),
        );

        // And it stays there: a still pose lets the pre-pass be skipped, and a
        // skipped frame must sample the same anchor a fresh one would.
        puppet.tick(&model, DT);
        assert!(
            (anchor_x(&puppet, idx) - SHIFT).abs() < 1e-4,
            "a skipped pre-pass must not pop the anchor back to {}",
            anchor_x(&puppet, idx),
        );
    }

    /// The `local_only` half of
    /// [`a_world_anchor_follows_the_shift_one_frame_late`]: the driver is
    /// itself the group's shift target and reads its own
    /// `transform.translation`, which the pre-pass leaves pre-shift and the
    /// final fold leaves post-shift. The replayed delta is what makes a
    /// skipped frame and a fresh pre-pass agree.
    #[test]
    fn a_local_anchor_follows_the_shift_one_frame_late() {
        let (model, driver, bend) = tc_over_driver(true);
        let mut puppet = Puppet::new(&model);
        let idx = puppet.node_idx(&driver).expect("the driver baked");

        puppet.tick(&model, DT);
        assert!(
            anchor_x(&puppet, idx).abs() < 1e-4,
            "at rest the anchor is the authored spot, not {}",
            anchor_x(&puppet, idx),
        );

        puppet.set_param_value(&bend, 1.0);
        puppet.tick(&model, DT);
        assert!(
            anchor_x(&puppet, idx).abs() < 1e-4,
            "the frame that first shifts must still anchor at the old spot, not {}",
            anchor_x(&puppet, idx),
        );

        puppet.tick(&model, DT);
        assert!(
            (anchor_x(&puppet, idx) - SHIFT).abs() < 1e-4,
            "the next frame anchors at the shifted spot, not {}",
            anchor_x(&puppet, idx),
        );

        puppet.tick(&model, DT);
        assert!(
            (anchor_x(&puppet, idx) - SHIFT).abs() < 1e-4,
            "a skipped pre-pass must not pop the anchor back to {}",
            anchor_x(&puppet, idx),
        );
    }
}
