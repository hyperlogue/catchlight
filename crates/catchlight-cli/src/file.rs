//! Reading and writing a `.clm` without going through a [`Model`] first.
//!
//! [`read`] decodes the container and the structure section and hands back the
//! [`ClmFile`] — the structure to edit and the texture table as verbatim bytes.
//! [`write`] encodes it again and replaces the destination atomically.
//!
//! The one thing that needs deciding before anything else is the file's
//! **shape**: a complete model has exactly one node whose `parent` is absent,
//! an addon fragment has none, and the two have separate readers that must not
//! be guessed between (`crates/catchlight-core/src/model/file.rs` says why).
//! [`shape_of`] answers that from the structure alone — it is a scan of one
//! field, no Model involved — and [`load`] uses it to pick the reader.

use std::path::{Path, PathBuf};

use catchlight_core::formats::clm::{self, ClmFile, ClmStructure};
use catchlight_core::{Model, ModelFormat};

use crate::Error;

/// Read a `.clm` off disk as a [`Model`], ready to build a puppet from.
///
/// The format dispatch lives in the core; this is only the filesystem half,
/// which the core deliberately does not have. `.clm` is the only model file
/// catchlight loads, so anything else is refused by its extension before a
/// byte is parsed — convert an inochi2d model once with
/// `cargo xtask import <model.inx>` and open the `.clm` it writes.
///
/// This is the load path for the commands that evaluate a model (`render`,
/// `poses`). The file operations go through [`read`] instead, which never
/// builds a `Model` at all.
pub fn load_model(path: &Path) -> Result<Model, Error> {
    let bytes = std::fs::read(path).map_err(|source| Error::io(path, source))?;
    let format = ModelFormat::from_path(path).ok_or_else(|| Error::NotAClm {
        path: path.to_path_buf(),
    })?;
    catchlight_core::load_model(&bytes, format).map_err(|source| Error::NotAModel {
        path: path.to_path_buf(),
        source,
    })
}

/// Which of the two disjoint wire shapes a structure is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// A complete model: exactly one node has no parent.
    Complete,
    /// An addon fragment: every node names a parent, and the ones the file
    /// does not carry are its roots.
    Fragment,
}

/// The shape of `doc`, or `None` for a structure with no nodes at all — which
/// is neither, and which no reader accepts.
///
/// This is deliberately the *whole* test: a node without a parent is what
/// makes a file a complete model, and the readers disagree about nothing else.
pub fn shape_of(doc: &ClmStructure) -> Option<Shape> {
    if doc.nodes.is_empty() {
        return None;
    }
    if doc.nodes.iter().any(|n| n.parent.is_none()) {
        Some(Shape::Complete)
    } else {
        Some(Shape::Fragment)
    }
}

/// Decode `path`. No image is decoded: the textures come back as the source
/// bytes the file stores.
pub fn read(path: &Path) -> Result<ClmFile, Error> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    clm::decode(&bytes).map_err(|source| Error::Decode {
        path: path.to_path_buf(),
        source,
    })
}

/// Encode `file` to bytes, reporting `path` as the file the failure is about.
pub fn encode(file: &ClmFile, path: &Path) -> Result<Vec<u8>, Error> {
    clm::encode(&file.doc, &file.textures, &file.extensions).map_err(|source| Error::Encode {
        path: path.to_path_buf(),
        source: catchlight_core::ModelError::from(source),
    })
}

/// Write `bytes` to `path`, replacing whatever was there in one step.
///
/// The bytes go to a temporary file beside the destination and are renamed
/// over it, so an interrupted write leaves the original file intact rather
/// than a half-written model. The temporary is removed if the rename fails.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty());
    let name = path.file_name().unwrap_or(path.as_os_str());
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".clm-tmp-{}", std::process::id()));
    let tmp: PathBuf = match dir {
        Some(dir) => dir.join(tmp_name),
        None => PathBuf::from(tmp_name),
    };

    std::fs::write(&tmp, bytes).map_err(|e| Error::io(&tmp, e))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::io(path, e));
    }
    Ok(())
}

/// Build a [`Model`] from an already-decoded file, using the reader its shape
/// calls for.
///
/// This decodes no textures and builds no puppet — it is the half of a load
/// that resolves Ids and checks invariants, which is what `extract`, `merge`
/// and `patch`'s verification need and all they need.
pub fn load(file: &ClmFile, path: &Path) -> Result<(Model, Shape), Error> {
    match shape_of(&file.doc) {
        None => Err(Error::Empty {
            path: path.to_path_buf(),
        }),
        Some(Shape::Complete) => Model::from_clm_file(file)
            .map(|m| (m, Shape::Complete))
            .map_err(|source| Error::NotAModel {
                path: path.to_path_buf(),
                source,
            }),
        Some(Shape::Fragment) => Model::from_clm_file_fragment(file)
            .map(|m| (m, Shape::Fragment))
            .map_err(|source| Error::NotAFragment {
                path: path.to_path_buf(),
                source,
            }),
    }
}

/// Read `path` as an addon fragment, whatever shape it is on the wire.
///
/// `merge` uses this rather than [`load`] so that being handed a complete
/// model reports itself as
/// [`ClmLoadError::FragmentHasNoParent`](catchlight_core::model::ClmLoadError::FragmentHasNoParent)
/// instead of quietly loading and then failing at install with
/// [`InstallError::NotAnAddon`](catchlight_core::InstallError::NotAnAddon).
pub fn load_fragment(file: &ClmFile, path: &Path) -> Result<Model, Error> {
    Model::from_clm_file_fragment(file).map_err(|source| Error::NotAFragment {
        path: path.to_path_buf(),
        source,
    })
}

/// Save `model` to `path`.
pub fn write_model(model: &Model, path: &Path) -> Result<(), Error> {
    let bytes = model.to_clm_bytes().map_err(|source| Error::Encode {
        path: path.to_path_buf(),
        source,
    })?;
    write(path, &bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use catchlight_core::formats::clm::{ClmNode, ClmNodeKind, ClmTransform};
    use catchlight_core::id::NodeId;

    fn node(id: &str, parent: Option<&str>) -> ClmNode {
        ClmNode {
            id: NodeId::new(id).unwrap(),
            parent: parent.map(|p| NodeId::new(p).unwrap()),
            name: String::new(),
            enabled: true,
            z_order: 0.0,
            transform: ClmTransform::default(),
            lock_to_root: false,
            kind: ClmNodeKind::Group,
        }
    }

    #[test]
    fn a_parentless_node_is_the_whole_difference_between_the_two_shapes() {
        let mut doc = ClmStructure::default();
        assert_eq!(shape_of(&doc), None, "no nodes is neither shape");

        doc.nodes = vec![node("root", None), node("root/a", Some("root"))];
        assert_eq!(shape_of(&doc), Some(Shape::Complete));

        doc.nodes = vec![node("hat", Some("base/head")), node("hat/a", Some("hat"))];
        assert_eq!(shape_of(&doc), Some(Shape::Fragment));
    }
}
