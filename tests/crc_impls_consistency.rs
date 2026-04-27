//! Consistency property test for the CRC-32C operator zoo.
//!
//! Asserts that every implementation in
//! `tren::research::crc_impls` returns the exact same value as the
//! production `crc32c_hw` reference for 10,000 random 128-byte
//! messages. This is the bit-exactness contract that the benchmark
//! results in `docs/research/simd_branchless_crc.md` rely on.

#![cfg(feature = "experimental")]

use rand::{RngCore, SeedableRng, rngs::StdRng};
use tren::research::crc_impls::{
    crc32c_branchless_bitwise, crc32c_branchless_bitwise_v2, crc32c_hw, crc32c_popcount_xor,
    crc32c_simd_sse2_x4, crc32c_slice_by_8, crc32c_table, is_avx2_available, warm_popcount_basis,
};
#[cfg(target_arch = "x86_64")]
use tren::research::crc_impls::crc32c_simd_avx2_x4;

const TRIALS: usize = 10_000;
const LEN: usize = 128;
const SEED: u64 = 0xA5A5_C3C3_5A5A_3C3Cu64;

fn random_messages(n: usize, len: usize, seed: u64) -> Vec<Vec<u8>> {
    let mut r = StdRng::seed_from_u64(seed);
    (0..n)
        .map(|_| {
            let mut v = vec![0u8; len];
            r.fill_bytes(&mut v);
            v
        })
        .collect()
}

#[test]
fn all_scalar_impls_match_hw_on_10k_random_messages() {
    warm_popcount_basis(LEN);
    let msgs = random_messages(TRIALS, LEN, SEED);
    let mut mismatches = 0u32;
    for (i, m) in msgs.iter().enumerate() {
        let golden = crc32c_hw(m);
        if crc32c_table(m) != golden {
            mismatches += 1;
            eprintln!("table mismatch at {i}");
        }
        if crc32c_slice_by_8(m) != golden {
            mismatches += 1;
            eprintln!("slice_by_8 mismatch at {i}");
        }
        if crc32c_branchless_bitwise(m) != golden {
            mismatches += 1;
            eprintln!("branchless_bitwise mismatch at {i}");
        }
        if crc32c_branchless_bitwise_v2(m) != golden {
            mismatches += 1;
            eprintln!("branchless_bitwise_v2 mismatch at {i}");
        }
        if crc32c_popcount_xor(m) != golden {
            mismatches += 1;
            eprintln!("popcount_xor mismatch at {i}");
        }
    }
    assert_eq!(mismatches, 0, "scalar implementations diverge from hw");
}

#[test]
fn simd_sse2_x4_matches_hw_on_10k_random_messages() {
    let msgs = random_messages(TRIALS * 4, LEN, SEED.wrapping_add(1));
    let mut mismatches = 0u32;
    for (i, chunk) in msgs.chunks_exact(4).enumerate() {
        let view: [&[u8]; 4] = [&chunk[0], &chunk[1], &chunk[2], &chunk[3]];
        let got = crc32c_simd_sse2_x4(view);
        for lane in 0..4 {
            let golden = crc32c_hw(&chunk[lane]);
            if got[lane] != golden {
                mismatches += 1;
                eprintln!("sse2_x4 mismatch at chunk {i} lane {lane}");
            }
        }
    }
    assert_eq!(mismatches, 0, "SSE2 four-way diverges from hw");
}

#[cfg(target_arch = "x86_64")]
#[test]
fn simd_avx2_x4_matches_hw_when_available() {
    if !is_avx2_available() {
        eprintln!("skipping AVX2 consistency test (CPU does not advertise AVX2)");
        return;
    }
    let msgs = random_messages(TRIALS * 4, LEN, SEED.wrapping_add(2));
    let mut mismatches = 0u32;
    for (i, chunk) in msgs.chunks_exact(4).enumerate() {
        let view: [&[u8]; 4] = [&chunk[0], &chunk[1], &chunk[2], &chunk[3]];
        let got = crc32c_simd_avx2_x4(view);
        for lane in 0..4 {
            let golden = crc32c_hw(&chunk[lane]);
            if got[lane] != golden {
                mismatches += 1;
                eprintln!("avx2_x4 mismatch at chunk {i} lane {lane}");
            }
        }
    }
    assert_eq!(mismatches, 0, "AVX2 four-way diverges from hw");
}

#[cfg(not(target_arch = "x86_64"))]
#[test]
fn simd_avx2_x4_skipped_on_non_x86() {
    let _ = is_avx2_available();
    eprintln!("AVX2 test skipped on non-x86_64 target");
}
