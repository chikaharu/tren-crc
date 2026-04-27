//! qstat — show the state of every node in the local tren tree.
//!
//! Walks up from cwd to discover `.tren-<uuid>/` and reads `tree/*/state`.
//! Output is sorted by binary-heap node id (BFS order).

use std::path::PathBuf;

use tren::{
    bit_addr_to_id, depth_of, find_workdir, format_unix_ts_utc, list_nodes,
    read_autogc_log, read_node_file, read_state,
};

fn main() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let workdir = match find_workdir(&cwd) {
        Some(p) => p,
        None    => { eprintln!("qstat: no .tren-<uuid>/ found from cwd upward"); std::process::exit(1); }
    };

    let nodes = list_nodes(&workdir).unwrap_or_default();

    println!("workdir: {}", workdir.display());
    println!("port:    {}", std::fs::read_to_string(workdir.join("port")).unwrap_or_default().trim());
    println!();
    println!("{:<10} {:>4} {:<14} {:<12} {}", "BIT_ADDR", "DPTH", "STATE", "PID", "CMD");
    println!("{}", "-".repeat(78));

    if nodes.is_empty() {
        println!("(empty)");
    } else {
        for addr in nodes {
            let id    = bit_addr_to_id(&addr).unwrap_or(0);
            let depth = depth_of(id);
            let state = read_state(&workdir, &addr);
            let pid   = read_node_file(&workdir, &addr, "pid").unwrap_or_default();
            let cmd   = read_node_file(&workdir, &addr, "cmd.sh")
                .or_else(|_| read_node_file(&workdir, &addr, "cmd"))
                .unwrap_or_default();
            println!("{:<10} {:>4} {:<14} {:<12} {}",
                addr, depth, state.label(), pid.trim(),
                cmd.lines().next().unwrap_or(""));
        }
    }

    // Surface recent background auto-GC removals so users get visible
    // feedback that the wrapper is keeping its parent directory tidy
    // without having to grep the wrapper's stderr log.
    let cleaned = read_autogc_log(&workdir);
    if !cleaned.is_empty() {
        println!();
        println!("Recently auto-cleaned (last {}):", cleaned.len());
        // Newest first reads better in a one-shot status command.
        for (ts, path) in cleaned.into_iter().rev() {
            println!("  {}  {}", format_unix_ts_utc(ts), path);
        }
    }
}
