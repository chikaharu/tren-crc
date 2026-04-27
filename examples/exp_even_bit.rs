//! exp_even_bit: detection-rate sweep for `parity_layer` alternatives.
//!
//! Compares the production `Crc32Only` layout against `RowXorPlusCrc { k }`,
//! `ColXorPlusCrc { k }`, and `SplitOddEven16` across a grid of (error_class,
//! n) cells. Each cell runs `TRIALS` independent injections and reports the
//! detection rate with a Wilson 95% interval.
//!
//! Run with:
//!
//! ```text
//! cargo run --release --example exp_even_bit --features experimental
//! ```

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tren::research::data::random_frame;
use tren::research::inject::{flip_burst, flip_random_n};
use tren::research::parity_layer::{
    update_with_layout, verify_with_layout, ParityLayout,
};
use tren::research::stats::detection_rate;

const TRIALS: u64 = 10_000;
const SEED: u64 = 0xCAFE_F00D;

#[derive(Clone, Copy)]
enum ErrorClass {
    Even(usize),
    Odd(usize),
    Burst(usize),
}

impl ErrorClass {
    fn label(&self) -> &'static str {
        match self {
            ErrorClass::Even(_) => "even",
            ErrorClass::Odd(_) => "odd",
            ErrorClass::Burst(_) => "burst",
        }
    }

    fn n(&self) -> usize {
        match self {
            ErrorClass::Even(n) | ErrorClass::Odd(n) | ErrorClass::Burst(n) => *n,
        }
    }
}

fn run_cell(layout: ParityLayout, err: ErrorClass, rng: &mut StdRng) -> (u64, u64) {
    let mut detected: u64 = 0;
    for _ in 0..TRIALS {
        // Start from a valid CRC-32C frame, then re-stamp with the layout's
        // own syndrome so the body has random data and the diagonal is correct
        // for this layout before injection.
        let mut f = random_frame(rng);
        update_with_layout(layout, &mut f);
        match err {
            ErrorClass::Even(n) | ErrorClass::Odd(n) => {
                flip_random_n(&mut f, n, rng);
            }
            ErrorClass::Burst(len) => {
                let start = rng.gen_range(0..=(1024 - len));
                flip_burst(&mut f, start, len);
            }
        }
        if verify_with_layout(layout, &f).is_err() {
            detected += 1;
        }
    }
    (detected, TRIALS)
}

fn layout_name(layout: ParityLayout) -> &'static str {
    match layout {
        ParityLayout::Crc32Only => "Crc32Only",
        ParityLayout::RowXorPlusCrc { .. } => "RowXorPlusCrc",
        ParityLayout::ColXorPlusCrc { .. } => "ColXorPlusCrc",
        ParityLayout::SplitOddEven16 => "SplitOddEven16",
    }
}

fn layout_k(layout: ParityLayout) -> u8 {
    match layout {
        ParityLayout::Crc32Only => 0,
        ParityLayout::RowXorPlusCrc { k } | ParityLayout::ColXorPlusCrc { k } => k,
        ParityLayout::SplitOddEven16 => 16,
    }
}

fn main() {
    println!("# tren-crc even-bit detection sweep (raw experiment output)");
    println!();
    println!("Trials per cell: {}", TRIALS);
    println!("Seed: 0x{:X}", SEED);
    println!();
    println!("| layout | k | error_class | n | detected | total | rate | wilson_low | wilson_high |");
    println!("|--------|--:|-------------|--:|---------:|------:|-----:|-----------:|------------:|");

    let mut rng = StdRng::seed_from_u64(SEED);

    let mut layouts: Vec<ParityLayout> = vec![ParityLayout::Crc32Only];
    for k in [1u8, 2, 4, 8, 12, 16] {
        layouts.push(ParityLayout::RowXorPlusCrc { k });
    }
    for k in [1u8, 2, 4, 8, 12, 16] {
        layouts.push(ParityLayout::ColXorPlusCrc { k });
    }
    layouts.push(ParityLayout::SplitOddEven16);

    let error_classes: Vec<ErrorClass> = {
        let mut v: Vec<ErrorClass> = vec![];
        for n in [2usize, 4, 6, 8, 16, 32] {
            v.push(ErrorClass::Even(n));
        }
        for n in [1usize, 3, 5, 7] {
            v.push(ErrorClass::Odd(n));
        }
        v.push(ErrorClass::Burst(32));
        v
    };

    for layout in layouts {
        for err in &error_classes {
            let (det, tot) = run_cell(layout, *err, &mut rng);
            let (rate, lo, hi) = detection_rate(det, tot);
            println!(
                "| {} | {} | {} | {} | {} | {} | {:.6} | {:.6} | {:.6} |",
                layout_name(layout),
                layout_k(layout),
                err.label(),
                err.n(),
                det,
                tot,
                rate,
                lo,
                hi,
            );
        }
    }
}
