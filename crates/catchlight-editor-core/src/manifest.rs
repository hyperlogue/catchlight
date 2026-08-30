//! The JSON manifest: a human/agent-authored description of a puppet built from
//! loose textures, and a best-effort text mirror of the model (`to_manifest`).
//!
//! The manifest is authoring sugar — meshes are *generated* (quad / grid) from
//! texture dimensions, not stored vertex-for-vertex. `.clm` is the lossless
//! form; `to_manifest` preserves structure (tree, transforms, textures, params)
//! but re-generates meshes on re-import.
//!
//! Params in a model are scalars, so a manifest param with `"vec2": true`
//! imports as the two params `<name>.x` and `<name>.y` — the pair a two-param
//! binding spans — and `to_manifest` writes every param back as a scalar.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use catchlight_core::formats::clm::{
    ClmIndices, ClmMesh, ClmPhysics, ClmTransform, TextureAlpha, TextureEncoding,
};
use catchlight_core::{LoadBudget, LoadLimitError, LoadResource};

/// A manifest import mints Ids from this seed, so importing the same manifest
/// twice produces the same Ids.
const IMPORT_SEED: u32 = 0x0c1a_7c17;

use catchlight_core::id::{Name, NodeId, SeededHex, TexId};
use catchlight_core::{
    Model, ModelError, ModelNode, ModelNodeKind, ModelParam, ModelPart, ModelTexture,
};

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("duplicate node id {0:?}")]
    DuplicateNode(String),
    #[error("node {0:?} references unknown parent {1:?}")]
    UnknownParent(String, String),
    #[error("node {0:?} has unknown kind {1:?}")]
    UnknownKind(String, String),
    #[error("node {0:?} references unknown texture {1:?}")]
    UnknownTexture(String, String),
    #[error("texture {0:?} was not provided to the importer")]
    MissingTextureData(String),
    #[error("part {0:?} needs a texture to auto-generate its mesh")]
    MeshNeedsTexture(String),
    #[error("decoding texture {0:?}: {1}")]
    Decode(String, String),
    #[error(transparent)]
    LoadLimit(#[from] LoadLimitError),
    #[error("building the model: {0}")]
    Model(#[from] ModelError),
}

/// The decoded PNG/TGA bytes for one manifest texture, supplied by the caller
/// (so the pure core never touches the filesystem).
#[derive(Debug, Clone)]
pub struct TextureData {
    pub encoding: TextureEncoding,
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub physics: ManifestPhysics,
    #[serde(default)]
    pub textures: Vec<ManifestTexture>,
    #[serde(default)]
    pub nodes: Vec<ManifestNode>,
    #[serde(default)]
    pub params: Vec<ManifestParam>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ManifestPhysics {
    pub pixels_per_meter: f32,
    pub gravity: f32,
}

impl Default for ManifestPhysics {
    fn default() -> Self {
        let p = ClmPhysics::default();
        Self {
            pixels_per_meter: p.pixels_per_meter,
            gravity: p.gravity,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTexture {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestNode {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "group_kind")]
    pub kind: String,
    #[serde(default)]
    pub translate: Option<[f32; 3]>,
    #[serde(default)]
    pub rotate: Option<[f32; 3]>,
    #[serde(default)]
    pub scale: Option<[f32; 2]>,
    #[serde(default)]
    pub z_order: f32,
    #[serde(default)]
    pub texture: Option<String>,
    #[serde(default)]
    pub mesh: Option<MeshSpec>,
}

fn group_kind() -> String {
    "group".into()
}

/// A mesh generator: a quad covering the texture, or a `cols`x`rows` grid.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "auto", rename_all = "lowercase")]
pub enum MeshSpec {
    Quad,
    Grid {
        #[serde(default = "one")]
        cols: u32,
        #[serde(default = "one")]
        rows: u32,
    },
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestParam {
    pub name: String,
    #[serde(default)]
    pub vec2: bool,
    #[serde(default)]
    pub min: [f32; 2],
    #[serde(default = "unit2")]
    pub max: [f32; 2],
    #[serde(default)]
    pub defaults: [f32; 2],
    #[serde(default)]
    pub axis_x: Vec<f32>,
    #[serde(default)]
    pub axis_y: Vec<f32>,
}

fn unit2() -> [f32; 2] {
    [1.0, 1.0]
}

impl Manifest {
    pub fn from_json(s: &str) -> Result<Manifest, ManifestError> {
        Self::from_json_with_budget(s, &mut LoadBudget::default())
    }

    pub fn from_json_with_budget(
        s: &str,
        budget: &mut LoadBudget,
    ) -> Result<Manifest, ManifestError> {
        budget.charge(LoadResource::EncodedBytes, s.len() as u64)?;
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json(&self) -> Result<String, ManifestError> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

/// Manifest authoring over a [`Model`]. An extension trait because the Model is
/// defined in `catchlight-core` while the manifest lives here.
pub trait ModelManifestExt: Sized {
    /// Build a model from a manifest plus the caller-loaded texture bytes. Parts
    /// auto-generate their mesh from the referenced texture's pixel dimensions.
    fn from_manifest(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
    ) -> Result<Self, ManifestError>;

    /// [`Self::from_manifest`], charged against a caller-supplied budget.
    fn from_manifest_with_budget(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
        budget: &mut LoadBudget,
    ) -> Result<Self, ManifestError>;

    /// Best-effort text mirror: structure, transforms, texture references and
    /// param definitions. Textures get synthetic ids/paths (the caller writes
    /// the bytes there); meshes are not emitted (re-import regenerates a quad).
    fn to_manifest(&self) -> Manifest;
}

impl ModelManifestExt for Model {
    fn from_manifest(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
    ) -> Result<Model, ManifestError> {
        Self::from_manifest_with_budget(manifest, data, &mut LoadBudget::default())
    }

    fn from_manifest_with_budget(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
        budget: &mut LoadBudget,
    ) -> Result<Model, ManifestError> {
        budget.charge(LoadResource::Textures, manifest.textures.len() as u64)?;
        budget.charge(LoadResource::Nodes, manifest.nodes.len() as u64)?;
        budget.charge(LoadResource::Params, manifest.params.len() as u64)?;
        // Ids are generated, not taken from the manifest, so the two id
        // spaces stay separate; a fixed seed keeps one manifest importing to
        // one set of Ids.
        let mut hex = SeededHex::new(IMPORT_SEED);
        let mut m = Model::new();
        m.set_physics(ClmPhysics {
            pixels_per_meter: manifest.physics.pixels_per_meter,
            gravity: manifest.physics.gravity,
        });

        let mut tex_ids: HashMap<&str, TexId> = HashMap::new();
        let mut tex_dims: HashMap<&str, (f32, f32)> = HashMap::new();
        for t in &manifest.textures {
            let d = data
                .get(&t.id)
                .ok_or_else(|| ManifestError::MissingTextureData(t.id.clone()))?;
            budget.charge(LoadResource::EncodedBytes, d.bytes.len() as u64)?;
            let (w, h) =
                image_dims(&d.bytes).map_err(|e| ManifestError::Decode(t.id.clone(), e))?;
            budget.check_texture_dimensions(w, h)?;
            let id = m.add_texture(
                ModelTexture {
                    encoding: d.encoding,
                    alpha: TextureAlpha::Straight,
                    data: d.bytes.clone(),
                },
                &mut hex,
            )?;
            tex_ids.insert(t.id.as_str(), id);
            tex_dims.insert(t.id.as_str(), (w as f32, h as f32));
        }

        let mut seen = HashSet::new();
        for mn in &manifest.nodes {
            if !seen.insert(mn.id.as_str()) {
                return Err(ManifestError::DuplicateNode(mn.id.clone()));
            }
        }

        // Place nodes in any order: resolve a node once its parent exists (or it
        // is top-level, attaching under the implicit root).
        let mut resolved: HashMap<&str, NodeId> = HashMap::new();
        let mut pending: Vec<&ManifestNode> = manifest.nodes.iter().collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut still = Vec::new();
            for mn in pending {
                let parent = match &mn.parent {
                    None => Some(m.root().clone()),
                    Some(pid) => resolved.get(pid.as_str()).cloned(),
                };
                let Some(parent) = parent else {
                    still.push(mn);
                    continue;
                };
                let (kind, albedo) = build_kind(mn, &tex_ids, &tex_dims, budget)?;
                let mut node =
                    ModelNode::new(mn.name.clone().unwrap_or_else(|| mn.id.clone()), kind);
                node.transform = ClmTransform {
                    translation: mn.translate.unwrap_or([0.0; 3]),
                    rotation: mn.rotate.unwrap_or([0.0; 3]),
                    scale: mn.scale.unwrap_or([1.0, 1.0]),
                };
                node.z_order = mn.z_order;
                let id = m
                    .add_node(&parent, node, &mut hex)
                    .map_err(|_| ManifestError::UnknownParent(mn.id.clone(), "<root>".into()))?;
                if albedo.is_some() {
                    m.set_part_albedo(&id, albedo)?;
                }
                resolved.insert(mn.id.as_str(), id);
            }
            if still.len() == before {
                let bad = still[0];
                return Err(ManifestError::UnknownParent(
                    bad.id.clone(),
                    bad.parent.clone().unwrap_or_default(),
                ));
            }
            pending = still;
        }

        for mp in &manifest.params {
            // Key positions are normalized 0..1 across [min, max] (see
            // ModelParam::key_positions), not param-value space.
            let axis_x = if mp.axis_x.is_empty() {
                vec![0.0, 1.0]
            } else {
                mp.axis_x.clone()
            };
            let axis_y = if mp.axis_y.is_empty() {
                vec![0.0, 1.0]
            } else {
                mp.axis_y.clone()
            };
            budget.charge_product(
                LoadResource::BindingCells,
                axis_x.len() as u64,
                if mp.vec2 { axis_y.len() as u64 } else { 1 },
            )?;
            // Params are scalars; a manifest asking for a 2-D one gets the two
            // halves a binding over the pair would span.
            let (name_x, name_y) = if mp.vec2 {
                (format!("{}.x", mp.name), format!("{}.y", mp.name))
            } else {
                (mp.name.clone(), String::new())
            };
            m.add_param(
                ModelParam {
                    name: Name::truncated(name_x),
                    min: mp.min[0],
                    max: mp.max[0],
                    default: mp.defaults[0],
                    key_positions: axis_x,
                },
                &mut hex,
            )?;
            if mp.vec2 {
                m.add_param(
                    ModelParam {
                        name: Name::truncated(name_y),
                        min: mp.min[1],
                        max: mp.max[1],
                        default: mp.defaults[1],
                        key_positions: axis_y,
                    },
                    &mut hex,
                )?;
            }
        }

        Ok(m)
    }

    fn to_manifest(&self) -> Manifest {
        let order = self.nodes_in_order();
        let node_name: HashMap<&NodeId, String> = order
            .iter()
            .enumerate()
            .map(|(i, id)| (id, format!("n{i}")))
            .collect();

        let mut tex_name: HashMap<&TexId, String> = HashMap::new();
        let mut textures = Vec::new();
        for (i, tid) in self.texture_ids().iter().enumerate() {
            let id = format!("tex{i}");
            let ext = match self.texture(tid).map(|t| t.encoding) {
                Some(TextureEncoding::Tga) => "tga",
                _ => "png",
            };
            textures.push(ManifestTexture {
                id: id.clone(),
                path: format!("{id}.{ext}"),
            });
            tex_name.insert(tid, id);
        }

        let mut nodes = Vec::new();
        for id in &order {
            if id == self.root() {
                continue;
            }
            let Some(n) = self.node(id) else { continue };
            let parent = match n.parent() {
                Some(p) if p == self.root() => None,
                Some(p) => node_name.get(p).cloned(),
                None => None,
            };
            let (kind, texture) = match &n.kind {
                ModelNodeKind::Part(p) => {
                    ("part", p.albedo().and_then(|t| tex_name.get(t).cloned()))
                }
                _ => ("group", None),
            };
            nodes.push(ManifestNode {
                id: node_name.get(id).cloned().unwrap_or_default(),
                parent,
                name: Some(n.name.to_string()),
                kind: kind.to_string(),
                translate: Some(n.transform.translation),
                rotate: Some(n.transform.rotation),
                scale: Some(n.transform.scale),
                z_order: n.z_order,
                texture,
                mesh: None,
            });
        }

        let params = self
            .param_ids()
            .iter()
            .filter_map(|pid| self.param(pid))
            .map(|p| ManifestParam {
                name: p.name.to_string(),
                vec2: false,
                min: [p.min, 0.0],
                max: [p.max, 0.0],
                defaults: [p.default, 0.0],
                axis_x: p.key_positions.clone(),
                axis_y: Vec::new(),
            })
            .collect();

        Manifest {
            name: String::new(),
            physics: ManifestPhysics {
                pixels_per_meter: self.physics().pixels_per_meter,
                gravity: self.physics().gravity,
            },
            textures,
            nodes,
            params,
        }
    }
}

fn build_kind(
    mn: &ManifestNode,
    tex_ids: &HashMap<&str, TexId>,
    tex_dims: &HashMap<&str, (f32, f32)>,
    budget: &mut LoadBudget,
) -> Result<(ModelNodeKind, Option<TexId>), ManifestError> {
    match mn.kind.as_str() {
        "group" => Ok((ModelNodeKind::Group, None)),
        "part" => {
            let albedo = match &mn.texture {
                Some(t) => Some(
                    tex_ids
                        .get(t.as_str())
                        .ok_or_else(|| ManifestError::UnknownTexture(mn.id.clone(), t.clone()))?
                        .clone(),
                ),
                None => None,
            };
            let dims = mn.texture.as_deref().and_then(|t| tex_dims.get(t).copied());
            let mesh = match mn.mesh {
                Some(spec) => {
                    let (w, h) =
                        dims.ok_or_else(|| ManifestError::MeshNeedsTexture(mn.id.clone()))?;
                    gen_mesh(spec, w, h, budget)?
                }
                None => match dims {
                    Some((w, h)) => quad_mesh(w, h, budget)?,
                    None => ClmMesh::default(),
                },
            };
            Ok((ModelNodeKind::Part(ModelPart::new(mesh)), albedo))
        }
        other => Err(ManifestError::UnknownKind(mn.id.clone(), other.to_string())),
    }
}

fn gen_mesh(
    spec: MeshSpec,
    w: f32,
    h: f32,
    budget: &mut LoadBudget,
) -> Result<ClmMesh, ManifestError> {
    match spec {
        MeshSpec::Quad => quad_mesh(w, h, budget),
        MeshSpec::Grid { cols, rows } => grid_mesh(w, h, cols, rows, budget),
    }
}

/// One quad centered on the origin, the texture mapped corner-to-corner. UV `v`
/// increases downward, matching texture coordinate space.
fn quad_mesh(w: f32, h: f32, budget: &mut LoadBudget) -> Result<ClmMesh, ManifestError> {
    budget.charge(LoadResource::Vertices, 4)?;
    budget.charge(LoadResource::Indices, 6)?;
    let (hw, hh) = (w / 2.0, h / 2.0);
    Ok(ClmMesh {
        verts: vec![-hw, -hh, hw, -hh, hw, hh, -hw, hh],
        uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
        origin: [0.0, 0.0],
    })
}

fn grid_mesh(
    w: f32,
    h: f32,
    cols: u32,
    rows: u32,
    budget: &mut LoadBudget,
) -> Result<ClmMesh, ManifestError> {
    let cols = u64::from(cols.max(1));
    let rows = u64::from(rows.max(1));
    let cells = budget.charge_product(LoadResource::ManifestGridCells, cols, rows)?;
    let vertex_count = cols
        .checked_add(1)
        .and_then(|cols| rows.checked_add(1).and_then(|rows| cols.checked_mul(rows)))
        .ok_or(LoadLimitError {
            resource: "vertices",
            limit: u64::MAX,
            got: u64::MAX,
        })?;
    let index_count = cells.checked_mul(6).ok_or(LoadLimitError {
        resource: "indices",
        limit: u64::MAX,
        got: u64::MAX,
    })?;
    budget.charge(LoadResource::Vertices, vertex_count)?;
    budget.charge(LoadResource::Indices, index_count)?;
    let cols = u32::try_from(cols).map_err(|_| LoadLimitError {
        resource: "manifest grid columns",
        limit: u32::MAX as u64,
        got: cols,
    })?;
    let rows = u32::try_from(rows).map_err(|_| LoadLimitError {
        resource: "manifest grid rows",
        limit: u32::MAX as u64,
        got: rows,
    })?;
    let scalar_count = usize::try_from(vertex_count.checked_mul(2).ok_or(LoadLimitError {
        resource: "vertex coordinates",
        limit: usize::MAX as u64,
        got: u64::MAX,
    })?)
    .map_err(|_| LoadLimitError {
        resource: "vertex coordinates",
        limit: usize::MAX as u64,
        got: vertex_count.saturating_mul(2),
    })?;
    let index_capacity = usize::try_from(index_count).map_err(|_| LoadLimitError {
        resource: "indices",
        limit: usize::MAX as u64,
        got: index_count,
    })?;
    let mut verts = Vec::with_capacity(scalar_count);
    let mut uvs = Vec::with_capacity(verts.capacity());
    for j in 0..=rows {
        let fy = j as f32 / rows as f32;
        for i in 0..=cols {
            let fx = i as f32 / cols as f32;
            verts.push(-w / 2.0 + fx * w);
            verts.push(-h / 2.0 + fy * h);
            uvs.push(fx);
            uvs.push(1.0 - fy);
        }
    }
    let stride = cols + 1;
    let mut idx: Vec<u32> = Vec::with_capacity(index_capacity);
    for j in 0..rows {
        for i in 0..cols {
            let a = j * stride + i;
            let (b, c, d) = (a + 1, a + stride, a + stride + 1);
            idx.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }
    let indices = if vertex_count <= u16::MAX as u64 {
        ClmIndices::U16(idx.iter().map(|&i| i as u16).collect())
    } else {
        ClmIndices::U32(idx)
    };
    Ok(ClmMesh {
        verts,
        uvs,
        indices,
        origin: [0.0, 0.0],
    })
}

pub fn image_dims(bytes: &[u8]) -> Result<(u32, u32), String> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::new(w, h);
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn data(id: &str, w: u32, h: u32) -> HashMap<String, TextureData> {
        let mut m = HashMap::new();
        m.insert(
            id.to_string(),
            TextureData {
                encoding: TextureEncoding::Png,
                bytes: Arc::new(png_bytes(w, h)),
            },
        );
        m
    }

    #[test]
    fn import_builds_part_with_grid_mesh() {
        let json = r#"{
            "textures": [{"id": "face", "path": "face.png"}],
            "nodes": [{"id": "face", "kind": "part", "texture": "face",
                       "mesh": {"auto": "grid", "cols": 2, "rows": 2}}],
            "params": [{"name": "ht", "min": [-1, 0], "max": [1, 0], "axis_x": [-1, 0, 1]}]
        }"#;
        let manifest = Manifest::from_json(json).unwrap();
        let m = Model::from_manifest(&manifest, &data("face", 16, 16)).unwrap();
        assert_eq!(m.node_count(), 2); // implicit root + face
        assert_eq!(m.texture_ids().len(), 1);
        assert_eq!(m.param_ids().len(), 1);
        let face = m
            .nodes_in_order()
            .into_iter()
            .find(|id| matches!(m.node(id).map(|n| &n.kind), Some(ModelNodeKind::Part(_))))
            .unwrap();
        if let Some(ModelNodeKind::Part(p)) = m.node(&face).map(|n| &n.kind) {
            assert_eq!(p.mesh().verts.len() / 2, 9); // 3x3 grid vertices
            assert!(p.albedo().is_some());
        } else {
            panic!("expected a part");
        }
        assert!(m.to_clm_bytes().is_ok());
        assert!(m.check().is_empty());
    }

    #[test]
    fn manifest_grid_limits_are_aggregate() {
        let manifest = Manifest {
            textures: vec![ManifestTexture {
                id: "face".into(),
                path: "face.png".into(),
            }],
            nodes: vec![
                ManifestNode {
                    id: "left".into(),
                    parent: None,
                    name: None,
                    kind: "part".into(),
                    translate: None,
                    rotate: None,
                    scale: None,
                    z_order: 0.0,
                    texture: Some("face".into()),
                    mesh: Some(MeshSpec::Grid { cols: 2, rows: 2 }),
                },
                ManifestNode {
                    id: "right".into(),
                    parent: None,
                    name: None,
                    kind: "part".into(),
                    translate: None,
                    rotate: None,
                    scale: None,
                    z_order: 0.0,
                    texture: Some("face".into()),
                    mesh: Some(MeshSpec::Grid { cols: 2, rows: 2 }),
                },
            ],
            ..Manifest::default()
        };

        let mut budget = LoadBudget::new(catchlight_core::LoadLimits {
            manifest_grid_cells: 7,
            ..catchlight_core::LoadLimits::default()
        });
        let err = Model::from_manifest_with_budget(&manifest, &data("face", 16, 16), &mut budget)
            .unwrap_err();

        assert!(matches!(
            err,
            ManifestError::LoadLimit(LoadLimitError {
                resource: "manifest grid cells",
                got: 8,
                ..
            })
        ));
    }

    #[test]
    fn manifest_grid_extreme_dimensions_fail_before_allocation() {
        let manifest = Manifest {
            textures: vec![ManifestTexture {
                id: "face".into(),
                path: "face.png".into(),
            }],
            nodes: vec![ManifestNode {
                id: "face".into(),
                parent: None,
                name: None,
                kind: "part".into(),
                translate: None,
                rotate: None,
                scale: None,
                z_order: 0.0,
                texture: Some("face".into()),
                mesh: Some(MeshSpec::Grid {
                    cols: u32::MAX,
                    rows: u32::MAX,
                }),
            }],
            ..Manifest::default()
        };

        let err = Model::from_manifest(&manifest, &data("face", 16, 16)).unwrap_err();

        assert!(matches!(
            err,
            ManifestError::LoadLimit(LoadLimitError {
                resource: "manifest grid cells",
                ..
            })
        ));
    }

    #[test]
    fn manifest_roundtrips_structure() {
        let json = r#"{
            "textures": [{"id": "t", "path": "t.png"}],
            "nodes": [
                {"id": "grp", "kind": "group"},
                {"id": "leaf", "parent": "grp", "kind": "part", "texture": "t", "z_order": 2.0}
            ],
            "params": [{"name": "p", "min": [0, 0], "max": [1, 0]}]
        }"#;
        let m =
            Model::from_manifest(&Manifest::from_json(json).unwrap(), &data("t", 8, 8)).unwrap();
        let exported = m.to_manifest().to_json().unwrap();
        let m2 = Model::from_manifest(
            &Manifest::from_json(&exported).unwrap(),
            &data("tex0", 8, 8),
        )
        .unwrap();
        assert_eq!(m.node_count(), m2.node_count());
        assert_eq!(m.texture_ids().len(), m2.texture_ids().len());
        assert_eq!(m.param_ids().len(), m2.param_ids().len());
    }

    #[test]
    fn check_flags_untextured_part_and_physics_without_target() {
        let mut m = Model::new();
        let root = m.root().clone();
        m.add_node(
            &root,
            ModelNode::new(
                "ghost",
                ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
            ),
            &mut catchlight_core::id::SeededHex::new(0),
        )
        .unwrap();
        let warnings = m.check();
        assert!(warnings.iter().any(|w| w.message.contains("no texture")));
    }
}
