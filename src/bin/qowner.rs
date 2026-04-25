//! qowner — minimal port. The new tren architecture does not have a
//! central owner-registry; owner names are now plain tags exposed inside
//! the executed shell as `$TREN_OWNER`.
//!
//! This binary is kept for back-compat: it prints a notice and exits 0 for
//! known subcommands so that pipelines that invoke it do not break.
//!
//! Usage (all become no-ops with friendly output):
//!   qowner create <name> [--max-parallel N]
//!   qowner list
//!   qowner drop <name>
//!   qowner kill <name>

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("create") => {
            let name = args.get(1).cloned().unwrap_or_default();
            println!("[qowner] create '{}': owners are now plain tags (forwarded via TREN_OWNER); no daemon-side state to track.", name);
        }
        Some("list") => {
            println!("[qowner] list: owners are stored per-node under .tren-<uuid>/tree/<addr>/cmd; use qstat to inspect.");
        }
        Some("drop") => {
            let name = args.get(1).cloned().unwrap_or_default();
            println!("[qowner] drop '{}': no-op in the new tren arch.", name);
        }
        Some("kill") => {
            let name = args.get(1).cloned().unwrap_or_default();
            println!("[qowner] kill '{}': not supported in this version. Use qdel <addr> per node.", name);
        }
        _ => {
            eprintln!("usage: qowner {{create|list|drop|kill}} [args]");
            std::process::exit(2);
        }
    }
}
