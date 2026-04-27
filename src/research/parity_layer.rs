//! Experimental parity-layer alternatives to the production CRC-32C diagonal.
//!
//! The production tren-crc layout (`Crc32Only`) uses all 32 diagonal positions
//! (bit `i` of slot `i`, for `i ∈ 0..32`) to hold a single CRC-32C syndrome
//! computed over the body bytes (the 32 × `u32` slots with the diagonal bits
//! cleared). This module explores re-budgeting those 32 bits:
//!
//! - [`ParityLayout::RowXorPlusCrc`] `{ k }` — first `k` diagonal positions hold
//!   per-row XOR parity (one bit per slot for the lowest `k` slots), the
//!   remaining `32 − k` positions hold the lower `32 − k` bits of the CRC-32C
//!   syndrome.
//! - [`ParityLayout::ColXorPlusCrc`] `{ k }` — first `k` diagonal positions hold
//!   per-column XOR parity (one bit per bit-position for the lowest `k`
//!   columns, XOR-ed across all 32 slots with diagonal cleared), the remaining
//!   `32 − k` positions hold the lower `32 − k` bits of the CRC-32C syndrome.
//! - [`ParityLayout::SplitOddEven16`] — the 16 *even* diagonal positions
//!   (bit `2j` of slot `2j` for `j ∈ 0..16`) hold a CRC-16/IBM (poly `0x8005`,
//!   reflected `0xA001`, has `(x+1)` factor → all odd-weight body errors are
//!   deterministically detected) computed over the full diagonal-cleared body
//!   bytes. The 16 *odd* diagonal positions (bit `2j+1` of slot `2j+1`) hold a
//!   CRC-16/CCITT-FALSE (poly `0x1021`, init `0xFFFF`) computed over the
//!   512-bit projection that takes only the *even* bit positions
//!   (`b ∈ {0, 2, …, 30}`) of every slot's diagonal-cleared `u32`.
//!
//! All layouts are linear over GF(2) and gated behind the `experimental`
//! feature; nothing here is wired into the production code path.

use crate::Frame32;
use crc32c::crc32c;

/// Mask for slot `i` with the diagonal bit (bit `i`) cleared.
const fn body_mask(i: usize) -> u32 {
    !(1u32 << i)
}

/// Serialize a frame to 128 little-endian bytes with the diagonal cleared.
fn body_bytes(frame: &Frame32) -> [u8; 128] {
    let mut out = [0u8; 128];
    for i in 0..32 {
        let cleared = frame.0[i] & body_mask(i);
        out[i * 4..i * 4 + 4].copy_from_slice(&cleared.to_le_bytes());
    }
    out
}

/// Extract the 16 even-bit-position bits of a slot (bits `0, 2, …, 30`).
fn even_bit_positions(slot: u32) -> u16 {
    let mut out: u16 = 0;
    for b in 0..16 {
        if (slot >> (2 * b)) & 1 == 1 {
            out |= 1u16 << b;
        }
    }
    out
}

/// Build the 64-byte projection (even bit positions of every slot, with
/// diagonal cleared). 32 slots × 16 even-position bits = 512 bits = 64 bytes.
fn even_bit_projection_bytes(frame: &Frame32) -> [u8; 64] {
    let mut out = [0u8; 64];
    for i in 0..32 {
        let cleared = frame.0[i] & body_mask(i);
        let proj = even_bit_positions(cleared);
        out[i * 2..i * 2 + 2].copy_from_slice(&proj.to_le_bytes());
    }
    out
}

/// CRC-16/IBM (a.k.a. CRC-16/ARC): poly `0x8005`, init `0x0000`,
/// `refin=true`, `refout=true`, `xorout=0x0000`. Reflected poly is `0xA001`.
/// Has `(x+1)` as a factor → all odd-weight body errors are deterministically
/// detected.
fn crc16_ibm(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            if crc & 1 == 1 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, `refin=false`,
/// `refout=false`, `xorout=0x0000`. The polynomial `x^16 + x^12 + x^5 + 1`
/// has 4 terms (even count) → it *also* contains `(x+1)` as a factor; the
/// "specialization for even errors" comes from being computed over the
/// 512-bit even-bit-position projection of the body, where many even-weight
/// body errors map to odd-weight projection errors and are caught
/// deterministically by the `(x+1)` factor.
fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 == 0x8000 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Alternative diagonal-bit budgets for `Frame32`'s 32 syndrome bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParityLayout {
    /// Production layout: full 32-bit CRC-32C, all 32 diagonal positions.
    Crc32Only,
    /// First `k` diagonal positions = row XOR parity; remaining `32 − k`
    /// positions = lower `32 − k` bits of CRC-32C.
    RowXorPlusCrc { k: u8 },
    /// First `k` diagonal positions = column XOR parity; remaining `32 − k`
    /// positions = lower `32 − k` bits of CRC-32C.
    ColXorPlusCrc { k: u8 },
    /// 16 even diagonal positions = CRC-16/IBM over body, 16 odd diagonal
    /// positions = CRC-16/CCITT-FALSE over the 512-bit even-bit-position
    /// projection.
    SplitOddEven16,
}

/// Result of [`verify_with_layout`] when stored diagonal bits don't match
/// the recomputed syndrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// The stored diagonal bits don't match the recomputed syndrome.
    Mismatch,
}

/// Compute the diagonal value for `frame` under `layout`. Bit `i` of the
/// returned `u32` is the syndrome bit destined for bit `i` of slot `i`.
/// The frame's existing diagonal bits are ignored (treated as zero) during
/// computation so the function is idempotent: calling it on a frame whose
/// diagonal already holds the correct syndrome returns the same value.
fn compute_diagonal(layout: ParityLayout, frame: &Frame32) -> u32 {
    match layout {
        ParityLayout::Crc32Only => crc32c(&body_bytes(frame)),
        ParityLayout::RowXorPlusCrc { k } => {
            let k = (k as usize).min(32);
            let crc = crc32c(&body_bytes(frame));
            let mut diag: u32 = 0;
            for i in 0..k {
                let cleared = frame.0[i] & body_mask(i);
                let parity = (cleared.count_ones() & 1) as u32;
                diag |= parity << i;
            }
            for i in k..32 {
                let bit = (crc >> (i - k)) & 1;
                diag |= bit << i;
            }
            diag
        }
        ParityLayout::ColXorPlusCrc { k } => {
            let k = (k as usize).min(32);
            let crc = crc32c(&body_bytes(frame));
            let mut diag: u32 = 0;
            for c in 0..k {
                let mut p: u32 = 0;
                for i in 0..32 {
                    let cleared = frame.0[i] & body_mask(i);
                    p ^= (cleared >> c) & 1;
                }
                diag |= p << c;
            }
            for i in k..32 {
                let bit = (crc >> (i - k)) & 1;
                diag |= bit << i;
            }
            diag
        }
        ParityLayout::SplitOddEven16 => {
            let crc_ibm = crc16_ibm(&body_bytes(frame));
            let crc_ccitt = crc16_ccitt(&even_bit_projection_bytes(frame));
            let mut diag: u32 = 0;
            // Even diagonal positions: bit b of crc_ibm → bit 2b of diag.
            for b in 0..16 {
                let bit = ((crc_ibm >> b) & 1) as u32;
                diag |= bit << (2 * b);
            }
            // Odd diagonal positions: bit b of crc_ccitt → bit 2b+1 of diag.
            for b in 0..16 {
                let bit = ((crc_ccitt >> b) & 1) as u32;
                diag |= bit << (2 * b + 1);
            }
            diag
        }
    }
}

/// Set bit `i` of slot `i` to bit `i` of `diag`, leaving all other bits
/// untouched.
fn write_diagonal(frame: &mut Frame32, diag: u32) {
    for i in 0..32 {
        let bit = (diag >> i) & 1;
        frame.0[i] = (frame.0[i] & body_mask(i)) | (bit << i);
    }
}

/// Read the bits at the diagonal positions (bit `i` of slot `i`) into a
/// single `u32`.
fn read_diagonal(frame: &Frame32) -> u32 {
    let mut diag: u32 = 0;
    for i in 0..32 {
        let bit = (frame.0[i] >> i) & 1;
        diag |= bit << i;
    }
    diag
}

/// Compute the layout's syndrome for `frame` and write it into the diagonal
/// positions, leaving non-diagonal body bits unchanged.
pub fn update_with_layout(layout: ParityLayout, frame: &mut Frame32) {
    let diag = compute_diagonal(layout, frame);
    write_diagonal(frame, diag);
}

/// Verify that the diagonal positions of `frame` match the recomputed
/// syndrome under `layout`. Returns `Err(VerifyError::Mismatch)` on any
/// mismatch.
pub fn verify_with_layout(
    layout: ParityLayout,
    frame: &Frame32,
) -> Result<(), VerifyError> {
    let stored = read_diagonal(frame);
    let expected = compute_diagonal(layout, frame);
    if stored == expected {
        Ok(())
    } else {
        Err(VerifyError::Mismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> Frame32 {
        let mut f = Frame32::new();
        for i in 0..32 {
            f.0[i] = (i as u32).wrapping_mul(0x9E37_79B1);
        }
        f
    }

    #[test]
    fn round_trip_crc32_only() {
        let mut f = sample_frame();
        update_with_layout(ParityLayout::Crc32Only, &mut f);
        assert_eq!(verify_with_layout(ParityLayout::Crc32Only, &f), Ok(()));
    }

    #[test]
    fn round_trip_row_xor_plus_crc_k4() {
        let mut f = sample_frame();
        let layout = ParityLayout::RowXorPlusCrc { k: 4 };
        update_with_layout(layout, &mut f);
        assert_eq!(verify_with_layout(layout, &f), Ok(()));
    }

    #[test]
    fn round_trip_col_xor_plus_crc_k8() {
        let mut f = sample_frame();
        let layout = ParityLayout::ColXorPlusCrc { k: 8 };
        update_with_layout(layout, &mut f);
        assert_eq!(verify_with_layout(layout, &f), Ok(()));
    }

    #[test]
    fn round_trip_split_odd_even_16() {
        let mut f = sample_frame();
        update_with_layout(ParityLayout::SplitOddEven16, &mut f);
        assert_eq!(
            verify_with_layout(ParityLayout::SplitOddEven16, &f),
            Ok(())
        );
    }

    #[test]
    fn flip_one_body_bit_breaks_split_verify() {
        let mut f = sample_frame();
        update_with_layout(ParityLayout::SplitOddEven16, &mut f);
        // Flip a non-diagonal body bit (slot 7, bit 5; diagonal of slot 7
        // is bit 7).
        f.0[7] ^= 1u32 << 5;
        assert_eq!(
            verify_with_layout(ParityLayout::SplitOddEven16, &f),
            Err(VerifyError::Mismatch)
        );
    }

    #[test]
    fn diagonal_extraction_round_trip() {
        let mut f = sample_frame();
        let target: u32 = 0xDEAD_BEEF;
        write_diagonal(&mut f, target);
        assert_eq!(read_diagonal(&f), target);
    }

    #[test]
    fn even_bit_positions_extracts_correctly() {
        // 0x55555555 = bits 0,2,4,...,30 set → result = 0xFFFF.
        assert_eq!(even_bit_positions(0x5555_5555), 0xFFFF);
        // 0xAAAAAAAA = bits 1,3,5,...,31 set → result = 0.
        assert_eq!(even_bit_positions(0xAAAA_AAAA), 0);
        // 0x00000001 = bit 0 set → result = 0x0001.
        assert_eq!(even_bit_positions(0x0000_0001), 0x0001);
    }

    #[test]
    fn crc16_ibm_known_vector() {
        // CRC-16/ARC of "123456789" = 0xBB3D.
        assert_eq!(crc16_ibm(b"123456789"), 0xBB3D);
    }

    #[test]
    fn crc16_ccitt_false_known_vector() {
        // CRC-16/CCITT-FALSE of "123456789" = 0x29B1.
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
    }
}
