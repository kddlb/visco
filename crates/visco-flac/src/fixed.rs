//! Fixed polynomial predictors of order 0–4.

/// Prediction of sample `n` (needs `order` previous samples).
#[inline]
pub fn predict(x: &[i64], n: usize, order: usize) -> i64 {
    match order {
        0 => 0,
        1 => x[n - 1],
        // Wrapping: valid streams never overflow, corrupt ones must not panic.
        2 => x[n - 1].wrapping_mul(2).wrapping_sub(x[n - 2]),
        3 => x[n - 1].wrapping_mul(3).wrapping_sub(x[n - 2].wrapping_mul(3)).wrapping_add(x[n - 3]),
        4 => x[n - 1]
            .wrapping_mul(4)
            .wrapping_sub(x[n - 2].wrapping_mul(6))
            .wrapping_add(x[n - 3].wrapping_mul(4))
            .wrapping_sub(x[n - 4]),
        _ => unreachable!("fixed order {order}"),
    }
}

/// Residuals for samples `order..len`.
pub fn residual(x: &[i64], order: usize) -> Vec<i64> {
    (order..x.len()).map(|n| x[n] - predict(x, n, order)).collect()
}

/// Restore in place: `x[..order]` holds warm-up, `res` the residuals.
pub fn restore(x: &mut [i64], order: usize, res: &[i64]) {
    for (i, &e) in res.iter().enumerate() {
        let n = order + i;
        x[n] = e.wrapping_add(predict(x, n, order));
    }
}

/// Sum of |residual| for each order, as libFLAC uses to pick a fixed order quickly.
pub fn abs_sums(x: &[i64]) -> [u64; 5] {
    let mut sums = [0u64; 5];
    for n in 4.min(x.len())..x.len() {
        for (o, s) in sums.iter_mut().enumerate() {
            *s += (x[n] - predict(x, n, o)).unsigned_abs();
        }
    }
    sums
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_orders() {
        let x: Vec<i64> = (0..300).map(|i| ((i as f64 * 0.07).sin() * 20000.0) as i64 + (i % 7) as i64).collect();
        for order in 0..=4 {
            let r = residual(&x, order);
            let mut y = vec![0; x.len()];
            y[..order].copy_from_slice(&x[..order]);
            restore(&mut y, order, &r);
            assert_eq!(x, y);
        }
        // a cubic is predicted perfectly by order 4
        let cubic: Vec<i64> = (0..50).map(|i: i64| i * i * i - 3 * i).collect();
        assert!(residual(&cubic, 4).iter().all(|&e| e == 0));
    }
}
