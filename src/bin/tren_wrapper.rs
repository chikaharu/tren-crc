//! tren-wrapper — PWD-local job scheduler wrapper process (one per spill).
//!
//! Behaviour
//! =========
//! * Self-locates inside the current working directory. Creates a
//!   `.tren-NNN-<uuid>/` spill directory (3-digit padded sequence; if the
//!   parent directory already contains the same spill index because we're
//!   spawned by an existing wrapper, that wrapper passes the index via
//!   the `TREN_SPILL_SEQ` environment variable).
//! * Binds a free localhost UDP port; writes it to `<workdir>/port`.
//! * Records its own PID in `<workdir>/pid`.
//! * Listens for binary [`tren::Frame32`] datagrams. See `tren::OP_*` for
//!   op codes. The previous text protocol is gone.
//! * Spawns one Rust thread per allocated node. Each thread waits for
//!   dependencies, queues for a worker slot, then runs `bash <bit_addr>/cmd.sh`.
//! * Enforces a [`tren::SPILL_LEAF_CAP`] (32 leaves) per spill. The
//!   33rd leaf would push leaf count past the cap; we instead spawn a
//!   sibling wrapper for the next spill index and reply with a REDIRECT
//!   frame pointing at its UDP port.

use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::cmp::Ordering as CmpOrdering;
use std::fs;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tren::{
    bind_free_udp, bit_addr_to_id, build_ok, build_err, build_redirect,
    build_state, decode_submit, format_spill_name, fresh_token,
    id_to_bit_addr, inbox_path, leaves_for_node_count, node_dir, parse_spill_seq,
    read_state, write_string, Frame32, NodeState, OP_KILL, OP_PING, OP_QUIT,
    OP_SUB, OP_SUBMIT, OP_UNSUB, SPILL_LEAF_CAP, SPILL_NODE_CAP,
    WORKDIR_PREFIX,
};
#[cfg(feature = "model")]
use tren::priority::{encode_features, Features, Tree32};

/// Maximum number of concurrently running nodes (resource constraint).
const MAX_WORKERS: usize = 4;

/// Default training-buffer capacity for the `feature = "model"` build.
/// Override via env `TREN_MODEL_BUFFER`.
#[cfg(feature = "model")]
const DEFAULT_MODEL_BUFFER: usize = 256;

/// Default re-train interval (number of new samples between rebuilds).
/// Override via env `TREN_MODEL_RETRAIN_EVERY`.
#[cfg(feature = "model")]
const DEFAULT_RETRAIN_EVERY: usize = 32;

type SubMap = Arc<Mutex<HashMap<SocketAddr, u32>>>; // value = filter_id (0 = wildcard)
type ReadyQueue = Arc<Mutex<BinaryHeap<QueueItem>>>;
type RunningCount = Arc<Mutex<usize>>;
/// Port of the next spill, set when this spill spills over for the first
/// time. 0 means "no spillover yet".
type NextPort = Arc<AtomicU16>;

/// Bookkeeping captured at SUBMIT time for label generation when the
/// node finishes (used only when `feature = "model"` is on).
#[cfg(feature = "model")]
#[derive(Clone, Copy, Debug)]
struct SubmitMeta {
    features: u16,
}

#[cfg(feature = "model")]
type ModelState = Arc<Mutex<ModelInner>>;
#[cfg(feature = "model")]
struct ModelInner {
    tree:   Tree32,
    buffer: VecDeque<(u16, bool)>,
    /// Number of samples appended since last retrain.
    new_since_train: usize,
    /// Generation byte stamped into the Tree32 header on every retrain;
    /// wraps modulo 256.
    generation: u8,
}

#[derive(Debug)]
struct NodeRecord {
    deps:         Vec<String>,
    pid:          Option<u32>,
    state:        NodeState,
    priority:     f64,
    /// Number of nodes (including self) reachable from this node via
    /// transitive descendants in the SUBMIT-declared DAG. Initialised
    /// to 1 on SUBMIT and incremented for every new node that lists
    /// this address in its transitive ancestors.
    subtree_size: u64,
    /// Depth in the DAG = max(parent depth) + 1. Used as a feature.
    depth:        u32,
    /// First byte of cmd.sh (cached as a feature). Read for the model
    /// path; kept on the record for observability and future signals.
    #[allow(dead_code)]
    cmd_first_byte: u8,
    /// Bookkeeping for the in-process model (only present under
    /// `feature = "model"`).
    #[cfg(feature = "model")]
    meta:         Option<SubmitMeta>,
}

/// Queue ordering: bigger `subtree_size` first, then bigger `priority`,
/// then lower `addr` (lexicographic) as a stable tiebreaker. Implemented
/// against the standard `BinaryHeap` (max-heap) so larger-`Ord` items
/// pop first.
#[derive(Clone)]
struct QueueItem {
    subtree_size: u64,
    priority:     f64,
    addr:         String,
}

impl Eq for QueueItem {}
impl PartialEq for QueueItem {
    fn eq(&self, other: &Self) -> bool { self.cmp(other) == CmpOrdering::Equal }
}
impl Ord for QueueItem {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        match self.subtree_size.cmp(&other.subtree_size) {
            CmpOrdering::Equal => {}
            o                  => return o,
        }
        match self.priority.partial_cmp(&other.priority).unwrap_or(CmpOrdering::Equal) {
            CmpOrdering::Equal => {}
            o                  => return o,
        }
        // Lexicographically smaller addr should pop first → reverse.
        other.addr.cmp(&self.addr)
    }
}
impl PartialOrd for QueueItem {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> { Some(self.cmp(other)) }
}

type Tree = Arc<Mutex<HashMap<String, NodeRecord>>>;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sig(_: libc::c_int) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

#[cfg(feature = "model")]
fn model_buffer_capacity() -> usize {
    std::env::var("TREN_MODEL_BUFFER")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_MODEL_BUFFER)
}

#[cfg(feature = "model")]
fn model_retrain_every() -> usize {
    std::env::var("TREN_MODEL_RETRAIN_EVERY")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_RETRAIN_EVERY)
}

/// Walk transitive ancestors of `start_deps` (in the local spill's
/// `tree`) and bump each ancestor's `subtree_size` by 1. Cycles are
/// impossible (BFS allocation produces a DAG), but a `visited` set
/// guards against shared parents being counted twice.
fn bump_ancestor_subtree(tree: &Tree, start_deps: &[String]) {
    if start_deps.is_empty() { return; }
    let mut g = tree.lock().unwrap();
    let mut visited: HashSet<String> = HashSet::new();
    let mut q: VecDeque<String> = start_deps.iter().cloned().collect();
    while let Some(addr) = q.pop_front() {
        if !visited.insert(addr.clone()) { continue; }
        if let Some(rec) = g.get_mut(&addr) {
            rec.subtree_size = rec.subtree_size.saturating_add(1);
            for d in rec.deps.clone() {
                if !visited.contains(&d) { q.push_back(d); }
            }
        }
    }
}

/// Compute the depth of a freshly submitted node = max(dep depth) + 1.
fn compute_depth(tree: &Tree, deps: &[String]) -> u32 {
    if deps.is_empty() { return 0; }
    let g = tree.lock().unwrap();
    let mut m = 0u32;
    for d in deps {
        if let Some(r) = g.get(d) { m = m.max(r.depth); }
    }
    m + 1
}

struct WorkdirGuard {
    path: PathBuf,
}

impl Drop for WorkdirGuard {
    fn drop(&mut self) {
        for _ in 0..3 {
            if !self.path.exists() { return; }
            match fs::remove_dir_all(&self.path) {
                Ok(())                                                    => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound        => return,
                Err(_)                                                    => {}
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn cleanup_workdir(workdir: &Path, tree: &Tree) {
    {
        let g = tree.lock().unwrap();
        for (_addr, rec) in g.iter() {
            if let Some(pid) = rec.pid {
                unsafe { libc::kill(pid as i32, libc::SIGTERM); }
            }
        }
    }
    thread::sleep(Duration::from_millis(200));
    for _ in 0..5 {
        match fs::remove_dir_all(workdir) {
            Ok(())                                                    => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound        => break,
            Err(_)                                                    => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Determine this spill's sequence number. If `TREN_SPILL_SEQ` is set
/// (parent wrapper passed it), use that. Otherwise scan cwd for the
/// highest existing `.tren-NNN-*/` and use one past it (or 0 if none).
fn determine_spill_seq(cwd: &Path) -> u64 {
    if let Ok(s) = std::env::var("TREN_SPILL_SEQ") {
        if let Ok(n) = s.trim().parse::<u64>() { return n; }
    }
    let mut max_seen: Option<u64> = None;
    if let Ok(rd) = fs::read_dir(cwd) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            let s = name.to_string_lossy();
            if let Some(seq) = parse_spill_seq(&s) {
                max_seen = Some(max_seen.map(|m| m.max(seq)).unwrap_or(seq));
            }
        }
    }
    max_seen.map(|m| m + 1).unwrap_or(0)
}

fn main() {
    install_signal_handlers();

    let cwd = std::env::current_dir().expect("cwd");

    let spill_seq = determine_spill_seq(&cwd);
    let token   = fresh_token();
    let workdir = cwd.join(format_spill_name(spill_seq, &token));

    // Race protection: if some other process is racing to create the
    // same spill index, the file system will reject the second create.
    if let Err(e) = fs::create_dir_all(workdir.join("tree")) {
        eprintln!("[tren-wrapper] create workdir {} failed: {}", workdir.display(), e);
        std::process::exit(1);
    }
    let _ = fs::create_dir_all(workdir.join("inbox"));

    let _workdir_guard = WorkdirGuard { path: workdir.clone() };

    let socket = bind_free_udp().expect("bind udp");
    let port   = socket.local_addr().expect("local_addr").port();

    write_string(&workdir.join("port"), &port.to_string()).expect("write port");
    write_string(&workdir.join("pid"),  &std::process::id().to_string()).expect("write pid");
    write_string(&workdir.join("seq"),  "0").expect("write seq");
    write_string(&workdir.join("spill_seq"), &spill_seq.to_string()).expect("write spill_seq");

    eprintln!(
        "[tren-wrapper] up  cwd={}  workdir={}  port={}  pid={}  spill={}",
        cwd.display(), workdir.display(), port, std::process::id(), spill_seq,
    );

    let tree:        Tree         = Arc::new(Mutex::new(HashMap::new()));
    let subs:        SubMap       = Arc::new(Mutex::new(HashMap::new()));
    let ready_queue: ReadyQueue   = Arc::new(Mutex::new(BinaryHeap::new()));
    let running:     RunningCount = Arc::new(Mutex::new(0usize));
    let next_port:   NextPort     = Arc::new(AtomicU16::new(0));
    #[cfg(feature = "model")]
    let model: ModelState = Arc::new(Mutex::new(ModelInner {
        tree: Tree32::baseline(),
        buffer: VecDeque::with_capacity(model_buffer_capacity()),
        new_since_train: 0,
        generation: 0,
    }));

    {
        let tree = Arc::clone(&tree);
        let subs = Arc::clone(&subs);
        let workdir = workdir.clone();
        thread::spawn(move || reaper_loop(tree, subs, workdir));
    }
    if autogc_enabled() {
        let workdir = workdir.clone();
        thread::spawn(move || autogc_loop(workdir));
    }

    socket.set_read_timeout(Some(Duration::from_millis(250))).ok();
    let mut buf = [0u8; 128];
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) { break; }

        match socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                if n != 128 { continue; }
                let frame = match Frame32::from_bytes(&buf[..n]) {
                    Ok(f)  => f,
                    Err(e) => {
                        eprintln!("[tren-wrapper] bad frame: {}", e);
                        continue;
                    }
                };
                let reply = handle_request(
                    &frame, &cwd, &workdir, spill_seq, &tree, &subs,
                    &ready_queue, &running, &next_port,
                    #[cfg(feature = "model")] &model,
                    src,
                );
                if let Some(r) = reply {
                    let _ = socket.send_to(&r.to_bytes(), src);
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => eprintln!("[tren-wrapper] recv error: {}", e),
        }
    }

    eprintln!("[tren-wrapper] shutting down");
    cleanup_workdir(&workdir, &tree);
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(libc::SIGINT,  handle_sig as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handle_sig as libc::sighandler_t);
        libc::signal(libc::SIGHUP,  handle_sig as libc::sighandler_t);
    }
}

// ─── Request dispatch ──────────────────────────────────────────────────────

fn handle_request(
    frame:       &Frame32,
    cwd:         &Path,
    workdir:     &Path,
    spill_seq:   u64,
    tree:        &Tree,
    subs:        &SubMap,
    ready_queue: &ReadyQueue,
    running:     &RunningCount,
    next_port:   &NextPort,
    #[cfg(feature = "model")] model: &ModelState,
    src:         SocketAddr,
) -> Option<Frame32> {
    match frame.op() {
        OP_SUBMIT => {
            let (deps_ids, token) = match decode_submit(frame) {
                Some(v) => v,
                None    => return Some(build_err(1)),
            };

            // If this spill is already at cap, redirect (and ensure next
            // spill exists).
            let leaves_now = current_leaf_count(tree);
            if leaves_now >= SPILL_LEAF_CAP {
                let np = ensure_next_spill(cwd, spill_seq, next_port);
                if np == 0 { return Some(build_err(2)); }
                return Some(build_redirect(np));
            }

            let deps_addrs: Vec<String> = deps_ids.iter()
                .filter(|&&d| d > 0)
                .map(|&d| id_to_bit_addr(d))
                .collect();

            let alloc_result = allocate_node(
                workdir, tree, subs, ready_queue, running, deps_addrs, token,
                #[cfg(feature = "model")] model,
            );
            match alloc_result {
                Ok(addr) => Some(build_ok(bit_addr_to_id(&addr).unwrap_or(0) as u32)),
                Err(e)   => {
                    eprintln!("[tren-wrapper] allocate err: {}", e);
                    Some(build_err(3))
                }
            }
        }

        OP_KILL => {
            let id = frame.get(1) as u64;
            if id == 0 { return Some(build_err(4)); }
            let addr = id_to_bit_addr(id);
            match kill_node(workdir, tree, subs, &addr) {
                Ok(())   => Some(build_ok(0)),
                Err(_e)  => Some(build_err(5)),
            }
        }

        OP_SUB => {
            let filter_id = frame.get(1);
            let port      = frame.get(2) as u16;
            let dest = if port == 0 { src } else { SocketAddr::from(([127,0,0,1], port)) };
            subs.lock().unwrap().insert(dest, filter_id);
            Some(build_ok(0))
        }

        OP_UNSUB => {
            let port = frame.get(1) as u16;
            let dest = if port == 0 { src } else { SocketAddr::from(([127,0,0,1], port)) };
            subs.lock().unwrap().remove(&dest);
            Some(build_ok(0))
        }

        OP_QUIT => {
            SHUTDOWN.store(true, Ordering::SeqCst);
            Some(build_ok(0))
        }

        OP_PING => Some(build_ok(0)),

        // Unknown op — silently drop.
        _ => None,
    }
}

/// Current leaf count for this spill = number of nodes in the heap whose
/// id has no recorded child (`2*id` not present in tree).
fn current_leaf_count(tree: &Tree) -> u64 {
    let g = tree.lock().unwrap();
    if g.is_empty() { return 0; }
    let n = g.len() as u64;
    // Equivalent shortcut: BFS allocation guarantees leaves = ceil(n/2).
    leaves_for_node_count(n)
}

/// Ensure the next spill exists. If `next_port` is already non-zero,
/// return it. Otherwise spawn a sibling wrapper and poll for its port
/// file. Returns 0 on failure.
fn ensure_next_spill(cwd: &Path, my_seq: u64, next_port: &NextPort) -> u16 {
    let cur = next_port.load(Ordering::SeqCst);
    if cur != 0 { return cur; }

    let wrapper_bin = tren::wrapper_bin_path();
    let next_seq = my_seq + 1;
    let _ = Command::new(&wrapper_bin)
        .current_dir(cwd)
        .env("TREN_SPILL_SEQ", next_seq.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    // Poll for the new spill's port.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(rd) = fs::read_dir(cwd) {
            for ent in rd.flatten() {
                let name = ent.file_name();
                let s = name.to_string_lossy();
                if !s.starts_with(WORKDIR_PREFIX) { continue; }
                if parse_spill_seq(&s) != Some(next_seq) { continue; }
                let p = ent.path();
                if let Ok(port) = tren::read_port(&p) {
                    if tren::wrapper_alive(&p) {
                        next_port.store(port, Ordering::SeqCst);
                        eprintln!("[tren-wrapper] spillover seq={} port={} workdir={}",
                            next_seq, port, p.display());
                        return port;
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    0
}

// ─── Allocation & execution ───────────────────────────────────────────────

fn next_seq(workdir: &Path) -> u64 {
    let p = workdir.join("seq");
    let cur = fs::read_to_string(&p).ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = cur + 1;
    let _ = write_string(&p, &next.to_string());
    next
}

fn publish_state(
    workdir: &Path,
    addr:    &str,
    state:   &NodeState,
    subs:    &SubMap,
) {
    if !SHUTDOWN.load(Ordering::SeqCst) {
        let _ = write_string(&node_dir(workdir, addr).join("state"), &state.label());
    }
    let id = bit_addr_to_id(addr).unwrap_or(0);
    let frame = build_state(id, state.code(), state.payload());
    let bytes = frame.to_bytes();
    let snap: Vec<(SocketAddr, u32)> = subs.lock().unwrap()
        .iter().map(|(a,f)| (*a, *f)).collect();
    if snap.is_empty() { return; }
    if let Ok(sock) = UdpSocket::bind("127.0.0.1:0") {
        for (dest, filter) in snap {
            if filter == 0 || filter as u64 == id {
                let _ = sock.send_to(&bytes, dest);
            }
        }
    }
}

fn allocate_node(
    workdir:     &Path,
    tree:        &Tree,
    subs:        &SubMap,
    ready_queue: &ReadyQueue,
    running:     &RunningCount,
    deps:        Vec<String>,
    token:       u32,
    #[cfg(feature = "model")] model: &ModelState,
) -> Result<String, String> {
    let id   = next_seq(workdir);
    if id > SPILL_NODE_CAP {
        return Err(format!("spill node cap {} exceeded", SPILL_NODE_CAP));
    }
    let addr = id_to_bit_addr(id);
    let dir  = node_dir(workdir, &addr);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;

    // Read cmd first byte from the staging file BEFORE the rename — used
    // as a feature even when `model` is off (kept on the NodeRecord for
    // observability and as a future signal source).
    let stage = inbox_path(workdir, token);
    if !stage.exists() {
        return Err(format!("inbox file missing for token {:08x}", token));
    }
    let cmd_first_byte = first_nonws_byte(&stage).unwrap_or(0);

    let dst = dir.join("cmd.sh");
    fs::rename(&stage, &dst).map_err(|e| format!("rename inbox: {}", e))?;

    if !deps.is_empty() {
        let _ = fs::write(dir.join("deps"), deps.join(" "));
    }

    let initial = if deps.is_empty() { NodeState::Pending } else { NodeState::Waiting };

    // Bump transitive-ancestor subtree_size counters BEFORE inserting
    // the new node so the new node itself doesn't count itself.
    bump_ancestor_subtree(tree, &deps);

    let depth = compute_depth(tree, &deps);

    #[cfg(not(feature = "model"))]
    let priority = 0.0f64;

    #[cfg(feature = "model")]
    let (priority, features_u16): (f64, u16) = {
        // Estimate subtree size at this moment as max(dep.subtree_size).
        // (The new node's own subtree is unknown at SUBMIT — the
        // dep-side counter is the best static proxy for "is this on a
        // long chain".)
        let est_subtree = {
            let g = tree.lock().unwrap();
            let mut m = 1u32;
            for d in &deps {
                if let Some(r) = g.get(d) {
                    m = m.max(r.subtree_size.min(u32::MAX as u64) as u32);
                }
            }
            m
        };
        let f = Features {
            dep_count: deps.len().min(255) as u8,
            subtree_size: est_subtree,
            depth: depth.min(255) as u8,
            cmd_first_byte,
        };
        let feats = encode_features(f);
        let pred = {
            let m = model.lock().unwrap();
            m.tree.infer(feats)
        };
        (pred as f64, feats)
    };

    {
        let mut g = tree.lock().unwrap();
        g.insert(addr.clone(), NodeRecord {
            deps:  deps.clone(),
            pid:   None,
            state: initial.clone(),
            priority,
            subtree_size: 1,
            depth,
            cmd_first_byte,
            #[cfg(feature = "model")]
            meta: Some(SubmitMeta { features: features_u16 }),
        });
    }
    publish_state(workdir, &addr, &initial, subs);

    let workdir_t    = workdir.to_path_buf();
    let tree_t       = Arc::clone(tree);
    let subs_t       = Arc::clone(subs);
    let queue_t      = Arc::clone(ready_queue);
    let running_t    = Arc::clone(running);
    let addr_t       = addr.clone();
    #[cfg(feature = "model")]
    let model_t      = Arc::clone(model);
    thread::spawn(move || run_node(
        workdir_t, tree_t, subs_t, queue_t, running_t, addr_t,
        #[cfg(feature = "model")] model_t,
    ));

    Ok(addr)
}

/// Read the first non-whitespace byte of a file (cheap; opens then
/// closes after at most ~64 bytes). Returns `None` on I/O error or all
/// whitespace.
fn first_nonws_byte(p: &Path) -> Option<u8> {
    use std::io::Read;
    let mut buf = [0u8; 64];
    let mut f = fs::File::open(p).ok()?;
    let n = f.read(&mut buf).ok()?;
    for &b in &buf[..n] {
        if !b.is_ascii_whitespace() { return Some(b); }
    }
    None
}

fn run_node(
    workdir:     PathBuf,
    tree:        Tree,
    subs:        SubMap,
    ready_queue: ReadyQueue,
    running:     RunningCount,
    addr:        String,
    #[cfg(feature = "model")] model: ModelState,
) {
    let deps = {
        let g = tree.lock().unwrap();
        g.get(&addr).map(|r| r.deps.clone()).unwrap_or_default()
    };
    if !deps.is_empty() {
        loop {
            if SHUTDOWN.load(Ordering::SeqCst) { return; }
            let mut all_ok      = true;
            let mut any_failed  = false;
            for d in &deps {
                let st = read_state(&workdir, d);
                if !st.is_finished() { all_ok = false; }
                if st.is_finished() && !st.is_success() { any_failed = true; }
            }
            if any_failed {
                let s = NodeState::Failed("dep failed".into());
                update_record_state(&tree, &addr, s.clone());
                publish_state(&workdir, &addr, &s, &subs);
                #[cfg(feature = "model")]
                record_training_sample(&tree, &model, &addr);
                return;
            }
            if all_ok { break; }
            thread::sleep(Duration::from_millis(100));
        }
        let s = NodeState::Pending;
        update_record_state(&tree, &addr, s.clone());
        publish_state(&workdir, &addr, &s, &subs);
    }

    let (priority, subtree_size) = {
        let g = tree.lock().unwrap();
        g.get(&addr)
            .map(|r| (r.priority, r.subtree_size))
            .unwrap_or((0.0, 1))
    };
    {
        let mut q = ready_queue.lock().unwrap();
        q.push(QueueItem { subtree_size, priority, addr: addr.clone() });
    }
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) { return; }
        let can_run = {
            let mut run_g = running.lock().unwrap();
            if *run_g < MAX_WORKERS {
                let mut q = ready_queue.lock().unwrap();
                if let Some(top) = q.peek() {
                    if top.addr == addr {
                        q.pop();
                        *run_g += 1;
                        true
                    } else { false }
                } else { false }
            } else { false }
        };
        if can_run { break; }
        thread::sleep(Duration::from_millis(50));
    }

    let cmd_path = node_dir(&workdir, &addr).join("cmd.sh");
    let log_path = node_dir(&workdir, &addr).join("log");
    let log_file = match fs::File::create(&log_path) {
        Ok(f)  => f,
        Err(e) => {
            { let mut run_g = running.lock().unwrap(); *run_g -= 1; }
            let s = NodeState::Failed(format!("log create: {}", e));
            update_record_state(&tree, &addr, s.clone());
            publish_state(&workdir, &addr, &s, &subs);
            return;
        }
    };
    let log_clone = match log_file.try_clone() {
        Ok(f)  => f,
        Err(e) => {
            { let mut run_g = running.lock().unwrap(); *run_g -= 1; }
            let s = NodeState::Failed(format!("log clone: {}", e));
            update_record_state(&tree, &addr, s.clone());
            publish_state(&workdir, &addr, &s, &subs);
            return;
        }
    };

    let spawn = Command::new("bash")
        .arg(&cmd_path)
        .env("TREN_BIT_ADDR", &addr)
        .env("TREN_WORKDIR",  workdir.to_string_lossy().to_string())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_clone))
        .spawn();

    match spawn {
        Err(e) => {
            { let mut run_g = running.lock().unwrap(); *run_g -= 1; }
            let s = NodeState::Failed(format!("spawn: {}", e));
            update_record_state(&tree, &addr, s.clone());
            publish_state(&workdir, &addr, &s, &subs);
        }
        Ok(mut child) => {
            let pid = child.id();
            let _ = fs::write(node_dir(&workdir, &addr).join("pid"), pid.to_string());
            {
                let mut g = tree.lock().unwrap();
                if let Some(r) = g.get_mut(&addr) {
                    r.state = NodeState::Running;
                    r.pid = Some(pid);
                }
            }
            publish_state(&workdir, &addr, &NodeState::Running, &subs);

            let status = child.wait();

            { let mut run_g = running.lock().unwrap(); *run_g -= 1; }
            { let mut g = tree.lock().unwrap();
              if let Some(r) = g.get_mut(&addr) { r.pid = None; } }

            let s = match status {
                Ok(es) => match es.code() {
                    Some(c) => NodeState::Done(c),
                    None    => NodeState::Failed("signal".into()),
                },
                Err(e)  => NodeState::Failed(format!("wait: {}", e)),
            };
            if let NodeState::Done(c) = &s {
                let _ = fs::write(node_dir(&workdir, &addr).join("exit_code"), c.to_string());
            }
            update_record_state(&tree, &addr, s.clone());
            publish_state(&workdir, &addr, &s, &subs);
            #[cfg(feature = "model")]
            record_training_sample(&tree, &model, &addr);
        }
    }
}

/// Append `(features, label)` for `addr` to the model's training buffer
/// and trigger a retrain when `new_since_train` reaches the configured
/// threshold. Label = "node had ≥ 2 transitive descendants by the time
/// it finished" (the proxy for "on a critical path"). Bounded to
/// `model_buffer_capacity()` via VecDeque pop_front.
#[cfg(feature = "model")]
fn record_training_sample(tree: &Tree, model: &ModelState, addr: &str) {
    let (feats, label) = {
        let g = tree.lock().unwrap();
        let r = match g.get(addr) {
            Some(r) => r,
            None    => return,
        };
        let feats = match r.meta {
            Some(m) => m.features,
            None    => return,
        };
        let label = r.subtree_size >= 2;
        (feats, label)
    };

    let cap   = model_buffer_capacity();
    let every = model_retrain_every();

    let mut m = model.lock().unwrap();
    if m.buffer.len() == cap {
        m.buffer.pop_front();
    }
    m.buffer.push_back((feats, label));
    m.new_since_train += 1;
    if m.new_since_train >= every {
        let snap: Vec<(u16, bool)> = m.buffer.iter().copied().collect();
        m.generation = m.generation.wrapping_add(1);
        let gen_byte = m.generation;
        m.tree = Tree32::train(&snap, gen_byte);
        m.new_since_train = 0;
    }
}

fn update_record_state(tree: &Tree, addr: &str, s: NodeState) {
    let mut g = tree.lock().unwrap();
    if let Some(r) = g.get_mut(addr) { r.state = s; }
}

fn kill_node(
    workdir: &Path,
    tree:    &Tree,
    subs:    &SubMap,
    addr:    &str,
) -> Result<(), String> {
    let mut g = tree.lock().unwrap();
    let rec = g.get_mut(addr).ok_or_else(|| format!("node {} not found", addr))?;
    if rec.state.is_finished() {
        return Err(format!("node {} already finished", addr));
    }
    if let Some(pid) = rec.pid {
        unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    } else if let Ok(pid_str) = fs::read_to_string(node_dir(workdir, addr).join("pid")) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe { libc::kill(pid, libc::SIGTERM); }
        }
    }
    let s = NodeState::Failed("killed".into());
    rec.state = s.clone();
    drop(g);
    publish_state(workdir, addr, &s, subs);
    Ok(())
}

// ─── Background auto-GC of stale sibling workdirs ─────────────────────────

fn autogc_enabled() -> bool {
    match std::env::var("TREN_AUTOGC") {
        Ok(v) => {
            let t = v.trim().to_ascii_lowercase();
            !(t == "0" || t == "off" || t == "false" || t == "no")
        }
        Err(_) => true,
    }
}

fn autogc_interval() -> Duration {
    let secs = std::env::var("TREN_AUTOGC_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(120);
    Duration::from_secs(secs.max(1))
}

fn autogc_loop(workdir: PathBuf) {
    let parent = match workdir.parent() {
        Some(p) => p.to_path_buf(),
        None    => return,
    };
    let interval = autogc_interval();
    loop {
        let mut waited = Duration::from_millis(0);
        while waited < interval {
            if SHUTDOWN.load(Ordering::SeqCst) { return; }
            thread::sleep(Duration::from_millis(250));
            waited += Duration::from_millis(250);
        }
        if SHUTDOWN.load(Ordering::SeqCst) { return; }
        sweep_siblings(&parent, &workdir);
    }
}

fn sweep_siblings(parent: &Path, self_workdir: &Path) {
    let rd = match fs::read_dir(parent) {
        Ok(r)  => r,
        Err(_) => return,
    };
    for ent in rd.flatten() {
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if !s.starts_with(WORKDIR_PREFIX) { continue; }
        let p = ent.path();
        if p == self_workdir { continue; }
        let ft = match ent.file_type() { Ok(t) => t, Err(_) => continue };
        if ft.is_symlink() || !ft.is_dir() { continue; }
        if !(p.join("port").is_file() && p.join("pid").is_file()) { continue; }
        if tren::wrapper_alive(&p) { continue; }
        match fs::remove_dir_all(&p) {
            Ok(()) => {
                eprintln!(
                    "[tren-wrapper] auto-gc removed stale workdir: {}", p.display()
                );
                if let Err(e) = tren::record_autogc_removal(self_workdir, &p) {
                    eprintln!(
                        "[tren-wrapper] auto-gc failed to record removal of {}: {}",
                        p.display(), e
                    );
                }
            }
            Err(e) => eprintln!(
                "[tren-wrapper] auto-gc failed to remove {}: {}", p.display(), e
            ),
        }
    }
}

// ─── Reaper loop (housekeeping) ───────────────────────────────────────────

fn reaper_loop(tree: Tree, subs: SubMap, workdir: PathBuf) {
    loop {
        if SHUTDOWN.load(Ordering::SeqCst) { break; }
        thread::sleep(Duration::from_millis(500));
        let snap: Vec<(String, NodeState)> = {
            let g = tree.lock().unwrap();
            g.iter().map(|(k,v)| (k.clone(), v.state.clone())).collect()
        };
        for (addr, s) in snap {
            let on_disk = read_state(&workdir, &addr);
            if on_disk != s {
                publish_state(&workdir, &addr, &s, &subs);
            }
        }
    }
}

// suppress unused warning for OP_STATE in match (decode helper used elsewhere)
const _: u32 = tren::OP_STATE;

#[cfg(test)]
mod tests {
    use super::*;

    fn qi(subtree_size: u64, priority: f64, addr: &str) -> QueueItem {
        QueueItem { subtree_size, priority, addr: addr.into() }
    }

    #[test]
    fn queue_item_orders_by_subtree_size_desc_then_priority_then_addr() {
        // v0.4.0 default ordering: bigger subtree_size pops first; ties on
        // subtree_size break by bigger priority; final tie breaks by
        // lexicographically smaller addr.
        let mut h: std::collections::BinaryHeap<QueueItem> =
            std::collections::BinaryHeap::new();
        h.push(qi(1, 0.0, "10"));     // small chain, leaf
        h.push(qi(5, 0.0, "11"));     // long chain root
        h.push(qi(1, 0.0, "100"));    // small chain, deeper leaf
        h.push(qi(3, 0.0, "101"));    // medium chain root
        h.push(qi(5, 0.0, "111"));    // tied with first long chain root, larger addr

        let popped: Vec<String> = (0..5)
            .map(|_| h.pop().unwrap().addr)
            .collect();
        // Expect: subtree_size 5/addr "11", subtree_size 5/addr "111",
        //         subtree_size 3, then the two size-1 leaves by addr.
        assert_eq!(popped, vec!["11", "111", "101", "10", "100"]);
    }

    #[test]
    fn queue_item_priority_breaks_size_ties() {
        // When subtree_size is tied, higher `priority` (the model's f64)
        // pops first. Same-priority falls back to the addr tiebreaker.
        let mut h: std::collections::BinaryHeap<QueueItem> =
            std::collections::BinaryHeap::new();
        h.push(qi(2, 0.10, "1"));
        h.push(qi(2, 0.90, "100"));   // higher priority, should win despite bigger addr
        h.push(qi(2, 0.50, "10"));

        let popped: Vec<String> = (0..3)
            .map(|_| h.pop().unwrap().addr)
            .collect();
        assert_eq!(popped, vec!["100", "10", "1"]);
    }

    #[test]
    fn bump_ancestor_subtree_walks_transitive_dag() {
        // Build a small DAG in memory and verify bump_ancestor_subtree
        // increments every transitive ancestor exactly once per call,
        // including diamond shapes where two paths share an ancestor.
        let tree: Tree = Arc::new(Mutex::new(HashMap::new()));
        let mk = |addr: &str, deps: Vec<&str>| -> NodeRecord {
            NodeRecord {
                deps:  deps.iter().map(|s| (*s).into()).collect(),
                pid:   None,
                state: NodeState::Pending,
                priority:     0.0,
                subtree_size: 1,
                depth:        0,
                cmd_first_byte: 0,
                #[cfg(feature = "model")]
                meta: None,
            }
        };
        {
            let mut g = tree.lock().unwrap();
            g.insert("1".into(),    mk("1",    vec![]));
            g.insert("10".into(),   mk("10",   vec!["1"]));
            g.insert("11".into(),   mk("11",   vec!["1"]));
            g.insert("110".into(),  mk("110",  vec!["10", "11"])); // diamond
            g.insert("111".into(),  mk("111",  vec!["110"]));
        }

        // Each SUBMIT bumps its dep ancestors once. Simulate the whole
        // submission sequence in order.
        bump_ancestor_subtree(&tree, &[]);                         // node "1"
        bump_ancestor_subtree(&tree, &["1".into()]);               // node "10"
        bump_ancestor_subtree(&tree, &["1".into()]);               // node "11"
        bump_ancestor_subtree(&tree, &["10".into(), "11".into()]); // node "110"
        bump_ancestor_subtree(&tree, &["110".into()]);             // node "111"

        let g = tree.lock().unwrap();
        // "1" sits above everyone: bumped by 10, 11, 110 (via both paths
        // but counted once thanks to the visited set), and 111.
        assert_eq!(g.get("1").unwrap().subtree_size,   5);
        assert_eq!(g.get("10").unwrap().subtree_size,  3); // bumped by 110, 111
        assert_eq!(g.get("11").unwrap().subtree_size,  3); // bumped by 110, 111
        assert_eq!(g.get("110").unwrap().subtree_size, 2); // bumped by 111
        assert_eq!(g.get("111").unwrap().subtree_size, 1); // never an ancestor
    }
}
