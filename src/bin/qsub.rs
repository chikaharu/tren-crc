//! qsub — submit a job to the PWD-local tren wrapper, then block until it
//! finishes.
//!
//! Usage:
//!   qsub <cmd> [args...]
//!   qsub --after <addr> [<addr>...] -- <cmd> [args...]
//!   qsub --owner <name> <cmd>             # forwarded as TREN_OWNER env
//!
//! If no `.tren-<uuid>/` is present in (or above) cwd, the wrapper is
//! auto-spawned. After SUBMIT succeeds, the new node's bit address is
//! printed on stdout and `qsub` then waits for that node to reach a
//! finished state, prints `[qsub] exit=N` on stderr, and exits with that
//! same code (0 on `DONE(0)`, 1 on `DONE(N>0)` or `FAILED(_)`).
//!
//! This makes `qsub` behave like an ordinary shell command: every
//! invocation blocks until *its own* job is done, including in chains
//! built with `--after`.

use std::path::PathBuf;
use std::time::Duration;

use tren::{connect_or_spawn, encode_text, udp_request, wait_for_addrs};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: qsub [--after <addr>...] [--owner <name>] [--] <cmd> [args...]");
        std::process::exit(2);
    }

    let mut deps:  Vec<String> = Vec::new();
    let mut owner: Option<String> = None;
    let mut cmd:   Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut sep_seen = false;

    while i < args.len() {
        let a = &args[i];
        if !sep_seen && a == "--after" {
            i += 1;
            while i < args.len() && !args[i].starts_with("--") {
                if args[i] == "--" { break; }
                deps.push(args[i].clone());
                i += 1;
            }
            continue;
        }
        if !sep_seen && a == "--owner" {
            i += 1;
            if i < args.len() { owner = Some(args[i].clone()); i += 1; }
            continue;
        }
        if !sep_seen && a == "--" { sep_seen = true; i += 1; continue; }
        cmd.extend_from_slice(&args[i..]);
        break;
    }

    if cmd.is_empty() {
        eprintln!("qsub: missing command");
        std::process::exit(2);
    }

    let cmd_str = if let Some(o) = owner.as_ref() {
        // Owner forwarded via env; out-of-scope wrapper just exposes
        // TREN_OWNER inside the executed shell.
        format!("TREN_OWNER={} {}", o, cmd.join(" "))
    } else {
        cmd.join(" ")
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let (workdir, port) = match connect_or_spawn(&cwd, true) {
        Ok(p) => p,
        Err(e) => { eprintln!("qsub: cannot connect to wrapper: {}", e); std::process::exit(1); }
    };

    let req = format!("SUBMIT\n{}\n{}\n", deps.join(" "), encode_text(&cmd_str));
    let addr = match udp_request(port, &req, Duration::from_secs(5)) {
        Ok(reply) => {
            let r = reply.trim().to_string();
            if let Some(addr) = r.strip_prefix("OK ") {
                println!("{}", addr);
                eprintln!("[qsub] node {}  workdir {}", addr, workdir.display());
                addr.to_string()
            } else {
                eprintln!("qsub: {}", r);
                std::process::exit(1);
            }
        }
        Err(e) => { eprintln!("qsub: udp error: {}", e); std::process::exit(1); }
    };

    // Implicit qwait on the address we just submitted.
    let targets = vec![addr.clone()];
    let failed_any = match wait_for_addrs(&workdir, port, &targets, false, "qsub") {
        Ok((f, _)) => f,
        Err(e) => {
            eprintln!("qsub: wait error: {}", e);
            eprintln!("[qsub] exit=1");
            std::process::exit(1);
        }
    };

    let exit = if failed_any { 1 } else { 0 };
    eprintln!("[qsub] exit={}", exit);
    std::process::exit(exit);
}
