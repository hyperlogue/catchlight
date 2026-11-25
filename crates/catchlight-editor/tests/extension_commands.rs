#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The extension commands end to end: a real editor behind its HTTP door
//! and the real binary.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use catchlight_editor_protocol::{Command, ParamId, Reply, Request, ResponseBody, SessionId};
use catchlight_editor_server::{bind_http, Editor, FileStorage, HttpOptions};
use serde_json::{json, Value};

const TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

struct Served {
    url: String,
    dir: PathBuf,
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// An editor with one session holding the params `jaw` (0..1 resting at
/// 0.2) and `turn` (-2..2 resting at -2).
fn serve(tag: &str) -> Served {
    let dir = std::env::temp_dir().join(format!("cl-ext-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let editor = Arc::new(Editor::with_storage(Arc::new(FileStorage::new(&dir))));
    let session = match editor.handle(Request {
        id: 1,
        command: Command::SessionNew {
            name: Some("rig".into()),
            source: None,
        },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("session_create: {other:?}"),
    };
    assert_eq!(session, SessionId(1));
    for (id, min, max, default) in [("jaw", 0.0, 1.0, 0.2), ("turn", -2.0, 2.0, -2.0)] {
        let reply = editor.handle(Request {
            id: 2,
            command: Command::ParamAdd {
                session,
                name: id.into(),
                min,
                max,
                default,
                param: Some(ParamId::new(id).unwrap()),
            },
        });
        assert!(matches!(reply, Reply::Ok { .. }), "{reply:?}");
    }
    let options = HttpOptions {
        token: Some(TOKEN.to_string()),
        ..HttpOptions::default()
    };
    let server = bind_http(editor, "127.0.0.1:0".parse().unwrap(), options).unwrap();
    let url = format!("http://127.0.0.1:{}", server.addr.port());
    std::thread::spawn(move || {
        let _ = server.serve();
    });
    Served { url, dir }
}

impl Served {
    fn run(&self, args: &[&str]) -> Output {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_catchlight-editor"));
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("CATCHLIGHT_") {
                command.env_remove(&name);
            }
        }
        command
            .current_dir(&self.dir)
            .env("CATCHLIGHT_STATE_DIR", self.dir.join("state"))
            .args(["--server", &self.url, "--token", TOKEN, "--session", "1"])
            .args(args)
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "{args:?}: {:?}\n{text}{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        text
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend(args);
        serde_json::from_str(&self.ok(&all)).unwrap()
    }

    /// Fails, and says why on stderr.
    fn refused(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(!out.status.success(), "{args:?} should fail");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

fn path(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn the_extension_group_sets_lists_gets_and_deletes_vendor_values() {
    let s = serve("group");
    let set = s.ok(&["extension", "set", "acme.notes", "--value", r#"{"a":1}"#]);
    assert_eq!(set.trim(), "extension acme.notes set");
    s.ok(&[
        "extension",
        "set",
        "acme.framing",
        "--version",
        "2",
        "--refs",
        r#"[{"param": "jaw"}, {"param": "ghost"}]"#,
        "--value",
        r#"{"shot":0}"#,
    ]);
    let blob = s.file("blob.bin");
    std::fs::write(&blob, b"cached bytes").unwrap();
    s.ok(&[
        "extension",
        "set",
        "acme.cache",
        "--version",
        "1",
        "--bytes",
        path(&blob),
    ]);

    let listed = s.ok(&["extension", "list"]);
    assert!(
        listed.contains(r#"extension acme.framing  version 2  refs [{"param":"jaw"},{"param":"ghost"}]  {"shot":0}"#),
        "{listed}"
    );
    assert!(
        listed.contains("extension acme.notes  {\"a\":1}"),
        "{listed}"
    );

    let got: Value = serde_json::from_str(&s.ok(&["extension", "get", "acme.notes"])).unwrap();
    assert_eq!(got, json!({"a": 1}));
    let got: Value = serde_json::from_str(&s.ok(&["extension", "get", "acme.framing"])).unwrap();
    assert_eq!(
        got,
        json!({"version": 2, "refs": [{"param": "jaw"}, {"param": "ghost"}], "payload": {"shot": 0}})
    );

    let why = s.refused(&["extension", "get", "acme.cache"]);
    assert!(why.contains("--out"), "{why}");
    let out = s.file("back.bin");
    let got: Value =
        serde_json::from_str(&s.ok(&["extension", "get", "acme.cache", "--out", path(&out)]))
            .unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"cached bytes");
    assert_eq!(
        (got["version"].clone(), got["size"].clone()),
        (json!(1), json!(12))
    );

    assert_eq!(
        s.ok(&["extension", "delete", "acme.notes"]).trim(),
        "extension acme.notes deleted"
    );
    let why = s.refused(&["extension", "delete", "acme.notes"]);
    assert!(why.contains("no_extension"), "{why}");
    let why = s.refused(&["extension", "set", "catchlight.humanoid", "--value", "{}"]);
    assert!(why.contains("reserved_extension"), "{why}");

    let listed = s.json(&["extension", "list"]);
    let keys: Vec<&str> = listed["body"]["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["acme.cache", "acme.framing"]);
}

#[test]
fn the_typed_commands_print_what_the_file_cli_prints() {
    let s = serve("typed");
    let controls: Value = serde_json::from_str(&s.ok(&["humanoid", "controls"])).unwrap();
    assert_eq!(controls[0], json!({"name": "head_yaw", "one_sided": false}));

    let mapped: Value =
        serde_json::from_str(&s.ok(&["humanoid", "map", "mouth_open", "jaw"])).unwrap();
    assert_eq!(
        mapped,
        json!({"control": "mouth_open", "param": "jaw", "stops": [0.2, 0.2, 1.0], "replaced": null})
    );
    let out = s.run(&["humanoid", "map", "head_yaw", "jaw", "--stops", "1,0,-1"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: catchlight.humanoid/shared-param"),
        "an edit prints the extension's warnings: {stderr}"
    );
    let why = s.refused(&["humanoid", "map", "mouth_opn", "jaw"]);
    assert!(
        why.contains("nonstandard_control") && why.contains("mouth_open"),
        "{why}"
    );

    let listed: Value = serde_json::from_str(&s.ok(&["humanoid", "list"])).unwrap();
    assert_eq!(listed["mappings"].as_array().unwrap().len(), 2);
    assert!(!listed["unmapped"]
        .as_array()
        .unwrap()
        .contains(&json!("mouth_open")));
    assert_eq!(
        listed["warnings"][0]["code"],
        "catchlight.humanoid/shared-param"
    );
    let removed: Value = serde_json::from_str(&s.ok(&["humanoid", "unmap", "head_yaw"])).unwrap();
    assert_eq!(
        removed,
        json!({"control": "head_yaw", "param": "jaw", "stops": [1.0, 0.0, -1.0]})
    );

    let set: Value = serde_json::from_str(&s.ok(&[
        "expression",
        "set",
        "sad",
        "--param",
        "turn=0.5",
        "--control",
        "mouth_smile=-0.5",
    ]))
    .unwrap();
    assert_eq!(
        set,
        json!({
            "expression": {"name": "sad", "settings": [
                {"param": "turn", "value": 0.5},
                {"control": "mouth_smile", "value": -0.5},
            ]},
            "replaced": null,
        }),
        "settings keep the order they were given in"
    );
    let names: Value = serde_json::from_str(&s.ok(&["expression", "names"])).unwrap();
    assert!(names.as_array().unwrap().contains(&json!("happy")));
    let listed: Value = serde_json::from_str(&s.ok(&["expression", "list"])).unwrap();
    assert_eq!(listed["expressions"][0]["name"], "sad");

    let added: Value = serde_json::from_str(&s.ok(&["viseme", "add-standard"])).unwrap();
    assert!(added["added"].as_array().unwrap().contains(&json!("aa")));
    let listed: Value = serde_json::from_str(&s.ok(&["viseme", "list"])).unwrap();
    assert_eq!(listed["visemes"][1]["name"], "aa");
    let removed: Value = serde_json::from_str(&s.ok(&["viseme", "remove", "aa"])).unwrap();
    assert_eq!(removed["name"], "aa");

    let why = s.refused(&["expression", "set", "x", "--control", "mouth_open=-1"]);
    assert!(why.contains("bad_request"), "{why}");

    let warnings = s.ok(&["extension", "warnings"]);
    assert!(
        warnings.contains("warning: catchlight.expressions: catchlight.expressions/"),
        "{warnings}"
    );
    let raw = s.json(&["extension", "warnings"]);
    assert_eq!(raw["body"]["result"], "extension_warnings");
}
