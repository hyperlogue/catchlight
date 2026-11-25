//! `extension`: the vendor annotations a `.clm` carries, at the file level.
//!
//! Byte values live in the `Extensions` section behind a `{size, hash}` marker
//! so an editor does not resend them with every unrelated structure push.

use std::path::Path;

use catchlight_core::extension::{decode_as, Extension, Payload, RawExtension, Unread};
use catchlight_core::formats::clm::{
    extension_hash, ClmExtension, ClmExtensionBlob, ClmExtensionMarker, ClmFile, ClmPayload,
    MAX_EXTENSION_BYTES,
};
use catchlight_core::id::{ExtensionKey, EXTENSION_RESERVED_PREFIX};
use catchlight_extension_animations::Animations;

use crate::{file, Error};

pub struct Listed {
    pub key: ExtensionKey,
    pub kind: &'static str,
    /// Present for a byte value or byte payload only.
    pub size: Option<u64>,
}

impl std::fmt::Display for Listed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.size {
            Some(size) => write!(f, "{}\t{}\t{size}", self.key, self.kind),
            None => write!(f, "{}\t{}", self.key, self.kind),
        }
    }
}

pub struct Changed {
    pub key: ExtensionKey,
    pub what: &'static str,
}

impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "extension {:?} {}", self.key.as_str(), self.what)
    }
}

pub enum Got {
    Json(String),
    Bytes(Vec<u8>),
}

pub fn list(path: &Path) -> Result<Vec<Listed>, Error> {
    let file = file::read(path)?;
    Ok(file
        .doc
        .extensions
        .iter()
        .map(|(key, value)| match value {
            ClmExtension::Json(_) => Listed {
                key: key.clone(),
                kind: "json",
                size: None,
            },
            ClmExtension::Bytes(marker) => Listed {
                key: key.clone(),
                kind: "bytes",
                size: Some(marker.size),
            },
            ClmExtension::Maintained { .. } => Listed {
                key: key.clone(),
                kind: "maintained",
                size: value.marker().map(|marker| marker.size),
            },
        })
        .collect())
}

/// A maintained extension this program has a type for prints as that type
/// decodes it, so its byte payload stays inspectable.
pub fn get(path: &Path, key: &str, out: Option<&Path>) -> Result<Got, Error> {
    let key = ExtensionKey::new(key).map_err(|source| Error::BadId {
        value: key.to_string(),
        source,
    })?;
    let file = file::read(path)?;
    if key.as_str() == Animations::KEY {
        if let Some(decoded) = typed::<Animations>(&file) {
            let clips = decoded.map_err(|why| Error::Unread {
                key: key.to_string(),
                why,
            })?;
            let text = serde_json::to_string(&clips.clips())
                .unwrap_or_else(|_| "<unrenderable>".to_string());
            if let Some(out) = out {
                std::fs::write(out, text.as_bytes()).map_err(|e| Error::io(out, e))?;
                return Ok(Got::Json(String::new()));
            }
            return Ok(Got::Json(text));
        }
    }
    match lookup(&file, &key, path)? {
        ClmExtension::Json(value) => {
            if let Some(out) = out {
                let text = render_json(value);
                std::fs::write(out, text.as_bytes()).map_err(|e| Error::io(out, e))?;
                return Ok(Got::Json(String::new()));
            }
            Ok(Got::Json(render_json(value)))
        }
        ClmExtension::Maintained {
            version,
            refs,
            payload: ClmPayload::Json(value),
        } => {
            let text = render_json(&serde_json::json!({
                "version": version,
                "refs": refs,
                "payload": value,
            }));
            if let Some(out) = out {
                std::fs::write(out, text.as_bytes()).map_err(|e| Error::io(out, e))?;
                return Ok(Got::Json(String::new()));
            }
            Ok(Got::Json(text))
        }
        ClmExtension::Bytes(_)
        | ClmExtension::Maintained {
            payload: ClmPayload::Bytes(_),
            ..
        } => {
            let out = out.ok_or_else(|| Error::BytesNeedAFile {
                key: key.to_string(),
            })?;
            let blob = file
                .extensions
                .iter()
                .find(|blob| blob.key == key)
                .ok_or_else(|| Error::NoSuchId {
                    path: path.to_path_buf(),
                    kind: "extension payload",
                    id: key.to_string(),
                })?;
            std::fs::write(out, &blob.data).map_err(|e| Error::io(out, e))?;
            Ok(Got::Bytes(blob.data.clone()))
        }
    }
}

pub fn set_json(path: &Path, key: &str, text: &str) -> Result<Changed, Error> {
    let key = parse_key(key)?;
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| Error::BadValue {
        field: format!("extension {}", key.as_str()),
        expected: format!("a JSON value ({e})"),
        value: text.to_string(),
    })?;
    let mut file = file::read(path)?;
    let what = replaced(&file, &key);
    file.extensions.retain(|blob| blob.key != key);
    file.doc
        .extensions
        .insert(key.clone(), ClmExtension::Json(value));
    write(path, &file)?;
    Ok(Changed { key, what })
}

pub fn set_bytes(path: &Path, key: &str, source: &Path) -> Result<Changed, Error> {
    let key = parse_key(key)?;
    let data = std::fs::read(source).map_err(|e| Error::io(source, e))?;
    if data.len() > MAX_EXTENSION_BYTES {
        return Err(Error::ExtensionTooLarge {
            key: key.to_string(),
            size: data.len(),
            max: MAX_EXTENSION_BYTES,
        });
    }
    let mut file = file::read(path)?;
    let what = replaced(&file, &key);
    file.doc.extensions.insert(
        key.clone(),
        ClmExtension::Bytes(ClmExtensionMarker {
            size: data.len() as u64,
            hash: extension_hash(&data),
        }),
    );
    file.extensions.retain(|blob| blob.key != key);
    file.extensions.push(ClmExtensionBlob {
        key: key.clone(),
        data,
    });
    // Key order makes the bytes independent of authoring order.
    file.extensions.sort_by(|a, b| a.key.cmp(&b.key));
    write(path, &file)?;
    Ok(Changed { key, what })
}

pub fn delete(path: &Path, key: &str) -> Result<Changed, Error> {
    let key = parse_key(key)?;
    let mut file = file::read(path)?;
    if file.doc.extensions.remove(&key).is_none() {
        return Err(Error::NoSuchId {
            path: path.to_path_buf(),
            kind: "extension",
            id: key.to_string(),
        });
    }
    file.extensions.retain(|blob| blob.key != key);
    write(path, &file)?;
    Ok(Changed {
        key,
        what: "deleted",
    })
}

pub fn render(value: &ClmExtension) -> String {
    match value {
        ClmExtension::Json(json) => render_json(json),
        ClmExtension::Bytes(marker) => {
            format!("{} bytes, blake3 {}", marker.size, marker.hash)
        }
        ClmExtension::Maintained {
            version,
            refs,
            payload,
        } => {
            let payload = match payload {
                ClmPayload::Json(json) => render_json(json),
                ClmPayload::Bytes(marker) => {
                    format!("{} bytes, blake3 {}", marker.size, marker.hash)
                }
            };
            format!("version {version}, {} reference(s), {payload}", refs.len())
        }
    }
}

/// The maintained extension `T` as `file` stores it, decoded the way a
/// model's typed read decodes it. `None` when the file has none, or a plain
/// value under the key.
pub fn typed<T: Extension>(file: &ClmFile) -> Option<Result<T, Unread>> {
    raw(file, T::KEY).map(decode_as::<T>)
}

fn raw(file: &ClmFile, key: &str) -> Option<RawExtension> {
    let ClmExtension::Maintained {
        version,
        refs,
        payload,
    } = file.doc.extensions.get(key)?
    else {
        return None;
    };
    let payload = match payload {
        ClmPayload::Json(json) => Payload::Json(json.clone()),
        ClmPayload::Bytes(_) => Payload::Bytes(
            file.extensions
                .iter()
                .find(|blob| blob.key.as_str() == key)?
                .data
                .as_slice()
                .into(),
        ),
    };
    Some(RawExtension {
        version: *version,
        refs: refs.clone(),
        payload,
    })
}

fn render_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_string())
}

fn lookup<'a>(
    file: &'a ClmFile,
    key: &ExtensionKey,
    path: &Path,
) -> Result<&'a ClmExtension, Error> {
    file.doc.extensions.get(key).ok_or_else(|| Error::NoSuchId {
        path: path.to_path_buf(),
        kind: "extension",
        id: key.to_string(),
    })
}

fn replaced(file: &ClmFile, key: &ExtensionKey) -> &'static str {
    if file.doc.extensions.contains_key(key) {
        "replaced"
    } else {
        "set"
    }
}

/// Readers accept `catchlight.` keys because the format may write one; only
/// authoring one is refused, as `Model::set_extension` does.
fn parse_key(text: &str) -> Result<ExtensionKey, Error> {
    let key = ExtensionKey::new(text).map_err(|source| Error::BadId {
        value: text.to_string(),
        source,
    })?;
    if key.is_reserved() {
        return Err(Error::ReservedExtension {
            key: key.to_string(),
            prefix: EXTENSION_RESERVED_PREFIX,
        });
    }
    Ok(key)
}

fn write(path: &Path, file: &ClmFile) -> Result<(), Error> {
    let bytes = file::encode(file, path)?;
    file::write(path, &bytes)
}
