mod common;
use common::*;
use visco_jpeg::{
    encoder::{EncodeParams, encode},
    sampling::Subsampling,
};

#[test]
fn reference_decoder_accepts_our_output() {
    let (w, h) = (77, 45); // odd sizes exercise MCU padding
    let rgb = synth(w, h);
    // Thresholds sit just under libjpeg's cjpeg on the same image (see compare_cjpeg.rs);
    // the synthetic image has strong chroma noise, so subsampling costs several dB.
    for (sub, floors) in [
        (Subsampling::S444, [36.0, 29.5, 26.0, 21.5]),
        (Subsampling::S422, [28.0, 26.5, 24.5, 21.0]),
        (Subsampling::S420, [27.0, 26.0, 23.5, 20.0]),
    ] {
        for (quality, min_psnr) in [95, 75, 30, 5].into_iter().zip(floors) {
            for ri in [0, 3] {
                let p = EncodeParams { quality, subsampling: sub, restart_interval: ri, ..Default::default() };
                let e = encode(&rgb, w, h, &p, true);
                let (rw, rh, out) = reference_decode(&e.bytes);
                assert_eq!((rw, rh), (w, h));
                let q = psnr(&rgb, &out);
                assert!(q > min_psnr, "{sub:?} q{quality} ri{ri}: PSNR {q:.2} < {min_psnr}");
            }
        }
    }
}

#[test]
fn grayscale() {
    let (w, h) = (40, 23);
    let rgb = synth(w, h);
    let p = EncodeParams { grayscale: true, quality: 90, ..Default::default() };
    let e = encode(&rgb, w, h, &p, false);
    let img = image::load_from_memory(&e.bytes).unwrap();
    assert_eq!(img.color(), image::ColorType::L8);
    let luma = img.to_luma8().into_raw();
    let expected: Vec<u8> = e.full_planes[0].data.clone();
    assert!(psnr(&expected, &luma) > 35.0);
}

#[test]
fn spans_are_sorted_contiguous_and_cover_blocks() {
    let (w, h) = (32, 16);
    let rgb = synth(w, h);
    let e = encode(&rgb, w, h, &EncodeParams::default(), true);
    assert!(!e.spans.is_empty());
    for pair in e.spans.windows(2) {
        assert!(pair[0].end() <= pair[1].start);
    }
    let total_blocks: usize = e.comps.iter().map(|c| c.coefs.len()).sum();
    let mut seen = vec![false; total_blocks];
    for s in &e.spans {
        seen[s.block as usize] = true;
    }
    assert!(seen.iter().all(|&b| b));
    // spans live inside the entropy-coded segment
    let ecs = e.segments.iter().find(|s| s.name == "Entropy-coded data").unwrap();
    assert!(e.spans[0].start >= ecs.bytes.start as u64 * 8);
    assert!(e.spans.last().unwrap().end() <= ecs.bytes.end as u64 * 8);
}
