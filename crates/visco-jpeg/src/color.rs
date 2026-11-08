//! JFIF RGB ↔ YCbCr (BT.601, full range).

pub fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32, g as f32, b as f32);
    (
        0.299 * r + 0.587 * g + 0.114 * b,
        -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0,
        0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0,
    )
}

pub fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (u8, u8, u8) {
    let (cb, cr) = (cb - 128.0, cr - 128.0);
    (
        clamp_u8(y + 1.402 * cr),
        clamp_u8(y - 0.344_136 * cb - 0.714_136 * cr),
        clamp_u8(y + 1.772 * cb),
    )
}

pub fn clamp_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Split interleaved RGB8 into Y, Cb, Cr planes (rounded to u8, as an encoder stores them).
pub fn split_ycbcr(rgb: &[u8]) -> [Vec<u8>; 3] {
    let n = rgb.len() / 3;
    let mut out = [Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n)];
    for px in rgb.as_chunks::<3>().0 {
        let (y, cb, cr) = rgb_to_ycbcr(px[0], px[1], px[2]);
        out[0].push(clamp_u8(y));
        out[1].push(clamp_u8(cb));
        out[2].push(clamp_u8(cr));
    }
    out
}

pub fn merge_ycbcr(y: &[u8], cb: &[u8], cr: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(y.len() * 3);
    for i in 0..y.len() {
        let (r, g, b) = ycbcr_to_rgb(y[i] as f32, cb[i] as f32, cr[i] as f32);
        out.extend_from_slice(&[r, g, b]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_within_one() {
        for &(r, g, b) in &[(0, 0, 0), (255, 255, 255), (255, 0, 0), (12, 200, 99), (90, 30, 240)] {
            let (y, cb, cr) = rgb_to_ycbcr(r, g, b);
            let (r2, g2, b2) = ycbcr_to_rgb(y, cb, cr);
            assert!((r as i32 - r2 as i32).abs() <= 1);
            assert!((g as i32 - g2 as i32).abs() <= 1);
            assert!((b as i32 - b2 as i32).abs() <= 1);
        }
    }
}
