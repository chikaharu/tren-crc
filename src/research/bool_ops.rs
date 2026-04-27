//! Branchless boolean helpers used by the CRC-32C operator
//! experiments.
//!
//! These are deliberately `#[inline]` and operate on word-sized
//! integers so the compiler can lift them into the surrounding
//! expression. They exist primarily so the alternative
//! [`crate::research::crc_impls::crc32c_branchless_bitwise_v2`]
//! implementation can be expressed in terms of XNOR / NAND /
//! three-input XOR rather than the canonical XOR / AND / NOT mix used
//! in [`crate::research::crc_impls::crc32c_branchless_bitwise`]. Both
//! evaluate to the same function but exercise different instruction
//! selection paths in the back-end (e.g. on x86_64 LLVM may fuse a
//! few of them into `ANDN` / `XOR` sequences differently).

/// Bitwise XNOR: `!(a ^ b)`.
#[inline(always)]
pub fn xnor(a: u32, b: u32) -> u32 {
    !(a ^ b)
}

/// Bitwise NAND: `!(a & b)`.
#[inline(always)]
pub fn nand(a: u32, b: u32) -> u32 {
    !(a & b)
}

/// Three-input XOR: `a ^ b ^ c`.
///
/// Some architectures (notably AVX-512 on x86 and SVE2 on ARM) have a
/// dedicated `XOR3` instruction; LLVM is usually able to recognise
/// the pattern when surfaced as a separate function.
#[inline(always)]
pub fn xor3(a: u32, b: u32, c: u32) -> u32 {
    a ^ b ^ c
}

/// Branchless conditional mask: returns `0xFFFF_FFFF` when `bit` is
/// 1, else `0`. Implemented as `0u32.wrapping_sub(bit & 1)` so it is
/// expressed as a subtract and an AND; on x86_64 LLVM often emits
/// `NEG`.
#[inline(always)]
pub fn lsb_mask(bit: u32) -> u32 {
    0u32.wrapping_sub(bit & 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xnor_table() {
        assert_eq!(xnor(0, 0), !0u32);
        assert_eq!(xnor(0xFFFF_FFFF, 0xFFFF_FFFF), !0u32);
        assert_eq!(xnor(0xAAAA_AAAA, 0x5555_5555), 0);
        assert_eq!(xnor(0xF0F0_F0F0, 0x0F0F_0F0F), 0);
    }

    #[test]
    fn nand_table() {
        assert_eq!(nand(0, 0), !0u32);
        assert_eq!(nand(0xFFFF_FFFF, 0xFFFF_FFFF), 0);
        assert_eq!(nand(0xAAAA_AAAA, 0x5555_5555), !0u32);
        assert_eq!(nand(0xFFFF_0000, 0x00FF_0000), 0xFF00_FFFF);
    }

    #[test]
    fn xor3_associates() {
        let triples = [(0u32, 0, 0), (1, 0, 0), (0xDEAD_BEEF, 0xCAFE_F00D, 0xFACE_B00C)];
        for &(a, b, c) in &triples {
            assert_eq!(xor3(a, b, c), (a ^ b) ^ c);
            assert_eq!(xor3(a, b, c), a ^ (b ^ c));
        }
    }

    #[test]
    fn lsb_mask_is_branchless_select() {
        assert_eq!(lsb_mask(0), 0);
        assert_eq!(lsb_mask(1), 0xFFFF_FFFF);
        assert_eq!(lsb_mask(2), 0); // only bit 0 considered
        assert_eq!(lsb_mask(3), 0xFFFF_FFFF);
        assert_eq!(lsb_mask(0xFFFF_FFFE), 0);
        assert_eq!(lsb_mask(0xFFFF_FFFF), 0xFFFF_FFFF);
    }
}
