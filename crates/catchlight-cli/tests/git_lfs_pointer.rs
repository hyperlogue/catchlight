//! A checkout without `git lfs pull` leaves pointer text where a model should
//! be; the CLI names that rather than reporting a corrupt file.

mod common;

const POINTER: &str = "version https://git-lfs.github.com/spec/v1
oid sha256:82d292f59ed7506074685c9d1d00473845dbca9efd0941c5cc91e0fc0904ef6a
size 11496246
";

#[test]
fn a_git_lfs_pointer_is_named_by_every_reader() {
    let dir = common::tmp("git_lfs_pointer");
    let pointer = dir.join("model.clm");
    std::fs::write(&pointer, POINTER).unwrap();
    let pointer = pointer.to_str().unwrap();
    let out = dir.join("poses.cbor");

    for args in [
        vec!["storage", pointer],
        vec!["diff", pointer, pointer],
        vec!["poses", pointer, "--out", out.to_str().unwrap()],
    ] {
        let (code, _, stderr) = common::run(&args);
        assert_eq!(code, 2, "{args:?}: {stderr}");
        assert!(
            stderr.contains("is a Git LFS pointer") && stderr.contains("git lfs pull"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn other_bad_magic_is_not_called_a_pointer() {
    let dir = common::tmp("git_lfs_pointer_lookalike");
    // The marker must be the exact first line; a rewritten newline or a file
    // that merely mentions it is just not a .clm.
    for (name, text) in [
        ("crlf.clm", POINTER.replace('\n', "\r\n")),
        ("mention.clm", format!("notes: {POINTER}")),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        let (code, _, stderr) = common::run(&["storage", path.to_str().unwrap()]);
        assert_eq!(code, 2, "{stderr}");
        assert!(stderr.contains("unexpected magic bytes"), "{stderr}");
        assert!(!stderr.contains("Git LFS"), "{stderr}");
    }
}
