//! qwait — wait until specified node(s) finish, via UDP subscription
//! (with file-poll fallback).
//!
//! Usage:
//!   qwait <addr> [<addr>...]
//!   qwait --any <addr> [<addr>...]
//!
//! Exit codes:
//!   0 — all (or any, with --any) target nodes ended in DONE(0)
//!   1 — at least one target ended in DONE(N>0) or FAILED
//!
//! Note: as of Task #9, `qsub` itself blocks until its own job finishes
//! and reports `[qsub] exit=N`, so `qwait` is mainly useful for waiting on
//! addresses you did not just submit (e.g. a fan-out across a tree).

use std::path::PathBuf;

use tren::{find_workdir, read_port, wait_for_addrs};

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        eprintln!("usage: qwait [--any] <addr> [<addr>...]");
        std::process::exit(2);
    }
    let any_mode = raw[0] == "--any";
    let targets: Vec<String> = if any_mode { raw[1..].to_vec() } else { raw.clone() };
    if targets.is_empty() { eprintln!("qwait: no targets"); std::process::exit(2); }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let workdir = match find_workdir(&cwd) {
        Some(p) => p,
        None    => { eprintln!("qwait: no .tren-<uuid>/ found"); std::process::exit(1); }
    };
    let port = match read_port(&workdir) {
        Ok(p)  => p,
        Err(e) => { eprintln!("qwait: read port: {}", e); std::process::exit(1); }
    };

    let (failed_any, elapsed) = match wait_for_addrs(&workdir, port, &targets, any_mode, "qwait") {
        Ok(v)  => v,
        Err(e) => { eprintln!("qwait: wait error: {}", e); std::process::exit(1); }
    };

    let exit = if failed_any { 1 } else { 0 };
    eprintln!("[qwait] complete in {}s  exit={}", elapsed.as_secs(), exit);
    std::process::exit(exit);
}
