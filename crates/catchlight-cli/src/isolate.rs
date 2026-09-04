//! `isolate`: one part, or a handful, drawn alone into a straight-alpha PNG.
//!
//! This is how a pipeline extracts a part's art back out of a rig — an iris
//! as its whole disc rather than the sliver an eye-white mask leaves of it.
//! Every Part not named by `--keep` is disabled on the in-memory model; the
//! file on disk is never written.
//!
//! **Parts only are disabled, never a group or a composite.** The collector
//! ANDs `enabled` down the tree, so switching off an ancestor would take
//! every kept part under it with it.
//!
//! **A disabled part still stencils.** Mask sources are resolved straight
//! from the runtime and the collector does not consult `enabled` for them, so
//! a kept part stays clipped by a mask whose source is not kept. That is why
//! `--strip-masks` exists: it deletes those masks from the kept parts, and
//! only then does the clipped part render as its full art. An Id listed there
//! that masks nothing is not an error, so a script can pass every mask source
//! in a rig without first working out which ones bite.
//!
//! **The rect is world space, Y-up, `x0,y0` its minimum corner** — the bottom
//! left, since catchlight world space is Y-up and the camera holds no flip.
//! The camera is centred on it and frames `h` world units, and the output is
//! `round(w × S) × round(h × S)` pixels.
//!
//! **The unpremultiply happens here and not in `render`.** The frame is
//! cleared to transparent, so it comes back with partial alpha all over its
//! edges, and a PNG is straight alpha. `render` clears to opaque white, where
//! alpha is 1 everywhere and there is nothing to undo. The readback is
//! premultiplied *linear* under sRGB encoding, which is
//! [`catchlight_core::texture::unpremultiply_linear_from_srgb_inplace`] and
//! emphatically not the byte-space inverse beside it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use catchlight_core::texture::unpremultiply_linear_from_srgb_inplace;
use catchlight_core::{Model, ModelNodeKind, NodeId, ParamId, Pose, Puppet};
use catchlight_wgpu::{collect, Framing, PrepareOptions, RenderCache, RenderContext};

use crate::Error;

/// The world-space window an isolate frames: `x0,y0` is the minimum corner,
/// Y-up, and `w` by `h` extends from it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// `x0,y0,w,h`, as `--rect` spells it.
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut parts = text.split(',').map(|field| field.trim().parse::<f32>());
        let mut next = || parts.next().transpose().ok().flatten();
        match (next(), next(), next(), next(), parts.next()) {
            (Some(x0), Some(y0), Some(w), Some(h), None) if w > 0.0 && h > 0.0 => {
                Ok(Self { x0, y0, w, h })
            }
            _ => Err(Error::BadValue {
                field: "--rect".into(),
                expected: "four numbers x0,y0,w,h with a positive width and height".into(),
                value: text.to_string(),
            }),
        }
    }

    fn center(self) -> glam::Vec2 {
        glam::Vec2::new(self.x0 + self.w / 2.0, self.y0 + self.h / 2.0)
    }

    /// The pixel size this rect renders at, at least one pixel each way.
    fn size(self, scale: f32) -> (u32, u32) {
        let px = |units: f32| (units * scale).round().max(1.0) as u32;
        (px(self.w), px(self.h))
    }
}

/// What `isolate` was asked for, past the two paths.
#[derive(Debug, Clone)]
pub struct Request {
    /// Node Ids, each of which must name a Part. Never empty.
    pub keep: Vec<String>,
    /// Node Ids whose masks come off the kept parts. Need not match anything.
    pub strip_masks: Vec<String>,
    pub rect: Rect,
    pub scale: f32,
    /// `param=value`, repeated.
    pub set: Vec<String>,
}

/// What a run produced.
pub struct Isolated {
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    pub kept: usize,
    pub stripped: usize,
}

impl std::fmt::Display for Isolated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wrote {} ({}x{}): {} part{} kept, {} mask{} stripped",
            self.out.display(),
            self.width,
            self.height,
            self.kept,
            plural(self.kept),
            self.stripped,
            plural(self.stripped),
        )
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `param=value`, as `--set` spells it.
pub fn parse_set(text: &str) -> Result<(ParamId, f32), Error> {
    let (name, value) = text.rsplit_once('=').ok_or_else(|| Error::BadValue {
        field: "--set".into(),
        expected: "param=value".into(),
        value: text.to_string(),
    })?;
    let param = ParamId::new(name).map_err(|source| Error::BadId {
        value: name.to_string(),
        source,
    })?;
    let value = value.trim().parse::<f32>().map_err(|_| Error::BadValue {
        field: format!("--set {name}"),
        expected: "a number".into(),
        value: value.to_string(),
    })?;
    Ok((param, value))
}

/// Draw only `request.keep` from `path` into `out`.
pub fn run(path: &Path, out: &Path, request: &Request) -> Result<Isolated, Error> {
    let mut model = crate::file::load_model(path)?;

    let keep = parts_named(&model, path, &request.keep)?;
    let strip: BTreeSet<NodeId> = request
        .strip_masks
        .iter()
        .map(|id| node_id(id))
        .collect::<Result<_, _>>()?;
    let pose = pose_from(&model, path, &request.set)?;

    hide_all_but(&mut model, &keep)?;
    let stripped = strip_masks(&mut model, &keep, &strip)?;

    let (width, height) = request.rect.size(request.scale);
    let mut ctx = pollster::block_on(RenderContext::new(width, height))
        .map_err(|e| Error::gpu("gpu init", e))?;
    let mut cache = RenderCache::prepare(&mut ctx.renderer, &model, PrepareOptions::default())
        .map_err(|e| Error::gpu("prepare", e))?;

    let mut puppet = Puppet::new(&model);
    // Never ticked, so a param a pendulum drives holds whatever `--set` said.
    puppet.set_physics_enabled(false);
    puppet.apply_pose(&pose);
    puppet.tick(&model, 0.0);
    cache
        .refresh(&mut ctx.renderer, &model, &puppet)
        .map_err(|e| Error::gpu("refresh", e))?;
    let render_list = collect(&cache, &puppet);

    let mut pixels = ctx
        .render_rgba(
            &render_list,
            Framing {
                center: request.rect.center(),
                height: request.rect.h,
            },
            width,
            height,
            Some(wgpu::Color::TRANSPARENT),
        )
        .map_err(|e| Error::gpu("render", e))?;
    unpremultiply_linear_from_srgb_inplace(&mut pixels);

    image::save_buffer(out, &pixels, width, height, image::ColorType::Rgba8).map_err(|source| {
        Error::Png {
            path: out.to_path_buf(),
            source,
        }
    })?;

    Ok(Isolated {
        out: out.to_path_buf(),
        width,
        height,
        kept: keep.len(),
        stripped,
    })
}

fn node_id(text: &str) -> Result<NodeId, Error> {
    NodeId::new(text).map_err(|source| Error::BadId {
        value: text.to_string(),
        source,
    })
}

/// The Ids `--keep` names, each checked to be a Part of `model`.
fn parts_named(model: &Model, path: &Path, keep: &[String]) -> Result<BTreeSet<NodeId>, Error> {
    keep.iter()
        .map(|text| {
            let id = node_id(text)?;
            match model.node(&id).map(|node| &node.kind) {
                Some(ModelNodeKind::Part(_)) => Ok(id),
                // A group or a composite is refused by the same message: what
                // `--keep` takes is a Part, and nothing else is one.
                _ => Err(Error::NoSuchId {
                    path: path.to_path_buf(),
                    kind: "part",
                    id: text.to_string(),
                }),
            }
        })
        .collect()
}

fn pose_from(model: &Model, path: &Path, set: &[String]) -> Result<Pose, Error> {
    let mut pose = Pose::new();
    for text in set {
        let (param, value) = parse_set(text)?;
        if model.param(&param).is_none() {
            return Err(Error::NoSuchId {
                path: path.to_path_buf(),
                kind: "param",
                id: param.to_string(),
            });
        }
        pose.set(param, value);
    }
    Ok(pose)
}

/// Disable every Part that is not kept. Only Parts: see the module doc.
fn hide_all_but(model: &mut Model, keep: &BTreeSet<NodeId>) -> Result<(), Error> {
    let hide: Vec<NodeId> = model
        .nodes_in_order()
        .into_iter()
        .filter(|id| {
            !keep.contains(id)
                && matches!(
                    model.node(id).map(|node| &node.kind),
                    Some(ModelNodeKind::Part(_))
                )
        })
        .collect();
    for id in hide {
        model.update_node(&id, |node| node.enabled = false)?;
    }
    Ok(())
}

/// Drop every mask on a kept part whose source is listed, and say how many
/// went. Indices descend so each removal leaves the rest where they were.
fn strip_masks(
    model: &mut Model,
    keep: &BTreeSet<NodeId>,
    strip: &BTreeSet<NodeId>,
) -> Result<usize, Error> {
    if strip.is_empty() {
        return Ok(0);
    }
    let mut stripped = 0;
    for id in keep {
        let Some(ModelNodeKind::Part(part)) = model.node(id).map(|node| &node.kind) else {
            continue;
        };
        let doomed: Vec<usize> = part
            .masks()
            .iter()
            .enumerate()
            .filter(|(_, mask)| strip.contains(mask.source()))
            .map(|(index, _)| index)
            .rev()
            .collect();
        for index in doomed {
            model.mask_delete(id, index)?;
            stripped += 1;
        }
    }
    Ok(stripped)
}
