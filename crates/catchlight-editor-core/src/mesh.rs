//! Mesh editing over a constrained-Delaunay model. The triangulation is always
//! derived from vertices plus non-crossing constraint edges, so a state the
//! tool accepted can always be applied.

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation as _};

use catchlight_core::id::{NodeId, SlotId};
use catchlight_core::{LoadLimitError, LoadResource, Model, ModelError};

/// Coincident points would merge in the triangulation and corrupt indexing.
const MIN_VERTEX_DISTANCE: f32 = 1e-3;

/// Most cells a generated grid may have along one axis. Named axis lines are
/// bounded by the lines this many cells need.
pub const MAX_GRID_DIVISIONS: u32 = 128;

/// Largest contour dilation, in texels.
pub const MAX_CONTOUR_MARGIN: u32 = 128;

/// Not a [`ModelError`]: a working mesh is not in a model yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MeshError {
    #[error("no vertex at that index")]
    NoSuchVertex,
    #[error("a vertex cannot land on another vertex")]
    Coincident,
    #[error("a vertex position must be a finite number")]
    NotFinite,
    /// Touching at a shared endpoint is fine.
    #[error("constraint edges may not cross")]
    ConstraintCross,
    #[error("the texture is empty above the alpha threshold")]
    NothingToMesh,
    #[error(
        "This mesh has a custom texture mapping. Mesh placement requires an undeformed artwork mesh."
    )]
    CustomTextureMapping,
    #[error("triangulation failed")]
    Triangulation,
    #[error(transparent)]
    Limit(#[from] LoadLimitError),
}

#[derive(Debug, Clone, Default)]
pub struct WorkingMesh {
    /// Flat `[x, y, …]` rest positions in node-local space.
    pub verts: Vec<f32>,
    /// Vertex index pairs, unordered. Always non-crossing.
    pub constraints: Vec<(u32, u32)>,
    pub origin: [f32; 2],
}

impl WorkingMesh {
    /// Every current triangle edge is seeded as a constraint, so the CDT
    /// preserves the topology until the user unpins.
    pub fn from_mesh(mesh: &ClmMesh) -> Self {
        let vcount = (mesh.verts.len() / 2) as u32;
        let mut constraints: Vec<(u32, u32)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // Out-of-range indices (a malformed file) are dropped rather than seeded.
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
            ClmIndices::U16(v) => tris(&mut v.iter().map(|&i| u32::from(i))),
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

    pub fn add_vertex(&mut self, pos: [f32; 2]) -> Result<u32, MeshError> {
        if !pos[0].is_finite() || !pos[1].is_finite() {
            return Err(MeshError::NotFinite);
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

    pub fn move_vertex(&mut self, i: u32, pos: [f32; 2]) -> Result<(), MeshError> {
        if i as usize >= self.vertex_count() {
            return Err(MeshError::NoSuchVertex);
        }
        if !pos[0].is_finite() || !pos[1].is_finite() {
            return Err(MeshError::NotFinite);
        }
        for j in 0..self.vertex_count() as u32 {
            if j != i && dist2(self.pos(j), pos) < MIN_VERTEX_DISTANCE * MIN_VERTEX_DISTANCE {
                return Err(MeshError::Coincident);
            }
        }
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

    /// Final positions are validated together, so adjacent vertices cannot
    /// collide with an intermediate state.
    pub fn move_vertices(&mut self, positions: &[(u32, [f32; 2])]) -> Result<(), MeshError> {
        let mut next = self.clone();
        for &(index, point) in positions {
            if index as usize >= next.vertex_count() {
                return Err(MeshError::NoSuchVertex);
            }
            next.verts[index as usize * 2..index as usize * 2 + 2].copy_from_slice(&point);
        }
        for &(index, point) in positions {
            next.move_vertex(index, point)?;
        }
        *self = next;
        Ok(())
    }

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

    /// Stored state is always triangulable.
    pub fn triangulate(&self) -> Result<Vec<[u32; 3]>, MeshError> {
        let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
            ConstrainedDelaunayTriangulation::new();
        let mut handles = Vec::with_capacity(self.vertex_count());
        for i in 0..self.vertex_count() {
            let p = self.pos(i as u32);
            let h = cdt
                .insert(Point2::new(f64::from(p[0]), f64::from(p[1])))
                .map_err(|_| MeshError::Triangulation)?;
            handles.push(h);
        }
        // A bijection: the add/move guard rules out coincident points.
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
        let indices = if u16::try_from(self.vertex_count()).is_ok() {
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
/// Catchlight meshes always map the texture axis-aligned.
#[derive(Debug, Clone, Copy)]
pub struct UvMap {
    pub sx: f32,
    pub ox: f32,
    pub sy: f32,
    pub oy: f32,
}

impl UvMap {
    /// Texture centered on the local origin, `v` increasing downward.
    pub fn from_texture_size(w: f32, h: f32) -> Self {
        Self {
            sx: 1.0 / w.max(1.0),
            ox: 0.5,
            sy: -1.0 / h.max(1.0),
            oy: 0.5,
        }
    }

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
                let p = f64::from(get_p(verts, i));
                let q = f64::from(get_q(uvs, i));
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

    pub fn local(&self, uv: [f32; 2]) -> [f32; 2] {
        [(uv[0] - self.ox) / self.sx, (uv[1] - self.oy) / self.sy]
    }
}

pub struct AlphaMask {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl AlphaMask {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        Some(Self::from_rgba(&img))
    }

    /// TGA has no reliable magic signature, so the declared encoding is used.
    pub fn decode_as(
        bytes: &[u8],
        encoding: catchlight_core::formats::clm::TextureEncoding,
    ) -> Option<Self> {
        let format = match encoding {
            catchlight_core::formats::clm::TextureEncoding::Png => image::ImageFormat::Png,
            catchlight_core::formats::clm::TextureEncoding::Tga => image::ImageFormat::Tga,
        };
        let img = image::load_from_memory_with_format(bytes, format)
            .ok()?
            .to_rgba8();
        Some(Self::from_rgba(&img))
    }

    fn from_rgba(img: &image::RgbaImage) -> Self {
        let (width, height) = (img.width(), img.height());
        let alpha = img.pixels().map(|p| p.0[3]).collect();
        Self {
            width,
            height,
            alpha,
        }
    }

    fn at(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return 0;
        }
        self.alpha[(y as u32 * self.width + x as u32) as usize]
    }

    fn sample_uv(&self, uv: [f32; 2]) -> u8 {
        let x = (uv[0] * self.width as f32).floor() as i64;
        let y = (uv[1] * self.height as f32).floor() as i64;
        self.at(x, y)
    }

    /// Sampled on a small barycentric grid plus corners; errs on keeping.
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

/// Interiors intersect; touching at an endpoint of either does not count.
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

/// Texel units.
#[derive(Debug, Clone)]
pub struct ContourKnobs {
    pub threshold: u8,
    pub simplify: f32,
    pub margin: u32,
    /// 0 = boundary only.
    pub spacing: u32,
    /// Ring scale factors about each component's centroid, clamped into `0..=1`
    /// (1 is the outline, which `margin` already dilated).
    pub rings: Vec<f32>,
    /// 0 drops only coincident vertices.
    pub min_distance: f32,
    /// Applies to free vertices only. The pinned outline is not mirrored: the
    /// alpha is the authority on where the art ends.
    pub mirror_x: Option<f32>,
}

impl Default for ContourKnobs {
    fn default() -> Self {
        Self {
            threshold: 16,
            simplify: 6.0,
            margin: 4,
            spacing: 0,
            rings: Vec::new(),
            min_distance: 0.0,
            mirror_x: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GridKnobs {
    pub threshold: u8,
    pub cols: u32,
    pub rows: u32,
    /// Fractions of the solid bounding box (0 left edge, 1 right; outside
    /// `0..=1` allowed). Non-empty replaces `cols` *and* `margin` on this axis.
    pub axes_x: Vec<f32>,
    pub axes_y: Vec<f32>,
    /// Applies when lines come from `cols`/`rows`. `None` is one texel, so edge
    /// texels sit inside the outer cells rather than on their edge.
    pub margin: Option<f32>,
}

impl Default for GridKnobs {
    fn default() -> Self {
        Self {
            threshold: 16,
            cols: 6,
            rows: 6,
            axes_x: Vec::new(),
            axes_y: Vec::new(),
            margin: None,
        }
    }
}

struct FreeVerts<'a> {
    uv_map: &'a UvMap,
    dims: [f32; 2],
    min_d2: f32,
    mirror_x: Option<f32>,
    placed: Vec<[f32; 2]>,
}

impl FreeVerts<'_> {
    fn record(&mut self, texel: [f32; 2]) {
        self.placed.push(texel);
    }

    /// A candidate on the far side of the mirror line is dropped.
    fn place(&mut self, mesh: &mut WorkingMesh, texel: [f32; 2]) {
        match self.mirror_x {
            Some(m) if texel[0] > m => (),
            Some(m) => {
                self.place_one(mesh, texel);
                self.place_one(mesh, [2.0 * m - texel[0], texel[1]]);
            }
            None => self.place_one(mesh, texel),
        }
    }

    fn place_one(&mut self, mesh: &mut WorkingMesh, texel: [f32; 2]) {
        if self.placed.iter().any(|q| dist2(*q, texel) < self.min_d2) {
            return;
        }
        let uv = [texel[0] / self.dims[0], texel[1] / self.dims[1]];
        if mesh.add_vertex(self.uv_map.local(uv)).is_ok() {
            self.placed.push(texel);
        }
    }
}

/// One pinned loop per connected alpha component, plus free interior
/// vertices. Vertices land in node-local space through `uv_map`.
pub fn contour_automesh(
    alpha: &AlphaMask,
    knobs: &ContourKnobs,
    uv_map: &UvMap,
    origin: [f32; 2],
) -> Result<WorkingMesh, MeshError> {
    within(
        LoadResource::ContourMargin,
        MAX_CONTOUR_MARGIN,
        knobs.margin as usize,
    )?;
    let (w, h) = (i64::from(alpha.width), i64::from(alpha.height));
    let mut solid = vec![false; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            solid[(y * w + x) as usize] = alpha.at(x, y) > knobs.threshold;
        }
    }
    if knobs.margin > 0 {
        solid = dilate(&solid, w, h, i64::from(knobs.margin));
    }

    let loops = trace_components(&solid, w, h);
    let mut mesh = WorkingMesh {
        verts: Vec::new(),
        constraints: Vec::new(),
        origin,
    };
    let mut free = FreeVerts {
        uv_map,
        dims: [alpha.width as f32, alpha.height as f32],
        // NaN compares false against every distance, so a bad number filters
        // nothing rather than everything; `max` folds it to the default.
        min_d2: {
            let d = knobs.min_distance.max(0.0);
            d * d
        },
        mirror_x: knobs.mirror_x.filter(|m| m.is_finite()),
        placed: Vec::new(),
    };

    // Outlines first: rings and `min_distance` depend on them.
    let mut outlines: Vec<Vec<[f32; 2]>> = Vec::new();
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
            let local = uv_map.local([p[0] / alpha.width as f32, p[1] / alpha.height as f32]);
            // An `Err` is a point coincident with an earlier contour point.
            if let Ok(id) = mesh.add_vertex(local) {
                ids.push(id);
                free.record(*p);
            }
        }
        for k in 0..ids.len() {
            let a = ids[k];
            let b = ids[(k + 1) % ids.len()];
            // Overlapping simplified loops: skip the pin rather than fail the trace.
            let _ = mesh.add_constraint(a, b);
        }
        outlines.push(simplified);
    }

    // A ring's vertex count scales with its factor, keeping density roughly
    // even across rings.
    for outline in &outlines {
        let c = centroid(outline);
        for &factor in &knobs.rings {
            if !factor.is_finite() {
                continue;
            }
            let factor = factor.clamp(0.0, 1.0);
            let n = ((outline.len() as f32 * factor).round() as usize).max(1);
            for p in ring_points(outline, c, factor, n) {
                free.place(&mut mesh, p);
            }
        }
    }

    if knobs.spacing > 0 {
        let s = i64::from(knobs.spacing);
        let mut y = s / 2;
        while y < h {
            let mut x = s / 2;
            while x < w {
                if interior(&solid, w, h, x, y, s / 2) {
                    free.place(&mut mesh, [x as f32, y as f32]);
                }
                x += s;
            }
            y += s;
        }
    }
    Ok(mesh)
}

fn centroid(points: &[[f32; 2]]) -> [f32; 2] {
    let n = points.len().max(1) as f32;
    let sum = points
        .iter()
        .fold([0.0f32, 0.0], |a, p| [a[0] + p[0], a[1] + p[1]]);
    [sum[0] / n, sum[1] / n]
}

/// A degenerate ring (factor 0, or a zero-length loop) is the centre itself.
fn ring_points(outline: &[[f32; 2]], c: [f32; 2], factor: f32, n: usize) -> Vec<[f32; 2]> {
    let scaled: Vec<[f32; 2]> = outline
        .iter()
        .map(|p| [c[0] + (p[0] - c[0]) * factor, c[1] + (p[1] - c[1]) * factor])
        .collect();
    let mut acc = Vec::with_capacity(scaled.len() + 1);
    let mut total = 0.0f32;
    acc.push(0.0);
    for k in 0..scaled.len() {
        total += dist2(scaled[k], scaled[(k + 1) % scaled.len()]).sqrt();
        acc.push(total);
    }
    if total <= f32::EPSILON {
        return vec![c];
    }
    let mut out = Vec::with_capacity(n);
    let mut seg = 0usize;
    for i in 0..n {
        let target = total * i as f32 / n as f32;
        while seg + 2 < acc.len() && acc[seg + 1] < target {
            seg += 1;
        }
        let span = acc[seg + 1] - acc[seg];
        let t = if span <= f32::EPSILON {
            0.0
        } else {
            (target - acc[seg]) / span
        };
        let a = scaled[seg];
        let b = scaled[(seg + 1) % scaled.len()];
        out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
    }
    out
}

pub fn grid_automesh(
    alpha: &AlphaMask,
    knobs: &GridKnobs,
    uv_map: &UvMap,
    origin: [f32; 2],
) -> Result<WorkingMesh, MeshError> {
    for (axis, cells) in [(&knobs.axes_x, knobs.cols), (&knobs.axes_y, knobs.rows)] {
        if axis.is_empty() {
            within(
                LoadResource::GridDivisions,
                MAX_GRID_DIVISIONS,
                cells as usize,
            )?;
        } else {
            within(LoadResource::GridLines, MAX_GRID_DIVISIONS + 1, axis.len())?;
        }
    }
    let (w, h) = (i64::from(alpha.width), i64::from(alpha.height));
    let mut min = [i64::MAX, i64::MAX];
    let mut max = [i64::MIN, i64::MIN];
    for y in 0..h {
        for x in 0..w {
            if alpha.at(x, y) > knobs.threshold {
                min = [min[0].min(x), min[1].min(y)];
                max = [max[0].max(x), max[1].max(y)];
            }
        }
    }
    if min[0] > max[0] {
        return Err(MeshError::NothingToMesh);
    }
    // Continuous texel coordinates, so fraction 1 is the far edge of the art
    // rather than the middle of its last texel.
    let lo = [min[0] as f32, min[1] as f32];
    let size = [(max[0] + 1 - min[0]) as f32, (max[1] + 1 - min[1]) as f32];

    let lines = |given: &[f32], n: u32, lo: f32, size: f32| -> Vec<f32> {
        let named: Vec<f32> = given
            .iter()
            .filter(|f| f.is_finite())
            .map(|f| lo + f * size)
            .collect();
        if !named.is_empty() {
            let mut named = named;
            named.sort_by(f32::total_cmp);
            return named;
        }
        let pad = knobs
            .margin
            .filter(|m| m.is_finite())
            .map_or(1.0, |m| m * size);
        let (a, b) = (lo - pad, lo + size + pad);
        let n = n.max(1);
        (0..=n).map(|i| a + (b - a) * i as f32 / n as f32).collect()
    };
    // Named lines can coincide; dedupe in vertex space, since stacked vertices
    // corrupt the triangulation.
    let dedup = |mut v: Vec<f32>| -> Vec<f32> {
        v.dedup_by(|a, b| (*a - *b).abs() < MIN_VERTEX_DISTANCE);
        v
    };
    let xs = dedup(
        lines(&knobs.axes_x, knobs.cols, lo[0], size[0])
            .into_iter()
            .map(|tx| uv_map.local([tx / alpha.width as f32, 0.0])[0])
            .collect(),
    );
    let ys = dedup(
        lines(&knobs.axes_y, knobs.rows, lo[1], size[1])
            .into_iter()
            .map(|ty| uv_map.local([0.0, ty / alpha.height as f32])[1])
            .collect(),
    );

    let mut mesh = WorkingMesh {
        verts: Vec::new(),
        constraints: Vec::new(),
        origin,
    };
    for &y in &ys {
        for &x in &xs {
            mesh.add_vertex([x, y])?;
        }
    }
    Ok(mesh)
}

fn within(resource: LoadResource, limit: u32, requested: usize) -> Result<(), MeshError> {
    let requested = requested as u64;
    if requested > u64::from(limit) {
        return Err(LoadLimitError {
            resource,
            limit: limit.into(),
            got: requested,
        }
        .into());
    }
    Ok(())
}

/// Separable box dilation, O(w·h) regardless of radius.
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

/// Moore-neighbor tracing.
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
        // Jacob's criterion: merely revisiting the start (a pinched, figure-eight
        // boundary) must keep tracing.
        if cx == sx && cy == sy {
            match first_exit {
                None => first_exit = Some(dir),
                Some(first) if dir == first => break,
                Some(_) => {}
            }
        }
        cx = nx;
        cy = ny;
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
    let mut out = dp(&points[..=mid], eps);
    out.pop();
    let mut wrapped: Vec<[f32; 2]> = points[mid..].to_vec();
    wrapped.push(points[0]);
    let mut second = dp(&wrapped, eps);
    second.pop();
    out.extend(second);
    out
}

/// Iterative: a contour can be millions of points long, and an unbalanced
/// split would recurse once per point.
fn dp(points: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    if points.len() < 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut spans = vec![(0usize, points.len() - 1)];
    while let Some((lo, hi)) = spans.pop() {
        let (a, b) = (points[lo], points[hi]);
        let mut far = lo;
        let mut far_d = -1.0f32;
        for (i, p) in points.iter().enumerate().take(hi).skip(lo + 1) {
            let d = point_segment_dist(*p, a, b);
            if d > far_d {
                far_d = d;
                far = i;
            }
        }
        if far_d > eps {
            keep[far] = true;
            spans.push((lo, far));
            spans.push((far, hi));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(p, k)| k.then_some(*p))
        .collect()
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

/// Each new vertex takes the barycentric blend of the old offsets in the old
/// rest triangle containing it, clamped to the nearest triangle outside.
pub fn refit_deform_offsets(old: &ClmMesh, new_verts: &[f32], old_offsets: &[f32]) -> Vec<f32> {
    let old_tris: Vec<[u32; 3]> = match &old.indices {
        ClmIndices::U16(v) => v
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [u32::from(t[0]), u32::from(t[1]), u32::from(t[2])])
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

    // Bit-identical vertices keep their offsets verbatim, covering isolated
    // vertices and duplicates the triangle walk would blend or drop.
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
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, off));
            }
            if d <= f32::EPSILON {
                break;
            }
        }
        let off = best.map_or([0.0, 0.0], |(_, o)| o);
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

pub trait ModelMeshExt {
    /// One undoable step. Slots are not re-fitted, since interpolation cannot
    /// say which vertex a slot names: the returned slots are the ones the edit
    /// emptied.
    fn set_mesh_with_refit(
        &mut self,
        node: &NodeId,
        mesh: ClmMesh,
    ) -> Result<Vec<SlotId>, ModelError>;
}

impl ModelMeshExt for Model {
    fn set_mesh_with_refit(
        &mut self,
        node: &NodeId,
        mesh: ClmMesh,
    ) -> Result<Vec<SlotId>, ModelError> {
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
        let v = wm.add_vertex([-1.5, -1.5]).unwrap();
        wm.add_constraint(0, v).unwrap();
        // Moving it to the far right would drag constraint (0,v) across the
        // quad's right edge (1,2) — rejected.
        assert!(matches!(
            wm.move_vertex(v, [2.0, 0.0]),
            Err(MeshError::ConstraintCross)
        ));
        wm.move_vertex(v, [-2.0, -1.2]).unwrap();
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
        assert!(
            wm.constraints
                .iter()
                .all(|&(a, b)| a < 3 && b < 3 && a != b)
        );
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
        assert!((2..8).contains(&tri_count), "got {tri_count} triangles");
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

    fn square_mask() -> AlphaMask {
        let size = 64u32;
        let mut alpha = vec![0u8; (size * size) as usize];
        for y in 16..48u32 {
            for x in 16..48u32 {
                alpha[(y * size + x) as usize] = 255;
            }
        }
        AlphaMask {
            width: size,
            height: size,
            alpha,
        }
    }

    fn offset_mask() -> AlphaMask {
        let size = 64u32;
        let mut alpha = vec![0u8; (size * size) as usize];
        for y in 16..48u32 {
            for x in 8..40u32 {
                alpha[(y * size + x) as usize] = 255;
            }
        }
        AlphaMask {
            width: size,
            height: size,
            alpha,
        }
    }

    /// Added after the outlines, so they are the tail of the list.
    fn free_verts(mesh: &WorkingMesh) -> Vec<[f32; 2]> {
        let pinned = mesh
            .constraints
            .iter()
            .map(|&(a, b)| a.max(b) + 1)
            .max()
            .unwrap_or(0);
        (pinned..mesh.vertex_count() as u32)
            .map(|i| mesh.pos(i))
            .collect()
    }

    fn tracing_knobs() -> ContourKnobs {
        ContourKnobs {
            simplify: 2.0,
            margin: 0,
            ..ContourKnobs::default()
        }
    }

    #[test]
    fn rings_land_at_the_radius_they_name() {
        let alpha = square_mask();
        let uv_map = UvMap::from_texture_size(64.0, 64.0);
        // The square spans texel centres 16.5..47.5, which is local ±15.5
        // under the centered convention, centred on the origin.
        let half = 15.5f32;

        let plain = contour_automesh(&alpha, &tracing_knobs(), &uv_map, [0.0, 0.0]).unwrap();
        assert!(free_verts(&plain).is_empty(), "no rings by default");

        for &factor in &[0.5f32, 0.25] {
            let knobs = ContourKnobs {
                rings: vec![factor],
                ..tracing_knobs()
            };
            let mesh = contour_automesh(&alpha, &knobs, &uv_map, [0.0, 0.0]).unwrap();
            let ring = free_verts(&mesh);
            assert!(!ring.is_empty(), "ring {factor} placed vertices");
            for p in &ring {
                let radius = p[0].abs().max(p[1].abs());
                assert!(
                    (radius - half * factor).abs() < 0.5,
                    "ring {factor} vertex {p:?} is at radius {radius}, not {}",
                    half * factor,
                );
            }
        }

        let knobs = ContourKnobs {
            rings: vec![0.0],
            ..tracing_knobs()
        };
        let mesh = contour_automesh(&alpha, &knobs, &uv_map, [0.0, 0.0]).unwrap();
        let centre = free_verts(&mesh);
        assert_eq!(centre.len(), 1);
        assert!(
            centre[0][0].abs() < 0.5 && centre[0][1].abs() < 0.5,
            "{centre:?}"
        );

        let knobs = ContourKnobs {
            rings: vec![4.0],
            ..tracing_knobs()
        };
        let mesh = contour_automesh(&alpha, &knobs, &uv_map, [0.0, 0.0]).unwrap();
        for p in free_verts(&mesh) {
            let radius = p[0].abs().max(p[1].abs());
            assert!(radius <= half + 0.5, "vertex {p:?} escaped the outline");
        }
    }

    #[test]
    fn min_distance_thins_the_free_vertices_and_never_the_outline() {
        let alpha = square_mask();
        let uv_map = UvMap::from_texture_size(64.0, 64.0);
        let base = ContourKnobs {
            spacing: 6,
            ..tracing_knobs()
        };
        let dense = contour_automesh(&alpha, &base, &uv_map, [0.0, 0.0]).unwrap();

        let thinned = contour_automesh(
            &alpha,
            &ContourKnobs {
                min_distance: 10.0,
                ..base.clone()
            },
            &uv_map,
            [0.0, 0.0],
        )
        .unwrap();

        assert!(
            free_verts(&thinned).len() < free_verts(&dense).len(),
            "10 texels apart thins a 6-texel fill",
        );
        assert!(!free_verts(&thinned).is_empty(), "and does not empty it");
        let kept = free_verts(&thinned);
        for (i, a) in kept.iter().enumerate() {
            for b in &kept[i + 1..] {
                assert!(dist2(*a, *b).sqrt() >= 10.0 - 1e-3, "{a:?} and {b:?} crowd");
            }
        }
        assert_eq!(thinned.constraints, dense.constraints);

        let same = contour_automesh(
            &alpha,
            &ContourKnobs {
                min_distance: 0.0,
                ..base
            },
            &uv_map,
            [0.0, 0.0],
        )
        .unwrap();
        assert_eq!(same.verts, dense.verts);
    }

    #[test]
    fn a_mirror_line_makes_the_free_vertices_symmetric() {
        let alpha = offset_mask();
        let uv_map = UvMap::from_texture_size(64.0, 64.0);
        // Texel x 32 is the texture's middle, which is local x 0.
        let knobs = ContourKnobs {
            spacing: 6,
            mirror_x: Some(32.0),
            ..tracing_knobs()
        };
        let mesh = contour_automesh(&alpha, &knobs, &uv_map, [0.0, 0.0]).unwrap();
        let free = free_verts(&mesh);
        assert!(free.len() >= 4, "the fill placed something: {}", free.len());

        for p in &free {
            let mirrored = [-p[0], p[1]];
            assert!(
                free.iter().any(|q| dist2(*q, mirrored) < 1e-4),
                "{p:?} has no reflection in {free:?}",
            );
        }
        assert!(
            free.iter().any(|p| p[0] > 1.0),
            "the reflection reached past the mirror line",
        );
    }

    #[test]
    fn a_grid_puts_its_lines_where_the_axes_ask() {
        let alpha = square_mask();
        let uv_map = UvMap::from_texture_size(64.0, 64.0);
        // The solid box is texels 16..48, which is local −16..16.
        let mesh = grid_automesh(
            &alpha,
            &GridKnobs {
                axes_x: vec![0.0, 0.5, 1.0],
                axes_y: vec![0.0, 1.0],
                ..GridKnobs::default()
            },
            &uv_map,
            [0.0, 0.0],
        )
        .unwrap();
        // Rows come out with y descending: v grows downward.
        let want = [
            [-16.0, 16.0],
            [0.0, 16.0],
            [16.0, 16.0],
            [-16.0, -16.0],
            [0.0, -16.0],
            [16.0, -16.0],
        ];
        assert_eq!(mesh.vertex_count(), want.len());
        for (i, w) in want.iter().enumerate() {
            let p = mesh.pos(i as u32);
            assert!(
                (p[0] - w[0]).abs() < 0.01 && (p[1] - w[1]).abs() < 0.01,
                "vertex {i} is {p:?}, not {w:?}",
            );
        }

        let outside = grid_automesh(
            &alpha,
            &GridKnobs {
                axes_x: vec![1.5, -0.5, -0.5],
                axes_y: vec![0.5],
                ..GridKnobs::default()
            },
            &uv_map,
            [0.0, 0.0],
        )
        .unwrap();
        // Half a box (16 texels) outside each edge: texels 0 and 64, which
        // is local −32 and 32.
        assert_eq!(outside.vertex_count(), 2, "the repeat collapsed");
        assert!(
            (outside.pos(0)[0] + 32.0).abs() < 0.01,
            "sorted, −0.5 first"
        );
        assert!((outside.pos(1)[0] - 32.0).abs() < 0.01);
    }

    #[test]
    fn a_grid_margin_is_a_fraction_of_the_box_and_defaults_to_one_texel() {
        let alpha = square_mask();
        let uv_map = UvMap::from_texture_size(64.0, 64.0);
        let one_cell = |margin| {
            grid_automesh(
                &alpha,
                &GridKnobs {
                    cols: 1,
                    rows: 1,
                    margin,
                    ..GridKnobs::default()
                },
                &uv_map,
                [0.0, 0.0],
            )
            .unwrap()
        };
        // Absent: the box is texels 15..49, local ±17.
        let default = one_cell(None);
        assert!(
            (default.pos(0)[0] + 17.0).abs() < 0.01,
            "{:?}",
            default.pos(0)
        );
        // Half the box (32 texels) outside each side: texels 0..64, local ±32.
        let wide = one_cell(Some(0.5));
        assert!((wide.pos(0)[0] + 32.0).abs() < 0.01, "{:?}", wide.pos(0));
        assert!((wide.pos(0)[1] - 32.0).abs() < 0.01, "{:?}", wide.pos(0));
    }

    #[test]
    fn automesh_knobs_past_their_bounds_are_refused() {
        let alpha = square_mask();
        let uv_map = UvMap::from_texture_size(64.0, 64.0);
        let grid = |knobs: GridKnobs| grid_automesh(&alpha, &knobs, &uv_map, [0.0, 0.0]);
        let limit = |resource, limit: u32, got: u64| {
            Err(MeshError::Limit(LoadLimitError {
                resource,
                limit: limit.into(),
                got,
            }))
        };

        assert!(
            grid(GridKnobs {
                cols: MAX_GRID_DIVISIONS,
                rows: 2,
                ..GridKnobs::default()
            })
            .is_ok()
        );
        assert_eq!(
            grid(GridKnobs {
                cols: u32::MAX,
                ..GridKnobs::default()
            })
            .map(drop),
            limit(
                LoadResource::GridDivisions,
                MAX_GRID_DIVISIONS,
                u32::MAX.into()
            )
        );
        assert_eq!(
            grid(GridKnobs {
                axes_y: vec![0.5; MAX_GRID_DIVISIONS as usize + 2],
                ..GridKnobs::default()
            })
            .map(drop),
            limit(
                LoadResource::GridLines,
                MAX_GRID_DIVISIONS + 1,
                u64::from(MAX_GRID_DIVISIONS) + 2
            )
        );
        // Named lines replace `rows`, so an unused count is not checked.
        assert!(
            grid(GridKnobs {
                rows: u32::MAX,
                axes_y: vec![0.0, 1.0],
                ..GridKnobs::default()
            })
            .is_ok()
        );

        assert_eq!(
            contour_automesh(
                &alpha,
                &ContourKnobs {
                    margin: u32::MAX,
                    ..ContourKnobs::default()
                },
                &uv_map,
                [0.0, 0.0],
            )
            .map(drop),
            limit(
                LoadResource::ContourMargin,
                MAX_CONTOUR_MARGIN,
                u32::MAX.into()
            )
        );
    }

    #[test]
    fn non_finite_positions_are_not_called_coincident() {
        let mut wm = WorkingMesh::from_mesh(&quad());
        assert_eq!(wm.add_vertex([f32::NAN, 0.0]), Err(MeshError::NotFinite));
        assert_eq!(
            wm.move_vertex(0, [0.0, f32::INFINITY]),
            Err(MeshError::NotFinite)
        );
    }

    #[test]
    fn simplifying_an_unbalanced_contour_keeps_a_small_stack() {
        // Each split lands next to an end, so a recursive split goes one
        // level deeper per point.
        let n = 5_000usize;
        let points: Vec<[f32; 2]> = (0..n)
            .map(|i| [i as f32, if i % 2 == 1 { (n - i) as f32 } else { 0.0 }])
            .collect();
        let kept = std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || dp(&points, 0.5).len())
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(kept, n, "every zigzag point survives");
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

        let mut new_mesh = quad();
        new_mesh.verts.extend_from_slice(&[0.0, 0.0]);
        new_mesh.uvs.extend_from_slice(&[0.5, 0.5]);
        new_mesh.indices = ClmIndices::U16(vec![0, 1, 4, 1, 2, 4, 2, 3, 4, 3, 0, 4]);
        m.set_mesh_with_refit(&part, new_mesh).unwrap();

        let b = m.binding(&key).unwrap();
        let cells = b.values().deform_cells().unwrap();
        assert_eq!(
            cells.len(),
            1,
            "refitting preserves the unauthored rest cell"
        );
        assert_eq!([cells[0].x, cells[0].y], [1, 0]);
        assert_eq!(cells[0].value.len(), 10);
        assert!((cells[0].value[8] - 10.0).abs() < 1e-4);
    }
}
