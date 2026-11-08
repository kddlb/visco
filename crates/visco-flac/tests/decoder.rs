mod common;
use common::*;
use visco_flac::decoder::{DecodeOptions, decode};

#[test]
fn bit_exact_on_all_fixtures() {
    let fx = fixtures("flac");
    assert!(fx.len() >= 25, "fixtures missing; run fixtures/flac/make.sh");
    for (name, bytes) in fx {
        let d = decode(&bytes, DecodeOptions::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(d.warnings.is_empty(), "{name}: {:?}", d.warnings);
        assert_eq!(d.md5_ok(), Some(true), "{name}: MD5");
        let ours = d.interleaved();
        assert_eq!(ours, claxon_decode(&bytes), "{name}: differs from claxon");
        let (rate, bits, ch, src) = read_wav(&std::fs::read(fixture_dir().join(source_for(&name))).unwrap());
        assert_eq!((d.info.sample_rate, d.info.bits_per_sample, d.info.channels), (rate, bits, ch), "{name}");
        assert_eq!(ours, src, "{name}: differs from source WAV");
        assert!(d.frames.iter().all(|f| f.crc_ok));
    }
}

#[test]
fn spans_and_records_are_consistent() {
    for (name, bytes) in fixtures("flac") {
        let d = decode(&bytes, DecodeOptions::default()).unwrap();
        for w in d.spans.windows(2) {
            assert!(w[0].end() <= w[1].start, "{name}: overlapping spans {:?} {:?}", w[0], w[1]);
        }
        let nch = d.info.channels as usize;
        assert_eq!(d.subframes.len(), d.frames.len() * nch, "{name}");
        for (i, s) in d.subframes.iter().enumerate() {
            assert_eq!((s.frame as usize, s.channel as usize), (i / nch, i % nch));
        }
        // Every subframe's spans tile its bit range exactly.
        let mut covered = vec![0u64; d.subframes.len()];
        for s in &d.spans {
            if let Some(b) = s.block_id() {
                covered[b] += s.len as u64;
            }
        }
        for (i, s) in d.subframes.iter().enumerate() {
            assert_eq!(covered[i], s.bits.end - s.bits.start, "{name}: subframe {i}");
        }
        for ch in 0..nch {
            assert_eq!(d.coded[ch].len(), d.channels[ch].len());
            assert_eq!(d.residual[ch].len(), d.channels[ch].len());
        }
    }
}

#[test]
fn exercises_interesting_features() {
    use visco_core::SubframeKind::*;
    let all: Vec<_> = fixtures("flac").into_iter().map(|(n, b)| (n, decode(&b, DecodeOptions::default()).unwrap())).collect();
    let has = |f: &dyn Fn(&visco_flac::decoder::SubframeRecord) -> bool| all.iter().any(|(_, d)| d.subframes.iter().any(f));
    assert!(has(&|s| s.kind == Constant), "no constant subframes");
    assert!(has(&|s| s.kind == Verbatim), "no verbatim subframes");
    assert!(has(&|s| s.kind == Fixed), "no fixed subframes");
    assert!(has(&|s| s.kind == Lpc && s.order > 12), "no high-order LPC");
    assert!(has(&|s| s.wasted > 0), "no wasted bits");
    assert!(has(&|s| s.rice.as_ref().is_some_and(|r| r.rice2)), "no RICE2");
    let stereo_modes: std::collections::BTreeSet<u8> =
        all.iter().flat_map(|(_, d)| d.frames.iter().map(|f| f.header.assignment.code())).collect();
    assert!(stereo_modes.len() >= 3, "{stereo_modes:?}");
}

#[test]
fn corrupt_input_does_not_panic() {
    let (_, bytes) = fixtures("flac").into_iter().find(|(n, _)| n == "s16_stereo_44k_flac5.flac").unwrap();
    for cut in [10, 100, bytes.len() / 2, bytes.len() - 1] {
        let _ = decode(&bytes[..cut], DecodeOptions::default());
    }
    let mut bad = bytes.clone();
    for i in (bytes.len() / 4..bytes.len()).step_by(211) {
        bad[i] ^= 0x41;
    }
    let d = decode(&bad, DecodeOptions::default()).unwrap();
    assert!(!d.warnings.is_empty());
}
