//! Bit-error injectors for `Frame32`.
//!
//! All injectors mutate the frame in place. They use the natural
//! slot-major / LSB-first flat indexing: bit `k` (0 ≤ k < 1024) lives
//! at slot `k / 32`, bit position `k % 32` of that slot's `u32`.
//!
//! This indexing is independent of the wire-byte layout used by
//! [`Frame32::to_bytes`]; experiments that need to compare against the
//! older `flip_bit(bytes, idx)` style of `tests/crc_detect.rs` should
//! be aware that those two indexings are *not* equivalent (the wire
//! layout is big-endian per slot, so wire bit 0 maps to slot 0 bit 31,
//! not slot 0 bit 0).

use crate::Frame32;
use rand::Rng;
use std::collections::HashSet;

/// Flip the bits at the given `(slot, bit)` positions. Each pair is
/// XOR-applied independently, so passing the same pair twice cancels
/// the flip.
pub fn flip_bits(frame: &mut Frame32, positions: &[(usize, usize)]) {
    for &(slot, bit) in positions {
        debug_assert!(slot < 32, "slot {} >= 32", slot);
        debug_assert!(bit < 32, "bit {} >= 32", bit);
        frame.0[slot] ^= 1u32 << bit;
    }
}

/// Pick `n` distinct `(slot, bit)` positions uniformly at random from
/// the 1024 bits of the frame, flip them, and return the chosen
/// positions. Behaviour for `n == 0` is well-defined: nothing is
/// flipped and an empty `Vec` is returned.
///
/// Panics if `n > 1024` because there are only that many distinct
/// positions in the frame.
pub fn flip_random_n<R: Rng>(
    frame: &mut Frame32,
    n: usize,
    rng: &mut R,
) -> Vec<(usize, usize)> {
    assert!(n <= 1024, "cannot flip more than 1024 distinct bits");
    let mut chosen: HashSet<usize> = HashSet::with_capacity(n);
    while chosen.len() < n {
        chosen.insert(rng.gen_range(0..1024));
    }
    let positions: Vec<(usize, usize)> =
        chosen.into_iter().map(|i| (i / 32, i % 32)).collect();
    flip_bits(frame, &positions);
    positions
}

/// Flip a contiguous burst of `len` bits starting at flat index
/// `start`. Panics if `start + len > 1024`.
pub fn flip_burst(frame: &mut Frame32, start: usize, len: usize) {
    assert!(
        start.checked_add(len).map_or(false, |e| e <= 1024),
        "burst out of range: start={}, len={}",
        start,
        len
    );
    for i in start..start + len {
        let slot = i / 32;
        let bit = i % 32;
        frame.0[slot] ^= 1u32 << bit;
    }
}
