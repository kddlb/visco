//! FLAC encoder: per-subframe search over constant / verbatim / fixed / LPC predictors,
//! stereo decorrelation, and Rice partitioning. Every decision is recorded for the visualizer.

use visco_core::{BitSpan, NO_BLOCK, Segment, SpanKind, SubframeKind};

use crate::{
    bitio::BitWriter,
    crc::crc16,
    decoder::{FrameRecord, MAX_SPANS, PartitionInfo, RiceInfo, SubframeRecord},
    fixed,
    frame::{self, FrameHeader},
    lpc::{self, Window},
    md5::{self, Md5},
    metadata::StreamInfo,
    rice::{self, RICE_ESCAPE, RICE2_ESCAPE, RicePlan},
    stereo::ChannelAssignment,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StereoMode {
    /// Code left and right separately.
    Independent,
    /// Try L/R, L/S, S/R and M/S and keep the smallest (libFLAC `-m`).
    Exhaustive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodeParams {
    pub block_size: u32,
    /// 0 disables LPC (fixed predictors only).
    pub max_lpc_order: u8,
    /// 0 = choose automatically like libFLAC.
    pub qlp_precision: u8,
    pub max_partition_order: u8,
    pub stereo: StereoMode,
    pub windows: Vec<Window>,
    /// Try every LPC order instead of the estimated best one (libFLAC `-e`).
    pub exhaustive: bool,
    /// Allow escaped (raw) Rice partitions. Off by default, like libFLAC; some decoders lack it.
    pub escape_coding: bool,
}

impl EncodeParams {
    /// Settings matching `flac -<level>`.
    pub fn preset(level: u8) -> Self {
        let tukey = vec![Window::Tukey(0.5)];
        let (block_size, max_lpc_order, max_partition_order, stereo, windows) = match level {
            0 => (1152, 0, 3, StereoMode::Independent, tukey),
            1 | 2 => (1152, 0, 3, StereoMode::Exhaustive, tukey),
            3 => (4096, 6, 4, StereoMode::Independent, tukey),
            4 => (4096, 8, 4, StereoMode::Exhaustive, tukey),
            5 => (4096, 8, 5, StereoMode::Exhaustive, vec![Window::Tukey(0.5), Window::PartialTukey { n: 2, i: 0, p: 0.5 }, Window::PartialTukey { n: 2, i: 1, p: 0.5 }]),
            6 => (4096, 8, 6, StereoMode::Exhaustive, Window::subdivide_tukey(2)),
            7 => (4096, 12, 6, StereoMode::Exhaustive, Window::subdivide_tukey(2)),
            _ => (4096, 12, 6, StereoMode::Exhaustive, Window::subdivide_tukey(3)),
        };
        Self { block_size, max_lpc_order, qlp_precision: 0, max_partition_order, stereo, windows, exhaustive: false, escape_coding: false }
    }
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self::preset(5)
    }
}

/// One option the encoder evaluated for a subframe.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub label: String,
    pub bits: u64,
}

#[derive(Clone, Debug)]
struct Choice {
    kind: SubframeKind,
    order: usize,
    qlp: Vec<i32>,
    precision: u32,
    shift: i32,
    plan: Option<RicePlan>,
    residual: Vec<i64>,
    bits: u64,
    label: String,
}

struct ChannelResult {
    choice: Choice,
    wasted: u32,
    bps: u32,
    candidates: Vec<Candidate>,
}

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub info: StreamInfo,
    pub frames: Vec<FrameRecord>,
    pub subframes: Vec<SubframeRecord>,
    /// Per subframe: every predictor the search evaluated, with its estimated cost.
    pub candidates: Vec<Vec<Candidate>>,
    /// Per frame: estimated bits for each stereo assignment tried.
    pub stereo_costs: Vec<Vec<(ChannelAssignment, u64)>>,
    pub segments: Vec<Segment>,
    pub spans: Vec<BitSpan>,
    pub spans_truncated: bool,
}

struct Sink {
    on: bool,
    spans: Vec<BitSpan>,
    truncated: bool,
}

impl Sink {
    fn push(&mut self, start: u64, end: u64, block: u32, kind: SpanKind) {
        if !self.on || end <= start {
            return;
        }
        if self.spans.len() >= MAX_SPANS {
            self.truncated = true;
            return;
        }
        self.spans.push(BitSpan { start, len: (end - start) as u32, block, kind });
    }
}

/// Encode interleaved PCM. `bits_per_sample` 4..=32, 1..=8 channels.
pub fn encode(samples: &[i32], channels: u8, sample_rate: u32, bits_per_sample: u8, p: &EncodeParams, record: bool) -> Encoded {
    let nch = channels as usize;
    assert!((1..=8).contains(&nch) && (4..=32).contains(&bits_per_sample));
    let total = samples.len() / nch;
    let bs = (p.block_size as usize).clamp(16, 65535);
    let mut md5 = Md5::default();
    md5::update_samples(&mut md5, &samples[..total * nch], bits_per_sample);

    let mut info = StreamInfo {
        min_block: bs as u16,
        max_block: bs as u16,
        min_frame: 0,
        max_frame: 0,
        sample_rate,
        channels,
        bits_per_sample,
        total_samples: total as u64,
        md5: md5.finish(),
    };
    // fLaC + STREAMINFO placeholder (patched at the end with frame sizes).
    let mut w = BitWriter::new(b"fLaC".to_vec());
    let mut segments = vec![Segment { name: "fLaC marker".into(), bytes: 0..4, fields: vec![] }];
    w.write(0x80, 8); // last block, type 0
    w.write(34, 24);
    let si_pos = w.out.len();
    w.out.extend_from_slice(&info.write());
    segments.push(Segment { name: "STREAMINFO".into(), bytes: 4..si_pos + 34, fields: vec![] });

    let mut sink = Sink { on: record, spans: vec![], truncated: false };
    let mut frames = vec![];
    let mut subframes = vec![];
    let mut all_candidates = vec![];
    let mut stereo_costs = vec![];
    let (mut min_frame, mut max_frame) = (u32::MAX, 0u32);

    for (fi, start) in (0..total).step_by(bs).enumerate() {
        let n = bs.min(total - start);
        let chans: Vec<Vec<i64>> =
            (0..nch).map(|c| (start..start + n).map(|i| samples[i * nch + c] as i64).collect()).collect();
        let bps = bits_per_sample as u32;

        // Choose the stereo assignment by encoding every candidate channel.
        let (assignment, coded, results, costs) = if nch == 2 && p.stereo == StereoMode::Exhaustive && n > 1 {
            let [l, r] = [&chans[0], &chans[1]];
            let side: Vec<i64> = l.iter().zip(r.iter()).map(|(a, b)| a - b).collect();
            let mid: Vec<i64> = l.iter().zip(r.iter()).map(|(a, b)| (a + b) >> 1).collect();
            let rl = best_subframe(l, bps, p);
            let rr = best_subframe(r, bps, p);
            let rs = best_subframe(&side, bps + 1, p);
            let rm = best_subframe(&mid, bps, p);
            let (bl, br, bsd, bm) = (rl.choice.bits, rr.choice.bits, rs.choice.bits, rm.choice.bits);
            let costs = vec![
                (ChannelAssignment::Independent(2), bl + br),
                (ChannelAssignment::LeftSide, bl + bsd),
                (ChannelAssignment::SideRight, bsd + br),
                (ChannelAssignment::MidSide, bm + bsd),
            ];
            let best = costs.iter().min_by_key(|c| c.1).unwrap().0;
            let (pair, res) = match best {
                ChannelAssignment::LeftSide => ([chans[0].clone(), side.clone()], [rl, rs]),
                ChannelAssignment::SideRight => ([side.clone(), chans[1].clone()], [rs, rr]),
                ChannelAssignment::MidSide => ([mid, side], [rm, rs]),
                _ => ([chans[0].clone(), chans[1].clone()], [rl, rr]),
            };
            (best, pair.to_vec(), res.into_iter().collect::<Vec<_>>(), costs)
        } else {
            let res: Vec<ChannelResult> = chans.iter().map(|c| best_subframe(c, bps, p)).collect();
            (ChannelAssignment::Independent(nch as u8), chans.clone(), res, vec![])
        };

        // Write the frame.
        let fpos = w.out.len();
        let header = FrameHeader {
            variable_block_size: false,
            block_size: n as u32,
            sample_rate,
            assignment,
            bits_per_sample,
            number: fi as u64,
        };
        frame::write(&mut w, &header, sample_rate, bits_per_sample);
        let body = w.out.len();
        sink.push(fpos as u64 * 8, body as u64 * 8 - 8, NO_BLOCK, SpanKind::FrameHeader { frame: fi as u32 });
        sink.push(body as u64 * 8 - 8, body as u64 * 8, NO_BLOCK, SpanKind::Crc8);
        let mut labels = vec![];
        for (ch, res) in results.into_iter().enumerate() {
            let id = (fi * nch + ch) as u32;
            let s0 = w.bit_pos();
            let x: Vec<i64> = coded[ch].iter().map(|&v| v >> res.wasted).collect();
            write_subframe(&mut w, &res.choice, &x, res.bps - res.wasted, res.wasted, id, &mut sink);
            labels.push(res.choice.label.clone());
            subframes.push(SubframeRecord {
                frame: fi as u32,
                channel: ch as u8,
                kind: res.choice.kind,
                order: res.choice.order as u8,
                wasted: res.wasted as u8,
                coded_bps: (res.bps - res.wasted) as u8,
                qlp: res.choice.qlp.clone(),
                qlp_precision: if res.choice.kind == SubframeKind::Lpc { res.choice.precision as u8 } else { 0 },
                shift: res.choice.shift as i8,
                rice: res.choice.plan.as_ref().map(|pl| RiceInfo {
                    rice2: pl.rice2,
                    order: pl.order as u8,
                    partitions: pl
                        .partitions
                        .iter()
                        .map(|q| PartitionInfo { k: q.k as u8, escaped: q.escaped, count: q.count as u32, bits: q.bits as u32 })
                        .collect(),
                }),
                bits: s0..w.bit_pos(),
            });
            all_candidates.push(res.candidates);
        }
        let pad = w.bit_pos();
        w.align();
        sink.push(pad, w.bit_pos(), NO_BLOCK, SpanKind::Padding);
        let crc_pos = w.out.len();
        let crc = crc16(&w.out[fpos..]);
        w.write(crc as u32, 16);
        sink.push(crc_pos as u64 * 8, crc_pos as u64 * 8 + 16, NO_BLOCK, SpanKind::Crc16);
        let end = w.out.len();
        let size = (end - fpos) as u32;
        min_frame = min_frame.min(size);
        max_frame = max_frame.max(size);
        segments.push(Segment {
            name: format!("Frame {fi}"),
            bytes: fpos..end,
            fields: vec![
                ("samples".into(), format!("{start}..{} ({n})", start + n)),
                ("channels".into(), assignment.label().into()),
                ("subframes".into(), labels.join(", ")),
            ],
        });
        frames.push(FrameRecord { header, first_sample: start as u64, bytes: fpos..end, header_bits: ((body - fpos) * 8) as u32, crc_ok: true });
        stereo_costs.push(costs);
    }

    if frames.is_empty() {
        min_frame = 0;
    }
    info.min_frame = min_frame;
    info.max_frame = max_frame;
    // A final short block is allowed below min_block; libFLAC reports the nominal size.
    let mut bytes = w.out;
    bytes[si_pos..si_pos + 34].copy_from_slice(&info.write());
    segments[1].fields = info.fields();
    Encoded {
        bytes,
        info,
        frames,
        subframes,
        candidates: all_candidates,
        stereo_costs,
        segments,
        spans: sink.spans,
        spans_truncated: sink.truncated,
    }
}

fn ilog2(v: usize) -> u32 {
    usize::BITS - 1 - v.max(1).leading_zeros()
}

/// Header bits of a subframe (type byte + wasted-bits unary).
fn header_bits(wasted: u32) -> u64 {
    8 + if wasted > 0 { wasted as u64 } else { 0 }
}

fn best_subframe(x: &[i64], bps: u32, p: &EncodeParams) -> ChannelResult {
    let n = x.len();
    // Wasted bits: common trailing zeros of every sample.
    let or = x.iter().fold(0i64, |a, &v| a | v);
    let wasted = if or == 0 { 0 } else { (or.trailing_zeros()).min(bps - 1) };
    let xs: Vec<i64> = if wasted > 0 { x.iter().map(|&v| v >> wasted).collect() } else { x.to_vec() };
    let bps_eff = bps - wasted;
    let hb = header_bits(wasted);
    let mut candidates = vec![];
    let push = |c: Choice, cands: &mut Vec<Candidate>, best: &mut Option<Choice>| {
        cands.push(Candidate { label: c.label.clone(), bits: c.bits });
        if best.as_ref().is_none_or(|b| c.bits < b.bits) {
            *best = Some(c);
        }
    };
    let mut best: Option<Choice> = None;

    // Constant.
    if xs.iter().all(|&v| v == xs[0]) {
        let c = Choice { kind: SubframeKind::Constant, order: 0, qlp: vec![], precision: 0, shift: 0, plan: None, residual: vec![], bits: hb + bps_eff as u64, label: "constant".into() };
        push(c, &mut candidates, &mut best);
        return ChannelResult { choice: best.unwrap(), wasted, bps, candidates };
    }

    // Verbatim.
    push(
        Choice { kind: SubframeKind::Verbatim, order: 0, qlp: vec![], precision: 0, shift: 0, plan: None, residual: vec![], bits: hb + n as u64 * bps_eff as u64, label: "verbatim".into() },
        &mut candidates,
        &mut best,
    );

    let fits_i32 = |r: &[i64]| r.iter().all(|&e| e >= i32::MIN as i64 && e <= i32::MAX as i64);

    // Fixed predictors 0..=4.
    for order in 0..=4.min(n.saturating_sub(1)) {
        let res = fixed::residual(&xs, order);
        if !fits_i32(&res) {
            continue;
        }
        let plan = rice::plan(&res, n, order, p.max_partition_order as u32, bps_eff, p.escape_coding);
        let bits = hb + (order as u64) * bps_eff as u64 + plan.bits;
        push(
            Choice { kind: SubframeKind::Fixed, order, qlp: vec![], precision: 0, shift: 0, plan: Some(plan), residual: res, bits, label: format!("fixed {order}") },
            &mut candidates,
            &mut best,
        );
    }

    // LPC.
    let max_order = (p.max_lpc_order as usize).min(lpc::MAX_ORDER).min(n.saturating_sub(1));
    if max_order > 0 {
        let xf: Vec<f64> = xs.iter().map(|&v| v as f64).collect();
        let base_precision = if p.qlp_precision > 0 { p.qlp_precision as u32 } else { lpc::default_precision(bps_eff, n) };
        for win in &p.windows {
            let wc = win.coefficients(n);
            let windowed: Vec<f64> = xf.iter().zip(&wc).map(|(a, b)| a * b).collect();
            let r = lpc::autocorrelation(&windowed, max_order);
            if r[0] <= 0.0 {
                continue;
            }
            let sets = lpc::levinson(&r, max_order);
            if sets.is_empty() {
                continue;
            }
            let orders: Vec<usize> = if p.exhaustive {
                (1..=sets.len()).collect()
            } else {
                // libFLAC's estimate: expected bits/sample from the prediction error.
                let overhead = (base_precision + bps_eff) as f64;
                let est = |o: usize| {
                    let err = sets[o - 1].1;
                    let bps_res = if err > 0.0 { (0.5 * (0.5 / n as f64 * err).log2()).max(0.0) } else { 0.0 };
                    bps_res * (n - o) as f64 + o as f64 * overhead
                };
                vec![(1..=sets.len()).min_by(|&a, &b| est(a).total_cmp(&est(b))).unwrap()]
            };
            for order in orders {
                // Keep predictions within 32 bits like libFLAC does for small sample depths.
                let mut precision = base_precision.clamp(5, lpc::MAX_QLP_PRECISION);
                if bps_eff <= 17 {
                    let cap = 32i32 - bps_eff as i32 - ilog2(order) as i32;
                    if cap >= 5 {
                        precision = precision.min(cap as u32);
                    }
                }
                let Some((qlp, shift)) = lpc::quantize(&sets[order - 1].0, precision) else { continue };
                let res = lpc::residual(&xs, &qlp, shift);
                if !fits_i32(&res) {
                    continue;
                }
                let plan = rice::plan(&res, n, order, p.max_partition_order as u32, bps_eff, p.escape_coding);
                let bits = hb + order as u64 * bps_eff as u64 + 4 + 5 + order as u64 * precision as u64 + plan.bits;
                push(
                    Choice {
                        kind: SubframeKind::Lpc,
                        order,
                        qlp,
                        precision,
                        shift,
                        plan: Some(plan),
                        residual: res,
                        bits,
                        label: format!("LPC {order} · {}", win.label()),
                    },
                    &mut candidates,
                    &mut best,
                );
            }
        }
    }
    ChannelResult { choice: best.unwrap(), wasted, bps, candidates }
}

fn write_subframe(w: &mut BitWriter, c: &Choice, x: &[i64], bps: u32, wasted: u32, id: u32, sink: &mut Sink) {
    let s0 = w.bit_pos();
    w.write(0, 1);
    let t = match c.kind {
        SubframeKind::Constant => 0,
        SubframeKind::Verbatim => 1,
        SubframeKind::Fixed => 8 + c.order as u32,
        SubframeKind::Lpc => 31 + c.order as u32,
    };
    w.write(t, 6);
    if wasted > 0 {
        w.write(1, 1);
        w.write_unary(wasted as u64 - 1);
    } else {
        w.write(0, 1);
    }
    sink.push(s0, w.bit_pos(), id, SpanKind::SubframeHeader { kind: c.kind, order: c.order as u8, wasted: wasted as u8 });
    match c.kind {
        SubframeKind::Constant => {
            let s = w.bit_pos();
            w.write_signed(x[0], bps);
            sink.push(s, w.bit_pos(), id, SpanKind::Constant);
        }
        SubframeKind::Verbatim => {
            let s = w.bit_pos();
            for &v in x {
                w.write_signed(v, bps);
            }
            sink.push(s, w.bit_pos(), id, SpanKind::Verbatim);
        }
        SubframeKind::Fixed | SubframeKind::Lpc => {
            let s = w.bit_pos();
            for &v in &x[..c.order] {
                w.write_signed(v, bps);
            }
            sink.push(s, w.bit_pos(), id, SpanKind::Warmup { n: c.order as u8 });
            if c.kind == SubframeKind::Lpc {
                let s = w.bit_pos();
                w.write(c.precision - 1, 4);
                w.write_signed(c.shift as i64, 5);
                for &q in &c.qlp {
                    w.write_signed(q as i64, c.precision);
                }
                sink.push(s, w.bit_pos(), id, SpanKind::LpcCoefs { order: c.order as u8, precision: c.precision as u8, shift: c.shift as i8 });
            }
            write_residual(w, c.plan.as_ref().unwrap(), &c.residual, id, sink);
        }
    }
}

fn write_residual(w: &mut BitWriter, plan: &RicePlan, res: &[i64], id: u32, sink: &mut Sink) {
    let s = w.bit_pos();
    w.write(plan.rice2 as u32, 2);
    w.write(plan.order, 4);
    sink.push(s, w.bit_pos(), id, SpanKind::ResidualHeader { rice2: plan.rice2, partition_order: plan.order as u8 });
    let (pbits, escape) = if plan.rice2 { (5, RICE2_ESCAPE) } else { (4, RICE_ESCAPE) };
    let mut i = 0;
    for (pi, part) in plan.partitions.iter().enumerate() {
        let s = w.bit_pos();
        let vals = &res[i..i + part.count];
        if part.escaped {
            w.write(escape, pbits);
            w.write(part.k, 5);
            for &e in vals {
                w.write_signed(e, part.k);
            }
        } else {
            w.write(part.k, pbits);
            for &e in vals {
                w.write_rice(e, part.k);
            }
        }
        sink.push(s, w.bit_pos(), id, SpanKind::RicePartition { partition: pi as u16, k: part.k as u8, escaped: part.escaped, count: part.count as u16 });
        i += part.count;
    }
}

/// Stereo-assignment helpers re-exported for traces.
pub fn assignment_channels(a: ChannelAssignment) -> [&'static str; 2] {
    a.coded_names()
}
