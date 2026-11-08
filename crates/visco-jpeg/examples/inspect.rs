//! Print a decode summary: `cargo run -p visco-jpeg --example inspect -- file.jpg`
use visco_jpeg::decoder::{DecodeOptions, decode};

fn main() {
    let path = std::env::args().nth(1).expect("usage: inspect FILE");
    let bytes = std::fs::read(&path).unwrap();
    let d = decode(&bytes, DecodeOptions::default()).unwrap();
    println!("{:?} {}x{} {} color {:?} ri {}", d.frame.kind, d.frame.width, d.frame.height, d.frame.subsampling_label(), d.color, d.restart_interval);
    for s in &d.segments {
        println!("  {:>7}..{:<7} {:<20} {:?}", s.bytes.start, s.bytes.end, s.name, s.fields);
    }
    for (i, s) in d.scans.iter().enumerate() {
        println!("  scan {i}: comps {:?} Ss={} Se={} Ah={} Al={}", s.header.comps.iter().map(|c| c.index).collect::<Vec<_>>(), s.header.ss, s.header.se, s.header.ah, s.header.al);
    }
    println!("  spans: {} (truncated: {})", d.spans.len(), d.spans_truncated);
    for w in &d.warnings {
        println!("  warning: {w}");
    }
}
