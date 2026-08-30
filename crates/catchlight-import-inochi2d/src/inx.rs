use std::io::Read;
use std::sync::Arc;
use thiserror::Error;

use catchlight_core::load_budget::{
    LoadBudget, LoadLimitError, LoadResource, MAX_TEXTURE_DIMENSION,
};
use catchlight_core::texture::{EncodedTexture, TextureFormat};

use crate::read::{read_be_u32, read_n, read_u8, read_vec};

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
    pub textures: Vec<EncodedTexture>,
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
) -> Result<EncodedTexture, InxParseError> {
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
    Ok(EncodedTexture {
        format,
        data: tex_data,
        premultiplied: true,
    })
}

fn placeholder_texture() -> Result<EncodedTexture, InxParseError> {
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(1, 1))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| InxParseError::TextureHeader(e.to_string()))?;
    Ok(EncodedTexture {
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
) -> Result<Vec<EncodedTexture>, InxParseError> {
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
        let mut budget = LoadBudget::new(catchlight_core::load_budget::LoadLimits {
            decoded_texture_bytes: 4,
            ..catchlight_core::load_budget::LoadLimits::default()
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

        let doc = crate::from_inx_model(&model).expect("import").doc;
        assert_eq!(doc.nodes.len(), 1);
        assert_eq!(doc.nodes[0].name, "Root");
    }

    #[test]
    fn parse_inp_rejects_wrong_magic() {
        let mut bytes = vec![b'N', b'O', b'P', b'E', 0, 0, 0, 0];
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let err = parse_inp(Cursor::new(bytes)).unwrap_err();
        assert!(matches!(err, InpParseError::IncorrectMagic));
    }
}
