//! Fitting a spine to a hair part.
//!
//! One pure function over one mesh: [`fit_strand`] measures where a strand
//! hangs from and how long each of its links is. Nothing here touches a model,
//! a param, or a binding — the caller writes what comes back.
//!
//! **Rest geometry only.** These functions read a mesh's rest vertices and
//! nothing else: never a deform binding, never a key pose, never posed
//! vertices, and no machine-learning crate participates in the fit. This is a
//! rule rather than a preference — it comes from a freedom-to-operate record —
//! so a future change that wants posed geometry needs that record revisited
//! first, not a wider signature.
//!
//! **The frame is Y-up, so a strand hangs toward -Y.** Model and node space
//! are Y-up: the physics drivers are the exception and they convert at their
//! own boundary, where `Arena::physics_anchor` flips Y going in and
//! `Puppet::write_physics_param_outputs` conjugates the inverse by the same
//! flip coming out (`crates/catchlight-core/src/physics.rs`). So gravity
//! points toward +Y *inside a driver only*; everywhere in this module, down is
//! -Y, which is [`DEFAULT_AXIS`].
//!
//! **A vertex position and a node translation are not the same coordinate.**
//! [`StrandFit::root`] is in the same space as `mesh.verts` — node-local,
//! *not* shifted by the mesh's `origin`. A caller placing a node at the root
//! has to shift: a renderer draws a vertex at `p - origin` in the node's own
//! frame (`crates/catchlight-wgpu/src/renderer.rs`, where the vertex buffer is
//! written), so the root sits at `root - origin` from the part node's
//! translation.
//!
//! **The bounding box is oriented to the axis**, not axis-aligned: extents are
//! measured along `axis` and along its counterclockwise perpendicular. For the
//! default axis the two coincide, and the root is the centre of the mesh's top
//! edge.
//!
//! Deterministic: same mesh and arguments, bit-identical output. No hashing,
//! no iteration order to depend on, no randomness.

use catchlight_core::formats::clm::ClmMesh;

/// The hang direction when none is given: straight down in the Y-up node
/// frame.
pub const DEFAULT_AXIS: [f32; 2] = [0.0, -1.0];

/// Shortest extent along the axis that still describes a strand, in model
/// pixels. Below it the link lengths are noise and the fit is refused.
const MIN_EXTENT: f32 = 1e-3;

/// Shortest vector accepted as a hang direction, before normalisation.
const MIN_AXIS_LENGTH: f64 = 1e-6;

/// Where a particle chain hangs on a part, and how its links divide the art.
///
/// Every field is in the mesh's own node-local space and in model pixels; see
/// the module doc for how `root` relates to a node's translation.
#[derive(Debug, Clone, PartialEq)]
pub struct StrandFit {
    /// Root of the chain in node-local space: the centre of the edge the
    /// strand hangs from. Expressed the way `mesh.verts` are, so it is *not*
    /// shifted by the mesh's `origin`.
    pub root: [f32; 2],
    /// Unit hang direction in node-local space. [`DEFAULT_AXIS`] unless one
    /// was given, in which case it is that vector normalised.
    pub axis: [f32; 2],
    /// Link lengths root to tip, in model pixels. Equal, and summing to the
    /// mesh's extent along `axis` up to float rounding.
    pub lengths: Vec<f32>,
}

/// Why [`fit_strand`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StrandError {
    /// The mesh holds no complete vertex, so there is no shape to hang.
    #[error("the mesh has no vertices")]
    NoVertices,
    /// A chain of no links has no bend to drive.
    #[error("a strand needs at least one link")]
    ZeroLinks,
    /// A vertex or the given axis is not a finite number.
    #[error("the mesh or the axis holds a non-finite number")]
    NotFinite,
    /// The given axis is too short to normalise into a direction.
    #[error("the hang axis has no direction")]
    DegenerateAxis,
    /// The art measures nothing along the axis, so the links would have no
    /// length.
    #[error("the mesh has no extent along the hang axis")]
    DegenerateExtent,
}

/// Fit a particle chain of `links` links to a part's rest mesh.
///
/// The mesh is measured in a box oriented to `axis` (defaulting to
/// [`DEFAULT_AXIS`], and normalised when given). The extent is the box's
/// length along that axis, the links divide it equally, and the root is the
/// centre of the box's edge opposite the hang direction — for the default
/// axis, the centre of the top edge.
///
/// Reads rest vertices only; see the module doc.
pub fn fit_strand(
    mesh: &ClmMesh,
    links: u32,
    axis: Option<[f32; 2]>,
) -> Result<StrandFit, StrandError> {
    if links == 0 {
        return Err(StrandError::ZeroLinks);
    }
    let count = mesh.vertex_count();
    if count == 0 {
        return Err(StrandError::NoVertices);
    }

    let axis = match axis {
        None => DEFAULT_AXIS,
        Some(given) => {
            if !finite2(given) {
                return Err(StrandError::NotFinite);
            }
            // In f64, so squaring a large-but-finite f32 cannot overflow to
            // infinity and turn a real direction into a degenerate one.
            let (x, y) = (f64::from(given[0]), f64::from(given[1]));
            let len = x.mul_add(x, y * y).sqrt();
            if len < MIN_AXIS_LENGTH {
                return Err(StrandError::DegenerateAxis);
            }
            [(x / len) as f32, (y / len) as f32]
        }
    };
    // Counterclockwise perpendicular: the box's lateral direction. Which of
    // the two perpendiculars it is never shows, since only the midpoint
    // between the lateral extremes is used.
    let lateral = [-axis[1], axis[0]];

    let (mut s_min, mut s_max) = (f32::INFINITY, f32::NEG_INFINITY);
    let (mut t_min, mut t_max) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in 0..count {
        let p = [mesh.verts[i * 2], mesh.verts[i * 2 + 1]];
        if !finite2(p) {
            return Err(StrandError::NotFinite);
        }
        let s = dot(p, axis);
        let t = dot(p, lateral);
        s_min = s_min.min(s);
        s_max = s_max.max(s);
        t_min = t_min.min(t);
        t_max = t_max.max(t);
    }

    let extent = s_max - s_min;
    if !extent.is_finite() {
        return Err(StrandError::NotFinite);
    }
    if extent < MIN_EXTENT {
        return Err(StrandError::DegenerateExtent);
    }

    // The near edge of the box along the axis, at the middle of its width.
    let t_mid = 0.5 * (t_min + t_max);
    let root = [
        axis[0] * s_min + lateral[0] * t_mid,
        axis[1] * s_min + lateral[1] * t_mid,
    ];
    if !finite2(root) {
        return Err(StrandError::NotFinite);
    }

    let each = extent / links as f32;
    Ok(StrandFit {
        root,
        axis,
        lengths: vec![each; links as usize],
    })
}

fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

fn finite2(v: [f32; 2]) -> bool {
    v[0].is_finite() && v[1].is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::formats::clm::ClmIndices;

    /// A mesh carrying only the rest positions a fit reads; indices and UVs
    /// play no part here.
    fn mesh_of(points: &[[f32; 2]]) -> ClmMesh {
        ClmMesh {
            verts: points.iter().flat_map(|p| [p[0], p[1]]).collect(),
            uvs: Vec::new(),
            indices: ClmIndices::U16(Vec::new()),
            origin: [0.0, 0.0],
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn a_tall_quad_hangs_along_minus_y() {
        // 20 wide, 100 tall, centred on x = 0, from y = 60 down to y = -40.
        let mesh = mesh_of(&[[-10.0, 60.0], [10.0, 60.0], [10.0, -40.0], [-10.0, -40.0]]);
        let fit = fit_strand(&mesh, 4, None).unwrap();

        assert_eq!(fit.axis, DEFAULT_AXIS);
        // The centre of the top edge, in vertex space.
        assert!(close(fit.root[0], 0.0));
        assert!(close(fit.root[1], 60.0));
        assert_eq!(fit.lengths.len(), 4);
        let total: f32 = fit.lengths.iter().sum();
        assert!(close(total, 100.0), "lengths sum to the height: {total}");
        for &l in &fit.lengths {
            assert!(close(l, 25.0));
        }
    }

    #[test]
    fn a_wide_quad_fits_along_an_explicit_axis() {
        // 80 wide, 10 tall, hanging toward +X. The axis is given unnormalised.
        let mesh = mesh_of(&[[5.0, -5.0], [85.0, -5.0], [85.0, 5.0], [5.0, 5.0]]);
        let fit = fit_strand(&mesh, 2, Some([3.0, 0.0])).unwrap();

        assert!(close(fit.axis[0], 1.0) && close(fit.axis[1], 0.0));
        // The centre of the left edge: the edge opposite the hang direction.
        assert!(close(fit.root[0], 5.0));
        assert!(close(fit.root[1], 0.0));
        let total: f32 = fit.lengths.iter().sum();
        assert!(close(total, 80.0), "lengths sum to the width: {total}");
    }

    #[test]
    fn the_fit_reads_vertices_and_never_the_origin() {
        // `root` is expressed the way vertices are, so moving the mesh origin
        // leaves it where it was; a caller placing a node shifts by
        // `root - origin` itself.
        let mut mesh = mesh_of(&[[-1.0, 5.0], [1.0, 5.0], [1.0, -5.0], [-1.0, -5.0]]);
        let plain = fit_strand(&mesh, 3, None).unwrap();
        mesh.origin = [100.0, -250.0];
        let shifted = fit_strand(&mesh, 3, None).unwrap();
        assert_eq!(plain, shifted);
        assert!(close(plain.root[1], 5.0));
    }

    #[test]
    fn zero_links_and_an_empty_mesh_are_refused() {
        let mesh = mesh_of(&[[-1.0, 5.0], [1.0, -5.0]]);
        assert_eq!(fit_strand(&mesh, 0, None), Err(StrandError::ZeroLinks));
        assert_eq!(
            fit_strand(&mesh_of(&[]), 4, None),
            Err(StrandError::NoVertices)
        );
        // ZeroLinks is checked before the mesh: an empty chain is refused
        // whatever it was going to be fitted to.
        assert_eq!(
            fit_strand(&mesh_of(&[]), 0, None),
            Err(StrandError::ZeroLinks)
        );
    }

    #[test]
    fn a_flat_mesh_and_a_bad_number_are_refused() {
        // No extent along -Y: a horizontal line.
        let flat = mesh_of(&[[-10.0, 0.0], [10.0, 0.0]]);
        assert_eq!(
            fit_strand(&flat, 2, None),
            Err(StrandError::DegenerateExtent)
        );
        // ... but it fits along +X.
        assert!(fit_strand(&flat, 2, Some([1.0, 0.0])).is_ok());

        let bad = mesh_of(&[[0.0, 10.0], [f32::NAN, -10.0]]);
        assert_eq!(fit_strand(&bad, 2, None), Err(StrandError::NotFinite));

        let good = mesh_of(&[[0.0, 10.0], [0.0, -10.0]]);
        assert_eq!(
            fit_strand(&good, 2, Some([0.0, 0.0])),
            Err(StrandError::DegenerateAxis)
        );
        assert_eq!(
            fit_strand(&good, 2, Some([f32::INFINITY, 0.0])),
            Err(StrandError::NotFinite)
        );
    }
}
