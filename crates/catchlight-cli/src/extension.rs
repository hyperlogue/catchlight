//! `extension`: the vendor annotations a `.clm` carries, at the file level.
//!
//! Byte values live in the `Extensions` section behind a `{size, hash}` marker
//! so an editor does not resend them with every unrelated structure push.

use std::path::Path;

use catchlight_core::extension::{
    codes, decode_as, Extension, ExtensionError, IdRef, Payload, RawExtension, Unread, Warning,
};
use catchlight_core::formats::clm::{
    extension_hash, ClmExtension, ClmExtensionBlob, ClmExtensionMarker, ClmFile, ClmPayload,
};
use catchlight_core::id::{ExtensionKey, EXTENSION_RESERVED_PREFIX};
use catchlight_core::{ExtensionValue, Model};
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
        ClmExtension::Bytes(_) => {
            let data = write_blob(&file, &key, path, out)?;
            Ok(Got::Bytes(data))
        }
        ClmExtension::Maintained {
            version,
            refs,
            payload: ClmPayload::Bytes(marker),
        } => {
            write_blob(&file, &key, path, out)?;
            Ok(Got::Json(render_json(&serde_json::json!({
                "version": version,
                "refs": refs,
                "size": marker.size,
                "hash": marker.hash,
            }))))
        }
    }
}

/// Writes the bytes `file` keeps for `key` to `out`, which a byte value needs.
fn write_blob(
    file: &ClmFile,
    key: &ExtensionKey,
    path: &Path,
    out: Option<&Path>,
) -> Result<Vec<u8>, Error> {
    let out = out.ok_or_else(|| Error::BytesNeedAFile {
        key: key.to_string(),
    })?;
    let blob = file
        .extensions
        .iter()
        .find(|blob| blob.key == *key)
        .ok_or_else(|| Error::NoSuchId {
            path: path.to_path_buf(),
            kind: "extension payload",
            id: key.to_string(),
        })?;
    std::fs::write(out, &blob.data).map_err(|e| Error::io(out, e))?;
    Ok(blob.data.clone())
}

pub fn set_json(path: &Path, key: &str, text: &str) -> Result<Changed, Error> {
    let key = parse_key(key)?;
    let value = parse_json(&key, text)?;
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
    let data = read_bytes(source)?;
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
    push_blob(&mut file, &key, data);
    write(path, &file)?;
    Ok(Changed { key, what })
}

fn parse_json(key: &ExtensionKey, text: &str) -> Result<serde_json::Value, Error> {
    serde_json::from_str(text).map_err(|e| Error::BadValue {
        field: format!("extension {}", key.as_str()),
        expected: format!("a JSON value ({e})"),
        value: text.to_string(),
    })
}

fn read_bytes(source: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(source).map_err(|e| Error::io(source, e))
}

fn push_blob(file: &mut ClmFile, key: &ExtensionKey, data: Vec<u8>) {
    file.extensions.push(ClmExtensionBlob {
        key: key.clone(),
        data,
    });
    // Key order makes the bytes independent of authoring order.
    file.extensions.sort_by(|a, b| a.key.cmp(&b.key));
}

/// What a value set from the command line holds: JSON text, or a file's bytes.
pub enum Source<'a> {
    Json(&'a str),
    Bytes(&'a Path),
}

/// Sets a maintained value: `version`, the reference table `refs` as a JSON
/// array in the file's own form (`[{"param": "eye_l"}, {"slot": ["head",
/// "slot-1"]}]`) and the payload. A reference the model does not hold is
/// kept and reported among the returned warnings, never refused.
pub fn set_maintained(
    path: &Path,
    key: &str,
    version: u32,
    refs: &str,
    payload: Source<'_>,
) -> Result<(Changed, Vec<Warning>), Error> {
    let key = parse_key(key)?;
    let refs: Vec<IdRef> = serde_json::from_str(refs).map_err(|e| Error::BadValue {
        field: "--refs".into(),
        expected: format!(
            "a JSON array of references such as [{{\"param\": \"id\"}}, {{\"slot\": \
             [\"node\", \"slot\"]}}] ({e})"
        ),
        value: refs.to_string(),
    })?;
    let mut file = file::read(path)?;
    let what = replaced(&file, &key);
    file.extensions.retain(|blob| blob.key != key);
    let payload = match payload {
        Source::Json(text) => ClmPayload::Json(parse_json(&key, text)?),
        Source::Bytes(source) => {
            let data = read_bytes(source)?;
            let marker = ClmExtensionMarker {
                size: data.len() as u64,
                hash: extension_hash(&data),
            };
            push_blob(&mut file, &key, data);
            ClmPayload::Bytes(marker)
        }
    };
    file.doc.extensions.insert(
        key.clone(),
        ClmExtension::Maintained {
            version,
            refs,
            payload,
        },
    );
    let (model, _) = file::load(&file, path)?;
    let warnings = model_warnings(&model, key.as_str())
        .into_iter()
        .filter(|w| w.code == codes::DANGLING || w.code == codes::REMOVED)
        .collect();
    write(path, &file)?;
    Ok((Changed { key, what }, warnings))
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

/// The maintained extension `T` in `file` for an edit: the default when the
/// file has none. Data this build cannot read is refused rather than
/// overwritten.
pub fn for_edit<T: Extension>(file: &ClmFile, path: &Path) -> Result<T, Error> {
    match typed::<T>(file) {
        Some(Ok(data)) => Ok(data),
        Some(Err(why)) => Err(Error::Unread {
            key: T::KEY.to_string(),
            why,
        }),
        None if file.doc.extensions.contains_key(T::KEY) => Err(Error::NotMaintained {
            path: path.to_path_buf(),
            key: T::KEY.to_string(),
        }),
        None => Ok(T::default()),
    }
}

/// Writes what `model` holds under `T::KEY` into `file`, or drops the key
/// when the model holds nothing there, so no empty extension is left behind.
pub fn store<T: Extension>(file: &mut ClmFile, model: &Model) -> Result<(), Error> {
    let key = ExtensionKey::new(T::KEY).map_err(|source| Error::BadId {
        value: T::KEY.to_string(),
        source,
    })?;
    file.extensions.retain(|blob| blob.key != key);
    let Some(value) = model.extensions().get(&key) else {
        file.doc.extensions.remove(&key);
        return Ok(());
    };
    let ExtensionValue::Maintained(raw) = value else {
        return Err(Error::Storage(format!(
            "extension {key:?} holds a plain value, which this command does not write"
        )));
    };
    let Payload::Json(payload) = &raw.payload else {
        return Err(Error::Storage(format!(
            "extension {key:?} encodes a byte payload, which this command does not write"
        )));
    };
    file.doc.extensions.insert(
        key,
        ClmExtension::Maintained {
            version: raw.version,
            refs: raw.refs.clone(),
            payload: ClmPayload::Json(payload.clone()),
        },
    );
    Ok(())
}

/// Why the data under `T::KEY` cannot be edited, as this program says it.
pub fn unreadable<T: Extension>(error: ExtensionError, path: &Path) -> Error {
    match error {
        ExtensionError::Unread(why) => Error::Unread {
            key: T::KEY.to_string(),
            why,
        },
        ExtensionError::NotMaintained => Error::NotMaintained {
            path: path.to_path_buf(),
            key: T::KEY.to_string(),
        },
        other => Error::Storage(format!("extension {:?}: {other}", T::KEY)),
    }
}

/// The warnings `model` raises under `key`: unresolved references and the
/// extension's own check.
pub fn model_warnings(model: &Model, key: &str) -> Vec<Warning> {
    model
        .extension_warnings()
        .into_iter()
        .filter(|w| w.key.as_str() == key)
        .map(|w| w.warning)
        .collect()
}

/// The warnings the model in `file` raises under `key`: unresolved
/// references and the extension's own check. Loading decodes no texture.
pub fn warnings(file: &ClmFile, path: &Path, key: &str) -> Result<Vec<Warning>, Error> {
    let (model, _) = file::load(file, path)?;
    Ok(model_warnings(&model, key))
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
