use crate::{PipelineOptions, Pipelines, StencilMode};

#[test]
fn pipelines_follow_the_devices_backend() {
    let (device, _) = super::shared_context();
    let gl = device.adapter_info().backend == wgpu::Backend::Gl;
    let pipelines = Pipelines::new(
        &device,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        PipelineOptions::default(),
    );
    assert_eq!(
        pipelines.base_instance, !gl,
        "hardware first_instance off GL"
    );
    assert_eq!(pipelines.has_stencil(), !gl, "the stencil mask path off GL");
}

#[test]
fn stencil_off_masks_through_sampled_alpha() {
    let (device, _) = super::shared_context();
    let pipelines = Pipelines::new(
        &device,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        PipelineOptions {
            stencil: StencilMode::Off,
        },
    );
    assert!(!pipelines.has_stencil());
}
