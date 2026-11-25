//! Writes the editor protocol out as TypeScript and Python.
//!
//! Generated files are committed and byte-compared by tests in each emitter,
//! so a stale file fails `cargo test`. `cargo xtask generate` with no target
//! writes both.
//!
//! `u64` renders as a plain number rather than `bigint`: the wire is JSON, and
//! session ids and revisions will not reach 2^53.
//!
//! `Request` is not generated: `#[serde(flatten)]` would restate the whole
//! `Command` union; the transport builds the envelope.
//!
//! The two `ts-rs` warnings about `#[serde(transparent)]` and
//! `deserialize_with` while building with `ts` are expected and harmless.

pub mod python;
pub mod typescript;

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use catchlight_editor_protocol as proto;
use ts_rs::{Config, Dependency, TS};

pub fn run(args: &[String]) -> Result<()> {
    let target = match args {
        [] => None,
        [one] => Some(one.as_str()),
        [_, other, ..] => bail!("unexpected argument: {other}"),
    };
    let cfg = config();
    let decls = declarations(&cfg);
    check_closed(&decls)?;
    match target {
        None => {
            typescript::write(&decls)?;
            python::write(&decls)?;
        }
        Some("typescript") => typescript::write(&decls)?,
        Some("python") => python::write(&decls)?,
        Some(other) => bail!("unknown generate target: {other} (want typescript or python)"),
    }
    Ok(())
}

fn config() -> Config {
    Config::new().with_large_int("number")
}

pub struct Decl {
    pub ident: String,
    pub docs: Option<String>,
    pub body: String,
    deps: Vec<Dependency>,
}

macro_rules! decls {
    ($cfg:expr, $($t:ty),* $(,)?) => {
        vec![$(Decl {
            ident: <$t as TS>::ident($cfg),
            docs: <$t as TS>::docs(),
            body: <$t as TS>::decl($cfg),
            deps: <$t as TS>::dependencies($cfg),
        }),*]
    };
}

pub fn declarations(cfg: &Config) -> Vec<Decl> {
    decls![
        cfg,
        proto::SessionId,
        proto::NodeId,
        proto::ParamId,
        proto::TexId,
        proto::SlotId,
        proto::Command,
        proto::NodeKindArg,
        proto::NodeKind,
        proto::MaskMode,
        proto::Interpolate,
        proto::BlendMode,
        proto::TextureEncoding,
        proto::TextureAlpha,
        proto::ScalarTarget,
        proto::BindingTarget,
        proto::NodePatch,
        proto::AutoMesh,
        proto::Rename,
        proto::EditOp,
        proto::EditHistoryEntry,
        proto::IndexRange,
        proto::BindingCellValue,
        proto::BindingCellWrite,
        proto::BindingCellRead,
        proto::BindingIdentity,
        proto::VertexWeight,
        proto::GeometryField,
        proto::GeometryNode,
        proto::ModelTextureHeader,
        proto::SessionSource,
        proto::ModelFormat,
        proto::SessionOrigin,
        proto::LimitInfo,
        proto::BindingParams,
        proto::ParamPose,
        proto::ImportTexture,
        proto::SlotAddr,
        proto::SlotPair,
        proto::Presence,
        proto::ThreadId,
        proto::ReviewTarget,
        proto::ReviewAuthor,
        proto::ReviewDelivery,
        proto::ThreadStatus,
        proto::ReviewComment,
        proto::ReviewImage,
        proto::Kept,
        proto::VersionInfo,
        proto::VersionViewing,
        proto::ReviewThread,
        proto::ReviewAgent,
        proto::ReviewIdentity,
        proto::ReviewHeld,
        proto::Review,
        proto::Camera,
        proto::ExtensionKey,
        proto::ExtensionSet,
        proto::ExtensionRef,
        proto::ExtensionValueInfo,
        proto::ExtensionInfo,
        proto::Reply,
        proto::ErrorCode,
        proto::ResponseBody,
        proto::Event,
        proto::SessionInfo,
        proto::StatusInfo,
        proto::TreeNode,
        proto::NodeInfo,
        proto::MeshInfo,
        proto::MaskInfo,
        proto::ResponseSource,
        proto::ResponseInput,
        proto::MotionResponse,
        proto::SpineInfo,
        proto::TexInfo,
        proto::ParamInfo,
        proto::BindingInfo,
        proto::SlotInfo,
        proto::WeldInfo,
        proto::PreviewInfo,
    ]
}

pub fn check_closed(decls: &[Decl]) -> Result<()> {
    let listed: BTreeSet<&str> = decls.iter().map(|d| d.ident.as_str()).collect();
    let missing: BTreeSet<&str> = decls
        .iter()
        .flat_map(|d| &d.deps)
        .map(|dep| dep.ts_name.as_str())
        .filter(|name| !listed.contains(name))
        .collect();
    if !missing.is_empty() {
        bail!(
            "the protocol names {} that `declarations` in crates/xtask/src/generate/mod.rs \
             does not list: {}.\n\
             Add them there — a type left out is a type no generated module can see.",
            if missing.len() == 1 {
                "a type"
            } else {
                "types"
            },
            missing.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

/// Read back from the rendered union so [`check_classified`] checks against
/// the real enum, not a second copy.
pub fn command_tags(decls: &[Decl]) -> Result<Vec<String>> {
    let command = decls
        .iter()
        .find(|d| d.ident == "Command")
        .context("the generated module has no `Command` type to split by kind")?;
    let mut tags = Vec::new();
    let mut rest = command.body.as_str();
    while let Some(at) = rest.find("\"cmd\": \"") {
        rest = &rest[at + 8..];
        let end = rest
            .find('"')
            .context("a `cmd` tag in the Command union is not terminated")?;
        tags.push(rest[..end].to_string());
        rest = &rest[end..];
    }
    if tags.is_empty() {
        bail!("the Command union carries no `cmd` tags; the tagging changed");
    }
    Ok(tags)
}

/// Holds the tags one emitter found against `COMMAND_KINDS`, both directions.
pub fn check_classified(tags: &[String]) -> Result<()> {
    let known: BTreeSet<&str> = proto::COMMAND_KINDS.iter().map(|(tag, _)| *tag).collect();
    let found: BTreeSet<&str> = tags.iter().map(String::as_str).collect();

    let unclassified: Vec<&str> = found.difference(&known).copied().collect();
    if !unclassified.is_empty() {
        bail!(
            "COMMAND_KINDS in crates/catchlight-editor-protocol/src/lib.rs does not \
             classify {}.\nAdd each one — a command a generated module cannot place \
             is a command a client cannot know how to send.",
            unclassified.join(", ")
        );
    }
    let stale: Vec<&str> = known.difference(&found).copied().collect();
    if !stale.is_empty() {
        bail!(
            "COMMAND_KINDS classifies {}, which the Command enum no longer carries.\n\
             Remove them.",
            stale.join(", ")
        );
    }
    Ok(())
}

/// Only one direction is checkable: commands without bytes are absent from
/// [`COMMAND_BYTES`](catchlight_editor_protocol::COMMAND_BYTES) on purpose.
pub fn check_byte_bearing(tags: &[String]) -> Result<()> {
    let found: BTreeSet<&str> = tags.iter().map(String::as_str).collect();
    let stale: Vec<&str> = proto::COMMAND_BYTES
        .iter()
        .map(|(tag, _)| *tag)
        .filter(|tag| !found.contains(tag))
        .collect();
    if !stale.is_empty() {
        bail!(
            "COMMAND_BYTES in crates/catchlight-editor-protocol/src/lib.rs declares bytes for \
             {}, which the Command enum does not carry.\nRemove them.",
            stale.join(", ")
        );
    }
    Ok(())
}

pub struct ByteRow {
    pub tag: &'static str,
    /// `(kind, name)` per attachment, `kind` being `"fixed"` or `"family"`.
    pub attachments: Vec<(&'static str, &'static str)>,
    pub payload: bool,
}

pub fn byte_rows() -> Vec<ByteRow> {
    proto::COMMAND_BYTES
        .iter()
        .map(|(tag, bytes)| ByteRow {
            tag,
            attachments: bytes
                .attachments
                .iter()
                .map(|a| match a {
                    proto::Attachment::Fixed(name) => ("fixed", *name),
                    proto::Attachment::Family(name) => ("family", *name),
                    proto::Attachment::Optional(name) => ("optional", *name),
                })
                .collect(),
            payload: bytes.payload,
        })
        .collect()
}

pub const BYTES_DOC: &[&str] = &[
    "What bytes each command carries, keyed by its `cmd` tag.",
    "",
    "A command absent from this table carries none. `attachments` names what",
    "travels in beside the command: a `fixed` name is one the command needs,",
    "an `optional` one is a name whose own fields decide whether it is there,",
    "and a `family` admits any number of `<name>:<suffix>` attachments.",
    "`payload` says the reply carries bytes back.",
];

pub fn tags_of(kind: proto::CommandKind, tags: &[String]) -> Vec<&str> {
    tags.iter()
        .map(String::as_str)
        .filter(|tag| {
            proto::COMMAND_KINDS
                .iter()
                .any(|(name, k)| name == tag && *k == kind)
        })
        .collect()
}

pub struct Kind {
    pub kind: proto::CommandKind,
    pub name: &'static str,
    pub doc: &'static [&'static str],
}

pub const KINDS: &[Kind] = &[
    Kind {
        kind: proto::CommandKind::Edit,
        name: "EditCommand",
        doc: &[
            "A command that changes a session's model, or which sessions exist.",
            "",
            "The session's revision moves, so every view of it re-reads. These are the",
            "commands that cost an undo entry and a React render, and the only ones",
            "that must reach the editor that owns the model.",
        ],
    },
    Kind {
        kind: proto::CommandKind::Presence,
        name: "PresenceCommand",
        doc: &[
            "A command that publishes shared view state: pose, camera, selection.",
            "",
            "It goes to the editor because other clients read it back, and it changes no",
            "model: no revision, no undo entry, invisible to a panel.",
        ],
    },
    Kind {
        kind: proto::CommandKind::Review,
        name: "ReviewCommand",
        doc: &[
            "A command that changes a session's review threads.",
            "",
            "Like presence it goes to the editor and changes no model: no revision, no",
            "undo entry. Other clients learn of it from `review_changed`.",
        ],
    },
    Kind {
        kind: proto::CommandKind::Version,
        name: "VersionCommand",
        doc: &[
            "A command that changes a session's saved versions.",
            "",
            "Like review it goes to the editor and changes no model: no revision, no",
            "undo entry. Other clients learn of it from `versions_changed`.",
        ],
    },
    Kind {
        kind: proto::CommandKind::ReplicaQuery,
        name: "ReplicaQueryCommand",
        doc: &[
            "A read that is a pure function of the model.",
            "",
            "A client holding a replica answers it locally, with no round trip. The",
            "editor answers it the same way, from the same bytes.",
        ],
    },
    Kind {
        kind: proto::CommandKind::ServerQuery,
        name: "ServerQueryCommand",
        doc: &[
            "A read that needs the editor itself: its session bookkeeping, its store or",
            "its renderer.",
            "",
            "A replica cannot answer one, so these always go over the wire.",
        ],
    },
];

/// `query` spans both read kinds: which side answers is a routing decision.
pub const QUERY_UNION_DOC: &[&str] = &[
    "A command that reads, whoever answers it.",
    "",
    "The union of the two query kinds, for a caller that only cares that nothing",
    "changed. A client routes on the halves; a caller sends either.",
];

pub fn check_aliased(tags: &[String]) -> Result<()> {
    let mut emitted: BTreeSet<&str> = BTreeSet::new();
    for Kind { kind, .. } in KINDS {
        let arms = tags_of(*kind, tags);
        if arms.is_empty() {
            bail!("no command is classified {kind:?}; the split would be empty");
        }
        emitted.extend(arms);
    }
    let found: BTreeSet<&str> = tags.iter().map(String::as_str).collect();
    let unaliased: Vec<&str> = found.difference(&emitted).copied().collect();
    if !unaliased.is_empty() {
        bail!(
            "KINDS in crates/xtask/src/generate/mod.rs emits no alias for {}.\n\
             Add the missing CommandKind there.",
            unaliased.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_closed_names_a_type_left_off_the_list() {
        let cfg = config();
        let decls = decls![&cfg, proto::WeldInfo];
        let err = check_closed(&decls).expect_err("WeldInfo names NodeId and SlotPair");
        let message = err.to_string();
        assert!(message.contains("NodeId"), "{message}");
        assert!(message.contains("SlotPair"), "{message}");
    }

    #[test]
    fn the_generated_modules_are_closed_over_their_dependencies() {
        check_closed(&declarations(&config())).expect("every wire type is listed");
    }

    #[test]
    fn every_command_is_classified_exactly_once() {
        let decls = declarations(&config());
        let tags = command_tags(&decls).expect("the Command union carries cmd tags");
        assert_eq!(
            tags.len(),
            proto::COMMAND_KINDS.len(),
            "COMMAND_KINDS and the Command enum disagree on how many commands there are",
        );
        // Duplicates could let a missing tag hide behind one.
        let unique: BTreeSet<&str> = proto::COMMAND_KINDS.iter().map(|(tag, _)| *tag).collect();
        assert_eq!(
            unique.len(),
            proto::COMMAND_KINDS.len(),
            "a tag is listed twice"
        );
        check_classified(&tags).expect("every command is classified");
        check_aliased(&tags).expect("every command lands in an alias");
        check_byte_bearing(&tags).expect("every byte-bearing tag is a command");
    }
}
