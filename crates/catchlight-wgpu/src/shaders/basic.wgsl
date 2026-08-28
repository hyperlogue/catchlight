// Color textures arrive pre-normalised by the importer: bytes encoding
// premultiplied LINEAR color (sRGB-encoded byte = srgb_encode(linear *
// alpha)). Uploaded as `Rgba8UnormSrgb`, the sampler decodes sRGB→linear
// and returns the premultiplied linear value. The shader operates on the
// premultiplied sample directly and emits premultiplied output for the
// standard `(One, 1-SrcAlpha)` blend in linear space. See
// `catchlight_core::formats::ModelTexture::decode` for the importer-side
// normalisation.

struct Camera {
    view_proj: mat4x4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: Camera;

struct PartUniforms {
    opacity: f32,
    // .xyz is tint (multiply); .w is unused padding (keeps the field a vec4).
    tint: vec4<f32>,
    // .xyz is screen_tint, .w is the mask_threshold for the mask_main
    // fragment shader — packed here so the uniform stays three vec4s
    // and keeps std140 alignment trivial.
    screen_tint_and_threshold: vec4<f32>,
}

@group(2) @binding(0)
var<uniform> part: PartUniforms;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
}

struct DeformInput {
    @location(6) deform: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Model-matrix columns x_axis, y_axis, w_axis. The z_axis column is
// omitted: vertices enter at z = 0 so it never contributes.
struct InstanceInput {
    @location(2) col_x: vec4<f32>,
    @location(3) col_y: vec4<f32>,
    @location(4) col_w: vec4<f32>,
}

@vertex
fn vs_main(
    vertex: VertexInput,
    instance: InstanceInput,
    deform_in: DeformInput,
) -> VertexOutput {
    var out: VertexOutput;
    let deformed = vertex.position + deform_in.deform;
    // Affine SRT transform of `vec4(deformed, 0, 1)`: the dropped z_axis
    // column would multiply z = 0, and an SRT matrix's projective row
    // makes w_out = 1, so this is the exact matrix product.
    let world_position = vec4<f32>(
        instance.col_x.xyz * deformed.x + instance.col_y.xyz * deformed.y + instance.col_w.xyz,
        1.0,
    );
    out.clip_position = camera.view_proj * world_position;
    out.uv = vertex.uv;
    return out;
}

@group(1) @binding(0)
var t_diffuse: texture_2d<f32>;
@group(1) @binding(1)
var s_diffuse: sampler;

fn shade_part(tex_color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    // tex_color is premultiplied LINEAR (Rgba8UnormSrgb sampler decode
    // applied; importer pre-stored linear * alpha).
    // Apply screen tint, then multiplicative tint, then opacity:
    //   albedoOut = screen(texColor.xyz, texColor.a) * mult
    //   outAlbedo = albedoOut * opacity
    // catchlight renders albedo only (no emissive/lighting pass).
    let screen_tint = part.screen_tint_and_threshold.xyz;
    let screen_out = vec3<f32>(1.0) - ((vec3<f32>(1.0) - tex_color.rgb) *
                                        (vec3<f32>(1.0) - (screen_tint * tex_color.a)));
    let tinted_rgb = screen_out * part.tint.xyz;

    let final_a = tex_color.a * part.opacity;
    let final_rgb = tinted_rgb * part.opacity;
    return vec4<f32>(final_rgb, final_a);
}

// ClampToBorder+TransparentBlack emulation for the WebGPU/WebGL fallback.
// Native catchlight uses a sampler with `ClampToBorder`+`TransparentBlack`,
// so bilinear filtering at uv=0/1 mixes the edge texel with (0,0,0,0).
// The WebGPU/WebGL fallback uses `ClampToEdge` because ADDRESS_MODE_CLAMP_TO_BORDER
// isn't exposed there — bilinear at the boundary then samples the edge
// texel at full weight, creating a faint "ghost" rectangle wherever a
// mesh triangle's UV footprint reaches an opaque texture edge. Some
// rigs have hair-shadow textures with α=255 along their top row, which
// the artist relied on the transparent border to mask.
//
// Modulate the (premultiplied) sample by the in-bounds fraction of the
// 1-texel bilinear footprint along each axis. At uv=0 / uv=1 this
// halves the contribution; at uv = 0.5/size and beyond it leaves the
// sample unchanged.
fn border_emulate(tex_color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {
    if (UV_DISCARD <= 0.5) {
        return tex_color;
    }
    let tex_size = vec2<f32>(textureDimensions(t_diffuse));
    let pixel = uv * tex_size;
    let in_x = clamp(min(pixel.x + 0.5, tex_size.x - pixel.x + 0.5), 0.0, 1.0);
    let in_y = clamp(min(pixel.y + 0.5, tex_size.y - pixel.y + 0.5), 0.0, 1.0);
    return tex_color * (in_x * in_y);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    if (UV_DISCARD > 0.5 &&
        (in.uv.x < 0.0 || in.uv.x > 1.0 || in.uv.y < 0.0 || in.uv.y > 1.0)) {
        discard;
    }

    let tex_color = border_emulate(textureSample(t_diffuse, s_diffuse, in.uv), in.uv);
    return shade_part(tex_color, in.uv);
}

@fragment
fn fs_mask(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = border_emulate(textureSample(t_diffuse, s_diffuse, in.uv), in.uv);
    let threshold = part.screen_tint_and_threshold.w;
    if (tex_color.a <= threshold) {
        discard;
    }
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}

// WebGL fallback shaders: when the adapter lacks Depth24PlusStencil8
// (Chromium swiftshader WebGL2), stencil masking is replaced by
// sampling an offscreen "mask alpha" texture. `fs_mask_alpha` /
// `fs_mask_alpha_dodge` write the mask shape into that texture
// (regular sources write 1, dodge sources write 0 — mirroring the
// stencil path's REPLACE references); `fs_masked_sampled` samples it
// by the fragment's screen position and discards fragments outside
// the mask. See Pipelines::new for the has_stencil branch.
@group(3) @binding(0)
var t_mask: texture_2d<f32>;
@group(3) @binding(1)
var s_mask: sampler;

@fragment
fn fs_mask_alpha(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = border_emulate(textureSample(t_diffuse, s_diffuse, in.uv), in.uv);
    let threshold = part.screen_tint_and_threshold.w;
    if (tex_color.a <= threshold) {
        discard;
    }
    return vec4<f32>(1.0, 1.0, 1.0, 1.0);
}

@fragment
fn fs_mask_alpha_dodge(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = border_emulate(textureSample(t_diffuse, s_diffuse, in.uv), in.uv);
    let threshold = part.screen_tint_and_threshold.w;
    if (tex_color.a <= threshold) {
        discard;
    }
    return vec4<f32>(0.0, 0.0, 0.0, 0.0);
}

@fragment
fn fs_masked_sampled(in: VertexOutput) -> @location(0) vec4<f32> {
    let mask_size = vec2<f32>(textureDimensions(t_mask));
    let screen_uv = in.clip_position.xy / mask_size;
    let mask_sample = textureSample(t_mask, s_mask, screen_uv);
    if (mask_sample.a < 0.5) {
        discard;
    }

    if (UV_DISCARD > 0.5 &&
        (in.uv.x < 0.0 || in.uv.x > 1.0 || in.uv.y < 0.0 || in.uv.y > 1.0)) {
        discard;
    }

    let tex_color = border_emulate(textureSample(t_diffuse, s_diffuse, in.uv), in.uv);
    return shade_part(tex_color, in.uv);
}
