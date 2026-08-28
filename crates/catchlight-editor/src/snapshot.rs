//! Viewport PNG snapshot encoding. The GPU readback itself is
//! `catchlight_wgpu::read_texture_to_rgba` (wasm-safe: the browser resolves
//! the buffer mapping while the future is parked).

pub(crate) fn encode_png(pixels: Vec<u8>, width: u32, height: u32) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, pixels)?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}
