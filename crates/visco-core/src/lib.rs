//! Codec-agnostic trace model shared by every codec and the UI.
//!
//! A codec run produces a [`Trace`]: an ordered list of [`Stage`]s, each carrying a generic
//! [`StageView`] payload the UI knows how to draw, plus the annotated [`Bitstream`].
//! Codec-specific detail (e.g. everything about one JPEG block) is reached through
//! [`BlockDetail`], so the UI never depends on a particular codec crate's internals.

use std::{ops::Range, sync::Arc};

pub mod params;
pub use params::{ParamSpec, ParamValue, Params};

/// Index into a codec's flat block table.
pub type BlockId = usize;

/// A single-channel or RGB raster.
#[derive(Clone, Debug)]
pub struct Plane {
    pub label: String,
    pub width: usize,
    pub height: usize,
    pub kind: PlaneKind,
}

#[derive(Clone, Debug)]
pub enum PlaneKind {
    /// 8-bit grayscale, displayed as-is.
    Gray(Arc<[u8]>),
    /// Interleaved RGB8.
    Rgb(Arc<[u8]>),
    /// Signed or real-valued data; the UI maps `range` to a color ramp.
    Scalar {
        data: Arc<[f32]>,
        range: (f32, f32),
        ramp: Ramp,
    },
}

/// How scalar data is colored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ramp {
    /// Black → white.
    Gray,
    /// Blue (negative) → black (zero) → red (positive).
    Diverging,
    /// Black → yellow heat.
    Heat,
}

/// A grid of addressable blocks overlaid on a plane (e.g. JPEG 8×8 blocks of one component).
#[derive(Clone, Debug)]
pub struct BlockGrid {
    /// Size of one block in the plane's pixels.
    pub block_w: usize,
    pub block_h: usize,
    /// Blocks per row / column of the grid.
    pub cols: usize,
    pub rows: usize,
    /// `cells[row * cols + col]` is the block shown at that position.
    pub cells: Arc<[BlockId]>,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub title: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// One frame in a sequence, e.g. the image after a progressive scan.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub label: String,
    pub detail: String,
    pub plane: Plane,
    /// Indices into `Bitstream::segments` covered by this snapshot (e.g. the scan's data).
    pub segments: Vec<usize>,
}

/// A plane plus an optional clickable block overlay.
#[derive(Clone, Debug)]
pub struct PlaneView {
    pub plane: Plane,
    pub grid: Option<BlockGrid>,
}

#[derive(Clone, Debug)]
pub enum StageView {
    /// One or more planes shown side by side.
    Planes(Vec<PlaneView>),
    Tables(Vec<Table>),
    Sequence(Vec<Snapshot>),
    /// Free text (e.g. marker summary).
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Stage {
    pub id: String,
    pub title: String,
    pub description: String,
    pub view: StageView,
}

/// A top-level region of the bitstream (JPEG marker segment, FLAC metadata block, frame...).
#[derive(Clone, Debug)]
pub struct Segment {
    pub name: String,
    pub bytes: Range<usize>,
    /// Human-readable parsed fields.
    pub fields: Vec<(String, String)>,
}

/// Sentinel for spans that belong to no block.
pub const NO_BLOCK: u32 = u32::MAX;

/// A run of bits that encodes one semantic unit. Kept compact: large images produce millions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BitSpan {
    /// Absolute bit offset into `Bitstream::bytes` (bit 0 = MSB of byte 0).
    pub start: u64,
    pub len: u16,
    /// Owning block id, or [`NO_BLOCK`].
    pub block: u32,
    pub kind: SpanKind,
}

impl BitSpan {
    pub fn end(&self) -> u64 {
        self.start + self.len as u64
    }
    pub fn contains(&self, bit: u64) -> bool {
        bit >= self.start && bit < self.end()
    }
    pub fn block_id(&self) -> Option<BlockId> {
        (self.block != NO_BLOCK).then_some(self.block as BlockId)
    }
}

/// What a span encodes. Variants are added as codecs need them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpanKind {
    /// DC difference: magnitude category + extra bits.
    Dc { cat: u8, diff: i16 },
    /// AC coefficient at zigzag index `k`, preceded by `run` zeros.
    Ac { k: u8, run: u8, size: u8, value: i16 },
    /// End of block (or of a band, with an EOB run length > 1 in progressive scans).
    Eob { run: u16 },
    /// Run of 16 zeros.
    Zrl,
    /// Progressive refinement bits for one block in one scan.
    Refine { scan: u8 },
    /// Byte-alignment padding before a marker.
    Padding,
}

impl SpanKind {
    pub fn describe(&self) -> String {
        match *self {
            Self::Dc { cat, diff } => format!("DC diff {diff:+} (cat {cat})"),
            Self::Ac { k, run, size, value } => {
                format!("AC[{k}] = {value:+} (run {run}, size {size})")
            }
            Self::Eob { run: 1 } => "EOB".into(),
            Self::Eob { run } => format!("EOB run {run}"),
            Self::Zrl => "ZRL (16 zeros)".into(),
            Self::Refine { scan } => format!("refinement bits (scan {scan})"),
            Self::Padding => "padding".into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Bitstream {
    pub bytes: Arc<[u8]>,
    pub segments: Vec<Segment>,
    /// Sorted by `start`.
    pub spans: Vec<BitSpan>,
}

impl Bitstream {
    /// Span covering the given bit, if any.
    pub fn span_at_bit(&self, bit: u64) -> Option<usize> {
        let i = self.spans.partition_point(|s| s.start <= bit);
        (i > 0 && self.spans[i - 1].contains(bit)).then(|| i - 1)
    }

    /// First span in the given byte.
    pub fn span_in_byte(&self, byte: usize) -> Option<usize> {
        let (lo, hi) = (byte as u64 * 8, byte as u64 * 8 + 8);
        let i = self.spans.partition_point(|s| s.end() <= lo);
        (i < self.spans.len() && self.spans[i].start < hi).then_some(i)
    }

    pub fn segment_at(&self, byte: usize) -> Option<usize> {
        self.segments.iter().position(|s| s.bytes.contains(&byte))
    }
}

/// A labelled 8×8 (or other) numeric grid shown in the block inspector.
#[derive(Clone, Debug)]
pub struct DetailGrid {
    pub title: String,
    pub width: usize,
    pub values: Vec<f32>,
    /// Display precision (0 = integer).
    pub precision: usize,
    pub ramp: Ramp,
}

#[derive(Clone, Debug)]
pub struct BlockInfo {
    pub title: String,
    pub summary: Vec<(String, String)>,
    pub grids: Vec<DetailGrid>,
    /// Indices into `Bitstream::spans` that belong to this block.
    pub spans: Vec<usize>,
}

/// Codec-specific block lookup for the inspector.
pub trait BlockDetail: Send + Sync {
    fn block_count(&self) -> usize;
    fn block_info(&self, id: BlockId, bitstream: &Bitstream) -> BlockInfo;
}

pub struct Trace {
    pub stages: Vec<Stage>,
    pub bitstream: Bitstream,
    pub blocks: Option<Arc<dyn BlockDetail>>,
    /// Summary metrics (e.g. size, PSNR).
    pub metrics: Vec<(String, String)>,
}

#[derive(Debug)]
pub struct CodecError(pub String);

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CodecError {}

/// Input for an encode: interleaved RGB8.
#[derive(Clone)]
pub struct RgbImage {
    pub width: usize,
    pub height: usize,
    pub data: Arc<[u8]>,
}

pub trait Codec: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// File extensions this codec decodes.
    fn extensions(&self) -> &'static [&'static str];
    fn params_schema(&self) -> Vec<ParamSpec>;
    fn encode(&self, input: &RgbImage, params: &Params) -> Result<Trace, CodecError>;
    fn decode(&self, bytes: Arc<[u8]>) -> Result<Trace, CodecError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs() -> Bitstream {
        Bitstream {
            bytes: Arc::from(vec![0u8; 4]),
            segments: vec![],
            spans: vec![
                BitSpan { start: 0, len: 5, block: NO_BLOCK, kind: SpanKind::Zrl },
                BitSpan { start: 5, len: 4, block: NO_BLOCK, kind: SpanKind::Zrl },
                BitSpan { start: 20, len: 4, block: NO_BLOCK, kind: SpanKind::Zrl },
            ],
        }
    }

    #[test]
    fn span_lookup() {
        let b = bs();
        assert_eq!(b.span_at_bit(0), Some(0));
        assert_eq!(b.span_at_bit(5), Some(1));
        assert_eq!(b.span_at_bit(9), None);
        assert_eq!(b.span_in_byte(0), Some(0));
        assert_eq!(b.span_in_byte(1), Some(1));
        assert_eq!(b.span_in_byte(2), Some(2));
        assert_eq!(b.span_in_byte(3), None);
    }
}
