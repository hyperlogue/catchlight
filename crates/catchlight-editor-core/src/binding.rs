//! Param bindings over the sparse authored-keypoint model: a binding stores
//! only the cells the rigger set; everything else is derived at puppet build by
//! `catchlight_core::fill`. Authored = present, so set/partial/unset UX reads
//! the data shape directly. Cells stay sorted by `(y, x)` so saves stay
//! byte-stable regardless of authoring order.

use catchlight_core::fill::{derive_dense, FillCell};
use catchlight_core::formats::clp::{ClpBindingValues, ClpCell, ClpCells};
use catchlight_core::params::InterpolateMode;

use crate::model::*;
use crate::EditError;

/// Finite, strictly ordered box. 1D params leave Y at `[0, 0]`.
pub fn param_range_is_valid(is_vec2: bool, min: [f32; 2], max: [f32; 2]) -> bool {
    min.iter().chain(max.iter()).all(|v| v.is_finite())
        && min[0] < max[0]
        && (!is_vec2 || min[1] < max[1])
}

/// A binding target whose value matrix is a single `f32` per cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingTarget {
    Deform,
    Scalar(ScalarTarget),
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

    fn wrap(self, c: ClpCells<f32>) -> ClpBindingValues {
        use ClpBindingValues as V;
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

pub fn target_of(v: &ClpBindingValues) -> BindingTarget {
    use ClpBindingValues as V;
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

fn scalar_cells_mut(v: &mut ClpBindingValues) -> Option<&mut Vec<ClpCell<f32>>> {
    use ClpBindingValues as V;
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

pub fn scalar_cells(v: &ClpBindingValues) -> Option<&[ClpCell<f32>]> {
    use ClpBindingValues as V;
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
pub fn mask_mode_name(m: catchlight_core::components::MaskMode) -> &'static str {
    match m {
        catchlight_core::components::MaskMode::Mask => "mask",
        catchlight_core::components::MaskMode::DodgeMask => "dodge",
    }
}

pub fn deform_cells(v: &ClpBindingValues) -> Option<&[ClpCell<Vec<f32>>]> {
    match v {
        ClpBindingValues::Deform(c) => Some(&c.cells),
        _ => None,
    }
}

fn upsert<T>(cells: &mut Vec<ClpCell<T>>, x: u32, y: u32, value: T) {
    match cells.iter_mut().find(|c| c.x == x && c.y == y) {
        Some(cell) => cell.value = value,
        None => {
            cells.push(ClpCell { x, y, value });
            cells.sort_by_key(|c| (c.y, c.x));
        }
    }
}

fn normed(v: f32, min: f32, max: f32) -> f32 {
    if (max - min).abs() > f32::EPSILON {
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn nearest_axis_index(points: &[f32], v: f32) -> u32 {
    let mut best = 0usize;
    let mut best_d = f32::INFINITY;
    for (i, &p) in points.iter().enumerate() {
        let d = (p - v).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best as u32
}

impl EditModel {
    /// Axis grid (width, height) of a param; each is at least 1.
    pub fn param_grid(&self, param: ParamId) -> Result<(u32, u32), EditError> {
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        Ok((
            p.axis_points_x.len().max(1) as u32,
            p.axis_points_y.len().max(1) as u32,
        ))
    }

    fn check_cell(&self, param: ParamId, x: u32, y: u32) -> Result<(), EditError> {
        let (w, h) = self.param_grid(param)?;
        if x >= w || y >= h {
            return Err(EditError::CellOutOfRange);
        }
        Ok(())
    }

    /// The grid cell holding the param's rest pose: nearest keypoint to
    /// `defaults`, mapped into the normalized space the axis points live in.
    fn rest_cell(&self, param: ParamId) -> Result<[u32; 2], EditError> {
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        Ok([
            nearest_axis_index(&p.axis_points_x, normed(p.defaults[0], p.min[0], p.max[0])),
            nearest_axis_index(&p.axis_points_y, normed(p.defaults[1], p.min[1], p.max[1])),
        ])
    }

    /// Whether the binding has no authored cells (or doesn't exist yet).
    fn binding_is_unauthored(&self, param: ParamId, node: NodeId, target: BindingTarget) -> bool {
        self.binding(param, node, target).is_none_or(|b| {
            deform_cells(&b.values)
                .map(<[_]>::is_empty)
                .or_else(|| scalar_cells(&b.values).map(<[_]>::is_empty))
                .unwrap_or(true)
        })
    }

    /// Authoring a binding's *first* cell also authors the identity at the
    /// param's rest keypoint. A single authored cell fills the whole grid, so
    /// without an implicit rest key the first recording would freeze the node
    /// at that value at every parameter position.
    fn author_rest_after_first_key(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
        was_unauthored: bool,
        authored: [u32; 2],
    ) -> Result<(), EditError> {
        if !was_unauthored {
            return Ok(());
        }
        let rest = self.rest_cell(param)?;
        if rest != authored {
            self.reset_binding_key(param, node, target, rest[0], rest[1])?;
        }
        Ok(())
    }

    fn binding_mut(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
    ) -> Result<&mut EditBinding, EditError> {
        let p = self.params.get_mut(param).ok_or(EditError::UnknownParam)?;
        p.bindings
            .iter_mut()
            .find(|b| b.node == node && target_of(&b.values) == target)
            .ok_or(EditError::UnknownBinding)
    }

    pub fn binding(
        &self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
    ) -> Option<&EditBinding> {
        self.param(param)?
            .bindings
            .iter()
            .find(|b| b.node == node && target_of(&b.values) == target)
    }

    /// Ensure `param` drives `node` on `target`, creating an everywhere-unset
    /// binding if it does not exist yet (an unset binding contributes nothing).
    pub fn add_scalar_binding(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: ScalarTarget,
    ) -> Result<(), EditError> {
        if !self.nodes.contains_key(node) {
            return Err(EditError::UnknownNode);
        }
        self.param_grid(param)?;
        let p = self.params.get_mut(param).ok_or(EditError::UnknownParam)?;
        let exists = p
            .bindings
            .iter()
            .any(|b| b.node == node && target_of(&b.values) == BindingTarget::Scalar(target));
        if !exists {
            p.bindings.push(EditBinding {
                node,
                interpolate_mode: InterpolateMode::Linear,
                values: target.wrap(ClpCells::default()).into(),
            });
        }
        Ok(())
    }

    /// Ensure a Deform binding for `node`, everywhere-unset when created.
    pub fn add_deform_binding(&mut self, param: ParamId, node: NodeId) -> Result<(), EditError> {
        match self.node(node).map(|n| &n.kind) {
            Some(EditNodeKind::Part(_)) | Some(EditNodeKind::MeshGroup(_)) => {}
            Some(_) => return Err(EditError::NotAPart),
            None => return Err(EditError::UnknownNode),
        }
        self.param_grid(param)?;
        let p = self.params.get_mut(param).ok_or(EditError::UnknownParam)?;
        let exists = p
            .bindings
            .iter()
            .any(|b| b.node == node && target_of(&b.values) == BindingTarget::Deform);
        if !exists {
            p.bindings.push(EditBinding {
                node,
                interpolate_mode: InterpolateMode::Linear,
                values: ClpBindingValues::Deform(ClpCells::default()).into(),
            });
        }
        Ok(())
    }

    /// Author one keypoint of a scalar binding (auto-creating the binding).
    /// `(x, y)` indexes the param's axis grid.
    pub fn set_binding_key(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: ScalarTarget,
        x: u32,
        y: u32,
        value: f32,
    ) -> Result<(), EditError> {
        // Validate before creating anything — a failed key write must not
        // leave a phantom binding behind.
        self.check_cell(param, x, y)?;
        let was_unauthored = self.binding_is_unauthored(param, node, BindingTarget::Scalar(target));
        self.add_scalar_binding(param, node, target)?;
        let binding = self.binding_mut(param, node, BindingTarget::Scalar(target))?;
        if let Some(cells) = scalar_cells_mut(&mut binding.values) {
            upsert(cells, x, y, value);
        }
        self.author_rest_after_first_key(
            param,
            node,
            BindingTarget::Scalar(target),
            was_unauthored,
            [x, y],
        )
    }

    /// Un-author a keypoint (the cell goes back to derived).
    pub fn unset_binding_key(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
        x: u32,
        y: u32,
    ) -> Result<(), EditError> {
        self.check_cell(param, x, y)?;
        let binding = self.binding_mut(param, node, target)?;
        match &mut *binding.values {
            ClpBindingValues::Deform(c) => c.cells.retain(|c| !(c.x == x && c.y == y)),
            other => {
                if let Some(cells) = scalar_cells_mut(other) {
                    cells.retain(|c| !(c.x == x && c.y == y));
                }
            }
        }
        Ok(())
    }

    /// Author the do-nothing identity value at a keypoint.
    pub fn reset_binding_key(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
        x: u32,
        y: u32,
    ) -> Result<(), EditError> {
        self.check_cell(param, x, y)?;
        let vcount = self.deform_len(node);
        let binding = self.binding_mut(param, node, target)?;
        match &mut *binding.values {
            ClpBindingValues::Deform(c) => upsert(&mut c.cells, x, y, vec![0.0; vcount]),
            other => {
                let identity = match target {
                    BindingTarget::Scalar(t) => t.identity(),
                    BindingTarget::Deform => 0.0,
                };
                if let Some(cells) = scalar_cells_mut(other) {
                    upsert(cells, x, y, identity);
                }
            }
        }
        Ok(())
    }

    pub fn delete_binding(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
    ) -> Result<(), EditError> {
        let p = self.params.get_mut(param).ok_or(EditError::UnknownParam)?;
        let before = p.bindings.len();
        p.bindings
            .retain(|b| !(b.node == node && target_of(&b.values) == target));
        if p.bindings.len() == before {
            return Err(EditError::UnknownBinding);
        }
        Ok(())
    }

    pub fn set_binding_interpolate(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
        mode: InterpolateMode,
    ) -> Result<(), EditError> {
        self.binding_mut(param, node, target)?.interpolate_mode = mode;
        Ok(())
    }

    /// Negate every authored value.
    pub fn invert_binding(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
    ) -> Result<(), EditError> {
        let binding = self.binding_mut(param, node, target)?;
        match &mut *binding.values {
            ClpBindingValues::Deform(c) => {
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
        Ok(())
    }

    /// The evaluated scalar value at a cell: authored if present, otherwise
    /// the derived fill — the single implementation every reader shares.
    pub fn scalar_value_at(
        &self,
        param: ParamId,
        node: NodeId,
        target: ScalarTarget,
        cell: [u32; 2],
    ) -> Result<f32, EditError> {
        self.check_cell(param, cell[0], cell[1])?;
        let (w, h) = self.param_grid(param)?;
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        let binding = self
            .binding(param, node, BindingTarget::Scalar(target))
            .ok_or(EditError::UnknownBinding)?;
        let cells = scalar_cells(&binding.values).unwrap_or(&[]);
        Ok(derived_at(
            cells,
            w,
            h,
            &p.axis_points_x,
            &p.axis_points_y,
            cell,
            &target.identity(),
        ))
    }

    /// The evaluated deform offsets at a cell: authored if present, otherwise
    /// the derived fill; the identity is zeros sized to the node's mesh, so an
    /// everywhere-unset binding evaluates to well-shaped rest offsets.
    pub fn deform_value_at(
        &self,
        param: ParamId,
        node: NodeId,
        cell: [u32; 2],
    ) -> Result<Vec<f32>, EditError> {
        self.check_cell(param, cell[0], cell[1])?;
        let (w, h) = self.param_grid(param)?;
        let identity = vec![0.0f32; self.deform_len(node)];
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        let cells = self
            .binding(param, node, BindingTarget::Deform)
            .and_then(|b| deform_cells(&b.values))
            .unwrap_or(&[]);
        Ok(derived_at(
            cells,
            w,
            h,
            &p.axis_points_x,
            &p.axis_points_y,
            cell,
            &identity,
        ))
    }

    /// Copy the (derived-or-authored) value at `from` and author it at `to`.
    pub fn copy_binding_key(
        &mut self,
        param: ParamId,
        node: NodeId,
        target: BindingTarget,
        from: [u32; 2],
        to: [u32; 2],
    ) -> Result<(), EditError> {
        self.check_cell(param, to[0], to[1])?;
        let was_unauthored = self.binding_is_unauthored(param, node, target);
        match target {
            BindingTarget::Deform => {
                if self.binding(param, node, target).is_none() {
                    return Err(EditError::UnknownBinding);
                }
                let value = self.deform_value_at(param, node, from)?;
                let binding = self.binding_mut(param, node, target)?;
                if let ClpBindingValues::Deform(c) = &mut *binding.values {
                    upsert(&mut c.cells, to[0], to[1], value);
                }
            }
            BindingTarget::Scalar(t) => {
                let value = self.scalar_value_at(param, node, t, from)?;
                let binding = self.binding_mut(param, node, target)?;
                if let Some(cells) = scalar_cells_mut(&mut binding.values) {
                    upsert(cells, to[0], to[1], value);
                }
            }
        }
        self.author_rest_after_first_key(param, node, target, was_unauthored, to)
    }

    /// Author per-vertex deform offsets at a cell. `offsets` is flat
    /// `[dx, dy, …]` and must match the node's mesh.
    pub fn set_deform_vertices(
        &mut self,
        param: ParamId,
        node: NodeId,
        cell: [u32; 2],
        offsets: Vec<f32>,
    ) -> Result<(), EditError> {
        let expected = self.deform_len(node);
        if expected == 0 || offsets.len() != expected {
            return Err(EditError::NotAPart);
        }
        self.check_cell(param, cell[0], cell[1])?;
        let was_unauthored = self.binding_is_unauthored(param, node, BindingTarget::Deform);
        self.add_deform_binding(param, node)?;
        let binding = self.binding_mut(param, node, BindingTarget::Deform)?;
        if let ClpBindingValues::Deform(c) = &mut *binding.values {
            upsert(&mut c.cells, cell[0], cell[1], offsets);
        }
        self.author_rest_after_first_key(param, node, BindingTarget::Deform, was_unauthored, cell)
    }

    /// Author a deform keypoint by applying an affine (scale, then rotate, then
    /// translate — about the part's mesh origin) to the part's rest vertices and
    /// storing the resulting per-vertex offsets in `cell`.
    pub fn set_deform_from_transform(
        &mut self,
        param: ParamId,
        node: NodeId,
        cell: [u32; 2],
        translate: [f32; 2],
        rotate: f32,
        scale: [f32; 2],
    ) -> Result<(), EditError> {
        let (verts, origin) = match self.node(node).map(|n| &n.kind) {
            Some(EditNodeKind::Part(p)) => (p.mesh.verts.clone(), p.mesh.origin),
            Some(EditNodeKind::MeshGroup(mg)) => (mg.mesh.verts.clone(), mg.mesh.origin),
            Some(_) => return Err(EditError::NotAPart),
            None => return Err(EditError::UnknownNode),
        };
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
        self.set_deform_vertices(param, node, cell, offsets)
    }

    /// Flat length of the node's mesh vertex array (`2 * vertex count`).
    pub fn deform_len(&self, node: NodeId) -> usize {
        match self.node(node).map(|n| &n.kind) {
            Some(EditNodeKind::Part(p)) => p.mesh.verts.len(),
            Some(EditNodeKind::MeshGroup(mg)) => mg.mesh.verts.len(),
            _ => 0,
        }
    }

    // ---- param structure ----

    pub fn set_param_name(&mut self, param: ParamId, name: String) -> Result<(), EditError> {
        self.param_mut(param).ok_or(EditError::UnknownParam)?.name = name;
        Ok(())
    }

    pub fn set_param_defaults(
        &mut self,
        param: ParamId,
        defaults: [f32; 2],
    ) -> Result<(), EditError> {
        self.param_mut(param)
            .ok_or(EditError::UnknownParam)?
            .defaults = defaults;
        Ok(())
    }

    /// Change the param's range, rescaling every axis point proportionally so
    /// keypoints keep their relative positions (authored cells are index-keyed
    /// and don't move).
    pub fn set_param_range(
        &mut self,
        param: ParamId,
        min: [f32; 2],
        max: [f32; 2],
    ) -> Result<(), EditError> {
        let p = self.param_mut(param).ok_or(EditError::UnknownParam)?;
        // A collapsed or inverted range can't map poses onto the normalized
        // axis. The Y axis of a 1D param legitimately stays [0, 0]. Axis
        // points are normalized, so the keypoints themselves don't move.
        if !param_range_is_valid(p.is_vec2, min, max) {
            return Err(EditError::CellOutOfRange);
        }
        p.min = min;
        p.max = max;
        Ok(())
    }

    /// Axis 0 is x; axis 1 is y and only exists on vec2 params.
    fn check_axis(&self, param: ParamId, axis: u8) -> Result<(), EditError> {
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        if axis > 1 || (axis == 1 && !p.is_vec2) {
            return Err(EditError::IndexOutOfRange);
        }
        Ok(())
    }

    /// Insert an axis point at normalized `value` (strictly inside (0, 1),
    /// distinct from existing points). Authored cells at or past the
    /// insertion index shift over; no new cells are authored — the new
    /// column/row derives.
    pub fn axis_insert(
        &mut self,
        param: ParamId,
        axis: u8,
        value: f32,
    ) -> Result<usize, EditError> {
        self.check_axis(param, axis)?;
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        let points = if axis == 0 {
            &p.axis_points_x
        } else {
            &p.axis_points_y
        };
        if points.iter().any(|&v| (v - value).abs() <= f32::EPSILON) {
            return Err(EditError::CellOutOfRange);
        }
        if !(value > 0.0 && value < 1.0) {
            return Err(EditError::CellOutOfRange);
        }
        let idx = points.iter().take_while(|&&v| v < value).count();

        let p = self.param_mut(param).ok_or(EditError::UnknownParam)?;
        if axis == 0 {
            p.axis_points_x.insert(idx, value);
        } else {
            p.axis_points_y.insert(idx, value);
        }
        shift_cells(p, axis, |coord| {
            if coord >= idx as u32 {
                coord + 1
            } else {
                coord
            }
        });
        Ok(idx)
    }

    /// Remove an interior axis point; its authored cells are dropped and the
    /// rest shift back.
    pub fn axis_delete(&mut self, param: ParamId, axis: u8, index: usize) -> Result<(), EditError> {
        self.check_axis(param, axis)?;
        let p = self.param(param).ok_or(EditError::UnknownParam)?;
        let len = if axis == 0 {
            p.axis_points_x.len()
        } else {
            p.axis_points_y.len()
        };
        if index == 0 || index + 1 >= len {
            // Endpoints define the range; they can't be removed.
            return Err(EditError::IndexOutOfRange);
        }
        let p = self.param_mut(param).ok_or(EditError::UnknownParam)?;
        if axis == 0 {
            p.axis_points_x.remove(index);
        } else {
            p.axis_points_y.remove(index);
        }
        drop_cells_at(p, axis, index as u32);
        shift_cells(p, axis, |coord| {
            if coord > index as u32 {
                coord - 1
            } else {
                coord
            }
        });
        Ok(())
    }

    /// Mirror a param along an axis: axis-point positions reflect within the
    /// normalized range and every binding cell moves to the mirrored index.
    /// Values are untouched (compose with `invert_binding` for negating
    /// semantics).
    pub fn param_flip(&mut self, param: ParamId, axis: u8) -> Result<(), EditError> {
        self.check_axis(param, axis)?;
        let p = self.param_mut(param).ok_or(EditError::UnknownParam)?;
        let points = if axis == 0 {
            &mut p.axis_points_x
        } else {
            &mut p.axis_points_y
        };
        for v in points.iter_mut() {
            *v = 1.0 - *v;
        }
        points.reverse();
        let len = points.len() as u32;
        shift_cells(p, axis, move |coord| {
            len.saturating_sub(1).saturating_sub(coord)
        });
        Ok(())
    }

    /// Move an interior axis point to normalized `value`; it must stay
    /// strictly between neighbors. Cells are index-keyed and stay authored.
    pub fn axis_move(
        &mut self,
        param: ParamId,
        axis: u8,
        index: usize,
        value: f32,
    ) -> Result<(), EditError> {
        self.check_axis(param, axis)?;
        let p = self.param_mut(param).ok_or(EditError::UnknownParam)?;
        let points = if axis == 0 {
            &mut p.axis_points_x
        } else {
            &mut p.axis_points_y
        };
        if index == 0 || index + 1 >= points.len() {
            return Err(EditError::IndexOutOfRange);
        }
        if value <= points[index - 1] || value >= points[index + 1] {
            return Err(EditError::CellOutOfRange);
        }
        points[index] = value;
        Ok(())
    }
}

/// Value of a cell as evaluated — authored value if present, derived otherwise.
fn derived_at<T: FillCell>(
    cells: &[ClpCell<T>],
    w: u32,
    h: u32,
    axis_x: &[f32],
    axis_y: &[f32],
    at: [u32; 2],
    identity: &T,
) -> T {
    if let Some(c) = cells.iter().find(|c| c.x == at[0] && c.y == at[1]) {
        return c.value.clone();
    }
    let authored: Vec<((u32, u32), T)> = cells
        .iter()
        .map(|c| ((c.x, c.y), c.value.clone()))
        .collect();
    let dense = derive_dense(w as usize, h as usize, axis_x, axis_y, &authored, identity);
    dense
        .into_iter()
        .nth((at[1] * w + at[0]) as usize)
        .unwrap_or_else(|| identity.clone())
}

fn shift_cells(p: &mut EditParam, axis: u8, f: impl Fn(u32) -> u32) {
    for b in &mut p.bindings {
        match &mut *b.values {
            ClpBindingValues::Deform(c) => {
                for cell in &mut c.cells {
                    if axis == 0 {
                        cell.x = f(cell.x);
                    } else {
                        cell.y = f(cell.y);
                    }
                }
                c.cells.sort_by_key(|c| (c.y, c.x));
            }
            other => {
                if let Some(cells) = scalar_cells_mut(other) {
                    for cell in cells.iter_mut() {
                        if axis == 0 {
                            cell.x = f(cell.x);
                        } else {
                            cell.y = f(cell.y);
                        }
                    }
                    cells.sort_by_key(|c| (c.y, c.x));
                }
            }
        }
    }
}

fn drop_cells_at(p: &mut EditParam, axis: u8, coord: u32) {
    for b in &mut p.bindings {
        match &mut *b.values {
            ClpBindingValues::Deform(c) => {
                c.cells.retain(|c| {
                    if axis == 0 {
                        c.x != coord
                    } else {
                        c.y != coord
                    }
                });
            }
            other => {
                if let Some(cells) = scalar_cells_mut(other) {
                    cells.retain(|c| {
                        if axis == 0 {
                            c.x != coord
                        } else {
                            c.y != coord
                        }
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_range_rejects_inverted_collapsed_and_nan() {
        assert!(param_range_is_valid(false, [0.0, 0.0], [1.0, 0.0]));
        assert!(param_range_is_valid(true, [-1.0, -1.0], [1.0, 1.0]));
        assert!(!param_range_is_valid(false, [1.0, 0.0], [0.0, 0.0]));
        assert!(!param_range_is_valid(false, [0.0, 0.0], [0.0, 0.0]));
        assert!(!param_range_is_valid(true, [0.0, 1.0], [1.0, 0.0]));
        assert!(!param_range_is_valid(false, [f32::NAN, 0.0], [1.0, 0.0]));
    }

    fn param_1d(m: &mut EditModel) -> ParamId {
        m.add_param(EditParam {
            name: "x".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 0.5, 1.0],
            axis_points_y: vec![0.0],
            bindings: Vec::new(),
        })
    }

    #[test]
    fn set_unset_reset_key_roundtrip() {
        let mut m = EditModel::new();
        let root = m.root();
        let node = m
            .add_node(root, EditNode::new("p", EditNodeKind::Empty))
            .unwrap();
        let param = param_1d(&mut m);

        m.set_binding_key(param, node, ScalarTarget::Tx, 2, 0, 60.0)
            .unwrap();
        m.set_binding_key(param, node, ScalarTarget::Tx, 0, 0, -60.0)
            .unwrap();
        // one binding; the first key also authored the rest identity at x=1.
        assert_eq!(m.param(param).unwrap().bindings.len(), 1);
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Tx))
            .unwrap();
        let cells = scalar_cells(&b.values).unwrap();
        assert_eq!(
            cells.iter().map(|c| (c.x, c.value)).collect::<Vec<_>>(),
            vec![(0, -60.0), (1, 0.0), (2, 60.0)]
        );

        m.unset_binding_key(param, node, BindingTarget::Scalar(ScalarTarget::Tx), 0, 0)
            .unwrap();
        m.reset_binding_key(param, node, BindingTarget::Scalar(ScalarTarget::Tx), 1, 0)
            .unwrap();
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Tx))
            .unwrap();
        let cells = scalar_cells(&b.values).unwrap();
        assert_eq!(
            cells.iter().map(|c| (c.x, c.value)).collect::<Vec<_>>(),
            vec![(1, 0.0), (2, 60.0)]
        );

        assert!(m
            .set_binding_key(param, node, ScalarTarget::Tx, 3, 0, 1.0)
            .is_err());
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn copy_key_takes_derived_values() {
        let mut m = EditModel::new();
        let root = m.root();
        let node = m
            .add_node(root, EditNode::new("p", EditNodeKind::Empty))
            .unwrap();
        let param = param_1d(&mut m);
        m.set_binding_key(param, node, ScalarTarget::Tx, 0, 0, -60.0)
            .unwrap();
        m.set_binding_key(param, node, ScalarTarget::Tx, 2, 0, 60.0)
            .unwrap();
        // cell 1 is derived (midpoint = 0); copying it to cell 2 authors 0 there.
        m.copy_binding_key(
            param,
            node,
            BindingTarget::Scalar(ScalarTarget::Tx),
            [1, 0],
            [2, 0],
        )
        .unwrap();
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Tx))
            .unwrap();
        let cells = scalar_cells(&b.values).unwrap();
        assert_eq!(cells.iter().find(|c| c.x == 2).unwrap().value, 0.0);
    }

    #[test]
    fn invert_and_delete_binding() {
        let mut m = EditModel::new();
        let root = m.root();
        let node = m
            .add_node(root, EditNode::new("p", EditNodeKind::Empty))
            .unwrap();
        let param = param_1d(&mut m);
        m.set_binding_key(param, node, ScalarTarget::Rz, 2, 0, 0.5)
            .unwrap();
        m.invert_binding(param, node, BindingTarget::Scalar(ScalarTarget::Rz))
            .unwrap();
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Rz))
            .unwrap();
        let cells = scalar_cells(&b.values).unwrap();
        assert_eq!(cells.iter().find(|c| c.x == 2).unwrap().value, -0.5);

        m.delete_binding(param, node, BindingTarget::Scalar(ScalarTarget::Rz))
            .unwrap();
        assert!(m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Rz))
            .is_none());
        assert!(m
            .delete_binding(param, node, BindingTarget::Scalar(ScalarTarget::Rz))
            .is_err());
    }

    #[test]
    fn deform_from_transform_writes_offsets() {
        use catchlight_core::components::BlendMode;
        use catchlight_core::formats::clp::{ClpIndices, ClpMesh};

        let mut m = EditModel::new();
        let root = m.root();
        let part = m
            .add_node(
                root,
                EditNode::new(
                    "q",
                    EditNodeKind::Part(EditPart {
                        mesh: ClpMesh {
                            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
                            uvs: vec![0.0; 8],
                            indices: ClpIndices::U16(vec![0, 1, 2, 0, 2, 3]),
                            origin: [0.0, 0.0],
                        }
                        .into(),
                        albedo: None,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        tint: [1.0; 3],
                        screen_tint: [0.0; 3],
                        masks: Vec::new(),
                        mask_threshold: 0.5,
                    }),
                ),
            )
            .unwrap();
        let param = m.add_param(EditParam {
            name: "d".into(),
            is_vec2: false,
            min: [0.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 1.0],
            axis_points_y: vec![0.0],
            bindings: Vec::new(),
        });
        m.set_deform_from_transform(param, part, [1, 0], [10.0, 0.0], 0.0, [1.0, 1.0])
            .unwrap();
        let b = m.binding(param, part, BindingTarget::Deform).unwrap();
        let cells = deform_cells(&b.values).unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].value, vec![0.0; 8]);
        assert_eq!(
            cells[1].value,
            vec![10.0, 0.0, 10.0, 0.0, 10.0, 0.0, 10.0, 0.0]
        );
        // wrong-length vertex writes are refused.
        assert!(m
            .set_deform_vertices(param, part, [0, 0], vec![1.0, 2.0])
            .is_err());
        assert!(m.to_clp_bytes().is_ok());
    }

    #[test]
    fn axis_ops_remap_authored_cells() {
        let mut m = EditModel::new();
        let root = m.root();
        let node = m
            .add_node(root, EditNode::new("p", EditNodeKind::Empty))
            .unwrap();
        let param = param_1d(&mut m);
        m.set_binding_key(param, node, ScalarTarget::Tx, 0, 0, -60.0)
            .unwrap();
        m.set_binding_key(param, node, ScalarTarget::Tx, 2, 0, 60.0)
            .unwrap();

        // insert between 0.5 and 1.0 → index 2; the authored cell at 2 shifts to 3.
        let idx = m.axis_insert(param, 0, 0.75).unwrap();
        assert_eq!(idx, 2);
        assert_eq!(
            m.param(param).unwrap().axis_points_x,
            vec![0.0, 0.5, 0.75, 1.0]
        );
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Tx))
            .unwrap();
        let xs: Vec<u32> = scalar_cells(&b.values)
            .unwrap()
            .iter()
            .map(|c| c.x)
            .collect();
        assert_eq!(xs, vec![0, 1, 3]);

        // endpoints can't be deleted; duplicates and out-of-range inserts rejected.
        assert!(m.axis_delete(param, 0, 0).is_err());
        assert!(m.axis_insert(param, 0, 0.5).is_err());
        assert!(m.axis_insert(param, 0, 2.0).is_err());

        // move the inserted point (must stay between neighbors).
        m.axis_move(param, 0, 2, 0.6).unwrap();
        assert!(m.axis_move(param, 0, 2, 0.4).is_err());

        // deleting it keeps the shifted cells consistent.
        m.axis_delete(param, 0, 2).unwrap();
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Tx))
            .unwrap();
        let xs: Vec<u32> = scalar_cells(&b.values)
            .unwrap()
            .iter()
            .map(|c| c.x)
            .collect();
        assert_eq!(xs, vec![0, 1, 2]);

        // a range change leaves the normalized axis points alone.
        m.set_param_range(param, [0.0, 0.0], [4.0, 0.0]).unwrap();
        assert_eq!(m.param(param).unwrap().axis_points_x, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn param_flip_mirrors_axis_points_and_cells() {
        let mut m = EditModel::new();
        let root = m.root();
        let node = m
            .add_node(root, EditNode::new("p", EditNodeKind::Empty))
            .unwrap();
        let param = m.add_param(EditParam {
            name: "x".into(),
            is_vec2: false,
            min: [-1.0, 0.0],
            max: [1.0, 0.0],
            defaults: [0.0, 0.0],
            axis_points_x: vec![0.0, 0.75, 1.0],
            axis_points_y: vec![0.0],
            bindings: Vec::new(),
        });
        m.set_binding_key(param, node, ScalarTarget::Tx, 0, 0, -60.0)
            .unwrap();
        m.set_binding_key(param, node, ScalarTarget::Tx, 1, 0, 10.0)
            .unwrap();

        m.param_flip(param, 0).unwrap();
        // 0, 0.75, 1 reflect to 1, 0.25, 0, then reverse to stay ascending.
        assert_eq!(m.param(param).unwrap().axis_points_x, vec![0.0, 0.25, 1.0]);
        let b = m
            .binding(param, node, BindingTarget::Scalar(ScalarTarget::Tx))
            .unwrap();
        let cells: Vec<(u32, f32)> = scalar_cells(&b.values)
            .unwrap()
            .iter()
            .map(|c| (c.x, c.value))
            .collect();
        // cell 0 -> 2, cell 1 -> 1; values untouched.
        assert_eq!(cells, vec![(1, 10.0), (2, -60.0)]);
    }
}
