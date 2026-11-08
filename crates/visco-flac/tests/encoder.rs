mod common;
use common::*;
use visco_flac::{
    decoder::{DecodeOptions, decode},
    encoder::{EncodeParams, StereoMode, encode},
};

type Wav = (u32, u8, u8, Vec<i32>);

fn sources() -> Vec<(String, Wav)> {
    fixtures("wav").into_iter().map(|(n, b)| (n, read_wav(&b))).collect()
}

#[test]
fn lossless_roundtrip_all_sources_and_presets() {
    for (name, (rate, bits, ch, samples)) in sources() {
        for level in [0, 5, 8] {
            let e = encode(&samples, ch, rate, bits, &EncodeParams::preset(level), true);
            let d = decode(&e.bytes, DecodeOptions::default()).unwrap_or_else(|err| panic!("{name} -{level}: {err}"));
            assert!(d.warnings.is_empty(), "{name} -{level}: {:?}", d.warnings);
            assert_eq!(d.md5_ok(), Some(true), "{name} -{level}");
            assert_eq!(d.interleaved(), samples, "{name} -{level}: our decoder");
            assert_eq!(claxon_decode(&e.bytes), samples, "{name} -{level}: claxon");
            // Our decoder must find exactly the spans and subframes our encoder wrote.
            assert_eq!(d.spans, e.spans, "{name} -{level}: spans");
            assert_eq!(d.subframes, e.subframes, "{name} -{level}: subframe records");
        }
    }
}

#[test]
fn options_roundtrip() {
    let (_, (rate, bits, ch, samples)) = sources().into_iter().find(|(n, _)| n == "s16_stereo_44k.wav").unwrap();
    let mut variants = vec![];
    for bs in [192, 576, 1000, 4608, 16384] {
        variants.push(EncodeParams { block_size: bs, ..EncodeParams::preset(5) });
    }
    variants.push(EncodeParams { max_lpc_order: 32, exhaustive: true, max_partition_order: 8, ..EncodeParams::preset(8) });
    variants.push(EncodeParams { qlp_precision: 15, ..EncodeParams::preset(5) });
    variants.push(EncodeParams { qlp_precision: 5, ..EncodeParams::preset(5) });
    variants.push(EncodeParams { stereo: StereoMode::Independent, max_partition_order: 0, ..EncodeParams::preset(5) });
    for p in variants {
        let e = encode(&samples, ch, rate, bits, &p, false);
        assert_eq!(claxon_decode(&e.bytes), samples, "{p:?}");
        let d = decode(&e.bytes, DecodeOptions { record_spans: false, record_signals: false }).unwrap();
        assert!(d.warnings.is_empty(), "{p:?}: {:?}", d.warnings);
    }
}

#[test]
fn synthetic_edge_cases() {
    let cases: Vec<(&str, u8, u8, Vec<i32>)> = vec![
        ("one sample", 1, 16, vec![1234]),
        ("tiny stereo", 2, 16, vec![1, -1, 2, -2, 3, -3]),
        ("32-bit extremes", 2, 32, (0..5000).map(|i| if i % 2 == 0 { i32::MAX - i } else { i32::MIN + i }).collect()),
        ("4-bit", 1, 4, (0..3000).map(|i| (i % 16) - 8).collect()),
        ("20-bit 3ch", 3, 20, (0..9000).map(|i| ((i as f64 * 0.01).sin() * 400_000.0) as i32).collect()),
        ("all zero", 2, 16, vec![0; 10_000]),
    ];
    for (name, ch, bps, s) in cases {
        let e = encode(&s, ch, 44_100, bps, &EncodeParams::preset(8), true);
        let d = decode(&e.bytes, DecodeOptions::default()).unwrap_or_else(|err| panic!("{name}: {err}"));
        assert!(d.warnings.is_empty(), "{name}: {:?}", d.warnings);
        assert_eq!(d.interleaved(), s, "{name}");
        assert_eq!(d.spans, e.spans, "{name}");
    }
}

#[test]
fn escape_partitions_roundtrip_through_our_decoder() {
    // Noise bursts in a quiet signal make raw (escaped) partitions the cheapest choice.
    let mut seed = 7u32;
    let s: Vec<i32> = (0..20_000)
        .map(|i| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            if (i / 512) % 3 == 0 { (seed >> 16) as i32 % 30000 } else { i % 3 }
        })
        .collect();
    let p = EncodeParams { escape_coding: true, ..EncodeParams::preset(8) };
    let e = encode(&s, 1, 44_100, 16, &p, true);
    let d = decode(&e.bytes, DecodeOptions::default()).unwrap();
    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    assert_eq!(d.interleaved(), s);
    assert!(d.subframes.iter().any(|sf| sf.rice.as_ref().is_some_and(|r| r.partitions.iter().any(|p| p.escaped))), "no escaped partitions produced");
}

/// Needs libFLAC's `flac` on PATH: validates our output and compares compressed size.
#[test]
#[ignore]
fn compare_with_libflac() {
    let dir = std::env::temp_dir().join("visco-flac-compare");
    std::fs::create_dir_all(&dir).unwrap();
    let mut worst = 0.0f64;
    for (name, (rate, bits, ch, samples)) in sources() {
        for level in [0u8, 5, 8] {
            let ours = encode(&samples, ch, rate, bits, &EncodeParams::preset(level), false);
            let ours_path = dir.join(format!("{name}.{level}.ours.flac"));
            std::fs::write(&ours_path, &ours.bytes).unwrap();
            let t = std::process::Command::new("flac").args(["-t", "-s"]).arg(&ours_path).status().unwrap();
            assert!(t.success(), "flac -t rejected {name} -{level}");
            let reference = dir.join(format!("{name}.{level}.ref.flac"));
            let st = std::process::Command::new("flac")
                .args(["-s", "-f", "--no-padding", "--no-seektable", &format!("-{level}"), "-o"])
                .arg(&reference)
                .arg(fixture_dir().join(&name))
                .status()
                .unwrap();
            assert!(st.success());
            let ref_audio = audio_bytes(&std::fs::read(&reference).unwrap());
            let our_audio = audio_bytes(&ours.bytes);
            let ratio = our_audio as f64 / ref_audio as f64;
            worst = worst.max(ratio);
            println!("{name:<24} -{level}: ours {our_audio:>8} B  libFLAC {ref_audio:>8} B  ({:+.2}%)", (ratio - 1.0) * 100.0);
        }
    }
    println!("worst ratio {worst:.4}");
}

/// Bytes of audio frames (file minus metadata).
fn audio_bytes(f: &[u8]) -> usize {
    let mut pos = 4;
    loop {
        let last = f[pos] & 0x80 != 0;
        let len = ((f[pos + 1] as usize) << 16) | ((f[pos + 2] as usize) << 8) | f[pos + 3] as usize;
        pos += 4 + len;
        if last {
            return f.len() - pos;
        }
    }
}
