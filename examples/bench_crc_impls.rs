//! Benchmark: CRC-32C operator zoo on 1024-bit (128-byte) frames.
//!
//! Run with:
//!     cargo run --release --example bench_crc_impls --features experimental
//!
//! Reports min / median / p99 latency and throughput
//! (frames/sec, GiB/s) for each implementation. The 4-way SIMD
//! implementations are reported per-frame (i.e. a single SIMD call
//! that processes 4 frames is amortised over 4 frames).
//!
//! Output is a Markdown table on stdout, suitable for embedding into
//! `docs/research/simd_branchless_crc.md` directly.

use std::hint::black_box;
use std::time::Instant;

use tren::research::crc_impls::{
    crc32c_branchless_bitwise, crc32c_branchless_bitwise_v2, crc32c_hw, crc32c_popcount_xor,
    crc32c_simd_sse2_x4, crc32c_slice_by_8, crc32c_table, is_avx2_available, warm_popcount_basis,
};
#[cfg(target_arch = "x86_64")]
use tren::research::crc_impls::crc32c_simd_avx2_x4;
use tren::research::data::random_frames;

const FRAME_BYTES: usize = 128;
const FRAME_BITS: usize = FRAME_BYTES * 8;

#[derive(Clone, Debug)]
struct Row {
    name: String,
    frames_per_call: usize,
    iters: u64,
    min_ns: u64,
    median_ns: u64,
    p99_ns: u64,
    frames_per_sec: f64,
    gib_per_sec: f64,
}

impl Row {
    fn skipped(name: &str, reason: &str) -> Self {
        Row {
            name: format!("{name} (skipped: {reason})"),
            frames_per_call: 0,
            iters: 0,
            min_ns: 0,
            median_ns: 0,
            p99_ns: 0,
            frames_per_sec: 0.0,
            gib_per_sec: 0.0,
        }
    }
}

fn bench<F: FnMut() -> u64>(name: &str, frames_per_call: usize, iters: u64, mut f: F) -> Row {
    // Warm-up: 1000 calls so caches and branch predictors settle.
    for _ in 0..1000 {
        let _ = black_box(f());
    }
    let mut samples: Vec<u64> = Vec::with_capacity(iters as usize);
    for _ in 0..iters {
        let t0 = Instant::now();
        let r = f();
        let dt = t0.elapsed().as_nanos() as u64;
        black_box(r);
        samples.push(dt);
    }
    samples.sort_unstable();
    let len = samples.len();
    let min_ns = samples[0];
    let median_ns = samples[len / 2];
    let p99_idx = ((len as f64 * 0.99) as usize).min(len - 1);
    let p99_ns = samples[p99_idx];
    let total_ns: u128 = samples.iter().map(|&s| s as u128).sum();
    let total_frames = (iters as u128) * (frames_per_call as u128);
    let frames_per_sec = if total_ns == 0 {
        f64::INFINITY
    } else {
        (total_frames as f64) / (total_ns as f64 / 1e9)
    };
    let gib_per_sec = frames_per_sec * (FRAME_BYTES as f64) / (1024.0 * 1024.0 * 1024.0);
    Row {
        name: name.to_string(),
        frames_per_call,
        iters,
        min_ns,
        median_ns,
        p99_ns,
        frames_per_sec,
        gib_per_sec,
    }
}

fn print_table(rows: &[Row]) {
    println!("| Implementation | frames/call | iters | min (ns) | median (ns) | p99 (ns) | frames/sec | GiB/s |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|");
    for r in rows {
        if r.frames_per_call == 0 {
            println!("| {} | — | — | — | — | — | — | — |", r.name);
        } else {
            println!(
                "| {} | {} | {} | {} | {} | {} | {:.0} | {:.3} |",
                r.name,
                r.frames_per_call,
                r.iters,
                r.min_ns,
                r.median_ns,
                r.p99_ns,
                r.frames_per_sec,
                r.gib_per_sec,
            );
        }
    }
}

fn main() {
    println!("# CRC-32C operator zoo benchmark\n");
    println!(
        "Frame size: {FRAME_BITS} bits ({FRAME_BYTES} bytes). Reported numbers are per-frame; \
         4-way SIMD calls are amortised over 4 frames."
    );
    println!();

    // Pre-build the popcount-XOR basis so per-call latency excludes
    // the one-time O(L^2) construction cost.
    warm_popcount_basis(FRAME_BYTES);

    // Generate a pool of 4 frames so SIMD impls have something to chew on.
    let frames = random_frames(4, 0xCAFE_F00D_DEAD_BEEFu64);
    let f0: Vec<u8> = frame_bytes(&frames[0]);
    let f1: Vec<u8> = frame_bytes(&frames[1]);
    let f2: Vec<u8> = frame_bytes(&frames[2]);
    let f3: Vec<u8> = frame_bytes(&frames[3]);

    let scalar_iters = 200_000u64;
    let simd_iters = 50_000u64;

    let mut rows = Vec::new();

    rows.push(bench("crc32c_hw (crc32c crate)", 1, scalar_iters, || {
        crc32c_hw(&f0) as u64
    }));
    rows.push(bench("crc32c_table (Sarwate 8-bit)", 1, scalar_iters, || {
        crc32c_table(&f0) as u64
    }));
    rows.push(bench("crc32c_slice_by_8", 1, scalar_iters, || {
        crc32c_slice_by_8(&f0) as u64
    }));
    rows.push(bench(
        "crc32c_branchless_bitwise",
        1,
        scalar_iters,
        || crc32c_branchless_bitwise(&f0) as u64,
    ));
    rows.push(bench(
        "crc32c_branchless_bitwise_v2 (xnor/nand/xor3)",
        1,
        scalar_iters,
        || crc32c_branchless_bitwise_v2(&f0) as u64,
    ));
    rows.push(bench("crc32c_popcount_xor", 1, scalar_iters, || {
        crc32c_popcount_xor(&f0) as u64
    }));

    // SSE2 is part of the x86_64 baseline, so always run on x86_64.
    if cfg!(target_arch = "x86_64") {
        let view: [&[u8]; 4] = [&f0, &f1, &f2, &f3];
        rows.push(bench(
            "crc32c_simd_sse2_x4",
            4,
            simd_iters,
            move || {
                let r = crc32c_simd_sse2_x4(view);
                (r[0] ^ r[1] ^ r[2] ^ r[3]) as u64
            },
        ));
    } else {
        rows.push(Row::skipped("crc32c_simd_sse2_x4", "non-x86_64 target"));
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_avx2_available() {
            let view: [&[u8]; 4] = [&f0, &f1, &f2, &f3];
            rows.push(bench(
                "crc32c_simd_avx2_x4",
                4,
                simd_iters,
                move || {
                    let r = crc32c_simd_avx2_x4(view);
                    (r[0] ^ r[1] ^ r[2] ^ r[3]) as u64
                },
            ));
        } else {
            rows.push(Row::skipped(
                "crc32c_simd_avx2_x4",
                "AVX2 not advertised by CPU",
            ));
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = is_avx2_available;
        rows.push(Row::skipped("crc32c_simd_avx2_x4", "non-x86_64 target"));
    }

    print_table(&rows);

    // Cross-check: every implementation's value on f0 must agree with
    // the hardware reference. Failing this means the bench is meaningless.
    let golden = crc32c_hw(&f0);
    assert_eq!(crc32c_table(&f0), golden);
    assert_eq!(crc32c_slice_by_8(&f0), golden);
    assert_eq!(crc32c_branchless_bitwise(&f0), golden);
    assert_eq!(crc32c_branchless_bitwise_v2(&f0), golden);
    assert_eq!(crc32c_popcount_xor(&f0), golden);
    if cfg!(target_arch = "x86_64") {
        let view: [&[u8]; 4] = [&f0, &f1, &f2, &f3];
        let sse = crc32c_simd_sse2_x4(view);
        assert_eq!(sse[0], golden);
    }
    println!("\n_All implementations agree with `crc32c_hw` on the first frame._");
}

fn frame_bytes(f: &tren::Frame32) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_BYTES);
    for slot in 0..32 {
        out.extend_from_slice(&f.0[slot].to_le_bytes());
    }
    out
}
