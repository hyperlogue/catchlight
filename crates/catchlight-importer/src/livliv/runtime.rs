// SPDX-License-Identifier: Apache-2.0
// Portions copyright 2026 LivLiv; adapted for import-time sampling.
//! Import-time pose evaluator derived from LivLiv Player's Apache-2.0
//! `src/runtime/puppet.ts` at UPSTREAM_REVISION (see THIRD-PARTY-NOTICES.md).
//! No evaluator is shipped in the .clm: bake.rs samples it into native bindings.
//! A sample is a settled pose: torso follow has reached its target, spring
//! displacement and wind are zero, and expressions have reached their target.
//! History-dependent motion cannot be inferred from a scalar pose.
use super::{Error, Result, ValidatedPackage};
use serde::Deserialize;

pub(super) const PARAMETERS: [(&str, f64, f64, f64); 13] = [
    ("angleX", -30., 30., 0.),
    ("angleY", -30., 30., 0.),
    ("angleZ", -30., 30., 0.),
    ("bodyX", -10., 10., 0.),
    ("bodyZ", -10., 10., 0.),
    ("breath", 0., 1., 0.),
    ("eyeLOpen", 0., 1., 1.),
    ("eyeROpen", 0., 1., 1.),
    ("eyeBallX", -1., 1., 0.),
    ("eyeBallY", -1., 1., 0.),
    ("browLY", -1., 1., 0.),
    ("browRY", -1., 1., 0.),
    ("mouthOpen", 0., 1., 0.),
];
#[derive(Clone, Copy, Debug)]
pub(super) struct Pose(pub [f64; 13], pub usize);
impl Default for Pose {
    fn default() -> Self {
        Self(PARAMETERS.map(|p| p.3), 0)
    }
}

#[derive(Deserialize, Clone)]
struct Ellipse {
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
}
#[derive(Deserialize)]
struct Anchors {
    head: Ellipse,
    face: [f64; 4],
    neck_y: f64,
    bottom_y: f64,
    #[serde(default)]
    brows: Vec<Option<[f64; 4]>>,
    #[serde(default)]
    brow_lines: Vec<Option<BrowLine>>,
    jaw: Option<Jaw>,
    chest: Option<Ellipse>,
}
#[derive(Deserialize)]
struct Jaw {
    top: f64,
    chin: f64,
}
#[derive(Deserialize)]
struct BrowLine {
    x: Vec<f64>,
    bottom: Vec<f64>,
    lid: Vec<f64>,
    cap: Option<Vec<f64>>,
}
#[derive(Deserialize)]
#[serde(default)]
struct Body {
    yaw: f64,
    roll: f64,
    breath: f64,
    bust: f64,
}
impl Default for Body {
    fn default() -> Self {
        Self {
            yaw: 0.35,
            roll: 0.3,
            breath: 1.,
            bust: 0.,
        }
    }
}
#[derive(Deserialize)]
#[serde(default)]
struct Rig {
    yaw: f64,
    pitch: f64,
    roll: f64,
    bulge: f64,
    pitch_ease: f64,
    bulge_shape: f64,
    bulge_extent: Option<Vec<f64>>,
    drift: [f64; 2],
    breath: f64,
    brow: f64,
    body: Body,
    neck: Option<Neck>,
}
impl Default for Rig {
    fn default() -> Self {
        Self {
            yaw: 0.27,
            pitch: 0.22,
            roll: 0.5,
            bulge: 0.6,
            pitch_ease: 0.25,
            bulge_shape: 1.,
            bulge_extent: None,
            drift: [0.12, 0.08],
            breath: 1.,
            brow: 0.06,
            body: Body::default(),
            neck: None,
        }
    }
}
#[derive(Deserialize)]
struct Neck {
    twist: f64,
    #[serde(default = "neck_reach")]
    reach: f64,
}
fn neck_reach() -> f64 {
    0.45
}
#[derive(Deserialize)]
#[serde(default)]
struct Tuning {
    turn: f64,
    nod: f64,
    breath: f64,
}
impl Default for Tuning {
    fn default() -> Self {
        Self {
            turn: 1.,
            nod: 1.,
            breath: 1.,
        }
    }
}

pub(super) struct Runtime {
    anchors: Anchors,
    rig: Rig,
    tuning: Tuning,
    brows_layer: bool,
    pub layers: Vec<MotionLayer>,
    pub mouth_id: Option<String>,
    pub frame_jaw: Vec<f64>,
    frame_kind: Vec<String>,
    keyforms: Option<Keyforms>,
}
#[derive(Deserialize)]
pub(super) struct MotionLayer {
    id: String,
    group: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    depth: f64,
    pivot: Option<[f64; 2]>,
    range: Option<[f64; 2]>,
    rigid: Option<[f64; 2]>,
    fall: Option<f64>,
    fall_end: Option<f64>,
    y: f64,
    h: f64,
    #[serde(default)]
    visemes: Vec<String>,
}
#[derive(Deserialize)]
struct Keyforms {
    cell: f64,
    cols: usize,
    rows: usize,
    units: f64,
    turn: f64,
    nod: f64,
    #[serde(default)]
    keys: Vec<Key>,
    interp: Option<String>,
    #[serde(default)]
    mask: std::collections::BTreeMap<String, String>,
}
#[derive(Deserialize)]
struct Key {
    param: String,
    value: f64,
    fields: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    same: Vec<String>,
}
struct SampledField {
    values: Vec<f64>,
    mask: Option<Vec<f64>>,
}

/// Position-only weights are measured on undeformed vertices, just as upstream.
pub(super) struct Vertex {
    pub rest: [f64; 2],
    head: f64,
    depth: f64,
    neck: f64,
    twist: f64,
    brow: Option<(usize, f64, f64)>,
    jaw: f64,
    keys: Vec<[f64; 2]>,
}

impl Runtime {
    pub fn new(package: &ValidatedPackage) -> Result<Self> {
        let m = &package.manifest;
        let anchors: Anchors = serde_json::from_value(m["anchors"].clone())?;
        let rig = serde_json::from_value(
            m.get("rig")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )?;
        let tuning = serde_json::from_value(
            m.get("tuning")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )?;
        let layers: Vec<MotionLayer> = serde_json::from_value(m["layers"].clone())?;
        for e in std::iter::once(&anchors.head).chain(anchors.chest.iter()) {
            if e.rx <= 0. || e.ry <= 0. {
                return Err(Error::Invalid(
                    "motion anchors require positive ellipse radii".into(),
                ));
            }
        }
        if anchors.bottom_y <= anchors.neck_y {
            return Err(Error::Invalid(
                "motion anchors require bottom_y > neck_y".into(),
            ));
        }
        for line in anchors.brow_lines.iter().flatten() {
            if line.x.is_empty()
                || line.x.windows(2).any(|p| p[0] >= p[1])
                || line.bottom.len() != line.x.len()
                || line.lid.len() != line.x.len()
                || line.cap.as_ref().is_some_and(|c| c.len() != line.x.len())
            {
                return Err(Error::Invalid(
                    "brow line arrays must pair on an increasing x axis".into(),
                ));
            }
        }
        let keyforms: Option<Keyforms> = package
            .model
            .files
            .get("keyforms")
            .map(|path| -> Result<_> { Ok(serde_json::from_slice(package.resource(path)?)?) })
            .transpose()?;
        if let Some(k) = &keyforms {
            if k.turn <= 0.
                || k.nod <= 0.
                || k.keys
                    .iter()
                    .any(|key| key.param != "diag" && key.value == 0.)
            {
                return Err(Error::Invalid(
                    "keyforms require positive turn/nod and nonzero key values for baking".into(),
                ));
            }
        }
        let mouth = layers.iter().find(|l| l.role == "mouth");
        let mouth_id = mouth.map(|l| l.id.clone());
        let frame_kind = mouth.map_or_else(Vec::new, |l| l.visemes.clone());
        let jaw = &m["anchors"]["jaw"];
        let mut frame_jaw: Vec<f64> = frame_kind
            .iter()
            .map(|kind| {
                if kind == "closed" || (kind == "half" && jaw["open"].as_f64().unwrap_or(0.) <= 0.)
                {
                    0.
                } else {
                    jaw[kind].as_f64().unwrap_or(0.)
                }
            })
            .collect();
        if anchors.jaw.is_some() && !frame_jaw.iter().any(|&v| v > 0.) {
            let amount =
                m["rig"]["jaw"].as_f64().unwrap_or(0.015) * (anchors.face[3] - anchors.face[1]);
            frame_jaw = frame_kind
                .iter()
                .map(|kind| {
                    amount
                        * match kind.as_str() {
                            "half" => 0.45,
                            "open" => 1.,
                            "round" => 0.7,
                            "wide" => 0.35,
                            _ => 0.,
                        }
                })
                .collect();
        }
        Ok(Self {
            anchors,
            rig,
            tuning,
            brows_layer: layers.iter().any(|l| l.id == "brows"),
            layers,
            keyforms,
            mouth_id,
            frame_jaw,
            frame_kind,
        })
    }
    pub fn controls(&self, layer: usize, vertices: &[Vertex]) -> Vec<usize> {
        let l = &self.layers[layer];
        let mut controls = vec![0, 1, 2, 3, 4, 5];
        if l.pivot.is_some()
            && (l.role.starts_with("eye_")
                || l.role.starts_with("iris_")
                || l.role.starts_with("shine_"))
        {
            controls.push(if l.role.ends_with("_L") { 6 } else { 7 });
            if l.role.starts_with("iris_") && l.range.is_some() {
                controls.extend([8, 9]);
            }
        }
        for side in 0..2 {
            if vertices.iter().any(|v| v.brow.is_some_and(|b| b.0 == side)) {
                controls.push(10 + side);
            }
        }
        controls
    }
    pub fn mouth_moves(&self, layer: usize, vertices: &[Vertex]) -> bool {
        self.layers[layer].role == "mouth" || vertices.iter().any(|v| v.jaw > 0.)
    }
    pub fn prepare(&self, layer: usize, points: &[[f64; 2]]) -> Result<Vec<Vertex>> {
        use base64::Engine;
        let l = &self.layers[layer];
        // Decode each applicable field once per layer, never once per sample.
        let fields: Vec<Option<SampledField>> = if let Some(k) = &self.keyforms {
            k.keys
                .iter()
                .map(|key| -> Result<_> {
                    let has =
                        |g: &str| key.fields.contains_key(g) || key.same.iter().any(|v| v == g);
                    let group = if key.fields.contains_key(&l.id) {
                        l.id.as_str()
                    } else if l.group == "head" {
                        "head"
                    } else if l.group.starts_with("hair") {
                        "hair"
                    } else if l.group == "body" && has("torso") {
                        "torso"
                    } else {
                        l.id.as_str()
                    };
                    let field = key.fields.get(group).or_else(|| {
                        if key.same.iter().any(|s| s == group) {
                            key.fields.get("head")
                        } else {
                            None
                        }
                    });
                    let Some(field) = field else { return Ok(None) };
                    let decode = |s: &str| {
                        base64::engine::general_purpose::STANDARD
                            .decode(s)
                            .map_err(|e| Error::Invalid(e.to_string()))
                    };
                    let bytes = decode(field)?;
                    let data = bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|p| i16::from_le_bytes([p[0], p[1]]) as f64)
                        .collect();
                    let mask = k
                        .mask
                        .get(group)
                        .map(|s| {
                            decode(s).map(|b| b.into_iter().map(|x| x as f64 / 255.).collect())
                        })
                        .transpose()?;
                    Ok(Some(SampledField { values: data, mask }))
                })
                .collect::<Result<_>>()?
        } else {
            vec![]
        };
        points
            .iter()
            .map(|&point| {
                let mut v = self.weights(l, point);
                if let Some(k) = &self.keyforms {
                    for (key, field) in k.keys.iter().zip(&fields) {
                        let offset = if let Some(SampledField {
                            values: field,
                            mask,
                        }) = field
                        {
                            let mut p = Pose::default();
                            if key.param == "angleX" {
                                p.0[0] = key.value
                            } else if key.param == "angleY" {
                                p.0[1] = key.value
                            }
                            let own = self.rig_position(layer, &v, p);
                            let dx = sample(field, k, point, 2, 0) * k.units;
                            let dy = sample(field, k, point, 2, 1) * k.units;
                            let w = mask
                                .as_ref()
                                .map_or(1., |mask| sample(mask, k, point, 1, 0));
                            [(point[0] + dx - own[0]) * w, (point[1] + dy - own[1]) * w]
                        } else {
                            [0., 0.]
                        };
                        v.keys.push(offset);
                    }
                }
                Ok(v)
            })
            .collect()
    }
    fn weights(&self, l: &MotionLayer, rest: [f64; 2]) -> Vertex {
        let [x, y] = rest;
        let a = &self.anchors;
        let h = &a.head;
        let fw = a.face[2] - a.face[0];
        let fh = a.face[3] - a.face[1];
        let radius = ((x - h.cx) / h.rx).hypot((y - h.cy) / h.ry);
        let depth = if l.group == "head" {
            l.depth
        } else {
            l.depth
                * smooth(
                    l.fall_end.unwrap_or(1.),
                    if l.group == "hair_front" {
                        l.fall.unwrap_or(0.55)
                    } else if l.fall_end.is_none() {
                        0.7
                    } else {
                        l.fall.unwrap_or(0.7)
                    },
                    radius,
                )
        };
        let long_head = 1. - smooth(a.neck_y + 0.05 * h.ry, a.neck_y + 1.1 * h.ry, y);
        let f = smooth(0.95, 0.7, radius)
            * (1. - smooth(a.neck_y - 0.2 * h.ry, a.neck_y + 0.3 * h.ry, y));
        let mut jaw_weight = 0.;
        let mut head = match l.group.as_str() {
            "head" => 1.,
            "body" => 0.,
            "hair_inner" => (f + (1. - f) * long_head) * 0.5,
            _ => f + (1. - f) * long_head,
        };
        if let Some(j) = &a.jaw {
            let hair_front = l.group == "hair_side" || l.group == "hair_front";
            if self.frame_jaw.iter().any(|&v| v > 0.)
                && (l.id == "face"
                    || l.role == "expr"
                    || hair_front
                    || l.role == "mouth"
                    || l.role == "neck_shadow"
                    || l.group == "body")
            {
                jaw_weight = smooth(j.top - 0.02 * h.ry, j.chin, y)
                    * (1. - smooth(0.22 * fw, 0.5 * fw, (x - h.cx).abs()));
                if hair_front {
                    jaw_weight *= 1. - smooth(j.chin, j.chin + 0.15 * h.ry, y);
                }
            }
            if l.role == "neck_shadow" {
                head = 1. - smooth(j.chin, j.chin + 0.3 * fh, y);
            }
            if l.group == "body" {
                head = (1. - smooth(j.chin, j.chin + 0.08 * fh, y))
                    * (1. - smooth(0.2 * fw, 0.34 * fw, (x - h.cx).abs()));
            }
        }
        if l.group == "body" || l.role == "neck_shadow" {
            jaw_weight *= head;
        }
        let neck = if l.group == "body" {
            (0.2 * (1. - smooth(a.neck_y, a.neck_y + 0.6 * fh, y))
                * (1. - smooth(0.6 * fw, 1.3 * fw, (x - h.cx).abs()))
                - head)
                .max(0.)
        } else {
            0.
        };
        let twist = if l.group == "body" {
            self.rig.neck.as_ref().map_or(0., |n| {
                let cy = a.jaw.as_ref().map_or(a.neck_y, |j| j.chin);
                (n.twist
                    * (1. - smooth(cy, cy + n.reach * fh, y))
                    * (1. - smooth(0.2 * fw, 0.34 * fw, (x - h.cx).abs()))
                    - head)
                    .max(0.)
            })
        } else {
            0.
        };
        let mut brow = None;
        if l.group == "head"
            && !l.role.starts_with("iris_")
            && (!self.brows_layer || l.id == "brows")
        {
            for (side, (b, line)) in a.brows.iter().zip(&a.brow_lines).enumerate() {
                if let (Some(b), Some(line)) = (b, line) {
                    let w = brow_weight(b, line, x, y);
                    if w > brow.map_or(0., |(_, w, _)| w) {
                        brow = Some((
                            side,
                            w,
                            line.cap
                                .as_ref()
                                .map_or(f64::INFINITY, |cap| interp(&line.x, cap, x)),
                        ));
                    }
                }
            }
        }
        Vertex {
            rest,
            head,
            depth,
            neck,
            twist,
            brow,
            jaw: jaw_weight,
            keys: vec![],
        }
    }
    pub fn position(&self, layer: usize, v: &Vertex, p: Pose) -> [f64; 2] {
        let mut out = self.rig_position(layer, v, p);
        if let Some(k) = &self.keyforms {
            let weights: Vec<_> = k
                .keys
                .iter()
                .map(|key| {
                    if key.param == "diag" {
                        return 0.;
                    }
                    let (value, nod) = match key.param.as_str() {
                        "angleX" => (p.0[0], false),
                        "angleY" => (p.0[1], true),
                        _ => (0., false),
                    };
                    (value * self.tuning.turn * if nod { self.tuning.nod } else { 1. }
                        / (k.turn * if nod { k.nod } else { 1. })
                        / key.value)
                        .clamp(0., 2.)
                })
                .collect();
            let mut eased = weights.clone();
            if k.interp.as_deref() == Some("hermite") {
                for (i, (key, &t)) in k.keys.iter().zip(&weights).enumerate() {
                    if t > 0. && t < 1. && key.param != "diag" {
                        if let Some(j) = k
                            .keys
                            .iter()
                            .position(|other| other.param == key.param && other.value == -key.value)
                        {
                            let h10 = t * t * t - 2. * t * t + t;
                            eased[i] = 3. * t * t - 2. * t * t * t + 0.5 * h10 + t * t * t - t * t;
                            eased[j] -= 0.5 * h10;
                        }
                    }
                }
            }
            let mut d = [0., 0.];
            for (w, k) in eased.iter().zip(&v.keys) {
                d[0] += w * k[0];
                d[1] += w * k[1];
            }
            let angle = p.0[2].to_radians() * self.rig.roll * self.tuning.turn * v.head;
            out[0] += d[0] * angle.cos() - d[1] * angle.sin();
            out[1] += d[0] * angle.sin() + d[1] * angle.cos();
        }
        out
    }
    fn rig_position(&self, layer: usize, v: &Vertex, p: Pose) -> [f64; 2] {
        let l = &self.layers[layer];
        let a = &self.anchors;
        let h = &a.head;
        let [mut x, mut y] = v.rest;
        let side = if l.role.ends_with("_L") {
            Some(0)
        } else if l.role.ends_with("_R") {
            Some(1)
        } else {
            None
        };
        if l.role.starts_with("eye_") || l.role.starts_with("iris_") || l.role.starts_with("shine_")
        {
            if let Some(pivot) = l.pivot {
                let open = side.map_or(1., |s| p.0[6 + s]);
                let squash = 0.12 + 0.88 * open;
                let iris = l.role.starts_with("iris_");
                let lid_iris = iris && l.range.is_some_and(|r| r[0] > 0. || r[1] > 0.);
                if iris {
                    if let Some(range) = l.range {
                        x += p.0[8] * range[0];
                        y -= p.0[9] * range[1];
                    }
                }
                if lid_iris {
                    y += 0.3 * (1. - squash) * (pivot[1] - (l.y + l.h / 2.));
                }
                if !lid_iris || y > pivot[1] {
                    y = pivot[1] + (y - pivot[1]) * squash;
                }
            }
        }
        if let Some((side, w, cap)) = v.brow {
            let mut up = h.ry * self.rig.brow;
            let mut down = up;
            for (b, line) in a.brows.iter().zip(&a.brow_lines) {
                if let (Some(b), Some(line)) = (b, line) {
                    up = up.min(0.6 * ((b[3] - b[1]) * 1.6 + 6.));
                    for (i, &x) in line.x.iter().enumerate() {
                        down =
                            down.min(0.6 * (line.lid[i] - line.bottom[i]) / brow_x(b, x).max(1e-3));
                    }
                }
            }
            let lift = p.0[10 + side.min(1)] * if p.0[10 + side.min(1)] > 0. { up } else { down };
            y -= lift.signum() * lift.abs().min(cap) * w;
        }
        let frame = p.1.min(self.frame_jaw.len().saturating_sub(1));
        let kind = self.frame_kind.get(frame).map_or("closed", String::as_str);
        let ease = match kind {
            "half" => smooth(0.12, 0.5, p.0[12]),
            "open" => smooth(0.5, 1., p.0[12]),
            _ => smooth(0.12, 1., p.0[12]),
        };
        let drop = self.frame_jaw.get(frame).copied().unwrap_or(0.)
            * if kind == "half" {
                0.6 + 0.4 * ease
            } else {
                0.75 + 0.25 * ease
            };
        y += drop * v.jaw;
        let neck_body = self.body_offset(h.cx, a.neck_y, p);
        let roll = p.0[2].to_radians() * self.rig.roll * self.tuning.turn;
        let mut out = if let Some(rigid) = l.rigid {
            let root = self.head(rigid[0], rigid[1], l.depth, p);
            let dx = x - rigid[0];
            let dy = y - rigid[1];
            [
                root[0] + neck_body[0] + dx * roll.cos() - dy * roll.sin(),
                root[1] + neck_body[1] + dx * roll.sin() + dy * roll.cos(),
            ]
        } else {
            let b = self.body_offset(x, y, p);
            let hp = self.head(x, y, v.depth, p);
            [
                (x + b[0]) * (1. - v.head) + (hp[0] + neck_body[0]) * v.head,
                (y + b[1]) * (1. - v.head) + (hp[1] + neck_body[1]) * v.head,
            ]
        };
        if v.twist > 0. {
            let cy = a.jaw.as_ref().map_or(a.neck_y, |j| j.chin);
            out[0] += (self.head(h.cx, cy, 0., p)[0] + neck_body[0]
                - (h.cx + self.body_offset(h.cx, cy, p)[0]))
                * v.twist;
        }
        if v.neck > 0. {
            let nx = h.cx + neck_body[0];
            let ny = a.neck_y + neck_body[1];
            let dx = out[0] - nx;
            let dy = out[1] - ny;
            let angle = roll * v.neck;
            out = [
                nx + dx * angle.cos() - dy * angle.sin(),
                ny + dx * angle.sin() + dy * angle.cos(),
            ];
        }
        if l.role == "mouth" && frame > 0 && kind != "shut" {
            let scale = if kind == "half" {
                0.88 + 0.12 * ease
            } else {
                0.9 + 0.1 * ease
            };
            out[1] += (v.rest[1] - l.pivot.map_or(l.y, |p| p[1])) * (scale - 1.) * (1. - v.jaw);
        }
        out
    }
    pub fn head(&self, x: f64, y: f64, depth: f64, p: Pose) -> [f64; 2] {
        let h = &self.anchors.head;
        let r = &self.rig;
        let u = (x - h.cx) / h.rx;
        let v = (y - h.cy) / h.ry;
        let (bu, bv) = if let Some(e) = &r.bulge_extent {
            let mut ex = e[0];
            if e.len() > 3 && v > 0. {
                let t = (v / (e[2] * e.get(4).copied().unwrap_or(1.))).min(1.);
                ex += (e[3] - e[0]) * t * t * (3. - 2. * t);
            }
            (u / ex, v / if v < 0. { e[1] } else { e[2] })
        } else {
            (u, v)
        };
        let bulge = r.bulge * (1. - (bu * bu + bv * bv).powf(r.bulge_shape)).max(0.);
        let z = bulge + depth;
        let yaw = p.0[0] / 30. * r.yaw * self.tuning.turn;
        let py = p.0[1] / 30.;
        let pitch =
            py * (1. - r.pitch_ease * py * py) * r.pitch * self.tuning.turn * self.tuning.nod;
        let roll = p.0[2].to_radians() * r.roll * self.tuning.turn;
        let u2 = u * yaw.cos() + z * yaw.sin() + r.drift[0] * yaw.sin();
        let z2 = -u * yaw.sin() + z * yaw.cos();
        let v2 = v * pitch.cos() - z2 * pitch.sin() - r.drift[1] * pitch.sin();
        let dx = u2 * h.rx;
        let dy = h.cy + v2 * h.ry - self.anchors.neck_y;
        [
            h.cx + dx * roll.cos() - dy * roll.sin(),
            self.anchors.neck_y + dx * roll.sin() + dy * roll.cos(),
        ]
    }
    pub fn body_offset(&self, x: f64, y: f64, p: Pose) -> [f64; 2] {
        let a = &self.anchors;
        let h = &a.head;
        let b = &self.rig.body;
        let t = smooth(a.bottom_y, a.neck_y, y);
        let br = p.0[5] * self.tuning.breath * self.rig.breath * b.breath;
        let mut dx = (x - h.cx) * 0.01 * br * t;
        let mut dy = -br * 0.024 * h.ry * t.powf(1.3);
        let yaw = (p.0[3] + p.0[0] * b.yaw * self.tuning.turn) / 30.;
        let bw = 1.4 * (a.face[2] - a.face[0]);
        let u = ((x - h.cx) / bw).clamp(-1.6, 1.6);
        dx += yaw * bw * 0.16 * t.powf(1.2) * (1. - 0.18 * u * u)
            + yaw * (x - h.cx) * -0.04 * u.signum() * t;
        let angle = (p.0[4] + p.0[2] * b.roll * self.tuning.turn).to_radians() * t;
        let px = h.cx;
        let py = a.bottom_y + 0.4 * (a.bottom_y - a.neck_y);
        let rx = x - px;
        let ry = y - py;
        dx += rx * (angle.cos() - 1.) - ry * angle.sin();
        dy += rx * angle.sin() + ry * (angle.cos() - 1.);
        if let Some(c) = &a.chest {
            let d = ((x - c.cx) / c.rx).hypot((y - c.cy) / c.ry);
            let w = 1. - smooth(0.2, 1.15, d);
            dy -= br * 0.012 * h.ry * w * (0.4 + b.bust);
        }
        [dx, dy]
    }
}
pub(super) fn smooth(a: f64, b: f64, x: f64) -> f64 {
    let t = if a == b {
        if x >= b {
            1.
        } else {
            0.
        }
    } else {
        ((x - a) / (b - a)).clamp(0., 1.)
    };
    t * t * (3. - 2. * t)
}
fn interp(xs: &[f64], ys: &[f64], x: f64) -> f64 {
    if x <= xs[0] {
        return ys[0];
    }
    for i in 1..xs.len() {
        if x <= xs[i] {
            return ys[i - 1] + (ys[i] - ys[i - 1]) * (x - xs[i - 1]) / (xs[i] - xs[i - 1]);
        }
    }
    ys[ys.len() - 1]
}
fn brow_x(b: &[f64; 4], x: f64) -> f64 {
    1. - smooth(0., (b[2] - b[0]) * 0.45, (b[0] - x).max(x - b[2]).max(0.))
}
fn brow_weight(b: &[f64; 4], l: &BrowLine, x: f64, y: f64) -> f64 {
    let wx = brow_x(b, x);
    if y < b[1] {
        return wx * (1. - smooth(0., (b[3] - b[1]) * 1.6 + 6., b[1] - y));
    }
    let bottom = interp(&l.x, &l.bottom, x);
    if y <= bottom {
        wx
    } else {
        wx * (1. - smooth(bottom, interp(&l.x, &l.lid, x), y))
    }
}
fn sample(f: &[f64], k: &Keyforms, p: [f64; 2], stride: usize, component: usize) -> f64 {
    let gx = (p[0] / k.cell).clamp(0., k.cols as f64 - 1.001);
    let gy = (p[1] / k.cell).clamp(0., k.rows as f64 - 1.001);
    let c = gx as usize;
    let r = gy as usize;
    let tx = gx - c as f64;
    let ty = gy - r as f64;
    let at = |c, r| f[stride * (r * k.cols + c) + component];
    (at(c, r) * (1. - tx) + at(c + 1, r) * tx) * (1. - ty)
        + (at(c, r + 1) * (1. - tx) + at(c + 1, r + 1) * tx) * ty
}
