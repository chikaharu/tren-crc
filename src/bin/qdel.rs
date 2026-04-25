//! qdel — kill a node by bit_addr.
//!
//! Usage: qdel <addr> [<addr>...]

use std::path::PathBuf;
use std::time::Duration;

use tren::{find_workdir, read_port, udp_request};

fn main() {
    let targets: Vec<String> = std::env::args().skip(1).collect();
    if targets.is_empty() {
        eprintln!("usage: qdel <addr> [<addr>...]");
        std::process::exit(2);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let workdir = match find_workdir(&cwd) {
        Some(p) => p,
        None    => { eprintln!("qdel: no .tren-<uuid>/ found"); std::process::exit(1); }
    };
    let port = read_port(&workdir).unwrap_or_else(|e| {
        eprintln!("qdel: read port: {}", e); std::process::exit(1);
    });

    let mut had_err = false;
    for t in &targets {
        let req = format!("KILL {}\n", t);
        match udp_request(port, &req, Duration::from_secs(3)) {
            Ok(reply) => {
                let r = reply.trim();
                println!("{}: {}", t, r);
                if !r.starts_with("OK") { had_err = true; }
            }
            Err(e) => { eprintln!("{}: udp error: {}", t, e); had_err = true; }
        }
    }
    if had_err { std::process::exit(1); }
}
