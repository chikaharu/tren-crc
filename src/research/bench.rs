//! Lightweight benchmark harness.
//!
//! Built deliberately without `criterion`: experiments run in CI on
//! shared GitHub Actions runners and we want to keep dependencies
//! minimal. The harness records per-call wall-clock nanoseconds via
//! [`std::time::Instant`], then reports min / median / p99 / mean
//! throughput.
//!
//! For absolute-microbenchmark accuracy a real harness like criterion
//! is preferred; for relative comparison of "implementation A vs
//! implementation B doing the same work" this is sufficient.

use std::hint::black_box;
use std::time::Instant;

/// Result of a single benchmark run.
#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub iters: u64,
    pub min_ns: u64,
    pub median_ns: u64,
    pub p99_ns: u64,
    /// Mean throughput in operations per second across the full run.
    pub throughput_per_sec: f64,
}

/// One-shot runner. The closure is called once for warm-up and then
/// `iters` more times for measurement. The closure's `u32` return
/// value is fed to [`std::hint::black_box`] to discourage the
/// optimiser from elevating the work out of the loop.
pub struct BenchHarness;

impl BenchHarness {
    pub fn run<F: FnMut() -> u32>(name: &str, iters: u64, mut f: F) -> BenchResult {
        assert!(iters >= 1, "iters must be >= 1");
        // Warm-up.
        let _ = black_box(f());

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
        let throughput = if total_ns == 0 {
            f64::INFINITY
        } else {
            (iters as f64) / (total_ns as f64 / 1e9)
        };

        BenchResult {
            name: name.to_string(),
            iters,
            min_ns,
            median_ns,
            p99_ns,
            throughput_per_sec: throughput,
        }
    }
}

impl BenchResult {
    /// Render a slice of results as a Markdown table to stdout.
    pub fn print_table(results: &[BenchResult]) {
        println!("| Name | Iters | Min (ns) | Median (ns) | p99 (ns) | Throughput (op/s) |");
        println!("|------|------:|---------:|------------:|---------:|------------------:|");
        for r in results {
            println!(
                "| {} | {} | {} | {} | {} | {:.0} |",
                r.name, r.iters, r.min_ns, r.median_ns, r.p99_ns, r.throughput_per_sec,
            );
        }
    }

    /// Render a slice of results as a Markdown table to a `String`.
    pub fn format_table(results: &[BenchResult]) -> String {
        let mut s = String::new();
        s.push_str("| Name | Iters | Min (ns) | Median (ns) | p99 (ns) | Throughput (op/s) |\n");
        s.push_str("|------|------:|---------:|------------:|---------:|------------------:|\n");
        for r in results {
            s.push_str(&format!(
                "| {} | {} | {} | {} | {} | {:.0} |\n",
                r.name, r.iters, r.min_ns, r.median_ns, r.p99_ns, r.throughput_per_sec,
            ));
        }
        s
    }
}
