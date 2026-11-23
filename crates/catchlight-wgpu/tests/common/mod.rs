//! Shared helpers for the wgpu integration tests. Synthetic textures are PNGs
//! and get alpha-cropped like a `.clm`'s; read exact texels away from the
//! crop boundary.
#![allow(dead_code)]

use catchlight_core::formats::clm::{ClmIndices, ClmMesh};
use catchlight_core::formats::clm::{TextureAlpha, TextureEncoding};
use catchlight_core::{
    Mesh, Model, ModelNode, ModelNodeKind, ModelPart, ModelTexture, NodeId, Puppet, SeededHex,
    TexId,
};
use catchlight_wgpu::{
    create_headless_context, PrepareOptions, RenderCache, RenderContext, RenderList, WgpuRenderer,
};
use std::collections::HashMap;
use std::sync::OnceLock;

pub const NO_ADAPTER: &str =
    "no Vulkan adapter for the headless context; see AGENTS.md, \"Native headless rendering\"";

/// One device per test binary, never destroyed: NVIDIA's Vulkan driver can
/// hang in `vkDestroyDevice` while other threads are inside the driver.
pub fn shared_context() -> (wgpu::Device, wgpu::Queue) {
    static CONTEXT: OnceLock<(wgpu::Device, wgpu::Queue)> = OnceLock::new();
    CONTEXT
        .get_or_init(|| pollster::block_on(create_headless_context()).expect(NO_ADAPTER))
        .clone()
}

pub fn render_context(width: u32, height: u32) -> RenderContext {
    let (device, queue) = shared_context();
    let renderer = pollster::block_on(WgpuRenderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ));
    RenderContext::with_renderer(renderer, width, height).expect("headless render target")
}

/// Straight alpha, as an editor writes.
pub fn solid_texture(width: u32, height: u32, rgba: [u8; 4]) -> ModelTexture {
    let pixels: Vec<u8> = (0..width * height).flat_map(|_| rgba).collect();
    png_texture(width, height, &pixels)
}

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
        data: data.into_inner().into(),
    }
}

pub fn quad(width: f32, height: f32) -> ClmMesh {
    mesh_to_clm(&Mesh::quad(width, height))
}

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

pub struct Build {
    pub model: Model,
    pub hex: SeededHex,
    /// Uploaded by the first [`Build::part`] that names it.
    pending: HashMap<TexId, ModelTexture>,
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
            pending: HashMap::new(),
        }
    }

    pub fn root(&self) -> NodeId {
        self.model.root().expect("a complete model").clone()
    }

    pub fn texture(&mut self, texture: ModelTexture) -> TexId {
        let id = std::iter::repeat_with(|| TexId::generate(&mut self.hex))
            .find(|id| !self.pending.contains_key(id) && self.model.texture(id).is_none())
            .expect("a free texture id");
        self.pending.insert(id.clone(), texture);
        id
    }

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
        match self.pending.remove(albedo) {
            Some(texture) => self
                .model
                .add_texture_with_id(albedo.clone(), &id, texture)
                .expect("add texture"),
            None => self
                .model
                .set_part_albedo(&id, Some(albedo.clone()))
                .expect("set albedo"),
        }
        id
    }
}

pub struct Scene {
    pub model: Model,
    pub puppet: Puppet,
    pub cache: RenderCache,
}

impl Scene {
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

    pub fn frame(&mut self, renderer: &mut WgpuRenderer, dt: f32) -> RenderList {
        self.puppet.tick(&self.model, dt);
        self.cache
            .refresh(renderer, &self.model, &self.puppet)
            .expect("refresh the render cache");
        catchlight_wgpu::collect(&self.cache, &self.puppet)
    }
}
