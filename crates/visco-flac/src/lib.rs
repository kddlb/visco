//! From-scratch FLAC codec with stage tracing.

pub mod bitio;
pub mod crc;
pub mod decoder;
pub mod encoder;
pub mod fixed;
pub mod frame;
pub mod lpc;
pub mod md5;
pub mod metadata;
pub mod rice;
pub mod stereo;
pub mod trace;

use std::sync::Arc;

use visco_core::{Codec, CodecError, Media, MediaKind, ParamSpec, Params, Trace};

use crate::{
    decoder::{DecodeOptions, decode},
    encoder::{EncodeParams, StereoMode, encode},
    lpc::Window,
};

pub struct FlacCodec;

const BLOCK_SIZES: [u32; 8] = [192, 576, 1152, 2304, 4096, 4608, 8192, 16384];
const WINDOWS: [&str; 7] = [
    "tukey(0.5)",
    "tukey + partial_tukey(2)",
    "subdivide_tukey(2)",
    "subdivide_tukey(3)",
    "hann",
    "welch",
    "rectangle",
];

impl FlacCodec {
    pub fn encode_params(p: &Params) -> EncodeParams {
        let windows = match p.choice("window", 1) {
            0 => vec![Window::Tukey(0.5)],
            2 => Window::subdivide_tukey(2),
            3 => Window::subdivide_tukey(3),
            4 => vec![Window::Hann],
            5 => vec![Window::Welch],
            6 => vec![Window::Rectangle],
            _ => EncodeParams::preset(5).windows,
        };
        EncodeParams {
            block_size: BLOCK_SIZES[p.choice("block_size", 4).min(BLOCK_SIZES.len() - 1)],
            max_lpc_order: p.int("lpc_order", 8).clamp(0, 32) as u8,
            qlp_precision: p.int("precision", 0).clamp(0, 15) as u8,
            max_partition_order: p.int("partition_order", 5).clamp(0, 8) as u8,
            stereo: if p.choice("stereo", 1) == 0 { StereoMode::Independent } else { StereoMode::Exhaustive },
            windows,
            exhaustive: p.bool("exhaustive", false),
            escape_coding: p.bool("escape", false),
        }
    }
}

impl Codec for FlacCodec {
    fn id(&self) -> &'static str {
        "flac"
    }

    fn name(&self) -> &'static str {
        "FLAC"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["flac"]
    }

    fn sniff(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(b"fLaC") || (bytes.starts_with(b"ID3") && bytes.windows(4).take(1 << 20).any(|w| w == b"fLaC"))
    }

    fn input_kind(&self) -> MediaKind {
        MediaKind::Audio
    }

    fn params_schema(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::Choice {
                key: "block_size",
                label: "Block size (samples)",
                options: vec!["192", "576", "1152", "2304", "4096", "4608", "8192", "16384"],
                default: 4,
            },
            ParamSpec::Int { key: "lpc_order", label: "Max LPC order (0 = fixed only)", min: 0, max: 32, default: 8 },
            ParamSpec::Int { key: "precision", label: "Coefficient precision (0 = auto)", min: 0, max: 15, default: 0 },
            ParamSpec::Int { key: "partition_order", label: "Max Rice partition order", min: 0, max: 8, default: 5 },
            ParamSpec::Choice { key: "stereo", label: "Stereo", options: vec!["independent", "best of 4 modes"], default: 1 },
            ParamSpec::Choice { key: "window", label: "LPC window", options: WINDOWS.to_vec(), default: 1 },
            ParamSpec::Bool { key: "exhaustive", label: "Exhaustive order search", default: false },
            ParamSpec::Bool { key: "escape", label: "Allow escaped partitions", default: false },
        ]
    }

    fn encode(&self, input: &Media, params: &Params) -> Result<Trace, CodecError> {
        let Media::Audio(pcm) = input else {
            return Err(CodecError("FLAC encodes audio, not images".into()));
        };
        if !(4..=32).contains(&pcm.bits_per_sample) || !(1..=8).contains(&pcm.channels) {
            return Err(CodecError(format!("FLAC cannot encode {}-bit, {}-channel audio", pcm.bits_per_sample, pcm.channels)));
        }
        if pcm.frames() == 0 {
            return Err(CodecError("no audio samples".into()));
        }
        let e = encode(&pcm.samples, pcm.channels, pcm.sample_rate, pcm.bits_per_sample, &Self::encode_params(params), true);
        let rt = decode(&e.bytes, DecodeOptions { record_spans: false, record_signals: true })
            .map_err(|err| CodecError(format!("round-trip decode failed: {err}")))?;
        Ok(trace::encode_trace(e, pcm, rt))
    }

    fn decode(&self, bytes: Arc<[u8]>) -> Result<Trace, CodecError> {
        let d = decode(&bytes, DecodeOptions::default()).map_err(|e| CodecError(e.to_string()))?;
        Ok(trace::decode_trace(d, bytes))
    }
}
