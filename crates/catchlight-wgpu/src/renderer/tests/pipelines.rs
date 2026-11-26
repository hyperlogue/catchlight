use crate::Pipelines;

#[test]
fn pipelines_follow_the_devices_backend() {
    let (device, _) = super::shared_context();
    let gl = device.adapter_info().backend == wgpu::Backend::Gl;
    let pipelines = Pipelines::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    assert_eq!(
        pipelines.base_instance, !gl,
        "hardware first_instance off GL"
    );
    if std::env::var_os("CATCHLIGHT_DISABLE_STENCIL").is_none() {
        assert_eq!(pipelines.has_stencil(), !gl, "the stencil mask path off GL");
    }
}
