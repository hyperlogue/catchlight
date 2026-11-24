#[test]
fn uploads_release_decoded_pixels_and_replace_texture_dimensions() {
    {
        let mut ctx = super::render_context(64, 64);
        ctx.renderer
            .upload_mesh(0, &catchlight_core::Mesh::quad(1.0, 1.0))
            .unwrap();
        let mut texture = catchlight_core::DecodedTexture {
            width: 1,
            height: 2,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255].into(),
        };
        let pixels = std::sync::Arc::downgrade(&texture.rgba);
        let mut list = crate::RenderList::default();
        list.root_drawables.push(crate::DrawableInfo::Part {
            mesh_id: 0,
            texture_id: 0,
            transform: glam::Mat4::IDENTITY,
            z_order: 0.0,
            blend_mode: catchlight_core::BlendMode::Normal,
            opacity: 1.0,
            tint: glam::Vec3::ONE,
            screen_tint: glam::Vec3::ZERO,
            mask_sources: Default::default(),
            mask_threshold: 0.5,
        });
        ctx.renderer.upload_texture(0, &texture).unwrap();
        assert_eq!(std::sync::Arc::strong_count(&texture.rgba), 1);
        ctx.render(&list, Some(wgpu::Color::TRANSPARENT));
        let vertical = ctx.read_rgba().unwrap();

        texture.width = 2;
        texture.height = 1;
        ctx.renderer.upload_texture(0, &texture).unwrap();
        drop(texture);
        assert!(
            pixels.upgrade().is_none(),
            "the GPU upload must not retain CPU pixels"
        );
        ctx.render(&list, Some(wgpu::Color::TRANSPARENT));
        let horizontal = ctx.read_rgba().unwrap();
        assert_ne!(
            vertical, horizontal,
            "the replacement uses its new dimensions"
        );
        assert!(horizontal.as_chunks::<4>().0.iter().any(|p| p[3] != 0));
    }
}
