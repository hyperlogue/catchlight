//! Typed extensions: data on a whole model under a vendor key, read through a
//! Rust type that implements [`Extension`].
//!
//! A maintained extension keeps every Id it names in a reference table beside
//! its payload. The core rewrites and marks that table on every Id change,
//! whether or not any program knows the extension, and never looks inside the
//! payload. A plain JSON or byte extension has no table: it is carried as
//! stored and never maintained.

mod registry;
mod store;

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::id::{NodeId, ParamId, SlotId, TexId};
use crate::model::Model;

pub(crate) use registry::decode_registered;
pub use registry::{Official, RegisterError, register, register_official, sealed};
pub use store::decode_as;
pub use store::{Extensions, Iter};
pub(crate) use store::{Stored, Typed};

/// Implemented on the data type itself. Typed access needs only the type in
/// hand; [`register`] is for programs that install addons or check models.
/// `Default` is the empty data: what an addon's data merges into when the
/// base has none.
pub trait Extension: Clone + Default + fmt::Debug + Send + Sync + 'static {
    /// Vendor first, as in `acme.framings`. `catchlight.` is reserved for the
    /// official extensions.
    const KEY: &'static str;
    /// The newest version this build reads and writes.
    const VERSION: u32;

    /// `raw` is already migrated to [`Self::VERSION`].
    fn decode(raw: RawExtension) -> Result<Self, DecodeError>;
    fn encode(&self) -> RawExtension;
    fn refs(&self) -> &[IdRef];
    fn refs_mut(&mut self) -> &mut [IdRef];

    /// Lifts `raw` from version `from` to `from + 1`; the core stamps the
    /// result with that version.
    fn migrate(from: u32, raw: RawExtension) -> Result<RawExtension, MigrationError> {
        let _ = raw;
        Err(MigrationError::Unknown(from))
    }

    /// What no longer fits the model. Never a reason to refuse anything.
    fn check(&self, _model: &Model) -> Vec<Warning> {
        Vec::new()
    }

    /// Applies this extension's policy to [`IdRef::Removed`] entries. Must be
    /// idempotent. `model` is the edited model, but while this runs it does
    /// not hold this extension's current data: read the other extensions only.
    fn reconcile(&mut self, _model: &Model) {}

    /// Install: folds an addon's data into this one, or refuses the install.
    fn merge_addon(&self, _addon: &Self) -> Result<(Self, Receipt), MergeRefused> {
        Err(MergeRefused::NotCarried)
    }

    /// Uninstall: removes exactly what `receipt` says was merged.
    fn unmerge_addon(&mut self, _receipt: &Receipt) {}
}

/// A maintained extension as stored: the only place its Ids appear is `refs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawExtension {
    pub version: u32,
    pub refs: Vec<IdRef>,
    pub payload: Payload,
}

/// JSON rides inline in every structure feed; bytes live in the file's
/// `Extensions` section and reach a browser tab by hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    Json(serde_json::Value),
    Bytes(Arc<[u8]>),
}

impl Payload {
    pub fn json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(value) => Some(value),
            Self::Bytes(_) => None,
        }
    }

    pub fn json_mut(&mut self) -> Result<&mut serde_json::Value, MigrationError> {
        match self {
            Self::Json(value) => Ok(value),
            Self::Bytes(_) => Err(MigrationError::Failed(
                "the payload is bytes, not JSON".into(),
            )),
        }
    }

    pub fn bytes(&self) -> Option<&Arc<[u8]>> {
        match self {
            Self::Bytes(data) => Some(data),
            Self::Json(_) => None,
        }
    }
}

/// One entry of a reference table, written in a file as `{"param": "eye_l"}`,
/// `{"slot": ["head", "slot-1a2b3c4d"]}` or `{"removed": {"param": "eye_l"}}`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdRef {
    Node(NodeId),
    Param(ParamId),
    Texture(TexId),
    Slot(NodeId, SlotId),
    /// The Id this entry named is gone from the model.
    Removed(RemovedRef),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovedRef {
    Node(NodeId),
    Param(ParamId),
    Texture(TexId),
    Slot(NodeId, SlotId),
}

impl IdRef {
    /// What this entry becomes once its Id is removed.
    pub fn removed(&self) -> Self {
        Self::Removed(match self {
            Self::Node(id) => RemovedRef::Node(id.clone()),
            Self::Param(id) => RemovedRef::Param(id.clone()),
            Self::Texture(id) => RemovedRef::Texture(id.clone()),
            Self::Slot(node, slot) => RemovedRef::Slot(node.clone(), slot.clone()),
            Self::Removed(gone) => return Self::Removed(gone.clone()),
        })
    }

    pub fn is_removed(&self) -> bool {
        matches!(self, Self::Removed(_))
    }

    /// Whether `model` holds what this entry names. A removed entry never
    /// resolves.
    pub fn resolves_in(&self, model: &Model) -> bool {
        match self {
            Self::Node(id) => model.node(id).is_some(),
            Self::Param(id) => model.param(id).is_some(),
            Self::Texture(id) => model.texture(id).is_some(),
            Self::Slot(node, slot) => model.slot(node, slot).is_some(),
            Self::Removed(_) => false,
        }
    }

    pub(crate) fn encoded_len_hint(&self) -> usize {
        match self {
            Self::Node(id) => id.as_str().len(),
            Self::Param(id) => id.as_str().len(),
            Self::Texture(id) => id.as_str().len(),
            Self::Slot(node, slot) => node.as_str().len() + slot.as_str().len(),
            Self::Removed(gone) => Self::from(gone.clone()).encoded_len_hint(),
        }
    }
}

impl From<RemovedRef> for IdRef {
    fn from(gone: RemovedRef) -> Self {
        match gone {
            RemovedRef::Node(id) => Self::Node(id),
            RemovedRef::Param(id) => Self::Param(id),
            RemovedRef::Texture(id) => Self::Texture(id),
            RemovedRef::Slot(node, slot) => Self::Slot(node, slot),
        }
    }
}

impl fmt::Display for IdRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(id) => write!(f, "node {:?}", id.as_str()),
            Self::Param(id) => write!(f, "param {:?}", id.as_str()),
            Self::Texture(id) => write!(f, "texture {:?}", id.as_str()),
            Self::Slot(node, slot) => {
                write!(
                    f,
                    "vertex slot {:?} on part {:?}",
                    slot.as_str(),
                    node.as_str()
                )
            }
            Self::Removed(gone) => write!(f, "removed {}", Self::from(gone.clone())),
        }
    }
}

/// The reference table a maintained extension keeps beside its payload; the
/// payload names an Id by its `u32` index here. Reads as a slice, which the
/// core rewrites in place, so only [`Self::intern`] and [`Self::compact`]
/// change its length.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RefTable(Vec<IdRef>);

impl RefTable {
    pub fn get(&self, index: u32) -> Option<&IdRef> {
        self.0.get(index as usize)
    }

    /// The param `index` names, or `None` once that param is removed.
    pub fn param(&self, index: u32) -> Option<&ParamId> {
        match self.get(index)? {
            IdRef::Param(param) => Some(param),
            _ => None,
        }
    }

    /// Whether `index` names a param, live or removed: what a decoder holds a
    /// param index to.
    pub fn names_param(&self, index: u32) -> bool {
        matches!(
            self.get(index),
            Some(IdRef::Param(_) | IdRef::Removed(RemovedRef::Param(_)))
        )
    }

    /// `r`'s index, appending it if the table does not hold it yet.
    pub fn intern(&mut self, r: IdRef) -> u32 {
        let at = self.0.iter().position(|x| *x == r).unwrap_or_else(|| {
            self.0.push(r);
            self.0.len() - 1
        });
        at as u32
    }

    /// Keeps exactly the entries `indices` use, numbered in the order they are
    /// first used, and rewrites each index to match. Every index must name an
    /// entry of this table.
    pub fn compact<'a>(&mut self, indices: impl IntoIterator<Item = &'a mut u32>) {
        let old = std::mem::take(&mut self.0);
        let mut at: HashMap<&IdRef, u32> = HashMap::new();
        for index in indices {
            let r = &old[*index as usize];
            *index = *at.entry(r).or_insert_with(|| {
                self.0.push(r.clone());
                (self.0.len() - 1) as u32
            });
        }
    }
}

impl From<Vec<IdRef>> for RefTable {
    fn from(refs: Vec<IdRef>) -> Self {
        Self(refs)
    }
}

impl Deref for RefTable {
    type Target = [IdRef];

    fn deref(&self) -> &[IdRef] {
        &self.0
    }
}

impl DerefMut for RefTable {
    fn deref_mut(&mut self) -> &mut [IdRef] {
        &mut self.0
    }
}

/// A finding an extension surfaces: stale data, a suspicious value, a
/// deprecation. Shown, never refused on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Namespaced by its owner, as in `acme.framings/empty-shot`.
    pub code: Cow<'static, str>,
    pub message: String,
    /// What an editor highlights; may be empty.
    pub subjects: Vec<IdRef>,
}

impl Warning {
    pub fn new(code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            subjects: Vec::new(),
        }
    }

    pub fn with_subjects(mut self, subjects: Vec<IdRef>) -> Self {
        self.subjects = subjects;
        self
    }
}

/// The codes of the warnings the core raises itself.
pub mod codes {
    pub const DANGLING: &str = "catchlight/dangling-reference";
    pub const REMOVED: &str = "catchlight/removed-reference";
    pub const NEWER: &str = "catchlight/newer-version";
    pub const UNREAD: &str = "catchlight/unread";
    pub const NOT_CHECKED: &str = "catchlight/not-checked";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DecodeError(pub String);

impl DecodeError {
    pub fn new(message: impl fmt::Display) -> Self {
        Self(message.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrationError {
    #[error("this build has no migration from version {0}")]
    Unknown(u32),
    #[error("{0}")]
    Failed(String),
}

/// Why data present in a model cannot be read as its type. The data is kept
/// and written back as stored.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unread {
    #[error("written at version {version} by a newer build; this build reads up to {readable}")]
    Newer { version: u32, readable: u32 },
    #[error("migration failed: {0}")]
    Migration(MigrationError),
    #[error("decode failed: {0}")]
    Decode(DecodeError),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtensionError {
    #[error("the model carries no such extension")]
    Missing,
    #[error(transparent)]
    Unread(Unread),
    #[error("the extension under this key is another type")]
    WrongType,
    #[error("the value under this key is a plain JSON or byte value, not a maintained extension")]
    NotMaintained,
    #[error("{0:?} is not a valid extension key")]
    InvalidKey(&'static str),
}

/// Why an extension refuses to fold an addon's data into a model's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeRefused {
    #[error("this extension does not take data from addons")]
    NotCarried,
    #[error("the addon's extension under this key is another type than the model's")]
    WrongType,
    #[error("{0}")]
    Conflict(String),
}

/// What an install merged, as the extension records it, handed back to
/// [`Extension::unmerge_addon`] on uninstall.
#[derive(Clone)]
pub struct Receipt(Arc<dyn Any + Send + Sync>);

impl Receipt {
    pub fn new<R: Any + Send + Sync>(record: R) -> Self {
        Self(Arc::new(record))
    }

    pub fn get<R: Any>(&self) -> Option<&R> {
        self.0.downcast_ref()
    }
}

impl fmt::Debug for Receipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Receipt(..)")
    }
}

/// The object-safe face of [`Extension`], which has associated consts and so
/// cannot be a trait object. Prefixed so it never shadows `Extension`'s
/// methods on a concrete type.
pub(crate) trait DynExtension: Any + fmt::Debug + Send + Sync {
    fn dyn_encode(&self) -> RawExtension;
    fn dyn_refs(&self) -> &[IdRef];
    fn dyn_refs_mut(&mut self) -> &mut [IdRef];
    fn dyn_check(&self, model: &Model) -> Vec<Warning>;
    fn dyn_reconcile(&mut self, model: &Model);
    fn dyn_merge_addon(
        &self,
        addon: &dyn DynExtension,
    ) -> Result<(Box<dyn DynExtension>, Receipt), MergeRefused>;
    fn dyn_unmerge_addon(&mut self, receipt: &Receipt);
    fn dyn_clone(&self) -> Box<dyn DynExtension>;
    /// An empty instance of this type, for an addon's data to merge into.
    fn dyn_default(&self) -> Box<dyn DynExtension>;
}

impl<T: Extension> DynExtension for T {
    fn dyn_encode(&self) -> RawExtension {
        Extension::encode(self)
    }

    fn dyn_refs(&self) -> &[IdRef] {
        Extension::refs(self)
    }

    fn dyn_refs_mut(&mut self) -> &mut [IdRef] {
        Extension::refs_mut(self)
    }

    fn dyn_check(&self, model: &Model) -> Vec<Warning> {
        Extension::check(self, model)
    }

    fn dyn_reconcile(&mut self, model: &Model) {
        Extension::reconcile(self, model);
    }

    fn dyn_merge_addon(
        &self,
        addon: &dyn DynExtension,
    ) -> Result<(Box<dyn DynExtension>, Receipt), MergeRefused> {
        let addon = (addon as &dyn Any)
            .downcast_ref::<T>()
            .ok_or(MergeRefused::WrongType)?;
        let (merged, receipt) = Extension::merge_addon(self, addon)?;
        Ok((Box::new(merged), receipt))
    }

    fn dyn_unmerge_addon(&mut self, receipt: &Receipt) {
        Extension::unmerge_addon(self, receipt);
    }

    fn dyn_clone(&self) -> Box<dyn DynExtension> {
        Box::new(self.clone())
    }

    fn dyn_default(&self) -> Box<dyn DynExtension> {
        Box::new(T::default())
    }
}
