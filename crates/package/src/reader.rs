use crate::error::{Error, ErrorKind, Result};

/// Little-endian cursor over part of a file. `base` is the absolute file offset of `data[0]`,
/// so errors and hexdumps always refer to real file offsets.
#[derive(Clone)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    base: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_base(data, 0)
    }

    pub fn with_base(data: &'a [u8], base: usize) -> Self {
        Self { data, pos: 0, base }
    }

    pub fn offset(&self) -> usize {
        self.base + self.pos
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_at_end(&self) -> bool {
        self.pos == self.data.len()
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(self.eof(pos - self.pos));
        }
        self.pos = pos;
        Ok(())
    }

    pub fn seek_abs(&mut self, offset: usize) -> Result<()> {
        let pos = offset
            .checked_sub(self.base)
            .ok_or_else(|| Error::invalid(self.offset(), format!("offset {offset:#x} is before the region")))?;
        self.seek(pos)
    }

    fn eof(&self, wanted: usize) -> Error {
        Error::new(self.offset(), ErrorKind::UnexpectedEof { wanted, available: self.remaining() })
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(self.eof(n));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.data[self.pos..];
        self.pos = self.data.len();
        s
    }

    /// Sub-reader over the next `n` bytes; advances `self`.
    pub fn sub(&mut self, n: usize) -> Result<Reader<'a>> {
        let base = self.offset();
        Ok(Reader::with_base(self.bytes(n)?, base))
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("fixed length"))
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.array()?))
    }

    /// Unreal "compact index". First byte: bit 7 sign, bit 6 continue, bits 0..5 value.
    /// Following bytes: bit 7 continue, bits 0..6 value. At most 5 bytes.
    pub fn compact(&mut self) -> Result<i32> {
        let start = self.offset();
        let b0 = self.u8()?;
        let negative = b0 & 0x80 != 0;
        let mut value: u32 = (b0 & 0x3f) as u32;
        let mut more = b0 & 0x40 != 0;
        let mut shift = 6;
        let mut count = 1;
        while more {
            if count == 5 {
                return Err(Error::new(start, ErrorKind::CompactIndexOverflow));
            }
            let b = self.u8()?;
            value |= ((b & 0x7f) as u32) << shift;
            more = b & 0x80 != 0;
            shift += 7;
            count += 1;
        }
        let v = value as i32;
        Ok(if negative { v.wrapping_neg() } else { v })
    }

    pub fn compact_len(&mut self) -> Result<usize> {
        let at = self.offset();
        let v = self.compact()?;
        usize::try_from(v).map_err(|_| Error::invalid(at, format!("negative length {v}")))
    }

    /// Element count, rejected if `count * min_elem_size` exceeds the remaining bytes.
    pub fn count(&mut self, min_elem_size: usize) -> Result<usize> {
        let at = self.offset();
        let n = self.compact_len()?;
        if min_elem_size > 0 && n > self.remaining() / min_elem_size {
            return Err(Error::invalid(
                at,
                format!("impossible count {n}: {} bytes left", self.remaining()),
            ));
        }
        Ok(n)
    }

    pub fn expect_end(&self, context: &str) -> Result<()> {
        if self.is_at_end() {
            Ok(())
        } else {
            Err(Error::new(
                self.offset(),
                ErrorKind::LengthMismatch {
                    context: context.to_string(),
                    expected: self.len(),
                    consumed: self.pos,
                },
            ))
        }
    }
}

/// Latin-1 decoding: one byte per code point, losslessly reversible.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_index() {
        let cases: &[(&[u8], i32)] = &[
            (&[0x00], 0),
            (&[0x3f], 63),
            (&[0x80 | 0x01], -1),
            (&[0x40, 0x01], 64),
            (&[0x7f, 0x7f], 63 + (127 << 6)),
            (&[0x40, 0x80, 0x01], 1 << 13),
        ];
        for (bytes, want) in cases {
            let mut r = Reader::new(bytes);
            assert_eq!(r.compact().unwrap(), *want, "{bytes:02x?}");
            assert!(r.is_at_end());
        }
    }
}
