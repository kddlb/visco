//! Rice parameter and partition-order selection.

use crate::bitio::zigzag;

pub const RICE_ESCAPE: u32 = 0xF;
pub const RICE2_ESCAPE: u32 = 0x1F;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    /// Rice parameter, or raw bits per sample when `escaped`.
    pub k: u32,
    pub escaped: bool,
    pub count: usize,
    pub bits: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RicePlan {
    pub rice2: bool,
    pub order: u32,
    pub partitions: Vec<Partition>,
    /// Total residual bits including the method/order header and parameters.
    pub bits: u64,
}

/// Exact bit cost of coding `us` (zigzagged residuals) with parameter `k`.
fn cost(sum_shifted: u64, n: usize, k: u32) -> u64 {
    n as u64 * (k as u64 + 1) + sum_shifted
}

/// Best parameter for one partition; considers escape coding.
pub fn best_partition(us: &[u64], max_k: u32, param_bits: u32, allow_escape: bool) -> Partition {
    let n = us.len();
    if n == 0 {
        return Partition { k: 0, escaped: false, count: 0, bits: param_bits as u64 };
    }
    let sum: u64 = us.iter().sum();
    let mean = sum / n as u64;
    let est = if mean == 0 { 0 } else { 64 - mean.leading_zeros() - 1 }.min(max_k);
    let mut best = (u64::MAX, 0u32);
    for k in est.saturating_sub(1)..=(est + 1).min(max_k) {
        let s: u64 = us.iter().map(|&u| u >> k).sum();
        let c = cost(s, n, k);
        if c < best.0 {
            best = (c, k);
        }
    }
    // Escape: raw two's-complement samples of `b` bits.
    let maxu = us.iter().copied().max().unwrap_or(0);
    let raw_bits = if maxu == 0 { 0 } else { 64 - maxu.leading_zeros() } as u64; // zigzag width ≈ signed width
    let esc = raw_bits * n as u64 + 5;
    if allow_escape && esc < best.0 && raw_bits <= 31 {
        return Partition { k: raw_bits as u32, escaped: true, count: n, bits: param_bits as u64 + esc };
    }
    Partition { k: best.1, escaped: false, count: n, bits: param_bits as u64 + best.0 }
}

/// Choose the partition order (0..=max_order) and parameters minimizing total bits.
/// `res` holds the `block_size - predictor_order` residuals.
pub fn plan(res: &[i64], block_size: usize, predictor_order: usize, max_order: u32, bps: u32, allow_escape: bool) -> RicePlan {
    let us: Vec<u64> = res.iter().map(|&e| zigzag(e)).collect();
    // RICE2 when residuals could need a parameter ≥ 15.
    let rice2 = bps > 16 || us.iter().any(|&u| u >= 1 << 20);
    let (max_k, param_bits) = if rice2 { (30, 5) } else { (14, 4) };
    let mut best: Option<RicePlan> = None;
    for order in 0..=max_order {
        let parts = 1usize << order;
        if !block_size.is_multiple_of(parts) || (block_size >> order) <= predictor_order {
            break;
        }
        let psize = block_size >> order;
        let mut partitions = Vec::with_capacity(parts);
        let mut start = 0;
        for p in 0..parts {
            let n = if p == 0 { psize - predictor_order } else { psize };
            partitions.push(best_partition(&us[start..start + n], max_k, param_bits, allow_escape));
            start += n;
        }
        let bits = 6 + partitions.iter().map(|p| p.bits).sum::<u64>();
        if best.as_ref().is_none_or(|b| bits < b.bits) {
            best = Some(RicePlan { rice2, order, partitions, bits });
        }
    }
    best.expect("partition order 0 is always valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_prefers_more_partitions_for_bursty_residuals() {
        let mut res = vec![1i64; 4096];
        for v in &mut res[2048..] {
            *v = 3000;
        }
        let p = plan(&res, 4096, 0, 8, 16, false);
        assert!(p.order >= 1);
        assert_eq!(p.partitions.iter().map(|p| p.count).sum::<usize>(), 4096);
        let flat = plan(&vec![5i64; 4096], 4096, 0, 8, 16, false);
        assert_eq!(flat.order, 0);
    }

    #[test]
    fn cost_is_exact() {
        use crate::bitio::BitWriter;
        // Laplacian-shaped residuals, like real prediction errors.
        let res: Vec<i64> = (1..=1000)
            .map(|i| {
                let f = ((i * 7919) % 997 + 1) as f64 / 998.0;
                let m = (-30.0 * f.ln()).round() as i64;
                if i % 2 == 0 { m } else { -m }
            })
            .collect();
        let us: Vec<u64> = res.iter().map(|&e| zigzag(e)).collect();
        let p = best_partition(&us, 14, 4, true);
        assert!(!p.escaped);
        let mut w = BitWriter::default();
        for &e in &res {
            w.write_rice(e, p.k);
        }
        assert_eq!(w.bit_pos() + 4, p.bits);
    }
}
