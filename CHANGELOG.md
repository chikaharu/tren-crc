# Changelog

  All notable changes to this project will be documented in this file.

  The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
  and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

  > **Fork note:** chikaharu/tren-crc is a fork of [chikaharu/tren](https://github.com/chikaharu/tren).
  > Version numbers carry a `-crc.*` suffix to distinguish fork releases from upstream releases.
  > The CHANGELOG covers fork-specific changes only; upstream changes are tracked in the upstream repo.

  ## [Unreleased]

  ### Added

  - **Experimental research harness** (`src/research/`, gated behind the new `experimental` cargo feature).
    Provides shared building blocks for the planned CRC-32C experiments: deterministic random `Frame32`
    generators (`research::data`), bit-error injectors (`research::inject`), Wilson detection-rate intervals
    and plug-in entropy estimators (`research::stats`), a `ScatterPattern` trait with four implementations
    (`Diagonal`, `AntiDiagonal`, `Permuted`, `Hadamard` — the latter is intentionally lossy with majority-vote
    read), and a small min/median/p99/throughput bench harness (`research::bench`). Default builds and
    `--features model` builds are unaffected.
  - **Experimental `parity_layer` module + `exp_even_bit` example** (`src/research/parity_layer.rs`,
    `examples/exp_even_bit.rs`, gated behind the `experimental` feature). Adds `ParityLayout` with four
    variants (`Crc32Only`, `RowXorPlusCrc { k }`, `ColXorPlusCrc { k }`, `SplitOddEven16`) and a sweep
    example that compares detection rates against the production CRC-32C-only layout. **Negative result**:
    within the spec range (n ≤ 32 bit flips, burst ≤ 32) all 154 cells × 10,000 trials hit 100 % detection,
    so re-budgeting CRC bits to an auxiliary parity layer (row/column XOR or split CRC-16/IBM + CRC-16/CCITT
    over odd/even diagonal positions) yields no observable improvement. Full theory and analysis in
    [`docs/research/even_bit_detection.md`](docs/research/even_bit_detection.md). CI builds the example to
    keep it from bit-rotting; running the sweep itself is left to maintainers.
  - **Experimental CRC-32C operator zoo** (`src/research/crc_impls.rs`, `src/research/bool_ops.rs`,
    `examples/bench_crc_impls.rs`, `tests/crc_impls_consistency.rs`, gated behind the `experimental`
    feature). Eight side-by-side implementations of CRC-32C — `crc32c_hw` (production reference, SSE
    4.2 hardware), `crc32c_table` (Sarwate 8-bit), `crc32c_slice_by_8`, `crc32c_branchless_bitwise`
    and `_v2` (XNOR/NAND/XOR3 helpers from `bool_ops`), `crc32c_popcount_xor` (GF(2)-linear basis),
    `crc32c_simd_sse2_x4`, and `crc32c_simd_avx2_x4` — all bit-exact verified against the production
    reference on 10,000 random 128-byte messages. Benchmark example reports min/median/p99 latency
    and throughput in frames-per-second / GiB/s. **Result: production `crc32c_hw` (~40 ns/frame,
    2.58 GiB/s) remains the right choice; the closest software alternative
    (`crc32c_slice_by_8`, ~90 ns/frame) is 2.3× slower, branchless and SIMD variants are
    14×–15× slower per frame**. Notably, `crc32c_simd_avx2_x4` is _slower_ than
    `crc32c_simd_sse2_x4` because the spec-mandated 4-lane signature leaves AVX2's upper four
    lanes idle, paying the wider instruction cost without parallelism gain. Full theory, results,
    and trade-offs in [`docs/research/simd_branchless_crc.md`](docs/research/simd_branchless_crc.md).
    CI builds the bench example to keep it from bit-rotting.

  ### Changed

  - **`rand` added as an optional dependency**, activated only by `--features experimental`. Default builds
    and `--features model` builds still have no `rand` in their dependency tree.

  ## [v0.5.0-crc.1] - 2026-04-27

  ### Changed

  - **Frame32 integrity scheme replaced with CRC-32C (Castagnoli).**
    The diagonal XOR parity previously used in `Frame32` has been replaced with a CRC-32C checksum.
    CRC-32C provides stronger burst-error detection and is hardware-accelerated on modern CPUs via SSE 4.2 / ARMv8.
    Downstream consumers that relied on the parity byte layout will need to update their decoders accordingly
    (see the wire-incompatibility notice in the README).
    _Commit: 69cd923_

  ### Added

  - **`CI_PENDING` helper for GitHub Actions.**
    A one-paste install script (`ci/pending.sh`) lets maintainers mark a job as pending while the full CI
    workflow is being wired up, preventing accidental green-status merges.
    _Commit: c7fc2d5_
  - **CRC-32C error-detection test suite** (`tests/crc_detect.rs`).
    Covers single-bit flip detection rate sweeps (n = 1 to 512 bits) to validate the new checksum scheme.
    _Commit: c8ac8f9, d4413c5_
  - **GitHub Actions CI workflow** running `cargo test` against both the default feature set and the
    `--features model` feature flag.
    _Commit: 59e9330_
  - **Architectural notes and wire-incompatibility notice** added to the README, documenting the CRC-32C
    design decisions and the impact on existing `tren` consumers.
    _Commit: 3fa285c_

  ### Meta

  - Package renamed from `tren` to `tren-crc` at version `0.5.0-crc.1`; `crc32c` crate added as a dependency.

  **Full release:** https://github.com/chikaharu/tren-crc/releases/tag/v0.5.0-crc.1

  ---

  ## Release process

  Before tagging a new release:

  1. Add a section `## [vX.Y.Z-crc.N] - YYYY-MM-DD` under `[Unreleased]`.
  2. Move the relevant items from `[Unreleased]` into the new section.
  3. Commit the updated CHANGELOG (`docs: update CHANGELOG for vX.Y.Z-crc.N`).
  4. Tag the commit and push the tag.
  5. Create a GitHub Release pointing to the tag.

  [v0.5.0-crc.1]: https://github.com/chikaharu/tren-crc/releases/tag/v0.5.0-crc.1
  