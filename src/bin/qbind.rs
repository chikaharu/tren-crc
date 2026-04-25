//! qbind — gather: submit a job that runs after every listed dependency
//! finishes successfully.
//!
//! Usage:
//!   qbind <addr1> [<addr2>...] -- <cmd> [args...]

use std::path::PathBuf;
use std::time::Duration;

use tren::{connect_or_spawn, encode_text, udp_request};

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        eprintln!("usage: qbind <addr> [<addr>...] -- <cmd> [args...]");
        std::process::exit(2);
    }
    let mut deps: Vec<String> = Vec::new();
    let mut cmd:  Vec<String> = Vec::new();
    let mut sep_seen = false;
    for a in raw {
        if !sep_seen {
            if a == "--" { sep_seen = true; continue; }
            deps.push(a);
        } else {
            cmd.push(a);
        }
    }
    if deps.is_empty() || cmd.is_empty() {
        eprintln!("qbind: need <deps> -- <cmd>");
        std::process::exit(2);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let (workdir, port) = match connect_or_spawn(&cwd, true) {
        Ok(p) => p,
        Err(e) => { eprintln!("qbind: {}", e); std::process::exit(1); }
    };

    let cmd_str = cmd.join(" ");
    let req = format!("SUBMIT\n{}\n{}\n", deps.join(" "), encode_text(&cmd_str));
    match udp_request(port, &req, Duration::from_secs(5)) {
        Ok(reply) => {
            let r = reply.trim();
            if let Some(addr) = r.strip_prefix("OK ") {
                println!("{}", addr);
                eprintln!("[qbind] node {}  deps {:?}  workdir {}",
                    addr, deps, workdir.display());
            } else {
                eprintln!("qbind: {}", r);
                std::process::exit(1);
            }
        }
        Err(e) => { eprintln!("qbind: udp: {}", e); std::process::exit(1); }
    }
}
