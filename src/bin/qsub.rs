//! qsub — submit a job to the PWD-local tren wrapper, then block until it
//! finishes.
//!
//! Usage:
//!   qsub <cmd> [args...]
//!   qsub --after <addr> [<addr>...] -- <cmd> [args...]
//!   qsub --owner <name> <cmd>             # forwarded as TREN_OWNER env
//!
//! v0.3 protocol: the command body is shipped through the filesystem
//! inbox (`<workdir>/inbox/<token>.sh`) and the binary SUBMIT frame
//! carries only the deps + the inbox token.

use std::path::PathBuf;

use tren::{submit_cmd, wait_for_addrs};

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
        format!("export TREN_OWNER={}\n{}", o, cmd.join(" "))
    } else {
        cmd.join(" ")
    };

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let res = match submit_cmd(&cwd, &deps, &cmd_str) {
        Ok(r)  => r,
        Err(e) => { eprintln!("qsub: {}", e); std::process::exit(1); }
    };

    println!("{}", res.addr);
    eprintln!("[qsub] node {}  workdir {}", res.addr, res.workdir.display());

    let targets = vec![res.addr.clone()];
    let failed_any = match wait_for_addrs(&res.workdir, res.port, &targets, false, "qsub") {
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
