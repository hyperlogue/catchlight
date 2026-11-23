//! A puppet: a model being animated.
//!
//! A [`Puppet`] holds everything animating a [`Model`] produces and a model
//! never does — the pose, the drivers' state and the evaluated frame — so one
//! model can back many puppets and posing one never touches the model.
//!
//! Invariants this module enforces:
//!
//! - Each tick synchronizes the model generation, advances animation, samples
//!   the pose for motion responses and evaluates the responses parent first.
//!   The final fold reads their outputs before applying spines, mesh groups
//!   and welds. An unchanged pose can reuse the previous fold.
//! - A rebake resolves surviving poses and scratch transforms by Id. Response
//!   state survives when its configuration and upstream dependencies match.
//!   Index-based caches are rebuilt for the new arena.
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
//! - A tick reports whether animation or a motion response is still moving.
//!
//! A part carries its albedo as an index into
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
mod response;
pub use response::{ResponseClock, ResponseInputSample, ResponseTrace};

pub use arena::GlobalTransforms;

pub(crate) use arena::Arena;
pub(crate) use bake::BakeMemo;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use glam::{Mat4, Vec2, Vec3};

use crate::animation::AnimationPlayState;
use crate::components::{Node, NodeIdx, NodeKind};
use crate::deform::DeformSource;
use crate::formats::clm::ClmAnimation;
use crate::id::{NodeId, ParamId};
use crate::model::{Model, Pose};
use crate::node::NodeTree;

use bake::{Baked, BakedBinding, BakedParam};

/// One driver's weighted claim on a param. `weight` is authority, at or above
/// zero: 0 asserts nothing, 1 fully specifies the value, and larger weights
/// only matter relative to whatever else claims the param.
#[derive(Debug, Clone, Copy)]
struct Contribution {
    slot: u32,
    source: NodeIdx,
    value: f32,
    weight: f32,
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
fn resolve_contributions(entries: &[Contribution], claims: &[u32], base: f32) -> f32 {
    let mut total = 0.0;
    let mut sum = 0.0;
    for &i in claims {
        let Some(e) = entries.get(i as usize) else {
            continue;
        };
        total += e.weight;
        sum += e.value * e.weight;
    }
    if total <= 0.0 {
        base
    } else if total < 1.0 {
        sum + base * (1.0 - total)
    } else {
        sum / total
    }
}

/// A live edit to one node's properties, held by the puppet and re-applied by
/// every fold — the transform half of an edit in progress, exactly as a
/// scratch deform is its per-vertex half.
///
/// It reaches the three properties a gesture drags: the local transform, the
/// z order and the opacity. Anything more belongs in a model edit. Every
/// field is optional and `None` means *leave what the fold produced*, so a
/// drag that only translates does not also pin the rotation a binding is
/// animating.
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
    /// A driver still has motion to simulate or display at the current pose.
    pub motion_responses: bool,
    /// A playing animation wrote a param this tick.
    pub animation: bool,
}

impl Motion {
    /// Whether anything at all is moving.
    pub fn any(self) -> bool {
        self.motion_responses || self.animation
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
/// Cloning one is how a caller forks an animation. Everything a bake derives
/// from the model — meshes, mesh-group and spine pins, the tree, the binding
/// grids, the Id tables — is held through `Arc`, so the clone shares it with
/// the original and copies only the frame: the pose, the drivers' state and
/// each node's evaluated transform, colour and deform stack.
#[derive(Clone)]
pub struct Puppet {
    /// The `Model::identity` of the model this puppet animates.
    model_identity: u64,
    /// The `Model::generation` this puppet is baked against.
    baked_generation: u64,
    arena: Arena,
    transforms: GlobalTransforms,

    node_of_id: Arc<HashMap<NodeId, NodeIdx>>,
    /// Indexed by `NodeIdx.0`, so the arena's DFS order is the Id order.
    id_of_node: Arc<Vec<NodeId>>,

    params: Arc<Vec<BakedParam>>,
    slot_of_param: Arc<HashMap<ParamId, u32>>,
    /// Posed value per param slot; `None` reads the param's default, which is
    /// the state `clear_param_value` leaves.
    param_values: Vec<Option<f32>>,
    /// Values posed for params this model does not (yet) have. Kept off the
    /// dense path so it never costs the hot loops, and promoted by the next
    /// rebake that gives the Id a slot.
    param_values_overflow: HashMap<ParamId, f32>,
    /// One claim per (param, source), indexed by slot in `slot_claims`.
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
    /// fold to one bool load for the overwhelming majority of
    /// params, which have no contributor at all.
    param_contributed: Vec<bool>,
    param_generation: u64,
    last_tick_folded_param_generation: Option<u64>,

    bindings: Arc<Vec<BakedBinding>>,
    // True for slots that can change a mesh group's input pose. Ordinary
    // writes and driver claims invalidate the same baked slot.
    mesh_group_inputs: Arc<Vec<bool>>,
    mesh_groups_dirty: bool,
    responses: response::Responses,
    motion_responses_enabled: bool,
    spine_targets: Arc<Vec<Vec<Option<u32>>>>,

    animations: Arc<Vec<ClmAnimation>>,
    wrap_cuts: Vec<ParamId>,
    play_state: Option<AnimationPlayState>,
    sampled_animation: Option<(AnimationPlayState, f32)>,

    /// Live node edits, re-applied by every fold. Sparse rather than one entry
    /// per node: a drag holds one or two, and every fold pays for a lookup per
    /// entry, not per node.
    scratch_transforms: HashMap<NodeIdx, ScratchTransform>,

    /// Reused by the fold: resolved and normalized inputs by param slot.
    located: Vec<Located>,
}

/// The actual fractional clip position sampled by one completed animation tick.
/// `time` is the runtime's f32 playback clock in seconds; it can wrap while an
/// export frame index continues increasing. `index` addresses Puppet::animations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnimationSample<'a> {
    pub index: usize,
    pub name: &'a str,
    pub timestep: f32,
    pub time: f32,
    pub frame: f32,
    pub looping: bool,
}

impl Puppet {
    /// A puppet of `model` at rest, recording the generation it was baked at.
    ///
    /// The model keeps what the first puppet of a generation bakes, so this
    /// bakes once per generation however many puppets are built: the others
    /// share the baked tables and pay only for a frame of their own.
    pub fn new(model: &Model) -> Self {
        let mut puppet = Self {
            model_identity: model.identity(),
            baked_generation: model.generation(),
            arena: Arena::new(),
            transforms: GlobalTransforms::new(),
            node_of_id: Arc::default(),
            id_of_node: Arc::default(),
            params: Arc::default(),
            slot_of_param: Arc::default(),
            param_values: Vec::new(),
            param_values_overflow: HashMap::new(),
            param_contributions: Vec::new(),
            slot_claims: Vec::new(),
            param_contributed: Vec::new(),
            param_generation: 0,
            last_tick_folded_param_generation: None,
            bindings: Arc::default(),
            mesh_group_inputs: Arc::default(),
            mesh_groups_dirty: true,
            responses: response::Responses::default(),
            motion_responses_enabled: true,
            spine_targets: Arc::default(),
            animations: Arc::default(),
            wrap_cuts: Vec::new(),
            play_state: None,
            sampled_animation: None,
            scratch_transforms: HashMap::new(),
            located: Vec::new(),
        };
        puppet.install(model.bake_memo().get_or_bake(model));
        puppet.responses = response::Responses::bake(&puppet, model, Default::default());
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
    /// animates one model, and handing it another is a programmer error.
    /// Debug builds assert on it. A release build does not check: it rebakes
    /// against whatever it is handed, because a different model never shares
    /// a generation, and keeps reporting the identity it was built with.
    ///
    /// Driver claims are not carried across: motion responses are their only
    /// source, and the rebaked responses publish again on their next step.
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
        let scratch: Vec<(NodeId, ScratchTransform)> = self
            .scratch_transforms
            .iter()
            .filter_map(|(idx, edit)| Some((self.id_of_node.get(idx.0 as usize)?.clone(), *edit)))
            .collect();

        let previous = std::mem::take(&mut self.responses);
        self.install(model.bake_memo().get_or_bake(model));
        self.responses = response::Responses::bake(self, model, previous);
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
        self.spine_targets = spine_targets;
        self.bake_mesh_group_inputs();
        self.param_values_overflow.clear();
        self.param_contributions.clear();
        // Keyed by slot, and the slots have moved. `sync` re-keys the entries
        // it saved by Id and puts them back; a bare `install` has none.
        self.scratch_transforms.clear();
        self.transforms = GlobalTransforms::new();
        self.param_generation = self.param_generation.wrapping_add(1);
        self.last_tick_folded_param_generation = None;
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
        if let Some(sampler) = &mut self.responses.sampler {
            sampler.set_scratch_deform(id, offsets);
        }
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
        self.invalidate_fold();
        true
    }

    pub fn clear_scratch_deform(&mut self, id: NodeIdx) -> bool {
        if let Some(sampler) = &mut self.responses.sampler {
            sampler.clear_scratch_deform(id);
        }
        let Some(node) = self.arena.get_mut(id) else {
            return false;
        };
        match &mut node.kind {
            NodeKind::Part(p) => p.deform_stack.clear_source(DeformSource::Scratch),
            NodeKind::MeshGroup(mg) => mg.deform_stack.clear_source(DeformSource::Scratch),
            _ => return false,
        }
        self.invalidate_fold();
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
        self.mesh_groups_dirty = true;
        self.last_tick_folded_param_generation = None;
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
                self.bump_param_generation(slot);
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
                    self.bump_param_generation(slot);
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

    /// Record `source`'s weighted claim on a param slot, replacing that
    /// source's previous one. Returns whether the resolved value moved.
    ///
    /// Motion responses are the only claimants: each publishes its output
    /// here, and two responses aimed at one param meet as a weighted mean.
    fn contribute(&mut self, slot: u32, source: NodeIdx, value: f32, weight: f32) -> bool {
        self.record_contribution(Contribution {
            slot,
            source,
            value,
            weight,
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
        self.bump_param_generation(slot);
        true
    }

    /// Drop every driver claim, restoring each param to what was posed.
    fn clear_param_contributions(&mut self) {
        if self.param_contributions.is_empty() {
            return;
        }
        for e in std::mem::take(&mut self.param_contributions) {
            if let Some(row) = self.slot_claims.get_mut(e.slot as usize) {
                row.clear();
            }
        }
        self.mesh_groups_dirty = true;
        self.param_generation = self.param_generation.wrapping_add(1);
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

    /// Geometry depends on ancestor transforms and on any guide read by an
    /// ancestor spine, including guides in a sibling branch.
    fn geometry_ancestors(&self, seeds: impl IntoIterator<Item = NodeIdx>) -> HashSet<NodeIdx> {
        let mut nodes = HashSet::new();
        let mut pending: Vec<_> = seeds.into_iter().collect();
        while let Some(id) = pending.pop() {
            if !nodes.insert(id) {
                continue;
            }
            pending.extend(self.tree().get_parent(id));
            if let Some(NodeKind::Spine(spine)) = self.get(id).map(|n| &n.kind) {
                pending.extend(spine.guide_part());
            }
        }
        nodes
    }

    fn geometry_inputs(&self) -> impl Iterator<Item = (NodeIdx, u32)> + '_ {
        let bindings = self
            .bindings
            .iter()
            .filter(|b| b.target.moves_geometry())
            .flat_map(|b| std::iter::once(b.x).chain(b.y).map(move |p| (b.node, p)));
        let bends = self
            .arena
            .spine_node_ids
            .iter()
            .zip(self.spine_targets.iter())
            .flat_map(|(&node, params)| params.iter().flatten().map(move |&p| (node, p)));
        bindings.chain(bends)
    }

    fn bake_mesh_group_inputs(&mut self) {
        self.mesh_groups_dirty = true;
        let mut inputs = vec![false; self.params.len()];
        if !self.arena.mesh_group_node_ids.is_empty() {
            let mut related =
                self.geometry_ancestors(self.arena.mesh_group_node_ids.iter().copied());
            for &group in &self.arena.mesh_group_node_ids {
                related.extend(self.tree().get_all_descendants(group));
            }
            for (node, slot) in self.geometry_inputs() {
                if related.contains(&node) {
                    inputs[slot as usize] = true;
                }
            }
        }
        self.mesh_group_inputs = Arc::new(inputs);
    }

    fn bump_param_generation(&mut self, slot: u32) {
        self.mesh_groups_dirty |= self
            .mesh_group_inputs
            .get(slot as usize)
            .copied()
            .unwrap_or(false);
        self.param_generation = self.param_generation.wrapping_add(1);
    }

    // ---- animations -------------------------------------------------------

    pub fn animations(&self) -> &[ClmAnimation] {
        &self.animations
    }

    /// Replace the animations this puppet can play with clips a caller built
    /// by hand. [`Self::set_animations_from`] is the one that takes the
    /// model's own.
    pub fn set_animations(&mut self, animations: Vec<ClmAnimation>) {
        self.animations = Arc::new(animations);
        // Any play state indexes into the old list.
        self.play_state = None;
        self.sampled_animation = None;
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
            // Loop continuity depends only on the chosen clip. Store IDs so
            // a model rebake can assign new parameter slots during playback.
            self.wrap_cuts.clear();
            let clip = &self.animations[index];
            if clip.length > 0 {
                let (begin, end) = clip.loop_region();
                self.wrap_cuts.extend(
                    clip.lanes
                        .iter()
                        .filter(|lane| lane.jumps_at_wrap(begin, end))
                        .map(|lane| lane.param.clone()),
                );
            }
            self.sampled_animation = None;
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
        self.sampled_animation = None;
    }

    /// The clip sample used by the most recent animation tick, after native
    /// loop/lead-region wrapping and clamping. Reading never advances playback.
    /// Starting, stopping or replacing playback clears this until a tick samples
    /// the new selection, so callers cannot mistake a scheduled clip for a frame
    /// already evaluated. A zero-dt tick records frame zero normally.
    pub fn animation_sample(&self) -> Option<AnimationSample<'_>> {
        let (state, frame) = self.sampled_animation?;
        let clip = self.animations.get(state.index)?;
        Some(AnimationSample {
            index: state.index,
            name: &clip.name,
            timestep: clip.timestep,
            time: state.time,
            frame,
            looping: state.looping,
        })
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
        if dt < 0. {
            self.responses.reset();
        }
        let mut jumps = smallvec::SmallVec::<[u32; 4]>::new();
        // Start after sync so a binding edit just before play uses the new
        // slot dependencies. A restart at the current sample preserves velocity.
        if self.sampled_animation.is_none() {
            jumps = anim
                .lanes
                .iter()
                .filter_map(|lane| {
                    let &slot = self.slot_of_param.get(&lane.param)?;
                    let p = self.params.get(slot as usize)?;
                    (!crate::animation::same_sample(
                        self.param_base(slot)?,
                        lane.value_at(0.),
                        p.min.abs().max(p.max.abs()),
                    ))
                    .then_some(slot)
                })
                .collect();
        }
        state.time += dt;
        // A looping animation snaps back to the loop region's start (the
        // lead-in plays once), and playback always clamps to the last frame.
        if anim.length > 0 && anim.timestep > 0.0 {
            let (loop_begin, loop_end) = anim.loop_region();
            let frame = (state.time / anim.timestep).round() as i64;
            if state.looping && frame >= loop_end as i64 {
                jumps.extend(
                    self.wrap_cuts
                        .iter()
                        .filter_map(|p| self.slot_of_param.get(p).copied()),
                );
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
        self.responses.reset_changed_inputs(&jumps);
        self.sampled_animation = Some((state, frame));
        // Hold the list by its own handle so the writes below can take
        // `&mut self`.
        let animations = Arc::clone(&self.animations);
        let mut changed = false;
        if let Some(anim) = animations.get(state.index) {
            for lane in &anim.lanes {
                if self.motion_responses_enabled && self.reserved_output(&lane.param) {
                    continue;
                }
                let value = lane.value_at(frame);
                if self.param_value_posed(&lane.param) != Some(value) {
                    self.set_param_value(&lane.param, value);
                    changed = true;
                }
            }
        }
        self.play_state = Some(state);
        changed
    }

    /// The value posed for a param, ignoring driver claims — what a lane
    /// compares against before writing. `None` means use the model default.
    /// This observes requested controls without reconstructing animation lanes.
    pub fn param_value_posed(&self, param: &ParamId) -> Option<f32> {
        match self.slot_of_param.get(param) {
            Some(&slot) => self.param_values.get(slot as usize).copied().flatten(),
            None => self.param_values_overflow.get(param).copied(),
        }
    }

    // ---- drivers ----------------------------------------------------------

    /// Disable motion responses to release their output params and evaluate
    /// only the authored pose. The editor uses this for its zero-time preview.
    pub fn set_motion_responses_enabled(&mut self, enabled: bool) {
        self.motion_responses_enabled = enabled;
        if !enabled {
            // Release the last output so authoring edits take effect immediately.
            self.clear_param_contributions();
            self.responses.reset();
        }
    }

    pub fn motion_responses_enabled(&self) -> bool {
        self.motion_responses_enabled
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
        let motion = Motion {
            animation: self.tick_animations(dt),
            motion_responses: self.advance_responses(model, dt, false),
        };
        let params_changed = self.last_tick_folded_param_generation != Some(self.param_generation);
        if params_changed {
            self.fold_frame(&mut out, root);
        } else {
            self.arena.compute_transforms_with_root(&mut out, root);
        }
        self.transforms = out;
        motion
    }

    /// Fold resolved params into transforms and geometry, then apply welds.
    fn fold_frame(&mut self, out: &mut GlobalTransforms, root: Mat4) {
        self.reset_frame();
        self.apply_params();
        self.apply_scratch_transforms();
        // Transforms BEFORE the propagation, so a mesh group and its
        // children sit where this frame's pose put them.
        self.arena.compute_transforms_with_root(out, root);
        // Spines before mesh groups: a group above a spine reads its
        // children's current deform, so it deforms art the spine has bent.
        // Unconditional on having any spine, because `reset_frame` just
        // dropped every spine source and only this rebuilds it.
        if !self.arena.spine_node_ids.is_empty() {
            self.apply_spine_bends();
            self.arena.propagate_spine_deforms(out);
        }
        if !self.arena.mesh_group_node_ids.is_empty() {
            if self.mesh_groups_dirty {
                self.arena.propagate_mesh_group_deforms(out, root);
                self.mesh_groups_dirty = false;
            } else {
                self.arena.restore_mesh_group_deforms(out, root);
            }
        }
        self.arena.apply_welds(out);
        self.arena.combine_deforms();
        self.last_tick_folded_param_generation = Some(self.param_generation);
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
        self.arena.reset_dynamic_state();
        self.arena.reset_deforms();
    }

    /// Fold every binding at the current pose. Additive on top of whatever
    /// the arena holds, so it only ever follows `reset_frame`.
    pub(crate) fn apply_params(&mut self) {
        let _span = tracing::trace_span!("apply_params").entered();
        // Normalize each input once; each binding locates it on its own axes.
        for slot in 0..self.params.len() {
            self.located[slot] = self.locate(slot as u32);
        }
        // Hold the bindings by their own handle so the fold can take
        // `&mut Arena`.
        let bindings = Arc::clone(&self.bindings);
        for b in bindings.iter() {
            self.fold_binding(b);
        }
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

    #[test]
    #[allow(clippy::unwrap_used)]
    fn unrelated_input_reuses_the_group_deforms_and_shifts_of_the_posed_model() {
        let mut model = Model::from_clm_bytes(
            &std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/models/lan.clm"
            ))
            .unwrap(),
        )
        .unwrap();
        let control = ParamId::new("visibility").unwrap();
        model
            .add_param_with_id(
                control.clone(),
                crate::ModelParam::new(crate::Name::truncated("Visibility"), 0., 1., 1.),
            )
            .unwrap();
        let part = model
            .node_ids()
            .find(|id| {
                matches!(
                    model.node(id).map(|n| &n.kind),
                    Some(crate::ModelNodeKind::Part(_))
                )
            })
            .unwrap()
            .clone();
        let opacity = crate::BindingKey::new(
            control.clone(),
            part.clone(),
            crate::BindingTarget::Scalar(crate::ScalarTarget::Opacity),
        );
        model.set_binding_key(&opacity, [0, 0], 0.).unwrap();
        let mut cached = Puppet::new(&model);
        cached.set_motion_responses_enabled(false);
        cached.set_param_value(&ParamId::new("head_roll").unwrap(), 20.);
        cached.tick(&model, 0.);
        assert!(!cached.arena.mg_written_sources.is_empty());
        cached.set_param_value(&control, 0.5);
        assert!(!cached.mesh_groups_dirty);
        let mut reference = cached.clone();
        reference.invalidate_fold();
        cached.tick(&model, 0.);
        reference.tick(&model, 0.);
        for &id in &cached.arena.deform_node_ids {
            assert_eq!(cached.transforms.get(id), reference.transforms.get(id));
            if let (Some(NodeKind::Part(a)), Some(NodeKind::Part(b))) = (
                cached.get(id).map(|n| &n.kind),
                reference.get(id).map(|n| &n.kind),
            ) {
                assert_eq!(a.deform_stack.combined(), b.deform_stack.combined());
                assert_eq!(a.opacity, b.opacity);
            }
        }
    }

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
        assert!(!motion.motion_responses, "this model has no drivers");

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

    /// One bake per generation, however many puppets: the model keeps the
    /// first puppet's bake and the next is built from it. An edit drops it,
    /// and the puppets that follow the edit share the new one.
    #[test]
    fn puppets_of_one_generation_share_a_bake() {
        use crate::id::SeededHex;
        use crate::model::ScalarTarget;
        use crate::model::{BindingKey, BindingTarget, ModelNode, ModelNodeKind, ModelParam};
        use crate::Name;

        let mut hex = SeededHex::new(5);
        let mut model = Model::new();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("slide"),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let group = model
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .expect("add group");
        let key = BindingKey::new(
            param.clone(),
            group.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        model.set_binding_key(&key, [0, 0], 0.0).expect("key");
        model.set_binding_key(&key, [1, 0], 40.0).expect("key");

        let shared = |a: &Puppet, b: &Puppet| {
            Arc::ptr_eq(&a.bindings, &b.bindings)
                && Arc::ptr_eq(&a.arena.tree, &b.arena.tree)
                && Arc::ptr_eq(&a.node_of_id, &b.node_of_id)
        };
        let mut first = Puppet::new(&model);
        let mut second = Puppet::new(&model);
        assert!(shared(&first, &second));
        // A snapshot is the same state, and brings the bake with it.
        let snapshot = model.clone();
        assert!(shared(&first, &Puppet::new(&snapshot)));

        // The frame is each puppet's own.
        let idx = first.node_idx(&group).expect("group baked");
        let tx = |p: &Puppet| p.get(idx).expect("node").transform.translation.x;
        first.set_param_value(&param, 1.0);
        first.tick(&model, 0.0);
        second.tick(&model, 0.0);
        assert_eq!((tx(&first), tx(&second)), (40.0, 0.0));

        // An edit is a new state: nothing baked before it is handed out.
        model.set_binding_key(&key, [1, 0], 10.0).expect("key");
        let after = Puppet::new(&model);
        assert!(!shared(&first, &after));
        first.sync(&model);
        assert!(shared(&first, &after));
        first.tick(&model, 0.0);
        assert_eq!(tx(&first), 10.0, "the pose crossed the rebake");
        // The snapshot still answers for the state it was taken from.
        let mut old = Puppet::new(&snapshot);
        old.set_param_value(&param, 1.0);
        old.tick(&snapshot, 0.0);
        assert_eq!(tx(&old), 40.0);
    }

    /// A clone forks the animation: it shares what the bake derived from the
    /// model and owns its frame, so posing one never moves the other.
    #[test]
    fn a_clone_shares_the_baked_model_and_owns_its_frame() {
        use crate::formats::clm::{ClmIndices, ClmMesh};
        use crate::id::SeededHex;
        use crate::model::{BindingKey, BindingTarget, ModelNode, ModelNodeKind, ModelParam};
        use crate::model::{ModelPart, ScalarTarget};
        use crate::Name;

        let mut hex = SeededHex::new(3);
        let mut model = Model::new();
        let param = model
            .add_param(
                ModelParam {
                    name: Name::truncated("slide"),
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .expect("add param");
        let root = model.root().expect("a fresh model has one root").clone();
        let mesh = ClmMesh {
            verts: vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
            uvs: vec![0.0; 6],
            indices: ClmIndices::U16(vec![0, 1, 2]),
            origin: [0.0, 0.0],
        };
        let part = model
            .add_node(
                &root,
                ModelNode::new("art", ModelNodeKind::Part(ModelPart::new(mesh))),
                &mut hex,
            )
            .expect("add part");
        let key = BindingKey::new(
            param.clone(),
            part.clone(),
            BindingTarget::Scalar(ScalarTarget::Tx),
        );
        // Both ends authored: a lone cell would fill the whole axis.
        model.set_binding_key(&key, [0, 0], 0.0).expect("key");
        model.set_binding_key(&key, [1, 0], 40.0).expect("key");

        let mut original = Puppet::new(&model);
        original.tick(&model, 0.0);
        let mut fork = original.clone();
        let idx = original.node_idx(&part).expect("part baked");

        let mesh_of = |p: &Puppet| match &p.get(idx).expect("node").kind {
            NodeKind::Part(part) => Arc::clone(&part.mesh),
            _ => unreachable!("the node is a part"),
        };
        assert!(Arc::ptr_eq(&mesh_of(&original), &mesh_of(&fork)));
        assert!(Arc::ptr_eq(&original.bindings, &fork.bindings));
        assert!(Arc::ptr_eq(&original.arena.tree, &fork.arena.tree));
        assert!(Arc::ptr_eq(&original.id_of_node, &fork.id_of_node));

        fork.set_param_value(&param, 1.0);
        fork.tick(&model, 0.0);
        let tx = |p: &Puppet| p.get(idx).expect("node").transform.translation.x;
        assert_eq!(tx(&fork), 40.0);
        assert_eq!(tx(&original), 0.0, "the original's frame is its own");
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
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");
        puppet.set_param_value(&param, 4.0);

        assert!(puppet.contribute(slot, a, 0.0, 1.0));
        assert_eq!(puppet.param_value(&param), Some(0.0), "one full claim wins");

        assert!(puppet.contribute(slot, b, 1.0, 1.0));
        assert_eq!(
            puppet.param_value(&param),
            Some(0.5),
            "two full claims meet in the middle rather than the later winning",
        );
        claim_index_agrees(&puppet);

        // Half authority leaves half the pose showing, and the two rows are
        // still one slot's.
        assert!(puppet.contribute(slot, b, 1.0, 0.0));
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
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");

        assert!(puppet.contribute(slot, a, 2.0, 1.0));
        assert!(puppet.contribute(slot, a, 6.0, 1.0));
        assert_eq!(
            puppet.param_value(&param),
            Some(6.0),
            "the second claim replaced the first instead of averaging with it",
        );
        assert_eq!(puppet.param_contributions.len(), 1);
        assert_eq!(puppet.slot_claims[slot as usize].len(), 1);
        claim_index_agrees(&puppet);

        // A claim that changes nothing says so, and still leaves one entry.
        assert!(!puppet.contribute(slot, a, 6.0, 1.0));
        assert_eq!(puppet.param_contributions.len(), 1);
        claim_index_agrees(&puppet);
    }

    /// Clearing the claims empties the index with them; a row left behind
    /// would point at claims that are gone.
    #[test]
    fn clearing_the_claims_empties_the_index() {
        let (_model, mut puppet, param, a, b) = two_claimants();
        let slot = puppet.slot_of_param.get(&param).copied().expect("a slot");
        puppet.set_param_value(&param, 3.0);
        puppet.contribute(slot, a, 0.0, 1.0);
        puppet.contribute(slot, b, 1.0, 1.0);
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
}
