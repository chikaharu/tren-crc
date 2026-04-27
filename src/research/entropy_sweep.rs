//! Entropy / detection-rate sweep over scatter patterns.
//!
//! Two complementary measurements:
//!
//! - [`measure_conditional_entropy`]: empirical estimate of
//!   `H(diag)`, `H(diag | body_bucket)` and the implied mutual
//!   information `I(diag; body_bucket) = H(diag) - H(diag|body_bucket)`
//!   for a given [`ScatterPattern`]. Theoretically `H(diag | body)`
//!   is exactly zero (CRC is a deterministic function of body); the
//!   *estimated* conditional entropy at finite sample size and
//!   reduced body resolution is what the estimator actually returns,
//!   and that is what we want to compare across patterns.
//!
//! - [`measure_detection_rate`]: empirical bit-error detection rate
//!   for a given pattern × [`ErrorClass`]. Returns a Wilson 95 %
//!   confidence interval.
//!
//! Both measurements take a deterministic `seed` so an external
//! sweep script (`examples/exp_diagonal_entropy.rs`) can reproduce
//! identical numbers across runs.

use rand::{Rng, RngCore, SeedableRng, rngs::StdRng};

use crate::Frame32;

use super::data::random_frame;
use super::inject::{flip_bits, flip_burst, flip_random_n};
use super::scatter::{ScatterPattern, update_with, verify_with};
use super::stats::{
    bucket as bucket_bytes, conditional_entropy_estimate, detection_rate, joint_entropy_estimate,
};

// ---------------------------------------------------------------------------
// Error classes
// ---------------------------------------------------------------------------

/// Bit-error patterns the detection-rate sweep injects.
#[derive(Debug, Clone, Copy)]
pub enum ErrorClass {
    /// `n` distinct positions chosen uniformly at random.
    Random(usize),
    /// Burst of `n` consecutive bits at a random start position.
    Burst(usize),
    /// `n` distinct positions chosen uniformly at random from the
    /// even-indexed bits (flat index 0, 2, 4, …, 1022).
    EvenBit(usize),
    /// `n` distinct positions chosen uniformly at random from the
    /// reserved (CRC) positions of the given scatter pattern.
    DiagonalOnly(usize),
}

impl ErrorClass {
    /// Short label used in result tables.
    pub fn label(&self) -> String {
        match *self {
            ErrorClass::Random(n) => format!("Random({n})"),
            ErrorClass::Burst(n) => format!("Burst({n})"),
            ErrorClass::EvenBit(n) => format!("EvenBit({n})"),
            ErrorClass::DiagonalOnly(n) => format!("DiagonalOnly({n})"),
        }
    }
}

/// Enumerate the reserved (CRC) `(slot, bit)` positions of a scatter
/// pattern by introspection: clear an all-ones frame and observe
/// which bits ended up zero.
pub fn reserved_positions<P: ScatterPattern>(pattern: &P) -> Vec<(usize, usize)> {
    let mut f = Frame32::new();
    for slot in 0..32 {
        f.0[slot] = u32::MAX;
    }
    pattern.clear_reserved(&mut f);
    let mut out = Vec::new();
    for slot in 0..32 {
        for bit in 0..32 {
            if (f.0[slot] >> bit) & 1 == 0 {
                out.push((slot, bit));
            }
        }
    }
    out
}

fn apply_error<P: ScatterPattern, R: Rng>(
    class: ErrorClass,
    pattern: &P,
    frame: &mut Frame32,
    rng: &mut R,
) {
    match class {
        ErrorClass::Random(n) => {
            let _ = flip_random_n(frame, n, rng);
        }
        ErrorClass::Burst(n) => {
            let max_start = 1024 - n;
            let start = if max_start == 0 { 0 } else { rng.gen_range(0..=max_start) };
            flip_burst(frame, start, n);
        }
        ErrorClass::EvenBit(n) => {
            // Flat indices: 0, 2, 4, …, 1022 → 512 candidates.
            let mut chosen = std::collections::HashSet::with_capacity(n);
            while chosen.len() < n {
                chosen.insert(rng.gen_range(0..512usize) * 2);
            }
            let positions: Vec<(usize, usize)> =
                chosen.into_iter().map(|i| (i / 32, i % 32)).collect();
            flip_bits(frame, &positions);
        }
        ErrorClass::DiagonalOnly(n) => {
            let candidates = reserved_positions(pattern);
            assert!(
                n <= candidates.len(),
                "DiagonalOnly({n}) requested but only {} reserved positions",
                candidates.len()
            );
            // Reservoir-style sample without replacement.
            let mut indices: Vec<usize> = (0..candidates.len()).collect();
            for i in 0..n {
                let j = rng.gen_range(i..indices.len());
                indices.swap(i, j);
            }
            let positions: Vec<(usize, usize)> =
                indices[..n].iter().map(|&i| candidates[i]).collect();
            flip_bits(frame, &positions);
        }
    }
}

// ---------------------------------------------------------------------------
// Detection-rate sweep
// ---------------------------------------------------------------------------

/// Per-(pattern, ErrorClass) detection-rate result.
#[derive(Debug, Clone)]
pub struct DetectionReport {
    pub pattern_label: String,
    pub class_label: String,
    pub trials: u64,
    pub detected: u64,
    pub rate: f64,
    pub wilson_low: f64,
    pub wilson_high: f64,
}

/// Run `n_trials` of the given (pattern, error_class) and return the
/// detection rate as a Wilson-95 interval.
pub fn measure_detection_rate<P: ScatterPattern>(
    pattern: &P,
    pattern_label: &str,
    class: ErrorClass,
    n_trials: u64,
    seed: u64,
) -> DetectionReport {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut detected = 0u64;
    for _ in 0..n_trials {
        // Build a body of random bits, install the CRC for this
        // pattern; the result therefore round-trips through verify.
        let mut frame = random_frame(&mut rng);
        // Re-encode under the test pattern (random_frame populated
        // the production diagonal CRC; we want this pattern's CRC).
        update_with(pattern, &mut frame);
        debug_assert!(verify_with(pattern, &frame));
        // Inject the error.
        apply_error(class, pattern, &mut frame, &mut rng);
        if !verify_with(pattern, &frame) {
            detected += 1;
        }
    }
    let (rate, wilson_low, wilson_high) = detection_rate(detected, n_trials);
    DetectionReport {
        pattern_label: pattern_label.to_string(),
        class_label: class.label(),
        trials: n_trials,
        detected,
        rate,
        wilson_low,
        wilson_high,
    }
}

// ---------------------------------------------------------------------------
// Conditional-entropy sweep
// ---------------------------------------------------------------------------

/// Per-(pattern, bucket-width) entropy estimates.
#[derive(Debug, Clone)]
pub struct EntropyReport {
    pub pattern_label: String,
    pub sample_size: usize,
    pub bucket_bits: u8,
    /// Plug-in `H(diag)` in bits (Miller-Madow corrected).
    pub h_diag: f64,
    /// Plug-in `H(diag | body_bucket)` in bits (Miller-Madow corrected).
    pub h_diag_given_body_bucket: f64,
    /// `H(diag) - H(diag | body_bucket)` in bits.
    pub mutual_info_estimate: f64,
}

/// Sample `n_samples` random bodies, compute the diag value (= CRC of
/// the body) and a `bucket_bits`-wide FNV-1a hash bucket of the body,
/// then report Miller-Madow-corrected entropy estimates.
pub fn measure_conditional_entropy<P: ScatterPattern>(
    pattern: &P,
    pattern_label: &str,
    n_samples: usize,
    bucket_bits: u8,
    seed: u64,
) -> EntropyReport {
    let mut rng = StdRng::seed_from_u64(seed);

    // (diag_value, body_bucket) samples.
    let mut samples: Vec<(u32, u128)> = Vec::with_capacity(n_samples);
    for _ in 0..n_samples {
        // Build a random "body": fill all 32 slots with random bits,
        // then clear the reserved positions for this pattern.
        let mut frame = Frame32::new();
        for slot in 0..32 {
            frame.0[slot] = rng.next_u32();
        }
        pattern.clear_reserved(&mut frame);

        // Serialise the cleared body to 128 bytes (matches the byte
        // ordering used in `scatter::update_with`).
        let mut body = [0u8; 128];
        for i in 0..32 {
            body[i * 4..(i + 1) * 4].copy_from_slice(&frame.0[i].to_be_bytes());
        }

        // diag = CRC over the body, masked to the pattern's information bits.
        let diag = crc32c::crc32c(&body) & pattern.mask();
        // body_bucket = FNV-1a hash of body, reduced to `bucket_bits`.
        let body_bucket = bucket_bytes(&body, bucket_bits) as u128;
        samples.push((diag, body_bucket));
    }

    // H(diag): build (diag, 0) pairs and reuse joint_entropy_estimate
    // (joint of a constant gives plain marginal).
    let diag_only: Vec<(u32, u128)> = samples.iter().map(|&(d, _)| (d, 0u128)).collect();
    let h_diag_raw = joint_entropy_estimate(&diag_only);
    let h_cond_raw = conditional_entropy_estimate(&samples);

    // Miller-Madow bias correction: H_corrected = H_plugin + (B - 1) / (2N ln 2)
    // where B is the number of distinct support points seen. We apply
    // it separately to each estimator so the corrected MI is
    // H_diag_corrected - H_cond_corrected.
    let n_f = n_samples as f64;
    let ln2 = std::f64::consts::LN_2;

    let mut diag_seen = std::collections::HashSet::with_capacity(n_samples);
    for &(d, _) in &samples {
        diag_seen.insert(d);
    }
    let b_diag = diag_seen.len() as f64;
    let h_diag = h_diag_raw + (b_diag - 1.0) / (2.0 * n_f * ln2);

    let mut joint_seen = std::collections::HashSet::with_capacity(n_samples);
    let mut bucket_seen = std::collections::HashSet::with_capacity(n_samples);
    for &(d, y) in &samples {
        joint_seen.insert((d, y));
        bucket_seen.insert(y);
    }
    let b_joint = joint_seen.len() as f64;
    let b_bucket = bucket_seen.len() as f64;
    // For H(X|Y) = H(X,Y) - H(Y), each component carries its own MM term.
    let h_cond = (h_cond_raw + (b_joint - 1.0) / (2.0 * n_f * ln2)
        - (b_bucket - 1.0) / (2.0 * n_f * ln2))
        .max(0.0);

    let mi = (h_diag - h_cond).max(0.0);
    EntropyReport {
        pattern_label: pattern_label.to_string(),
        sample_size: n_samples,
        bucket_bits,
        h_diag,
        h_diag_given_body_bucket: h_cond,
        mutual_info_estimate: mi,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::scatter::{AntiDiagonal, Diagonal, Hadamard, Permuted};

    #[test]
    fn reserved_positions_diagonal_count() {
        let p = Diagonal;
        let r = reserved_positions(&p);
        assert_eq!(r.len(), 32);
        for i in 0..32 {
            assert!(r.contains(&(i, i)));
        }
    }

    #[test]
    fn reserved_positions_antidiagonal_count() {
        let p = AntiDiagonal;
        let r = reserved_positions(&p);
        assert_eq!(r.len(), 32);
        for i in 0..32 {
            assert!(r.contains(&(i, 31 - i)));
        }
    }

    #[test]
    fn reserved_positions_hadamard_count() {
        let p = Hadamard;
        let r = reserved_positions(&p);
        // 8 information bits × 4 replicas = 32 reserved positions.
        assert_eq!(r.len(), 32);
    }

    #[test]
    fn reserved_positions_permuted_count() {
        let p = Permuted::new(0xDEAD_BEEF);
        let r = reserved_positions(&p);
        assert_eq!(r.len(), 32);
    }

    #[test]
    fn detection_random_8_bits_all_patterns_high() {
        // 8 random bit flips should be detected with very high
        // probability by all 32-bit-CRC patterns.
        let class = ErrorClass::Random(8);
        for (label, rate) in [
            ("Diagonal", measure_detection_rate(&Diagonal, "Diagonal", class, 1000, 1).rate),
            (
                "AntiDiagonal",
                measure_detection_rate(&AntiDiagonal, "AntiDiagonal", class, 1000, 2).rate,
            ),
            (
                "Permuted",
                measure_detection_rate(
                    &Permuted::new(0x1234),
                    "Permuted(0x1234)",
                    class,
                    1000,
                    3,
                )
                .rate,
            ),
        ] {
            assert!(rate >= 0.999, "{label} detection rate {rate} too low");
        }
    }

    #[test]
    fn detection_diagonal_only_one_bit_full_crc() {
        // Flipping a single reserved bit should always be detected
        // by a full 32-bit CRC pattern (the read CRC differs by 1
        // bit from the recomputed CRC).
        let class = ErrorClass::DiagonalOnly(1);
        let rate =
            measure_detection_rate(&Diagonal, "Diagonal", class, 500, 4).rate;
        assert_eq!(rate, 1.0);
    }

    #[test]
    fn detection_diagonal_only_one_bit_hadamard_undetected() {
        // Flipping 1 of 4 Hadamard replicas is corrected by majority
        // vote, so the *read* CRC matches the recomputed CRC →
        // undetected.
        let class = ErrorClass::DiagonalOnly(1);
        let rate = measure_detection_rate(&Hadamard, "Hadamard", class, 500, 5).rate;
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn entropy_diag_close_to_information_bits() {
        // For a 32-bit CRC, H(diag) on a random body should be close
        // to 32 bits (CRC is approximately uniform). With N = 4096
        // samples we'll see ≈4096 distinct values, so the plug-in
        // H_diag is close to log2(4096) = 12 bits — not 32 bits.
        // What we *can* assert is that H(diag) ≤ information_bits.
        let r = measure_conditional_entropy(&Diagonal, "Diagonal", 4096, 16, 7);
        assert!(r.h_diag <= 32.0 + 0.1);
        assert!(r.h_diag >= 11.0); // close to log2(4096)
    }

    #[test]
    fn entropy_cond_zero_at_full_resolution() {
        // With bucket_bits = 32 and N small, every bucket is unique,
        // so the conditional entropy estimate is 0 (no within-bucket
        // variation).
        let r = measure_conditional_entropy(&Diagonal, "Diagonal", 1024, 32, 9);
        assert!(
            r.h_diag_given_body_bucket < 0.05,
            "expected ~0, got {}",
            r.h_diag_given_body_bucket
        );
    }
}
