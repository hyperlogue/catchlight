//! Addons: a partial model, authored against a base model and installed into
//! it by Id.
//!
//! Rules install enforces:
//! - An Id an addon provides is exclusive; a collision is refused, not renamed
//!   around, since renaming would break the addon's internal references.
//! - An addon carries its own textures and no params. Each maintained
//!   extension it carries is merged by its registered type; a plain one is
//!   refused, having nothing to merge it. Bindings
//!   and welds must touch a node the addon provides; one wholly on base nodes
//!   would be an unowned edit two addons could fight over.
//! - Install is one edit: a refused install leaves the base untouched, and
//!   success bumps the generation once.
//! - Install appends: an addon records no position among its parent's children.

use std::fmt;
use std::sync::OnceLock;

use super::*;
use crate::extension::{MergeRefused, Receipt, Stored, Typed, Unread};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Required {
    /// What a fragment root's `parent` names.
    Node(NodeId),
    /// A part or a composite: what a mask source names.
    Drawable(NodeId),
    /// A part only: what a pair-less weld end names. A composite can clip but
    /// has no slots and cannot be welded.
    Part(NodeId),
    Param(ParamId),
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Requirement {
    pub id: Required,
    pub field: &'static str,
    /// The animation, for a lane; the extension, for a reference.
    pub owner: String,
}

/// Sorted, with the same Id listed once per field that names it.
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

fn sorted<T: Ord>(it: impl Iterator<Item = T>) -> impl Iterator<Item = T> {
    let mut v: Vec<T> = it.collect();
    v.sort();
    v.dedup();
    v.into_iter()
}

/// On any refusal the base model is untouched.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InstallError {
    /// Carried whole so a caller can branch on it.
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
        "the addon carries plain extension {key:?}: a plain annotation has no module to merge \
         it into the base, so only a maintained extension may ride in an addon"
    )]
    CarriesExtension { key: String },
    #[error(
        "the addon carries extension {key:?}, which this program has not registered, so it \
         cannot be merged; register {key:?} before installing"
    )]
    ExtensionNotRegistered { key: String },
    #[error("extension {key:?} cannot be read, so the addon's cannot be merged into it: {why}")]
    ExtensionUnread { key: String, why: Unread },
    #[error("extension {key:?} refused the addon's data: {why}")]
    ExtensionRefused { key: String, why: MergeRefused },
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

#[derive(Debug, Clone)]
pub struct Installed {
    nodes: Vec<NodeId>,
    roots: Vec<NodeId>,
    textures: Vec<TexId>,
    bindings: Vec<BindingKey>,
    welds: Vec<ModelWeld>,
    animations: Vec<ClmAnimation>,
    /// What each extension merged, for its uninstall to take back.
    extensions: Vec<(ExtensionKey, Receipt)>,
}

/// An extension merge worked out before the install touches the base.
struct Merge {
    key: ExtensionKey,
    merged: Box<dyn crate::extension::DynExtension>,
    receipt: Receipt,
}

impl Installed {
    /// In tree order.
    pub fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }

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

    /// The keys of the extensions the install added or merged into.
    pub fn extensions(&self) -> impl Iterator<Item = &ExtensionKey> {
        self.extensions.iter().map(|(key, _)| key)
    }
}

impl Model {
    /// Empty for a complete model.
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
                // A weld with no pairs still names the part.
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

        for key in self.extensions.keys() {
            let owner = format_args!("extension {:?}", key.as_str());
            for r in self.extensions.refs(key).unwrap_or_default() {
                let id = match r {
                    IdRef::Node(id) if !self.nodes.contains_key(id) => Required::Node(id.clone()),
                    IdRef::Param(id) if !self.params.contains_key(id) => {
                        Required::Param(id.clone())
                    }
                    IdRef::Slot(node, slot) if !self.nodes.contains_key(node) => {
                        Required::Slot(node.clone(), slot.clone())
                    }
                    _ => continue,
                };
                need(id, "extension reference", &owner);
            }
        }

        out.sort();
        out.dedup();
        Requirements { entries: out }
    }

    /// On failure this model is untouched; on success the generation is bumped once.
    pub fn install(&mut self, addon: &Model) -> Result<Installed, InstallError> {
        self.check_install(addon)?;
        let merges = self.plan_extension_merges(addon)?;
        let order = addon.nodes_in_order();
        let mut installed = Installed {
            nodes: Vec::with_capacity(order.len()),
            roots: addon.roots.clone(),
            textures: addon.texture_order.clone(),
            bindings: addon.bindings.iter().map(|b| b.key.clone()).collect(),
            welds: addon.welds.clone(),
            animations: addon.animations.clone(),
            extensions: Vec::with_capacity(merges.len()),
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
                // The base's key positions are what this binding fills against.
                dense: OnceLock::new(),
            });
        }

        self.animations.extend(addon.animations.iter().cloned());
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
        for Merge {
            key,
            merged,
            receipt,
        } in merges
        {
            self.extensions
                .map
                .insert(key.clone(), Stored::typed(merged));
            installed.extensions.push((key, receipt));
        }
        self.bump();
        Ok(installed)
    }

    /// Refuses before anything changes: an unregistered key, data either side
    /// cannot read, or a merge the extension refuses.
    fn plan_extension_merges(&self, addon: &Model) -> Result<Vec<Merge>, InstallError> {
        let mut merges = Vec::new();
        for (key, stored) in &addon.extensions.map {
            let refused = |why: Unread| InstallError::ExtensionUnread {
                key: key.to_string(),
                why,
            };
            let theirs = match Self::registered_extension(stored, key) {
                Some(Ok(typed)) => typed,
                Some(Err(why)) => return Err(refused(why)),
                None => {
                    return Err(InstallError::ExtensionNotRegistered {
                        key: key.to_string(),
                    })
                }
            };
            let (merged, receipt) = match self.extensions.map.get(key) {
                // The same path as a base with data, so uninstall takes back
                // only what was merged and keeps what was added since.
                None => theirs.data.dyn_default().dyn_merge_addon(&*theirs.data),
                Some(base) => {
                    let ours: Arc<Typed> = match Self::registered_extension(base, key) {
                        Some(Ok(typed)) => typed,
                        Some(Err(why)) => return Err(refused(why)),
                        None => {
                            return Err(InstallError::ExtensionNotRegistered {
                                key: key.to_string(),
                            })
                        }
                    };
                    ours.data.dyn_merge_addon(&*theirs.data)
                }
            }
            .map_err(|why| InstallError::ExtensionRefused {
                key: key.to_string(),
                why,
            })?;
            merges.push(Merge {
                key: key.clone(),
                merged,
                receipt,
            });
        }
        Ok(merges)
    }

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
        if let Some(key) = addon
            .extensions
            .keys()
            .find(|key| addon.extensions.refs(*key).is_none())
        {
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
                Required::Drawable(id) => self
                    .nodes
                    .get(id)
                    .map(|n| &n.kind)
                    .is_some_and(is_mask_source),
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

    /// A parentless root (or one not here, or nested in another given) is dropped:
    /// it would yield a model neither reader accepts. A texture a part left behind
    /// also draws is copied, not moved.
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
            extensions: Extensions::default(),
            unknown_sections: Vec::new(),
            binding_index: OnceLock::new(),
            bake: Default::default(),
        }
    }

    /// Total. An installed node goes with its whole current subtree, including
    /// nodes added under it since; anything already deleted is skipped.
    pub fn uninstall(&mut self, installed: &Installed) {
        let mut removed: HashSet<NodeId> = HashSet::new();
        for id in &installed.nodes {
            if self.nodes.contains_key(id) && !removed.contains(id) {
                removed.extend(self.subtree(id));
            }
        }
        self.remove_nodes(&removed);

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
        self.remove_in_extensions(
            |r| matches!(r, IdRef::Texture(t) if installed.textures.contains(t)),
        );
        self.gc_textures();
        retain_less(&mut self.welds, &installed.welds, |_| true);
        retain_less(&mut self.animations, &installed.animations, |_| true);
        for (key, receipt) in &installed.extensions {
            let Some(stored) = self.extensions.map.get(key) else {
                continue;
            };
            // Data no registered type reads cannot be unmerged; it stays as it
            // is.
            let Some(Ok(mut typed)) = Self::registered_extension(stored, key) else {
                continue;
            };
            Arc::make_mut(&mut typed)
                .data_mut()
                .dyn_unmerge_addon(receipt);
            self.extensions
                .map
                .insert(key.clone(), Stored::Typed(typed));
        }
        self.bump();
    }

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

    fn author(base: &Base, f: impl FnOnce(&mut Model, &mut SeededHex) -> Vec<NodeId>) -> Model {
        let mut scratch = base.m.clone();
        let mut hex = base.hex.clone();
        let roots = f(&mut scratch, &mut hex);
        scratch.extract(&roots)
    }

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
        assert_eq!(b.m.param_ids(), [b.param.clone()]);
    }

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

    #[test]
    fn a_weld_end_has_to_be_a_part_where_a_mask_source_need_not() {
        let b = base();
        let mut addon = hat_addon(&b);
        let hat = named(&addon, "Hat");
        for slot in ["left", "right"] {
            addon.slot_delete(&hat, &slot_id(slot)).unwrap();
        }
        assert!(addon.welds().iter().all(|w| w.pairs().is_empty()));

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

    #[test]
    fn an_addon_weld_end_that_is_not_a_part_is_refused() {
        let b = base();
        let mut addon = hat_addon(&b);
        let hat = named(&addon, "Hat");
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

    #[test]
    fn uninstall_takes_an_installed_nodes_later_children_with_it() {
        let b = base();
        let addon = hat_addon(&b);
        let mut m = b.m.clone();
        let mut hex = SeededHex::new(99);
        let installed = m.install(&addon).unwrap();
        let hat = named(&m, "Hat");

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
        b.m.set_part_albedo(&brim, Some(b.tex.clone())).unwrap();

        let addon = b.m.extract(&[hat]);
        assert_eq!(addon.texture_ids(), [b.tex.clone(), mine]);
        assert!(addon.requirements().iter().all(|r| r.field != "albedo"));
        assert!(b.m.texture(&b.tex).is_some());
    }

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

        let cut = b.m.extract(&[b.root.clone(), b.body.clone()]);
        assert_eq!(cut.roots(), std::slice::from_ref(&b.body));
        assert_eq!(cut.bindings().count(), 1);
        Model::from_clm_bytes_fragment(&cut.to_clm_bytes().unwrap()).unwrap();
    }

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

    #[test]
    fn a_fragment_is_edited_like_any_other_model() {
        let b = base();
        let mut addon = hat_addon(&b);
        let (hat, sway) = (named(&addon, "Hat"), named(&addon, "Sway"));
        assert_eq!(addon.roots(), [hat.clone(), sway.clone()]);

        addon.reorder(&sway, 0).unwrap();
        assert_eq!(addon.roots(), [sway.clone(), hat.clone()]);

        let mut hex = SeededHex::new(5);
        let copy = addon.duplicate_subtree(&hat, &mut hex).unwrap();
        assert_eq!(addon.roots(), [sway.clone(), hat.clone(), copy.clone()]);
        assert!(addon.is_fragment());

        addon.reparent(&copy, &hat).unwrap();
        assert_eq!(addon.roots(), [sway.clone(), hat.clone()]);
        assert_eq!(
            addon.node(&hat).unwrap().children(),
            std::slice::from_ref(&copy)
        );

        addon.delete_node(&sway).unwrap();
        assert_eq!(addon.roots(), std::slice::from_ref(&hat));

        let mut m = b.m.clone();
        m.install(&addon).unwrap();
        assert_eq!(m.node(&copy).unwrap().parent(), Some(&hat));
    }

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
