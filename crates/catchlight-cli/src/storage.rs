//! Storage accounting without decoding images or evaluating a puppet.
//!
//! Container section lengths and texture payload lengths are read from the
//! input. Element sizes count the bytes the current CBOR serializer writes,
//! including each element's own headers. They partition the **canonical**
//! file; array/map headers and field names live in explicit overhead buckets.
//! `canonical_matches_input` is a byte comparison, not a size comparison. Only
//! when it is true are the canonical element counts exact on-disk attribution.
//! Historical formats, unknown fields and noncanonical CBOR remain inspectable
//! without silently presenting re-encoded sizes as original storage sizes.
//!
//! Texture references are not charged repeatedly to nodes. The texture table
//! lists their users separately. Node rows include that node's mesh and bindings,
//! never its descendants. All detailed lists are sorted largest first.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;

use catchlight_core::formats::{
    clm::{self, ClmNodeKind},
    container,
};
use catchlight_core::{deform_cells, scalar_cells, target_of};
use serde::Serialize;

use crate::{file, Error};

#[derive(Debug, Serialize)]
pub struct Size {
    pub name: String,
    pub bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct Texture {
    pub id: String,
    pub encoding: clm::TextureEncoding,
    pub bytes: usize,
    pub dimensions: Option<[u32; 2]>,
    pub parts: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Mesh {
    pub node: String,
    pub bytes: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub position_bytes: usize,
    pub uv_bytes: usize,
    pub index_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct Binding {
    pub node: String,
    pub params: Vec<String>,
    pub target: &'static str,
    pub bytes: usize,
    pub cells: usize,
    pub key_positions: Vec<usize>,
    pub deform_vectors: usize,
    pub zero_deform_vectors: usize,
    pub uniform_deform_cells: usize,
}

#[derive(Debug, Serialize)]
pub struct Element {
    pub id: String,
    pub name: String,
    pub mesh_bytes: usize,
    pub binding_bytes: usize,
    pub node_properties_bytes: usize,
    pub total_bytes: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub stored_bytes: usize,
    pub format_version: u16,
    pub stored_sections: Vec<Size>,
    pub canonical_bytes: usize,
    pub canonical_matches_input: bool,
    pub canonical_categories: Vec<Size>,
    pub textures: Vec<Texture>,
    pub elements: Vec<Element>,
    pub meshes: Vec<Mesh>,
    pub bindings: Vec<Binding>,
}

pub fn run(path: &Path) -> Result<Report, Error> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    inspect(&bytes, path)
}

pub fn inspect(bytes: &[u8], path: &Path) -> Result<Report, Error> {
    let input = container::read(bytes, &clm::MAGIC).map_err(|source| Error::Decode {
        path: path.into(),
        source: source.into(),
    })?;
    let model = clm::decode(bytes).map_err(|source| Error::Decode {
        path: path.into(),
        source,
    })?;
    let canonical = file::encode(&model, path)?;
    let mut categories = BTreeMap::<String, usize>::new();
    let mut elements = BTreeMap::new();
    let mut meshes = Vec::new();
    let mut bindings = Vec::new();
    let mut users = BTreeMap::<String, Vec<String>>::new();
    let mut node_bytes = 0;
    let mut mesh_bytes = 0;
    let mut binding_bytes = 0;
    let mut deform_bytes = 0;

    for node in &model.doc.nodes {
        let total = encoded_size(node, path)?;
        let mesh = match &node.kind {
            ClmNodeKind::Part(part) => {
                if let Some(texture) = &part.albedo {
                    users
                        .entry(texture.to_string())
                        .or_default()
                        .push(node.id.to_string());
                }
                Some(&part.mesh)
            }
            ClmNodeKind::MeshGroup(group) => Some(&group.mesh),
            _ => None,
        };
        let size = if let Some(mesh) = mesh {
            let size = encoded_size(mesh, path)?;
            meshes.push(Mesh {
                node: node.id.to_string(),
                bytes: size,
                vertices: mesh.vertex_count(),
                triangles: mesh.triangle_count(),
                position_bytes: encoded_size(&mesh.verts, path)?,
                uv_bytes: encoded_size(&mesh.uvs, path)?,
                index_bytes: encoded_size(&mesh.indices, path)?,
            });
            size
        } else {
            0
        };
        node_bytes += total;
        mesh_bytes += size;
        elements.insert(
            node.id.to_string(),
            Element {
                id: node.id.to_string(),
                name: node.name.clone(),
                mesh_bytes: size,
                binding_bytes: 0,
                node_properties_bytes: total - size,
                total_bytes: total,
            },
        );
    }
    for binding in &model.doc.bindings {
        let size = encoded_size(binding, path)?;
        binding_bytes += size;
        let mut row = Binding {
            node: binding.node.to_string(),
            params: binding.params.iter().map(ToString::to_string).collect(),
            target: target_of(&binding.values).name(),
            bytes: size,
            cells: 0,
            key_positions: binding.key_positions.iter().map(Vec::len).collect(),
            deform_vectors: 0,
            zero_deform_vectors: 0,
            uniform_deform_cells: 0,
        };
        if let Some(cells) = deform_cells(&binding.values) {
            deform_bytes += size;
            row.cells = cells.len();
            for cell in cells {
                let (pairs, _) = cell.value.as_chunks::<2>();
                row.deform_vectors += pairs.len();
                row.zero_deform_vectors += pairs.iter().filter(|p| **p == [0.0, 0.0]).count();
                row.uniform_deform_cells += usize::from(
                    pairs
                        .first()
                        .is_some_and(|first| pairs.iter().all(|p| p == first)),
                );
            }
        } else if let Some(cells) = scalar_cells(&binding.values) {
            row.cells = cells.len();
        }
        if let Some(element) = elements.get_mut(binding.node.as_str()) {
            element.binding_bytes += size;
            element.total_bytes += size;
        }
        bindings.push(row);
    }
    categories.insert("base_meshes".into(), mesh_bytes);
    categories.insert("node_properties".into(), node_bytes - mesh_bytes);
    categories.insert("deform_bindings".into(), deform_bytes);
    categories.insert("other_bindings".into(), binding_bytes - deform_bytes);
    categories.insert(
        "param_definitions".into(),
        encoded_size(&model.doc.params, path)?,
    );
    categories.insert(
        "physics_settings".into(),
        encoded_size(&model.doc.physics, path)?,
    );
    categories.insert("welds".into(), encoded_size(&model.doc.welds, path)?);
    categories.insert(
        "animations".into(),
        encoded_size(&model.doc.animations, path)?,
    );
    if !model.doc.extensions.is_empty() {
        categories.insert(
            "inline_extensions".into(),
            encoded_size(&model.doc.extensions, path)?,
        );
    }
    let structure = encoded_size(&model.doc, path)?;
    let attributed: usize = categories.values().sum();
    categories.insert(
        "structure_overhead".into(),
        structure
            .checked_sub(attributed)
            .ok_or_else(|| Error::Storage("element sizes exceed the structure".into()))?,
    );
    let mut textures: Vec<_> = model
        .textures
        .iter()
        .map(|t| Texture {
            id: t.id.to_string(),
            encoding: t.encoding,
            bytes: t.data.len(),
            dimensions: png_dimensions(&t.data),
            parts: users.remove(t.id.as_str()).unwrap_or_default(),
        })
        .collect();
    let payloads: usize = textures.iter().map(|t| t.bytes).sum();
    let texture_table = encoded_size(&model.textures, path)?;
    categories.insert("texture_payloads".into(), payloads);
    categories.insert("texture_table_overhead".into(), texture_table - payloads);
    let extensions = if model.extensions.is_empty() {
        0
    } else {
        encoded_size(&model.extensions, path)?
    };
    categories.insert("binary_extensions".into(), extensions);
    categories.insert(
        "container_overhead".into(),
        canonical.len() - structure - texture_table - extensions,
    );

    let stored_sections = input
        .sections
        .iter()
        .map(|section| Size {
            name: match section.kind {
                0 => "structure".into(),
                1 => "textures".into(),
                2 => "texture_manifest".into(),
                3 => "extensions".into(),
                other => format!("unknown_{other}"),
            },
            bytes: section.data.len(),
        })
        .collect();
    let mut canonical_categories: Vec<_> = categories
        .into_iter()
        .map(|(name, bytes)| Size { name, bytes })
        .collect();
    canonical_categories.sort_by_key(|s| std::cmp::Reverse(s.bytes));
    textures.sort_by_key(|t| std::cmp::Reverse(t.bytes));
    meshes.sort_by_key(|m| std::cmp::Reverse(m.bytes));
    bindings.sort_by_key(|b| std::cmp::Reverse(b.bytes));
    let mut elements: Vec<_> = elements.into_values().collect();
    elements.sort_by_key(|e| std::cmp::Reverse(e.total_bytes));
    Ok(Report {
        stored_bytes: bytes.len(),
        format_version: input.version,
        stored_sections,
        canonical_bytes: canonical.len(),
        canonical_matches_input: canonical == bytes,
        canonical_categories,
        textures,
        elements,
        meshes,
        bindings,
    })
}

/// Read only the PNG header, including for a file whose pixels cannot decode.
fn png_dimensions(data: &[u8]) -> Option<[u32; 2]> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" || data.get(12..16)? != b"IHDR" {
        return None;
    }
    Some([
        u32::from_be_bytes(data.get(16..20)?.try_into().ok()?),
        u32::from_be_bytes(data.get(20..24)?.try_into().ok()?),
    ])
}

#[derive(Default)]
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encoded_size<T: Serialize>(value: &T, path: &Path) -> Result<usize, Error> {
    let mut counter = Counter::default();
    ciborium::into_writer(value, &mut counter).map_err(|source| Error::Cbor {
        path: path.into(),
        source,
    })?;
    Ok(counter.0)
}
