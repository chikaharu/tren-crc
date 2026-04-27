# Changelog

  All notable changes to this project will be documented in this file.

  The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
  and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

  > **Fork note:** chikaharu/tren-crc is a fork of [chikaharu/tren](https://github.com/chikaharu/tren).
  > Version numbers carry a `-crc.*` suffix to distinguish fork releases from upstream releases.
  > The CHANGELOG covers fork-specific changes only; upstream changes are tracked in the upstream repo.

  ## [Unreleased]

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
  