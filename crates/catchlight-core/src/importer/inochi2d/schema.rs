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
    pub(super) children: Vec<serde_json::Value>,

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
    pub(super) dynamic_deformation: Option<bool>,
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

    // The authored Composite flag the Puppet path hardcodes to true;
    // the `.clp` importer reads it.
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) propagate_meshgroup: Option<bool>,
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
