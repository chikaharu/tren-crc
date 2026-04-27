//! qclone — pre-allocate a binary tree of `:` (no-op) jobs.
//!
//! Usage:
//!   qclone [--depth N]   default N=1 → 2 leaves

use std::path::PathBuf;

use tren::{id_to_bit_addr, submit_cmd};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut depth = 1usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--depth" => {
                i += 1;
                depth = args.get(i).and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                    eprintln!("qclone: --depth needs a number");
                    std::process::exit(2);
                });
            }
            other => {
                eprintln!("qclone: unknown arg '{}'", other);
                std::process::exit(2);
            }
        }
        i += 1;
    }
    if depth > 5 {
        // 2^(depth+1)-1 nodes; with the 32-leaf cap a single spill can host
        // depth ≤ 5 (2^6 - 1 = 63 nodes ≤ 64 = SPILL_NODE_CAP).
        eprintln!("qclone: depth > 5 not allowed under the 32-leaf-per-spill cap");
        std::process::exit(2);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let total = (1u64 << (depth + 1)) - 1;
    eprintln!("[qclone] depth={} → submitting {} no-op jobs", depth, total);
    for _ in 0..total {
        if let Err(e) = submit_cmd(&cwd, &[], ":") {
            eprintln!("qclone: SUBMIT failed: {}", e);
            std::process::exit(1);
        }
    }

    let from = 1u64 << depth;
    let to   = (1u64 << (depth + 1)) - 1;
    eprintln!("[qclone] leaves (ids {}..{}):", from, to);
    for id in from..=to {
        println!("{}", id_to_bit_addr(id));
    }
}
