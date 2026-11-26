//! Reading and writing a `.clm` without going through a [`Model`] first.

use std::io::Write;
use std::path::Path;

use catchlight_core::formats::clm::{self, ClmError, ClmFile, ClmStructure};
use catchlight_core::formats::container::ContainerError;
use catchlight_core::{LoadError, Model, ModelError, ModelFormat};

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

pub(crate) fn not_a_model(path: &Path, bytes: &[u8], source: LoadError) -> Error {
    match &source {
        LoadError::Codec(clm) if is_bad_magic(clm) && is_git_lfs_pointer(bytes) => {
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
        source: ModelError::Encode(source),
    })
}

/// An output path, or an alias of one, must never replace the model it was
/// derived from.
pub fn check_output_path(input: &Path, output: &Path) -> Result<(), Error> {
    if let (Ok(input), Ok(output)) = (input.canonicalize(), output.canonicalize())
        && input == output
    {
        return Err(Error::OutputIsInput { path: output });
    }
    Ok(())
}

/// Atomic: see [`write_atomic`].
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    write_atomic(path, |file| {
        file.write_all(bytes).map_err(|e| Error::io(path, e))
    })
}

/// `fill` writes a temporary beside `path`, which is synced and renamed over
/// it only once `fill` succeeds, so a failed write leaves `path` as it was.
/// A destination that exists but is no regular file, such as `/dev/stdout` or
/// a FIFO, cannot be replaced and is written in place instead.
pub fn write_atomic(
    path: &Path,
    fill: impl FnOnce(&mut std::fs::File) -> Result<(), Error>,
) -> Result<(), Error> {
    if std::fs::metadata(path).is_ok_and(|m| !m.is_file()) {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| Error::io(path, e))?;
        return fill(&mut file);
    }
    let mut temp = temp_beside(path, "")?;
    fill(temp.as_file_mut())?;
    temp.as_file().sync_all().map_err(|e| Error::io(path, e))?;
    rename_into_place(temp.into_temp_path(), path)
}

/// A temporary in `path`'s directory, for content that will replace `path`
/// through [`persist`]. It carries the mode the finished file should have: a
/// replaced file keeps its own, and a new one takes the umask's default
/// rather than tempfile's owner-only one.
pub fn temp_beside(path: &Path, suffix: &str) -> Result<tempfile::NamedTempFile, Error> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".catchlight-").suffix(suffix);
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let temp = builder
        .tempfile_in(parent_dir(path))
        .map_err(|e| Error::io(path, e))?;
    if let Ok(existing) = std::fs::metadata(path) {
        temp.as_file()
            .set_permissions(existing.permissions())
            .map_err(|e| Error::io(path, e))?;
    }
    Ok(temp)
}

/// Syncs a finished [`temp_beside`] file and renames it over `path`.
pub fn persist(temp: tempfile::TempPath, path: &Path) -> Result<(), Error> {
    std::fs::File::open(&temp)
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::io(path, e))?;
    rename_into_place(temp, path)
}

fn rename_into_place(temp: tempfile::TempPath, path: &Path) -> Result<(), Error> {
    temp.persist(path).map_err(|e| Error::io(path, e.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent_dir(path))
        .and_then(|f| f.sync_all())
        .map_err(|e| Error::io(path, e))?;
    Ok(())
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
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

    #[cfg(unix)]
    #[test]
    fn a_new_file_takes_the_umask_mode_and_a_replaced_one_keeps_its_own() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mode = |name: &str| {
            std::fs::metadata(dir.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        std::fs::write(dir.path().join("reference"), b"").unwrap();
        write(&dir.path().join("new"), b"a").unwrap();
        assert_eq!(mode("new"), mode("reference"));

        let kept = dir.path().join("kept");
        std::fs::write(&kept, b"old").unwrap();
        std::fs::set_permissions(&kept, std::fs::Permissions::from_mode(0o640)).unwrap();
        write(&kept, b"new").unwrap();
        assert_eq!(mode("kept"), 0o640);
        assert_eq!(std::fs::read(&kept).unwrap(), b"new");

        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["kept", "new", "reference"], "no temporary is left");
    }

    #[test]
    fn a_failed_fill_leaves_the_destination_and_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out");
        std::fs::write(&path, b"before").unwrap();
        let result = write_atomic(&path, |file| {
            file.write_all(b"partial").unwrap();
            Err(Error::Empty { path: path.clone() })
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_destination_that_is_no_regular_file_is_written_in_place() {
        write(Path::new("/dev/null"), b"discarded").unwrap();
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
