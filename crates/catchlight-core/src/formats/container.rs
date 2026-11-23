//! Framing for the `.clm` container; all fields little-endian.
//!
//! Every count, length, and offset read from the file is validated against
//! the buffer before use. The writer refuses input over the reader's caps
//! rather than truncating a length into the table's `u32`.

use thiserror::Error;

const HEADER_LEN: usize = 8 + 2 + 4;
const SECTION_ENTRY_LEN: usize = 4 + 8 + 4;

pub const MAX_SECTION_COUNT: u32 = 4096;
/// A cap generous enough for a texture atlas; turns a corrupt `u32::MAX`
/// length into an error rather than an allocation.
pub const MAX_SECTION_LEN: u64 = 512 * 1024 * 1024;

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
    /// Duplicate kinds resolve to the first.
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

/// Offsets are absolute from the file start; output is deterministic.
pub fn write(
    magic: &[u8; 8],
    version: u16,
    sections: &[Section],
) -> Result<Vec<u8>, ContainerError> {
    for s in sections {
        check_section_len(s.kind, s.data.len() as u64)?;
    }
    let mut file = Writer::new(magic, version, sections.len())?;
    file.reserve(sections.iter().map(|s| s.data.len()).sum());
    for s in sections {
        file.section(s.kind, |out| {
            out.extend_from_slice(s.data);
            Ok::<(), ContainerError>(())
        })?;
    }
    Ok(file.finish())
}

/// Writes each payload straight into the file's buffer, avoiding the
/// file-sized copy [`write()`] makes. The section count is fixed up front.
pub struct Writer {
    out: Vec<u8>,
    declared: usize,
    written: usize,
}

impl Writer {
    pub fn new(magic: &[u8; 8], version: u16, sections: usize) -> Result<Self, ContainerError> {
        let count = check_section_count(sections)?;
        let mut out = Vec::with_capacity(HEADER_LEN + sections * SECTION_ENTRY_LEN);
        out.extend_from_slice(magic);
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.resize(HEADER_LEN + sections * SECTION_ENTRY_LEN, 0);
        Ok(Self {
            out,
            declared: sections,
            written: 0,
        })
    }

    pub fn reserve(&mut self, bytes: usize) {
        self.out.reserve(bytes);
    }

    pub fn section<E: From<ContainerError>>(
        &mut self,
        kind: u32,
        fill: impl FnOnce(&mut SectionWriter<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.written == self.declared {
            let shown = u32::try_from(self.declared + 1).unwrap_or(u32::MAX);
            return Err(ContainerError::TooManySections(shown).into());
        }
        let start = self.out.len();
        fill(&mut SectionWriter(&mut self.out))?;
        let len = check_section_len(kind, (self.out.len() - start) as u64)?;

        let entry = HEADER_LEN + self.written * SECTION_ENTRY_LEN;
        self.out[entry..entry + 4].copy_from_slice(&kind.to_le_bytes());
        self.out[entry + 4..entry + 12].copy_from_slice(&(start as u64).to_le_bytes());
        self.out[entry + 12..entry + 16].copy_from_slice(&len.to_le_bytes());
        self.written += 1;
        Ok(())
    }

    /// # Panics
    ///
    /// If fewer sections were written than declared.
    pub fn finish(self) -> Vec<u8> {
        assert_eq!(
            self.written, self.declared,
            "a container writer declared more sections than it wrote"
        );
        self.out
    }
}

pub struct SectionWriter<'a>(&'a mut Vec<u8>);

impl SectionWriter<'_> {
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

impl std::io::Write for SectionWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0.extend_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

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
    fn the_in_place_writer_writes_the_same_file() {
        let a = vec![1u8, 2, 3, 4, 5];
        let b = vec![9u8; 100];
        let expected = write(
            MAGIC,
            3,
            &[
                Section { kind: 7, data: &a },
                Section {
                    kind: 42,
                    data: &[],
                },
                Section { kind: 9, data: &b },
            ],
        )
        .unwrap();

        let mut file = Writer::new(MAGIC, 3, 3).unwrap();
        let ok = Ok::<(), ContainerError>(());
        file.section(7, |out| {
            out.extend_from_slice(&a[..2]);
            std::io::Write::write_all(out, &a[2..]).unwrap();
            ok
        })
        .unwrap();
        file.section(42, |_| Ok::<(), ContainerError>(())).unwrap();
        file.section(9, |out| {
            out.extend_from_slice(&b);
            Ok::<(), ContainerError>(())
        })
        .unwrap();

        assert_eq!(file.finish(), expected);
    }

    #[test]
    fn the_in_place_writer_refuses_a_section_it_did_not_declare() {
        let mut file = Writer::new(MAGIC, 1, 1).unwrap();
        file.section(0, |_| Ok::<(), ContainerError>(())).unwrap();
        assert_eq!(
            file.section(1, |_| Ok::<(), ContainerError>(())).err(),
            Some(ContainerError::TooManySections(2))
        );
        assert!(matches!(
            Writer::new(MAGIC, 1, MAX_SECTION_COUNT as usize + 1),
            Err(ContainerError::TooManySections(_))
        ));
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
