//! Marker segments (T.81 Annex B): frame geometry, writing, and parsing.

use crate::{huffman::HuffSpec, zigzag::ZIGZAG};
use visco_core::Segment;

pub const SOI: u8 = 0xD8;
pub const EOI: u8 = 0xD9;
pub const SOF0: u8 = 0xC0;
pub const SOF1: u8 = 0xC1;
pub const SOF2: u8 = 0xC2;
pub const DHT: u8 = 0xC4;
pub const DQT: u8 = 0xDB;
pub const DRI: u8 = 0xDD;
pub const SOS: u8 = 0xDA;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Baseline,
    ExtendedSequential,
    Progressive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    pub id: u8,
    pub h: u8,
    pub v: u8,
    pub tq: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub precision: u8,
    pub width: u16,
    pub height: u16,
    pub comps: Vec<Component>,
}

impl Frame {
    pub fn hmax(&self) -> usize {
        self.comps.iter().map(|c| c.h as usize).max().unwrap_or(1)
    }
    pub fn vmax(&self) -> usize {
        self.comps.iter().map(|c| c.v as usize).max().unwrap_or(1)
    }
    /// MCUs per row / column for interleaved scans.
    pub fn mcus(&self) -> (usize, usize) {
        (
            (self.width as usize).div_ceil(8 * self.hmax()),
            (self.height as usize).div_ceil(8 * self.vmax()),
        )
    }
    /// Component sample dimensions (A.1.1).
    pub fn comp_size(&self, i: usize) -> (usize, usize) {
        let c = &self.comps[i];
        (
            (self.width as usize * c.h as usize).div_ceil(self.hmax()),
            (self.height as usize * c.v as usize).div_ceil(self.vmax()),
        )
    }
    /// Blocks allocated per component (covering whole MCUs).
    pub fn comp_alloc_blocks(&self, i: usize) -> (usize, usize) {
        let (mx, my) = self.mcus();
        (mx * self.comps[i].h as usize, my * self.comps[i].v as usize)
    }
    /// Blocks visited by a non-interleaved scan of component `i`.
    pub fn comp_scan_blocks(&self, i: usize) -> (usize, usize) {
        let (w, h) = self.comp_size(i);
        (w.div_ceil(8), h.div_ceil(8))
    }
    pub fn subsampling_label(&self) -> String {
        if self.comps.len() == 1 {
            return "grayscale".into();
        }
        let f: Vec<String> = self.comps.iter().map(|c| format!("{}x{}", c.h, c.v)).collect();
        let named = match (self.hmax(), self.vmax()) {
            _ if self.comps[1..].iter().any(|c| c.h != 1 || c.v != 1) => None,
            (1, 1) => Some("4:4:4"),
            (2, 1) => Some("4:2:2"),
            (2, 2) => Some("4:2:0"),
            (1, 2) => Some("4:4:0"),
            (4, 1) => Some("4:1:1"),
            _ => None,
        };
        match named {
            Some(n) => format!("{n} ({})", f.join(", ")),
            None => f.join(", "),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanComp {
    /// Index into `Frame::comps`.
    pub index: usize,
    pub td: u8,
    pub ta: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanHeader {
    pub comps: Vec<ScanComp>,
    pub ss: u8,
    pub se: u8,
    pub ah: u8,
    pub al: u8,
}

pub fn marker_name(m: u8) -> String {
    match m {
        SOI => "SOI".into(),
        EOI => "EOI".into(),
        SOF0 => "SOF0 (baseline)".into(),
        SOF1 => "SOF1 (extended)".into(),
        SOF2 => "SOF2 (progressive)".into(),
        0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => format!("SOF{}", m - 0xC0),
        DHT => "DHT".into(),
        0xCC => "DAC".into(),
        DQT => "DQT".into(),
        DRI => "DRI".into(),
        SOS => "SOS".into(),
        0xD0..=0xD7 => format!("RST{}", m - 0xD0),
        0xE0..=0xEF => format!("APP{}", m - 0xE0),
        0xFE => "COM".into(),
        _ => format!("marker {m:#04X}"),
    }
}

// ---------------------------------------------------------------- writing

/// Builds the header bytes and records each segment.
#[derive(Default)]
pub struct SegWriter {
    pub out: Vec<u8>,
    pub segments: Vec<Segment>,
}

impl SegWriter {
    fn seg(&mut self, marker: u8, payload: &[u8], fields: Vec<(String, String)>) {
        let start = self.out.len();
        self.out.extend_from_slice(&[0xFF, marker]);
        if !payload.is_empty() || !matches!(marker, SOI | EOI) {
            self.out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
            self.out.extend_from_slice(payload);
        }
        self.segments.push(Segment { name: marker_name(marker), bytes: start..self.out.len(), fields });
    }

    pub fn soi(&mut self) {
        self.seg(SOI, &[], vec![]);
    }

    pub fn eoi(&mut self) {
        self.seg(EOI, &[], vec![]);
    }

    pub fn app0_jfif(&mut self) {
        let p = [b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];
        self.seg(0xE0, &p, vec![("identifier".into(), "JFIF 1.01".into())]);
    }

    /// `table` in natural order; written in zigzag order.
    pub fn dqt(&mut self, id: u8, table: &[u16; 64]) {
        let mut p = vec![id];
        p.extend(ZIGZAG.iter().map(|&n| table[n] as u8));
        let fields = vec![("table".into(), id.to_string()), ("precision".into(), "8-bit".into())];
        self.seg(DQT, &p, fields);
    }

    pub fn sof(&mut self, f: &Frame) {
        let marker = match f.kind {
            FrameKind::Baseline => SOF0,
            FrameKind::ExtendedSequential => SOF1,
            FrameKind::Progressive => SOF2,
        };
        let mut p = vec![f.precision];
        p.extend_from_slice(&f.height.to_be_bytes());
        p.extend_from_slice(&f.width.to_be_bytes());
        p.push(f.comps.len() as u8);
        for c in &f.comps {
            p.extend_from_slice(&[c.id, (c.h << 4) | c.v, c.tq]);
        }
        self.seg(marker, &p, sof_fields(f));
    }

    pub fn dht(&mut self, class: u8, id: u8, spec: &HuffSpec) {
        let mut p = vec![(class << 4) | id];
        p.extend_from_slice(&spec.bits);
        p.extend_from_slice(&spec.vals);
        self.seg(DHT, &p, dht_fields(class, id, spec));
    }

    pub fn dri(&mut self, interval: u16) {
        self.seg(DRI, &interval.to_be_bytes(), vec![("interval".into(), format!("{interval} MCUs"))]);
    }

    pub fn sos(&mut self, f: &Frame, s: &ScanHeader) {
        let mut p = vec![s.comps.len() as u8];
        for c in &s.comps {
            p.extend_from_slice(&[f.comps[c.index].id, (c.td << 4) | c.ta]);
        }
        p.extend_from_slice(&[s.ss, s.se, (s.ah << 4) | s.al]);
        self.seg(SOS, &p, sos_fields(f, s));
    }
}

fn sof_fields(f: &Frame) -> Vec<(String, String)> {
    let mut v = vec![
        ("size".into(), format!("{}×{}", f.width, f.height)),
        ("precision".into(), format!("{} bits", f.precision)),
        ("sampling".into(), f.subsampling_label()),
    ];
    for c in &f.comps {
        v.push((format!("component {}", c.id), format!("{}×{}, quant table {}", c.h, c.v, c.tq)));
    }
    v
}

fn dht_fields(class: u8, id: u8, spec: &HuffSpec) -> Vec<(String, String)> {
    vec![
        ("table".into(), format!("{} {id}", if class == 0 { "DC" } else { "AC" })),
        ("symbols".into(), spec.vals.len().to_string()),
        ("lengths".into(), format!("{:?}", spec.bits)),
    ]
}

fn sos_fields(f: &Frame, s: &ScanHeader) -> Vec<(String, String)> {
    let comps: Vec<String> = s
        .comps
        .iter()
        .map(|c| format!("{} (DC {}, AC {})", f.comps[c.index].id, c.td, c.ta))
        .collect();
    vec![
        ("components".into(), comps.join(", ")),
        ("spectral".into(), format!("{}..={}", s.ss, s.se)),
        ("approximation".into(), format!("Ah={} Al={}", s.ah, s.al)),
    ]
}

// ---------------------------------------------------------------- parsing

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("not a JPEG (missing SOI)")]
    NoSoi,
    #[error("truncated {0} segment")]
    Truncated(String),
    #[error("{0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// A raw marker segment located in the file.
pub struct RawSegment<'a> {
    pub marker: u8,
    /// Byte offset of the 0xFF.
    pub start: usize,
    /// Offset just past the segment.
    pub end: usize,
    pub payload: &'a [u8],
}

/// Read the marker segment at `pos` (skipping fill bytes).
pub fn read_segment(file: &[u8], mut pos: usize) -> Result<RawSegment<'_>, ParseError> {
    while pos < file.len() && file[pos] != 0xFF {
        pos += 1; // tolerate garbage between segments
    }
    let start = pos;
    while pos < file.len() && file[pos] == 0xFF {
        pos += 1;
    }
    let marker = *file.get(pos).ok_or_else(|| ParseError::Truncated("marker".into()))?;
    pos += 1;
    if matches!(marker, SOI | EOI | 0x01 | 0xD0..=0xD7) {
        return Ok(RawSegment { marker, start, end: pos, payload: &[] });
    }
    if pos + 2 > file.len() {
        return Err(ParseError::Truncated(marker_name(marker)));
    }
    let len = u16::from_be_bytes([file[pos], file[pos + 1]]) as usize;
    if len < 2 || pos + len > file.len() {
        return Err(ParseError::Truncated(marker_name(marker)));
    }
    Ok(RawSegment { marker, start, end: pos + len, payload: &file[pos + 2..pos + len] })
}

/// Parse a DQT payload: (table id, natural-order table) pairs.
pub fn parse_dqt(p: &[u8]) -> Result<Vec<(u8, [u16; 64])>, ParseError> {
    let mut out = vec![];
    let mut i = 0;
    while i < p.len() {
        let (pq, tq) = (p[i] >> 4, p[i] & 15);
        i += 1;
        let n = if pq == 0 { 64 } else { 128 };
        if i + n > p.len() || tq > 3 {
            return Err(ParseError::Truncated("DQT".into()));
        }
        let mut t = [0u16; 64];
        for k in 0..64 {
            t[ZIGZAG[k]] = if pq == 0 {
                p[i + k] as u16
            } else {
                u16::from_be_bytes([p[i + 2 * k], p[i + 2 * k + 1]])
            };
        }
        if t.contains(&0) {
            return Err(ParseError::Invalid("zero quantizer in DQT".into()));
        }
        i += n;
        out.push((tq, t));
    }
    Ok(out)
}

/// Parse a DHT payload: (class, id, spec) triples.
pub fn parse_dht(p: &[u8]) -> Result<Vec<(u8, u8, HuffSpec)>, ParseError> {
    let mut out = vec![];
    let mut i = 0;
    while i < p.len() {
        if i + 17 > p.len() {
            return Err(ParseError::Truncated("DHT".into()));
        }
        let (tc, th) = (p[i] >> 4, p[i] & 15);
        let bits: [u8; 16] = p[i + 1..i + 17].try_into().unwrap();
        let n: usize = bits.iter().map(|&b| b as usize).sum();
        i += 17;
        if i + n > p.len() || tc > 1 || th > 3 {
            return Err(ParseError::Truncated("DHT".into()));
        }
        out.push((tc, th, HuffSpec { bits, vals: p[i..i + n].to_vec() }));
        i += n;
    }
    Ok(out)
}

pub fn parse_sof(marker: u8, p: &[u8]) -> Result<Frame, ParseError> {
    let kind = match marker {
        SOF0 => FrameKind::Baseline,
        SOF1 => FrameKind::ExtendedSequential,
        SOF2 => FrameKind::Progressive,
        0xC3 | 0xC7 | 0xCB | 0xCF => return Err(ParseError::Unsupported("lossless JPEG".into())),
        0xC9..=0xCB | 0xCD..=0xCF => {
            return Err(ParseError::Unsupported("arithmetic coding".into()));
        }
        _ => return Err(ParseError::Unsupported(marker_name(marker))),
    };
    if p.len() < 6 {
        return Err(ParseError::Truncated("SOF".into()));
    }
    let precision = p[0];
    if precision != 8 {
        return Err(ParseError::Unsupported(format!("{precision}-bit samples")));
    }
    let height = u16::from_be_bytes([p[1], p[2]]);
    let width = u16::from_be_bytes([p[3], p[4]]);
    let n = p[5] as usize;
    if p.len() < 6 + 3 * n || n == 0 || n > 4 {
        return Err(ParseError::Truncated("SOF".into()));
    }
    if height == 0 {
        return Err(ParseError::Unsupported("DNL (height defined after scan)".into()));
    }
    if width == 0 {
        return Err(ParseError::Invalid("zero width".into()));
    }
    let comps = (0..n)
        .map(|k| {
            let b = &p[6 + 3 * k..9 + 3 * k];
            Component { id: b[0], h: b[1] >> 4, v: b[1] & 15, tq: b[2] & 3 }
        })
        .collect::<Vec<_>>();
    if comps.iter().any(|c| !(1..=4).contains(&c.h) || !(1..=4).contains(&c.v)) {
        return Err(ParseError::Invalid("sampling factor out of range".into()));
    }
    Ok(Frame { kind, precision, width, height, comps })
}

pub fn parse_sos(p: &[u8], f: &Frame) -> Result<ScanHeader, ParseError> {
    let n = *p.first().ok_or_else(|| ParseError::Truncated("SOS".into()))? as usize;
    if p.len() < 1 + 2 * n + 3 || n == 0 || n > 4 {
        return Err(ParseError::Truncated("SOS".into()));
    }
    let mut comps = vec![];
    for k in 0..n {
        let (id, t) = (p[1 + 2 * k], p[2 + 2 * k]);
        let index = f
            .comps
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| ParseError::Invalid(format!("scan references unknown component {id}")))?;
        comps.push(ScanComp { index, td: t >> 4, ta: t & 15 });
    }
    let q = &p[1 + 2 * n..];
    Ok(ScanHeader { comps, ss: q[0], se: q[1], ah: q[2] >> 4, al: q[2] & 15 })
}

/// Human-readable fields for a parsed segment.
pub fn describe(marker: u8, payload: &[u8], frame: Option<&Frame>) -> Vec<(String, String)> {
    match marker {
        DQT => parse_dqt(payload)
            .map(|ts| {
                ts.iter()
                    .map(|(id, t)| (format!("table {id}"), format!("DC q={} … q63={}", t[0], t[63])))
                    .collect()
            })
            .unwrap_or_default(),
        DHT => parse_dht(payload)
            .map(|ts| ts.iter().flat_map(|(c, i, s)| dht_fields(*c, *i, s)).collect())
            .unwrap_or_default(),
        SOF0..=0xCF if marker != DHT && marker != 0xC8 && marker != 0xCC => {
            parse_sof(marker, payload).map(|f| sof_fields(&f)).unwrap_or_default()
        }
        SOS => frame
            .and_then(|f| parse_sos(payload, f).ok().map(|s| sos_fields(f, &s)))
            .unwrap_or_default(),
        DRI if payload.len() >= 2 => {
            vec![("interval".into(), format!("{} MCUs", u16::from_be_bytes([payload[0], payload[1]])))]
        }
        0xE0..=0xEF => {
            let id: String = payload.iter().take_while(|&&b| b != 0).take(16).map(|&b| b as char).collect();
            vec![("identifier".into(), id), ("length".into(), format!("{} bytes", payload.len()))]
        }
        0xFE => vec![("text".into(), String::from_utf8_lossy(payload).chars().take(120).collect())],
        _ => vec![],
    }
}
