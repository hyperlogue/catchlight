//! Browser-owned drafts over Rust authoring tools. A draft owns geometry or
//! a captured keypoint, never a mutable reference to the replica's model.

use catchlight_core::{BindingParams, Model, ModelNodeKind, NodeId, ParamId, Puppet};
use catchlight_editor_core::{AlphaMask, MeshDraft, RecordProperties, Recording};
use catchlight_editor_protocol::MeshInfo;
use serde::Serialize;

pub struct MeshDraftState {
    pub draft: MeshDraft,
    pub texture: String,
}

#[derive(Serialize)]
pub struct MeshDraftView {
    pub mesh: MeshInfo,
    pub constraints: Vec<(u32, u32)>,
    /// Local coordinates at the texture's top left and bottom right, signed
    /// so flipped mappings keep their orientation in a browser image transform.
    pub texture_rect: [f32; 4],
    pub texture: String,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

impl MeshDraftState {
    pub fn new(model: &Model, node: &str) -> Result<Self, String> {
        let id = NodeId::new(node).map_err(|e| e.to_string())?;
        let part = model.node(&id).ok_or("The part no longer exists.")?;
        let ModelNodeKind::Part(part) = &part.kind else {
            return Err("Select a textured part to edit its mesh.".into());
        };
        let texture = part
            .albedo()
            .ok_or("Import artwork before editing this mesh.")?;
        let image = model
            .texture(texture)
            .ok_or("The artwork is unavailable.")?;
        let alpha = AlphaMask::decode_as(&image.data, image.encoding)
            .ok_or("The artwork could not be decoded.")?;
        Ok(Self {
            draft: MeshDraft::new(part.mesh(), alpha).map_err(|e| e.to_string())?,
            texture: texture.to_string(),
        })
    }

    pub fn view(&self) -> Result<MeshDraftView, String> {
        let d = &self.draft;
        let mesh = d.working();
        let uv = d.uv_map();
        let a = uv.local([0., 0.]);
        let b = uv.local([1., 1.]);
        let verts: Vec<_> = mesh.verts.as_chunks::<2>().0.to_vec();
        Ok(MeshDraftView {
            mesh: MeshInfo {
                uvs: verts.iter().map(|v| uv.uv(*v)).collect(),
                verts,
                indices: mesh.triangulate().map_err(|e| e.to_string())?,
                origin: mesh.origin,
            },
            constraints: mesh.constraints.clone(),
            texture_rect: [a[0], a[1], b[0], b[1]],
            texture: self.texture.clone(),
            dirty: d.dirty(),
            can_undo: d.can_undo(),
            can_redo: d.can_redo(),
        })
    }

    pub fn finish(&self) -> Result<MeshInfo, String> {
        let mesh = self.draft.finish().map_err(|e| e.to_string())?;
        let flat: Vec<u32> = match mesh.indices {
            catchlight_core::formats::clm::ClmIndices::U16(v) => {
                v.into_iter().map(u32::from).collect()
            }
            catchlight_core::formats::clm::ClmIndices::U32(v) => v,
        };
        Ok(MeshInfo {
            verts: mesh.verts.as_chunks::<2>().0.to_vec(),
            uvs: mesh.uvs.as_chunks::<2>().0.to_vec(),
            indices: flat.as_chunks::<3>().0.to_vec(),
            origin: mesh.origin,
        })
    }
}

pub fn capture(
    model: &Model,
    puppet: &Puppet,
    node: &str,
    param: &str,
    param_y: Option<&str>,
    position: [f32; 2],
) -> Result<Recording, String> {
    let id = NodeId::new(node).map_err(|e| e.to_string())?;
    let x = ParamId::new(param).map_err(|e| e.to_string())?;
    let params = match param_y {
        Some(y) => BindingParams::Two(x, ParamId::new(y).map_err(|e| e.to_string())?),
        None => BindingParams::One(x),
    };
    Recording::capture(model, puppet, &id, params, position)
}

pub fn recording_patch(
    recording: &Recording,
    json: &str,
    authored_basis: bool,
) -> Result<String, String> {
    let patch: RecordProperties = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let writes = recording.writes(&patch, authored_basis)?;
    serde_json::to_string(&writes).map_err(|e| e.to_string())
}

#[cfg(target_arch = "wasm32")]
pub mod browser {
    use super::*;
    use wasm_bindgen::prelude::*;

    fn js(e: impl std::fmt::Display) -> JsValue {
        JsValue::from_str(&e.to_string())
    }

    #[wasm_bindgen]
    pub struct MeshEditDraft {
        pub(crate) state: MeshDraftState,
    }

    #[wasm_bindgen]
    impl MeshEditDraft {
        pub fn vertices(&self) -> Vec<f32> {
            self.state.draft.working().verts.clone()
        }
        pub fn triangles(&self) -> Result<Vec<u32>, JsValue> {
            self.state
                .draft
                .working()
                .triangulate()
                .map(|v| v.into_iter().flatten().collect())
                .map_err(js)
        }
        pub fn view(&self) -> Result<String, JsValue> {
            serde_json::to_string(&self.state.view().map_err(js)?).map_err(js)
        }
        pub fn finish(&self) -> Result<String, JsValue> {
            serde_json::to_string(&self.state.finish().map_err(js)?).map_err(js)
        }
        #[wasm_bindgen(js_name = beginGesture)]
        pub fn begin_gesture(&mut self) {
            self.state.draft.begin_gesture();
        }
        #[wasm_bindgen(js_name = endGesture)]
        pub fn end_gesture(&mut self, commit: bool) {
            self.state.draft.end_gesture(commit);
        }
        #[wasm_bindgen(js_name = moveVertex)]
        pub fn move_vertex(&mut self, index: u32, x: f32, y: f32) -> Result<(), JsValue> {
            self.state.draft.move_vertex(index, [x, y]).map_err(js)
        }
        #[wasm_bindgen(js_name = translateVertices)]
        pub fn translate_vertices(
            &mut self,
            indices: &[u32],
            dx: f32,
            dy: f32,
        ) -> Result<(), JsValue> {
            self.state
                .draft
                .translate_vertices(indices, [dx, dy])
                .map_err(js)
        }
        #[wasm_bindgen(js_name = addVertex)]
        pub fn add_vertex(&mut self, x: f32, y: f32) -> Result<u32, JsValue> {
            self.state.draft.add_vertex([x, y]).map_err(js)
        }
        #[wasm_bindgen(js_name = deleteVertices)]
        pub fn delete_vertices(&mut self, indices: &[u32]) -> Result<(), JsValue> {
            self.state.draft.delete_vertices(indices).map_err(js)
        }
        #[wasm_bindgen(js_name = toggleEdge)]
        pub fn toggle_edge(&mut self, a: u32, b: u32) -> Result<(), JsValue> {
            self.state.draft.toggle_edge(a, b).map_err(js)
        }
        #[wasm_bindgen(js_name = generateContour)]
        pub fn generate_contour(&mut self, spacing: u32, margin: u32) -> Result<(), JsValue> {
            self.state
                .draft
                .generate_contour(spacing, margin)
                .map_err(js)
        }
        #[wasm_bindgen(js_name = generateGrid)]
        pub fn generate_grid(&mut self, cols: u32, rows: u32) -> Result<(), JsValue> {
            self.state.draft.generate_grid(cols, rows).map_err(js)
        }
        pub fn undo(&mut self) {
            self.state.draft.undo();
        }
        pub fn redo(&mut self) {
            self.state.draft.redo();
        }
    }

    #[wasm_bindgen]
    pub struct RecordingGesture {
        pub(crate) state: Recording,
    }
    #[wasm_bindgen]
    impl RecordingGesture {
        pub fn posed(&self) -> Result<String, JsValue> {
            serde_json::to_string(&self.state.posed).map_err(js)
        }
        pub fn patch(&self, json: &str, authored_basis: bool) -> Result<String, JsValue> {
            recording_patch(&self.state, json, authored_basis).map_err(js)
        }
        pub fn deform(&self, deltas: &[f32]) -> Result<String, JsValue> {
            serde_json::to_string(&self.state.deform_write(deltas).map_err(js)?).map_err(js)
        }
    }
}
