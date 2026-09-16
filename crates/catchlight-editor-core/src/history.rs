//! Bounded branching history of authored models, independent of sessions.
//!
//! Entries keep their creation revision. Navigation publishes a fresh revision
//! alias for an existing entry; neither old branches nor their creation revisions
//! disappear until retention prunes them. The caller owns the session lock,
//! revision guards, installation of returned models and observer notification.
//! Perform those steps in the same critical section as history publication.
//!
//! Retention first removes the oldest noncurrent leaf, then advances the root
//! when only the current entry's ancestor chain remains. Removing a preferred
//! child selects its newest surviving sibling. Extra activation aliases expire
//! oldest first, except for the current live revision. Creation revisions do not
//! consume the extra-alias budget. Reads, failures and no-op navigation never
//! prune. The current snapshot is always retained, even if it alone exceeds the
//! byte budget; a zero entry or alias limit still allows the current entry and
//! its live alias. [`ModelHistory::bytes`] exposes that oversized snapshot.
//!
//! Immutable texture and binary-extension allocations are charged once across
//! all retained branches, using allocation identity, not content hashes. Other
//! model data is conservatively charged per snapshot even where core shares it.
//! The byte budget is an estimate of snapshot storage; the entry/alias budgets
//! bound the tree and lookup metadata separately.

use std::{collections::BTreeMap, mem::size_of, sync::Arc};

use catchlight_core::{ExtensionValue, Model};

/// Default maximum retained entries, including the current entry.
pub const HISTORY_ENTRIES: usize = 64;
/// Default retained snapshot budget, with immutable payloads counted once.
pub const HISTORY_BYTES: usize = 256 * 1024 * 1024;
/// Default maximum extra activation revisions, in addition to entry identities.
pub const HISTORY_ALIASES: usize = 512;

/// Storage limits applied after each changed publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    pub entries: usize,
    pub bytes: usize,
    /// Extra navigation aliases; creation revisions are retained with entries.
    pub aliases: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            entries: HISTORY_ENTRIES,
            bytes: HISTORY_BYTES,
            aliases: HISTORY_ALIASES,
        }
    }
}

/// Select a retained authored state. `Goto` accepts either kind of revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryNavigation {
    Undo,
    Redo,
    Goto(u64),
}

/// History validation failures; none of these alter retained state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HistoryError {
    #[error("nothing to undo")]
    NothingToUndo,
    #[error("nothing to redo")]
    NothingToRedo,
    #[error("revision {0} is not retained")]
    RevisionUnavailable(u64),
    #[error("publication revision {supplied} must exceed live revision {current}")]
    InvalidPublicationRevision { supplied: u64, current: u64 },
}

/// One retained entry, without its model payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub revision: u64,
    pub parent: Option<u64>,
    pub redo: Option<u64>,
    /// Retained publication revisions, in increasing order, including creation.
    pub revisions: Vec<u64>,
}

/// A coherent read of the tree and cursor. The session supplies the outer rev.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMetadata {
    pub root: u64,
    pub current: u64,
    pub pruned: bool,
    /// Increasing creation-revision order.
    pub entries: Vec<HistoryEntry>,
}

struct Entry {
    model: Model,
    parent: Option<u64>,
    redo: Option<u64>,
    own_bytes: usize,
    /// One allocation identity and size per distinct immutable payload.
    payloads: BTreeMap<usize, usize>,
}

impl Entry {
    fn new(model: Model, parent: Option<u64>) -> Self {
        let mut own_bytes = model.estimated_size_bytes();
        let mut payloads = BTreeMap::new();
        for id in model.texture_ids() {
            if let Some(texture) = model.texture(id) {
                // Core charges each texture reference, so subtract each one,
                // even if several references hold the same Arc allocation.
                own_bytes = own_bytes.saturating_sub(texture.data.len());
                hold_payload(&mut payloads, &texture.data);
            }
        }
        // Core's estimate does not currently include extension entries. JSON
        // is cloned rather than shared; binary payloads join the same ledger
        // as textures (including when both refer to the same allocation).
        for (key, value) in model.extensions() {
            own_bytes = own_bytes
                .saturating_add(size_of_val(key))
                .saturating_add(key.as_str().len())
                .saturating_add(size_of::<ExtensionValue>());
            match value {
                ExtensionValue::Bytes(data) => hold_payload(&mut payloads, data),
                ExtensionValue::Json(value) => {
                    own_bytes = own_bytes.saturating_add(json_heap_bytes(value));
                }
            }
        }
        Self {
            model,
            parent,
            redo: None,
            own_bytes,
            payloads,
        }
    }
}

fn hold_payload(payloads: &mut BTreeMap<usize, usize>, data: &Arc<[u8]>) {
    // The Arc allocation remains alive in the snapshot. Unlike Vec::as_ptr,
    // even an empty Arc allocation has an identity unique to its lifetime.
    let at = Arc::as_ptr(data) as *const u8 as usize;
    payloads.insert(at, data.len());
}

fn json_heap_bytes(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    match value {
        Value::String(value) => value.capacity(),
        Value::Array(values) => values.iter().fold(
            values.capacity().saturating_mul(size_of::<Value>()),
            |bytes, value| bytes.saturating_add(json_heap_bytes(value)),
        ),
        Value::Object(values) => values.iter().fold(0usize, |bytes, (key, value)| {
            bytes
                .saturating_add(size_of::<(String, Value)>())
                .saturating_add(key.capacity())
                .saturating_add(json_heap_bytes(value))
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

/// Pure history over [`Model`] snapshots. Construct a fresh root for every
/// newly created, opened or forked session; do not copy history across sessions.
pub struct ModelHistory {
    entries: BTreeMap<u64, Entry>,
    /// Navigation publication revision -> creation revision.
    aliases: BTreeMap<u64, u64>,
    root: u64,
    current: u64,
    live_revision: u64,
    pruned: bool,
    limits: HistoryLimits,
    /// Allocation identity -> (payload bytes, number of holding entries).
    payloads: BTreeMap<usize, (usize, usize)>,
    own_bytes: usize,
}

impl ModelHistory {
    /// Start with the loaded model as revision zero and no Undo steps.
    pub fn new(model: Model, limits: HistoryLimits) -> Self {
        let entry = Entry::new(model, None);
        let payloads = entry
            .payloads
            .iter()
            .map(|(&at, &bytes)| (at, (bytes, 1)))
            .collect();
        Self {
            own_bytes: entry.own_bytes,
            entries: BTreeMap::from([(0, entry)]),
            aliases: BTreeMap::new(),
            root: 0,
            current: 0,
            live_revision: 0,
            pruned: false,
            limits,
            payloads,
        }
    }

    /// Current publication counter, distinct from the current entry identity.
    pub fn live_revision(&self) -> u64 {
        self.live_revision
    }

    /// Snapshot storage estimate, including all branches and shared assets once.
    pub fn bytes(&self) -> usize {
        self.payloads
            .values()
            .fold(self.own_bytes, |bytes, &(size, _)| {
                bytes.saturating_add(size)
            })
    }

    /// Resolve a retained creation revision or navigation alias without mutation.
    pub fn model_at(&self, revision: u64) -> Result<&Model, HistoryError> {
        let identity = self.resolve(revision)?;
        Ok(&self.entries[&identity].model)
    }

    /// Retain one changed edit or complete batch. The caller determines whether
    /// authored content changed; do not call this for failed or no-op edits.
    /// Revisions may skip numbers but must always exceed the last publication.
    pub fn record(&mut self, model: Model, revision: u64) -> Result<(), HistoryError> {
        self.check_revision(revision)?;
        let entry = Entry::new(model, Some(self.current));
        self.own_bytes = self.own_bytes.saturating_add(entry.own_bytes);
        for (&at, &bytes) in &entry.payloads {
            self.payloads.entry(at).or_insert((bytes, 0)).1 += 1;
        }
        if let Some(parent) = self.entries.get_mut(&self.current) {
            parent.redo = Some(revision);
        }
        self.entries.insert(revision, entry);
        self.current = revision;
        self.live_revision = revision;
        self.trim();
        Ok(())
    }

    /// Restore a retained entry and record its new publication alias. The
    /// returned shallow clone is for installation under the same session lock.
    /// All fallible validation happens before any history mutation. `None`
    /// means Goto already selected this entry: no publication or pruning occurs.
    /// Even on a no-op, the supplied candidate revision must be greater than
    /// the live revision; a successful no-op does not adopt that candidate.
    pub fn navigate(
        &mut self,
        action: HistoryNavigation,
        revision: u64,
    ) -> Result<Option<Model>, HistoryError> {
        self.check_revision(revision)?;
        let target = match action {
            HistoryNavigation::Undo => self.entries[&self.current]
                .parent
                .ok_or(HistoryError::NothingToUndo)?,
            HistoryNavigation::Redo => self.entries[&self.current]
                .redo
                .ok_or(HistoryError::NothingToRedo)?,
            HistoryNavigation::Goto(target) => self.resolve(target)?,
        };
        if target == self.current {
            return Ok(None);
        }
        let model = self.entries[&target].model.clone();
        if action == HistoryNavigation::Undo {
            if let Some(parent) = self.entries.get_mut(&target) {
                parent.redo = Some(self.current);
            }
        }
        let mut child = target;
        while let Some(parent) = self.entries[&child].parent {
            if let Some(entry) = self.entries.get_mut(&parent) {
                entry.redo = Some(child);
            }
            child = parent;
        }
        self.current = target;
        self.live_revision = revision;
        self.aliases.insert(revision, target);
        self.trim();
        Ok(Some(model))
    }

    /// Number of retained ancestors and entries along the preferred redo path.
    pub fn depths(&self) -> (usize, usize) {
        let mut undo = 0;
        let mut at = self.current;
        while let Some(parent) = self.entries[&at].parent {
            undo += 1;
            at = parent;
        }
        let mut redo = 0;
        let mut at = self.current;
        while let Some(child) = self.entries[&at].redo {
            redo += 1;
            at = child;
        }
        (undo, redo)
    }

    pub fn metadata(&self) -> HistoryMetadata {
        HistoryMetadata {
            root: self.root,
            current: self.current,
            pruned: self.pruned,
            entries: self
                .entries
                .iter()
                .map(|(&revision, entry)| {
                    let mut revisions = vec![revision];
                    revisions.extend(
                        self.aliases
                            .iter()
                            .filter_map(|(&alias, &target)| (target == revision).then_some(alias)),
                    );
                    HistoryEntry {
                        revision,
                        parent: entry.parent,
                        redo: entry.redo,
                        revisions,
                    }
                })
                .collect(),
        }
    }

    fn check_revision(&self, supplied: u64) -> Result<(), HistoryError> {
        if supplied <= self.live_revision {
            return Err(HistoryError::InvalidPublicationRevision {
                supplied,
                current: self.live_revision,
            });
        }
        Ok(())
    }

    fn resolve(&self, revision: u64) -> Result<u64, HistoryError> {
        if self.entries.contains_key(&revision) {
            return Ok(revision);
        }
        self.aliases
            .get(&revision)
            .copied()
            .ok_or(HistoryError::RevisionUnavailable(revision))
    }

    fn trim(&mut self) {
        while self.entries.len() > 1
            && (self.entries.len() > self.limits.entries || self.bytes() > self.limits.bytes)
        {
            let leaf = self.entries.keys().copied().find(|&candidate| {
                candidate != self.current
                    && !self
                        .entries
                        .values()
                        .any(|entry| entry.parent == Some(candidate))
            });
            if let Some(leaf) = leaf {
                self.remove(leaf);
            } else {
                // Every noncurrent branch has gone. The remaining tree is a
                // chain ending at current, so the root has exactly one child.
                let child = self
                    .entries
                    .iter()
                    .find_map(|(&id, entry)| (entry.parent == Some(self.root)).then_some(id));
                if let Some(child) = child {
                    let old_root = self.root;
                    self.root = child;
                    if let Some(entry) = self.entries.get_mut(&child) {
                        entry.parent = None;
                    }
                    self.remove(old_root);
                }
            }
        }
        while self.aliases.len() > self.limits.aliases {
            let oldest = self
                .aliases
                .keys()
                .copied()
                .find(|&alias| alias != self.live_revision);
            let Some(oldest) = oldest else {
                break;
            };
            self.aliases.remove(&oldest);
            self.pruned = true;
        }
    }

    fn remove(&mut self, revision: u64) {
        let Some(removed) = self.entries.remove(&revision) else {
            return;
        };
        self.pruned = true;
        self.own_bytes = self.own_bytes.saturating_sub(removed.own_bytes);
        for at in removed.payloads.keys() {
            if let std::collections::btree_map::Entry::Occupied(mut held) = self.payloads.entry(*at)
            {
                held.get_mut().1 -= 1;
                if held.get().1 == 0 {
                    held.remove();
                }
            }
        }
        self.aliases.retain(|_, target| *target != revision);
        if let Some(parent) = removed.parent {
            let replacement = self
                .entries
                .iter()
                .rev()
                .find_map(|(&id, entry)| (entry.parent == Some(parent)).then_some(id));
            if let Some(entry) = self.entries.get_mut(&parent) {
                if entry.redo == Some(revision) {
                    entry.redo = replacement;
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use catchlight_core::{
        formats::clm::{ClmMesh, TextureAlpha, TextureEncoding},
        ExtensionKey, ModelNode, ModelNodeKind, ModelPart, ModelTexture, Name, SeededHex,
    };

    use super::*;

    fn named_model(name: &str) -> Model {
        let mut model = Model::new();
        let root = model.root().unwrap().clone();
        model
            .update_node(&root, |node| node.name = Name::truncated(name))
            .unwrap();
        model
    }

    fn root_name(model: &Model) -> &str {
        model.node(model.root().unwrap()).unwrap().name.as_str()
    }

    fn history() -> ModelHistory {
        ModelHistory::new(named_model("initial"), HistoryLimits::default())
    }

    fn limits(entries: usize, aliases: usize) -> HistoryLimits {
        HistoryLimits {
            entries,
            bytes: usize::MAX,
            aliases,
        }
    }

    fn entry(
        revision: u64,
        parent: Option<u64>,
        redo: Option<u64>,
        revisions: &[u64],
    ) -> HistoryEntry {
        HistoryEntry {
            revision,
            parent,
            redo,
            revisions: revisions.to_vec(),
        }
    }

    #[test]
    fn initial_model_is_the_clean_revision_zero_root() {
        let history = history();
        assert_eq!(history.live_revision(), 0);
        assert_eq!(history.depths(), (0, 0));
        assert_eq!(root_name(history.model_at(0).unwrap()), "initial");
        assert_eq!(
            history.metadata(),
            HistoryMetadata {
                root: 0,
                current: 0,
                pruned: false,
                entries: vec![entry(0, None, None, &[0])],
            }
        );
    }

    #[test]
    fn protocol_example_keeps_both_branches_and_activation_revisions() {
        let mut history = history();
        history.record(named_model("A"), 1).unwrap();
        history.record(named_model("B"), 2).unwrap();
        let model = history
            .navigate(HistoryNavigation::Undo, 3)
            .unwrap()
            .unwrap();
        assert_eq!(root_name(&model), "A");
        history.record(named_model("C"), 4).unwrap();
        let model = history
            .navigate(HistoryNavigation::Goto(2), 5)
            .unwrap()
            .unwrap();
        assert_eq!(root_name(&model), "B");
        assert_eq!(history.live_revision(), 5);
        assert_eq!(history.depths(), (2, 0));
        assert_eq!(
            history.metadata(),
            HistoryMetadata {
                root: 0,
                current: 2,
                pruned: false,
                entries: vec![
                    entry(0, None, Some(1), &[0]),
                    entry(1, Some(0), Some(2), &[1, 3]),
                    entry(2, Some(1), None, &[2, 5]),
                    entry(4, Some(1), None, &[4]),
                ],
            }
        );
        let model = history
            .navigate(HistoryNavigation::Goto(3), 6)
            .unwrap()
            .unwrap();
        assert_eq!(root_name(&model), "A");
        assert_eq!(history.metadata().current, 1);
        assert_eq!(history.depths(), (1, 1));
        let model = history
            .navigate(HistoryNavigation::Redo, 7)
            .unwrap()
            .unwrap();
        assert_eq!(root_name(&model), "B");
        assert_eq!(history.metadata().entries.len(), 4);
    }

    #[test]
    fn goto_selects_the_entire_preferred_path_not_just_the_last_edge() {
        let mut history = history();
        history.record(named_model("A"), 1).unwrap();
        history.record(named_model("B"), 2).unwrap();
        history.record(named_model("C"), 3).unwrap();
        history.navigate(HistoryNavigation::Goto(0), 4).unwrap();
        history.record(named_model("D"), 5).unwrap();
        history.record(named_model("E"), 6).unwrap();
        history.navigate(HistoryNavigation::Goto(3), 7).unwrap();
        history.navigate(HistoryNavigation::Goto(0), 8).unwrap();
        assert_eq!(history.depths(), (0, 3));
        for (revision, expected) in [(9, "A"), (10, "B"), (11, "C")] {
            let model = history
                .navigate(HistoryNavigation::Redo, revision)
                .unwrap()
                .unwrap();
            assert_eq!(root_name(&model), expected);
        }
        assert_eq!(history.metadata().entries.len(), 6);
    }

    #[test]
    fn validation_errors_leave_every_history_observable_unchanged() {
        let mut history = history();
        let before = history.metadata();
        let bytes = history.bytes();
        for (action, revision, expected) in [
            (HistoryNavigation::Undo, 1, HistoryError::NothingToUndo),
            (HistoryNavigation::Redo, 1, HistoryError::NothingToRedo),
            (
                HistoryNavigation::Goto(98),
                1,
                HistoryError::RevisionUnavailable(98),
            ),
            (
                HistoryNavigation::Goto(0),
                0,
                HistoryError::InvalidPublicationRevision {
                    supplied: 0,
                    current: 0,
                },
            ),
        ] {
            assert_eq!(history.navigate(action, revision).unwrap_err(), expected);
            assert_eq!(history.metadata(), before);
            assert_eq!(history.live_revision(), 0);
            assert_eq!(history.bytes(), bytes);
        }
        assert!(history.record(named_model("invalid"), 0).is_err());
        assert_eq!(history.metadata(), before);
        assert_eq!(root_name(history.model_at(0).unwrap()), "initial");
    }

    #[test]
    fn goto_current_through_either_revision_is_a_complete_noop() {
        let mut history = history();
        history.record(named_model("A"), 1).unwrap();
        history.navigate(HistoryNavigation::Undo, 2).unwrap();
        let before = history.metadata();
        let bytes = history.bytes();
        for target in [0, 2] {
            assert!(history
                .navigate(HistoryNavigation::Goto(target), 100)
                .unwrap()
                .is_none());
            assert_eq!(history.metadata(), before);
            assert_eq!(history.live_revision(), 2);
            assert_eq!(history.bytes(), bytes);
        }
        history.navigate(HistoryNavigation::Redo, 3).unwrap();
        assert_eq!(history.live_revision(), 3);
    }

    #[test]
    fn navigation_uses_entry_identity_even_when_authored_contents_match() {
        let model = named_model("same");
        let mut history = ModelHistory::new(model.clone(), HistoryLimits::default());
        // Equality detection belongs to the caller, which may reach identical
        // contents through a series of meaningful edits or separate branches.
        history.record(model, 4).unwrap();
        assert!(history
            .navigate(HistoryNavigation::Undo, 7)
            .unwrap()
            .is_some());
        assert_eq!(history.live_revision(), 7);
        assert_eq!(history.metadata().current, 0);
        assert_eq!(history.metadata().entries.len(), 2);
        assert!(history.record(named_model("stale"), 6).is_err());
        assert_eq!(history.live_revision(), 7);
    }

    #[test]
    fn retention_discards_oldest_noncurrent_leaf_before_advancing_root() {
        let mut history = ModelHistory::new(named_model("root"), limits(4, 512));
        history.record(named_model("A"), 1).unwrap();
        history.record(named_model("B"), 2).unwrap();
        history.navigate(HistoryNavigation::Undo, 3).unwrap();
        history.record(named_model("C"), 4).unwrap();
        assert!(!history.metadata().pruned);
        history.record(named_model("D"), 5).unwrap();
        assert_eq!(
            history.model_at(2).unwrap_err().to_string(),
            "revision 2 is not retained"
        );
        assert_eq!(history.metadata().root, 0);
        assert!(history.metadata().pruned);
        assert_eq!(history.depths(), (3, 0));
        history.record(named_model("E"), 6).unwrap();
        assert!(history.model_at(0).is_err());
        assert_eq!(history.metadata().root, 1);
        assert_eq!(history.metadata().entries[0].parent, None);
        assert_eq!(history.metadata().entries[0].revisions, [1, 3]);
        assert_eq!(history.depths(), (3, 0));
    }

    #[test]
    fn entry_identity_survives_pruning_while_aliases_expire_oldest_first() {
        let mut history = ModelHistory::new(named_model("root"), limits(8, 2));
        history.record(named_model("A"), 1).unwrap();
        history.navigate(HistoryNavigation::Undo, 2).unwrap();
        history.navigate(HistoryNavigation::Redo, 3).unwrap();
        assert!(!history.metadata().pruned);
        history.navigate(HistoryNavigation::Undo, 4).unwrap();
        assert!(history.metadata().pruned);
        assert!(history.model_at(2).is_err());
        for revision in [0, 1, 3, 4] {
            assert!(history.model_at(revision).is_ok());
        }
        assert_eq!(history.metadata().entries[0].revisions, [0, 4]);
        assert_eq!(history.metadata().entries[1].revisions, [1, 3]);
    }

    #[test]
    fn zero_budgets_still_retain_current_snapshot_and_live_alias() {
        let model = named_model("A");
        let mut history = ModelHistory::new(
            model.clone(),
            HistoryLimits {
                entries: 0,
                bytes: 0,
                aliases: 0,
            },
        );
        assert!(history.bytes() > 0);
        assert!(!history.metadata().pruned);
        history.record(model, 1).unwrap();
        assert_eq!(history.metadata().entries.len(), 1);
        assert_eq!(history.metadata().root, 1);
        assert_eq!(history.metadata().current, 1);
        assert!(history.bytes() > 0);
        assert!(history.model_at(1).is_ok());

        let mut history = ModelHistory::new(named_model("root"), limits(2, 0));
        history.record(named_model("A"), 1).unwrap();
        history.navigate(HistoryNavigation::Undo, 2).unwrap();
        assert!(history.model_at(2).is_ok());
        history.navigate(HistoryNavigation::Redo, 3).unwrap();
        assert!(history.model_at(2).is_err());
        assert!(history.model_at(3).is_ok());
        assert!(history.model_at(1).is_ok());
        assert!(history.model_at(0).is_ok());
    }

    fn with_payload(payload: Arc<[u8]>) -> (Model, catchlight_core::NodeId) {
        let mut model = Model::new();
        let mut hex = SeededHex::new(7);
        let root = model.root().unwrap().clone();
        let part = model
            .add_node(
                &root,
                ModelNode::new(
                    "part",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
                ),
                &mut hex,
            )
            .unwrap();
        model
            .add_texture(
                &part,
                ModelTexture {
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: payload,
                },
                &mut hex,
            )
            .unwrap();
        (model, part)
    }

    #[test]
    fn shared_texture_and_extension_payloads_are_charged_once_across_branches() {
        let payload: Arc<[u8]> = vec![0; 1024 * 1024].into();
        let (mut model, _) = with_payload(payload.clone());
        model
            .set_extension(
                ExtensionKey::new("test.asset").unwrap(),
                ExtensionValue::Bytes(payload.clone()),
            )
            .unwrap();
        let own_bytes = Entry::new(model.clone(), None).own_bytes;
        let mut history = ModelHistory::new(model.clone(), HistoryLimits::default());
        history.record(model.clone(), 1).unwrap();
        history.navigate(HistoryNavigation::Undo, 2).unwrap();
        history.record(model.clone(), 3).unwrap();
        assert_eq!(history.bytes(), payload.len() + 3 * own_bytes);
        assert_eq!(history.payloads.len(), 1);
        assert_eq!(history.payloads.values().next().unwrap().1, 3);
        let bytes = history.bytes();
        history.navigate(HistoryNavigation::Goto(1), 4).unwrap();
        assert_eq!(history.bytes(), bytes);
    }

    #[test]
    fn equal_bytes_in_distinct_allocations_are_billed_separately() {
        let (first, _) = with_payload(vec![0; 1024].into());
        let (second, _) = with_payload(vec![0; 1024].into());
        let expected = first.estimated_size_bytes() + second.estimated_size_bytes();
        let mut history = ModelHistory::new(first, HistoryLimits::default());
        history.record(second, 1).unwrap();
        assert_eq!(history.bytes(), expected);
        assert_eq!(history.payloads.len(), 2);
    }

    #[test]
    fn last_pruned_asset_owner_releases_its_charge() {
        let payload: Arc<[u8]> = vec![0; 1024 * 1024].into();
        let (mut model, part) = with_payload(payload.clone());
        let mut history = ModelHistory::new(model.clone(), limits(2, 2));
        history.record(model.clone(), 1).unwrap();
        model.delete_node(&part).unwrap();
        history.record(model.clone(), 2).unwrap();
        assert!(history.bytes() > payload.len());
        history.record(model, 3).unwrap();
        assert!(history.bytes() < payload.len());
        assert!(history.payloads.is_empty());
    }

    #[test]
    fn snapshot_byte_limit_accepts_exact_boundary_then_prunes_old_root() {
        let model = named_model("fixed");
        let bytes = Entry::new(model.clone(), None).own_bytes;
        let mut history = ModelHistory::new(
            model.clone(),
            HistoryLimits {
                entries: 8,
                bytes: 2 * bytes,
                aliases: 8,
            },
        );
        history.record(model.clone(), 1).unwrap();
        assert_eq!(history.bytes(), 2 * bytes);
        assert!(!history.metadata().pruned);
        history.record(model, 2).unwrap();
        assert_eq!(history.bytes(), 2 * bytes);
        assert_eq!(history.metadata().root, 1);
        assert!(history.metadata().pruned);
    }

    #[test]
    fn extension_json_heap_is_charged_per_snapshot() {
        let mut model = named_model("root");
        let empty_bytes = Entry::new(model.clone(), None).own_bytes;
        model
            .set_extension(
                ExtensionKey::new("test.json").unwrap(),
                ExtensionValue::Json(serde_json::json!({"nested": ["x".repeat(4096)]})),
            )
            .unwrap();
        let own_bytes = Entry::new(model.clone(), None).own_bytes;
        assert!(own_bytes >= empty_bytes + 4096);
        let mut history = ModelHistory::new(model.clone(), HistoryLimits::default());
        history.record(model, 1).unwrap();
        assert_eq!(history.bytes(), 2 * own_bytes);
    }

    #[test]
    fn repeated_branching_and_pruning_keep_all_references_and_accounting_valid() {
        let mut history = ModelHistory::new(named_model("root"), limits(4, 3));
        let mut random = 0x1234_5678u64;
        for revision in 1..1000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let before = history.metadata();
            match random % 5 {
                0 | 1 => history.record(named_model("edit"), revision).unwrap(),
                2 => {
                    let _ = history.navigate(HistoryNavigation::Undo, revision);
                }
                3 => {
                    let _ = history.navigate(HistoryNavigation::Redo, revision);
                }
                _ => {
                    let target = before.entries[(random as usize) % before.entries.len()].revision;
                    history
                        .navigate(HistoryNavigation::Goto(target), revision)
                        .unwrap();
                }
            }
            let metadata = history.metadata();
            assert!(metadata.entries.len() <= 4);
            assert!(history.aliases.len() <= 3);
            assert!(metadata
                .entries
                .iter()
                .any(|entry| entry.revision == metadata.current));
            assert!(history.model_at(history.live_revision()).is_ok());
            for entry in &metadata.entries {
                assert!(entry.revisions.contains(&entry.revision));
                assert!(entry.revisions.windows(2).all(|pair| pair[0] < pair[1]));
                if let Some(parent) = entry.parent {
                    assert!(parent < entry.revision);
                    assert!(history.entries.contains_key(&parent));
                } else {
                    assert_eq!(entry.revision, metadata.root);
                }
                if let Some(redo) = entry.redo {
                    assert_eq!(history.entries[&redo].parent, Some(entry.revision));
                }
                for alias in &entry.revisions {
                    assert_eq!(history.resolve(*alias).unwrap(), entry.revision);
                }
            }
            assert_eq!(
                history.bytes(),
                history
                    .entries
                    .values()
                    .map(|entry| entry.own_bytes)
                    .sum::<usize>()
            );
        }
    }
}
