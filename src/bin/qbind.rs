//! qbind — gather: submit a job that runs after every listed dependency
//! finishes successfully. Each invocation allocates a fresh namespace
//! token (logical group tag) under the active spill.
//!
//! Usage:
//!   qbind <addr1> [<addr2>...] -- <cmd> [args...]

use std::path::PathBuf;

use tren::submit_cmd;

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

    let cmd_str = cmd.join(" ");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let ns = tren::fresh_token();
    let body = format!("export TREN_NS={}\n{}", ns, cmd_str);

    match submit_cmd(&cwd, &deps, &body) {
        Ok(r) => {
            println!("{}", r.addr);
            eprintln!("[qbind] node {}  ns={}  deps {:?}  workdir {}",
                r.addr, ns, deps, r.workdir.display());
        }
        Err(e) => { eprintln!("qbind: {}", e); std::process::exit(1); }
    }
}
