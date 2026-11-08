//! Turns FLAC encoder/decoder internals into codec-agnostic `visco_core` stages.

use std::{collections::BTreeMap, sync::Arc};

use visco_core::{
    Bitstream, BlockDetail, BlockId, BlockInfo, BlockStrip, DetailPlot, Lane, Media, Pcm, Plane,
    PlaneKind, Ramp, Series, SeriesRole, SignalView, Stage, StageView, SubframeKind, Table, Trace,
};

use crate::{
    decoder::{Decoded, FrameRecord, SubframeRecord, analyze, subframe_label},
    encoder::{Candidate, Encoded},
    md5,
    metadata::StreamInfo,
    stereo::{self, ChannelAssignment},
};

const KIND_LEGEND: [&str; 4] = ["constant", "verbatim", "fixed", "LPC"];
const STEREO_LEGEND: [&str; 4] = ["left/right", "left/side", "side/right", "mid/side"];

fn kind_index(k: SubframeKind) -> u8 {
    match k {
        SubframeKind::Constant => 0,
        SubframeKind::Verbatim => 1,
        SubframeKind::Fixed => 2,
        SubframeKind::Lpc => 3,
    }
}

fn stereo_index(a: ChannelAssignment) -> u8 {
    match a {
        ChannelAssignment::Independent(_) => 0,
        ChannelAssignment::LeftSide => 1,
        ChannelAssignment::SideRight => 2,
        ChannelAssignment::MidSide => 3,
    }
}

fn channel_name(nch: usize, ch: usize) -> String {
    match (nch, ch) {
        (1, _) => "mono".into(),
        (2, 0) => "left".into(),
        (2, 1) => "right".into(),
        _ => format!("channel {}", ch + 1),
    }
}

/// Inspector detail for one subframe.
pub struct FlacBlocks {
    info: StreamInfo,
    frames: Vec<FrameRecord>,
    subframes: Vec<SubframeRecord>,
    /// Output channels (exact).
    channels: Arc<Vec<Vec<i32>>>,
    candidates: Option<Vec<Vec<Candidate>>>,
    stereo_costs: Option<Vec<Vec<(ChannelAssignment, u64)>>>,
}

impl FlacBlocks {
    fn coded_frame(&self, f: usize, ch: usize) -> Vec<i64> {
        let fr = &self.frames[f];
        let (s, n) = (fr.first_sample as usize, fr.header.block_size as usize);
        let get = |c: usize| -> Vec<i64> { self.channels[c][s..s + n].iter().map(|&v| v as i64).collect() };
        match fr.header.assignment {
            ChannelAssignment::Independent(_) => get(ch),
            a => {
                let pair = stereo::encode(&get(0), &get(1), a);
                pair[ch].clone()
            }
        }
    }
}

fn ser(label: &str, role: SeriesRole, v: impl IntoIterator<Item = f32>) -> Series {
    Series { label: label.into(), role, data: v.into_iter().collect::<Vec<_>>().into() }
}

impl BlockDetail for FlacBlocks {
    fn block_count(&self) -> usize {
        self.subframes.len()
    }

    fn block_info(&self, id: BlockId, bs: &Bitstream) -> BlockInfo {
        let Some(rec) = self.subframes.get(id) else { return BlockInfo { title: format!("subframe {id}"), ..Default::default() } };
        let f = rec.frame as usize;
        let ch = rec.channel as usize;
        let fr = &self.frames[f];
        let n = fr.header.block_size as usize;
        let nch = self.info.channels as usize;
        let coded_name = if nch == 2 { fr.header.assignment.coded_names()[ch].to_string() } else { channel_name(nch, ch) };
        let x = self.coded_frame(f, ch);
        let (pred, res) = analyze(rec, &x);
        let bits = rec.bits.end - rec.bits.start;
        let verbatim_bits = n as u64 * (rec.coded_bps as u64 + rec.wasted as u64);
        let rate = self.info.sample_rate.max(1) as f64;
        let t0 = fr.first_sample as f64 / rate;

        let mut summary = vec![
            ("frame".into(), format!("{f} · {} · {} samples", fr.header.assignment.label(), n)),
            ("channel".into(), coded_name.clone()),
            ("time".into(), format!("{t0:.3} s – {:.3} s", t0 + n as f64 / rate)),
            ("predictor".into(), subframe_label(rec)),
            ("coded depth".into(), format!("{} bits{}", rec.coded_bps, if rec.wasted > 0 { format!(" (+{} wasted)", rec.wasted) } else { String::new() })),
            ("coded size".into(), format!("{bits} bits · {:.2} bits/sample", bits as f64 / n as f64)),
            ("vs verbatim".into(), format!("{:.1}% of {verbatim_bits} bits", bits as f64 * 100.0 / verbatim_bits.max(1) as f64)),
        ];
        if rec.kind == SubframeKind::Lpc {
            summary.push(("quantization".into(), format!("{}-bit coefficients, shift {}", rec.qlp_precision, rec.shift)));
        }
        if let Some(r) = &rec.rice {
            summary.push(("residual coding".into(), format!("{}, {} partition(s)", if r.rice2 { "RICE2" } else { "RICE" }, r.partitions.len())));
        }

        let mut plots = vec![DetailPlot {
            title: format!("{coded_name}: signal and prediction"),
            series: vec![
                ser("signal", SeriesRole::Primary, x.iter().map(|&v| v as f32)),
                ser("prediction", SeriesRole::Secondary, pred.iter().map(|&v| v as f32)),
            ],
            markers: if rec.order > 0 { vec![(rec.order as usize, "end of warm-up".into())] } else { vec![] },
        }];
        let mut markers = vec![];
        if let Some(r) = &rec.rice {
            let mut pos = rec.order as usize;
            for (i, p) in r.partitions.iter().enumerate() {
                markers.push((pos, if p.escaped { format!("#{i} raw {}", p.k) } else { format!("#{i} k={}", p.k) }));
                pos += p.count as usize;
            }
        }
        if !matches!(rec.kind, SubframeKind::Constant) {
            plots.push(DetailPlot {
                title: "residual (signal − prediction)".into(),
                series: vec![ser("residual", SeriesRole::Residual, res.iter().map(|&v| (v << rec.wasted) as f32))],
                markers,
            });
        }

        let mut tables = vec![];
        if rec.kind == SubframeKind::Lpc {
            tables.push(Table {
                title: "LPC coefficients".into(),
                columns: vec!["tap".into(), "quantized".into(), "value".into()],
                rows: rec
                    .qlp
                    .iter()
                    .enumerate()
                    .map(|(j, &q)| vec![format!("x[n−{}]", j + 1), q.to_string(), format!("{:.5}", q as f64 / (1u64 << rec.shift) as f64)])
                    .collect(),
            });
        }
        if let Some(r) = &rec.rice {
            tables.push(Table {
                title: format!("Rice partitions (order {})", r.order),
                columns: vec!["partition".into(), "samples".into(), "parameter".into(), "bits".into(), "bits/sample".into()],
                rows: r
                    .partitions
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        vec![
                            i.to_string(),
                            p.count.to_string(),
                            if p.escaped { format!("raw {}-bit", p.k) } else { format!("k = {}", p.k) },
                            p.bits.to_string(),
                            format!("{:.2}", p.bits as f64 / p.count.max(1) as f64),
                        ]
                    })
                    .collect(),
            });
        }
        if let Some(c) = self.candidates.as_ref().and_then(|c| c.get(id)) {
            let mut c = c.clone();
            c.sort_by_key(|c| c.bits);
            let best = c.first().map(|c| c.bits).unwrap_or(0);
            tables.push(Table {
                title: format!("Predictors tried ({})", c.len()),
                columns: vec!["candidate".into(), "bits".into(), "vs chosen".into()],
                rows: c
                    .iter()
                    .take(60)
                    .map(|c| {
                        vec![
                            c.label.clone(),
                            c.bits.to_string(),
                            if c.bits == best { "✓ chosen".into() } else { format!("+{}", c.bits - best) },
                        ]
                    })
                    .collect(),
            });
        }
        if let Some(costs) = self.stereo_costs.as_ref().and_then(|s| s.get(f)).filter(|c| !c.is_empty()) {
            let best = costs.iter().map(|c| c.1).min().unwrap_or(0);
            tables.push(Table {
                title: "Stereo modes for this frame".into(),
                columns: vec!["mode".into(), "bits".into(), "".into()],
                rows: costs
                    .iter()
                    .map(|(a, b)| vec![a.label().into(), b.to_string(), if *b == best { "✓".into() } else { format!("+{}", b - best) }])
                    .collect(),
            });
        }
        let spans: Vec<usize> = bs.spans.iter().enumerate().filter(|(_, s)| s.block as usize == id).map(|(i, _)| i).collect();
        BlockInfo { title: format!("Frame {f}, {coded_name}"), summary, grids: vec![], plots, tables, spans }
    }
}

// ---------------------------------------------------------------- signal helpers

struct Strips {
    /// Per channel: (range, id) pairs.
    ranges: Vec<Vec<(std::ops::Range<u64>, BlockId)>>,
    kinds: Vec<Vec<u8>>,
    bps: Vec<Vec<f32>>,
    stereo: Vec<u8>,
}

fn strips(frames: &[FrameRecord], subframes: &[SubframeRecord], nch: usize) -> Strips {
    let mut s = Strips { ranges: vec![vec![]; nch], kinds: vec![vec![]; nch], bps: vec![vec![]; nch], stereo: vec![] };
    for (fi, fr) in frames.iter().enumerate() {
        let r = fr.first_sample..fr.first_sample + fr.header.block_size as u64;
        s.stereo.push(stereo_index(fr.header.assignment));
        for ch in 0..nch {
            let id = fi * nch + ch;
            let sf = &subframes[id];
            s.ranges[ch].push((r.clone(), id));
            s.kinds[ch].push(kind_index(sf.kind));
            s.bps[ch].push((sf.bits.end - sf.bits.start) as f32 / fr.header.block_size as f32);
        }
    }
    s
}

fn strip(s: &Strips, ch: usize, categories: Option<(Vec<u8>, &[&str])>, heat: bool) -> BlockStrip {
    let (cats, legend) = match categories {
        Some((c, l)) => (Some(c.into()), l.iter().map(|s| s.to_string()).collect()),
        None => (None, vec![]),
    };
    BlockStrip {
        ranges: s.ranges[ch].clone().into(),
        categories: cats,
        legend,
        heat: heat.then(|| s.bps[ch].clone().into()),
    }
}

fn normalized(v: &[i32], full_scale: f32) -> Arc<[f32]> {
    v.iter().map(|&x| x as f32 / full_scale).collect()
}

fn stage(id: &str, title: &str, description: String, view: StageView) -> Stage {
    Stage { id: id.into(), title: title.into(), description, view }
}

/// Magnitude spectrogram (dB) of a normalized signal: time on x, frequency on y (low at bottom).
fn spectrogram(x: &[f32], label: &str) -> Option<Plane> {
    const N: usize = 512;
    if x.len() < N {
        return None;
    }
    let hop = (x.len() / 2048).max(N / 4);
    let cols = (x.len() - N) / hop + 1;
    let rows = N / 2;
    let win: Vec<f32> = (0..N).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / N as f32).cos()).collect();
    let mut data = vec![0f32; cols * rows];
    let mut re = vec![0f32; N];
    let mut im = vec![0f32; N];
    for c in 0..cols {
        for i in 0..N {
            re[i] = x[c * hop + i] * win[i];
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        for k in 0..rows {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() / (N as f32 / 4.0);
            let db = 20.0 * (mag + 1e-7).log10();
            data[(rows - 1 - k) * cols + c] = db.clamp(-100.0, 0.0);
        }
    }
    Some(Plane {
        label: label.into(),
        width: cols,
        height: rows,
        kind: PlaneKind::Scalar { data: data.into(), range: (-100.0, 0.0), ramp: Ramp::Heat },
    })
}

/// In-place iterative radix-2 FFT.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((ang * k as f32).cos(), (ang * k as f32).sin());
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * wr - im[b] * wi, re[b] * wi + im[b] * wr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

fn statistics(frames: &[FrameRecord], subframes: &[SubframeRecord]) -> Vec<Table> {
    let total_bits: u64 = subframes.iter().map(|s| s.bits.end - s.bits.start).sum::<u64>().max(1);
    let mut by_kind: BTreeMap<(u8, u8), (usize, u64)> = BTreeMap::new();
    let mut by_k: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_po: BTreeMap<u8, usize> = BTreeMap::new();
    for s in subframes {
        let e = by_kind.entry((kind_index(s.kind), s.order)).or_default();
        e.0 += 1;
        e.1 += s.bits.end - s.bits.start;
        if let Some(r) = &s.rice {
            *by_po.entry(r.order).or_default() += 1;
            for p in &r.partitions {
                *by_k.entry(if p.escaped { "escaped".into() } else { format!("k = {:>2}", p.k) }).or_default() += 1;
            }
        }
    }
    let n = subframes.len().max(1);
    let mut tables = vec![Table {
        title: "Predictor usage".into(),
        columns: vec!["predictor".into(), "subframes".into(), "share".into(), "bits".into(), "bit share".into()],
        rows: by_kind
            .iter()
            .map(|(&(k, o), &(c, b))| {
                let label = match k {
                    0 => "constant".to_string(),
                    1 => "verbatim".to_string(),
                    2 => format!("fixed order {o}"),
                    _ => format!("LPC order {o}"),
                };
                vec![label, c.to_string(), format!("{:.1}%", c as f64 * 100.0 / n as f64), b.to_string(), format!("{:.1}%", b as f64 * 100.0 / total_bits as f64)]
            })
            .collect(),
    }];
    let mut stereo: BTreeMap<&str, usize> = BTreeMap::new();
    for f in frames {
        *stereo.entry(f.header.assignment.label()).or_default() += 1;
    }
    tables.push(Table {
        title: "Channel modes".into(),
        columns: vec!["mode".into(), "frames".into(), "share".into()],
        rows: stereo.iter().map(|(m, c)| vec![m.to_string(), c.to_string(), format!("{:.1}%", *c as f64 * 100.0 / frames.len().max(1) as f64)]).collect(),
    });
    tables.push(Table {
        title: "Rice partition orders".into(),
        columns: vec!["order".into(), "subframes".into()],
        rows: by_po.iter().map(|(o, c)| vec![format!("{o} ({} partitions)", 1u32 << o), c.to_string()]).collect(),
    });
    tables.push(Table {
        title: "Rice parameters".into(),
        columns: vec!["parameter".into(), "partitions".into()],
        rows: by_k.iter().map(|(k, c)| vec![k.clone(), c.to_string()]).collect(),
    });
    tables
}

fn size_metrics(info: &StreamInfo, file_len: usize) -> Vec<(String, String)> {
    let raw = info.total_samples as f64 * info.channels as f64 * info.bits_per_sample as f64 / 8.0;
    vec![
        ("format".into(), format!("{} Hz · {} ch · {}-bit", info.sample_rate, info.channels, info.bits_per_sample)),
        ("duration".into(), format!("{:.2} s", info.total_samples as f64 / info.sample_rate.max(1) as f64)),
        ("file size".into(), format!("{file_len} bytes")),
        ("compression".into(), format!("{:.1}% of uncompressed PCM", file_len as f64 * 100.0 / raw.max(1.0))),
        (
            "bitrate".into(),
            format!("{:.0} kbit/s", file_len as f64 * 8.0 / (info.total_samples as f64 / info.sample_rate.max(1) as f64).max(1e-9) / 1000.0),
        ),
    ]
}

fn truncation_note(t: bool) -> &'static str {
    if t { " (bit spans were recorded for the first part of the file only)" } else { "" }
}

/// Common pipeline stages shared by encode and decode traces.
fn signal_stages(
    d: &Decoded,
    strips_: &Strips,
    out: &[Arc<[f32]>],
    stages: &mut Vec<Stage>,
    encoding: bool,
) {
    let info = &d.info;
    let nch = info.channels as usize;
    let len = d.channels[0].len() as u64;
    let rate = info.sample_rate;
    let coded_names = |ch: usize| -> String {
        if nch == 2 { ["left / mid / side", "right / side"][ch].to_string() } else { channel_name(nch, ch) }
    };
    let sig = |lanes: Vec<Lane>, spec: Option<Plane>| StageView::Signals(SignalView { sample_rate: rate, len, lanes, spectrogram: spec });

    if nch == 2 {
        let lanes = (0..2)
            .map(|ch| Lane {
                label: coded_names(ch),
                series: vec![Series { label: "coded".into(), role: SeriesRole::Primary, data: d.coded[ch].clone().into() }],
                blocks: Some(strip(strips_, ch, Some((strips_.stereo.clone(), &STEREO_LEGEND)), false)),
            })
            .collect();
        stages.push(stage(
            "stereo",
            if encoding { "Stereo decorrelation" } else { "Coded channels" },
            if encoding {
                "Left and right are usually similar, so FLAC can code their difference instead. For each frame the encoder \
                 coded four options — left/right, left/side, side/right and mid/side (mid = (L+R)/2, side = L−R) — and kept the \
                 smallest. The strip shows which mode won in each frame; click a frame to compare all four."
                    .into()
            } else {
                "The two channels as they are stored: for each frame, left/right or a mid/side-style pair. The strip shows the \
                 channel mode per frame."
                    .into()
            },
            sig(lanes, None),
        ));
    }

    let lanes = (0..nch)
        .map(|ch| {
            let coded: &[f32] = &d.coded[ch];
            let pred: Vec<f32> = coded.iter().zip(&d.residual[ch]).map(|(c, r)| c - r).collect();
            Lane {
                label: coded_names(ch),
                series: vec![
                    Series { label: "signal".into(), role: SeriesRole::Primary, data: d.coded[ch].clone().into() },
                    Series { label: "prediction".into(), role: SeriesRole::Secondary, data: pred.into() },
                ],
                blocks: Some(strip(strips_, ch, Some((strips_.kinds[ch].clone(), &KIND_LEGEND)), false)),
            }
        })
        .collect();
    stages.push(stage(
        "prediction",
        "Prediction",
        "Each subframe predicts every sample from the previous ones: a fixed polynomial (order 0–4) or a linear predictor \
         whose coefficients are computed per block (Levinson–Durbin on the windowed autocorrelation) and stored quantized. \
         Silent blocks become a single constant; unpredictable ones are stored verbatim. The strip colors each subframe by \
         predictor type. Click one to see its coefficients and every predictor the encoder tried."
            .into(),
        sig(lanes, None),
    ));

    let lanes = (0..nch)
        .map(|ch| Lane {
            label: coded_names(ch),
            series: vec![Series { label: "residual".into(), role: SeriesRole::Residual, data: d.residual[ch].clone().into() }],
            blocks: Some(strip(strips_, ch, None, true)),
        })
        .collect();
    stages.push(stage(
        "residual",
        "Residual and Rice coding",
        format!(
            "What the predictor got wrong (signal − prediction) is all that is stored besides the predictor itself. Residuals are \
             small and roughly Laplacian, so Rice codes fit them well: each value becomes a unary quotient plus k low bits. Blocks \
             are split into 2^order partitions with their own k. The heat strip shows bits per sample for each subframe.{}",
            truncation_note(d.spans_truncated)
        ),
        sig(lanes, None),
    ));

    let lanes = (0..nch)
        .map(|ch| Lane {
            label: channel_name(nch, ch),
            series: vec![Series { label: "output".into(), role: SeriesRole::Primary, data: out[ch].clone() }],
            blocks: Some(strip(strips_, ch, None, true)),
        })
        .collect();
    let md5_line = match d.md5_ok() {
        Some(true) => format!("MD5 {} matches STREAMINFO — the decode is bit-exact.", md5::hex(&d.md5)),
        Some(false) => format!("MD5 {} does NOT match STREAMINFO {}.", md5::hex(&d.md5), md5::hex(&d.info.md5)),
        None => "The file has no MD5 to check against.".into(),
    };
    stages.push(stage(
        "output",
        if encoding { "Decoded result" } else { "Output" },
        format!(
            "{}Prediction plus residual restores every sample exactly; stereo frames are converted back to left/right. {md5_line}",
            if encoding { "The encoded file decoded again by visco's decoder. " } else { "" }
        ),
        sig(lanes, spectrogram(&out[0], "spectrogram")),
    ));

    stages.push(stage(
        "stats",
        "Statistics",
        "How often each predictor, channel mode, partition order and Rice parameter was used, and where the bits went.".into(),
        StageView::Tables(statistics(&d.frames, &d.subframes)),
    ));
}

pub fn encode_trace(e: Encoded, input: &Pcm, rt: Decoded) -> Trace {
    let info = e.info.clone();
    let nch = info.channels as usize;
    let full_scale = (1u64 << (info.bits_per_sample - 1)) as f32;
    let len = rt.channels[0].len() as u64;
    let src: Vec<Vec<i32>> = (0..nch).map(|c| input.samples.iter().skip(c).step_by(nch).copied().collect()).collect();
    let src_norm: Vec<Arc<[f32]>> = src.iter().map(|c| normalized(c, full_scale)).collect();
    let out: Vec<Arc<[f32]>> = rt.channels.iter().map(|c| normalized(c, full_scale)).collect();
    let st = strips(&e.frames, &e.subframes, nch);
    let sig = |lanes, spec| StageView::Signals(SignalView { sample_rate: info.sample_rate, len, lanes, spectrogram: spec });

    let mut stages = vec![stage(
        "source",
        "Source audio",
        format!(
            "The PCM input: {} Hz, {} channel(s), {}-bit. The spectrogram shows its frequency content over time.",
            info.sample_rate, nch, info.bits_per_sample
        ),
        sig(
            (0..nch)
                .map(|ch| Lane { label: channel_name(nch, ch), series: vec![Series { label: "input".into(), role: SeriesRole::Primary, data: src_norm[ch].clone() }], blocks: None })
                .collect(),
            spectrogram(&src_norm[0], "spectrogram"),
        ),
    )];
    stages.push(stage(
        "framing",
        "Framing",
        format!(
            "The audio is cut into blocks of {} samples ({:.1} ms). Each block becomes a frame with one subframe per channel, \
             and every subframe is coded independently — so each can pick its own predictor.",
            info.max_block,
            info.max_block as f64 * 1000.0 / info.sample_rate.max(1) as f64
        ),
        sig(
            (0..nch)
                .map(|ch| Lane {
                    label: channel_name(nch, ch),
                    series: vec![Series { label: "input".into(), role: SeriesRole::Primary, data: src_norm[ch].clone() }],
                    blocks: Some(strip(&st, ch, None, false)),
                })
                .collect(),
            None,
        ),
    ));
    signal_stages(&rt, &st, &out, &mut stages, true);

    let exact = rt.channels == src;
    let mut metrics = size_metrics(&info, e.bytes.len());
    metrics.push(("lossless check".into(), if exact { "bit-exact ✓".into() } else { "MISMATCH".into() }));
    metrics.push(("MD5".into(), md5::hex(&info.md5)));
    let channels = Arc::new(rt.channels);
    Trace {
        stages,
        bitstream: Bitstream { bytes: e.bytes.into(), segments: e.segments, spans: e.spans },
        blocks: Some(Arc::new(FlacBlocks {
            info: info.clone(),
            frames: e.frames,
            subframes: e.subframes,
            channels: channels.clone(),
            candidates: Some(e.candidates),
            stereo_costs: Some(e.stereo_costs),
        })),
        metrics,
        output: Some(Media::Audio(pcm_from(&info, &channels))),
    }
}

pub fn decode_trace(d: Decoded, bytes: Arc<[u8]>) -> Trace {
    let info = d.info.clone();
    let nch = info.channels as usize;
    let full_scale = (1u64 << (info.bits_per_sample - 1)) as f32;
    let out: Vec<Arc<[f32]>> = d.channels.iter().map(|c| normalized(c, full_scale)).collect();
    let st = strips(&d.frames, &d.subframes, nch);

    let mut text = format!(
        "FLAC stream: {} Hz, {} channel(s), {}-bit, {} frames.\n\n",
        info.sample_rate,
        nch,
        info.bits_per_sample,
        d.frames.len()
    );
    for s in d.segments.iter().filter(|s| !s.name.starts_with("Frame ")) {
        text.push_str(&format!("{:>8}  {:<16} {} bytes\n", s.bytes.start, s.name, s.bytes.len()));
        for (k, v) in &s.fields {
            text.push_str(&format!("            {k}: {v}\n"));
        }
    }
    if let (Some(first), Some(last)) = (d.frames.first(), d.frames.last()) {
        text.push_str(&format!("{:>8}  {} audio frames up to byte {}\n", first.bytes.start, d.frames.len(), last.bytes.end));
    }
    for w in &d.warnings {
        text.push_str(&format!("\nwarning: {w}"));
    }
    let mut stages = vec![stage(
        "metadata",
        "Metadata blocks",
        "A FLAC file starts with \"fLaC\" and a chain of metadata blocks; STREAMINFO (always first) gives the format, the \
         block-size range and an MD5 of the decoded audio. Audio frames follow."
            .into(),
        StageView::Text(text),
    )];
    stages.push(stage(
        "frames",
        "Frames",
        "Each frame starts with a sync code and a header (block size, sample rate, channel mode, bit depth, frame number, \
         CRC-8) and ends with a CRC-16. The heat strip shows the bits per sample each subframe used."
            .into(),
        StageView::Signals(SignalView {
            sample_rate: info.sample_rate,
            len: d.channels[0].len() as u64,
            lanes: (0..nch)
                .map(|ch| Lane {
                    label: channel_name(nch, ch),
                    series: vec![Series { label: "output".into(), role: SeriesRole::Primary, data: out[ch].clone() }],
                    blocks: Some(strip(&st, ch, None, true)),
                })
                .collect(),
            spectrogram: None,
        }),
    ));
    signal_stages(&d, &st, &out, &mut stages, false);

    let mut metrics = size_metrics(&info, bytes.len());
    metrics.push((
        "MD5".into(),
        match d.md5_ok() {
            Some(true) => "matches ✓".into(),
            Some(false) => "MISMATCH".into(),
            None => "not stored".into(),
        },
    ));
    if !d.warnings.is_empty() {
        metrics.push(("warnings".into(), d.warnings.len().to_string()));
    }
    let channels = Arc::new(d.channels);
    Trace {
        stages,
        bitstream: Bitstream { bytes, segments: d.segments, spans: d.spans },
        blocks: Some(Arc::new(FlacBlocks {
            info: info.clone(),
            frames: d.frames,
            subframes: d.subframes,
            channels: channels.clone(),
            candidates: None,
            stereo_costs: None,
        })),
        metrics,
        output: Some(Media::Audio(pcm_from(&info, &channels))),
    }
}

fn pcm_from(info: &StreamInfo, ch: &[Vec<i32>]) -> Pcm {
    let n = ch.first().map_or(0, |c| c.len());
    let mut s = Vec::with_capacity(n * ch.len());
    for i in 0..n {
        for c in ch {
            s.push(c[i]);
        }
    }
    Pcm { sample_rate: info.sample_rate, bits_per_sample: info.bits_per_sample, channels: info.channels, samples: s.into() }
}
