//! Smoke tests for the experimental research harness.
//!
//! Built only when the `experimental` cargo feature is enabled.
//! Verifies that:
//! * `random_frame` produces frames that pass `verify_crc`.
//! * Single-bit injection trips `verify_crc`.
//! * No-op `flip_random_n(.., 0, ..)` is a true no-op.
//! * The `Diagonal` scatter pattern is bit-exact equivalent to the
//!   production `update_crc` / `verify_crc` over a thousand frames.
//! * Statistical helpers return their pinned values at the easy points.

#![cfg(feature = "experimental")]

use rand::SeedableRng;
use rand::rngs::StdRng;
use tren::research::bench::{BenchHarness, BenchResult};
use tren::research::data::{random_body, random_frame, random_frames};
use tren::research::inject::{flip_bits, flip_burst, flip_random_n};
use tren::research::scatter::{
    update_with, verify_with, AntiDiagonal, Diagonal, Hadamard, Permuted, ScatterPattern,
};
use tren::research::stats::{
    binary_entropy, bucket, conditional_entropy_estimate, detection_rate,
    joint_entropy_estimate,
};
use tren::Frame32;

#[test]
fn random_frame_verifies() {
    for seed in 0..32 {
        let frames = random_frames(8, seed);
        for (i, f) in frames.iter().enumerate() {
            assert!(
                f.verify_crc(),
                "random frame seed={} idx={} failed verify_crc",
                seed,
                i
            );
        }
    }
}

#[test]
fn single_bit_flip_breaks_verify() {
    let mut rng = StdRng::seed_from_u64(42);
    for _ in 0..256 {
        let mut f = random_frame(&mut rng);
        assert!(f.verify_crc());
        let positions = flip_random_n(&mut f, 1, &mut rng);
        assert_eq!(positions.len(), 1);
        assert!(
            !f.verify_crc(),
            "1-bit flip at {:?} undetected",
            positions[0]
        );
    }
}

#[test]
fn flip_random_n_zero_is_noop() {
    let mut rng = StdRng::seed_from_u64(7);
    for _ in 0..16 {
        let f0 = random_frame(&mut rng);
        let mut f = f0;
        let positions = flip_random_n(&mut f, 0, &mut rng);
        assert!(positions.is_empty());
        assert_eq!(f.0, f0.0, "0-bit flip mutated the frame");
    }
}

#[test]
fn flip_bits_xor_cancels() {
    let mut rng = StdRng::seed_from_u64(11);
    let f0 = random_frame(&mut rng);
    let mut f = f0;
    let positions = vec![(3, 17), (3, 17), (29, 0), (0, 31), (29, 0), (0, 31)];
    flip_bits(&mut f, &positions);
    assert_eq!(f.0, f0.0);
}

#[test]
fn flip_burst_basic_range() {
    let mut rng = StdRng::seed_from_u64(13);
    let f0 = random_frame(&mut rng);
    // Burst of length 0 is a no-op (boundary check).
    let mut f = f0;
    flip_burst(&mut f, 100, 0);
    assert_eq!(f.0, f0.0);

    // A 5-bit burst at the end of the frame.
    let mut f = f0;
    flip_burst(&mut f, 1019, 5);
    let mut diffs = 0;
    for i in 0..32 {
        diffs += (f.0[i] ^ f0.0[i]).count_ones();
    }
    assert_eq!(diffs, 5);
}

#[test]
fn diagonal_scatter_matches_production() {
    // For a thousand random frames, Diagonal scatter must agree
    // bit-exact with the production `Frame32::update_crc` / `verify_crc`.
    let mut rng = StdRng::seed_from_u64(999);
    let pat = Diagonal;
    for trial in 0..1000 {
        // Start from a random body with the diagonal cleared.
        let mut f = Frame32::new();
        for slot in 0..32 {
            f.0[slot] = rand::Rng::gen::<u32>(&mut rng) & !(1u32 << slot);
        }

        // Apply both methods to fresh copies.
        let mut prod = f;
        prod.update_crc();

        let mut harness = f;
        update_with(&pat, &mut harness);

        assert_eq!(prod.0, harness.0, "trial {} differs from production", trial);
        assert!(verify_with(&pat, &harness));
        assert!(prod.verify_crc());
    }
}

#[test]
fn alt_scatter_patterns_round_trip() {
    let mut rng = StdRng::seed_from_u64(2024);

    // Each pattern, on its own terms, must round-trip.
    fn check<P: ScatterPattern>(label: &str, p: &P, rng: &mut StdRng) {
        for trial in 0..50 {
            let mut f = Frame32::new();
            for slot in 0..32 {
                f.0[slot] = rand::Rng::gen::<u32>(rng);
            }
            update_with(p, &mut f);
            assert!(verify_with(p, &f), "{}: trial {} failed verify", label, trial);

            // Flipping any single non-reserved bit either flips a
            // body bit (must trip verify) OR flips a reserved bit
            // (must also trip verify since the CRC will not match).
            let mut tampered = f;
            tampered.0[7] ^= 1u32 << 11;
            assert!(
                !verify_with(p, &tampered),
                "{}: tamper went undetected (trial {})",
                label,
                trial
            );
        }
    }

    check("AntiDiagonal", &AntiDiagonal, &mut rng);
    check("Permuted(0xC0FFEE)", &Permuted::new(0xC0FF_EE), &mut rng);
    // Hadamard carries only 8 bits, but its own round-trip must hold.
    check("Hadamard", &Hadamard, &mut rng);
}

#[test]
fn permuted_positions_are_distinct() {
    let p = Permuted::new(0xDEAD_BEEF);
    let mut seen = std::collections::HashSet::new();
    for &(s, b) in p.positions() {
        assert!(s < 32 && b < 32);
        assert!(
            seen.insert((s, b)),
            "Permuted produced duplicate position ({}, {})",
            s,
            b
        );
    }
    assert_eq!(seen.len(), 32);
}

#[test]
fn binary_entropy_pinned_values() {
    assert!((binary_entropy(0.5) - 1.0).abs() < 1e-12);
    assert_eq!(binary_entropy(0.0), 0.0);
    assert_eq!(binary_entropy(1.0), 0.0);
    assert_eq!(binary_entropy(-0.1), 0.0);
    assert_eq!(binary_entropy(1.1), 0.0);
    // Symmetry: H(p) = H(1-p).
    for p in [0.1_f64, 0.25, 0.3, 0.4] {
        assert!((binary_entropy(p) - binary_entropy(1.0 - p)).abs() < 1e-12);
    }
}

#[test]
fn detection_rate_basic() {
    let (rate, lo, hi) = detection_rate(0, 0);
    assert_eq!((rate, lo, hi), (0.0, 0.0, 0.0));

    let (rate, lo, hi) = detection_rate(100, 100);
    assert!((rate - 1.0).abs() < 1e-12);
    assert!(hi >= 0.99);
    assert!(lo <= 1.0);

    let (rate, lo, hi) = detection_rate(50, 100);
    assert!((rate - 0.5).abs() < 1e-12);
    assert!(lo > 0.39 && lo < 0.5);
    assert!(hi > 0.5 && hi < 0.61);
}

#[test]
fn joint_and_conditional_entropy_zero_when_perfectly_correlated() {
    // Y = f(X) for some f → H(Y|X) = 0.
    let samples: Vec<(u32, u128)> = (0..200).map(|i| ((i % 7) as u32, (i % 7) as u128)).collect();
    let h_xy = joint_entropy_estimate(&samples);
    let h_cond = conditional_entropy_estimate(&samples);
    assert!(h_xy > 0.0);
    assert!(
        h_cond.abs() < 1e-10,
        "conditional entropy should be ~0 when Y=f(X), got {}",
        h_cond
    );
}

#[test]
fn joint_entropy_independent_pair_is_sum() {
    // Construct samples where X and Y are independent uniform over 4
    // values each. H(X,Y) ≈ H(X) + H(Y) ≈ 4 bits.
    let mut samples = Vec::with_capacity(4 * 4 * 256);
    for x in 0..4u32 {
        for y in 0..4u128 {
            for _ in 0..256 {
                samples.push((x, y));
            }
        }
    }
    let h = joint_entropy_estimate(&samples);
    assert!(
        (h - 4.0).abs() < 0.05,
        "joint entropy of indep 4×4 should be ~4 bits, got {}",
        h
    );
}

#[test]
fn bucket_is_deterministic_and_masked() {
    let body = [0xA5u8; 128];
    let b8 = bucket(&body, 8);
    assert!(b8 < 256);
    assert_eq!(bucket(&body, 8), b8);
    let b16 = bucket(&body, 16);
    assert!(b16 < 65536);
    let full = bucket(&body, 32);
    assert_eq!(full, bucket(&body, 32));
}

#[test]
fn bench_harness_returns_sorted_stats() {
    // The work returned must influence the result so the optimiser
    // doesn't elide it. We use a simple xor-shift accumulator.
    let mut acc: u32 = 1;
    let result = BenchHarness::run("xor_shift", 128, || {
        acc = acc.wrapping_mul(1_103_515_245).wrapping_add(12345);
        acc
    });
    assert_eq!(result.iters, 128);
    assert!(result.min_ns <= result.median_ns);
    assert!(result.median_ns <= result.p99_ns);
    assert!(result.throughput_per_sec.is_finite());
    assert!(result.throughput_per_sec > 0.0);

    // print/format must not panic.
    let _ = BenchResult::format_table(&[result]);
}

#[test]
fn random_body_is_full_length() {
    let mut rng = StdRng::seed_from_u64(5);
    let body = random_body(&mut rng);
    assert_eq!(body.len(), 128);
    // Two seeds should give different bodies (overwhelmingly likely).
    let mut rng2 = StdRng::seed_from_u64(6);
    let body2 = random_body(&mut rng2);
    assert_ne!(body, body2);
}
