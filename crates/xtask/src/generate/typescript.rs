//! Writes `packages/core/src/protocol.gen.ts` from the Rust wire types.
//!
//! Why the protocol is generated rather than hand-copied, and what both
//! targets share, is in [the parent module](super). Three things this one owns
//! that the `ts-rs` derive cannot:
//!
//! - **One file, no imports.** `ts-rs` exports a file per type with relative
//!   imports between them. A protocol is one thing and gets one module, so
//!   this asks each type for its declaration alone and concatenates them in
//!   the order [`declarations`](super::declarations) lists.
//!
//! - **The kind split.** `ts-rs` sees one `Command` union; a client needs it
//!   cut five ways by what applying a command does. [`kind_aliases`] renders
//!   [`KINDS`] as `Extract<…>` aliases, so picking the wrong send method does
//!   not typecheck.
//!
//! - **Formatting.** `ts-rs` emits one long line per type. [`pretty`] breaks it
//!   on the structure, deterministically and with no JavaScript involved —
//!   the check compares bytes, so a formatter whose version differed between a
//!   developer's shell and CI would report drift that is not there.

use anyhow::{Context, Result};

use super::{check_aliased, check_classified, command_tags, tags_of, Decl, KINDS, QUERY_UNION_DOC};

/// Where the generated module lands, relative to the workspace root.
const OUT: &str = "packages/core/src/protocol.gen.ts";

pub fn write(decls: &[Decl]) -> Result<()> {
    let module = render(decls)?;
    let out = crate::workspace_root()?.join(OUT);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &module).with_context(|| format!("writing {OUT}"))?;
    eprintln!("wrote {OUT} ({} types)", decls.len());
    Ok(())
}

fn render(decls: &[Decl]) -> Result<String> {
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
    out.push_str(&kind_aliases(&command_tags(decls)?)?);
    Ok(out)
}

/// Splits the `Command` union five ways, by what applying a command does.
fn kind_aliases(tags: &[String]) -> Result<String> {
    check_classified(tags)?;
    check_aliased(tags)?;

    let mut out = String::new();
    for kind in KINDS {
        out.push('\n');
        push_comment(&mut out, &block_comment(kind.doc), 0);
        out.push_str(&format!("\nexport type {}Tag =", kind.name));
        for arm in tags_of(kind.kind, tags) {
            out.push_str(&format!("\n  | \"{arm}\""));
        }
        out.push_str(&format!(
            ";\nexport type {name} = Extract<Command, {{ cmd: {name}Tag }}>;\n",
            name = kind.name
        ));
    }
    out.push('\n');
    push_comment(&mut out, &block_comment(QUERY_UNION_DOC), 0);
    out.push_str(
        "\nexport type QueryCommandTag = ReplicaQueryCommandTag | ServerQueryCommandTag;\n\
         export type QueryCommand = ReplicaQueryCommand | ServerQueryCommand;\n",
    );
    Ok(out)
}

/// Wraps plain doc lines in the `/** … */` block [`push_comment`] re-indents.
fn block_comment(lines: &[&str]) -> String {
    let mut out = String::from("/**");
    for line in lines {
        out.push_str("\n *");
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
    }
    out.push_str("\n */");
    out
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
// Generated by `cargo xtask generate typescript` from crates/catchlight-editor-protocol.
// Do not edit: run `cargo xtask generate` and commit the result. CI fails on drift.
";

#[cfg(test)]
mod tests {
    use super::super::{config, declarations};
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

    /// The drift check. A stale `protocol.gen.ts` is a wire-level bug, and
    /// `cargo test` is where one is expected to surface — a contributor who
    /// edits the Rust types and runs the suite is told then, not after pushing.
    /// CI runs this same test rather than a second mechanism.
    #[test]
    fn the_committed_module_matches_the_rust_types() {
        let module = render(&declarations(&config())).expect("the module renders");
        let out = crate::workspace_root().expect("a workspace root").join(OUT);
        let committed =
            std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("reading {OUT}: {e}"));
        assert_eq!(
            committed, module,
            "{OUT} is out of date with the Rust protocol types. \
             Run `cargo xtask generate` and commit the result.",
        );
    }

    /// A client routes by kind, so every kind has to reach TypeScript as its
    /// own alias. A `CommandKind` that `KINDS` forgot would leave its commands
    /// in no union at all, and nothing else would notice.
    #[test]
    fn every_kind_reaches_typescript_as_its_own_alias() {
        let module = render(&declarations(&config())).expect("the module renders");
        for kind in KINDS {
            assert!(
                module.contains(&format!("export type {}Tag =", kind.name)),
                "the generated module declares no {}",
                kind.name,
            );
        }
        assert!(module.contains("export type QueryCommand = "));
    }
}
