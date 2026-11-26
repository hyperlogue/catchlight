use serde::{Deserialize, Deserializer};

/// A field of the wrong JSON type reads as absent rather than failing the file.
fn de_lenient<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
where
    T: for<'a> Deserialize<'a>,
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

/// A list of records whose order carries no meaning: a malformed record is
/// dropped and the rest are kept.
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

/// An array whose elements mean something by position (coordinates,
/// indices, texture slots): dropping a bad element would shift every later
/// one, so it fails the field and the import. A non-array reads as absent.
fn de_positional<'de, T, D>(d: D) -> Result<Vec<T>, D::Error>
where
    T: for<'a> Deserialize<'a>,
    D: Deserializer<'de>,
{
    Ok(de_positional_opt(d)?.unwrap_or_default())
}

fn de_positional_opt<'de, T, D>(d: D) -> Result<Option<Vec<T>>, D::Error>
where
    T: for<'a> Deserialize<'a>,
    D: Deserializer<'de>,
{
    let serde_json::Value::Array(arr) = serde_json::Value::deserialize(d)? else {
        return Ok(None);
    };
    arr.iter()
        .enumerate()
        .map(|(i, item)| {
            T::deserialize(item)
                .map_err(|e| serde::de::Error::custom(format!("element {i} of an array: {e}")))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// A non-object reads as absent; an object's own positional fields still
/// fail the import.
fn de_lenient_object<'de, T, D>(d: D) -> Result<Option<T>, D::Error>
where
    T: for<'a> Deserialize<'a>,
    D: Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(d)?;
    if !v.is_object() {
        return Ok(None);
    }
    T::deserialize(&v)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SchemaTransform {
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) trans: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) rot: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional")]
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
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) verts: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) uvs: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) indices: Vec<u32>,
    #[serde(default, deserialize_with = "de_positional")]
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
    #[serde(default, deserialize_with = "de_lenient_object")]
    pub(super) transform: Option<SchemaTransform>,
    #[serde(default, rename = "lockToRoot", deserialize_with = "de_lenient")]
    pub(super) lock_to_root: Option<bool>,
    #[serde(default, rename = "type", deserialize_with = "de_lenient")]
    pub(super) ty: Option<String>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) textures: Vec<i64>,
    #[serde(default, deserialize_with = "de_lenient_object")]
    pub(super) mesh: Option<SchemaMesh>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) opacity: Option<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) blend_mode: Option<String>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) tint: Vec<f32>,
    #[serde(default, rename = "screenTint", deserialize_with = "de_positional")]
    pub(super) screen_tint: Vec<f32>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) masks: Option<Vec<SchemaMask>>,
    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) mask_threshold: Option<f32>,

    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) translate_children: Option<bool>,

    #[serde(default, deserialize_with = "de_lenient")]
    pub(super) propagate_meshgroup: Option<bool>,
}

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
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) min: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) max: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional")]
    pub(super) defaults: Vec<f32>,
    #[serde(default, deserialize_with = "de_positional_opt")]
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
