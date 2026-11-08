//! Baseline, extended-sequential and progressive Huffman JPEG decoder with stage recording.

use std::ops::Range;

use crate::{
    bitio::{BitReader, Ecs},
    color,
    dct::idct,
    encoder::MAX_SPANS,
    huffman::{HuffTable, extend},
    markers::{self, FrameKind, ParseError, ScanHeader, read_segment},
    quant::dequantize,
    sampling::{Plane8, Upsample, upsample},
    zigzag::ZIGZAG,
};
pub use crate::markers::Frame;
use visco_core::{BitSpan, Segment, SpanKind};

#[derive(Clone, Copy, Debug)]
pub struct DecodeOptions {
    pub record_spans: bool,
    /// Reconstruct the image after every scan (progressive files only).
    pub snapshots: bool,
    pub upsample: Upsample,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self { record_spans: true, snapshots: true, upsample: Upsample::Fancy }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("{0}")]
    Other(String),
}

pub struct DecodedComp {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub tq: u8,
    /// Quantization table in effect when the component was first scanned (natural order).
    pub qt: [u16; 64],
    pub blocks_w: usize,
    pub blocks_h: usize,
    /// Quantized coefficients, natural order, `by * blocks_w + bx`.
    pub coefs: Vec<[i16; 64]>,
    pub block_base: usize,
    /// Reconstructed samples at component resolution.
    pub plane: Plane8,
}

pub struct ScanRecord {
    pub header: ScanHeader,
    /// File byte range of the entropy-coded data.
    pub data: Range<usize>,
    /// Image after this scan (RGB8), when snapshots are enabled.
    pub snapshot: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorModel {
    Gray,
    YCbCr,
    Rgb,
    Cmyk,
    Ycck,
}

pub struct Decoded {
    pub frame: Frame,
    pub comps: Vec<DecodedComp>,
    pub scans: Vec<ScanRecord>,
    pub segments: Vec<Segment>,
    pub spans: Vec<BitSpan>,
    pub spans_truncated: bool,
    /// Every DQT table seen: (id, natural-order table).
    pub qtables: Vec<(u8, [u16; 64])>,
    /// Every DHT table seen: (class, id, spec).
    pub htables: Vec<(u8, u8, crate::huffman::HuffSpec)>,
    pub restart_interval: u16,
    pub color: ColorModel,
    /// Full-resolution component planes after upsampling.
    pub upsampled: Vec<Plane8>,
    /// Final RGB8.
    pub rgb: Vec<u8>,
    pub warnings: Vec<String>,
}

struct Sink {
    on: bool,
    spans: Vec<BitSpan>,
    truncated: bool,
}

impl Sink {
    #[inline]
    fn push(&mut self, ecs: &Ecs, start: u64, end: u64, block: u32, kind: SpanKind) {
        if !self.on || end <= start {
            return;
        }
        if self.spans.len() >= MAX_SPANS {
            self.truncated = true;
            return;
        }
        // Map the last bit, not the end: the next data byte may sit after an RST marker.
        let (s, e) = (ecs.file_bit(start), ecs.file_bit(end - 1) + 1);
        self.spans.push(BitSpan { start: s, len: (e - s).min(u16::MAX as u64) as u16, block, kind });
    }
}

struct Tables {
    dc: [Option<HuffTable>; 4],
    ac: [Option<HuffTable>; 4],
}

pub fn decode(file: &[u8], opts: DecodeOptions) -> Result<Decoded, DecodeError> {
    let first = read_segment(file, 0)?;
    if first.marker != markers::SOI || first.start != 0 {
        return Err(ParseError::NoSoi.into());
    }
    let mut segments = vec![Segment { name: "SOI".into(), bytes: 0..first.end, fields: vec![] }];
    let mut pos = first.end;
    let mut frame: Option<Frame> = None;
    let mut qt: [Option<[u16; 64]>; 4] = [None; 4];
    let mut tables = Tables { dc: Default::default(), ac: Default::default() };
    let mut qtables = vec![];
    let mut htables = vec![];
    let mut ri = 0u16;
    let mut comps: Vec<DecodedComp> = vec![];
    let mut scans = vec![];
    let mut warnings = vec![];
    let mut adobe_transform: Option<u8> = None;
    let mut sink = Sink { on: opts.record_spans, spans: vec![], truncated: false };
    let mut saw_eoi = false;

    while pos < file.len() {
        let seg = match read_segment(file, pos) {
            Ok(s) => s,
            Err(e) => {
                warnings.push(format!("stopped at byte {pos}: {e}"));
                break;
            }
        };
        segments.push(Segment {
            name: markers::marker_name(seg.marker),
            bytes: seg.start..seg.end,
            fields: markers::describe(seg.marker, seg.payload, frame.as_ref()),
        });
        pos = seg.end;
        match seg.marker {
            markers::DQT => {
                for (id, t) in markers::parse_dqt(seg.payload)? {
                    qt[id as usize] = Some(t);
                    qtables.push((id, t));
                }
            }
            markers::DHT => {
                for (class, id, spec) in markers::parse_dht(seg.payload)? {
                    let t = HuffTable::new(spec.clone()).map_err(DecodeError::Other)?;
                    if class == 0 {
                        tables.dc[id as usize] = Some(t);
                    } else {
                        tables.ac[id as usize] = Some(t);
                    }
                    htables.push((class, id, spec));
                }
            }
            markers::DRI => {
                if seg.payload.len() < 2 {
                    return Err(ParseError::Truncated("DRI".into()).into());
                }
                ri = u16::from_be_bytes([seg.payload[0], seg.payload[1]]);
            }
            0xEE if seg.payload.len() >= 12 && seg.payload.starts_with(b"Adobe") => {
                adobe_transform = Some(seg.payload[11]);
            }
            m @ (markers::SOF0..=0xCF) if !matches!(m, markers::DHT | 0xC8 | 0xCC) => {
                if frame.is_some() {
                    return Err(DecodeError::Other("multiple frames (hierarchical JPEG)".into()));
                }
                let f = markers::parse_sof(m, seg.payload)?;
                let mut base = 0;
                for (i, c) in f.comps.iter().enumerate() {
                    let (bw, bh) = f.comp_alloc_blocks(i);
                    let (w, h) = f.comp_size(i);
                    comps.push(DecodedComp {
                        id: c.id,
                        h: c.h,
                        v: c.v,
                        tq: c.tq,
                        qt: [0; 64],
                        blocks_w: bw,
                        blocks_h: bh,
                        coefs: vec![[0; 64]; bw * bh],
                        block_base: base,
                        plane: Plane8::new(w, h, vec![0; w * h]),
                    });
                    base += bw * bh;
                }
                frame = Some(f);
            }
            markers::SOS => {
                let f = frame.as_ref().ok_or_else(|| DecodeError::Other("SOS before SOF".into()))?;
                let header = markers::parse_sos(seg.payload, f)?;
                validate_scan(f, &header)?;
                for sc in &header.comps {
                    let c = &mut comps[sc.index];
                    if c.qt == [0; 64] {
                        c.qt = qt[c.tq as usize].ok_or_else(|| {
                            DecodeError::Other(format!("component {} uses undefined quant table {}", c.id, c.tq))
                        })?;
                    }
                }
                let ecs = Ecs::extract(file, seg.end);
                let scan_no = scans.len() as u8;
                if let Err(e) =
                    decode_scan(f, &mut comps, &tables, &header, &ecs, ri, scan_no, &mut sink)
                {
                    warnings.push(format!("scan {}: {e}; remainder of scan skipped", scans.len() + 1));
                }
                segments.push(Segment {
                    name: "Entropy-coded data".into(),
                    bytes: seg.end..ecs.end,
                    fields: vec![
                        ("bytes".into(), (ecs.end - seg.end).to_string()),
                        ("restart markers".into(), ecs.restarts.len().to_string()),
                    ],
                });
                let snapshot = (opts.snapshots && f.kind == FrameKind::Progressive)
                    .then(|| reconstruct(f, &comps, opts.upsample, color_model(f, adobe_transform)).2);
                scans.push(ScanRecord { header, data: seg.end..ecs.end, snapshot });
                pos = ecs.end;
            }
            markers::EOI => {
                saw_eoi = true;
                break;
            }
            _ => {}
        }
    }

    let frame = frame.ok_or_else(|| DecodeError::Other("no frame header (SOF)".into()))?;
    if scans.is_empty() {
        return Err(DecodeError::Other("no scans".into()));
    }
    if !saw_eoi {
        warnings.push("missing EOI marker (truncated file?)".into());
    }
    let color = color_model(&frame, adobe_transform);
    let (planes, upsampled, rgb) = reconstruct(&frame, &comps, opts.upsample, color);
    for (c, p) in comps.iter_mut().zip(planes) {
        c.plane = p;
    }
    Ok(Decoded {
        frame,
        comps,
        scans,
        segments,
        spans: sink.spans,
        spans_truncated: sink.truncated,
        qtables,
        htables,
        restart_interval: ri,
        color,
        upsampled,
        rgb,
        warnings,
    })
}

fn validate_scan(f: &Frame, s: &ScanHeader) -> Result<(), DecodeError> {
    let bad = |m: &str| Err(DecodeError::Other(format!("invalid scan: {m}")));
    if f.kind == FrameKind::Progressive {
        if s.ss > s.se || s.se > 63 {
            return bad("spectral range");
        }
        if s.ss == 0 && s.se != 0 {
            return bad("DC scan must have Se = 0");
        }
        if s.ss > 0 && s.comps.len() != 1 {
            return bad("AC scans must contain one component");
        }
        if s.ah > 13 || s.al > 13 {
            return bad("successive approximation");
        }
    } else if s.ss != 0 || s.se != 63 || s.ah != 0 || s.al != 0 {
        // Tolerate non-standard values from some encoders; sequential decoding ignores them.
    }
    let blocks: usize = s.comps.iter().map(|c| (f.comps[c.index].h * f.comps[c.index].v) as usize).sum();
    if s.comps.len() > 1 && blocks > 10 {
        return bad("more than 10 blocks per MCU");
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Mode {
    Sequential,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    f: &Frame,
    comps: &mut [DecodedComp],
    t: &Tables,
    s: &ScanHeader,
    ecs: &Ecs,
    ri: u16,
    scan_no: u8,
    sink: &mut Sink,
) -> Result<(), String> {
    let mode = match (f.kind == FrameKind::Progressive, s.ss, s.ah) {
        (false, _, _) => Mode::Sequential,
        (true, 0, 0) => Mode::DcFirst,
        (true, 0, _) => Mode::DcRefine,
        (true, _, 0) => Mode::AcFirst,
        (true, _, _) => Mode::AcRefine,
    };
    // Resolve tables up front.
    let need_dc = matches!(mode, Mode::Sequential | Mode::DcFirst);
    let need_ac = matches!(mode, Mode::Sequential | Mode::AcFirst | Mode::AcRefine);
    let mut dcs = vec![];
    let mut acs = vec![];
    for c in &s.comps {
        let dc = t.dc[c.td as usize & 3].as_ref();
        let ac = t.ac[c.ta as usize & 3].as_ref();
        if need_dc && dc.is_none() {
            return Err(format!("missing DC table {}", c.td));
        }
        if need_ac && ac.is_none() {
            return Err(format!("missing AC table {}", c.ta));
        }
        dcs.push(dc);
        acs.push(ac);
    }

    let single = s.comps.len() == 1;
    let (ux, uy) = if single { f.comp_scan_blocks(s.comps[0].index) } else { f.mcus() };
    let mut r = BitReader::new(&ecs.data);
    let mut preds = [0i32; 4];
    let mut eobrun = 0u32;
    let mut rst_idx = 0;

    for u in 0..ux * uy {
        if ri > 0 && u > 0 && u % ri as usize == 0 {
            match ecs.restarts.get(rst_idx) {
                Some(&b) => r.seek_byte(b),
                None => return Err("expected restart marker".into()),
            }
            rst_idx += 1;
            preds = [0; 4];
            eobrun = 0;
        }
        if r.past_end() {
            return Err("entropy-coded data ended early".into());
        }
        let (mx, my) = (u % ux, u / ux);
        for (k, sc) in s.comps.iter().enumerate() {
            let c = &mut comps[sc.index];
            let (nh, nv) = if single { (1, 1) } else { (c.h as usize, c.v as usize) };
            for v in 0..nv {
                for h in 0..nh {
                    let (bx, by) = if single { (mx, my) } else { (mx * nh + h, my * nv + v) };
                    let idx = by * c.blocks_w + bx;
                    let id = (c.block_base + idx) as u32;
                    let coef = &mut c.coefs[idx];
                    let start = r.pos();
                    match mode {
                        Mode::Sequential => {
                            decode_dc(&mut r, ecs, dcs[k].unwrap(), &mut preds[k], coef, 0, sink, id)?;
                            decode_ac_first(&mut r, ecs, acs[k].unwrap(), coef, 1, 63, 0, &mut eobrun, sink, id)?;
                        }
                        Mode::DcFirst => {
                            decode_dc(&mut r, ecs, dcs[k].unwrap(), &mut preds[k], coef, s.al, sink, id)?
                        }
                        Mode::DcRefine => {
                            if r.bit() {
                                coef[0] |= 1 << s.al;
                            }
                            sink.push(ecs, start, r.pos(), id, SpanKind::Refine { scan: scan_no });
                        }
                        Mode::AcFirst => decode_ac_first(
                            &mut r,
                            ecs,
                            acs[k].unwrap(),
                            coef,
                            s.ss as usize,
                            s.se as usize,
                            s.al,
                            &mut eobrun,
                            sink,
                            id,
                        )?,
                        Mode::AcRefine => {
                            decode_ac_refine(&mut r, acs[k].unwrap(), coef, s, &mut eobrun)?;
                            sink.push(ecs, start, r.pos(), id, SpanKind::Refine { scan: scan_no });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn decode_dc(
    r: &mut BitReader,
    ecs: &Ecs,
    dc: &HuffTable,
    pred: &mut i32,
    coef: &mut [i16; 64],
    al: u8,
    sink: &mut Sink,
    id: u32,
) -> Result<(), String> {
    let s0 = r.pos();
    let (cat, _) = dc.decode(r)?;
    if cat > 11 {
        return Err(format!("DC category {cat} > 11"));
    }
    let diff = extend(r.read(cat), cat);
    *pred += diff;
    coef[0] = (*pred << al) as i16;
    sink.push(ecs, s0, r.pos(), id, SpanKind::Dc { cat, diff: diff as i16 });
    Ok(())
}

/// Sequential AC decoding (with `eobrun` pinned to 0) and progressive AC first scans (G.1.2.2).
#[allow(clippy::too_many_arguments)]
#[inline]
fn decode_ac_first(
    r: &mut BitReader,
    ecs: &Ecs,
    ac: &HuffTable,
    coef: &mut [i16; 64],
    ss: usize,
    se: usize,
    al: u8,
    eobrun: &mut u32,
    sink: &mut Sink,
    id: u32,
) -> Result<(), String> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Ok(());
    }
    let mut k = ss;
    while k <= se {
        let s0 = r.pos();
        let (rs, _) = ac.decode(r)?;
        let (run, size) = (rs >> 4, rs & 15);
        if size == 0 {
            if run == 15 {
                sink.push(ecs, s0, r.pos(), id, SpanKind::Zrl);
                k += 16;
                continue;
            }
            // EOB / EOBn: this block plus (2^run - 1 + extra) more are done.
            let total = (1u32 << run) + r.read(run);
            *eobrun = total - 1;
            sink.push(ecs, s0, r.pos(), id, SpanKind::Eob { run: total.min(u16::MAX as u32) as u16 });
            break;
        }
        k += run as usize;
        if k > 63 {
            return Err("AC coefficient index past 63".into());
        }
        let v = extend(r.read(size), size);
        coef[ZIGZAG[k]] = (v << al) as i16;
        sink.push(ecs, s0, r.pos(), id, SpanKind::Ac { k: k as u8, run, size, value: v as i16 });
        k += 1;
    }
    Ok(())
}

/// Progressive AC refinement (G.1.2.3), following libjpeg's `decode_mcu_AC_refine`.
fn decode_ac_refine(
    r: &mut BitReader,
    ac: &HuffTable,
    coef: &mut [i16; 64],
    s: &ScanHeader,
    eobrun: &mut u32,
) -> Result<(), String> {
    let p1: i32 = 1 << s.al;
    let m1: i32 = -1 << s.al;
    let se = s.se as usize;
    let mut k = s.ss as usize;
    let refine = |r: &mut BitReader, c: &mut i16| {
        if r.bit() && (*c as i32 & p1) == 0 {
            *c = (*c as i32 + if *c >= 0 { p1 } else { m1 }) as i16;
        }
    };

    if *eobrun == 0 {
        while k <= se {
            let (rs, _) = ac.decode(r)?;
            let mut run = (rs >> 4) as i32;
            let size = rs & 15;
            let mut value = 0;
            if size != 0 {
                if size != 1 {
                    return Err("refinement coefficient size must be 1".into());
                }
                value = if r.bit() { p1 } else { m1 };
            } else if run != 15 {
                *eobrun = 1 << run;
                if run > 0 {
                    *eobrun += r.read(run as u8);
                }
                break;
            }
            while k <= se {
                let c = &mut coef[ZIGZAG[k]];
                if *c != 0 {
                    refine(r, c);
                } else {
                    run -= 1;
                    if run < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if value != 0 && k <= se {
                coef[ZIGZAG[k]] = value as i16;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        while k <= se {
            let c = &mut coef[ZIGZAG[k]];
            if *c != 0 {
                refine(r, c);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Ok(())
}

fn color_model(f: &Frame, adobe: Option<u8>) -> ColorModel {
    match f.comps.len() {
        1 => ColorModel::Gray,
        3 => {
            let ids: Vec<u8> = f.comps.iter().map(|c| c.id).collect();
            if adobe == Some(0) || ids == b"RGB" { ColorModel::Rgb } else { ColorModel::YCbCr }
        }
        _ => {
            if adobe == Some(2) { ColorModel::Ycck } else { ColorModel::Cmyk }
        }
    }
}

/// IDCT one block of quantized coefficients into 8-bit samples.
pub fn block_pixels(q: &[i16; 64], qt: &[u16; 64]) -> [u8; 64] {
    let s = idct(&dequantize(q, qt));
    std::array::from_fn(|i| color::clamp_u8(s[i] + 128.0))
}

/// Dequantize + IDCT every component, upsample, and color convert.
/// Returns (component planes, full-resolution planes, RGB8).
pub fn reconstruct(
    f: &Frame,
    comps: &[DecodedComp],
    mode: Upsample,
    color: ColorModel,
) -> (Vec<Plane8>, Vec<Plane8>, Vec<u8>) {
    let (w, h) = (f.width as usize, f.height as usize);
    let (hmax, vmax) = (f.hmax(), f.vmax());
    let mut planes = Vec::with_capacity(comps.len());
    let mut full = Vec::with_capacity(comps.len());
    for (i, c) in comps.iter().enumerate() {
        let (cw, ch) = f.comp_size(i);
        let mut data = vec![0u8; cw * ch];
        for by in 0..ch.div_ceil(8) {
            for bx in 0..cw.div_ceil(8) {
                let px = block_pixels(&c.coefs[by * c.blocks_w + bx], &c.qt);
                for y in 0..8.min(ch - by * 8) {
                    let row = (by * 8 + y) * cw + bx * 8;
                    let n = 8.min(cw - bx * 8);
                    data[row..row + n].copy_from_slice(&px[y * 8..y * 8 + n]);
                }
            }
        }
        let p = Plane8::new(cw, ch, data);
        let (hn, vn) = (c.h as usize, c.v as usize);
        let up = if hmax % hn == 0 && vmax % vn == 0 {
            upsample(&p, hmax / hn, vmax / vn, w, h, mode)
        } else {
            // Non-integer ratio: nearest neighbor by proportional mapping.
            let d = (0..w * h).map(|j| p.at_clamped((j % w) * hn / hmax, (j / w) * vn / vmax)).collect();
            Plane8::new(w, h, d)
        };
        planes.push(p);
        full.push(up);
    }
    let rgb = to_rgb(&full, color, w * h);
    (planes, full, rgb)
}

fn to_rgb(p: &[Plane8], color: ColorModel, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 3);
    match color {
        ColorModel::Gray => {
            for &v in &p[0].data {
                out.extend_from_slice(&[v, v, v]);
            }
        }
        ColorModel::YCbCr => return color::merge_ycbcr(&p[0].data, &p[1].data, &p[2].data),
        ColorModel::Rgb => {
            for i in 0..n {
                out.extend_from_slice(&[p[0].data[i], p[1].data[i], p[2].data[i]]);
            }
        }
        ColorModel::Cmyk | ColorModel::Ycck => {
            // Adobe stores inverted CMYK; YCCK is YCbCr-coded inverted CMY plus K.
            for i in 0..n {
                let k = p[3].data[i] as u32;
                let (c, m, y) = if color == ColorModel::Ycck {
                    let (r, g, b) =
                        color::ycbcr_to_rgb(p[0].data[i] as f32, p[1].data[i] as f32, p[2].data[i] as f32);
                    (255 - r as u32, 255 - g as u32, 255 - b as u32)
                } else {
                    (p[0].data[i] as u32, p[1].data[i] as u32, p[2].data[i] as u32)
                };
                out.extend_from_slice(&[(c * k / 255) as u8, (m * k / 255) as u8, (y * k / 255) as u8]);
            }
        }
    }
    out
}
