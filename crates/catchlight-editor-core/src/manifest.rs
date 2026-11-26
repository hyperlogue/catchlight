//! The JSON manifest: authoring sugar for a puppet built from loose textures.
//!
//! Meshes are generated from texture dimensions, so `to_manifest` is a lossy
//! mirror; `.clm` is the lossless form.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use catchlight_core::formats::clm::{
    ClmIndices, ClmMesh, ClmTransform, TextureAlpha, TextureEncoding,
};
use catchlight_core::{LoadBudget, LoadLimitError, LoadResource};

/// A fixed seed, so importing the same manifest twice produces the same Ids.
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
    #[error("node {0:?} references unknown texture {1:?}")]
    UnknownTexture(String, String),
    #[error("texture {0:?} was not provided to the importer")]
    MissingTextureData(String),
    #[error("part {0:?} needs a texture to auto-generate its mesh")]
    MeshNeedsTexture(String),
    #[error("decoding texture {id:?}: {source}")]
    Decode {
        id: String,
        #[source]
        source: image::ImageError,
    },
    #[error(transparent)]
    LoadLimit(#[from] LoadLimitError),
    #[error("building the model: {0}")]
    Model(#[from] ModelError),
}

#[derive(Debug, Clone)]
pub struct TextureData {
    pub encoding: TextureEncoding,
    pub bytes: Arc<[u8]>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub textures: Vec<ManifestTexture>,
    #[serde(default)]
    pub nodes: Vec<ManifestNode>,
    #[serde(default)]
    pub params: Vec<ManifestParam>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTexture {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestNode {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: ManifestNodeKind,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ManifestNodeKind {
    #[default]
    Group,
    Part,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(tag = "auto", rename_all = "lowercase", deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
}

fn unit2() -> [f32; 2] {
    [1.0, 1.0]
}

impl Manifest {
    pub fn from_json(s: &str) -> Result<Self, ManifestError> {
        Self::from_json_with_budget(s, &mut LoadBudget::default())
    }

    pub fn from_json_with_budget(s: &str, budget: &mut LoadBudget) -> Result<Self, ManifestError> {
        budget.charge(LoadResource::EncodedBytes, s.len() as u64)?;
        Ok(serde_json::from_str(s)?)
    }

    pub fn to_json(&self) -> Result<String, ManifestError> {
        Ok(serde_json::to_string_pretty(self)?)
    }
}

pub trait ModelManifestExt: Sized {
    fn from_manifest(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
    ) -> Result<Self, ManifestError>;

    fn from_manifest_with_budget(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
        budget: &mut LoadBudget,
    ) -> Result<Self, ManifestError>;

    /// Best-effort and lossy: meshes are not emitted (re-import regenerates a
    /// quad), and textures get synthetic ids/paths the caller writes bytes to.
    fn to_manifest(&self) -> Manifest;
}

impl ModelManifestExt for Model {
    fn from_manifest(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
    ) -> Result<Self, ManifestError> {
        Self::from_manifest_with_budget(manifest, data, &mut LoadBudget::default())
    }

    fn from_manifest_with_budget(
        manifest: &Manifest,
        data: &HashMap<String, TextureData>,
        budget: &mut LoadBudget,
    ) -> Result<Self, ManifestError> {
        budget.charge(LoadResource::Textures, manifest.textures.len() as u64)?;
        budget.charge(LoadResource::Nodes, manifest.nodes.len() as u64)?;
        budget.charge(LoadResource::Params, manifest.params.len() as u64)?;
        let mut hex = SeededHex::new(IMPORT_SEED);
        let mut m = Self::new();

        // A texture goes to the part that draws it, so a manifest texture no node
        // names never reaches the model.
        let mut tex_data: HashMap<&str, &TextureData> = HashMap::new();
        let mut tex_dims: HashMap<&str, (f32, f32)> = HashMap::new();
        for t in &manifest.textures {
            let d = data
                .get(&t.id)
                .ok_or_else(|| ManifestError::MissingTextureData(t.id.clone()))?;
            budget.charge(LoadResource::EncodedBytes, d.bytes.len() as u64)?;
            let (w, h) =
                image_dims_as(&d.bytes, d.encoding).map_err(|source| ManifestError::Decode {
                    id: t.id.clone(),
                    source,
                })?;
            budget.check_texture_dimensions(w, h)?;
            tex_data.insert(t.id.as_str(), d);
            tex_dims.insert(t.id.as_str(), (w as f32, h as f32));
        }
        let mut tex_ids: HashMap<&str, TexId> = HashMap::new();

        let mut seen = HashSet::new();
        for mn in &manifest.nodes {
            if !seen.insert(mn.id.as_str()) {
                return Err(ManifestError::DuplicateNode(mn.id.clone()));
            }
        }

        let mut resolved: HashMap<&str, NodeId> = HashMap::new();
        let mut pending: Vec<&ManifestNode> = manifest.nodes.iter().collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut still = Vec::new();
            for mn in pending {
                let parent = match &mn.parent {
                    None => m.root().cloned(),
                    Some(pid) => resolved.get(pid.as_str()).cloned(),
                };
                let Some(parent) = parent else {
                    still.push(mn);
                    continue;
                };
                let (kind, albedo) = build_kind(mn, &tex_dims, budget)?;
                let mut node =
                    ModelNode::new(mn.name.clone().unwrap_or_else(|| mn.id.clone()), kind);
                node.properties.transform = ClmTransform {
                    translation: mn.translate.unwrap_or([0.0; 3]),
                    rotation: mn.rotate.unwrap_or([0.0; 3]),
                    scale: mn.scale.unwrap_or([1.0, 1.0]),
                };
                node.properties.z_order = mn.z_order;
                let id = m.add_node(&parent, node, &mut hex)?;
                if let Some(name) = albedo {
                    match tex_ids.get(name) {
                        Some(tid) => m.set_part_albedo(&id, Some(tid.clone()))?,
                        None => {
                            let d = tex_data.get(name).ok_or_else(|| {
                                ManifestError::UnknownTexture(mn.id.clone(), name.to_string())
                            })?;
                            let tid = m.add_texture(
                                &id,
                                ModelTexture {
                                    encoding: d.encoding,
                                    alpha: TextureAlpha::Straight,
                                    data: d.bytes.clone(),
                                },
                                &mut hex,
                            )?;
                            tex_ids.insert(name, tid);
                        }
                    }
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
            if Some(id) == self.root() {
                continue;
            }
            let Some(n) = self.node(id) else { continue };
            let parent = match n.parent() {
                Some(p) if Some(p) == self.root() => None,
                Some(p) => node_name.get(p).cloned(),
                None => None,
            };
            let (kind, texture) = match &n.kind {
                ModelNodeKind::Part(p) => (
                    ManifestNodeKind::Part,
                    p.albedo().and_then(|t| tex_name.get(t).cloned()),
                ),
                _ => (ManifestNodeKind::Group, None),
            };
            nodes.push(ManifestNode {
                id: node_name.get(id).cloned().unwrap_or_default(),
                parent,
                name: Some(n.properties.name.to_string()),
                kind,
                translate: Some(n.properties.transform.translation),
                rotate: Some(n.properties.transform.rotation),
                scale: Some(n.properties.transform.scale),
                z_order: n.properties.z_order,
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
            })
            .collect();

        Manifest {
            name: String::new(),
            textures,
            nodes,
            params,
        }
    }
}

/// The model texture Id is minted by the upload, which needs the part to
/// exist, so this returns the *manifest* texture.
fn build_kind<'a>(
    mn: &'a ManifestNode,
    tex_dims: &HashMap<&str, (f32, f32)>,
    budget: &mut LoadBudget,
) -> Result<(ModelNodeKind, Option<&'a str>), ManifestError> {
    match mn.kind {
        ManifestNodeKind::Group => Ok((ModelNodeKind::Group, None)),
        ManifestNodeKind::Part => {
            let albedo = match &mn.texture {
                Some(t) if !tex_dims.contains_key(t.as_str()) => {
                    return Err(ManifestError::UnknownTexture(mn.id.clone(), t.clone()));
                }
                Some(t) => Some(t.as_str()),
                None => None,
            };
            let dims = albedo.and_then(|t| tex_dims.get(t).copied());
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

/// UV `v` increases downward, matching texture coordinate space.
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
            resource: LoadResource::Vertices,
            limit: u64::MAX,
            got: u64::MAX,
        })?;
    let index_count = cells.checked_mul(6).ok_or(LoadLimitError {
        resource: LoadResource::Indices,
        limit: u64::MAX,
        got: u64::MAX,
    })?;
    budget.charge(LoadResource::Vertices, vertex_count)?;
    budget.charge(LoadResource::Indices, index_count)?;
    let cols = u32::try_from(cols).map_err(|_| LoadLimitError {
        resource: LoadResource::ManifestGridColumns,
        limit: u64::from(u32::MAX),
        got: cols,
    })?;
    let rows = u32::try_from(rows).map_err(|_| LoadLimitError {
        resource: LoadResource::ManifestGridRows,
        limit: u64::from(u32::MAX),
        got: rows,
    })?;
    let scalar_count = usize::try_from(vertex_count.checked_mul(2).ok_or(LoadLimitError {
        resource: LoadResource::VertexCoordinates,
        limit: usize::MAX as u64,
        got: u64::MAX,
    })?)
    .map_err(|_| LoadLimitError {
        resource: LoadResource::VertexCoordinates,
        limit: usize::MAX as u64,
        got: vertex_count.saturating_mul(2),
    })?;
    let index_capacity = usize::try_from(index_count).map_err(|_| LoadLimitError {
        resource: LoadResource::Indices,
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
    let indices = if u16::try_from(vertex_count).is_ok() {
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

pub fn image_dims(bytes: &[u8]) -> Result<(u32, u32), image::ImageError> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()
}

/// The encoded format is authoritative; TGA cannot be identified by magic.
pub fn image_dims_as(
    bytes: &[u8],
    encoding: TextureEncoding,
) -> Result<(u32, u32), image::ImageError> {
    let format = match encoding {
        TextureEncoding::Png => image::ImageFormat::Png,
        TextureEncoding::Tga => image::ImageFormat::Tga,
    };
    image::ImageReader::with_format(Cursor::new(bytes), format).into_dimensions()
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
                bytes: png_bytes(w, h).into(),
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
            "params": [{"name": "ht", "min": [-1, 0], "max": [1, 0]}]
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
                    kind: ManifestNodeKind::Part,
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
                    kind: ManifestNodeKind::Part,
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

        let mut limits = catchlight_core::LoadLimits::default();
        limits.manifest_grid_cells = 7;
        let mut budget = LoadBudget::new(limits);
        let err = Model::from_manifest_with_budget(&manifest, &data("face", 16, 16), &mut budget)
            .unwrap_err();

        assert!(matches!(
            err,
            ManifestError::LoadLimit(LoadLimitError {
                resource: LoadResource::ManifestGridCells,
                got: 8,
                ..
            })
        ));
    }

    #[test]
    fn a_refused_node_reports_why_rather_than_an_unknown_parent() {
        let manifest = Manifest {
            nodes: vec![ManifestNode {
                id: "g".into(),
                parent: None,
                name: None,
                kind: ManifestNodeKind::Group,
                translate: None,
                rotate: None,
                scale: None,
                z_order: f32::NAN,
                texture: None,
                mesh: None,
            }],
            ..Manifest::default()
        };
        let err = Model::from_manifest(&manifest, &HashMap::new()).unwrap_err();
        assert!(matches!(err, ManifestError::Model(_)), "{err:?}");
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
                kind: ManifestNodeKind::Part,
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
                resource: LoadResource::ManifestGridCells,
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
    fn a_misspelt_manifest_key_is_refused() {
        for json in [
            r#"{"nodes": [{"id": "face", "kind": "part", "textrue": "face"}]}"#,
            r#"{"nodes": [], "parms": []}"#,
            r#"{"textures": [{"id": "t", "path": "t.png", "alpha": "straight"}]}"#,
            r#"{"params": [{"name": "p", "default": [0, 0]}]}"#,
            r#"{"nodes": [{"id": "f", "kind": "part", "mesh": {"auto": "grid", "colls": 2}}]}"#,
        ] {
            let err = Manifest::from_json(json).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{json}: {err}");
        }
    }

    #[test]
    fn a_node_kind_outside_group_and_part_does_not_parse() {
        let err = Manifest::from_json(r#"{"nodes": [{"id": "g", "kind": "folder"}]}"#).unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
        let manifest = Manifest::from_json(r#"{"nodes": [{"id": "g"}]}"#).unwrap();
        assert_eq!(manifest.nodes[0].kind, ManifestNodeKind::Group);
    }

    #[test]
    fn check_flags_a_part_without_a_texture() {
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        m.add_node(
            &root,
            ModelNode::new(
                "ghost",
                ModelNodeKind::Part(ModelPart::new(ClmMesh::default())),
            ),
            &mut SeededHex::new(0),
        )
        .unwrap();
        let warnings = m.check();
        assert!(warnings.iter().any(|w| w.message.contains("no texture")));
    }
}
