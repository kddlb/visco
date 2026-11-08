//! From-scratch JPEG codec with stage tracing.

pub mod bitio;
pub mod color;
pub mod dct;
pub mod decoder;
pub mod encoder;
pub mod huffman;
pub mod markers;
pub mod quant;
pub mod sampling;
pub mod trace;
pub mod zigzag;

use std::sync::Arc;

use visco_core::{Codec, CodecError, Media, MediaKind, ParamSpec, Params, Trace};

use crate::{
    decoder::{DecodeOptions, decode},
    encoder::{EncodeParams, encode},
    sampling::Subsampling,
};

pub struct JpegCodec;

impl JpegCodec {
    pub fn encode_params(p: &Params) -> EncodeParams {
        EncodeParams {
            quality: p.int("quality", 75).clamp(1, 100) as u8,
            subsampling: Subsampling::ALL[p.choice("subsampling", 2).min(2)],
            restart_interval: p.int("restart", 0).clamp(0, 65535) as u16,
            grayscale: p.bool("grayscale", false),
            qtables: None,
        }
    }
}

impl Codec for JpegCodec {
    fn id(&self) -> &'static str {
        "jpeg"
    }

    fn name(&self) -> &'static str {
        "JPEG"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["jpg", "jpeg", "jpe", "jfif"]
    }

    fn sniff(&self, bytes: &[u8]) -> bool {
        bytes.starts_with(&[0xFF, 0xD8, 0xFF])
    }

    fn input_kind(&self) -> MediaKind {
        MediaKind::Image
    }

    fn params_schema(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::Int { key: "quality", label: "Quality", min: 1, max: 100, default: 75 },
            ParamSpec::Choice {
                key: "subsampling",
                label: "Chroma subsampling",
                options: Subsampling::ALL.iter().map(|s| s.label()).collect(),
                default: 2,
            },
            ParamSpec::Int { key: "restart", label: "Restart interval (MCUs)", min: 0, max: 64, default: 0 },
            ParamSpec::Bool { key: "grayscale", label: "Grayscale", default: false },
        ]
    }

    fn encode(&self, input: &Media, params: &Params) -> Result<Trace, CodecError> {
        let Media::Image(input) = input else {
            return Err(CodecError("JPEG encodes images, not audio".into()));
        };
        if input.width == 0 || input.height == 0 || input.width > 65535 || input.height > 65535 {
            return Err(CodecError(format!("JPEG cannot encode a {}×{} image", input.width, input.height)));
        }
        let e = encode(&input.data, input.width, input.height, &Self::encode_params(params), true);
        let rt = decode(&e.bytes, DecodeOptions { record_spans: false, snapshots: false, ..Default::default() })
            .map_err(|err| CodecError(format!("round-trip decode failed: {err}")))?;
        Ok(trace::encode_trace(e, &input.data, &rt))
    }

    fn decode(&self, bytes: Arc<[u8]>) -> Result<Trace, CodecError> {
        let d = decode(&bytes, DecodeOptions::default()).map_err(|e| CodecError(e.to_string()))?;
        Ok(trace::decode_trace(d, bytes))
    }
}
