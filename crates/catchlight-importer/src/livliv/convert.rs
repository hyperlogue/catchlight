//! Format conversion. Coordinates are centred on the canvas and Y is reflected
//! exactly once; UVs stay top-left image coordinates. Higher layer indices get
//! higher z order. Texture switches become parts controlled by a discrete param.
use std::collections::BTreeMap;
use std::io::Cursor;

use catchlight_core::components::{BlendMode, MaskMode};
use catchlight_core::formats::clm::*;
use catchlight_core::interpolate::InterpolateMode;
use catchlight_core::{Model, NodeId, ParamId, TexId};
use serde::Serialize;
use serde_json::json;

use super::{decode_image, Error, Layer, Result, ValidatedPackage, UPSTREAM_REVISION};

/// Machine-readable findings, also stored in the resulting model extension.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub upstream_revision: &'static str,
    pub source_version: u32,
    pub mappings: Vec<String>,
    pub losses: Vec<String>,
    pub blockers: Vec<String>,
}

/// Explicit acknowledgement is required for an approximate conversion.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    pub allow_lossy: bool,
}

/// Model and report returned together so callers cannot mistake conversion for
/// a fidelity guarantee. Rights and findings also survive save/load in .clm.
pub struct Conversion {
    pub model: Model,
    pub report: Report,
}

impl ValidatedPackage {
    /// Inspect conversion feasibility without producing a model. Watermarks
    /// need output-space rendering; metadata alone cannot preserve them.
    pub fn report(&self) -> Report {
        let mut report = Report {
            upstream_revision: UPSTREAM_REVISION,
            source_version: 4,
            mappings: vec![
                "Canvas-centred Y-up layer geometry, authored drawing order and straight-alpha textures".into(),
                "Mouth frames exposed by a discrete mouthFrame param; expressions by expression/<id> params".into(),
                "Original manifest, author, license and compatibility report retained in livliv.import".into(),
                "Iris clipping uses the parent eye mask at alpha threshold 0.5".into(),
            ],
            losses: vec![
                "Procedural head turns, nod, roll, parallax, body movement, breathing, brow movement and automatic idle actions are not converted; geometry is the authored flat pose".into(),
                "Head/hair/independent-chain springs are not converted; no trajectory equivalence is claimed".into(),
                "Mouth loudness/vowel hysteresis, jaw deformation, talk latch and expression talk texture switching are not converted; use mouthFrame to select a drawing".into(),
                "Expression opacity easing is sampled at 17 positions; multi-expression show/hide/blush and redrawn-brow suppression are not converted".into(),
                "Iris soft clipping is approximated with a binary mask. Blink/lid deformation and gaze are not converted; eye patches remain in their authored open pose".into(),
            ],
            blockers: Vec::new(),
        };
        let mark = &self.manifest["meta"]["watermark"];
        if !mark.is_null() && mark["kind"] != "none" {
            report.blockers.push(format!("watermark {} requires screen-space output rendering, which Catchlight does not implement", mark["kind"]));
        }
        if self.model.files.contains_key("keyforms") {
            report.losses.push("keyforms are validated but not baked; group displacement fields, masks and Hermite interpolation are not applied".into());
        }
        for (i, layer) in self.model.layers.iter().enumerate() {
            let raw = &self.manifest["layers"][i];
            let mut omitted = Vec::new();
            for key in [
                "talk", "brow_tex", "brows", "bones", "rigid", "blush", "show", "hide", "depth",
                "pivot", "range", "fall", "fall_end",
            ] {
                if raw.get(key).is_some() {
                    omitted.push(key);
                }
            }
            if !omitted.is_empty() {
                report.losses.push(format!(
                    "layer {}: unconverted fields {}",
                    layer.id,
                    omitted.join(", ")
                ));
            }
            if let Some(role) = &layer.role {
                if !matches!(
                    role.as_str(),
                    "mouth"
                        | "expr"
                        | "expr_brows"
                        | "lid_L"
                        | "lid_R"
                        | "blush"
                        | "hole"
                        | "edge"
                        | "edge_wide"
                ) {
                    report.losses.push(format!(
                        "layer {}: role {role} is drawn as a plain layer",
                        layer.id
                    ));
                }
            }
        }
        report
    }

    /// Convert the supported subset. Refuse required rendering behavior rather
    /// than issue a file that loses it. Other approximations require opt-in.
    pub fn convert(&self, options: Options) -> Result<Conversion> {
        let report = self.report();
        if !report.blockers.is_empty() {
            return Err(Error::Invalid(report.blockers.join("; ")));
        }
        if !options.allow_lossy && !report.losses.is_empty() {
            return Err(Error::Invalid(
                "conversion is lossy; inspect report() and explicitly set allow_lossy".into(),
            ));
        }
        let mut file = ClmFile {
            doc: ClmStructure::default(),
            textures: Vec::new(),
            extensions: Vec::new(),
        };
        let root = NodeId::new("root")?;
        file.doc.nodes.push(node(
            root.clone(),
            None,
            self.name().into(),
            0.0,
            ClmNodeKind::Group,
        ));
        let mut textures: BTreeMap<String, TexId> = BTreeMap::new();
        for expr in &self.model.expressions {
            file.doc.params.push(ClmParam {
                id: ParamId::new(format!("expression/{}", expr.id))?,
                name: expr.id.clone(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
            });
        }
        for (i, layer) in self.model.layers.iter().enumerate() {
            let is_mouth = layer.role.as_deref() == Some("mouth");
            let paths: Vec<&str> = if is_mouth {
                layer.frames.iter().map(String::as_str).collect()
            } else {
                vec![&layer.texture]
            };
            let mouth_param = ParamId::new(format!("mouthFrame/{}", layer.id))?;
            if is_mouth {
                file.doc.params.push(ClmParam {
                    id: mouth_param.clone(),
                    name: format!("{} ({})", layer.id, layer.visemes.join(", ")),
                    min: 0.0,
                    max: (paths.len() - 1) as f32,
                    default: 0.0,
                });
            }
            let mask_path = layer.mask.as_deref();
            let mut paths = paths;
            if let Some(mask) = mask_path {
                paths.push(mask);
            }
            for (frame, path) in paths.iter().enumerate() {
                let is_mask = mask_path.is_some() && frame == paths.len() - 1;
                let tex_id = if let Some(id) = textures.get(*path) {
                    id.clone()
                } else {
                    let id = TexId::new(format!("tex-{}", textures.len()))?;
                    let bytes = self.package.get(path)?;
                    let data = if path.ends_with(".png") {
                        bytes.to_vec()
                    } else {
                        let image = decode_image(bytes, path, (layer.w, layer.h))?;
                        let mut png = Cursor::new(Vec::new());
                        image.write_to(&mut png, image::ImageFormat::Png)?;
                        png.into_inner()
                    };
                    file.textures.push(ClmTexture {
                        id: id.clone(),
                        encoding: TextureEncoding::Png,
                        alpha: TextureAlpha::Straight,
                        data,
                    });
                    textures.insert((*path).to_owned(), id.clone());
                    id
                };
                let id = NodeId::new(if is_mask {
                    format!("layer-{i}/mask")
                } else {
                    format!("layer-{i}/frame-{frame}")
                })?;
                let opacity = if is_mask {
                    0.0
                } else if is_mouth || matches!(layer.role.as_deref(), Some("expr" | "expr_brows")) {
                    1.0
                } else {
                    match layer.role.as_deref() {
                        Some(
                            "expr" | "expr_brows" | "lid_L" | "lid_R" | "blush" | "hole" | "edge"
                            | "edge_wide",
                        ) => 0.0,
                        _ if layer.show.is_empty() && frame == 0 => 1.0,
                        _ => 0.0,
                    }
                };
                let masks = if !is_mask {
                    layer
                        .clip
                        .as_ref()
                        .and_then(|clip| {
                            self.model
                                .layers
                                .iter()
                                .enumerate()
                                .find(|(_, l)| &l.id == clip && l.mask.is_some())
                        })
                        .map(|(index, _)| -> Result<ClmMask> {
                            Ok(ClmMask {
                                source: NodeId::new(format!("layer-{index}/mask"))?,
                                mode: MaskMode::Mask,
                            })
                        })
                        .transpose()?
                        .into_iter()
                        .collect()
                } else {
                    Vec::new()
                };
                let part = ClmPart {
                    mesh: mesh(layer, self.model.canvas),
                    albedo: Some(tex_id),
                    opacity,
                    blend_mode: BlendMode::Normal,
                    tint: [1.0; 3],
                    screen_tint: [0.0; 3],
                    masks,
                    mask_threshold: 0.5,
                    slots: Vec::new(),
                };
                file.doc.nodes.push(node(
                    id.clone(),
                    Some(root.clone()),
                    layer.id.clone(),
                    i as f32,
                    ClmNodeKind::Part(part),
                ));
                if is_mask {
                    continue;
                }
                if is_mouth {
                    let last = layer.frames.len() - 1;
                    opacity_binding(
                        &mut file.doc,
                        id.clone(),
                        mouth_param.clone(),
                        (0..=last).map(|n| n as f32 / last as f32).collect(),
                        (0..=last)
                            .map(|n| if n == frame { 1.0 } else { 0.0 })
                            .collect(),
                        InterpolateMode::Nearest,
                    );
                } else if matches!(layer.role.as_deref(), Some("expr" | "expr_brows")) {
                    if let Some(expr) = &layer.expression {
                        let axis: Vec<_> = (0..=16).map(|n| n as f32 / 16.0).collect();
                        let values = axis
                            .iter()
                            .map(|x| {
                                let t = ((x - 0.3) / 0.4).clamp(0.0, 1.0);
                                t * t * (3.0 - 2.0 * t)
                            })
                            .collect();
                        opacity_binding(
                            &mut file.doc,
                            id,
                            ParamId::new(format!("expression/{expr}"))?,
                            axis,
                            values,
                            InterpolateMode::Linear,
                        );
                    }
                }
            }
        }
        file.doc.extensions.insert(
            catchlight_core::id::ExtensionKey::new("livliv.import")?,
            ClmExtension::Json(json!({"manifest": self.manifest, "report": report})),
        );
        let model = Model::from_clm_file(&file)?;
        Ok(Conversion { model, report })
    }
}

fn node(
    id: NodeId,
    parent: Option<NodeId>,
    name: String,
    z_order: f32,
    kind: ClmNodeKind,
) -> ClmNode {
    ClmNode {
        id,
        parent,
        name,
        enabled: true,
        z_order,
        transform: ClmTransform::default(),
        lock_to_root: false,
        kind,
    }
}

fn mesh(l: &Layer, canvas: [u32; 2]) -> ClmMesh {
    let x = l.x as f32 - canvas[0] as f32 / 2.0;
    let y = canvas[1] as f32 / 2.0 - l.y as f32;
    let w = l.w as f32;
    let h = l.h as f32;
    ClmMesh {
        verts: vec![x, y, x + w, y, x, y - h, x + w, y - h],
        uvs: vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
        indices: ClmIndices::U16(vec![0, 2, 1, 1, 2, 3]),
        origin: [0.0, 0.0],
    }
}

fn opacity_binding(
    doc: &mut ClmStructure,
    node: NodeId,
    param: ParamId,
    axis: Vec<f32>,
    values: Vec<f32>,
    interpolate_mode: InterpolateMode,
) {
    doc.bindings.push(ClmBinding {
        node,
        params: vec![param],
        key_positions: vec![axis],
        interpolate_mode,
        values: ClmBindingValues::Opacity(ClmCells {
            cells: values
                .into_iter()
                .enumerate()
                .map(|(x, value)| ClmCell {
                    x: x as u32,
                    y: 0,
                    value,
                })
                .collect(),
        }),
    });
}
