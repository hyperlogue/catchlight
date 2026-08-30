//! Mesh editing over a constrained-Delaunay model: a [`WorkingMesh`] is
//! vertices + a non-crossing set of constraint edges, and the triangulation is
//! always *derived* (spade CDT). The mesh is valid by construction — the
//! Connect tool pins/unpins constraints, ops that would make constraints cross
//! are rejected up front, and Apply can never fail. Concave shapes and holes
//! come out of alpha culling: triangles covering only fully-transparent texels
//! are dropped, so the convex-hull fill never bridges disjoint shapes.
//!
//! Runtime and `.clm` never see any of this — they keep plain indexed triangle
//! lists; [`WorkingMesh::to_mesh`] flattens on Apply, and
//! [`Model::set_mesh_with_refit`] re-fits existing deform bindings onto the
//! new topology (triangle-affine interpolation over the old rest mesh).

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation as _};

use catchlight_core::id::{NodeId, SeamId, SlotId};
use catchlight_core::{Model, ModelError};

/// Minimum distance between distinct vertices; closer placements are rejected
/// (coincident points would merge in the triangulation and corrupt indexing).
const MIN_VERTEX_DISTANCE: f32 = 1e-3;

/// Why a [`WorkingMesh`] op was refused. These are the mesh tool's own
/// rejections, not a [`ModelError`]: a working mesh is not in a model yet, and
/// the tool's whole promise is that a state it accepted can always be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MeshError {
    /// The index names no vertex of this mesh.
    #[error("no vertex at that index")]
    NoSuchVertex,
    /// The position is not finite, or sits on top of another vertex —
    /// coincident points merge in the triangulation and corrupt indexing.
    #[error("a vertex cannot land on another vertex")]
    Coincident,
    /// A constraint edge would cross one already pinned. Touching at a shared
    /// endpoint is fine; crossing is what the triangulation cannot represent.
    #[error("constraint edges may not cross")]
    ConstraintCross,
    /// The texture has no pixel above the alpha threshold, so there is no
    /// shape to mesh.
    #[error("the texture is empty above the alpha threshold")]
    NothingToMesh,
    /// The triangulator refused the point set.
    #[error("triangulation failed")]
    Triangulation,
}

#[derive(Debug, Clone, Default)]
pub struct WorkingMesh {
    /// Flat `[x, y, …]` rest positions in node-local space.
    pub verts: Vec<f32>,
    /// Pinned edges (vertex index pairs, unordered). Always non-crossing.
    pub constraints: Vec<(u32, u32)>,
    pub origin: [f32; 2],
}

impl WorkingMesh {
    /// Start from an existing mesh, seeding **every current triangle edge as a
    /// constraint** so the CDT preserves the current topology exactly until
    /// the user unpins.
    pub fn from_mesh(mesh: &ClmMesh) -> Self {
        let vcount = (mesh.verts.len() / 2) as u32;
        let mut constraints: Vec<(u32, u32)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Out-of-range indices (a malformed file) are dropped rather than
        // seeded — triangulate() can only pin edges between real vertices.
        let mut push_edge = |a: u32, b: u32| {
            let key = (a.min(b), a.max(b));
            if a != b && a < vcount && b < vcount && seen.insert(key) {
                constraints.push(key);
            }
        };
        let mut tris = |ix: &mut dyn Iterator<Item = u32>| {
            let v: Vec<u32> = ix.collect();
            for t in v.as_chunks::<3>().0 {
                push_edge(t[0], t[1]);
                push_edge(t[1], t[2]);
                push_edge(t[2], t[0]);
            }
        };
        match &mesh.indices {
            ClmIndices::U16(v) => tris(&mut v.iter().map(|&i| i as u32)),
            ClmIndices::U32(v) => tris(&mut v.iter().copied()),
        }
        Self {
            verts: mesh.verts.clone(),
            constraints,
            origin: mesh.origin,
        }
    }

    pub fn vertex_count(&self) -> usize {
        self.verts.len() / 2
    }

    pub fn pos(&self, i: u32) -> [f32; 2] {
        [self.verts[i as usize * 2], self.verts[i as usize * 2 + 1]]
    }

    /// Add a vertex; rejected when it lands on an existing vertex.
    pub fn add_vertex(&mut self, pos: [f32; 2]) -> Result<u32, MeshError> {
        if !pos[0].is_finite() || !pos[1].is_finite() {
            return Err(MeshError::Coincident);
        }
        for i in 0..self.vertex_count() {
            let p = self.pos(i as u32);
            if dist2(p, pos) < MIN_VERTEX_DISTANCE * MIN_VERTEX_DISTANCE {
                return Err(MeshError::Coincident);
            }
        }
        self.verts.extend_from_slice(&pos);
        Ok(self.vertex_count() as u32 - 1)
    }

    /// Move a vertex; rejected when the move would make constraint edges cross
    /// or stack the vertex onto another.
    pub fn move_vertex(&mut self, i: u32, pos: [f32; 2]) -> Result<(), MeshError> {
        if i as usize >= self.vertex_count() {
            return Err(MeshError::NoSuchVertex);
        }
        if !pos[0].is_finite() || !pos[1].is_finite() {
            return Err(MeshError::Coincident);
        }
        for j in 0..self.vertex_count() as u32 {
            if j != i && dist2(self.pos(j), pos) < MIN_VERTEX_DISTANCE * MIN_VERTEX_DISTANCE {
                return Err(MeshError::Coincident);
            }
        }
        // Every constraint incident to `i` (at its new position) must stay
        // clear of every constraint that doesn't share an endpoint with it.
        for &(a, b) in self.constraints.iter().filter(|&&(a, b)| a == i || b == i) {
            let other = if a == i { b } else { a };
            let seg = (pos, self.pos(other));
            for &(c, d) in &self.constraints {
                if c == i || d == i || c == other || d == other {
                    continue;
                }
                if segments_cross(seg.0, seg.1, self.pos(c), self.pos(d)) {
                    return Err(MeshError::ConstraintCross);
                }
            }
        }
        self.verts[i as usize * 2] = pos[0];
        self.verts[i as usize * 2 + 1] = pos[1];
        Ok(())
    }

    /// Delete vertices; constraints touching them are dropped and the rest of
    /// the indices remapped.
    pub fn delete_vertices(&mut self, remove: &[u32]) {
        let set: std::collections::HashSet<u32> = remove.iter().copied().collect();
        let count = self.vertex_count() as u32;
        let mut remap = vec![u32::MAX; count as usize];
        let mut next = 0u32;
        let mut verts = Vec::with_capacity(self.verts.len());
        for i in 0..count {
            if !set.contains(&i) {
                remap[i as usize] = next;
                next += 1;
                let p = self.pos(i);
                verts.extend_from_slice(&p);
            }
        }
        self.verts = verts;
        self.constraints
            .retain(|&(a, b)| remap[a as usize] != u32::MAX && remap[b as usize] != u32::MAX);
        for c in &mut self.constraints {
            *c = (remap[c.0 as usize], remap[c.1 as usize]);
        }
    }

    /// Pin an edge as a constraint. Rejected when it would cross an existing
    /// constraint (touching at a shared endpoint is fine).
    pub fn add_constraint(&mut self, a: u32, b: u32) -> Result<(), MeshError> {
        let n = self.vertex_count() as u32;
        if a >= n || b >= n {
            return Err(MeshError::NoSuchVertex);
        }
        if a == b {
            return Err(MeshError::ConstraintCross);
        }
        let key = (a.min(b), a.max(b));
        if self.constraints.contains(&key) {
            return Ok(());
        }
        for &(c, d) in &self.constraints {
            if c == a || c == b || d == a || d == b {
                continue;
            }
            if segments_cross(self.pos(a), self.pos(b), self.pos(c), self.pos(d)) {
                return Err(MeshError::ConstraintCross);
            }
        }
        self.constraints.push(key);
        Ok(())
    }

    pub fn remove_constraint(&mut self, a: u32, b: u32) {
        let key = (a.min(b), a.max(b));
        self.constraints.retain(|&c| c != key);
    }

    pub fn has_constraint(&self, a: u32, b: u32) -> bool {
        let key = (a.min(b), a.max(b));
        self.constraints.contains(&key)
    }

    /// Derived triangulation (the live "triangulate preview"). Stored state is
    /// always triangulable — the ops above refuse to persist a crossing set.
    pub fn triangulate(&self) -> Result<Vec<[u32; 3]>, MeshError> {
        let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
            ConstrainedDelaunayTriangulation::new();
        let mut handles = Vec::with_capacity(self.vertex_count());
        for i in 0..self.vertex_count() {
            let p = self.pos(i as u32);
            let h = cdt
                .insert(Point2::new(p[0] as f64, p[1] as f64))
                .map_err(|_| MeshError::Triangulation)?;
            handles.push(h);
        }
        // Reverse map: spade vertex index -> our vertex index. Coincident
        // points can't occur (add/move guard), so this is a bijection.
        let mut rev = vec![u32::MAX; handles.len() + 1];
        for (i, h) in handles.iter().enumerate() {
            let idx = h.index();
            if idx >= rev.len() {
                rev.resize(idx + 1, u32::MAX);
            }
            rev[idx] = i as u32;
        }
        for &(a, b) in &self.constraints {
            let (Some(&ha), Some(&hb)) = (handles.get(a as usize), handles.get(b as usize)) else {
                continue;
            };
            if cdt.can_add_constraint(ha, hb) {
                cdt.add_constraint(ha, hb);
            }
        }
        let mut tris = Vec::with_capacity(cdt.num_inner_faces());
        for face in cdt.inner_faces() {
            let [a, b, c] = face.vertices();
            let (a, b, c) = (
                rev[a.fix().index()],
                rev[b.fix().index()],
                rev[c.fix().index()],
            );
            if a != u32::MAX && b != u32::MAX && c != u32::MAX {
                tris.push([a, b, c]);
            }
        }
        Ok(tris)
    }

    /// Flatten to the plain indexed triangle list runtime/`.clm` consume.
    /// UVs are re-derived from texture space; triangles covering only
    /// transparent texels are culled when an alpha mask is given.
    pub fn to_mesh(&self, uv_map: &UvMap, alpha: Option<&AlphaMask>) -> Result<ClmMesh, MeshError> {
        let tris = self.triangulate()?;
        let uvs: Vec<f32> = (0..self.vertex_count())
            .flat_map(|i| uv_map.uv(self.pos(i as u32)))
            .collect();
        let kept: Vec<[u32; 3]> = match alpha {
            Some(mask) => tris
                .into_iter()
                .filter(|t| {
                    let uv = |i: u32| [uvs[i as usize * 2], uvs[i as usize * 2 + 1]];
                    mask.triangle_covers_opaque(uv(t[0]), uv(t[1]), uv(t[2]))
                })
                .collect(),
            None => tris,
        };
        let flat: Vec<u32> = kept.into_iter().flatten().collect();
        let indices = if self.vertex_count() <= u16::MAX as usize {
            ClmIndices::U16(flat.iter().map(|&i| i as u16).collect())
        } else {
            ClmIndices::U32(flat)
        };
        Ok(ClmMesh {
            verts: self.verts.clone(),
            uvs,
            indices,
            origin: self.origin,
        })
    }
}

/// Axis-aligned local-space → UV mapping (`u = sx·x + ox`, `v = sy·y + oy`).
/// Catchlight meshes always map the texture axis-aligned, so a per-axis
/// least-squares fit over the existing vertex↔UV pairs recovers the mapping;
/// fresh meshes use the centered-texture convention.
#[derive(Debug, Clone, Copy)]
pub struct UvMap {
    pub sx: f32,
    pub ox: f32,
    pub sy: f32,
    pub oy: f32,
}

impl UvMap {
    /// The quad/grid convention: texture of `w`×`h` texels centered on the
    /// local origin, `v` increasing downward.
    pub fn from_texture_size(w: f32, h: f32) -> Self {
        Self {
            sx: 1.0 / w.max(1.0),
            ox: 0.5,
            sy: -1.0 / h.max(1.0),
            oy: 0.5,
        }
    }

    /// Per-axis linear regression over existing pairs; `None` when an axis is
    /// degenerate (all verts on a line).
    pub fn fit(verts: &[f32], uvs: &[f32]) -> Option<Self> {
        let n = (verts.len() / 2).min(uvs.len() / 2);
        if n < 2 {
            return None;
        }
        let axis = |get_p: fn(&[f32], usize) -> f32, get_q: fn(&[f32], usize) -> f32| {
            let mut sp = 0.0f64;
            let mut sq = 0.0f64;
            let mut spp = 0.0f64;
            let mut spq = 0.0f64;
            for i in 0..n {
                let p = get_p(verts, i) as f64;
                let q = get_q(uvs, i) as f64;
                sp += p;
                sq += q;
                spp += p * p;
                spq += p * q;
            }
            let denom = n as f64 * spp - sp * sp;
            if denom.abs() < 1e-9 {
                return None;
            }
            let s = (n as f64 * spq - sp * sq) / denom;
            let o = (sq - s * sp) / n as f64;
            // A zero scale (constant UVs on the axis) has no inverse — the
            // caller falls back to the texture convention.
            if s.abs() < 1e-12 {
                return None;
            }
            Some((s as f32, o as f32))
        };
        let (sx, ox) = axis(|v, i| v[i * 2], |u, i| u[i * 2])?;
        let (sy, oy) = axis(|v, i| v[i * 2 + 1], |u, i| u[i * 2 + 1])?;
        Some(Self { sx, ox, sy, oy })
    }

    pub fn uv(&self, pos: [f32; 2]) -> [f32; 2] {
        [self.sx * pos[0] + self.ox, self.sy * pos[1] + self.oy]
    }

    /// UV → local space (for automesh, which works in texel space).
    pub fn local(&self, uv: [f32; 2]) -> [f32; 2] {
        [(uv[0] - self.ox) / self.sx, (uv[1] - self.oy) / self.sy]
    }
}

/// Decoded alpha channel of a part's albedo.
pub struct AlphaMask {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl AlphaMask {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (width, height) = (img.width(), img.height());
        let alpha = img.pixels().map(|p| p.0[3]).collect();
        Some(Self {
            width,
            height,
            alpha,
        })
    }

    fn at(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return 0;
        }
        self.alpha[(y as u32 * self.width + x as u32) as usize]
    }

    fn sample_uv(&self, uv: [f32; 2]) -> u8 {
        let x = (uv[0] * self.width as f32).floor() as i64;
        let y = (uv[1] * self.height as f32).floor() as i64;
        self.at(x, y)
    }

    /// Does the triangle cover any texel above the cull threshold? Sampled on
    /// a small barycentric grid plus corners — cheap and errs on keeping.
    fn triangle_covers_opaque(&self, a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
        const THRESHOLD: u8 = 4;
        const STEPS: u32 = 6;
        for i in 0..=STEPS {
            for j in 0..=(STEPS - i) {
                let u = i as f32 / STEPS as f32;
                let v = j as f32 / STEPS as f32;
                let w = 1.0 - u - v;
                let p = [
                    a[0] * u + b[0] * v + c[0] * w,
                    a[1] * u + b[1] * v + c[1] * w,
                ];
                if self.sample_uv(p) > THRESHOLD {
                    return true;
                }
            }
        }
        false
    }
}

fn dist2(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    dx * dx + dy * dy
}

/// Proper segment crossing (interiors intersect). Shared endpoints are the
/// caller's business; touching at a point that is an endpoint of either
/// segment does not count as crossing.
fn segments_cross(p1: [f32; 2], p2: [f32; 2], p3: [f32; 2], p4: [f32; 2]) -> bool {
    let d1 = orient(p3, p4, p1);
    let d2 = orient(p3, p4, p2);
    let d3 = orient(p1, p2, p3);
    let d4 = orient(p1, p2, p4);
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    // Collinear overlap (not a single touching point) also breaks the CDT.
    if d1 == 0.0 && d2 == 0.0 && d3 == 0.0 && d4 == 0.0 {
        let overlap_1d = |a1: f32, a2: f32, b1: f32, b2: f32| {
            let (alo, ahi) = (a1.min(a2), a1.max(a2));
            let (blo, bhi) = (b1.min(b2), b1.max(b2));
            alo.max(blo) < ahi.min(bhi)
        };
        return overlap_1d(p1[0], p2[0], p3[0], p4[0]) || overlap_1d(p1[1], p2[1], p3[1], p4[1]);
    }
    false
}

fn orient(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

// ---- automesh ----

/// Contour automesh knobs (texel units).
#[derive(Debug, Clone, Copy)]
pub struct ContourKnobs {
    /// Alpha above this counts as solid.
    pub threshold: u8,
    /// Douglas-Peucker simplification tolerance.
    pub simplify: f32,
    /// Outward dilation of the solid mask before tracing.
    pub margin: u32,
    /// Interior fill-point spacing; 0 = boundary only.
    pub spacing: u32,
}

impl Default for ContourKnobs {
    fn default() -> Self {
        Self {
            threshold: 16,
            simplify: 6.0,
            margin: 4,
            spacing: 0,
        }
    }
}

/// Trace the texture's alpha contours into a boundary-constrained working
/// mesh: one pinned loop per connected component, plus optional interior fill
/// points. Vertices land in node-local space through `uv_map`.
pub fn contour_automesh(
    alpha: &AlphaMask,
    knobs: &ContourKnobs,
    uv_map: &UvMap,
    origin: [f32; 2],
) -> Result<WorkingMesh, MeshError> {
    let (w, h) = (alpha.width as i64, alpha.height as i64);
    let mut solid = vec![false; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            solid[(y * w + x) as usize] = alpha.at(x, y) > knobs.threshold;
        }
    }
    if knobs.margin > 0 {
        solid = dilate(&solid, w, h, knobs.margin as i64);
    }

    let loops = trace_components(&solid, w, h);
    let mut mesh = WorkingMesh {
        verts: Vec::new(),
        constraints: Vec::new(),
        origin,
    };
    let texel_uv =
        |p: [f32; 2]| -> [f32; 2] { [p[0] / alpha.width as f32, p[1] / alpha.height as f32] };
    for contour in loops {
        if contour.len() < 3 {
            continue;
        }
        let simplified = simplify_loop(&contour, knobs.simplify);
        if simplified.len() < 3 {
            continue;
        }
        let mut ids = Vec::with_capacity(simplified.len());
        for p in &simplified {
            let local = uv_map.local(texel_uv(*p));
            match mesh.add_vertex(local) {
                Ok(id) => ids.push(id),
                Err(_) => continue, // coincident with an earlier contour point
            }
        }
        for k in 0..ids.len() {
            let a = ids[k];
            let b = ids[(k + 1) % ids.len()];
            // A crossing here means two simplified loops overlap; skip the
            // offending pin rather than fail the whole trace.
            let _ = mesh.add_constraint(a, b);
        }
    }

    if knobs.spacing > 0 {
        let s = knobs.spacing as i64;
        let mut y = s / 2;
        while y < h {
            let mut x = s / 2;
            while x < w {
                if interior(&solid, w, h, x, y, s / 2) {
                    let local = uv_map.local(texel_uv([x as f32, y as f32]));
                    let _ = mesh.add_vertex(local);
                }
                x += s;
            }
            y += s;
        }
    }
    Ok(mesh)
}

/// Regular grid over the solid texels' bounding box.
pub fn grid_automesh(
    alpha: &AlphaMask,
    threshold: u8,
    cols: u32,
    rows: u32,
    uv_map: &UvMap,
    origin: [f32; 2],
) -> Result<WorkingMesh, MeshError> {
    let (w, h) = (alpha.width as i64, alpha.height as i64);
    let mut min = [i64::MAX, i64::MAX];
    let mut max = [i64::MIN, i64::MIN];
    for y in 0..h {
        for x in 0..w {
            if alpha.at(x, y) > threshold {
                min = [min[0].min(x), min[1].min(y)];
                max = [max[0].max(x), max[1].max(y)];
            }
        }
    }
    if min[0] > max[0] {
        return Err(MeshError::NothingToMesh);
    }
    // One texel of margin so boundary texels stay inside the outer cells.
    let min = [(min[0] - 1) as f32, (min[1] - 1) as f32];
    let max = [(max[0] + 2) as f32, (max[1] + 2) as f32];
    let (cols, rows) = (cols.max(1), rows.max(1));
    let mut mesh = WorkingMesh {
        verts: Vec::new(),
        constraints: Vec::new(),
        origin,
    };
    for j in 0..=rows {
        for i in 0..=cols {
            let fx = i as f32 / cols as f32;
            let fy = j as f32 / rows as f32;
            let tx = min[0] + fx * (max[0] - min[0]);
            let ty = min[1] + fy * (max[1] - min[1]);
            let local = uv_map.local([tx / alpha.width as f32, ty / alpha.height as f32]);
            mesh.add_vertex(local)?;
        }
    }
    Ok(mesh)
}

/// Separable box dilation: a horizontal then a vertical sliding-window pass,
/// O(w·h) regardless of radius (a square structuring element, which is what
/// a safety margin wants).
fn dilate(solid: &[bool], w: i64, h: i64, r: i64) -> Vec<bool> {
    let mut horiz = vec![false; solid.len()];
    for y in 0..h {
        let row = (y * w) as usize;
        let mut run = 0i64; // solid cells among the last (2r+1) scanned
        for x in -r..w {
            if x + r < w && solid[row + (x + r) as usize] {
                run += 1;
            }
            if x - r > 0 && solid[row + (x - r - 1) as usize] {
                run -= 1;
            }
            if x >= 0 && run > 0 {
                horiz[row + x as usize] = true;
            }
        }
    }
    let mut out = vec![false; solid.len()];
    for x in 0..w {
        let mut run = 0i64;
        for y in -r..h {
            if y + r < h && horiz[((y + r) * w + x) as usize] {
                run += 1;
            }
            if y - r > 0 && horiz[((y - r - 1) * w + x) as usize] {
                run -= 1;
            }
            if y >= 0 && run > 0 {
                out[(y * w + x) as usize] = true;
            }
        }
    }
    out
}

fn interior(solid: &[bool], w: i64, h: i64, x: i64, y: i64, r: i64) -> bool {
    let at = |x: i64, y: i64| -> bool {
        x >= 0 && y >= 0 && x < w && y < h && solid[(y * w + x) as usize]
    };
    if !at(x, y) {
        return false;
    }
    for (dx, dy) in [
        (r, 0),
        (-r, 0),
        (0, r),
        (0, -r),
        (r, r),
        (-r, r),
        (r, -r),
        (-r, -r),
    ] {
        if !at(x + dx, y + dy) {
            return false;
        }
    }
    true
}

/// Boundary loops of each connected component (Moore-neighbor tracing).
fn trace_components(solid: &[bool], w: i64, h: i64) -> Vec<Vec<[f32; 2]>> {
    let at = |x: i64, y: i64| -> bool {
        x >= 0 && y >= 0 && x < w && y < h && solid[(y * w + x) as usize]
    };
    let mut labeled = vec![false; solid.len()];
    let mut loops = Vec::new();
    for y in 0..h {
        for x in 0..w {
            if !at(x, y) || labeled[(y * w + x) as usize] || at(x - 1, y) {
                continue;
            }
            // Trace this component's outer boundary, then flood-fill the label.
            loops.push(moore_trace(&at, x, y));
            flood_label(solid, &mut labeled, w, h, x, y);
        }
    }
    loops
}

fn moore_trace(at: &dyn Fn(i64, i64) -> bool, sx: i64, sy: i64) -> Vec<[f32; 2]> {
    // 8-neighborhood, clockwise starting from west.
    const N: [(i64, i64); 8] = [
        (-1, 0),
        (-1, -1),
        (0, -1),
        (1, -1),
        (1, 0),
        (1, 1),
        (0, 1),
        (-1, 1),
    ];
    let mut contour = Vec::new();
    let (mut cx, mut cy) = (sx, sy);
    // Entered from the west (the scan guarantees (sx-1, sy) is empty).
    let mut backtrack = 0usize;
    let mut first_exit: Option<usize> = None;
    let limit = 4 * (1 << 20);
    loop {
        contour.push([cx as f32 + 0.5, cy as f32 + 0.5]);
        let mut found = None;
        for k in 0..8 {
            let dir = (backtrack + k) % 8;
            let (nx, ny) = (cx + N[dir].0, cy + N[dir].1);
            if at(nx, ny) {
                found = Some((nx, ny, dir));
                break;
            }
        }
        let Some((nx, ny, dir)) = found else {
            break; // isolated pixel
        };
        // Jacob's criterion: stop when leaving the start pixel in the same
        // direction as the first step — merely revisiting the start (a
        // pinched, figure-eight boundary) must keep tracing.
        if cx == sx && cy == sy {
            match first_exit {
                None => first_exit = Some(dir),
                Some(first) if dir == first => break,
                Some(_) => {}
            }
        }
        cx = nx;
        cy = ny;
        // Next scan starts from the neighbor after the one we came from.
        backtrack = (dir + 5) % 8;
        if contour.len() > limit {
            break;
        }
    }
    contour
}

fn flood_label(solid: &[bool], labeled: &mut [bool], w: i64, h: i64, sx: i64, sy: i64) {
    let mut stack = vec![(sx, sy)];
    while let Some((x, y)) = stack.pop() {
        if x < 0 || y < 0 || x >= w || y >= h {
            continue;
        }
        let i = (y * w + x) as usize;
        if !solid[i] || labeled[i] {
            continue;
        }
        labeled[i] = true;
        stack.extend([(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)]);
    }
}

/// Douglas-Peucker on a closed loop (anchored at 0 and the midpoint).
fn simplify_loop(points: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    if points.len() < 4 || eps <= 0.0 {
        return points.to_vec();
    }
    let mid = points.len() / 2;
    let mut out = Vec::new();
    dp(&points[..=mid], eps, &mut out);
    out.pop();
    let mut second = Vec::new();
    let mut wrapped: Vec<[f32; 2]> = points[mid..].to_vec();
    wrapped.push(points[0]);
    dp(&wrapped, eps, &mut second);
    second.pop();
    out.extend(second);
    out
}

fn dp(points: &[[f32; 2]], eps: f32, out: &mut Vec<[f32; 2]>) {
    if points.len() < 2 {
        out.extend_from_slice(points);
        return;
    }
    let (a, b) = (points[0], points[points.len() - 1]);
    let mut far = 0usize;
    let mut far_d = -1.0f32;
    for (i, p) in points.iter().enumerate().skip(1).take(points.len() - 2) {
        let d = point_segment_dist(*p, a, b);
        if d > far_d {
            far_d = d;
            far = i;
        }
    }
    if far_d > eps {
        dp(&points[..=far], eps, out);
        out.pop();
        dp(&points[far..], eps, out);
    } else {
        out.push(a);
        out.push(b);
    }
}

fn point_segment_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let ap = [p[0] - a[0], p[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        ((ap[0] * ab[0] + ap[1] * ab[1]) / len2).clamp(0.0, 1.0)
    };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t];
    dist2(p, q).sqrt()
}

// ---- deform re-fit ----

/// Re-fit one deform cell's offsets from the old mesh onto new rest vertices:
/// each new vertex takes the barycentric blend of the old offsets in the old
/// rest triangle containing it (clamped to the nearest triangle outside).
/// Re-fitting onto identical topology is the identity.
pub fn refit_deform_offsets(old: &ClmMesh, new_verts: &[f32], old_offsets: &[f32]) -> Vec<f32> {
    let old_tris: Vec<[u32; 3]> = match &old.indices {
        ClmIndices::U16(v) => v
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [t[0] as u32, t[1] as u32, t[2] as u32])
            .collect(),
        ClmIndices::U32(v) => v
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [t[0], t[1], t[2]])
            .collect(),
    };
    let old_pos = |i: u32| -> [f32; 2] {
        [
            old.verts.get(i as usize * 2).copied().unwrap_or(0.0),
            old.verts.get(i as usize * 2 + 1).copied().unwrap_or(0.0),
        ]
    };
    let old_off = |i: u32| -> [f32; 2] {
        [
            old_offsets.get(i as usize * 2).copied().unwrap_or(0.0),
            old_offsets.get(i as usize * 2 + 1).copied().unwrap_or(0.0),
        ]
    };

    // Bit-exact position → old vertex index; identical vertices keep their
    // offsets verbatim, covering isolated vertices and duplicates that the
    // triangle walk would blend or drop.
    let mut exact: std::collections::HashMap<(u32, u32), u32> = std::collections::HashMap::new();
    for i in (0..(old.verts.len() / 2) as u32).rev() {
        let q = old_pos(i);
        exact.insert((q[0].to_bits(), q[1].to_bits()), i);
    }
    let mut out = Vec::with_capacity(new_verts.len());
    for p in new_verts.as_chunks::<2>().0 {
        let p = [p[0], p[1]];
        if let Some(&i) = exact.get(&(p[0].to_bits(), p[1].to_bits())) {
            out.extend_from_slice(&old_off(i));
            continue;
        }
        let mut best: Option<(f32, [f32; 2])> = None;
        for t in &old_tris {
            let (a, b, c) = (old_pos(t[0]), old_pos(t[1]), old_pos(t[2]));
            let Some(bary) = barycentric(p, a, b, c) else {
                continue;
            };
            let clamped = clamp_bary(bary);
            let q = [
                a[0] * clamped[0] + b[0] * clamped[1] + c[0] * clamped[2],
                a[1] * clamped[0] + b[1] * clamped[1] + c[1] * clamped[2],
            ];
            let d = dist2(p, q);
            let (oa, ob, oc) = (old_off(t[0]), old_off(t[1]), old_off(t[2]));
            let off = [
                oa[0] * clamped[0] + ob[0] * clamped[1] + oc[0] * clamped[2],
                oa[1] * clamped[0] + ob[1] * clamped[1] + oc[1] * clamped[2],
            ];
            if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, off));
            }
            if d <= f32::EPSILON {
                break;
            }
        }
        let off = best.map(|(_, o)| o).unwrap_or([0.0, 0.0]);
        out.extend_from_slice(&off);
    }
    out
}

fn barycentric(p: [f32; 2], a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> Option<[f32; 3]> {
    let v0 = [b[0] - a[0], b[1] - a[1]];
    let v1 = [c[0] - a[0], c[1] - a[1]];
    let v2 = [p[0] - a[0], p[1] - a[1]];
    let den = v0[0] * v1[1] - v1[0] * v0[1];
    if den.abs() <= f32::EPSILON {
        return None;
    }
    let v = (v2[0] * v1[1] - v1[0] * v2[1]) / den;
    let w = (v0[0] * v2[1] - v2[0] * v0[1]) / den;
    Some([1.0 - v - w, v, w])
}

fn clamp_bary(b: [f32; 3]) -> [f32; 3] {
    let clamped = [b[0].max(0.0), b[1].max(0.0), b[2].max(0.0)];
    let sum = clamped[0] + clamped[1] + clamped[2];
    if sum <= f32::EPSILON {
        [1.0, 0.0, 0.0]
    } else {
        [clamped[0] / sum, clamped[1] / sum, clamped[2] / sum]
    }
}

/// Mesh authoring over a [`Model`]. An extension trait because the Model is
/// defined in `catchlight-core` while the mesh tools live here.
pub trait ModelMeshExt {
    /// Replace a meshed node's mesh and re-fit every deform binding that drives
    /// the node onto the new topology — one undoable step. The offsets are
    /// carried over by triangle-affine interpolation across the old rest mesh;
    /// [`Model::set_node_mesh_with`] validates the mesh and moves the cells and
    /// the mesh together.
    ///
    /// Seams are not re-fitted, because which vertex a slot names is not
    /// something interpolation can answer: the returned slots are the ones the
    /// edit emptied, for the caller to put in front of the author.
    fn set_mesh_with_refit(
        &mut self,
        node: &NodeId,
        mesh: ClmMesh,
    ) -> Result<Vec<(SeamId, SlotId)>, ModelError>;
}

impl ModelMeshExt for Model {
    fn set_mesh_with_refit(
        &mut self,
        node: &NodeId,
        mesh: ClmMesh,
    ) -> Result<Vec<(SeamId, SlotId)>, ModelError> {
        self.set_node_mesh_with(node, mesh, |old, new, offsets| {
            refit_deform_offsets(old, &new.verts, offsets)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> ClmMesh {
        ClmMesh {
            verts: vec![-1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0],
            uvs: vec![0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0],
            indices: ClmIndices::U16(vec![0, 1, 2, 0, 2, 3]),
            origin: [0.0, 0.0],
        }
    }

    #[test]
    fn seeded_constraints_reproduce_topology() {
        let wm = WorkingMesh::from_mesh(&quad());
        assert_eq!(wm.constraints.len(), 5); // 4 border + 1 diagonal
        let tris = wm.triangulate().unwrap();
        assert_eq!(tris.len(), 2);
        // The seeded diagonal (0,2) survives: both triangles use edge 0-2.
        for t in &tris {
            assert!(t.contains(&0) && t.contains(&2));
        }
    }

    #[test]
    fn crossing_constraints_are_rejected_at_the_op() {
        let mut wm = WorkingMesh::from_mesh(&quad());
        // The other diagonal (1,3) crosses the seeded (0,2).
        assert!(matches!(
            wm.add_constraint(1, 3),
            Err(MeshError::ConstraintCross)
        ));
        // Unpinning the first diagonal makes room.
        wm.remove_constraint(0, 2);
        wm.add_constraint(1, 3).unwrap();
        let tris = wm.triangulate().unwrap();
        for t in &tris {
            assert!(t.contains(&1) && t.contains(&3));
        }
    }

    #[test]
    fn vertex_moves_that_cross_pins_are_rejected() {
        let mut wm = WorkingMesh::from_mesh(&quad());
        // Hang a pinned edge off corner 0 toward the lower-left, clear of the
        // quad.
        let v = wm.add_vertex([-1.5, -1.5]).unwrap();
        wm.add_constraint(0, v).unwrap();
        // Moving it to the far right would drag constraint (0,v) across the
        // quad's right edge (1,2) — rejected.
        assert!(matches!(
            wm.move_vertex(v, [2.0, 0.0]),
            Err(MeshError::ConstraintCross)
        ));
        // A harmless move is fine.
        wm.move_vertex(v, [-2.0, -1.2]).unwrap();
        // Stacking onto an existing vertex is refused.
        assert!(wm.move_vertex(v, [1.0, -1.0]).is_err());
        // Pinning an edge that would cross is refused too (0 -> corner 2's
        // diagonal already pinned; try v -> corner 2 which crosses edge 0-3).
        assert!(wm.add_constraint(v, 2).is_err());
    }

    #[test]
    fn delete_remaps_constraints() {
        let mut wm = WorkingMesh::from_mesh(&quad());
        wm.delete_vertices(&[1]);
        assert_eq!(wm.vertex_count(), 3);
        assert!(wm
            .constraints
            .iter()
            .all(|&(a, b)| a < 3 && b < 3 && a != b));
        assert!(wm.triangulate().unwrap().len() == 1);
    }

    #[test]
    fn to_mesh_derives_uvs_and_culls_transparent_triangles() {
        // 4x4 texture: left half opaque, right half transparent.
        let alpha = AlphaMask {
            width: 4,
            height: 4,
            alpha: (0..16).map(|i| if i % 4 < 2 { 255 } else { 0 }).collect(),
        };
        let mut wm = WorkingMesh::default();
        // Two side-by-side quads spanning the texture, y in [-2, 2], x in [-2, 2].
        for &p in &[
            [-2.0, -2.0],
            [0.0, -2.0],
            [2.0, -2.0],
            [-2.0, 2.0],
            [0.0, 2.0],
            [2.0, 2.0],
        ] {
            wm.add_vertex(p).unwrap();
        }
        let uv_map = UvMap::from_texture_size(4.0, 4.0);
        let mesh = wm.to_mesh(&uv_map, Some(&alpha)).unwrap();
        let tri_count = match &mesh.indices {
            ClmIndices::U16(v) => v.len() / 3,
            ClmIndices::U32(v) => v.len() / 3,
        };
        // The right half's triangles are culled.
        assert!((2..8).contains(&tri_count), "got {tri_count} triangles");
        // UVs follow the texture convention: x=-2 -> u=0..(-2/4+0.5)=0.
        let uv0 = [mesh.uvs[0], mesh.uvs[1]];
        assert!((uv0[0] - 0.0).abs() < 1e-5);
        assert!((uv0[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn uv_fit_recovers_the_texture_convention() {
        let q = quad();
        let map = UvMap::fit(&q.verts, &q.uvs).unwrap();
        for i in 0..4 {
            let uv = map.uv([q.verts[i * 2], q.verts[i * 2 + 1]]);
            assert!((uv[0] - q.uvs[i * 2]).abs() < 1e-4);
            assert!((uv[1] - q.uvs[i * 2 + 1]).abs() < 1e-4);
        }
    }

    #[test]
    fn refit_identity_on_same_topology() {
        let q = quad();
        let offsets = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let out = refit_deform_offsets(&q, &q.verts, &offsets);
        assert_eq!(out, offsets);
    }

    #[test]
    fn refit_interpolates_inside_old_triangles() {
        let q = quad();
        // Uniform +10 x offset everywhere -> any interior point gets +10.
        let offsets = vec![10.0, 0.0, 10.0, 0.0, 10.0, 0.0, 10.0, 0.0];
        let out = refit_deform_offsets(&q, &[0.0, 0.0, 0.5, 0.5], &offsets);
        assert!((out[0] - 10.0).abs() < 1e-4);
        assert!((out[1]).abs() < 1e-4);
        assert!((out[2] - 10.0).abs() < 1e-4);
    }

    #[test]
    fn set_mesh_with_refit_updates_bindings_in_one_step() {
        use catchlight_core::id::{Name, SeededHex};
        use catchlight_core::{
            BindingKey, BindingTarget, ModelNode, ModelNodeKind, ModelParam, ModelPart,
        };

        let mut hex = SeededHex::new(1);
        let mut m = Model::new();
        let root = m.root().unwrap().clone();
        let part = m
            .add_node(
                &root,
                ModelNode::new("p", ModelNodeKind::Part(ModelPart::new(quad()))),
                &mut hex,
            )
            .unwrap();
        let param = m
            .add_param(
                ModelParam::new(Name::truncated("d"), 0.0, 1.0, 0.0),
                &mut hex,
            )
            .unwrap();
        let key = BindingKey::new(param, part.clone(), BindingTarget::Deform);
        m.set_deform_from_transform(&key, [1, 0], [10.0, 0.0], 0.0, [1.0, 1.0])
            .unwrap();

        // New topology: the same quad plus a center vertex.
        let mut new_mesh = quad();
        new_mesh.verts.extend_from_slice(&[0.0, 0.0]);
        new_mesh.uvs.extend_from_slice(&[0.5, 0.5]);
        new_mesh.indices = ClmIndices::U16(vec![0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4]);
        m.set_mesh_with_refit(&part, new_mesh).unwrap();

        let b = m.binding(&key).unwrap();
        let cells = catchlight_core::deform_cells(b.values()).unwrap();
        assert_eq!(cells[0].value.len(), 10);
        // Old corners keep the uniform offset; the new center interpolates it.
        assert!((cells[1].value[8] - 10.0).abs() < 1e-4);
    }
}
