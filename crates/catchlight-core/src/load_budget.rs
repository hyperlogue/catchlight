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
        }
    }
}

pub const MAX_PARAM_GRID_CELLS: u64 = 65_536;

pub fn charge_clp_structure(
    file: &crate::formats::clp::ClpFile,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    use crate::formats::clp::{ClpBindingValues, ClpIndices, ClpNodeKind};

    budget.charge(LoadResource::Textures, file.textures.len() as u64)?;
    budget.charge(LoadResource::Nodes, file.doc.nodes.len() as u64)?;
    budget.charge(LoadResource::Params, file.doc.params.len() as u64)?;
    for texture in &file.textures {
        budget.charge(LoadResource::EncodedBytes, texture.data.len() as u64)?;
    }

    for node in &file.doc.nodes {
        let mesh = match &node.kind {
            ClpNodeKind::Part(part) => Some(&part.mesh),
            ClpNodeKind::MeshGroup(group) => Some(&group.mesh),
            _ => None,
        };
        let Some(mesh) = mesh else { continue };
        budget.charge(LoadResource::Vertices, (mesh.verts.len() / 2) as u64)?;
        let index_count = match &mesh.indices {
            ClpIndices::U16(indices) => indices.len(),
            ClpIndices::U32(indices) => indices.len(),
        };
        budget.charge(LoadResource::Indices, index_count as u64)?;
        if matches!(node.kind, ClpNodeKind::MeshGroup(_)) {
            budget.charge(
                LoadResource::MeshGroupBitmapCells,
                clp_mesh_group_bitmap_cells(mesh),
            )?;
        }
    }

    for param in &file.doc.params {
        let width = param.axis_points_x.len().max(1) as u64;
        let height = param.axis_points_y.len().max(1) as u64;
        let cells = width.checked_mul(height).ok_or(LoadLimitError {
            resource: "param grid",
            limit: MAX_PARAM_GRID_CELLS,
            got: u64::MAX,
        })?;
        if cells > MAX_PARAM_GRID_CELLS {
            return Err(LoadLimitError {
                resource: "param grid",
                limit: MAX_PARAM_GRID_CELLS,
                got: cells,
            });
        }
        for binding in &param.bindings {
            let Some(node) = file.doc.nodes.get(binding.node as usize) else {
                continue;
            };
            let authored = match &binding.values {
                ClpBindingValues::Deform(values) => values.cells.len(),
                ClpBindingValues::ZSort(values)
                | ClpBindingValues::TransformTX(values)
                | ClpBindingValues::TransformTY(values)
                | ClpBindingValues::TransformSX(values)
                | ClpBindingValues::TransformSY(values)
                | ClpBindingValues::TransformRX(values)
                | ClpBindingValues::TransformRY(values)
                | ClpBindingValues::TransformRZ(values)
                | ClpBindingValues::Opacity(values)
                | ClpBindingValues::TintR(values)
                | ClpBindingValues::TintG(values)
                | ClpBindingValues::TintB(values)
                | ClpBindingValues::ScreenTintR(values)
                | ClpBindingValues::ScreenTintG(values)
                | ClpBindingValues::ScreenTintB(values)
                | ClpBindingValues::OutputScaleX(values)
                | ClpBindingValues::OutputScaleY(values) => values.cells.len(),
            };
            budget.charge(LoadResource::BindingCells, cells)?;
            budget.charge(LoadResource::BindingCells, authored as u64)?;
            if let ClpBindingValues::Deform(values) = &binding.values {
                let authored_vertices = values
                    .cells
                    .iter()
                    .map(|cell| cell.value.len() / 2)
                    .max()
                    .unwrap_or(0);
                let mesh_vertices = match &node.kind {
                    ClpNodeKind::Part(part) => part.mesh.verts.len() / 2,
                    ClpNodeKind::MeshGroup(group) => group.mesh.verts.len() / 2,
                    _ => 0,
                };
                budget.charge_product(
                    LoadResource::DeformOffsets,
                    cells,
                    authored_vertices.max(mesh_vertices) as u64,
                )?;
            }
        }
    }
    Ok(())
}

fn clp_mesh_group_bitmap_cells(mesh: &crate::formats::clp::ClpMesh) -> u64 {
    let index_count = match &mesh.indices {
        crate::formats::clp::ClpIndices::U16(indices) => indices.len(),
        crate::formats::clp::ClpIndices::U32(indices) => indices.len(),
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
