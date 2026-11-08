//! Entropy-coded segment bit I/O with 0xFF byte stuffing.

/// MSB-first bit writer that stuffs a 0x00 after every 0xFF data byte.
#[derive(Default)]
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u32,
    nbits: u8,
}

impl BitWriter {
    pub fn new(out: Vec<u8>) -> Self {
        Self { out, acc: 0, nbits: 0 }
    }

    /// Absolute bit position in `out` of the next bit to be written.
    pub fn bit_pos(&self) -> u64 {
        self.out.len() as u64 * 8 + self.nbits as u64
    }

    /// Write the low `n` bits of `bits` (n ≤ 16).
    pub fn write(&mut self, bits: u32, n: u8) {
        debug_assert!(n <= 16);
        for i in (0..n).rev() {
            self.acc = (self.acc << 1) | ((bits >> i) & 1);
            self.nbits += 1;
            if self.nbits == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0x00);
                }
                self.acc = 0;
                self.nbits = 0;
            }
        }
    }

    /// Pad to a byte boundary with 1-bits (T.81 F.1.2.3).
    pub fn flush(&mut self) {
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            self.write((1 << pad) - 1, pad);
        }
    }

    /// Flush then emit RSTn.
    pub fn restart(&mut self, n: u8) {
        self.flush();
        self.out.extend_from_slice(&[0xFF, 0xD0 + (n & 7)]);
    }
}

/// An entropy-coded segment with stuffing and RST markers removed.
#[derive(Debug, Default)]
pub struct Ecs {
    pub data: Vec<u8>,
    /// File offset of each byte in `data`.
    pub offsets: Vec<usize>,
    /// `data` index at which each RST marker occurred (in order).
    pub restarts: Vec<usize>,
    /// File offset of the marker that terminated the segment (or file length).
    pub end: usize,
}

impl Ecs {
    /// Extract the entropy-coded data starting at `start` up to the next non-RST marker.
    pub fn extract(file: &[u8], start: usize) -> Self {
        let mut e = Ecs::default();
        let mut i = start;
        while i < file.len() {
            let b = file[i];
            if b != 0xFF {
                e.data.push(b);
                e.offsets.push(i);
                i += 1;
                continue;
            }
            match file.get(i + 1) {
                Some(0x00) => {
                    e.data.push(0xFF);
                    e.offsets.push(i);
                    i += 2;
                }
                Some(0xFF) => i += 1, // fill byte
                Some(m) if (0xD0..=0xD7).contains(m) => {
                    e.restarts.push(e.data.len());
                    i += 2;
                }
                _ => break,
            }
        }
        e.end = i.min(file.len());
        e
    }

    /// Map a bit index in `data` to an absolute file bit position.
    pub fn file_bit(&self, bit: u64) -> u64 {
        let byte = (bit / 8) as usize;
        match self.offsets.get(byte) {
            Some(&o) => o as u64 * 8 + bit % 8,
            None => self.end as u64 * 8,
        }
    }
}

/// MSB-first bit reader over unstuffed data. Reading past the end yields zeros.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: u64,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn pos(&self) -> u64 {
        self.pos
    }

    pub fn past_end(&self) -> bool {
        self.pos > self.data.len() as u64 * 8
    }

    /// Peek up to 16 bits without consuming.
    pub fn peek(&self, n: u8) -> u32 {
        let byte = (self.pos / 8) as usize;
        let mut w = 0u32;
        for k in 0..4 {
            w = (w << 8) | *self.data.get(byte + k).unwrap_or(&0) as u32;
        }
        let shift = 32 - (self.pos % 8) as u32 - n as u32;
        (w >> shift) & ((1u32 << n) - 1)
    }

    pub fn skip(&mut self, n: u8) {
        self.pos += n as u64;
    }

    pub fn read(&mut self, n: u8) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.pos += n as u64;
        v
    }

    pub fn bit(&mut self) -> bool {
        self.read(1) == 1
    }

    /// Advance to the next byte boundary.
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    pub fn byte_pos(&self) -> usize {
        (self.pos / 8) as usize
    }

    pub fn seek_byte(&mut self, byte: usize) {
        self.pos = byte as u64 * 8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuffing_roundtrip() {
        let mut w = BitWriter::default();
        w.write(0xFF, 8);
        w.write(0b101, 3);
        w.restart(0);
        w.write(0xAB, 8);
        w.flush();
        assert_eq!(w.out, vec![0xFF, 0x00, 0b1011_1111, 0xFF, 0xD0, 0xAB]);

        let mut file = w.out.clone();
        file.extend_from_slice(&[0xFF, 0xD9]);
        let e = Ecs::extract(&file, 0);
        assert_eq!(e.data, vec![0xFF, 0b1011_1111, 0xAB]);
        assert_eq!(e.restarts, vec![2]);
        assert_eq!(e.end, 6);
        assert_eq!(e.file_bit(8), 2 * 8); // second data byte lives at file offset 2

        let mut r = BitReader::new(&e.data);
        assert_eq!(r.read(8), 0xFF);
        assert_eq!(r.read(3), 0b101);
        r.align();
        assert_eq!(r.read(8), 0xAB);
        assert_eq!(r.read(4), 0); // past end → zeros
    }
}
