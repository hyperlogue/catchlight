//! `replace-texture <file> <tex-id> <image>` — swap one texture's source bytes.
//!
//! `.clm` stores a texture exactly as the author supplied it and never
//! re-encodes it, so replacing one is a byte swap: read the image file, put
//! its bytes in the slot the Id names, write the model back. Nothing here
//! decodes the image — not the old one, not the new one — which is also why
//! the tool cannot tell you the new image is the same size as the old.
//!
//! The two things it does look at:
//!
//! - **The encoding**, because the slot records it. The bytes' own signature
//!   decides when there is one (PNG's 8-byte magic, TGA 2.0's
//!   `TRUEVISION-XFILE.` footer); otherwise the file extension does; and if
//!   neither says, the swap is refused rather than guessed at.
//! - **The alpha convention**, which the bytes cannot tell anyone. The slot's
//!   existing convention is kept unless `--alpha` says otherwise — a straight
//!   image dropped into a premultiplied slot renders wrong, and only the
//!   person doing the swap knows which it is.

use std::path::Path;

use catchlight_core::formats::clm::{TextureAlpha, TextureEncoding};

use crate::{file, Error};

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const TGA_FOOTER: &[u8] = b"TRUEVISION-XFILE.\0";

/// What one `replace-texture` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    pub id: String,
    pub encoding: TextureEncoding,
    pub alpha: TextureAlpha,
    pub before_bytes: usize,
    pub after_bytes: usize,
}

impl std::fmt::Display for Replaced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "texture {:?}: {} bytes -> {} bytes ({:?}, {:?})",
            self.id, self.before_bytes, self.after_bytes, self.encoding, self.alpha
        )
    }
}

/// Put `image`'s bytes into the texture `tex_id` names, writing to `out` if
/// given and over `path` otherwise.
pub fn run(
    path: &Path,
    tex_id: &str,
    image: &Path,
    alpha: Option<TextureAlpha>,
    out: Option<&Path>,
) -> Result<Replaced, Error> {
    let mut clm = file::read(path)?;
    let bytes = std::fs::read(image).map_err(|e| Error::io(image, e))?;
    let encoding = encoding_of(image, &bytes).ok_or_else(|| Error::UnknownImageEncoding {
        path: image.to_path_buf(),
    })?;

    let Some(slot) = clm.textures.iter_mut().find(|t| t.id.as_str() == tex_id) else {
        return Err(Error::NoSuchId {
            path: path.to_path_buf(),
            kind: "texture",
            id: tex_id.to_string(),
        });
    };
    let replaced = Replaced {
        id: tex_id.to_string(),
        encoding,
        alpha: alpha.unwrap_or(slot.alpha),
        before_bytes: slot.data.len(),
        after_bytes: bytes.len(),
    };
    slot.encoding = encoding;
    slot.alpha = replaced.alpha;
    slot.data = bytes;

    let dest = out.unwrap_or(path);
    let encoded = file::encode(&clm, dest)?;
    file::write(dest, &encoded)?;
    Ok(replaced)
}

/// Which of the two encodings `.clm` stores these bytes are, from the bytes'
/// own signature first and the file extension second.
///
/// TGA has no leading magic, only an optional 2.0 footer, so a TGA 1.0 file is
/// recognised by its extension alone — which is the reason the extension is
/// consulted at all.
pub fn encoding_of(path: &Path, bytes: &[u8]) -> Option<TextureEncoding> {
    if bytes.starts_with(PNG_MAGIC) {
        return Some(TextureEncoding::Png);
    }
    if bytes.ends_with(TGA_FOOTER) {
        return Some(TextureEncoding::Tga);
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(TextureEncoding::Png),
        "tga" => Some(TextureEncoding::Tga),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_signature_beats_the_extension_and_neither_decodes_the_image() {
        let png = [PNG_MAGIC, b"not actually an image"].concat();
        assert_eq!(
            encoding_of(Path::new("mislabelled.tga"), &png),
            Some(TextureEncoding::Png)
        );

        let tga = [b"whatever".as_slice(), TGA_FOOTER].concat();
        assert_eq!(
            encoding_of(Path::new("mislabelled.png"), &tga),
            Some(TextureEncoding::Tga)
        );
    }

    #[test]
    fn the_extension_answers_when_the_bytes_do_not() {
        assert_eq!(
            encoding_of(Path::new("skin.TGA"), b"tga 1.0 has no footer"),
            Some(TextureEncoding::Tga)
        );
        assert_eq!(
            encoding_of(Path::new("skin.png"), b"truncated"),
            Some(TextureEncoding::Png)
        );
        assert_eq!(encoding_of(Path::new("skin.webp"), b"riff"), None);
        assert_eq!(encoding_of(Path::new("skin"), b"riff"), None);
    }
}
