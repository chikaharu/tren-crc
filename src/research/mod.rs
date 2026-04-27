//! Experimental research harness for tren-crc.
//!
//! Gated behind the `experimental` cargo feature; default builds and
//! release binaries do **not** include this module. Adding this layer
//! keeps shared experimental code (data generators, error injectors,
//! statistics utilities, scatter pattern abstraction, bench harness)
//! in one place so the three planned experiment tasks (even-bit error
//! detection sweep, branchless / SIMD CRC operators, scatter pattern
//! conditional-entropy sweep) can use the same primitives without
//! duplication.
//!
//! The harness intentionally has minimal API surface: each submodule
//! exposes only the types and free functions actually consumed by the
//! downstream experiments.

pub mod bench;
pub mod data;
pub mod inject;
pub mod scatter;
pub mod stats;
