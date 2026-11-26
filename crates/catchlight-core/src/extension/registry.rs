//! The process-wide registry: which type reads each key, for the operations
//! that meet extensions nobody names (installing addons, checking models).

use std::any::TypeId;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use super::store::{Typed, decode_as};
use super::{Extension, RawExtension, Unread};
use crate::id::{EXTENSION_RESERVED_PREFIX, ExtensionKey, IdError};

#[doc(hidden)]
pub mod sealed {
    /// Implemented only by catchlight's own extension crates.
    pub trait Sealed {}
}

/// An extension catchlight itself defines, which may take a reserved key
/// through [`register_official`].
pub trait Official: Extension + sealed::Sealed {}

struct Entry {
    type_id: TypeId,
    type_name: &'static str,
    decode: fn(RawExtension) -> Result<Arc<Typed>, Unread>,
}

static REGISTRY: RwLock<BTreeMap<&'static str, Entry>> = RwLock::new(BTreeMap::new());

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegisterError {
    #[error("extension key {key:?} is already registered to {registered}")]
    Taken {
        key: &'static str,
        registered: &'static str,
    },
    #[error("extension key {key:?} is catchlight's own; a vendor key names its vendor first")]
    Reserved { key: &'static str },
    #[error("extension key {key:?}: {source}")]
    InvalidKey { key: &'static str, source: IdError },
}

/// Idempotent for the same `T`; a different type under a taken key is an
/// error, and so is a key under [`EXTENSION_RESERVED_PREFIX`]. Allowed at any
/// time: a model built earlier consults the registry on its next install or
/// check.
pub fn register<T: Extension>() -> Result<(), RegisterError> {
    if T::KEY.starts_with(EXTENSION_RESERVED_PREFIX) {
        return Err(RegisterError::Reserved { key: T::KEY });
    }
    insert::<T>()
}

/// [`register`] for catchlight's own extensions, whose keys are reserved.
pub fn register_official<T: Official>() -> Result<(), RegisterError> {
    insert::<T>()
}

fn insert<T: Extension>() -> Result<(), RegisterError> {
    ExtensionKey::new(T::KEY).map_err(|source| RegisterError::InvalidKey {
        key: T::KEY,
        source,
    })?;
    let type_name = std::any::type_name::<T>();
    let mut registry = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = registry.get(T::KEY) {
        if entry.type_id == TypeId::of::<T>() {
            return Ok(());
        }
        return Err(RegisterError::Taken {
            key: T::KEY,
            registered: entry.type_name,
        });
    }
    registry.insert(
        T::KEY,
        Entry {
            type_id: TypeId::of::<T>(),
            type_name,
            decode: decode_typed::<T>,
        },
    );
    Ok(())
}

fn decode_typed<T: Extension>(raw: RawExtension) -> Result<Arc<Typed>, Unread> {
    decode_as::<T>(raw).map(|data| Arc::new(Typed::new(Box::new(data))))
}

/// `None` when no type is registered under `key`.
pub fn decode_registered(key: &str, raw: &RawExtension) -> Option<Result<Arc<Typed>, Unread>> {
    let decode = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(key)?
        .decode;
    Some(decode(raw.clone()))
}
