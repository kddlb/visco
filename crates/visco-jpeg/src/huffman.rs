//! Huffman tables (T.81 Annex C, F.2.2.3) and the Annex K standard tables.

use crate::bitio::{BitReader, BitWriter};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HuffSpec {
    /// `bits[i]` = number of codes of length i+1.
    pub bits: [u8; 16],
    pub vals: Vec<u8>,
}

/// Code assignment for one table: (code, length) per symbol, plus decoding tables.
#[derive(Clone, Debug)]
pub struct HuffTable {
    pub spec: HuffSpec,
    /// Encoder: `ehuf[sym] = (code, len)`; len 0 = symbol absent.
    pub ehuf: [(u16, u8); 256],
    /// Decoder: 9-bit lookahead → (len, sym); len 0 = slow path.
    look: Vec<(u8, u8)>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
}

const LOOKAHEAD: u8 = 9;

impl HuffTable {
    pub fn new(spec: HuffSpec) -> Result<Self, String> {
        let total: usize = spec.bits.iter().map(|&b| b as usize).sum();
        if total != spec.vals.len() || total > 256 {
            return Err(format!("Huffman table: {total} codes but {} values", spec.vals.len()));
        }
        // C.2: generate code sizes and codes
        let mut ehuf = [(0u16, 0u8); 256];
        let mut look = vec![(0u8, 0u8); 1 << LOOKAHEAD];
        let mut maxcode = [-1i32; 18];
        let mut valptr = [0i32; 17];
        let mut mincode = [0i32; 17];
        let mut code: u32 = 0;
        let mut k = 0usize;
        for len in 1..=16u8 {
            let n = spec.bits[len as usize - 1] as usize;
            if n > 0 {
                valptr[len as usize] = k as i32;
                mincode[len as usize] = code as i32;
            }
            for _ in 0..n {
                if code >= (1 << len) {
                    return Err("Huffman table: code space overflow".into());
                }
                let sym = spec.vals[k];
                ehuf[sym as usize] = (code as u16, len);
                if len <= LOOKAHEAD {
                    let shift = LOOKAHEAD - len;
                    let base = (code << shift) as usize;
                    for e in &mut look[base..base + (1 << shift)] {
                        *e = (len, sym);
                    }
                }
                code += 1;
                k += 1;
            }
            if n > 0 {
                maxcode[len as usize] = code as i32 - 1;
            }
            code <<= 1;
        }
        maxcode[17] = i32::MAX; // sentinel
        Ok(Self { spec, ehuf, look, maxcode, valptr, mincode })
    }

    pub fn encode(&self, w: &mut BitWriter, sym: u8) {
        let (code, len) = self.ehuf[sym as usize];
        debug_assert!(len > 0, "symbol {sym:#x} not in table");
        w.write(code as u32, len);
    }

    /// Decode one symbol, returning (symbol, code length).
    pub fn decode(&self, r: &mut BitReader) -> Result<(u8, u8), String> {
        let (len, sym) = self.look[r.peek(LOOKAHEAD) as usize];
        if len > 0 {
            r.skip(len);
            return Ok((sym, len));
        }
        // F.2.2.3 slow path
        let mut code = r.read(LOOKAHEAD) as i32;
        let mut len = LOOKAHEAD as usize;
        loop {
            if code <= self.maxcode[len] {
                let idx = self.valptr[len] + code - self.mincode[len];
                return Ok((self.spec.vals[idx as usize], len as u8));
            }
            if len >= 16 {
                return Err("invalid Huffman code".into());
            }
            code = (code << 1) | r.read(1) as i32;
            len += 1;
        }
    }
}

/// Magnitude category (SSSS) of a coefficient.
pub fn category(v: i32) -> u8 {
    (32 - v.unsigned_abs().leading_zeros()) as u8
}

/// The `cat` extra bits encoding `v` (one's-complement for negatives).
pub fn extra_bits(v: i32, cat: u8) -> u32 {
    let v = if v < 0 { v - 1 } else { v };
    (v as u32) & ((1u32 << cat) - 1)
}

/// F.2.2.1 EXTEND: decode `cat` extra bits back to a signed value.
pub fn extend(bits: u32, cat: u8) -> i32 {
    if cat == 0 {
        return 0;
    }
    let v = bits as i32;
    if v < (1 << (cat - 1)) { v - (1 << cat) + 1 } else { v }
}

// Annex K.3 standard tables.
pub fn std_dc_luma() -> HuffSpec {
    HuffSpec { bits: [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], vals: (0..12).collect() }
}

pub fn std_dc_chroma() -> HuffSpec {
    HuffSpec { bits: [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], vals: (0..12).collect() }
}

pub fn std_ac_luma() -> HuffSpec {
    HuffSpec {
        bits: [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
        vals: vec![
            0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
            0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
            0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
            0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
            0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
            0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
            0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
            0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
            0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
            0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
            0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ],
    }
}

pub fn std_ac_chroma() -> HuffSpec {
    HuffSpec {
        bits: [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
        vals: vec![
            0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
            0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
            0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
            0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
            0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
            0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
            0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
            0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
            0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
            0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
            0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_tables_valid() {
        for s in [std_dc_luma(), std_dc_chroma(), std_ac_luma(), std_ac_chroma()] {
            HuffTable::new(s).unwrap();
        }
    }

    #[test]
    fn encode_decode_identity() {
        let t = HuffTable::new(std_ac_luma()).unwrap();
        let syms: Vec<u8> = t.spec.vals.clone();
        let mut w = BitWriter::default();
        for &s in &syms {
            t.encode(&mut w, s);
        }
        w.flush();
        let e = crate::bitio::Ecs::extract(&w.out, 0);
        let mut r = BitReader::new(&e.data);
        for &s in &syms {
            assert_eq!(t.decode(&mut r).unwrap().0, s);
        }
    }

    #[test]
    fn categories_and_extend() {
        assert_eq!(category(0), 0);
        assert_eq!(category(1), 1);
        assert_eq!(category(-1), 1);
        assert_eq!(category(255), 8);
        assert_eq!(category(-1024), 11);
        for v in [-1023, -100, -3, -1, 1, 2, 7, 500, 2047] {
            let c = category(v);
            assert_eq!(extend(extra_bits(v, c), c), v, "v={v}");
        }
        // DC luma: category 0 code is "00"
        let t = HuffTable::new(std_dc_luma()).unwrap();
        assert_eq!(t.ehuf[0], (0b00, 2));
        assert_eq!(t.ehuf[11], (0b1_1111_1110, 9));
    }
}
