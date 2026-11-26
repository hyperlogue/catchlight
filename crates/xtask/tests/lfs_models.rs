//! Checked-in `.clm` files are Git LFS objects. A checkout without
//! `git lfs pull` leaves pointer text in their place, and every test that
//! reads one then fails as bad magic; this names the cause once.

use std::path::Path;
use std::process::Command;

use catchlight_core::formats::clm::{self, ClmError};
use catchlight_core::formats::container::ContainerError;

#[test]
fn every_checked_in_model_is_fetched_and_decodes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");
    let listed = Command::new("git")
        .args(["ls-files", "-z", "--", "*.clm"])
        .current_dir(root)
        .output()
        .expect("run git ls-files");
    assert!(listed.status.success(), "git ls-files failed");
    let paths: Vec<&str> = std::str::from_utf8(&listed.stdout)
        .expect("utf-8 paths")
        .split('\0')
        .filter(|p| !p.is_empty())
        .collect();
    assert!(!paths.is_empty(), "no checked-in .clm files");

    let mut pointers = Vec::new();
    let mut broken = Vec::new();
    for path in paths {
        let bytes = std::fs::read(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        match clm::decode(&bytes) {
            Ok(_) => {}
            Err(ClmError::Container(ContainerError::BadMagic))
                if bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n") =>
            {
                pointers.push(path);
            }
            Err(e) => broken.push(format!("{path}: {e}")),
        }
    }
    assert!(
        pointers.is_empty(),
        "these are Git LFS pointers, not models; fetch them with `git lfs pull`:\n  {}",
        pointers.join("\n  ")
    );
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}
