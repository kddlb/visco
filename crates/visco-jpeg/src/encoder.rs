//! Baseline sequential JPEG encoder with stage recording.

use crate::{
    bitio::BitWriter,
    color,
    dct::fdct,
    huffman::{self, HuffTable, category, extra_bits},
    markers::{Component, Frame, FrameKind, ScanComp, ScanHeader, SegWriter},
    quant::{self, quantize},
    sampling::{Plane8, Subsampling, downsample},
    zigzag::ZIGZAG,
};
use visco_core::{BitSpan, Segment, SpanKind};

/// Spans recorded per run are capped to keep memory bounded on huge images.
pub const MAX_SPANS: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq)]
pub struct EncodeParams {
    pub quality: u8,
    pub subsampling: Subsampling,
    /// MCUs between RST markers (0 = none).
    pub restart_interval: u16,
    pub grayscale: bool,
    /// Custom (luma, chroma) tables in natural order; overrides `quality`.
    pub qtables: Option<[[u16; 64]; 2]>,
}

impl Default for EncodeParams {
    fn default() -> Self {
        Self { quality: 75, subsampling: Subsampling::S420, restart_interval: 0, grayscale: false, qtables: None }
    }
}

/// One encoded component: its input samples and the quantized coefficients of every block.
pub struct EncodedComp {
    /// Samples at component resolution (after color conversion and subsampling).
    pub plane: Plane8,
    pub qt: [u16; 64],
    /// Allocated blocks (whole MCUs).
    pub blocks_w: usize,
    pub blocks_h: usize,
    /// Quantized coefficients, natural order, `by * blocks_w + bx`.
    pub coefs: Vec<[i16; 64]>,
    /// First global block id of this component.
    pub block_base: usize,
}

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub frame: Frame,
    /// Full-resolution Y, Cb, Cr before subsampling (only Y when grayscale).
    pub full_planes: Vec<Plane8>,
    pub comps: Vec<EncodedComp>,
    pub segments: Vec<Segment>,
    pub spans: Vec<BitSpan>,
    pub spans_truncated: bool,
}

/// Extract the 8×8 block at block coords (bx, by) with edge replication, level-shifted.
pub fn block_samples(p: &Plane8, bx: usize, by: usize) -> [f32; 64] {
    std::array::from_fn(|i| p.at_clamped(bx * 8 + i % 8, by * 8 + i / 8) as f32 - 128.0)
}

pub fn encode(rgb: &[u8], width: usize, height: usize, params: &EncodeParams, record: bool) -> Encoded {
    assert_eq!(rgb.len(), width * height * 3);
    assert!(width > 0 && height > 0 && width <= 65535 && height <= 65535);

    // 1. Color conversion
    let [y, cb, cr] = color::split_ycbcr(rgb);
    let mk = |d| Plane8::new(width, height, d);
    let full_planes = if params.grayscale { vec![mk(y)] } else { vec![mk(y), mk(cb), mk(cr)] };

    // 2. Frame geometry and subsampling
    let (hy, vy) = if params.grayscale { (1, 1) } else { params.subsampling.luma_factors() };
    let mut comps = vec![Component { id: 1, h: hy, v: vy, tq: 0 }];
    if !params.grayscale {
        comps.push(Component { id: 2, h: 1, v: 1, tq: 1 });
        comps.push(Component { id: 3, h: 1, v: 1, tq: 1 });
    }
    let frame = Frame { kind: FrameKind::Baseline, precision: 8, width: width as u16, height: height as u16, comps };
    let (hmax, vmax) = (frame.hmax(), frame.vmax());

    let qts = params.qtables.unwrap_or_else(|| {
        [quant::scaled(&quant::STD_LUMA, params.quality), quant::scaled(&quant::STD_CHROMA, params.quality)]
    });

    let mut base = 0;
    let mut ecomps: Vec<EncodedComp> = Vec::new();
    for (i, full) in full_planes.iter().enumerate() {
        let c = frame.comps[i];
        let plane = downsample(full, hmax / c.h as usize, vmax / c.v as usize);
        debug_assert_eq!((plane.width, plane.height), frame.comp_size(i));
        let (bw, bh) = frame.comp_alloc_blocks(i);
        ecomps.push(EncodedComp {
            plane,
            qt: qts[c.tq as usize],
            blocks_w: bw,
            blocks_h: bh,
            coefs: vec![[0; 64]; bw * bh],
            block_base: base,
        });
        base += bw * bh;
    }

    // 3. Headers
    let dc = [HuffTable::new(huffman::std_dc_luma()).unwrap(), HuffTable::new(huffman::std_dc_chroma()).unwrap()];
    let ac = [HuffTable::new(huffman::std_ac_luma()).unwrap(), HuffTable::new(huffman::std_ac_chroma()).unwrap()];
    let mut sw = SegWriter::default();
    sw.soi();
    sw.app0_jfif();
    sw.dqt(0, &qts[0]);
    if !params.grayscale {
        sw.dqt(1, &qts[1]);
    }
    sw.sof(&frame);
    let ntables = if params.grayscale { 1 } else { 2 };
    for t in 0..ntables {
        sw.dht(0, t as u8, &dc[t].spec);
        sw.dht(1, t as u8, &ac[t].spec);
    }
    if params.restart_interval > 0 {
        sw.dri(params.restart_interval);
    }
    let scan = ScanHeader {
        comps: (0..frame.comps.len())
            .map(|i| {
                let t = frame.comps[i].tq;
                ScanComp { index: i, td: t, ta: t }
            })
            .collect(),
        ss: 0,
        se: 63,
        ah: 0,
        al: 0,
    };
    sw.sos(&frame, &scan);

    // 4. Blocks: DCT, quantization, entropy coding
    let SegWriter { out, mut segments } = sw;
    let ecs_start = out.len();
    let mut w = BitWriter::new(out);
    let mut spans = Vec::new();
    let mut spans_truncated = false;
    let mut preds = vec![0i32; frame.comps.len()];
    let (mx, my) = frame.mcus();
    let mut rst = 0u8;
    let mut mcu_index = 0usize;
    for mcu_y in 0..my {
        for mcu_x in 0..mx {
            if params.restart_interval > 0 && mcu_index > 0 && mcu_index.is_multiple_of(params.restart_interval as usize) {
                w.restart(rst);
                rst = (rst + 1) & 7;
                preds.fill(0);
            }
            mcu_index += 1;
            for (ci, comp) in frame.comps.iter().enumerate() {
                let t = comp.tq as usize;
                for v in 0..comp.v as usize {
                    for h in 0..comp.h as usize {
                        let (bx, by) = (mcu_x * comp.h as usize + h, mcu_y * comp.v as usize + v);
                        let ec = &mut ecomps[ci];
                        let q = quantize(&fdct(&block_samples(&ec.plane, bx, by)), &ec.qt);
                        let q = clamp_coefs(q);
                        let id = ec.block_base + by * ec.blocks_w + bx;
                        ec.coefs[by * ec.blocks_w + bx] = q;
                        let rec = if record && spans.len() < MAX_SPANS {
                            Some(&mut spans)
                        } else {
                            spans_truncated |= record;
                            None
                        };
                        encode_block(&mut w, &q, &mut preds[ci], &dc[t], &ac[t], rec, id as u32);
                    }
                }
            }
        }
    }
    w.flush();
    let mut out = w.out;
    segments.push(Segment {
        name: "Entropy-coded data".into(),
        bytes: ecs_start..out.len(),
        fields: vec![
            ("bytes".into(), (out.len() - ecs_start).to_string()),
            ("MCUs".into(), format!("{mx}×{my}")),
        ],
    });
    let mut tail = SegWriter { out: std::mem::take(&mut out), segments };
    tail.eoi();

    Encoded {
        bytes: tail.out,
        frame,
        full_planes,
        comps: ecomps,
        segments: tail.segments,
        spans,
        spans_truncated,
    }
}

/// Baseline limits: DC diff fits category 11, AC values category 10.
fn clamp_coefs(mut q: [i16; 64]) -> [i16; 64] {
    q[0] = q[0].clamp(-1024, 1023);
    for v in &mut q[1..] {
        *v = (*v).clamp(-1023, 1023);
    }
    q
}

fn encode_block(
    w: &mut BitWriter,
    q: &[i16; 64],
    pred: &mut i32,
    dc: &HuffTable,
    ac: &HuffTable,
    mut spans: Option<&mut Vec<BitSpan>>,
    block: u32,
) {
    let mut push = |start: u64, end: u64, kind: SpanKind| {
        if let Some(s) = spans.as_deref_mut() {
            s.push(BitSpan { start, len: (end - start) as u16, block, kind });
        }
    };

    let diff = q[0] as i32 - *pred;
    *pred = q[0] as i32;
    let cat = category(diff);
    let start = w.bit_pos();
    dc.encode(w, cat);
    w.write(extra_bits(diff, cat), cat);
    push(start, w.bit_pos(), SpanKind::Dc { cat, diff: diff as i16 });

    let mut run = 0u8;
    for k in 1..64 {
        let v = q[ZIGZAG[k]] as i32;
        if v == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            let s = w.bit_pos();
            ac.encode(w, 0xF0);
            push(s, w.bit_pos(), SpanKind::Zrl);
            run -= 16;
        }
        let size = category(v);
        let s = w.bit_pos();
        ac.encode(w, (run << 4) | size);
        w.write(extra_bits(v, size), size);
        push(s, w.bit_pos(), SpanKind::Ac { k: k as u8, run, size, value: v as i16 });
        run = 0;
    }
    if run > 0 {
        let s = w.bit_pos();
        ac.encode(w, 0x00);
        push(s, w.bit_pos(), SpanKind::Eob { run: 1 });
    }
}
