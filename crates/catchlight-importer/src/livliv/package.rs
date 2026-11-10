//! Bounded package IO. Nothing is extracted, executed or fetched from a URL.
//! Directory imports reject symlinks, including intermediate components.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::path::Path;

use super::{Error, Result};

pub(super) const MAX_PACKAGE: usize = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 2048;

/// A LivLiv package, with paths relative to its manifest.
#[derive(Debug)]
pub struct Package {
    pub(super) files: BTreeMap<String, Vec<u8>>,
}

impl Package {
    /// Read a ZIP by its contents, regardless of filename. Stored and DEFLATE
    /// entries are supported. Reject ambiguous names before reading resources.
    pub fn from_zip(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PACKAGE {
            return Err(Error::Invalid("ZIP exceeds 256 MiB".into()));
        }
        preflight_zip(bytes)?;
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
        // ZipArchive indexes by name, so validate before iteration to reject
        // duplicates that its index otherwise collapses. Also reject overlaps
        // and disagreements between local and central-directory headers.
        zip.validate_header_consistency()?;
        if zip.len() > MAX_ENTRIES {
            return Err(Error::Invalid("ZIP exceeds 2048 entries".into()));
        }
        let mut files = BTreeMap::new();
        let mut names = BTreeSet::new();
        let mut total = 0;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = std::str::from_utf8(entry.name_raw())
                .map_err(|_| Error::Invalid("ZIP name is not UTF-8".into()))?
                .to_owned();
            safe_path(name.trim_end_matches('/'))?;
            if !names.insert(name.clone()) {
                return Err(Error::Invalid(format!("duplicate ZIP entry: {name}")));
            }
            if entry.encrypted() || entry.is_symlink() {
                return Err(Error::Invalid(format!(
                    "encrypted or symlink entry: {name}"
                )));
            }
            if !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            ) {
                return Err(Error::Invalid(format!("unsupported compression: {name}")));
            }
            let size = usize::try_from(entry.size()).unwrap_or(usize::MAX);
            total = charge(total, size)?;
            if entry.is_dir() || ignored(&name) {
                continue;
            }
            let data = bounded_read(&mut entry, size)?;
            if data.len() != size {
                return Err(Error::Invalid(format!("incorrect entry size: {name}")));
            }
            files.insert(name, data);
        }
        Self::from_files(files)
    }

    /// Read an unpacked package. The directory must directly contain model.json.
    /// Symlinks and special files are refused, even when they point inside it.
    pub fn from_directory(root: &Path) -> Result<Self> {
        for ancestor in root.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            if std::fs::symlink_metadata(ancestor)?
                .file_type()
                .is_symlink()
            {
                return Err(Error::Invalid(
                    "package directory traverses a symlink".into(),
                ));
            }
        }
        let mut pending = vec![(root.to_path_buf(), String::new())];
        let mut files = BTreeMap::new();
        let mut total = 0;
        let mut entries = 0;
        while let Some((dir, prefix)) = pending.pop() {
            for item in std::fs::read_dir(dir)? {
                let item = item?;
                entries += 1;
                if entries > MAX_ENTRIES {
                    return Err(Error::Invalid("package exceeds 2048 entries".into()));
                }
                let leaf = item
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::Invalid("non-UTF-8 path".into()))?;
                let name = format!("{prefix}{leaf}");
                safe_path(&name)?;
                let kind = item.file_type()?;
                if kind.is_symlink() {
                    return Err(Error::Invalid(format!("symlink: {name}")));
                }
                if ignored(&name) {
                    continue;
                }
                if kind.is_dir() {
                    pending.push((item.path(), format!("{name}/")));
                } else if kind.is_file() {
                    let mut file = std::fs::File::open(item.path())?;
                    let size = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
                    total = charge(total, size)?;
                    files.insert(name, bounded_read(&mut file, size)?);
                } else {
                    return Err(Error::Invalid(format!("not a regular file: {name}")));
                }
            }
        }
        Self::from_files(files)
    }

    /// Construct from in-memory resources (also usable in wasm). Accept either
    /// root files or exactly one wrapping folder, never two competing manifests.
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        if files.len() > MAX_ENTRIES {
            return Err(Error::Invalid("package exceeds 2048 entries".into()));
        }
        let mut total = 0;
        for (name, data) in &files {
            safe_path(name)?;
            total = charge(total, data.len())?;
        }
        let files: BTreeMap<_, _> = files.into_iter().filter(|(n, _)| !ignored(n)).collect();
        let candidates: Vec<_> = files
            .keys()
            .filter(|n| n.as_str() == "model.json" || n.ends_with("/model.json"))
            .collect();
        if candidates.len() != 1 {
            return Err(Error::Invalid(
                "package must contain exactly one model.json".into(),
            ));
        }
        let prefix = candidates[0]
            .strip_suffix("model.json")
            .unwrap_or_default()
            .to_owned();
        if prefix.matches('/').count() > 1 || files.keys().any(|n| !n.starts_with(&prefix)) {
            return Err(Error::Invalid(
                "files must share the manifest's root or single wrapping folder".into(),
            ));
        }
        Ok(Self {
            files: files
                .into_iter()
                .map(|(n, d)| (n[prefix.len()..].to_owned(), d))
                .collect(),
        })
    }

    pub(super) fn get(&self, name: &str) -> Result<&[u8]> {
        safe_path(name)?;
        self.files
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::Invalid(format!("missing resource: {name}")))
    }
}

fn charge(total: usize, size: usize) -> Result<usize> {
    let total = total.saturating_add(size);
    if total > MAX_PACKAGE {
        return Err(Error::Invalid(
            "uncompressed package exceeds 256 MiB".into(),
        ));
    }
    Ok(total)
}

fn bounded_read(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Invalid("resource exceeds declared size".into()));
    }
    Ok(bytes)
}

pub(super) fn safe_path(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(['\\', ':', '\0'])
        || name
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err(Error::Invalid(format!("unsafe package path: {name:?}")));
    }
    Ok(())
}

fn ignored(name: &str) -> bool {
    name.split('/')
        .any(|s| s == "__MACOSX" || s.starts_with("._") || s == ".DS_Store" || s == "Thumbs.db")
}

// Bound the central-directory count before the ZIP library allocates entries.
// ZIP64 and spanning are outside LivLiv's package contract.
fn preflight_zip(bytes: &[u8]) -> Result<()> {
    let start = bytes.len().saturating_sub(65_535 + 22);
    let end = (start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            bytes.get(i..i + 4) == Some(b"PK\x05\x06")
                && i + 22 + u16::from_le_bytes([bytes[i + 20], bytes[i + 21]]) as usize
                    == bytes.len()
        })
        .ok_or_else(|| Error::Invalid("missing ZIP end record".into()))?;
    let field = |n| u16::from_le_bytes([bytes[end + n], bytes[end + n + 1]]);
    if field(4) != 0
        || field(6) != 0
        || field(8) != field(10)
        || field(10) as usize > MAX_ENTRIES
        || bytes[end + 12..end + 16] == [255; 4]
        || bytes[end + 16..end + 20] == [255; 4]
        || (end >= 20 && bytes.get(end - 20..end - 16) == Some(b"PK\x06\x07"))
    {
        return Err(Error::Invalid(
            "ZIP64, spanning or excessive ZIP entries".into(),
        ));
    }
    Ok(())
}
