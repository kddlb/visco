mod common;
use common::*;
use visco_jpeg::{encoder::{EncodeParams, encode}, sampling::Subsampling};

#[test]
#[ignore]
fn compare_with_cjpeg() {
    let (w, h) = (77, 45);
    let rgb = synth(w, h);
    let dir = std::env::var("SCRATCH").unwrap();
    let ppm = format!("{dir}/synth.ppm");
    let mut f = format!("P6\n{w} {h}\n255\n").into_bytes();
    f.extend_from_slice(&rgb);
    std::fs::write(&ppm, f).unwrap();
    for (sub, flag) in [(Subsampling::S444, "1x1"), (Subsampling::S422, "2x1"), (Subsampling::S420, "2x2")] {
        for q in [95, 75, 30, 5] {
            let ours = encode(&rgb, w, h, &EncodeParams { quality: q, subsampling: sub, ..Default::default() }, false);
            let out = std::process::Command::new("cjpeg")
                .args(["-quality", &q.to_string(), "-sample", flag, "-baseline", &ppm])
                .output().unwrap().stdout;
            let (_, _, a) = reference_decode(&ours.bytes);
            let (_, _, b) = reference_decode(&out);
            println!("{flag} q{q}: ours {:.2} dB {} B | cjpeg {:.2} dB {} B", psnr(&rgb, &a), ours.bytes.len(), psnr(&rgb, &b), out.len());
        }
    }
}
