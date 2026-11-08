//! Chroma subsampling and upsampling.

/// A single 8-bit plane.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane8 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl Plane8 {
    pub fn new(width: usize, height: usize, data: Vec<u8>) -> Self {
        assert_eq!(data.len(), width * height);
        Self { width, height, data }
    }

    /// Sample with edge replication (JPEG pads partial MCUs this way).
    #[inline]
    pub fn at_clamped(&self, x: usize, y: usize) -> u8 {
        self.data[y.min(self.height - 1) * self.width + x.min(self.width - 1)]
    }
}

/// Encoder-side subsampling choices (luma sampling factors; chroma is always 1×1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subsampling {
    S444,
    S422,
    S420,
}

impl Subsampling {
    pub const ALL: [Subsampling; 3] = [Self::S444, Self::S422, Self::S420];

    pub fn luma_factors(self) -> (u8, u8) {
        match self {
            Self::S444 => (1, 1),
            Self::S422 => (2, 1),
            Self::S420 => (2, 2),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::S444 => "4:4:4",
            Self::S422 => "4:2:2",
            Self::S420 => "4:2:0",
        }
    }
}

/// Box-average downsample by integer factors; output size is ceil(w/fx) × ceil(h/fy).
pub fn downsample(p: &Plane8, fx: usize, fy: usize) -> Plane8 {
    if fx == 1 && fy == 1 {
        return p.clone();
    }
    let (w, h) = (p.width.div_ceil(fx), p.height.div_ceil(fy));
    let mut data = Vec::with_capacity(w * h);
    let n = (fx * fy) as u32;
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0u32;
            for dy in 0..fy {
                for dx in 0..fx {
                    sum += p.at_clamped(x * fx + dx, y * fy + dy) as u32;
                }
            }
            data.push(((sum + n / 2) / n) as u8);
        }
    }
    Plane8::new(w, h, data)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upsample {
    Nearest,
    /// Triangle filter (libjpeg "fancy upsampling") for 2× axes; nearest otherwise.
    Fancy,
}

/// Upsample by integer factors into exactly `out_w × out_h`.
pub fn upsample(p: &Plane8, fx: usize, fy: usize, out_w: usize, out_h: usize, mode: Upsample) -> Plane8 {
    if mode == Upsample::Fancy && (fx == 2 || fy == 2) && fx <= 2 && fy <= 2 {
        return fancy(p, fx, fy, out_w, out_h);
    }
    let mut data = Vec::with_capacity(out_w * out_h);
    for y in 0..out_h {
        for x in 0..out_w {
            data.push(p.at_clamped(x / fx, y / fy));
        }
    }
    Plane8::new(out_w, out_h, data)
}

fn fancy(p: &Plane8, fx: usize, fy: usize, out_w: usize, out_h: usize) -> Plane8 {
    // Each output sample sits at source coordinate (x + 0.5)/f - 0.5; for f=2 this gives
    // weights 3/4 on the nearer and 1/4 on the farther neighbor (separable in 2D).
    let taps = |o: usize, f: usize, n: usize| -> [(usize, f32); 2] {
        if f == 1 {
            return [(o.min(n - 1), 1.0), (0, 0.0)];
        }
        let i = o / 2;
        let j = if o.is_multiple_of(2) { i.saturating_sub(1) } else { (i + 1).min(n - 1) };
        [(i.min(n - 1), 0.75), (j, 0.25)]
    };
    let mut data = Vec::with_capacity(out_w * out_h);
    for y in 0..out_h {
        let ty = taps(y, fy, p.height);
        for x in 0..out_w {
            let tx = taps(x, fx, p.width);
            let mut v = 0.0;
            for &(sy, wy) in &ty {
                for &(sx, wx) in &tx {
                    if wx * wy > 0.0 {
                        v += wx * wy * p.data[sy * p.width + sx] as f32;
                    }
                }
            }
            data.push(v.round().clamp(0.0, 255.0) as u8);
        }
    }
    Plane8::new(out_w, out_h, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_averages() {
        let p = Plane8::new(3, 2, vec![0, 10, 20, 30, 40, 50]);
        let d = downsample(&p, 2, 2);
        assert_eq!((d.width, d.height), (2, 1));
        assert_eq!(d.data, vec![20, 35]); // (0+10+30+40)/4, (20+20+50+50)/4
    }

    #[test]
    fn upsample_flat_is_flat() {
        let p = Plane8::new(4, 4, vec![77; 16]);
        for mode in [Upsample::Nearest, Upsample::Fancy] {
            let u = upsample(&p, 2, 2, 7, 8, mode);
            assert_eq!((u.width, u.height), (7, 8));
            assert!(u.data.iter().all(|&v| v == 77));
        }
    }
}
