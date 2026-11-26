//! How a model holds its extensions.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};

use super::{DynExtension, Extension, IdRef, RawExtension, Unread};
use crate::id::ExtensionKey;
use crate::model::ExtensionValue;

/// Decoded data, shared between a model, its clones and history snapshots
/// until one of them writes.
pub(crate) struct Typed {
    pub(crate) data: Box<dyn DynExtension>,
    /// Filled on first save, structure feed or comparison; a write replaces
    /// the whole `Typed`, so the cache never outlives the data it encodes.
    encoded: OnceLock<ExtensionValue>,
}

impl Typed {
    pub(crate) fn new(data: Box<dyn DynExtension>) -> Self {
        Self {
            data,
            encoded: OnceLock::new(),
        }
    }

    pub(crate) fn encoded(&self) -> &ExtensionValue {
        self.encoded
            .get_or_init(|| ExtensionValue::Maintained(self.data.dyn_encode()))
    }

    /// Every write goes through here, so the encoding cache cannot go stale.
    pub(crate) fn data_mut(&mut self) -> &mut dyn DynExtension {
        self.encoded = OnceLock::new();
        &mut *self.data
    }
}

impl Clone for Typed {
    fn clone(&self) -> Self {
        Self {
            data: self.data.dyn_clone(),
            encoded: self.encoded.clone(),
        }
    }
}

impl fmt::Debug for Typed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.data.fmt(f)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Stored {
    /// A plain value, or a maintained one not yet decoded. A typed read fills
    /// `decoded`; for a plain value it stays empty.
    Value {
        value: ExtensionValue,
        decoded: OnceLock<Result<Arc<Typed>, Unread>>,
    },
    Typed(Arc<Typed>),
}

impl Stored {
    pub(crate) fn value(value: ExtensionValue) -> Self {
        Self::Value {
            value,
            decoded: OnceLock::new(),
        }
    }

    pub(crate) fn typed(data: Box<dyn DynExtension>) -> Self {
        Self::Typed(Arc::new(Typed::new(data)))
    }

    pub(crate) fn as_value(&self) -> &ExtensionValue {
        match self {
            Self::Value { value, .. } => value,
            Self::Typed(typed) => typed.encoded(),
        }
    }

    /// `None` for a plain value, which has no table.
    pub(crate) fn refs(&self) -> Option<&[IdRef]> {
        match self {
            Self::Value {
                value: ExtensionValue::Maintained(raw),
                ..
            } => Some(&raw.refs),
            Self::Value { .. } => None,
            Self::Typed(typed) => Some(typed.data.dyn_refs()),
        }
    }

    /// The decoded data when it is at hand without a registry lookup.
    pub(crate) fn decoded(&self) -> Option<&Arc<Typed>> {
        match self {
            Self::Typed(typed) => Some(typed),
            Self::Value { decoded, .. } => decoded.get()?.as_ref().ok(),
        }
    }

    /// Rewrites each entry `f` maps. Returns whether a typed slot changed. A
    /// raw slot whose decode is cached is promoted first, so the cache never
    /// disagrees with the table.
    fn rewrite(&mut self, f: &impl Fn(&IdRef) -> Option<IdRef>) -> bool {
        let Some(refs) = self.refs() else {
            return false;
        };
        if !refs.iter().any(|r| f(r).is_some()) {
            return false;
        }
        let promoted = match self {
            Self::Value { decoded, .. } => match decoded.take() {
                Some(Ok(typed)) => Some(typed),
                _ => None,
            },
            Self::Typed(_) => None,
        };
        if let Some(typed) = promoted {
            *self = Self::Typed(typed);
        }
        let refs = match self {
            Self::Typed(typed) => Arc::make_mut(typed).data_mut().dyn_refs_mut(),
            Self::Value {
                value: ExtensionValue::Maintained(raw),
                ..
            } => &mut raw.refs[..],
            Self::Value { .. } => return false,
        };
        for r in refs.iter_mut() {
            if let Some(new) = f(r) {
                *r = new;
            }
        }
        matches!(self, Self::Typed(_))
    }
}

impl PartialEq for Stored {
    fn eq(&self, other: &Self) -> bool {
        if let (Self::Typed(a), Self::Typed(b)) = (self, other) {
            if Arc::ptr_eq(a, b) {
                return true;
            }
        }
        self.as_value() == other.as_value()
    }
}

/// Every extension a model carries, in key order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extensions {
    pub(crate) map: BTreeMap<ExtensionKey, Stored>,
}

impl Extensions {
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains_key(&self, key: &(impl AsRef<str> + ?Sized)) -> bool {
        self.map.contains_key(key.as_ref())
    }

    pub fn keys(&self) -> impl Iterator<Item = &ExtensionKey> {
        self.map.keys()
    }

    /// As it would be saved: a decoded extension is encoded on first ask.
    pub fn get(&self, key: &(impl AsRef<str> + ?Sized)) -> Option<&ExtensionValue> {
        self.map.get(key.as_ref()).map(Stored::as_value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ExtensionKey, &ExtensionValue)> {
        self.map
            .iter()
            .map(|(key, stored)| (key, stored.as_value()))
    }

    /// The reference table of a maintained extension; `None` for a plain value
    /// or a missing key.
    pub fn refs(&self, key: &(impl AsRef<str> + ?Sized)) -> Option<&[IdRef]> {
        self.map.get(key.as_ref())?.refs()
    }

    /// Returns the keys of the typed slots that changed.
    pub(crate) fn rewrite(&mut self, f: impl Fn(&IdRef) -> Option<IdRef>) -> Vec<ExtensionKey> {
        let mut touched = Vec::new();
        for (key, stored) in &mut self.map {
            if stored.rewrite(&f) {
                touched.push(key.clone());
            }
        }
        touched
    }
}

impl<'a> IntoIterator for &'a Extensions {
    type Item = (&'a ExtensionKey, &'a ExtensionValue);
    type IntoIter = Box<dyn Iterator<Item = Self::Item> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

/// Migrates `raw` and decodes it as `T`, as a typed read does; for data read
/// straight from a file. Needs no registry: `T` knows its own migrations and
/// decoding.
pub fn decode_as<T: Extension>(raw: RawExtension) -> Result<T, Unread> {
    if raw.version > T::VERSION {
        return Err(Unread::Newer {
            version: raw.version,
            readable: T::VERSION,
        });
    }
    let mut raw = raw;
    while raw.version < T::VERSION {
        let from = raw.version;
        raw = T::migrate(from, raw).map_err(Unread::Migration)?;
        raw.version = from + 1;
    }
    T::decode(raw).map_err(Unread::Decode)
}
