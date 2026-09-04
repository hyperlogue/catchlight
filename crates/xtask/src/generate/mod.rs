//! Writes the editor protocol out in the languages that speak it.
//!
//! The protocol is defined once, in `catchlight-editor-protocol`. Every other
//! language reads a translation of it, and hand-maintaining those translations
//! is the failure this architecture invites: thirty-odd types across a boundary
//! no compiler checks, where the drift is silent until a command reaches the
//! server with a field spelled the way last month's Rust spelled it. So each
//! translation is generated, committed — a TypeScript or Python build needs no
//! Rust toolchain — and byte-compared by a test in its own emitter module, so a
//! stale file fails `cargo test` locally and in CI.
//!
//! Two targets today: [`typescript`] writes `packages/core/src/protocol.gen.ts`
//! and [`python`] writes `python/catchlight/protocol_gen.py`. `cargo xtask
//! generate` with no target writes both.
//!
//! Three things live here rather than in either emitter, because a fact that
//! reached one language and not the other would be worse than one that reached
//! neither:
//!
//! - **One list of types.** [`declarations`] is the single hand-written roster
//!   of what the wire carries. Both emitters walk it, in its order, so a type
//!   cannot be in one module and missing from the other.
//!
//! - **Nothing may fall out of that list.** [`check_closed`] asks every listed
//!   type what it depends on and fails naming any dependency that is not itself
//!   listed. Hand-written lists rot; this one cannot rot quietly.
//!
//! - **No command reaches a language unclassified.** [`check_classified`] holds
//!   the tags a language found against
//!   [`COMMAND_KINDS`](catchlight_editor_protocol::COMMAND_KINDS), both ways, and
//!   [`KINDS`] is the one place a [`CommandKind`](proto::CommandKind) is given a
//!   name and an explanation. Both emitters render every entry of it, so a kind
//!   added in Rust and forgotten here fails the build rather than leaving its
//!   commands in no union at all.
//!
//! One deliberate departure from what the `ts-rs` derive would do on its own,
//! shared by both targets: `u64` renders as a plain number rather than
//! `ts-rs`'s default `bigint`. The wire is JSON and `JSON.parse` yields a
//! `number`; session ids and revisions are counters that will not reach 2^53.
//!
//! [`Request`](catchlight_editor_protocol::Request) is generated for no
//! language. `#[serde(flatten)]` inlines, so its declaration would restate the
//! whole `Command` union a second time. The envelope is the command's own
//! fields next to a correlation `id`, and building it is the transport's job.
//!
//! Building the protocol crate with `ts` on prints one `ts-rs` warning:
//! `SessionId`'s `#[serde(transparent)]` is an attribute it does not parse. It
//! does not need to — a newtype already serializes as its inner value, which is
//! what the generated `type SessionId = number` says. The attribute stays
//! because it is what the Rust type means; the warning is informational and
//! fails nothing.

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

/// The `ts-rs` configuration both targets read the Rust types through.
fn config() -> Config {
    Config::new().with_large_int("number")
}

/// One type's contribution to a generated module.
pub struct Decl {
    pub ident: String,
    pub docs: Option<String>,
    pub body: String,
    deps: Vec<Dependency>,
}

/// Collects each listed type's declaration, in the order given.
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

/// Every type the wire carries, in the order the generated modules declare
/// them: the Ids, then commands, then replies, then the records they name.
pub fn declarations(cfg: &Config) -> Vec<Decl> {
    decls![
        cfg,
        proto::SessionId,
        proto::NodeId,
        proto::ParamId,
        proto::TexId,
        proto::SeamId,
        proto::SlotId,
        proto::Command,
        proto::NodeKindArg,
        proto::NodePatch,
        proto::PhysicsTargets,
        proto::AutoMesh,
        proto::Rename,
        proto::BindingParams,
        proto::BindingKeyEntry,
        proto::ParamPose,
        proto::SeamAddr,
        proto::SlotAddr,
        proto::SeamSlot,
        proto::SlotWeight,
        proto::Presence,
        proto::Camera,
        proto::Reply,
        proto::ErrorCode,
        proto::ResponseBody,
        proto::Event,
        proto::SessionInfo,
        proto::StatusInfo,
        proto::TreeNode,
        proto::NodeInfo,
        proto::TexInfo,
        proto::ParamInfo,
        proto::BindingInfo,
        proto::SeamInfo,
        proto::SlotInfo,
        proto::WeldInfo,
        proto::PreviewInfo,
    ]
}

/// Fails if a listed type depends on a type that is not itself listed — the
/// one way this generator could quietly emit an incomplete module.
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

/// The `cmd` tags the generated `Command` union carries, in declaration order.
///
/// Read back out of the rendered union rather than listed again here: this is
/// what makes [`check_classified`] hold `COMMAND_KINDS` against the real enum
/// instead of against a second copy of it.
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
///
/// This is the whole reason `COMMAND_KINDS` exists. Neither TypeScript nor
/// Python can see that `scratch_deform` leaves the document alone and
/// `node_set` does not, or that `node_tree` is answerable from a local replica
/// and `status` is not, so a client that took one `Command` would have to
/// remember which of its calls are quiet and which can be served without a
/// round trip — exactly the thing nobody remembers.
///
/// Every tag a language sees must appear in `COMMAND_KINDS`, and every entry in
/// `COMMAND_KINDS` must be a tag it sees. Either half failing is a build error
/// naming the offenders: a new Rust command reaches a client classified, or it
/// does not reach it at all. Both emitters call this, so a command cannot be
/// classified for one language and unclassified for the other.
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

/// The tags of one kind, in the order the `Command` union declares them.
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

/// One split of the `Command` union, and what it means to a client.
pub struct Kind {
    pub kind: proto::CommandKind,
    /// The alias every language names this split by.
    pub name: &'static str,
    /// The explanation, as plain lines. Each emitter wraps them in whatever
    /// its own comment syntax is; this is the only place the text is written.
    pub doc: &'static [&'static str],
}

/// The five splits. Hand-listed, so a kind added to
/// [`CommandKind`](proto::CommandKind) and not to this table would classify
/// commands in Rust that reach no client alias at all — the same silent gap
/// [`check_classified`] closes, one level up. Both emitters fail on it.
pub const KINDS: &[Kind] = &[
    Kind {
        kind: proto::CommandKind::Document,
        name: "DocumentCommand",
        doc: &[
            "A command that changes the document, or which documents exist.",
            "",
            "The session's revision moves, so every view of it re-reads. These are the",
            "commands that cost an undo entry and a React render, and the only ones",
            "that must reach the editor that owns the document.",
        ],
    },
    Kind {
        kind: proto::CommandKind::Presence,
        name: "PresenceCommand",
        doc: &[
            "A command that publishes shared view state: pose, camera, selection.",
            "",
            "It goes to the editor because other clients read it back, and it changes no",
            "document: no revision, no undo entry, invisible to a panel.",
        ],
    },
    Kind {
        kind: proto::CommandKind::Scratch,
        name: "ScratchCommand",
        doc: &[
            "A command that shows a live edit on a puppet without authoring it.",
            "",
            "The drag path. Whoever owns the puppet being drawn serves it — a client",
            "with a local replica serves its own, and never asks the editor. A gesture",
            "of any length repaints the canvas and re-renders nothing.",
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

/// The one alias that is a union of two kinds rather than a kind of its own.
///
/// A read is a read: which side can answer it is a routing decision, not
/// something a caller picks a method by. So `query` takes both, and the two
/// halves stay separate for the router that has to tell them apart.
pub const QUERY_UNION_DOC: &[&str] = &[
    "A command that reads, whoever answers it.",
    "",
    "The union of the two query kinds, for a caller that only cares that nothing",
    "changed. A client routes on the halves; a caller sends either.",
];

/// Fails naming any command that [`KINDS`] left in no alias.
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
        let err = check_closed(&decls).expect_err("WeldInfo names SeamAddr and SlotWeight");
        let message = err.to_string();
        assert!(message.contains("SeamAddr"), "{message}");
        assert!(message.contains("SlotWeight"), "{message}");
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
        // `check_classified` is what enforces the set equality; this asserts
        // the list has no duplicate that would let a missing tag hide behind
        // one.
        let unique: BTreeSet<&str> = proto::COMMAND_KINDS.iter().map(|(tag, _)| *tag).collect();
        assert_eq!(
            unique.len(),
            proto::COMMAND_KINDS.len(),
            "a tag is listed twice"
        );
        check_classified(&tags).expect("every command is classified");
        check_aliased(&tags).expect("every command lands in an alias");
    }
}
