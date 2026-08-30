use std::io::{self, Read};

#[inline]
pub(crate) fn read_n<R: Read, const N: usize>(data: &mut R) -> io::Result<[u8; N]> {
    let mut buf = [0_u8; N];
    data.read_exact(&mut buf)?;
    Ok(buf)
}

#[inline]
pub(crate) fn read_u8<R: Read>(data: &mut R) -> io::Result<u8> {
    let buf = read_n::<_, 1>(data)?;
    Ok(u8::from_ne_bytes(buf))
}

#[inline]
pub(crate) fn read_be_u32<R: Read>(data: &mut R) -> io::Result<u32> {
    let buf = read_n::<_, 4>(data)?;
    Ok(u32::from_be_bytes(buf))
}

#[inline]
pub(crate) fn read_vec<R: Read>(data: &mut R, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0_u8; n];
    data.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_read_u8() {
        let data = vec![42u8];
        let mut cursor = Cursor::new(data);
        assert_eq!(read_u8(&mut cursor).unwrap(), 42);
    }

    #[test]
    fn test_read_be_u32() {
        let data = vec![0x00, 0x00, 0x01, 0x00];
        let mut cursor = Cursor::new(data);
        assert_eq!(read_be_u32(&mut cursor).unwrap(), 256);
    }

    #[test]
    fn test_read_vec() {
        let data = vec![1, 2, 3, 4, 5];
        let mut cursor = Cursor::new(data);
        let result = read_vec(&mut cursor, 3).unwrap();
        assert_eq!(result, vec![1, 2, 3]);
    }
}
