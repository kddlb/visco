//! Inter-channel decorrelation for stereo frames.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelAssignment {
    /// 1–8 independent channels.
    Independent(u8),
    LeftSide,
    SideRight,
    MidSide,
}

impl ChannelAssignment {
    pub fn from_code(c: u8) -> Option<Self> {
        match c {
            0..=7 => Some(Self::Independent(c + 1)),
            8 => Some(Self::LeftSide),
            9 => Some(Self::SideRight),
            10 => Some(Self::MidSide),
            _ => None,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            Self::Independent(n) => n - 1,
            Self::LeftSide => 8,
            Self::SideRight => 9,
            Self::MidSide => 10,
        }
    }

    pub fn channels(self) -> usize {
        match self {
            Self::Independent(n) => n as usize,
            _ => 2,
        }
    }

    /// Which coded channel carries the side signal (needs one extra bit).
    pub fn side_channel(self) -> Option<usize> {
        match self {
            Self::Independent(_) => None,
            Self::LeftSide | Self::MidSide => Some(1),
            Self::SideRight => Some(0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Independent(1) => "mono",
            Self::Independent(2) => "left/right",
            Self::Independent(_) => "independent",
            Self::LeftSide => "left/side",
            Self::SideRight => "side/right",
            Self::MidSide => "mid/side",
        }
    }

    pub fn coded_names(self) -> [&'static str; 2] {
        match self {
            Self::Independent(_) => ["left", "right"],
            Self::LeftSide => ["left", "side"],
            Self::SideRight => ["side", "right"],
            Self::MidSide => ["mid", "side"],
        }
    }
}

/// Coded channel pair for a stereo assignment.
pub fn encode(l: &[i64], r: &[i64], a: ChannelAssignment) -> [Vec<i64>; 2] {
    let side = || l.iter().zip(r).map(|(a, b)| a - b).collect::<Vec<_>>();
    match a {
        ChannelAssignment::Independent(_) => [l.to_vec(), r.to_vec()],
        ChannelAssignment::LeftSide => [l.to_vec(), side()],
        ChannelAssignment::SideRight => [side(), r.to_vec()],
        ChannelAssignment::MidSide => [l.iter().zip(r).map(|(a, b)| (a + b) >> 1).collect(), side()],
    }
}

/// Undo decorrelation in place, turning coded channels back into left/right.
pub fn decode(c0: &mut [i64], c1: &mut [i64], a: ChannelAssignment) {
    match a {
        ChannelAssignment::Independent(_) => {}
        ChannelAssignment::LeftSide => {
            for (l, s) in c0.iter().zip(c1.iter_mut()) {
                *s = l.wrapping_sub(*s);
            }
        }
        ChannelAssignment::SideRight => {
            for (s, r) in c0.iter_mut().zip(c1.iter()) {
                *s = s.wrapping_add(*r);
            }
        }
        ChannelAssignment::MidSide => {
            for (m, s) in c0.iter_mut().zip(c1.iter_mut()) {
                let mid = m.wrapping_shl(1) | (*s & 1);
                let side = *s;
                *m = mid.wrapping_add(side) >> 1;
                *s = mid.wrapping_sub(side) >> 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_modes_roundtrip() {
        let l: Vec<i64> = vec![0, 1, -1, 32767, -32768, 12345, -7, 3];
        let r: Vec<i64> = vec![0, -1, -1, -32768, 32767, 12344, 5, 3];
        for a in [ChannelAssignment::Independent(2), ChannelAssignment::LeftSide, ChannelAssignment::SideRight, ChannelAssignment::MidSide] {
            let [mut c0, mut c1] = encode(&l, &r, a);
            decode(&mut c0, &mut c1, a);
            assert_eq!((c0, c1), (l.clone(), r.clone()), "{a:?}");
            assert_eq!(ChannelAssignment::from_code(a.code()), Some(a));
        }
    }
}
