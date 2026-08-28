use std::io::Read;
use std::sync::Arc;
use thiserror::Error;

use crate::components::{srgb_encode_to_byte, PuppetTexture};
use crate::load_budget::{LoadBudget, LoadLimitError, LoadResource, MAX_TEXTURE_DIMENSION};

use super::utils::{read_be_u32, read_n, read_u8, read_vec};

const MAGIC: &[u8] = b"TRNSRTS\0";
const TEX_SECT: &[u8] = b"TEX_SECT";
const EXT_SECT: &[u8] = b"EXT_SECT";

// Sanity ceilings on untrusted sizes read from the file. A malicious or
// corrupt INX can claim length = u32::MAX and OOM the process otherwise.
const MAX_PAYLOAD_SIZE: usize = 256 * 1024 * 1024;
const MAX_TEXTURE_SIZE: usize = 256 * 1024 * 1024;
const MAX_VENDOR_NAME_SIZE: usize = 64 * 1024;
const MAX_TEXTURE_COUNT: usize = 65_536;
const MAX_VENDOR_COUNT: usize = 1_024;
const MAX_TEXTURE_PIXELS: u64 = MAX_TEXTURE_DIMENSION as u64 * MAX_TEXTURE_DIMENSION as u64;

#[derive(Debug, Error)]
pub enum InxParseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid magic bytes, expected 'TRNSRTS\\0'")]
    IncorrectMagic,

    #[error("Missing texture section header")]
    NoTexSect,

    #[error("Invalid UTF-8 in payload")]
    InvalidUtf8(#[from] std::str::Utf8Error),

    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("Invalid texture encoding: {0}")]
    InvalidTexEncoding(u8),

    #[error("BC7 DDS parse failed: {0}")]
    Bc7DdsParse(String),

    #[error("BC7 decode failed: {0}")]
    Bc7Decode(String),

    #[error("BC7 PNG re-encode failed: {0}")]
    Bc7Encode(String),

    #[error("{what} size {size} exceeds limit of {limit} bytes")]
    SizeExceedsLimit {
        what: &'static str,
        size: usize,
        limit: usize,
    },

    #[error("Texture dimensions {width}x{height} exceed limit {limit}")]
    TextureTooLarge { width: u32, height: u32, limit: u32 },

    #[error("Failed to read texture header: {0}")]
    TextureHeader(String),

    #[error(transparent)]
    LoadLimit(#[from] LoadLimitError),
}

fn check_texture_dims(width: u32, height: u32) -> Result<(), InxParseError> {
    if width > MAX_TEXTURE_DIMENSION || height > MAX_TEXTURE_DIMENSION {
        return Err(InxParseError::TextureTooLarge {
            width,
            height,
            limit: MAX_TEXTURE_DIMENSION,
        });
    }
    if width as u64 * height as u64 > MAX_TEXTURE_PIXELS {
        return Err(InxParseError::TextureTooLarge {
            width,
            height,
            limit: MAX_TEXTURE_DIMENSION,
        });
    }
    Ok(())
}

fn check_size(what: &'static str, size: usize, limit: usize) -> Result<(), InxParseError> {
    if size > limit {
        Err(InxParseError::SizeExceedsLimit { what, size, limit })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureFormat {
    Png,
    Tga,
}

impl TextureFormat {
    pub fn to_image_format(self) -> image::ImageFormat {
        match self {
            TextureFormat::Png => image::ImageFormat::Png,
            TextureFormat::Tga => image::ImageFormat::Tga,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelTexture {
    pub format: TextureFormat,
    pub data: Arc<[u8]>,
    /// `true` for premultiplied-in-sRGB on-disk bytes (every `.inx`
    /// texture); `false` for editor-authored straight-alpha. Decides whether
    /// `decode` un-premultiplies before re-premultiplying into linear.
    pub premultiplied: bool,
}

impl ModelTexture {
    /// Decode into the canonical [`PuppetTexture`] form: bytes encoding
    /// premultiplied LINEAR color, ready to upload as `Rgba8UnormSrgb`.
    /// The sampler then decodes sRGB→linear and returns premultiplied
    /// linear values; the shader treats the sample as premultiplied and
    /// runs all blend / tint math in linear space.
    ///
    /// The inx/inp on-disk convention pre-multiplies RGB by alpha in
    /// sRGB byte space (`byte = srgb_encode(linear) * α`). To convert
    /// to "premultiplied linear, encoded as sRGB byte" we
    /// (1) unpremultiply in sRGB byte space → straight sRGB bytes,
    /// (2) decode each channel sRGB→linear, multiply by α, encode
    ///     linear→sRGB byte back into the buffer.
    ///
    /// Premultiplied storage handles alpha-edge bilinear correctly
    /// without an explicit alpha-bleed pass: at an edge, the filter
    /// mixes `(rgb·α, α)` with `(0, 0)` and the resulting gradient is
    /// already premultiplied.
    /// Width/height from the image header alone — no pixel decode. Lets
    /// the importer plan every texture crop before paying
    /// for any full decode.
    pub fn dimensions(&self) -> Result<(u32, u32), image::ImageError> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&self.data[..]));
        reader.set_format(self.format.to_image_format());
        reader.into_dimensions()
    }

    pub fn decode(&self) -> Result<PuppetTexture, image::ImageError> {
        let img = image::load_from_memory_with_format(&self.data, self.format.to_image_format())?;
        let (width, height) = (img.width(), img.height());
        let mut rgba = img.into_rgba8().into_raw();
        // Both conventions converge on premultiplied-linear; only a
        // premultiplied-sRGB source needs unwinding to straight first.
        if self.premultiplied {
            premultiplied_srgb_to_premultiplied_linear_inplace(&mut rgba);
        } else {
            premultiply_linear_into_srgb_inplace(&mut rgba);
        }
        Ok(PuppetTexture {
            width,
            height,
            rgba: rgba.into(),
        })
    }
}

/// Every `(channel byte, alpha byte)` pair's final value under the inx
/// conversion. Indexed `alpha * 256 + channel`.
static PREMULTIPLY_SRGB_LUT: std::sync::OnceLock<Box<[u8; 65536]>> = std::sync::OnceLock::new();

fn premultiply_srgb_lut() -> &'static [u8; 65536] {
    PREMULTIPLY_SRGB_LUT.get_or_init(|| {
        let decode = crate::components::srgb_decode_table();
        let mut table = Box::new([0u8; 65536]);
        for a in 1..=255usize {
            let inv = 255.0 / a as f32;
            let af = a as f32 / 255.0;
            for c in 0..256usize {
                table[a * 256 + c] = if a == 255 {
                    c as u8
                } else {
                    let straight = (c as f32 * inv).round().min(255.0) as u8;
                    srgb_encode_to_byte(decode[straight as usize] * af)
                };
            }
        }
        table
    })
}

/// Convert premultiplied-in-sRGB bytes straight to the
/// premultiplied-linear encoding, in one pass.
///
/// Composing [`unpremultiply_srgb_inplace`] with
/// [`premultiply_linear_into_srgb_inplace`] gives the same bytes, but each
/// output depends only on its own channel and alpha, so the composition is
/// a pure function of two bytes and tabulates completely. That turns the
/// per-pixel divide, `powf` pair, and second sweep over the buffer into one
/// indexed read — and the second sweep is most of the cost, because the
/// overwhelming majority of an atlas is fully transparent or fully opaque
/// and does no arithmetic either way.
pub fn premultiplied_srgb_to_premultiplied_linear_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    let lut = premultiply_srgb_lut();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3] as usize;
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let row = &lut[a * 256..a * 256 + 256];
        for c in &mut px[..3] {
            *c = row[*c as usize];
        }
    }
}

/// Undo premultiplication in sRGB byte space on an RGBA8 buffer.
/// For each pixel: `straight = round((rgb * 255) / α)`, computed in
/// f32 so partial-alpha pixels don't lose precision to integer
/// division. Pixels with α=0 are left as `(0, 0, 0, 0)`.
pub fn unpremultiply_srgb_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let inv = 255.0 / a as f32;
        for c in &mut px[..3] {
            *c = (*c as f32 * inv).round().min(255.0) as u8;
        }
    }
}

/// Convert straight-alpha sRGB-encoded RGBA to bytes encoding
/// premultiplied LINEAR color: for each channel, `srgb_decode(rgb)`,
/// multiply by `α`, `srgb_encode` back to byte. Alpha is unchanged.
/// α=0 pixels emit `(0, 0, 0, 0)`; α=255 pixels are unchanged
/// (premultiply by 1 is a no-op).
///
/// Result is suitable for upload as `Rgba8UnormSrgb`: the sampler
/// decodes sRGB→linear and returns premultiplied linear values, which
/// the shader can blend / tint directly without re-multiplying by α.
pub fn premultiply_linear_into_srgb_inplace(rgba: &mut [u8]) {
    debug_assert_eq!(rgba.len() % 4, 0, "rgba buffer must be a multiple of 4");
    let decode = crate::components::srgb_decode_table();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = px[3];
        if a == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            continue;
        }
        if a == 255 {
            continue;
        }
        let af = a as f32 / 255.0;
        for c in &mut px[..3] {
            let premul = decode[*c as usize] * af;
            *c = srgb_encode_to_byte(premul);
        }
    }
}

/// Edge-bleed (a.k.a. alpha bleed / edge padding): for every α=0 pixel,
/// copy the RGB of the nearest α>0 pixel, leaving α=0 untouched. Without
/// this, bilinear texture filtering at a Part's alpha boundary mixes
/// `(rgb, 1)` with `(0, 0)` and produces a darkening colour shift that
/// shows up as faint mesh-edge halos on top of underlying parts. The imported
/// format gets the same effect for free because it stores premultiplied bytes:
/// at the boundary the filter mixes `rgb·α` with `0` and the gradient is
/// already premultiplied. Catchlight stores straight-alpha so we have to
/// stage the boundary RGB manually.
///
/// BFS from every α>0 pixel; each α=0 neighbour reached for the first
/// time copies the source's RGB. O(W·H), runs once per texture at load.
pub fn alpha_bleed_inplace(rgba: &mut [u8], width: u32, height: u32) {
    let w = width as usize;
    let h = height as usize;
    debug_assert_eq!(rgba.len(), w * h * 4);
    if w == 0 || h == 0 {
        return;
    }

    // `seen[i]` is true when pixel i has α>0, OR has already been bled
    // from a neighbour. Doubles as the "in queue" marker.
    let mut seen = vec![false; w * h];
    let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::new();
    for i in 0..(w * h) {
        if rgba[i * 4 + 3] > 0 {
            seen[i] = true;
            queue.push_back(i as u32);
        }
    }

    while let Some(idx) = queue.pop_front() {
        let idx = idx as usize;
        let x = idx % w;
        let y = idx / w;
        let r = rgba[idx * 4];
        let g = rgba[idx * 4 + 1];
        let b = rgba[idx * 4 + 2];
        let visit = |nx: usize,
                     ny: usize,
                     queue: &mut std::collections::VecDeque<u32>,
                     seen: &mut [bool],
                     rgba: &mut [u8]| {
            let ni = ny * w + nx;
            if seen[ni] {
                return;
            }
            seen[ni] = true;
            rgba[ni * 4] = r;
            rgba[ni * 4 + 1] = g;
            rgba[ni * 4 + 2] = b;
            // alpha stays 0 — the pixel is still invisible
            queue.push_back(ni as u32);
        };
        if x > 0 {
            visit(x - 1, y, &mut queue, &mut seen, rgba);
        }
        if x + 1 < w {
            visit(x + 1, y, &mut queue, &mut seen, rgba);
        }
        if y > 0 {
            visit(x, y - 1, &mut queue, &mut seen, rgba);
        }
        if y + 1 < h {
            visit(x, y + 1, &mut queue, &mut seen, rgba);
        }
    }
}

/// Raw bytes of one EXT_SECT entry. The reference stores vendor
/// payloads opaquely (`puppet.extData[name] = bytes`); they may be
/// binary, so no JSON decoding happens here.
#[derive(Debug, Clone)]
pub struct VendorData {
    pub name: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InxModel {
    pub payload: serde_json::Value,
    pub textures: Vec<ModelTexture>,
    pub vendors: Vec<VendorData>,
}

fn parse_payload<R: Read>(
    data: &mut R,
    budget: &mut LoadBudget,
) -> Result<serde_json::Value, InxParseError> {
    let length = read_be_u32(data)? as usize;
    check_size("payload", length, MAX_PAYLOAD_SIZE)?;
    budget.charge(LoadResource::EncodedBytes, length as u64)?;
    let payload_bytes = read_vec(data, length)?;
    let payload_str = std::str::from_utf8(&payload_bytes)?;
    Ok(serde_json::from_str(payload_str)?)
}

fn parse_texture<R: Read>(
    data: &mut R,
    budget: &mut LoadBudget,
) -> Result<ModelTexture, InxParseError> {
    let tex_length = read_be_u32(data)? as usize;
    check_size("texture", tex_length, MAX_TEXTURE_SIZE)?;
    budget.charge(LoadResource::EncodedBytes, tex_length as u64)?;
    let tex_encoding = read_u8(data)?;

    // The reference treats a zero-length slot as an empty placeholder
    // (fmt/package.d: `inAddTextureBinary(ShallowTexture([], 0, 0, 4))`)
    // and ignores the type byte. Substitute a 1x1 transparent texture so
    // slot indices stay aligned.
    if tex_length == 0 {
        budget.check_texture_dimensions(1, 1)?;
        return placeholder_texture();
    }

    let format = match tex_encoding {
        0 => TextureFormat::Png,
        1 => TextureFormat::Tga,
        2 => TextureFormat::Png, // Decoded from BC7 into RGBA, re-encoded as PNG below.
        n => return Err(InxParseError::InvalidTexEncoding(n)),
    };

    let raw = read_vec(data, tex_length)?;
    let (tex_data, width, height): (Arc<[u8]>, u32, u32) = if tex_encoding == 2 {
        let (png, width, height) = decode_bc7_dds_to_png(&raw)?;
        (png.into(), width, height)
    } else {
        let (width, height) = image_dimensions(&raw, format.to_image_format())?;
        (raw.into(), width, height)
    };
    budget.check_texture_dimensions(width, height)?;
    Ok(ModelTexture {
        format,
        data: tex_data,
        premultiplied: true,
    })
}

fn placeholder_texture() -> Result<ModelTexture, InxParseError> {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(1, 1))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| InxParseError::TextureHeader(e.to_string()))?;
    Ok(ModelTexture {
        format: TextureFormat::Png,
        data: png.into(),
        premultiplied: true,
    })
}

fn image_dimensions(bytes: &[u8], format: image::ImageFormat) -> Result<(u32, u32), InxParseError> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
    reader.set_format(format);
    let (width, height) = reader
        .into_dimensions()
        .map_err(|e| InxParseError::TextureHeader(e.to_string()))?;
    check_texture_dims(width, height)?;
    Ok((width, height))
}

fn decode_bc7_dds_to_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), InxParseError> {
    let dds = image_dds::ddsfile::Dds::read(bytes)
        .map_err(|e| InxParseError::Bc7DdsParse(e.to_string()))?;
    let (width, height) = (dds.get_width(), dds.get_height());
    check_texture_dims(width, height)?;
    let img =
        image_dds::image_from_dds(&dds, 0).map_err(|e| InxParseError::Bc7Decode(e.to_string()))?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| InxParseError::Bc7Encode(e.to_string()))?;
    Ok((out, width, height))
}

#[doc(hidden)]
pub fn decode_texture_for_fuzz(encoding: u8, bytes: &[u8]) -> Result<Vec<u8>, InxParseError> {
    match encoding {
        0 => image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map(|img| img.into_bytes())
            .map_err(|e| InxParseError::Bc7Decode(e.to_string())),
        1 => image::load_from_memory_with_format(bytes, image::ImageFormat::Tga)
            .map(|img| img.into_bytes())
            .map_err(|e| InxParseError::Bc7Decode(e.to_string())),
        2 => decode_bc7_dds_to_png(bytes).map(|(png, _, _)| png),
        n => Err(InxParseError::InvalidTexEncoding(n)),
    }
}

fn parse_textures<R: Read>(
    data: &mut R,
    budget: &mut LoadBudget,
) -> Result<Vec<ModelTexture>, InxParseError> {
    let tex_sect = read_n::<_, 8>(data).map_err(|_| InxParseError::NoTexSect)?;
    if tex_sect != TEX_SECT {
        return Err(InxParseError::NoTexSect);
    }

    let tex_count = read_be_u32(data)? as usize;
    check_size("texture count", tex_count, MAX_TEXTURE_COUNT)?;
    budget.charge(LoadResource::Textures, tex_count as u64)?;
    let mut textures = Vec::with_capacity(tex_count);
    for _ in 0..tex_count {
        textures.push(parse_texture(data, budget)?);
    }
    Ok(textures)
}

fn parse_vendor<R: Read>(
    data: &mut R,
    budget: &mut LoadBudget,
) -> Result<VendorData, InxParseError> {
    let length = read_be_u32(data)? as usize;
    check_size("vendor name", length, MAX_VENDOR_NAME_SIZE)?;
    budget.charge(LoadResource::EncodedBytes, length as u64)?;
    let name_bytes = read_vec(data, length)?;
    let name = String::from_utf8(name_bytes).map_err(|e| e.utf8_error())?;

    let payload_length = read_be_u32(data)? as usize;
    check_size("vendor payload", payload_length, MAX_PAYLOAD_SIZE)?;
    budget.charge(LoadResource::EncodedBytes, payload_length as u64)?;
    let payload = read_vec(data, payload_length)?;
    Ok(VendorData { name, payload })
}

fn parse_vendors<R: Read>(
    data: &mut R,
    budget: &mut LoadBudget,
) -> Result<Vec<VendorData>, InxParseError> {
    match read_n::<_, 8>(data) {
        Ok(ext_sect) if ext_sect == EXT_SECT => {
            let ext_count = read_be_u32(data)? as usize;
            check_size("vendor count", ext_count, MAX_VENDOR_COUNT)?;
            let mut vendors = Vec::with_capacity(ext_count);
            for _ in 0..ext_count {
                vendors.push(parse_vendor(data, budget)?);
            }
            Ok(vendors)
        }
        _ => Ok(Vec::new()),
    }
}

impl InxModel {
    pub fn parse<R: Read>(mut data: R) -> Result<Self, InxParseError> {
        Self::parse_with_budget(&mut data, &mut LoadBudget::default())
    }

    pub fn parse_with_budget<R: Read>(
        mut data: R,
        budget: &mut LoadBudget,
    ) -> Result<Self, InxParseError> {
        let magic = read_n::<_, 8>(&mut data)?;
        if magic != MAGIC {
            return Err(InxParseError::IncorrectMagic);
        }

        let payload = parse_payload(&mut data, budget)?;
        let textures = parse_textures(&mut data, budget)?;
        let vendors = parse_vendors(&mut data, budget)?;

        Ok(InxModel {
            payload,
            textures,
            vendors,
        })
    }
}

// `.inp` and `.inx` share the same binary framing (TRNSRTS\0 magic, BE u32
// length-prefixed JSON, TEX_SECT, optional EXT_SECT). The names are
// historical; one parser handles both.
pub type InpModel = InxModel;
pub type InpParseError = InxParseError;

pub fn parse_inp<R: Read>(data: R) -> Result<InpModel, InpParseError> {
    InxModel::parse(data)
}

#[cfg(test)]
#[allow(clippy::identity_op)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fused_premultiply_matches_the_two_pass_composition() {
        // The fused table replaces unpremultiply-then-premultiply, so it
        // has to agree on every input it can ever see — and it can see all
        // of them, since each output channel depends only on its own byte
        // and the pixel's alpha. Exhaustive, not sampled.
        let mut fused: Vec<u8> = Vec::with_capacity(256 * 256 * 4);
        for a in 0..256usize {
            for c in 0..256usize {
                fused.extend_from_slice(&[c as u8, c as u8, c as u8, a as u8]);
            }
        }
        let mut two_pass = fused.clone();
        unpremultiply_srgb_inplace(&mut two_pass);
        premultiply_linear_into_srgb_inplace(&mut two_pass);
        premultiplied_srgb_to_premultiplied_linear_inplace(&mut fused);

        for (i, (f, t)) in fused
            .as_chunks::<4>()
            .0
            .iter()
            .zip(two_pass.as_chunks::<4>().0)
            .enumerate()
        {
            assert_eq!(f, t, "channel {} alpha {}", i % 256, i / 256);
        }
    }

    #[test]
    fn test_magic_bytes_validation() {
        let data = vec![b'W', b'R', b'O', b'N', b'G', 0, 0, 0];
        let mut cursor = Cursor::new(data);
        let result = InxModel::parse(&mut cursor);
        assert!(matches!(result, Err(InxParseError::IncorrectMagic)));
    }

    fn build_inx_with_payload_len(claimed_len: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&claimed_len.to_be_bytes());
        data
    }

    #[test]
    fn rejects_oversized_payload_length() {
        // Claim a 2 GB payload — must be rejected before allocation.
        let data = build_inx_with_payload_len(2 * 1024 * 1024 * 1024);
        let err = InxModel::parse(Cursor::new(data)).unwrap_err();
        assert!(
            matches!(
                err,
                InxParseError::SizeExceedsLimit {
                    what: "payload",
                    ..
                }
            ),
            "expected payload size limit, got {:?}",
            err
        );
    }

    #[test]
    fn rejects_oversized_texture_count() {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        // Empty JSON payload ("{}")
        let payload = b"{}";
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        data.extend_from_slice(TEX_SECT);
        // Claim ~1M textures
        data.extend_from_slice(&1_000_000_u32.to_be_bytes());
        let err = InxModel::parse(Cursor::new(data)).unwrap_err();
        assert!(
            matches!(
                err,
                InxParseError::SizeExceedsLimit {
                    what: "texture count",
                    ..
                }
            ),
            "expected texture count limit, got {:?}",
            err
        );
    }

    #[test]
    fn rejects_aggregate_decoded_texture_bytes() {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&2u32.to_be_bytes());
        data.extend_from_slice(b"{}");
        data.extend_from_slice(TEX_SECT);
        data.extend_from_slice(&2u32.to_be_bytes());
        for _ in 0..2 {
            data.extend_from_slice(&0u32.to_be_bytes());
            data.push(0);
        }
        let mut budget = LoadBudget::new(crate::load_budget::LoadLimits {
            decoded_texture_bytes: 4,
            ..crate::load_budget::LoadLimits::default()
        });

        let err = InxModel::parse_with_budget(Cursor::new(data), &mut budget).unwrap_err();

        assert!(matches!(
            err,
            InxParseError::LoadLimit(LoadLimitError {
                resource: "decoded texture bytes",
                got: 8,
                ..
            })
        ));
    }

    // Minimal PNG: 8-byte signature + IHDR (w,h, 8-bit RGBA) + empty IDAT + IEND.
    // CRCs are pre-computed over (chunk_type || chunk_data).
    fn minimal_png(width: u32, height: u32, ihdr_crc: u32) -> Vec<u8> {
        const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        const IDAT_EMPTY_CRC: u32 = 0x35af061e;
        const IEND_CRC: u32 = 0xae426082;

        let mut png = Vec::new();
        png.extend_from_slice(&PNG_SIG);
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png.extend_from_slice(&ihdr_crc.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&IDAT_EMPTY_CRC.to_be_bytes());
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IEND");
        png.extend_from_slice(&IEND_CRC.to_be_bytes());
        png
    }

    fn build_inx_with_single_png(png: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        let payload = b"{}";
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        data.extend_from_slice(TEX_SECT);
        data.extend_from_slice(&1u32.to_be_bytes());
        data.extend_from_slice(&(png.len() as u32).to_be_bytes());
        data.push(0);
        data.extend_from_slice(png);
        data
    }

    #[test]
    fn rejects_oversized_texture_dimensions() {
        let png = minimal_png(16384, 16384, 0xa9c81084);
        let data = build_inx_with_single_png(&png);
        let err = InxModel::parse(Cursor::new(data)).unwrap_err();
        assert!(
            matches!(
                err,
                InxParseError::TextureTooLarge {
                    width: 16384,
                    height: 16384,
                    ..
                }
            ),
            "expected TextureTooLarge, got {:?}",
            err
        );
    }

    #[test]
    fn unpremultiply_srgb_recovers_straight_alpha() {
        // Pixel layout: opaque (α=255), fully transparent (α=0),
        // half-alpha grey, tiny-alpha saturated color.
        let mut px = [
            200, 100, 50, 255, // fully opaque: untouched
            123, 234, 89, 0, // α=0: rgb zeroed
            64, 64, 64, 128, // α≈0.5: premul 0.5 byte → straight ≈ 0.5 sRGB = 128
            10, 5, 2, 20, // α≈8%: premul 10/20 → straight ≈ 0.5 sRGB = 128
        ];
        unpremultiply_srgb_inplace(&mut px);
        assert_eq!(&px[0..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
        // (64 * 255 / 128).round() = 127.5 → banker's/half-away-up 128.
        assert!((px[4 + 4] as i32 - 127).abs() <= 1);
        assert!((px[5 + 4] as i32 - 127).abs() <= 1);
        assert!((px[6 + 4] as i32 - 127).abs() <= 1);
        assert_eq!(px[7 + 4], 128);
        // α=20: inv = 12.75. 10*12.75 = 127.5; 5*12.75 = 63.75; 2*12.75 = 25.5.
        assert!((px[12] as i32 - 128).abs() <= 1);
        assert!((px[13] as i32 - 64).abs() <= 1);
        assert!((px[14] as i32 - 26).abs() <= 1);
        assert_eq!(px[15], 20);
    }

    #[test]
    fn unpremultiply_srgb_clamps_noise() {
        // Slightly-invalid asset where rgb > alpha (impossible in a
        // correct premultiply). The clamp to 255 must hold.
        let mut px = [200u8, 100, 50, 10];
        unpremultiply_srgb_inplace(&mut px);
        assert_eq!(px, [255, 255, 255, 10]);
    }

    #[test]
    fn premultiply_linear_zeros_transparent_keeps_opaque_unchanged() {
        let mut px = [
            200, 100, 50, 255, // opaque: untouched (premultiply by 1)
            200, 100, 50, 0, // α=0: rgb zeroed
        ];
        premultiply_linear_into_srgb_inplace(&mut px);
        assert_eq!(&px[0..4], &[200, 100, 50, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn premultiply_linear_half_alpha_matches_linear_premul_then_encode() {
        // sRGB byte 188 ≈ linear 0.5026. Premul by α=128/255≈0.502 →
        // linear 0.2523. srgb_encode → byte ≈ 138 (linear premul,
        // not the byte-space premul which would give 188*0.5=94).
        let mut px = [188u8, 188, 188, 128];
        premultiply_linear_into_srgb_inplace(&mut px);
        for (c, &v) in px[..3].iter().enumerate() {
            assert!(
                (v as i32 - 138).abs() <= 1,
                "channel {} = {}, expected ~138",
                c,
                v,
            );
        }
        assert_eq!(px[3], 128, "alpha unchanged");
    }

    #[test]
    fn alpha_bleed_extends_visible_rgb_into_transparent_neighbors() {
        // 3x3 with a single visible (red, opaque) centre pixel and an
        // 8-pixel transparent ring around it. After bleed, every ring
        // pixel must hold the centre's RGB (alpha still 0).
        let mut rgba = vec![0u8; 3 * 3 * 4];
        // centre at (1, 1)
        rgba[(1 * 3 + 1) * 4] = 220;
        rgba[(1 * 3 + 1) * 4 + 1] = 30;
        rgba[(1 * 3 + 1) * 4 + 2] = 60;
        rgba[(1 * 3 + 1) * 4 + 3] = 255;

        alpha_bleed_inplace(&mut rgba, 3, 3);

        for i in 0..9 {
            let r = rgba[i * 4];
            let g = rgba[i * 4 + 1];
            let b = rgba[i * 4 + 2];
            let a = rgba[i * 4 + 3];
            assert_eq!((r, g, b), (220, 30, 60), "pixel {} RGB", i);
            if i == 4 {
                assert_eq!(a, 255, "centre alpha");
            } else {
                assert_eq!(a, 0, "ring pixel {} alpha", i);
            }
        }
    }

    #[test]
    fn alpha_bleed_no_op_when_fully_opaque_or_fully_transparent() {
        // Fully opaque buffer: no α=0 pixels, nothing to bleed into.
        let mut opaque = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let snapshot = opaque.clone();
        alpha_bleed_inplace(&mut opaque, 2, 1);
        assert_eq!(opaque, snapshot);

        // Fully transparent buffer: no α>0 source — every pixel stays
        // at its initial RGB (the BFS queue is empty so the function
        // exits immediately).
        let mut blank = vec![0u8; 2 * 1 * 4];
        alpha_bleed_inplace(&mut blank, 2, 1);
        assert_eq!(blank, vec![0u8; 8]);
    }

    #[test]
    fn zero_length_texture_slot_becomes_placeholder() {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        let payload = b"{}";
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        data.extend_from_slice(TEX_SECT);
        data.extend_from_slice(&2u32.to_be_bytes());
        // Slot 0: zero-length with a garbage type byte (the reference
        // ignores it). Slot 1: a real 1x1 PNG so indices must line up.
        data.extend_from_slice(&0u32.to_be_bytes());
        data.push(7);
        let png = minimal_png(1, 1, 0x1f15c489);
        data.extend_from_slice(&(png.len() as u32).to_be_bytes());
        data.push(0);
        data.extend_from_slice(&png);

        let model = InxModel::parse(Cursor::new(data)).expect("zero-length slot must parse");
        assert_eq!(model.textures.len(), 2);
        let decoded = model.textures[0].decode().expect("placeholder decodes");
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(&decoded.rgba[..], &[0, 0, 0, 0]);
    }

    #[test]
    fn binary_vendor_section_is_kept_as_raw_bytes() {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        let payload = b"{}";
        data.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        data.extend_from_slice(payload);
        data.extend_from_slice(TEX_SECT);
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(EXT_SECT);
        data.extend_from_slice(&1u32.to_be_bytes());
        let name = b"vendor.bin";
        data.extend_from_slice(&(name.len() as u32).to_be_bytes());
        data.extend_from_slice(name);
        let blob = [0xDEu8, 0xAD, 0xBE, 0xEF]; // not valid JSON/UTF-8
        data.extend_from_slice(&(blob.len() as u32).to_be_bytes());
        data.extend_from_slice(&blob);

        let model = InxModel::parse(Cursor::new(data)).expect("binary vendor data must parse");
        assert_eq!(model.vendors.len(), 1);
        assert_eq!(model.vendors[0].name, "vendor.bin");
        assert_eq!(model.vendors[0].payload, blob);
    }

    #[test]
    fn accepts_dimensions_at_limit() {
        let png = minimal_png(MAX_TEXTURE_DIMENSION, MAX_TEXTURE_DIMENSION, 0x72aaca59);
        let data = build_inx_with_single_png(&png);
        let result = InxModel::parse(Cursor::new(data));
        // Dim check must pass; downstream decode of an empty IDAT may or may not
        // succeed, but must not trip TextureTooLarge / TextureHeader on dims.
        if let Err(e) = &result {
            assert!(
                !matches!(
                    e,
                    InxParseError::TextureTooLarge { .. } | InxParseError::TextureHeader(_)
                ),
                "at-limit dims must pass gate, got {:?}",
                e
            );
        }
    }

    fn build_minimal_inp(payload_json: &[u8]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&(payload_json.len() as u32).to_be_bytes());
        data.extend_from_slice(payload_json);
        data.extend_from_slice(TEX_SECT);
        data.extend_from_slice(&0u32.to_be_bytes());
        data
    }

    #[test]
    fn parse_inp_roundtrip_produces_single_root_node() {
        let payload_json = br#"{
            "nodes": {
                "uuid": 1,
                "name": "Root",
                "type": "Node",
                "transform": {
                    "trans": [0.0, 0.0, 0.0],
                    "rot": [0.0, 0.0, 0.0],
                    "scale": [1.0, 1.0]
                },
                "children": []
            },
            "param": []
        }"#;
        let bytes = build_minimal_inp(payload_json);

        let model = parse_inp(Cursor::new(&bytes)).expect("parse .inp bytes");
        assert!(model.textures.is_empty());
        assert!(model.vendors.is_empty());

        let puppet = crate::importer::from_inx_model(&model).expect("import");
        assert_eq!(puppet.len(), 2);
        assert!(puppet.node_for_uuid(1).is_some());
    }

    #[test]
    fn parse_inp_rejects_wrong_magic() {
        let mut bytes = vec![b'N', b'O', b'P', b'E', 0, 0, 0, 0];
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let err = parse_inp(Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, InpParseError::IncorrectMagic));
    }
}
