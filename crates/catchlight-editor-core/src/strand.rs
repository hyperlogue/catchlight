//! Fitting a spine to a hair part from its rest mesh. Y-up frame.
//!
//! Rest geometry only: never a deform binding, key pose, posed vertices, or
//! machine-learning crate. This comes from a freedom-to-operate record, which
//! must be revisited before the inputs widen.

use catchlight_core::formats::clm::ClmMesh;
use catchlight_core::{LoadLimitError, LoadResource};

pub const DEFAULT_AXIS: [f32; 2] = [0.0, -1.0];

/// Most links one fit makes; each becomes a spine joint.
pub const MAX_LINKS: u32 = 256;

/// In model pixels; below it the link lengths are noise.
const MIN_EXTENT: f32 = 1e-3;

const MIN_AXIS_LENGTH: f64 = 1e-6;

/// Every field is node-local and in model pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct StrandFit {
    /// Expressed like `mesh.verts`, *not* shifted by the mesh's `origin`: a
    /// node placed at the root sits at `root - origin`.
    pub root: [f32; 2],
    pub axis: [f32; 2],
    pub lengths: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StrandError {
    #[error("the mesh has no vertices")]
    NoVertices,
    #[error("a strand needs at least one link")]
    ZeroLinks,
    #[error("the mesh or the axis holds a non-finite number")]
    NotFinite,
    #[error("the hang axis has no direction")]
    DegenerateAxis,
    #[error("the mesh has no extent along the hang axis")]
    DegenerateExtent,
    #[error(transparent)]
    Limit(#[from] LoadLimitError),
}

/// Fit `links` equal links to a part's rest mesh, measured in a box oriented
/// to `axis`. The root is the centre of the box edge opposite the hang
/// direction.
pub fn fit_strand(
    mesh: &ClmMesh,
    links: u32,
    axis: Option<[f32; 2]>,
) -> Result<StrandFit, StrandError> {
    if links == 0 {
        return Err(StrandError::ZeroLinks);
    }
    if links > MAX_LINKS {
        return Err(LoadLimitError {
            resource: LoadResource::SpineJoints,
            limit: MAX_LINKS.into(),
            got: links.into(),
        }
        .into());
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

const fn finite2(v: [f32; 2]) -> bool {
    v[0].is_finite() && v[1].is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use catchlight_core::formats::clm::ClmIndices;

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
        let mesh = mesh_of(&[[5.0, -5.0], [85.0, -5.0], [85.0, 5.0], [5.0, 5.0]]);
        let fit = fit_strand(&mesh, 2, Some([3.0, 0.0])).unwrap();

        assert!(close(fit.axis[0], 1.0) && close(fit.axis[1], 0.0));
        assert!(close(fit.root[0], 5.0));
        assert!(close(fit.root[1], 0.0));
        let total: f32 = fit.lengths.iter().sum();
        assert!(close(total, 80.0), "lengths sum to the width: {total}");
    }

    #[test]
    fn the_fit_reads_vertices_and_never_the_origin() {
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
        assert_eq!(
            fit_strand(&mesh_of(&[]), 0, None),
            Err(StrandError::ZeroLinks)
        );
    }

    #[test]
    fn more_links_than_the_limit_are_refused() {
        let mesh = mesh_of(&[[-1.0, 5.0], [1.0, -5.0]]);
        assert_eq!(
            fit_strand(&mesh, MAX_LINKS, None).unwrap().lengths.len(),
            256
        );
        assert_eq!(
            fit_strand(&mesh, MAX_LINKS + 1, None),
            Err(StrandError::Limit(LoadLimitError {
                resource: LoadResource::SpineJoints,
                limit: 256,
                got: 257,
            }))
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
