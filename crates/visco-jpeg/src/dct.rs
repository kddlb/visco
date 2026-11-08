//! Separable floating-point reference DCT-II / DCT-III on 8×8 blocks (JPEG Annex A.3.3).

use std::sync::OnceLock;

/// `M[u][x] = c(u)/2 · cos((2x+1)uπ/16)`, with `c(0) = 1/√2`, else 1.
fn basis() -> &'static [[f32; 8]; 8] {
    static M: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    M.get_or_init(|| {
        let mut m = [[0f32; 8]; 8];
        for (u, row) in m.iter_mut().enumerate() {
            let c = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
            for (x, v) in row.iter_mut().enumerate() {
                let a = ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI) / 16.0;
                *v = (c / 2.0 * a.cos()) as f32;
            }
        }
        m
    })
}

/// Forward DCT of a level-shifted block (row-major, values roughly in -128..127).
pub fn fdct(input: &[f32; 64]) -> [f32; 64] {
    let m = basis();
    let mut tmp = [0f32; 64];
    // rows: tmp[y][u] = Σx M[u][x] in[y][x]
    for y in 0..8 {
        for u in 0..8 {
            tmp[y * 8 + u] = (0..8).map(|x| m[u][x] * input[y * 8 + x]).sum();
        }
    }
    let mut out = [0f32; 64];
    // cols: out[v][u] = Σy M[v][y] tmp[y][u]
    for v in 0..8 {
        for u in 0..8 {
            out[v * 8 + u] = (0..8).map(|y| m[v][y] * tmp[y * 8 + u]).sum();
        }
    }
    out
}

/// Inverse DCT; returns level-shifted samples (add 128 and clamp to get pixels).
pub fn idct(input: &[f32; 64]) -> [f32; 64] {
    let m = basis();
    let mut tmp = [0f32; 64];
    // cols: tmp[y][u] = Σv M[v][y] in[v][u]
    for y in 0..8 {
        for u in 0..8 {
            tmp[y * 8 + u] = (0..8).map(|v| m[v][y] * input[v * 8 + u]).sum();
        }
    }
    let mut out = [0f32; 64];
    for y in 0..8 {
        for x in 0..8 {
            out[y * 8 + x] = (0..8).map(|u| m[u][x] * tmp[y * 8 + u]).sum();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let block: [f32; 64] = std::array::from_fn(|i| ((i * 37 % 256) as f32) - 128.0);
        let back = idct(&fdct(&block));
        for (a, b) in block.iter().zip(back.iter()) {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn flat_block_is_dc_only() {
        let block = [10f32; 64];
        let c = fdct(&block);
        assert!((c[0] - 80.0).abs() < 1e-3); // DC = 8 · mean
        assert!(c[1..].iter().all(|v| v.abs() < 1e-3));
    }
}
