//! qworkdir — print the on-disk directory for a node (or for the wrapper),
//! or garbage-collect stale `.tren-<uuid>/` workdirs under cwd.
//!
//! Usage:
//!   qworkdir              # print wrapper workdir (.tren-<uuid>/)
//!   qworkdir <bit_addr>   # print tree/<bit_addr>/
//!   qworkdir --gc         # recursively delete stale .tren-<uuid>/ under cwd
//!   qworkdir --gc -n      # dry-run (only list what would be removed)

use std::io;
use std::path::{Path, PathBuf};
use tren::{find_workdir, node_dir, wrapper_alive, WORKDIR_PREFIX};

fn main() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--gc") => {
            let dry_run = matches!(args.next().as_deref(), Some("-n") | Some("--dry-run"));
            if let Err(e) = gc_stale(&cwd, dry_run) {
                eprintln!("qworkdir --gc: {}", e);
                std::process::exit(1);
            }
        }
        Some(a) => {
            let workdir = find_workdir(&cwd).unwrap_or_else(|| {
                eprintln!("qworkdir: no .tren-<uuid>/ found");
                std::process::exit(1);
            });
            println!("{}", node_dir(&workdir, a).display());
        }
        None => {
            let workdir = find_workdir(&cwd).unwrap_or_else(|| {
                eprintln!("qworkdir: no .tren-<uuid>/ found");
                std::process::exit(1);
            });
            println!("{}", workdir.display());
        }
    }
}

/// Recursively walk `start`, find `.tren-<uuid>/` directories whose recorded
/// wrapper PID is no longer alive, and delete them. Alive workdirs are
/// listed but kept. Hidden directories (other than `.tren-*` themselves)
/// are skipped to avoid wandering into `.git` etc.
fn gc_stale(start: &Path, dry_run: bool) -> io::Result<()> {
    let mut stack = vec![start.to_path_buf()];
    let mut removed = 0usize;
    let mut would_remove = 0usize;
    let mut alive = 0usize;
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let ft = match ent.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_symlink() || !ft.is_dir() { continue; }
            let name = ent.file_name();
            let s = name.to_string_lossy();
            let p = ent.path();
            if s.starts_with(WORKDIR_PREFIX) {
                // Only treat dirs that look like real wrapper workdirs as
                // GC candidates. `port` + `pid` are written by the wrapper
                // at startup; refusing to touch anything else avoids
                // accidentally nuking a hand-rolled `.tren-foo` directory.
                let looks_like_workdir =
                    p.join("port").is_file() && p.join("pid").is_file();
                if !looks_like_workdir {
                    println!("skipped (not a wrapper workdir): {}", p.display());
                    continue;
                }
                if wrapper_alive(&p) {
                    println!("alive  : {}", p.display());
                    alive += 1;
                } else if dry_run {
                    println!("would remove: {}", p.display());
                    would_remove += 1;
                } else {
                    match std::fs::remove_dir_all(&p) {
                        Ok(()) => {
                            println!("removed: {}", p.display());
                            removed += 1;
                        }
                        Err(e) => eprintln!("failed to remove {}: {}", p.display(), e),
                    }
                }
                // do not descend into a `.tren-*` directory
                continue;
            }
            // skip hidden dirs that aren't `.tren-*` (e.g. `.git`, `.local`)
            if s.starts_with('.') { continue; }
            stack.push(p);
        }
    }
    if dry_run {
        eprintln!(
            "qworkdir --gc (dry-run): {} stale workdir(s) would be removed, {} alive kept",
            would_remove, alive
        );
    } else {
        eprintln!(
            "qworkdir --gc: removed {} stale workdir(s), kept {} alive",
            removed, alive
        );
    }
    Ok(())
}
