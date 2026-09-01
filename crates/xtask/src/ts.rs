//! Writes `packages/core/src/protocol.gen.ts` from the Rust wire types.
//!
//! The editor protocol is defined once, in `catchlight-editor-protocol`, and
//! TypeScript reads a translation of it. Hand-maintaining a second copy of
//! thirty-odd types across a boundary where no compiler checks either side
//! against the other is the failure this architecture invites: the drift is
//! silent until a command reaches the server with a field spelled the way last
//! month's Rust spelled it. So the declarations are generated, the result is
//! committed — a TypeScript build needs no Rust toolchain — and CI runs
//! `cargo xtask ts --check`, which fails if the file and the types disagree.
//!
//! Three things this module owns that the derive cannot:
//!
//! - **One file, no imports.** `ts-rs` exports a file per type with relative
//!   imports between them. A protocol is one thing and gets one module, so
//!   this asks each type for its declaration alone and concatenates them in
//!   the order [`declarations`] lists.
//!
//! - **Nothing may fall out of the list.** [`declarations`] is written by hand,
//!   so a new type on the wire can be left out of it. It cannot be left out
//!   silently: every listed type is asked what it depends on, and a dependency
//!   that is not itself listed fails the build naming it.
//!
//! - **Formatting.** `ts-rs` emits one long line per type. [`pretty`] breaks it
//!   on the structure, deterministically and with no JavaScript involved —
//!   the check compares bytes, so a formatter whose version differed between a
//!   developer's shell and CI would report drift that is not there.
//!
//! Two deliberate departures from what the derive would do on its own:
//!
//! - `u64` renders as `number`, not `ts-rs`'s default `bigint`. The wire is
//!   JSON and `JSON.parse` yields a `number`; a `bigint` declaration would
//!   describe a value that never arrives. Session ids and revisions are
//!   counters that will not reach 2^53.
//!
//! - [`Request`](catchlight_editor_protocol::Request) is not generated.
//!   `#[serde(flatten)]` inlines, so its declaration would restate the whole
//!   `Command` union a second time. The envelope is `{ id: number } & Command`
//!   in TypeScript, and building it is the transport's job — see
//!   `packages/core/src/transport.ts`.
//!
//! Building the protocol crate with `ts` on prints one `ts-rs` warning:
//! `SessionId`'s `#[serde(transparent)]` is an attribute it does not parse. It
//! does not need to — a newtype already serializes as its inner value, which is
//! what the generated `type SessionId = number` says. The attribute stays
//! because it is what the Rust type means; the warning is informational and
//! fails nothing.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use catchlight_editor_protocol as proto;
use ts_rs::{Config, Dependency, TS};

/// Where the generated module lands, relative to the workspace root.
const OUT: &str = "packages/core/src/protocol.gen.ts";

/// One type's contribution to the generated module.
struct Decl {
    ident: String,
    docs: Option<String>,
    body: String,
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

pub fn run(args: &[String]) -> Result<()> {
    let mut check = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            other => bail!("unexpected argument: {other}"),
        }
    }

    let cfg = Config::new().with_large_int("number");
    let decls = declarations(&cfg);
    let module = render(&decls)?;
    let out = crate::workspace_root()?.join(OUT);

    if check {
        let found = std::fs::read_to_string(&out)
            .with_context(|| format!("reading {OUT} to check it against the Rust types"))?;
        if found != module {
            bail!(
                "{OUT} is out of date with the Rust protocol types.\n\
                 Run `cargo xtask ts` and commit the result."
            );
        }
        eprintln!("{OUT} matches the Rust types");
        return Ok(());
    }

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &module).with_context(|| format!("writing {OUT}"))?;
    eprintln!("wrote {OUT} ({} types)", decls.len());
    Ok(())
}

/// Every type the wire carries, in the order the generated module declares
/// them: the Ids, then commands, then replies, then the records they name.
fn declarations(cfg: &Config) -> Vec<Decl> {
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
        proto::TexInfo,
        proto::ParamInfo,
        proto::SeamInfo,
        proto::SlotInfo,
        proto::WeldInfo,
        proto::PreviewInfo,
    ]
}

fn render(decls: &[Decl]) -> Result<String> {
    check_closed(decls)?;

    let mut out = String::from(HEADER);
    for decl in decls {
        out.push('\n');
        if let Some(docs) = &decl.docs {
            // `docs()` hands back the comment already wrapped in `/** … */`.
            push_comment(&mut out, docs.trim(), 0);
            out.push('\n');
        }
        out.push_str("export ");
        out.push_str(pretty(&decl.body).trim_end().trim_end_matches(';'));
        out.push_str(";\n");
    }
    Ok(out)
}

/// Fails if a listed type depends on a type that is not itself listed — the
/// one way this generator could quietly emit an incomplete module.
fn check_closed(decls: &[Decl]) -> Result<()> {
    let listed: BTreeSet<&str> = decls.iter().map(|d| d.ident.as_str()).collect();
    let missing: BTreeSet<&str> = decls
        .iter()
        .flat_map(|d| &d.deps)
        .map(|dep| dep.ts_name.as_str())
        .filter(|name| !listed.contains(name))
        .collect();
    if !missing.is_empty() {
        bail!(
            "the protocol names {} that `declarations` in crates/xtask/src/ts.rs \
             does not list: {}.\n\
             Add them there — a type left out is a type TypeScript cannot see.",
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

/// Breaks one `ts-rs` declaration across lines.
///
/// The grammar to lay out is small and known: object literals, unions,
/// intersections, tuples, `Array<…>`, and the doc comments the derive
/// interleaves between fields. Three rules cover it — a union arm gets its own
/// line, a member of an object gets its own line, and a doc comment keeps the
/// indentation of the member it introduces. Everything else stays where it is,
/// which is what keeps `[number, number]` and `string | null` on one line,
/// where they read best.
fn pretty(decl: &str) -> String {
    let Some(eq) = top_level(decl, '=').first().copied() else {
        return decl.to_string();
    };
    let (head, rhs) = decl.split_at(eq + 1);
    let head = head.trim_end();

    let arms = split_top_level(rhs, '|');
    if arms.len() == 1 {
        return format!("{head} {}", layout(rhs, 0));
    }
    // Idiomatic TypeScript for a union that does not fit on one line: every
    // arm on its own line behind a leading `|`, including the first.
    let mut out = String::from(head);
    for arm in arms {
        out.push_str("\n  | ");
        out.push_str(&layout(&arm, 1));
    }
    out
}

/// Byte offsets of every `sep` that sits outside brackets, quotes and comments.
fn top_level(s: &str, sep: char) -> Vec<usize> {
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => {
                for (_, s) in chars.by_ref() {
                    if s == '"' {
                        break;
                    }
                }
            }
            '/' if chars.peek().map(|(_, c)| *c) == Some('*') => {
                let mut last = ' ';
                for (_, s) in chars.by_ref() {
                    if last == '*' && s == '/' {
                        break;
                    }
                    last = s;
                }
            }
            '{' | '[' | '(' | '<' => depth += 1,
            '}' | ']' | ')' | '>' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => found.push(i),
            _ => {}
        }
    }
    found
}

fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut start = 0;
    for at in top_level(s, sep) {
        parts.push(s[start..at].to_string());
        start = at + sep.len_utf8();
    }
    parts.push(s[start..].to_string());
    parts
}

/// Lays out one type expression, indenting everything it opens from `base`.
fn layout(expr: &str, base: usize) -> String {
    let mut out = String::new();
    let mut stack: Vec<char> = Vec::new();
    let mut chars = expr.trim().chars().peekable();
    // Whether the last thing written was a line break, so runs of whitespace
    // leave neither blank lines nor trailing spaces.
    let mut at_line_start = false;

    while let Some(c) = chars.next() {
        let depth = base + stack.len();
        match c {
            '"' => {
                out.push('"');
                for s in chars.by_ref() {
                    out.push(s);
                    if s == '"' {
                        break;
                    }
                }
                at_line_start = false;
            }
            '/' if chars.peek() == Some(&'*') => {
                let mut comment = String::from('/');
                for s in chars.by_ref() {
                    comment.push(s);
                    if comment.ends_with("*/") {
                        break;
                    }
                }
                if !at_line_start {
                    break_line(&mut out, depth);
                }
                push_comment(&mut out, &comment, depth);
                break_line(&mut out, depth);
                at_line_start = true;
            }
            '{' | '[' | '(' | '<' => {
                stack.push(c);
                out.push(c);
                if c == '{' {
                    break_line(&mut out, base + stack.len());
                    at_line_start = true;
                } else {
                    at_line_start = false;
                }
            }
            '}' | ']' | ')' | '>' => {
                let was = stack.pop();
                if was == Some('{') {
                    break_line(&mut out, base + stack.len());
                }
                out.push(c);
                at_line_start = false;
            }
            ',' if stack.last() == Some(&'{') => {
                out.push(',');
                break_line(&mut out, depth);
                at_line_start = true;
            }
            c if c.is_whitespace() => {
                if !at_line_start && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            c => {
                out.push(c);
                at_line_start = false;
            }
        }
    }
    out
}

/// Writes a `/** … */` block at `depth`, with its continuation lines aligned
/// on the star the way a hand-written comment is.
fn push_comment(out: &mut String, comment: &str, depth: usize) {
    for (i, line) in comment.lines().enumerate() {
        if i > 0 {
            break_line(out, depth);
            out.push(' ');
        }
        let line = line.trim();
        // A `#[doc = "…"]` attribute — which is how the Id types get theirs —
        // arrives without the space a `///` comment would have left.
        match line.strip_prefix('*') {
            // Not `*/`, which closes the block and takes no text.
            Some(rest) if i > 0 && !rest.is_empty() && !rest.starts_with([' ', '/']) => {
                out.push_str("* ");
                out.push_str(rest);
            }
            _ => out.push_str(line),
        }
    }
}

fn break_line(out: &mut String, depth: usize) {
    trim_trailing(out);
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn trim_trailing(out: &mut String) {
    while out.ends_with(' ') || out.ends_with('\n') {
        out.pop();
    }
}

const HEADER: &str = "\
// Generated by `cargo xtask ts` from crates/catchlight-editor-protocol.
// Do not edit: run `cargo xtask ts` and commit the result. CI fails on drift.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_gives_every_object_member_its_own_line() {
        assert_eq!(
            pretty("type A = { a: string, b: number, };"),
            "type A = {\n  a: string,\n  b: number,\n};"
        );
    }

    #[test]
    fn pretty_puts_every_union_arm_behind_its_own_bar() {
        assert_eq!(
            pretty(r#"type A = { "k": "x", v: string | null, } | { "k": "y" };"#),
            "type A =\n  | {\n    \"k\": \"x\",\n    v: string | null,\n  }\n  | {\n    \"k\": \"y\"\n  };"
        );
    }

    #[test]
    fn pretty_keeps_a_tuple_and_an_inner_union_on_one_line() {
        assert_eq!(
            pretty("type A = { t: [number, number], v: string | null, };"),
            "type A = {\n  t: [number, number],\n  v: string | null,\n};"
        );
    }

    #[test]
    fn pretty_aligns_a_doc_comment_with_its_member() {
        assert_eq!(
            pretty("type A = { \n/**\n * hi\n */\na: string, };"),
            "type A = {\n  /**\n   * hi\n   */\n  a: string,\n};"
        );
    }

    #[test]
    fn pretty_does_not_split_a_bar_inside_a_string_literal() {
        assert_eq!(
            pretty(r#"type A = { mode: "a|b", };"#),
            "type A = {\n  mode: \"a|b\",\n};"
        );
    }

    #[test]
    fn check_closed_names_a_type_left_off_the_list() {
        let cfg = Config::new().with_large_int("number");
        let decls = decls![&cfg, proto::WeldInfo];
        let err = check_closed(&decls).expect_err("WeldInfo names SeamAddr and SlotWeight");
        let message = err.to_string();
        assert!(message.contains("SeamAddr"), "{message}");
        assert!(message.contains("SlotWeight"), "{message}");
    }

    #[test]
    fn the_generated_module_is_closed_over_its_dependencies() {
        let cfg = Config::new().with_large_int("number");
        check_closed(&declarations(&cfg)).expect("every wire type is listed");
    }
}
