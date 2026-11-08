//! MSB-first bit I/O for FLAC (no byte stuffing).

#[derive(Default)]
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    pub fn new(out: Vec<u8>) -> Self {
        Self { out, acc: 0, nbits: 0 }
    }

    /// Absolute bit position of the next bit.
    pub fn bit_pos(&self) -> u64 {
        self.out.len() as u64 * 8 + self.nbits as u64
    }

    pub fn is_aligned(&self) -> bool {
        self.nbits == 0
    }

    /// Write the low `n` bits of `v` (n ≤ 32).
    #[inline]
    pub fn write(&mut self, v: u32, n: u32) {
        debug_assert!(n <= 32);
        if n == 0 {
            return;
        }
        let v = (v as u64) & ((1u64 << n) - 1);
        self.acc = (self.acc << n) | v;
        self.nbits += n;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.out.push((self.acc >> self.nbits) as u8);
        }
        self.acc &= (1u64 << self.nbits) - 1;
    }

    pub fn write64(&mut self, v: u64, n: u32) {
        if n > 32 {
            self.write((v >> 32) as u32, n - 32);
            self.write(v as u32, 32);
        } else {
            self.write(v as u32, n);
        }
    }

    /// Two's-complement signed value in `n` bits.
    #[inline]
    pub fn write_signed(&mut self, v: i64, n: u32) {
        self.write64(v as u64, n);
    }

    /// `q` zeros followed by a one.
    #[inline]
    pub fn write_unary(&mut self, mut q: u64) {
        while q >= 32 {
            self.write(0, 32);
            q -= 32;
        }
        self.write(1, q as u32 + 1);
    }

    /// Rice code of a signed residual with parameter `k`.
    #[inline]
    pub fn write_rice(&mut self, e: i64, k: u32) {
        let u = zigzag(e);
        self.write_unary(u >> k);
        if k > 0 {
            self.write64(u & ((1u64 << k) - 1), k);
        }
    }

    /// FLAC's UTF-8-like coding of frame / sample numbers (up to 36 bits).
    pub fn write_utf8(&mut self, v: u64) {
        if v < 0x80 {
            self.write(v as u32, 8);
            return;
        }
        let n = (1..=6).find(|&n| v < 1u64 << (5 * n + 6)).unwrap_or(6) + 1; // total bytes
        let first_bits = 7 - n; // payload bits in the first byte
        let lead = (0xFFu32 << (8 - n)) & 0xFF;
        self.write(lead | (v >> (6 * (n - 1))) as u32 & ((1 << first_bits) - 1), 8);
        for i in (0..n - 1).rev() {
            self.write(0x80 | ((v >> (6 * i)) & 0x3F) as u32, 8);
        }
    }

    /// Zero-pad to a byte boundary.
    pub fn align(&mut self) {
        if self.nbits > 0 {
            self.write(0, 8 - self.nbits);
        }
    }
}

#[inline]
pub fn zigzag(e: i64) -> u64 {
    ((e << 1) ^ (e >> 63)) as u64
}

#[inline]
pub fn unzigzag(u: u64) -> i64 {
    (u >> 1) as i64 ^ -((u & 1) as i64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("unexpected end of data")]
pub struct Eof;

/// MSB-first reader over a byte slice.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: u64,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], byte: usize) -> Self {
        Self { data, pos: byte as u64 * 8 }
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn byte_pos(&self) -> usize {
        (self.pos / 8) as usize
    }

    pub fn bits_left(&self) -> u64 {
        (self.data.len() as u64 * 8).saturating_sub(self.pos)
    }

    /// 64 bits starting at the current position (zero-padded past the end).
    #[inline]
    fn window(&self) -> u64 {
        let byte = (self.pos / 8) as usize;
        let mut buf = [0u8; 8];
        let n = self.data.len().saturating_sub(byte).min(8);
        buf[..n].copy_from_slice(&self.data[byte..byte + n]);
        u64::from_be_bytes(buf) << (self.pos % 8)
    }

    /// Read `n` ≤ 32 bits.
    #[inline]
    pub fn read(&mut self, n: u32) -> Result<u32, Eof> {
        if n == 0 {
            return Ok(0);
        }
        if self.bits_left() < n as u64 {
            return Err(Eof);
        }
        // `window` holds at least 57 valid bits after the shift.
        let v = (self.window() >> (64 - n)) as u32;
        self.pos += n as u64;
        Ok(v)
    }

    pub fn read64(&mut self, n: u32) -> Result<u64, Eof> {
        if n > 32 {
            let hi = self.read(n - 32)? as u64;
            Ok((hi << 32) | self.read(32)? as u64)
        } else {
            Ok(self.read(n)? as u64)
        }
    }

    pub fn read_signed(&mut self, n: u32) -> Result<i64, Eof> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read64(n)?;
        Ok(((v << (64 - n)) as i64) >> (64 - n))
    }

    pub fn bit(&mut self) -> Result<bool, Eof> {
        Ok(self.read(1)? == 1)
    }

    /// Count zeros up to and including the terminating one.
    #[inline]
    pub fn read_unary(&mut self) -> Result<u64, Eof> {
        let mut q = 0u64;
        loop {
            if self.bits_left() == 0 {
                return Err(Eof);
            }
            let w = self.window();
            let avail = self.bits_left().min(57) as u32;
            let z = w.leading_zeros().min(avail);
            if z < avail {
                self.pos += z as u64 + 1;
                return Ok(q + z as u64);
            }
            q += z as u64;
            self.pos += z as u64;
        }
    }

    #[inline]
    pub fn read_rice(&mut self, k: u32) -> Result<i64, Eof> {
        let q = self.read_unary()?;
        let r = if k > 0 { self.read64(k)? } else { 0 };
        Ok(unzigzag((q << k) | r))
    }

    pub fn read_utf8(&mut self) -> Result<Option<u64>, Eof> {
        let first = self.read(8)?;
        let n = (first as u8).leading_ones();
        if n == 0 {
            return Ok(Some(first as u64));
        }
        if n == 1 || n > 7 {
            return Ok(None);
        }
        let mut v = (first & ((1 << (7 - n)) - 1)) as u64;
        for _ in 1..n {
            let b = self.read(8)?;
            if b & 0xC0 != 0x80 {
                return Ok(None);
            }
            v = (v << 6) | (b & 0x3F) as u64;
        }
        Ok(Some(v))
    }

    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    pub fn seek_bit(&mut self, bit: u64) {
        self.pos = bit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed() {
        let mut w = BitWriter::default();
        w.write(0b101, 3);
        w.write_signed(-5, 7);
        w.write_unary(0);
        w.write_unary(70);
        w.write_rice(-17, 3);
        w.write_rice(123_456, 0);
        w.write64(0x1_2345_6789, 36);
        for v in [0u64, 0x7F, 0x80, 0x7FF, 0x800, 0xFFFF, 0x1_0000, 0x7FF_FFFF, 0xF_FFFF_FFFF] {
            w.write_utf8(v);
        }
        w.align();
        let mut r = BitReader::new(&w.out);
        assert_eq!(r.read(3).unwrap(), 0b101);
        assert_eq!(r.read_signed(7).unwrap(), -5);
        assert_eq!(r.read_unary().unwrap(), 0);
        assert_eq!(r.read_unary().unwrap(), 70);
        assert_eq!(r.read_rice(3).unwrap(), -17);
        assert_eq!(r.read_rice(0).unwrap(), 123_456);
        assert_eq!(r.read64(36).unwrap(), 0x1_2345_6789);
        for v in [0u64, 0x7F, 0x80, 0x7FF, 0x800, 0xFFFF, 0x1_0000, 0x7FF_FFFF, 0xF_FFFF_FFFF] {
            assert_eq!(r.read_utf8().unwrap(), Some(v), "{v:#x}");
        }
        r.align();
        assert_eq!(r.read(1), Err(Eof));
    }

    #[test]
    fn utf8_matches_spec_examples() {
        let mut w = BitWriter::default();
        w.write_utf8(0x80);
        assert_eq!(w.out, vec![0xC2, 0x80]);
        let mut w = BitWriter::default();
        w.write_utf8(0x10000);
        assert_eq!(w.out, vec![0xF0, 0x90, 0x80, 0x80]);
    }

    #[test]
    fn zigzag_inverse() {
        for e in [0i64, 1, -1, 2, -2, i32::MAX as i64, i32::MIN as i64] {
            assert_eq!(unzigzag(zigzag(e)), e);
        }
        assert_eq!(zigzag(-1), 1);
        assert_eq!(zigzag(1), 2);
    }
}
