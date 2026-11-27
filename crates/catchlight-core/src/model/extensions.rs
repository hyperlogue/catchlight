//! A model's extensions: stored values, typed access, and the core's side of
//! keeping them consistent with the model.

use std::any::Any;

use crate::extension::{
    Extension, ExtensionError, Stored, Typed, Warning, codes, decode_as, decode_registered,
};

use super::*;

/// An extension as stored. JSON values travel inline with the structure;
/// bytes live in their own section and travel by hash.
#[derive(Clone, PartialEq, Eq)]
pub enum ExtensionValue {
    /// A plain annotation: carried, never maintained.
    Json(serde_json::Value),
    /// A plain annotation: carried, never maintained.
    Bytes(Arc<[u8]>),
    Maintained(RawExtension),
}

impl std::fmt::Debug for ExtensionValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(value) => f.debug_tuple("Json").field(value).finish(),
            Self::Bytes(bytes) => f
                .debug_struct("Bytes")
                .field("len", &bytes.len())
                .finish_non_exhaustive(),
            Self::Maintained(raw) => f.debug_tuple("Maintained").field(raw).finish(),
        }
    }
}

impl ExtensionValue {
    /// The bytes this value keeps in the `Extensions` section, a maintained
    /// extension's byte payload among them.
    #[must_use]
    pub const fn bytes(&self) -> Option<&Arc<[u8]>> {
        match self {
            Self::Bytes(data) => Some(data),
            Self::Maintained(raw) => raw.payload.bytes(),
            Self::Json(_) => None,
        }
    }

    /// The JSON this value carries inline, a maintained extension's JSON
    /// payload among them.
    #[must_use]
    pub const fn json(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Json(value) => Some(value),
            Self::Maintained(raw) => raw.payload.json(),
            Self::Bytes(_) => None,
        }
    }
}

impl Model {
    pub const fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    pub fn unknown_sections(&self) -> &[clm::ClmUnknownSection] {
        &self.unknown_sections
    }

    /// `catchlight.` keys are reserved and refused. Size caps match the reader's,
    /// so any value taken can be saved.
    pub fn set_extension(
        &mut self,
        key: ExtensionKey,
        value: ExtensionValue,
    ) -> Result<(), ModelError> {
        if key.is_reserved() {
            return Err(ModelError::ReservedExtension(key));
        }
        self.extensions.map.insert(key, Stored::value(value));
        self.bump();
        Ok(())
    }

    /// A missing key is an error, so a typo is not silently ignored.
    pub fn delete_extension(&mut self, key: &ExtensionKey) -> Result<(), ModelError> {
        if key.is_reserved() {
            return Err(ModelError::ReservedExtension(key.clone()));
        }
        if self.extensions.map.remove(key).is_none() {
            return Err(ModelError::UnknownExtension(key.clone()));
        }
        self.bump();
        Ok(())
    }

    /// Decodes on first read and caches the result; needs no registry.
    pub fn extension<T: Extension>(&self) -> Result<&T, ExtensionError> {
        let stored = self
            .extensions
            .map
            .get(T::KEY)
            .ok_or(ExtensionError::Missing)?;
        let typed = match stored {
            Stored::Typed(typed) => typed,
            Stored::Value {
                value: ExtensionValue::Maintained(raw),
                decoded,
            } => decoded
                .get_or_init(|| {
                    decode_as::<T>(raw.clone()).map(|data| Arc::new(Typed::new(Box::new(data))))
                })
                .as_ref()
                .map_err(|why| ExtensionError::Unread(why.clone()))?,
            Stored::Value { .. } => return Err(ExtensionError::NotMaintained),
        };
        (&*typed.data as &dyn Any)
            .downcast_ref::<T>()
            .ok_or(ExtensionError::WrongType)
    }

    /// Copy-on-write: unshares the data only if a clone or a history snapshot
    /// still holds it. Bumps the generation up front, so a borrow that changes
    /// nothing still counts as an edit.
    pub fn extension_mut<T: Extension>(&mut self) -> Result<&mut T, ExtensionError> {
        let stored = self
            .extensions
            .map
            .get(T::KEY)
            .ok_or(ExtensionError::Missing)?;
        let promoted = match stored {
            Stored::Typed(typed) => {
                if !(&*typed.data as &dyn Any).is::<T>() {
                    return Err(ExtensionError::WrongType);
                }
                None
            }
            Stored::Value {
                value: ExtensionValue::Maintained(raw),
                decoded,
            } => {
                let typed = match decoded.get() {
                    Some(Ok(typed)) => (**typed).clone(),
                    Some(Err(why)) => return Err(ExtensionError::Unread(why.clone())),
                    None => Typed::new(Box::new(
                        decode_as::<T>(raw.clone()).map_err(ExtensionError::Unread)?,
                    )),
                };
                if !(&*typed.data as &dyn Any).is::<T>() {
                    return Err(ExtensionError::WrongType);
                }
                Some(typed)
            }
            Stored::Value { .. } => return Err(ExtensionError::NotMaintained),
        };
        if let Some(mut typed) = promoted {
            let key = self.extension_key::<T>()?;
            // A raw slot keeps its removed references until its first write.
            // Out of the map while it reconciles, as in `remove_in_extensions`.
            self.extensions.map.remove(&key);
            typed.data_mut().dyn_reconcile(self);
            self.extensions
                .map
                .insert(key, Stored::Typed(Arc::new(typed)));
        }
        self.bump();
        match self.extensions.map.get_mut(T::KEY) {
            Some(Stored::Typed(typed)) => (Arc::make_mut(typed).data_mut() as &mut dyn Any)
                .downcast_mut::<T>()
                .ok_or(ExtensionError::WrongType),
            _ => Err(ExtensionError::Missing),
        }
    }

    /// Creates the extension when absent; needs no registry.
    pub fn extension_or_default<T: Extension>(&mut self) -> Result<&mut T, ExtensionError> {
        if !self.extensions.map.contains_key(T::KEY) {
            let key = self.extension_key::<T>()?;
            self.extensions
                .map
                .insert(key, Stored::typed(Box::new(T::default())));
        }
        self.extension_mut::<T>()
    }

    /// Drops whatever is stored under `T::KEY`, reserved keys included, so a
    /// tool can remove an official extension it has emptied. Returns whether
    /// anything was there.
    pub fn remove_extension<T: Extension>(&mut self) -> bool {
        let removed = self.extensions.map.remove(T::KEY).is_some();
        if removed {
            self.bump();
        }
        removed
    }

    fn extension_key<T: Extension>(&self) -> Result<ExtensionKey, ExtensionError> {
        if let Some((key, _)) = self.extensions.map.get_key_value(T::KEY) {
            return Ok(key.clone());
        }
        ExtensionKey::new(T::KEY).map_err(|_| ExtensionError::InvalidKey(T::KEY))
    }

    /// Marks every reference `gone` matches as removed, then lets each typed
    /// extension apply its policy. A raw slot keeps the marker.
    pub(super) fn remove_in_extensions(&mut self, gone: impl Fn(&IdRef) -> bool) {
        let touched = self
            .extensions
            .rewrite(|r| (!r.is_removed() && gone(r)).then(|| r.removed()));
        for key in touched {
            // Out of the map while it reconciles, so it can read the model.
            let Some(mut stored) = self.extensions.map.remove(&key) else {
                continue;
            };
            if let Stored::Typed(typed) = &mut stored {
                Arc::make_mut(typed).data_mut().dyn_reconcile(self);
            }
            self.extensions.map.insert(key, stored);
        }
    }

    /// The typed data under `key`, decoding a raw slot through the registry.
    /// `None` when the key is not registered.
    pub(super) fn registered_extension(
        stored: &Stored,
        key: &ExtensionKey,
    ) -> Option<Result<Arc<Typed>, crate::extension::Unread>> {
        if let Some(typed) = stored.decoded() {
            return Some(Ok(typed.clone()));
        }
        match stored {
            Stored::Value {
                value: ExtensionValue::Maintained(raw),
                ..
            } => decode_registered(key.as_ref(), raw),
            _ => None,
        }
    }

    /// Every finding about this model's maintained extensions: references
    /// that no longer resolve, each extension's own check, and data this
    /// program cannot read or check.
    pub fn extension_warnings(&self) -> Vec<ExtensionWarning> {
        let fragment = self.is_fragment();
        let mut out = Vec::new();
        for (key, stored) in &self.extensions.map {
            let Some(refs) = stored.refs() else { continue };
            let mut push = |warning: Warning| {
                out.push(ExtensionWarning {
                    key: key.clone(),
                    warning,
                });
            };
            for r in refs {
                // In a fragment, a missing node, param or slot is a
                // requirement on the base; a texture never is.
                let required = fragment && !matches!(r, IdRef::Texture(_));
                if r.is_removed() {
                    push(
                        Warning::new(codes::REMOVED, format!("refers to {r}"))
                            .with_subjects(vec![r.clone()]),
                    );
                } else if !required && !r.resolves_in(self) {
                    push(
                        Warning::new(
                            codes::DANGLING,
                            format!("refers to {r}, which the model lacks"),
                        )
                        .with_subjects(vec![r.clone()]),
                    );
                }
            }
            let typed = match Self::registered_extension(stored, key) {
                Some(Ok(typed)) => typed,
                Some(Err(why)) => {
                    let code = match why {
                        crate::extension::Unread::Newer { .. } => codes::NEWER,
                        _ => codes::UNREAD,
                    };
                    push(Warning::new(
                        code,
                        format!("kept as stored and not checked: {why}"),
                    ));
                    continue;
                }
                None => {
                    push(Warning::new(
                        codes::NOT_CHECKED,
                        format!("not checked: this program has not registered {key:?}"),
                    ));
                    continue;
                }
            };
            for warning in typed.data.dyn_check(self) {
                push(warning);
            }
        }
        out
    }
}
