//! Error-detection coverage for the diagonal CRC-32C of Frame32.
//!
//! These tests exercise the strength gain we get by replacing per-row
//! XOR parity with CRC-32C: any 1, 2, or 3 random bit errors, any odd
//! number of bit errors, and any burst error of length ≤ 32 bits must
//! all be detected, including errors that hit only the diagonal bits.
//!
//! There is also a small set of pinned CRC-32C vectors so that any
//! accidental change to the polynomial, byte order, or scatter mapping
//! shows up as a hard test failure rather than silent wire-format drift.

use tren::{build_submit, Frame32, FrameError};

/// Build a representative non-trivial frame: a SUBMIT with a few deps
/// and a client token. We use this everywhere we need a `Frame32` whose
/// data bits are non-zero across many slots.
fn sample_frame() -> Frame32 {
    let deps: Vec<u64> = (1..=8).map(|i| (i as u64) * 0x0123_4567 & 0x7FFF_FFFF).collect();
    build_submit(&deps, 0x1ABC_DEF0, 0x004A_55AA)
}

/// Flip bit `bit_index` (0..1024) of the 128-byte serialised frame.
fn flip_bit(bytes: &mut [u8; 128], bit_index: usize) {
    let byte = bit_index / 8;
    let bit  = bit_index % 8;
    bytes[byte] ^= 1u8 << bit;
}

/// Linear-congruential PRNG so the tests are deterministic and we don't
/// need to pull in `rand`. Period is huge enough for our sample sizes.
struct Lcg(u64);
impl Lcg {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0
    }
    fn range(&mut self, n: u64) -> u64 { self.next_u64() % n }
}

#[test]
fn single_bit_flip_in_data_is_detected_everywhere() {
    // Every single bit of the 1024-bit frame, when flipped, must be
    // detected. CRC-32C catches all 1-bit errors trivially.
    let bytes_orig = sample_frame().to_bytes();
    for bit in 0..1024 {
        let mut bytes = bytes_orig;
        flip_bit(&mut bytes, bit);
        match Frame32::from_bytes(&bytes) {
            Err(FrameError::CrcMismatch) => {}
            Ok(_) => panic!("undetected 1-bit flip at bit {}", bit),
            Err(e) => panic!("wrong error variant for bit {}: {:?}", bit, e),
        }
    }
}

#[test]
fn diagonal_bit_flip_alone_is_detected() {
    // The XOR-parity scheme was structurally weak against errors that
    // hit only the diagonal (each diagonal bit *was* its own parity →
    // a single diagonal flip simply looked like a legitimate new
    // parity value, so it slipped through verify_parity). With CRC-32C
    // the diagonal is a hash of the rest, so any flip on it must trip
    // detection.
    let f = sample_frame();
    let bytes_orig = f.to_bytes();
    let f_signed = Frame32::from_bytes(&bytes_orig).expect("baseline must verify");
    for slot in 0..32 {
        let mut tampered = f_signed;
        tampered.0[slot] ^= 1u32 << slot; // flip exactly the diagonal bit of this slot
        let mut bytes = [0u8; 128];
        for i in 0..32 {
            bytes[i*4..(i+1)*4].copy_from_slice(&tampered.0[i].to_be_bytes());
        }
        assert!(matches!(
            Frame32::from_bytes(&bytes),
            Err(FrameError::CrcMismatch)
        ), "undetected diagonal flip at slot {}", slot);
    }
}

#[test]
fn two_bit_flips_are_always_detected() {
    // CRC-32C with a 32-bit register detects all 2-bit errors in any
    // message of length ≤ 2^31 - 1 bits — well beyond our 1024-bit
    // frame. Sample 200 random pairs and require 100% detection.
    let bytes_orig = sample_frame().to_bytes();
    let mut rng = Lcg(0xDEAD_BEEF_CAFE_F00D);
    for trial in 0..200 {
        let mut a = rng.range(1024) as usize;
        let mut b = rng.range(1024) as usize;
        if a == b { b = (b + 1) % 1024; }
        if a > b { std::mem::swap(&mut a, &mut b); }
        let mut bytes = bytes_orig;
        flip_bit(&mut bytes, a);
        flip_bit(&mut bytes, b);
        assert!(matches!(
            Frame32::from_bytes(&bytes),
            Err(FrameError::CrcMismatch)
        ), "undetected 2-bit flip ({}, {}) on trial {}", a, b, trial);
    }
}

#[test]
fn three_bit_flips_are_always_detected_in_sample() {
    // CRC-32C is not provably 3-bit-perfect for arbitrary lengths, but
    // for a 1024-bit message the Hamming distance ≥ 4. Sample 200
    // distinct triplets and require all to be detected.
    let bytes_orig = sample_frame().to_bytes();
    let mut rng = Lcg(0x1234_5678_9ABC_DEF0);
    for trial in 0..200 {
        let mut bits = [0usize; 3];
        let mut filled = 0;
        while filled < 3 {
            let candidate = rng.range(1024) as usize;
            if !bits[..filled].contains(&candidate) {
                bits[filled] = candidate;
                filled += 1;
            }
        }
        let mut bytes = bytes_orig;
        for &b in &bits { flip_bit(&mut bytes, b); }
        assert!(matches!(
            Frame32::from_bytes(&bytes),
            Err(FrameError::CrcMismatch)
        ), "undetected 3-bit flip {:?} on trial {}", bits, trial);
    }
}

#[test]
fn odd_bit_flips_are_always_detected() {
    // CRC-32C, as a CRC over GF(2) with polynomial having (x+1) as a
    // factor, detects every error pattern with an odd number of flipped
    // bits. Try odd Hamming weights 1, 5, 7, 11.
    let bytes_orig = sample_frame().to_bytes();
    let mut rng = Lcg(0xABAD_1DEA_F00D_BABE);
    for &weight in &[1usize, 5, 7, 11] {
        for trial in 0..50 {
            let mut chosen: Vec<usize> = Vec::with_capacity(weight);
            while chosen.len() < weight {
                let c = rng.range(1024) as usize;
                if !chosen.contains(&c) { chosen.push(c); }
            }
            let mut bytes = bytes_orig;
            for &b in &chosen { flip_bit(&mut bytes, b); }
            assert!(matches!(
                Frame32::from_bytes(&bytes),
                Err(FrameError::CrcMismatch)
            ), "undetected {}-bit (odd) error {:?} on trial {}", weight, chosen, trial);
        }
    }
}

#[test]
fn burst_errors_up_to_32_bits_are_detected() {
    // CRC-32C with a 32-bit register detects every burst of length ≤ 32
    // (the CRC's degree). A "burst of length L" is any error pattern
    // bounded by L consecutive bit positions — we model this as flipping
    // the leftmost and rightmost bits of an L-wide window plus an
    // arbitrary subset between, which is more than what the bound
    // requires but still must be 100% detected.
    let bytes_orig = sample_frame().to_bytes();
    let mut rng = Lcg(0xCAFE_F00D_DEAD_C0DE);
    for length in 1usize..=32 {
        for _trial in 0..10 {
            let start = rng.range((1024 - length as u64 + 1) as u64) as usize;
            // Mask of `length` bits, leftmost and rightmost forced 1
            // (so it really is a burst of exactly `length`); middle
            // bits randomised.
            let mut mask: u32 = 1u32 | (1u32 << (length - 1));
            if length > 2 {
                let middle = (rng.next_u64() as u32) & ((1u32 << (length - 1)) - 2);
                mask |= middle;
            }
            let mut bytes = bytes_orig;
            for k in 0..length {
                if (mask >> k) & 1 == 1 {
                    flip_bit(&mut bytes, start + k);
                }
            }
            assert!(matches!(
                Frame32::from_bytes(&bytes),
                Err(FrameError::CrcMismatch)
            ), "undetected burst length={} start={} mask=0x{:08x}", length, start, mask);
        }
    }
}

#[test]
fn pinned_crc32c_vector_all_zero_input() {
    // Sanity-pin the underlying CRC-32C (Castagnoli) implementation:
    // the CRC-32C of a zero-length input is 0x00000000, and the CRC-32C
    // of 9 ASCII bytes "123456789" is the standard 0xE3069283 check
    // vector listed by the Castagnoli reference. If the crc32c crate
    // ever changes polynomial or initial value, this test will catch
    // it before the diagonal scatter logic obscures the regression.
    assert_eq!(crc32c::crc32c(b""), 0x0000_0000);
    assert_eq!(crc32c::crc32c(b"123456789"), 0xE306_9283);
}

#[test]
fn pinned_diagonal_crc_for_zero_frame() {
    // A freshly-constructed Frame32 has all-zero data bits. After
    // update_crc the diagonal must equal CRC-32C of 128 zero bytes,
    // bit i scattered into slot i bit i. We verify the CRC value here
    // and round-trip via from_bytes.
    let mut f = Frame32::new();
    f.update_crc();
    let mut received: u32 = 0;
    for i in 0..32 {
        if (f.0[i] >> i) & 1 == 1 { received |= 1u32 << i; }
    }
    let expected = crc32c::crc32c(&[0u8; 128]);
    assert_eq!(received, expected, "diagonal CRC of zero frame mismatch");
    // And the frame must verify cleanly.
    let bytes = f.to_bytes();
    let g = Frame32::from_bytes(&bytes).expect("zero frame must verify");
    assert_eq!(g, f);
}

#[test]
fn verify_crc_rejects_legal_looking_diagonal_corruption() {
    // Spot-check: replace the entire diagonal with a different but
    // self-consistent-looking 32-bit value (just the bitwise NOT of the
    // real CRC) and confirm verify_crc rejects it.
    let mut f = sample_frame();
    f.update_crc();
    let mut tampered = f;
    // Flip every diagonal bit.
    for i in 0..32 { tampered.0[i] ^= 1u32 << i; }
    assert!(!tampered.verify_crc(), "tampered diagonal must fail verify_crc");
}

#[test]
fn nbit_flip_detection_rate_sweep() {
    // Sweep n-bit random flip detection rate for n = 1 … 512.
    //
    // CRC-32C properties:
    //   n=1       : 100% guaranteed (all single-bit errors detected)
    //   n=2       : 100% guaranteed (HD ≥ 4 for 1024-bit messages)
    //   n=3       : 100% guaranteed (HD ≥ 4)
    //   n odd     : 100% guaranteed ((x+1) | generator polynomial)
    //   n even >3 : ≈(1 - 2^-32) per trial — statistically indistinguishable
    //               from 100% at any practical sample size
    //
    // Run with `cargo test -- --nocapture` to see the detection-rate table.
    //
    // Contrast: under the old XOR-parity scheme, flipping all 32 diagonal
    // bits (one bit per slot) was UNDETECTED (each diagonal bit was its own
    // parity so flipping it just looked like a new valid parity). CRC-32C
    // catches every such pattern — confirmed by diagonal_bit_flip_alone_is_detected.

    const TRIALS: usize = 500;
    let bytes_orig = sample_frame().to_bytes();

    // n values to sweep: small (provably 100%), medium, large
    let ns: &[usize] = &[1, 2, 3, 4, 5, 6, 7, 8, 16, 32, 33, 64, 128, 256, 512];

    let mut rng = Lcg(0xDECA_FBAD_1337_CAFE);

    eprintln!("\n{:>6}  {:>9}  {:>7}", "n bits", "detected", "rate");
    eprintln!("{}", "-".repeat(28));

    for &n in ns {
        let mut detected = 0usize;

        for _ in 0..TRIALS {
            // Pick n distinct random bit positions (0..1024) to flip.
            let mut positions: Vec<usize> = Vec::with_capacity(n);
            while positions.len() < n {
                let pos = rng.range(1024) as usize;
                if !positions.contains(&pos) {
                    positions.push(pos);
                }
            }
            let mut bytes = bytes_orig;
            for &p in &positions {
                flip_bit(&mut bytes, p);
            }
            if matches!(Frame32::from_bytes(&bytes), Err(FrameError::CrcMismatch)) {
                detected += 1;
            }
        }

        let rate_pct = detected as f64 / TRIALS as f64 * 100.0;
        eprintln!("n={:4}:  {:4}/{:4}  = {:6.2}%", n, detected, TRIALS, rate_pct);

        // Assert correctness:
        //   n ≤ 3 or odd  →  provably 100%, require exact
        //   even n > 3    →  ≥ 499/500 (miss rate 1/2^32 makes < 1 miss
        //                     expected over the entire lifetime of the universe)
        if n <= 3 || n % 2 == 1 {
            assert_eq!(
                detected, TRIALS,
                "n={}: CRC-32C guarantees 100% detection but got {}/{}",
                n, detected, TRIALS
            );
        } else {
            assert!(
                detected >= TRIALS - 1,
                "n={}: expected ≥499/500 detection, got {}/{}",
                n, detected, TRIALS
            );
        }
    }

    eprintln!("{}", "-".repeat(28));
    eprintln!("All {} n-values verified.\n", ns.len());
}
