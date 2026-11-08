//! CRC-8 (poly 0x07) and CRC-16 (poly 0x8005), both MSB-first with zero init, as FLAC uses.

const fn table8() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const fn table16() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static T8: [u8; 256] = table8();
static T16: [u16; 256] = table16();

pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| T8[(c ^ b) as usize])
}

pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |c, &b| (c << 8) ^ T16[((c >> 8) as u8 ^ b) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_values() {
        // Standard "123456789" check values: CRC-8/SMBUS = 0xF4, CRC-16/UMTS (BUYPASS) = 0xFEE8.
        assert_eq!(crc8(b"123456789"), 0xF4);
        assert_eq!(crc16(b"123456789"), 0xFEE8);
    }
}
