//! `extract`, `merge` and `requirements` — the addon operations, at the file.
//!
//! `extract` and `merge` are [`Model::extract`](catchlight_core::Model::extract) and
//! [`Model::install`](catchlight_core::Model::install) with a
//! reader on one side and a writer on the other. Going through a `Model` is
//! deliberate: "without loading a Model" means without decoding textures and
//! without building a puppet, not without the type that knows what an addon
//! is. A `Model` built from a `.clm` holds the texture bytes exactly as the
//! file did, so cutting a subtree out of a model with a hundred megabytes of
//! images moves those bytes and looks at none of them.
//!
//! `requirements` is the exception, and the reason it is: it answers what an
//! addon needs from a **plain scan of the wire**, because every
//! cross-reference in a `.clm` is a plain CBOR string. That makes it usable on
//! a file no reader would accept, and it makes it a second opinion — [`scan`]
//! and [`Model::requirements`](catchlight_core::Model::requirements) walk the same
//! fields and must always agree,
//! which `requirements_agree_with_the_model` pins. A texture is never among
//! them: an addon carries the textures its own parts draw, so `extract`
//! copies one the base still draws into the addon instead of listing it as
//! something a base has to supply.
//!
//! Two rules the commands here enforce that the Model API alone does not:
//!
//! - **A parentless node cannot be extracted.** `Model::extract` would
//!   happily return it, but the result is a complete model rather than an
//!   addon — its root names no parent, so nothing says where it attaches, and
//!   `merge` would refuse it later with
//!   [`InstallError::NotAnAddon`](catchlight_core::InstallError::NotAnAddon).
//!   Refusing at the cut says so while the author can still act on it.
//! - **`merge`'s addon argument is read as a fragment, always.** The two wire
//!   shapes are disjoint and neither reader guesses between them, so handing
//!   `merge` a complete model has to report itself as one; reading it with the
//!   shape-sniffing [`crate::file::load`] would instead succeed and fail one
//!   step later with a worse message.

use std::path::Path;

use catchlight_core::formats::clm::{ClmDocument, ClmFile, ClmNodeKind};
use catchlight_core::id::NodeId;
use catchlight_core::{Required, Requirement};

use crate::{file, Error};

/// What one `extract` cut out.
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

/// What one `merge` installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    pub nodes: usize,
    pub roots: Vec<String>,
    pub textures: usize,
    pub bindings: usize,
    pub welds: usize,
    pub animations: usize,
}

impl std::fmt::Display for Merged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "installed {} node(s), {} texture(s), {} binding(s), {} weld(s), {} animation(s)",
            self.nodes, self.textures, self.bindings, self.welds, self.animations
        )
    }
}

/// Cut `ids` and their subtrees out of `path` as an addon, written to `out`.
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

/// Every id names a node the file carries, and none of them is parentless.
fn resolve_roots(doc: &ClmDocument, path: &Path, ids: &[String]) -> Result<Vec<NodeId>, Error> {
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

/// Install `addon` into `base`, writing the merged model to `out`.
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
        animations: installed.animations().len(),
    };
    file::write_model(&model, out)?;
    Ok(merged)
}

/// What the addon at `path` needs from a base model.
pub fn requirements(path: &Path) -> Result<Vec<Requirement>, Error> {
    Ok(scan(&file::read(path)?))
}

/// Every Id the document names but does not carry, found by walking the wire's
/// own reference fields.
///
/// This is [`Model::requirements`](catchlight_core::Model::requirements) with the
/// file's tables in place of a
/// Model's, and it walks exactly the same seven fields: `nodes[].parent`,
/// `nodes[].kind.*.masks[].source`,
/// `nodes[].kind.SimplePhysics.target_params`, `bindings[].node`,
/// `bindings[].params`, `welds[].{a,b}.node` and `animations[].lanes[].param`.
/// `nodes[].kind.Part.albedo` is not among them: an addon carries the textures
/// its own parts draw, so a texture is not something a base can supply.
/// Sorted and deduplicated, so the same Id appears once per field that names
/// it.
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
            // A part's albedo is not scanned: an addon carries the textures
            // its own parts draw, so it is never a requirement. The fragment
            // reader refuses one that dangles.
            ClmNodeKind::Part(p) => p.masks.as_slice(),
            ClmNodeKind::Composite(c) => c.masks.as_slice(),
            ClmNodeKind::SimplePhysics(s) => {
                for target in s.target_params.iter().flatten() {
                    if !has_param(target) {
                        need(
                            Required::Param(target.clone()),
                            "physics target",
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
                    Required::Part(mask.source.clone()),
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
        for (end, far) in [(&weld.a, &weld.b), (&weld.b, &weld.a)] {
            if !has_node(&end.node) {
                need(
                    Required::Seam(end.node.clone(), end.seam.clone()),
                    "weld end",
                    far.node.to_string(),
                );
            }
        }
    }

    for animation in &doc.animations {
        for lane in &animation.lanes {
            if !has_param(&lane.param) {
                need(
                    Required::Param(lane.param.clone()),
                    "animation lane",
                    format!("animation {:?}", animation.name),
                );
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

// ---- rendering ------------------------------------------------------------

/// One requirement as five TAB-separated columns: `kind`, `id`, `seam`,
/// `field`, `owner`. `seam` is empty for every kind but `seam`, so the column
/// count never varies and `cut -f2` is always the Id.
pub fn render_line(requirement: &Requirement) -> String {
    let (kind, id, seam) = match &requirement.id {
        Required::Node(id) => ("node", id.to_string(), String::new()),
        Required::Part(id) => ("part", id.to_string(), String::new()),
        Required::Param(id) => ("param", id.to_string(), String::new()),
        Required::Seam(node, seam) => ("seam", node.to_string(), seam.to_string()),
    };
    format!(
        "{kind}\t{id}\t{seam}\t{}\t{}",
        requirement.field, requirement.owner
    )
}

/// The same list as a JSON array, for a CI step that would rather not split
/// on tabs.
pub fn render_json(requirements: &[Requirement]) -> String {
    let entries: Vec<serde_json::Value> = requirements
        .iter()
        .map(|r| {
            let (kind, id, seam) = match &r.id {
                Required::Node(id) => ("node", id.to_string(), None),
                Required::Part(id) => ("part", id.to_string(), None),
                Required::Param(id) => ("param", id.to_string(), None),
                Required::Seam(node, seam) => ("seam", node.to_string(), Some(seam.to_string())),
            };
            serde_json::json!({
                "kind": kind,
                "id": id,
                "seam": seam,
                "field": r.field,
                "owner": r.owner,
            })
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}
