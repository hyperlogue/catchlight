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
//!   `shoes` are alternatives, and only one is installed at a time. A seam Id
//!   is unique per part rather than per model, so an addon's seams cannot
//!   collide on their own: the part carrying them collides first.
//! - **An addon never adds or changes a param.** A `ParamId` it uses has to be
//!   in the base already — that is a requirement like any other — so
//!   [`Model::extract`] carries no params and install refuses an addon that
//!   does ([`InstallError::CarriesParam`]). For the same reason a binding may
//!   only drive a node the addon itself provides
//!   ([`InstallError::BindsOffAddon`]): a binding on a *base* node would be an
//!   edit to the base with no Id of its own to collide on, so two addons could
//!   silently fight over one node's deform.
//! - **Install is one edit.** Everything is checked against the merged model
//!   before anything moves, so a refused install leaves the base untouched,
//!   and the whole merge bumps [`Model::generation`] exactly once — a puppet
//!   ticking through an install pays one rebake and keeps its pose, which is
//!   carried by Id.
//! - **Extract is install's inverse, up to order.** `extract` takes the
//!   subtrees, the bindings on them, the welds touching them and the textures
//!   *only* they use. Installing that back into the base it was cut from
//!   restores every value — but install appends: an addon's roots go last
//!   among their parent's children and its textures last in texture order,
//!   because an addon records no position to be put back into.
//! - **Animations travel, lanes are not extracted.** A lane addresses a param,
//!   and params stay in the base, so there is no such thing as the animations
//!   "over" a subtree and `extract` returns none. An addon that *was* authored
//!   with animations installs them, and its lanes' params are requirements
//!   like any others.
//! - **The world is the base's.** An addon carries the physics settings of
//!   whatever it was cut from; install keeps the base model's and ignores the
//!   addon's.
//! - **What an addon cannot express is a change to a base node.** A mask lives
//!   on the node it clips, so a *base* drawable masked by an addon's part is
//!   not something an addon can carry: [`Model::extract`] leaves that mask
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
    /// A node that has to be a part — what a mask's source names.
    Part(NodeId),
    Param(ParamId),
    Texture(TexId),
    /// A seam on a base part, which needs the part and the seam both — what
    /// one end of a weld names.
    Seam(NodeId, SeamId),
}

impl fmt::Display for Required {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(id) => write!(f, "node {:?}", id.as_str()),
            Self::Part(id) => write!(f, "part {:?}", id.as_str()),
            Self::Param(id) => write!(f, "param {:?}", id.as_str()),
            Self::Texture(id) => write!(f, "texture {:?}", id.as_str()),
            Self::Seam(node, seam) => {
                write!(f, "seam {:?} on part {:?}", seam.as_str(), node.as_str())
            }
        }
    }
}

/// One thing an addon needs from the base model: the Id, the field that names
/// it, and what in the addon carries that field.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Requirement {
    pub id: Required,
    /// `"parent"`, `"albedo"`, `"mask source"`, `"physics target"`,
    /// `"binding param"`, `"binding node"`, `"weld end"` or
    /// `"animation lane"`.
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
    /// have to be parts and the ones a weld end reaches through.
    pub fn nodes(&self) -> impl Iterator<Item = &NodeId> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Node(id) | Required::Part(id) | Required::Seam(id, _) => Some(id),
            _ => None,
        }))
    }

    pub fn params(&self) -> impl Iterator<Item = &ParamId> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Param(id) => Some(id),
            _ => None,
        }))
    }

    pub fn textures(&self) -> impl Iterator<Item = &TexId> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Texture(id) => Some(id),
            _ => None,
        }))
    }

    pub fn seams(&self) -> impl Iterator<Item = (&NodeId, &SeamId)> {
        sorted(self.entries.iter().filter_map(|r| match &r.id {
            Required::Seam(node, seam) => Some((node, seam)),
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
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
    #[error("the addon welds seam {seam:?} on its own node {node:?}, which carries no such seam")]
    UnknownSeam { node: String, seam: String },
    #[error(
        "the addon's weld between seam {a:?} and seam {b:?} does not weight each of the two \
         seams' slots exactly once"
    )]
    WeldSlotMismatch { a: String, b: String },
}

/// What one [`Model::install`] put into a base model, so [`Model::uninstall`]
/// can take back exactly that and nothing else.
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
    /// Every node the install added, in document order.
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
            match &node.kind {
                ModelNodeKind::Part(p) => {
                    if let Some(t) = p.albedo() {
                        if !self.textures.contains_key(t) {
                            need(Required::Texture(t.clone()), "albedo", &id);
                        }
                    }
                }
                ModelNodeKind::SimplePhysics(ph) => {
                    for t in ph.target_params().iter().flatten() {
                        if !self.params.contains_key(t) {
                            need(Required::Param(t.clone()), "physics target", &id);
                        }
                    }
                }
                _ => {}
            }
            for mask in node.masks().unwrap_or_default() {
                if !self.nodes.contains_key(mask.source()) {
                    need(Required::Part(mask.source().clone()), "mask source", &id);
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
            for (end, far) in [(&w.a, &w.b), (&w.b, &w.a)] {
                if !self.nodes.contains_key(&end.0) {
                    need(
                        Required::Seam(end.0.clone(), end.1.clone()),
                        "weld end",
                        &far.0,
                    );
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

        for id in order {
            let Some(node) = addon.nodes.get(&id) else {
                continue;
            };
            self.nodes.insert(id.clone(), node.clone());
            installed.nodes.push(id);
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

        for b in &addon.bindings {
            self.bindings.push(ModelBinding {
                key: b.key.clone(),
                interpolate_mode: b.interpolate_mode,
                values: b.values.clone(),
                // The addon has no params, so its grid was never derived; the
                // base's key positions are what this binding fills against.
                dense: OnceLock::new(),
            });
        }

        self.welds.extend(addon.welds.iter().cloned());
        self.animations.extend(addon.animations.iter().cloned());
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
                Required::Part(id) => {
                    matches!(
                        self.nodes.get(id).map(|n| &n.kind),
                        Some(ModelNodeKind::Part(_))
                    )
                }
                Required::Param(id) => self.params.contains_key(id),
                Required::Texture(id) => self.textures.contains_key(id),
                Required::Seam(node, seam) => self.seam(node, seam).is_some(),
            };
            if !held {
                return Err(InstallError::Missing {
                    id: req.id.clone(),
                    field: req.field,
                    owner: req.owner.clone(),
                });
            }
        }

        // Both ends of every weld resolve now: one in the addon, one in
        // whichever of the two models carries it.
        for w in &addon.welds {
            let seam_of = |end: &(NodeId, SeamId)| {
                addon
                    .seam(&end.0, &end.1)
                    .or_else(|| self.seam(&end.0, &end.1))
                    .ok_or_else(|| InstallError::UnknownSeam {
                        node: end.0.to_string(),
                        seam: end.1.to_string(),
                    })
            };
            let (a, b) = (seam_of(&w.a)?, seam_of(&w.b)?);
            let mismatch = || InstallError::WeldSlotMismatch {
                a: w.a.1.to_string(),
                b: w.b.1.to_string(),
            };
            if a.slots.len() != b.slots.len()
                || !a.slots.iter().all(|s| b.slot(&s.id).is_some())
                || w.weights.len() != a.slots.len()
            {
                return Err(mismatch());
            }
            let mut seen = HashSet::with_capacity(w.weights.len());
            for (slot, _) in w.weights.iter() {
                if a.slot(slot).is_none() || !seen.insert(slot) {
                    return Err(mismatch());
                }
            }
        }
        Ok(())
    }

    /// Cut `roots` and their subtrees out as an addon: the nodes, the bindings
    /// on them, the welds touching them, and the textures only they use.
    ///
    /// This model is not changed. A root that is not here, or that sits inside
    /// another one given, is dropped; the rest keep their `parent`, which is
    /// what makes the result a fragment. Params and animations stay behind —
    /// a lane addresses a param, so there is no animation "over" a subtree.
    pub fn extract(&self, roots: &[NodeId]) -> Model {
        let given: HashSet<&NodeId> = roots
            .iter()
            .filter(|r| self.nodes.contains_key(*r))
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
        fn albedo_of(n: &ModelNode) -> Option<&TexId> {
            match &n.kind {
                ModelNodeKind::Part(p) => p.albedo(),
                _ => None,
            }
        }
        let used_outside: HashSet<&TexId> = self
            .nodes
            .iter()
            .filter(|(id, _)| !inside(id))
            .filter_map(|(_, n)| albedo_of(n))
            .collect();
        let used_inside: HashSet<&TexId> = nodes.values().filter_map(albedo_of).collect();
        let texture_order: Vec<TexId> = self
            .texture_order
            .iter()
            .filter(|t| used_inside.contains(t) && !used_outside.contains(t))
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
            physics: self.physics,
            welds: self
                .welds
                .iter()
                .filter(|w| inside(&w.a.0) || inside(&w.b.0))
                .cloned()
                .collect(),
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
                    key: b.key.clone(),
                    interpolate_mode: b.interpolate_mode,
                    values: b.values.clone(),
                    dense: OnceLock::new(),
                })
                .collect(),
            animations: Vec::new(),
        }
    }

    /// Take back exactly what one [`Self::install`] added.
    ///
    /// Total: anything the receipt names that has since been deleted is
    /// already gone, and a node moved elsewhere in the tree is still removed.
    pub fn uninstall(&mut self, installed: &Installed) {
        let removed: HashSet<&NodeId> = installed.nodes.iter().collect();
        for r in &installed.roots {
            let parent = self.nodes.get(r).and_then(|n| n.parent.clone());
            match parent.and_then(|p| self.nodes.get_mut(&p)) {
                Some(p) => p.children.retain(|c| c != r),
                None => self.roots.retain(|x| x != r),
            }
        }
        for id in &installed.nodes {
            self.nodes.remove(id);
        }
        self.roots.retain(|r| !removed.contains(r));
        for node in self.nodes.values_mut() {
            node.children.retain(|c| !removed.contains(c));
            if let Some(masks) = node.masks_mut() {
                masks.retain(|m| !removed.contains(&m.source));
            }
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
            .retain(|b| !removed.contains(&b.key.node) && !installed.bindings.contains(&b.key));
        for t in &installed.textures {
            self.textures.remove(t);
            self.texture_order.retain(|x| x != t);
        }
        retain_less(&mut self.welds, &installed.welds, |w| {
            !removed.contains(&w.a.0) && !removed.contains(&w.b.0)
        });
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
    use crate::params::InterpolateMode;
    use crate::physics::PendulumKind;

    fn quad() -> ClmMesh {
        ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    fn seam_id(s: &str) -> SeamId {
        SeamId::new(s).unwrap()
    }

    fn slot_id(s: &str) -> SlotId {
        SlotId::new(s).unwrap()
    }

    fn texture(byte: u8) -> ModelTexture {
        ModelTexture {
            encoding: crate::formats::clm::TextureEncoding::Png,
            alpha: crate::formats::clm::TextureAlpha::Straight,
            data: Arc::new(vec![byte; 4]),
        }
    }

    /// A base model an addon can be authored against: a `body` part carrying
    /// the seam `hem`, a `head` group to hang things under, one param and one
    /// texture two parts share.
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
        let tex = m.add_texture(texture(1), &mut hex).unwrap();
        let body = m
            .add_node(
                &root,
                ModelNode::new("Body", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut hex,
            )
            .unwrap();
        m.set_part_albedo(&body, Some(tex.clone())).unwrap();
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
        m.seam_add(&body, seam_id("hem")).unwrap();
        for (slot, vertex) in [("left", 0), ("right", 1)] {
            m.slot_add(&body, &seam_id("hem"), slot_id(slot)).unwrap();
            m.slot_fill(&body, &seam_id("hem"), &slot_id(slot), vertex)
                .unwrap();
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
    /// a base parent, a base texture, a base part as a mask source, a base
    /// param through a binding and through a pendulum, and a weld into the
    /// base's `hem`.
    fn hat_addon(b: &Base) -> Model {
        author(b, |m, hex| {
            let hat = m
                .add_node(
                    &b.head,
                    ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                    hex,
                )
                .unwrap();
            m.set_part_albedo(&hat, Some(b.tex.clone())).unwrap();
            m.mask_add(&hat, &b.body, MaskMode::DodgeMask).unwrap();

            let key = BindingKey::new(b.param.clone(), hat.clone(), BindingTarget::Deform);
            m.add_binding(&key).unwrap();
            m.set_deform_vertices(&key, [1, 0], vec![0.5; 8]).unwrap();

            m.seam_add(&hat, seam_id("brim")).unwrap();
            for (slot, vertex) in [("left", 2), ("right", 3)] {
                m.slot_add(&hat, &seam_id("brim"), slot_id(slot)).unwrap();
                m.slot_fill(&hat, &seam_id("brim"), &slot_id(slot), vertex)
                    .unwrap();
            }
            let mut welds = m.welds().to_vec();
            welds.push(ModelWeld::new(
                (hat.clone(), seam_id("brim")),
                (b.body.clone(), seam_id("hem")),
                vec![(slot_id("left"), 0.5), (slot_id("right"), 0.25)],
            ));
            m.set_welds(welds).unwrap();

            let sway = m
                .add_node(
                    &b.head,
                    ModelNode::new(
                        "Sway",
                        ModelNodeKind::SimplePhysics(ModelPhysics::new(
                            PendulumKind::RigidPendulum,
                        )),
                    ),
                    hex,
                )
                .unwrap();
            m.set_physics_targets(&sway, [Some(b.param.clone()), None])
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
                    format!("part {:?}", b.body.as_str()),
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
                    "physics target",
                    sway.to_string()
                ),
                (
                    format!("texture {:?}", b.tex.as_str()),
                    "albedo",
                    hat.to_string()
                ),
                (
                    format!("seam \"hem\" on part {:?}", b.body.as_str()),
                    "weld end",
                    hat.to_string()
                ),
            ]
        );
        assert_eq!(addon.requirements().nodes().count(), 2);
        assert_eq!(addon.requirements().params().count(), 1);
        assert_eq!(addon.requirements().textures().count(), 1);
        assert_eq!(addon.requirements().seams().count(), 1);
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

        assert_eq!(b.m.generation(), generation + 1, "one edit, one bump");
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

        let err = stripped.install(&addon).unwrap_err();
        let InstallError::Missing { id, field, owner } = &err else {
            panic!("expected a missing requirement, got {err:?}");
        };
        assert_eq!(id, &Required::Param(b.param.clone()));
        assert_eq!(*field, "binding param");
        assert_eq!(owner, named(&addon, "Hat").as_str());
        assert!(err.to_string().contains(b.param.as_str()));
        assert_eq!(
            stripped.generation(),
            b.m.generation() + 1,
            "no half-install"
        );
    }

    /// A weld end is one requirement, not two: the part and the seam on it.
    #[test]
    fn a_missing_seam_is_named_with_the_part_that_should_carry_it() {
        let b = base();
        let addon = hat_addon(&b);
        let mut stripped = b.m.clone();
        stripped.seam_delete(&b.body, &seam_id("hem")).unwrap();

        assert_eq!(
            stripped.install(&addon).unwrap_err(),
            InstallError::Missing {
                id: Required::Seam(b.body.clone(), seam_id("hem")),
                field: "weld end",
                owner: named(&addon, "Hat").to_string(),
            }
        );
    }

    #[test]
    fn a_provided_id_the_base_already_has_is_refused() {
        let mut b = base();
        let addon = hat_addon(&b);
        b.m.install(&addon).unwrap();
        let before = b.m.node_count();

        assert_eq!(
            b.m.install(&addon).unwrap_err(),
            InstallError::Collision {
                kind: "node",
                id: named(&b.m, "Hat").to_string(),
            }
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
            m.install(&sandals).unwrap_err(),
            InstallError::Collision {
                kind: "node",
                id: "shoes".into(),
            }
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
        // Give the addon a texture of its own, under the base's Id.
        addon
            .add_texture_with_id(b.tex.clone(), texture(2))
            .unwrap();
        assert_eq!(
            b.m.clone().install(&addon).unwrap_err(),
            InstallError::Collision {
                kind: "texture",
                id: b.tex.to_string(),
            }
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
            b.m.clone().install(&addon).unwrap_err(),
            InstallError::CarriesParam {
                param: mine.to_string()
            }
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
            key: key.clone(),
            interpolate_mode: InterpolateMode::Linear,
            values: deform_values(),
            dense: OnceLock::new(),
        });
        assert_eq!(
            b.m.clone().install(&addon).unwrap_err(),
            InstallError::BindsOffAddon {
                node: b.body.to_string(),
                target: "deform",
            }
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
            b.m.clone().install(&other.m).unwrap_err(),
            InstallError::NotAnAddon {
                node: other.m.root().unwrap().to_string(),
            }
        );
    }

    #[test]
    fn a_weld_from_an_addon_resolves_against_the_base_seam_after_install() {
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
                .find(|w| w.a().0 == named(&b.m, "Hat"))
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

    #[test]
    fn extract_takes_the_textures_only_the_subtree_uses() {
        let mut b = base();
        let mine = b.m.add_texture(texture(7), &mut b.hex).unwrap();
        let hat =
            b.m.add_node(
                &b.head,
                ModelNode::new("Hat", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut b.hex,
            )
            .unwrap();
        b.m.set_part_albedo(&hat, Some(mine.clone())).unwrap();
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
        assert_eq!(addon.texture_ids(), [mine]);
        assert_eq!(
            addon.requirements().textures().collect::<Vec<_>>(),
            [&b.tex]
        );
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
                .any(|w| w.message.contains("carries no such seam")),
            "{:?}",
            addon.check()
        );
    }

    /// Extracting from the root gives back a complete model, not a fragment:
    /// the root's parent is `None`, which is the only thing that ever made it
    /// one.
    #[test]
    fn extracting_the_whole_tree_is_not_a_fragment() {
        let b = base();
        let whole = b.m.extract(std::slice::from_ref(&b.root));
        assert!(!whole.is_fragment());
        assert_eq!(whole.root(), Some(&b.root));
        assert!(whole.requirements().is_empty());
    }
}
