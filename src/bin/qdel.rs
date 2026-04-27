//! qdel — kill a node by bit_addr.
//!
//! Usage: qdel <addr> [<addr>...]

use std::path::PathBuf;
use std::time::Duration;

use tren::{
    bit_addr_to_id, build_kill, find_all_alive_spills, find_workdir,
    frame_request, read_port,
};

fn main() {
    let targets: Vec<String> = std::env::args().skip(1).collect();
    if targets.is_empty() {
        eprintln!("usage: qdel <addr> [<addr>...]");
        std::process::exit(2);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let primary = match find_workdir(&cwd) {
        Some(p) => p,
        None    => { eprintln!("qdel: no .tren-NNN-<uuid>/ found"); std::process::exit(1); }
    };
    // Build the set of spills to ask: all alive siblings.
    let parent = primary.parent().unwrap_or(&cwd).to_path_buf();
    let spills = {
        let mut v = find_all_alive_spills(&parent);
        if !v.iter().any(|p| p == &primary) {
            v.push(primary.clone());
        }
        v
    };

    let mut had_err = false;
    for t in &targets {
        let id = match bit_addr_to_id(t) {
            Some(v) => v,
            None    => { eprintln!("{}: bad addr", t); had_err = true; continue; }
        };
        let mut killed = false;
        for spill in &spills {
            if !spill.join("tree").join(t).is_dir() { continue; }
            let port = match read_port(spill) {
                Ok(p)  => p,
                Err(_) => continue,
            };
            match frame_request(port, &build_kill(id), Duration::from_secs(3)) {
                Ok(r) => {
                    if r.op() == tren::OP_OK {
                        println!("{}: OK", t);
                        killed = true;
                        break;
                    } else {
                        println!("{}: ERR {}", t, r.get(1));
                    }
                }
                Err(e) => eprintln!("{}: udp error: {}", t, e),
            }
        }
        if !killed { had_err = true; }
    }
    if had_err { std::process::exit(1); }
}
