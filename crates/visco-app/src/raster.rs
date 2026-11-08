//! Plane → GPU image conversion and value → color ramps.

use std::sync::Arc;

use gpui_kit::{Hsla, RenderImage, Rgba, rgb};
use image::{Frame, ImageBuffer};
use visco_core::{Plane, PlaneKind, Ramp};

/// Max pixels in one pre-scaled image (≈32 MB of BGRA).
pub const SCALE_BUDGET: usize = 8_000_000;

/// Largest integer pre-scale ≤ `want` that fits the budget (at least 1).
pub fn prescale_factor(p: &Plane, want: usize) -> usize {
    let mut k = want.clamp(1, 8);
    while k > 1 && p.width * p.height * k * k > SCALE_BUDGET {
        k -= 1;
    }
    k
}

/// Map a scalar to RGB8 on a ramp; `t` is pre-normalized per ramp.
pub fn ramp_rgb(ramp: Ramp, v: f32, range: (f32, f32)) -> [u8; 3] {
    let (lo, hi) = range;
    match ramp {
        Ramp::Gray => {
            let t = ((v - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
            let g = (t * 255.0) as u8;
            [g, g, g]
        }
        Ramp::Diverging => {
            let m = lo.abs().max(hi.abs()).max(1e-6);
            let t = (v / m).clamp(-1.0, 1.0);
            let a = t.abs().powf(0.8);
            if t >= 0.0 {
                [(a * 255.0) as u8, (a * 90.0) as u8, (a * 40.0) as u8]
            } else {
                [(a * 40.0) as u8, (a * 130.0) as u8, (a * 255.0) as u8]
            }
        }
        Ramp::Heat => {
            let t = ((v - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
            // black → red → yellow → white
            let r = (t * 3.0).min(1.0);
            let g = (t * 3.0 - 1.0).clamp(0.0, 1.0);
            let b = (t * 3.0 - 2.0).clamp(0.0, 1.0);
            [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
        }
    }
}

/// RGB8 of one source pixel.
#[inline]
pub fn pixel(p: &Plane, x: usize, y: usize) -> [u8; 3] {
    let i = y * p.width + x;
    match &p.kind {
        PlaneKind::Gray(d) => [d[i]; 3],
        PlaneKind::Rgb(d) => [d[3 * i], d[3 * i + 1], d[3 * i + 2]],
        PlaneKind::Scalar { data, range, ramp } => ramp_rgb(*ramp, data[i], *range),
    }
}

pub fn pixel_hsla(p: &Plane, x: usize, y: usize) -> Hsla {
    let [r, g, b] = pixel(p, x, y);
    rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into()
}

/// Build a BGRA image of the plane, nearest-neighbor scaled by integer `k`.
pub fn render_image(p: &Plane, k: usize) -> Arc<RenderImage> {
    let (w, h) = (p.width * k, p.height * k);
    let mut buf = vec![0u8; w * h * 4];
    let mut row = vec![0u8; w * 4];
    for y in 0..p.height {
        for x in 0..p.width {
            let [r, g, b] = pixel(p, x, y);
            for dx in 0..k {
                let o = (x * k + dx) * 4;
                row[o..o + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        for dy in 0..k {
            let o = (y * k + dy) * w * 4;
            buf[o..o + w * 4].copy_from_slice(&row);
        }
    }
    let img = ImageBuffer::from_raw(w as u32, h as u32, buf).expect("buffer size");
    Arc::new(RenderImage::new(vec![Frame::new(img)]))
}

/// Text color that stays readable on a ramp background.
pub fn contrast_text(bg: [u8; 3]) -> Rgba {
    let lum = 0.299 * bg[0] as f32 + 0.587 * bg[1] as f32 + 0.114 * bg[2] as f32;
    if lum > 140.0 { rgb(0x111111) } else { rgb(0xf5f5f5) }
}
