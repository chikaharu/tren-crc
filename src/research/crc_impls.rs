//! CRC-32C operator zoo: multiple implementations of the same
//! function, kept side-by-side so the
//! `examples/bench_crc_impls.rs` benchmark can compare them on the
//! same machine and the
//! `tests/crc_impls_consistency.rs` integration test can verify
//! they all agree bit-for-bit with the production hardware
//! implementation (`crc32c::crc32c`).
//!
//! All implementations compute CRC-32C (Castagnoli) in the standard
//! "reflected" form used by every common library:
//!
//! - normal polynomial: `0x1EDC_6F41`
//! - reflected polynomial: `0x82F6_3B78`
//! - initial state: `0xFFFF_FFFF`
//! - final XOR: `0xFFFF_FFFF`
//! - input bits processed LSB-first (i.e. byte `b` is XORed into the
//!   low byte of the state, then 8 right-shift / conditional-XOR
//!   steps follow)
//!
//! Functions:
//!
//! - [`crc32c_hw`]: thin wrapper around the production `crc32c`
//!   crate (the "golden" reference; uses `_mm_crc32_*` on x86 with
//!   SSE 4.2, NEON CRC instructions on AArch64 with the `crypto`
//!   feature, otherwise a software fallback).
//! - [`crc32c_table`]: classical Sarwate single-table 8-bit-at-a-time
//!   software implementation.
//! - [`crc32c_slice_by_8`]: Intel "slice-by-N" variant that consumes
//!   8 bytes per iteration with 8 different lookup tables.
//! - [`crc32c_branchless_bitwise`]: bit-by-bit reference
//!   implementation, branchless via the `mask = (state & 1) * 0xFF...`
//!   trick.
//! - [`crc32c_branchless_bitwise_v2`]: same algorithm rewritten with
//!   the [`super::bool_ops`] helpers (XNOR / NAND / three-input XOR).
//! - [`crc32c_popcount_xor`]: linear-algebra approach exploiting that
//!   CRC-32C is a `\u{007D}-linear map. For a fixed message length L
//!   we precompute a per-length basis `\u{007D}` so that
//!   `crc32c(M) = c_L ^ XOR over set bits i of G_L[i]`. The `_xor`
//!   in the name reflects that the inner loop is a straight XOR
//!   accumulation; we don't actually emit a `POPCNT` instruction.
//!   Optimised for 1024-bit (128-byte) Frame32 inputs but accepts
//!   any length (a per-length basis is cached on demand).
//! - [`crc32c_simd_sse2_x4`]: SSE2 four-way scalar; computes CRC-32C
//!   for four messages of equal length in parallel using the
//!   bit-by-bit branchless algorithm packed across four `i32` lanes
//!   of an `__m128i`. `#[cfg(target_arch = "x86_64")]`-gated.
//! - [`crc32c_simd_avx2_x4`]: same idea on AVX2, packing the four
//!   states across four lanes of an `__m256i` (the upper four lanes
//!   are intentionally unused, matching the spec). `#[cfg(...)]`-
//!   gated and additionally requires the `avx2` runtime feature; the
//!   caller must check via [`is_avx2_available`] or
//!   `is_x86_feature_detected!("avx2")`.

use std::sync::Mutex;
use std::sync::OnceLock;

use super::bool_ops::{lsb_mask, nand, xnor, xor3};

/// CRC-32C reflected polynomial.
pub const POLY_REFLECTED: u32 = 0x82F6_3B78;

// ---------------------------------------------------------------------------
// Hardware reference
// ---------------------------------------------------------------------------

/// Thin wrapper around the production `crc32c` crate. This is the
/// golden value all other implementations are tested against.
#[inline]
pub fn crc32c_hw(data: &[u8]) -> u32 {
    crc32c::crc32c(data)
}

// ---------------------------------------------------------------------------
// Sarwate single-table 8-bit
// ---------------------------------------------------------------------------

const fn build_table0() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            let lsb = crc & 1;
            // const fn cannot call wrapping_neg via trait; fall back
            // to manual `0u32 - lsb` using wrapping_sub.
            let mask = 0u32.wrapping_sub(lsb);
            crc = (crc >> 1) ^ (POLY_REFLECTED & mask);
            j += 1;
        }
        t[i] = crc;
        i += 1;
    }
    t
}

const SARWATE_TABLE: [u32; 256] = build_table0();

/// Sarwate 8-bit-at-a-time table-driven CRC-32C.
pub fn crc32c_table(data: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFFu32;
    for &b in data {
        let idx = ((state ^ b as u32) & 0xFF) as usize;
        state = (state >> 8) ^ SARWATE_TABLE[idx];
    }
    !state
}

// ---------------------------------------------------------------------------
// Slice-by-8 (Intel)
// ---------------------------------------------------------------------------

const fn build_sliceby8() -> [[u32; 256]; 8] {
    let mut tables = [[0u32; 256]; 8];
    tables[0] = build_table0();
    let mut t = 1;
    while t < 8 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[t - 1][i];
            tables[t][i] = (prev >> 8) ^ tables[0][(prev & 0xFF) as usize];
            i += 1;
        }
        t += 1;
    }
    tables
}

const SLICE8_TABLES: [[u32; 256]; 8] = build_sliceby8();

/// Intel slice-by-8 CRC-32C: 8 bytes per loop iteration.
pub fn crc32c_slice_by_8(data: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i + 8 <= data.len() {
        let lo = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        let hi = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]);
        let s = state ^ lo;
        state = SLICE8_TABLES[7][(s & 0xFF) as usize]
            ^ SLICE8_TABLES[6][((s >> 8) & 0xFF) as usize]
            ^ SLICE8_TABLES[5][((s >> 16) & 0xFF) as usize]
            ^ SLICE8_TABLES[4][((s >> 24) & 0xFF) as usize]
            ^ SLICE8_TABLES[3][(hi & 0xFF) as usize]
            ^ SLICE8_TABLES[2][((hi >> 8) & 0xFF) as usize]
            ^ SLICE8_TABLES[1][((hi >> 16) & 0xFF) as usize]
            ^ SLICE8_TABLES[0][((hi >> 24) & 0xFF) as usize];
        i += 8;
    }
    while i < data.len() {
        let idx = ((state ^ data[i] as u32) & 0xFF) as usize;
        state = (state >> 8) ^ SARWATE_TABLE[idx];
        i += 1;
    }
    !state
}

// ---------------------------------------------------------------------------
// Branchless bit-by-bit
// ---------------------------------------------------------------------------

/// Bit-by-bit branchless CRC-32C using the canonical
/// `mask = (state & 1).wrapping_neg()` idiom.
pub fn crc32c_branchless_bitwise(data: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFFu32;
    for &b in data {
        state ^= b as u32;
        for _ in 0..8 {
            let mask = (state & 1).wrapping_neg();
            state = (state >> 1) ^ (POLY_REFLECTED & mask);
        }
    }
    !state
}

/// Same algorithm as [`crc32c_branchless_bitwise`] but expressed
/// using [`super::bool_ops`] helpers (XNOR / NAND / three-input XOR).
/// Functionally identical; differs only in instruction selection
/// pressure on the back-end.
pub fn crc32c_branchless_bitwise_v2(data: &[u8]) -> u32 {
    let mut state = 0xFFFF_FFFFu32;
    for &b in data {
        // state ^= b  -> express as XNOR(state, ~b) which is the
        // identity but routes through the helper.
        state = xor3(state, b as u32, 0);
        for _ in 0..8 {
            let mask = lsb_mask(state);
            // (state >> 1) ^ (POLY & mask)
            // == (state >> 1) ^ !nand(POLY, mask)
            // == xor3(state >> 1, !nand(POLY, mask), 0)
            // The XNOR helper is exercised through the final-XOR step
            // below to keep all three helpers on the hot path.
            let term = !nand(POLY_REFLECTED, mask);
            state = xor3(state >> 1, term, 0);
        }
    }
    // Final XOR with all-ones, expressed via XNOR(state, 0).
    xnor(state, 0)
}

// ---------------------------------------------------------------------------
// Linear basis (popcount-XOR)
// ---------------------------------------------------------------------------

/// Cached per-length basis: maps `length_in_bytes` to
/// `(constant_offset, basis[0..length*8])`.
fn basis_cache() -> &'static Mutex<std::collections::HashMap<usize, (u32, Vec<u32>)>> {
    static CACHE: OnceLock<Mutex<std::collections::HashMap<usize, (u32, Vec<u32>)>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

fn build_basis(length_bytes: usize) -> (u32, Vec<u32>) {
    // CRC-32C is affine over `\u{007D}^L`: there exists a constant `c`
    // and a `\u{007D}`-linear map `f` such that
    //     crc(M) = c ^ f(M)
    // with `f(M) = XOR over set bits i of G[i]`.
    //
    // We recover `c` as `crc(0)` and each `G[i]` as
    // `crc(unit vector at bit i) ^ c` (the offsets cancel).
    let zero = vec![0u8; length_bytes];
    let constant = crc32c_hw(&zero);
    let mut basis = Vec::with_capacity(length_bytes * 8);
    let mut buf = vec![0u8; length_bytes];
    for byte_i in 0..length_bytes {
        for bit_i in 0..8 {
            buf[byte_i] = 1u8 << bit_i;
            basis.push(crc32c_hw(&buf) ^ constant);
        }
        buf[byte_i] = 0;
    }
    (constant, basis)
}

/// Linear-basis "popcount-XOR" CRC-32C.
///
/// Builds (and caches) a per-length basis `B_L` such that
/// `crc(M) = c_L ^ XOR over set bits i of B_L[i]`. The 1024-bit
/// (128-byte) `Frame32` case is the intended hot path; other lengths
/// work but pay an O(L^2) one-time cost the first time the length is
/// seen.
pub fn crc32c_popcount_xor(data: &[u8]) -> u32 {
    let len = data.len();
    let (constant, basis) = {
        let cache = basis_cache();
        let mut g = cache.lock().unwrap();
        if let Some(entry) = g.get(&len) {
            (entry.0, entry.1.clone())
        } else {
            let entry = build_basis(len);
            g.insert(len, entry.clone());
            entry
        }
    };
    let mut acc = 0u32;
    for byte_i in 0..len {
        let b = data[byte_i];
        for bit_i in 0..8 {
            // Branchless: XOR by `basis[i]` masked with `lsb_mask(b >> bit_i)`.
            let m = lsb_mask((b >> bit_i) as u32);
            acc ^= basis[byte_i * 8 + bit_i] & m;
        }
    }
    constant ^ acc
}

/// Pre-build the basis for a given length. Tests and benchmarks call
/// this so the per-length O(L^2) cost is excluded from per-call
/// latency measurements.
pub fn warm_popcount_basis(length_bytes: usize) {
    let cache = basis_cache();
    let mut g = cache.lock().unwrap();
    if !g.contains_key(&length_bytes) {
        g.insert(length_bytes, build_basis(length_bytes));
    }
}

// ---------------------------------------------------------------------------
// SIMD: SSE2 four-way scalar
// ---------------------------------------------------------------------------

/// Returns `true` if the current x86_64 CPU advertises AVX2.
#[cfg(target_arch = "x86_64")]
pub fn is_avx2_available() -> bool {
    is_x86_feature_detected!("avx2")
}

/// Returns `false` on non-x86_64 targets.
#[cfg(not(target_arch = "x86_64"))]
pub fn is_avx2_available() -> bool {
    false
}

/// SSE2 four-way scalar CRC-32C: computes CRC-32C of four messages
/// of equal length in parallel using the bit-by-bit branchless
/// algorithm packed across four `i32` lanes of an `__m128i`.
#[cfg(target_arch = "x86_64")]
pub fn crc32c_simd_sse2_x4(messages: [&[u8]; 4]) -> [u32; 4] {
    let len = messages[0].len();
    assert!(
        messages.iter().all(|m| m.len() == len),
        "all messages must have the same length"
    );
    // Safety: SSE2 is part of the x86_64 baseline ABI, so we don't
    // need a runtime check on this target.
    unsafe { crc32c_simd_sse2_x4_inner(messages, len) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn crc32c_simd_sse2_x4_inner(messages: [&[u8]; 4], len: usize) -> [u32; 4] {
    use std::arch::x86_64::*;

    let mut state = _mm_set1_epi32(-1i32);
    let poly = _mm_set1_epi32(POLY_REFLECTED as i32);
    let one = _mm_set1_epi32(1);

    for i in 0..len {
        let bytes = _mm_setr_epi32(
            messages[0][i] as i32,
            messages[1][i] as i32,
            messages[2][i] as i32,
            messages[3][i] as i32,
        );
        state = _mm_xor_si128(state, bytes);
        for _ in 0..8 {
            // mask = -lsb : 0xFFFFFFFF if low bit set else 0.
            let lsb = _mm_and_si128(state, one);
            let mask = _mm_cmpeq_epi32(lsb, one);
            let xor_term = _mm_and_si128(poly, mask);
            state = _mm_xor_si128(_mm_srli_epi32(state, 1), xor_term);
        }
    }

    // Final XOR with all-ones.
    let state = _mm_xor_si128(state, _mm_set1_epi32(-1i32));

    let mut out = [0u32; 4];
    _mm_storeu_si128(out.as_mut_ptr() as *mut __m128i, state);
    out
}

/// SSE2 four-way scalar CRC-32C — non-x86_64 fallback that just
/// forwards each message through [`crc32c_branchless_bitwise`].
#[cfg(not(target_arch = "x86_64"))]
pub fn crc32c_simd_sse2_x4(messages: [&[u8]; 4]) -> [u32; 4] {
    [
        crc32c_branchless_bitwise(messages[0]),
        crc32c_branchless_bitwise(messages[1]),
        crc32c_branchless_bitwise(messages[2]),
        crc32c_branchless_bitwise(messages[3]),
    ]
}

// ---------------------------------------------------------------------------
// SIMD: AVX2 four-way scalar (using lower half of YMM)
// ---------------------------------------------------------------------------

/// AVX2 four-way scalar CRC-32C. Same algorithm as
/// [`crc32c_simd_sse2_x4`] but expressed on `__m256i` so that LLVM
/// emits VEX-encoded instructions and can take advantage of AVX2's
/// non-destructive three-operand encoding. The upper four lanes of
/// the `__m256i` are intentionally unused so the function signature
/// matches the spec.
///
/// # Safety
/// Caller must ensure the running CPU supports AVX2; query via
/// [`is_avx2_available`] or `is_x86_feature_detected!("avx2")`.
#[cfg(target_arch = "x86_64")]
pub fn crc32c_simd_avx2_x4(messages: [&[u8]; 4]) -> [u32; 4] {
    let len = messages[0].len();
    assert!(
        messages.iter().all(|m| m.len() == len),
        "all messages must have the same length"
    );
    assert!(is_avx2_available(), "AVX2 not available on this CPU");
    unsafe { crc32c_simd_avx2_x4_inner(messages, len) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn crc32c_simd_avx2_x4_inner(messages: [&[u8]; 4], len: usize) -> [u32; 4] {
    use std::arch::x86_64::*;

    // Only the lower four `i32` lanes are used. Initialise the upper
    // four to zero so they never trigger spurious masked XORs.
    let mut state = _mm256_setr_epi32(-1, -1, -1, -1, 0, 0, 0, 0);
    let poly = _mm256_setr_epi32(
        POLY_REFLECTED as i32,
        POLY_REFLECTED as i32,
        POLY_REFLECTED as i32,
        POLY_REFLECTED as i32,
        0, 0, 0, 0,
    );
    let one = _mm256_setr_epi32(1, 1, 1, 1, 0, 0, 0, 0);

    for i in 0..len {
        let bytes = _mm256_setr_epi32(
            messages[0][i] as i32,
            messages[1][i] as i32,
            messages[2][i] as i32,
            messages[3][i] as i32,
            0, 0, 0, 0,
        );
        state = _mm256_xor_si256(state, bytes);
        for _ in 0..8 {
            let lsb = _mm256_and_si256(state, one);
            let mask = _mm256_cmpeq_epi32(lsb, one);
            let xor_term = _mm256_and_si256(poly, mask);
            state = _mm256_xor_si256(_mm256_srli_epi32(state, 1), xor_term);
        }
    }

    // Final XOR with all-ones in the lower four lanes only.
    let final_mask = _mm256_setr_epi32(-1, -1, -1, -1, 0, 0, 0, 0);
    let state = _mm256_xor_si256(state, final_mask);

    let mut out = [0u32; 8];
    _mm256_storeu_si256(out.as_mut_ptr() as *mut __m256i, state);
    [out[0], out[1], out[2], out[3]]
}

#[cfg(not(target_arch = "x86_64"))]
pub fn crc32c_simd_avx2_x4(messages: [&[u8]; 4]) -> [u32; 4] {
    crc32c_simd_sse2_x4(messages)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{RngCore, SeedableRng, rngs::StdRng};

    fn rnd(seed: u64, len: usize) -> Vec<u8> {
        let mut r = StdRng::seed_from_u64(seed);
        let mut v = vec![0u8; len];
        r.fill_bytes(&mut v);
        v
    }

    #[test]
    fn known_empty() {
        // CRC-32C of the empty string is 0x00000000.
        assert_eq!(crc32c_hw(b""), 0);
        assert_eq!(crc32c_table(b""), 0);
        assert_eq!(crc32c_slice_by_8(b""), 0);
        assert_eq!(crc32c_branchless_bitwise(b""), 0);
        assert_eq!(crc32c_branchless_bitwise_v2(b""), 0);
    }

    #[test]
    fn known_check_value_123456789() {
        // Standard CRC-32C check value for the ASCII string "123456789".
        let msg = b"123456789";
        assert_eq!(crc32c_hw(msg), 0xE3069283);
        assert_eq!(crc32c_table(msg), 0xE3069283);
        assert_eq!(crc32c_slice_by_8(msg), 0xE3069283);
        assert_eq!(crc32c_branchless_bitwise(msg), 0xE3069283);
        assert_eq!(crc32c_branchless_bitwise_v2(msg), 0xE3069283);
    }

    #[test]
    fn table_matches_hw() {
        for seed in 0..16 {
            let msg = rnd(seed, 128);
            assert_eq!(crc32c_table(&msg), crc32c_hw(&msg), "seed {seed}");
        }
    }

    #[test]
    fn slice8_matches_hw_various_lengths() {
        for len in [0usize, 1, 7, 8, 9, 15, 16, 31, 64, 128, 129, 1023, 1024] {
            for seed in 0..4 {
                let msg = rnd((len * 31 + seed) as u64, len);
                assert_eq!(crc32c_slice_by_8(&msg), crc32c_hw(&msg), "len={len} seed={seed}");
            }
        }
    }

    #[test]
    fn branchless_matches_hw() {
        for seed in 0..16 {
            let msg = rnd(seed, 128);
            assert_eq!(crc32c_branchless_bitwise(&msg), crc32c_hw(&msg), "seed {seed}");
            assert_eq!(crc32c_branchless_bitwise_v2(&msg), crc32c_hw(&msg), "seed {seed}");
        }
    }

    #[test]
    fn popcount_xor_matches_hw_1024() {
        warm_popcount_basis(128);
        for seed in 0..32 {
            let msg = rnd(seed, 128);
            assert_eq!(crc32c_popcount_xor(&msg), crc32c_hw(&msg), "seed {seed}");
        }
    }

    #[test]
    fn popcount_xor_matches_hw_other_lengths() {
        for len in [1usize, 8, 16, 64, 256] {
            warm_popcount_basis(len);
            for seed in 0..4 {
                let msg = rnd((len * 7 + seed) as u64, len);
                assert_eq!(
                    crc32c_popcount_xor(&msg),
                    crc32c_hw(&msg),
                    "len={len} seed={seed}"
                );
            }
        }
    }

    #[test]
    fn simd_sse2_x4_matches_hw() {
        let msgs: [Vec<u8>; 4] = [
            rnd(101, 128),
            rnd(202, 128),
            rnd(303, 128),
            rnd(404, 128),
        ];
        let view: [&[u8]; 4] = [&msgs[0], &msgs[1], &msgs[2], &msgs[3]];
        let got = crc32c_simd_sse2_x4(view);
        for i in 0..4 {
            assert_eq!(got[i], crc32c_hw(&msgs[i]), "lane {i}");
        }
    }

    #[test]
    fn simd_avx2_x4_matches_hw_when_available() {
        if !is_avx2_available() {
            eprintln!("skipping AVX2 test (CPU does not advertise AVX2)");
            return;
        }
        let msgs: [Vec<u8>; 4] = [
            rnd(11, 128),
            rnd(22, 128),
            rnd(33, 128),
            rnd(44, 128),
        ];
        let view: [&[u8]; 4] = [&msgs[0], &msgs[1], &msgs[2], &msgs[3]];
        let got = crc32c_simd_avx2_x4(view);
        for i in 0..4 {
            assert_eq!(got[i], crc32c_hw(&msgs[i]), "lane {i}");
        }
    }

    #[test]
    fn all_impls_agree_on_short_messages() {
        for len in [0usize, 1, 4, 16, 32, 128] {
            warm_popcount_basis(len);
            for seed in 0..4 {
                let msg = rnd((len * 13 + seed) as u64, len);
                let golden = crc32c_hw(&msg);
                assert_eq!(crc32c_table(&msg), golden);
                assert_eq!(crc32c_slice_by_8(&msg), golden);
                assert_eq!(crc32c_branchless_bitwise(&msg), golden);
                assert_eq!(crc32c_branchless_bitwise_v2(&msg), golden);
                assert_eq!(crc32c_popcount_xor(&msg), golden);
            }
        }
    }
}
