//! Writes `packages/core/src/protocol.gen.ts` from the Rust wire types.
//!
//! Emits one import-free module and formats it with [`pretty`] rather than a
//! JavaScript formatter: the drift check compares bytes, so a formatter whose
//! version differs between shells and CI would report false drift.

use anyhow::{Context, Result};

use super::{BYTES_DOC, KINDS, Protocol, QUERY_UNION_DOC, byte_rows, tags_of};

const OUT: &str = "packages/core/src/protocol.gen.ts";

pub fn write(protocol: &Protocol) -> Result<()> {
    let module = render(protocol);
    let out = crate::workspace_root()?.join(OUT);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &module).with_context(|| format!("writing {OUT}"))?;
    eprintln!("wrote {OUT} ({} types)", protocol.decls.len());
    Ok(())
}

fn render(protocol: &Protocol) -> String {
    let mut out = String::from(HEADER);
    for decl in &protocol.decls {
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
    out.push_str(&kind_aliases(&protocol.tags));
    out.push_str(&command_bytes());
    out
}

/// A value, not a type: the tab looks it up at run time to decide what to
/// attach.
fn command_bytes() -> String {
    let mut out = String::from("\n");
    push_comment(&mut out, &block_comment(BYTES_DOC), 0);
    out.push_str(
        "\nexport const COMMAND_BYTES: Record<\n\
         \x20 string,\n\
         \x20 {\n\
         \x20   attachments: ReadonlyArray<{\n\
         \x20     kind: \"fixed\" | \"optional\" | \"family\";\n\
         \x20     name: string;\n\
         \x20   }>;\n\
         \x20   payload: boolean;\n\
         \x20 }\n\
         > = {\n",
    );
    for row in byte_rows() {
        out.push_str(&format!("  {}: {{\n    attachments: [", row.tag));
        for (i, (kind, name)) in row.attachments.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("\n      {{ kind: \"{kind}\", name: \"{name}\" }}"));
        }
        if !row.attachments.is_empty() {
            out.push_str(",\n    ");
        }
        out.push_str(&format!("],\n    payload: {},\n  }},\n", row.payload));
    }
    out.push_str("};\n");
    out
}

fn kind_aliases(tags: &[String]) -> String {
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
    out
}

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

/// Breaks a union arm or object member onto its own line; everything else
/// stays put so `[number, number]` and `string | null` remain inline.
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
    let mut out = String::from(head);
    for arm in arms {
        out.push_str("\n  | ");
        out.push_str(&layout(&arm, 1));
    }
    out
}

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

fn layout(expr: &str, base: usize) -> String {
    let mut out = String::new();
    let mut stack: Vec<char> = Vec::new();
    let mut chars = expr.trim().chars().peekable();
    // Avoids blank lines and trailing spaces from whitespace runs.
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

fn push_comment(out: &mut String, comment: &str, depth: usize) {
    for (i, line) in comment.lines().enumerate() {
        if i > 0 {
            break_line(out, depth);
            out.push(' ');
        }
        let line = line.trim();
        // `#[doc = "…"]` attributes arrive without the leading space `///` leaves.
        match line.strip_prefix('*') {
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
    fn the_committed_module_matches_the_rust_types() {
        let module = render(&Protocol::load().expect("the protocol loads"));
        let out = crate::workspace_root().expect("a workspace root").join(OUT);
        let committed =
            std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("reading {OUT}: {e}"));
        assert_eq!(
            committed, module,
            "{OUT} is out of date with the Rust protocol types. \
             Run `cargo xtask generate` and commit the result.",
        );
    }

    #[test]
    fn every_kind_reaches_typescript_as_its_own_alias() {
        let module = render(&Protocol::load().expect("the protocol loads"));
        for kind in KINDS {
            assert!(
                module.contains(&format!("export type {}Tag =", kind.name)),
                "the generated module declares no {}",
                kind.name,
            );
        }
        assert!(module.contains("export type QueryCommand = "));
    }

    #[test]
    fn the_byte_table_reaches_typescript_as_a_value() {
        let module = render(&Protocol::load().expect("the protocol loads"));
        assert!(module.contains("export const COMMAND_BYTES: Record<"));
        assert!(module.contains(r#"{ kind: "fixed", name: "texture" }"#));
        assert!(module.contains("payload: true"));
    }
}
