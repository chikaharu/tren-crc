//! Experimental research harness for tren-crc.
//!
//! Gated behind the `experimental` cargo feature; default builds and
//! release binaries do **not** include this module. Adding this layer
//! keeps shared experimental code (data generators, error injectors,
//! statistics utilities, scatter pattern abstraction, bench harness,
//! parity-layer alternatives, and CRC-32C operator implementations)
//! in one place so the planned experiment tasks can use the same
//! primitives without duplication.
//!
//! The harness intentionally has minimal API surface: each submodule
//! exposes only the types and free functions actually consumed by the
//! downstream experiments.

pub mod bench;
pub mod bool_ops;
pub mod crc_impls;
pub mod data;
pub mod inject;
pub mod parity_layer;
pub mod scatter;
pub mod stats;
