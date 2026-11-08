#![allow(dead_code)]

/// Deterministic test image: gradients, a hard edge, and some texture.
pub fn synth(w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3);
    let mut seed = 0x1234_5678u32;
    for y in 0..h {
        for x in 0..w {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 27) as i32 - 16;
            let edge = if x > w / 2 { 60 } else { 0 };
            let r = (x * 255 / w.max(1)) as i32 + noise;
            let g = (y * 255 / h.max(1)) as i32 + edge;
            let b = (((x + y) * 4) % 256) as i32 - noise;
            out.extend([r, g, b].map(|v| v.clamp(0, 255) as u8));
        }
    }
    out
}

pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

pub fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y)).max().unwrap_or(0)
}

/// Reference decode via the `image` crate (zune-jpeg) → (w, h, RGB8).
pub fn reference_decode(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .expect("reference decoder rejected file")
        .to_rgb8();
    (img.width() as usize, img.height() as usize, img.into_raw())
}

/// Read a binary PGM/PPM (as written by djpeg) → (w, h, RGB8).
pub fn read_pnm(bytes: &[u8]) -> (usize, usize, Vec<u8>) {
    let mut fields = vec![];
    let mut i = 0;
    while fields.len() < 4 {
        while bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes[i] == b'#' {
            while bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let s = i;
        while !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&bytes[s..i]).unwrap().to_string());
    }
    let data = &bytes[i + 1..];
    let (w, h): (usize, usize) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    let rgb = match fields[0].as_str() {
        "P6" => data[..w * h * 3].to_vec(),
        "P5" => data[..w * h].iter().flat_map(|&v| [v, v, v]).collect(),
        m => panic!("unsupported PNM {m}"),
    };
    (w, h, rgb)
}
