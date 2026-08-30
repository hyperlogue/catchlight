//! Shared helpers for the wgpu integration tests: building a small [`Model`]
//! by hand, and driving one through a [`RenderCache`].
//!
//! A model keeps its textures source-encoded, so a synthetic texture is
//! written as a PNG here rather than handed over as raw bytes. That is the
//! same path a `.clm` takes, alpha crop and all — a test that wants exact
//! texels reads away from the crop's boundary.
#![allow(dead_code)]

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::formats::clm::{TextureAlpha, TextureEncoding};
use catchlight_core::{
    Mesh, Model, ModelNode, ModelNodeKind, ModelPart, ModelTexture, NodeId, Puppet, SeededHex,
    TexId,
};
use catchlight_wgpu::{PrepareOptions, RenderCache, RenderList, WgpuRenderer};
use std::sync::Arc;

pub const NO_ADAPTER: &str =
    "no Vulkan adapter for the headless context; see AGENTS.md, \"Native headless rendering\"";

/// A `width` x `height` texture of one colour, PNG-encoded the way a model
/// stores it. Straight alpha, which is what an editor writes.
pub fn solid_texture(width: u32, height: u32, rgba: [u8; 4]) -> ModelTexture {
    let pixels: Vec<u8> = (0..width * height).flat_map(|_| rgba).collect();
    png_texture(width, height, &pixels)
}

/// PNG-encode `pixels` (straight-alpha RGBA8, row-major) as a model texture.
pub fn png_texture(width: u32, height: u32, pixels: &[u8]) -> ModelTexture {
    let image = image::RgbaImage::from_raw(width, height, pixels.to_vec())
        .expect("pixel buffer matches the dimensions");
    let mut data = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut data, image::ImageFormat::Png)
        .expect("encode png");
    ModelTexture {
        encoding: TextureEncoding::Png,
        alpha: TextureAlpha::Straight,
        data: Arc::new(data.into_inner()),
    }
}

/// The model form of [`Mesh::quad`] — a `width` x `height` quad centred on
/// the origin, with UVs spanning the whole texture.
pub fn quad(width: f32, height: f32) -> ClmMesh {
    mesh_to_clm(&Mesh::quad(width, height))
}

/// A `components::Mesh` in the form a model stores.
pub fn mesh_to_clm(mesh: &Mesh) -> ClmMesh {
    ClmMesh {
        verts: mesh.vertices.iter().flat_map(|v| [v.x, v.y]).collect(),
        uvs: mesh.uvs.iter().flat_map(|v| [v.x, v.y]).collect(),
        indices: match &mesh.indices {
            catchlight_core::MeshIndices::U16(v) => ClmIndices::U16(v.clone()),
            catchlight_core::MeshIndices::U32(v) => ClmIndices::U32(v.clone()),
        },
        origin: [mesh.origin.x, mesh.origin.y],
    }
}

/// Builds a model with generated Ids, so a test says what it means rather
/// than bookkeeping Ids it never reads.
pub struct Build {
    pub model: Model,
    /// Public so a test can keep minting Ids after the model has moved into
    /// a [`Scene`] — an edit between frames is a case worth writing.
    pub hex: SeededHex,
}

impl Default for Build {
    fn default() -> Self {
        Self::new()
    }
}

impl Build {
    pub fn new() -> Self {
        Self {
            model: Model::new(),
            hex: SeededHex::new(1),
        }
    }

    pub fn root(&self) -> NodeId {
        self.model.root().expect("a complete model").clone()
    }

    pub fn texture(&mut self, texture: ModelTexture) -> TexId {
        self.model
            .add_texture(texture, &mut self.hex)
            .expect("add texture")
    }

    /// Add `kind` under `parent` at `z_order`.
    pub fn node(
        &mut self,
        parent: &NodeId,
        name: &str,
        z_order: f32,
        kind: ModelNodeKind,
    ) -> NodeId {
        let mut node = ModelNode::new(name, kind);
        node.z_order = z_order;
        self.model
            .add_node(parent, node, &mut self.hex)
            .expect("add node")
    }

    /// Add a part drawing `mesh` with `albedo`.
    pub fn part(
        &mut self,
        parent: &NodeId,
        name: &str,
        z_order: f32,
        mesh: ClmMesh,
        albedo: &TexId,
        configure: impl FnOnce(&mut ModelPart),
    ) -> NodeId {
        let mut part = ModelPart::new(mesh);
        configure(&mut part);
        let id = self.node(parent, name, z_order, ModelNodeKind::Part(part));
        self.model
            .set_part_albedo(&id, Some(albedo.clone()))
            .expect("set albedo");
        id
    }
}

/// A model, its puppet and the cache that holds its GPU state: the three
/// things every render needs.
pub struct Scene {
    pub model: Model,
    pub puppet: Puppet,
    pub cache: RenderCache,
}

impl Scene {
    /// Prepare `model` against `renderer` and build a puppet for it.
    pub fn new(renderer: &mut WgpuRenderer, model: Model) -> Self {
        let cache = RenderCache::prepare(renderer, &model, PrepareOptions::default())
            .expect("prepare the render cache");
        let puppet = Puppet::new(&model);
        Self {
            model,
            puppet,
            cache,
        }
    }

    /// Prepare `model` with a non-default texture budget.
    pub fn with_options(
        renderer: &mut WgpuRenderer,
        model: Model,
        options: PrepareOptions,
    ) -> Self {
        let cache =
            RenderCache::prepare(renderer, &model, options).expect("prepare the render cache");
        let puppet = Puppet::new(&model);
        Self {
            model,
            puppet,
            cache,
        }
    }

    /// Tick one frame, push its deforms, and collect the drawables.
    pub fn frame(&mut self, renderer: &mut WgpuRenderer, dt: f32) -> RenderList {
        self.puppet.tick(&self.model, dt);
        self.cache
            .refresh(renderer, &self.model, &self.puppet)
            .expect("refresh the render cache");
        catchlight_wgpu::collect(&self.cache, &self.puppet)
    }
}
