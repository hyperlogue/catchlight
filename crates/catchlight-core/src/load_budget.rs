//! The shared budget a load charges what it is about to allocate against.
//!
//! [`LoadLimits`] and [`LoadResource`] are `#[non_exhaustive]`: every new
//! thing a file can make large gets a resource and a limit here, and that
//! has to stay an additive change. A caller outside this crate starts from
//! [`LoadLimits::default`] and assigns the fields it wants to move.

/// One thing a load counts. Each has a limit of the same name in
/// [`LoadLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The encoded `Structure` section, charged before it is parsed. See
    /// [`LoadLimits::structure_bytes`].
    StructureBytes,
    /// Every spine's joints and bend targets.
    SpineJoints,
    /// Welds, and the slot pairs they hold.
    WeldPairs,
    /// Animations, their lanes, and the lanes' keyframes.
    Keyframes,
    /// Extension values: a JSON value's length as compact JSON text, a byte
    /// value's size.
    ExtensionBytes,
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
            Self::ManifestGridCells => "manifest grid cells",
            Self::ResponseInputs => "response inputs",
            Self::Slots => "slots",
            Self::StructureBytes => "structure bytes",
            Self::SpineJoints => "spine joints",
            Self::WeldPairs => "weld pairs",
            Self::Keyframes => "animation keyframes",
            Self::ExtensionBytes => "extension bytes",
        }
    }
}

/// The most of each [`LoadResource`] one load may use.
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
    /// The most bytes the encoded `Structure` section may be, checked before
    /// any of it is parsed.
    ///
    /// Every other limit here is a count read *out of* a decoded structure,
    /// which is too late to bound what decoding it allocates: a null is one
    /// byte on the wire and a pointer-sized `Option` in memory, a two-key
    /// node map is a few hundred bytes of struct, so a section decodes to as
    /// much as ~30x its size. `encoded_bytes` bounds the whole file, textures
    /// and all, and is far too large to be that bound — hence this one, on
    /// the one section that is parsed into a tree.
    ///
    /// The default is 64 MiB: about 8x the largest structure among the
    /// committed models (7.6 MB), and a worst case of roughly 2 GiB decoded.
    /// A host with less room than that lowers it.
    pub structure_bytes: u64,
    pub spine_joints: u64,
    pub weld_pairs: u64,
    pub keyframes: u64,
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
            keyframes: 16_000_000,
            extension_bytes: 64 * 1024 * 1024,
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
                manifest_grid_cells: 0,
                response_inputs: 0,
                slots: 0,
                structure_bytes: 0,
                spine_joints: 0,
                weld_pairs: 0,
                keyframes: 0,
                extension_bytes: 0,
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
            LoadResource::ManifestGridCells => (
                &mut self.used.manifest_grid_cells,
                self.limits.manifest_grid_cells,
            ),
            LoadResource::ResponseInputs => {
                (&mut self.used.response_inputs, self.limits.response_inputs)
            }
            LoadResource::Slots => (&mut self.used.slots, self.limits.slots),
            LoadResource::StructureBytes => {
                (&mut self.used.structure_bytes, self.limits.structure_bytes)
            }
            LoadResource::SpineJoints => (&mut self.used.spine_joints, self.limits.spine_joints),
            LoadResource::WeldPairs => (&mut self.used.weld_pairs, self.limits.weld_pairs),
            LoadResource::Keyframes => (&mut self.used.keyframes, self.limits.keyframes),
            LoadResource::ExtensionBytes => {
                (&mut self.used.extension_bytes, self.limits.extension_bytes)
            }
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
            LoadResource::ManifestGridCells => self.limits.manifest_grid_cells,
            LoadResource::ResponseInputs => self.limits.response_inputs,
            LoadResource::Slots => self.limits.slots,
            LoadResource::StructureBytes => self.limits.structure_bytes,
            LoadResource::SpineJoints => self.limits.spine_joints,
            LoadResource::WeldPairs => self.limits.weld_pairs,
            LoadResource::Keyframes => self.limits.keyframes,
            LoadResource::ExtensionBytes => self.limits.extension_bytes,
        }
    }
}

pub const MAX_BINDING_GRID_CELLS: u64 = 65_536;

/// Bound an individual binding grid before allocating its dense values or axes.
pub(crate) fn binding_grid_cells(
    mut axis_lengths: impl Iterator<Item = u64>,
) -> Result<u64, LoadLimitError> {
    axis_lengths
        .try_fold(1u64, |acc, len| acc.checked_mul(len))
        .filter(|cells| *cells <= MAX_BINDING_GRID_CELLS)
        .ok_or(LoadLimitError {
            resource: "binding grid",
            limit: MAX_BINDING_GRID_CELLS,
            got: u64::MAX,
        })
}

/// Charge a decoded `.clm` against the shared budget before any of it is
/// turned into a [`Model`](crate::Model): the counts and products a hostile
/// file could make enormous are all here, and each is checked against its
/// limit before a `Model` or a puppet sizes anything from it.
///
/// It does not bound the decode that produced `file` — that has already
/// happened. What bounds it is the two charges
/// [`decode_with_budget`](crate::formats::clm::decode_with_budget) makes on
/// the encoded bytes before parsing them: `encoded_bytes` for the whole
/// input and `structure_bytes` for the section that becomes a tree.
///
/// This is the charge for a `ClmFile` that arrived *as* a `ClmFile`, built
/// by hand or by an importer, so the payload bytes are charged here. A file
/// read from bytes had all of them charged as part of the input, and its
/// reader charges only what is left — the texture count and the structure's
/// counts: charging the payloads a second time would count every texture
/// byte twice and halve the file size `encoded_bytes` admits.
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

/// What is left to charge for a `.clm` whose encoded bytes were charged
/// whole when they were decoded: how many textures it holds, and the
/// structure's counts. See [`charge_clm_file`] for why not the payloads.
pub(crate) fn charge_decoded_clm(
    doc: &crate::formats::clm::ClmStructure,
    textures: usize,
    budget: &mut LoadBudget,
) -> Result<(), LoadLimitError> {
    budget.charge(LoadResource::Textures, textures as u64)?;
    charge_clm_structure(doc, budget)
}

/// The texture half of [`charge_clm_file`], over payload sizes alone —
/// so a replica applying a structure charges the payloads it sourced from its
/// own store rather than ones it read out of a file.
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

/// The structure half of [`charge_clm_file`]: everything a hostile file
/// could make enormous that is not a texture payload.
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

    budget.charge(LoadResource::WeldPairs, doc.welds.len() as u64)?;
    for weld in &doc.welds {
        budget.charge(LoadResource::WeldPairs, weld.pairs.len() as u64)?;
    }

    budget.charge(LoadResource::Keyframes, doc.animations.len() as u64)?;
    for animation in &doc.animations {
        budget.charge(LoadResource::Keyframes, animation.lanes.len() as u64)?;
        for lane in &animation.lanes {
            budget.charge(LoadResource::Keyframes, lane.keyframes.len() as u64)?;
        }
    }

    // A byte value is charged by what its marker states: the marker is what
    // the reader holds the payload to, in a file and in a structure feed
    // alike, so the charge does not depend on which one this is.
    for value in doc.extensions.values() {
        let size = match value {
            ClmExtension::Json(json) => crate::formats::clm::extension_json_len(json) as u64,
            ClmExtension::Bytes(marker) => marker.size,
        };
        budget.charge(LoadResource::ExtensionBytes, size)?;
    }
    Ok(())
}

fn index_count(mesh: &crate::formats::clm::ClmMesh) -> usize {
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
