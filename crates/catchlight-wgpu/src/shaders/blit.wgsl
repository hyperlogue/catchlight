struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) tex_coords: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;

    let x = f32((vertex_index & 1u) << 1u);
    let y = f32((vertex_index & 2u));

    out.position = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.tex_coords = vec2<f32>(x, y);

    return out;
}

@group(0) @binding(0)
var composite_texture: texture_2d<f32>;
@group(0) @binding(1)
var composite_sampler: sampler;

struct BlitUniforms {
    opacity: f32,
    tint: vec3<f32>,
    // Shares layout with basic.wgsl's PartUniforms; .w is the composite's
    // mask threshold (unused by fs_main but needed for layout parity).
    screen_tint_and_threshold: vec4<f32>,
}

@group(1) @binding(0)
var<uniform> uniforms: BlitUniforms;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Composite slot is Rgba8UnormSrgb; sampler returns premultiplied
    // linear values. Operate on the premultiplied sample directly,
    // before applying the composite's tint and opacity.
    let tex_color = textureSample(composite_texture, composite_sampler, in.tex_coords);
    let pm_rgb = tex_color.rgb;

    let screen_tint = uniforms.screen_tint_and_threshold.xyz;
    let screen_out = vec3<f32>(1.0) - ((vec3<f32>(1.0) - pm_rgb) *
                                        (vec3<f32>(1.0) - (screen_tint * tex_color.a)));
    let tinted_rgb = screen_out * uniforms.tint;

    let final_a = tex_color.a * uniforms.opacity;
    let final_rgb = tinted_rgb * uniforms.opacity;

    return vec4<f32>(final_rgb, final_a);
}

@fragment
fn fs_composite_mask(in: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = textureSample(composite_texture, composite_sampler, in.tex_coords).a
        * uniforms.opacity;
    if (alpha <= uniforms.screen_tint_and_threshold.w) {
        discard;
    }
    return vec4<f32>(1.0);
}

@fragment
fn fs_composite_mask_dodge(in: VertexOutput) -> @location(0) vec4<f32> {
    let alpha = textureSample(composite_texture, composite_sampler, in.tex_coords).a
        * uniforms.opacity;
    if (alpha <= uniforms.screen_tint_and_threshold.w) {
        discard;
    }
    return vec4<f32>(0.0);
}

// WebGL fallback masked blit: sampled mask path. Bind group 2 carries
// the viewport-sized mask alpha texture (written via fs_mask_alpha in
// basic.wgsl). Each fragment reads the mask at its screen position and
// discards outside the shape — stencil-test replacement for platforms
// without Depth24PlusStencil8.
@group(2) @binding(0)
var t_mask: texture_2d<f32>;
@group(2) @binding(1)
var s_mask: sampler;

@fragment
fn fs_masked_sampled(in: VertexOutput) -> @location(0) vec4<f32> {
    let mask_size = vec2<f32>(textureDimensions(t_mask));
    let screen_uv = in.position.xy / mask_size;
    let mask_sample = textureSample(t_mask, s_mask, screen_uv);
    if (mask_sample.a < 0.5) {
        discard;
    }

    // Premultiplied linear sample (Rgba8UnormSrgb composite slot);
    // same formula as fs_main.
    let tex_color = textureSample(composite_texture, composite_sampler, in.tex_coords);
    let pm_rgb = tex_color.rgb;

    let screen_tint = uniforms.screen_tint_and_threshold.xyz;
    let screen_out = vec3<f32>(1.0) - ((vec3<f32>(1.0) - pm_rgb) *
                                        (vec3<f32>(1.0) - (screen_tint * tex_color.a)));
    let tinted_rgb = screen_out * uniforms.tint;

    let final_a = tex_color.a * uniforms.opacity;
    let final_rgb = tinted_rgb * uniforms.opacity;

    return vec4<f32>(final_rgb, final_a);
}
