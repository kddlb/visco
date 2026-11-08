//! Summarize the subframe choices in a FLAC file: `cargo run -p visco-flac --example flacinfo -- f.flac`
use std::collections::BTreeMap;
use visco_flac::decoder::{DecodeOptions, decode, subframe_label};

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let d = decode(&bytes, DecodeOptions { record_spans: false, record_signals: false }).unwrap();
        let mut hist: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        for s in &d.subframes {
            let mut k = subframe_label(s);
            if !s.qlp.is_empty() {
                k = format!("{k} p{} s{}", s.qlp_precision, s.shift);
            }
            if let Some(r) = &s.rice {
                k = format!("{k} po{}", r.order);
            }
            let e = hist.entry(k).or_default();
            e.0 += 1;
            e.1 += s.bits.end - s.bits.start;
        }
        println!("{path}: {} frames, md5 ok {:?}", d.frames.len(), d.md5_ok());
        for (k, (n, bits)) in hist {
            println!("  {k:<28} ×{n:<4} {:>9} bits", bits);
        }
    }
}
