//! Scatter pattern abstraction.
//!
//! A [`ScatterPattern`] decides where the bits of a 32-bit CRC are
//! stored inside a [`Frame32`]. The current production wire format
//! corresponds exactly to [`Diagonal`]: CRC bit `i` lives at bit `i`
//! of slot `i`. Other patterns rearrange these "reserved" positions
//! and let downstream experiments compare the resulting detection
//! behaviour and statistical properties.
//!
//! All patterns expose `clear_reserved`, `write`, and `read` so the
//! "compute CRC over a body with the reserved bits set to zero, then
//! write the CRC to the reserved positions" recipe used by
//! [`Frame32::update_crc`] generalises uniformly.
//!
//! The [`Hadamard`] pattern is special: it carries only 8 information
//! bits, replicated 4× each, with majority vote on read. Comparing
//! its read-back value against a 32-bit CRC requires masking both
//! sides with [`ScatterPattern::mask`].

use crate::Frame32;

/// Trait for "where do the 32 (or fewer) CRC bits live in a Frame32?".
pub trait ScatterPattern {
    /// Number of CRC information bits actually carried (≤32).
    fn information_bits(&self) -> u8;

    /// Mask of meaningful CRC bits. Both the value passed to `write`
    /// and the value returned by `read` are guaranteed to be `& mask`.
    fn mask(&self) -> u32 {
        let b = self.information_bits();
        if b >= 32 {
            u32::MAX
        } else {
            (1u32 << b) - 1
        }
    }

    /// Set every reserved position in `frame` to 0. Used to compute
    /// the "body" against which the CRC is taken.
    fn clear_reserved(&self, frame: &mut Frame32);

    /// Write the (low `information_bits()`) bits of `crc` into the
    /// reserved positions of `frame`. Reserved bits are overwritten
    /// (not XOR-ed); non-reserved bits are untouched.
    fn write(&self, frame: &mut Frame32, crc: u32);

    /// Read the CRC value from the reserved positions of `frame`.
    /// Bits above `information_bits()` are 0.
    fn read(&self, frame: &Frame32) -> u32;
}

// ─── Diagonal (production layout) ──────────────────────────────────────────

/// CRC bit `i` lives at bit `i` of slot `i`. This is the layout used
/// by [`Frame32::update_crc`] / [`Frame32::verify_crc`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Diagonal;

impl ScatterPattern for Diagonal {
    fn information_bits(&self) -> u8 {
        32
    }

    fn clear_reserved(&self, frame: &mut Frame32) {
        for i in 0..32 {
            frame.0[i] &= !(1u32 << i);
        }
    }

    fn write(&self, frame: &mut Frame32, crc: u32) {
        for i in 0..32 {
            let mask = 1u32 << i;
            if (crc >> i) & 1 == 1 {
                frame.0[i] |= mask;
            } else {
                frame.0[i] &= !mask;
            }
        }
    }

    fn read(&self, frame: &Frame32) -> u32 {
        let mut v = 0u32;
        for i in 0..32 {
            if (frame.0[i] >> i) & 1 == 1 {
                v |= 1u32 << i;
            }
        }
        v
    }
}

// ─── AntiDiagonal (mirrored) ───────────────────────────────────────────────

/// CRC bit `i` lives at bit `31 - i` of slot `i`.
#[derive(Debug, Clone, Copy, Default)]
pub struct AntiDiagonal;

impl ScatterPattern for AntiDiagonal {
    fn information_bits(&self) -> u8 {
        32
    }

    fn clear_reserved(&self, frame: &mut Frame32) {
        for i in 0..32 {
            let bit = 31 - i;
            frame.0[i] &= !(1u32 << bit);
        }
    }

    fn write(&self, frame: &mut Frame32, crc: u32) {
        for i in 0..32 {
            let bit = 31 - i;
            let mask = 1u32 << bit;
            if (crc >> i) & 1 == 1 {
                frame.0[i] |= mask;
            } else {
                frame.0[i] &= !mask;
            }
        }
    }

    fn read(&self, frame: &Frame32) -> u32 {
        let mut v = 0u32;
        for i in 0..32 {
            let bit = 31 - i;
            if (frame.0[i] >> bit) & 1 == 1 {
                v |= 1u32 << i;
            }
        }
        v
    }
}

// ─── Permuted (deterministic random scatter) ───────────────────────────────

/// CRC bits are stored at 32 distinct positions chosen pseudo-randomly
/// from the 1024 frame bits using a deterministic LCG seeded by `seed`.
#[derive(Debug, Clone, Copy)]
pub struct Permuted {
    seed: u64,
    /// `positions[i]` is the `(slot, bit)` of CRC bit `i`.
    positions: [(u8, u8); 32],
}

impl Permuted {
    pub fn new(seed: u64) -> Self {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut used = [false; 1024];
        let mut positions = [(0u8, 0u8); 32];
        let mut k = 0;
        while k < 32 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let pos = (state % 1024) as usize;
            if !used[pos] {
                used[pos] = true;
                positions[k] = ((pos / 32) as u8, (pos % 32) as u8);
                k += 1;
            }
        }
        Self { seed, positions }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn positions(&self) -> &[(u8, u8); 32] {
        &self.positions
    }
}

impl ScatterPattern for Permuted {
    fn information_bits(&self) -> u8 {
        32
    }

    fn clear_reserved(&self, frame: &mut Frame32) {
        for &(slot, bit) in &self.positions {
            frame.0[slot as usize] &= !(1u32 << bit);
        }
    }

    fn write(&self, frame: &mut Frame32, crc: u32) {
        for (i, &(slot, bit)) in self.positions.iter().enumerate() {
            let mask = 1u32 << bit;
            if (crc >> i) & 1 == 1 {
                frame.0[slot as usize] |= mask;
            } else {
                frame.0[slot as usize] &= !mask;
            }
        }
    }

    fn read(&self, frame: &Frame32) -> u32 {
        let mut v = 0u32;
        for (i, &(slot, bit)) in self.positions.iter().enumerate() {
            if (frame.0[slot as usize] >> bit) & 1 == 1 {
                v |= 1u32 << i;
            }
        }
        v
    }
}

// ─── Hadamard (8 bits × 4 replicas with majority vote) ─────────────────────

/// Carries only 8 information bits. Each CRC bit `b ∈ 0..8` is stored
/// at four positions: `(b, b)`, `(b + 8, b)`, `(b + 16, b)`, `(b + 24, b)`.
/// Read uses majority vote across the four replicas.
///
/// Note: this is intentionally a *lossy* scheme — comparing against a
/// full 32-bit CRC requires masking both sides with `mask()` (which is
/// `0xFF` for this pattern).
#[derive(Debug, Clone, Copy, Default)]
pub struct Hadamard;

impl Hadamard {
    fn replicas_for(b: usize) -> [(usize, usize); 4] {
        debug_assert!(b < 8);
        [(b, b), (b + 8, b), (b + 16, b), (b + 24, b)]
    }
}

impl ScatterPattern for Hadamard {
    fn information_bits(&self) -> u8 {
        8
    }

    fn clear_reserved(&self, frame: &mut Frame32) {
        for b in 0..8 {
            for (slot, bit) in Self::replicas_for(b) {
                frame.0[slot] &= !(1u32 << bit);
            }
        }
    }

    fn write(&self, frame: &mut Frame32, crc: u32) {
        for b in 0..8 {
            let one = (crc >> b) & 1 == 1;
            for (slot, bit) in Self::replicas_for(b) {
                let mask = 1u32 << bit;
                if one {
                    frame.0[slot] |= mask;
                } else {
                    frame.0[slot] &= !mask;
                }
            }
        }
    }

    fn read(&self, frame: &Frame32) -> u32 {
        let mut v = 0u32;
        for b in 0..8 {
            let mut votes = 0u32;
            for (slot, bit) in Self::replicas_for(b) {
                votes += (frame.0[slot] >> bit) & 1;
            }
            if votes >= 2 {
                v |= 1u32 << b;
            }
        }
        v
    }
}

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Generic "compute CRC-32C over the frame body with reserved bits
/// cleared, then write that CRC into the reserved positions". The
/// returned frame round-trips through `verify_with` for the same
/// pattern.
///
/// For [`Diagonal`] this is bit-exact equivalent to
/// [`Frame32::update_crc`].
pub fn update_with<P: ScatterPattern>(pattern: &P, frame: &mut Frame32) {
    pattern.clear_reserved(frame);
    let mut buf = [0u8; 128];
    for i in 0..32 {
        buf[i * 4..(i + 1) * 4].copy_from_slice(&frame.0[i].to_be_bytes());
    }
    let crc = crc32c::crc32c(&buf);
    pattern.write(frame, crc & pattern.mask());
}

/// Verify that the CRC stored in `frame` (according to `pattern`)
/// matches the CRC recomputed from the body. Returns `true` if the
/// frame is intact.
pub fn verify_with<P: ScatterPattern>(pattern: &P, frame: &Frame32) -> bool {
    let received = pattern.read(frame);
    let mut cleared = *frame;
    pattern.clear_reserved(&mut cleared);
    let mut buf = [0u8; 128];
    for i in 0..32 {
        buf[i * 4..(i + 1) * 4].copy_from_slice(&cleared.0[i].to_be_bytes());
    }
    let expected = crc32c::crc32c(&buf) & pattern.mask();
    received == expected
}
