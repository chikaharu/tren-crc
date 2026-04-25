//! tren — PWD-local sparse-binary-tree job scheduler core library.
//!
//! See `README.md` (next to this crate's Cargo.toml) for an end-user guide
//! covering `source $TREN/env.sh`, `qsub` / `qbind` / `qmap` / `qwait` /
//! `qstat` / `qdel` / `qlog` / `qworkdir`, the bit_addr scheme, and the
//! on-disk layout summarised below.
//!
//! Layout in each working directory:
//!
//!   .tren-<uuid>/
//!     ├── port              UDP port the wrapper listens on (text)
//!     ├── pid               wrapper PID                     (text)
//!     ├── seq               next sequence number to allocate (text)
//!     └── tree/
//!         └── <bit_addr>/   directory per node; bit_addr is the binary
//!                           heap representation of the node ID
//!                           (root="1", left of root="10", right="11", …)
//!             ├── cmd       command string
//!             ├── state     WAITING | PENDING | RUNNING |
//!                           DONE(<exit>) | FAILED(<msg>)
//!             ├── pid       running subprocess PID (only while RUNNING)
//!             ├── log       merged stdout/stderr
//!             ├── exit_code numeric exit code (DONE only; not written
//!                           on FAILED)
//!             └── deps      space-separated bit_addr deps (omitted when
//!                           no deps were given)
//!
//! Node ID convention (binary-heap style):
//!   id 1 = root (bit_addr "1")
//!   left  child of id k  = 2k     (bit_addr suffixed with "0")
//!   right child of id k  = 2k + 1 (bit_addr suffixed with "1")
//!   parent  of id k      = k >> 1
//!   sibling of id k      = k ^ 1
//!   depth(id) = 63 - id.leading_zeros()  (root depth 0)
//!   path(id)  = id ^ (1 << depth(id))    (path bits from root)
//!
//! Allocation is BFS by sequence: the n-th SUBMIT receives ID n, naturally
//! producing a sparse but contiguous BFS layout.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const WORKDIR_PREFIX: &str = ".tren-";

/// Filename, inside the wrapper's `.tren-<uuid>/`, that records recent
/// background auto-GC removals so external tools (e.g. `qstat`) can
/// surface them. One line per removed sibling, format
/// `<unix_ts_secs> <path>`. Capped to [`AUTOGC_LOG_MAX`] entries.
pub const AUTOGC_LOG_NAME: &str = "autogc_log";

/// Maximum number of recent auto-GC removals retained in
/// `<workdir>/autogc_log`. Older entries are dropped FIFO.
pub const AUTOGC_LOG_MAX: usize = 16;

/// Convert a binary-heap node ID into its bit_addr string (used as
/// directory name). ID 1 → "1"; ID 5 → "101".
pub fn id_to_bit_addr(id: u64) -> String {
    debug_assert!(id >= 1);
    format!("{:b}", id)
}

/// Inverse of [`id_to_bit_addr`].
pub fn bit_addr_to_id(addr: &str) -> Option<u64> {
    if addr.is_empty() { return None; }
    u64::from_str_radix(addr, 2).ok().filter(|&n| n >= 1)
}

/// Depth of a node ID (root is 0).
pub fn depth_of(id: u64) -> u32 {
    if id == 0 { 0 } else { 63 - id.leading_zeros() }
}

/// Path-bits of the node from the root (excludes the leading "1").
pub fn path_of(id: u64) -> u64 {
    let d = depth_of(id);
    id ^ (1u64 << d)
}

/// Parent ID, or `None` if `id` is the root.
pub fn parent_of(id: u64) -> Option<u64> {
    if id <= 1 { None } else { Some(id >> 1) }
}

/// Sibling ID, or `None` if `id` is the root.
pub fn sibling_of(id: u64) -> Option<u64> {
    if id <= 1 { None } else { Some(id ^ 1) }
}

// ─── UUID (poor man's, no deps) ────────────────────────────────────────────

/// Generate a short hex token unique enough for `.tren-<uuid>/`.
pub fn fresh_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let mix = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15_u128)
        .wrapping_add(pid.wrapping_mul(0xBF58_476D_1CE4_E5B9_u128));
    format!("{:016x}", (mix as u64) ^ ((mix >> 64) as u64))
}

// ─── Workdir discovery ─────────────────────────────────────────────────────

/// Search for `.tren-<uuid>/` starting at `start` and walking up to `/`.
///
/// Returns the full path of the first **alive** workdir found while walking
/// upward (i.e. one whose recorded wrapper PID is still running). If no
/// alive workdir exists anywhere on the upward path, the *first* stale
/// workdir encountered is returned instead — `ensure_workdir()` then takes
/// care of reclaiming it before respawning. Returns `None` only when
/// nothing matching `.tren-*/port` exists at all.
///
/// Preferring alive workdirs prevents the historical pitfall where a stale
/// `.tren-*` shadows a newer alive sibling in the same directory and
/// causes `qsub` to keep redirecting requests to a dead wrapper.
pub fn find_workdir(start: &Path) -> Option<PathBuf> {
    let mut stale_fallback: Option<PathBuf> = None;
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if let Ok(rd) = fs::read_dir(dir) {
            let mut alive_at_level: Option<PathBuf> = None;
            let mut stale_at_level: Option<PathBuf> = None;
            for ent in rd.flatten() {
                let name = ent.file_name();
                let s = name.to_string_lossy();
                if !s.starts_with(WORKDIR_PREFIX) { continue; }
                let p = ent.path();
                if !(p.is_dir() && p.join("port").is_file()) { continue; }
                if wrapper_alive(&p) {
                    alive_at_level = Some(p);
                    break;
                } else if stale_at_level.is_none() {
                    stale_at_level = Some(p);
                }
            }
            if let Some(p) = alive_at_level {
                return Some(p);
            }
            if stale_fallback.is_none() {
                stale_fallback = stale_at_level;
            }
        }
        cur = dir.parent();
    }
    stale_fallback
}

/// Read the wrapper UDP port from a `.tren-<uuid>/` directory.
pub fn read_port(workdir: &Path) -> io::Result<u16> {
    let s = fs::read_to_string(workdir.join("port"))?;
    s.trim().parse::<u16>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read the wrapper PID from a `.tren-<uuid>/` directory.
pub fn read_wrapper_pid(workdir: &Path) -> io::Result<u32> {
    let s = fs::read_to_string(workdir.join("pid"))?;
    s.trim().parse::<u32>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Locate workdir from `cwd`; if absent, spawn `tren-wrapper` in `cwd` and
/// poll for the new workdir to appear. Returns the workdir path.
///
/// Race fix (v0.2.0): two concurrent `qsub` invocations in the same `cwd`
/// would each see `find_workdir() == None` and each spawn a wrapper,
/// resulting in two duplicate `.tren-<uuid>/` siblings. To prevent this,
/// the function takes an exclusive `flock` on `<cwd>/.tren.lock` for the
/// entire find→spawn→poll critical section. The lock is released
/// automatically when the `CwdLock` guard goes out of scope. The lock
/// file itself is a 0-byte sentinel that survives across runs and is
/// safely re-locked next time; auto-GC ignores it (it does not match the
/// `.tren-` prefix).
pub fn ensure_workdir(cwd: &Path, wrapper_bin: &Path) -> io::Result<PathBuf> {
    let _lock = CwdLock::acquire(cwd)?;
    if let Some(p) = find_workdir(cwd) {
        if wrapper_alive(&p) { return Ok(p); }
        // stale — remove and recreate
        let _ = fs::remove_dir_all(&p);
    }
    // spawn wrapper
    use std::process::{Command, Stdio};
    Command::new(wrapper_bin)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // poll for up to 5 seconds
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(p) = find_workdir(cwd) {
            if wrapper_alive(&p) { return Ok(p); }
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "tren-wrapper failed to come up within 5s",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Cwd-local advisory lock used by `ensure_workdir` to serialize the
/// find→spawn critical section across concurrent `qsub` processes in the
/// same working directory. Implemented with `libc::flock(LOCK_EX)` on a
/// sentinel `<cwd>/.tren.lock` file that is created once and never
/// removed. The lock is released automatically on `Drop` (file close).
struct CwdLock {
    file: fs::File,
}

impl CwdLock {
    fn acquire(cwd: &Path) -> io::Result<Self> {
        let lock_path = cwd.join(".tren.lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        // Block until we can take an exclusive lock; flock auto-releases
        // when the file descriptor is closed (Drop).
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(CwdLock { file })
    }
}

impl Drop for CwdLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Check whether the wrapper process recorded in `<workdir>/pid` is alive.
pub fn wrapper_alive(workdir: &Path) -> bool {
    match read_wrapper_pid(workdir) {
        Ok(pid) => unsafe { libc::kill(pid as libc::pid_t, 0) == 0 },
        Err(_)  => false,
    }
}

// ─── UDP request/reply primitive ───────────────────────────────────────────

/// Send `req` to the wrapper at `port` and return its reply, with a timeout.
pub fn udp_request(port: u16, req: &str, timeout: Duration) -> io::Result<String> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    sock.set_read_timeout(Some(timeout))?;
    sock.send_to(req.as_bytes(), ("127.0.0.1", port))?;
    let mut buf = vec![0u8; 64 * 1024];
    let (n, _src) = sock.recv_from(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Bind a UDP socket on an arbitrary free localhost port.
pub fn bind_free_udp() -> io::Result<UdpSocket> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    Ok(sock)
}

// ─── Per-node file helpers ─────────────────────────────────────────────────

pub fn node_dir(workdir: &Path, addr: &str) -> PathBuf {
    workdir.join("tree").join(addr)
}

pub fn read_node_file(workdir: &Path, addr: &str, file: &str) -> io::Result<String> {
    let mut s = String::new();
    fs::File::open(node_dir(workdir, addr).join(file))?.read_to_string(&mut s)?;
    Ok(s)
}

pub fn write_node_file(workdir: &Path, addr: &str, file: &str, contents: &str) -> io::Result<()> {
    let dir = node_dir(workdir, addr);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join(file), contents)
}

/// List all node bit_addrs currently in `<workdir>/tree/`.
pub fn list_nodes(workdir: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let tree = workdir.join("tree");
    if !tree.exists() { return Ok(out); }
    for ent in fs::read_dir(tree)? {
        let ent = ent?;
        if ent.file_type()?.is_dir() {
            if let Some(s) = ent.file_name().to_str() {
                out.push(s.to_string());
            }
        }
    }
    // sort by ID (i.e. numerically by parsed binary)
    out.sort_by_key(|a| bit_addr_to_id(a).unwrap_or(0));
    Ok(out)
}

// ─── Auto-GC log (background sibling cleanups) ────────────────────────────

/// Append a removal record to `<workdir>/autogc_log` and truncate the file
/// to the last [`AUTOGC_LOG_MAX`] entries. Each record is a single line
/// `<unix_ts_secs> <path>` so consumers can parse with one `splitn(2, ' ')`.
///
/// Called by the wrapper's background sweeper every time it successfully
/// removes a stale sibling workdir. Only the wrapper writes this file, so
/// no cross-process locking is needed; the worst a concurrent reader sees
/// is a slightly older snapshot.
pub fn record_autogc_removal(workdir: &Path, removed: &Path) -> io::Result<()> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut lines = read_autogc_log_lines(workdir);
    lines.push(format!("{} {}", secs, removed.display()));
    while lines.len() > AUTOGC_LOG_MAX {
        lines.remove(0);
    }
    let mut body = lines.join("\n");
    body.push('\n');
    fs::write(workdir.join(AUTOGC_LOG_NAME), body)
}

fn read_autogc_log_lines(workdir: &Path) -> Vec<String> {
    match fs::read_to_string(workdir.join(AUTOGC_LOG_NAME)) {
        Ok(s)  => s.lines().filter(|l| !l.is_empty()).map(str::to_string).collect(),
        Err(_) => Vec::new(),
    }
}

/// Read recent auto-GC removals recorded by the wrapper. Returns
/// `(unix_ts_secs, path)` pairs in oldest-first order. Returns an empty
/// vec when no log file exists or it is unreadable.
pub fn read_autogc_log(workdir: &Path) -> Vec<(u64, String)> {
    read_autogc_log_lines(workdir)
        .into_iter()
        .filter_map(|line| {
            let mut it = line.splitn(2, ' ');
            let ts   = it.next()?.parse::<u64>().ok()?;
            let path = it.next()?.to_string();
            Some((ts, path))
        })
        .collect()
}

/// Format a Unix timestamp (seconds since epoch) as
/// `YYYY-MM-DDTHH:MM:SSZ` UTC. Falls back to `"@<secs>"` if the libc
/// conversion fails.
pub fn format_unix_ts_utc(secs: u64) -> String {
    unsafe {
        let t: libc::time_t = secs as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&t, &mut tm).is_null() {
            return format!("@{}", secs);
        }
        let mut buf = [0u8; 32];
        let fmt = b"%Y-%m-%dT%H:%M:%SZ\0";
        let n = libc::strftime(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            fmt.as_ptr() as *const libc::c_char,
            &tm,
        );
        if n == 0 {
            return format!("@{}", secs);
        }
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

// ─── State enum ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeState {
    Waiting,
    Pending,
    Running,
    Done(i32),
    Failed(String),
}

impl NodeState {
    pub fn label(&self) -> String {
        match self {
            NodeState::Waiting   => "WAITING".into(),
            NodeState::Pending   => "PENDING".into(),
            NodeState::Running   => "RUNNING".into(),
            NodeState::Done(c)   => format!("DONE({})", c),
            NodeState::Failed(e) => format!("FAILED({})", e),
        }
    }
    pub fn parse(s: &str) -> Self {
        let t = s.trim();
        if t == "WAITING" { return NodeState::Waiting; }
        if t == "PENDING" { return NodeState::Pending; }
        if t == "RUNNING" { return NodeState::Running; }
        if let Some(rest) = t.strip_prefix("DONE(") {
            if let Some(end) = rest.find(')') {
                if let Ok(c) = rest[..end].parse::<i32>() {
                    return NodeState::Done(c);
                }
            }
        }
        if let Some(rest) = t.strip_prefix("FAILED(") {
            if let Some(end) = rest.rfind(')') {
                return NodeState::Failed(rest[..end].into());
            }
        }
        if t.starts_with("DONE")   { return NodeState::Done(0); }
        if t.starts_with("FAILED") { return NodeState::Failed(t.into()); }
        NodeState::Waiting
    }
    pub fn is_finished(&self) -> bool {
        matches!(self, NodeState::Done(_) | NodeState::Failed(_))
    }
    pub fn is_success(&self) -> bool {
        matches!(self, NodeState::Done(0))
    }
}

/// Read state of a node from disk (or NodeState::Waiting if file missing).
pub fn read_state(workdir: &Path, addr: &str) -> NodeState {
    match read_node_file(workdir, addr, "state") {
        Ok(s) => NodeState::parse(&s),
        Err(_) => NodeState::Waiting,
    }
}

// ─── Wait helper (shared by qsub / qwait) ──────────────────────────────────

/// Wait for one or more node `targets` to reach a finished state
/// (`DONE(_)` or `FAILED(_)`).
///
/// Subscribes to UDP `STATE` push events from the wrapper at `port` and
/// also re-polls each node's `state` file every ~500ms so a missed packet
/// can never leave us hanging.
///
/// Returns `(failed_any, elapsed)`:
///   - `failed_any` is `true` if at least one finished target ended in
///     `DONE(N>0)` or `FAILED(_)`.
///   - `elapsed` is total time spent waiting.
///
/// Modes:
///   - `any_mode = false` (AND): block until **all** targets finish.
///   - `any_mode = true`  (OR):  block until **any** target finishes.
///
/// `progress_label` is used for periodic stderr lines of the form
/// `[<label>] X/Y done at Ns`, emitted at most once every 10 seconds.
pub fn wait_for_addrs(
    workdir: &Path,
    port: u16,
    targets: &[String],
    any_mode: bool,
    progress_label: &str,
) -> io::Result<(bool, Duration)> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    sock.set_read_timeout(Some(Duration::from_millis(500)))?;
    let my_port = sock.local_addr()?.port();
    for t in targets {
        let req = format!("SUB {} {}\n", t, my_port);
        let _ = sock.send_to(req.as_bytes(), ("127.0.0.1", port));
    }

    let start = Instant::now();
    let mut done: HashSet<String> = HashSet::new();
    let mut failed_any = false;

    // Initial state poll (covers nodes that already finished before we SUBed).
    for t in targets {
        let s = read_state(workdir, t);
        if s.is_finished() {
            done.insert(t.clone());
            if !s.is_success() { failed_any = true; }
        }
    }

    let mut buf = vec![0u8; 8 * 1024];
    let condition_met = |done: &HashSet<String>| {
        if any_mode { !done.is_empty() } else { done.len() == targets.len() }
    };

    let mut last_progress = 0u64;
    while !condition_met(&done) {
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
                for line in msg.lines() {
                    if let Some(rest) = line.strip_prefix("STATE ") {
                        let mut it = rest.splitn(2, ' ');
                        let addr  = it.next().unwrap_or("").to_string();
                        let state_s = it.next().unwrap_or("");
                        if !targets.contains(&addr) { continue; }
                        let s = NodeState::parse(state_s);
                        if s.is_finished() {
                            done.insert(addr);
                            if !s.is_success() { failed_any = true; }
                        }
                    }
                }
            }
            Err(_) => {
                // Timeout — re-poll filesystem in case we missed events.
                for t in targets {
                    if done.contains(t) { continue; }
                    let s = read_state(workdir, t);
                    if s.is_finished() {
                        done.insert(t.clone());
                        if !s.is_success() { failed_any = true; }
                    }
                }
            }
        }
        // Periodic progress line (rate-limited to once per 10s window).
        let elapsed = start.elapsed().as_secs();
        if elapsed > 0 && elapsed % 10 == 0 && elapsed != last_progress {
            last_progress = elapsed;
            eprintln!(
                "[{}] {}/{} done at {}s",
                progress_label, done.len(), targets.len(), elapsed,
            );
        }
    }

    let _ = sock.send_to(
        format!("UNSUB {}\n", my_port).as_bytes(),
        ("127.0.0.1", port),
    );

    Ok((failed_any, start.elapsed()))
}

// ─── Locate the wrapper binary ─────────────────────────────────────────────

/// Best-effort location of the `tren-wrapper` binary: same dir as current
/// executable, or PATH lookup.
pub fn wrapper_bin_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("tren-wrapper");
            if cand.exists() { return cand; }
        }
    }
    PathBuf::from("tren-wrapper")
}

/// URL-safe encoding for paths that travel through line-oriented protocol.
pub fn encode_text(s: &str) -> String {
    s.replace('%', "%25").replace('\n', "%0A").replace(' ', "%20")
}

pub fn decode_text(s: &str) -> String {
    s.replace("%0A", "\n").replace("%20", " ").replace("%25", "%")
}

/// Helper used by binaries: resolve the wrapper for `cwd`, optionally
/// auto-spawning. Returns `(workdir, port)`.
pub fn connect_or_spawn(cwd: &Path, auto_spawn: bool) -> io::Result<(PathBuf, u16)> {
    let workdir = if auto_spawn {
        ensure_workdir(cwd, &wrapper_bin_path())?
    } else {
        find_workdir(cwd).ok_or_else(|| io::Error::new(
            io::ErrorKind::NotFound,
            "no .tren-<uuid>/ found from cwd upward",
        ))?
    };
    let port = read_port(&workdir)?;
    Ok((workdir, port))
}

/// Convenience: open a writable file (truncating).
pub fn write_string(p: &Path, s: &str) -> io::Result<()> {
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(p)?;
    f.write_all(s.as_bytes())
}
