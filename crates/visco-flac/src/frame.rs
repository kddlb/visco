//! Frame headers (RFC 9639 §9.1).

use crate::{
    bitio::{BitReader, BitWriter},
    crc::crc8,
    stereo::ChannelAssignment,
};

/// The 15-bit frame sync code (0b111111111111100).
const SYNC: u32 = 0x7FFC;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub variable_block_size: bool,
    pub block_size: u32,
    pub sample_rate: u32,
    pub assignment: ChannelAssignment,
    pub bits_per_sample: u8,
    /// Frame number (fixed blocking) or first sample number (variable blocking).
    pub number: u64,
}

impl FrameHeader {
    pub fn first_sample(&self, fixed_block_size: u32) -> u64 {
        if self.variable_block_size { self.number } else { self.number * fixed_block_size as u64 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("no frame sync")]
    NoSync,
    #[error("reserved value in frame header ({0})")]
    Reserved(&'static str),
    #[error("frame header CRC mismatch")]
    Crc,
    #[error("truncated frame header")]
    Eof,
}

impl From<crate::bitio::Eof> for HeaderError {
    fn from(_: crate::bitio::Eof) -> Self {
        Self::Eof
    }
}

/// Parse a header starting at byte `pos`. Returns the header and the byte offset just past CRC-8.
pub fn parse(data: &[u8], pos: usize, si_rate: u32, si_bps: u8) -> Result<(FrameHeader, usize), HeaderError> {
    let mut r = BitReader::at(data, pos);
    let sync = r.read(15)?;
    if sync != SYNC {
        return Err(HeaderError::NoSync);
    }
    let variable = r.bit()?;
    let bs_code = r.read(4)?;
    let sr_code = r.read(4)?;
    let ch_code = r.read(4)? as u8;
    let ss_code = r.read(3)?;
    if r.bit()? {
        return Err(HeaderError::Reserved("bit after sample size"));
    }
    let number = r.read_utf8()?.ok_or(HeaderError::Reserved("frame number coding"))?;
    let block_size = match bs_code {
        0 => return Err(HeaderError::Reserved("block size code 0")),
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => r.read(8)? + 1,
        7 => r.read(16)? + 1,
        _ => 256 << (bs_code - 8),
    };
    let sample_rate = match sr_code {
        0 => si_rate,
        1 => 88_200,
        2 => 176_400,
        3 => 192_000,
        4 => 8_000,
        5 => 16_000,
        6 => 22_050,
        7 => 24_000,
        8 => 32_000,
        9 => 44_100,
        10 => 48_000,
        11 => 96_000,
        12 => r.read(8)? * 1000,
        13 => r.read(16)?,
        14 => r.read(16)? * 10,
        _ => return Err(HeaderError::Reserved("sample rate code 15")),
    };
    let assignment = ChannelAssignment::from_code(ch_code).ok_or(HeaderError::Reserved("channel assignment"))?;
    let bits_per_sample = match ss_code {
        0 => si_bps,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        7 => 32,
        _ => return Err(HeaderError::Reserved("sample size code 3")),
    };
    debug_assert!(r.pos().is_multiple_of(8));
    let end = r.byte_pos();
    let crc = r.read(8)? as u8;
    if crc8(&data[pos..end]) != crc {
        return Err(HeaderError::Crc);
    }
    Ok((FrameHeader { variable_block_size: variable, block_size, sample_rate, assignment, bits_per_sample, number }, end + 1))
}

/// Write a header (including CRC-8). The writer must be byte-aligned.
pub fn write(w: &mut BitWriter, h: &FrameHeader, si_rate: u32, si_bps: u8) {
    assert!(w.is_aligned());
    let start = w.out.len();
    w.write(SYNC, 15);
    w.write(h.variable_block_size as u32, 1);
    let (bs_code, bs_extra) = match h.block_size {
        192 => (1, None),
        576 | 1152 | 2304 | 4608 => (2 + (h.block_size / 576).trailing_zeros(), None),
        256 | 512 | 1024 | 2048 | 4096 | 8192 | 16384 | 32768 => (8 + (h.block_size / 256).trailing_zeros(), None),
        n if n <= 256 => (6, Some((n - 1, 8))),
        n => (7, Some((n - 1, 16))),
    };
    // Like libFLAC, prefer explicit codes over "see STREAMINFO" (some decoders require them).
    let (sr_code, sr_extra) = {
        match h.sample_rate {
            88_200 => (1, None),
            176_400 => (2, None),
            192_000 => (3, None),
            8_000 => (4, None),
            16_000 => (5, None),
            22_050 => (6, None),
            24_000 => (7, None),
            32_000 => (8, None),
            44_100 => (9, None),
            48_000 => (10, None),
            96_000 => (11, None),
            r if r % 1000 == 0 && r / 1000 <= 255 => (12, Some((r / 1000, 8))),
            r if r <= 65535 => (13, Some((r, 16))),
            r if r % 10 == 0 && r / 10 <= 65535 => (14, Some((r / 10, 16))),
            r => {
                assert_eq!(r, si_rate, "sample rate {r} needs STREAMINFO");
                (0, None)
            }
        }
    };
    let ss_code = match h.bits_per_sample {
        8 => 1,
        12 => 2,
        16 => 4,
        20 => 5,
        24 => 6,
        32 => 7,
        b => {
            assert_eq!(b, si_bps, "{b}-bit frames need STREAMINFO");
            0
        }
    };
    w.write(bs_code, 4);
    w.write(sr_code, 4);
    w.write(h.assignment.code() as u32, 4);
    w.write(ss_code, 3);
    w.write(0, 1);
    w.write_utf8(h.number);
    if let Some((v, n)) = bs_extra {
        w.write(v, n);
    }
    if let Some((v, n)) = sr_extra {
        w.write(v, n);
    }
    let crc = crc8(&w.out[start..]);
    w.write(crc as u32, 8);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        for (bs, rate, a, bps, num) in [
            (4096, 44_100, ChannelAssignment::MidSide, 16, 0u64),
            (4608, 48_000, ChannelAssignment::Independent(1), 24, 1234),
            (1000, 44_100, ChannelAssignment::LeftSide, 16, 99_999),
            (37, 37_000, ChannelAssignment::SideRight, 20, 7),
            (65535, 12_345, ChannelAssignment::Independent(6), 32, 0x0FFF_FFFF),
        ] {
            let h = FrameHeader { variable_block_size: num > 1000, block_size: bs, sample_rate: rate, assignment: a, bits_per_sample: bps, number: num };
            let mut w = BitWriter::default();
            write(&mut w, &h, 44_100, 16);
            let (back, end) = parse(&w.out, 0, 44_100, 16).unwrap();
            assert_eq!(back, h);
            assert_eq!(end, w.out.len());
        }
    }
}
