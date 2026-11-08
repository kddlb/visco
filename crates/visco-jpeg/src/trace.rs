//! Turns encoder/decoder internals into codec-agnostic `visco_core` stages.

use std::sync::Arc;

use visco_core::{
    BitSpan, Bitstream, BlockDetail, Media, RgbImage, BlockGrid, BlockId, BlockInfo, DetailGrid, Plane, PlaneKind, PlaneView,
    Ramp, Segment, Snapshot, Stage, StageView, Table, Trace,
};

use crate::{
    decoder::{ColorModel, Decoded, block_pixels},
    dct::fdct,
    encoder::{Encoded, block_samples},
    huffman::{HuffSpec, HuffTable},
    markers::{Frame, FrameKind},
    quant::dequantize,
    sampling::Plane8,
    zigzag::ZIGZAG,
};

const COMP_NAMES: [&str; 4] = ["Y", "Cb", "Cr", "K"];

/// Everything the block inspector needs about one component.
struct CompInfo {
    name: String,
    /// Samples the blocks were taken from (encoder input, or decoder output).
    plane: Plane8,
    qt: [u16; 64],
    blocks_w: usize,
    blocks_h: usize,
    base: usize,
    coefs: Arc<Vec<[i16; 64]>>,
}

pub struct JpegBlocks {
    comps: Vec<CompInfo>,
    /// Encoder traces know the exact (unquantized) DCT.
    encoder: bool,
}

impl JpegBlocks {
    fn locate(&self, id: BlockId) -> Option<(usize, usize, usize)> {
        let ci = self.comps.iter().rposition(|c| c.base <= id)?;
        let local = id - self.comps[ci].base;
        let c = &self.comps[ci];
        (local < c.blocks_w * c.blocks_h).then_some((ci, local % c.blocks_w, local / c.blocks_w))
    }
}

fn grid(title: &str, values: Vec<f32>, precision: usize, ramp: Ramp) -> DetailGrid {
    DetailGrid { title: title.into(), width: 8, values, precision, ramp }
}

impl BlockDetail for JpegBlocks {
    fn block_count(&self) -> usize {
        self.comps.last().map_or(0, |c| c.base + c.coefs.len())
    }

    fn block_info(&self, id: BlockId, bs: &Bitstream) -> BlockInfo {
        let Some((ci, bx, by)) = self.locate(id) else {
            return BlockInfo { title: format!("block {id}"), ..Default::default() };
        };
        let c = &self.comps[ci];
        let q = &c.coefs[by * c.blocks_w + bx];
        let samples: [f32; 64] = block_samples(&c.plane, bx, by).map(|v| v + 128.0);
        let recon = block_pixels(q, &c.qt);
        let deq = dequantize(q, &c.qt);
        let spans: Vec<usize> =
            bs.spans.iter().enumerate().filter(|(_, s)| s.block as usize == id).map(|(i, _)| i).collect();
        let bits: u64 = spans.iter().map(|&i| bs.spans[i].len as u64).sum();
        let nonzero = q.iter().filter(|&&v| v != 0).count();
        let padded = bx * 8 >= c.plane.width || by * 8 >= c.plane.height;

        let mut grids = vec![grid(
            if self.encoder { "Input samples" } else { "Decoded samples" },
            samples.to_vec(),
            0,
            Ramp::Gray,
        )];
        if self.encoder {
            let level_shifted = samples.map(|v| v - 128.0);
            grids.push(grid("Level shifted (−128)", level_shifted.to_vec(), 0, Ramp::Diverging));
            grids.push(grid("DCT coefficients", fdct(&level_shifted).to_vec(), 1, Ramp::Diverging));
        }
        grids.push(grid("Quantization table", c.qt.map(|v| v as f32).to_vec(), 0, Ramp::Heat));
        grids.push(grid("Quantized", q.map(|v| v as f32).to_vec(), 0, Ramp::Diverging));
        grids.push(grid("Zigzag sequence (row-major k)", ZIGZAG.map(|n| q[n] as f32).to_vec(), 0, Ramp::Diverging));
        grids.push(grid("Dequantized", deq.to_vec(), 0, Ramp::Diverging));
        grids.push(grid("Reconstructed (IDCT)", recon.map(|v| v as f32).to_vec(), 0, Ramp::Gray));
        if self.encoder {
            let err: Vec<f32> = (0..64).map(|i| recon[i] as f32 - samples[i]).collect();
            grids.push(grid("Error (reconstructed − input)", err, 0, Ramp::Diverging));
        }

        let mut summary = vec![
            ("component".into(), c.name.clone()),
            ("block".into(), format!("({bx}, {by}) of {}×{}", c.blocks_w, c.blocks_h)),
            ("pixel origin".into(), format!("({}, {}) in component plane", bx * 8, by * 8)),
            ("DC".into(), q[0].to_string()),
            ("non-zero coefficients".into(), format!("{nonzero} / 64")),
            ("coded bits".into(), if spans.is_empty() { "not recorded".into() } else { bits.to_string() }),
        ];
        if padded {
            summary.push(("note".into(), "padding block outside the visible image".into()));
        }
        BlockInfo { title: format!("{} block ({bx}, {by})", c.name), summary, grids, spans, ..Default::default() }
    }
}

fn gray(label: impl Into<String>, p: &Plane8) -> Plane {
    Plane { label: label.into(), width: p.width, height: p.height, kind: PlaneKind::Gray(p.data.clone().into()) }
}

fn rgb_plane(label: &str, w: usize, h: usize, rgb: &[u8]) -> Plane {
    Plane { label: label.into(), width: w, height: h, kind: PlaneKind::Rgb(rgb.to_vec().into()) }
}

fn comp_grid(c: &CompInfo) -> BlockGrid {
    let (cols, rows) = (c.plane.width.div_ceil(8), c.plane.height.div_ceil(8));
    let cells: Vec<BlockId> =
        (0..rows).flat_map(|r| (0..cols).map(move |col| (r, col))).map(|(r, col)| c.base + r * c.blocks_w + col).collect();
    BlockGrid { block_w: 8, block_h: 8, cols, rows, cells: cells.into() }
}

/// Per-pixel mosaic of each block's 8×8 coefficient pattern.
fn mosaic(c: &CompInfo, label: &str, f: impl Fn(&[i16; 64], &[u16; 64], usize) -> f32, ramp: Ramp) -> Plane {
    let (w, h) = (c.plane.width, c.plane.height);
    let mut data = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let q = &c.coefs[(y / 8) * c.blocks_w + x / 8];
            data[y * w + x] = f(q, &c.qt, (y % 8) * 8 + x % 8);
        }
    }
    let (lo, hi) = data.iter().fold((0f32, 0f32), |(a, b), &v| (a.min(v), b.max(v)));
    let range = match ramp {
        Ramp::Diverging => {
            let m = lo.abs().max(hi.abs()).max(1e-6);
            (-m, m)
        }
        _ => (lo.min(0.0), hi.max(1e-6)),
    };
    Plane { label: label.into(), width: w, height: h, kind: PlaneKind::Scalar { data: data.into(), range, ramp } }
}

/// Bits spent per block, painted over each block's area.
fn bits_map(c: &CompInfo, spans: &[BitSpan]) -> Plane {
    let mut per_block = vec![0f32; c.coefs.len()];
    for s in spans {
        let b = s.block as usize;
        if b >= c.base && b < c.base + c.coefs.len() {
            per_block[b - c.base] += s.len as f32;
        }
    }
    let (w, h) = (c.plane.width, c.plane.height);
    let data: Vec<f32> = (0..w * h).map(|i| per_block[(i / w / 8) * c.blocks_w + (i % w) / 8]).collect();
    let hi = data.iter().cloned().fold(1.0, f32::max);
    Plane {
        label: format!("{} bits/block (max {hi:.0})", c.name),
        width: w,
        height: h,
        kind: PlaneKind::Scalar { data: data.into(), range: (0.0, hi), ramp: Ramp::Heat },
    }
}

fn signed_log(v: f32) -> f32 {
    v.signum() * (1.0 + v.abs()).log2()
}

fn qtable_table(title: &str, t: &[u16; 64]) -> Table {
    Table {
        title: title.into(),
        columns: (0..8).map(|i| format!("u{i}")).collect(),
        rows: (0..8).map(|r| (0..8).map(|c| t[r * 8 + c].to_string()).collect()).collect(),
    }
}

fn huff_table(title: String, spec: &HuffSpec, ac: bool) -> Table {
    let t = HuffTable::new(spec.clone()).ok();
    let rows = spec
        .vals
        .iter()
        .map(|&sym| {
            let (code, len) = t.as_ref().map_or((0, 0), |t| t.ehuf[sym as usize]);
            let meaning = match (ac, sym) {
                (false, s) => format!("category {s}"),
                (true, 0x00) => "EOB".into(),
                (true, 0xF0) => "ZRL".into(),
                (true, s) if s & 15 == 0 => format!("EOB run 2^{}", s >> 4),
                (true, s) => format!("run {} size {}", s >> 4, s & 15),
            };
            vec![format!("{sym:#04x}"), meaning, len.to_string(), format!("{:0width$b}", code, width = len as usize)]
        })
        .collect();
    Table { title, columns: vec!["symbol".into(), "meaning".into(), "length".into(), "code".into()], rows }
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len().max(1) as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64.powi(2) / mse).log10() }
}

fn stage(id: &str, title: &str, description: &str, view: StageView) -> Stage {
    Stage { id: id.into(), title: title.into(), description: description.into(), view }
}

fn pv(plane: Plane, grid: Option<BlockGrid>) -> PlaneView {
    PlaneView { plane, grid }
}

/// Luma's block grid lines up with full-resolution planes when luma has the max sampling factors.
fn full_res_grid(frame: &Frame, comps: &[CompInfo]) -> Option<BlockGrid> {
    let c0 = frame.comps.first()?;
    (c0.h as usize == frame.hmax() && c0.v as usize == frame.vmax()).then(|| comp_grid(&comps[0]))
}

fn frame_metrics(frame: &Frame, bytes: usize) -> Vec<(String, String)> {
    let px = frame.width as usize * frame.height as usize;
    vec![
        ("size".into(), format!("{}×{}", frame.width, frame.height)),
        ("mode".into(), format!("{:?}", frame.kind)),
        ("sampling".into(), frame.subsampling_label()),
        ("file size".into(), format!("{bytes} bytes")),
        ("bits per pixel".into(), format!("{:.3}", bytes as f64 * 8.0 / px as f64)),
        ("compression ratio".into(), format!("{:.1}:1", (px * frame.comps.len().max(3)) as f64 / bytes as f64)),
    ]
}

fn truncation_note(truncated: bool) -> String {
    if truncated {
        format!(" Bit spans were recorded for the first {} units only.", crate::encoder::MAX_SPANS)
    } else {
        String::new()
    }
}

fn comp_name(frame: &Frame, i: usize, color: Option<ColorModel>) -> String {
    match color {
        Some(ColorModel::Gray) => "Y".into(),
        Some(ColorModel::Rgb) => ["R", "G", "B"].get(i).unwrap_or(&"?").to_string(),
        Some(ColorModel::Cmyk) => ["C", "M", "Y", "K"].get(i).unwrap_or(&"?").to_string(),
        _ => COMP_NAMES.get(i).map_or_else(|| format!("C{}", frame.comps[i].id), |s| s.to_string()),
    }
}

pub fn encode_trace(e: Encoded, input_rgb: &[u8], roundtrip: &Decoded) -> Trace {
    let (w, h) = (e.frame.width as usize, e.frame.height as usize);
    let comps: Vec<CompInfo> = e
        .comps
        .into_iter()
        .enumerate()
        .map(|(i, c)| CompInfo {
            name: comp_name(&e.frame, i, None),
            plane: c.plane,
            qt: c.qt,
            blocks_w: c.blocks_w,
            blocks_h: c.blocks_h,
            base: c.block_base,
            coefs: Arc::new(c.coefs),
        })
        .collect();
    let fgrid = full_res_grid(&e.frame, &comps);
    let gray_input = comps.len() == 1;
    let mut stages = vec![stage(
        "source",
        "Source image",
        "The RGB input, before any processing.",
        StageView::Planes(vec![pv(rgb_plane("RGB", w, h, input_rgb), fgrid.clone())]),
    )];

    stages.push(stage(
        "color",
        "Color conversion",
        if gray_input {
            "Grayscale mode keeps only luma: Y = 0.299 R + 0.587 G + 0.114 B."
        } else {
            "RGB becomes YCbCr (JFIF / BT.601 full range). Y carries brightness; Cb and Cr carry color \
             differences centered on 128, which the eye resolves less sharply."
        },
        StageView::Planes(
            e.full_planes.iter().enumerate().map(|(i, p)| pv(gray(COMP_NAMES[i], p), fgrid.clone())).collect(),
        ),
    ));

    if !gray_input {
        stages.push(stage(
            "subsample",
            "Chroma subsampling",
            &format!(
                "Chroma planes are box-averaged down to match the sampling factors ({}). Each plane is \
                 then cut into 8×8 blocks; partial blocks at the edges are padded by repeating the last row/column.",
                e.frame.subsampling_label()
            ),
            StageView::Planes(
                comps
                    .iter()
                    .map(|c| pv(gray(format!("{} {}×{}", c.name, c.plane.width, c.plane.height), &c.plane), Some(comp_grid(c))))
                    .collect(),
            ),
        ));
    }

    stages.push(stage(
        "dct",
        "Discrete cosine transform",
        "Each level-shifted 8×8 block is expressed as a weighted sum of 64 cosine patterns. The mosaic shows \
         each block's 64 coefficient magnitudes in place (log scale), recomputed from the dequantized values: \
         energy collects in the top-left (low frequencies) of every block. Click a block for the exact DCT.",
        StageView::Planes(
            comps
                .iter()
                .map(|c| {
                    pv(
                        mosaic(c, &format!("{} |DCT|", c.name), |q, t, i| (1.0 + (q[i] as f32 * t[i] as f32).abs()).log2(), Ramp::Heat),
                        Some(comp_grid(c)),
                    )
                })
                .collect(),
        ),
    ));

    let mut qtabs = vec![qtable_table("Luma (table 0)", &comps[0].qt)];
    if comps.len() > 1 {
        qtabs.push(qtable_table("Chroma (table 1)", &comps[1].qt));
    }
    stages.push(stage(
        "qtables",
        "Quantization tables",
        "Every coefficient is divided by its table entry and rounded. This is the only lossy step besides \
         subsampling; larger divisors at high frequencies throw away detail the eye barely notices.",
        StageView::Tables(qtabs),
    ));

    stages.push(stage(
        "quantize",
        "Quantized coefficients",
        "Quantized values per block (signed log scale: red positive, blue negative, black zero). Most high-frequency \
         coefficients become zero, which is what makes the next steps compress well.",
        StageView::Planes(
            comps
                .iter()
                .map(|c| pv(mosaic(c, &format!("{} quantized", c.name), |q, _, i| signed_log(q[i] as f32), Ramp::Diverging), Some(comp_grid(c))))
                .collect(),
        ),
    ));

    stages.push(stage(
        "entropy",
        "Entropy coding",
        &format!(
            "Coefficients are read in zigzag order. The DC value is coded as a difference from the previous block; AC \
             values become (zero-run, size) symbols plus extra bits, all Huffman coded with the standard Annex K tables. \
             The heat map shows how many bits each block cost. Hover the bitstream panel to see individual symbols.{}",
            truncation_note(e.spans_truncated)
        ),
        StageView::Planes(comps.iter().map(|c| pv(bits_map(c, &e.spans), Some(comp_grid(c)))).collect()),
    ));

    let ntab = if gray_input { 1 } else { 2 };
    let mut htabs = vec![];
    for t in 0..ntab {
        let which = if t == 0 { "luma" } else { "chroma" };
        let (dc, ac) = if t == 0 {
            (crate::huffman::std_dc_luma(), crate::huffman::std_ac_luma())
        } else {
            (crate::huffman::std_dc_chroma(), crate::huffman::std_ac_chroma())
        };
        htabs.push(huff_table(format!("DC {which}"), &dc, false));
        htabs.push(huff_table(format!("AC {which}"), &ac, true));
    }
    stages.push(stage("huffman", "Huffman tables", "Code assignments written in the DHT segments.", StageView::Tables(htabs)));

    let err: Vec<f32> = input_rgb
        .as_chunks::<3>().0.iter()
        .zip(roundtrip.rgb.as_chunks::<3>().0)
        .map(|(a, b)| (0..3).map(|k| (a[k] as f32 - b[k] as f32).abs()).fold(0.0, f32::max))
        .collect();
    let max_err = err.iter().cloned().fold(1.0, f32::max);
    let q = psnr(input_rgb, &roundtrip.rgb);
    stages.push(stage(
        "output",
        "Decoded result",
        &format!(
            "The encoded file decoded again by visco's decoder, beside the per-pixel error (max channel difference, \
             brightest = {max_err:.0}). PSNR {q:.2} dB."
        ),
        StageView::Planes(vec![
            pv(rgb_plane("decoded", w, h, &roundtrip.rgb), fgrid.clone()),
            pv(
                Plane {
                    label: "absolute error".into(),
                    width: w,
                    height: h,
                    kind: PlaneKind::Scalar { data: err.into(), range: (0.0, max_err), ramp: Ramp::Heat },
                },
                fgrid,
            ),
        ]),
    ));

    let mut metrics = frame_metrics(&e.frame, e.bytes.len());
    metrics.push(("PSNR".into(), format!("{q:.2} dB")));
    Trace {
        stages,
        bitstream: Bitstream { bytes: e.bytes.into(), segments: e.segments, spans: e.spans },
        blocks: Some(Arc::new(JpegBlocks { comps, encoder: true })),
        metrics,
        output: Some(Media::Image(RgbImage { width: w, height: h, data: roundtrip.rgb.clone().into() })),
    }
}

pub fn decode_trace(d: Decoded, bytes: Arc<[u8]>) -> Trace {
    let f = &d.frame;
    let (w, h) = (f.width as usize, f.height as usize);
    let comps: Vec<CompInfo> = d
        .comps
        .into_iter()
        .enumerate()
        .map(|(i, c)| CompInfo {
            name: comp_name(f, i, Some(d.color)),
            plane: c.plane,
            qt: c.qt,
            blocks_w: c.blocks_w,
            blocks_h: c.blocks_h,
            base: c.block_base,
            coefs: Arc::new(c.coefs),
        })
        .collect();
    let fgrid = full_res_grid(f, &comps);
    let mut stages = vec![];

    let mut summary = format!(
        "{:?} JPEG, {}×{}, {}, {} component(s), {} scan(s){}.\n\n",
        f.kind,
        w,
        h,
        f.subsampling_label(),
        f.comps.len(),
        d.scans.len(),
        if d.restart_interval > 0 { format!(", restart every {} MCUs", d.restart_interval) } else { String::new() }
    );
    for s in &d.segments {
        summary.push_str(&format!("{:>8}  {:<20} {} bytes\n", s.bytes.start, s.name, s.bytes.len()));
    }
    for wmsg in &d.warnings {
        summary.push_str(&format!("\nwarning: {wmsg}"));
    }
    stages.push(stage(
        "markers",
        "Marker segments",
        "A JPEG file is a sequence of marker segments. Click a byte in the bitstream panel to see a segment's fields.",
        StageView::Text(summary),
    ));

    let mut tabs: Vec<Table> = d.qtables.iter().map(|(id, t)| qtable_table(&format!("DQT table {id}"), t)).collect();
    for (class, id, spec) in &d.htables {
        tabs.push(huff_table(format!("{} table {id}", if *class == 0 { "DC" } else { "AC" }), spec, *class == 1));
    }
    stages.push(stage("tables", "Tables", "Quantization and Huffman tables defined in the file.", StageView::Tables(tabs)));

    if f.kind == FrameKind::Progressive {
        let ecs_segments: Vec<usize> =
            d.segments.iter().enumerate().filter(|(_, s)| s.name == "Entropy-coded data").map(|(i, _)| i).collect();
        let snaps = d
            .scans
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let snap = s.snapshot.as_ref()?;
                let names: Vec<String> = s.header.comps.iter().map(|c| comps[c.index].name.clone()).collect();
                let what = match (s.header.ss, s.header.ah) {
                    (0, 0) => "DC first pass".to_string(),
                    (0, _) => "DC refinement".to_string(),
                    (_, 0) => format!("AC {}..={} first pass", s.header.ss, s.header.se),
                    _ => format!("AC {}..={} refinement", s.header.ss, s.header.se),
                };
                Some(Snapshot {
                    label: format!("Scan {} of {}", i + 1, d.scans.len()),
                    detail: format!(
                        "{what} · {} · Ss={} Se={} Ah={} Al={} · {} bytes",
                        names.join("+"),
                        s.header.ss,
                        s.header.se,
                        s.header.ah,
                        s.header.al,
                        s.data.len()
                    ),
                    plane: rgb_plane("image after scan", w, h, snap),
                    segments: ecs_segments.get(i).copied().into_iter().collect(),
                })
            })
            .collect();
        stages.push(stage(
            "scans",
            "Progressive scans",
            "Progressive files send coefficients in several passes: first the DC (average) of every block, then bands \
             of AC coefficients, then refinement bits. Scrub through the scans to watch the image sharpen.",
            StageView::Sequence(snaps),
        ));
    }

    stages.push(stage(
        "entropy",
        "Entropy decoding",
        &format!(
            "Huffman decoding recovers each block's quantized coefficients (signed log scale). The bits map shows what \
             each block cost.{}",
            truncation_note(d.spans_truncated)
        ),
        StageView::Planes(
            comps
                .iter()
                .flat_map(|c| {
                    [
                        pv(mosaic(c, &format!("{} quantized", c.name), |q, _, i| signed_log(q[i] as f32), Ramp::Diverging), Some(comp_grid(c))),
                        pv(bits_map(c, &d.spans), Some(comp_grid(c))),
                    ]
                })
                .collect(),
        ),
    ));

    stages.push(stage(
        "dequantize",
        "Dequantization",
        "Each coefficient is multiplied back by its quantization table entry. Shown as |coefficient| per block position (log scale).",
        StageView::Planes(
            comps
                .iter()
                .map(|c| {
                    pv(
                        mosaic(c, &format!("{} |DCT|", c.name), |q, t, i| (1.0 + (q[i] as f32 * t[i] as f32).abs()).log2(), Ramp::Heat),
                        Some(comp_grid(c)),
                    )
                })
                .collect(),
        ),
    ));

    stages.push(stage(
        "idct",
        "Inverse DCT",
        "Each block's 64 coefficients are turned back into 8×8 samples, giving every component at its own resolution.",
        StageView::Planes(
            comps
                .iter()
                .map(|c| pv(gray(format!("{} {}×{}", c.name, c.plane.width, c.plane.height), &c.plane), Some(comp_grid(c))))
                .collect(),
        ),
    ));

    if comps.len() > 1 && comps.iter().any(|c| c.plane.width != w || c.plane.height != h) {
        stages.push(stage(
            "upsample",
            "Upsampling",
            "Subsampled components are stretched back to full size with a triangle (\"fancy\") filter.",
            StageView::Planes(
                d.upsampled.iter().enumerate().map(|(i, p)| pv(gray(comps[i].name.clone(), p), fgrid.clone())).collect(),
            ),
        ));
    }

    stages.push(stage(
        "output",
        "Output image",
        match d.color {
            ColorModel::YCbCr => "YCbCr converted back to RGB.",
            ColorModel::Gray => "Grayscale output.",
            ColorModel::Rgb => "The components are stored as RGB directly (Adobe transform 0).",
            ColorModel::Cmyk | ColorModel::Ycck => "Adobe CMYK/YCCK converted to RGB (approximate, no color management).",
        },
        StageView::Planes(vec![pv(rgb_plane("RGB", w, h, &d.rgb), fgrid)]),
    ));

    let mut metrics = frame_metrics(f, bytes.len());
    if !d.warnings.is_empty() {
        metrics.push(("warnings".into(), d.warnings.len().to_string()));
    }
    Trace {
        stages,
        bitstream: Bitstream { bytes, segments: d.segments, spans: d.spans },
        blocks: Some(Arc::new(JpegBlocks { comps, encoder: false })),
        metrics,
        output: Some(Media::Image(RgbImage { width: w, height: h, data: d.rgb.into() })),
    }
}

/// Marker segment lookup helper for UIs that want parsed fields by byte.
pub fn segment_for_byte(segments: &[Segment], byte: usize) -> Option<&Segment> {
    segments.iter().find(|s| s.bytes.contains(&byte))
}
