//! qrun — convenience wrapper that submits a command, waits for it, then
//! appends `1`/`-1` to a marker file.
//!
//! Usage:
//!   qrun [--marker <path>] [--any] <cmd> [args...]
//!   qrun [--marker <path>] --parallel <cmd1> ::: <cmd2> ::: ...

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use tren::submit_cmd;

const RELEASE_QWAIT: &str =
    "/home/runner/workspace/artifacts/bitrag/scheduler/target/release/qwait";

fn resolve_qwait() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("QRUN_QWAIT_BIN") {
        if !p.is_empty() {
            let pb = PathBuf::from(p);
            if pb.exists() { return Some(pb); }
        }
    }
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

fn append_marker(marker: &PathBuf, value: &str) {
    if let Err(e) = OpenOptions::new().create(true).append(true).open(marker)
        .and_then(|mut f| f.write_all(format!("{}\n", value).as_bytes()))
    {
        eprintln!("qrun: write {}: {}", marker.display(), e);
    }
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        eprintln!("usage: qrun [--marker <path>] [--any] <cmd> [args...]");
        eprintln!("       qrun [--marker <path>] --parallel <cmd1> ::: <cmd2> ::: ...");
        std::process::exit(2);
    }

    let mut marker = PathBuf::from("processed");
    let mut any_mode = false;
    let mut parallel = false;
    let mut tail: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "--marker" => {
                i += 1;
                if i >= raw.len() { eprintln!("--marker needs path"); std::process::exit(2); }
                marker = PathBuf::from(&raw[i]);
            }
            "--any"      => any_mode = true,
            "--parallel" => parallel = true,
            _ => { tail.extend_from_slice(&raw[i..]); break; }
        }
        i += 1;
    }
    if tail.is_empty() {
        eprintln!("qrun: no command");
        std::process::exit(2);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));

    let cmds: Vec<String> = if parallel {
        let mut out: Vec<String> = Vec::new();
        let mut buf: Vec<String> = Vec::new();
        for w in tail {
            if w == ":::" {
                if !buf.is_empty() { out.push(buf.join(" ")); buf.clear(); }
            } else {
                buf.push(w);
            }
        }
        if !buf.is_empty() { out.push(buf.join(" ")); }
        out
    } else {
        vec![tail.join(" ")]
    };

    let mut addrs: Vec<String> = Vec::new();
    for c in &cmds {
        match submit_cmd(&cwd, &[], c) {
            Ok(r)  => { eprintln!("[qrun] {}: {}", r.addr, c); addrs.push(r.addr); }
            Err(e) => { eprintln!("qrun: submit '{}': {}", c, e); std::process::exit(1); }
        }
    }

    let qwait = match resolve_qwait() {
        Some(p) => p,
        None    => { eprintln!("qrun: qwait binary not found"); std::process::exit(127); }
    };
    let mut qwait_args: Vec<String> = Vec::new();
    if any_mode { qwait_args.push("--any".into()); }
    qwait_args.extend(addrs.iter().cloned());

    let status = Command::new(&qwait).args(&qwait_args).status();
    let exit = match status {
        Ok(s)  => s.code().unwrap_or(1),
        Err(e) => { eprintln!("qrun: qwait spawn: {}", e); std::process::exit(1); }
    };
    append_marker(&marker, if exit == 0 { "1" } else { "-1" });
    std::process::exit(exit);
}
