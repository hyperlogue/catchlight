use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIdx(pub u32);

impl NodeIdx {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

/// Identifier for a GPU mesh buffer. Currently allocated one-per-Part
/// from its NodeIdx, but kept as a distinct newtype so callers can't mix
/// mesh_id and texture_id arguments, and so we can swap in a sharing
/// strategy later without touching the renderer surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MeshId(pub u32);

impl MeshId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

/// Identifier for an entry in the puppet's texture table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureId(pub u32);

impl TextureId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

/// Canonical texture representation in a Puppet: pre-decoded bytes
/// encoding premultiplied LINEAR colour, ready for any renderer to upload
/// as `Rgba8UnormSrgb` (the sampler's decode then hands shaders
/// premultiplied linear). The importer owns the conversion from source-file
/// alpha conventions, including premultiplied sRGB, into
/// this shape; consumers downstream never need to know the origin format.
///
/// Premultiplied storage is what makes filtering correct at an alpha edge
/// without a separate bleed pass: the filter mixes `(rgb*a, a)` with
/// `(0, 0)` and the result is already premultiplied.
#[derive(Debug, Clone)]
pub struct PuppetTexture {
    pub width: u32,
    pub height: u32,
    /// RGBA8 premultiplied-linear encoded as sRGB bytes, tightly packed
    /// (`4 * width` bytes per row).
    pub rgba: Arc<[u8]>,
}

impl PuppetTexture {
    /// Half-resolution copy via the same linear-space box filter renderers
    /// use for mip generation, so importing at `halved()` resolution is
    /// texel-identical to sampling mip 1 of the full texture. Normalized
    /// UVs are unaffected. Used to right-size import resolution for
    /// deployments whose maximum on-screen sampling rate never reaches
    /// the authored texel density.
    pub fn halved(&self) -> PuppetTexture {
        let (width, height, rgba) = downsample_box_filter(&self.rgba, self.width, self.height);
        PuppetTexture {
            width,
            height,
            rgba: rgba.into(),
        }
    }
}

pub fn downsample_box_filter(src: &[u8], w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    // Mip averaging must happen in LINEAR space. The texture is sampled as
    // `Rgba8UnormSrgb` (sRGB→linear decode at draw time), so averaging the
    // gamma-encoded RGB bytes directly biases minified texels dark and
    // over-sharpens high-contrast seams (verified against a 4x-supersampled
    // ground truth on the reference rig's shirt). Decode RGB → average in linear →
    // re-encode. Alpha is stored linear (sRGB formats don't gamma the A
    // channel), so it averages directly.
    let dw = (w / 2).max(1);
    let dh = (h / 2).max(1);
    let decode = srgb_decode_table();
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        for x in 0..dw {
            let sx = x * 2;
            let sy = y * 2;
            let mut rgb = [0.0f32; 3];
            let mut a = 0.0f32;
            let mut n = 0.0f32;
            for oy in 0..2 {
                for ox in 0..2 {
                    let ssx = (sx + ox).min(w - 1);
                    let ssy = (sy + oy).min(h - 1);
                    let i = ((ssy * w + ssx) * 4) as usize;
                    rgb[0] += decode[src[i] as usize];
                    rgb[1] += decode[src[i + 1] as usize];
                    rgb[2] += decode[src[i + 2] as usize];
                    a += src[i + 3] as f32;
                    n += 1.0;
                }
            }
            let di = ((y * dw + x) * 4) as usize;
            out[di] = srgb_encode_to_byte(rgb[0] / n);
            out[di + 1] = srgb_encode_to_byte(rgb[1] / n);
            out[di + 2] = srgb_encode_to_byte(rgb[2] / n);
            out[di + 3] = (a / n + 0.5) as u8;
        }
    }
    (dw, dh, out)
}

/// The sRGB transfer function's inverse on a normalized [0, 1] signal.
/// Only used to build the tables below; the pixel paths go through those.
fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Every sRGB byte's linear value. The input domain is 256 values wide,
/// so this is the whole function, not an approximation of it.
static SRGB_DECODE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();

/// Hoist this out of a per-texel loop and index it directly: the
/// `OnceLock` probe costs several times the array read it guards, so
/// probing it per channel is markedly slower than binding it once.
pub(crate) fn srgb_decode_table() -> &'static [f32; 256] {
    SRGB_DECODE.get_or_init(|| std::array::from_fn(|b| srgb_to_linear(b as f32 / 255.0)))
}

/// Unlike the decode this takes an `f32`, so reaching a table means
/// searching for the input's place among the 255 step points — branches
/// that image-derived values leave unpredictable, and that measured no
/// faster than the `powf` they would replace.
#[inline]
pub(crate) fn srgb_encode_to_byte(linear: f32) -> u8 {
    let s = if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Each node carries two transforms: `base_transform` / `base_z_order`
/// are the loader-parsed values, set once and never mutated; `transform`
/// / `z_order` are the per-frame working copies that `Puppet::
/// reset_dynamic_state` resets to base at frame start and
/// `Puppet::apply_params` additively modifies. Callers that don't use
/// parameters see identical behavior: base == working at load, nothing
/// changes between frames.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub enabled: bool,
    pub transform: Transform,
    pub z_order: f32,
    pub base_transform: Transform,
    pub base_z_order: f32,
    pub lock_to_root: bool,
    pub kind: NodeKind,
}

impl Default for Node {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            transform: Transform::default(),
            z_order: 0.0,
            base_transform: Transform::default(),
            base_z_order: 0.0,
            lock_to_root: false,
            kind: NodeKind::Empty,
        }
    }
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    Empty,
    Part(Box<PartData>),
    Composite(Box<CompositeData>),
    MeshGroup(Box<MeshGroupData>),
    SimplePhysics(Box<crate::physics::SimplePhysicsData>),
}

const _: () = assert!(
    std::mem::size_of::<NodeKind>() <= 48,
    "NodeKind is too large - consider boxing new variants"
);

#[derive(Debug, Clone)]
pub struct PartData {
    pub mesh: Mesh,
    pub albedo_texture: TextureId,
    pub opacity: f32,
    pub base_opacity: f32,
    pub tint: Vec3,
    pub base_tint: Vec3,
    pub screen_tint: Vec3,
    pub base_screen_tint: Vec3,
    pub blend_mode: BlendMode,
    pub masks: Vec<Mask>,
    pub mask_threshold: f32,
    pub deform_stack: crate::deform::DeformStack,
}

impl Default for PartData {
    fn default() -> Self {
        Self {
            mesh: Mesh::default(),
            albedo_texture: TextureId(0),
            opacity: 1.0,
            base_opacity: 1.0,
            tint: Vec3::ONE,
            base_tint: Vec3::ONE,
            screen_tint: Vec3::ZERO,
            base_screen_tint: Vec3::ZERO,
            blend_mode: BlendMode::Normal,
            masks: Vec::new(),
            mask_threshold: 0.5,
            deform_stack: crate::deform::DeformStack::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompositeData {
    pub opacity: f32,
    pub base_opacity: f32,
    pub tint: Vec3,
    pub base_tint: Vec3,
    pub screen_tint: Vec3,
    pub base_screen_tint: Vec3,
    pub blend_mode: BlendMode,
    pub masks: Vec<Mask>,
    pub propagate_mesh_group: bool,
    pub mask_threshold: f32,
}

impl Default for CompositeData {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            base_opacity: 1.0,
            tint: Vec3::ONE,
            base_tint: Vec3::ONE,
            screen_tint: Vec3::ZERO,
            base_screen_tint: Vec3::ZERO,
            blend_mode: BlendMode::Normal,
            masks: Vec::new(),
            propagate_mesh_group: true,
            mask_threshold: 0.5,
        }
    }
}

/// A mesh group is never drawn (`drawable_collector` skips it), so it carries
/// no colour at all — the fields a drawable has for it are absent here, and a
/// `.clp` binding that drives colour on a mesh group is rejected at load; see
/// [`crate::params::MeshGroupColorBindingError`].
#[derive(Debug, Clone)]
pub struct MeshGroupData {
    pub mesh: Mesh,
    pub dynamic: bool,
    pub translate_children: bool,
    pub deform_stack: crate::deform::DeformStack,
    pub(crate) attachments: crate::meshgroup::MeshGroupAttachments,
    /// O(1) point-in-triangle bitmap baked from `mesh` at load time
    /// (alongside `attachments`). `None` when the mesh is empty or
    /// degenerate; the dynamic-MG propagation path then falls back to
    /// the linear hinted scan.
    pub(crate) bitmap: Option<crate::meshgroup::MgTriangleBitmap>,
}

impl Default for MeshGroupData {
    fn default() -> Self {
        Self {
            mesh: Mesh::default(),
            dynamic: false,
            translate_children: true,
            deform_stack: crate::deform::DeformStack::default(),
            attachments: crate::meshgroup::MeshGroupAttachments::default(),
            bitmap: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Vec3,
    pub scale: Vec2,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: Vec3::ZERO,
            rotation: Vec3::ZERO,
            scale: Vec2::ONE,
        }
    }
}

impl Transform {
    pub fn to_matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(
            self.scale.extend(1.0),
            Quat::from_euler(
                EulerRot::XYZ,
                self.rotation.x,
                self.rotation.y,
                self.rotation.z,
            ),
            self.translation,
        )
    }
}

pub fn checked_affine_inverse(matrix: Mat4) -> Option<Mat4> {
    // Smaller determinants amplify ordinary coordinates into unstable values;
    // treat them as singular even when glam can produce a finite inverse.
    const MIN_ABS_DETERMINANT: f32 = 1e-12;
    if !matrix.is_finite() {
        return None;
    }
    let determinant = matrix.determinant();
    if !determinant.is_finite() || determinant.abs() < MIN_ABS_DETERMINANT {
        return None;
    }
    let inverse = matrix.inverse();
    inverse.is_finite().then_some(inverse)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshIndices {
    U16(Vec<u16>),
    U32(Vec<u32>),
}

impl MeshIndices {
    pub fn len(&self) -> usize {
        match self {
            MeshIndices::U16(v) => v.len(),
            MeshIndices::U32(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter_u32(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        match self {
            MeshIndices::U16(v) => Box::new(v.iter().map(|&i| i as u32)),
            MeshIndices::U32(v) => Box::new(v.iter().copied()),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            MeshIndices::U16(v) => bytemuck::cast_slice(v),
            MeshIndices::U32(v) => bytemuck::cast_slice(v),
        }
    }

    pub fn get(&self, i: usize) -> Option<u32> {
        match self {
            MeshIndices::U16(v) => v.get(i).map(|&n| n as u32),
            MeshIndices::U32(v) => v.get(i).copied(),
        }
    }

    pub fn from_usize_iter(iter: impl IntoIterator<Item = usize>) -> Self {
        let v: Vec<usize> = iter.into_iter().collect();
        let max = v.iter().copied().max().unwrap_or(0);
        if max <= u16::MAX as usize {
            MeshIndices::U16(v.into_iter().map(|i| i as u16).collect())
        } else {
            MeshIndices::U32(v.into_iter().map(|i| i as u32).collect())
        }
    }
}

impl Default for MeshIndices {
    fn default() -> Self {
        MeshIndices::U16(Vec::new())
    }
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub vertices: Vec<Vec2>,
    pub uvs: Vec<Vec2>,
    pub indices: MeshIndices,
    pub origin: Vec2,
}

impl Default for Mesh {
    fn default() -> Self {
        Self {
            vertices: Vec::new(),
            uvs: Vec::new(),
            indices: MeshIndices::default(),
            origin: Vec2::ZERO,
        }
    }
}

impl Mesh {
    pub fn new(vertices: Vec<Vec2>, uvs: Vec<Vec2>, indices: MeshIndices, origin: Vec2) -> Self {
        Self {
            vertices,
            uvs,
            indices,
            origin,
        }
    }

    pub fn quad(width: f32, height: f32) -> Self {
        let hw = width / 2.0;
        let hh = height / 2.0;

        let vertices = vec![
            Vec2::new(-hw, -hh),
            Vec2::new(hw, -hh),
            Vec2::new(hw, hh),
            Vec2::new(-hw, hh),
        ];

        let uvs = vec![
            Vec2::new(0.0, 1.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 0.0),
        ];

        let indices = MeshIndices::U16(vec![0, 1, 2, 2, 3, 0]);

        Self::new(vertices, uvs, indices, Vec2::ZERO)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    ColorDodge,
    LinearDodge,
    Screen,
    ClipToLower,
    SliceFromLower,
    Overlay,
    ColorBurn,
    LinearBurn,
    Darken,
    Lighten,
    Add,
    Inverse,
    Subtract,
}

impl BlendMode {
    /// Parse a serialized blend-mode name without silently falling back.
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "Normal" => BlendMode::Normal,
            "Multiply" => BlendMode::Multiply,
            "ColorDodge" => BlendMode::ColorDodge,
            "LinearDodge" => BlendMode::LinearDodge,
            "Screen" => BlendMode::Screen,
            "ClipToLower" => BlendMode::ClipToLower,
            "SliceFromLower" => BlendMode::SliceFromLower,
            "Overlay" => BlendMode::Overlay,
            "ColorBurn" => BlendMode::ColorBurn,
            "LinearBurn" => BlendMode::LinearBurn,
            "Darken" => BlendMode::Darken,
            "Lighten" => BlendMode::Lighten,
            "Add" => BlendMode::Add,
            "Inverse" => BlendMode::Inverse,
            "Subtract" => BlendMode::Subtract,
            _ => return None,
        })
    }
}

impl BlendMode {
    /// Canonical serialized name used by editors and serializers.
    pub fn as_str(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::ColorDodge => "ColorDodge",
            BlendMode::LinearDodge => "LinearDodge",
            BlendMode::Screen => "Screen",
            BlendMode::ClipToLower => "ClipToLower",
            BlendMode::SliceFromLower => "SliceFromLower",
            BlendMode::Overlay => "Overlay",
            BlendMode::ColorBurn => "ColorBurn",
            BlendMode::LinearBurn => "LinearBurn",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::Add => "Add",
            BlendMode::Inverse => "Inverse",
            BlendMode::Subtract => "Subtract",
        }
    }
}

impl std::str::FromStr for BlendMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| s.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MaskMode {
    Mask,
    DodgeMask,
}

#[derive(Debug, Clone)]
pub struct Mask {
    pub source_uuid: u32,
    pub mode: MaskMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_uniform_4x4_to_2x2_preserves_color() {
        let src = vec![255u8; 4 * 4 * 4];
        let (dw, dh, out) = downsample_box_filter(&src, 4, 4);
        assert_eq!((dw, dh), (2, 2));
        assert!(out.iter().all(|&b| b == 255));
    }

    #[test]
    fn downsample_2x2_to_1x1_averages_four_samples() {
        // Red and green diagonal, black on off-diagonal, full alpha.
        // RGB is averaged in LINEAR space: one lit texel of four -> mean
        // linear 0.25 -> sRGB byte ~137 (a gamma-space byte mean would be
        // 63). B has no lit texel -> 0; alpha is linear, all 255 -> 255.
        let src: Vec<u8> = vec![
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 0, 255, 0, 0, 0, 255, //
        ];
        let (dw, dh, out) = downsample_box_filter(&src, 2, 2);
        assert_eq!((dw, dh), (1, 1));
        assert!((136..=138).contains(&out[0]), "R was {}", out[0]);
        assert!((136..=138).contains(&out[1]), "G was {}", out[1]);
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 255);
    }

    #[test]
    fn downsample_1x1_to_1x1_clamps_sampling_coordinates() {
        // With w=h=1 the 2x2 sampling box would read indices (0..2, 0..2)
        // without clamping and index out of bounds. Output should equal
        // input after min(w-1)/min(h-1) clamp.
        let src = vec![7u8, 8, 9, 10];
        let (dw, dh, out) = downsample_box_filter(&src, 1, 1);
        assert_eq!((dw, dh), (1, 1));
        assert_eq!(out, src);
    }

    #[test]
    fn transform_default() {
        let transform = Transform::default();
        assert_eq!(transform.translation, Vec3::ZERO);
        assert_eq!(transform.rotation, Vec3::ZERO);
        assert_eq!(transform.scale, Vec2::ONE);
    }

    #[test]
    fn transform_to_matrix_identity() {
        let transform = Transform::default();
        let matrix = transform.to_matrix();
        assert_eq!(matrix, Mat4::IDENTITY);
    }

    #[test]
    fn transform_to_matrix_translation() {
        let transform = Transform {
            translation: Vec3::new(10.0, 20.0, 0.0),
            ..Transform::default()
        };
        let matrix = transform.to_matrix();

        let point = matrix.transform_point3(Vec3::ZERO);
        assert_eq!(point, Vec3::new(10.0, 20.0, 0.0));
    }

    #[test]
    fn checked_inverse_rejects_singular_and_unstable_affines() {
        assert!(checked_affine_inverse(Mat4::from_scale(Vec3::new(0.0, 1.0, 1.0))).is_none());
        assert!(checked_affine_inverse(Mat4::from_scale(Vec3::new(1e-7, 1e-7, 1.0))).is_none());
        assert!(checked_affine_inverse(Mat4::from_translation(Vec3::splat(f32::NAN))).is_none());

        let matrix = Mat4::from_scale_rotation_translation(
            Vec3::new(2.0, 3.0, 1.0),
            Quat::from_rotation_z(0.4),
            Vec3::new(5.0, -2.0, 0.0),
        );
        let inverse = checked_affine_inverse(matrix).expect("stable affine inverse");
        assert!((matrix * inverse).abs_diff_eq(Mat4::IDENTITY, 1e-5));
    }

    #[test]
    fn mesh_quad() {
        let mesh = Mesh::quad(100.0, 50.0);
        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.uvs.len(), 4);
        assert_eq!(mesh.indices.len(), 6);
    }

    #[test]
    fn node_default() {
        let node = Node::default();
        assert!(node.name.is_empty());
        assert!(node.enabled);
        assert!(matches!(node.kind, NodeKind::Empty));
    }

    #[test]
    fn node_kind_size() {
        use std::mem::size_of;
        let size = size_of::<NodeKind>();
        assert!(
            size <= 48,
            "NodeKind is {} bytes, should be <= 48. Box large variants.",
            size
        );
    }

    #[test]
    fn mesh_indices_picks_u16_when_max_fits() {
        let indices = MeshIndices::from_usize_iter([0, 1, 2, u16::MAX as usize]);
        assert!(matches!(indices, MeshIndices::U16(_)));
        assert_eq!(indices.len(), 4);
        assert_eq!(indices.as_bytes().len(), 4 * 2);
    }

    #[test]
    fn mesh_indices_upgrades_to_u32_above_u16_max() {
        // >65K vertex mesh: indices that exceed u16::MAX must land in U32
        // so they don't silently wrap (the exact bug MeshIndices was
        // introduced to prevent).
        let over = u16::MAX as usize + 1;
        let indices = MeshIndices::from_usize_iter([0, 1, over]);
        let MeshIndices::U32(_) = &indices else {
            panic!("expected U32 variant, got {:?}", indices);
        };
        assert_eq!(indices.len(), 3);
        assert_eq!(indices.as_bytes().len(), 3 * 4);
        assert_eq!(indices.get(2), Some(over as u32));
        let collected: Vec<u32> = indices.iter_u32().collect();
        assert_eq!(collected, vec![0, 1, over as u32]);
    }

    #[test]
    fn mesh_indices_u32_roundtrips_70k_vertex_mesh() {
        let vertex_count = 70_000usize;
        let indices = MeshIndices::from_usize_iter(0..vertex_count);
        assert!(matches!(indices, MeshIndices::U32(_)));
        assert_eq!(indices.len(), vertex_count);
        assert_eq!(
            indices.get(vertex_count - 1),
            Some((vertex_count - 1) as u32)
        );
        assert_eq!(indices.get(vertex_count), None);
    }
}
