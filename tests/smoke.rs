//! Smoke integration tests for `tren-wrapper` + `qsub` + `qwait`.
//!
//! These spin up a real wrapper in an isolated temp directory and exercise
//! the end-to-end flow exposed by the binaries. They cover:
//!
//!   * a basic submit + wait round-trip,
//!   * a dependency chain (`qsub --after`) where the dependent job must
//!     run strictly after the parent finishes.
//!
//! The whole file is designed to finish well under 30 s on a cold build.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const QSUB:    &str = env!("CARGO_BIN_EXE_qsub");
const QWAIT:   &str = env!("CARGO_BIN_EXE_qwait");
const WRAPPER: &str = env!("CARGO_BIN_EXE_tren-wrapper");

/// RAII handle for a per-test sandbox: a fresh temp directory plus a
/// wrapper process pinned to it. Dropping the guard kills the wrapper and
/// removes the directory tree.
struct Sandbox {
    dir:     PathBuf,
    wrapper: Child,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tren-smoke-{}-{}-{}",
            label, std::process::id(), stamp,
        ));
        std::fs::create_dir_all(&dir).expect("mkdir sandbox");

        // Spawn the wrapper directly so the workdir lives *inside* `dir`
        // and `find_workdir` (which walks upward from cwd) hits ours first
        // before any leftover `.tren-*` further up the filesystem.
        let wrapper = Command::new(WRAPPER)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tren-wrapper");

        let sb = Sandbox { dir, wrapper };
        sb.wait_for_workdir();
        sb
    }

    fn wait_for_workdir(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if Self::find_workdir(&self.dir).is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("wrapper did not create .tren-*/port within 5s");
    }

    fn find_workdir(dir: &Path) -> Option<PathBuf> {
        for ent in std::fs::read_dir(dir).ok()?.flatten() {
            let name = ent.file_name();
            let s = name.to_string_lossy();
            if s.starts_with(".tren-") {
                let p = ent.path();
                if p.join("port").is_file() {
                    return Some(p);
                }
            }
        }
        None
    }

    fn qsub(&self, args: &[&str]) -> String {
        let out = Command::new(QSUB)
            .args(args)
            .current_dir(&self.dir)
            .output()
            .expect("spawn qsub");
        assert!(
            out.status.success(),
            "qsub {:?} failed (status {:?})\nstdout: {}\nstderr: {}",
            args,
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Like `qsub` but does not assert success — returns
    /// (process_exit, stdout, stderr). Used to verify failure semantics
    /// of the implicit qwait built into qsub.
    fn qsub_raw(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(QSUB)
            .args(args)
            .current_dir(&self.dir)
            .output()
            .expect("spawn qsub");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn qwait(&self, args: &[&str]) -> i32 {
        let status = Command::new(QWAIT)
            .args(args)
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn qwait");
        status.code().unwrap_or(-1)
    }

    fn read_node_state(&self, addr: &str) -> String {
        let workdir = Self::find_workdir(&self.dir).expect("workdir gone");
        std::fs::read_to_string(workdir.join("tree").join(addr).join("state"))
            .unwrap_or_default()
            .trim()
            .to_string()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // Best-effort: ask the wrapper to quit so it cleans up its workdir,
        // then make sure the process is gone before we wipe the sandbox.
        if let Some(workdir) = Self::find_workdir(&self.dir) {
            if let Ok(port_str) = std::fs::read_to_string(workdir.join("port")) {
                if let Ok(port) = port_str.trim().parse::<u16>() {
                    let _ = tren::udp_request(
                        port,
                        "QUIT\n",
                        Duration::from_millis(500),
                    );
                }
            }
        }
        let _ = self.wrapper.kill();
        let _ = self.wrapper.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `qsub` of a failing command must:
///   * still print the bit_addr to stdout,
///   * block until the job finishes,
///   * print `[qsub] exit=1` on stderr,
///   * exit the qsub process with code 1,
///   * leave the node in `DONE(<nonzero>)` on disk.
#[test]
fn smoke_qsub_failing_command_exits_one() {
    let sb = Sandbox::new("fail");
    let (rc, stdout, stderr) = sb.qsub_raw(&["false"]);
    let addr = stdout.trim();
    assert!(!addr.is_empty(),
        "qsub should still print the bit_addr (got stdout {:?})", stdout);
    assert_eq!(rc, 1,
        "qsub of `false` should exit 1 (stderr: {})", stderr);
    assert!(stderr.contains("[qsub] exit=1"),
        "stderr should contain `[qsub] exit=1`, got: {}", stderr);
    let state = sb.read_node_state(addr);
    assert!(
        state.starts_with("DONE(") && state != "DONE(0)",
        "node {} should be DONE(<nonzero>), got {:?}", addr, state,
    );
}

/// Submit + wait round-trip on a trivial job.
#[test]
fn smoke_qsub_then_qwait_succeeds() {
    let sb = Sandbox::new("basic");
    let addr = sb.qsub(&["true"]);
    assert!(!addr.is_empty(), "qsub returned no bit_addr");

    let exit = sb.qwait(&[&addr]);
    assert_eq!(exit, 0, "qwait should exit 0 on DONE(0)");

    let state = sb.read_node_state(&addr);
    assert!(
        state.starts_with("DONE("),
        "node {} should be DONE, got {:?}",
        addr,
        state,
    );
}

/// `--after` enforces ordering: the dependent job must observe the side
/// effect of the parent (here: a marker line in a shared log file) before
/// it runs its own command.
#[test]
fn smoke_dependency_runs_after_parent() {
    let sb = Sandbox::new("dep");

    // A appends "A" then sleeps; B (depending on A) appends "B".
    // With correct dependency handling, the file contains "A\nB\n" in
    // that order. If B raced ahead it would either be empty/absent on
    // disk or come before A.
    let a = sb.qsub(&["echo A >> shared.log && sleep 0.3"]);
    let b = sb.qsub(&["--after", &a, "--", "echo B >> shared.log"]);

    let exit = sb.qwait(&[&a, &b]);
    assert_eq!(exit, 0, "qwait of {a} {b} should exit 0");

    let log_path = sb.dir.join("shared.log");
    let log = std::fs::read_to_string(&log_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", log_path.display()));
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines,
        vec!["A", "B"],
        "shared log should record A before B (got {:?})",
        log,
    );

    assert!(sb.read_node_state(&a).starts_with("DONE("));
    assert!(sb.read_node_state(&b).starts_with("DONE("));
}

/// What triggers the wrapper to shut down for a given test case.
enum ShutdownKind {
    Signal(libc::c_int),
    UdpQuit,
}

/// Every documented normal-shutdown path (SIGINT / SIGTERM / SIGHUP /
/// UDP `QUIT`) must leave **no** `.tren-<uuid>/` directory behind: the
/// wrapper's own workdir cleanup must run even when worker threads are
/// actively writing state files at the moment of shutdown.
#[test]
fn smoke_workdir_removed_after_signal_shutdown() {
    let cases: &[(&str, ShutdownKind)] = &[
        ("sigterm", ShutdownKind::Signal(libc::SIGTERM)),
        ("sigint",  ShutdownKind::Signal(libc::SIGINT)),
        ("sighup",  ShutdownKind::Signal(libc::SIGHUP)),
        ("udpquit", ShutdownKind::UdpQuit),
    ];

    for (label, kind) in cases {
        let mut sb = Sandbox::new(label);

        // Submit a few short jobs in the background so worker threads are
        // actively writing state files when shutdown arrives. We use
        // `&` via `sh -c` to avoid qsub's implicit wait blocking the test.
        for _ in 0..3 {
            let _ = Command::new("sh")
                .arg("-c")
                .arg(format!("{} sleep 0.2 &", QSUB))
                .current_dir(&sb.dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        // Tiny pause so the SUBMITs land before we trigger shutdown.
        thread::sleep(Duration::from_millis(150));

        let workdir = Sandbox::find_workdir(&sb.dir)
            .expect("workdir should exist while wrapper is alive");
        assert!(workdir.is_dir());

        match kind {
            ShutdownKind::Signal(sig) => {
                let pid = sb.wrapper.id() as i32;
                unsafe { libc::kill(pid, *sig); }
            }
            ShutdownKind::UdpQuit => {
                let port_str = std::fs::read_to_string(workdir.join("port"))
                    .expect("read port file");
                let port: u16 = port_str.trim().parse().expect("parse port");
                let _ = tren::udp_request(
                    port, "QUIT\n", Duration::from_millis(500),
                );
            }
        }

        // Wait for the wrapper to actually exit. Signal-driven paths
        // need ~250ms to observe SHUTDOWN plus a few hundred ms for the
        // cleanup pass; UDP QUIT is roughly the same.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match sb.wrapper.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = sb.wrapper.kill();
                        panic!("wrapper did not exit within 5s of {label}");
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait: {e}"),
            }
        }

        // The wrapper has exited cleanly — the workdir must be gone.
        assert!(
            !workdir.exists(),
            "{label}: workdir {} should have been removed by wrapper shutdown",
            workdir.display(),
        );
        assert!(
            Sandbox::find_workdir(&sb.dir).is_none(),
            "{label}: no .tren-* should remain under sandbox after shutdown",
        );

        // Forget the Sandbox guard's QUIT/kill path — the wrapper is
        // already gone. Drop will still wipe the temp dir for us.
        drop(sb);
    }
}

/// v0.2.0 race fix: N concurrent `qsub` invocations in the *same* fresh
/// cwd (no wrapper pre-existing) must end up sharing exactly one
/// `.tren-<uuid>/` workdir. Previously each racer would `find_workdir`
/// → `None` and each spawn its own wrapper, leaving 2+ duplicate
/// siblings. The `.tren.lock` flock in `ensure_workdir` serializes the
/// find→spawn critical section.
#[test]
fn smoke_concurrent_qsub_creates_single_workdir() {
    // Fresh sandbox without a pre-spawned wrapper — we let qsub itself
    // trigger `ensure_workdir`.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "tren-race-{}-{}", std::process::id(), stamp,
    ));
    std::fs::create_dir_all(&dir).expect("mkdir race sandbox");

    // Launch N qsub processes in parallel and wait for all of them.
    const N: usize = 6;
    let handles: Vec<_> = (0..N).map(|i| {
        let dir = dir.clone();
        thread::spawn(move || {
            Command::new(QSUB)
                .arg(format!("echo race-{}", i))
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .status()
                .expect("spawn qsub")
        })
    }).collect();
    for h in handles {
        let st = h.join().expect("qsub thread");
        assert!(st.success(), "qsub exit status: {:?}", st);
    }

    // Count `.tren-*` siblings directly under the sandbox.
    let mut workdirs: Vec<PathBuf> = Vec::new();
    for ent in std::fs::read_dir(&dir).expect("read sandbox") {
        let ent = ent.expect("dirent");
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(".tren-") && ent.file_type().unwrap().is_dir() {
            workdirs.push(ent.path());
        }
    }
    assert_eq!(
        workdirs.len(), 1,
        "race fix violated: expected exactly 1 .tren-* under {}, got {}: {:?}",
        dir.display(), workdirs.len(), workdirs,
    );

    // Tear down: shut the lone wrapper down via UDP QUIT so the test
    // does not leak processes, then wipe the sandbox.
    if let Some(wd) = workdirs.first() {
        if let Ok(port_str) = std::fs::read_to_string(wd.join("port")) {
            if let Ok(port) = port_str.trim().parse::<u16>() {
                let _ = tren::udp_request(
                    port, "QUIT\n", Duration::from_millis(500),
                );
            }
        }
    }
    // Give the wrapper a moment to exit before removing the dir.
    thread::sleep(Duration::from_millis(200));
    let _ = std::fs::remove_dir_all(&dir);
}
