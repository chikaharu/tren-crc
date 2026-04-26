//! tren-wrapper — PWD-local job scheduler wrapper process.
  //!
  //! Behaviour
  //! =========
  //! * Self-locates inside the current working directory. Creates
  //!   `.tren-<uuid>/` if no live wrapper directory is found.
  //! * Binds a free localhost UDP port; writes it to `<workdir>/port`.
  //! * Records its own PID in `<workdir>/pid`.
  //! * Listens for line-oriented UDP commands:
  //!     SUBMIT\n<deps>\n<cmd>\n      → reply `OK <bit_addr>` / `ERR ...`
  //!     KILL <bit_addr>              → reply `OK killed` / `ERR ...`
  //!     SUB <bit_addr_or_*> <port>   → reply `OK subscribed`
  //!     UNSUB <port>                 → reply `OK unsubscribed`
  //!     QUIT                         → reply `OK` then shut down
  //! * Spawns one Rust thread per allocated node. Each thread executes its
  //!   command (after dependencies finish), writes per-node files, then
  //!   pushes a state-change event to all matching subscribers via UDP.
  //!
  //! Recursive sparse binary tree
  //! ---------------------------
  //! Allocation is BFS by sequence: the n-th SUBMIT becomes node id `n`, so
  //! the tree fills root → row 1 → row 2 → … in a sparse-but-contiguous
  //! pattern. Each node is reachable from any other by simple bit ops on the
  //! id (parent = id>>1, children = id<<1 | {0,1}, sibling = id^1).

  use std::collections::{BinaryHeap, HashMap};
  use std::cmp::Ordering as CmpOrdering;
  use std::fs;
  use std::net::{SocketAddr, UdpSocket};
  use std::path::{Path, PathBuf};
  use std::process::{Child, Command, Stdio};
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::{Arc, Mutex};
  use std::thread;
  use std::time::Duration;

  use tren::{
      bind_free_udp, decode_text, fresh_token, id_to_bit_addr, node_dir,
      read_state, write_string, NodeState, WORKDIR_PREFIX,
  };

  /// Maximum number of concurrently running nodes (resource constraint).
  /// Override at compile time or adjust here as needed.
  const MAX_WORKERS: usize = 4;

  /// Global subscribers: addr_filter ("*" or bit_addr) → set of subscriber sockets.
  type SubMap = Arc<Mutex<HashMap<SocketAddr, String>>>;

  /// Priority-ordered ready queue for resource-constrained scheduling.
  type ReadyQueue = Arc<Mutex<BinaryHeap<QueueItem>>>;

  /// Count of currently running worker threads.
  type RunningCount = Arc<Mutex<usize>>;

  /// In-memory record of an allocated node.
  #[derive(Debug)]
  struct NodeRecord {
      cmd:        String,
      deps:       Vec<String>,
      /// Subprocess once it is running.
      child:      Option<Child>,
      /// Last published state (mirrors the on-disk `state` file).
      state:      NodeState,
      /// Scheduling priority: higher value = higher priority.
      /// Set by the external model (feature = "model") or defaults to 0.0.
      priority:   f64,
  }

  /// Item in the priority-ordered ready queue.
  #[derive(Clone)]
  struct QueueItem {
      priority: f64,
      addr:     String,
  }

  impl Eq for QueueItem {}

  impl PartialEq for QueueItem {
      fn eq(&self, other: &Self) -> bool {
          self.priority == other.priority
      }
  }

  impl Ord for QueueItem {
      fn cmp(&self, other: &Self) -> CmpOrdering {
          self.priority
              .partial_cmp(&other.priority)
              .unwrap_or(CmpOrdering::Equal)
      }
  }

  impl PartialOrd for QueueItem {
      fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
          Some(self.cmp(other))
      }
  }

  type Tree = Arc<Mutex<HashMap<String, NodeRecord>>>;

  static SHUTDOWN: AtomicBool = AtomicBool::new(false);

  extern "C" fn handle_sig(_: libc::c_int) {
      SHUTDOWN.store(true, Ordering::SeqCst);
  }

  /// Call the external `tren-model` binary to obtain a scheduling priority.
  /// Returns `None` if the binary is unavailable or returns non-numeric output.
  /// Only compiled when the `model` feature is enabled.
  #[cfg(feature = "model")]
  fn call_model(cmd: &str, deps: &[String], addr: &str) -> Option<f64> {
      use std::process::Command as Cmd;
      let payload = format!(
          r#"{{"cmd":"{}","deps":{:?},"addr":"{}"}}"#,
          cmd.replace('"', "'"),
          deps,
          addr
      );
      let out = Cmd::new("tren-model")
          .arg(payload)
          .output()
          .ok()?;
      if !out.status.success() {
          return None;
      }
      let s = String::from_utf8_lossy(&out.stdout);
      s.trim().parse::<f64>().ok()
  }

  /// RAII guard that ensures the wrapper's own workdir is removed on every
  /// shutdown path that lets `main` unwind — normal QUIT, SIGINT, SIGTERM,
  /// SIGHUP, panics — even if the explicit cleanup earlier in the shutdown
  /// sequence raced with a worker thread that recreated some files. The
  /// drop is best-effort and idempotent, so combining it with an explicit
  /// `fs::remove_dir_all` call is safe.
  ///
  /// Note: `Drop` cannot help against `SIGKILL` or other process-level
  /// forced kills — those still rely on the sibling-wrapper auto-GC sweep
  /// to remove the stale directory later.
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

  /// Idempotent shutdown cleanup. Kills any in-flight children, then gives
  /// worker threads a brief window to observe `SHUTDOWN` and stop writing
  /// into the workdir, then removes the directory with a few retries to
  /// defeat any late writes that slipped through.
  fn cleanup_workdir(workdir: &Path, tree: &Tree) {
      {
          let mut tree_g = tree.lock().unwrap();
          for (_addr, rec) in tree_g.iter_mut() {
              if let Some(child) = rec.child.as_mut() { let _ = child.kill(); }
          }
      }
      // Worker threads poll SHUTDOWN in their dep-wait loop and `publish_state`
      // skips on-disk writes once SHUTDOWN is set, so a short pause here is
      // enough for in-flight writes to drain.
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

  fn main() {
      install_signal_handlers();

      let cwd = std::env::current_dir().expect("cwd");

      // If a live workdir already exists in this exact directory, stop —
      // duplicate wrapper. (We do not walk up from cwd here: the wrapper
      // is responsible for *cwd specifically*; ascending discovery is a
      // client-side concern.)
      if let Some(existing) = find_workdir_local(&cwd) {
          if tren::wrapper_alive(&existing) {
              eprintln!("[tren-wrapper] active workdir already present: {}", existing.display());
              std::process::exit(0);
          }
          let _ = fs::remove_dir_all(&existing);
      }

      let token   = fresh_token();
      let workdir = cwd.join(format!("{}{}", WORKDIR_PREFIX, token));
      fs::create_dir_all(workdir.join("tree")).expect("create tree dir");

      // Final-pass cleanup guard: ensures the workdir is gone whenever
      // `main` returns, even if the explicit cleanup below loses a race
      // with a worker thread. Held for the rest of `main`.
      let _workdir_guard = WorkdirGuard { path: workdir.clone() };

      // Bind UDP first so we can record the port.
      let socket = bind_free_udp().expect("bind udp");
      let port   = socket.local_addr().expect("local_addr").port();

      write_string(&workdir.join("port"), &port.to_string()).expect("write port");
      write_string(&workdir.join("pid"),  &std::process::id().to_string()).expect("write pid");
      write_string(&workdir.join("seq"),  "0").expect("write seq");

      eprintln!(
          "[tren-wrapper] up  cwd={}  workdir={}  port={}  pid={}",
          cwd.display(), workdir.display(), port, std::process::id()
      );

      let tree:        Tree         = Arc::new(Mutex::new(HashMap::new()));
      let subs:        SubMap       = Arc::new(Mutex::new(HashMap::new()));
      let ready_queue: ReadyQueue   = Arc::new(Mutex::new(BinaryHeap::new()));
      let running:     RunningCount = Arc::new(Mutex::new(0usize));

      // Watcher: periodically reaps finished children and republishes state.
      {
          let tree = Arc::clone(&tree);
          let subs = Arc::clone(&subs);
          let workdir = workdir.clone();
          thread::spawn(move || reaper_loop(tree, subs, workdir));
      }

      // Background sweeper: opportunistically remove sibling `.tren-*`
      // workdirs whose wrapper PID is dead. Opt-out via `TREN_AUTOGC=0`.
      if autogc_enabled() {
          let workdir = workdir.clone();
          thread::spawn(move || autogc_loop(workdir));
      }

      // Read loop. UDP datagrams are bounded by typical 64 KiB.
      socket.set_read_timeout(Some(Duration::from_millis(250))).ok();
      let mut buf = vec![0u8; 64 * 1024];
      loop {
          if SHUTDOWN.load(Ordering::SeqCst) { break; }

          match socket.recv_from(&mut buf) {
              Ok((n, src)) => {
                  let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
                  let reply = handle_request(
                      &msg, &workdir, &tree, &subs,
                      &ready_queue, &running, src,
                  );
                  if let Some(r) = reply {
                      let _ = socket.send_to(r.as_bytes(), src);
                  }
              }
              Err(ref e)
                  if e.kind() == std::io::ErrorKind::WouldBlock
                  || e.kind() == std::io::ErrorKind::TimedOut => {}
              Err(e) => eprintln!("[tren-wrapper] recv error: {}", e),
          }
      }

      // Shutdown: kill running children, drain in-flight worker writes,
      // remove the workdir. `_workdir_guard` runs at function exit and
      // does one more best-effort pass in case anything slipped through.
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

  /// Look only inside `dir` itself (no walk-up) for a workdir entry.
  fn find_workdir_local(dir: &Path) -> Option<PathBuf> {
      if let Ok(rd) = fs::read_dir(dir) {
          for ent in rd.flatten() {
              let n = ent.file_name();
              let s = n.to_string_lossy();
              if s.starts_with(WORKDIR_PREFIX) {
                  let p = ent.path();
                  if p.is_dir() && p.join("port").is_file() {
                      return Some(p);
                  }
              }
          }
      }
      None
  }

  // ─── Request dispatch ──────────────────────────────────────────────────────

  fn handle_request(
      msg:         &str,
      workdir:     &Path,
      tree:        &Tree,
      subs:        &SubMap,
      ready_queue: &ReadyQueue,
      running:     &RunningCount,
      src:         SocketAddr,
  ) -> Option<String> {
      let mut lines = msg.split('\n');
      let head = lines.next().unwrap_or("").trim();

      if head == "SUBMIT" {
          let deps_line = lines.next().unwrap_or("").trim();
          let cmd = decode_text(lines.next().unwrap_or("").trim());
          if cmd.is_empty() {
              return Some("ERR empty cmd".into());
          }
          let deps: Vec<String> = if deps_line.is_empty() {
              Vec::new()
          } else {
              deps_line.split_whitespace().map(|s| s.to_string()).collect()
          };
          match allocate_node(workdir, tree, subs, ready_queue, running, cmd, deps) {
              Ok(addr) => Some(format!("OK {}", addr)),
              Err(e)   => Some(format!("ERR {}", e)),
          }
      } else if let Some(rest) = head.strip_prefix("KILL ") {
          let addr = rest.trim().to_string();
          match kill_node(workdir, tree, subs, &addr) {
              Ok(())   => Some("OK killed".into()),
              Err(e)   => Some(format!("ERR {}", e)),
          }
      } else if let Some(rest) = head.strip_prefix("SUB ") {
          // SUB <filter> <port>
          let mut it = rest.split_whitespace();
          let filter = it.next().unwrap_or("*").to_string();
          let port   = it.next().and_then(|s| s.parse::<u16>().ok());
          let dest = match port {
              Some(p) => SocketAddr::from(([127,0,0,1], p)),
              None    => src,
          };
          subs.lock().unwrap().insert(dest, filter);
          Some("OK subscribed".into())
      } else if let Some(rest) = head.strip_prefix("UNSUB ") {
          let port = rest.trim().parse::<u16>().ok();
          let dest = match port {
              Some(p) => SocketAddr::from(([127,0,0,1], p)),
              None    => src,
          };
          subs.lock().unwrap().remove(&dest);
          Some("OK unsubscribed".into())
      } else if head == "QUIT" {
          SHUTDOWN.store(true, Ordering::SeqCst);
          Some("OK".into())
      } else if head == "PING" {
          Some("OK pong".into())
      } else if head.is_empty() {
          None
      } else {
          Some(format!("ERR unknown: {}", head))
      }
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
      // Once shutdown has begun we must not touch the workdir on disk —
      // doing so would race with `cleanup_workdir` and re-create the
      // directory tree we just removed. UDP notifications are still safe.
      if !SHUTDOWN.load(Ordering::SeqCst) {
          let _ = write_string(&node_dir(workdir, addr).join("state"), &state.label());
      }
      let payload = format!("STATE {} {}\n", addr, state.label());
      // Snapshot subscribers — drop dead ones lazily.
      let snap: Vec<(SocketAddr, String)> = subs.lock().unwrap()
          .iter().map(|(a,f)| (*a, f.clone())).collect();
      if snap.is_empty() { return; }
      if let Ok(sock) = UdpSocket::bind("127.0.0.1:0") {
          for (dest, filter) in snap {
              if filter == "*" || filter == addr {
                  let _ = sock.send_to(payload.as_bytes(), dest);
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
      cmd:         String,
      deps:        Vec<String>,
  ) -> Result<String, String> {
      let id   = next_seq(workdir);
      let addr = id_to_bit_addr(id);
      let dir  = node_dir(workdir, &addr);
      fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {}", e))?;
      let _ = fs::write(dir.join("cmd"), &cmd);
      if !deps.is_empty() {
          let _ = fs::write(dir.join("deps"), deps.join(" "));
      }

      let initial = if deps.is_empty() { NodeState::Pending } else { NodeState::Waiting };

      // Default priority; optionally overridden by the external model.
      let mut priority = 0.0f64;
      #[cfg(feature = "model")]
      {
          if let Some(p) = call_model(&cmd, &deps, &addr) {
              priority = p;
          }
      }

      {
          let mut g = tree.lock().unwrap();
          g.insert(addr.clone(), NodeRecord {
              cmd:   cmd.clone(),
              deps:  deps.clone(),
              child: None,
              state: initial.clone(),
              priority,
          });
      }
      publish_state(workdir, &addr, &initial, subs);

      // Spawn the worker thread immediately. It will block until deps finish,
      // then wait in the priority queue until a worker slot is free.
      let workdir_t    = workdir.to_path_buf();
      let tree_t       = Arc::clone(tree);
      let subs_t       = Arc::clone(subs);
      let queue_t      = Arc::clone(ready_queue);
      let running_t    = Arc::clone(running);
      let addr_t       = addr.clone();
      thread::spawn(move || run_node(workdir_t, tree_t, subs_t, queue_t, running_t, addr_t));

      Ok(addr)
  }

  fn run_node(
      workdir:     PathBuf,
      tree:        Tree,
      subs:        SubMap,
      ready_queue: ReadyQueue,
      running:     RunningCount,
      addr:        String,
  ) {
      // ── 1. Wait for deps ───────────────────────────────────────────────────
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
                  return;
              }
              if all_ok { break; }
              thread::sleep(Duration::from_millis(100));
          }
          let s = NodeState::Pending;
          update_record_state(&tree, &addr, s.clone());
          publish_state(&workdir, &addr, &s, &subs);
      }

      // ── 2. Enqueue in priority queue and wait for a worker slot ───────────
      let priority = {
          let g = tree.lock().unwrap();
          g.get(&addr).map(|r| r.priority).unwrap_or(0.0)
      };
      {
          let mut q = ready_queue.lock().unwrap();
          q.push(QueueItem { priority, addr: addr.clone() });
      }
      loop {
          if SHUTDOWN.load(Ordering::SeqCst) { return; }
          let can_run = {
              let mut run_g = running.lock().unwrap();
              if *run_g < MAX_WORKERS {
                  let mut q = ready_queue.lock().unwrap();
                  // Only run if we are at the front of the queue (highest priority).
                  if let Some(top) = q.peek() {
                      if top.addr == addr {
                          q.pop();
                          *run_g += 1;
                          true
                      } else {
                          false
                      }
                  } else {
                      false
                  }
              } else {
                  false
              }
          };
          if can_run { break; }
          thread::sleep(Duration::from_millis(50));
      }

      // ── 3. Execute ────────────────────────────────────────────────────────
      let cmd = {
          let g = tree.lock().unwrap();
          g.get(&addr).map(|r| r.cmd.clone()).unwrap_or_default()
      };
      let log_path = node_dir(&workdir, &addr).join("log");
      let log_file = match fs::File::create(&log_path) {
          Ok(f)  => f,
          Err(e) => {
              // Release worker slot before returning on error.
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

      let spawn = Command::new("sh")
          .args(["-c", &cmd])
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
                  }
              }
              publish_state(&workdir, &addr, &NodeState::Running, &subs);

              let status = child.wait();

              // ── 4. Release worker slot ─────────────────────────────────────
              { let mut run_g = running.lock().unwrap(); *run_g -= 1; }

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
              {
                  let mut g = tree.lock().unwrap();
                  if let Some(r) = g.get_mut(&addr) { r.child = None; }
              }
              publish_state(&workdir, &addr, &s, &subs);
          }
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
      if let Some(child) = rec.child.as_mut() {
          let _ = child.kill();
      } else {
          // Try via pid file (subprocess might have outlived our handle).
          if let Ok(pid_str) = fs::read_to_string(node_dir(workdir, addr).join("pid")) {
              if let Ok(pid) = pid_str.trim().parse::<i32>() {
                  unsafe { libc::kill(pid, libc::SIGTERM); }
              }
          }
      }
      let s = NodeState::Failed("killed".into());
      rec.state = s.clone();
      drop(g);
      publish_state(workdir, addr, &s, subs);
      Ok(())
  }

  // ─── Background auto-GC of stale sibling workdirs ─────────────────────────

  /// Whether the in-wrapper auto-GC sweeper is enabled. Defaults to on.
  /// Set `TREN_AUTOGC=0` (or `off`/`false`/`no`) to disable for debugging.
  fn autogc_enabled() -> bool {
      match std::env::var("TREN_AUTOGC") {
          Ok(v) => {
              let t = v.trim().to_ascii_lowercase();
              !(t == "0" || t == "off" || t == "false" || t == "no")
          }
          Err(_) => true,
      }
  }

  /// Sweep interval in seconds. Defaults to 120s. Override with
  /// `TREN_AUTOGC_INTERVAL_SECS=<n>`. Values < 1 are clamped to 1.
  fn autogc_interval() -> Duration {
      let secs = std::env::var("TREN_AUTOGC_INTERVAL_SECS")
          .ok()
          .and_then(|s| s.trim().parse::<u64>().ok())
          .unwrap_or(120);
      Duration::from_secs(secs.max(1))
  }

  /// Periodically scan the parent of `workdir` for sibling `.tren-*` entries
  /// whose wrapper process is dead and remove them. Exits cleanly on
  /// `SHUTDOWN`. Sleeps in short slices so shutdown latency stays bounded.
  fn autogc_loop(workdir: PathBuf) {
      let parent = match workdir.parent() {
          Some(p) => p.to_path_buf(),
          None    => return,
      };
      let interval = autogc_interval();
      loop {
          // Sleep in 250ms slices so SIGTERM doesn't have to wait `interval`.
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

  /// One pass: remove every sibling `.tren-*` directory in `parent` whose
  /// recorded wrapper PID is no longer alive. The wrapper's own workdir is
  /// always skipped, as are entries that don't look like real wrapper
  /// workdirs (`port` + `pid` files present).
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
          // Refresh state files for any nodes whose record state diverges
          // from disk (cheap consistency check).
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
  