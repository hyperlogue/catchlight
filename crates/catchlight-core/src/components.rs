use glam::{EulerRot, Mat4, Quat, Vec2, Vec3};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIdx(pub u32);

impl NodeIdx {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

/// A part's albedo, as a position in [`Model::texture_ids`](crate::Model::texture_ids).
/// `u32::MAX` means the part draws no texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureIdx(pub u32);

impl TextureIdx {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

/// Premultiplied LINEAR colour stored as `Rgba8UnormSrgb` bytes. Premultiplied
/// storage makes filtering correct at an alpha edge without a bleed pass.
#[derive(Debug, Clone)]
pub struct DecodedTexture {
    pub width: u32,
    pub height: u32,
    /// Tightly packed (`4 * width` bytes per row).
    pub rgba: Arc<[u8]>,
}

impl DecodedTexture {
    /// Half-resolution copy via the same linear-space box filter renderers use
    /// for mip generation, so it is texel-identical to mip 1.
    pub fn halved(&self) -> Self {
        let (width, height, rgba) = downsample_box_filter(&self.rgba, self.width, self.height);
        Self {
            width,
            height,
            rgba: rgba.into(),
        }
    }
}

/// An empty image comes back unchanged.
pub fn downsample_box_filter(src: &[u8], w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    if w == 0 || h == 0 {
        return (w, h, src.to_vec());
    }
    // Average in LINEAR space: averaging gamma-encoded bytes biases minified
    // texels dark. Alpha is stored linear, so it averages directly.
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
                    a += f32::from(src[i + 3]);
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

fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

static SRGB_DECODE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();

/// Hoist this out of per-texel loops: the `OnceLock` probe costs several
/// times the array read.
pub(crate) fn srgb_decode_table() -> &'static [f32; 256] {
    SRGB_DECODE.get_or_init(|| std::array::from_fn(|b| srgb_to_linear(b as f32 / 255.0)))
}

/// Not table-driven: a search over 255 step points measured no faster than
/// `powf`.
#[inline]
pub(crate) fn srgb_encode_to_byte(linear: f32) -> u8 {
    let s = if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// `base_*` fields are authored and never mutated; `transform` / `z_order`
/// are per-frame working copies reset to base each tick.
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
            kind: NodeKind::Group,
        }
    }
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    MotionResponse,
    Group,
    Part(Box<PartData>),
    Composite(Box<CompositeData>),
    MeshGroup(Box<MeshGroupData>),
    Spine(Box<crate::spine::SpineData>),
}

const _: () = assert!(
    size_of::<NodeKind>() <= 48,
    "NodeKind is too large - consider boxing new variants"
);

#[derive(Debug, Clone)]
pub struct PartData {
    pub mesh: Arc<Mesh>,
    pub albedo_texture: TextureIdx,
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
            mesh: Arc::default(),
            albedo_texture: TextureIdx(0),
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

/// A mesh group is never drawn, so it carries no colour.
#[derive(Debug, Clone)]
pub struct MeshGroupData {
    pub mesh: Arc<Mesh>,
    pub translate_children: bool,
    pub deform_stack: crate::deform::DeformStack,
    pub(crate) pins: Arc<crate::meshgroup::MeshGroupPins>,
    /// `None` when the cache cannot be built within its limits; propagation then
    /// falls back to the linear hinted scan.
    pub(crate) bins: Option<Arc<crate::meshgroup::MgTriangleBins>>,
}

impl Default for MeshGroupData {
    fn default() -> Self {
        Self {
            mesh: Arc::default(),
            translate_children: true,
            deform_stack: crate::deform::DeformStack::default(),
            pins: Arc::default(),
            bins: None,
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

impl From<&crate::formats::clm::ClmTransform> for Transform {
    fn from(t: &crate::formats::clm::ClmTransform) -> Self {
        Self {
            translation: Vec3::from(t.translation),
            rotation: Vec3::from(t.rotation),
            scale: Vec2::new(t.scale[0], t.scale[1]),
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
    // Smaller determinants amplify coordinates into unstable values even when
    // glam produces a finite inverse.
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
            Self::U16(v) => v.len(),
            Self::U32(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter_u32(&self) -> MeshIndicesIter<'_> {
        match self {
            Self::U16(v) => MeshIndicesIter::U16(v.iter()),
            Self::U32(v) => MeshIndicesIter::U32(v.iter()),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::U16(v) => bytemuck::cast_slice(v),
            Self::U32(v) => bytemuck::cast_slice(v),
        }
    }

    pub fn get(&self, i: usize) -> Option<u32> {
        match self {
            Self::U16(v) => v.get(i).map(|&n| u32::from(n)),
            Self::U32(v) => v.get(i).copied(),
        }
    }

    /// `U16` unless an index needs more.
    pub fn from_usize_iter(
        iter: impl IntoIterator<Item = usize>,
    ) -> Result<Self, MeshIndexOutOfRange> {
        let iter = iter.into_iter();
        let mut out = Self::U16(Vec::with_capacity(iter.size_hint().0));
        for index in iter {
            if let Self::U16(v) = &mut out {
                if let Ok(index) = u16::try_from(index) {
                    v.push(index);
                    continue;
                }
                out = Self::U32(v.iter().map(|&i| u32::from(i)).collect());
            }
            if let Self::U32(v) = &mut out {
                v.push(u32::try_from(index).map_err(|_| MeshIndexOutOfRange(index))?);
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("mesh index {0} does not fit in 32 bits")]
pub struct MeshIndexOutOfRange(pub usize);

/// [`MeshIndices::iter_u32`].
#[derive(Debug, Clone)]
pub enum MeshIndicesIter<'a> {
    U16(std::slice::Iter<'a, u16>),
    U32(std::slice::Iter<'a, u32>),
}

impl Iterator for MeshIndicesIter<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<u32> {
        match self {
            Self::U16(it) => it.next().map(|&i| u32::from(i)),
            Self::U32(it) => it.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::U16(it) => it.size_hint(),
            Self::U32(it) => it.size_hint(),
        }
    }
}

impl ExactSizeIterator for MeshIndicesIter<'_> {}

impl Default for MeshIndices {
    fn default() -> Self {
        Self::U16(Vec::new())
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
    /// Every blend mode, in declaration order.
    pub const ALL: &[Self] = &[
        Self::Normal,
        Self::Multiply,
        Self::ColorDodge,
        Self::LinearDodge,
        Self::Screen,
        Self::ClipToLower,
        Self::SliceFromLower,
        Self::Overlay,
        Self::ColorBurn,
        Self::LinearBurn,
        Self::Darken,
        Self::Lighten,
        Self::Add,
        Self::Inverse,
        Self::Subtract,
    ];

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "Normal" => Self::Normal,
            "Multiply" => Self::Multiply,
            "ColorDodge" => Self::ColorDodge,
            "LinearDodge" => Self::LinearDodge,
            "Screen" => Self::Screen,
            "ClipToLower" => Self::ClipToLower,
            "SliceFromLower" => Self::SliceFromLower,
            "Overlay" => Self::Overlay,
            "ColorBurn" => Self::ColorBurn,
            "LinearBurn" => Self::LinearBurn,
            "Darken" => Self::Darken,
            "Lighten" => Self::Lighten,
            "Add" => Self::Add,
            "Inverse" => Self::Inverse,
            "Subtract" => Self::Subtract,
            _ => return None,
        })
    }
}

impl BlendMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::ColorDodge => "ColorDodge",
            Self::LinearDodge => "LinearDodge",
            Self::Screen => "Screen",
            Self::ClipToLower => "ClipToLower",
            Self::SliceFromLower => "SliceFromLower",
            Self::Overlay => "Overlay",
            Self::ColorBurn => "ColorBurn",
            Self::LinearBurn => "LinearBurn",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::Add => "Add",
            Self::Inverse => "Inverse",
            Self::Subtract => "Subtract",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown blend mode {0:?}")]
pub struct UnknownBlendMode(pub String);

impl std::str::FromStr for BlendMode {
    type Err = UnknownBlendMode;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_name(s).ok_or_else(|| UnknownBlendMode(s.to_string()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MaskMode {
    Mask,
    DodgeMask,
}

impl MaskMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Mask => "mask",
            Self::DodgeMask => "dodge",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mask {
    pub source: NodeIdx,
    pub mode: MaskMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_leaves_an_empty_image_empty() {
        for (w, h) in [(0, 4), (4, 0), (0, 0)] {
            assert_eq!(downsample_box_filter(&[], w, h), (w, h, Vec::new()));
        }
    }

    #[test]
    fn downsample_uniform_4x4_to_2x2_preserves_color() {
        let src = vec![255u8; 4 * 4 * 4];
        let (dw, dh, out) = downsample_box_filter(&src, 4, 4);
        assert_eq!((dw, dh), (2, 2));
        assert!(out.iter().all(|&b| b == 255));
    }

    #[test]
    fn downsample_2x2_to_1x1_averages_four_samples() {
        // One lit texel of four -> linear mean 0.25 -> sRGB byte ~137.
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
        assert!(matches!(node.kind, NodeKind::Group));
    }

    #[test]
    fn node_kind_size() {
        use std::mem::size_of;
        let size = size_of::<NodeKind>();
        assert!(
            size <= 48,
            "NodeKind is {size} bytes, should be <= 48. Box large variants."
        );
    }

    #[test]
    fn mesh_indices_picks_u16_when_max_fits() {
        let indices = MeshIndices::from_usize_iter([0, 1, 2, u16::MAX as usize]).unwrap();
        assert!(matches!(indices, MeshIndices::U16(_)));
        assert_eq!(indices.len(), 4);
        assert_eq!(indices.as_bytes().len(), 4 * 2);
    }

    #[test]
    fn mesh_indices_upgrades_to_u32_above_u16_max() {
        let over = u16::MAX as usize + 1;
        let indices = MeshIndices::from_usize_iter([0, 1, over]).unwrap();
        let MeshIndices::U32(_) = &indices else {
            panic!("expected U32 variant, got {indices:?}");
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
        let indices = MeshIndices::from_usize_iter(0..vertex_count).unwrap();
        assert!(matches!(indices, MeshIndices::U32(_)));
        assert_eq!(indices.len(), vertex_count);
        assert_eq!(
            indices.get(vertex_count - 1),
            Some((vertex_count - 1) as u32)
        );
        assert_eq!(indices.get(vertex_count), None);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn mesh_indices_refuse_an_index_past_u32() {
        let over = u32::MAX as usize + 1;
        assert_eq!(
            MeshIndices::from_usize_iter([0, over]).unwrap_err(),
            MeshIndexOutOfRange(over)
        );
    }
}
