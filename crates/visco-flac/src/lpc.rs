//! Linear prediction: windows, autocorrelation, Levinson-Durbin, coefficient quantization.

pub const MAX_ORDER: usize = 32;
pub const MAX_QLP_PRECISION: u32 = 15;
/// Shift is a signed 5-bit field; libFLAC never emits negative shifts.
pub const MAX_SHIFT: i32 = 15;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Window {
    Rectangle,
    Hann,
    Welch,
    /// Tukey with tapered fraction `p`.
    Tukey(f32),
    /// Tukey (taper `p`) over segment `i` of `n`, zero elsewhere.
    PartialTukey { n: u8, i: u8, p: f32 },
    /// Tukey (taper `p`) with segment `i` of `n` zeroed.
    PunchoutTukey { n: u8, i: u8, p: f32 },
}

impl Window {
    pub fn label(&self) -> String {
        match *self {
            Self::Rectangle => "rectangle".into(),
            Self::Hann => "hann".into(),
            Self::Welch => "welch".into(),
            Self::Tukey(p) => format!("tukey({p})"),
            Self::PartialTukey { n, i, .. } => format!("partial_tukey({n}) #{}", i + 1),
            Self::PunchoutTukey { n, i, .. } => format!("punchout_tukey({n}) #{}", i + 1),
        }
    }

    /// libFLAC's `subdivide_tukey(n)`: a full tukey plus partial and punchout windows for every
    /// subdivision up to `n`, with the taper scaled down by the number of parts.
    pub fn subdivide_tukey(n: u8) -> Vec<Window> {
        let p = 0.5 / n as f32;
        let mut v = vec![Window::Tukey(p)];
        for parts in 2..=n {
            for i in 0..parts {
                v.push(Window::PartialTukey { n: parts, i, p });
                v.push(Window::PunchoutTukey { n: parts, i, p });
            }
        }
        v
    }

    pub fn coefficients(&self, len: usize) -> Vec<f64> {
        let n = len as f64;
        let tukey = |start: usize, end: usize, p: f64| -> Vec<f64> {
            let mut w = vec![0.0; len];
            let m = end - start;
            if m == 0 {
                return w;
            }
            let taper = ((p / 2.0) * m as f64) as usize;
            for k in 0..m {
                w[start + k] = if taper > 0 && k < taper {
                    0.5 - 0.5 * (std::f64::consts::PI * k as f64 / taper as f64).cos()
                } else if taper > 0 && k >= m - taper {
                    0.5 - 0.5 * (std::f64::consts::PI * (m - 1 - k) as f64 / taper as f64).cos()
                } else {
                    1.0
                };
            }
            w
        };
        match *self {
            Self::Rectangle => vec![1.0; len],
            Self::Hann => (0..len).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1.0)).cos()).collect(),
            Self::Welch => {
                let h = (n - 1.0) / 2.0;
                (0..len).map(|i| 1.0 - ((i as f64 - h) / (h + 1.0)).powi(2)).collect()
            }
            Self::Tukey(p) => tukey(0, len, p as f64),
            Self::PartialTukey { n: parts, i, p } => {
                let (s, e) = (len * i as usize / parts as usize, len * (i as usize + 1) / parts as usize);
                tukey(s, e, p as f64)
            }
            Self::PunchoutTukey { n: parts, i, p } => {
                let (s, e) = (len * i as usize / parts as usize, len * (i as usize + 1) / parts as usize);
                let mut w = tukey(0, len, p as f64);
                let hole = tukey(s, e, p as f64);
                for k in 0..len {
                    w[k] = (w[k] - hole[k]).max(0.0);
                }
                w
            }
        }
    }
}

pub fn autocorrelation(x: &[f64], max_lag: usize) -> Vec<f64> {
    (0..=max_lag)
        .map(|lag| if lag >= x.len() { 0.0 } else { x[lag..].iter().zip(x).map(|(a, b)| a * b).sum() })
        .collect()
}

/// Levinson-Durbin recursion. Returns, for each order 1..=max, the predictor coefficients
/// (`coefs[j]` multiplies `x[n-1-j]`) and the remaining prediction error.
pub fn levinson(r: &[f64], max_order: usize) -> Vec<(Vec<f64>, f64)> {
    let mut out = Vec::with_capacity(max_order);
    let mut a = vec![0.0f64; max_order];
    let mut err = r[0];
    if err <= 0.0 {
        return out;
    }
    for i in 0..max_order {
        let mut acc = r[i + 1];
        for j in 0..i {
            acc -= a[j] * r[i - j];
        }
        let k = acc / err;
        let prev = a.clone();
        a[i] = k;
        for j in 0..i {
            a[j] = prev[j] - k * prev[i - 1 - j];
        }
        err *= 1.0 - k * k;
        out.push((a[..=i].to_vec(), err.max(0.0)));
        if err <= 0.0 {
            break;
        }
    }
    out
}

/// libFLAC's default QLP precision for a block size and sample depth.
pub fn default_precision(bps: u32, block_size: usize) -> u32 {
    if bps < 16 {
        (2 + bps / 2).max(5)
    } else if bps == 16 {
        [(192, 7), (384, 8), (576, 9), (1152, 10), (2304, 11), (4608, 12)]
            .iter()
            .find(|&&(max, _)| block_size <= max)
            .map_or(13, |&(_, p)| p)
    } else {
        if block_size <= 384 {
            MAX_QLP_PRECISION - 2
        } else if block_size <= 1152 {
            MAX_QLP_PRECISION - 1
        } else {
            MAX_QLP_PRECISION
        }
    }
}

/// Quantize coefficients to `precision` bits (sign included) with error feedback, as libFLAC does.
/// Returns (qlp coefficients, shift), or None if the coefficients are too large to represent.
pub fn quantize(coefs: &[f64], precision: u32) -> Option<(Vec<i32>, i32)> {
    let p = precision - 1; // magnitude bits
    let qmax = (1i32 << p) - 1;
    let qmin = -(1i32 << p);
    let cmax = coefs.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if cmax <= 0.0 {
        return Some((vec![0; coefs.len()], 0));
    }
    let log2cmax = libm_frexp_exp(cmax);
    let mut shift = p as i32 - log2cmax;
    if shift > MAX_SHIFT {
        shift = MAX_SHIFT;
    } else if shift < 0 {
        return None;
    }
    let mut err = 0.0f64;
    let q = coefs
        .iter()
        .map(|&c| {
            err += c * (1i64 << shift) as f64;
            let v = (err.round() as i64).clamp(qmin as i64, qmax as i64) as i32;
            err -= v as f64;
            v
        })
        .collect();
    Some((q, shift))
}

/// Exponent `e` such that `x = m · 2^e` with 0.5 ≤ m < 1 (C's frexp).
fn libm_frexp_exp(x: f64) -> i32 {
    if x == 0.0 {
        return 0;
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7FF) as i32;
    if exp == 0 {
        // subnormal
        return libm_frexp_exp(x * (1u64 << 54) as f64) - 54;
    }
    exp - 1022
}

#[inline]
pub fn predict(x: &[i64], n: usize, qlp: &[i32], shift: i32) -> i64 {
    // Valid streams never overflow i64; wrapping keeps corrupt input from panicking.
    let mut acc: i64 = 0;
    for (j, &c) in qlp.iter().enumerate() {
        acc = acc.wrapping_add((c as i64).wrapping_mul(x[n - 1 - j]));
    }
    acc >> shift
}

pub fn residual(x: &[i64], qlp: &[i32], shift: i32) -> Vec<i64> {
    let order = qlp.len();
    (order..x.len()).map(|n| x[n] - predict(x, n, qlp, shift)).collect()
}

pub fn restore(x: &mut [i64], qlp: &[i32], shift: i32, res: &[i64]) {
    let order = qlp.len();
    for (i, &e) in res.iter().enumerate() {
        let n = order + i;
        x[n] = e.wrapping_add(predict(x, n, qlp, shift));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levinson_ar1() {
        // AR(1) autocorrelation r[k] = a^k gives coefficient a at order 1 and ~0 beyond.
        let a = 0.9f64;
        let r: Vec<f64> = (0..5).map(|k| a.powi(k)).collect();
        let sets = levinson(&r, 3);
        assert!((sets[0].0[0] - a).abs() < 1e-12);
        assert!((sets[0].1 - (1.0 - a * a)).abs() < 1e-12);
        assert!(sets[2].0[1].abs() < 1e-12 && sets[2].0[2].abs() < 1e-12);
    }

    #[test]
    fn lpc_roundtrip() {
        let x: Vec<i64> = (0..1024).map(|i| ((i as f64 * 0.031).sin() * 12000.0 + (i as f64 * 0.17).cos() * 3000.0) as i64).collect();
        let xf: Vec<f64> = x.iter().map(|&v| v as f64).collect();
        let w = Window::Tukey(0.5).coefficients(x.len());
        let windowed: Vec<f64> = xf.iter().zip(&w).map(|(a, b)| a * b).collect();
        let r = autocorrelation(&windowed, 8);
        let sets = levinson(&r, 8);
        let (q, shift) = quantize(&sets[7].0, 12).unwrap();
        let res = residual(&x, &q, shift);
        let mut y = vec![0; x.len()];
        y[..8].copy_from_slice(&x[..8]);
        restore(&mut y, &q, shift, &res);
        assert_eq!(x, y);
        // prediction actually helps
        let energy: i64 = x.iter().map(|v| v.abs()).sum();
        let res_energy: i64 = res.iter().map(|v| v.abs()).sum();
        assert!(res_energy * 20 < energy, "{res_energy} vs {energy}");
    }

    #[test]
    fn frexp() {
        assert_eq!(libm_frexp_exp(1.0), 1);
        assert_eq!(libm_frexp_exp(0.75), 0);
        assert_eq!(libm_frexp_exp(1.9), 1);
        assert_eq!(libm_frexp_exp(2.0), 2);
    }

    #[test]
    fn windows_are_sane() {
        for w in Window::subdivide_tukey(3).into_iter().chain([Window::Hann, Window::Welch, Window::Rectangle]) {
            let c = w.coefficients(4096);
            assert_eq!(c.len(), 4096);
            assert!(c.iter().all(|&v| (0.0..=1.0).contains(&v)), "{w:?}");
            assert!(c.iter().any(|&v| v > 0.99), "{w:?}");
        }
    }
}
