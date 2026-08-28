// Fullscreen triangle whose only effect is the pipeline's stencil
// REPLACE: it seeds the whole viewport with the pass's stencil
// reference before an all-DodgeMask batch punches 0 out of it. Color
// writes are masked off in the pipeline; no bind groups.

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex_index & 1u) << 1u);
    let y = f32(vertex_index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0);
}
