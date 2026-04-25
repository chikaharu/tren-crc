//! qlog — print the captured log of a node.

use std::path::PathBuf;
use tren::{find_workdir, node_dir};

fn main() {
    let addr = match std::env::args().nth(1) {
        Some(a) => a,
        None => { eprintln!("usage: qlog <bit_addr>"); std::process::exit(2); }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let workdir = find_workdir(&cwd).unwrap_or_else(|| {
        eprintln!("qlog: no .tren-<uuid>/ found"); std::process::exit(1);
    });
    let log_path = node_dir(&workdir, &addr).join("log");
    match std::fs::read_to_string(&log_path) {
        Ok(s) => print!("{}", s),
        Err(e) => { eprintln!("qlog: {}: {}", log_path.display(), e); std::process::exit(1); }
    }
}
