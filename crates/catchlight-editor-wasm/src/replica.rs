//! The tab's own copy of one session: [`ReplicaState`].
//!
//! A replica is what makes the browser editor feel local. It holds the
//! session's [`Model`], a [`Puppet`] posing it, and the texture payloads both
//! were built from, so the tab answers a read and draws a frame without a
//! round trip. It is a *copy*: the server's session is the document, and this
//! follows it.
//!
//! Invariants this module carries:
//!
//! - **The revision only moves forward.** Every feed carries the revision it
//!   is, and one at or below the revision already held is dropped whole —
//!   nothing is applied, nothing is half-applied. Structure pushes and
//!   reconnects arrive out of order over a live connection, and a stale one
//!   that overwrote a newer document would show the user an edit being undone.
//!   The one exception is a **pristine** replica, whose revision is not `0`
//!   but *absent*: a session that has never been edited is itself at rev `0`,
//!   so a replica that refused it would sit empty forever.
//!
//! - **There are exactly two ways the document changes**, and neither is
//!   local. [`ReplicaState::apply_structure`] takes a structure-only container
//!   the server pushed; [`ReplicaState::sync_from_editor`] takes an in-tab
//!   [`Editor`]'s own model. Nothing else in this crate writes to the model.
//!   The tab authors commands, never documents — that is what keeps one
//!   [`catchlight_editor_server::replica_query`] answering for both ends.
//!
//! - **Pose and scratch live on the puppet and never reach the model.** A
//!   param value, a drag's scratch transform and a scratch deform are the
//!   frame the user is looking at, not the document: they survive the rebake a
//!   feed triggers precisely because the model does not carry them, and a
//!   commit turns into a command like any other edit. Writing them into the
//!   model would make every pointer move a document change and the next
//!   structure push would erase it. A scratch deform is also **summed before
//!   the write returns** — see [`ReplicaState::combine_scratch_deforms`].
//!
//! - **One tick per animation frame, however many canvases draw.** The puppet
//!   belongs to the replica, not to a viewport, so advancing it does too:
//!   [`ReplicaState::frame`] takes the animation frame's own timestamp and
//!   ticks only when that timestamp moved, deriving `dt` from the last tick.
//!   Two viewports on one session are two callbacks in one
//!   `requestAnimationFrame` batch carrying the same `DOMHighResTimeStamp`, so
//!   the second one finds the puppet already advanced and only redraws it —
//!   otherwise a second canvas would double every session's physics and
//!   animation rate. The [`Motion`] the tick reported is remembered and
//!   returned to *every* caller that frames at that timestamp, not only to the
//!   one that ticked, or the viewports that did not tick would go clean and
//!   stop redrawing a puppet that is still moving.
//!
//! - **Gizmo math lives here.** TypeScript owns the gesture — which pointer,
//!   which modifier, when it started — and Rust owns everything that has to
//!   read the model to answer. [`ReplicaState::node_world_transform`] and
//!   [`ReplicaState::translation_after_world_delta`] are the two numbers a
//!   node drag needs, so the page never rebuilds a transform hierarchy it
//!   would then have to keep in step with the one that draws.
//!
//! - **Texture payloads are held by Id, beside the model.** A structure names
//!   its textures and carries none of their bytes, so the tab fetches what it
//!   lacks once and every later structure is applied over the same `Arc`s —
//!   which is what lets a render cache rebuild after an edit without decoding
//!   or re-uploading a single image. After a feed the
//!   held set is exactly what the model names: a payload the document dropped
//!   is dropped here too, or a replica would grow by the size of every texture
//!   the session ever had.

use std::collections::HashMap;
use std::sync::Arc;

use catchlight_core::formats::clm::{
    structure_texture_ids, ClmTextureRef, TextureAlpha, TextureEncoding,
};
use catchlight_core::{
    BlendMode, Mat4, Model, ModelTexture, Motion, NodeId, NodeKind, ParamId, Puppet,
    ScratchTransform, TexId, Vec2, Vec3,
};
use catchlight_editor_protocol::{ErrorCode, Reply, Request, RequestId, SessionId};
use catchlight_editor_server::{replica_reply, Editor};

/// One session's document and pose, in the tab. No GPU: everything here is
/// testable natively, and the renderer that draws it lives one layer up.
pub struct ReplicaState {
    model: Model,
    puppet: Puppet,
    /// The encoded bytes of every texture the tab has fetched, by Id. Shared
    /// with the model, so a rebuild is a pointer copy.
    textures: HashMap<TexId, Arc<[u8]>>,
    /// The revision this document reads as, or `None` while pristine.
    rev: Option<u64>,
    /// The animation-frame timestamp of the last tick, in milliseconds, or
    /// `None` before the first one. See [`ReplicaState::frame`].
    last_frame_ms: Option<f64>,
    /// What that tick reported, replayed to every later caller framing at the
    /// same timestamp.
    motion: Motion,
}

impl Default for ReplicaState {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicaState {
    /// An empty model, no textures, no revision.
    pub fn new() -> Self {
        let model = Model::new();
        let puppet = Puppet::new(&model);
        Self {
            model,
            puppet,
            textures: HashMap::new(),
            rev: None,
            last_frame_ms: None,
            motion: Motion::default(),
        }
    }

    /// The revision the document reads as; `0` while pristine.
    pub fn rev(&self) -> u64 {
        self.rev.unwrap_or(0)
    }

    /// Whether a feed at `rev` is newer than what is held. A pristine replica
    /// takes anything — see the module doc.
    fn accepts(&self, rev: u64) -> bool {
        self.rev.is_none_or(|held| rev > held)
    }

    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The pair a render cache is refreshed from — the same model the puppet
    /// was last baked against, which is what its generation gate checks.
    pub fn model_and_puppet(&self) -> (&Model, &Puppet) {
        (&self.model, &self.puppet)
    }

    // ---- feeding it --------------------------------------------------------

    /// The textures `structure` names that this replica does not hold, headers
    /// only, in the model's own order. Applies nothing.
    pub fn textures_needed(&self, structure: &[u8]) -> Result<Vec<ClmTextureRef>, String> {
        let named = structure_texture_ids(structure).map_err(|e| e.to_string())?;
        Ok(named
            .into_iter()
            .filter(|t| !self.textures.contains_key(&t.id))
            .collect())
    }

    /// Hold `bytes` under `id`, for a structure that is about to name it. An
    /// Id that is not one is dropped: the structure that wanted it will fail
    /// naming it, which says more than an error here would.
    pub fn put_texture(&mut self, id: &str, bytes: Vec<u8>) {
        if let Ok(id) = id.parse::<TexId>() {
            self.textures.insert(id, bytes.into());
        }
    }

    /// Rebuild the document from a structure-only container at `rev`, over the
    /// payloads already held.
    ///
    /// `Ok(false)` when `rev` is not newer, with nothing touched. `Err` when
    /// the structure is malformed or names a texture that was not put — also
    /// with nothing touched, because
    /// [`Model::replace_structure`] builds the new state whole before it swaps.
    pub fn apply_structure(&mut self, structure: &[u8], rev: u64) -> Result<bool, String> {
        if !self.accepts(rev) {
            return Ok(false);
        }
        let textures = &self.textures;
        self.model
            .replace_structure(structure, |id| {
                textures.get(id).map(|data| ModelTexture {
                    // `replace_structure` takes the payload and nothing else:
                    // the structure's own manifest says how each texture is
                    // read, and overrides both of these. Any value does.
                    encoding: TextureEncoding::Png,
                    alpha: TextureAlpha::Straight,
                    data: data.clone(),
                })
            })
            .map_err(|e| e.to_string())?;
        self.accept(rev);
        Ok(true)
    }

    /// Take the document straight from an in-tab [`Editor`]'s session, at that
    /// session's revision. The in-page path: no encode, no decode, and every
    /// texture payload `Arc`-shared with the editor.
    ///
    /// Returns the revision held afterwards, moved or not.
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

    /// Stamp the revision a feed just applied, drop the payloads the new
    /// document does not name, and rebake the puppet onto it.
    fn accept(&mut self, rev: u64) {
        self.rev = Some(rev);
        self.textures = self
            .model
            .texture_ids()
            .iter()
            .filter_map(|id| Some((id.clone(), self.model.texture(id)?.data.clone())))
            .collect();
        // Eagerly, not at the next tick: `node_idx` answers off the bake, and
        // a caller may pose or scratch a node the feed just added before a
        // frame ever runs.
        self.puppet.sync(&self.model);
    }

    // ---- reading it --------------------------------------------------------

    /// Answer one JSON [`Request`] against this document, in the same envelope
    /// [`crate::CatchlightEditor::handle`] uses. A command that is not a
    /// model-only query is `bad_request`, named.
    pub fn query(&self, request_json: &str) -> String {
        let reply = match serde_json::from_str::<Request>(request_json) {
            Ok(request) => replica_reply(&self.model, self.rev(), request),
            // Answer against the id the caller is waiting on, not against 0.
            Err(e) => Reply::Err {
                id: serde_json::from_str::<RequestId>(request_json)
                    .map(|r| r.id)
                    .unwrap_or(0),
                code: ErrorCode::BadRequest,
                message: e.to_string(),
            },
        };
        serde_json::to_string(&reply).unwrap_or_else(|_| {
            r#"{"reply":"err","id":0,"code":"bad_request","message":"reply could not be serialized"}"#
                .to_string()
        })
    }

    // ---- pose and scratch --------------------------------------------------

    /// Pose one param. `false` when the document has no such param.
    pub fn set_param(&mut self, id: &str, value: f32) -> bool {
        let Ok(id) = id.parse::<ParamId>() else {
            return false;
        };
        if !self.puppet.param_ids().any(|p| *p == id) {
            return false;
        }
        self.puppet.set_param_value(&id, value);
        true
    }

    /// The param's effective value, or `None` when nothing posed it and no
    /// driver claims it — which is also the answer for a param that is not
    /// there at all.
    pub fn param_value(&self, id: &str) -> Option<f32> {
        let id = id.parse::<ParamId>().ok()?;
        self.puppet.param_value(&id)
    }

    /// Write a node's scratch deform from a flat `[dx, dy, ...]` array. A
    /// trailing odd value is dropped rather than read as half a vertex.
    ///
    /// Flat where the wire's `scratch_deform` carries pairs: this is not the
    /// wire. A drag calls it once per pointer move from JavaScript, where the
    /// cheap thing to hand over is one `Float32Array`, and there is no parser
    /// in between for a pair to buy a check from.
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

    /// Write a node's scratch transform. Every field is absolute, and a field
    /// whose value is NaN is `None` — "leave what the fold produced". A vector
    /// field is dropped whole when any component is NaN, so a caller says
    /// "not this one" the same way for a scalar and for a triple.
    #[allow(clippy::too_many_arguments)]
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

    /// End every edit in progress at once — what a committed or abandoned
    /// gesture calls instead of remembering which nodes it touched.
    pub fn clear_all_scratch(&mut self) {
        self.puppet.clear_all_scratch();
        self.combine_scratch_deforms();
    }

    /// Sum every deform stack's active sources into the offsets a read and a
    /// draw actually look at, which is the caller's job and so this one's.
    ///
    /// A tick combines, but only on a frame that folded, and a scratch deform
    /// moves neither the pose nor the model: it sets the stack's dirty flag
    /// and nothing else. So on a still session — no physics, no animation, the
    /// pointer the only thing moving — no later tick would ever sum it, and
    /// the drag would reach neither [`Self::bounds`] nor the deform generation
    /// a render cache gates its upload on. Every scratch-deform write here
    /// ends with this, exactly as the server's `ScratchDeform` command does.
    ///
    /// Combining is a whole pass over the deform nodes, so it stays out of
    /// [`Puppet::set_scratch_deform`] itself: a caller writing several nodes —
    /// [`Puppet::clear_all_scratch`] is one — wants one pass, not one per
    /// node.
    fn combine_scratch_deforms(&mut self) {
        self.puppet.combine_deforms();
    }

    // ---- gizmo math --------------------------------------------------------

    /// The node's evaluated world transform after the last tick, as 16 floats
    /// in column-major order. `None` for a node the document does not have.
    ///
    /// This is where a handle is drawn, so it is the *evaluated* transform —
    /// bindings, physics, mesh groups and any standing scratch already folded
    /// in — and not the authored one. A puppet that has never ticked reports
    /// the identity, which is what an unposed model is.
    pub fn node_world_transform(&self, node: &str) -> Option<[f32; 16]> {
        let idx = self.node_idx(node)?;
        Some(self.puppet.transforms().get(idx).to_cols_array())
    }

    /// The node's **authored** local translation moved by a world-space delta,
    /// as `[x, y, z]`. `None` for a node the document does not have.
    ///
    /// This is the other half of a drag: the pointer moves in world units, and
    /// what a `node_set` patch commits is a local translation. So the delta is
    /// mapped through the inverse of the parent's evaluated world transform —
    /// as a *direction*, which takes the parent's rotation and scale and drops
    /// its position — and added to what the model actually stores. Z is left
    /// alone: a drag is two-dimensional and the authored depth is not its to
    /// change.
    ///
    /// Starting from the authored value rather than the evaluated one is what
    /// makes preview and commit agree: the same number goes to
    /// [`Self::set_scratch_transform`] while the pointer is down and into the
    /// patch when it lifts.
    ///
    /// A node with no parent is its own frame, so the delta is already local.
    /// A parent whose scale collapsed has no invertible frame and the node
    /// does not move, rather than moving to NaN and being committed there.
    pub fn translation_after_world_delta(&self, node: &str, dx: f32, dy: f32) -> Option<[f32; 3]> {
        let id = node.parse::<NodeId>().ok()?;
        let authored = self.model.node(&id)?.transform.translation;
        let parent = self
            .model
            .node(&id)
            .and_then(|n| n.parent())
            .and_then(|p| self.puppet.node_idx(p))
            .map(|idx| self.puppet.transforms().get(idx))
            .unwrap_or(Mat4::IDENTITY);
        let local = parent.inverse().transform_vector3(Vec3::new(dx, dy, 0.0));
        if !local.is_finite() {
            return Some(authored);
        }
        Some([authored[0] + local.x, authored[1] + local.y, authored[2]])
    }

    /// Evaluate one step of `dt` seconds. The raw primitive; a viewport goes
    /// through [`Self::frame`], which is what keeps two canvases on one
    /// session from stepping the same puppet twice.
    ///
    /// The [`Motion`] it returns is why a viewport stays dirty: a settled
    /// puppet reports none and the loop idles.
    pub fn tick(&mut self, dt: f32) -> Motion {
        self.puppet.tick(&self.model, dt)
    }

    /// Advance to the animation frame stamped `now_ms`, and report what is
    /// still moving.
    ///
    /// Ticks **at most once per timestamp**: `dt` is the distance from the
    /// last tick, and a second caller arriving at the same `now_ms` gets the
    /// puppet as that tick left it plus the same [`Motion`], having advanced
    /// nothing. Every viewport drawing this replica calls this once a frame,
    /// and `requestAnimationFrame` hands every callback in one batch the same
    /// `DOMHighResTimeStamp`, so equality is the whole signal — see the module
    /// doc.
    ///
    /// `dt` is clamped to 100 ms: a tab that was hidden comes back with a
    /// timestamp minutes later, and a puppet must resume rather than integrate
    /// the whole gap in one step.
    pub fn frame(&mut self, now_ms: f64) -> Motion {
        if self.last_frame_ms != Some(now_ms) {
            let dt = self
                .last_frame_ms
                .map(|last| ((now_ms - last) / 1000.0).clamp(0.0, 0.1) as f32)
                .unwrap_or(0.0);
            self.last_frame_ms = Some(now_ms);
            self.motion = self.tick(dt);
        }
        self.motion
    }

    /// The world-space axis-aligned box the last tick left the drawn geometry
    /// in, as `[min_x, min_y, max_x, max_y]`. `None` when nothing is drawn.
    ///
    /// A read like any other: bounds are a question about the document and the
    /// pose in front of the user, so the replica answers them synchronously
    /// off state it already holds, rather than the page asking the editor and
    /// waiting a round trip for a number it needs to place a camera.
    ///
    /// They are **posed** bounds — every vertex through its part's combined
    /// deform and its evaluated world transform, the same
    /// `world · (vertex − origin + deform)` the vertex shader computes — for
    /// the same reason: a fit frames what is on screen, and a rest-pose box
    /// would cut off a limb that physics or a param has swung outside it.
    ///
    /// What counts as drawn is what the render list counts: a part whose
    /// `enabled` holds all the way up the tree, whose albedo names a texture
    /// the document carries, and which opacity has not culled. Upload state is
    /// the one thing not consulted — that belongs to a render cache, and this
    /// half has no GPU.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        let tree = self.puppet.tree();
        let transforms = self.puppet.transforms();
        // `enabled` is ANDed down the tree, so a disabled group hides its whole
        // subtree. Depth-first visits a parent before its children, which is
        // what makes one pass enough.
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
            // A part with no albedo in the document draws nothing, and opacity
            // 0 culls every blend mode but Darken, whose Min blend ignores
            // blend factors — both exactly as the collector decides it.
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

/// NaN is the wire's "leave this field alone"; see
/// [`ReplicaState::set_scratch_transform`].
fn scalar(v: f32) -> Option<f32> {
    (!v.is_nan()).then_some(v)
}

fn vec2(x: f32, y: f32) -> Option<Vec2> {
    (!x.is_nan() && !y.is_nan()).then(|| Vec2::new(x, y))
}

fn vec3(x: f32, y: f32, z: f32) -> Option<Vec3> {
    (!x.is_nan() && !y.is_nan() && !z.is_nan()).then(|| Vec3::new(x, y, z))
}

// ---- the browser's replica ------------------------------------------------

/// [`ReplicaState`] with the GPU half attached: what JavaScript holds.
///
/// **The renderer arrives with the first viewport, and so does the device.**
/// A `RenderCache`'s slots name GPU state inside one renderer ("one cache, one
/// renderer"), so the cache is per replica and so is the renderer holding it —
/// but a renderer's pipelines are compiled against a surface format, and no
/// format exists until a canvas does. So a [`Replica`] is constructed with
/// nothing at all: it answers every query, holds every texture and poses its
/// puppet before any GPU exists, and `Viewport::new` is what hands it the
/// device, queue and adapter it will draw with. The cache follows one step
/// later still, on the first frame, because that is when a model worth
/// uploading is likeliest to have arrived.
#[cfg(target_arch = "wasm32")]
pub(crate) mod browser {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    use catchlight_core::Motion;
    use catchlight_editor_protocol::SessionId;
    use catchlight_wgpu::{Pipelines, PrepareOptions, RenderCache, RenderList, WgpuRenderer};
    use wasm_bindgen::prelude::*;

    use super::ReplicaState;
    use crate::{CatchlightEditor, Gpu};

    /// The renderer a replica draws through and the cache it draws from.
    pub(crate) struct ReplicaRender {
        pub(crate) renderer: WgpuRenderer,
        cache: Option<RenderCache>,
    }

    /// Everything one session's replica owns, shared with every viewport
    /// showing it. `Rc<RefCell<_>>` and not a lock: the browser runs the frame
    /// callback and the JS-facing methods on one thread with no overlap.
    pub(crate) struct ReplicaInner {
        pub(crate) state: ReplicaState,
        pub(crate) render: Option<ReplicaRender>,
    }

    impl ReplicaInner {
        /// The device this replica draws with, once a viewport has given it
        /// one — what that viewport reconfigures its surface against.
        pub(crate) fn device(&self) -> Option<&wgpu::Device> {
            self.render.as_ref().map(|r| &r.renderer.device)
        }

        /// The renderer for a surface of `format` on `gpu`, compiling its
        /// pipelines the first time.
        ///
        /// The two `Err`s are both "a second viewport disagrees with the
        /// first": a different device, or a different swapchain format. One
        /// [`Gpu`] per editor makes the first unreachable and one browser
        /// makes the second so, but the renderer's pipelines and the cache's
        /// slots would be wrong for the newcomer, so say it rather than draw
        /// it.
        pub(crate) fn ensure_renderer(
            &mut self,
            gpu: &Gpu,
            format: wgpu::TextureFormat,
        ) -> Result<&mut ReplicaRender, String> {
            let render = self.render.get_or_insert_with(|| ReplicaRender {
                // `new_autodetect`, not `new`: WebGL2 takes the shader
                // alpha-discard path because Chromium's software WebGL2 fails
                // to bind a Depth24PlusStencil8 attachment.
                // `Arc` because that is what a renderer shares pipelines
                // through; on wasm there is one thread and nothing to send,
                // exactly as in `WgpuRenderer::new`.
                #[allow(clippy::arc_with_non_send_sync)]
                renderer: WgpuRenderer::from_pipelines(
                    gpu.device.clone(),
                    gpu.queue.clone(),
                    Arc::new(Pipelines::new_autodetect(&gpu.adapter, &gpu.device, format)),
                ),
                cache: None,
            });
            if render.renderer.device != gpu.device {
                return Err("this replica is already drawing on another device; \
                     one editor holds one Gpu and every viewport shares it"
                    .to_string());
            }
            if render.renderer.shared.surface_format != format {
                return Err(format!(
                    "this replica's renderer draws {:?} and the canvas wants {format:?}; \
                     one tab shares one device and one swapchain format",
                    render.renderer.shared.surface_format,
                ));
            }
            Ok(render)
        }

        /// Advance the puppet to the animation frame stamped `now_ms` and fill
        /// `list` with what to draw. Returns what is still moving, so the
        /// caller stays dirty for it.
        ///
        /// The tick happens at most once per `now_ms`; a second viewport
        /// framing in the same `requestAnimationFrame` batch collects and
        /// draws the state the first one produced, and hears the same
        /// [`Motion`]. Collecting is per viewport because the list is: each
        /// canvas fills its own.
        pub(crate) fn frame(
            &mut self,
            now_ms: f64,
            list: &mut RenderList,
        ) -> Result<Motion, String> {
            let motion = self.state.frame(now_ms);
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
                return Ok(motion);
            };
            // Rebuilds itself when the model moved; a feed is the only thing
            // that moves it, and it keeps every texture whose payload it
            // already holds.
            cache
                .refresh(&mut render.renderer, model, puppet)
                .map_err(|e| e.to_string())?;
            cache.collect_into(puppet, list);
            Ok(motion)
        }
    }

    /// One session's model, puppet, textures and render cache, in the tab.
    #[wasm_bindgen]
    pub struct Replica {
        inner: Rc<RefCell<ReplicaInner>>,
    }

    impl Replica {
        /// A handle every viewport drawing this replica shares.
        pub(crate) fn inner(&self) -> Rc<RefCell<ReplicaInner>> {
            self.inner.clone()
        }
    }

    impl Default for Replica {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Revisions and session ids cross as `f64`: both are JSON `number`s on
    /// the wire, and a `u64` parameter would reach JavaScript as a `bigint`,
    /// so the one value would have two spellings and every call site would
    /// convert.
    #[wasm_bindgen]
    impl Replica {
        /// An empty model, no textures, no revision — and no GPU. The device
        /// arrives with the first viewport.
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            Self {
                inner: Rc::new(RefCell::new(ReplicaInner {
                    state: ReplicaState::new(),
                    render: None,
                })),
            }
        }

        /// The revision this document reads as; `0` while pristine.
        pub fn rev(&self) -> f64 {
            self.inner.borrow().state.rev() as f64
        }

        /// The textures `structure` names that are not held, as JSON
        /// `[{ id, encoding, alpha }]`. Applies nothing.
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

        /// Hold a texture payload under its Id, for a structure about to name
        /// it.
        #[wasm_bindgen(js_name = putTexture)]
        pub fn put_texture(&self, id: &str, bytes: Vec<u8>) {
            self.inner.borrow_mut().state.put_texture(id, bytes);
        }

        /// Apply a structure-only container at `rev`. `false` when `rev` is
        /// not newer; throws when the structure is malformed or names a
        /// texture that was not put, with the document untouched.
        #[wasm_bindgen(js_name = applyStructure)]
        pub fn apply_structure(&self, structure: &[u8], rev: f64) -> Result<bool, JsValue> {
            self.inner
                .borrow_mut()
                .state
                .apply_structure(structure, rev as u64)
                .map_err(|e| JsValue::from_str(&e))
        }

        /// Take the document from an in-tab editor's session. Returns the
        /// revision held afterwards.
        #[wasm_bindgen(js_name = syncFromEditor)]
        pub fn sync_from_editor(&self, editor: &CatchlightEditor, session: f64) -> f64 {
            self.inner
                .borrow_mut()
                .state
                .sync_from_editor(editor.editor(), SessionId(session as u64)) as f64
        }

        /// Answer one JSON `Request` against this document. A command that is
        /// not a model-only query is `bad_request`.
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
        #[allow(clippy::too_many_arguments)]
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

        /// The node's evaluated world transform after the last tick: 16 floats,
        /// column-major. Where a gizmo draws its handles.
        #[wasm_bindgen(js_name = nodeWorldTransform)]
        pub fn node_world_transform(&self, node: &str) -> Option<Vec<f32>> {
            self.inner
                .borrow()
                .state
                .node_world_transform(node)
                .map(|m| m.to_vec())
        }

        /// The world-space box the last tick left the drawn geometry in:
        /// `[min_x, min_y, max_x, max_y]` in world units, Y-up. `undefined`
        /// when the model draws nothing. What a "fit the model" camera is
        /// computed from.
        pub fn bounds(&self) -> Option<Vec<f32>> {
            self.inner.borrow().state.bounds().map(|b| b.to_vec())
        }

        /// The node's authored local translation moved by a world-space delta:
        /// 3 floats. What a drag previews and then commits.
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::CatchlightEditor;
    use catchlight_editor_server::Attachments;
    use serde_json::{json, Value};

    /// The root every `Model::new` starts with, which `node_add` parents to.
    const ROOT: &str = "root";

    fn call(editor: &CatchlightEditor, value: Value) -> Value {
        serde_json::from_str(&editor.handle(&value.to_string())).unwrap()
    }

    fn new_session(editor: &CatchlightEditor) -> SessionId {
        let reply = call(editor, json!({"id": 1, "cmd": "session_new"}));
        SessionId(reply["body"]["session"].as_u64().unwrap())
    }

    /// Adds a node under `parent` and returns its Id.
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

    /// A part with one texture on it, and the texture's Id.
    fn add_part_with_texture(editor: &CatchlightEditor, session: SessionId) -> String {
        let part = add_node(editor, session, ROOT, "part", "face");
        let reply = with_texture(editor, 3, session, &part);
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
        reply["body"]["texture"].as_str().unwrap().to_string()
    }

    /// `texture_add` with a one-pixel PNG attached — the one way an image
    /// reaches a model.
    fn with_texture(
        editor: &CatchlightEditor,
        id: u64,
        session: SessionId,
        node: &str,
    ) -> serde_json::Value {
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

    /// The structure push a server would send for this session.
    fn structure_of(editor: &CatchlightEditor, session: SessionId) -> Vec<u8> {
        editor
            .editor()
            .with_model(session, |m| m.to_structure_bytes())
            .unwrap()
            .unwrap()
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

        // The same revision, and an older one, are both dropped whole.
        assert!(!replica.apply_structure(&structure, 7).unwrap());
        assert!(!replica.apply_structure(&structure, 3).unwrap());
        assert_eq!(replica.rev(), 7);
        assert_eq!(replica.model().generation(), generation);
    }

    #[test]
    fn a_missing_texture_leaves_the_document_alone() {
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
    fn textures_needed_lists_the_missing_and_nothing_else() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);
        let texture = add_part_with_texture(&editor, session);
        let structure = structure_of(&editor, session);

        let mut replica = ReplicaState::new();
        let needed = replica.textures_needed(&structure).unwrap();
        assert_eq!(needed.len(), 1, "needed {needed:?}");
        assert_eq!(needed[0].id.as_str(), texture);

        // Held, so no longer needed — and an applied structure keeps it held.
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

        // A pristine replica takes the session as it stands, rev 0 and all.
        assert_eq!(replica.sync_from_editor(editor.editor(), session), 0);

        add_node(&editor, session, ROOT, "group", "head");
        assert_eq!(replica.sync_from_editor(editor.editor(), session), 1);

        let tree: Value = serde_json::from_str(
            &replica.query(&json!({"id": 5, "cmd": "node_tree", "session": 0}).to_string()),
        )
        .unwrap();
        assert_eq!(tree["reply"], "ok", "reply was {tree}");
        assert_eq!(tree["rev"], 1);
        let children = tree["body"]["root"]["children"].as_array().unwrap();
        assert!(
            children.iter().any(|c| c["name"] == "head"),
            "tree was {tree}",
        );

        // Nothing moved in the editor, so nothing moves here.
        assert_eq!(replica.sync_from_editor(editor.editor(), session), 1);
    }

    /// How big a node's mesh is reaches the tab without a round trip: the
    /// counts ride `node_info`, which the replica answers from its own model.
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
            &json!({"id": 9, "cmd": "node_info", "session": session.0, "node": part}).to_string(),
        ))
        .unwrap();
        assert_eq!(info["reply"], "ok", "reply was {info}");
        assert_eq!(info["body"]["node"]["vertex_count"], 3, "info was {info}");
        assert_eq!(info["body"]["node"]["triangle_count"], 1, "info was {info}");

        // A group could not hold a mesh, so it reports no count at all.
        let group = add_node(&editor, session, ROOT, "group", "head");
        replica.sync_from_editor(editor.editor(), session);
        let info: Value = serde_json::from_str(&replica.query(
            &json!({"id": 10, "cmd": "node_info", "session": session.0, "node": group}).to_string(),
        ))
        .unwrap();
        assert!(
            info["body"]["node"]["vertex_count"].is_null(),
            "info was {info}"
        );
    }

    #[test]
    fn a_document_command_is_refused_by_a_replica() {
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
        // session_new is a session change, not a document one.
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
            .filter(|e: &Value| e["event"] == "document_changed")
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
        // The parent doubles everything under it, so a world delta of 2 is a
        // local delta of 1.
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

        // A node with no parent is already its own frame.
        let root = replica
            .translation_after_world_delta(ROOT, 3.0, 0.0)
            .unwrap();
        assert_eq!(root[0], 3.0, "root moved by the world delta, got {root:?}");

        assert!(replica
            .translation_after_world_delta("root/part-deadbeef", 1.0, 1.0)
            .is_none());
    }

    /// A part named `name` under the root, with a texture of its own so the
    /// bounds walk counts it as drawn. Returns the node Id.
    fn add_drawn_part(editor: &CatchlightEditor, session: SessionId, name: &str) -> String {
        let part = add_node(editor, session, ROOT, "part", name);
        let reply = with_texture(editor, 20, session, &part);
        assert_eq!(reply["reply"], "ok", "reply was {reply}");
        part
    }

    /// A square of side `2 * half` centred on the node's own origin.
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

        // A second part widens the box, and its own scale reaches its vertices.
        let hair = add_drawn_part(&editor, session, "hair");
        set_quad_mesh(&editor, session, &hair, 1.0);
        node_set(&editor, session, &hair, json!({"scale": [4.0, 2.0]}));
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(16.0);
        assert_bounds(&replica, [-4.0, -2.0, 11.0, 21.0]);

        // A disabled part is not drawn, so it is not framed either.
        node_set(&editor, session, &hair, json!({"enabled": false}));
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(32.0);
        assert_bounds(&replica, [9.0, 19.0, 11.0, 21.0]);

        // Neither is one opacity culled.
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

        // Pull the top-right corner (vertex 2) out and the box follows it,
        // with no tick in between — the write combines. Bounds read exactly
        // what the render list reads.
        assert!(replica.set_scratch_deform(&face, &[0.0, 0.0, 0.0, 0.0, 4.0, 8.0, 0.0, 0.0]));
        assert_bounds(&replica, [-1.0, -1.0, 5.0, 9.0]);

        // Clearing deactivates the source, and an inactive stack contributes
        // nothing whether or not anything combined since.
        assert!(replica.clear_scratch_deform(&face));
        replica.frame(16.0);
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
    }

    /// The part's deform generation: what a render cache gates its upload on,
    /// so a deform that does not move it never reaches the screen.
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

        // Nothing here moves the pose: no param, no driver, no model edit. A
        // tick folds only when one of those did, so a write that did not
        // combine would leave this drag in its source slot forever — the bug
        // this is against.
        assert!(replica.set_scratch_deform(&face, &[0.0, 0.0, 0.0, 0.0, 4.0, 8.0, 0.0, 0.0]));
        assert_bounds(&replica, [-1.0, -1.0, 5.0, 9.0]);
        let dragged = deform_generation(&replica, &face);
        assert_ne!(
            dragged, at_rest,
            "the deform generation is what gates the upload; unmoved, the \
             renderer goes on drawing the rest mesh",
        );

        // The frames after it keep the drag: they fold nothing, and a scratch
        // source is the one source a fold would not rebuild anyway.
        assert_eq!(replica.frame(16.0), Motion::default());
        assert_bounds(&replica, [-1.0, -1.0, 5.0, 9.0]);
        assert_eq!(deform_generation(&replica, &face), dragged);

        // Clearing is the same story in reverse: the rest box is back before
        // any tick, and the generation moves so the renderer hears about it.
        assert!(replica.clear_scratch_deform(&face));
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
        assert_ne!(deform_generation(&replica, &face), dragged);
    }

    #[test]
    fn bounds_are_absent_when_nothing_is_drawn() {
        let editor = CatchlightEditor::new();
        let session = new_session(&editor);

        // An empty replica, and then a session with no geometry at all.
        let mut replica = ReplicaState::new();
        assert_eq!(replica.bounds(), None);
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(0.0);
        assert_eq!(replica.bounds(), None);

        // A part with a mesh but no texture draws nothing, so it frames
        // nothing.
        let bare = add_node(&editor, session, ROOT, "part", "bare");
        set_quad_mesh(&editor, session, &bare, 1.0);
        replica.sync_from_editor(editor.editor(), session);
        replica.frame(16.0);
        assert_eq!(replica.bounds(), None);

        // A textured part with no mesh has no vertices to frame either.
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
        let driver = call(
            &editor,
            json!({"id": 31, "cmd": "physics_add", "session": session.0, "parent": ROOT,
                   "kind": "rigid", "target_params": {"angle": param.clone()},
                   "length": 1.0, "gravity": 1.0}),
        );
        assert_eq!(driver["reply"], "ok", "reply was {driver}");
        let driver = driver["body"]["node"].as_str().unwrap().to_string();

        // A pendulum hanging straight down never moves, so displace the bob:
        // now every tick is a step of a swing whose position depends on how
        // much time the puppet was told has passed.
        let swinging = |editor: &CatchlightEditor| {
            let mut replica = ReplicaState::new();
            replica.sync_from_editor(editor.editor(), session);
            let idx = replica.node_idx(&driver).expect("the driver was baked");
            assert!(replica.puppet.place_driver(idx, Vec2::new(0.7, 0.7)));
            replica
        };

        // One canvas: one call per animation frame.
        let mut alone = swinging(&editor);
        // Two canvases on one replica: two calls carrying the same timestamp,
        // exactly as two `requestAnimationFrame` callbacks in one batch do.
        let mut shared = swinging(&editor);
        // The bug this is against: each viewport kept its own `last_ms`, so
        // each one measured a whole frame and stepped the puppet by it.
        let mut two_clocks = swinging(&editor);

        let mut motions = Vec::new();
        let mut previous_ms = 0.0f64;
        for step in 0..30 {
            let now_ms = step as f64 * 16.0;
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
        // The comparison above is only worth anything if a doubled tick would
        // have shown, and if the puppet moved at all.
        assert_ne!(
            alone_value,
            two_clocks
                .param_value(&param)
                .expect("the driver claims it"),
            "two dt clocks must land somewhere else, or the assertion above \
             would hold however the puppet was ticked",
        );
        assert!(
            motions.iter().any(|m: &Motion| m.physics),
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
        // A still puppet reports no motion however it is framed, and framing
        // it twice at one timestamp is not an error.
        assert_eq!(replica.frame(0.0), Motion::default());
        assert_eq!(replica.frame(0.0), Motion::default());
        assert_eq!(replica.frame(16.0), Motion::default());
        assert_bounds(&replica, [-1.0, -1.0, 1.0, 1.0]);
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
