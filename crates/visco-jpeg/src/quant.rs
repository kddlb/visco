//! Quantization tables (Annex K.1) and IJG quality scaling.

/// Annex K Table K.1, natural order.
pub const STD_LUMA: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
    56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104,
    113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Annex K Table K.2, natural order.
pub const STD_CHROMA: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

/// IJG (libjpeg `jpeg_quality_scaling` + `jpeg_add_quant_table`) scaling, baseline-clamped to 1..=255.
pub fn scaled(base: &[u16; 64], quality: u8) -> [u16; 64] {
    let q = quality.clamp(1, 100) as u32;
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    std::array::from_fn(|i| ((base[i] as u32 * scale + 50) / 100).clamp(1, 255) as u16)
}

pub fn quantize(coefs: &[f32; 64], table: &[u16; 64]) -> [i16; 64] {
    std::array::from_fn(|i| (coefs[i] / table[i] as f32).round() as i16)
}

pub fn dequantize(q: &[i16; 64], table: &[u16; 64]) -> [f32; 64] {
    std::array::from_fn(|i| q[i] as f32 * table[i] as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ijg_scaling() {
        assert_eq!(scaled(&STD_LUMA, 50), STD_LUMA);
        assert!(scaled(&STD_LUMA, 100).iter().all(|&v| v == 1));
        // q=75 → scale 50: 16 → 8, 11 → 6 (5.5+0.5 rounds), 99 → 50
        let t = scaled(&STD_LUMA, 75);
        assert_eq!((t[0], t[1], t[63]), (8, 6, 50));
        // q=1 → scale 5000, everything clamps to 255
        assert!(scaled(&STD_LUMA, 1).iter().all(|&v| v == 255));
    }
}
