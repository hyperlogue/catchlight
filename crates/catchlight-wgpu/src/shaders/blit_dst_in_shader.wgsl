// Dst-in-shader blend modes (Overlay/ColorBurn/LinearBurn) read the
// destination at the same screen pixel and compute the per-channel
// blend in the fragment shader, since wgpu's BlendState can't express
// these formulas with fixed-function blend factors.
//
// Pipeline contract: bind group 0 carries the composite (src) texture +
// sampler (matches blit_bind_group_layout in renderer.rs); bind group 1
// carries the BlitUniforms uniform with dynamic offset (shared
// part_uniform_bind_group_layout); bind group 2 carries a snapshot of
// the framebuffer copied via copy_texture_to_texture immediately before
// this pass. The pipeline uses Replace blend (no fixed-function blend
// math) since the shader itself emits the final composited pixel.

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
    screen_tint_and_threshold: vec4<f32>,
}

@group(1) @binding(0)
var<uniform> uniforms: BlitUniforms;

@group(2) @binding(0)
var t_snapshot: texture_2d<f32>;
@group(2) @binding(1)
var s_snapshot: sampler;

// Sample the composite + apply tint/screen_tint/opacity exactly like
// blit.wgsl's fs_main, but return the pre-multiplied RGBA src. Shared
// by all four dst-in-shader entry points.
//
// Composite slot is Rgba8UnormSrgb; sampler returns premultiplied
// linear values. Operate on the premultiplied sample directly.
fn sample_blit_src(in: VertexOutput) -> vec4<f32> {
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

// Sample the framebuffer snapshot at this fragment. tex_coords spans
// [0,1] across the fullscreen quad; the snapshot view uses the
// surface's sRGB format, so textureSample applies the standard
// sRGB→linear read transform — the result is in linear space, matching
// how `src` is sampled from the composite slot.
fn load_snapshot(in: VertexOutput) -> vec4<f32> {
    return textureSample(t_snapshot, s_snapshot, in.tex_coords);
}

// KHR_blend_equation_advanced unpremultiplies BOTH operands before the
// per-mode blend function (Cs' = Cs/As, Cd' = Cd/Ad; 0 when the alpha
// is 0), then composites with X = Y = Z = 1:
//   R = f(Cs', Cd')*As*Ad + Cs'*As*(1-Ad) + Cd'*Ad*(1-As)
//   A = As*Ad + As*(1-Ad) + Ad*(1-As)  (= As + Ad - As*Ad)
// R is already premultiplied by A — exactly what the framebuffer
// stores. With Ad = 0 this degrades to plain src; with As = 0, to dst.
fn unpremultiply(c: vec4<f32>) -> vec3<f32> {
    if (c.a < 0.001) {
        return vec3<f32>(0.0);
    }
    return c.rgb / c.a;
}

fn khr_composite(blend_rgb: vec3<f32>, s: vec3<f32>, d: vec3<f32>, sa: f32, da: f32) -> vec4<f32> {
    let p0 = sa * da;
    let p1 = sa * (1.0 - da);
    let p2 = da * (1.0 - sa);
    let out_rgb = blend_rgb * p0 + s * p1 + d * p2;
    let out_a = p0 + p1 + p2;
    return vec4<f32>(out_rgb, out_a);
}

@fragment
fn fs_overlay(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = sample_blit_src(in);
    let dst = load_snapshot(in);
    if (src.a < 0.001) {
        return dst;
    }
    let s = unpremultiply(src);
    let d = unpremultiply(dst);
    // Per-channel: d < 0.5 ? 2*s*d : 1 - 2*(1-s)*(1-d)
    let low = 2.0 * s * d;
    let high = vec3<f32>(1.0) - 2.0 * (vec3<f32>(1.0) - s) * (vec3<f32>(1.0) - d);
    let mask = step(vec3<f32>(0.5), d);
    let blend = mix(low, high, mask);
    return khr_composite(clamp(blend, vec3<f32>(0.0), vec3<f32>(1.0)), s, d, src.a, dst.a);
}

@fragment
fn fs_color_burn(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = sample_blit_src(in);
    let dst = load_snapshot(in);
    if (src.a < 0.001) {
        return dst;
    }
    let s = unpremultiply(src);
    let d = unpremultiply(dst);
    // 1 - min(1, (1 - d) / s); s == 0 collapses d to black.
    let safe_s = max(s, vec3<f32>(1.0e-4));
    let blend = vec3<f32>(1.0) - (vec3<f32>(1.0) - d) / safe_s;
    return khr_composite(clamp(blend, vec3<f32>(0.0), vec3<f32>(1.0)), s, d, src.a, dst.a);
}

@fragment
fn fs_linear_burn(in: VertexOutput) -> @location(0) vec4<f32> {
    let src = sample_blit_src(in);
    let dst = load_snapshot(in);
    if (src.a < 0.001) {
        return dst;
    }
    let s = unpremultiply(src);
    let d = unpremultiply(dst);
    // src + dst - 1; clamped to [0,1].
    let blend = s + d - vec3<f32>(1.0);
    return khr_composite(clamp(blend, vec3<f32>(0.0), vec3<f32>(1.0)), s, d, src.a, dst.a);
}

