mod common;
use common::*;
use std::path::PathBuf;
use visco_jpeg::{
    decoder::{DecodeOptions, decode},
    encoder::{EncodeParams, encode},
    markers::FrameKind,
    sampling::Subsampling,
};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/jpeg")
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = fixture_dir();
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "jpg").then(|| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        })
        .collect();
    v.sort();
    assert!(v.len() >= 20, "fixtures missing; run fixtures/jpeg/make.sh");
    v
}

#[test]
fn matches_reference_on_all_fixtures() {
    let mut report = vec![];
    for (name, bytes) in fixtures() {
        let d = decode(&bytes, DecodeOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(d.warnings.is_empty(), "{name}: {:?}", d.warnings);
        let ref_path = fixture_dir().join(name.replace(".jpg", ".ref.pnm"));
        let (w, h, reference) = read_pnm(&std::fs::read(ref_path).unwrap());
        assert_eq!((d.frame.width as usize, d.frame.height as usize), (w, h), "{name}");
        let q = psnr(&reference, &d.rgb);
        let m = max_abs_diff(&reference, &d.rgb);
        report.push(format!("{name}: PSNR vs libjpeg {q:.1} dB, max diff {m}"));
        // Float vs integer IDCT and rounding in upsampling differ slightly from libjpeg.
        assert!(q > 50.0 && m <= 4, "{name}: PSNR {q:.2}, max diff {m}");
    }
    println!("{}", report.join("\n"));
}

#[test]
fn progressive_records_scans_and_snapshots() {
    for (name, bytes) in fixtures().into_iter().filter(|(n, _)| n.contains("prog")) {
        let d = decode(&bytes, DecodeOptions::default()).unwrap();
        assert_eq!(d.frame.kind, FrameKind::Progressive, "{name}");
        assert!(d.scans.len() > 1, "{name}");
        assert!(d.scans.iter().all(|s| s.snapshot.is_some()), "{name}");
        // the last snapshot is the final image
        assert_eq!(d.scans.last().unwrap().snapshot.as_ref().unwrap(), &d.rgb, "{name}");
        // later scans should get closer to the final image
        let first = psnr(&d.rgb, d.scans[0].snapshot.as_ref().unwrap());
        let mid = psnr(&d.rgb, d.scans[d.scans.len() / 2].snapshot.as_ref().unwrap());
        assert!(mid >= first, "{name}: {first:.1} → {mid:.1}");
    }
}

#[test]
fn decodes_own_encoder_exactly() {
    // Our decoder must reproduce our encoder's quantized coefficients bit for bit.
    let (w, h) = (61, 37);
    let rgb = synth(w, h);
    for sub in Subsampling::ALL {
        for ri in [0, 1, 5] {
            let p = EncodeParams { quality: 80, subsampling: sub, restart_interval: ri, ..Default::default() };
            let e = encode(&rgb, w, h, &p, true);
            let d = decode(&e.bytes, DecodeOptions::default()).unwrap();
            assert!(d.warnings.is_empty());
            for (ec, dc) in e.comps.iter().zip(&d.comps) {
                assert_eq!(ec.coefs, dc.coefs, "{sub:?} ri{ri}");
            }
            if let Some(i) = (0..e.spans.len().max(d.spans.len())).find(|&i| e.spans.get(i) != d.spans.get(i)) {
                panic!("{sub:?} ri{ri}: span {i} differs: enc {:?} dec {:?}", e.spans.get(i), d.spans.get(i));
            }
        }
    }
}

#[test]
fn spans_sorted_and_in_bounds() {
    for (name, bytes) in fixtures() {
        let d = decode(&bytes, DecodeOptions::default()).unwrap();
        for pair in d.spans.windows(2) {
            assert!(pair[0].end() <= pair[1].start, "{name}: overlapping spans");
        }
        assert!(d.spans.last().unwrap().end() <= bytes.len() as u64 * 8, "{name}");
    }
}

#[test]
fn rejects_garbage_cleanly() {
    assert!(decode(b"not a jpeg", DecodeOptions::default()).is_err());
    let (_, bytes) = fixtures().into_iter().find(|(n, _)| n == "large_base_420.jpg").unwrap();
    // truncated file: decodes partially with a warning instead of panicking
    for cut in [bytes.len() / 2, bytes.len() - 3, 700] {
        if let Ok(d) = decode(&bytes[..cut], DecodeOptions::default()) {
            assert!(!d.warnings.is_empty());
        }
    }
    // corrupted entropy data must not panic
    let mut bad = bytes.clone();
    for i in (bytes.len() / 3..bytes.len() - 2).step_by(97) {
        bad[i] ^= 0x5A;
    }
    let _ = decode(&bad, DecodeOptions::default());
}
