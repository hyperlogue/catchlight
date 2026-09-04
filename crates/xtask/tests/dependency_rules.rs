#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Workspace dependency rules that no single crate's manifest can state.
//!
//! A crate's `Cargo.toml` says what it depends on; it cannot say what it must
//! never come to depend on, and a rule like that is broken transitively, by a
//! dependency of a dependency, long before anyone edits the guilty manifest.
//! So the check reads the resolved graph out of `cargo metadata` instead.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Command;

/// The crate the rule is about: the command line over a `.clm` file.
const CLI: &str = "catchlight-cli";

/// What must never reach it. The editor protocol, the editor server, and the
/// three clients of that server. See `crates/catchlight-cli/src/lib.rs` for
/// why the line sits exactly here.
const ABOVE_THE_LINE: [&str; 5] = [
    "catchlight-editor-protocol",
    "catchlight-editor-server",
    "catchlight-editor",
    "catchlight-editor-cli",
    "catchlight-editor-wasm",
];

#[test]
fn the_cli_never_depends_on_the_editor_server_its_protocol_or_a_client() {
    let metadata = metadata();

    let names: HashMap<&str, &str> = metadata["packages"]
        .as_array()
        .expect("cargo metadata lists packages")
        .iter()
        .map(|package| {
            (
                package["id"].as_str().expect("a package id"),
                package["name"].as_str().expect("a package name"),
            )
        })
        .collect();
    let id_of = |name: &str| -> Option<&str> {
        names
            .iter()
            .find(|(_, package)| **package == name)
            .map(|(id, _)| *id)
    };

    let nodes: HashMap<&str, &serde_json::Value> = metadata["resolve"]["nodes"]
        .as_array()
        .expect("cargo metadata resolves the graph")
        .iter()
        .map(|node| (node["id"].as_str().expect("a node id"), node))
        .collect();

    let root = id_of(CLI).unwrap_or_else(|| panic!("{CLI} is a workspace member"));
    let reached = closure(&nodes, root);

    for name in ABOVE_THE_LINE {
        let id = id_of(name)
            .unwrap_or_else(|| panic!("{name} is not in the workspace; fix this test's list"));
        assert!(
            !reached.contains_key(id),
            "{CLI} depends on {name}, which it must not: {}",
            trail(&reached, &names, root, id),
        );
    }
}

/// `cargo metadata` for the whole workspace, deps included.
fn metadata() -> serde_json::Value {
    // crates/xtask/ -> crates/ -> workspace root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root above the xtask manifest");
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1"])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata writes json")
}

/// Every package reachable from `root`, each mapped to the package that
/// reached it first, so a failure can name a path rather than a bare crate.
///
/// Dev-dependency edges are followed out of `root` only: the CLI's own test
/// deps are as much a dependency on the server as its normal ones, while a
/// dev-dependency of something further down is never built into it.
fn closure<'a>(
    nodes: &HashMap<&'a str, &'a serde_json::Value>,
    root: &'a str,
) -> HashMap<&'a str, Option<&'a str>> {
    let mut reached: HashMap<&str, Option<&str>> = HashMap::from([(root, None)]);
    let mut queue: VecDeque<&str> = VecDeque::from([root]);
    while let Some(id) = queue.pop_front() {
        let Some(node) = nodes.get(id) else { continue };
        for dep in node["deps"].as_array().into_iter().flatten() {
            let pkg = dep["pkg"].as_str().expect("a dep names a package");
            let kinds = dep["dep_kinds"].as_array();
            let dev_only = kinds.is_some_and(|kinds| {
                !kinds.is_empty() && kinds.iter().all(|kind| kind["kind"] == "dev")
            });
            if dev_only && id != root {
                continue;
            }
            if !reached.contains_key(pkg) {
                reached.insert(pkg, Some(id));
                queue.push_back(pkg);
            }
        }
    }
    reached
}

/// The path the closure took from `root` to `id`, by crate name.
fn trail(
    reached: &HashMap<&str, Option<&str>>,
    names: &HashMap<&str, &str>,
    root: &str,
    id: &str,
) -> String {
    let mut hops = vec![id];
    let mut at = id;
    while at != root {
        let Some(Some(from)) = reached.get(at) else {
            break;
        };
        hops.push(from);
        at = from;
    }
    hops.reverse();
    hops.iter()
        .map(|hop| *names.get(hop).unwrap_or(hop))
        .collect::<Vec<_>>()
        .join(" -> ")
}
