// Four explicit texel fetches, not one bilinear tap: alpha-cropped textures
// have odd sizes, where a bilinear tap is not the box average. The
// `Rgba8UnormSrgb` format makes the average linear.

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
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
