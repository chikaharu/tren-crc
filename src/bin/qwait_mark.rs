//! qwait-mark — run qwait, then append `1`/`-1` to a marker file based on
//! the result.
//!
//! Usage:
//!   qwait-mark <addr> [<addr>...] [marker_path]
//!   qwait-mark [--marker <path>] [--any] <addr> [<addr>...]
//!
//! Marker convention:
//!   1   — every (or any, with --any) target ended in DONE(0)
//!  -1   — at least one target FAILED or DONE(N>0)

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

const RELEASE_QWAIT: &str =
    "/home/runner/workspace/artifacts/bitrag/scheduler/target/release/qwait";

fn resolve_qwait() -> Option<PathBuf> {
    let release = PathBuf::from(RELEASE_QWAIT);
    if release.exists() { return Some(release); }
    if let Ok(out) = Command::new("which").arg("qwait").output() {
        if out.status.success() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() { return Some(PathBuf::from(p)); }
        }
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: qwait-mark [--marker <path>] [--any] <addr> [<addr>...] [marker_path]");
        std::process::exit(2);
    }

    let mut explicit_marker: Option<PathBuf> = None;
    let mut qwait_args: Vec<String> = Vec::new();
    let mut positional: Vec<String> = Vec::new();
    let mut had_explicit_marker = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--marker" => {
                i += 1;
                if i >= args.len() { eprintln!("--marker needs path"); std::process::exit(2); }
                explicit_marker = Some(PathBuf::from(&args[i]));
                had_explicit_marker = true;
            }
            "--any" => qwait_args.push("--any".into()),
            "-h" | "--help" => {
                eprintln!("usage: qwait-mark [--marker <path>] [--any] <addr> ... [marker_path]");
                std::process::exit(0);
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    // Last positional is treated as marker if it does not look like a
    // bit_addr (ie. is not pure binary digits starting with '1') AND no
    // explicit --marker was provided.
    let mut marker = explicit_marker.unwrap_or_else(|| PathBuf::from("processed"));
    if !had_explicit_marker {
        if let Some(last) = positional.last() {
            let looks_like_addr = !last.is_empty()
                && last.starts_with('1')
                && last.chars().all(|c| c == '0' || c == '1');
            if !looks_like_addr {
                marker = PathBuf::from(positional.pop().unwrap());
            }
        }
    }

    if positional.is_empty() {
        eprintln!("qwait-mark: no targets given");
        std::process::exit(2);
    }
    qwait_args.extend(positional);

    let qwait = match resolve_qwait() {
        Some(p) => p,
        None => { eprintln!("qwait-mark: cannot find qwait binary"); std::process::exit(127); }
    };
    let status = Command::new(&qwait).args(&qwait_args).status();
    let exit = match status {
        Ok(s)  => s.code().unwrap_or(1),
        Err(e) => { eprintln!("qwait-mark: spawn qwait: {}", e); std::process::exit(1); }
    };
    let mark = if exit == 0 { "1" } else { "-1" };
    if let Err(e) = OpenOptions::new().create(true).append(true).open(&marker)
        .and_then(|mut f| f.write_all(format!("{}\n", mark).as_bytes()))
    {
        eprintln!("qwait-mark: write {}: {}", marker.display(), e);
        std::process::exit(1);
    }
    std::process::exit(exit);
}
