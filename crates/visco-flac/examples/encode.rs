//! Encode a WAV with a preset: `cargo run -p visco-flac --example encode -- in.wav out.flac 8`
use visco_flac::encoder::{EncodeParams, encode};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut r = hound::WavReader::open(&a[1]).unwrap();
    let spec = r.spec();
    let s: Vec<i32> = r.samples::<i32>().map(|s| s.unwrap()).collect();
    let level: u8 = a.get(3).map_or(5, |l| l.parse().unwrap());
    let t = std::time::Instant::now();
    let e = encode(&s, spec.channels as u8, spec.sample_rate, spec.bits_per_sample as u8, &EncodeParams::preset(level), false);
    eprintln!("encoded {} samples in {:?} → {} bytes", s.len(), t.elapsed(), e.bytes.len());
    std::fs::write(&a[2], e.bytes).unwrap();
}
