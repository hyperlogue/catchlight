//! The tab's own copy of one session: [`ReplicaState`].
//!
//! Feeds at or below the held revision are dropped whole, except into a
//! pristine replica: its revision is absent, since a never-edited session is
//! at rev `0`. Textures are fetched by Id (immutable) and byte extensions by
//! hash (their bytes change under a stable key). Pose and scratch live on the
//! puppet, never in the model, so they survive a rebake.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use catchlight_core::formats::clm::structure_texture_ids;
use catchlight_core::formats::clm::{
    ClmTextureRef, TextureAlpha, TextureEncoding, decode_structure, extension_hash,
};
use catchlight_core::{
    BlendMode, ExtensionKey, LoadBudget, Mat4, Model, ModelTexture, NodeId, NodeKind, ParamId,
    Puppet, ScratchTransform, TexId, Vec2, Vec3,
};
use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId, SessionId};
use catchlight_editor_server::{Editor, replica_reply};
use catchlight_extension_animations::{AnimationPlayer, Animations, LiveInputs, Rig};
use catchlight_extension_expressions::Expressions;
use catchlight_extension_humanoid::Humanoid;
use serde::{Deserialize, Serialize};

/// One byte extension a structure names and the replica has yet to fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionNeed {
    pub key: ExtensionKey,
    pub hash: String,
}

/// The clip a replica plays and where it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playing {
    pub name: String,
    /// Seconds from the clip's start.
    pub time: f32,
    /// Fractional.
    pub frame: f32,
}

/// The live inputs a replica poses its puppet with, by name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveInputsJson {
    pub controls: BTreeMap<String, f32>,
    pub expressions: BTreeMap<String, f32>,
    pub visemes: BTreeMap<String, f32>,
}

/// Straight-alpha sRGB artwork, tightly packed (`4 * width` bytes per row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One session's model and pose, in the tab. No GPU, so it tests natively.
///
/// Live inputs and the playing clip are the tab's own, like its pose:
/// never written to the model.
pub struct ReplicaState {
    model: Model,
    puppet: Puppet,
    /// Shared with the model, so a rebuild is a pointer copy.
    textures: HashMap<TexId, Arc<[u8]>>,
    /// The hash rides along so deciding what to fetch costs no re-hashing.
    extension_bytes: HashMap<ExtensionKey, (String, Arc<[u8]>)>,
    /// The revision this model reads as, or `None` while pristine.
    rev: Option<u64>,
    /// The last tick's animation-frame timestamp, in milliseconds.
    last_frame_ms: Option<f64>,
    /// Whether that tick left the puppet still moving, replayed to every later
    /// caller framing at the same timestamp.
    moving: bool,
    player: AnimationPlayer,
    inputs: LiveInputs,
    editing: bool,
    motion_paused: bool,
}

impl Default for ReplicaState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicaState {
    pub fn new() -> Self {
        // A connected tab builds only replicas, and must check the official
        // extensions as the editor does. Registering the same types again is
        // a no-op, and no other type takes these keys.
        let _ = catchlight_core::extension::register::<Animations>()
            .and_then(|()| catchlight_core::extension::register::<Humanoid>())
            .and_then(|()| catchlight_core::extension::register::<Expressions>());
        let model = Model::new();
        let puppet = Puppet::new(&model);
        Self {
            model,
            puppet,
            textures: HashMap::new(),
            extension_bytes: HashMap::new(),
            rev: None,
            last_frame_ms: None,
            moving: false,
            player: AnimationPlayer::default(),
            inputs: LiveInputs::default(),
            editing: false,
            motion_paused: false,
        }
    }

    pub fn rev(&self) -> u64 {
        self.rev.unwrap_or(0)
    }

    fn accepts(&self, rev: u64) -> bool {
        self.rev.is_none_or(|held| rev > held)
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The model the puppet was last baked against, which the render cache's
    /// generation gate checks.
    pub fn model_and_puppet(&self) -> (&Model, &Puppet) {
        (&self.model, &self.puppet)
    }

    /// The textures `structure` names that are not held, in model order.
    pub fn textures_needed(&self, structure: &[u8]) -> Result<Vec<ClmTextureRef>, String> {
        let named = structure_texture_ids(structure, &mut LoadBudget::default())
            .map_err(|e| e.to_string())?;
        Ok(named
            .into_iter()
            .filter(|t| !self.textures.contains_key(&t.id))
            .collect())
    }

    /// The byte extensions `structure` names whose bytes are not held, by hash:
    /// a stale value under the same key must still be fetched.
    pub fn extensions_needed(&self, structure: &[u8]) -> Result<Vec<ExtensionNeed>, String> {
        let structure = decode_structure(structure).map_err(|e| e.to_string())?;
        Ok(structure
            .doc
            .extensions
            .iter()
            .filter_map(|(key, value)| Some((key, value.marker()?)))
            .filter(|(key, marker)| {
                self.extension_bytes
                    .get(*key)
                    .is_none_or(|(held, _)| *held != marker.hash)
            })
            .map(|(key, marker)| ExtensionNeed {
                key: key.clone(),
                hash: marker.hash.clone(),
            })
            .collect())
    }

    /// Hold `bytes` under `key`; an invalid key is dropped.
    pub fn put_extension(&mut self, key: &str, bytes: Vec<u8>) {
        if let Ok(key) = key.parse::<ExtensionKey>() {
            let bytes: Arc<[u8]> = bytes.into();
            self.extension_bytes
                .insert(key, (extension_hash(&bytes), bytes));
        }
    }

    /// Hold `bytes` under `id`. An invalid Id is dropped: the structure naming it
    /// fails with a better error.
    pub fn put_texture(&mut self, id: &str, bytes: Vec<u8>) {
        if let Ok(id) = id.parse::<TexId>() {
            self.textures.insert(id, bytes.into());
        }
    }

    /// Rebuild the model from a structure-only container at `rev`, over the held
    /// payloads. `Ok(false)` when `rev` is not newer; on either refusal nothing is
    /// touched.
    pub fn apply_structure(&mut self, structure: &[u8], rev: u64) -> Result<bool, String> {
        if !self.accepts(rev) {
            return Ok(false);
        }
        let textures = &self.textures;
        let extension_bytes = &self.extension_bytes;
        self.model
            .replace_structure_with_extensions(
                structure,
                |id| {
                    textures.get(id).map(|data| ModelTexture {
                        // `replace_structure` reads the structure's own manifest for these; any
                        // value does.
                        encoding: TextureEncoding::Png,
                        alpha: TextureAlpha::Straight,
                        data: data.clone(),
                    })
                },
                // A marker whose bytes were never put is an error naming the key.
                |key| extension_bytes.get(key).map(|(_, data)| data.clone()),
            )
            .map_err(|e| e.to_string())?;
        self.accept(rev);
        Ok(true)
    }

    /// Take the model from an in-tab [`Editor`]'s session, sharing its texture
    /// `Arc`s. Returns the revision held afterwards.
    pub fn sync_from_editor(&mut self, editor: &Editor, session: SessionId) -> u64 {
        let Some(rev) = editor.revision(session) else {
            return self.rev();
        };
        if !self.accepts(rev) {
            return self.rev();
        }
        if editor
            .with_model(session, |model| self.model.replace_from(model))
            .is_ok()
        {
            self.accept(rev);
        }
        self.rev()
    }

    fn accept(&mut self, rev: u64) {
        self.rev = Some(rev);
        self.textures = self
            .model
            .texture_ids()
            .iter()
            .filter_map(|id| Some((id.clone(), self.model.texture(id)?.data.clone())))
            .collect();
        let held = std::mem::take(&mut self.extension_bytes);
        self.extension_bytes = self
            .model
            .extensions()
            .iter()
            .filter_map(|(key, value)| {
                let data = value.bytes()?;
                let hash = match held.get(key) {
                    Some((hash, before)) if Arc::ptr_eq(before, data) => hash.clone(),
                    _ => extension_hash(data),
                };
                Some((key.clone(), (hash, data.clone())))
            })
            .collect();
        // Eagerly: `node_idx` answers off the bake, and a caller may pose a node the
        // feed just added before any frame runs.
        self.puppet.sync(&self.model);
        let clips = self
            .model
            .extension::<Animations>()
            .map(|animations| animations.clips().collect::<Vec<_>>())
            .unwrap_or_default();
        self.player.replace_clips(clips);
    }

    /// Answer one model-only JSON [`Request`]; anything else is `bad_request`.
    pub fn query(&self, request_json: &str) -> String {
        let reply = match serde_json::from_str::<Request>(request_json) {
            Ok(request) => replica_reply(&self.model, self.rev(), request),
            // Answer against the id the caller is waiting on, not against 0.
            Err(e) => Reply::Err {
                op_index: None,
                limit: None,
                id: serde_json::from_str::<RequestId>(request_json).map_or(0, |r| r.id),
                code: ErrorCode::BadRequest,
                message: e.to_string(),
            },
        };
        serde_json::to_string(&reply).unwrap_or_else(|_| {
            r#"{"reply":"err","id":0,"code":"bad_request","message":"reply could not be serialized"}"#
                .to_string()
        })
    }

    pub fn set_param(&mut self, id: &str, value: f32) -> bool {
        let Ok(id) = id.parse::<ParamId>() else {
            return false;
        };
        if !self.puppet.param_ids().any(|p| *p == id) {
            return false;
        }
        self.puppet.set_param_value(&id, value);
        // The slider wins over a live control mapped onto the same param.
        if let Ok(humanoid) = self.model.extension::<Humanoid>() {
            for control in humanoid.controls_for(&id) {
                self.inputs.controls.remove(control);
            }
        }
        true
    }

    /// Sets or, with `None`, clears one control's live value, clamped to
    /// the control's range. Applied at once, so reads that follow see it.
    pub fn set_control(&mut self, name: &str, value: Option<f32>) {
        match value {
            Some(value) => self.inputs.controls.set(name, value),
            None => {
                self.inputs.controls.remove(name);
            }
        }
        self.tick(0.0);
    }

    /// Sets or clears one expression's live weight, clamped to 0..1.
    pub fn set_expression_weight(&mut self, name: &str, weight: Option<f32>) {
        match weight {
            Some(weight) => self.inputs.expressions.set(name, weight),
            None => {
                self.inputs.expressions.remove(name);
            }
        }
        self.tick(0.0);
    }

    /// Sets or clears one viseme's live weight, clamped to 0..1.
    pub fn set_viseme_weight(&mut self, name: &str, weight: Option<f32>) {
        match weight {
            Some(weight) => self.inputs.visemes.set(name, weight),
            None => {
                self.inputs.visemes.remove(name);
            }
        }
        self.tick(0.0);
    }

    pub fn clear_inputs(&mut self) {
        self.inputs = LiveInputs::default();
        self.tick(0.0);
    }

    pub fn inputs(&self) -> LiveInputsJson {
        let inputs = &self.inputs;
        LiveInputsJson {
            controls: inputs.controls.iter().map(|(n, v)| (n.into(), v)).collect(),
            expressions: inputs
                .expressions
                .iter()
                .map(|(n, v)| (n.into(), v))
                .collect(),
            visemes: inputs.visemes.iter().map(|(n, v)| (n.into(), v)).collect(),
        }
    }

    /// Plays the model's clip named `name` from its start. `false` when the
    /// model has no such clip.
    pub fn play(&mut self, name: &str) -> bool {
        if !self.player.play(name) {
            return false;
        }
        self.tick(0.0);
        true
    }

    pub fn stop(&mut self) {
        self.player.stop();
        self.tick(0.0);
    }

    pub fn playing(&self) -> Option<Playing> {
        let sample = self.player.sample()?;
        Some(Playing {
            name: sample.name.to_owned(),
            time: sample.time,
            frame: sample.frame,
        })
    }

    /// `None` when nothing posed it and no driver claims it, or it does not exist.
    pub fn param_value(&self, id: &str) -> Option<f32> {
        let id = id.parse::<ParamId>().ok()?;
        self.puppet.param_value(&id)
    }

    /// Write a node's scratch deform from a flat `[dx, dy, ...]` array, the cheap
    /// shape for a per-pointer-move call. A trailing odd value is dropped.
    pub fn set_scratch_deform(&mut self, node: &str, offsets: &[f32]) -> bool {
        let Some(idx) = self.node_idx(node) else {
            return false;
        };
        let (pairs, _odd) = offsets.as_chunks::<2>();
        let pairs: Vec<Vec2> = pairs.iter().map(|p| Vec2::new(p[0], p[1])).collect();
        if !self.puppet.set_scratch_deform(idx, &pairs) {
            return false;
        }
        self.combine_scratch_deforms();
        true
    }

    pub fn clear_scratch_deform(&mut self, node: &str) -> bool {
        let Some(idx) = self.node_idx(node) else {
            return false;
        };
        if !self.puppet.clear_scratch_deform(idx) {
            return false;
        }
        self.combine_scratch_deforms();
        true
    }

    /// Every field is absolute; NaN means "leave what the fold produced", and a
    /// vector field with any NaN component is dropped whole.
    #[expect(
        clippy::too_many_arguments,
        reason = "a wasm-bindgen export takes each field as its own scalar"
    )]
    pub fn set_scratch_transform(
        &mut self,
        node: &str,
        tx: f32,
        ty: f32,
        tz: f32,
        rx: f32,
        ry: f32,
        rz: f32,
        sx: f32,
        sy: f32,
        z_order: f32,
        opacity: f32,
    ) -> bool {
        let Some(idx) = self.node_idx(node) else {
            return false;
        };
        self.puppet.set_scratch_transform(
            idx,
            ScratchTransform {
                translation: vec3(tx, ty, tz),
                rotation: vec3(rx, ry, rz),
                scale: vec2(sx, sy),
                z_order: scalar(z_order),
                opacity: scalar(opacity),
            },
        )
    }

    pub fn clear_scratch_transform(&mut self, node: &str) -> bool {
        let Some(idx) = self.node_idx(node) else {
            return false;
        };
        self.puppet.clear_scratch_transform(idx)
    }

    /// End every edit in progress, without the caller tracking which nodes.
    pub fn clear_all_scratch(&mut self) {
        self.puppet.clear_all_scratch();
        self.combine_scratch_deforms();
    }

    /// Sum every deform stack's active sources. A scratch deform only sets a dirty
    /// flag, and a still session's tick never folds, so every scratch-deform
    /// write must end with this or the drag never reaches [`Self::bounds`] or the
    /// render cache.
    fn combine_scratch_deforms(&mut self) {
        self.puppet.combine_deforms();
    }

    /// Pick evaluated mesh triangles, front to back. Hidden ancestors and culled
    /// drawables do not hit.
    pub fn pick_node(&self, x: f32, y: f32) -> Option<String> {
        let hit =
            catchlight_editor_core::picking::pick_all(&self.model, &self.puppet, Vec2::new(x, y))
                .into_iter()
                .next()?;
        self.puppet
            .node_id(catchlight_core::NodeIdx(hit))
            .map(ToString::to_string)
    }

    /// Evaluated part vertices in world coordinates, as the GPU draws them.
    pub fn node_vertices(&self, node: &str) -> Option<Vec<f32>> {
        let id = NodeId::new(node).ok()?;
        let idx = self.puppet.node_idx(&id)?;
        let NodeKind::Part(part) = &self.puppet.get(idx)?.kind else {
            let mut vertices = Vec::new();
            let mut pending = self.puppet.tree().get_children(idx);
            while let Some(child) = pending.pop() {
                let Some(node) = self.puppet.get(child) else {
                    continue;
                };
                if !node.enabled {
                    continue;
                }
                if let NodeKind::Part(part) = &node.kind {
                    let transform = self.puppet.transforms().get(child);
                    for i in 0..part.mesh.vertices.len() {
                        vertices.extend(
                            catchlight_editor_core::picking::part_world_vertex(part, &transform, i)
                                .to_array(),
                        );
                    }
                }
                pending.extend(self.puppet.tree().get_children(child));
            }
            return Some(vertices);
        };
        let transform = self.puppet.transforms().get(idx);
        Some(
            (0..part.mesh.vertices.len())
                .flat_map(|i| {
                    catchlight_editor_core::picking::part_world_vertex(part, &transform, i)
                        .to_array()
                })
                .collect(),
        )
    }

    /// Bounded artwork preview in straight-alpha sRGB, for a browser `ImageData`.
    pub fn texture_thumbnail(&self, texture: &str) -> Option<TextureImage> {
        self.texture_image(texture, 128)
    }

    pub fn texture_image(&self, texture: &str, limit: u32) -> Option<TextureImage> {
        let id = TexId::new(texture).ok()?;
        let encoded = catchlight_core::texture::EncodedTexture::from(self.model.texture(&id)?);
        let mut decoded = encoded.decode().ok()?;
        while limit > 0 && decoded.width.max(decoded.height) > limit {
            decoded = decoded.halved();
        }
        let mut rgba = decoded.rgba.to_vec();
        catchlight_core::texture::unpremultiply_linear_from_srgb_inplace(&mut rgba);
        Some(TextureImage {
            width: decoded.width,
            height: decoded.height,
            rgba,
        })
    }

    /// Editing pauses motion responses in this tab and stops the playing
    /// clip. Live inputs stay.
    pub fn set_editing(&mut self, editing: bool) {
        self.editing = editing;
        if editing {
            self.player.stop();
        }
        self.apply_motion();
    }

    /// The person's own pause, apart from editing. A playing clip keeps
    /// playing.
    pub fn set_motion_paused(&mut self, paused: bool) {
        self.motion_paused = paused;
        self.apply_motion();
    }

    pub fn motion_paused(&self) -> bool {
        self.motion_paused
    }

    pub fn motion_running(&self) -> bool {
        !self.editing && !self.motion_paused
    }

    fn apply_motion(&mut self) {
        self.puppet
            .set_motion_responses_enabled(self.motion_running());
        self.tick(0.0);
    }

    pub fn mesh_draft(&self, node: &str) -> Result<crate::authoring::MeshDraftState, String> {
        crate::authoring::MeshDraftState::new(&self.model, node)
    }

    pub fn recording_pairs(&self, node: &str, param: &str) -> Vec<[String; 2]> {
        let (Ok(node), Ok(param)) = (NodeId::new(node), ParamId::new(param)) else {
            return Vec::new();
        };
        let mut pairs = Vec::new();
        for b in self
            .model
            .bindings_of_node(&node)
            .chain(self.model.bindings_of_param(&param))
        {
            if b.params().contains(&param)
                && let Some(y) = b.params().y()
            {
                let pair = [b.params().x().to_string(), y.to_string()];
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
        }
        pairs
    }

    pub fn recording(
        &mut self,
        node: &str,
        param: &str,
        param_y: Option<&str>,
        position: [f32; 2],
    ) -> Result<catchlight_editor_core::Recording, String> {
        self.tick(0.0);
        crate::authoring::capture(&self.model, &self.puppet, node, param, param_y, position)
    }

    /// A gesture captures these once, keeping the posed contribution while it
    /// edits the base.
    pub fn node_local_transform(&self, node: &str) -> Option<Vec<f32>> {
        let transform = &self.puppet.get(self.node_idx(node)?)?.transform;
        Some(
            transform
                .translation
                .to_array()
                .into_iter()
                .chain(transform.rotation.to_array())
                .chain(transform.scale.to_array())
                .collect(),
        )
    }

    /// A world-space direction expressed in the selected mesh's frame.
    /// Collapsed transforms refuse a drag rather than producing NaNs.
    pub fn node_delta_from_world(&self, node: &str, dx: f32, dy: f32) -> Option<Vec<f32>> {
        let transform = self.puppet.transforms().get(self.node_idx(node)?);
        let local = transform
            .inverse()
            .transform_vector3(Vec3::new(dx, dy, 0.0));
        local.is_finite().then(|| vec![local.x, local.y])
    }

    /// The node's evaluated world transform after the last tick: 16 floats,
    /// column-major. A never-ticked puppet reports the identity.
    pub fn node_world_transform(&self, node: &str) -> Option<[f32; 16]> {
        let idx = self.node_idx(node)?;
        Some(self.puppet.transforms().get(idx).to_cols_array())
    }

    /// The node's **authored** local translation moved by a world-space delta,
    /// as `[x, y, z]`. The delta maps through the parent's inverse world
    /// transform as a direction; Z is untouched. Starting from the authored
    /// value makes preview and commit agree. A collapsed parent scale leaves the
    /// node where it is rather than at NaN.
    pub fn translation_after_world_delta(&self, node: &str, dx: f32, dy: f32) -> Option<[f32; 3]> {
        let id = node.parse::<NodeId>().ok()?;
        let authored = self.model.node(&id)?.transform.translation;
        let parent = self
            .model
            .node(&id)
            .and_then(|n| n.parent())
            .and_then(|p| self.puppet.node_idx(p))
            .map_or(Mat4::IDENTITY, |idx| self.puppet.transforms().get(idx));
        let local = parent.inverse().transform_vector3(Vec3::new(dx, dy, 0.0));
        if !local.is_finite() {
            return Some(authored);
        }
        Some([authored[0] + local.x, authored[1] + local.y, authored[2]])
    }

    /// Evaluate one step of `dt` seconds: the live inputs and the playing
    /// clip, then the puppet. Returns whether the puppet is still moving: a
    /// clip plays or a motion response has not settled. Viewports use
    /// [`Self::frame`] so two canvases do not step the puppet twice.
    pub fn tick(&mut self, dt: f32) -> bool {
        self.player
            .advance_with(&mut self.puppet, dt, Rig::of(&self.model), &self.inputs);
        let motion = self.puppet.tick(&self.model, dt);
        motion.motion_responses || self.player.is_playing()
    }

    /// Advance to the animation frame stamped `now_ms`; ticks at most once per
    /// timestamp, since `requestAnimationFrame` gives one batch the same
    /// timestamp. Later callers get the same answer to whether it is still
    /// moving. `dt` is clamped to 100 ms so a tab returning from hidden does
    /// not integrate the whole gap.
    pub fn frame(&mut self, now_ms: f64) -> bool {
        if self.last_frame_ms != Some(now_ms) {
            let dt = self.last_frame_ms.map_or(0.0, |last| {
                ((now_ms - last) / 1000.0).clamp(0.0, 0.1) as f32
            });
            self.last_frame_ms = Some(now_ms);
            self.moving = self.tick(dt);
        }
        self.moving
    }

    /// Posed world-space bounds of the drawn geometry after the last tick, as
    /// `[min_x, min_y, max_x, max_y]`; `None` when nothing is drawn. Drawn means
    /// what the render list counts, ignoring upload state.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let tree = self.puppet.tree();
        let transforms = self.puppet.transforms();
        // `enabled` is ANDed down the tree; depth-first visits parents first.
        let mut enabled_cum = vec![true; self.puppet.len()];
        let mut min = Vec2::splat(f32::INFINITY);
        let mut max = Vec2::splat(f32::NEG_INFINITY);

        tree.traverse_depth_first(|idx| {
            let Some(node) = self.puppet.get(idx) else {
                return;
            };
            let slot = idx.0 as usize;
            let enabled = node.enabled
                && tree
                    .get_parent(idx)
                    .and_then(|p| enabled_cum.get(p.0 as usize).copied())
                    .unwrap_or(true);
            if let Some(cell) = enabled_cum.get_mut(slot) {
                *cell = enabled;
            }
            if !enabled {
                return;
            }
            let NodeKind::Part(part) = &node.kind else {
                return;
            };
            // Opacity 0 culls every blend mode but Darken, whose Min blend ignores blend
            // factors, as the collector decides it.
            if (part.albedo_texture.0 as usize) >= self.model.texture_ids().len() {
                return;
            }
            if part.opacity == 0.0 && part.blend_mode != BlendMode::Darken {
                return;
            }
            let deform = part
                .deform_stack
                .is_active()
                .then(|| part.deform_stack.combined());
            let world = transforms.get(idx);
            let origin = part.mesh.origin;
            for (i, vertex) in part.mesh.vertices.iter().enumerate() {
                let offset = deform.and_then(|d| d.get(i).copied()).unwrap_or(Vec2::ZERO);
                let p = world.transform_point3((*vertex - origin + offset).extend(0.0));
                min = min.min(p.truncate());
                max = max.max(p.truncate());
            }
        });

        // Only an empty sweep leaves the seeds untouched; a NaN vertex would
        // also poison the box, and a camera fitted to NaN shows nothing at all.
        (min.x <= max.x && min.y <= max.y && min.is_finite() && max.is_finite())
            .then_some([min.x, min.y, max.x, max.y])
    }

    fn node_idx(&self, node: &str) -> Option<catchlight_core::components::NodeIdx> {
        let id = node.parse::<NodeId>().ok()?;
        self.puppet.node_idx(&id)
    }
}

/// NaN is the wire's "leave this field alone".
fn scalar(v: f32) -> Option<f32> {
    (!v.is_nan()).then_some(v)
}

fn vec2(x: f32, y: f32) -> Option<Vec2> {
    (!x.is_nan() && !y.is_nan()).then(|| Vec2::new(x, y))
}

fn vec3(x: f32, y: f32, z: f32) -> Option<Vec3> {
    (!x.is_nan() && !y.is_nan() && !z.is_nan()).then(|| Vec3::new(x, y, z))
}

/// [`ReplicaState`] with the GPU half attached: what JavaScript holds.
///
/// Built with no GPU: pipelines need a surface format, so the first
/// `Viewport::new` hands it the device, and the first frame builds the cache.
#[cfg(target_arch = "wasm32")]
pub(crate) mod browser {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use catchlight_editor_protocol::SessionId;
    use catchlight_wgpu::{Pipelines, PrepareOptions, RenderCache, RenderList, WgpuRenderer};
    use wasm_bindgen::prelude::*;

    use super::ReplicaState;
    use crate::{CatchlightEditor, Gpu};

    pub(crate) struct ReplicaRender {
        pub(crate) renderer: WgpuRenderer,
        cache: Option<RenderCache>,
    }

    /// `Rc<RefCell<_>>`, not a lock: the browser runs the frame callback and the
    /// JS-facing methods on one thread with no overlap.
    pub(crate) struct ReplicaInner {
        pub(crate) state: ReplicaState,
        pub(crate) render: Option<ReplicaRender>,
    }

    impl ReplicaInner {
        pub(crate) fn device(&self) -> Option<&wgpu::Device> {
            self.render.as_ref().map(|r| &r.renderer.device)
        }

        /// The renderer for a surface of `format` on `gpu`. Errs when a later
        /// viewport brings a different device or format, which its pipelines and
        /// cache slots would not fit.
        pub(crate) fn ensure_renderer(
            &mut self,
            gpu: &Gpu,
            format: wgpu::TextureFormat,
        ) -> Result<&mut ReplicaRender, String> {
            let render = self.render.get_or_insert_with(|| ReplicaRender {
                // `Arc` because that is what a renderer shares pipelines
                // through; on wasm there is one thread and nothing to send,
                // exactly as in `WgpuRenderer::new`.
                #[allow(
                    clippy::arc_with_non_send_sync,
                    reason = "fires only on wasm32, where wgpu types are not Send"
                )]
                renderer: WgpuRenderer::from_pipelines(
                    gpu.device.clone(),
                    gpu.queue.clone(),
                    Arc::new(Pipelines::new(&gpu.device, format)),
                ),
                cache: None,
            });
            if render.renderer.device != gpu.device {
                return Err("this replica is already drawing on another device; \
                     one editor holds one Gpu and every viewport shares it"
                    .to_string());
            }
            if render.renderer.pipelines().surface_format != format {
                return Err(format!(
                    "this replica's renderer draws {:?} and the canvas wants {format:?}; \
                     one tab shares one device and one swapchain format",
                    render.renderer.pipelines().surface_format,
                ));
            }
            Ok(render)
        }

        /// Advance to `now_ms` (at most one tick per timestamp) and fill this
        /// viewport's `list`. Returns whether the puppet is still moving.
        pub(crate) fn frame(&mut self, now_ms: f64, list: &mut RenderList) -> Result<bool, String> {
            let moving = self.state.frame(now_ms);
            let render = self
                .render
                .as_mut()
                .ok_or("this replica has no renderer; a viewport builds one")?;
            let (model, puppet) = self.state.model_and_puppet();
            if render.cache.is_none() {
                render.cache = Some(
                    RenderCache::prepare(&mut render.renderer, model, PrepareOptions::default())
                        .map_err(|e| format!("preparing the render cache: {e}"))?,
                );
            }
            let Some(cache) = render.cache.as_mut() else {
                return Ok(moving);
            };
            // Rebuilds itself when the model moved, keeping every texture payload it
            // already holds.
            cache
                .refresh(&mut render.renderer, model, puppet)
                .map_err(|e| e.to_string())?;
            cache.collect_into(puppet, list);
            Ok(moving)
        }
    }

    /// One bounded, straight-alpha artwork thumbnail.
    #[wasm_bindgen]
    pub struct TextureThumbnail {
        pub width: u32,
        pub height: u32,
        rgba: Vec<u8>,
    }

    #[wasm_bindgen]
    impl TextureThumbnail {
        /// Copied straight into a JavaScript array, never through a Rust clone.
        pub fn pixels(&self) -> js_sys::Uint8Array {
            js_sys::Uint8Array::from(self.rgba.as_slice())
        }
    }

    impl From<super::TextureImage> for TextureThumbnail {
        fn from(image: super::TextureImage) -> Self {
            Self {
                width: image.width,
                height: image.height,
                rgba: image.rgba,
            }
        }
    }

    fn js_u64(value: f64, what: &str) -> Result<u64, JsValue> {
        super::wire_u64(value).ok_or_else(|| {
            JsValue::from_str(&format!("{what} must be a safe integer, not {value}"))
        })
    }

    #[wasm_bindgen]
    pub struct Replica {
        inner: Rc<RefCell<ReplicaInner>>,
    }

    impl Replica {
        pub(crate) fn inner(&self) -> Rc<RefCell<ReplicaInner>> {
            self.inner.clone()
        }
    }

    impl Default for Replica {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Revisions and session ids cross as `f64`: a `u64` would reach JavaScript
    /// as a `bigint`, giving one wire number two spellings.
    #[wasm_bindgen]
    impl Replica {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self {
                inner: Rc::new(RefCell::new(ReplicaInner {
                    state: ReplicaState::new(),
                    render: None,
                })),
            }
        }

        pub fn rev(&self) -> f64 {
            self.inner.borrow().state.rev() as f64
        }

        /// As JSON `[{ id, encoding, alpha }]`.
        #[wasm_bindgen(js_name = texturesNeeded)]
        pub fn textures_needed(&self, structure: &[u8]) -> Result<String, JsValue> {
            let needed = self
                .inner
                .borrow()
                .state
                .textures_needed(structure)
                .map_err(|e| JsValue::from_str(&e))?;
            serde_json::to_string(&needed).map_err(|e| JsValue::from_str(&e.to_string()))
        }

        #[wasm_bindgen(js_name = putTexture)]
        pub fn put_texture(&self, id: &str, bytes: Vec<u8>) {
            self.inner.borrow_mut().state.put_texture(id, bytes);
        }

        /// As JSON `[{ key, hash }]`.
        #[wasm_bindgen(js_name = extensionsNeeded)]
        pub fn extensions_needed(&self, structure: &[u8]) -> Result<String, JsValue> {
            let needed = self
                .inner
                .borrow()
                .state
                .extensions_needed(structure)
                .map_err(|e| JsValue::from_str(&e))?;
            serde_json::to_string(&needed).map_err(|e| JsValue::from_str(&e.to_string()))
        }

        #[wasm_bindgen(js_name = putExtension)]
        pub fn put_extension(&self, key: &str, bytes: Vec<u8>) {
            self.inner.borrow_mut().state.put_extension(key, bytes);
        }

        /// Throws when the structure is malformed or names a texture that was not
        /// put, with the model untouched.
        #[wasm_bindgen(js_name = applyStructure)]
        pub fn apply_structure(&self, structure: &[u8], rev: f64) -> Result<bool, JsValue> {
            let rev = js_u64(rev, "rev")?;
            self.inner
                .borrow_mut()
                .state
                .apply_structure(structure, rev)
                .map_err(|e| JsValue::from_str(&e))
        }

        #[wasm_bindgen(js_name = syncFromEditor)]
        pub fn sync_from_editor(
            &self,
            editor: &CatchlightEditor,
            session: f64,
        ) -> Result<f64, JsValue> {
            let session = SessionId(js_u64(session, "session")?);
            Ok(self
                .inner
                .borrow_mut()
                .state
                .sync_from_editor(editor.editor(), session) as f64)
        }

        pub fn query(&self, request_json: &str) -> String {
            self.inner.borrow().state.query(request_json)
        }

        #[wasm_bindgen(js_name = setParam)]
        pub fn set_param(&self, id: &str, value: f32) -> bool {
            self.inner.borrow_mut().state.set_param(id, value)
        }

        #[wasm_bindgen(js_name = paramValue)]
        pub fn param_value(&self, id: &str) -> Option<f32> {
            self.inner.borrow().state.param_value(id)
        }

        /// `undefined` clears the control's live value.
        #[wasm_bindgen(js_name = setControl)]
        pub fn set_control(&self, name: &str, value: Option<f32>) {
            self.inner.borrow_mut().state.set_control(name, value);
        }

        #[wasm_bindgen(js_name = setExpressionWeight)]
        pub fn set_expression_weight(&self, name: &str, weight: Option<f32>) {
            self.inner
                .borrow_mut()
                .state
                .set_expression_weight(name, weight);
        }

        #[wasm_bindgen(js_name = setVisemeWeight)]
        pub fn set_viseme_weight(&self, name: &str, weight: Option<f32>) {
            self.inner
                .borrow_mut()
                .state
                .set_viseme_weight(name, weight);
        }

        #[wasm_bindgen(js_name = clearInputs)]
        pub fn clear_inputs(&self) {
            self.inner.borrow_mut().state.clear_inputs();
        }

        /// As JSON `{ controls, expressions, visemes }`, each a map of name to
        /// value.
        pub fn inputs(&self) -> String {
            serde_json::to_string(&self.inner.borrow().state.inputs())
                .unwrap_or_else(|_| "{}".to_string())
        }

        pub fn play(&self, name: &str) -> bool {
            self.inner.borrow_mut().state.play(name)
        }

        pub fn stop(&self) {
            self.inner.borrow_mut().state.stop();
        }

        /// As JSON `{ name, time, frame }`; `undefined` when nothing plays.
        pub fn playing(&self) -> Option<String> {
            let playing = self.inner.borrow().state.playing()?;
            serde_json::to_string(&playing).ok()
        }

        /// Per-vertex offsets as pairs `[dx0, dy0, dx1, dy1, ...]`.
        #[wasm_bindgen(js_name = scratchDeform)]
        pub fn scratch_deform(&self, node: &str, offsets: &[f32]) -> bool {
            self.inner
                .borrow_mut()
                .state
                .set_scratch_deform(node, offsets)
        }

        #[wasm_bindgen(js_name = clearScratchDeform)]
        pub fn clear_scratch_deform(&self, node: &str) -> bool {
            self.inner.borrow_mut().state.clear_scratch_deform(node)
        }

        /// Absolute values, the ones a `node_set` patch will commit. NaN means
        /// "leave what the fold produced" for that field.
        #[wasm_bindgen(js_name = scratchTransform)]
        #[expect(
            clippy::too_many_arguments,
            reason = "a wasm-bindgen export takes each field as its own scalar"
        )]
        pub fn scratch_transform(
            &self,
            node: &str,
            tx: f32,
            ty: f32,
            tz: f32,
            rx: f32,
            ry: f32,
            rz: f32,
            sx: f32,
            sy: f32,
            z_order: f32,
            opacity: f32,
        ) -> bool {
            self.inner
                .borrow_mut()
                .state
                .set_scratch_transform(node, tx, ty, tz, rx, ry, rz, sx, sy, z_order, opacity)
        }

        #[wasm_bindgen(js_name = clearScratchTransform)]
        pub fn clear_scratch_transform(&self, node: &str) -> bool {
            self.inner.borrow_mut().state.clear_scratch_transform(node)
        }

        #[wasm_bindgen(js_name = clearAllScratch)]
        pub fn clear_all_scratch(&self) {
            self.inner.borrow_mut().state.clear_all_scratch();
        }

        #[wasm_bindgen(js_name = pickNode)]
        pub fn pick_node(&self, x: f32, y: f32) -> Option<String> {
            self.inner.borrow().state.pick_node(x, y)
        }

        #[wasm_bindgen(js_name = nodeVertices)]
        pub fn node_vertices(&self, node: &str) -> Option<Vec<f32>> {
            self.inner.borrow().state.node_vertices(node)
        }

        #[wasm_bindgen(js_name = meshDraft)]
        pub fn mesh_draft(
            &self,
            node: &str,
        ) -> Result<crate::authoring::browser::MeshEditDraft, JsValue> {
            self.inner
                .borrow()
                .state
                .mesh_draft(node)
                .map(|state| crate::authoring::browser::MeshEditDraft { state })
                .map_err(|e| JsValue::from_str(&e))
        }

        #[wasm_bindgen(js_name = recordingPairs)]
        pub fn recording_pairs(&self, node: &str, param: &str) -> String {
            serde_json::to_string(&self.inner.borrow().state.recording_pairs(node, param))
                .unwrap_or_default()
        }

        pub fn recording(
            &self,
            node: &str,
            param: &str,
            param_y: Option<String>,
            x: f32,
            y: f32,
        ) -> Result<crate::authoring::browser::RecordingGesture, JsValue> {
            self.inner
                .borrow_mut()
                .state
                .recording(node, param, param_y.as_deref(), [x, y])
                .map(|state| crate::authoring::browser::RecordingGesture { state })
                .map_err(|e| JsValue::from_str(&e))
        }

        #[wasm_bindgen(js_name = setEditing)]
        pub fn set_editing(&self, editing: bool) {
            self.inner.borrow_mut().state.set_editing(editing);
        }

        #[wasm_bindgen(js_name = setMotionPaused)]
        pub fn set_motion_paused(&self, paused: bool) {
            self.inner.borrow_mut().state.set_motion_paused(paused);
        }

        #[wasm_bindgen(js_name = motionPaused)]
        pub fn motion_paused(&self) -> bool {
            self.inner.borrow().state.motion_paused()
        }

        #[wasm_bindgen(js_name = motionRunning)]
        pub fn motion_running(&self) -> bool {
            self.inner.borrow().state.motion_running()
        }

        #[wasm_bindgen(js_name = textureImage)]
        pub fn texture_image(&self, texture: &str) -> Option<TextureThumbnail> {
            let image = self.inner.borrow().state.texture_image(texture, 0)?;
            Some(image.into())
        }

        #[wasm_bindgen(js_name = textureThumbnail)]
        pub fn texture_thumbnail(&self, texture: &str) -> Option<TextureThumbnail> {
            let image = self.inner.borrow().state.texture_thumbnail(texture)?;
            Some(image.into())
        }

        #[wasm_bindgen(js_name = nodeWorldTransform)]
        pub fn node_world_transform(&self, node: &str) -> Option<Vec<f32>> {
            self.inner
                .borrow()
                .state
                .node_world_transform(node)
                .map(|m| m.to_vec())
        }

        #[wasm_bindgen(js_name = nodeLocalTransform)]
        pub fn node_local_transform(&self, node: &str) -> Option<Vec<f32>> {
            self.inner.borrow().state.node_local_transform(node)
        }

        #[wasm_bindgen(js_name = nodeDeltaFromWorld)]
        pub fn node_delta_from_world(&self, node: &str, dx: f32, dy: f32) -> Option<Vec<f32>> {
            self.inner
                .borrow()
                .state
                .node_delta_from_world(node, dx, dy)
        }

        /// `[min_x, min_y, max_x, max_y]` in world units, Y-up; `undefined` when
        /// nothing is drawn.
        pub fn bounds(&self) -> Option<Vec<f32>> {
            self.inner.borrow().state.bounds().map(|b| b.to_vec())
        }

        #[wasm_bindgen(js_name = translationAfterWorldDelta)]
        pub fn translation_after_world_delta(
            &self,
            node: &str,
            dx: f32,
            dy: f32,
        ) -> Option<Vec<f32>> {
            self.inner
                .borrow()
                .state
                .translation_after_world_delta(node, dx, dy)
                .map(|t| t.to_vec())
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use browser::Replica;

/// A JavaScript number as a wire id or revision: a non-negative integer no
/// larger than `Number.MAX_SAFE_INTEGER`.
#[cfg(any(target_arch = "wasm32", test))]
fn wire_u64(value: f64) -> Option<u64> {
    const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= MAX_SAFE_INTEGER)
        .then_some(value as u64)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::CatchlightEditor;
    use catchlight_editor_server::Attachments;
    use serde_json::{Value, json};

    const ROOT: &str = "root";

    fn call(editor: &CatchlightEditor, value: Value) -> Value {
        serde_json::from_str(&editor.handle(&value.to_string())).unwrap()
    }

    fn new_session(editor: &CatchlightEditor) -> SessionId {
        let reply = call(editor, json!({"id": 1, "cmd": "session_create"}));
        SessionId(reply["body"]["session"].as_u64().unwrap())
    }

    fn add_node(
        editor: &CatchlightEditor,
        session: SessionId,
        parent: &str,
        kind: &str,
        name: &str,
    ) -> String {
        let reply = call(
            editor,
            json!({"id": 2, "cmd": "node_add", "session": session.0,
                   "parent": parent, "kind": kind, "name": name}),
        );
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
        reply["body"]["node"].as_str().unwrap().to_string()
    }

    fn add_part_with_texture(editor: &CatchlightEditor, session: SessionId) -> String {
        let part = add_node(editor, session, ROOT, "part", "face");
        let reply = with_texture(editor, 3, session, &part);
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
        reply["body"]["texture"].as_str().unwrap().to_string()
    }

    fn with_texture(editor: &CatchlightEditor, id: u64, session: SessionId, node: &str) -> Value {
        let mut attachments = Attachments::none();
        attachments.insert("texture", one_pixel_png());
        let (reply, _) = editor.dispatch_with(
            &json!({"id": id, "cmd": "texture_add", "session": session.0,
                    "node": node, "encoding": "png"})
            .to_string(),
            attachments,
        );
        serde_json::from_str(&reply).unwrap()
    }

    fn structure_of(editor: &CatchlightEditor, session: SessionId) -> Vec<u8> {
        editor
            .editor()
            .with_model(session, Model::to_structure_bytes)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn only_safe_integers_cross_as_wire_numbers() {
        assert_eq!(wire_u64(0.0), Some(0));
        assert_eq!(
            wire_u64(9_007_199_254_740_991.0),
            Some(9_007_199_254_740_991)
        );
        for bad in [-1.0, 0.5, f64::NAN, f64::INFINITY, 9_007_199_254_740_992.0] {
            assert_eq!(wire_u64(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_structure_is_applied_once_and_never_backwards() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        add_node(&editor, session, ROOT, "group", "head");
        let structure = structure_of(&editor, session);

        let mut replica = ReplicaState::new();
        assert!(replica.apply_structure(&structure, 7).unwrap());
        assert_eq!(replica.rev(), 7);
        let generation = replica.model().generation();

        assert!(!replica.apply_structure(&structure, 7).unwrap());
        assert!(!replica.apply_structure(&structure, 3).unwrap());
        assert_eq!(replica.rev(), 7);
        assert_eq!(replica.model().generation(), generation);
    }

    #[test]
    fn a_missing_texture_leaves_the_model_alone() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        add_node(&editor, session, ROOT, "group", "head");
        let plain = structure_of(&editor, session);
        add_part_with_texture(&editor, session);
        let with_texture = structure_of(&editor, session);

        let mut replica = ReplicaState::new();
        assert!(replica.apply_structure(&plain, 1).unwrap());
        let generation = replica.model().generation();

        let err = replica
            .apply_structure(&with_texture, 2)
            .expect_err("a texture that was never put must fail");
        assert!(err.contains("texture"), "message was {err}");
        assert_eq!(
            replica.rev(),
            1,
            "a failed apply must not move the revision"
        );
        assert_eq!(replica.model().generation(), generation);
    }

    #[test]
    fn extensions_needed_follows_the_hash_and_not_the_key() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        set_extension(&editor, session, "acme.thumb", b"first");
        set_json_extension(&editor, session, "acme.meta");
        let structure = structure_of(&editor, session);

        let mut replica = ReplicaState::new();
        let needed = replica.extensions_needed(&structure).unwrap();
        assert_eq!(needed.len(), 1, "needed {needed:?}");
        assert_eq!(needed[0].key.as_str(), "acme.thumb");
        assert_eq!(needed[0].hash, extension_hash(b"first"));

        replica.put_extension("acme.thumb", b"first".to_vec());
        assert!(replica.extensions_needed(&structure).unwrap().is_empty());
        assert!(replica.apply_structure(&structure, 1).unwrap());
        assert!(replica.extensions_needed(&structure).unwrap().is_empty());
        assert_eq!(
            replica
                .model()
                .extensions()
                .get("acme.thumb")
                .and_then(|v| v.bytes().cloned())
                .as_deref(),
            Some(&b"first"[..]),
        );

        set_extension(&editor, session, "acme.thumb", b"second");
        let moved = structure_of(&editor, session);
        let needed = replica.extensions_needed(&moved).unwrap();
        assert_eq!(needed.len(), 1, "needed {needed:?}");
        assert_eq!(needed[0].hash, extension_hash(b"second"));

        assert!(replica.apply_structure(&moved, 2).is_err());
        replica.put_extension("acme.thumb", b"second".to_vec());
        assert!(replica.apply_structure(&moved, 2).unwrap());
    }

    #[test]
    fn an_unrelated_edit_needs_no_extension_fetch() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        set_extension(&editor, session, "acme.thumb", b"held");

        let mut replica = ReplicaState::new();
        replica.put_extension("acme.thumb", b"held".to_vec());
        assert!(
            replica
                .apply_structure(&structure_of(&editor, session), 1)
                .unwrap()
        );

        add_node(&editor, session, ROOT, "group", "head");
        let after = structure_of(&editor, session);
        assert!(replica.extensions_needed(&after).unwrap().is_empty());
        assert!(replica.apply_structure(&after, 2).unwrap());
    }

    fn set_extension(editor: &CatchlightEditor, session: SessionId, key: &str, bytes: &[u8]) {
        let mut attachments = Attachments::none();
        attachments.insert("value", bytes.to_vec());
        let (reply, _) = editor.dispatch_with(
            &json!({"id": 90, "cmd": "extension_set", "session": session.0,
                    "key": key, "value": {"kind": "bytes"}})
            .to_string(),
            attachments,
        );
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
    }

    fn set_json_extension(editor: &CatchlightEditor, session: SessionId, key: &str) {
        let reply = call(
            editor,
            json!({"id": 91, "cmd": "extension_set", "session": session.0,
                   "key": key, "value": {"kind": "json", "value": {"v": 1}}}),
        );
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
    }

    #[test]
    fn textures_needed_lists_the_missing_and_nothing_else() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let texture = add_part_with_texture(&editor, session);
        let structure = structure_of(&editor, session);

        let mut replica = ReplicaState::new();
        let needed = replica.textures_needed(&structure).unwrap();
        assert_eq!(needed.len(), 1, "needed {needed:?}");
        assert_eq!(needed[0].id.as_str(), texture);

        replica.put_texture(&texture, one_pixel_png());
        assert!(replica.textures_needed(&structure).unwrap().is_empty());
        assert!(replica.apply_structure(&structure, 1).unwrap());
        assert!(
            replica.textures_needed(&structure).unwrap().is_empty(),
            "an applied structure's payloads are still held",
        );
    }

    #[test]
    fn sync_from_editor_follows_the_session_it_reads() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let mut replica = ReplicaState::new();

        assert_eq!(replica.sync_from_editor(editor.editor(), session), 0);

        add_node(&editor, session, ROOT, "group", "head");
        assert_eq!(replica.sync_from_editor(editor.editor(), session), 1);

        let tree: Value = serde_json::from_str(
            &replica.query(&json!({"id": 5, "cmd": "node_tree_get", "session": 0}).to_string()),
        )
        .unwrap();
        assert_eq!(tree["reply"], "ok", "reply was {tree}");
        assert_eq!(tree["rev"], 1);
        let children = tree["body"]["root"]["children"].as_array().unwrap();
        assert!(
            children.iter().any(|c| c["name"] == "head"),
            "tree was {tree}",
        );

        assert_eq!(replica.sync_from_editor(editor.editor(), session), 1);
    }

    #[test]
    fn a_replica_answers_the_mesh_counts_a_node_carries() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let part = add_node(&editor, session, ROOT, "part", "face");
        let meshed = call(
            &editor,
            json!({"id": 4, "cmd": "mesh_set", "session": session.0, "node": part,
                   "verts": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                   "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
                   "indices": [[0, 1, 2]], "origin": [0.0, 0.0]}),
        );
        assert_eq!(meshed["reply"], "ok", "reply was {meshed}");

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        let info: Value = serde_json::from_str(&replica.query(
            &json!({"id": 9, "cmd": "node_get", "session": session.0, "node": part}).to_string(),
        ))
        .unwrap();
        assert_eq!(info["reply"], "ok", "reply was {info}");
        assert_eq!(info["body"]["node"]["vertex_count"], 3, "info was {info}");
        assert_eq!(info["body"]["node"]["triangle_count"], 1, "info was {info}");

        let group = add_node(&editor, session, ROOT, "group", "head");
        replica.sync_from_editor(editor.editor(), session);
        let info: Value = serde_json::from_str(&replica.query(
            &json!({"id": 10, "cmd": "node_get", "session": session.0, "node": group}).to_string(),
        ))
        .unwrap();
        assert!(
            info["body"]["node"]["vertex_count"].is_null(),
            "info was {info}"
        );
    }

    #[test]
    fn an_edit_command_is_refused_by_a_replica() {
        let replica = ReplicaState::new();
        let reply: Value = serde_json::from_str(
            &replica.query(
                &json!({"id": 11, "cmd": "node_add", "session": 1,
                    "parent": ROOT, "kind": "group", "name": "x"})
                .to_string(),
            ),
        )
        .unwrap();
        assert_eq!(reply["id"], 11, "reply was {reply}");
        assert_eq!(reply["code"], "bad_request");
        assert!(
            reply["message"].as_str().unwrap().contains("node_add"),
            "reply was {reply}",
        );
    }

    #[test]
    fn drain_events_yields_what_a_command_caused_and_then_nothing() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let opened = editor.drain_events();
        assert!(
            opened.iter().all(|e| e.contains("sessions_changed")),
            "events were {opened:?}",
        );
        assert!(
            editor.drain_events().is_empty(),
            "a drain empties the queue"
        );

        add_node(&editor, session, ROOT, "group", "head");
        let events = editor.drain_events();
        let changed: Vec<Value> = events
            .iter()
            .map(|e| serde_json::from_str(e).unwrap())
            .filter(|e: &Value| e["event"] == "model_changed")
            .collect();
        assert_eq!(changed.len(), 1, "events were {events:?}");
        assert_eq!(changed[0]["session"], session.0);
        assert_eq!(changed[0]["rev"], 1);
        assert!(editor.drain_events().is_empty());
    }

    #[test]
    fn a_scratch_transform_drops_the_fields_that_are_not_numbers() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let node = add_node(&editor, session, ROOT, "group", "head");

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);

        let nan = f32::NAN;
        assert!(
            replica.set_scratch_transform(&node, 1.0, 2.0, 3.0, nan, nan, nan, nan, nan, nan, 0.5,)
        );
        let idx = replica.node_idx(&node).unwrap();
        let scratch = *replica.puppet.scratch_transform(idx).unwrap();
        assert_eq!(scratch.translation, Some(Vec3::new(1.0, 2.0, 3.0)));
        assert_eq!(scratch.rotation, None);
        assert_eq!(scratch.scale, None);
        assert_eq!(scratch.z_order, None);
        assert_eq!(scratch.opacity, Some(0.5));

        assert!(replica.clear_scratch_transform(&node));
        assert!(!replica.clear_scratch_transform(&node));
        assert!(
            !replica.set_scratch_transform(
                "root/part-deadbeef",
                0.0,
                0.0,
                0.0,
                nan,
                nan,
                nan,
                nan,
                nan,
                nan,
                nan,
            ),
            "an unknown node is false, not a panic",
        );
    }

    #[test]
    fn a_node_world_transform_is_where_the_last_tick_put_it() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let node = add_node(&editor, session, ROOT, "group", "head");
        let moved = call(
            &editor,
            json!({"id": 3, "cmd": "node_set", "session": session.0,
                   "node": node, "translate": [4.0, 5.0, 0.0]}),
        );
        assert_eq!(moved["reply"], "ok", "reply was {moved}");

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.tick(0.0);

        let m = replica.node_world_transform(&node).unwrap();
        assert_eq!(m.len(), 16);
        // Column-major: the translation is the last column.
        assert_eq!((m[12], m[13]), (4.0, 5.0), "matrix was {m:?}");
        assert!(replica.node_world_transform("root/part-deadbeef").is_none());
    }

    #[test]
    fn a_world_delta_lands_in_the_parents_frame() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let parent = add_node(&editor, session, ROOT, "group", "torso");
        let child = add_node(&editor, session, &parent, "group", "head");
        call(
            &editor,
            json!({"id": 3, "cmd": "node_set", "session": session.0,
                   "node": parent, "scale": [2.0, 2.0]}),
        );
        call(
            &editor,
            json!({"id": 4, "cmd": "node_set", "session": session.0,
                   "node": child, "translate": [1.0, 0.0, 7.0]}),
        );

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.tick(0.0);

        let t = replica
            .translation_after_world_delta(&child, 2.0, 4.0)
            .unwrap();
        assert_eq!(t[0], 2.0, "x moved by half the world delta, got {t:?}");
        assert_eq!(t[1], 2.0, "y moved by half the world delta, got {t:?}");
        assert_eq!(t[2], 7.0, "a drag never touches the authored depth");

        let root = replica
            .translation_after_world_delta(ROOT, 3.0, 0.0)
            .unwrap();
        assert_eq!(root[0], 3.0, "root moved by the world delta, got {root:?}");

        assert!(
            replica
                .translation_after_world_delta("root/part-deadbeef", 1.0, 1.0)
                .is_none()
        );
    }

    fn add_drawn_part(editor: &CatchlightEditor, session: SessionId, name: &str) -> String {
        let part = add_node(editor, session, ROOT, "part", name);
        let reply = with_texture(editor, 20, session, &part);
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
        part
    }

    #[test]
    fn selection_geometry_and_picking_follow_pose_visibility_and_order() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let back = add_drawn_part(&editor, session, "back");
        let front = add_drawn_part(&editor, session, "front");
        set_quad_mesh(&editor, session, &back, 2.0);
        set_quad_mesh(&editor, session, &front, 1.0);
        node_set(
            &editor,
            session,
            &front,
            json!({"z_order": 2.0, "translate": [1.0, 0.0, 0.0], "scale": [2.0, 2.0]}),
        );
        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.tick(0.0);
        assert_eq!(replica.pick_node(0.0, 0.0), Some(front.clone()));
        assert_eq!(
            replica.node_vertices(&front).unwrap(),
            vec![-1.0, -2.0, 3.0, -2.0, 3.0, 2.0, -1.0, 2.0]
        );
        assert_eq!(replica.node_vertices(ROOT).unwrap().len(), 16);
        assert_eq!(
            replica.node_delta_from_world(&front, 4.0, 2.0),
            Some(vec![2.0, 1.0])
        );
        assert_eq!(
            replica.node_local_transform(&front).unwrap(),
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 2.0]
        );
        node_set(&editor, session, &front, json!({"enabled": false}));
        replica.sync_from_editor(editor.editor(), session);
        replica.tick(0.0);
        assert_eq!(replica.pick_node(0.0, 0.0), Some(back));
        assert_eq!(replica.node_vertices(ROOT).unwrap().len(), 8);
        assert!(replica.pick_node(100.0, 100.0).is_none());
        assert!(replica.node_vertices("missing").is_none());
        node_set(&editor, session, &front, json!({"scale": [0.0, 0.0]}));
        replica.sync_from_editor(editor.editor(), session);
        replica.tick(0.0);
        assert!(replica.node_delta_from_world(&front, 1.0, 1.0).is_none());
    }

    #[test]
    fn artwork_thumbnails_are_bounded_and_straight_alpha_on_both_formats() {
        for (encoding, format) in [
            ("png", image::ImageFormat::Png),
            ("tga", image::ImageFormat::Tga),
        ] {
            let editor = CatchlightEditor::new();
            let session = new_session(&editor);
            let part = add_node(&editor, session, ROOT, "part", "artwork");
            let mut bytes = Vec::new();
            image::RgbaImage::from_pixel(512, 256, image::Rgba([200, 80, 40, 128]))
                .write_to(&mut std::io::Cursor::new(&mut bytes), format)
                .unwrap();
            let mut attachments = Attachments::none();
            attachments.insert("texture", bytes);
            let (reply, _) = editor.dispatch_with(&json!({"id": 10, "cmd": "texture_add", "session": session.0, "node": part, "encoding": encoding}).to_string(), attachments);
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["reply"], "ok", "{reply}");
            let texture = reply["body"]["texture"].as_str().unwrap();
            let mut replica = ReplicaState::new();
            replica.sync_from_editor(editor.editor(), session);
            let thumbnail = replica.texture_thumbnail(texture).unwrap();
            assert_eq!((thumbnail.width, thumbnail.height), (128, 64));
            for (actual, expected) in thumbnail.rgba[..4].iter().zip([200u8, 80, 40, 128]) {
                assert!(
                    actual.abs_diff(expected) <= 2,
                    "{encoding}: {:?}",
                    &thumbnail.rgba[..4]
                );
            }
            assert!(replica.texture_thumbnail("missing").is_none());
            let listed = call(
                &editor,
                json!({"id": 11, "cmd": "texture_list", "session": session.0}),
            );
            assert_eq!(listed["body"]["textures"][0]["width"], 512);
            assert_eq!(listed["body"]["textures"][0]["height"], 256);
            // TGA has no magic bytes for a format sniffer. Both tracing and
            // dimension reads must use the texture's declared encoding.
            let meshed = call(
                &editor,
                json!({"id": 12, "cmd": "mesh_generate", "session": session.0, "node": part}),
            );
            assert_eq!(meshed["reply"], "ok", "{encoding}: {meshed}");
        }
    }

    fn set_quad_mesh(editor: &CatchlightEditor, session: SessionId, node: &str, half: f32) {
        let reply = call(
            editor,
            json!({"id": 21, "cmd": "mesh_set", "session": session.0, "node": node,
                   "verts": [[-half, -half], [half, -half], [half, half], [-half, half]],
                   "uvs": [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]],
                   "indices": [[0, 1, 2], [0, 2, 3]],
                   "origin": [0.0, 0.0]}),
        );
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
    }

    fn node_set(editor: &CatchlightEditor, session: SessionId, node: &str, patch: Value) {
        let mut body = json!({"id": 22, "cmd": "node_set", "session": session.0, "node": node});
        let (Some(body), Some(patch)) = (body.as_object_mut(), patch.as_object()) else {
            panic!("node_set takes an object patch");
        };
        for (k, v) in patch {
            body.insert(k.clone(), v.clone());
        }
        let reply = call(editor, Value::Object(body.clone()));
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
    }

    /// Bounds compare as exact floats: every number below is a sum of small
    /// powers of two, so the transform walk reproduces it bit for bit.
    fn assert_bounds(replica: &ReplicaState, expected: [f32; 4]) {
        assert_eq!(replica.bounds(), Some(expected));
    }

    #[test]
    fn bounds_cover_every_drawn_part_where_the_tick_left_it() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let face = add_drawn_part(&editor, session, "face");
        set_quad_mesh(&editor, session, &face, 1.0);
        node_set(
            &editor,
            session,
            &face,
            json!({"translate": [10.0, 20.0, 0.0]}),
        );

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(0.0);
        assert_bounds(&replica, [9.0, 19.0, 11.0, 21.0]);

        let hair = add_drawn_part(&editor, session, "hair");
        set_quad_mesh(&editor, session, &hair, 1.0);
        node_set(&editor, session, &hair, json!({"scale": [4.0, 2.0]}));
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(16.0);
        assert_bounds(&replica, [-4.0, -2.0, 11.0, 21.0]);

        node_set(&editor, session, &hair, json!({"enabled": false}));
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(32.0);
        assert_bounds(&replica, [9.0, 19.0, 11.0, 21.0]);

        node_set(
            &editor,
            session,
            &hair,
            json!({"enabled": true, "opacity": 0.0}),
        );
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(48.0);
        assert_bounds(&replica, [9.0, 19.0, 11.0, 21.0]);
    }

    #[test]
    fn bounds_are_of_the_posed_mesh_and_not_the_rest_one() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let face = add_drawn_part(&editor, session, "face");
        set_quad_mesh(&editor, session, &face, 1.0);

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(0.0);
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);

        // No tick in between: the write combines.
        assert!(replica.set_scratch_deform(&face, &[0.0, 0.0, 0.0, 0.0, 4.0, 8.0, 0.0, 0.0]));
        assert_bounds(&replica, [-1.0, -1.0, 5.0, 9.0]);

        assert!(replica.clear_scratch_deform(&face));
        replica.frame(16.0);
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
    }

    fn deform_generation(replica: &ReplicaState, node: &str) -> u64 {
        let idx = replica.node_idx(node).expect("the node was baked");
        let Some(NodeKind::Part(part)) = replica.puppet.get(idx).map(|n| &n.kind) else {
            panic!("{node} is not a baked part");
        };
        part.deform_stack.generation()
    }

    #[test]
    fn a_scratch_deform_reaches_the_frame_with_no_pose_change() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let face = add_drawn_part(&editor, session, "face");
        set_quad_mesh(&editor, session, &face, 1.0);

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(0.0);
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
        let at_rest = deform_generation(&replica, &face);

        assert!(replica.set_scratch_deform(&face, &[0.0, 0.0, 0.0, 0.0, 4.0, 8.0, 0.0, 0.0]));
        assert_bounds(&replica, [-1.0, -1.0, 5.0, 9.0]);
        let dragged = deform_generation(&replica, &face);
        assert_ne!(
            dragged, at_rest,
            "the deform generation is what gates the upload; unmoved, the \
             renderer goes on drawing the rest mesh",
        );

        assert!(!replica.frame(16.0));
        assert_bounds(&replica, [-1.0, -1.0, 5.0, 9.0]);
        assert_eq!(deform_generation(&replica, &face), dragged);

        assert!(replica.clear_scratch_deform(&face));
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
        assert_ne!(deform_generation(&replica, &face), dragged);
    }

    #[test]
    fn bounds_are_absent_when_nothing_is_drawn() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);

        let mut replica = ReplicaState::new();
        assert_eq!(replica.bounds(), None);
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(0.0);
        assert_eq!(replica.bounds(), None);

        let bare = add_node(&editor, session, ROOT, "part", "bare");
        set_quad_mesh(&editor, session, &bare, 1.0);
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(16.0);
        assert_eq!(replica.bounds(), None);

        let empty = add_drawn_part(&editor, session, "empty");
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(32.0);
        assert_eq!(replica.bounds(), None, "node {empty} has no vertices");
    }

    #[test]
    fn one_animation_frame_ticks_once_however_many_viewports_draw() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let param = call(
            &editor,
            json!({"id": 30, "cmd": "param_add", "session": session.0,
                   "name": "swing", "min": -1.0, "max": 1.0}),
        )["body"]["param"]
            .as_str()
            .unwrap()
            .to_string();
        let drive = call(
            &editor,
            json!({"id":31, "cmd":"param_add","session":session.0,"name":"drive","min":-1,"max":1}),
        )["body"]["param"]
            .as_str()
            .unwrap()
            .to_string();
        let driver = add_node(&editor, session, ROOT, "motion_response", "follow");
        let reply = call(
            &editor,
            json!({"id":32, "cmd":"motion_response_set","session":session.0,"node":driver,
            "response":{"inputs":[{"source":{"kind":"pose_param","param":drive},"gain":1}],"period_s":0.4,"damping_ratio":0.7,"output_param":param}}),
        );
        assert_eq!(reply["reply"], "ok");
        let swinging = |editor: &CatchlightEditor| {
            let mut replica = ReplicaState::new();
            replica.sync_from_editor(editor.editor(), session);
            replica.tick(0.0);
            replica
                .puppet
                .set_param_value(&ParamId::new(drive.as_str()).unwrap(), 1.0);
            replica
        };

        let mut alone = swinging(&editor);
        let mut shared = swinging(&editor);
        let mut two_clocks = swinging(&editor);

        let mut motions = Vec::new();
        let mut previous_ms = 0.0f64;
        for step in 0..30 {
            let now_ms = f64::from(step) * 16.0;
            alone.frame(now_ms);

            let first = shared.frame(now_ms);
            let second = shared.frame(now_ms);
            assert_eq!(
                first, second,
                "the second viewport must hear the same motion, or it goes clean \
                 and stops following a puppet that is still moving",
            );
            motions.push(first);

            let dt = ((now_ms - previous_ms) / 1000.0) as f32;
            two_clocks.tick(dt);
            two_clocks.tick(dt);
            previous_ms = now_ms;
        }

        let alone_value = alone.param_value(&param).expect("the driver claims it");
        let shared_value = shared.param_value(&param).expect("the driver claims it");
        assert_eq!(
            alone_value, shared_value,
            "a second viewport must not advance the puppet a second time",
        );
        assert_ne!(
            alone_value,
            two_clocks
                .param_value(&param)
                .expect("the driver claims it"),
            "two dt clocks must land somewhere else, or the assertion above \
             would hold however the puppet was ticked",
        );
        assert!(
            motions.iter().any(|moving| *moving),
            "the driver never reported motion, so nothing was being compared",
        );
    }

    #[test]
    fn a_frame_at_a_new_timestamp_steps_by_the_time_between_them() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let face = add_drawn_part(&editor, session, "face");
        set_quad_mesh(&editor, session, &face, 1.0);

        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        assert!(!replica.frame(0.0));
        assert!(!replica.frame(0.0));
        assert!(!replica.frame(16.0));
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn recording_captures_one_cell_without_baking_other_params_into_it() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let face = add_drawn_part(&editor, session, "record-face");
        set_quad_mesh(&editor, session, &face, 1.0);
        let send = |value: Value| {
            let mut value = value;
            value["id"] = json!(90);
            value["session"] = json!(session.0);
            let reply = call(&editor, value);
            assert_eq!(reply["reply"], "ok", "{reply}");
            reply
        };
        for param in ["x", "y", "other"] {
            send(json!({"cmd":"param_add", "param":param, "name":param,
                "min":0, "max":1, "default":0}));
        }
        send(json!({"cmd":"node_set", "node":face, "translate":[100,0,0], "scale":[2,2]}));
        for (param, param_y, cell, tx, sx) in [
            ("x", Some("y"), [1, 1], 10.0, 3.0),
            ("other", None, [1, 0], 20.0, 2.0),
        ] {
            let axes = if param_y.is_some() {
                json!([[0, 1], [0, 1]])
            } else {
                json!([[0, 1]])
            };
            for (target, value) in [
                ("tx", json!({"scalar": tx})),
                ("sx", json!({"scalar": sx})),
                ("deform", json!({"offsets": [[tx,0],[tx,0],[tx,0],[tx,0]]})),
            ] {
                send(
                    json!({"cmd":"edit_apply", "if_rev":editor.editor().revision(session).unwrap(), "edits":[
                        {"op":"binding_add", "node":face, "param":param, "param_y":param_y, "target":target, "key_positions":axes},
                        {"op":"binding_cells_set", "node":face, "param":param, "param_y":param_y, "target":target, "cells":[{"cell":cell,"value":value}]}
                    ]}),
                );
            }
        }
        let mut replica = ReplicaState::new();
        replica.sync_from_editor(editor.editor(), session);
        replica.set_editing(true);
        for p in ["x", "y", "other"] {
            replica.set_param(p, 1.0);
        }
        let revision = replica.rev();
        let recording = replica
            .recording(&face, "x", Some("y"), [1.0, 1.0])
            .unwrap();
        assert_eq!(replica.rev(), revision, "arming never authors");
        assert_eq!(recording.posed.translate.unwrap()[0], 130.0);
        assert_eq!(recording.posed.scale.unwrap()[0], 12.0);
        let patch = catchlight_editor_core::RecordProperties {
            translate: Some([133.0, 0.0, 0.0]),
            scale: Some([18.0, 2.0]),
            ..Default::default()
        };
        let entries = recording.patch(&patch, false).unwrap();
        assert_eq!(
            entries,
            vec![
                (catchlight_core::ScalarTarget::Tx, 13.0),
                (catchlight_core::ScalarTarget::Sx, 4.5)
            ]
        );
        let offsets = recording
            .deform(&[5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])
            .unwrap();
        assert_eq!(offsets, vec![15.0, 0.0, 10.0, 0.0, 10.0, 0.0, 10.0, 0.0]);
        assert_eq!(
            replica.recording_pairs(&face, "y"),
            vec![["x".to_string(), "y".to_string()]]
        );
        replica.set_param("y", 0.25);
        assert!(
            replica
                .recording(&face, "x", Some("y"), [1.0, 1.0])
                .is_err()
        );
        send(json!({"cmd":"node_set", "node":face, "name":"renamed"}));
        replica.sync_from_editor(editor.editor(), session);
        assert!(
            !replica.puppet.motion_responses_enabled(),
            "rebaking keeps motion responses paused while editing"
        );
        replica.set_editing(false);
        assert!(replica.puppet.motion_responses_enabled());
    }

    #[test]
    fn authoring_commands_refuse_a_stale_revision_without_history_or_model_changes() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let face = add_drawn_part(&editor, session, "draft-face");
        set_quad_mesh(&editor, session, &face, 1.0);
        let revision = editor.editor().revision(session).unwrap();
        let before = structure_of(&editor, session);
        let status = call(
            &editor,
            json!({"id":80,"cmd":"session_get","session":session.0}),
        );
        let commands = [
            json!({"cmd":"mesh_set","verts":[[0,0],[1,0],[1,1]],"uvs":[[0,0],[1,0],[1,1]],"indices":[[0,1,2]],"origin":[0,0]}),
            json!({"cmd":"binding_cells_set","param":"missing","target":"tx","cells":[{"cell":[0,0],"value":{"scalar":20}}]}),
            json!({"cmd":"binding_cells_set","param":"missing","target":"deform","cells":[{"cell":[0,0],"value":{"offsets":[[0,0],[0,0],[0,0],[0,0]]}}]}),
        ];
        for mut command in commands {
            command["id"] = json!(81);
            command["session"] = json!(session.0);
            command["node"] = json!(face);
            command["if_rev"] = json!(revision - 1);
            let reply = call(&editor, command);
            assert_eq!(reply["reply"], "err", "{reply}");
            assert_eq!(reply["code"], "revision_conflict", "{reply}");
            assert_eq!(editor.editor().revision(session), Some(revision));
            assert_eq!(structure_of(&editor, session), before);
            assert_eq!(
                call(
                    &editor,
                    json!({"id":80,"cmd":"session_get","session":session.0})
                ),
                status
            );
        }
    }

    /// The smallest valid PNG: one opaque pixel.
    fn one_pixel_png() -> Vec<u8> {
        use std::io::Cursor;
        let mut bytes = Vec::new();
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .unwrap();
        bytes
    }
}
