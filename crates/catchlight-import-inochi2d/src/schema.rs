use serde::{Deserialize, Deserializer};

/// Try to deserialize a value as `T`; if the JSON shape/type doesn't
/// match (e.g. an array where we expect a bool), yield `None` rather
/// than propagating the error. Lets the schema tolerate garbage fields
/// the same way the legacy JSON-walking loader did.
fn de_lenient<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
where
    T: for<'a> Deserialize<'a>,
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

fn de_lenient_vec<'de, T, D>(d: D) -> Result<Vec<T>, D::Error>
where
    T: for<'a> Deserialize<'a>,
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    match v {
        serde_json::Value::Array(arr) => Ok(arr
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect()),
        _ => Ok(serde_json::from_value(v).unwrap_or_default()),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaPuppetPhysics {
    #[serde(default, rename = "pixelsPerMeter")]
    pub(super) pixels_per_meter: Option<f32>,
    #[serde(default)]
    pub(super) gravity: Option<f32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaTransform {
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) trans: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) rot: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) scale: Vec<f32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaMask {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) source: Option<u32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) mode: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaMesh {
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) verts: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) uvs: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) indices: Vec<u32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) origin: Vec<f32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaNode {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) uuid: Option<u32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) name: Option<String>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) enabled: Option<bool>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) zsort: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) transform: Option<SchemaTransform>,
    #[serde(default, rename = "lockToRoot", deserialize_with = "de_lenient")]
    pub(super) lock_to_root: Option<bool>,
    #[serde(default, rename = "type", deserialize_with = "de_lenient")]
    pub(super) ty: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) textures: Vec<i64>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) mesh: Option<SchemaMesh>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) opacity: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) blend_mode: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) tint: Vec<f32>,
    #[serde(default, rename = "screenTint", deserialize_with = "de_lenient_vec")]
    pub(super) screen_tint: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) masks: Option<Vec<SchemaMask>>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) mask_threshold: Option<f32>,

    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) translate_children: Option<bool>,

    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) model_type: Option<String>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) map_mode: Option<String>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) local_only: Option<bool>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) param: Option<u32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) gravity: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) length: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) frequency: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) angle_damping: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) length_damping: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) output_scale: Vec<f32>,

    // The authored Composite flag the old runtime path hardcoded to true;
    // the `.clm` importer reads it.
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) propagate_meshgroup: Option<bool>,
}

/// Does this source binding kind (`param_name`) drive colour? Colour lands on
/// a part or a composite; a mesh group is never drawn, so both import paths
/// drop such a binding when its target is one.
pub(super) fn source_binding_is_color(kind: &str) -> bool {
    matches!(
        kind,
        "opacity"
            | "tint.r"
            | "tint.g"
            | "tint.b"
            | "screenTint.r"
            | "screenTint.g"
            | "screenTint.b"
    )
}

impl SchemaNode {
    /// A source mesh group's colour maps to nothing — catchlight never draws a
    /// mesh group, so it has no opacity, blend mode, tint or screen tint. Both
    /// import paths call this so the model's author can see what was dropped.
    pub(super) fn log_dropped_mesh_group_color(&self) {
        let opacity = self.opacity.filter(|o| *o != 1.0);
        let blend_mode = self
            .blend_mode
            .as_deref()
            .filter(|b| !b.is_empty() && *b != "Normal");
        let tint = self.tint.iter().any(|c| *c != 1.0).then_some(&self.tint);
        let screen_tint = self
            .screen_tint
            .iter()
            .any(|c| *c != 0.0)
            .then_some(&self.screen_tint);
        if opacity.is_none() && blend_mode.is_none() && tint.is_none() && screen_tint.is_none() {
            return;
        }
        tracing::debug!(
            "mesh group {:?}: dropping colour a mesh group cannot carry \
             (opacity {:?}, blend mode {:?}, tint {:?}, screen tint {:?})",
            self.name.as_deref().unwrap_or_default(),
            opacity,
            blend_mode,
            tint,
            screen_tint,
        );
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaParam {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) uuid: Option<u32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) name: Option<String>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) is_vec2: Option<bool>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) min: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) max: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) defaults: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) axis_points: Option<Vec<Vec<f32>>>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) bindings: Vec<SchemaBinding>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaBinding {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) node: Option<u32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) param_name: Option<String>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) interpolate_mode: Option<String>,
    #[serde(default)]
    pub(super) values: Option<serde_json::Value>,
    /// Authored-keypoint mask, `[x][y]` like `values` — which cells the rigger
    /// set vs. inochi's baked re-interpolation fill.
    #[serde(default, rename = "isSet", deserialize_with = "de_lenient")]
    pub(super) is_set: Option<Vec<Vec<bool>>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaAnimation {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) timestep: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) length: Option<i32>,
    #[serde(default, rename = "leadIn", deserialize_with = "de_lenient")]
    pub(super) lead_in: Option<i32>,
    #[serde(default, rename = "leadOut", deserialize_with = "de_lenient")]
    pub(super) lead_out: Option<i32>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) lanes: Vec<SchemaAnimationLane>,
}

/// One lane of a source clip. `uuid` names the *param* it drives and `target`
/// picks that param's axis — 0 is x, anything else is y — because a source
/// param may be 2-D where catchlight's are scalar.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaAnimationLane {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) uuid: Option<u32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) target: Option<u8>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) interpolation: Option<String>,
    #[serde(default, deserialize_with = "de_lenient_vec")]
    pub(super) keyframes: Vec<SchemaKeyframe>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaKeyframe {
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) frame: Option<i32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) value: Option<f32>,
}
