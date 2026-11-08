//! Zigzag scan order.

/// `ZIGZAG[k]` is the natural (row-major) index of the k-th coefficient in zigzag order.
pub const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// `UNZIGZAG[n]` is the zigzag position of natural index n.
pub const UNZIGZAG: [usize; 64] = {
    let mut out = [0; 64];
    let mut k = 0;
    while k < 64 {
        out[ZIGZAG[k]] = k;
        k += 1;
    }
    out
};

pub fn to_zigzag<T: Copy + Default>(natural: &[T; 64]) -> [T; 64] {
    let mut out = [T::default(); 64];
    for k in 0..64 {
        out[k] = natural[ZIGZAG[k]];
    }
    out
}

pub fn from_zigzag<T: Copy + Default>(zz: &[T; 64]) -> [T; 64] {
    let mut out = [T::default(); 64];
    for k in 0..64 {
        out[ZIGZAG[k]] = zz[k];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_permutation_and_inverse() {
        let mut seen = [false; 64];
        for &n in &ZIGZAG {
            assert!(!seen[n]);
            seen[n] = true;
        }
        let nat: [i32; 64] = std::array::from_fn(|i| i as i32);
        assert_eq!(from_zigzag(&to_zigzag(&nat)), nat);
        // spot check: zigzag position 2 is row 1 col 0, position 3 is row 2 col 0
        assert_eq!(ZIGZAG[2], 8);
        assert_eq!(ZIGZAG[3], 16);
        assert_eq!(UNZIGZAG[63], 63);
    }
}
