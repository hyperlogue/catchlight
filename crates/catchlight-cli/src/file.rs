//! Reading and writing a `.clm` without going through a [`Model`] first.

use std::path::{Path, PathBuf};

use catchlight_core::formats::clm::{self, ClmError, ClmFile, ClmStructure};
use catchlight_core::formats::container::ContainerError;
use catchlight_core::{Model, ModelError, ModelFormat};

use crate::Error;

pub fn load_model(path: &Path) -> Result<Model, Error> {
    let bytes = std::fs::read(path).map_err(|source| Error::io(path, source))?;
    let format = ModelFormat::from_path(path).ok_or_else(|| Error::NotAClm {
        path: path.to_path_buf(),
    })?;
    catchlight_core::load_model(&bytes, format).map_err(|source| not_a_model(path, &bytes, source))
}

/// A checkout without `git lfs pull` leaves pointer text in place of the file,
/// which the container rejects as bad magic. The spec keys a pointer on this
/// exact first line.
fn is_git_lfs_pointer(bytes: &[u8]) -> bool {
    bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n")
}

fn is_bad_magic(error: &ClmError) -> bool {
    matches!(error, ClmError::Container(ContainerError::BadMagic))
}

pub(crate) fn not_a_model(path: &Path, bytes: &[u8], source: ModelError) -> Error {
    match &source {
        ModelError::Clm(clm) if is_bad_magic(clm) && is_git_lfs_pointer(bytes) => {
            Error::GitLfsPointer {
                path: path.to_path_buf(),
            }
        }
        _ => Error::NotAModel {
            path: path.to_path_buf(),
            source,
        },
    }
}

pub(crate) fn not_a_clm(path: &Path, bytes: &[u8], source: ClmError) -> Error {
    if is_bad_magic(&source) && is_git_lfs_pointer(bytes) {
        return Error::GitLfsPointer {
            path: path.to_path_buf(),
        };
    }
    Error::Decode {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Complete,
    Fragment,
}

/// `None` for a structure with no nodes, which no reader accepts.
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

pub fn read(path: &Path) -> Result<ClmFile, Error> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    clm::decode(&bytes).map_err(|source| not_a_clm(path, &bytes, source))
}

pub fn encode(file: &ClmFile, path: &Path) -> Result<Vec<u8>, Error> {
    file.encode().map_err(|source| Error::Encode {
        path: path.to_path_buf(),
        source: catchlight_core::ModelError::from(source),
    })
}

/// Atomic: writes a temporary beside the destination and renames it over.
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

/// Decodes no textures and builds no puppet.
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

/// Reads as a fragment regardless of wire shape, so a complete model reports
/// `FragmentHasNoParent` rather than failing later at install.
pub fn load_fragment(file: &ClmFile, path: &Path) -> Result<Model, Error> {
    Model::from_clm_file_fragment(file).map_err(|source| Error::NotAFragment {
        path: path.to_path_buf(),
        source,
    })
}

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
