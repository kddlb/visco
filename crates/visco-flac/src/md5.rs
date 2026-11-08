//! MD5 (RFC 1321), for STREAMINFO's audio signature.

pub struct Md5 {
    state: [u32; 4],
    buf: [u8; 64],
    buf_len: usize,
    len: u64,
}

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14,
    20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6,
    10, 15, 21,
];

fn k_table() -> [u32; 64] {
    std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
}

impl Default for Md5 {
    fn default() -> Self {
        Self { state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476], buf: [0; 64], buf_len: 0, len: 0 }
    }
}

impl Md5 {
    pub fn update(&mut self, mut data: &[u8]) {
        self.len += data.len() as u64;
        if self.buf_len > 0 {
            let n = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len == 64 {
                let b = self.buf;
                self.block(&b);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            self.block(data[..64].try_into().unwrap());
            data = &data[64..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.buf_len += data.len();
    }

    pub fn finish(mut self) -> [u8; 16] {
        let bits = self.len.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_le_bytes());
        let mut out = [0u8; 16];
        for (i, w) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    fn block(&mut self, b: &[u8; 64]) {
        let k = k_table();
        let m: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes(b[4 * i..4 * i + 4].try_into().unwrap()));
        let [mut a, mut bb, mut c, mut d] = self.state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((bb & c) | (!bb & d), i),
                1 => ((d & bb) | (!d & c), (5 * i + 1) % 16),
                2 => (bb ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (bb | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = bb;
            bb = bb.wrapping_add(f.rotate_left(S[i]));
        }
        for (s, v) in self.state.iter_mut().zip([a, bb, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
}

/// FLAC's audio MD5: interleaved samples, little-endian, ceil(bps/8) bytes each.
pub fn update_samples(md5: &mut Md5, interleaved: &[i32], bits_per_sample: u8) {
    let nb = (bits_per_sample as usize).div_ceil(8);
    let mut buf = Vec::with_capacity(interleaved.len() * nb);
    for &s in interleaved {
        buf.extend_from_slice(&s.to_le_bytes()[..nb]);
    }
    md5.update(&buf);
}

pub fn hex(d: &[u8; 16]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md5(s: &[u8]) -> String {
        let mut m = Md5::default();
        m.update(s);
        hex(&m.finish())
    }

    #[test]
    fn rfc1321_vectors() {
        assert_eq!(md5(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5(b"message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(
            md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
        // streaming in odd chunks gives the same digest
        let mut m = Md5::default();
        for chunk in b"12345678901234567890123456789012345678901234567890123456789012345678901234567890".chunks(7) {
            m.update(chunk);
        }
        assert_eq!(hex(&m.finish()), "57edf4a22be3c955ac49da2e2107b67a");
    }
}
