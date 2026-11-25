//! `extension`: the vendor data a model carries under its own key, as the
//! file CLI's `extension` group edits it in a file. `catchlight.` keys are the
//! official extensions', which only their typed commands edit.

use super::*;

#[derive(Subcommand)]
pub(super) enum ExtensionCmd {
    /// List every extension: a JSON value whole, bytes as their size and
    /// hash, and a maintained value with its version and reference table.
    List,
    /// Print one extension's JSON, or write a byte value to --out.
    Get {
        /// The extension key.
        key: ExtensionKey,
        /// Write the bytes here. Required for a byte value or payload.
        #[arg(short, long)]
        out: Option<String>,
    },
    /// Set one extension, replacing whatever was under that key. With
    /// --version it is maintained: --value or --bytes is its payload, and the
    /// editor keeps its --refs current as the model's Ids change.
    Set {
        /// The extension key.
        key: ExtensionKey,
        /// A JSON value, as text. (`--json` is the global flag for raw replies.)
        #[arg(long, conflicts_with = "bytes", required_unless_present = "bytes")]
        value: Option<String>,
        /// A file whose contents become the value.
        #[arg(long)]
        bytes: Option<String>,
        /// Store a maintained value at this version of its format.
        #[arg(long)]
        version: Option<u32>,
        /// The maintained value's reference table, as a JSON array:
        /// {"param": "id"}, {"node": "id"}, {"texture": "id"} or
        /// {"slot": ["node", "slot"]}. A reference the model lacks is kept
        /// and listed by `extension warnings`.
        #[arg(long, requires = "version")]
        refs: Option<String>,
    },
    /// Drop one extension.
    Delete {
        /// The extension key.
        key: ExtensionKey,
    },
    /// Print every finding about the maintained extensions: references the
    /// model lacks, each extension's own check, and data this editor cannot
    /// read or check.
    Warnings,
}

pub(super) fn run(
    cli: &Cli,
    action: &ExtensionCmd,
    stream: &mut transport::Transport,
) -> Result<i32> {
    let session = resolve_session(cli)?;
    let mut siblings = serde_json::Map::new();
    // `extension_get` always names a file for bytes, which only a byte value
    // writes; without --out it goes to a scratch file removed afterwards.
    let mut scratch = None;
    let command = match action {
        ExtensionCmd::List => Command::Extensions { session },
        ExtensionCmd::Get { key, out } => {
            let out = match out {
                Some(out) => absolute(out)?,
                None => {
                    let path = std::env::temp_dir().join(format!(
                        "catchlight-extension-{}-{}",
                        std::process::id(),
                        key
                    ));
                    let path = path.display().to_string();
                    scratch = Some(path.clone());
                    path
                }
            };
            siblings.insert("out".into(), out.into());
            Command::ExtensionGet {
                session,
                key: key.clone(),
            }
        }
        ExtensionCmd::Set {
            key,
            value,
            bytes,
            version,
            refs,
        } => {
            let value = match value {
                Some(text) => Some(
                    serde_json::from_str::<serde_json::Value>(text)
                        .map_err(|e| anyhow!("--value is not a JSON value: {e}"))?,
                ),
                None => None,
            };
            if let Some(path) = bytes {
                siblings.insert(
                    "files".into(),
                    serde_json::json!({"value": absolute(path)?}),
                );
            }
            let value = match (version, value) {
                (Some(version), value) => ExtensionSet::Maintained {
                    version: *version,
                    refs: match refs {
                        Some(text) => serde_json::from_str(text).map_err(|e| {
                            anyhow!(
                                "--refs takes a JSON array of references such as \
                                 [{{\"param\": \"id\"}}, {{\"slot\": [\"node\", \"slot\"]}}]: {e}"
                            )
                        })?,
                        None => Vec::new(),
                    },
                    value,
                },
                (None, Some(value)) => ExtensionSet::Json { value },
                (None, None) => ExtensionSet::Bytes,
            };
            Command::ExtensionSet {
                session,
                key: key.clone(),
                value,
            }
        }
        ExtensionCmd::Delete { key } => Command::ExtensionDelete {
            session,
            key: key.clone(),
        },
        ExtensionCmd::Warnings => Command::ExtensionWarnings { session },
    };
    let reply = stream.call(Request { id: 1, command }, siblings);
    let wrote_bytes = scratch.as_ref().is_some_and(|path| {
        let wrote = std::path::Path::new(path).exists();
        let _ = std::fs::remove_file(path);
        wrote
    });
    let reply = reply?;
    if cli.json {
        print_reply(cli, &reply);
        return Ok(i32::from(matches!(reply, Reply::Err { .. })));
    }
    match (&reply, action) {
        (Reply::Ok { body, .. }, ExtensionCmd::Get { key, .. }) => {
            if wrote_bytes {
                bail!("extension {key} is a byte value; say where to write it with --out");
            }
            if let ResponseBody::Extension { value, .. } = body {
                if let Some(text) = value_json(value) {
                    println!("{text}");
                }
            }
        }
        (Reply::Ok { .. }, ExtensionCmd::Set { key, .. }) => println!("extension {key} set"),
        (Reply::Ok { .. }, ExtensionCmd::Delete { key }) => println!("extension {key} deleted"),
        (
            Reply::Ok {
                body: ResponseBody::ExtensionWarnings { warnings },
                ..
            },
            ExtensionCmd::Warnings,
        ) => official::print_warnings(warnings),
        _ => print_reply(cli, &reply),
    }
    Ok(i32::from(matches!(reply, Reply::Err { .. })))
}

/// What `extension get` prints, as the file CLI prints it: a JSON value
/// whole, a maintained value as its envelope. A plain byte value prints
/// nothing; its bytes went to --out.
fn value_json(value: &ExtensionValueInfo) -> Option<String> {
    let json = match value {
        ExtensionValueInfo::Json { value } => value.clone(),
        ExtensionValueInfo::Bytes { .. } => return None,
        ExtensionValueInfo::Maintained {
            version,
            refs,
            value: Some(payload),
            ..
        } => serde_json::json!({"version": version, "refs": refs, "payload": payload}),
        ExtensionValueInfo::Maintained {
            version,
            refs,
            size,
            hash,
            ..
        } => serde_json::json!({"version": version, "refs": refs, "size": size, "hash": hash}),
    };
    serde_json::to_string(&json).ok()
}
