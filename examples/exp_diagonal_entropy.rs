//! Sweep: 4 scatter patterns × 6 error classes detection-rate matrix
//! plus 4 patterns × 3 bucket widths conditional-entropy matrix.
//!
//! Run with:
//!     cargo run --release --example exp_diagonal_entropy --features experimental
//!
//! Outputs a Markdown report on stdout (suitable for direct pasting
//! into `docs/research/diagonal_entropy.md`) and writes the raw
//! samples table to `docs/research/diagonal_entropy_data.tsv` for
//! later plotting.

use std::fs::File;
use std::io::Write;

use tren::research::entropy_sweep::{
    DetectionReport, EntropyReport, ErrorClass, measure_conditional_entropy,
    measure_detection_rate,
};
use tren::research::scatter::{AntiDiagonal, Diagonal, Hadamard, Permuted, ScatterPattern};

const N_DETECTION_TRIALS: u64 = 10_000;
const N_ENTROPY_SAMPLES: usize = 100_000;
const PERMUTED_SEED: u64 = 0xCAFE_F00D_DEAD_BEEFu64;
const TSV_PATH: &str = "docs/research/diagonal_entropy_data.tsv";

fn error_classes() -> Vec<ErrorClass> {
    vec![
        ErrorClass::Random(8),
        ErrorClass::Random(32),
        ErrorClass::Burst(16),
        ErrorClass::Burst(32),
        ErrorClass::EvenBit(16),
        ErrorClass::DiagonalOnly(8),
    ]
}

fn bucket_widths() -> Vec<u8> {
    vec![8, 16, 32]
}

fn run_pattern<P: ScatterPattern>(
    pattern: &P,
    label: &str,
    seed_base: u64,
) -> (Vec<DetectionReport>, Vec<EntropyReport>) {
    let mut det = Vec::new();
    for (i, class) in error_classes().into_iter().enumerate() {
        det.push(measure_detection_rate(
            pattern,
            label,
            class,
            N_DETECTION_TRIALS,
            seed_base.wrapping_add(i as u64),
        ));
    }
    let mut ent = Vec::new();
    for (j, b) in bucket_widths().into_iter().enumerate() {
        ent.push(measure_conditional_entropy(
            pattern,
            label,
            N_ENTROPY_SAMPLES,
            b,
            seed_base.wrapping_add(1000 + j as u64),
        ));
    }
    (det, ent)
}

fn print_detection_table(rows: &[DetectionReport]) {
    println!("| Pattern | ErrorClass | trials | detected | rate | Wilson 95% low | Wilson 95% high |");
    println!("|---|---|---:|---:|---:|---:|---:|");
    for r in rows {
        println!(
            "| {} | {} | {} | {} | {:.6} | {:.6} | {:.6} |",
            r.pattern_label,
            r.class_label,
            r.trials,
            r.detected,
            r.rate,
            r.wilson_low,
            r.wilson_high,
        );
    }
}

fn print_entropy_table(rows: &[EntropyReport]) {
    println!("| Pattern | bucket bits | N | H(diag) | H(diag\\|body_bucket) | I(diag;body_bucket) |");
    println!("|---|---:|---:|---:|---:|---:|");
    for r in rows {
        println!(
            "| {} | {} | {} | {:.4} | {:.4} | {:.4} |",
            r.pattern_label,
            r.bucket_bits,
            r.sample_size,
            r.h_diag,
            r.h_diag_given_body_bucket,
            r.mutual_info_estimate,
        );
    }
}

fn write_tsv(
    path: &str,
    detections: &[DetectionReport],
    entropies: &[EntropyReport],
) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    writeln!(
        f,
        "section\tpattern\tclass_or_bucket\ttrials_or_N\tdetected_or_value\trate_or_h_diag\twilson_low_or_h_cond\twilson_high_or_mi"
    )?;
    for r in detections {
        writeln!(
            f,
            "detection\t{}\t{}\t{}\t{}\t{:.6}\t{:.6}\t{:.6}",
            r.pattern_label,
            r.class_label,
            r.trials,
            r.detected,
            r.rate,
            r.wilson_low,
            r.wilson_high,
        )?;
    }
    for r in entropies {
        writeln!(
            f,
            "entropy\t{}\tbucket{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{:.6}",
            r.pattern_label,
            r.bucket_bits,
            r.sample_size,
            r.h_diag,
            r.h_diag,
            r.h_diag_given_body_bucket,
            r.mutual_info_estimate,
        )?;
    }
    Ok(())
}

fn main() {
    println!("# Diagonal-as-trace conditional-entropy sweep (Task #26)\n");
    println!(
        "Patterns: Diagonal, AntiDiagonal, Permuted(seed=0x{:X}), Hadamard.",
        PERMUTED_SEED
    );
    println!(
        "Detection: {} trials per cell. Entropy: {} samples per cell, bucket widths {{8, 16, 32}} bits.\n",
        N_DETECTION_TRIALS, N_ENTROPY_SAMPLES
    );

    let mut all_det = Vec::new();
    let mut all_ent = Vec::new();

    let (d, e) = run_pattern(&Diagonal, "Diagonal", 0x1000);
    all_det.extend(d);
    all_ent.extend(e);

    let (d, e) = run_pattern(&AntiDiagonal, "AntiDiagonal", 0x2000);
    all_det.extend(d);
    all_ent.extend(e);

    let perm = Permuted::new(PERMUTED_SEED);
    let (d, e) = run_pattern(&perm, "Permuted", 0x3000);
    all_det.extend(d);
    all_ent.extend(e);

    let (d, e) = run_pattern(&Hadamard, "Hadamard", 0x4000);
    all_det.extend(d);
    all_ent.extend(e);

    println!("## Detection-rate matrix\n");
    print_detection_table(&all_det);

    println!("\n## Conditional-entropy matrix\n");
    print_entropy_table(&all_ent);

    match write_tsv(TSV_PATH, &all_det, &all_ent) {
        Ok(()) => println!("\n_Raw data written to `{}`._", TSV_PATH),
        Err(e) => eprintln!("\nWARNING: failed to write {}: {}", TSV_PATH, e),
    }
}
