//! `extract`, `merge` and `requirements` — the addon operations, at the file.
//!
//! [`scan`] and [`Model::requirements`](catchlight_core::Model::requirements)
//! walk the same fields and must agree (`requirements_agree_with_the_model`).

use std::path::Path;

use catchlight_core::extension::IdRef;
use catchlight_core::formats::clm::{ClmExtension, ClmFile, ClmNodeKind, ClmStructure};
use catchlight_core::id::NodeId;
use catchlight_core::{Required, Requirement};

use crate::{file, Error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub roots: Vec<String>,
    pub nodes: usize,
    pub textures: usize,
    pub requirements: usize,
}

impl std::fmt::Display for Extracted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "extracted {} node(s) under {} root(s) with {} texture(s); {} requirement(s)",
            self.nodes,
            self.roots.len(),
            self.textures,
            self.requirements
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub nodes: usize,
    pub roots: Vec<String>,
    pub textures: usize,
    pub bindings: usize,
    pub welds: usize,
    /// The keys of the extensions the addon's data merged into.
    pub extensions: Vec<String>,
}

impl std::fmt::Display for Merged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "installed {} node(s), {} texture(s), {} binding(s), {} weld(s)",
            self.nodes, self.textures, self.bindings, self.welds
        )?;
        if !self.extensions.is_empty() {
            write!(f, "; merged extension(s) {}", self.extensions.join(", "))?;
        }
        Ok(())
    }
}

pub fn extract(path: &Path, ids: &[String], out: &Path) -> Result<Extracted, Error> {
    let clm = file::read(path)?;
    let roots = resolve_roots(&clm.doc, path, ids)?;
    let (model, _) = file::load(&clm, path)?;

    let addon = model.extract(&roots);
    file::write_model(&addon, out)?;

    Ok(Extracted {
        roots: addon.roots().iter().map(ToString::to_string).collect(),
        nodes: addon.node_count(),
        textures: addon.texture_ids().len(),
        requirements: addon.requirements().len(),
    })
}

fn resolve_roots(doc: &ClmStructure, path: &Path, ids: &[String]) -> Result<Vec<NodeId>, Error> {
    let mut roots = Vec::with_capacity(ids.len());
    for id in ids {
        let parsed = NodeId::new(id).map_err(|source| Error::BadId {
            value: id.clone(),
            source,
        })?;
        let Some(node) = doc.nodes.iter().find(|n| n.id == parsed) else {
            return Err(Error::NoSuchId {
                path: path.to_path_buf(),
                kind: "node",
                id: id.clone(),
            });
        };
        if node.parent.is_none() {
            return Err(Error::ExtractingARoot { node: id.clone() });
        }
        roots.push(parsed);
    }
    Ok(roots)
}

pub fn merge(base: &Path, addon: &Path, out: &Path) -> Result<Merged, Error> {
    let base_clm = file::read(base)?;
    let (mut model, _) = file::load(&base_clm, base)?;
    let addon_clm = file::read(addon)?;
    let addon_model = file::load_fragment(&addon_clm, addon)?;

    let installed = model.install(&addon_model)?;
    let merged = Merged {
        nodes: installed.nodes().len(),
        roots: installed.roots().iter().map(ToString::to_string).collect(),
        textures: installed.textures().len(),
        bindings: installed.bindings().len(),
        welds: installed.welds().len(),
        extensions: installed.extensions().map(ToString::to_string).collect(),
    };
    file::write_model(&model, out)?;
    Ok(merged)
}

pub fn requirements(path: &Path) -> Result<Vec<Requirement>, Error> {
    Ok(scan(&file::read(path)?))
}

/// Reads the wire directly so it works on files no reader accepts. Textures
/// are never requirements: an addon carries the textures its own parts draw.
pub fn scan(clm: &ClmFile) -> Vec<Requirement> {
    let doc = &clm.doc;
    let mut out: Vec<Requirement> = Vec::new();
    let mut need = |id: Required, field: &'static str, owner: String| {
        out.push(Requirement { id, field, owner });
    };

    let has_node = |id: &NodeId| doc.nodes.iter().any(|n| &n.id == id);
    let has_param = |id: &catchlight_core::id::ParamId| doc.params.iter().any(|p| &p.id == id);

    for node in &doc.nodes {
        let owner = node.id.to_string();
        if let Some(parent) = &node.parent {
            if !has_node(parent) {
                need(Required::Node(parent.clone()), "parent", owner.clone());
            }
        }
        let masks = match &node.kind {
            ClmNodeKind::Part(p) => p.masks.as_slice(),
            ClmNodeKind::Composite(c) => c.masks.as_slice(),
            ClmNodeKind::MotionResponse(r) => {
                for p in
                    r.output_param
                        .iter()
                        .chain(r.inputs.iter().filter_map(|t| match &t.source {
                            catchlight_core::ResponseSource::PoseParam { param: p } => Some(p),
                            _ => None,
                        }))
                {
                    if !has_param(p) {
                        need(
                            Required::Param(p.clone()),
                            "motion response param",
                            owner.clone(),
                        );
                    }
                }
                for t in &r.inputs {
                    if let catchlight_core::ResponseSource::PartSlot { part, slot, .. } = &t.source
                    {
                        if !has_node(part) {
                            need(
                                Required::Slot(part.clone(), slot.clone()),
                                "motion response slot",
                                owner.clone(),
                            );
                        }
                    }
                }
                &[]
            }
            ClmNodeKind::Spine(sp) => {
                for target in sp.targets.iter().flatten() {
                    if !has_param(target) {
                        need(
                            Required::Param(target.clone()),
                            "spine target",
                            owner.clone(),
                        );
                    }
                }
                &[]
            }
            ClmNodeKind::Group | ClmNodeKind::MeshGroup(_) => &[],
        };
        for mask in masks {
            if !has_node(&mask.source) {
                need(
                    Required::Drawable(mask.source.clone()),
                    "mask source",
                    owner.clone(),
                );
            }
        }
    }

    for binding in &doc.bindings {
        let owner = binding.node.to_string();
        if !has_node(&binding.node) {
            need(
                Required::Node(binding.node.clone()),
                "binding node",
                owner.clone(),
            );
        }
        for param in &binding.params {
            if !has_param(param) {
                need(
                    Required::Param(param.clone()),
                    "binding param",
                    owner.clone(),
                );
            }
        }
    }

    for weld in &doc.welds {
        for (end, far, on_a) in [(&weld.a, &weld.b, true), (&weld.b, &weld.a, false)] {
            if has_node(end) {
                continue;
            }
            // One requirement per paired base slot; a weld with no pairs still names the part.
            for pair in &weld.pairs {
                let slot = if on_a { &pair.a } else { &pair.b };
                need(
                    Required::Slot(end.clone(), slot.clone()),
                    "weld end",
                    far.to_string(),
                );
            }
            if weld.pairs.is_empty() {
                need(Required::Part(end.clone()), "weld end", far.to_string());
            }
        }
    }

    for (key, value) in &doc.extensions {
        let ClmExtension::Maintained { refs, .. } = value else {
            continue;
        };
        for r in refs {
            let id = match r {
                IdRef::Node(id) if !has_node(id) => Required::Node(id.clone()),
                IdRef::Param(id) if !has_param(id) => Required::Param(id.clone()),
                IdRef::Slot(node, slot) if !has_node(node) => {
                    Required::Slot(node.clone(), slot.clone())
                }
                _ => continue,
            };
            need(
                id,
                "extension reference",
                format!("extension {:?}", key.as_str()),
            );
        }
    }

    out.sort();
    out.dedup();
    out
}

fn columns(id: &Required) -> (&'static str, String, Option<String>) {
    match id {
        Required::Node(id) => ("node", id.to_string(), None),
        Required::Drawable(id) => ("drawable", id.to_string(), None),
        Required::Part(id) => ("part", id.to_string(), None),
        Required::Param(id) => ("param", id.to_string(), None),
        Required::Slot(node, slot) => ("slot", node.to_string(), Some(slot.to_string())),
    }
}

/// `slot` is empty for every kind but `slot`, so the column count never varies.
pub fn render_line(requirement: &Requirement) -> String {
    let (kind, id, slot) = columns(&requirement.id);
    format!(
        "{kind}\t{id}\t{}\t{}\t{}",
        slot.unwrap_or_default(),
        requirement.field,
        requirement.owner
    )
}

pub fn render_json(requirements: &[Requirement]) -> String {
    let entries: Vec<serde_json::Value> = requirements
        .iter()
        .map(|r| {
            let (kind, id, slot) = columns(&r.id);
            serde_json::json!({
                "kind": kind,
                "id": id,
                "slot": slot,
                "field": r.field,
                "owner": r.owner,
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}
