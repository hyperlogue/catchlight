//! Writes `python/catchlight/protocol_gen.py` from the Rust wire types.
//!
//! The structure comes from parsing the Rust source with `syn`, not from
//! `ts-rs` text, because Python must keep `u32` (`int`) and `f32` (`float`)
//! apart. Foreign Id types are taken from their `ts-rs` declaration and must
//! be plain string aliases.
//!
//! `Option<Option<T>>` becomes `T | Clear | None`: `to_wire()` drops `None`,
//! so the `CLEAR` singleton is the only way to write an explicit null.
//!
//! The output is stdlib-only and targets Python 3.11.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, bail, Context, Result};
use catchlight_editor_protocol as proto;
use syn::{Attribute, Expr, Fields, File, Item, Lit, Meta, Type};

use super::{
    byte_rows, check_aliased, check_byte_bearing, check_classified, tags_of, Decl, BYTES_DOC,
    KINDS, QUERY_UNION_DOC,
};

/// Not `protocol.gen.py`: a Python module name must be an identifier.
const OUT: &str = "python/catchlight/protocol_gen.py";

/// Parsed alongside the shared core response types. Unsupported shapes are a
/// build error naming the type.
const SOURCE: &str = "crates/catchlight-editor-protocol/src/lib.rs";

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

enum Shape {
    Alias(String),
    Choice(Vec<Choice>),
    Class(Vec<Field>),
    Union {
        tag: Option<String>,
        variants: Vec<Variant>,
    },
}

struct Wire {
    name: String,
    docs: Vec<String>,
    shape: Shape,
}

struct Choice {
    member: String,
    wire: String,
    docs: Vec<String>,
}

struct Variant {
    class: String,
    wire: String,
    docs: Vec<String>,
    fields: Vec<Field>,
    /// Decoded from the object carrying the tag rather than a key of their own.
    flatten: Vec<String>,
}

struct Field {
    /// `from` is a keyword, so it lands as `from_`.
    py: String,
    wire: String,
    ty: String,
    default: Option<Default_>,
    docs: Vec<String>,
    flatten: bool,
}

enum Default_ {
    Value(String),
    Factory(String),
}

struct Source {
    file: File,
    items: BTreeMap<String, Item>,
}

impl Source {
    fn read() -> Result<Self> {
        let path = crate::workspace_root()?.join(SOURCE);
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {SOURCE}"))?;
        let mut file = syn::parse_file(&text).with_context(|| format!("parsing {SOURCE}"))?;
        let path = crate::workspace_root()?.join("crates/catchlight-core/src/motion_response.rs");
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        file.items.extend(syn::parse_file(&source)?.items);
        let mut items = BTreeMap::new();
        for item in &file.items {
            let ident = match item {
                Item::Struct(s) => s.ident.to_string(),
                Item::Enum(e) => e.ident.to_string(),
                _ => continue,
            };
            items.insert(ident, item.clone());
        }
        Ok(Self { file, items })
    }

    fn literal_default(&self, name: &str) -> Result<String> {
        for item in &self.file.items {
            let Item::Fn(f) = item else { continue };
            if f.sig.ident != name {
                continue;
            }
            if let [syn::Stmt::Expr(expr, None)] = f.block.stmts.as_slice() {
                return py_literal(expr);
            }
            bail!("`{name}` is a serde default {OUT} cannot read: its body must be one literal");
        }
        bail!("{SOURCE} has no `{name}` to read a serde default from")
    }
}

#[derive(Default)]
struct Serde {
    tag: Option<String>,
    rename: Option<String>,
    rename_all: Option<String>,
    transparent: bool,
    untagged: bool,
    flatten: bool,
    /// `Some(None)` is `#[serde(default)]`; `Some(Some(f))` is
    /// `#[serde(default = "f")]`.
    default: Option<Option<String>>,
}

/// `item` names what carries `attrs`, for the error on a key this generator
/// does not translate.
fn serde_attrs(attrs: &[Attribute], item: &dyn std::fmt::Display) -> Result<Serde> {
    let mut found = Serde::default();
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("tag") {
                found.tag = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("rename") {
                found.rename = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("rename_all") {
                found.rename_all = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("untagged") {
                found.untagged = true;
            } else if meta.path.is_ident("transparent") {
                found.transparent = true;
            } else if meta.path.is_ident("flatten") {
                found.flatten = true;
            } else if meta.path.is_ident("default") {
                found.default = Some(if meta.input.peek(syn::Token![=]) {
                    Some(meta.value()?.parse::<syn::LitStr>()?.value())
                } else {
                    None
                });
            } else if meta.path.is_ident("deny_unknown_fields") {
            } else if meta.path.is_ident("skip_serializing_if")
                || meta.path.is_ident("deserialize_with")
            {
                let _: syn::LitStr = meta.value()?.parse()?;
            } else {
                let key = meta
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::");
                return Err(meta.error(format!(
                    "{item} uses #[serde({key})], which {OUT} does not translate; teach \
                     serde_attrs in crates/xtask/src/generate/python.rs what it means"
                )));
            }
            Ok(())
        })
        .map_err(|e| anyhow!("reading a #[serde(…)] in {SOURCE}: {e}"))?;
    }
    Ok(found)
}

fn doc_lines(attrs: &[Attribute]) -> Vec<String> {
    let mut lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(syn::ExprLit {
                    lit: Lit::Str(s), ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .map(|line| line.strip_prefix(' ').unwrap_or(&line).to_string())
        .collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

fn py_literal(expr: &Expr) -> Result<String> {
    let Expr::Lit(syn::ExprLit { lit, .. }) = expr else {
        bail!("a serde default in {SOURCE} is not a literal")
    };
    match lit {
        Lit::Bool(b) => Ok(if b.value { "True" } else { "False" }.to_string()),
        Lit::Int(i) => Ok(i.base10_digits().to_string()),
        Lit::Float(f) => Ok(f.base10_digits().to_string()),
        Lit::Str(s) => Ok(format!("{:?}", s.value())),
        other => {
            bail!("a serde default in {SOURCE} is a literal Python has no spelling for: {other:?}")
        }
    }
}

fn py_type(ty: &Type, known: &BTreeSet<String>) -> Result<String> {
    match ty {
        Type::Path(path) => {
            let last = path
                .path
                .segments
                .last()
                .context("an empty type path in the protocol")?;
            let ident = last.ident.to_string();
            let args = generic_args(&last.arguments);
            match ident.as_str() {
                "String" | "str" => Ok("str".into()),
                "bool" => Ok("bool".into()),
                "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32" | "i64" | "isize" => {
                    Ok("int".into())
                }
                "f32" | "f64" => Ok("float".into()),
                "Option" => {
                    let [inner] = args.as_slice() else {
                        bail!("an Option with no type argument in the protocol")
                    };
                    let inner = py_type(inner, known)?;
                    // Merge-patch idiom: `None` already spells absent, so null needs a word.
                    Ok(match inner.strip_suffix(" | None") {
                        Some(base) => format!("{base} | Clear | None"),
                        None => format!("{inner} | None"),
                    })
                }
                "Vec" => {
                    let [inner] = args.as_slice() else {
                        bail!("a Vec with no type argument in the protocol")
                    };
                    Ok(format!("list[{}]", py_type(inner, known)?))
                }
                "Box" => {
                    let [inner] = args.as_slice() else {
                        bail!("a Box with no type argument in the protocol")
                    };
                    py_type(inner, known)
                }
                // Extension JSON is opaque to Catchlight.
                "Value" => Ok("Any".into()),
                other if known.contains(other) => Ok(other.to_string()),
                other => bail!(
                    "the protocol names `{other}`, which `declarations` in \
                     crates/xtask/src/generate/mod.rs does not list.\n\
                     Add it there — a type left out is a type Python cannot see."
                ),
            }
        }
        Type::Array(array) => {
            let inner = py_type(&array.elem, known)?;
            let Expr::Lit(syn::ExprLit {
                lit: Lit::Int(len), ..
            }) = &array.len
            else {
                bail!("a fixed-size array in the protocol has a length Python cannot read")
            };
            let len: usize = len.base10_parse()?;
            let arms = vec![inner; len];
            Ok(format!("tuple[{}]", arms.join(", ")))
        }
        Type::Tuple(tuple) if !tuple.elems.is_empty() => {
            let arms = tuple
                .elems
                .iter()
                .map(|elem| py_type(elem, known))
                .collect::<Result<Vec<_>>>()?;
            Ok(format!("tuple[{}]", arms.join(", ")))
        }
        other => bail!("the protocol carries a type this generator cannot map: {other:?}"),
    }
}

fn generic_args(arguments: &syn::PathArguments) -> Vec<Type> {
    let syn::PathArguments::AngleBracketed(angled) = arguments else {
        return Vec::new();
    };
    angled
        .args
        .iter()
        .filter_map(|arg| match arg {
            syn::GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect()
}

fn build(decls: &[Decl], source: &Source) -> Result<Vec<Wire>> {
    let known: BTreeSet<String> = decls.iter().map(|d| d.ident.clone()).collect();
    let variant_names = name_variants(decls, source, &known)?;

    let mut wires = Vec::new();
    for decl in decls {
        let Some(item) = source.items.get(&decl.ident) else {
            let target = string_alias(decl)?;
            wires.push(Wire {
                name: decl.ident.clone(),
                docs: ts_docs(decl),
                shape: Shape::Alias(target),
            });
            continue;
        };
        let shape = match item {
            Item::Struct(s) => {
                let attrs = serde_attrs(&s.attrs, &format_args!("`{}`", s.ident))?;
                match &s.fields {
                    Fields::Unnamed(unnamed) if attrs.transparent => {
                        let [only] = unnamed.unnamed.iter().collect::<Vec<_>>()[..] else {
                            bail!("`{}` is transparent over more than one field", decl.ident)
                        };
                        Shape::Alias(py_type(&only.ty, &known)?)
                    }
                    Fields::Named(_) => Shape::Class(struct_fields(item, source, &known)?),
                    _ => bail!(
                        "`{}` is a tuple struct this generator cannot map; give it named \
                         fields or `#[serde(transparent)]`",
                        decl.ident
                    ),
                }
            }
            Item::Enum(e) => {
                let attrs = serde_attrs(&e.attrs, &format_args!("`{}`", e.ident))?;
                let rename_all = attrs.rename_all.as_deref();
                if e.variants.iter().all(|v| matches!(v.fields, Fields::Unit)) {
                    let mut choices = Vec::new();
                    for variant in &e.variants {
                        let wire = variant_wire(variant, rename_all)?;
                        choices.push(Choice {
                            member: wire.to_uppercase(),
                            wire,
                            docs: doc_lines(&variant.attrs),
                        });
                    }
                    Shape::Choice(choices)
                } else {
                    let tag = attrs.tag.clone();
                    let mut variants = Vec::new();
                    for variant in &e.variants {
                        let wire = variant_wire(variant, rename_all)?;
                        let class = variant_names
                            .get(&(decl.ident.clone(), variant.ident.to_string()))
                            .cloned()
                            .context("every variant was named")?;
                        let (fields, flatten) = if tag.is_none() && !attrs.untagged {
                            // Externally tagged newtypes are one-field JSON objects.
                            let Fields::Unnamed(unnamed) = &variant.fields else {
                                bail!("external enum `{}` needs newtype variants", decl.ident);
                            };
                            let [only] = unnamed.unnamed.iter().collect::<Vec<_>>()[..] else {
                                bail!("external enum `{}` needs one value per variant", decl.ident);
                            };
                            (
                                vec![Field {
                                    py: escape_keyword(&wire),
                                    wire: wire.clone(),
                                    ty: py_type(&only.ty, &known)?,
                                    default: None,
                                    docs: Vec::new(),
                                    flatten: false,
                                }],
                                Vec::new(),
                            )
                        } else {
                            variant_fields(&decl.ident, variant, source, &known)?
                        };
                        variants.push(Variant {
                            class,
                            wire,
                            docs: doc_lines(&variant.attrs),
                            fields,
                            flatten,
                        });
                    }
                    Shape::Union { tag, variants }
                }
            }
            _ => bail!("`{}` is neither a struct nor an enum", decl.ident),
        };
        wires.push(Wire {
            name: decl.ident.clone(),
            docs: item_docs(item),
            shape,
        });
    }
    Ok(wires)
}

/// Bare `Command` variant names unless taken on the wire; other unions prefix
/// with their own name. Fails rather than emit two classes under one name.
fn name_variants(
    decls: &[Decl],
    source: &Source,
    known: &BTreeSet<String>,
) -> Result<BTreeMap<(String, String), String>> {
    let mut taken: BTreeSet<String> = known.clone();
    let mut named = BTreeMap::new();
    for decl in decls {
        let Some(Item::Enum(e)) = source.items.get(&decl.ident) else {
            continue;
        };
        if e.variants.iter().all(|v| matches!(v.fields, Fields::Unit)) {
            continue;
        }
        for variant in &e.variants {
            let bare = variant.ident.to_string();
            let prefixed = format!("{}{bare}", decl.ident);
            let class = if decl.ident == "Command" && !taken.contains(&bare) {
                bare.clone()
            } else {
                prefixed
            };
            if !taken.insert(class.clone()) {
                bail!(
                    "`{}::{bare}` and something else both want the Python name `{class}`",
                    decl.ident
                );
            }
            named.insert((decl.ident.clone(), bare), class);
        }
    }
    Ok(named)
}

fn string_alias(decl: &Decl) -> Result<String> {
    let body = decl.body.trim().trim_end_matches(';');
    match body.rsplit_once('=').map(|(_, rhs)| rhs.trim()) {
        Some("string") => Ok("str".into()),
        _ => bail!(
            "`{}` is not declared in {SOURCE} and is not a plain string alias, so this \
             generator has no Python type for it. Declare it there or teach \
             crates/xtask/src/generate/python.rs what it is.",
            decl.ident
        ),
    }
}

fn item_docs(item: &Item) -> Vec<String> {
    match item {
        Item::Struct(s) => doc_lines(&s.attrs),
        Item::Enum(e) => doc_lines(&e.attrs),
        _ => Vec::new(),
    }
}

fn ts_docs(decl: &Decl) -> Vec<String> {
    let Some(docs) = &decl.docs else {
        return Vec::new();
    };
    docs.lines()
        .map(str::trim)
        .filter(|line| *line != "/**" && *line != "*/")
        .map(|line| line.trim_start_matches('*').trim_start().to_string())
        .collect()
}

fn variant_wire(variant: &syn::Variant, rename_all: Option<&str>) -> Result<String> {
    let attrs = serde_attrs(&variant.attrs, &format_args!("variant `{}`", variant.ident))?;
    Ok(match (attrs.rename, rename_all) {
        (Some(name), _) => name,
        (None, Some("snake_case")) => snake_case(&variant.ident.to_string()),
        (None, None) => variant.ident.to_string(),
        (None, Some(other)) => bail!("this generator does not know the rename rule `{other}`"),
    })
}

fn struct_fields(item: &Item, source: &Source, known: &BTreeSet<String>) -> Result<Vec<Field>> {
    let Item::Struct(s) = item else {
        bail!("expected a struct")
    };
    let Fields::Named(named) = &s.fields else {
        bail!("`{}` has no named fields", s.ident)
    };
    let mut fields = Vec::new();
    for field in &named.named {
        push_field(&mut fields, field, source, known)?;
    }
    Ok(fields)
}

fn variant_fields(
    enum_name: &str,
    variant: &syn::Variant,
    source: &Source,
    known: &BTreeSet<String>,
) -> Result<(Vec<Field>, Vec<String>)> {
    let mut fields = Vec::new();
    match &variant.fields {
        Fields::Unit => {}
        Fields::Named(named) => {
            for field in &named.named {
                push_field(&mut fields, field, source, known)?;
            }
        }
        Fields::Unnamed(unnamed) => {
            let [only] = unnamed.unnamed.iter().collect::<Vec<_>>()[..] else {
                bail!("`{enum_name}::{}` holds more than one value", variant.ident)
            };
            let ty = py_type(&only.ty, known)?;
            let inner = source.items.get(&ty);
            if !matches!(inner, Some(Item::Enum(_))) {
                bail!(
                    "`{enum_name}::{}` wraps `{ty}`, which is not a tagged enum; this \
                     generator flattens a newtype variant only into one of those",
                    variant.ident
                );
            }
            fields.push(Field {
                py: snake_case(&ty),
                wire: snake_case(&ty),
                ty,
                default: None,
                docs: Vec::new(),
                flatten: true,
            });
        }
    }
    let flatten = fields
        .iter()
        .filter(|f| f.flatten)
        .map(|f| f.py.clone())
        .collect();
    Ok((fields, flatten))
}

fn push_field(
    into: &mut Vec<Field>,
    field: &syn::Field,
    source: &Source,
    known: &BTreeSet<String>,
) -> Result<()> {
    let ident = field
        .ident
        .as_ref()
        .context("a named field with no name")?
        .to_string();
    let attrs = serde_attrs(&field.attrs, &format_args!("field `{ident}`"))?;

    if attrs.flatten {
        // `#[serde(flatten)]` splices fields so `to_wire()` stays one level deep.
        let ty = py_type(&field.ty, known)?;
        let Some(item @ Item::Struct(_)) = source.items.get(&ty) else {
            bail!("`{ident}` flattens `{ty}`, which is not a struct in {SOURCE}")
        };
        for spliced in struct_fields(item, source, known)? {
            if into.iter().any(|f| f.py == spliced.py) {
                bail!(
                    "flattening `{ty}` would give two fields named `{}`",
                    spliced.py
                );
            }
            into.push(spliced);
        }
        return Ok(());
    }

    let wire = attrs.rename.unwrap_or_else(|| ident.clone());
    let mut ty = py_type(&field.ty, known)?;
    let optional = ty.ends_with(" | None");
    let default = if optional {
        Some(Default_::Value("None".into()))
    } else {
        match &attrs.default {
            None => None,
            Some(Some(path)) => Some(Default_::Value(source.literal_default(path)?)),
            Some(None) => Some(match ty.as_str() {
                "bool" => Default_::Value("False".into()),
                "int" => Default_::Value("0".into()),
                "float" => Default_::Value("0.0".into()),
                "str" => Default_::Value("\"\"".into()),
                other if other.starts_with("list[") => Default_::Factory("list".into()),
                // Rust `Default` unreadable here; absent means the editor's default.
                _ => {
                    ty = format!("{ty} | None");
                    Default_::Value("None".into())
                }
            }),
        }
    };
    if into.iter().any(|f| f.wire == wire) {
        bail!("two fields would share the wire key `{wire}`");
    }
    into.push(Field {
        py: escape_keyword(&wire),
        wire,
        ty,
        default,
        docs: doc_lines(&field.attrs),
        flatten: false,
    });
    Ok(())
}

fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.char_indices() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

fn escape_keyword(name: &str) -> String {
    if KEYWORDS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

fn render(decls: &[Decl]) -> Result<String> {
    let source = Source::read()?;
    let wires = build(decls, &source)?;

    let tags = command_wire_tags(&wires)?;
    check_classified(&tags)?;
    check_aliased(&tags)?;
    check_byte_bearing(&tags)?;

    let mut out = String::from(HEADER);
    out.push_str(PREAMBLE);
    out.push_str(&render_kind_enum());
    out.push_str(ATTACHMENTS);
    out.push_str(RUNTIME);

    let mut exports: Vec<String> = vec![
        "Clear".into(),
        "CLEAR".into(),
        "CommandKind".into(),
        "AttachmentKind".into(),
        "Attachment".into(),
        "CommandBytes".into(),
    ];
    for wire in &wires {
        out.push_str(&render_wire(wire, &tags, &mut exports)?);
    }

    out.push_str("\n__all__ = [\n");
    for name in &exports {
        out.push_str(&format!("    {name:?},\n"));
    }
    out.push_str("]\n");
    Ok(out)
}

fn command_wire_tags(wires: &[Wire]) -> Result<Vec<String>> {
    let command = wires
        .iter()
        .find(|w| w.name == "Command")
        .context("the protocol has no `Command` type to split by kind")?;
    let Shape::Union { variants, .. } = &command.shape else {
        bail!("`Command` is not a tagged union")
    };
    Ok(variants.iter().map(|v| v.wire.clone()).collect())
}

fn render_kind_enum() -> String {
    let mut out = String::from(
        "\nclass CommandKind(StrEnum):\n\
         \x20   \"\"\"What applying a command does to the model it addresses.\n\n\
         \x20   This is what a client routes by: one send method per kind, so picking\n\
         \x20   the wrong one is a type error rather than a missing repaint.\n\
         \x20   \"\"\"\n\n",
    );
    for kind in KINDS {
        let value = snake_case(&format!("{:?}", kind.kind));
        for line in kind.doc {
            out.push_str(&comment_line(line, 4));
        }
        out.push_str(&format!("    {} = {value:?}\n", value.to_uppercase()));
        out.push('\n');
    }
    out.pop();
    out
}

fn render_wire(wire: &Wire, tags: &[String], exports: &mut Vec<String>) -> Result<String> {
    let mut out = String::new();
    match &wire.shape {
        Shape::Alias(target) => {
            out.push('\n');
            out.push_str(&comment_block(&wire.docs, 0));
            out.push_str(&format!("{} = {target}\n", wire.name));
            exports.push(wire.name.clone());
        }
        Shape::Choice(choices) => {
            out.push_str(&format!("\n\nclass {}(StrEnum):\n", wire.name));
            out.push_str(&docstring(&wire.docs, 4));
            for choice in choices {
                out.push_str(&comment_block(&choice.docs, 4));
                out.push_str(&format!("    {} = {:?}\n", choice.member, choice.wire));
            }
            exports.push(wire.name.clone());
        }
        Shape::Class(fields) => {
            out.push_str(&render_class(&wire.name, &wire.docs, fields, None, &[])?);
            exports.push(wire.name.clone());
        }
        Shape::Union { tag, variants } => {
            let is_command = wire.name == "Command";
            for variant in variants {
                let kind = if is_command {
                    Some(command_kind(&variant.wire)?)
                } else {
                    None
                };
                out.push_str(&render_class(
                    &variant.class,
                    &variant.docs,
                    &variant.fields,
                    tag.as_ref()
                        .map(|tag| (tag.as_str(), variant.wire.as_str(), kind)),
                    &variant.flatten,
                )?);
                exports.push(variant.class.clone());
            }
            out.push_str("\n\n");
            out.push_str(&docstring_or_comment(&wire.docs));
            out.push_str(&union_alias(
                &wire.name,
                &variants
                    .iter()
                    .map(|v| v.class.as_str())
                    .collect::<Vec<_>>(),
            ));
            exports.push(wire.name.clone());

            if let Some(tag) = tag {
                out.push_str(&format!(
                    "\n{}_VARIANTS: dict[str, type[{}]] = {{\n",
                    snake_case(&wire.name).to_uppercase(),
                    wire.name
                ));
                for variant in variants {
                    out.push_str(&format!("    {:?}: {},\n", variant.wire, variant.class));
                }
                out.push_str("}\n");
                exports.push(format!(
                    "{}_VARIANTS",
                    snake_case(&wire.name).to_uppercase()
                ));

                out.push_str(&render_parse(&wire.name, tag));
                exports.push(format!("parse_{}", snake_case(&wire.name)));
            }

            if is_command {
                out.push_str(&render_command_kinds(variants)?);
                exports.push("COMMAND_KINDS".into());
                out.push_str(&render_command_bytes());
                exports.push("COMMAND_BYTES".into());
                out.push_str(&render_kind_aliases(variants, tags)?);
                for kind in KINDS {
                    exports.push(kind.name.to_string());
                }
                exports.push("QueryCommand".into());
            }
        }
    }
    Ok(out)
}

fn command_kind(tag: &str) -> Result<&'static str> {
    let (_, kind) = proto::COMMAND_KINDS
        .iter()
        .find(|(name, _)| *name == tag)
        .with_context(|| format!("`{tag}` is not in COMMAND_KINDS"))?;
    KINDS
        .iter()
        .find(|k| k.kind == *kind)
        .map(|_| ())
        .with_context(|| format!("no alias for {kind:?}"))?;
    Ok(match kind {
        proto::CommandKind::Edit => "EDIT",
        proto::CommandKind::Presence => "PRESENCE",
        proto::CommandKind::Review => "REVIEW",
        proto::CommandKind::Version => "VERSION",
        proto::CommandKind::ReplicaQuery => "REPLICA_QUERY",
        proto::CommandKind::ServerQuery => "SERVER_QUERY",
    })
}

fn render_class(
    name: &str,
    docs: &[String],
    fields: &[Field],
    tag: Option<(&str, &str, Option<&'static str>)>,
    flatten: &[String],
) -> Result<String> {
    let mut out = format!("\n\n@dataclass(frozen=True, kw_only=True)\nclass {name}:\n");
    let mut body = String::new();
    body.push_str(&docstring(docs, 4));

    if let Some((tag_field, tag_value, kind)) = tag {
        body.push_str(&format!("    TAG_FIELD: ClassVar[str] = {tag_field:?}\n"));
        body.push_str(&format!("    TAG: ClassVar[str] = {tag_value:?}\n"));
        if let Some(kind) = kind {
            body.push_str("    CMD: ClassVar[str] = TAG\n");
            body.push_str(&format!(
                "    KIND: ClassVar[CommandKind] = CommandKind.{kind}\n"
            ));
        }
    }
    let renamed: Vec<&Field> = fields.iter().filter(|f| f.py != f.wire).collect();
    if !renamed.is_empty() {
        body.push_str("    WIRE: ClassVar[Mapping[str, str]] = {\n");
        for field in renamed {
            body.push_str(&format!("        {:?}: {:?},\n", field.py, field.wire));
        }
        body.push_str("    }\n");
    }
    if !flatten.is_empty() {
        let names: Vec<String> = flatten.iter().map(|n| format!("{n:?}")).collect();
        let trailing = if names.len() == 1 { "," } else { "" };
        body.push_str(&format!(
            "    FLATTEN: ClassVar[tuple[str, ...]] = ({}{trailing})\n",
            names.join(", ")
        ));
    }

    if !fields.is_empty() {
        if !body.is_empty() && !body.ends_with("\n\n") {
            body.push('\n');
        }
        for field in fields {
            body.push_str(&comment_block(&field.docs, 4));
            let default = match &field.default {
                None => String::new(),
                Some(Default_::Value(value)) => format!(" = {value}"),
                Some(Default_::Factory(call)) => format!(" = field(default_factory={call})"),
            };
            body.push_str(&format!("    {}: {}{default}\n", field.py, field.ty));
        }
    }

    if tag.is_some() {
        body.push_str(
            "\n    def to_wire(self) -> dict[str, Any]:\n\
             \x20       \"\"\"This value, as one JSON object: its tag, then every field it set.\"\"\"\n\
             \x20       return _wire_fields(self)\n",
        );
    }
    if body.trim().is_empty() {
        body.push_str("    pass\n");
    }
    out.push_str(&body);
    Ok(out)
}

fn union_alias(name: &str, arms: &[&str]) -> String {
    if arms.len() == 1 {
        return format!("{name} = {}\n", arms[0]);
    }
    let mut out = format!("{name} = (\n    {}\n", arms[0]);
    for arm in &arms[1..] {
        out.push_str(&format!("    | {arm}\n"));
    }
    out.push_str(")\n");
    out
}

fn render_parse(name: &str, tag: &str) -> String {
    let snake = snake_case(name);
    let registry = format!("{}_VARIANTS", snake.to_uppercase());
    format!(
        "\n\ndef parse_{snake}(message: Mapping[str, Any]) -> {name}:\n\
         \x20   \"\"\"One JSON object as the {name} it is, by its {tag:?} tag.\"\"\"\n\
         \x20   tag = message.get({tag:?})\n\
         \x20   found = {registry}.get(tag) if isinstance(tag, str) else None\n\
         \x20   if found is None:\n\
         \x20       raise ValueError(f\"no {name} carries the {tag} {{tag!r}}\")\n\
         \x20   return _decode_class(found, message)\n"
    )
}

fn render_command_kinds(variants: &[Variant]) -> Result<String> {
    let mut out = String::from(
        "\n\n# What each command does to the model, keyed by its `cmd` tag. Written\n\
         # down once in Rust, in `COMMAND_KINDS`; a command missing from it fails the\n\
         # build rather than reaching a client unclassified.\n\
         COMMAND_KINDS: dict[str, CommandKind] = {\n",
    );
    for variant in variants {
        out.push_str(&format!(
            "    {:?}: CommandKind.{},\n",
            variant.wire,
            command_kind(&variant.wire)?
        ));
    }
    out.push_str("}\n");
    Ok(out)
}

fn render_command_bytes() -> String {
    let mut out = String::from("\n\n");
    out.push_str(&comment_block(BYTES_DOC, 0));
    out.push_str("COMMAND_BYTES: dict[str, CommandBytes] = {\n");
    for row in byte_rows() {
        out.push_str(&format!("    {:?}: CommandBytes(\n        (", row.tag));
        for (kind, name) in &row.attachments {
            out.push_str(&format!(
                "\n            Attachment(AttachmentKind.{}, {name:?}),",
                kind.to_uppercase()
            ));
        }
        if !row.attachments.is_empty() {
            out.push_str("\n        ");
        }
        out.push_str(&format!(
            "),\n        {},\n    ),\n",
            if row.payload { "True" } else { "False" }
        ));
    }
    out.push_str("}\n");
    out
}

fn render_kind_aliases(variants: &[Variant], tags: &[String]) -> Result<String> {
    let class_of: BTreeMap<&str, &str> = variants
        .iter()
        .map(|v| (v.wire.as_str(), v.class.as_str()))
        .collect();
    let mut out = String::new();
    for kind in KINDS {
        let arms: Vec<&str> = tags_of(kind.kind, tags)
            .into_iter()
            .map(|tag| {
                class_of
                    .get(tag)
                    .copied()
                    .with_context(|| format!("no class for `{tag}`"))
            })
            .collect::<Result<_>>()?;
        out.push('\n');
        out.push_str(&comment_block(kind.doc, 0));
        out.push_str(&union_alias(kind.name, &arms));
    }
    out.push('\n');
    out.push_str(&comment_block(QUERY_UNION_DOC, 0));
    out.push_str("QueryCommand = ReplicaQueryCommand | ServerQueryCommand\n");
    Ok(out)
}

fn comment_line(line: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    if line.is_empty() {
        format!("{pad}#\n")
    } else {
        format!("{pad}# {line}\n")
    }
}

fn comment_block<S: AsRef<str>>(lines: &[S], indent: usize) -> String {
    lines
        .iter()
        .map(|line| comment_line(line.as_ref(), indent))
        .collect()
}

fn docstring(lines: &[String], indent: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let pad = " ".repeat(indent);
    let escaped: Vec<String> = lines
        .iter()
        .map(|line| line.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\""))
        .collect();
    if escaped.len() == 1 {
        return format!("{pad}\"\"\"{}\"\"\"\n\n", escaped[0]);
    }
    let mut out = format!("{pad}\"\"\"{}\n", escaped[0]);
    for line in &escaped[1..] {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("{pad}{line}\n"));
        }
    }
    out.push_str(&format!("{pad}\"\"\"\n\n"));
    out
}

fn docstring_or_comment(lines: &[String]) -> String {
    comment_block(lines, 0)
}

const HEADER: &str = "\
# Generated by `cargo xtask generate python` from crates/catchlight-editor-protocol.
# Do not edit: run `cargo xtask generate` and commit the result. CI fails on drift.
";

const PREAMBLE: &str = r#"
"""The catchlight editor protocol, as Python.

A client builds a command, calls `to_wire()` on it, and sends the result next
to a correlation `id` of its own. The editor answers with a line `parse_reply`
turns back into a `Reply`, or with an unsolicited `Event`.

Ids are plain strings — the same ones the model file stores — and a `SessionId`
is an int the editor allocates. Every command carries `CMD`, the tag it travels
under, and `KIND`, what applying it does to the model; a client routes by the
second. Nothing here is a transport: this module opens no socket and holds no
state.

Most optional fields are two-state: a value, or `None` for "leave the key off".
A few are three-state, annotated `T | Clear | None` — those spell "set this to
nothing" with the `CLEAR` singleton, which `to_wire()` writes as JSON null.
`NodePatch.texture` is one: `None` leaves the part drawing what it drew, `CLEAR`
makes it draw none, and a `TexId` points it at that texture.
"""

from __future__ import annotations

import dataclasses
from collections.abc import Mapping
from dataclasses import dataclass, field
from enum import StrEnum
from functools import lru_cache
from types import UnionType
from typing import Any, ClassVar, Union, get_args, get_origin, get_type_hints

"#;

/// Hand-written: these records have no wire shape of their own.
const ATTACHMENTS: &str = r#"

class AttachmentKind(StrEnum):
    """Whether an attachment name is exact or a family of them."""

    # Exactly this name, and the command needs it.
    FIXED = "fixed"
    # Exactly this name, and the command's own fields decide whether it is
    # there.
    OPTIONAL = "optional"
    # Any number of `<name>:<suffix>` attachments; the suffixes are the
    # command's own to say.
    FAMILY = "family"


@dataclass(frozen=True)
class Attachment:
    """One name a command's bytes travel under."""

    kind: AttachmentKind
    name: str


@dataclass(frozen=True)
class CommandBytes:
    """What one command carries in, and whether its reply carries bytes out."""

    attachments: tuple[Attachment, ...]
    payload: bool
"#;

const RUNTIME: &str = r#"

class Clear:
    """The one value that means "set this field to nothing".

    `None` on a field means the key is left off entirely, so a field that has
    to be able to say "nothing" as well as "unchanged" needs a third word for
    it. Fields annotated `T | Clear | None` take `CLEAR`, and `to_wire()`
    writes it as JSON null.
    """

    __slots__ = ()

    def __repr__(self) -> str:
        return "CLEAR"


CLEAR = Clear()


def _wire(value: Any) -> Any:
    """One value, as JSON carries it."""
    if isinstance(value, Clear):
        return None
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return _wire_fields(value)
    if isinstance(value, StrEnum):
        return value.value
    if isinstance(value, (list, tuple)):
        return [_wire(item) for item in value]
    return value


def _wire_fields(obj: Any) -> dict[str, Any]:
    """One dataclass, as JSON carries it: its tag, then every field it set.

    A field left `None` is left off entirely rather than sent as null. Almost
    every optional field on this wire has a serde default, so absent and null
    mean the same thing to the editor, and absent is the shorter of the two.
    A three-state field is the exception, and says its null with `CLEAR`.
    """
    cls = type(obj)
    out: dict[str, Any] = {}
    tag_field = getattr(cls, "TAG_FIELD", None)
    if tag_field is not None:
        out[tag_field] = cls.TAG
    names: Mapping[str, str] = getattr(cls, "WIRE", {})
    flat: tuple[str, ...] = getattr(cls, "FLATTEN", ())
    for spec in dataclasses.fields(obj):
        value = getattr(obj, spec.name)
        if value is None:
            continue
        if spec.name in flat:
            out.update(_wire(value))
            continue
        out[names.get(spec.name, spec.name)] = _wire(value)
    return out


@lru_cache(maxsize=None)
def _hints(cls: type) -> dict[str, Any]:
    return get_type_hints(cls)


def _decode(annotation: Any, value: Any) -> Any:
    """One value, as the annotation says to read it."""
    origin = get_origin(annotation)
    if origin is UnionType or origin is Union:
        if value is None:
            # Only a three-state field has a null to tell from an absent key.
            return CLEAR if Clear in get_args(annotation) else None
        arms = [
            arm
            for arm in get_args(annotation)
            if arm is not type(None) and arm is not Clear
        ]
        if len(arms) == 1:
            return _decode(arms[0], value)
        for arm in arms:
            tag_field = getattr(arm, "TAG_FIELD", None)
            if (
                tag_field is not None
                and isinstance(value, Mapping)
                and value.get(tag_field) == arm.TAG
            ):
                return _decode_class(arm, value)
        # Untagged structs and externally tagged newtypes have disjoint
        # required wire fields. Decode in declaration order like serde.
        if isinstance(value, Mapping):
            for arm in arms:
                if dataclasses.is_dataclass(arm) and getattr(arm, "TAG_FIELD", None) is None:
                    try:
                        return _decode_class(arm, value)
                    except (ValueError, TypeError):
                        continue
        raise ValueError(f"no variant of {annotation} matches {value!r}")
    if origin is list:
        (arm,) = get_args(annotation)
        return [_decode(arm, item) for item in value]
    if origin is tuple:
        arms = get_args(annotation)
        return tuple(_decode(arm, item) for arm, item in zip(arms, value, strict=True))
    if isinstance(annotation, type):
        if issubclass(annotation, StrEnum):
            return annotation(value)
        if dataclasses.is_dataclass(annotation):
            return _decode_class(annotation, value)
    return value


def _decode_class(cls: Any, data: Mapping[str, Any]) -> Any:
    """One JSON object as an instance of `cls`.

    A key that is absent or null falls back to the field's default — except on
    a three-state field, where a null present in `data` is `CLEAR` rather than
    the default. A field with no default is an error naming the key, because a
    reply that lost one is not a reply this client can act on. A field named in
    `FLATTEN` is read from `data` itself, which is where an internally tagged
    newtype variant puts it.
    """
    hints = _hints(cls)
    names: Mapping[str, str] = getattr(cls, "WIRE", {})
    flat: tuple[str, ...] = getattr(cls, "FLATTEN", ())
    kwargs: dict[str, Any] = {}
    for spec in dataclasses.fields(cls):
        if spec.name in flat:
            kwargs[spec.name] = _decode(hints[spec.name], data)
            continue
        key = names.get(spec.name, spec.name)
        present = data.get(key)
        if present is not None:
            kwargs[spec.name] = _decode(hints[spec.name], present)
        elif key in data and Clear in get_args(hints[spec.name]):
            kwargs[spec.name] = CLEAR
        elif (
            spec.default is dataclasses.MISSING
            and spec.default_factory is dataclasses.MISSING
        ):
            raise ValueError(f"{cls.__name__} needs {key!r}")
    return cls(**kwargs)

"#;

#[cfg(test)]
mod tests {
    use super::super::{config, declarations};
    use super::*;

    #[test]
    fn snake_case_follows_serdes_rule() {
        assert_eq!(snake_case("NodeAdd"), "node_add");
        assert_eq!(snake_case("Io"), "io");
        assert_eq!(snake_case("ResponseBody"), "response_body");
        assert_eq!(snake_case("UnfilledSlots"), "unfilled_slots");
    }

    #[test]
    fn a_serde_key_the_generator_does_not_translate_is_refused() {
        for (attr, key) in [
            ("#[serde(skip)]", "skip"),
            ("#[serde(alias = \"b\")]", "alias"),
            ("#[serde(with = \"m\")]", "with"),
        ] {
            let item: syn::ItemStruct =
                syn::parse_str(&format!("struct S {{ {attr} a: u32 }}")).unwrap();
            let field = item.fields.iter().next().unwrap();
            let Err(error) = serde_attrs(&field.attrs, &"field `a`") else {
                panic!("{attr} is refused");
            };
            let error = error.to_string();
            assert!(
                error.contains("field `a`") && error.contains(&format!("#[serde({key})]")),
                "{error}"
            );
        }
        let item: syn::ItemStruct = syn::parse_str(
            "#[serde(tag = \"t\", rename_all = \"snake_case\", deny_unknown_fields)] struct S { \
             #[serde(default, deserialize_with = \"f\", skip_serializing_if = \"g\")] a: u32 }",
        )
        .unwrap();
        assert!(serde_attrs(&item.attrs, &"`S`").is_ok());
        let field = item.fields.iter().next().unwrap();
        assert!(serde_attrs(&field.attrs, &"field `a`").is_ok());
    }

    #[test]
    fn a_python_keyword_is_not_a_field_name() {
        assert_eq!(escape_keyword("from"), "from_");
        assert_eq!(escape_keyword("to"), "to");
    }

    #[test]
    fn a_colliding_command_variant_is_named_by_its_enum() {
        let source = Source::read().expect("the protocol source parses");
        let decls = declarations(&config());
        let known: BTreeSet<String> = decls.iter().map(|d| d.ident.clone()).collect();
        let named = name_variants(&decls, &source, &known).expect("every variant is named");
        assert_eq!(
            named
                .get(&("Command".into(), "NodeInfo".into()))
                .map(String::as_str),
            Some("CommandNodeInfo")
        );
        assert_eq!(
            named
                .get(&("Command".into(), "NodeAdd".into()))
                .map(String::as_str),
            Some("NodeAdd")
        );
        assert_eq!(
            named
                .get(&("Reply".into(), "Ok".into()))
                .map(String::as_str),
            Some("ReplyOk")
        );
    }

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

    #[test]
    fn every_kind_reaches_python_as_its_own_alias() {
        let module = render(&declarations(&config())).expect("the module renders");
        for kind in KINDS {
            assert!(
                module.contains(&format!("\n{} = ", kind.name)),
                "the generated module declares no {}",
                kind.name,
            );
        }
        assert!(module.contains("QueryCommand = ReplicaQueryCommand | ServerQueryCommand"));
        assert!(module.contains("COMMAND_KINDS: dict[str, CommandKind]"));
    }

    #[test]
    fn a_count_is_an_int_and_a_measurement_is_a_float() {
        let module = render(&declarations(&config())).expect("the module renders");
        assert!(module.contains("    node_count: int\n"), "node_count");
        assert!(module.contains("    height: float\n"), "Camera.height");
        assert!(
            module.contains("    cell: tuple[int, int]\n"),
            "a binding cell is a pair of indices",
        );
    }
}

/// Holds the hand-written shapes in `python/tests/test_protocol.py` against
/// real `serde` output, so a self-consistent generator bug still fails.
///
/// [`without_nulls`] removes the one licensed difference: serde writes
/// `"name": null` where `to_wire()` omits the key. Merge-patch nulls are values,
/// so [`a_merge_patch_field_writes_absent_null_and_a_value`] reads raw JSON.
#[cfg(test)]
mod wire_shapes {
    use catchlight_editor_protocol::{
        AutoMesh, BindingCellValue, BindingIdentity, Camera, Command, MotionResponse, NodeKindArg,
        NodePatch, ParamPose, Presence, Rename, ResponseInput, ResponseSource, SessionId, TexId,
    };
    use serde_json::{json, Value};

    fn id<T: std::str::FromStr>(s: &str) -> T
    where
        T::Err: std::fmt::Debug,
    {
        s.parse().expect("a valid id")
    }

    /// List elements are left alone: there, null is a value.
    fn without_nulls(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, v)| (k, without_nulls(v)))
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
            other => other,
        }
    }

    #[track_caller]
    fn assert_wire(command: Command, expected: Value) {
        let written = serde_json::to_value(&command).expect("a command serializes");
        assert_eq!(without_nulls(written), expected);
    }

    #[test]
    fn one_command_of_each_kind() {
        assert_wire(
            Command::NodeAdd {
                session: SessionId(1),
                parent: id("root"),
                kind: NodeKindArg::Part,
                name: Some("Body".into()),
                node: None,
            },
            json!({"cmd": "node_add", "session": 1, "parent": "root", "kind": "part", "name": "Body"}),
        );
        assert_wire(
            Command::PresenceSet {
                session: SessionId(2),
                presence: Presence {
                    pose: vec![ParamPose {
                        param: id("head.x"),
                        value: 0.25,
                    }],
                    camera: Some(Camera {
                        center: [0.0, 1.0],
                        height: 2.0,
                    }),
                    selection: None,
                },
            },
            json!({
                "cmd": "presence_set",
                "session": 2,
                "pose": [{"param": "head.x", "value": 0.25}],
                "camera": {"center": [0.0, 1.0], "height": 2.0},
            }),
        );
        assert_wire(
            Command::BindingList {
                session: SessionId(4),
                node: id("root/part-1"),
            },
            json!({"cmd": "binding_list", "session": 4, "node": "root/part-1"}),
        );
        assert_wire(
            Command::Status {
                session: SessionId(5),
            },
            json!({"cmd": "session_get", "session": 5}),
        );
    }

    #[test]
    fn exact_cell_values_and_compact_identities_match_the_wire() {
        assert_eq!(
            serde_json::to_value(BindingCellValue::Scalar(2.0)).unwrap(),
            json!({"scalar": 2.0})
        );
        assert_eq!(
            serde_json::to_value(BindingCellValue::Offsets(vec![[1.0, -2.0]])).unwrap(),
            json!({"offsets": [[1.0, -2.0]]})
        );
        assert_eq!(
            serde_json::to_value(BindingIdentity::Scalar { scalar: 1.0 }).unwrap(),
            json!({"scalar": 1.0})
        );
        assert_eq!(
            serde_json::to_value(BindingIdentity::Deform {
                offset: [0.0, 0.0],
                vertex_count: 3
            })
            .unwrap(),
            json!({"offset": [0.0, 0.0], "vertex_count": 3})
        );
    }

    #[test]
    fn a_flattened_struct_is_flat_on_the_wire() {
        assert_wire(
            Command::NodeSet {
                session: SessionId(1),
                node: id("hair"),
                patch: NodePatch {
                    opacity: Some(0.5),
                    texture: Some(Some(id::<TexId>("tex-1"))),
                    ..NodePatch::default()
                },
            },
            json!({"cmd": "node_set", "session": 1, "node": "hair", "opacity": 0.5, "texture": "tex-1"}),
        );
    }

    #[test]
    fn a_merge_patch_field_writes_absent_null_and_a_value() {
        let set = |texture| Command::NodeSet {
            session: SessionId(1),
            node: id("hair"),
            patch: NodePatch {
                texture,
                ..NodePatch::default()
            },
        };
        let written = |command| serde_json::to_value(&command).expect("a command serializes");
        let key = |command| match written(command) {
            Value::Object(map) => map.get("texture").cloned(),
            other => panic!("a command is an object, not {other}"),
        };

        assert_eq!(key(set(None)), None, "absent leaves the key off");
        assert_eq!(key(set(Some(None))), Some(Value::Null), "null draws none");
        assert_eq!(
            key(set(Some(Some(id::<TexId>("tex-1"))))),
            Some(Value::String("tex-1".into())),
            "an Id draws that one",
        );
    }

    #[test]
    fn a_python_keyword_still_travels_under_its_real_name() {
        assert_wire(
            Command::RenameId {
                session: SessionId(1),
                rename: Rename::Param {
                    from: id("old"),
                    to: id("new"),
                },
            },
            json!({"cmd": "id_rename", "session": 1, "rename": {"kind": "param", "from": "old", "to": "new"}}),
        );
    }

    #[test]
    fn a_nested_tagged_enum_carries_its_own_tag() {
        assert_wire(
            Command::MeshAuto {
                session: SessionId(1),
                node: id("hair"),
                mode: AutoMesh::Grid {
                    threshold: None,
                    cols: Some(4),
                    rows: Some(3),
                    axes_x: None,
                    axes_y: None,
                    margin: None,
                },
            },
            json!({"cmd": "mesh_generate", "session": 1, "node": "hair", "mode": {"mode": "grid", "cols": 4, "rows": 3}}),
        );
    }

    #[test]
    fn a_mesh_travels_as_lists_of_points() {
        assert_wire(
            Command::MeshSet {
                deform_mapping: None,
                if_rev: None,
                session: SessionId(1),
                node: id("hair"),
                verts: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                uvs: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                indices: vec![[0, 1, 2]],
                origin: [0.0, 0.0],
            },
            json!({
                "cmd": "mesh_set",
                "session": 1,
                "node": "hair",
                "verts": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                "indices": [[0, 1, 2]],
                "origin": [0.0, 0.0],
            }),
        );
    }

    #[test]
    fn a_response_carries_nested_sources() {
        assert_wire(
            Command::MotionResponseSet {
                session: SessionId(1),
                node: id("tip"),
                response: MotionResponse {
                    inputs: vec![ResponseInput {
                        source: ResponseSource::Parent,
                        gain: 0.75,
                    }],
                    period_s: 0.5,
                    damping_ratio: 1.0,
                    output_param: Some(id("bend")),
                },
            },
            json!({"cmd":"motion_response_set","session":1,"node":"tip","response":{
                "inputs":[{"source":{"kind":"parent"},"gain":0.75}],"period_s":0.5,
                "damping_ratio":1.0,"output_param":"bend"
            }}),
        );
    }
}
