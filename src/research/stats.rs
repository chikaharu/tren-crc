//! Statistical utilities: detection rate confidence intervals and
//! plug-in entropy estimators.
//!
//! The entropy estimators here are deliberately the naïve
//! "maximum-likelihood plug-in" form. They are **biased downward** in
//! the low-sample regime (the bias is roughly `(B - 1) / (2 * N * ln 2)`
//! bits where `B` is the number of distinct values observed and `N` is
//! the sample size). Downstream experiments that need finer estimates
//! should bucket their inputs first and report sample sizes alongside
//! results.

use std::collections::HashMap;

/// Returns `(rate, wilson_low, wilson_high)` for `detected` successes
/// out of `trials`. The Wilson 95% interval is computed with `z = 1.96`.
/// Returns `(0.0, 0.0, 0.0)` for `trials == 0` so callers don't need
/// to special-case empty data.
pub fn detection_rate(detected: u64, trials: u64) -> (f64, f64, f64) {
    if trials == 0 {
        return (0.0, 0.0, 0.0);
    }
    let n = trials as f64;
    let p = detected as f64 / n;
    let z = 1.96_f64;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = (p + z2 / (2.0 * n)) / denom;
    let halfw = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
    (p, (center - halfw).max(0.0), (center + halfw).min(1.0))
}

/// Binary Shannon entropy `H(p) = -p log₂ p - (1-p) log₂(1-p)` in bits.
/// Returns 0 for `p` outside `(0, 1)`.
pub fn binary_entropy(p: f64) -> f64 {
    if p <= 0.0 || p >= 1.0 || !p.is_finite() {
        return 0.0;
    }
    -p * p.log2() - (1.0 - p) * (1.0 - p).log2()
}

/// Plug-in joint entropy `H(X, Y)` in bits over the given `(x, y)`
/// samples, where `x: u32` is intended for a CRC / diagonal value and
/// `y: u128` is intended for a body-bucket reduction.
///
/// Heavily biased downward when `samples.len()` is small relative to
/// the support size. Caller is expected to bucket the body to keep the
/// support tractable.
pub fn joint_entropy_estimate(samples: &[(u32, u128)]) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f64;
    let mut counts: HashMap<(u32, u128), u64> = HashMap::with_capacity(n);
    for &s in samples {
        *counts.entry(s).or_insert(0) += 1;
    }
    let mut h = 0.0_f64;
    for &c in counts.values() {
        let p = c as f64 / n_f;
        h -= p * p.log2();
    }
    h
}

/// Plug-in conditional entropy `H(X | Y) = H(X, Y) - H(Y)` in bits,
/// computed from `(x, y)` samples where `x` is the dependent variable
/// (CRC bits) and `y` the conditioning variable (body bucket).
pub fn conditional_entropy_estimate(samples: &[(u32, u128)]) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let n_f = n as f64;
    let h_xy = joint_entropy_estimate(samples);
    let mut y_counts: HashMap<u128, u64> = HashMap::with_capacity(n);
    for &(_, y) in samples {
        *y_counts.entry(y).or_insert(0) += 1;
    }
    let mut h_y = 0.0_f64;
    for &c in y_counts.values() {
        let p = c as f64 / n_f;
        h_y -= p * p.log2();
    }
    (h_xy - h_y).max(0.0)
}

/// FNV-1a 32-bit hash, useful as a fast body-bucket reduction. Not
/// cryptographic.
pub fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Reduce `bytes` to a `bits`-wide bucket via FNV-1a + low-bit mask.
/// Saturates at 32 bits (returns the full hash if `bits >= 32`).
pub fn bucket(bytes: &[u8], bits: u8) -> u32 {
    let h = fnv1a_32(bytes);
    if bits >= 32 {
        h
    } else {
        h & ((1u32 << bits) - 1)
    }
}
