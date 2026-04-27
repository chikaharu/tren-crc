//! Random `Frame32` generators for experiments.
//!
//! Frames are filled with random data in every slot, then run through
//! [`Frame32::update_crc`] so the diagonal carries a valid CRC-32C.
//! The result therefore round-trips through [`Frame32::verify_crc`].

use crate::Frame32;
use rand::{Rng, RngCore, SeedableRng};
use rand::rngs::StdRng;

/// Build a single `Frame32` whose 32 slots are filled with random
/// bits, with the diagonal CRC-32C correctly populated by
/// [`Frame32::update_crc`].
///
/// Any random bits placed in the diagonal positions are overwritten by
/// `update_crc`, so the returned frame always verifies.
pub fn random_frame<R: RngCore>(rng: &mut R) -> Frame32 {
    let mut f = Frame32::new();
    for slot in 0..32 {
        f.0[slot] = rng.next_u32();
    }
    f.update_crc();
    f
}

/// Build `n` random frames from a deterministic seed. Two calls with
/// the same `(n, seed)` return identical sequences, which keeps the
/// downstream experiments reproducible.
pub fn random_frames(n: usize, seed: u64) -> Vec<Frame32> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..n).map(|_| random_frame(&mut rng)).collect()
}

/// Build a single random frame from a deterministic seed (convenience).
pub fn random_frame_seeded(seed: u64) -> Frame32 {
    let mut rng = StdRng::seed_from_u64(seed);
    random_frame(&mut rng)
}

/// Fill `body` (a 128-byte buffer that represents the serialised frame
/// with the diagonal cleared) with random bytes derived from `rng`.
/// Useful for entropy / scatter experiments that don't need a
/// `Frame32` round-trip.
pub fn random_body<R: Rng>(rng: &mut R) -> [u8; 128] {
    let mut buf = [0u8; 128];
    rng.fill_bytes(&mut buf);
    buf
}
