//! Fitting a particle chain to a hair part, and the deform keyforms its link
//! params drive.
//!
//! Two pure functions over one mesh: [`fit_strand`] measures where a strand
//! hangs from and how long each of its links is, and [`chain_keyforms`] builds
//! the per-vertex deform one link's bend param authors at one bend value.
//! Nothing here touches a model, a param, or a binding — the caller writes
//! what comes back.
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
//! **A positive bend swings the tip toward +X.** A chain link's param is the
//! bend at the joint above it, relative to the link above (link 0 relative to
//! straight down), in half turns. The angle is `bend * pi` applied
//! counterclockwise in the Y-up frame, which takes a vertex hanging straight
//! below its joint toward +X: at bend 0.5, a vertex a distance `d` below the
//! joint lands at `joint + (d, 0)`. [`chain_keyforms`] is the one place that
//! sign is written down on this side of the wire.
//!
//! **A vertex position and a node translation are not the same coordinate.**
//! [`StrandFit::root`] is in the same space as `mesh.verts` — node-local,
//! *not* shifted by the mesh's `origin`. That is what [`chain_keyforms`] needs,
//! since it measures vertices against the root. A caller placing a chain node
//! at the root has to shift: a renderer draws a vertex at `p - origin` in the
//! node's own frame (`crates/catchlight-wgpu/src/renderer.rs`, where the
//! vertex buffer is written), so the root sits at `root - origin` from the
//! part node's translation, and a sibling node reaches it at
//! `part.translation + (root - origin)`.
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

/// The bend values a link param's deform binding is keyed at, in half turns:
/// a quarter turn each way, in thirds. Ordered ascending, as a binding's grid
/// wants them.
pub const BEND_KEYS: [f32; 7] = [-0.5, -1.0 / 3.0, -1.0 / 6.0, 0.0, 1.0 / 6.0, 1.0 / 3.0, 0.5];

/// The range a link's bend param spans, `[min, max]` — the ends of
/// [`BEND_KEYS`], a half turn in total.
pub const BEND_RANGE: [f32; 2] = [-0.5, 0.5];

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

/// The deform one link's bend param authors at one bend value: a flat
/// `[dx, dy, …]`, one pair per vertex in the mesh's own order.
///
/// `bend` is in half turns, so the joint turns by `bend * pi`
/// counterclockwise in the Y-up frame — positive swings the tip toward +X.
/// The joint of link `link` sits `sum(lengths[..link])` down the axis from
/// [`StrandFit::root`]. A vertex at or above that joint does not move; below
/// it, the rotation about the joint is weighted by how far into this link the
/// vertex falls, `clamp((s - s_joint) / lengths[link], 0, 1)`, so the crease
/// ramps across the link rather than hinging at a line. Only the distance
/// along the axis enters that weight, so a band across the art moves as one.
///
/// Total: a `link` past the end of the chain, a non-finite `bend`, a
/// degenerate fit or a non-finite vertex all yield zeros rather than a
/// refusal, and `bend` of zero yields zeros exactly.
///
/// Reads rest vertices only; see the module doc.
#[must_use]
pub fn chain_keyforms(mesh: &ClmMesh, fit: &StrandFit, link: usize, bend: f32) -> Vec<f32> {
    let count = mesh.vertex_count();
    let mut out = vec![0.0f32; count * 2];

    let Some(&span) = fit.lengths.get(link) else {
        return out;
    };
    if bend == 0.0 || !bend.is_finite() || !span.is_finite() || span <= 0.0 {
        return out;
    }
    if !finite2(fit.root) || !finite2(fit.axis) {
        return out;
    }

    let s_joint: f32 = fit.lengths[..link].iter().sum();
    if !s_joint.is_finite() {
        return out;
    }
    let joint = [
        fit.root[0] + fit.axis[0] * s_joint,
        fit.root[1] + fit.axis[1] * s_joint,
    ];

    let (sin, cos) = (bend * std::f32::consts::PI).sin_cos();
    for i in 0..count {
        let p = [mesh.verts[i * 2], mesh.verts[i * 2 + 1]];
        if !finite2(p) {
            continue;
        }
        // Arc-length coordinate measured from the root, so the joint sits at
        // `s_joint` and the strand's tip at the sum of every length.
        let s = dot([p[0] - fit.root[0], p[1] - fit.root[1]], fit.axis);
        // A NaN fails both comparisons the same way and is skipped here.
        if !s.is_finite() || s <= s_joint {
            continue;
        }
        let w = ((s - s_joint) / span).clamp(0.0, 1.0);

        let d = [p[0] - joint[0], p[1] - joint[1]];
        // Counterclockwise in the Y-up frame: at +pi/2 this sends (0, -d) to
        // (d, 0), a tip hanging straight down swung toward +X.
        let r = [d[0] * cos - d[1] * sin, d[0] * sin + d[1] * cos];
        out[i * 2] = w * (r[0] - d[0]);
        out[i * 2 + 1] = w * (r[1] - d[1]);
    }
    out
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

    /// A tall strip: 2 columns wide, `rows` rows from `top` down to `bottom`.
    fn strip(x: f32, top: f32, bottom: f32, rows: usize) -> ClmMesh {
        let mut points = Vec::new();
        for r in 0..rows {
            let t = r as f32 / (rows - 1) as f32;
            let y = top + (bottom - top) * t;
            points.push([-x, y]);
            points.push([x, y]);
        }
        mesh_of(&points)
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

    #[test]
    fn a_quarter_turn_swings_a_hanging_vertex_toward_plus_x() {
        // One vertex a distance 1 straight below the joint, at full weight.
        let fit = StrandFit {
            root: [0.0, 0.0],
            axis: DEFAULT_AXIS,
            lengths: vec![1.0],
        };
        let mesh = mesh_of(&[[0.0, -1.0]]);
        let k = chain_keyforms(&mesh, &fit, 0, 0.5);
        // It lands at joint + (1, 0), so the deform off (0, -1) is (1, 1).
        assert!(close(k[0], 1.0), "dx {}", k[0]);
        assert!(close(k[1], 1.0), "dy {}", k[1]);

        // The same at the second joint of a two-link chain, whose joint sits
        // at (0, -2).
        let fit = StrandFit {
            root: [0.0, 0.0],
            axis: DEFAULT_AXIS,
            lengths: vec![2.0, 2.0],
        };
        let mesh = mesh_of(&[[0.0, -4.0]]);
        let k = chain_keyforms(&mesh, &fit, 1, 0.5);
        // Lands at (0, -2) + (2, 0) = (2, -2); the deform off (0, -4) is
        // (2, 2).
        assert!(close(k[0], 2.0), "dx {}", k[0]);
        assert!(close(k[1], 2.0), "dy {}", k[1]);

        // And a negative bend swings the other way.
        let k = chain_keyforms(&mesh, &fit, 1, -0.5);
        assert!(close(k[0], -2.0), "dx {}", k[0]);
        assert!(close(k[1], 2.0), "dy {}", k[1]);
    }

    #[test]
    fn a_vertex_at_or_above_the_root_never_moves() {
        // The fit comes off a strip whose top edge, and so whose root, is at
        // y = 50.
        let fit = fit_strand(&strip(8.0, 50.0, -50.0, 9), 3, None).unwrap();
        assert!(close(fit.root[1], 50.0));
        // A row well above the root, the root's own row, and one at the very
        // bottom that every link does move.
        let mesh = mesh_of(&[
            [-8.0, 90.0],
            [8.0, 90.0],
            [-8.0, 50.0],
            [8.0, 50.0],
            [-8.0, -50.0],
            [8.0, -50.0],
        ]);
        for link in 0..fit.lengths.len() {
            for &bend in &BEND_KEYS {
                let k = chain_keyforms(&mesh, &fit, link, bend);
                for i in 0..4 {
                    assert_eq!(
                        (k[i * 2], k[i * 2 + 1]),
                        (0.0, 0.0),
                        "vertex {i} moved on link {link} at bend {bend}"
                    );
                }
                // Not vacuous: the bottom row does move, except at bend 0.
                let moved = k[8] != 0.0 || k[9] != 0.0;
                assert_eq!(moved, bend != 0.0, "the tip on link {link} at bend {bend}");
            }
        }
    }

    #[test]
    fn the_tip_moves_most() {
        let mesh = strip(8.0, 50.0, -50.0, 11);
        let fit = fit_strand(&mesh, 1, None).unwrap();
        let k = chain_keyforms(&mesh, &fit, 0, 0.25);
        let mag = |i: usize| k[i * 2].hypot(k[i * 2 + 1]);
        // Row by row down the strip, each pair further than the last, with
        // the top row pinned at zero.
        assert_eq!(mag(0), 0.0);
        for row in 1..11 {
            let (prev, here) = (mag((row - 1) * 2), mag(row * 2));
            assert!(here > prev, "row {row}: {here} not past {prev}");
        }
    }

    #[test]
    fn lateral_position_never_changes_the_weight() {
        // Two vertices at the same height, far apart across the strand: the
        // band moves as one, so their deforms differ only by the lever arm
        // the rotation gives them, never by weight.
        let fit = StrandFit {
            root: [0.0, 0.0],
            axis: DEFAULT_AXIS,
            lengths: vec![10.0],
        };
        let narrow = mesh_of(&[[0.0, -3.0]]);
        let wide = mesh_of(&[[40.0, -3.0]]);
        let a = chain_keyforms(&narrow, &fit, 0, 0.2);
        let b = chain_keyforms(&wide, &fit, 0, 0.2);
        // Same weight (0.3) and same angle, so `b`'s deform is `a`'s plus what
        // the rotation does to the extra 40 of lateral offset.
        let (sin, cos) = (0.2 * std::f32::consts::PI).sin_cos();
        let w = 0.3f32;
        assert!(close(b[0] - a[0], w * (40.0 * cos - 40.0)));
        assert!(close(b[1] - a[1], w * (40.0 * sin)));
    }

    #[test]
    fn bend_zero_is_exactly_zero() {
        let mesh = strip(8.0, 50.0, -50.0, 7);
        let fit = fit_strand(&mesh, 3, None).unwrap();
        for link in 0..3 {
            let k = chain_keyforms(&mesh, &fit, link, 0.0);
            assert_eq!(k.len(), mesh.verts.len());
            assert!(k.iter().all(|&v| v == 0.0), "link {link} at bend 0 moved");
        }
    }

    #[test]
    fn mirrored_bends_mirror_in_x() {
        // Symmetric about x = 0, which is where the root lands.
        let mesh = strip(12.0, 40.0, -40.0, 6);
        let fit = fit_strand(&mesh, 2, None).unwrap();
        assert!(close(fit.root[0], 0.0));

        for link in 0..2 {
            for &bend in &[1.0 / 6.0, 1.0 / 3.0, 0.5] {
                let plus = chain_keyforms(&mesh, &fit, link, bend);
                let minus = chain_keyforms(&mesh, &fit, link, -bend);
                // Row r holds vertices 2r (x = -12) and 2r+1 (x = +12); the
                // pair swaps under the mirror.
                for row in 0..6 {
                    let (l, r) = (row * 2, row * 2 + 1);
                    assert!(
                        close(plus[l * 2], -minus[r * 2]),
                        "link {link} bend {bend} row {row} dx"
                    );
                    assert!(
                        close(plus[l * 2 + 1], minus[r * 2 + 1]),
                        "link {link} bend {bend} row {row} dy"
                    );
                }
            }
        }
    }

    #[test]
    fn a_link_past_the_end_of_the_chain_is_all_zeros() {
        let mesh = strip(8.0, 50.0, -50.0, 5);
        let fit = fit_strand(&mesh, 2, None).unwrap();
        let k = chain_keyforms(&mesh, &fit, 2, 0.5);
        assert_eq!(k.len(), mesh.verts.len());
        assert!(k.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn the_same_input_twice_is_byte_identical() {
        let mesh = strip(11.0, 63.5, -27.25, 13);
        let fit = fit_strand(&mesh, 5, Some([0.3, -1.7])).unwrap();
        let again = fit_strand(&mesh, 5, Some([0.3, -1.7])).unwrap();
        assert_eq!(fit.root[0].to_bits(), again.root[0].to_bits());
        assert_eq!(fit.root[1].to_bits(), again.root[1].to_bits());
        assert_eq!(fit.axis[0].to_bits(), again.axis[0].to_bits());
        assert_eq!(fit.axis[1].to_bits(), again.axis[1].to_bits());

        for link in 0..fit.lengths.len() {
            for &bend in &BEND_KEYS {
                let a = chain_keyforms(&mesh, &fit, link, bend);
                let b = chain_keyforms(&mesh, &fit, link, bend);
                let bits = |v: &[f32]| v.iter().map(|f| f.to_bits()).collect::<Vec<u32>>();
                assert_eq!(bits(&a), bits(&b), "link {link} at bend {bend}");
            }
        }
    }

    #[test]
    fn the_bend_keys_span_the_bend_range() {
        assert_eq!(BEND_KEYS[0], BEND_RANGE[0]);
        assert_eq!(BEND_KEYS[BEND_KEYS.len() - 1], BEND_RANGE[1]);
        assert!(BEND_KEYS.windows(2).all(|w| w[0] < w[1]));
        assert!(BEND_KEYS.contains(&0.0));
    }
}
