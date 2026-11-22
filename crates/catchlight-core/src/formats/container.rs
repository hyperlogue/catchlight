//! Shared low-level framing for catchlight's binary model container
//! (`.clm`). A fixed header — 8-byte magic, `format_version: u16`,
//! `section_count: u32` — followed by a section table of
//! `{ kind: u32, offset: u64, len: u32 }`, then the section payloads, all
//! little-endian. Every count, length, and offset read from the file is
//! validated against the buffer size before use, so a malformed or hostile
//! file errors instead of panicking or over-allocating.
//!
//! **Whatever [`write`] returns, [`read`] accepts.** The writer holds its
//! input to the reader's caps — [`MAX_SECTION_COUNT`] sections, each at most
//! [`MAX_SECTION_LEN`] bytes — and refuses what is over them, rather than
//! truncating a length into the table's `u32` and leaving a file that only
//! fails, or silently misreads, at the next open.
//!
//! Section *meaning* — which `kind` is Structure, Textures, TextureManifest
//! or Extensions — lives in the `.clm` layer; this module frames opaque byte
//! sections and owns only the version word that layer branches on.

use thiserror::Error;

const HEADER_LEN: usize = 8 + 2 + 4;
const SECTION_ENTRY_LEN: usize = 4 + 8 + 4;

/// The most sections one container may frame. A cap on an untrusted count
/// read from the file, and on what [`write`] will frame.
pub const MAX_SECTION_COUNT: u32 = 4096;
/// The most bytes one section may hold. A section can hold a whole texture
/// atlas, so the cap is generous; on the way in it turns a corrupt `u32::MAX`
/// length into an error rather than an allocation, and on the way out it is
/// what keeps a length from being truncated into the table's `u32`.
pub const MAX_SECTION_LEN: u64 = 512 * 1024 * 1024;

/// Hold one section's length to [`MAX_SECTION_LEN`], the same test on the
/// way in and on the way out. Returns the length as the table stores it.
pub fn check_section_len(kind: u32, len: u64) -> Result<u32, ContainerError> {
    u32::try_from(len)
        .ok()
        .filter(|_| len <= MAX_SECTION_LEN)
        .ok_or(ContainerError::SectionTooLarge { kind, len })
}

fn check_section_count(count: usize) -> Result<u32, ContainerError> {
    let shown = u32::try_from(count).unwrap_or(u32::MAX);
    if shown > MAX_SECTION_COUNT {
        return Err(ContainerError::TooManySections(shown));
    }
    Ok(shown)
}

#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerError {
    #[error("unexpected magic bytes")]
    BadMagic,
    #[error("file shorter than the container header/section table")]
    Truncated,
    #[error("section count {0} exceeds the limit")]
    TooManySections(u32),
    #[error("section (kind {kind}) length {len} exceeds the limit")]
    SectionTooLarge { kind: u32, len: u64 },
    #[error("section (kind {kind}) at [{offset}, +{len}) runs past the {file_len}-byte file")]
    SectionOutOfBounds {
        kind: u32,
        offset: u64,
        len: u64,
        file_len: usize,
    },
}

/// One framed byte range. Used both as write input (borrowing the caller's
/// bytes) and as the read view (borrowing the parsed file buffer).
#[derive(Debug)]
pub struct Section<'a> {
    pub kind: u32,
    pub data: &'a [u8],
}

#[derive(Debug)]
pub struct Container<'a> {
    pub version: u16,
    pub sections: Vec<Section<'a>>,
}

impl<'a> Container<'a> {
    /// The first section with this `kind`, or `None`. Section kinds are
    /// expected unique per file; duplicates resolve to the first.
    pub fn section(&self, kind: u32) -> Option<&'a [u8]> {
        self.sections
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.data)
    }
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let b: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
    Some(u16::from_le_bytes(b))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(b))
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    let b: [u8; 8] = bytes.get(at..at + 8)?.try_into().ok()?;
    Some(u64::from_le_bytes(b))
}

/// Serialize `magic` + `version` + the section table + the payloads. Section
/// offsets are absolute from the file start and laid out in `sections` order
/// directly after the table; the result is deterministic for a given input.
///
/// Refuses what [`read`] would refuse — too many sections, or one too long —
/// before copying a byte, so the error costs nothing.
pub fn write(
    magic: &[u8; 8],
    version: u16,
    sections: &[Section],
) -> Result<Vec<u8>, ContainerError> {
    let count = check_section_count(sections.len())?;
    let mut lens = Vec::with_capacity(sections.len());
    for s in sections {
        lens.push(check_section_len(s.kind, s.data.len() as u64)?);
    }

    let table_len = sections.len() * SECTION_ENTRY_LEN;
    let payloads_len: usize = sections.iter().map(|s| s.data.len()).sum();
    let mut out = Vec::with_capacity(HEADER_LEN + table_len + payloads_len);

    out.extend_from_slice(magic);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());

    let mut offset = (HEADER_LEN + table_len) as u64;
    for (s, len) in sections.iter().zip(lens) {
        out.extend_from_slice(&s.kind.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&len.to_le_bytes());
        offset += u64::from(len);
    }
    for s in sections {
        out.extend_from_slice(s.data);
    }
    Ok(out)
}

/// Parse a container, checking `magic` and bounds-checking the table against
/// `bytes`. The returned sections borrow `bytes`.
pub fn read<'a>(
    bytes: &'a [u8],
    expected_magic: &[u8; 8],
) -> Result<Container<'a>, ContainerError> {
    if bytes.len() < HEADER_LEN {
        return Err(ContainerError::Truncated);
    }
    if &bytes[..8] != expected_magic {
        return Err(ContainerError::BadMagic);
    }
    let version = le_u16(bytes, 8).ok_or(ContainerError::Truncated)?;
    let count = le_u32(bytes, 10).ok_or(ContainerError::Truncated)?;
    if count > MAX_SECTION_COUNT {
        return Err(ContainerError::TooManySections(count));
    }

    let table_end = HEADER_LEN + count as usize * SECTION_ENTRY_LEN;
    if bytes.len() < table_end {
        return Err(ContainerError::Truncated);
    }

    let file_len = bytes.len();
    let mut sections = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let base = HEADER_LEN + i * SECTION_ENTRY_LEN;
        let kind = le_u32(bytes, base).ok_or(ContainerError::Truncated)?;
        let offset = le_u64(bytes, base + 4).ok_or(ContainerError::Truncated)?;
        let len = le_u32(bytes, base + 12).ok_or(ContainerError::Truncated)? as u64;

        check_section_len(kind, len)?;
        let oob = ContainerError::SectionOutOfBounds {
            kind,
            offset,
            len,
            file_len,
        };
        let end = offset
            .checked_add(len)
            .ok_or(ContainerError::SectionOutOfBounds {
                kind,
                offset,
                len,
                file_len,
            })?;
        if end > file_len as u64 {
            return Err(oob);
        }
        let data =
            bytes
                .get(offset as usize..end as usize)
                .ok_or(ContainerError::SectionOutOfBounds {
                    kind,
                    offset,
                    len,
                    file_len,
                })?;
        sections.push(Section { kind, data });
    }
    Ok(Container { version, sections })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: &[u8; 8] = b"CLTEST\0\0";

    #[test]
    fn roundtrip_preserves_version_kinds_and_payloads() {
        let a = vec![1u8, 2, 3, 4, 5];
        let b = vec![9u8; 100];
        let sections = [
            Section { kind: 7, data: &a },
            Section { kind: 42, data: &b },
        ];
        let bytes = write(MAGIC, 3, &sections).unwrap();
        let c = read(&bytes, MAGIC).unwrap();
        assert_eq!(c.version, 3);
        assert_eq!(c.sections.len(), 2);
        assert_eq!(c.section(7), Some(a.as_slice()));
        assert_eq!(c.section(42), Some(b.as_slice()));
        assert_eq!(c.section(99), None);
    }

    #[test]
    fn empty_section_list_roundtrips() {
        let bytes = write(MAGIC, 1, &[]).unwrap();
        let c = read(&bytes, MAGIC).unwrap();
        assert_eq!(c.version, 1);
        assert!(c.sections.is_empty());
    }

    /// The writer refuses what the reader would: a count over the cap never
    /// reaches the header, and the largest count the reader takes round-trips.
    #[test]
    fn write_refuses_more_sections_than_read_accepts() {
        let at_cap: Vec<Section> = (0..MAX_SECTION_COUNT)
            .map(|kind| Section { kind, data: &[] })
            .collect();
        let bytes = write(MAGIC, 1, &at_cap).unwrap();
        assert_eq!(
            read(&bytes, MAGIC).unwrap().sections.len(),
            MAX_SECTION_COUNT as usize
        );

        let over: Vec<Section> = (0..=MAX_SECTION_COUNT)
            .map(|kind| Section { kind, data: &[] })
            .collect();
        assert_eq!(
            write(MAGIC, 1, &over).err(),
            Some(ContainerError::TooManySections(MAX_SECTION_COUNT + 1))
        );
    }

    /// A section over the cap used to be written with its length truncated
    /// to the table's `u32` — or, under 4 GiB, written whole into a file the
    /// reader then refused. Refused before a byte is copied, so the test's
    /// zero pages are never touched.
    #[test]
    fn write_refuses_a_section_longer_than_read_accepts() {
        let over = vec![0u8; MAX_SECTION_LEN as usize + 1];
        let sections = [
            Section {
                kind: 7,
                data: &[1, 2, 3],
            },
            Section {
                kind: 9,
                data: &over,
            },
        ];
        assert_eq!(
            write(MAGIC, 1, &sections).err(),
            Some(ContainerError::SectionTooLarge {
                kind: 9,
                len: MAX_SECTION_LEN + 1,
            })
        );
    }

    #[test]
    fn the_section_length_check_is_the_readers() {
        assert_eq!(check_section_len(0, 0), Ok(0));
        assert_eq!(
            check_section_len(0, MAX_SECTION_LEN),
            Ok(MAX_SECTION_LEN as u32)
        );
        for len in [MAX_SECTION_LEN + 1, u64::from(u32::MAX) + 1, u64::MAX] {
            assert_eq!(
                check_section_len(3, len),
                Err(ContainerError::SectionTooLarge { kind: 3, len })
            );
        }
    }

    #[test]
    fn wrong_magic_is_rejected() {
        let bytes = write(MAGIC, 1, &[]).unwrap();
        assert_eq!(
            read(&bytes, b"OTHER\0\0\0").err(),
            Some(ContainerError::BadMagic)
        );
    }

    #[test]
    fn short_buffer_is_truncated_not_panic() {
        assert_eq!(
            read(&[0u8; 3], MAGIC).err(),
            Some(ContainerError::Truncated)
        );
    }

    #[test]
    fn declared_count_past_eof_is_truncated() {
        // Valid header claiming one section, but no table bytes follow.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(read(&bytes, MAGIC).err(), Some(ContainerError::Truncated));
    }

    #[test]
    fn section_length_past_eof_is_out_of_bounds() {
        // One section whose declared length overruns the file.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&5u32.to_le_bytes()); // kind
        bytes.extend_from_slice(&(HEADER_LEN as u64 + SECTION_ENTRY_LEN as u64).to_le_bytes()); // offset
        bytes.extend_from_slice(&1000u32.to_le_bytes()); // len overruns
        match read(&bytes, MAGIC) {
            Err(ContainerError::SectionOutOfBounds { kind: 5, .. }) => {}
            other => panic!("expected SectionOutOfBounds, got {other:?}"),
        }
    }

    #[test]
    fn excessive_section_count_is_rejected_before_allocation() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            read(&bytes, MAGIC).err(),
            Some(ContainerError::TooManySections(u32::MAX))
        );
    }
}
