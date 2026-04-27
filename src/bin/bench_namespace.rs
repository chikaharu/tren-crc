//! bench_namespace — measure throughput of repeated qsub submissions
//! across one and many spills.
//!
//! Usage:
//!   bench_namespace                # default 200 jobs
//!   bench_namespace 1000           # 1000 jobs
//!
//! For each scenario it prints elapsed wall time and ops/sec.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

fn main() {
    let n: usize = std::env::args().nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);

    let qsub = std::env::var("BENCH_QSUB_BIN")
        .unwrap_or_else(|_| "qsub".to_string());

    let workdir_root = std::env::temp_dir().join(format!("tren-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap().as_nanos()));
    std::fs::create_dir_all(&workdir_root).unwrap();
    let cwd: PathBuf = workdir_root.clone();

    eprintln!("[bench_namespace] qsub={} jobs={} cwd={}",
        qsub, n, cwd.display());

    let start = Instant::now();
    for i in 0..n {
        let st = Command::new(&qsub)
            .arg(format!("true # bench {}", i))
            .current_dir(&cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn qsub");
        if !st.success() {
            eprintln!("[bench_namespace] qsub #{} failed (exit {:?})",
                i, st.code());
        }
    }
    let elapsed = start.elapsed();
    let ops = n as f64 / elapsed.as_secs_f64();

    println!("jobs={} elapsed={:.3}s ops/s={:.1}", n, elapsed.as_secs_f64(), ops);

    // Count how many spills the run produced (one per 64 nodes).
    let mut count = 0usize;
    if let Ok(rd) = std::fs::read_dir(&cwd) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            if name.to_string_lossy().starts_with(".tren-") {
                count += 1;
            }
        }
    }
    println!("spills_created={}", count);

    // Cleanup wrappers + workdirs.
    if let Ok(rd) = std::fs::read_dir(&cwd) {
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_dir() { continue; }
            let port_file = p.join("port");
            if let Ok(s) = std::fs::read_to_string(&port_file) {
                if let Ok(port) = s.trim().parse::<u16>() {
                    let _ = tren::send_quit(port);
                }
            }
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let _ = std::fs::remove_dir_all(&cwd);
}
