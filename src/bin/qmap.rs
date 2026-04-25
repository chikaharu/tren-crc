//! qmap — scatter: submit one job per item, substituting `{}` with each
//! item in `<cmd_template>`.
//!
//! Usage:
//!   qmap <items_file_or_colon_list> <cmd_template>
//!   qmap --after <addr> [<addr>...] -- <items> <cmd_template>

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use tren::{connect_or_spawn, encode_text, udp_request};

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.len() < 2 {
        eprintln!("usage: qmap [--after <addr>...] [--] <items> <cmd_template>");
        std::process::exit(2);
    }

    let mut deps: Vec<String> = Vec::new();
    let mut pos:  Vec<String> = Vec::new();
    let mut sep_seen = false;
    let mut i = 0usize;
    while i < raw.len() {
        if !sep_seen && raw[i] == "--after" {
            i += 1;
            while i < raw.len() && raw[i] != "--" && !raw[i].starts_with("--") {
                deps.push(raw[i].clone());
                i += 1;
            }
            continue;
        }
        if !sep_seen && raw[i] == "--" { sep_seen = true; i += 1; continue; }
        pos.push(raw[i].clone());
        i += 1;
    }
    if pos.len() < 2 {
        eprintln!("qmap: need <items> <cmd_template>");
        std::process::exit(2);
    }
    let items_arg = &pos[0];
    let template  = pos[1..].join(" ");

    let items: Vec<String> = if std::path::Path::new(items_arg).exists() {
        fs::read_to_string(items_arg).unwrap_or_default()
            .lines().map(|l| l.trim().to_string())
            .filter(|s| !s.is_empty()).collect()
    } else if items_arg.contains(':') {
        items_arg.split(':').map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()).collect()
    } else {
        vec![items_arg.clone()]
    };
    if items.is_empty() {
        eprintln!("qmap: empty item list"); std::process::exit(2);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let (_workdir, port) = match connect_or_spawn(&cwd, true) {
        Ok(p) => p,
        Err(e) => { eprintln!("qmap: {}", e); std::process::exit(1); }
    };

    let mut addrs: Vec<String> = Vec::new();
    for item in &items {
        let cmd = template.replace("{}", item);
        let req = format!("SUBMIT\n{}\n{}\n", deps.join(" "), encode_text(&cmd));
        match udp_request(port, &req, Duration::from_secs(5)) {
            Ok(reply) => {
                let r = reply.trim();
                if let Some(a) = r.strip_prefix("OK ") {
                    addrs.push(a.to_string());
                    eprintln!("[qmap] {}  cmd: {}", a, cmd);
                } else {
                    eprintln!("qmap: SUBMIT failed: {}", r);
                    std::process::exit(1);
                }
            }
            Err(e) => { eprintln!("qmap: udp: {}", e); std::process::exit(1); }
        }
    }
    println!("{}", addrs.join(" "));
}
