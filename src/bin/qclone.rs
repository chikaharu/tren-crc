//! qclone — minimal port. In the daemon-era this spawned a binary tree of
//! sub-schedulers each on its own Unix socket. With the new PWD-local tren
//! arch the entire tree lives inside a single wrapper process; the bit
//! addressing is a property of the in-process tree rather than separate
//! processes.
//!
//! qclone now simply pre-allocates an empty binary tree of the requested
//! depth by submitting `:` (no-op) jobs in BFS order and printing each
//! resulting bit_addr. This preserves the API surface (callers get the set
//! of leaf addresses) while running on the new arch.
//!
//! Usage:
//!   qclone [--depth N]   default N=1 → 2 leaves

use std::path::PathBuf;
use std::time::Duration;

use tren::{connect_or_spawn, encode_text, id_to_bit_addr, udp_request};

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
    if depth > 10 {
        eprintln!("qclone: depth > 10 not allowed (would create {} leaves)", 1usize << depth);
        std::process::exit(2);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let (workdir, port) = match connect_or_spawn(&cwd, true) {
        Ok(p) => p,
        Err(e) => { eprintln!("qclone: {}", e); std::process::exit(1); }
    };
    eprintln!("[qclone] depth={} workdir={}", depth, workdir.display());

    // Submit `(1 << (depth+1)) - 1` no-op jobs in BFS order: this allocates
    // ids 1..(1<<(depth+1))-1, ie. root + every internal + every leaf.
    let total = (1u64 << (depth + 1)) - 1;
    let mut last_addr = String::new();
    for _ in 0..total {
        let req = format!("SUBMIT\n\n{}\n", encode_text(":"));
        match udp_request(port, &req, Duration::from_secs(5)) {
            Ok(reply) => {
                let r = reply.trim();
                if let Some(a) = r.strip_prefix("OK ") {
                    last_addr = a.to_string();
                } else {
                    eprintln!("qclone: SUBMIT failed: {}", r);
                    std::process::exit(1);
                }
            }
            Err(e) => { eprintln!("qclone: udp: {}", e); std::process::exit(1); }
        }
    }

    // Print leaf bit_addrs (ids in [1<<depth, (1<<(depth+1))-1])
    let from = 1u64 << depth;
    let to   = (1u64 << (depth + 1)) - 1;
    eprintln!("[qclone] leaves (ids {}..{}):", from, to);
    for id in from..=to {
        println!("{}", id_to_bit_addr(id));
    }
    let _ = last_addr;
}
