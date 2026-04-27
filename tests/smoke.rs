//! Smoke integration tests for `tren-wrapper` + `qsub` + `qwait`.
  //!
  //! Original 5 smoke cases, plus 100 primary cases (P001-P100) and
  //! 100 edge cases (E001-E100) for the v0.2.0 priority-queue / worker-cap
  //! changes. Tests are macro-generated so each one becomes a real
  //! `#[test]` function — `cargo test` parallelises them automatically.

  use std::path::{Path, PathBuf};
  use std::process::{Child, Command, Stdio};
  use std::thread;
  use std::time::{Duration, Instant};

  const QSUB:    &str = env!("CARGO_BIN_EXE_qsub");
  const QWAIT:   &str = env!("CARGO_BIN_EXE_qwait");
  const WRAPPER: &str = env!("CARGO_BIN_EXE_tren-wrapper");

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
          if let Some(workdir) = Self::find_workdir(&self.dir) {
              if let Ok(port_str) = std::fs::read_to_string(workdir.join("port")) {
                  if let Ok(port) = port_str.trim().parse::<u16>() {
                      let _ = tren::send_quit(port);
                  }
              }
          }
          let _ = self.wrapper.kill();
          let _ = self.wrapper.wait();
          let _ = std::fs::remove_dir_all(&self.dir);
      }
  }

  // ─── Original smoke cases (kept verbatim) ──────────────────────────────────

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

  #[test]
  fn smoke_dependency_runs_after_parent() {
      let sb = Sandbox::new("dep");

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

  enum ShutdownKind {
      Signal(libc::c_int),
      UdpQuit,
  }

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

          for _ in 0..3 {
              let _ = Command::new("sh")
                  .arg("-c")
                  .arg(format!("{} sleep 0.2 &", QSUB))
                  .current_dir(&sb.dir)
                  .stdout(Stdio::null())
                  .stderr(Stdio::null())
                  .status();
          }
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
                  let _ = tren::send_quit(port);
              }
          }

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

          assert!(
              !workdir.exists(),
              "{label}: workdir {} should have been removed by wrapper shutdown",
              workdir.display(),
          );
          assert!(
              Sandbox::find_workdir(&sb.dir).is_none(),
              "{label}: no .tren-* should remain under sandbox after shutdown",
          );

          drop(sb);
      }
  }

  #[test]
  fn smoke_concurrent_qsub_creates_single_workdir() {
      let stamp = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .unwrap()
          .as_nanos();
      let dir = std::env::temp_dir().join(format!(
          "tren-race-{}-{}", std::process::id(), stamp,
      ));
      std::fs::create_dir_all(&dir).expect("mkdir race sandbox");

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

      if let Some(wd) = workdirs.first() {
          if let Ok(port_str) = std::fs::read_to_string(wd.join("port")) {
              if let Ok(port) = port_str.trim().parse::<u16>() {
                  let _ = tren::send_quit(port);
              }
          }
      }
      thread::sleep(Duration::from_millis(200));
      let _ = std::fs::remove_dir_all(&dir);
  }

  // ═══════════════════════════════════════════════════════════════════════════
  // Test macros — each macro invocation expands to one real `#[test]` fn so
  // `cargo test` parallelises every case independently.
  // ═══════════════════════════════════════════════════════════════════════════

  /// Primary: trivial successful command. Just expects DONE(0).
  macro_rules! p_ok {
      ($name:ident, $cmd:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let addr = sb.qsub(&[$cmd]);
              assert!(!addr.is_empty());
              let state = sb.read_node_state(&addr);
              assert_eq!(state, "DONE(0)", "expected DONE(0) for {:?}, got {:?}", $cmd, state);
          }
      };
  }

  /// Primary: explicit exit code N — qsub itself exits 0 only when the
  /// node finished DONE(0); for any nonzero code it exits 1. The actual
  /// exit code is preserved in the node state file as DONE(N).
  macro_rules! p_exit {
      ($name:ident, $code:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let cmd = format!("exit {}", $code);
              let (rc, stdout, stderr) = sb.qsub_raw(&[&cmd]);
              let addr = stdout.trim();
              assert!(!addr.is_empty(), "no bit_addr returned");
              let expected_rc: i32 = if $code == 0 { 0 } else { 1 };
              assert_eq!(rc, expected_rc,
                  "qsub rc for exit {} should be {} (stderr: {})",
                  $code, expected_rc, stderr);
              let state = sb.read_node_state(addr);
              assert_eq!(state, format!("DONE({})", $code));
          }
      };
  }

  /// Primary: dependency chain where child runs only after parent completes.
  macro_rules! p_dep_chain {
      ($name:ident, $marker:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let parent = sb.qsub(&[&format!("echo P{} >> chain.log", $marker)]);
              let child  = sb.qsub(&["--after", &parent, "--",
                  &format!("echo C{} >> chain.log", $marker)]);
              assert_eq!(sb.qwait(&[&parent, &child]), 0);
              let log = std::fs::read_to_string(sb.dir.join("chain.log")).unwrap();
              let lines: Vec<&str> = log.lines().collect();
              assert_eq!(lines, vec![format!("P{}", $marker), format!("C{}", $marker)]);
          }
      };
  }

  /// Primary: small batch of independent jobs all succeed.
  macro_rules! p_batch_ok {
      ($name:ident, $n:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let mut addrs = Vec::new();
              for _ in 0..$n {
                  addrs.push(sb.qsub(&["true"]));
              }
              let refs: Vec<&str> = addrs.iter().map(|s| s.as_str()).collect();
              assert_eq!(sb.qwait(&refs), 0);
              for a in &addrs {
                  assert_eq!(sb.read_node_state(a), "DONE(0)");
              }
          }
      };
  }

  /// Edge: failing command must propagate non-zero exit. qsub itself
  /// returns 1 for any nonzero job exit code; the exact code is recorded
  /// in the node state file as DONE(N).
  macro_rules! e_fail {
      ($name:ident, $code:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let cmd = format!("exit {}", $code);
              let (rc, stdout, stderr) = sb.qsub_raw(&[&cmd]);
              let addr = stdout.trim();
              assert!(!addr.is_empty());
              assert_eq!(rc, 1,
                  "qsub rc for nonzero exit {} should be 1 (stderr: {})",
                  $code, stderr);
              let state = sb.read_node_state(addr);
              assert_eq!(state, format!("DONE({})", $code));
          }
      };
  }

  /// Edge: child of a failing parent must end in FAILED(dep failed).
  macro_rules! e_dep_fail {
      ($name:ident, $tag:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let (_rc, parent_out, _) = sb.qsub_raw(&["false"]);
              let parent = parent_out.trim().to_string();
              assert!(!parent.is_empty());
              let (_rc2, child_out, _) = sb.qsub_raw(&[
                  "--after", &parent, "--", &format!("echo {}", $tag)
              ]);
              let child = child_out.trim().to_string();
              assert!(!child.is_empty());
              // Wait briefly for dep-fail propagation.
              let deadline = Instant::now() + Duration::from_secs(5);
              loop {
                  let s = sb.read_node_state(&child);
                  if s.starts_with("FAILED") { break; }
                  if Instant::now() > deadline {
                      panic!("child {} did not transition to FAILED, last={:?}", child, s);
                  }
                  thread::sleep(Duration::from_millis(50));
              }
          }
      };
  }

  /// Edge: command containing a special shell character. We don't care
  /// about its exit code — only that the node finishes (DONE(*) or FAILED).
  macro_rules! e_special {
      ($name:ident, $cmd:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let (_rc, stdout, _stderr) = sb.qsub_raw(&[$cmd]);
              let addr = stdout.trim();
              assert!(!addr.is_empty(), "no bit_addr for cmd {:?}", $cmd);
              let deadline = Instant::now() + Duration::from_secs(5);
              loop {
                  let s = sb.read_node_state(addr);
                  if s.starts_with("DONE(") || s.starts_with("FAILED") { break; }
                  if Instant::now() > deadline {
                      panic!("node {} stuck in state {:?} for cmd {:?}", addr, s, $cmd);
                  }
                  thread::sleep(Duration::from_millis(50));
              }
          }
      };
  }

  /// Edge: many concurrent SUBMITs must all finish DONE(0) (worker cap
  /// keeps in-flight count bounded; queue drains all of them).
  macro_rules! e_concurrent {
      ($name:ident, $n:expr) => {
          #[test]
          fn $name() {
              let sb = Sandbox::new(stringify!($name));
              let mut addrs = Vec::new();
              for i in 0..$n {
                  addrs.push(sb.qsub(&[&format!("echo c{}", i)]));
              }
              let refs: Vec<&str> = addrs.iter().map(|s| s.as_str()).collect();
              assert_eq!(sb.qwait(&refs), 0);
              for a in &addrs {
                  assert_eq!(sb.read_node_state(a), "DONE(0)",
                      "node {} should be DONE(0)", a);
              }
          }
      };
  }
  
// ─── Primary cases (P001–P100) ──────────────────────────────────────────

p_ok!(p001_ok, "true");
p_ok!(p002_ok, "echo hello");
p_ok!(p003_ok, "echo world");
p_ok!(p004_ok, "echo a");
p_ok!(p005_ok, "echo 1");
p_ok!(p006_ok, "pwd");
p_ok!(p007_ok, "ls");
p_ok!(p008_ok, "echo hi");
p_ok!(p009_ok, "echo p");
p_ok!(p010_ok, "echo q");
p_ok!(p011_ok, "true && true");
p_ok!(p012_ok, ": ");
p_ok!(p013_ok, "echo ok");
p_ok!(p014_ok, "true && echo done");
p_ok!(p015_ok, "echo x > /dev/null");
p_ok!(p016_ok, "echo y");
p_ok!(p017_ok, "echo z");
p_ok!(p018_ok, "true && pwd");
p_ok!(p019_ok, "echo foo");
p_ok!(p020_ok, "echo bar");
p_exit!(p021_exit_0_v0, 0);
p_exit!(p022_exit_1_v0, 1);
p_exit!(p023_exit_2_v0, 2);
p_exit!(p024_exit_3_v0, 3);
p_exit!(p025_exit_4_v0, 4);
p_exit!(p026_exit_5_v0, 5);
p_exit!(p027_exit_6_v0, 6);
p_exit!(p028_exit_7_v0, 7);
p_exit!(p029_exit_8_v0, 8);
p_exit!(p030_exit_9_v0, 9);
p_exit!(p031_exit_0_v1, 0);
p_exit!(p032_exit_1_v1, 1);
p_exit!(p033_exit_2_v1, 2);
p_exit!(p034_exit_3_v1, 3);
p_exit!(p035_exit_4_v1, 4);
p_exit!(p036_exit_5_v1, 5);
p_exit!(p037_exit_6_v1, 6);
p_exit!(p038_exit_7_v1, 7);
p_exit!(p039_exit_8_v1, 8);
p_exit!(p040_exit_9_v1, 9);
p_dep_chain!(p041_dep_chain, 0);
p_dep_chain!(p042_dep_chain, 1);
p_dep_chain!(p043_dep_chain, 2);
p_dep_chain!(p044_dep_chain, 3);
p_dep_chain!(p045_dep_chain, 4);
p_dep_chain!(p046_dep_chain, 5);
p_dep_chain!(p047_dep_chain, 6);
p_dep_chain!(p048_dep_chain, 7);
p_dep_chain!(p049_dep_chain, 8);
p_dep_chain!(p050_dep_chain, 9);
p_dep_chain!(p051_dep_chain, 10);
p_dep_chain!(p052_dep_chain, 11);
p_dep_chain!(p053_dep_chain, 12);
p_dep_chain!(p054_dep_chain, 13);
p_dep_chain!(p055_dep_chain, 14);
p_dep_chain!(p056_dep_chain, 15);
p_dep_chain!(p057_dep_chain, 16);
p_dep_chain!(p058_dep_chain, 17);
p_dep_chain!(p059_dep_chain, 18);
p_dep_chain!(p060_dep_chain, 19);
p_batch_ok!(p061_batch_1, 1);
p_batch_ok!(p062_batch_2, 2);
p_batch_ok!(p063_batch_3, 3);
p_batch_ok!(p064_batch_4, 4);
p_batch_ok!(p065_batch_5, 5);
p_batch_ok!(p066_batch_6, 6);
p_batch_ok!(p067_batch_7, 7);
p_batch_ok!(p068_batch_8, 8);
p_batch_ok!(p069_batch_9, 9);
p_batch_ok!(p070_batch_10, 10);
p_batch_ok!(p071_batch_11, 11);
p_batch_ok!(p072_batch_12, 12);
p_batch_ok!(p073_batch_13, 13);
p_batch_ok!(p074_batch_14, 14);
p_batch_ok!(p075_batch_15, 15);
p_batch_ok!(p076_batch_16, 16);
p_batch_ok!(p077_batch_17, 17);
p_batch_ok!(p078_batch_18, 18);
p_batch_ok!(p079_batch_19, 19);
p_batch_ok!(p080_batch_20, 20);
p_ok!(p081_ok_extra, "echo job-0 && true");
p_ok!(p082_ok_extra, "echo job-1 && true");
p_ok!(p083_ok_extra, "echo job-2 && true");
p_ok!(p084_ok_extra, "echo job-3 && true");
p_ok!(p085_ok_extra, "echo job-4 && true");
p_ok!(p086_ok_extra, "echo job-5 && true");
p_ok!(p087_ok_extra, "echo job-6 && true");
p_ok!(p088_ok_extra, "echo job-7 && true");
p_ok!(p089_ok_extra, "echo job-8 && true");
p_ok!(p090_ok_extra, "echo job-9 && true");
p_ok!(p091_ok_extra, "echo job-10 && true");
p_ok!(p092_ok_extra, "echo job-11 && true");
p_ok!(p093_ok_extra, "echo job-12 && true");
p_ok!(p094_ok_extra, "echo job-13 && true");
p_ok!(p095_ok_extra, "echo job-14 && true");
p_ok!(p096_ok_extra, "echo job-15 && true");
p_ok!(p097_ok_extra, "echo job-16 && true");
p_ok!(p098_ok_extra, "echo job-17 && true");
p_ok!(p099_ok_extra, "echo job-18 && true");
p_ok!(p100_ok_extra, "echo job-19 && true");

// ─── Edge cases (E001–E100) ──────────────────────────────────────────────

e_fail!(e001_fail_1_v0, 1);
e_fail!(e002_fail_2_v0, 2);
e_fail!(e003_fail_3_v0, 3);
e_fail!(e004_fail_4_v0, 4);
e_fail!(e005_fail_5_v0, 5);
e_fail!(e006_fail_6_v0, 6);
e_fail!(e007_fail_7_v0, 7);
e_fail!(e008_fail_8_v0, 8);
e_fail!(e009_fail_9_v0, 9);
e_fail!(e010_fail_10_v0, 10);
e_fail!(e011_fail_11_v0, 11);
e_fail!(e012_fail_12_v0, 12);
e_fail!(e013_fail_13_v0, 13);
e_fail!(e014_fail_14_v0, 14);
e_fail!(e015_fail_15_v0, 15);
e_fail!(e016_fail_16_v0, 16);
e_fail!(e017_fail_17_v0, 17);
e_fail!(e018_fail_18_v0, 18);
e_fail!(e019_fail_19_v0, 19);
e_fail!(e020_fail_1_v1, 1);
e_dep_fail!(e021_dep_fail, "tag0");
e_dep_fail!(e022_dep_fail, "tag1");
e_dep_fail!(e023_dep_fail, "tag2");
e_dep_fail!(e024_dep_fail, "tag3");
e_dep_fail!(e025_dep_fail, "tag4");
e_dep_fail!(e026_dep_fail, "tag5");
e_dep_fail!(e027_dep_fail, "tag6");
e_dep_fail!(e028_dep_fail, "tag7");
e_dep_fail!(e029_dep_fail, "tag8");
e_dep_fail!(e030_dep_fail, "tag9");
e_dep_fail!(e031_dep_fail, "tag10");
e_dep_fail!(e032_dep_fail, "tag11");
e_dep_fail!(e033_dep_fail, "tag12");
e_dep_fail!(e034_dep_fail, "tag13");
e_dep_fail!(e035_dep_fail, "tag14");
e_dep_fail!(e036_dep_fail, "tag15");
e_dep_fail!(e037_dep_fail, "tag16");
e_dep_fail!(e038_dep_fail, "tag17");
e_dep_fail!(e039_dep_fail, "tag18");
e_dep_fail!(e040_dep_fail, "tag19");
e_special!(e041_special, "echo 'single quotes'");
e_special!(e042_special, "echo \\\"double quotes\\\"");
e_special!(e043_special, "echo a;echo b");
e_special!(e044_special, "echo a && echo b");
e_special!(e045_special, "echo a || echo b");
e_special!(e046_special, "echo $$");
e_special!(e047_special, "echo $HOME");
e_special!(e048_special, "echo \\\"hello world\\\"");
e_special!(e049_special, "echo a|cat");
e_special!(e050_special, "echo {1,2,3}");
e_special!(e051_special, "echo *.nonexistent_glob");
e_special!(e052_special, "echo \\\"\\\\$var\\\"");
e_special!(e053_special, "echo \\\"backslash: \\\\\\\\\\\"");
e_special!(e054_special, "echo  multiple  spaces  ");
e_special!(e055_special, "echo -n no_newline");
e_special!(e056_special, "echo 'a\\\\tb'");
e_special!(e057_special, "true; true; true");
e_special!(e058_special, "echo x > /dev/null");
e_special!(e059_special, "(echo subshell)");
e_special!(e060_special, "echo \\\"end\\\"");
e_concurrent!(e061_conc_5_v0, 5);
e_concurrent!(e062_conc_6_v0, 6);
e_concurrent!(e063_conc_7_v0, 7);
e_concurrent!(e064_conc_8_v0, 8);
e_concurrent!(e065_conc_9_v0, 9);
e_concurrent!(e066_conc_10_v0, 10);
e_concurrent!(e067_conc_11_v0, 11);
e_concurrent!(e068_conc_12_v0, 12);
e_concurrent!(e069_conc_5_v1, 5);
e_concurrent!(e070_conc_6_v1, 6);
e_concurrent!(e071_conc_7_v1, 7);
e_concurrent!(e072_conc_8_v1, 8);
e_concurrent!(e073_conc_9_v1, 9);
e_concurrent!(e074_conc_10_v1, 10);
e_concurrent!(e075_conc_11_v1, 11);
e_concurrent!(e076_conc_12_v1, 12);
e_concurrent!(e077_conc_5_v2, 5);
e_concurrent!(e078_conc_6_v2, 6);
e_concurrent!(e079_conc_7_v2, 7);
e_concurrent!(e080_conc_8_v2, 8);
e_special!(e081_more, "true");
e_special!(e082_more, "false || true");
e_special!(e083_more, "false; true");
e_special!(e084_more, "test 1 -eq 1");
e_special!(e085_more, "test 1 -eq 2 || true");
e_special!(e086_more, "[ 1 = 1 ]");
e_special!(e087_more, "[ 1 = 2 ] || true");
e_special!(e088_more, "echo $((1+1))");
e_special!(e089_more, "echo $((5*5))");
e_special!(e090_more, "for i in 1 2 3; do echo $i; done");
e_special!(e091_more, "while false; do echo never; done");
e_special!(e092_more, "if true; then echo yes; fi");
e_special!(e093_more, "if false; then echo no; else echo else; fi");
e_special!(e094_more, "case x in x) echo match;; esac");
e_special!(e095_more, "x=1; echo $x");
e_special!(e096_more, "f() { echo fn; }; f");
e_special!(e097_more, "echo a > /tmp/tren_test_$$_unused && rm -f /tmp/tren_test_$$_unused");
e_special!(e098_more, "true > /dev/null 2>&1");
e_special!(e099_more, "{ echo grouped; }");
e_special!(e100_more, "true # trailing comment");

// ═══════════════════════════════════════════════════════════════════════════
// v0.3.0: Frame32, cmd.sh delivery, namespace, spillover
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn frame32_roundtrip_and_parity() {
    use tren::{build_submit, decode_submit, Frame32, OP_SUBMIT};
    let deps = vec![3u64, 5, 7, 11];
    let token = 0xDEAD_BEEF & 0x7FFF_FFFF;
    let f = build_submit(&deps, token, 0);
    assert_eq!(f.op(), OP_SUBMIT);

    // Round-trip via bytes (also forces parity update + verify).
    let bytes = f.to_bytes();
    assert_eq!(bytes.len(), 128);
    let g = Frame32::from_bytes(&bytes).expect("frame parses");
    assert_eq!(g.op(), OP_SUBMIT);
    let (got_deps, got_tok) = decode_submit(&g).expect("decode");
    assert_eq!(got_deps, deps);
    assert_eq!(got_tok, token);

    // Single-bit corruption MUST be detected by the diagonal parity check.
    let mut bad = bytes;
    bad[17] ^= 0x04; // flip one data bit deep in the frame
    assert!(matches!(
        Frame32::from_bytes(&bad),
        Err(tren::FrameError::ParityMismatch)
    ), "single-bit flip should fail parity verification");
}

#[test]
fn frame32_data_slot_roundtrip_all_positions() {
    // The "diagonal parity" bit in slot[i] is bit i. The set/get helpers
    // must round-trip a 31-bit data value through every slot, never
    // colliding with the parity bit.
    use tren::Frame32;
    for slot in 0..32usize {
        let mut f = Frame32::new();
        // Use a different bit-pattern for each slot to catch off-by-one bugs.
        let value = (0x55AA_55AAu32 ^ (slot as u32 * 17)) & 0x7FFF_FFFF;
        f.set(slot, value);
        f.update_parity();
        assert_eq!(f.get(slot), value, "slot {} round-trip mismatch", slot);
        assert!(f.verify_parity(), "slot {} parity invalid", slot);
    }
}

#[test]
fn cmd_sh_delivery_multiline_commands_work() {
    // Verify the inbox→cmd.sh rename pipeline handles multi-line shell
    // bodies (the old text protocol couldn't carry these reliably).
    let sb = Sandbox::new("multiline");
    let multi = "echo first
echo second
exit 7";
    let (rc, stdout, _stderr) = sb.qsub_raw(&[multi]);
    let addr = stdout.trim().to_string();
    assert!(!addr.is_empty(), "no addr returned");
    assert_eq!(rc, 1, "exit 7 should propagate as qsub rc=1");
    assert_eq!(sb.read_node_state(&addr), "DONE(7)");

    let workdir = Sandbox::find_workdir(&sb.dir).expect("workdir gone");
    let cmd_sh = workdir.join("tree").join(&addr).join("cmd.sh");
    let body = std::fs::read_to_string(&cmd_sh).expect("read cmd.sh");
    assert!(body.contains("echo first"));
    assert!(body.contains("echo second"));
    assert!(body.contains("exit 7"));
    let log = std::fs::read_to_string(workdir.join("tree").join(&addr).join("log"))
        .unwrap_or_default();
    assert!(log.contains("first") && log.contains("second"));
}

#[test]
fn namespace_token_propagates_to_qbind_subjobs() {
    // qbind sets TREN_NS=<token> in the executed shell. Two concurrent
    // qbind calls must produce two distinct namespace tokens.
    let sb = Sandbox::new("ns");
    let parent = sb.qsub(&["true"]);
    let qbind_bin = env!("CARGO_BIN_EXE_qbind");

    let mk = |out_file: &str| {
        // Execute a single token: cmd.sh is already run by bash, so we
        // simply emit a literal redirection that captures $TREN_NS.
        let cmd = format!("printf %s $TREN_NS > {}", out_file);
        let st = Command::new(qbind_bin)
            .args(&[&parent, "--", &cmd])
            .current_dir(&sb.dir)
            .output()
            .expect("spawn qbind");
        String::from_utf8_lossy(&st.stdout).trim().to_string()
    };
    let a1 = mk("ns1.txt");
    let a2 = mk("ns2.txt");
    assert_eq!(sb.qwait(&[&a1, &a2]), 0);
    let n1 = std::fs::read_to_string(sb.dir.join("ns1.txt")).unwrap_or_default();
    let n2 = std::fs::read_to_string(sb.dir.join("ns2.txt")).unwrap_or_default();
    assert!(!n1.is_empty(), "TREN_NS empty in qbind1");
    assert!(!n2.is_empty(), "TREN_NS empty in qbind2");
    assert_ne!(n1, n2, "two qbind calls should yield distinct namespaces");
}

#[test]
fn spill_name_format_3digit_then_4digit() {
    use tren::{format_spill_name, parse_spill_seq};
    assert_eq!(format_spill_name(0,    "abc"), ".tren-000-abc");
    assert_eq!(format_spill_name(7,    "abc"), ".tren-007-abc");
    assert_eq!(format_spill_name(999,  "abc"), ".tren-999-abc");
    assert_eq!(format_spill_name(1000, "abc"), ".tren-1000-abc");
    assert_eq!(format_spill_name(9999, "abc"), ".tren-9999-abc");
    // Inverse parse.
    assert_eq!(parse_spill_seq(".tren-000-abc"),  Some(0));
    assert_eq!(parse_spill_seq(".tren-007-abc"),  Some(7));
    assert_eq!(parse_spill_seq(".tren-1000-abc"), Some(1000));
    // Legacy: name without numeric prefix → None.
    assert_eq!(parse_spill_seq(".tren-deadbeef"), None);
}

#[test]
fn spillover_creates_second_spill_past_32_leaves() {
    // Submit enough no-op jobs to fill one spill (cap = 32 leaves =
    // SPILL_NODE_CAP nodes). The 65th submit must land in a freshly
    // spawned sibling spill.
    let sb = Sandbox::new("spillover");
    let cap = tren::SPILL_NODE_CAP as usize;

    let mut handles = Vec::new();
    for i in 0..(cap + 4) {
        let dir = sb.dir.clone();
        let qsub = QSUB.to_string();
        handles.push(thread::spawn(move || {
            Command::new(&qsub)
                .arg(format!("echo s{} > /dev/null", i))
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .expect("qsub")
        }));
    }
    for h in handles {
        let out = h.join().expect("thread");
        assert!(out.status.success(), "qsub failed");
    }

    // After at least cap+1 submits, a second spill must exist.
    let mut spills: Vec<PathBuf> = Vec::new();
    for ent in std::fs::read_dir(&sb.dir).expect("read sandbox") {
        let ent = ent.expect("dirent");
        let name = ent.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(".tren-") && ent.file_type().unwrap().is_dir() {
            spills.push(ent.path());
        }
    }
    assert!(
        spills.len() >= 2,
        "spillover should have created at least 2 spills, found: {:?}",
        spills,
    );

    // Best-effort: shut down every alive spill before sandbox drop so the
    // wrapper child processes don't leak.
    for s in &spills {
        if let Ok(p) = std::fs::read_to_string(s.join("port")) {
            if let Ok(port) = p.trim().parse::<u16>() {
                let _ = tren::send_quit(port);
            }
        }
    }
    thread::sleep(Duration::from_millis(200));
}

// ═══════════════════════════════════════════════════════════════════════════
// v0.4.0: subtree-size default priority + in-process Frame32 decision tree
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn subtree_size_chain_runs_to_completion_under_v04_default_priority() {
    // v0.4.0 default (feature OFF) priority is `subtree_size desc`. The
    // exact pop order is timing-dependent under heavy parallel load
    // (covered by deterministic unit tests in `tren_wrapper.rs::tests`
    // and `priority::tests`). At the integration level we still want to
    // confirm that:
    //   * the new priority codepath runs end-to-end without deadlocks
    //     when many siblings + a long chain are queued behind workers,
    //   * dependency edges are honoured (chain runs in topological
    //     order: j1 -> j2 -> ... -> j5 -> j6),
    //   * sibling no-dep jobs (n1..n4) all complete cleanly.
    let sb = Sandbox::new("subtree_prio_v04");
    let log = sb.dir.join("order.log");
    let log_str = log.display().to_string();

    // Submit a chain rooted at j1 (depth 6). Each chained SUBMIT bumps
    // every transitive ancestor's subtree_size, so by the end j1.size=6.
    let j1 = tren::submit_cmd(&sb.dir, &[],
        &format!("echo j1 >> {}", log_str)).expect("submit j1");
    let j2 = tren::submit_cmd(&sb.dir, &[j1.addr.clone()],
        &format!("echo j2 >> {}", log_str)).expect("submit j2");
    let j3 = tren::submit_cmd(&sb.dir, &[j2.addr.clone()],
        &format!("echo j3 >> {}", log_str)).expect("submit j3");
    let j4 = tren::submit_cmd(&sb.dir, &[j3.addr.clone()],
        &format!("echo j4 >> {}", log_str)).expect("submit j4");
    let j5 = tren::submit_cmd(&sb.dir, &[j4.addr.clone()],
        &format!("echo j5 >> {}", log_str)).expect("submit j5");
    let j6 = tren::submit_cmd(&sb.dir, &[j5.addr.clone()],
        &format!("echo j6 >> {}", log_str)).expect("submit j6");

    // Submit several no-dep siblings. These exercise the queue pop path
    // alongside the chain.
    let mut n_addrs: Vec<String> = Vec::new();
    for i in 0..4 {
        let r = tren::submit_cmd(&sb.dir, &[],
            &format!("echo n{} >> {}", i, log_str)).expect("submit n");
        n_addrs.push(r.addr);
    }

    // Wait for everything via qwait.
    let mut all: Vec<&str> = vec![
        j1.addr.as_str(), j2.addr.as_str(), j3.addr.as_str(),
        j4.addr.as_str(), j5.addr.as_str(), j6.addr.as_str(),
    ];
    for a in &n_addrs { all.push(a.as_str()); }
    let exit = sb.qwait(&all);
    assert_eq!(exit, 0, "qwait of all jobs should exit 0");

    let contents = std::fs::read_to_string(&log)
        .unwrap_or_else(|e| panic!("read {}: {e}", log.display()));
    let lines: Vec<&str> = contents.lines().collect();
    let pos = |tag: &str| -> usize {
        lines.iter().position(|l| l.trim() == tag)
            .unwrap_or_else(|| panic!("missing {} in log:\n{}", tag, contents))
    };

    // Topological order MUST be honoured: each chain step only runs
    // after its predecessor.
    assert!(pos("j1") < pos("j2"), "j1 must finish before j2:\n{}", contents);
    assert!(pos("j2") < pos("j3"), "j2 must finish before j3:\n{}", contents);
    assert!(pos("j3") < pos("j4"), "j3 must finish before j4:\n{}", contents);
    assert!(pos("j4") < pos("j5"), "j4 must finish before j5:\n{}", contents);
    assert!(pos("j5") < pos("j6"), "j5 must finish before j6:\n{}", contents);

    // All siblings completed.
    for i in 0..4 {
        let _ = pos(&format!("n{}", i)); // panics if missing
    }
}

#[cfg(feature = "model")]
#[test]
fn model_build_works_without_external_tren_model_in_path() {
    // v0.4.0: under `--features model` the wrapper must do all priority
    // inference IN-PROCESS via the Frame32-resident decision tree. The old
    // `call_model` fork-exec is gone, so the wrapper must run successfully
    // even when PATH is empty (no `tren-model` binary discoverable anywhere).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "tren-smoke-nopath-{}-{}", std::process::id(), stamp));
    std::fs::create_dir_all(&dir).expect("mkdir nopath sandbox");

    // Spawn the wrapper with PATH cleared. If the model build still tried
    // to fork-exec `tren-model`, the SUBMIT below would either hang on the
    // 5s UDP timeout or come back with a degraded reply.
    let mut wrapper = Command::new(WRAPPER)
        .current_dir(&dir)
        .env_clear()
        .env("HOME", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn tren-wrapper with empty PATH");

    // Wait for workdir to come up.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        if Sandbox::find_workdir(&dir).is_some() { found = true; break; }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(found, "wrapper without PATH did not create workdir");

    // Submit a few jobs through the lib API (exercises the model inference
    // path) and verify they complete normally.
    let mut addrs = Vec::new();
    for i in 0..3 {
        let r = tren::submit_cmd(&dir, &[], &format!("echo nopath_{}", i))
            .expect("submit_cmd in no-PATH wrapper");
        addrs.push(r.addr);
    }

    let qwait_status = Command::new(QWAIT)
        .args(addrs.iter().map(String::as_str))
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn qwait");
    assert_eq!(qwait_status.code().unwrap_or(-1), 0,
        "qwait should exit 0 when wrapper runs in-process model with no PATH");

    // Cleanup.
    if let Some(workdir) = Sandbox::find_workdir(&dir) {
        if let Ok(p) = std::fs::read_to_string(workdir.join("port")) {
            if let Ok(port) = p.trim().parse::<u16>() {
                let _ = tren::send_quit(port);
            }
        }
    }
    let _ = wrapper.kill();
    let _ = wrapper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
