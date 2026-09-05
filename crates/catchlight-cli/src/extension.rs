//! `extension`: the vendor annotations a `.clm` carries, at the file level.
//!
//! An extension is a key a vendor owns and a value catchlight never reads.
//! The key is the Id charset with a required dot, vendor first
//! (`molan.caster`), and `catchlight.` is the format's own: a file may carry
//! one, because the format may write one some day, but nothing here will
//! author one.
//!
//! A value is one of two things. **JSON** — string-keyed maps, arrays,
//! strings, numbers, bools, null — lives inline in the structure document.
//! **Bytes** live in the file's own `Extensions` section, and the structure
//! carries only a `{size, hash}` marker. That split is not about size: an
//! editor pushes its structure to a browser tab after every edit, and a
//! thumbnail inline in the structure would ride along with every unrelated
//! change. Behind a marker it travels once, and a client that already has
//! that hash fetches nothing.
//!
//! Like every other file operation here this works on the decoded
//! [`ClmFile`], never on a [`Model`](catchlight_core::Model), and decodes no
//! image.

use std::path::Path;

use catchlight_core::formats::clm::{
    extension_hash, ClmExtension, ClmExtensionBlob, ClmExtensionMarker, ClmFile,
    MAX_EXTENSION_BYTES,
};
use catchlight_core::id::{ExtensionKey, EXTENSION_RESERVED_PREFIX};

use crate::{file, Error};

/// One row of `extension list`.
pub struct Listed {
    pub key: ExtensionKey,
    pub kind: &'static str,
    /// Present for a byte value only.
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

/// What a `set` or a `delete` did.
pub struct Changed {
    pub key: ExtensionKey,
    pub what: &'static str,
}

impl std::fmt::Display for Changed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "extension {:?} {}", self.key.as_str(), self.what)
    }
}

/// What `get` found: text to print, or bytes to write.
pub enum Got {
    Json(String),
    Bytes(Vec<u8>),
}

/// Every extension in the file, in key order.
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
        })
        .collect())
}

/// One extension's value. A byte value needs somewhere to go, so `out` is
/// required for it: bytes are not something to print.
pub fn get(path: &Path, key: &str, out: Option<&Path>) -> Result<Got, Error> {
    let key = parse_key(key)?;
    let file = file::read(path)?;
    match lookup(&file, &key, path)? {
        ClmExtension::Json(value) => {
            if let Some(out) = out {
                let text = render_json(value);
                std::fs::write(out, text.as_bytes()).map_err(|e| Error::io(out, e))?;
                return Ok(Got::Json(String::new()));
            }
            Ok(Got::Json(render_json(value)))
        }
        ClmExtension::Bytes(_) => {
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

/// Put a JSON value under `key`, replacing whatever was there.
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

/// Put the contents of `source` under `key` as a byte value.
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
    // The section is written in key order, so two files holding the same
    // extensions hold the same bytes whatever order they were authored in.
    file.extensions.sort_by(|a, b| a.key.cmp(&b.key));
    write(path, &file)?;
    Ok(Changed { key, what })
}

/// Drop an extension. Deleting a key the file does not carry is an error.
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

/// How a `diff` renders one extension value: JSON shown whole, bytes as the
/// two things the marker carries.
pub fn render(value: &ClmExtension) -> String {
    match value {
        ClmExtension::Json(json) => render_json(json),
        ClmExtension::Bytes(marker) => {
            format!("{} bytes, blake3 {}", marker.size, marker.hash)
        }
    }
}

fn render_json(value: &serde_json::Value) -> String {
    // Compact, so a diff line stays a line.
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

/// Parse a key and refuse a reserved one. A reader accepts `catchlight.`
/// because the format may write such a key one day; authoring one here is
/// what is refused, exactly as `Model::set_extension` refuses it.
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
