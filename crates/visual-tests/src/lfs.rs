use std::path::Path;

/// A checkout without `git lfs pull` leaves pointer text in place of a model
/// or baseline. The spec keys a pointer on this exact first line.
pub(crate) fn is_pointer(bytes: &[u8]) -> bool {
    bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n")
}

/// For a file that already failed to decode.
pub(crate) fn is_pointer_file(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|bytes| is_pointer(&bytes))
}

pub(crate) fn pointer_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "{} is a Git LFS pointer, not the file it names; fetch it with `git lfs pull`",
        path.display()
    )
}
