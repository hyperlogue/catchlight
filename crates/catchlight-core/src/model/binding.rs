//! Param bindings over the sparse authored-keypoint model: a binding stores
//! only the cells the rigger set; everything else is derived at puppet build by
//! `crate::fill`. Authored = present, so set/partial/unset UX reads
//! the data shape directly. Cells stay sorted by `(y, x)` so saves stay
//! byte-stable regardless of authoring order.
//!
//! A binding belongs to the model, not to the param: it is addressed by its
//! [`BindingKey`] — the param, the node and the property — so nothing has to
//! walk a param's private list to find one.
//!
//! **A binding spans at most two params, and its grid is the only space it is
//! read over.** [`BindingParams`] has two variants and stays that way: a third
//! axis would make the grid a volume whose cells grow with the product of
//! three key-position sets. Rigs needing more reach compose instead —
//! independent bindings write independent deform sources that
//! [`crate::deform::DeformStack`] sums onto the base mesh, so the product is
//! never formed and never has to be taken apart again.

use std::sync::OnceLock;

use crate::fill::derive_dense;
use crate::formats::clm::{ClmBindingValues, ClmCell, ClmCells};
use crate::interpolate::InterpolateMode;

use super::*;

/// A binding's dense evaluation grid, row-major over its cell grid: what the
/// author set at a keypoint and what [`crate::fill`] derived everywhere else.
/// Always exactly one shape for one binding — a deform binding's grid holds a
/// flat `[dx, dy, …]` per cell, every other target one `f32`.
#[derive(Debug, Clone, PartialEq)]
pub enum DenseGrid {
    Scalar(Vec<f32>),
    Deform(Vec<Vec<f32>>),
}

/// A finite, strictly increasing range. A collapsed or inverted one cannot map
/// a pose onto the normalized key positions.
pub fn param_range_is_valid(min: f32, max: f32) -> bool {
    min.is_finite() && max.is_finite() && min < max
}

/// The second axis of a one-param binding: one position, so the grid is a row
/// and `derive_dense` treats it as 1-D.
const SINGLE_POSITION: [f32; 1] = [0.0];

/// A binding target whose value matrix is a single `f32` per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarTarget {
    Tx,
    Ty,
    Sx,
    Sy,
    Rx,
    Ry,
    Rz,
    ZOrder,
    Opacity,
    TintR,
    TintG,
    TintB,
    ScreenTintR,
    ScreenTintG,
    ScreenTintB,
    OutputScaleX,
    OutputScaleY,
}

/// Any binding target — the per-vertex deform or one of the scalars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingTarget {
    Deform,
    Scalar(ScalarTarget),
}

/// The one or two params a binding's grid spans. Two params are jointly
/// authored — "head left *and* up" is its own shape, not left plus up — so the
/// grid is the product of their key positions and the pair belongs to the
/// binding, not to either param.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BindingParams {
    /// One param. The grid is a row: every cell's `y` is 0.
    One(ParamId),
    /// Two params, x then y. The binding owns one position axis for each.
    Two(ParamId, ParamId),
}

impl BindingParams {
    /// The param along the grid's x axis.
    pub fn x(&self) -> &ParamId {
        match self {
            Self::One(p) | Self::Two(p, _) => p,
        }
    }

    /// The param along the grid's y axis, if the binding spans two.
    pub fn y(&self) -> Option<&ParamId> {
        match self {
            Self::One(_) => None,
            Self::Two(_, p) => Some(p),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &ParamId> {
        std::iter::once(self.x()).chain(self.y())
    }

    /// Which grid axis `param` drives, if it is one of the binding's.
    pub fn axis_of(&self, param: &ParamId) -> Option<u8> {
        if self.x() == param {
            Some(0)
        } else if self.y() == Some(param) {
            Some(1)
        } else {
            None
        }
    }

    pub fn contains(&self, param: &ParamId) -> bool {
        self.axis_of(param).is_some()
    }
}

/// What a binding is: one or two params' control over one property of one
/// node. Two bindings with the same key are the same binding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingKey {
    pub params: BindingParams,
    pub node: NodeId,
    pub target: BindingTarget,
}

impl BindingKey {
    /// One param drives `target` on `node`.
    pub fn new(param: ParamId, node: NodeId, target: BindingTarget) -> Self {
        Self {
            params: BindingParams::One(param),
            node,
            target,
        }
    }

    /// Two params jointly drive `target` on `node`; `x` runs along the grid's
    /// first axis and `y` along its second.
    pub fn pair(x: ParamId, y: ParamId, node: NodeId, target: BindingTarget) -> Self {
        Self {
            params: BindingParams::Two(x, y),
            node,
            target,
        }
    }
}

impl BindingTarget {
    pub fn parse(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("deform") {
            return Some(Self::Deform);
        }
        ScalarTarget::parse(s).map(Self::Scalar)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Deform => "deform",
            Self::Scalar(t) => t.name(),
        }
    }

    fn scalar(self) -> Result<ScalarTarget, ModelError> {
        match self {
            Self::Scalar(t) => Ok(t),
            Self::Deform => Err(ModelError::WrongTarget),
        }
    }
}

impl ScalarTarget {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "tx" | "translatex" => Self::Tx,
            "ty" | "translatey" => Self::Ty,
            "sx" | "scalex" => Self::Sx,
            "sy" | "scaley" => Self::Sy,
            "rx" | "rotatex" => Self::Rx,
            "ry" | "rotatey" => Self::Ry,
            "rz" | "rotatez" => Self::Rz,
            "z_order" => Self::ZOrder,
            "opacity" => Self::Opacity,
            "tintr" => Self::TintR,
            "tintg" => Self::TintG,
            "tintb" => Self::TintB,
            "screentintr" => Self::ScreenTintR,
            "screentintg" => Self::ScreenTintG,
            "screentintb" => Self::ScreenTintB,
            "outputscalex" => Self::OutputScaleX,
            "outputscaley" => Self::OutputScaleY,
            _ => return None,
        })
    }

    /// The wire name `parse` accepts — the single string table both sides of
    /// the protocol share.
    pub fn name(self) -> &'static str {
        match self {
            Self::Tx => "tx",
            Self::Ty => "ty",
            Self::Sx => "sx",
            Self::Sy => "sy",
            Self::Rx => "rx",
            Self::Ry => "ry",
            Self::Rz => "rz",
            Self::ZOrder => "z_order",
            Self::Opacity => "opacity",
            Self::TintR => "tintr",
            Self::TintG => "tintg",
            Self::TintB => "tintb",
            Self::ScreenTintR => "screentintr",
            Self::ScreenTintG => "screentintg",
            Self::ScreenTintB => "screentintb",
            Self::OutputScaleX => "outputscalex",
            Self::OutputScaleY => "outputscaley",
        }
    }

    /// Does this target drive colour? Colour lands on a part or a composite; a
    /// mesh group is never drawn, so [`Model::add_binding`] refuses to aim one
    /// at a mesh group and `catchlight_core` refuses to load a file that does
    /// ([`Model::check`] flags it).
    pub fn is_color(self) -> bool {
        matches!(
            self,
            Self::Opacity
                | Self::TintR
                | Self::TintG
                | Self::TintB
                | Self::ScreenTintR
                | Self::ScreenTintG
                | Self::ScreenTintB
        )
    }

    /// The value a cell holds when the binding has no effect. Multiplicative
    /// targets (scale, opacity, tint, output-scale) rest at 1; additive at 0.
    pub fn identity(self) -> f32 {
        match self {
            Self::Sx
            | Self::Sy
            | Self::Opacity
            | Self::TintR
            | Self::TintG
            | Self::TintB
            | Self::OutputScaleX
            | Self::OutputScaleY => 1.0,
            _ => 0.0,
        }
    }

    fn wrap(self, c: ClmCells<f32>) -> ClmBindingValues {
        use ClmBindingValues as V;
        match self {
            Self::Tx => V::TransformTX(c),
            Self::Ty => V::TransformTY(c),
            Self::Sx => V::TransformSX(c),
            Self::Sy => V::TransformSY(c),
            Self::Rx => V::TransformRX(c),
            Self::Ry => V::TransformRY(c),
            Self::Rz => V::TransformRZ(c),
            Self::ZOrder => V::ZOrder(c),
            Self::Opacity => V::Opacity(c),
            Self::TintR => V::TintR(c),
            Self::TintG => V::TintG(c),
            Self::TintB => V::TintB(c),
            Self::ScreenTintR => V::ScreenTintR(c),
            Self::ScreenTintG => V::ScreenTintG(c),
            Self::ScreenTintB => V::ScreenTintB(c),
            Self::OutputScaleX => V::OutputScaleX(c),
            Self::OutputScaleY => V::OutputScaleY(c),
        }
    }
}

pub fn target_of(v: &ClmBindingValues) -> BindingTarget {
    use ClmBindingValues as V;
    BindingTarget::Scalar(match v {
        V::Deform(_) => return BindingTarget::Deform,
        V::TransformTX(_) => ScalarTarget::Tx,
        V::TransformTY(_) => ScalarTarget::Ty,
        V::TransformSX(_) => ScalarTarget::Sx,
        V::TransformSY(_) => ScalarTarget::Sy,
        V::TransformRX(_) => ScalarTarget::Rx,
        V::TransformRY(_) => ScalarTarget::Ry,
        V::TransformRZ(_) => ScalarTarget::Rz,
        V::ZOrder(_) => ScalarTarget::ZOrder,
        V::Opacity(_) => ScalarTarget::Opacity,
        V::TintR(_) => ScalarTarget::TintR,
        V::TintG(_) => ScalarTarget::TintG,
        V::TintB(_) => ScalarTarget::TintB,
        V::ScreenTintR(_) => ScalarTarget::ScreenTintR,
        V::ScreenTintG(_) => ScalarTarget::ScreenTintG,
        V::ScreenTintB(_) => ScalarTarget::ScreenTintB,
        V::OutputScaleX(_) => ScalarTarget::OutputScaleX,
        V::OutputScaleY(_) => ScalarTarget::OutputScaleY,
    })
}

fn scalar_cells_mut(v: &mut ClmBindingValues) -> Option<&mut Vec<ClmCell<f32>>> {
    use ClmBindingValues as V;
    match v {
        V::Deform(_) => None,
        V::ZOrder(c)
        | V::TransformTX(c)
        | V::TransformTY(c)
        | V::TransformSX(c)
        | V::TransformSY(c)
        | V::TransformRX(c)
        | V::TransformRY(c)
        | V::TransformRZ(c)
        | V::Opacity(c)
        | V::TintR(c)
        | V::TintG(c)
        | V::TintB(c)
        | V::ScreenTintR(c)
        | V::ScreenTintG(c)
        | V::ScreenTintB(c)
        | V::OutputScaleX(c)
        | V::OutputScaleY(c) => Some(&mut c.cells),
    }
}

pub fn scalar_cells(v: &ClmBindingValues) -> Option<&[ClmCell<f32>]> {
    use ClmBindingValues as V;
    match v {
        V::Deform(_) => None,
        V::ZOrder(c)
        | V::TransformTX(c)
        | V::TransformTY(c)
        | V::TransformSX(c)
        | V::TransformSY(c)
        | V::TransformRX(c)
        | V::TransformRY(c)
        | V::TransformRZ(c)
        | V::Opacity(c)
        | V::TintR(c)
        | V::TintG(c)
        | V::TintB(c)
        | V::ScreenTintR(c)
        | V::ScreenTintG(c)
        | V::ScreenTintB(c)
        | V::OutputScaleX(c)
        | V::OutputScaleY(c) => Some(&c.cells),
    }
}

/// Wire names for mask modes (the inverse of the server's parse).
pub fn mask_mode_name(m: crate::components::MaskMode) -> &'static str {
    match m {
        crate::components::MaskMode::Mask => "mask",
        crate::components::MaskMode::DodgeMask => "dodge",
    }
}

pub fn deform_cells(v: &ClmBindingValues) -> Option<&[ClmCell<Vec<f32>>]> {
    match v {
        ClmBindingValues::Deform(c) => Some(&c.cells),
        _ => None,
    }
}

fn upsert<T>(cells: &mut Vec<ClmCell<T>>, cell: [u32; 2], value: T) {
    let [x, y] = cell;
    match cells.iter_mut().find(|c| c.x == x && c.y == y) {
        Some(cell) => cell.value = value,
        None => {
            cells.push(ClmCell { x, y, value });
            cells.sort_by_key(|c| (c.y, c.x));
        }
    }
}

/// Validate binding-owned normalized axes before any grid is allocated.
pub(crate) fn validate_binding_positions(
    params: usize,
    positions: &[Vec<f32>],
) -> Result<(), ModelError> {
    if positions.len() != params
        || positions.iter().any(|axis| {
            axis.is_empty()
                || axis
                    .iter()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
                || axis.windows(2).any(|pair| pair[0] >= pair[1])
        })
    {
        return Err(ModelError::CellOutOfRange);
    }
    let cells = positions
        .iter()
        .try_fold(1u64, |n, axis| n.checked_mul(axis.len() as u64));
    if cells.is_none_or(|n| n > crate::load_budget::MAX_BINDING_GRID_CELLS) {
        return Err(ModelError::CellOutOfRange);
    }
    Ok(())
}

const DEFAULT_POSITIONS: [f32; 2] = [0.0, 1.0];

impl Model {
    /// How many positions this binding has on the named param's axis.
    pub fn key_count(&self, key: &BindingKey, param: &ParamId) -> Result<u32, ModelError> {
        let axis = key.params.axis_of(param).ok_or(ModelError::UnknownParam)? as usize;
        let binding = self.binding(key).ok_or(ModelError::UnknownBinding)?;
        Ok(binding.key_positions[axis].len() as u32)
    }

    /// The binding's own cell grid. A missing binding uses the two-position
    /// axes that an auto-creating write would create.
    pub fn binding_grid(&self, key: &BindingKey) -> Result<(u32, u32), ModelError> {
        let (x, y) = self.binding_axes(key)?;
        Ok((x.len() as u32, y.len() as u32))
    }

    /// Normalized binding-owned positions, in driving-param order.
    pub(super) fn binding_axes(&self, key: &BindingKey) -> Result<(&[f32], &[f32]), ModelError> {
        for param in key.params.iter() {
            self.param(param).ok_or(ModelError::UnknownParam)?;
        }
        if let Some(binding) = self.binding(key) {
            Ok((
                &binding.key_positions[0],
                binding
                    .key_positions
                    .get(1)
                    .map_or(&SINGLE_POSITION[..], Vec::as_slice),
            ))
        } else {
            Ok((
                &DEFAULT_POSITIONS,
                if key.params.y().is_some() {
                    &DEFAULT_POSITIONS
                } else {
                    &SINGLE_POSITION
                },
            ))
        }
    }

    /// Every binding in the model, in creation order.
    pub fn bindings(&self) -> impl Iterator<Item = &ModelBinding> {
        self.bindings.iter()
    }

    /// The bindings one param drives, in creation order. A two-param binding
    /// appears for each of its params.
    pub fn bindings_of_param<'a>(
        &'a self,
        param: &'a ParamId,
    ) -> impl Iterator<Item = &'a ModelBinding> + 'a {
        self.bindings
            .iter()
            .filter(move |b| b.key.params.contains(param))
    }

    /// The bindings that drive one node, in creation order.
    pub fn bindings_of_node<'a>(
        &'a self,
        node: &'a NodeId,
    ) -> impl Iterator<Item = &'a ModelBinding> + 'a {
        self.bindings.iter().filter(move |b| &b.key.node == node)
    }

    pub fn binding(&self, key: &BindingKey) -> Option<&ModelBinding> {
        self.bindings.get(*self.binding_index().get(key)?)
    }

    /// `bindings` by key, built on the first lookup after an edit. A puppet
    /// bakes every binding in the model and a rebake does it again, so the
    /// lookup this backs is on a path that runs once per binding — a scan
    /// there is quadratic in the model's size.
    fn binding_index(&self) -> &HashMap<BindingKey, usize> {
        self.binding_index.get_or_init(|| {
            self.bindings
                .iter()
                .enumerate()
                .map(|(i, b)| (b.key.clone(), i))
                .collect()
        })
    }

    fn binding_mut(&mut self, key: &BindingKey) -> Result<&mut ModelBinding, ModelError> {
        let i = *self
            .binding_index()
            .get(key)
            .ok_or(ModelError::UnknownBinding)?;
        self.bindings.get_mut(i).ok_or(ModelError::UnknownBinding)
    }

    fn check_cell(&self, key: &BindingKey, cell: [u32; 2]) -> Result<(), ModelError> {
        let (w, h) = self.binding_grid(key)?;
        if cell[0] >= w || cell[1] >= h {
            return Err(ModelError::CellOutOfRange);
        }
        Ok(())
    }

    /// Ensure `key`'s binding exists, creating an everywhere-unset one if it
    /// does not (an unset binding contributes nothing). A deform binding needs
    /// a meshed node; a colour binding needs a drawable one, because a mesh
    /// group is never drawn and has no colour to fold into; and a two-param
    /// binding needs two different params, or its grid would be a param
    /// crossed with itself.
    pub fn add_binding(&mut self, key: &BindingKey) -> Result<(), ModelError> {
        self.add_binding_with_positions(
            key,
            key.params
                .iter()
                .map(|_| DEFAULT_POSITIONS.to_vec())
                .collect(),
        )
    }

    /// Create an all-unset binding with explicit normalized axes. An existing
    /// binding is left unchanged, including its positions and interpolation.
    pub fn add_binding_with_positions(
        &mut self,
        key: &BindingKey,
        key_positions: Vec<Vec<f32>>,
    ) -> Result<(), ModelError> {
        if key.params.y() == Some(key.params.x()) {
            return Err(ModelError::SelfPairedBinding);
        }
        let kind = self
            .node(&key.node)
            .map(|n| &n.kind)
            .ok_or(ModelError::UnknownNode)?;
        let values = match key.target {
            BindingTarget::Deform => {
                if !matches!(kind, ModelNodeKind::Part(_) | ModelNodeKind::MeshGroup(_)) {
                    return Err(ModelError::NotMeshed);
                }
                ClmBindingValues::Deform(ClmCells::default())
            }
            BindingTarget::Scalar(t) => {
                if t.is_color() && matches!(kind, ModelNodeKind::MeshGroup(_)) {
                    return Err(ModelError::ColorOnMeshGroup);
                }
                t.wrap(ClmCells::default())
            }
        };
        self.binding_grid(key)?;
        if self.binding(key).is_some() {
            return Ok(());
        }
        validate_binding_positions(key.params.iter().count(), &key_positions)?;
        self.bindings.push(ModelBinding {
            key: key.clone(),
            key_positions,
            interpolate_mode: InterpolateMode::Linear,
            values: values.into(),
            dense: OnceLock::new(),
        });
        self.bump();
        Ok(())
    }

    /// Author one keypoint of a scalar binding (auto-creating the binding).
    /// `cell` indexes this binding's grid. No other cell is authored.
    pub fn set_binding_key(
        &mut self,
        key: &BindingKey,
        cell: [u32; 2],
        value: f32,
    ) -> Result<(), ModelError> {
        key.target.scalar()?;
        if !value.is_finite() {
            return Err(ModelError::CellOutOfRange);
        }
        // Validate before creating anything — a failed key write must not
        // leave a phantom binding behind.
        self.check_cell(key, cell)?;
        self.add_binding(key)?;
        let binding = self.binding_mut(key)?;
        if let Some(cells) = scalar_cells_mut(binding.values_mut()) {
            upsert(cells, cell, value);
        }
        self.bump();
        Ok(())
    }

    /// Un-author a keypoint (the cell goes back to derived).
    pub fn unset_binding_key(
        &mut self,
        key: &BindingKey,
        cell: [u32; 2],
    ) -> Result<(), ModelError> {
        self.check_cell(key, cell)?;
        let binding = self.binding_mut(key)?;
        let [x, y] = cell;
        match binding.values_mut() {
            ClmBindingValues::Deform(c) => c.cells.retain(|c| !(c.x == x && c.y == y)),
            other => {
                if let Some(cells) = scalar_cells_mut(other) {
                    cells.retain(|c| !(c.x == x && c.y == y));
                }
            }
        }
        self.bump();
        Ok(())
    }

    /// Author the do-nothing identity value at a keypoint.
    pub fn reset_binding_key(
        &mut self,
        key: &BindingKey,
        cell: [u32; 2],
    ) -> Result<(), ModelError> {
        self.check_cell(key, cell)?;
        self.write_identity_at(key, cell)
    }

    fn write_identity_at(&mut self, key: &BindingKey, cell: [u32; 2]) -> Result<(), ModelError> {
        let vcount = self.deform_len(&key.node);
        let target = key.target;
        let binding = self.binding_mut(key)?;
        match binding.values_mut() {
            ClmBindingValues::Deform(c) => upsert(&mut c.cells, cell, vec![0.0; vcount]),
            other => {
                let identity = match target {
                    BindingTarget::Scalar(t) => t.identity(),
                    BindingTarget::Deform => 0.0,
                };
                if let Some(cells) = scalar_cells_mut(other) {
                    upsert(cells, cell, identity);
                }
            }
        }
        self.bump();
        Ok(())
    }

    pub fn delete_binding(&mut self, key: &BindingKey) -> Result<(), ModelError> {
        let before = self.bindings.len();
        self.bindings.retain(|b| &b.key != key);
        if self.bindings.len() == before {
            return Err(ModelError::UnknownBinding);
        }
        self.bump();
        Ok(())
    }

    pub fn set_binding_interpolate(
        &mut self,
        key: &BindingKey,
        mode: InterpolateMode,
    ) -> Result<(), ModelError> {
        self.binding_mut(key)?.interpolate_mode = mode;
        self.bump();
        Ok(())
    }

    /// Negate every authored value.
    pub fn invert_binding(&mut self, key: &BindingKey) -> Result<(), ModelError> {
        let binding = self.binding_mut(key)?;
        match binding.values_mut() {
            ClmBindingValues::Deform(c) => {
                for cell in &mut c.cells {
                    for v in &mut cell.value {
                        *v = -*v;
                    }
                }
            }
            other => {
                if let Some(cells) = scalar_cells_mut(other) {
                    for cell in cells {
                        cell.value = -cell.value;
                    }
                }
            }
        }
        self.bump();
        Ok(())
    }

    /// The binding's dense grid: the value at every cell of it, authored or
    /// derived by [`crate::fill`]. Built on the first read and shared until
    /// the cells, the key positions or the mesh it came from move — so a
    /// reader that walks a whole grid pays for the fill once.
    pub fn binding_dense(&self, key: &BindingKey) -> Option<&Arc<DenseGrid>> {
        let binding = self.binding(key)?;
        Some(
            binding
                .dense
                .get_or_init(|| Arc::new(self.derive_grid(key))),
        )
    }

    fn derive_grid(&self, key: &BindingKey) -> DenseGrid {
        let (w, h) = self.binding_grid(key).unwrap_or((1, 1));
        let (axis_x, axis_y) = self
            .binding_axes(key)
            .unwrap_or((&SINGLE_POSITION[..], &SINGLE_POSITION[..]));
        let (w, h) = (w as usize, h as usize);
        let values = self.binding(key).map(|b| &b.values);
        match values.map(|v| &**v) {
            Some(ClmBindingValues::Deform(c)) => {
                let identity = vec![0.0f32; self.deform_len(&key.node)];
                let authored: Vec<((u32, u32), Vec<f32>)> = c
                    .cells
                    .iter()
                    .map(|c| ((c.x, c.y), c.value.clone()))
                    .collect();
                DenseGrid::Deform(derive_dense(w, h, axis_x, axis_y, &authored, &identity))
            }
            other => {
                let identity = match key.target {
                    BindingTarget::Scalar(t) => t.identity(),
                    BindingTarget::Deform => 0.0,
                };
                let authored: Vec<((u32, u32), f32)> = other
                    .and_then(scalar_cells)
                    .unwrap_or(&[])
                    .iter()
                    .map(|c| ((c.x, c.y), c.value))
                    .collect();
                DenseGrid::Scalar(derive_dense(w, h, axis_x, axis_y, &authored, &identity))
            }
        }
    }

    /// The evaluated scalar value at a cell: authored if present, otherwise
    /// the derived fill — the single implementation every reader shares.
    pub fn scalar_value_at(&self, key: &BindingKey, cell: [u32; 2]) -> Result<f32, ModelError> {
        key.target.scalar()?;
        self.check_cell(key, cell)?;
        let (w, _) = self.binding_grid(key)?;
        let dense = self.binding_dense(key).ok_or(ModelError::UnknownBinding)?;
        match &**dense {
            DenseGrid::Scalar(values) => values
                .get((cell[1] * w + cell[0]) as usize)
                .copied()
                .ok_or(ModelError::CellOutOfRange),
            DenseGrid::Deform(_) => Err(ModelError::WrongTarget),
        }
    }

    /// The evaluated deform offsets at a cell: authored if present, otherwise
    /// the derived fill; the identity is zeros sized to the node's mesh, so an
    /// everywhere-unset — or entirely absent — binding evaluates to
    /// well-shaped rest offsets.
    pub fn deform_value_at(
        &self,
        key: &BindingKey,
        cell: [u32; 2],
    ) -> Result<Vec<f32>, ModelError> {
        if key.target != BindingTarget::Deform {
            return Err(ModelError::WrongTarget);
        }
        self.check_cell(key, cell)?;
        let (w, _) = self.binding_grid(key)?;
        let Some(dense) = self.binding_dense(key) else {
            return Ok(vec![0.0f32; self.deform_len(&key.node)]);
        };
        match &**dense {
            DenseGrid::Deform(values) => values
                .get((cell[1] * w + cell[0]) as usize)
                .cloned()
                .ok_or(ModelError::CellOutOfRange),
            DenseGrid::Scalar(_) => Err(ModelError::WrongTarget),
        }
    }

    /// Copy the (derived-or-authored) value at `from` and author it at `to`.
    pub fn copy_binding_key(
        &mut self,
        key: &BindingKey,
        from: [u32; 2],
        to: [u32; 2],
    ) -> Result<(), ModelError> {
        self.check_cell(key, to)?;
        match key.target {
            BindingTarget::Deform => {
                if self.binding(key).is_none() {
                    return Err(ModelError::UnknownBinding);
                }
                let value = self.deform_value_at(key, from)?;
                let binding = self.binding_mut(key)?;
                if let ClmBindingValues::Deform(c) = binding.values_mut() {
                    upsert(&mut c.cells, to, value);
                }
            }
            BindingTarget::Scalar(_) => {
                let value = self.scalar_value_at(key, from)?;
                let binding = self.binding_mut(key)?;
                if let Some(cells) = scalar_cells_mut(binding.values_mut()) {
                    upsert(cells, to, value);
                }
            }
        }
        self.bump();
        Ok(())
    }

    /// Author per-vertex deform offsets at a cell. `offsets` is flat
    /// `[dx, dy, …]` and must match the node's mesh.
    pub fn set_deform_vertices(
        &mut self,
        key: &BindingKey,
        cell: [u32; 2],
        offsets: Vec<f32>,
    ) -> Result<(), ModelError> {
        if key.target != BindingTarget::Deform {
            return Err(ModelError::WrongTarget);
        }
        let expected = self.deform_len(&key.node);
        if self.node_mesh(&key.node).is_none() || offsets.len() != expected {
            return Err(ModelError::NotMeshed);
        }
        if offsets.iter().any(|v| !v.is_finite()) {
            return Err(ModelError::CellOutOfRange);
        }
        self.check_cell(key, cell)?;
        self.add_binding(key)?;
        let binding = self.binding_mut(key)?;
        if let ClmBindingValues::Deform(c) = binding.values_mut() {
            upsert(&mut c.cells, cell, offsets);
        }
        self.bump();
        Ok(())
    }

    /// Author a deform keypoint by applying an affine (scale, then rotate, then
    /// translate — about the node's mesh origin) to the node's rest vertices and
    /// storing the resulting per-vertex offsets in `cell`.
    pub fn set_deform_from_transform(
        &mut self,
        key: &BindingKey,
        cell: [u32; 2],
        translate: [f32; 2],
        rotate: f32,
        scale: [f32; 2],
    ) -> Result<(), ModelError> {
        let mesh = match self.node(&key.node) {
            Some(n) => n.mesh().ok_or(ModelError::NotMeshed)?,
            None => return Err(ModelError::UnknownNode),
        };
        let (verts, origin) = (mesh.verts.clone(), mesh.origin);
        let vcount = verts.len() / 2;
        let mut offsets = Vec::with_capacity(vcount * 2);
        let (sin, cos) = rotate.sin_cos();
        for i in 0..vcount {
            let (vx, vy) = (verts[2 * i], verts[2 * i + 1]);
            let dx = (vx - origin[0]) * scale[0];
            let dy = (vy - origin[1]) * scale[1];
            let nx = dx * cos - dy * sin + origin[0] + translate[0];
            let ny = dx * sin + dy * cos + origin[1] + translate[1];
            offsets.push(nx - vx);
            offsets.push(ny - vy);
        }
        self.set_deform_vertices(key, cell, offsets)
    }

    /// Flat length of the node's mesh vertex array (`2 * vertex count`).
    pub fn deform_len(&self, node: &NodeId) -> usize {
        self.node(node)
            .and_then(ModelNode::mesh)
            .map_or(0, |m| m.verts.len())
    }

    // ---- param structure ----

    pub fn set_param_name(&mut self, param: &ParamId, name: Name) -> Result<(), ModelError> {
        self.param_mut(param)?.name = name;
        self.bump();
        Ok(())
    }

    pub fn set_param_default(&mut self, param: &ParamId, default: f32) -> Result<(), ModelError> {
        self.param_mut(param)?.default = default;
        self.bump();
        Ok(())
    }

    /// Change the param's range. Key positions are normalized, so they keep
    /// their relative places and authored cells (which are index-keyed) don't
    /// move.
    pub fn set_param_range(
        &mut self,
        param: &ParamId,
        min: f32,
        max: f32,
    ) -> Result<(), ModelError> {
        if !param_range_is_valid(min, max) {
            return Err(ModelError::CellOutOfRange);
        }
        let p = self.param_mut(param)?;
        p.min = min;
        p.max = max;
        self.bump();
        Ok(())
    }

    fn param_mut(&mut self, param: &ParamId) -> Result<&mut ModelParam, ModelError> {
        self.params.get_mut(param).ok_or(ModelError::UnknownParam)
    }

    /// Insert one normalized position into this binding only. Existing cells
    /// shift to keep their sampled positions; the new row/column is unauthored.
    pub fn key_insert(
        &mut self,
        key: &BindingKey,
        param: &ParamId,
        value: f32,
    ) -> Result<usize, ModelError> {
        let axis = key.params.axis_of(param).ok_or(ModelError::UnknownParam)?;
        let binding = self.binding(key).ok_or(ModelError::UnknownBinding)?;
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(ModelError::CellOutOfRange);
        }
        let points = &binding.key_positions[axis as usize];
        if points.contains(&value) {
            return Err(ModelError::CellOutOfRange);
        }
        let idx = points.partition_point(|&p| p < value);
        let mut positions = binding.key_positions.clone();
        positions[axis as usize].insert(idx, value);
        validate_binding_positions(key.params.iter().count(), &positions)?;
        let binding = self.binding_mut(key)?;
        binding.key_positions = positions;
        binding.cell_coords_mut().map_coords(axis, &|coord| {
            if coord >= idx as u32 {
                coord + 1
            } else {
                coord
            }
        });
        self.bump();
        Ok(idx)
    }

    /// Delete a binding position and its authored row/column. At least one
    /// position remains; endpoints may be removed because params own ranges.
    pub fn key_delete(
        &mut self,
        key: &BindingKey,
        param: &ParamId,
        index: usize,
    ) -> Result<(), ModelError> {
        let axis = key.params.axis_of(param).ok_or(ModelError::UnknownParam)?;
        let binding = self.binding_mut(key)?;
        let points = &mut binding.key_positions[axis as usize];
        if points.len() <= 1 || index >= points.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        points.remove(index);
        let cells = binding.cell_coords_mut();
        cells.drop_at(axis, index as u32);
        cells.map_coords(axis, &|coord| {
            if coord > index as u32 {
                coord - 1
            } else {
                coord
            }
        });
        self.bump();
        Ok(())
    }

    /// Move one normalized position without crossing neighbors. Only this
    /// binding's derived grid is invalidated; authored indices stay unchanged.
    pub fn key_move(
        &mut self,
        key: &BindingKey,
        param: &ParamId,
        index: usize,
        value: f32,
    ) -> Result<(), ModelError> {
        let axis = key.params.axis_of(param).ok_or(ModelError::UnknownParam)?;
        let binding = self.binding_mut(key)?;
        let points = &mut binding.key_positions[axis as usize];
        if index >= points.len() {
            return Err(ModelError::IndexOutOfRange);
        }
        if !value.is_finite()
            || !(0.0..=1.0).contains(&value)
            || (index > 0 && value <= points[index - 1])
            || (index + 1 < points.len() && value >= points[index + 1])
        {
            return Err(ModelError::CellOutOfRange);
        }
        points[index] = value;
        binding.invalidate_dense();
        self.bump();
        Ok(())
    }
}

impl ModelBinding {
    fn cell_coords_mut(&mut self) -> &mut dyn CellCoords {
        match self.values_mut() {
            ClmBindingValues::Deform(c) => &mut c.cells,
            ClmBindingValues::ZOrder(c)
            | ClmBindingValues::TransformTX(c)
            | ClmBindingValues::TransformTY(c)
            | ClmBindingValues::TransformSX(c)
            | ClmBindingValues::TransformSY(c)
            | ClmBindingValues::TransformRX(c)
            | ClmBindingValues::TransformRY(c)
            | ClmBindingValues::TransformRZ(c)
            | ClmBindingValues::Opacity(c)
            | ClmBindingValues::TintR(c)
            | ClmBindingValues::TintG(c)
            | ClmBindingValues::TintB(c)
            | ClmBindingValues::ScreenTintR(c)
            | ClmBindingValues::ScreenTintG(c)
            | ClmBindingValues::ScreenTintB(c)
            | ClmBindingValues::OutputScaleX(c)
            | ClmBindingValues::OutputScaleY(c) => &mut c.cells,
        }
    }
}

/// The two cell-value shapes share their `(x, y)` bookkeeping; this is the one
/// place an axis edit rewrites coordinates, whatever a cell holds.
trait CellCoords {
    fn map_coords(&mut self, axis: u8, f: &dyn Fn(u32) -> u32);
    fn drop_at(&mut self, axis: u8, coord: u32);
}

impl<T> CellCoords for Vec<ClmCell<T>> {
    fn map_coords(&mut self, axis: u8, f: &dyn Fn(u32) -> u32) {
        for cell in self.iter_mut() {
            if axis == 0 {
                cell.x = f(cell.x);
            } else {
                cell.y = f(cell.y);
            }
        }
        self.sort_by_key(|c| (c.y, c.x));
    }

    fn drop_at(&mut self, axis: u8, coord: u32) {
        self.retain(|c| {
            if axis == 0 {
                c.x != coord
            } else {
                c.y != coord
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::clm::{ClmIndices, ClmMesh};
    use crate::id::SeededHex;

    #[test]
    fn param_range_rejects_inverted_collapsed_and_nan() {
        assert!(param_range_is_valid(0.0, 1.0));
        assert!(param_range_is_valid(-1.0, 1.0));
        assert!(!param_range_is_valid(1.0, 0.0));
        assert!(!param_range_is_valid(0.0, 0.0));
        assert!(!param_range_is_valid(f32::NAN, 1.0));
        assert!(!param_range_is_valid(0.0, f32::INFINITY));
    }

    /// A model with one group, one quad part and a shared scalar input.
    struct Fixture {
        m: Model,
        hex: SeededHex,
        group: NodeId,
        part: NodeId,
        param: ParamId,
    }

    fn fixture() -> Fixture {
        let mut hex = SeededHex::new(5);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let group = m
            .add_node(&root, ModelNode::new("g", ModelNodeKind::Group), &mut hex)
            .unwrap();
        let part = m
            .add_node(
                &root,
                ModelNode::new(
                    "q",
                    ModelNodeKind::Part(ModelPart::new(ClmMesh {
                        verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
                        uvs: vec![0.0; 8],
                        indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                        origin: [0.0, 0.0],
                    })),
                ),
                &mut hex,
            )
            .unwrap();
        let param = m
            .add_param(
                ModelParam {
                    name: Name::truncated("x"),
                    min: -1.0,
                    max: 1.0,
                    default: 0.0,
                },
                &mut hex,
            )
            .unwrap();
        Fixture {
            m,
            hex,
            group,
            part,
            param,
        }
    }

    impl Fixture {
        fn tx(&self, node: &NodeId) -> BindingKey {
            BindingKey::new(
                self.param.clone(),
                node.clone(),
                BindingTarget::Scalar(ScalarTarget::Tx),
            )
        }

        fn deform(&self, node: &NodeId) -> BindingKey {
            BindingKey::new(self.param.clone(), node.clone(), BindingTarget::Deform)
        }
    }

    fn cells_of(m: &Model, key: &BindingKey) -> Vec<(u32, f32)> {
        scalar_cells(m.binding(key).unwrap().values())
            .unwrap()
            .iter()
            .map(|c| (c.x, c.value))
            .collect()
    }

    #[test]
    fn set_unset_reset_key_roundtrip() {
        let mut r = fixture();
        let key = r.tx(&r.group.clone());
        r.m.add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();

        r.m.set_binding_key(&key, [2, 0], 60.0).unwrap();
        r.m.set_binding_key(&key, [0, 0], -60.0).unwrap();
        // Raw writes author only the requested cells.
        assert_eq!(r.m.bindings_of_param(&r.param).count(), 1);
        assert_eq!(cells_of(&r.m, &key), vec![(0, -60.0), (2, 60.0)]);

        r.m.unset_binding_key(&key, [0, 0]).unwrap();
        r.m.reset_binding_key(&key, [1, 0]).unwrap();
        assert_eq!(cells_of(&r.m, &key), vec![(1, 0.0), (2, 60.0)]);

        assert!(r.m.set_binding_key(&key, [3, 0], 1.0).is_err());
        assert!(r.m.to_clm_bytes().is_ok());
    }

    #[test]
    fn copy_key_takes_derived_values() {
        let mut r = fixture();
        let key = r.tx(&r.group.clone());
        r.m.add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        r.m.set_binding_key(&key, [0, 0], -60.0).unwrap();
        r.m.set_binding_key(&key, [2, 0], 60.0).unwrap();
        // cell 1 is derived (midpoint = 0); copying it to cell 2 authors 0 there.
        r.m.copy_binding_key(&key, [1, 0], [2, 0]).unwrap();
        assert_eq!(
            cells_of(&r.m, &key).into_iter().find(|c| c.0 == 2),
            Some((2, 0.0))
        );
    }

    #[test]
    fn invert_and_delete_binding() {
        let mut r = fixture();
        let key = BindingKey::new(
            r.param.clone(),
            r.group.clone(),
            BindingTarget::Scalar(ScalarTarget::Rz),
        );
        r.m.add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        r.m.set_binding_key(&key, [2, 0], 0.5).unwrap();
        r.m.invert_binding(&key).unwrap();
        assert_eq!(
            cells_of(&r.m, &key).into_iter().find(|c| c.0 == 2),
            Some((2, -0.5))
        );

        r.m.delete_binding(&key).unwrap();
        assert!(r.m.binding(&key).is_none());
        assert!(r.m.delete_binding(&key).is_err());
    }

    #[test]
    fn deform_from_transform_writes_offsets() {
        let mut r = fixture();
        let key = r.deform(&r.part.clone());
        r.m.add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        r.m.set_deform_from_transform(&key, [2, 0], [10.0, 0.0], 0.0, [1.0, 1.0])
            .unwrap();
        let cells = deform_cells(r.m.binding(&key).unwrap().values()).unwrap();
        // The rest key is caller-owned; raw writes do not seed one.
        assert_eq!(cells.len(), 1);
        assert_eq!(
            (cells[0].x, &cells[0].value),
            (2, &vec![10.0, 0.0, 10.0, 0.0, 10.0, 0.0, 10.0, 0.0])
        );
        // wrong-length vertex writes are refused.
        assert!(r
            .m
            .set_deform_vertices(&key, [0, 0], vec![1.0, 2.0])
            .is_err());
        assert!(r.m.to_clm_bytes().is_ok());
    }

    /// A binding is one param's control over one property of one node, so the
    /// key has to reject an operation aimed at the wrong kind of value.
    #[test]
    fn the_key_target_decides_which_operations_apply() {
        let mut r = fixture();
        let deform = r.deform(&r.part.clone());
        let scalar = r.tx(&r.part.clone());

        assert!(matches!(
            r.m.set_binding_key(&deform, [0, 0], 1.0),
            Err(ModelError::WrongTarget)
        ));
        assert!(matches!(
            r.m.set_deform_vertices(&scalar, [0, 0], vec![0.0; 8]),
            Err(ModelError::WrongTarget)
        ));
        assert!(matches!(
            r.m.deform_value_at(&scalar, [0, 0]),
            Err(ModelError::WrongTarget)
        ));
    }

    #[test]
    fn axis_ops_remap_authored_cells() {
        let mut r = fixture();
        let key = r.tx(&r.group.clone());
        r.m.add_binding_with_positions(&key, vec![vec![0.0, 0.5, 1.0]])
            .unwrap();
        r.m.set_binding_key(&key, [0, 0], -60.0).unwrap();
        r.m.set_binding_key(&key, [2, 0], 60.0).unwrap();

        // insert between 0.5 and 1.0 → index 2; the authored cell at 2 shifts to 3.
        let idx = r.m.key_insert(&key, &r.param, 0.75).unwrap();
        assert_eq!(idx, 2);
        assert_eq!(
            r.m.binding(&key).unwrap().key_positions()[0],
            vec![0.0, 0.5, 0.75, 1.0]
        );
        let xs: Vec<u32> = cells_of(&r.m, &key).into_iter().map(|c| c.0).collect();
        assert_eq!(xs, vec![0, 3]);

        // Duplicates and out-of-range inserts are rejected.
        assert!(r.m.key_insert(&key, &r.param, 0.5).is_err());
        assert!(r.m.key_insert(&key, &r.param, 2.0).is_err());

        // move the inserted position (must stay between neighbours).
        r.m.key_move(&key, &r.param, 2, 0.6).unwrap();
        assert!(r.m.key_move(&key, &r.param, 2, 0.4).is_err());

        // deleting it keeps the shifted cells consistent.
        r.m.key_delete(&key, &r.param, 2).unwrap();
        let xs: Vec<u32> = cells_of(&r.m, &key).into_iter().map(|c| c.0).collect();
        assert_eq!(xs, vec![0, 2]);

        // a range change leaves the normalized key positions alone.
        r.m.set_param_range(&r.param, 0.0, 4.0).unwrap();
        assert_eq!(
            r.m.binding(&key).unwrap().key_positions()[0],
            vec![0.0, 0.5, 1.0]
        );
    }

    /// A mesh group is never drawn, so a colour binding on one has nowhere to
    /// land and the runtime refuses to load the file it would flatten to.
    #[test]
    fn a_colour_binding_cannot_be_authored_on_a_mesh_group() {
        let mut r = fixture();
        let root = r.m.root().unwrap().clone();
        let group =
            r.m.add_node(
                &root,
                ModelNode::new(
                    "lattice",
                    ModelNodeKind::MeshGroup(ModelMeshGroup::new(ClmMesh::default())),
                ),
                &mut r.hex,
            )
            .unwrap();
        assert!(matches!(
            r.m.add_binding(&BindingKey::new(
                r.param.clone(),
                group.clone(),
                BindingTarget::Scalar(ScalarTarget::Opacity)
            )),
            Err(ModelError::ColorOnMeshGroup)
        ));
        // A non-colour target on the same node is fine.
        r.m.add_binding(&r.tx(&group)).unwrap();
    }

    /// Every key in `bindings()` resolves to the binding at that position, and
    /// nothing else resolves at all. `binding` reads a derived index, so this
    /// is the check that the index never outlives the edit that invalidated
    /// it.
    fn index_agrees_with_the_vec(m: &Model, what: &str) {
        for (i, b) in m.bindings().enumerate() {
            let found = m.binding(b.key());
            assert!(
                found.is_some_and(|f| std::ptr::eq(f, b)),
                "{what}: binding {i} ({:?}) is not what its own key resolves to",
                b.key()
            );
        }
        assert_eq!(
            m.bindings().count(),
            m.bindings()
                .map(|b| b.key().clone())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "{what}: two bindings share a key"
        );
    }

    #[test]
    fn the_binding_index_survives_every_edit_that_moves_a_binding() {
        let mut r = fixture();
        let (group, part, param) = (r.group.clone(), r.part.clone(), r.param.clone());

        r.m.add_binding(&r.tx(&group)).unwrap();
        r.m.add_binding(&r.deform(&part)).unwrap();
        index_agrees_with_the_vec(&r.m, "after add");

        let gone = r.tx(&group);
        r.m.delete_binding(&gone).unwrap();
        assert!(
            r.m.binding(&gone).is_none(),
            "a deleted key resolves to none"
        );
        index_agrees_with_the_vec(&r.m, "after delete");

        r.m.add_binding(&r.tx(&group)).unwrap();
        let renamed = ParamId::new("renamed").unwrap();
        r.m.rename_param_id(&param, renamed.clone()).unwrap();
        assert!(
            r.m.binding(&BindingKey::new(
                param.clone(),
                group.clone(),
                BindingTarget::Scalar(ScalarTarget::Tx)
            ))
            .is_none(),
            "the old param id no longer resolves"
        );
        index_agrees_with_the_vec(&r.m, "after a param rename");

        let renamed_node = NodeId::new("renamed-part").unwrap();
        r.m.rename_node_id(&part, renamed_node).unwrap();
        index_agrees_with_the_vec(&r.m, "after a node rename");

        r.m.delete_node(&group).unwrap();
        index_agrees_with_the_vec(&r.m, "after deleting a bound node");

        r.m.delete_param(&renamed).unwrap();
        assert_eq!(r.m.bindings().count(), 0, "the param took its bindings");
        index_agrees_with_the_vec(&r.m, "after deleting the param");
    }
}
