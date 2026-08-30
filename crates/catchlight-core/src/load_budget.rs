#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadResource {
    EncodedBytes,
    Textures,
    DecodedTextureBytes,
    Nodes,
    Params,
    Vertices,
    Indices,
    BindingCells,
    DeformOffsets,
    MeshGroupBitmapCells,
    ManifestGridCells,
    SeamSlots,
}

pub const MAX_TEXTURE_DIMENSION: u32 = 8_192;

impl LoadResource {
    fn name(self) -> &'static str {
        match self {
            Self::EncodedBytes => "encoded bytes",
            Self::Textures => "textures",
            Self::DecodedTextureBytes => "decoded texture bytes",
            Self::Nodes => "nodes",
            Self::Params => "params",
            Self::Vertices => "vertices",
            Self::Indices => "indices",
            Self::BindingCells => "binding cells",
            Self::DeformOffsets => "deform offsets",
            Self::MeshGroupBitmapCells => "mesh-group bitmap cells",
            Self::ManifestGridCells => "manifest grid cells",
            Self::SeamSlots => "seam slots",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadLimits {
    pub encoded_bytes: u64,
    pub textures: u64,
    pub decoded_texture_bytes: u64,
    pub nodes: u64,
    pub params: u64,
    pub vertices: u64,
    pub indices: u64,
    pub binding_cells: u64,
    pub deform_offsets: u64,
    pub mesh_group_bitmap_cells: u64,
    pub manifest_grid_cells: u64,
    pub seam_slots: u64,
}

impl Default for LoadLimits {
    fn default() -> Self {
        Self {
            encoded_bytes: 512 * 1024 * 1024,
            textures: 4_096,
            decoded_texture_bytes: 512 * 1024 * 1024,
            nodes: 131_072,
            params: 65_536,
            vertices: 16_000_000,
            indices: 48_000_000,
            binding_cells: 16_000_000,
            deform_offsets: 64_000_000,
            mesh_group_bitmap_cells: 32_000_000,
            manifest_grid_cells: 1_000_000,
            seam_slots: 4_000_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{resource} exceeded load limit: {got} > {limit}")]
pub struct LoadLimitError {
    pub resource: &'static str,
    pub limit: u64,
    pub got: u64,
}

#[derive(Debug, Clone)]
pub struct LoadBudget {
    limits: LoadLimits,
    used: LoadLimits,
}

impl Default for LoadBudget {
    fn default() -> Self {
        Self::new(LoadLimits::default())
    }
}

impl LoadBudget {
    pub fn new(limits: LoadLimits) -> Self {
        Self {
            limits,
            used: LoadLimits {
                encoded_bytes: 0,
                textures: 0,
                decoded_texture_bytes: 0,
                nodes: 0,
                params: 0,
                vertices: 0,
                indices: 0,
                binding_cells: 0,
                deform_offsets: 0,
                mesh_group_bitmap_cells: 0,
                manifest_grid_cells: 0,
                seam_slots: 0,
            },
        }
    }

    pub fn charge(&mut self, resource: LoadResource, amount: u64) -> Result<(), LoadLimitError> {
        let (used, limit) = match resource {
            LoadResource::EncodedBytes => (&mut self.used.encoded_bytes, self.limits.encoded_bytes),
            LoadResource::Textures => (&mut self.used.textures, self.limits.textures),
            LoadResource::DecodedTextureBytes => (
                &mut self.used.decoded_texture_bytes,
                self.limits.decoded_texture_bytes,
            ),
            LoadResource::Nodes => (&mut self.used.nodes, self.limits.nodes),
            LoadResource::Params => (&mut self.used.params, self.limits.params),
            LoadResource::Vertices => (&mut self.used.vertices, self.limits.vertices),
            LoadResource::Indices => (&mut self.used.indices, self.limits.indices),
            LoadResource::BindingCells => (&mut self.used.binding_cells, self.limits.binding_cells),
            LoadResource::DeformOffsets => {
                (&mut self.used.deform_offsets, self.limits.deform_offsets)
            }
            LoadResource::MeshGroupBitmapCells => (
                &mut self.used.mesh_group_bitmap_cells,
                self.limits.mesh_group_bitmap_cells,
            ),
            LoadResource::ManifestGridCells => (
                &mut self.used.manifest_grid_cells,
                self.limits.manifest_grid_cells,
            ),
            LoadResource::SeamSlots => (&mut self.used.seam_slots, self.limits.seam_slots),
        };
        let got = used.checked_add(amount).ok_or_else(|| LoadLimitError {
            resource: resource.name(),
            limit,
            got: u64::MAX,
        })?;
        if got > limit {
            return Err(LoadLimitError {
                resource: resource.name(),
                limit,
                got,
            });
        }
        *used = got;
        Ok(())
    }

    pub fn charge_product(
        &mut self,
        resource: LoadResource,
        left: u64,
        right: u64,
    ) -> Result<u64, LoadLimitError> {
        let amount = left.checked_mul(right).ok_or_else(|| LoadLimitError {
            resource: resource.name(),
            limit: self.limit(resource),
            got: u64::MAX,
        })?;
        self.charge(resource, amount)?;
        Ok(amount)
    }

    pub fn check_texture_dimensions(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<(), LoadLimitError> {
        if width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION {
            return Err(LoadLimitError {
                resource: "texture dimension",
                limit: MAX_TEXTURE_DIMENSION as u64,
                got: width.max(height) as u64,
            });
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(LoadLimitError {
                resource: "decoded texture bytes",
                limit: self.limits.decoded_texture_bytes,
                got: u64::MAX,
            })?;
        self.charge_product(LoadResource::DecodedTextureBytes, pixels, 4)?;
        Ok(())
    }

    fn limit(&self, resource: LoadResource) -> u64 {
        match resource {
            LoadResource::EncodedBytes => self.limits.encoded_bytes,
            LoadResource::Textures => self.limits.textures,
            LoadResource::DecodedTextureBytes => self.limits.decoded_texture_bytes,
            LoadResource::Nodes => self.limits.nodes,
            LoadResource::Params => self.limits.params,
            LoadResource::Vertices => self.limits.vertices,
            LoadResource::Indices => self.limits.indices,
            LoadResource::BindingCells => self.limits.binding_cells,
            LoadResource::DeformOffsets => self.limits.deform_offsets,
            LoadResource::MeshGroupBitmapCells => self.limits.mesh_group_bitmap_cells,
            LoadResource::ManifestGridCells => self.limits.manifest_grid_cells,
            LoadResource::SeamSlots => self.limits.seam_slots,
        }
    }
}

pub const MAX_PARAM_GRID_CELLS: u64 = 65_536;

/// Charge a decoded `.clm` against the shared budget before any of it is
/// turned into a [`Model`](crate::Model): the counts and products a hostile
/// file could make enormous are all here, and each is checked against the
/// limit before it is used to size anything.
pub fn charge_clm_structure(
    file: &crate::formats::clm::ClmFile,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    use crate::formats::clm::{ClmBindingValues, ClmMesh, ClmNodeKind};
    use std::collections::HashMap;

    let doc = &file.doc;
    budget.charge(LoadResource::Textures, file.textures.len() as u64)?;
    budget.charge(LoadResource::Nodes, doc.nodes.len() as u64)?;
    budget.charge(LoadResource::Params, doc.params.len() as u64)?;
    for texture in &file.textures {
        budget.charge(LoadResource::EncodedBytes, texture.data.len() as u64)?;
    }

    let mut meshes: HashMap<&crate::id::NodeId, &ClmMesh> = HashMap::new();
    for node in &doc.nodes {
        let mesh = match &node.kind {
            ClmNodeKind::Part(part) => {
                for seam in &part.seams {
                    budget.charge(LoadResource::SeamSlots, seam.slots.len() as u64)?;
                }
                Some(&part.mesh)
            }
            ClmNodeKind::MeshGroup(group) => Some(&group.mesh),
            _ => None,
        };
        let Some(mesh) = mesh else { continue };
        meshes.insert(&node.id, mesh);
        budget.charge(LoadResource::Vertices, mesh.vertex_count() as u64)?;
        budget.charge(LoadResource::Indices, index_count(mesh) as u64)?;
        if matches!(node.kind, ClmNodeKind::MeshGroup(_)) {
            budget.charge(
                LoadResource::MeshGroupBitmapCells,
                mesh_group_bitmap_cells(mesh),
            )?;
        }
    }

    let keys: HashMap<&crate::id::ParamId, u64> = doc
        .params
        .iter()
        .map(|p| (&p.id, p.key_positions.len().max(1) as u64))
        .collect();
    for binding in &doc.bindings {
        // A binding over a param the file does not carry is a load error; the
        // reader reports it, and one key position is the safe charge here.
        let cells = binding
            .params
            .iter()
            .map(|p| keys.get(p).copied().unwrap_or(1))
            .try_fold(1u64, |acc, k| acc.checked_mul(k))
            .filter(|cells| *cells <= MAX_PARAM_GRID_CELLS)
            .ok_or(LoadLimitError {
                resource: "param grid",
                limit: MAX_PARAM_GRID_CELLS,
                got: u64::MAX,
            })?;
        let authored = match &binding.values {
            ClmBindingValues::Deform(values) => values.cells.len(),
            other => crate::model::scalar_cells(other).map_or(0, <[_]>::len),
        };
        budget.charge(LoadResource::BindingCells, cells)?;
        budget.charge(LoadResource::BindingCells, authored as u64)?;
        if let ClmBindingValues::Deform(values) = &binding.values {
            let authored_vertices = values
                .cells
                .iter()
                .map(|cell| cell.value.len() / 2)
                .max()
                .unwrap_or(0);
            let mesh_vertices = meshes.get(&binding.node).map_or(0, |m| m.vertex_count());
            budget.charge_product(
                LoadResource::DeformOffsets,
                cells,
                authored_vertices.max(mesh_vertices) as u64,
            )?;
        }
    }
    Ok(())
}

fn index_count(mesh: &crate::formats::clm::ClmMesh) -> usize {
    match &mesh.indices {
        crate::formats::clm::ClmIndices::U16(indices) => indices.len(),
        crate::formats::clm::ClmIndices::U32(indices) => indices.len(),
    }
}

fn mesh_group_bitmap_cells(mesh: &crate::formats::clm::ClmMesh) -> u64 {
    let index_count = match &mesh.indices {
        crate::formats::clm::ClmIndices::U16(indices) => indices.len(),
        crate::formats::clm::ClmIndices::U32(indices) => indices.len(),
    };
    if mesh.verts.len() < 2 || index_count < 3 || index_count / 3 + 1 > u16::MAX as usize {
        return 0;
    }
    let mut min = glam::Vec2::splat(f32::MAX);
    let mut max = glam::Vec2::splat(f32::MIN);
    let origin = glam::Vec2::from_array(mesh.origin);
    for pair in mesh.verts.as_chunks::<2>().0 {
        let point = glam::Vec2::new(pair[0], pair[1]) - origin;
        min = min.min(point);
        max = max.max(point);
    }
    if !min.x.is_finite() || !min.y.is_finite() || !max.x.is_finite() || !max.y.is_finite() {
        return 0;
    }
    let width = (max.x.ceil() - min.x.floor() + 1.0).max(1.0) as u32;
    let height = (max.y.ceil() - min.y.floor() + 1.0).max(1.0) as u32;
    if width > 4_096 || height > 4_096 {
        return 0;
    }
    u64::from(width) * u64::from(height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_charges_fail_before_mutating_usage() {
        let mut budget = LoadBudget::new(LoadLimits {
            nodes: 3,
            ..LoadLimits::default()
        });
        budget.charge(LoadResource::Nodes, 2).unwrap();
        let err = budget.charge(LoadResource::Nodes, 2).unwrap_err();
        assert_eq!(err.resource, "nodes");
        assert_eq!(err.got, 4);
        budget.charge(LoadResource::Nodes, 1).unwrap();
    }

    #[test]
    fn checked_products_report_overflow_as_a_limit_error() {
        let mut budget = LoadBudget::default();
        let err = budget
            .charge_product(LoadResource::DeformOffsets, u64::MAX, 2)
            .unwrap_err();
        assert_eq!(err.got, u64::MAX);
    }
}
