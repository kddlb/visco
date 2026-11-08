#![allow(dead_code)]
use std::path::PathBuf;

pub fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/flac")
}

pub fn fixtures(ext: &str) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == ext).then(|| (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()))
        })
        .collect();
    v.sort();
    v
}

/// (sample rate, bits, channels, interleaved samples) from a WAV.
pub fn read_wav(bytes: &[u8]) -> (u32, u8, u8, Vec<i32>) {
    let mut r = hound::WavReader::new(bytes).unwrap();
    let spec = r.spec();
    let samples: Vec<i32> = r.samples::<i32>().map(|s| s.unwrap()).collect();
    (spec.sample_rate, spec.bits_per_sample as u8, spec.channels as u8, samples)
}

/// Reference decode with claxon → interleaved samples.
pub fn claxon_decode(bytes: &[u8]) -> Vec<i32> {
    let mut r = claxon::FlacReader::new(bytes).unwrap();
    r.samples().map(|s| s.unwrap()).collect()
}

/// The WAV source a fixture was encoded from.
pub fn source_for(flac_name: &str) -> String {
    let stem = flac_name.trim_end_matches(".flac");
    let cut = ["_flac", "_lax", "_metadata", "_b576", "_ffmpeg"].iter().filter_map(|m| stem.find(m)).min().unwrap();
    format!("{}.wav", &stem[..cut])
}
