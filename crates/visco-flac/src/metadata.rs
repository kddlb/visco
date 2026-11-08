//! METADATA_BLOCKs: STREAMINFO parsing/writing and human-readable fields for the rest.

use crate::md5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block: u16,
    pub max_block: u16,
    pub min_frame: u32,
    pub max_frame: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    /// 0 = unknown.
    pub total_samples: u64,
    pub md5: [u8; 16],
}

impl StreamInfo {
    pub fn parse(p: &[u8]) -> Option<Self> {
        if p.len() < 34 {
            return None;
        }
        let be = |a: &[u8]| a.iter().fold(0u64, |v, &b| (v << 8) | b as u64);
        let packed = be(&p[10..18]);
        Some(Self {
            min_block: be(&p[0..2]) as u16,
            max_block: be(&p[2..4]) as u16,
            min_frame: be(&p[4..7]) as u32,
            max_frame: be(&p[7..10]) as u32,
            sample_rate: (packed >> 44) as u32,
            channels: ((packed >> 41) & 7) as u8 + 1,
            bits_per_sample: ((packed >> 36) & 31) as u8 + 1,
            total_samples: packed & ((1 << 36) - 1),
            md5: p[18..34].try_into().unwrap(),
        })
    }

    pub fn write(&self) -> [u8; 34] {
        let mut o = [0u8; 34];
        o[0..2].copy_from_slice(&self.min_block.to_be_bytes());
        o[2..4].copy_from_slice(&self.max_block.to_be_bytes());
        o[4..7].copy_from_slice(&self.min_frame.to_be_bytes()[1..]);
        o[7..10].copy_from_slice(&self.max_frame.to_be_bytes()[1..]);
        let packed = ((self.sample_rate as u64) << 44)
            | (((self.channels - 1) as u64) << 41)
            | (((self.bits_per_sample - 1) as u64) << 36)
            | (self.total_samples & ((1 << 36) - 1));
        o[10..18].copy_from_slice(&packed.to_be_bytes());
        o[18..34].copy_from_slice(&self.md5);
        o
    }

    pub fn fields(&self) -> Vec<(String, String)> {
        vec![
            ("sample rate".into(), format!("{} Hz", self.sample_rate)),
            ("channels".into(), self.channels.to_string()),
            ("bits per sample".into(), self.bits_per_sample.to_string()),
            (
                "total samples".into(),
                if self.total_samples == 0 {
                    "unknown".into()
                } else {
                    format!("{} ({:.3} s)", self.total_samples, self.total_samples as f64 / self.sample_rate.max(1) as f64)
                },
            ),
            ("block size".into(), format!("{}..={}", self.min_block, self.max_block)),
            ("frame size".into(), format!("{}..={} bytes", self.min_frame, self.max_frame)),
            ("MD5".into(), if self.md5 == [0; 16] { "not set".into() } else { md5::hex(&self.md5) }),
        ]
    }
}

pub fn block_name(t: u8) -> String {
    match t {
        0 => "STREAMINFO".into(),
        1 => "PADDING".into(),
        2 => "APPLICATION".into(),
        3 => "SEEKTABLE".into(),
        4 => "VORBIS_COMMENT".into(),
        5 => "CUESHEET".into(),
        6 => "PICTURE".into(),
        127 => "invalid".into(),
        n => format!("reserved ({n})"),
    }
}

/// Parsed fields for display.
pub fn describe(t: u8, p: &[u8]) -> Vec<(String, String)> {
    let mut f = vec![("length".into(), format!("{} bytes", p.len()))];
    let le32 = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap()) as usize;
    match t {
        0 => {
            if let Some(si) = StreamInfo::parse(p) {
                f.extend(si.fields());
            }
        }
        2 if p.len() >= 4 => f.push(("application id".into(), String::from_utf8_lossy(&p[..4]).into())),
        3 => {
            let n = p.len() / 18;
            f.push(("seek points".into(), n.to_string()));
            for k in 0..n.min(8) {
                let e = &p[18 * k..18 * k + 18];
                let sample = u64::from_be_bytes(e[0..8].try_into().unwrap());
                let off = u64::from_be_bytes(e[8..16].try_into().unwrap());
                if sample != u64::MAX {
                    f.push((format!("point {k}"), format!("sample {sample} → byte +{off}")));
                }
            }
        }
        4 => {
            // little-endian lengths: vendor, count, then comments
            let mut i = 0;
            let mut take = |n: usize| -> Option<&[u8]> {
                let s = p.get(i..i + n)?;
                i += n;
                Some(s)
            };
            if let Some(vl) = take(4).map(le32)
                && let Some(v) = take(vl)
            {
                f.push(("vendor".into(), String::from_utf8_lossy(v).into()));
                if let Some(n) = take(4).map(le32) {
                    for _ in 0..n.min(32) {
                        let Some(l) = take(4).map(le32) else { break };
                        let Some(c) = take(l) else { break };
                        let c = String::from_utf8_lossy(c);
                        let (k, v) = c.split_once('=').unwrap_or((&c, ""));
                        f.push((k.to_string(), v.chars().take(120).collect()));
                    }
                }
            }
        }
        6 if p.len() >= 8 => {
            let be32 = |b: &[u8]| u32::from_be_bytes(b.try_into().unwrap()) as usize;
            let ml = be32(&p[4..8]);
            if let Some(mime) = p.get(8..8 + ml) {
                f.push(("picture type".into(), be32(&p[0..4]).to_string()));
                f.push(("MIME type".into(), String::from_utf8_lossy(mime).into()));
            }
        }
        _ => {}
    }
    f
}
