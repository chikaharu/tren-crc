//! tren — PWD-local sparse-binary-tree job scheduler core library.
//!
//! v0.4.0 changes vs v0.3.x:
//! * Default (feature OFF) ready-queue priority is now **subtree-size
//!   descending**. Each node tracks `subtree_size` and SUBMIT bumps every
//!   transitive ancestor by 1, so jobs that root a long chain (i.e. lie
//!   on the critical path) drain the queue ahead of leaf-only jobs.
//! * `feature = "model"` no longer fork-execs an external `tren-model`
//!   binary. Instead the wrapper carries an in-process **Frame32-resident
//!   decision tree** (depth-4 ID3, branchless infer, 128-byte tree),
//!   trains on accumulated SUBMIT outcomes, and infers per-job priority
//!   in tens of instructions. The external `tren-model` interface is
//!   gone (breaking change) — see `src/priority.rs`.
//! * Tunables: `TREN_MODEL_BUFFER` (training-buffer cap, default 256) and
//!   `TREN_MODEL_RETRAIN_EVERY` (samples between retrains, default 32).
//! * New `bench_priority` binary compares FIFO / subtree-size / decision
//!   tree on random DAGs.
//!
//! v0.3.0 changes vs v0.2.x:
//! * Workdir naming changed from `.tren-<uuid>/` to `.tren-NNN-<uuid>/`
//!   (3-digit zero-padded sequence; auto-extends to 4 digits past 999).
//!   Each `.tren-NNN-<uuid>/` is one **spill**; each spill runs an
//!   independent wrapper bound to its own UDP port.
//! * Hard cap of 32 leaves per spill. When the next SUBMIT would push the
//!   leaf count above 32, the receiving wrapper spawns a sibling wrapper
//!   for the next spill (`.tren-001-<uuid>/`, …) and replies with a
//!   `REDIRECT` frame pointing at that spill's port.
//! * `qbind` / `qmap` allocate a fresh **namespace** token per call so
//!   each scatter/gather group is logically separable (the namespace is
//!   purely a tag; spillover is driven by leaf count, not namespace).
//! * **All UDP messages are 128-byte binary [`Frame32`] frames** with a
//!   diagonal XOR parity bit per slot. The previous text protocol
//!   (`SUBMIT\n…`, `SUB <addr> <port>\n`, etc.) is gone.
//! * Job command bodies are no longer transported in UDP. Clients write
//!   `<workdir>/inbox/<client_token>.sh`, the SUBMIT frame carries the
//!   deps + the `client_token`, and the wrapper renames the inbox file
//!   to `<workdir>/tree/<bit_addr>/cmd.sh` and runs it via `bash cmd.sh`.
//!
//! Layout in each working directory (one per spill):
//!
//!   .tren-NNN-<uuid>/
//!     ├── port              UDP port the wrapper listens on (text)
//!     ├── pid               wrapper PID                     (text)
//!     ├── seq               next sequence number to allocate (text)
//!     ├── spill_seq         this spill's sequence number     (text)
//!     ├── next_port         port of next spill (only after a spillover)
//!     ├── inbox/            staging area for cmd.sh files
//!     │   └── <token>.sh    command body waiting to be allocated
//!     └── tree/
//!         └── <bit_addr>/   directory per node; bit_addr is the binary
//!                           heap representation of the node ID
//!             ├── cmd.sh    command body executed via `bash cmd.sh`
//!             ├── state     WAITING | PENDING | RUNNING |
//!             │             DONE(<exit>) | FAILED(<msg>)
//!             ├── pid       running subprocess PID (only while RUNNING)
//!             ├── log       merged stdout/stderr
//!             ├── exit_code numeric exit code (DONE only)
//!             ├── deps      space-separated bit_addr deps
//!             └── ns        namespace token (set on SUBMIT)
//!
//! Node ID convention (binary-heap style):
//!   id 1 = root (bit_addr "1")
//!   left  child of id k  = 2k     (bit_addr suffixed with "0")
//!   right child of id k  = 2k + 1 (bit_addr suffixed with "1")
//!   parent  of id k      = k >> 1
//!   sibling of id k      = k ^ 1
//!   depth(id) = 63 - id.leading_zeros()  (root depth 0)

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod priority;

pub const WORKDIR_PREFIX: &str = ".tren-";

/// Maximum leaves allowed in one spill. The 33rd leaf triggers spillover
/// to a freshly spawned sibling spill.
pub const SPILL_LEAF_CAP: u64 = 32;

/// Maximum nodes a spill may host. Equals `2 * SPILL_LEAF_CAP` because in
/// BFS allocation the leaf count after N nodes is `ceil(N/2)`; that
/// reaches `SPILL_LEAF_CAP` precisely at `N == 2 * SPILL_LEAF_CAP`.
pub const SPILL_NODE_CAP: u64 = SPILL_LEAF_CAP * 2;

/// Filename, inside the wrapper's `.tren-NNN-<uuid>/`, that records recent
/// background auto-GC removals so external tools (e.g. `qstat`) can
/// surface them. One line per removed sibling, format
/// `<unix_ts_secs> <path>`. Capped to [`AUTOGC_LOG_MAX`] entries.
pub const AUTOGC_LOG_NAME: &str = "autogc_log";

/// Maximum number of recent auto-GC removals retained in
/// `<workdir>/autogc_log`. Older entries are dropped FIFO.
pub const AUTOGC_LOG_MAX: usize = 16;

/// Convert a binary-heap node ID into its bit_addr string.
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

/// Number of leaves in a spill that has allocated `node_count` nodes
/// using BFS heap allocation. After `n` nodes the leaves are exactly
/// the ids in `((n>>1) + 1) ..= n`, i.e. `n - n/2 = ceil(n/2)`.
pub fn leaves_for_node_count(node_count: u64) -> u64 {
    if node_count == 0 { 0 } else { node_count - (node_count / 2) }
}

// ─── UUID (poor man's, no deps) ────────────────────────────────────────────

/// Generate a short hex token unique enough for `.tren-NNN-<uuid>/`.
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

/// Generate a short u32 token used as a `client_token` for the inbox
/// staging file (`<workdir>/inbox/<token>.sh`). Combines current nanos,
/// pid, and a thread-local atomic counter so two concurrent submits
/// always get distinct tokens.
pub fn fresh_client_token() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let c = COUNTER.fetch_add(1, Ordering::Relaxed) as u64;
    let mix = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(pid.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(c.wrapping_mul(0x94D0_49BB_1331_11EB));
    // Keep within 31 bits so the value fits in a Frame32 data slot.
    ((mix as u32) ^ ((mix >> 32) as u32)) & 0x7FFF_FFFF
}

// ─── Workdir discovery ─────────────────────────────────────────────────────

/// Search for `.tren-NNN-<uuid>/` (or legacy `.tren-<uuid>/`) starting at
/// `start` and walking up to `/`.
///
/// Returns the **highest-numbered** alive spill at the closest level (so
/// new SUBMITs land on the most recent spill), falling back to a stale
/// one only if nothing alive is found anywhere on the upward path.
pub fn find_workdir(start: &Path) -> Option<PathBuf> {
    let mut stale_fallback: Option<PathBuf> = None;
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if let Ok(rd) = fs::read_dir(dir) {
            let mut alive: Vec<(u64, PathBuf)> = Vec::new();
            let mut stale: Vec<(u64, PathBuf)> = Vec::new();
            for ent in rd.flatten() {
                let name = ent.file_name();
                let s = name.to_string_lossy();
                if !s.starts_with(WORKDIR_PREFIX) { continue; }
                let p = ent.path();
                if !(p.is_dir() && p.join("port").is_file()) { continue; }
                let seq = parse_spill_seq(&s).unwrap_or(0);
                if wrapper_alive(&p) {
                    alive.push((seq, p));
                } else {
                    stale.push((seq, p));
                }
            }
            if !alive.is_empty() {
                alive.sort_by_key(|(s, _)| *s);
                return Some(alive.into_iter().last().unwrap().1);
            }
            if stale_fallback.is_none() && !stale.is_empty() {
                stale.sort_by_key(|(s, _)| *s);
                stale_fallback = Some(stale.into_iter().last().unwrap().1);
            }
        }
        cur = dir.parent();
    }
    stale_fallback
}

/// Walk up from `start`, returning every alive spill at the closest level
/// (sorted by spill_seq ascending). Used when a client needs to query
/// across all spills (e.g. `qwait` on an addr that may live in any spill).
pub fn find_all_alive_spills(start: &Path) -> Vec<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        if let Ok(rd) = fs::read_dir(dir) {
            let mut alive: Vec<(u64, PathBuf)> = Vec::new();
            for ent in rd.flatten() {
                let name = ent.file_name();
                let s = name.to_string_lossy();
                if !s.starts_with(WORKDIR_PREFIX) { continue; }
                let p = ent.path();
                if !(p.is_dir() && p.join("port").is_file()) { continue; }
                if wrapper_alive(&p) {
                    let seq = parse_spill_seq(&s).unwrap_or(0);
                    alive.push((seq, p));
                }
            }
            if !alive.is_empty() {
                alive.sort_by_key(|(s, _)| *s);
                return alive.into_iter().map(|(_, p)| p).collect();
            }
        }
        cur = dir.parent();
    }
    Vec::new()
}

/// Parse the spill sequence number from a workdir name like
/// `.tren-007-abcdef…`. Returns `None` for legacy `.tren-<uuid>/`.
pub fn parse_spill_seq(name: &str) -> Option<u64> {
    let rest = name.strip_prefix(WORKDIR_PREFIX)?;
    let dash = rest.find('-')?;
    let head = &rest[..dash];
    if head.is_empty() || !head.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    head.parse::<u64>().ok()
}

/// Format a spill workdir name: `.tren-NNN-<uuid>` (3-digit minimum,
/// auto-expanding to 4+ digits past 999).
pub fn format_spill_name(seq: u64, token: &str) -> String {
    if seq < 1000 {
        format!("{}{:03}-{}", WORKDIR_PREFIX, seq, token)
    } else {
        // 4+ digits, no leading zero padding past the natural width.
        format!("{}{}-{}", WORKDIR_PREFIX, seq, token)
    }
}

/// Read the wrapper UDP port from a workdir.
pub fn read_port(workdir: &Path) -> io::Result<u16> {
    let s = fs::read_to_string(workdir.join("port"))?;
    s.trim().parse::<u16>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read the wrapper PID from a workdir.
pub fn read_wrapper_pid(workdir: &Path) -> io::Result<u32> {
    let s = fs::read_to_string(workdir.join("pid"))?;
    s.trim().parse::<u32>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Locate workdir from `cwd`; if absent, spawn `tren-wrapper` in `cwd` and
/// poll for the new workdir to appear. Returns the workdir path.
pub fn ensure_workdir(cwd: &Path, wrapper_bin: &Path) -> io::Result<PathBuf> {
    let _lock = CwdLock::acquire(cwd)?;
    if let Some(p) = find_workdir(cwd) {
        if wrapper_alive(&p) { return Ok(p); }
        let _ = fs::remove_dir_all(&p);
    }
    use std::process::{Command, Stdio};
    Command::new(wrapper_bin)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
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
/// find→spawn critical section across concurrent client processes in the
/// same working directory.
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

// ─── Frame32 binary protocol ───────────────────────────────────────────────

/// Op codes carried in the high 8 bits of Frame32 slot 0.
pub const OP_SUBMIT: u32   = 1;
pub const OP_SUB: u32      = 2;
pub const OP_UNSUB: u32    = 3;
pub const OP_STATE: u32    = 4;
pub const OP_OK: u32       = 5;
pub const OP_ERR: u32      = 6;
pub const OP_QUIT: u32     = 7;
pub const OP_PING: u32     = 8;
pub const OP_KILL: u32     = 9;
pub const OP_REDIRECT: u32 = 10;

/// State codes carried in STATE / OK frames.
pub const STATE_WAITING: u32 = 0;
pub const STATE_PENDING: u32 = 1;
pub const STATE_RUNNING: u32 = 2;
pub const STATE_DONE: u32    = 3;
pub const STATE_FAILED: u32  = 4;

/// Fixed 128-byte UDP frame: 32 × u32 slots laid out as a 32×32 bit
/// matrix. Bit `i` of slot `i` (the diagonal) holds the **even XOR parity**
/// of the other 31 bits in that slot, so a single-bit error in any row is
/// detected by [`Frame32::verify_parity`].
///
/// Wire format is big-endian per slot (network byte order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame32(pub [u32; 32]);

impl Frame32 {
    pub fn new() -> Self { Frame32([0u32; 32]) }

    /// Encode a 31-bit value into the data of slot `slot`, **leaving the
    /// diagonal parity bit (bit `slot`) untouched**. Call
    /// [`Self::update_parity`] before transmitting.
    pub fn set(&mut self, slot: usize, value: u32) {
        debug_assert!(slot < 32);
        let v = value & 0x7FFF_FFFF;
        let lo_mask = if slot == 0 { 0 } else { (1u32 << slot) - 1 };
        let lo = v & lo_mask;
        let hi = if slot >= 31 { 0 } else { (v >> slot) << (slot + 1) };
        let parity_keep = self.0[slot] & (1u32 << slot);
        self.0[slot] = parity_keep | lo | hi;
    }

    /// Inverse of [`Self::set`] — recover the 31-bit data value of slot
    /// `slot`, ignoring the diagonal parity bit.
    pub fn get(&self, slot: usize) -> u32 {
        debug_assert!(slot < 32);
        let s = self.0[slot] & !(1u32 << slot);
        let lo_mask = if slot == 0 { 0 } else { (1u32 << slot) - 1 };
        let lo = s & lo_mask;
        let hi = if slot >= 31 { 0 } else { (s >> (slot + 1)) << slot };
        (lo | hi) & 0x7FFF_FFFF
    }

    /// Recompute and write the diagonal XOR parity bit of every slot so
    /// every row's full 32-bit XOR is 0. Must be called immediately
    /// before serialising for transmission.
    pub fn update_parity(&mut self) {
        for i in 0..32 {
            let val = self.0[i] & !(1u32 << i);
            let p = (val.count_ones() & 1) as u32;
            self.0[i] = val | (p << i);
        }
    }

    /// Verify that every row's full 32-bit XOR is 0. Returns `false` on
    /// any single-bit error.
    pub fn verify_parity(&self) -> bool {
        (0..32).all(|i| (self.0[i].count_ones() & 1) == 0)
    }

    /// Op code (high 8 bits of the slot-0 data field).
    pub fn op(&self) -> u32 { (self.get(0) >> 23) & 0xFF }

    /// Set op + msg_seq into the slot-0 header. `msg_seq` is masked to 23
    /// bits.
    pub fn set_header(&mut self, op: u32, msg_seq: u32) {
        let h = ((op & 0xFF) << 23) | (msg_seq & 0x007F_FFFF);
        self.set(0, h);
    }

    pub fn msg_seq(&self) -> u32 { self.get(0) & 0x007F_FFFF }

    /// Bit-mask helpers. Treat the 32 slots' low bits collectively as a
    /// 32-leaf bitmap stored in slot `bitmap_slot` (must not be 0).
    pub fn leaf_bitmap(&self, slot: usize) -> u32 { self.get(slot) }
    pub fn set_leaf_bitmap(&mut self, slot: usize, mask: u32) { self.set(slot, mask & 0x7FFF_FFFF); }

    /// Serialise to 128 big-endian bytes. Refreshes parity first.
    pub fn to_bytes(&self) -> [u8; 128] {
        let mut frame = *self;
        frame.update_parity();
        let mut out = [0u8; 128];
        for i in 0..32 {
            out[i*4..(i+1)*4].copy_from_slice(&frame.0[i].to_be_bytes());
        }
        out
    }

    /// Deserialise from 128 big-endian bytes. Returns `Err` if length is
    /// wrong or parity check fails.
    pub fn from_bytes(b: &[u8]) -> Result<Self, FrameError> {
        if b.len() != 128 { return Err(FrameError::WrongLength(b.len())); }
        let mut f = Frame32::new();
        for i in 0..32 {
            f.0[i] = u32::from_be_bytes([b[i*4], b[i*4+1], b[i*4+2], b[i*4+3]]);
        }
        if !f.verify_parity() { return Err(FrameError::ParityMismatch); }
        Ok(f)
    }
}

impl Default for Frame32 {
    fn default() -> Self { Self::new() }
}

#[derive(Debug)]
pub enum FrameError {
    WrongLength(usize),
    ParityMismatch,
    Truncated,
    UnknownOp(u32),
    Io(io::Error),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::WrongLength(n)    => write!(f, "frame length {} != 128", n),
            FrameError::ParityMismatch    => write!(f, "frame parity mismatch"),
            FrameError::Truncated         => write!(f, "frame truncated"),
            FrameError::UnknownOp(o)      => write!(f, "unknown op {}", o),
            FrameError::Io(e)             => write!(f, "io: {}", e),
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self { FrameError::Io(e) }
}

// ─── Frame helpers (ergonomic wrappers per op) ─────────────────────────────

/// Build a SUBMIT frame: deps[0..30] then client_token in slot 31.
pub fn build_submit(deps: &[u64], client_token: u32, msg_seq: u32) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_SUBMIT, msg_seq);
    let n = deps.len().min(30);
    f.set(1, n as u32);
    for (i, d) in deps.iter().take(n).enumerate() {
        f.set(i + 2, (*d as u32) & 0x7FFF_FFFF);
    }
    f.set(31, client_token & 0x7FFF_FFFF);
    f
}

/// Decode a SUBMIT frame.
pub fn decode_submit(f: &Frame32) -> Option<(Vec<u64>, u32)> {
    if f.op() != OP_SUBMIT { return None; }
    let n = f.get(1) as usize;
    if n > 30 { return None; }
    let mut deps = Vec::with_capacity(n);
    for i in 0..n {
        deps.push(f.get(i + 2) as u64);
    }
    let token = f.get(31);
    Some((deps, token))
}

/// Build a STATE notification (single-node).
pub fn build_state(addr_id: u64, state_code: u32, payload: u32) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_STATE, 0);
    f.set(1, (addr_id as u32) & 0x7FFF_FFFF);
    f.set(2, state_code & 0x7FFF_FFFF);
    f.set(3, payload & 0x7FFF_FFFF);
    f
}

/// Decode a STATE notification.
pub fn decode_state(f: &Frame32) -> Option<(u64, u32, u32)> {
    if f.op() != OP_STATE { return None; }
    Some((f.get(1) as u64, f.get(2), f.get(3)))
}

/// Build an OK reply (returns the assigned bit_addr id and optional
/// payload field — used as the new spill's port for REDIRECT).
pub fn build_ok(value: u32) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_OK, 0);
    f.set(1, value & 0x7FFF_FFFF);
    f
}

pub fn build_err(code: u32) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_ERR, 0);
    f.set(1, code & 0x7FFF_FFFF);
    f
}

pub fn build_redirect(new_port: u16) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_REDIRECT, 0);
    f.set(1, new_port as u32);
    f
}

pub fn build_sub(filter_id: u64, sub_port: u16) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_SUB, 0);
    // 0 = wildcard
    f.set(1, (filter_id as u32) & 0x7FFF_FFFF);
    f.set(2, sub_port as u32);
    f
}

pub fn build_unsub(sub_port: u16) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_UNSUB, 0);
    f.set(1, sub_port as u32);
    f
}

pub fn build_kill(addr_id: u64) -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_KILL, 0);
    f.set(1, (addr_id as u32) & 0x7FFF_FFFF);
    f
}

pub fn build_quit() -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_QUIT, 0);
    f
}

pub fn build_ping() -> Frame32 {
    let mut f = Frame32::new();
    f.set_header(OP_PING, 0);
    f
}

// ─── UDP request/reply primitive ───────────────────────────────────────────

/// Send a binary frame to the wrapper at `port` and wait for a single
/// reply frame with a timeout.
pub fn frame_request(port: u16, req: &Frame32, timeout: Duration) -> Result<Frame32, FrameError> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    sock.set_read_timeout(Some(timeout))?;
    sock.send_to(&req.to_bytes(), ("127.0.0.1", port))?;
    let mut buf = [0u8; 128];
    let (n, _src) = sock.recv_from(&mut buf)?;
    Frame32::from_bytes(&buf[..n])
}

/// Send a binary frame fire-and-forget (no reply expected).
pub fn frame_send(port: u16, req: &Frame32) -> io::Result<()> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    sock.send_to(&req.to_bytes(), ("127.0.0.1", port))?;
    Ok(())
}

/// Convenience: ask the wrapper at `port` to shut down.
pub fn send_quit(port: u16) -> Result<(), FrameError> {
    let _ = frame_request(port, &build_quit(), Duration::from_millis(500));
    Ok(())
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
    out.sort_by_key(|a| bit_addr_to_id(a).unwrap_or(0));
    Ok(out)
}

// ─── Inbox staging (cmd.sh delivery) ──────────────────────────────────────

/// Write `body` to `<workdir>/inbox/<token>.sh` so the wrapper can rename
/// it into the allocated node directory on SUBMIT. Creates the inbox
/// directory if absent.
pub fn write_inbox(workdir: &Path, token: u32, body: &str) -> io::Result<PathBuf> {
    let dir = workdir.join("inbox");
    fs::create_dir_all(&dir)?;
    let p = dir.join(format!("{:08x}.sh", token));
    fs::write(&p, body)?;
    Ok(p)
}

/// Path of the inbox staging file for `token`.
pub fn inbox_path(workdir: &Path, token: u32) -> PathBuf {
    workdir.join("inbox").join(format!("{:08x}.sh", token))
}

// ─── Auto-GC log (background sibling cleanups) ────────────────────────────

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
    pub fn code(&self) -> u32 {
        match self {
            NodeState::Waiting => STATE_WAITING,
            NodeState::Pending => STATE_PENDING,
            NodeState::Running => STATE_RUNNING,
            NodeState::Done(_) => STATE_DONE,
            NodeState::Failed(_) => STATE_FAILED,
        }
    }
    /// Single u32 payload to ship inside a STATE frame. For `Done(c)`
    /// this is the unsigned exit code; for `Failed`, the lower 31 bits of
    /// a stable hash so subscribers can dedupe; otherwise 0.
    pub fn payload(&self) -> u32 {
        match self {
            NodeState::Done(c)   => (*c as i64 as u64) as u32 & 0x7FFF_FFFF,
            NodeState::Failed(s) => {
                let mut h: u32 = 2166136261;
                for b in s.bytes() {
                    h ^= b as u32;
                    h = h.wrapping_mul(16777619);
                }
                h & 0x7FFF_FFFF
            }
            _ => 0,
        }
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

/// Wait for one or more node `targets` (bit_addr strings) to reach a
/// finished state (`DONE(_)` or `FAILED(_)`).
///
/// Subscribes to UDP STATE push events from the wrapper at `port` (binary
/// frames) and also re-polls each node's `state` file every ~500 ms.
/// Targets that live in **other spills** are polled-only — the function
/// scans up the parent path for any `.tren-*` workdir whose `tree/<addr>`
/// directory exists.
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
    // Wildcard subscribe — we filter against `targets` ourselves.
    let _ = sock.send_to(&build_sub(0, my_port).to_bytes(), ("127.0.0.1", port));

    let start = Instant::now();
    let mut done: HashSet<String> = HashSet::new();
    let mut failed_any = false;

    // Initial state poll across the local spill and any sibling spills.
    let parents: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Some(parent) = workdir.parent() {
            v.extend(find_all_alive_spills(parent));
        }
        if !v.iter().any(|p| p == workdir) {
            v.push(workdir.to_path_buf());
        }
        v
    };
    let probe_state = |addr: &str| -> NodeState {
        for w in &parents {
            if node_dir(w, addr).exists() {
                let s = read_state(w, addr);
                if s != NodeState::Waiting { return s; }
            }
        }
        NodeState::Waiting
    };
    for t in targets {
        let s = probe_state(t);
        if s.is_finished() {
            done.insert(t.clone());
            if !s.is_success() { failed_any = true; }
        }
    }

    let mut buf = [0u8; 128];
    let condition_met = |done: &HashSet<String>| {
        if any_mode { !done.is_empty() } else { done.len() == targets.len() }
    };

    let mut last_progress = 0u64;
    while !condition_met(&done) {
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if n == 128 {
                    if let Ok(f) = Frame32::from_bytes(&buf[..n]) {
                        if let Some((id, _code, _payload)) = decode_state(&f) {
                            let addr = id_to_bit_addr(id);
                            if !targets.contains(&addr) { continue; }
                            let s = probe_state(&addr);
                            if s.is_finished() {
                                done.insert(addr);
                                if !s.is_success() { failed_any = true; }
                            }
                        }
                    }
                }
            }
            Err(_) => {
                // Timeout — re-poll filesystem (covers cross-spill targets).
                for t in targets {
                    if done.contains(t) { continue; }
                    let s = probe_state(t);
                    if s.is_finished() {
                        done.insert(t.clone());
                        if !s.is_success() { failed_any = true; }
                    }
                }
            }
        }
        let elapsed = start.elapsed().as_secs();
        if elapsed > 0 && elapsed % 10 == 0 && elapsed != last_progress {
            last_progress = elapsed;
            eprintln!(
                "[{}] {}/{} done at {}s",
                progress_label, done.len(), targets.len(), elapsed,
            );
        }
    }

    let _ = sock.send_to(&build_unsub(my_port).to_bytes(), ("127.0.0.1", port));

    Ok((failed_any, start.elapsed()))
}

// ─── Locate the wrapper binary ─────────────────────────────────────────────

pub fn wrapper_bin_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("tren-wrapper");
            if cand.exists() { return cand; }
        }
    }
    PathBuf::from("tren-wrapper")
}

/// Helper used by binaries: resolve the wrapper for `cwd`, optionally
/// auto-spawning. Returns `(workdir, port)`.
pub fn connect_or_spawn(cwd: &Path, auto_spawn: bool) -> io::Result<(PathBuf, u16)> {
    let workdir = if auto_spawn {
        ensure_workdir(cwd, &wrapper_bin_path())?
    } else {
        find_workdir(cwd).ok_or_else(|| io::Error::new(
            io::ErrorKind::NotFound,
            "no .tren-NNN-<uuid>/ found from cwd upward",
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

// ─── Submit-with-redirect (client side) ────────────────────────────────────

/// Result of a high-level SUBMIT call.
pub struct SubmitResult {
    /// Workdir where the node landed (may differ from the initial workdir
    /// if a spillover REDIRECT was followed).
    pub workdir: PathBuf,
    pub port: u16,
    pub addr: String,
}

/// High-level SUBMIT: writes the inbox staging file under the active
/// spill, sends a binary SUBMIT frame, and follows up to two REDIRECTs
/// caused by the active spill being full.
pub fn submit_cmd(
    cwd: &Path,
    deps_addrs: &[String],
    cmd_body: &str,
) -> Result<SubmitResult, String> {
    let (mut workdir, mut port) = connect_or_spawn(cwd, true)
        .map_err(|e| format!("connect: {}", e))?;
    let dep_ids: Vec<u64> = deps_addrs.iter()
        .map(|s| bit_addr_to_id(s).ok_or_else(|| format!("bad dep addr: {}", s)))
        .collect::<Result<Vec<_>, _>>()?;

    for _attempt in 0..3 {
        let token = fresh_client_token();
        write_inbox(&workdir, token, cmd_body)
            .map_err(|e| format!("inbox write: {}", e))?;
        let req = build_submit(&dep_ids, token, 0);
        let reply = frame_request(port, &req, Duration::from_secs(5))
            .map_err(|e| format!("submit udp: {}", e))?;
        match reply.op() {
            OP_OK => {
                let id = reply.get(1) as u64;
                let addr = id_to_bit_addr(id);
                return Ok(SubmitResult { workdir, port, addr });
            }
            OP_REDIRECT => {
                let new_port = reply.get(1) as u16;
                // Discard inbox file in old spill; rebuild on new one.
                let _ = fs::remove_file(inbox_path(&workdir, token));
                // Find the workdir that owns `new_port`.
                let mut found: Option<PathBuf> = None;
                let parent = cwd.parent().unwrap_or(cwd);
                // Poll briefly for the new spill to come up.
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline && found.is_none() {
                    let mut search_dirs = vec![cwd.to_path_buf()];
                    search_dirs.push(parent.to_path_buf());
                    for d in &search_dirs {
                        for s in find_all_alive_spills(d) {
                            if read_port(&s).ok() == Some(new_port) {
                                found = Some(s);
                                break;
                            }
                        }
                        if found.is_some() { break; }
                    }
                    if found.is_none() {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                let new_workdir = found.ok_or_else(||
                    format!("redirected to port {} but no matching spill workdir found", new_port))?;
                workdir = new_workdir;
                port = new_port;
                continue;
            }
            OP_ERR => {
                let code = reply.get(1);
                return Err(format!("wrapper ERR code {}", code));
            }
            other => return Err(format!("unexpected reply op {}", other)),
        }
    }
    Err("too many spillover redirects".into())
}
