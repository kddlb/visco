//! FLAC decoder (RFC 9639) with stage recording: every frame, subframe, and Rice partition.

use std::ops::Range;

use visco_core::{BitSpan, NO_BLOCK, Segment, SpanKind, SubframeKind};

use crate::{
    bitio::{BitReader, Eof},
    crc::crc16,
    fixed, frame, lpc,
    md5::{self, Md5},
    metadata::{self, StreamInfo},
    stereo::{self, ChannelAssignment},
};

pub const MAX_SPANS: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartitionInfo {
    pub k: u8,
    pub escaped: bool,
    pub count: u32,
    pub bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiceInfo {
    pub rice2: bool,
    pub order: u8,
    pub partitions: Vec<PartitionInfo>,
}

/// Everything about one decoded (or encoded) subframe.
#[derive(Clone, Debug, PartialEq)]
pub struct SubframeRecord {
    pub frame: u32,
    pub channel: u8,
    pub kind: SubframeKind,
    pub order: u8,
    pub wasted: u8,
    /// Sample depth the subframe was coded at (includes the side channel's extra bit, minus wasted bits).
    pub coded_bps: u8,
    pub qlp: Vec<i32>,
    pub qlp_precision: u8,
    pub shift: i8,
    pub rice: Option<RiceInfo>,
    /// Absolute file bit range.
    pub bits: Range<u64>,
}

#[derive(Clone, Debug)]
pub struct FrameRecord {
    pub header: frame::FrameHeader,
    pub first_sample: u64,
    pub bytes: Range<usize>,
    pub header_bits: u32,
    pub crc_ok: bool,
}

pub struct Decoded {
    pub info: StreamInfo,
    pub frames: Vec<FrameRecord>,
    /// `frame * channels + channel`.
    pub subframes: Vec<SubframeRecord>,
    /// Decoded output per channel.
    pub channels: Vec<Vec<i32>>,
    /// Coded channel signals before stereo reconstruction (e.g. mid/side), normalized to ±1.
    pub coded: Vec<Vec<f32>>,
    /// Prediction residual per coded channel, aligned with samples (warm-up = 0), normalized.
    pub residual: Vec<Vec<f32>>,
    pub segments: Vec<Segment>,
    pub spans: Vec<BitSpan>,
    pub spans_truncated: bool,
    pub md5: [u8; 16],
    pub warnings: Vec<String>,
}

impl Decoded {
    pub fn md5_ok(&self) -> Option<bool> {
        (self.info.md5 != [0; 16]).then(|| self.md5 == self.info.md5)
    }

    pub fn interleaved(&self) -> Vec<i32> {
        let n = self.channels.first().map_or(0, |c| c.len());
        let mut out = Vec::with_capacity(n * self.channels.len());
        for i in 0..n {
            for c in &self.channels {
                out.push(c[i]);
            }
        }
        out
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("not a FLAC stream (missing fLaC marker)")]
    NotFlac,
    #[error("missing or invalid STREAMINFO")]
    NoStreamInfo,
    #[error("{0}")]
    Other(String),
}

#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    pub record_spans: bool,
    pub record_signals: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self { record_spans: true, record_signals: true }
    }
}

struct Sink {
    on: bool,
    spans: Vec<BitSpan>,
    truncated: bool,
}

impl Sink {
    #[inline]
    fn push(&mut self, start: u64, end: u64, block: u32, kind: SpanKind) {
        if !self.on || end <= start {
            return;
        }
        if self.spans.len() >= MAX_SPANS {
            self.truncated = true;
            return;
        }
        self.spans.push(BitSpan { start, len: (end - start).min(u32::MAX as u64) as u32, block, kind });
    }
}

/// Skip an ID3v2 tag if present; returns the offset of "fLaC".
fn skip_id3(data: &[u8]) -> usize {
    if data.len() >= 10 && &data[..3] == b"ID3" {
        let size = data[6..10].iter().fold(0usize, |v, &b| (v << 7) | (b & 0x7F) as usize);
        let footer = if data[5] & 0x10 != 0 { 10 } else { 0 };
        10 + size + footer
    } else {
        0
    }
}

pub fn decode(data: &[u8], opts: DecodeOptions) -> Result<Decoded, DecodeError> {
    let mut segments = vec![];
    let start = skip_id3(data);
    if start > 0 {
        segments.push(Segment { name: "ID3v2 tag".into(), bytes: 0..start.min(data.len()), fields: vec![] });
    }
    if data.get(start..start + 4) != Some(b"fLaC") {
        return Err(DecodeError::NotFlac);
    }
    segments.push(Segment { name: "fLaC marker".into(), bytes: start..start + 4, fields: vec![] });
    let mut pos = start + 4;
    let mut info = None;
    loop {
        let Some(h) = data.get(pos..pos + 4) else { return Err(DecodeError::NoStreamInfo) };
        let last = h[0] & 0x80 != 0;
        let t = h[0] & 0x7F;
        let len = ((h[1] as usize) << 16) | ((h[2] as usize) << 8) | h[3] as usize;
        let Some(payload) = data.get(pos + 4..pos + 4 + len) else {
            return Err(DecodeError::Other(format!("truncated {} block", metadata::block_name(t))));
        };
        if t == 0 {
            info = StreamInfo::parse(payload);
        }
        segments.push(Segment { name: metadata::block_name(t), bytes: pos..pos + 4 + len, fields: metadata::describe(t, payload) });
        pos += 4 + len;
        if last {
            break;
        }
    }
    let info = info.ok_or(DecodeError::NoStreamInfo)?;
    let nch = info.channels as usize;
    let full_scale = (1u64 << (info.bits_per_sample - 1)) as f32;

    let mut sink = Sink { on: opts.record_spans, spans: vec![], truncated: false };
    let mut frames = vec![];
    let mut subframes = vec![];
    let mut channels: Vec<Vec<i32>> = vec![Vec::with_capacity(info.total_samples as usize); nch];
    let mut coded: Vec<Vec<f32>> = vec![vec![]; if opts.record_signals { nch } else { 0 }];
    let mut residual: Vec<Vec<f32>> = vec![vec![]; if opts.record_signals { nch } else { 0 }];
    let mut warnings = vec![];
    let mut md5 = Md5::default();

    while pos + 2 <= data.len() {
        let (h, body) = match frame::parse(data, pos, info.sample_rate, info.bits_per_sample) {
            Ok(x) => x,
            Err(e) => {
                // Resynchronize: look for the next plausible sync code.
                let next = (pos + 1..data.len().saturating_sub(1)).find(|&i| data[i] == 0xFF && data[i + 1] & 0xFE == 0xF8);
                if !matches!(e, frame::HeaderError::NoSync) || next.is_some() {
                    warnings.push(format!("byte {pos}: {e}, resynchronizing"));
                }
                match next {
                    Some(n) => {
                        pos = n;
                        continue;
                    }
                    None => break,
                }
            }
        };
        if h.assignment.channels() != nch {
            warnings.push(format!("frame at byte {pos} has {} channels, expected {nch}", h.assignment.channels()));
            pos += 1;
            continue;
        }
        let fi = frames.len() as u32;
        let span_mark = sink.spans.len();
        let header_bits = ((body - pos) * 8) as u32;
        sink.push(pos as u64 * 8, body as u64 * 8 - 8, NO_BLOCK, SpanKind::FrameHeader { frame: fi });
        sink.push(body as u64 * 8 - 8, body as u64 * 8, NO_BLOCK, SpanKind::Crc8);

        let mut r = BitReader::at(data, body);
        let bs = h.block_size as usize;
        let mut chans: Vec<Vec<i64>> = Vec::with_capacity(nch);
        let mut frame_subs = Vec::with_capacity(nch);
        let mut failed = None;
        for ch in 0..nch {
            let bps = h.bits_per_sample as u32 + (h.assignment.side_channel() == Some(ch)) as u32;
            let id = (fi as usize * nch + ch) as u32;
            match decode_subframe(&mut r, bs, bps, fi, ch as u8, id, &mut sink) {
                Ok((samples, rec, res)) => {
                    if opts.record_signals {
                        coded[ch].extend(samples.iter().map(|&v| v as f32 / full_scale));
                        residual[ch].extend(res.iter().map(|&v| v as f32 / full_scale));
                    }
                    chans.push(samples);
                    frame_subs.push(rec);
                }
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = failed {
            warnings.push(format!("frame {fi} at byte {pos}: {e}; frame skipped"));
            sink.spans.truncate(span_mark);
            // Undo partial signal recording for this frame.
            for ch in 0..coded.len() {
                let keep = channels[ch].len();
                coded[ch].truncate(keep);
                residual[ch].truncate(keep);
            }
            pos += 2;
            continue;
        }
        // Byte alignment + CRC-16.
        let pad_start = r.pos();
        r.align();
        sink.push(pad_start, r.pos(), NO_BLOCK, SpanKind::Padding);
        let crc_pos = r.byte_pos();
        let Ok(stored) = r.read(16) else {
            warnings.push(format!("frame {fi}: truncated before CRC-16"));
            break;
        };
        sink.push(crc_pos as u64 * 8, crc_pos as u64 * 8 + 16, NO_BLOCK, SpanKind::Crc16);
        let crc_ok = crc16(&data[pos..crc_pos]) == stored as u16;
        if !crc_ok {
            warnings.push(format!("frame {fi}: CRC-16 mismatch"));
        }
        let end = crc_pos + 2;

        if chans.len() == 2 {
            let (a, b) = chans.split_at_mut(1);
            stereo::decode(&mut a[0], &mut b[0], h.assignment);
        }
        let first_sample = channels[0].len() as u64;
        let mut inter = Vec::with_capacity(bs * nch);
        for i in 0..bs {
            for c in &chans {
                inter.push(c[i] as i32);
            }
        }
        md5::update_samples(&mut md5, &inter, info.bits_per_sample);
        for (dst, src) in channels.iter_mut().zip(&chans) {
            dst.extend(src.iter().map(|&v| v as i32));
        }
        segments.push(Segment {
            name: format!("Frame {fi}"),
            bytes: pos..end,
            fields: vec![
                ("samples".into(), format!("{first_sample}..{} ({bs})", first_sample + bs as u64)),
                ("channels".into(), h.assignment.label().into()),
                ("bits per sample".into(), h.bits_per_sample.to_string()),
                ("blocking".into(), if h.variable_block_size { "variable".into() } else { "fixed".into() }),
                (
                    "subframes".into(),
                    frame_subs.iter().map(|s: &SubframeRecord| subframe_label(s)).collect::<Vec<_>>().join(", "),
                ),
                ("CRC-16".into(), if crc_ok { "ok".into() } else { "MISMATCH".into() }),
            ],
        });
        frames.push(FrameRecord { header: h, first_sample, bytes: pos..end, header_bits, crc_ok });
        subframes.extend(frame_subs);
        pos = end;
    }

    if frames.is_empty() {
        return Err(DecodeError::Other("no audio frames".into()));
    }
    let md5 = md5.finish();
    let decoded_samples = channels[0].len() as u64;
    if info.total_samples != 0 && decoded_samples != info.total_samples {
        warnings.push(format!("STREAMINFO says {} samples, decoded {decoded_samples}", info.total_samples));
    }
    if info.md5 != [0; 16] && md5 != info.md5 {
        warnings.push("MD5 of decoded audio does not match STREAMINFO".into());
    }
    if pos < data.len() {
        segments.push(Segment { name: "trailing data".into(), bytes: pos..data.len(), fields: vec![] });
    }
    Ok(Decoded {
        info,
        frames,
        subframes,
        channels,
        coded,
        residual,
        segments,
        spans: sink.spans,
        spans_truncated: sink.truncated,
        md5,
        warnings,
    })
}

pub fn subframe_label(s: &SubframeRecord) -> String {
    match s.kind {
        SubframeKind::Constant => "constant".into(),
        SubframeKind::Verbatim => "verbatim".into(),
        SubframeKind::Fixed => format!("fixed {}", s.order),
        SubframeKind::Lpc => format!("LPC {}", s.order),
    }
}

type SubframeOut = (Vec<i64>, SubframeRecord, Vec<i64>);

#[allow(clippy::too_many_arguments)]
fn decode_subframe(
    r: &mut BitReader,
    bs: usize,
    bps: u32,
    frame: u32,
    channel: u8,
    id: u32,
    sink: &mut Sink,
) -> Result<SubframeOut, String> {
    let eof = |_: Eof| "truncated subframe".to_string();
    let start = r.pos();
    if r.bit().map_err(eof)? {
        return Err("subframe padding bit set".into());
    }
    let t = r.read(6).map_err(eof)?;
    let wasted = if r.bit().map_err(eof)? { r.read_unary().map_err(eof)? as u32 + 1 } else { 0 };
    if wasted >= bps {
        return Err(format!("{wasted} wasted bits for {bps}-bit samples"));
    }
    let bps_eff = bps - wasted;
    let (kind, order) = match t {
        0 => (SubframeKind::Constant, 0),
        1 => (SubframeKind::Verbatim, 0),
        8..=12 => (SubframeKind::Fixed, t - 8),
        32..=63 => (SubframeKind::Lpc, t - 31),
        _ => return Err(format!("reserved subframe type {t:#08b}")),
    };
    sink.push(start, r.pos(), id, SpanKind::SubframeHeader { kind, order: order as u8, wasted: wasted as u8 });
    let order = order as usize;
    if order > bs {
        return Err(format!("predictor order {order} exceeds block size {bs}"));
    }

    let mut x = vec![0i64; bs];
    let mut res = vec![0i64; bs];
    let mut rec = SubframeRecord {
        frame,
        channel,
        kind,
        order: order as u8,
        wasted: wasted as u8,
        coded_bps: bps_eff as u8,
        qlp: vec![],
        qlp_precision: 0,
        shift: 0,
        rice: None,
        bits: start..start,
    };
    match kind {
        SubframeKind::Constant => {
            let s = r.pos();
            let v = r.read_signed(bps_eff).map_err(eof)?;
            x.fill(v);
            sink.push(s, r.pos(), id, SpanKind::Constant);
        }
        SubframeKind::Verbatim => {
            let s = r.pos();
            for v in &mut x {
                *v = r.read_signed(bps_eff).map_err(eof)?;
            }
            res.copy_from_slice(&x);
            sink.push(s, r.pos(), id, SpanKind::Verbatim);
        }
        SubframeKind::Fixed | SubframeKind::Lpc => {
            let s = r.pos();
            for v in &mut x[..order] {
                *v = r.read_signed(bps_eff).map_err(eof)?;
            }
            sink.push(s, r.pos(), id, SpanKind::Warmup { n: order as u8 });
            if kind == SubframeKind::Lpc {
                let s = r.pos();
                let p = r.read(4).map_err(eof)?;
                if p == 15 {
                    return Err("invalid QLP precision".into());
                }
                let precision = p + 1;
                let shift = r.read_signed(5).map_err(eof)? as i32;
                if shift < 0 {
                    return Err("negative LPC shift".into());
                }
                rec.qlp = (0..order).map(|_| r.read_signed(precision).map(|v| v as i32)).collect::<Result<_, _>>().map_err(eof)?;
                rec.qlp_precision = precision as u8;
                rec.shift = shift as i8;
                sink.push(s, r.pos(), id, SpanKind::LpcCoefs { order: order as u8, precision: precision as u8, shift: shift as i8 });
            }
            let (rice, e) = decode_residual(r, bs, order, id, sink)?;
            res[order..].copy_from_slice(&e);
            if kind == SubframeKind::Fixed {
                fixed::restore(&mut x, order, &e);
            } else {
                lpc::restore(&mut x, &rec.qlp, rec.shift as i32, &e);
            }
            rec.rice = Some(rice);
        }
    }
    if wasted > 0 {
        // Restore wasted bits on both the samples and (for display) the residual.
        for v in x.iter_mut().chain(res.iter_mut()) {
            *v <<= wasted;
        }
    }
    rec.bits = start..r.pos();
    Ok((x, rec, res))
}

fn decode_residual(r: &mut BitReader, bs: usize, order: usize, id: u32, sink: &mut Sink) -> Result<(RiceInfo, Vec<i64>), String> {
    let eof = |_: Eof| "truncated residual".to_string();
    let s = r.pos();
    let method = r.read(2).map_err(eof)?;
    if method > 1 {
        return Err(format!("reserved residual coding method {method}"));
    }
    let rice2 = method == 1;
    let po = r.read(4).map_err(eof)?;
    sink.push(s, r.pos(), id, SpanKind::ResidualHeader { rice2, partition_order: po as u8 });
    let parts = 1usize << po;
    if !bs.is_multiple_of(parts) || (bs >> po) < order {
        return Err(format!("partition order {po} invalid for block size {bs}, order {order}"));
    }
    let (pbits, escape) = if rice2 { (5, 0x1F) } else { (4, 0xF) };
    let mut out = Vec::with_capacity(bs - order);
    let mut partitions = Vec::with_capacity(parts);
    for p in 0..parts {
        let n = if p == 0 { (bs >> po) - order } else { bs >> po };
        let ps = r.pos();
        let k = r.read(pbits).map_err(eof)?;
        let escaped = k == escape;
        let k = if escaped {
            let raw = r.read(5).map_err(eof)?;
            for _ in 0..n {
                out.push(r.read_signed(raw).map_err(eof)?);
            }
            raw
        } else {
            for _ in 0..n {
                out.push(r.read_rice(k).map_err(eof)?);
            }
            k
        };
        let bits = (r.pos() - ps) as u32;
        sink.push(ps, r.pos(), id, SpanKind::RicePartition { partition: p as u16, k: k as u8, escaped, count: n as u16 });
        partitions.push(PartitionInfo { k: k as u8, escaped, count: n as u32, bits });
    }
    Ok((RiceInfo { rice2, order: po as u8, partitions }, out))
}

/// Recompute a subframe's prediction and residual from decoded samples (for the inspector).
/// `x` is the coded channel signal for the frame (after wasted-bit restoration).
pub fn analyze(rec: &SubframeRecord, x: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let w = rec.wasted as u32;
    let xs: Vec<i64> = x.iter().map(|&v| v >> w).collect();
    let o = rec.order as usize;
    let mut pred = vec![0i64; xs.len()];
    let mut res = vec![0i64; xs.len()];
    match rec.kind {
        SubframeKind::Constant => pred.copy_from_slice(&xs),
        SubframeKind::Verbatim => res.copy_from_slice(&xs),
        SubframeKind::Fixed => {
            for n in o..xs.len() {
                pred[n] = fixed::predict(&xs, n, o);
                res[n] = xs[n] - pred[n];
            }
            pred[..o].copy_from_slice(&xs[..o]);
        }
        SubframeKind::Lpc => {
            for n in o..xs.len() {
                pred[n] = lpc::predict(&xs, n, &rec.qlp, rec.shift as i32);
                res[n] = xs[n] - pred[n];
            }
            pred[..o].copy_from_slice(&xs[..o]);
        }
    }
    (pred.into_iter().map(|v| v << w).collect(), res)
}

pub fn assignment_label(a: ChannelAssignment) -> &'static str {
    a.label()
}
