//! The shared budget a load charges what it is about to allocate against.
//!
//! [`LoadLimits`] and [`LoadResource`] are `#[non_exhaustive]` so adding a
//! resource stays an additive change.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
    ManifestGridCells,
    ResponseInputs,
    Slots,
    StructureBytes,
    SpineJoints,
    WeldPairs,
    /// Bytes: a JSON value's compact-text length, a byte value's size.
    ExtensionBytes,
    // The resources below are checked against a fixed cap where they are
    // read rather than charged, so a budget does not limit them.
    /// [`MAX_TEXTURE_DIMENSION`].
    TextureDimension,
    /// [`MAX_BINDING_GRID_CELLS`].
    BindingGrid,
    /// [`MAX_TREE_DEPTH`].
    TreeDepth,
    ManifestGridColumns,
    ManifestGridRows,
    GridDivisions,
    GridLines,
    ContourMargin,
    ContourRings,
    VertexCoordinates,
}

pub const MAX_TEXTURE_DIMENSION: u32 = 8_192;

impl LoadResource {
    const COUNT: usize = Self::VertexCoordinates as usize + 1;

    /// The name editor clients receive in a limit error.
    #[must_use]
    pub const fn name(self) -> &'static str {
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
            Self::ManifestGridCells => "manifest grid cells",
            Self::ResponseInputs => "response inputs",
            Self::Slots => "slots",
            Self::StructureBytes => "structure bytes",
            Self::SpineJoints => "spine joints",
            Self::WeldPairs => "weld pairs",
            Self::ExtensionBytes => "extension bytes",
            Self::TextureDimension => "texture dimension",
            Self::BindingGrid => "binding grid",
            Self::TreeDepth => "tree depth",
            Self::ManifestGridColumns => "manifest grid columns",
            Self::ManifestGridRows => "manifest grid rows",
            Self::GridDivisions => "grid divisions",
            Self::GridLines => "grid lines",
            Self::ContourMargin => "contour margin",
            Self::ContourRings => "contour rings",
            Self::VertexCoordinates => "vertex coordinates",
        }
    }
}

impl std::fmt::Display for LoadResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
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
    pub manifest_grid_cells: u64,
    pub response_inputs: u64,
    pub slots: u64,
    /// Checked before parsing: a section decodes to as much as ~30x its size,
    /// and the counts below are read too late to bound that. Default 64 MiB,
    /// about 8x the largest committed model's structure.
    pub structure_bytes: u64,
    pub spine_joints: u64,
    pub weld_pairs: u64,
    pub extension_bytes: u64,
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
            manifest_grid_cells: 1_000_000,
            response_inputs: 1_000_000,
            slots: 4_000_000,
            structure_bytes: 64 * 1024 * 1024,
            spine_joints: 1_000_000,
            weld_pairs: 4_000_000,
            extension_bytes: 512 * 1024 * 1024,
        }
    }
}

impl LoadLimits {
    const fn of(&self, resource: LoadResource) -> u64 {
        match resource {
            LoadResource::EncodedBytes => self.encoded_bytes,
            LoadResource::Textures => self.textures,
            LoadResource::DecodedTextureBytes => self.decoded_texture_bytes,
            LoadResource::Nodes => self.nodes,
            LoadResource::Params => self.params,
            LoadResource::Vertices => self.vertices,
            LoadResource::Indices => self.indices,
            LoadResource::BindingCells => self.binding_cells,
            LoadResource::DeformOffsets => self.deform_offsets,
            LoadResource::ManifestGridCells => self.manifest_grid_cells,
            LoadResource::ResponseInputs => self.response_inputs,
            LoadResource::Slots => self.slots,
            LoadResource::StructureBytes => self.structure_bytes,
            LoadResource::SpineJoints => self.spine_joints,
            LoadResource::WeldPairs => self.weld_pairs,
            LoadResource::ExtensionBytes => self.extension_bytes,
            LoadResource::TextureDimension
            | LoadResource::BindingGrid
            | LoadResource::TreeDepth
            | LoadResource::ManifestGridColumns
            | LoadResource::ManifestGridRows
            | LoadResource::GridDivisions
            | LoadResource::GridLines
            | LoadResource::ContourMargin
            | LoadResource::ContourRings
            | LoadResource::VertexCoordinates => u64::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{resource} exceeded load limit: {got} > {limit}")]
pub struct LoadLimitError {
    pub resource: LoadResource,
    pub limit: u64,
    pub got: u64,
}

#[derive(Debug, Clone)]
pub struct LoadBudget {
    limits: LoadLimits,
    used: [u64; LoadResource::COUNT],
}

impl Default for LoadBudget {
    fn default() -> Self {
        Self::new(LoadLimits::default())
    }
}

impl LoadBudget {
    #[must_use]
    pub const fn new(limits: LoadLimits) -> Self {
        Self {
            limits,
            used: [0; LoadResource::COUNT],
        }
    }

    pub fn charge(&mut self, resource: LoadResource, amount: u64) -> Result<(), LoadLimitError> {
        let limit = self.limits.of(resource);
        let used = &mut self.used[resource as usize];
        let got = used.checked_add(amount).ok_or(LoadLimitError {
            resource,
            limit,
            got: u64::MAX,
        })?;
        if got > limit {
            return Err(LoadLimitError {
                resource,
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
            resource,
            limit: self.limits.of(resource),
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
                resource: LoadResource::TextureDimension,
                limit: u64::from(MAX_TEXTURE_DIMENSION),
                got: u64::from(width.max(height)),
            });
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(LoadLimitError {
                resource: LoadResource::DecodedTextureBytes,
                limit: self.limits.decoded_texture_bytes,
                got: u64::MAX,
            })?;
        self.charge_product(LoadResource::DecodedTextureBytes, pixels, 4)?;
        Ok(())
    }
}

pub const MAX_BINDING_GRID_CELLS: u64 = 65_536;

pub(crate) fn binding_grid_cells(
    mut axis_lengths: impl Iterator<Item = u64>,
) -> Result<u64, LoadLimitError> {
    let cells = axis_lengths.try_fold(1u64, u64::checked_mul);
    match cells {
        Some(cells) if cells <= MAX_BINDING_GRID_CELLS => Ok(cells),
        got => Err(LoadLimitError {
            resource: LoadResource::BindingGrid,
            limit: MAX_BINDING_GRID_CELLS,
            got: got.unwrap_or(u64::MAX),
        }),
    }
}

/// Levels on the longest root-to-leaf path, both ends counted: a lone root
/// is one level deep.
pub const MAX_TREE_DEPTH: u64 = 256;

pub(crate) const fn tree_depth(levels: usize) -> Result<(), LoadLimitError> {
    let got = levels as u64;
    if got > MAX_TREE_DEPTH {
        return Err(LoadLimitError {
            resource: LoadResource::TreeDepth,
            limit: MAX_TREE_DEPTH,
            got,
        });
    }
    Ok(())
}

/// Charges payload bytes too, for a `ClmFile` built in memory; a file read
/// from bytes already charged them as `encoded_bytes` and must use
/// `charge_decoded_clm` instead, or every texture byte counts twice.
pub fn charge_clm_file(
    file: &crate::formats::clm::ClmFile,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    charge_texture_payloads(file.textures.iter().map(|t| t.data.len() as u64), budget)?;
    for blob in &file.extensions {
        budget.charge(LoadResource::EncodedBytes, blob.data.len() as u64)?;
    }
    for section in &file.unknown_sections {
        budget.charge(LoadResource::EncodedBytes, section.data.len() as u64)?;
    }
    charge_clm_structure(&file.doc, budget)
}

pub(crate) fn charge_decoded_clm(
    doc: &crate::formats::clm::ClmStructure,
    textures: usize,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    budget.charge(LoadResource::Textures, textures as u64)?;
    charge_clm_structure(doc, budget)
}

pub fn charge_texture_payloads(
    sizes: impl Iterator<Item = u64>,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    let mut count = 0u64;
    for size in sizes {
        count += 1;
        budget.charge(LoadResource::EncodedBytes, size)?;
    }
    budget.charge(LoadResource::Textures, count)
}

pub fn charge_clm_structure(
    doc: &crate::formats::clm::ClmStructure,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    use crate::formats::clm::{ClmBindingValues, ClmExtension, ClmMesh, ClmNodeKind};
    use std::collections::HashMap;

    budget.charge(LoadResource::Nodes, doc.nodes.len() as u64)?;
    budget.charge(LoadResource::Params, doc.params.len() as u64)?;

    let mut meshes: HashMap<&crate::id::NodeId, &ClmMesh> = HashMap::new();
    for node in &doc.nodes {
        let mesh = match &node.kind {
            ClmNodeKind::MotionResponse(r) => {
                budget.charge(LoadResource::ResponseInputs, r.inputs.len() as u64)?;
                None
            }
            ClmNodeKind::Part(part) => {
                budget.charge(LoadResource::Slots, part.slots.len() as u64)?;
                Some(&part.mesh)
            }
            ClmNodeKind::MeshGroup(group) => Some(&group.mesh),
            ClmNodeKind::Spine(spine) => {
                budget.charge(LoadResource::SpineJoints, spine.joints.len() as u64)?;
                budget.charge(LoadResource::SpineJoints, spine.targets.len() as u64)?;
                None
            }
            _ => None,
        };
        let Some(mesh) = mesh else { continue };
        meshes.insert(&node.id, mesh);
        budget.charge(LoadResource::Vertices, mesh.vertex_count() as u64)?;
        budget.charge(LoadResource::Indices, index_count(mesh) as u64)?;
    }

    for binding in &doc.bindings {
        // Axes travel with the binding, including fragments referencing a
        // base param, so their allocation charge never depends on resolution.
        let cells = binding_grid_cells(binding.key_positions.iter().map(|axis| axis.len() as u64))?;
        let authored = match &binding.values {
            ClmBindingValues::Deform(values) => values.cells.len(),
            other => other.scalar_cells().map_or(0, <[_]>::len),
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

    budget.charge(LoadResource::WeldPairs, doc.welds.len() as u64)?;
    for weld in &doc.welds {
        budget.charge(LoadResource::WeldPairs, weld.pairs.len() as u64)?;
    }

    // A byte value is charged by what its marker states: the marker is what
    // the reader holds the payload to, in a file and in a structure feed
    // alike, so the charge does not depend on which one this is.
    for value in doc.extensions.values() {
        let size = match (value.marker(), value.inline_json()) {
            (Some(marker), _) => marker.size,
            (None, Some(json)) => crate::formats::clm::extension_json_len(json) as u64,
            (None, None) => 0,
        };
        budget.charge(LoadResource::ExtensionBytes, size)?;
        if let ClmExtension::Maintained { refs, .. } = value {
            let refs: usize = refs
                .iter()
                .map(super::extension::IdRef::encoded_len_hint)
                .sum();
            budget.charge(LoadResource::ExtensionBytes, refs as u64)?;
        }
    }
    Ok(())
}

const fn index_count(mesh: &crate::formats::clm::ClmMesh) -> usize {
    match &mesh.indices {
        crate::formats::clm::ClmIndices::U16(indices) => indices.len(),
        crate::formats::clm::ClmIndices::U32(indices) => indices.len(),
    }
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
        assert_eq!(err.resource, LoadResource::Nodes);
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

    #[test]
    fn an_oversized_binding_grid_reports_its_cell_count() {
        let err = binding_grid_cells([256, 256, 2].into_iter()).unwrap_err();
        assert_eq!(
            (err.resource, err.limit, err.got),
            (LoadResource::BindingGrid, MAX_BINDING_GRID_CELLS, 131_072)
        );
        let err = binding_grid_cells([u64::MAX, 2].into_iter()).unwrap_err();
        assert_eq!(err.got, u64::MAX);
    }
}
