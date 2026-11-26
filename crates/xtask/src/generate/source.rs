//! The protocol's Rust source as parsed with `syn`: each wire type's item
//! and the serde attributes that shape it on the wire. Parsing starts at the
//! protocol crate's root and follows its `mod` declarations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use syn::{Attribute, Item, ItemFn};

pub struct Source {
    /// Every struct and enum, by its bare name.
    pub items: BTreeMap<String, Item>,
    /// Every free function, by name, for `#[serde(default = "…")]`.
    fns: BTreeMap<String, Vec<ItemFn>>,
}

impl Source {
    /// Parses the crate rooted at `lib` with its modules, then each file of
    /// `foreign` alone.
    pub fn read(lib: &Path, foreign: &[PathBuf]) -> Result<Self> {
        let mut source = Self {
            items: BTreeMap::new(),
            fns: BTreeMap::new(),
        };
        source.read_file(lib, true)?;
        for path in foreign {
            source.read_file(path, false)?;
        }
        Ok(source)
    }

    fn read_file(&mut self, path: &Path, follow: bool) -> Result<()> {
        let shown = path.display();
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {shown}"))?;
        let file = syn::parse_file(&text).with_context(|| format!("parsing {shown}"))?;
        // A `mod` in `lib.rs` or `mod.rs` sits beside it; one in `x.rs` sits
        // in `x/`.
        let dir = match (path.parent(), path.file_stem().and_then(|s| s.to_str())) {
            (Some(parent), Some("lib" | "mod" | "main")) => parent.to_path_buf(),
            (Some(parent), Some(stem)) => parent.join(stem),
            _ => bail!("{shown} is not a Rust source file"),
        };
        self.add(path, &dir, file.items, follow)
    }

    fn add(&mut self, file: &Path, dir: &Path, items: Vec<Item>, follow: bool) -> Result<()> {
        for item in items {
            let ident = match item {
                Item::Struct(ref s) => s.ident.to_string(),
                Item::Enum(ref e) => e.ident.to_string(),
                Item::Fn(f) => {
                    self.fns.entry(f.sig.ident.to_string()).or_default().push(f);
                    continue;
                }
                Item::Mod(m) if follow && !is_test(&m.attrs) => {
                    let name = m.ident.to_string();
                    if m.attrs.iter().any(|a| a.path().is_ident("path")) {
                        bail!(
                            "{}: `mod {name}` has a #[path], which the generators do not follow",
                            file.display()
                        );
                    }
                    match m.content {
                        Some((_, items)) => self.add(file, &dir.join(&name), items, follow)?,
                        None => {
                            let flat = dir.join(format!("{name}.rs"));
                            let path = if flat.exists() {
                                flat
                            } else {
                                dir.join(&name).join("mod.rs")
                            };
                            self.read_file(&path, follow)?;
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            if self.items.insert(ident.clone(), item).is_some() {
                bail!(
                    "{} declares a second `{ident}`; the generated modules name each wire \
                     type by its bare name, so it must be unique",
                    file.display()
                );
            }
        }
        Ok(())
    }

    /// The free function `name` a serde default points at.
    pub fn function(&self, name: &str) -> Result<&ItemFn> {
        match self.fns.get(name).map(Vec::as_slice) {
            Some([f]) => Ok(f),
            Some([_, _, ..]) => {
                bail!("the protocol source has more than one `{name}` to read a serde default from")
            }
            _ => bail!("the protocol source has no `{name}` to read a serde default from"),
        }
    }

    /// The `cmd` tag of each `Command` variant, in declaration order.
    pub fn command_tags(&self) -> Result<Vec<String>> {
        let Some(Item::Enum(command)) = self.items.get("Command") else {
            bail!("the protocol source has no `Command` enum to split by kind");
        };
        let attrs = serde_attrs(&command.attrs, &"`Command`")?;
        if attrs.tag.as_deref() != Some("cmd") {
            bail!("`Command` is not tagged by `cmd`; the tagging changed");
        }
        command
            .variants
            .iter()
            .map(|variant| variant_wire(variant, attrs.rename_all.as_deref()))
            .collect()
    }
}

fn is_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Ident>().is_ok_and(|cfg| cfg == "test")
    })
}

#[derive(Default)]
pub struct Serde {
    pub tag: Option<String>,
    pub rename: Option<String>,
    pub rename_all: Option<String>,
    pub transparent: bool,
    pub untagged: bool,
    pub flatten: bool,
    /// `Some(None)` is `#[serde(default)]`; `Some(Some(f))` is
    /// `#[serde(default = "f")]`.
    pub default: Option<Option<String>>,
}

/// `item` names what carries `attrs`, for the error on a key the generators
/// do not translate.
pub fn serde_attrs(attrs: &[Attribute], item: &dyn std::fmt::Display) -> Result<Serde> {
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
                    "{item} uses #[serde({key})], which the generated modules do not \
                     translate; teach serde_attrs in crates/xtask/src/generate/source.rs \
                     what it means"
                )));
            }
            Ok(())
        })
        .map_err(|e| anyhow!("reading a #[serde(…)] in the protocol source: {e}"))?;
    }
    Ok(found)
}

/// The name `variant` travels under.
pub fn variant_wire(variant: &syn::Variant, rename_all: Option<&str>) -> Result<String> {
    let attrs = serde_attrs(&variant.attrs, &format_args!("variant `{}`", variant.ident))?;
    Ok(match (attrs.rename, rename_all) {
        (Some(name), _) => name,
        (None, Some("snake_case")) => snake_case(&variant.ident.to_string()),
        (None, None) => variant.ident.to_string(),
        (None, Some(other)) => bail!("the generators do not know the rename rule `{other}`"),
    })
}

pub fn snake_case(name: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_follows_serdes_rule() {
        assert_eq!(snake_case("NodeAdd"), "node_add");
        assert_eq!(snake_case("Io"), "io");
        assert_eq!(snake_case("ResponseBody"), "response_body");
        assert_eq!(snake_case("UnfilledSlots"), "unfilled_slots");
    }

    #[test]
    fn a_serde_key_the_generators_do_not_translate_is_refused() {
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
    fn modules_are_followed_into_their_files() {
        let dir = std::env::temp_dir().join(format!("xtask-source-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "proto/lib.rs",
            "mod flat; mod inline { mod deep; } mod nested; #[cfg(test)] mod tests; \
             #[derive(Serialize)] #[serde(tag = \"cmd\", rename_all = \"snake_case\")] \
             enum Command { NodeAdd, #[serde(rename = \"other\")] Moved }",
        );
        write("proto/flat.rs", "struct Flat; mod below;");
        write("proto/flat/below.rs", "enum Below {}");
        write(
            "proto/inline/deep.rs",
            "struct Deep; fn ten() -> u32 { 10 }",
        );
        write("proto/nested/mod.rs", "struct Nested;");
        write("core/foreign.rs", "struct Foreign; mod ignored;");
        let source = Source::read(&dir.join("proto/lib.rs"), &[dir.join("core/foreign.rs")]);
        let _ = std::fs::remove_dir_all(&dir);
        let source = source.unwrap();
        assert_eq!(
            source.items.keys().collect::<Vec<_>>(),
            ["Below", "Command", "Deep", "Flat", "Foreign", "Nested"]
        );
        assert!(source.function("ten").is_ok());
        assert_eq!(source.command_tags().unwrap(), ["node_add", "other"]);
    }

    #[test]
    fn a_wire_type_declared_twice_is_refused() {
        let dir = std::env::temp_dir().join(format!("xtask-source-twice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.rs"), "struct A; mod b { struct A; }").unwrap();
        let read = Source::read(&dir.join("lib.rs"), &[]);
        let _ = std::fs::remove_dir_all(&dir);
        let error = read.err().expect("two `A`s are refused").to_string();
        assert!(error.contains("a second `A`"), "{error}");
    }
}
