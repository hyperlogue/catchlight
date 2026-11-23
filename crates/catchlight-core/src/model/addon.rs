//! Addons: a partial model, authored against a base model and installed into
//! it by Id.
//!
//! An **addon** is an ordinary [`Model`] whose cross-references reach into
//! another one. It is authored against a **base model**, references what it
//! needs from that base by Id, and is merged in by [`Model::install`].
//! Nothing about what it needs is declared: [`Model::requirements`] finds the
//! dangling Ids by scanning, so an addon and the base agree through the Ids
//! themselves and through nothing else.
//!
//! Invariants this module enforces:
//!
//! - **A fragment is a forest whose roots name absent parents.** A complete
//!   model has one root and its parent is `None`; an addon has one or more
//!   [`Model::roots`], each of whose `parent` names a base node the addon does
//!   not carry. That is the whole of the difference, in memory and on the wire
//!   — which is why an addon that names no parent at all is not an addon
//!   ([`InstallError::NotAnAddon`]) and why the two shapes have their own
//!   readers ([`Model::from_clm_bytes`] against
//!   [`Model::from_clm_bytes_fragment`]).
//! - **An Id an addon *provides* is exclusive.** Install refuses an addon
//!   whose node or texture Id the base already has
//!   ([`InstallError::Collision`]) instead of renaming around it: renaming
//!   would silently break the addon's own internal references, and refusing
//!   is what gives outfit slots for free — two addons that both provide
//!   `shoes` are alternatives, and only one is installed at a time. A slot Id
//!   is unique per part rather than per model, so an addon's slots cannot
//!   collide on their own: the part carrying them collides first.
//! - **An addon carries the textures its own parts draw.** A texture's only
//!   user is a part's albedo, so an addon that reached into the base for one
//!   would be an addon whose parts stop drawing when the base drops a texture
//!   it has no reason to keep. A fragment's albedo therefore may not dangle
//!   (the reader refuses it, [`Model::from_clm_bytes_fragment`]), a texture
//!   is not a [`Required`] kind at all, and [`Model::extract`] *copies* a
//!   texture the cut subtree shares with the parts left behind rather than
//!   leaving it as a requirement. The price is paid where it is cheapest to
//!   pay: duplicated bytes in the addon file, and — because an Id an addon
//!   provides is exclusive — a shared texture that makes the addon collide
//!   with the very base it was cut from until that base drops it.
//! - **An addon carries no extensions.** An extension annotates a whole
//!   model, and nothing in it says which subtree it is about, so
//!   [`Model::extract`] leaves every one behind and install refuses an addon
//!   that carries one ([`InstallError::CarriesExtension`]). Merging two
//!   vendors' annotations is a question the format cannot answer.
//! - **An addon never adds or changes a param.** A `ParamId` it uses has to be
//!   in the base already — that is a requirement like any other — so
//!   [`Model::extract`] carries no params and install refuses an addon that
//!   does ([`InstallError::CarriesParam`]). For the same reason a binding may
//!   only drive a node the addon itself provides
//!   ([`InstallError::BindsOffAddon`]): a binding on a *base* node would be an
//!   edit to the base with no Id of its own to collide on, so two addons could
//!   silently fight over one node's deform.
//! - **An addon welds its own parts, and install finishes the weld rule.** A
//!   weld needs at least one end the addon provides
//!   ([`InstallError::WeldsOffAddon`]): one between two base parts is an edit
//!   to the base, as a binding on a base node is. A fragment can only check
//!   the end it carries, so install — the first place both ends are visible —
//!   holds every addon weld to the whole rule [`Model::set_welds`] and the
//!   reader keep: each end a part, each pair naming a slot its part carries,
//!   and each part pair welded once across the merged list. A merged model is
//!   therefore one that saves and loads.
//! - **Install is one edit.** Everything is checked against the merged model
//!   before anything moves, so a refused install leaves the base untouched,
//!   and the whole merge bumps [`Model::generation`] exactly once — a puppet
//!   ticking through an install pays one rebake and keeps its pose, which is
//!   carried by Id.
//! - **Extract is install's inverse, up to order.** `extract` takes the
//!   subtrees, the bindings on them, the welds touching them and every
//!   texture they draw. Installing that back into the base it was cut from
//!   restores every value — but install appends: an addon's roots go last
//!   among their parent's children and its textures last in texture order,
//!   because an addon records no position to be put back into. "The base it
//!   was cut from" means the base after the cut: deleting the subtree is what
//!   frees the texture Ids the addon now provides.
//! - **Animations travel, lanes are not extracted.** A lane addresses a param,
//!   and params stay in the base, so there is no such thing as the animations
//!   "over" a subtree and `extract` returns none. An addon that *was* authored
//!   with animations installs them, and its lanes' params are requirements
//!   like any others.
//! - **Masks travel with their owner.** When an addon's part masks a *base*
//!   drawable, the mask belongs to that drawable. [`Model::extract`] leaves that mask
//!   behind and deleting the subtree drops it. That is the same rule as
//!   [`InstallError::BindsOffAddon`] and as "an addon never adds a param" —
//!   an addon adds to a base model, it does not edit one — and
//!   `a_mask_on_a_base_node_is_not_the_addons_to_carry` in
//!   `tests/addons.rs` is where it is pinned.

use std::fmt;
use std::sync::OnceLock;

use super::*;

/// One Id an addon names in another model: what a requirement is about, and
/// what a collision is about.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Required {
    /// A node of any kind — what a fragment root's `parent` names.
    Node(NodeId),
    /// A node the renderer draws — a part or a composite — which is what a
    /// mask's source names; see [`Model::mask_add`] for the rule.
    Drawable(NodeId),
    /// A part and nothing else — what a weld's end names when the weld pairs
    /// no slots. Kept apart from [`Self::Drawable`] because the two rules
    /// differ: a composite can clip, but it has no slots and cannot be welded.
    Part(NodeId),
    Param(ParamId),
    /// A slot on a base part, which needs the part and the slot both — what
    /// one side of a weld's pairs names.
    Slot(NodeId, SlotId),
}

impl fmt::Display for Required {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(id) => write!(f, "node {:?}", id.as_str()),
            Self::Drawable(id) => write!(f, "drawable {:?}", id.as_str()),
            Self::Part(id) => write!(f, "part {:?}", id.as_str()),
            Self::Param(id) => write!(f, "param {:?}", id.as_str()),
            Self::Slot(node, slot) => {
                write!(f, "slot {:?} on part {:?}", slot.as_str(), node.as_str())
            }
        }
    }
}

/// One thing an addon needs from the base model: the Id, the field that names
/// it, and what in the addon carries that field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Requirement {
    pub id: Required,
    /// `"parent"`, `"mask source"`, `"motion response param"`,
    /// `"motion response slot"`, `"spine target"`, `"binding param"`,
    /// `"binding node"`, `"weld end"` or `"animation lane"`. Never an
    /// albedo: an addon carries the textures its own parts draw.
    pub field: &'static str,
    /// The node in the addon whose field names it — or the animation, for a
    /// lane.
    pub owner: String,
}

/// Everything an addon needs from the base model it is installed into, found
/// by scanning the addon: sorted, and with the same Id listed once per field
/// that names it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Requirements {
    entries: Vec<Requirement>,
}

impl Requirements {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Requirement> {
        self.entries.iter()
    }

    /// Every node Id required, sorted, once each — including the ones that
    /// have to be drawn, the ones that have to be parts and the ones a weld
    /// end reaches through.
    pub fn nodes(&self) -> impl Iterator<Item = &NodeId> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Node(id)
            | Required::Drawable(id)
            | Required::Part(id)
            | Required::Slot(id, _) => Some(id),
            Required::Param(_) => None,
        }))
    }

    pub fn params(&self) -> impl Iterator<Item = &ParamId> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Param(id) => Some(id),
            _ => None,
        }))
    }

    pub fn slots(&self) -> impl Iterator<Item = (&NodeId, &SlotId)> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Slot(node, slot) => Some((node, slot)),
            _ => None,
        }))
    }
}

impl<'a> IntoIterator for &'a Requirements {
    type Item = &'a Requirement;
    type IntoIter = std::slice::Iter<'a, Requirement>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

/// Sorted and deduplicated. One [`Requirement`] per field that names an Id,
/// so the same Id shows up several times in `entries` on purpose; the
/// per-kind accessors are the set view of it.
fn sorted<T: Ord>(it: impl Iterator<Item = T>) -> impl Iterator<Item = T> {
    let mut v: Vec<T> = it.collect();
    v.sort();
    v.dedup();
    v.into_iter()
}

/// Why an addon could not be installed. Every variant names the Id that
/// stopped it; the base model is untouched.
///
/// Non-exhaustive, like [`ModelError`]: install checks the merged model, so
/// it grows a refusal whenever the model grows a rule.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    /// The merged model broke a rule the model itself enforces — a motion
    /// response whose references no longer resolve, say. Carried whole rather
    /// than as its message, so a caller can branch on it exactly as it would
    /// on the same refusal from an edit.
    #[error(transparent)]
    Model(Box<ModelError>),
    #[error(
        "the addon's root {node:?} names no parent, so it is a complete model and there is \
         nowhere in the base to attach it"
    )]
    NotAnAddon { node: String },
    #[error(
        "the addon carries param {param:?}: an addon never adds or changes a param, so the \
         params it drives have to be in the base model already"
    )]
    CarriesParam { param: String },
    #[error(
        "the addon carries extension {key:?}: an extension annotates a whole model, and an addon \
         is a subtree of one"
    )]
    CarriesExtension { key: String },
    #[error(
        "the addon's {target} binding names node {node:?}, which the addon does not provide: an \
         addon binds its own nodes"
    )]
    BindsOffAddon { node: String, target: &'static str },
    #[error(
        "the addon provides {kind} {id:?}, which the base model already has: two addons that \
         provide one Id are alternatives, and only one can be installed at a time"
    )]
    Collision { kind: &'static str, id: String },
    #[error("the addon's {field} on {owner} names {id}, which the base model does not have")]
    Missing {
        id: Required,
        field: &'static str,
        owner: String,
    },
    #[error("the addon welds slot {slot:?} on node {node:?}, which carries no such slot")]
    WeldUnknownSlot { node: String, slot: String },
    #[error("the addon's weld between {a:?} and {b:?} pairs slot {slot:?} twice")]
    WeldSlotPairedTwice { a: String, b: String, slot: String },
    #[error("the addon welds node {node:?}, which is a {kind}; a weld joins two parts")]
    WeldEndNotAPart { node: String, kind: &'static str },
    #[error(
        "the addon welds {a:?} to {b:?}, which are already welded; one weld records a pair \
         of parts"
    )]
    DuplicateWeld { a: String, b: String },
    #[error(
        "the addon welds {a:?} to {b:?}, neither of which it provides; an addon may only weld \
         its own parts"
    )]
    WeldsOffAddon { a: String, b: String },
}

impl From<ModelError> for InstallError {
    fn from(error: ModelError) -> Self {
        Self::Model(Box::new(error))
    }
}

/// What one [`Model::install`] put into a base model, so [`Model::uninstall`]
/// can take it back: those nodes, with whatever has come to hang under them
/// since, and the bindings, welds, textures and animations that came along.
#[derive(Debug, Clone)]
pub struct Installed {
    nodes: Vec<NodeId>,
    roots: Vec<NodeId>,
    textures: Vec<TexId>,
    bindings: Vec<BindingKey>,
    welds: Vec<ModelWeld>,
    animations: Vec<ClmAnimation>,
}

impl Installed {
    /// Every node the install added, in tree order.
    pub fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }

    /// The addon's own roots, as attached to their base parents.
    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    pub fn textures(&self) -> &[TexId] {
        &self.textures
    }

    pub fn bindings(&self) -> &[BindingKey] {
        &self.bindings
    }

    pub fn welds(&self) -> &[ModelWeld] {
        &self.welds
    }

    pub fn animations(&self) -> &[ClmAnimation] {
        &self.animations
    }
}

impl Model {
    /// Every Id this model names but does not carry: what a base model has to
    /// have before it can take this one as an addon.
    ///
    /// A complete model requires nothing — its own invariants say so — so this
    /// is empty for anything but a fragment.
    pub fn requirements(&self) -> Requirements {
        let mut out: Vec<Requirement> = Vec::new();
        let mut need = |id: Required, field: &'static str, owner: &dyn fmt::Display| {
            out.push(Requirement {
                id,
                field,
                owner: owner.to_string(),
            })
        };

        for id in self.nodes_in_order() {
            let Some(node) = self.nodes.get(&id) else {
                continue;
            };
            if let Some(parent) = node.parent() {
                if !self.nodes.contains_key(parent) {
                    need(Required::Node(parent.clone()), "parent", &id);
                }
            }
            if let ModelNodeKind::MotionResponse(r) = &node.kind {
                for p in
                    r.output_param
                        .iter()
                        .chain(r.inputs.iter().filter_map(|t| match &t.source {
                            crate::ResponseSource::PoseParam { param: p } => Some(p),
                            _ => None,
                        }))
                {
                    if !self.params.contains_key(p) {
                        need(Required::Param(p.clone()), "motion response param", &id);
                    }
                }
                for t in &r.inputs {
                    if let crate::ResponseSource::PartSlot { part, slot, .. } = &t.source {
                        if !self.nodes.contains_key(part) {
                            need(
                                Required::Slot(part.clone(), slot.clone()),
                                "motion response slot",
                                &id,
                            );
                        }
                    }
                }
            }
            if let ModelNodeKind::Spine(spine) = &node.kind {
                for t in spine.targets().iter().flatten() {
                    if !self.params.contains_key(t) {
                        need(Required::Param(t.clone()), "spine target", &id);
                    }
                }
            }
            for mask in node.masks().unwrap_or_default() {
                if !self.nodes.contains_key(mask.source()) {
                    need(
                        Required::Drawable(mask.source().clone()),
                        "mask source",
                        &id,
                    );
                }
            }
        }

        for b in &self.bindings {
            if !self.nodes.contains_key(&b.key.node) {
                need(
                    Required::Node(b.key.node.clone()),
                    "binding node",
                    &b.key.node,
                );
            }
            for p in b.key.params.iter() {
                if !self.params.contains_key(p) {
                    need(Required::Param(p.clone()), "binding param", &b.key.node);
                }
            }
        }

        for w in &self.welds {
            for (end, far, slot_of) in [
                (
                    &w.a,
                    &w.b,
                    (|p: &SlotPair| &p.a) as fn(&SlotPair) -> &SlotId,
                ),
                (&w.b, &w.a, |p: &SlotPair| &p.b),
            ] {
                if self.nodes.contains_key(end) {
                    continue;
                }
                // One requirement per base slot the weld pairs: the part and
                // the slot on it are needed together. A weld with no pairs
                // still names the part.
                for pair in w.pairs() {
                    need(
                        Required::Slot(end.clone(), slot_of(pair).clone()),
                        "weld end",
                        far,
                    );
                }
                if w.pairs().is_empty() {
                    need(Required::Part(end.clone()), "weld end", far);
                }
            }
        }

        for animation in &self.animations {
            for lane in &animation.lanes {
                if !self.params.contains_key(&lane.param) {
                    need(
                        Required::Param(lane.param.clone()),
                        "animation lane",
                        &format_args!("animation {:?}", animation.name),
                    );
                }
            }
        }

        out.sort();
        out.dedup();
        Requirements { entries: out }
    }

    /// Merge `addon` into this model.
    ///
    /// Fails — leaving this model untouched — if any Id the addon needs is
    /// missing, or if any Id it provides is one this model already has. On
    /// success this model carries the addon's nodes (each root appended under
    /// the base parent it names), the bindings on those nodes, its welds, its
    /// textures and its animations, and the generation is bumped once.
    pub fn install(&mut self, addon: &Model) -> Result<Installed, InstallError> {
        self.check_install(addon)?;
        let order = addon.nodes_in_order();
        let mut installed = Installed {
            nodes: Vec::with_capacity(order.len()),
            roots: addon.roots.clone(),
            textures: addon.texture_order.clone(),
            bindings: addon.bindings.iter().map(|b| b.key.clone()).collect(),
            welds: addon.welds.clone(),
            animations: addon.animations.clone(),
        };

        self.response_count += addon.response_count;
        for id in order {
            let Some(node) = addon.nodes.get(&id) else {
                continue;
            };
            self.nodes.insert(id.clone(), node.clone());
            installed.nodes.push(id);
        }
        for b in &addon.bindings {
            self.bindings.push(ModelBinding {
                key_positions: b.key_positions.clone(),
                key: b.key.clone(),
                interpolate_mode: b.interpolate_mode,
                values: b.values.clone(),
                // The addon has no params, so its grid was never derived; the
                // base's key positions are what this binding fills against.
                dense: OnceLock::new(),
            });
        }

        self.animations.extend(addon.animations.iter().cloned());
        // Only appended rows participate in this transaction. Validate with
        // base params and addon references visible together, before attaching
        // roots or changing assets. A refusal restores the original revision.
        self.response_index.take();
        if let Err(e) = self.validate_responses() {
            self.bindings
                .truncate(self.bindings.len() - addon.bindings.len());
            self.animations
                .truncate(self.animations.len() - addon.animations.len());
            for node in &installed.nodes {
                self.nodes.remove(node);
            }
            self.response_count -= addon.response_count;
            self.response_index.take();
            return Err(e.into());
        }
        // Sibling order inside the addon rode along on the copied child lists;
        // only its roots need a place among the base's children, and they go
        // last — an addon records no position to be put back into.
        for r in &addon.roots {
            if let Some(parent) = addon.nodes.get(r).and_then(|n| n.parent.clone()) {
                self.attach_child(&parent, r.clone());
            }
        }

        for id in &addon.texture_order {
            if let Some(t) = addon.textures.get(id) {
                self.textures.insert(id.clone(), t.clone());
                self.texture_order.push(id.clone());
            }
        }

        self.welds.extend(addon.welds.iter().cloned());
        self.bump();
        Ok(installed)
    }

    /// Everything [`Self::install`] refuses, checked against the model the
    /// merge *would* produce, before any of it happens.
    fn check_install(&self, addon: &Model) -> Result<(), InstallError> {
        for r in &addon.roots {
            if addon.nodes.get(r).is_some_and(|n| n.parent.is_none()) {
                return Err(InstallError::NotAnAddon {
                    node: r.to_string(),
                });
            }
        }
        if let Some(param) = addon.param_order.first() {
            return Err(InstallError::CarriesParam {
                param: param.to_string(),
            });
        }
        if let Some((key, _)) = addon.extensions.first_key_value() {
            return Err(InstallError::CarriesExtension {
                key: key.to_string(),
            });
        }
        for b in &addon.bindings {
            if !addon.nodes.contains_key(&b.key.node) {
                return Err(InstallError::BindsOffAddon {
                    node: b.key.node.to_string(),
                    target: b.key.target.name(),
                });
            }
        }

        for id in &addon.nodes_in_order() {
            if self.nodes.contains_key(id) {
                return Err(InstallError::Collision {
                    kind: "node",
                    id: id.to_string(),
                });
            }
        }
        for id in &addon.texture_order {
            if self.textures.contains_key(id) {
                return Err(InstallError::Collision {
                    kind: "texture",
                    id: id.to_string(),
                });
            }
        }

        for req in addon.requirements().iter() {
            let held = match &req.id {
                Required::Node(id) => self.nodes.contains_key(id),
                // A mask source has to be one the renderer draws, in the
                // base exactly as in the addon: install is the one place a
                // fragment's dangling mask source is finally kind-checked.
                Required::Drawable(id) => self
                    .nodes
                    .get(id)
                    .map(|n| &n.kind)
                    .is_some_and(is_mask_source),
                // A weld end is a part and nothing else. A composite is
                // drawn but carries no slots, and a base that welded one
                // could not be written.
                Required::Part(id) => matches!(
                    self.nodes.get(id).map(|n| &n.kind),
                    Some(ModelNodeKind::Part(_))
                ),
                Required::Param(id) => self.params.contains_key(id),
                Required::Slot(node, slot) => self.slot(node, slot).is_some(),
            };
            if !held {
                return Err(InstallError::Missing {
                    id: req.id.clone(),
                    field: req.field,
                    owner: req.owner.clone(),
                });
            }
        }

        // Every weld is held to the weld rule now that both ends can be
        // seen: one side in the addon, the other in whichever of the two
        // models carries it, and the merged weld list as the one the rule's
        // "once per part pair" is counted over. This is the rule
        // `Model::set_welds` and the reader keep, so a merged model is one
        // that saves and loads.
        let mut welded = validate::Welded::with_capacity(self.welds.len() + addon.welds.len());
        for w in &self.welds {
            welded.insert(&w.a, &w.b);
        }
        let kind_of = |id: &NodeId| {
            addon
                .nodes
                .get(id)
                .or_else(|| self.nodes.get(id))
                .map(|n| &n.kind)
        };
        for w in &addon.welds {
            let (a, b) = (w.a(), w.b());
            // A weld between two base parts is an edit to the base, the same
            // way a binding on a base node is: it has no addon node to be
            // installed with or uninstalled after.
            if !addon.nodes.contains_key(a) && !addon.nodes.contains_key(b) {
                return Err(InstallError::WeldsOffAddon {
                    a: a.to_string(),
                    b: b.to_string(),
                });
            }
            validate::weld(
                a,
                b,
                w.pairs().iter().map(|pair| (&pair.a, &pair.b, pair.weight)),
                &mut welded,
                |end| validate::WeldEnd::of(kind_of(end), false),
                |part, slot| addon.slot(part, slot).is_some() || self.slot(part, slot).is_some(),
            )
            .map_err(|fault| match fault {
                validate::WeldFault::SelfPaired => ModelError::WeldSelfPaired.into(),
                validate::WeldFault::WeightOutOfRange { .. } => {
                    ModelError::WeldWeightOutOfRange.into()
                }
                validate::WeldFault::EndNotAPart { end, kind } => InstallError::WeldEndNotAPart {
                    node: end.to_string(),
                    kind,
                },
                validate::WeldFault::EndMissing { end } => InstallError::Missing {
                    id: Required::Part(end.clone()),
                    field: "weld end",
                    owner: if end == a { b } else { a }.to_string(),
                },
                validate::WeldFault::Duplicate => InstallError::DuplicateWeld {
                    a: a.to_string(),
                    b: b.to_string(),
                },
                validate::WeldFault::UnknownSlot { end, slot } => InstallError::WeldUnknownSlot {
                    node: end.to_string(),
                    slot: slot.to_string(),
                },
                validate::WeldFault::SlotPairedTwice { slot, .. } => {
                    InstallError::WeldSlotPairedTwice {
                        a: a.to_string(),
                        b: b.to_string(),
                        slot: slot.to_string(),
                    }
                }
            })?;
        }
        Ok(())
    }

    /// Cut `roots` and their subtrees out as an addon: the nodes, the bindings
    /// on them, the welds touching them, and every texture their parts draw
    /// — copied, not moved, when a part left behind draws it too.
    ///
    /// This model is not changed. A root that is not here, that has no parent
    /// of its own, or that sits inside another one given, is dropped; the
    /// rest keep their `parent`, which is what makes the result a fragment.
    /// Params and animations stay behind — a lane addresses a param, so there
    /// is no animation "over" a subtree.
    ///
    /// **A parentless node cannot be cut.** It is a complete model's root: it
    /// names no parent, so nothing would say where the addon attaches, and
    /// the result would be neither shape — a root with no parent is not a
    /// fragment, and bindings with their params left behind are not a
    /// complete model — so neither reader would take it back. Asking for one
    /// yields what asking for nothing yields, an empty fragment.
    pub fn extract(&self, roots: &[NodeId]) -> Model {
        let given: HashSet<&NodeId> = roots
            .iter()
            .filter(|r| self.nodes.get(*r).is_some_and(|n| n.parent.is_some()))
            .collect();
        let picked: Vec<NodeId> = self
            .nodes_in_order()
            .into_iter()
            .filter(|id| given.contains(id) && !self.has_ancestor_in(id, &given))
            .collect();

        let mut kept: HashSet<NodeId> = HashSet::new();
        for r in &picked {
            kept.extend(self.subtree(r));
        }

        let nodes: HashMap<NodeId, ModelNode> = kept
            .iter()
            .filter_map(|id| self.nodes.get(id).map(|n| (id.clone(), n.clone())))
            .collect();

        let inside = |id: &NodeId| kept.contains(id);
        // Every texture the cut parts draw, whether or not the base keeps a
        // part drawing it too: an addon carries its own, so a shared one is
        // *copied* rather than left behind as a requirement. The bytes are
        // `Arc`-shared until one of the two models is written.
        let used_inside: HashSet<&TexId> = nodes.values().filter_map(albedo_of).collect();
        let texture_order: Vec<TexId> = self
            .texture_order
            .iter()
            .filter(|t| used_inside.contains(t))
            .cloned()
            .collect();
        let textures = texture_order
            .iter()
            .filter_map(|t| self.textures.get(t).map(|v| (t.clone(), v.clone())))
            .collect();

        Model {
            // Extracting builds a model of its own, so it draws its own
            // identity: a puppet of the base model is not a puppet of this.
            identity: super::next_identity(),
            generation: 0,
            welds: self
                .welds
                .iter()
                .filter(|w| inside(&w.a) || inside(&w.b))
                .cloned()
                .collect(),
            response_index: OnceLock::new(),
            response_count: nodes
                .values()
                .filter(|n| matches!(n.kind, ModelNodeKind::MotionResponse(_)))
                .count(),
            nodes,
            roots: picked,
            params: HashMap::new(),
            param_order: Vec::new(),
            textures,
            texture_order,
            bindings: self
                .bindings
                .iter()
                .filter(|b| inside(&b.key.node))
                .map(|b| ModelBinding {
                    key_positions: b.key_positions.clone(),
                    key: b.key.clone(),
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone(),
                    dense: OnceLock::new(),
                })
                .collect(),
            animations: Vec::new(),
            // An extension annotates a whole model, so there is no telling
            // which of them a cut subtree is about; the base keeps them all.
            extensions: BTreeMap::new(),
            binding_index: OnceLock::new(),
            bake: Default::default(),
        }
    }

    /// Take back what one [`Self::install`] added.
    ///
    /// Total: anything the receipt names that has since been deleted is
    /// already gone, and a node moved elsewhere in the tree is still removed.
    ///
    /// **An installed node goes with its whole current subtree**, not only
    /// with the nodes the receipt lists. A node added or moved under one
    /// since the install has nowhere to live once its parent is gone, and
    /// leaving it would leave a node no walk from the root reaches, still
    /// carrying bindings, welds and textures. The removal is the one
    /// [`Self::delete_node`] runs, so everything that named a removed node —
    /// masks, response inputs, bindings, welds, textures only they drew — is
    /// swept the same way.
    pub fn uninstall(&mut self, installed: &Installed) {
        let mut removed: HashSet<NodeId> = HashSet::new();
        for id in &installed.nodes {
            if self.nodes.contains_key(id) && !removed.contains(id) {
                removed.extend(self.subtree(id));
            }
        }
        self.remove_nodes(&removed);

        // What the receipt names beyond its nodes. A base part re-pointed at
        // an addon texture since the install goes back to drawing nothing.
        for node in self.nodes.values_mut() {
            if let ModelNodeKind::Part(p) = &mut node.kind {
                if p.albedo
                    .as_ref()
                    .is_some_and(|t| installed.textures.contains(t))
                {
                    p.albedo = None;
                }
            }
        }
        self.bindings
            .retain(|b| !installed.bindings.contains(&b.key));
        for t in &installed.textures {
            self.textures.remove(t);
            self.texture_order.retain(|x| x != t);
        }
        // Un-pointing those parts may have left a *base* texture with nothing
        // drawing it. Uninstall is total, so it sweeps again.
        self.gc_textures();
        retain_less(&mut self.welds, &installed.welds, |_| true);
        retain_less(&mut self.animations, &installed.animations, |_| true);
        self.bump();
    }

    /// Whether any of `id`'s ancestors is in `set`.
    fn has_ancestor_in(&self, id: &NodeId, set: &HashSet<&NodeId>) -> bool {
        let mut cur = self.nodes.get(id).and_then(|n| n.parent.as_ref());
        while let Some(p) = cur {
            if set.contains(p) {
                return true;
            }
            cur = self.nodes.get(p).and_then(|n| n.parent.as_ref());
        }
        false
    }
}

/// Drop everything `keep` rejects, plus one occurrence of each of `once` —
/// what uninstalling needs for the two lists whose entries carry no Id.
fn retain_less<T: PartialEq>(list: &mut Vec<T>, once: &[T], keep: impl Fn(&T) -> bool) {
    let mut left: Vec<&T> = once.iter().collect();
    list.retain(|item| {
        if !keep(item) {
            return false;
        }
        match left.iter().position(|x| *x == item) {
            Some(i) => {
                left.remove(i);
                false
            }
            None => true,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::MaskMode;
    use crate::formats::clm::{ClmIndices, ClmKeyframe, ClmLane, ClmMesh};
    use crate::id::SeededHex;
    use crate::interpolate::InterpolateMode;

    fn quad() -> ClmMesh {
        ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    fn slot_id(s: &str) -> SlotId {
        SlotId::new(s).unwrap()
    }

    fn texture(byte: u8) -> ModelTexture {
        ModelTexture {
            encoding: crate::formats::clm::TextureEncoding::Png,
            alpha: crate::formats::clm::TextureAlpha::Straight,
            data: vec![byte; 4].into(),
        }
    }

    /// A base model an addon can be authored against: a `body` part carrying
    /// the slots `left` and `right` and drawing the one texture, a `head`
    /// group to hang things under, and one param.
    struct Base {
        m: Model,
        root: NodeId,
        body: NodeId,
        head: NodeId,
        param: ParamId,
        tex: TexId,
        hex: SeededHex,
    }

    fn base() -> Base {
        let mut hex = SeededHex::new(11);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let body = m
            .add_node(
                &root,
                ModelNode::new("Body", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut hex,
            )
            .unwrap();
        let tex = m.add_texture(&body, texture(1), &mut hex).unwrap();
        let head = m
            .add_node(
                &root,
                ModelNode::new("Head", ModelNodeKind::Group),
                &mut hex,
            )
            .unwrap();
        let param = m
            .add_param(
                ModelParam::new(Name::truncated("Lean"), -1.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        for (slot, vertex) in [("left", 0), ("right", 1)] {
            m.slot_add(&body, slot_id(slot)).unwrap();
            m.slot_fill(&body, &slot_id(slot), vertex).unwrap();
        }
        Base {
            m,
            root,
            body,
            head,
            param,
            tex,
            hex,
        }
    }

    /// Author an addon the way an author would: add nodes to a copy of the
    /// base, then cut them out. What `f` returns are the addon's roots.
    fn author(base: &Base, f: impl FnOnce(&mut Model, &mut SeededHex) -> Vec<NodeId>) -> Model {
        let mut scratch = base.m.clone();
        let mut hex = base.hex.clone();
        let roots = f(&mut scratch, &mut hex);
        scratch.extract(&roots)
    }

    /// A hat under `head` that reaches into the base every way a fragment can:
    /// a base parent, a base part as a mask source, a base param through a
    /// binding and through a motion response, and a weld into the base's `Body`. Its
    /// texture is its own — that is the one reference an addon never makes.
    fn hat_addon(b: &Base) -> Model {
        author(b, |m, hex| {
            let hat = m
                .add_node(
                    &b.head,
                    ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                    hex,
                )
                .unwrap();
            m.add_texture(&hat, texture(2), hex).unwrap();
            m.mask_add(&hat, &b.body, MaskMode::DodgeMask).unwrap();

            let key = BindingKey::new(b.param.clone(), hat.clone(), BindingTarget::Deform);
            m.add_binding(&key).unwrap();
            m.set_deform_vertices(&key, [1, 0], vec![0.5; 8]).unwrap();

            for (slot, vertex) in [("left", 2), ("right", 3)] {
                m.slot_add(&hat, slot_id(slot)).unwrap();
                m.slot_fill(&hat, &slot_id(slot), vertex).unwrap();
            }
            let mut welds = m.welds().to_vec();
            welds.push(ModelWeld::new(
                hat.clone(),
                b.body.clone(),
                vec![
                    SlotPair {
                        a: slot_id("left"),
                        b: slot_id("left"),
                        weight: 0.5,
                    },
                    SlotPair {
                        a: slot_id("right"),
                        b: slot_id("right"),
                        weight: 0.25,
                    },
                ],
            ));
            m.set_welds(welds).unwrap();

            let sway = m
                .add_node(
                    &b.head,
                    ModelNode::new(
                        "Sway",
                        ModelNodeKind::MotionResponse(crate::MotionResponse {
                            inputs: vec![crate::ResponseInput {
                                source: crate::ResponseSource::PoseParam {
                                    param: b.param.clone(),
                                },
                                gain: 1.0,
                            }],
                            output_param: None,
                            ..Default::default()
                        }),
                    ),
                    hex,
                )
                .unwrap();
            vec![hat, sway]
        })
    }

    fn listed(r: &Requirements) -> Vec<(String, &'static str, String)> {
        r.iter()
            .map(|q| (q.id.to_string(), q.field, q.owner.clone()))
            .collect()
    }

    fn named(m: &Model, name: &str) -> NodeId {
        m.nodes_in_order()
            .into_iter()
            .find(|id| m.node(id).is_some_and(|n| n.name.as_str() == name))
            .unwrap_or_else(|| panic!("no node named {name}"))
    }

    #[test]
    fn requirements_of_an_extracted_fragment_are_exactly_its_cut_edges() {
        let b = base();
        let addon = hat_addon(&b);
        let (hat, sway) = (named(&addon, "Hat"), named(&addon, "Sway"));

        assert_eq!(
            listed(&addon.requirements()),
            vec![
                (
                    format!("node {:?}", b.head.as_str()),
                    "parent",
                    hat.to_string()
                ),
                (
                    format!("node {:?}", b.head.as_str()),
                    "parent",
                    sway.to_string()
                ),
                (
                    format!("drawable {:?}", b.body.as_str()),
                    "mask source",
                    hat.to_string()
                ),
                (
                    format!("param {:?}", b.param.as_str()),
                    "binding param",
                    hat.to_string()
                ),
                (
                    format!("param {:?}", b.param.as_str()),
                    "motion response param",
                    sway.to_string()
                ),
                (
                    format!("slot \"left\" on part {:?}", b.body.as_str()),
                    "weld end",
                    hat.to_string()
                ),
                (
                    format!("slot \"right\" on part {:?}", b.body.as_str()),
                    "weld end",
                    hat.to_string()
                ),
            ]
        );
        assert_eq!(addon.requirements().nodes().count(), 2);
        assert_eq!(addon.requirements().params().count(), 1);
        assert_eq!(addon.requirements().slots().count(), 2);
    }

    /// A complete model needs nothing: its own invariants say every Id it
    /// names is one it carries.
    #[test]
    fn a_complete_model_requires_nothing() {
        assert!(base().m.requirements().is_empty());
    }

    #[test]
    fn an_addon_installs_everything_it_carries() {
        let mut b = base();
        let addon = hat_addon(&b);
        let generation = b.m.generation();
        let before = b.m.node_count();

        let installed = b.m.install(&addon).unwrap();

        assert_ne!(b.m.generation(), generation, "an install is an edit");
        assert_eq!(b.m.node_count(), before + 2);
        assert_eq!(installed.nodes().len(), 2);
        assert_eq!(installed.roots().len(), 2);
        let hat = named(&b.m, "Hat");
        assert_eq!(b.m.node(&hat).unwrap().parent(), Some(&b.head));
        assert!(b.m.requirements().is_empty(), "nothing dangles any more");
        // The addon's own params section is empty, and the base's is untouched.
        assert_eq!(b.m.param_ids(), [b.param.clone()]);
    }

    /// The one ordering rule install has: an addon records no position among
    /// its parent's children, so its roots go last.
    #[test]
    fn an_addons_roots_are_appended_under_their_parent() {
        let mut b = base();
        let first =
            b.m.add_node(
                &b.head,
                ModelNode::new("Ear", ModelNodeKind::Group),
                &mut b.hex,
            )
            .unwrap();
        let addon = hat_addon(&b);
        b.m.install(&addon).unwrap();
        assert_eq!(
            b.m.node(&b.head).unwrap().children(),
            [first, named(&b.m, "Hat"), named(&b.m, "Sway")]
        );
    }

    #[test]
    fn a_missing_requirement_names_the_id_and_the_field() {
        let b = base();
        let addon = hat_addon(&b);
        let mut stripped = b.m.clone();
        stripped.delete_param(&b.param).unwrap();

        let before = stripped.generation();
        let err = stripped.install(&addon).unwrap_err();
        let InstallError::Missing { id, field, owner } = &err else {
            panic!("expected a missing requirement, got {err:?}");
        };
        assert_eq!(id, &Required::Param(b.param.clone()));
        assert_eq!(*field, "binding param");
        assert_eq!(owner, named(&addon, "Hat").as_str());
        assert!(err.to_string().contains(b.param.as_str()));
        assert_eq!(stripped.generation(), before, "no half-install");
    }

    /// A weld end is one requirement per slot it pairs, not one for the part:
    /// the part and the slot on it are needed together.
    #[test]
    fn a_missing_slot_is_named_with_the_part_that_should_carry_it() {
        let b = base();
        let addon = hat_addon(&b);
        let mut stripped = b.m.clone();
        stripped.slot_delete(&b.body, &slot_id("left")).unwrap();

        assert_eq!(
            stripped.install(&addon).unwrap_err().to_string(),
            InstallError::Missing {
                id: Required::Slot(b.body.clone(), slot_id("left")),
                field: "weld end",
                owner: named(&addon, "Hat").to_string(),
            }
            .to_string()
        );
    }

    /// A weld whose pairs were all dropped still reaches the base part, so
    /// the part is a requirement even with no slot to name.
    #[test]
    fn a_weld_with_no_pairs_still_requires_the_part() {
        let b = base();
        let mut addon = hat_addon(&b);
        let hat = named(&addon, "Hat");
        addon.mask_delete(&hat, 0).unwrap();
        for slot in ["left", "right"] {
            addon.slot_delete(&hat, &slot_id(slot)).unwrap();
        }
        assert!(addon.welds().iter().all(|w| w.pairs().is_empty()));
        let mut stripped = b.m.clone();
        stripped.delete_node(&b.body).unwrap();

        assert_eq!(
            stripped.install(&addon).unwrap_err().to_string(),
            InstallError::Missing {
                id: Required::Part(b.body.clone()),
                field: "weld end",
                owner: hat.to_string(),
            }
            .to_string()
        );
    }

    /// A mask source may be a composite; a weld end may not. While the two
    /// shared one requirement kind, a pair-less weld into a base composite
    /// installed, and the merged model could no longer be written.
    #[test]
    fn a_weld_end_has_to_be_a_part_where_a_mask_source_need_not() {
        let b = base();
        let mut addon = hat_addon(&b);
        let hat = named(&addon, "Hat");
        for slot in ["left", "right"] {
            addon.slot_delete(&hat, &slot_id(slot)).unwrap();
        }
        assert!(addon.welds().iter().all(|w| w.pairs().is_empty()));

        // The same Ids as `base()`, but `Body` is a composite: drawn, so the
        // hat's mask is satisfied, and not a part, so its weld is not.
        let mut composite = Model::new();
        let root = composite.root().unwrap().clone();
        composite
            .add_node_with_id(
                b.body.clone(),
                &root,
                ModelNode::new("Body", ModelNodeKind::Composite(ModelComposite::new())),
            )
            .unwrap();
        composite
            .add_node_with_id(
                b.head.clone(),
                &root,
                ModelNode::new("Head", ModelNodeKind::Group),
            )
            .unwrap();
        composite
            .add_param_with_id(
                b.param.clone(),
                ModelParam::new(Name::truncated("Lean"), -1.0, 1.0, 0.0),
            )
            .unwrap();

        assert_eq!(
            composite.install(&addon).unwrap_err().to_string(),
            InstallError::Missing {
                id: Required::Part(b.body.clone()),
                field: "weld end",
                owner: hat.to_string(),
            }
            .to_string()
        );
        composite
            .to_clm_file()
            .expect("a refused install leaves a model that still saves");
    }

    /// A fragment may hold a weld whose ends are both in the base — neither
    /// can be checked until install. Install used to append it, so a base
    /// that already welded those two parts held the pair twice: a model that
    /// saved and that no reader would take back.
    #[test]
    fn a_weld_between_two_base_parts_is_not_the_addons_to_install() {
        let mut b = base();
        let arm =
            b.m.add_node(
                &b.root,
                ModelNode::new("Arm", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut b.hex,
            )
            .unwrap();
        b.m.set_welds(vec![ModelWeld::new(
            b.body.clone(),
            arm.clone(),
            Vec::new(),
        )])
        .unwrap();

        let mut addon = author(&b, |m, hex| {
            let hat = m
                .add_node(
                    &b.head,
                    ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                    hex,
                )
                .unwrap();
            vec![hat]
        });
        addon
            .set_welds(vec![ModelWeld::new(
                arm.clone(),
                b.body.clone(),
                Vec::new(),
            )])
            .expect("a fragment cannot see either end, so it takes the weld");

        let before = b.m.generation();
        let err = b.m.install(&addon).unwrap_err();
        assert!(
            matches!(&err, InstallError::WeldsOffAddon { a, b: other }
                if *a == arm.to_string() && *other == b.body.to_string()),
            "{err:?}"
        );
        assert_eq!(b.m.generation(), before, "no half-install");
        assert_eq!(b.m.welds().len(), 1);
        Model::from_clm_bytes(&b.m.to_clm_bytes().unwrap())
            .expect("the base still saves and loads");
    }

    /// Install holds an addon's own weld ends to the weld rule too, rather
    /// than trusting the addon to have kept it: a welded end that is not a
    /// part would merge into a model that cannot be written.
    #[test]
    fn an_addon_weld_end_that_is_not_a_part_is_refused() {
        let b = base();
        let mut addon = hat_addon(&b);
        let hat = named(&addon, "Hat");
        // No edit builds this state; it is what a kind swap used to leave.
        addon.bindings.clear();
        addon.nodes.get_mut(&hat).unwrap().kind = ModelNodeKind::Group;

        let mut merged = b.m.clone();
        let err = merged.install(&addon).unwrap_err();
        assert!(
            matches!(&err, InstallError::WeldEndNotAPart { node, kind: "group" }
                if *node == hat.to_string()),
            "{err:?}"
        );
        merged
            .to_clm_file()
            .expect("a refused install leaves a model that still saves");
    }

    #[test]
    fn a_provided_id_the_base_already_has_is_refused() {
        let mut b = base();
        let addon = hat_addon(&b);
        b.m.install(&addon).unwrap();
        let before = b.m.node_count();

        assert_eq!(
            b.m.install(&addon).unwrap_err().to_string(),
            InstallError::Collision {
                kind: "node",
                id: named(&b.m, "Hat").to_string(),
            }
            .to_string()
        );
        assert_eq!(
            b.m.node_count(),
            before,
            "a refused install changes nothing"
        );
    }

    /// Decision 2: two addons that provide one Id are alternatives, and the
    /// second is refused rather than renamed around.
    #[test]
    fn two_addons_providing_shoes_cannot_coexist() {
        let b = base();
        let shoes = |style: &str| {
            author(&b, |m, _| {
                let id = NodeId::new("shoes").unwrap();
                m.add_node_with_id(
                    id.clone(),
                    &b.root,
                    ModelNode::new(style, ModelNodeKind::Part(ModelPart::new(quad()))),
                )
                .unwrap();
                vec![id]
            })
        };
        let (boots, sandals) = (shoes("Boots"), shoes("Sandals"));

        let mut m = b.m.clone();
        m.install(&boots).unwrap();
        assert_eq!(
            m.install(&sandals).unwrap_err().to_string(),
            InstallError::Collision {
                kind: "node",
                id: "shoes".into(),
            }
            .to_string()
        );
        assert_eq!(
            m.node(&NodeId::new("shoes").unwrap())
                .unwrap()
                .name
                .as_str(),
            "Boots"
        );

        // Either one on its own, though — that is what makes it a slot.
        let mut m = b.m.clone();
        m.install(&sandals).unwrap();
        assert_eq!(
            m.node(&NodeId::new("shoes").unwrap())
                .unwrap()
                .name
                .as_str(),
            "Sandals"
        );
    }

    #[test]
    fn a_texture_id_the_base_already_has_is_refused() {
        let b = base();
        let mut addon = hat_addon(&b);
        // Re-upload the hat's texture under the Id the base already uses.
        let hat = named(&addon, "Hat");
        addon
            .add_texture_with_id(b.tex.clone(), &hat, texture(3))
            .unwrap();
        assert_eq!(
            b.m.clone().install(&addon).unwrap_err().to_string(),
            InstallError::Collision {
                kind: "texture",
                id: b.tex.to_string(),
            }
            .to_string()
        );
    }

    #[test]
    fn an_addon_never_carries_a_param() {
        let b = base();
        let mut addon = hat_addon(&b);
        let mine = ParamId::new("mine").unwrap();
        addon
            .add_param_with_id(
                mine.clone(),
                ModelParam::new(Name::truncated("Mine"), 0.0, 1.0, 0.0),
            )
            .unwrap();
        assert_eq!(
            b.m.clone().install(&addon).unwrap_err().to_string(),
            InstallError::CarriesParam {
                param: mine.to_string()
            }
            .to_string()
        );
    }

    /// A binding on a base node would be an edit to the base with no Id of its
    /// own to collide on, so two addons could fight over one node silently.
    #[test]
    fn an_addon_binds_only_its_own_nodes() {
        let b = base();
        let addon = author(&b, |m, hex| {
            let hat = m
                .add_node(
                    &b.head,
                    ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                    hex,
                )
                .unwrap();
            let key = BindingKey::new(b.param.clone(), b.body.clone(), BindingTarget::Deform);
            m.add_binding(&key).unwrap();
            vec![hat]
        });
        // `extract` never cuts a binding off its node, so this one has to be
        // moved onto the fragment by hand.
        let mut addon = addon;
        let key = BindingKey::new(b.param.clone(), b.body.clone(), BindingTarget::Deform);
        addon.bindings.push(ModelBinding {
            key_positions: vec![vec![0.0, 1.0]],
            key: key.clone(),
            interpolate_mode: InterpolateMode::Linear,
            values: deform_values(),
            dense: OnceLock::new(),
        });
        assert_eq!(
            b.m.clone().install(&addon).unwrap_err().to_string(),
            InstallError::BindsOffAddon {
                node: b.body.to_string(),
                target: "deform",
            }
            .to_string()
        );
    }

    fn deform_values() -> ModelBindingValues {
        ClmBindingValues::Deform(crate::formats::clm::ClmCells { cells: Vec::new() }).into()
    }

    #[test]
    fn a_complete_model_is_not_an_addon() {
        let b = base();
        let other = base();
        assert_eq!(
            b.m.clone().install(&other.m).unwrap_err().to_string(),
            InstallError::NotAnAddon {
                node: other.m.root().unwrap().to_string(),
            }
            .to_string()
        );
    }

    /// Install's response check is the model's own, so the refusal is the
    /// model's own error — something a caller can branch on — and not just
    /// its message.
    #[test]
    fn a_refused_response_carries_the_models_own_error() {
        let b = base();
        let addon = author(&b, |m, hex| {
            let sway = m
                .add_node(
                    &b.head,
                    ModelNode::new(
                        "Sway",
                        ModelNodeKind::MotionResponse(crate::MotionResponse {
                            output_param: Some(b.param.clone()),
                            ..Default::default()
                        }),
                    ),
                    hex,
                )
                .unwrap();
            vec![sway]
        });
        // A response's output rests at zero; this base's param no longer does.
        let mut moved = b.m.clone();
        moved.set_param_default(&b.param, 0.5).unwrap();
        let before = moved.generation();

        let err = moved.install(&addon).unwrap_err();
        let sway = named(&addon, "Sway");
        assert!(
            matches!(
                &err,
                InstallError::Model(e)
                    if matches!(&**e, ModelError::InvalidResponse { node, .. } if *node == sway)
            ),
            "{err:?}"
        );
        assert_eq!(moved.generation(), before, "no half-install");
        assert_eq!(moved.node_count(), b.m.node_count());
    }

    #[test]
    fn a_weld_from_an_addon_resolves_against_the_base_part_after_install() {
        let mut b = base();
        let addon = hat_addon(&b);
        assert!(
            addon.welds()[0].resolve(&addon).is_empty(),
            "the base end is not there to resolve against yet"
        );

        b.m.install(&addon).unwrap();
        let weld =
            b.m.welds()
                .iter()
                .find(|w| w.a() == &named(&b.m, "Hat"))
                .unwrap();
        let pairs = weld.resolve(&b.m);
        assert_eq!(pairs.len(), 2);
        assert_eq!(
            (pairs[0].a_vert, pairs[0].b_vert, pairs[0].weight),
            (2, 0, 0.5)
        );
        assert_eq!(
            (pairs[1].a_vert, pairs[1].b_vert, pairs[1].weight),
            (3, 1, 0.25)
        );
    }

    #[test]
    fn an_addons_animation_installs_and_its_lane_param_is_a_requirement() {
        let b = base();
        let mut addon = hat_addon(&b);
        addon.animations.push(ClmAnimation {
            name: "tip".into(),
            length: 8,
            lanes: vec![ClmLane {
                param: b.param.clone(),
                interpolation: InterpolateMode::Linear,
                keyframes: vec![ClmKeyframe {
                    frame: 2,
                    value: 0.5,
                }],
            }],
            ..ClmAnimation::default()
        });
        assert!(addon
            .requirements()
            .iter()
            .any(|r| r.field == "animation lane"));

        let mut m = b.m.clone();
        let installed = m.install(&addon).unwrap();
        assert_eq!(m.animations().len(), 1);
        m.uninstall(&installed);
        assert!(m.animations().is_empty());
    }

    #[test]
    fn uninstall_takes_back_exactly_what_install_added() {
        let b = base();
        let addon = hat_addon(&b);
        let mut m = b.m.clone();

        let installed = m.install(&addon).unwrap();
        m.uninstall(&installed);

        assert_eq!(m.node_count(), b.m.node_count());
        assert_eq!(m.nodes_in_order(), b.m.nodes_in_order());
        assert_eq!(m.texture_ids(), b.m.texture_ids());
        assert_eq!(m.param_ids(), b.m.param_ids());
        assert_eq!(m.bindings().count(), b.m.bindings().count());
        assert_eq!(m.welds().len(), b.m.welds().len());
        assert_eq!(m.to_clm_bytes().unwrap(), b.m.to_clm_bytes().unwrap());
    }

    /// Uninstall used to remove only the nodes the receipt listed. A node
    /// added under an addon node since the install was left behind with a
    /// parent that no longer existed: unreachable from the root, still
    /// holding its binding, its weld, its texture and the mask it was the
    /// source of.
    #[test]
    fn uninstall_takes_an_installed_nodes_later_children_with_it() {
        let b = base();
        let addon = hat_addon(&b);
        let mut m = b.m.clone();
        let mut hex = SeededHex::new(99);
        let installed = m.install(&addon).unwrap();
        let hat = named(&m, "Hat");

        // Authored after the install, under an installed node, and reaching
        // into the base every way a node can.
        let feather = m
            .add_node(
                &hat,
                ModelNode::new("Feather", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut hex,
            )
            .unwrap();
        let feather_tex = m.add_texture(&feather, texture(9), &mut hex).unwrap();
        let key = BindingKey::new(b.param.clone(), feather.clone(), BindingTarget::Deform);
        m.add_binding(&key).unwrap();
        m.slot_add(&feather, slot_id("tip")).unwrap();
        let mut welds = m.welds().to_vec();
        welds.push(ModelWeld::new(
            feather.clone(),
            b.body.clone(),
            vec![SlotPair {
                a: slot_id("tip"),
                b: slot_id("left"),
                weight: 0.5,
            }],
        ));
        m.set_welds(welds).unwrap();
        m.mask_add(&b.body, &feather, MaskMode::Mask).unwrap();

        m.uninstall(&installed);

        assert!(m.node(&feather).is_none(), "the child went with its parent");
        assert_eq!(
            m.node_count(),
            b.m.node_count(),
            "no node is left that the root does not reach"
        );
        assert!(m.binding(&key).is_none());
        assert!(m.texture(&feather_tex).is_none());
        assert_eq!(m.to_clm_bytes().unwrap(), b.m.to_clm_bytes().unwrap());
    }

    /// The whole point of the file shape: an addon survives being a file.
    #[test]
    fn an_addon_installs_the_same_after_a_clm_round_trip() {
        let b = base();
        let addon = hat_addon(&b);
        let bytes = addon.to_clm_bytes().unwrap();
        let reopened = Model::from_clm_bytes_fragment(&bytes).unwrap();
        assert_eq!(reopened.to_clm_bytes().unwrap(), bytes);
        assert_eq!(
            listed(&reopened.requirements()),
            listed(&addon.requirements())
        );

        let (mut direct, mut through_a_file) = (b.m.clone(), b.m.clone());
        direct.install(&addon).unwrap();
        through_a_file.install(&reopened).unwrap();
        assert_eq!(
            through_a_file.to_clm_bytes().unwrap(),
            direct.to_clm_bytes().unwrap()
        );
    }

    /// An addon carries the textures its parts draw — all of them. A texture
    /// the base still draws is copied rather than left as a requirement,
    /// because a texture is not a requirement kind any more.
    #[test]
    fn extract_copies_a_texture_the_base_still_draws() {
        let mut b = base();
        let hat =
            b.m.add_node(
                &b.head,
                ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut b.hex,
            )
            .unwrap();
        let mine = b.m.add_texture(&hat, texture(7), &mut b.hex).unwrap();
        let brim =
            b.m.add_node(
                &hat,
                ModelNode::new("Brim", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut b.hex,
            )
            .unwrap();
        // Shared with `Body`, which stays behind.
        b.m.set_part_albedo(&brim, Some(b.tex.clone())).unwrap();

        let addon = b.m.extract(&[hat]);
        // Texture order is the base's, and the base's own came first.
        assert_eq!(addon.texture_ids(), [b.tex.clone(), mine]);
        assert!(addon.requirements().iter().all(|r| r.field != "albedo"));
        // The cut is not a move: the base still draws it too.
        assert!(b.m.texture(&b.tex).is_some());
    }

    /// Cutting a complete model's root used to hand back a model that was
    /// neither shape: its root named no parent, so the fragment reader
    /// refused it, and its bindings named params that had stayed behind, so
    /// the complete reader did too. A parentless node is dropped from the cut
    /// like one that is not there.
    #[test]
    fn a_parentless_root_cannot_be_cut_into_a_model_no_reader_takes() {
        let mut b = base();
        let key = BindingKey::new(b.param.clone(), b.body.clone(), BindingTarget::Deform);
        b.m.add_binding(&key).unwrap();

        let cut = b.m.extract(std::slice::from_ref(&b.root));
        assert_eq!(cut.node_count(), 0, "the root is not there to cut");
        assert!(cut.roots().is_empty());
        assert_eq!(cut.bindings().count(), 0);
        let reopened = Model::from_clm_bytes_fragment(&cut.to_clm_bytes().unwrap())
            .expect("what is left is an empty fragment, which reads back");
        assert_eq!(reopened.node_count(), 0);

        // Naming the root beside a node inside it does not hide that node
        // behind an ancestor that was never going to be cut.
        let cut = b.m.extract(&[b.root.clone(), b.body.clone()]);
        assert_eq!(cut.roots(), std::slice::from_ref(&b.body));
        assert_eq!(cut.bindings().count(), 1);
        Model::from_clm_bytes_fragment(&cut.to_clm_bytes().unwrap()).unwrap();
    }

    /// Cut, delete, install: the base that dropped the subtree dropped its
    /// texture with it, so the Id the addon now provides is free and the
    /// round trip closes.
    #[test]
    fn an_extracted_addon_installs_into_the_base_the_cut_left_behind() {
        let mut b = base();
        let hat =
            b.m.add_node(
                &b.head,
                ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut b.hex,
            )
            .unwrap();
        let mine = b.m.add_texture(&hat, texture(7), &mut b.hex).unwrap();
        let before = b.m.to_clm_bytes().unwrap();

        let addon = b.m.extract(std::slice::from_ref(&hat));
        assert_eq!(addon.texture_ids(), std::slice::from_ref(&mine));

        let mut cut = b.m.clone();
        cut.delete_node(&hat).unwrap();
        assert!(
            cut.texture(&mine).is_none(),
            "the only part drawing it went with the subtree"
        );

        cut.install(&addon).unwrap();
        assert_eq!(cut.to_clm_bytes().unwrap(), before);
    }

    #[test]
    fn extract_drops_a_root_that_sits_inside_another_one() {
        let mut b = base();
        let child =
            b.m.add_node(
                &b.head,
                ModelNode::new("Ear", ModelNodeKind::Group),
                &mut b.hex,
            )
            .unwrap();
        let missing = NodeId::new("nope").unwrap();
        let addon = b.m.extract(&[child.clone(), b.head.clone(), missing]);
        assert_eq!(addon.roots(), [b.head.clone()]);
        assert_eq!(addon.node_count(), 2);
        assert!(addon.node(&child).is_some());
    }

    /// A fragment is still an editable model: its roots are a sibling list of
    /// their own, because their parents are not here to hold one.
    #[test]
    fn a_fragment_is_edited_like_any_other_model() {
        let b = base();
        let mut addon = hat_addon(&b);
        let (hat, sway) = (named(&addon, "Hat"), named(&addon, "Sway"));
        assert_eq!(addon.roots(), [hat.clone(), sway.clone()]);

        addon.reorder(&sway, 0).unwrap();
        assert_eq!(addon.roots(), [sway.clone(), hat.clone()]);

        // A duplicated root lands beside the original, still a root.
        let mut hex = SeededHex::new(5);
        let copy = addon.duplicate_subtree(&hat, &mut hex).unwrap();
        assert_eq!(addon.roots(), [sway.clone(), hat.clone(), copy.clone()]);
        assert!(addon.is_fragment());

        // Reparenting one root under another takes it off the root list.
        addon.reparent(&copy, &hat).unwrap();
        assert_eq!(addon.roots(), [sway.clone(), hat.clone()]);
        assert_eq!(
            addon.node(&hat).unwrap().children(),
            std::slice::from_ref(&copy)
        );

        addon.delete_node(&sway).unwrap();
        assert_eq!(addon.roots(), std::slice::from_ref(&hat));

        // Two hats now, so install renames nothing and both arrive.
        let mut m = b.m.clone();
        m.install(&addon).unwrap();
        assert_eq!(m.node(&copy).unwrap().parent(), Some(&hat));
    }

    /// A fragment's dangling weld end is a requirement, not a broken weld, so
    /// the editor's lint pass has nothing to say about it.
    #[test]
    fn check_does_not_lint_a_fragments_dangling_weld() {
        let b = base();
        let addon = hat_addon(&b);
        assert!(
            !addon
                .check()
                .iter()
                .any(|w| w.message.contains("no longer a part")),
            "{:?}",
            addon.check()
        );
    }

    /// Extracting from the root used to give back the whole tree as a
    /// "complete model" — complete only while nothing in it was bound or
    /// animated, since params never travel. The root is not cut at all now
    /// (`a_parentless_root_cannot_be_cut_into_a_model_no_reader_takes`), so
    /// what comes back is always a fragment.
    #[test]
    fn extracting_the_whole_tree_is_an_empty_fragment() {
        let b = base();
        let whole = b.m.extract(std::slice::from_ref(&b.root));
        assert!(whole.is_fragment());
        assert_eq!(whole.root(), None);
        assert_eq!(whole.node_count(), 0);
        assert!(whole.requirements().is_empty());
    }
}
