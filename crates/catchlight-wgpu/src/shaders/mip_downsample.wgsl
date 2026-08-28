// Mip generation: one full-screen triangle per level, averaging the 2x2
// block of the level above.
//
// The four texels are fetched explicitly rather than gathered with one
// bilinear tap. A bilinear tap only coincides with the box average when
// the parent's dimensions are even; textures here are alpha-cropped to
// arbitrary sizes, and `downsample_box_filter` handles an odd dimension by
// clamping the out-of-range column or row to the edge.
//
// The averaging lands in linear space for free: the texture is
// `Rgba8UnormSrgb`, so `textureLoad` decodes and the ROP re-encodes on
// write. Averaging gamma-encoded bytes would bias minified texels dark.
// Alpha is not gamma-coded in sRGB formats and passes through both ends
// untouched, matching the CPU path's direct average.

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle rather than two quad triangles: no seam along
    // the diagonal and one fewer vertex.
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@group(0) @binding(0) var src: texture_2d<f32>;

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let last = vec2<i32>(textureDimensions(src)) - vec2<i32>(1, 1);
    let base = vec2<i32>(floor(position.xy)) * 2;
    var sum = vec4<f32>(0.0);
    for (var oy = 0; oy < 2; oy++) {
        for (var ox = 0; ox < 2; ox++) {
            sum += textureLoad(src, min(base + vec2<i32>(ox, oy), last), 0);
        }
    }
    return sum * 0.25;
}
