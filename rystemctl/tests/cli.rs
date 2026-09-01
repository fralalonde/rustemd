#![cfg(unix)]

//! End-to-end test: run the real manager daemon, then drive it with the
//! compiled `rystemctl` binary (the extracted `systemctl`-compatible CLI) over
//! the socket. This proves the extraction kept the CLI talking to the daemon
//! through `rystemd::client`/`rystemd::paths`.
//!
//! The daemon runs as a *separate process* (the test binary re-exec'd as a
//! daemon) rather than a thread: the manager installs a `signalfd` + `SIGCHLD`
//! handler and reaps children with `waitpid(-1)`, which would steal the
//! `rystemctl` child processes this test spawns and turn `Command::wait()`
//! into `ECHILD`. A process boundary keeps the reaper out of our way.

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Serializes `RYSTEMD_*` env mutation across tests in this binary (tests run
/// in parallel threads, and the env vars are process-global).
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Scratch {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        // Recover from a previous test that panicked while holding the env
        // lock (std poisons the mutex on drop-during-unwind), so a single
        // failing test doesn't cascade into unrelated PoisonError panics.
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let units = root.join("units");
        let config = root.join("config");
        let run = root.join("run");
        let journal = root.join("journal");
        for d in [&units, &config, &run, &journal] {
            std::fs::create_dir_all(d).unwrap();
        }
        unsafe {
            std::env::set_var("RYSTEMD_UNIT_PATH", &units);
            std::env::set_var("RYSTEMD_CONFIG_DIR", &config);
            std::env::set_var("RYSTEMD_RUNTIME_DIR", &run);
            std::env::set_var("RYSTEMD_JOURNAL_DIR", &journal);
            std::env::set_var("RYSTEMD_SOCKET", run.join("control.sock"));
        }
        Scratch { _lock: lock, dir }
    }

    fn write_unit(&self, name: &str, body: &str) {
        std::fs::write(self.dir.path().join("units").join(name), body).unwrap();
    }
}

fn wait_for<F: FnMut() -> bool>(timeout: Duration, mut f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

/// Hosts the real manager. When this test is re-exec'd as a child process
/// with `RUSTEMCTL_DAEMON=1`, it becomes the daemon (serving the `RYSTEMD_*`
/// scratch env it inherited) and blocks until the shutdown op arrives.
#[test]
fn daemon_subprocess() {
    if std::env::var_os("RUSTEMCTL_DAEMON").is_none() {
        return; // ordinary `cargo test` run: this test is a no-op.
    }
    let mut mgr =
        rystemd::manager::Manager::new(rystemd::manager::ManagerCfg::for_mode(false).unwrap())
            .unwrap();
    mgr.load_all();
    // Mirror the real daemon (daemon::run_daemon): enumerate kernel devices
    // into runtime `.device` units before serving requests.
    #[cfg(target_os = "linux")]
    mgr.udev_init();
    mgr.bind_ipc().unwrap();
    mgr.bind_notify().ok();
    mgr.setup_signals();
    mgr.run();
}

/// Run the compiled `rystemctl` binary and return its raw output (no exit
/// code assertion — `is-active`/`is-failed`/`is-enabled` return non-zero for
/// their "negative" answers by design).
fn rystemctl_raw(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rystemctl"))
        .args(args)
        .output()
        .expect("failed to run rystemctl")
}

/// Run `rystemctl`, asserting a zero exit code, and return stdout.
fn rystemctl(args: &[&str]) -> String {
    let out = rystemctl_raw(args);
    assert!(
        out.status.success(),
        "rystemctl {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The state `rystemctl is-active <unit>` reports (ignores its exit code).
fn is_active(unit: &str) -> String {
    String::from_utf8_lossy(&rystemctl_raw(&["is-active", unit]).stdout)
        .trim()
        .to_string()
}

#[test]
fn cli_drives_daemon_roundtrip() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "hello.service",
        "[Unit]\nDescription=hello service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n",
    );

    // Fork the daemon as a child process (see module doc for why a process,
    // not a thread). It inherits the scratch `RYSTEMD_*` env we just set.
    let exe = std::env::current_exe().unwrap();
    let daemon = Command::new(&exe)
        .arg("daemon_subprocess")
        .env("RUSTEMCTL_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn daemon");

    let socket: std::path::PathBuf = std::env::var_os("RYSTEMD_SOCKET").unwrap().into();
    assert!(wait_for(Duration::from_secs(5), || {
        std::path::Path::new(&socket).exists()
    }));

    // list-units works against the live daemon.
    let stdout = rystemctl(&["list-units"]);
    assert!(
        stdout.contains("UNIT"),
        "list-units should print the header: {stdout}"
    );

    // start → active round-trip.
    rystemctl(&["start", "hello.service"]);
    let active = wait_for(Duration::from_secs(3), || {
        is_active("hello.service") == "active"
    });
    assert!(active, "hello.service should become active");

    let stdout = rystemctl(&["status", "hello.service"]);
    assert!(
        stdout.contains("active"),
        "status should report active: {stdout}"
    );

    // stop → inactive round-trip.
    rystemctl(&["stop", "hello.service"]);
    let inactive = wait_for(Duration::from_secs(3), || {
        is_active("hello.service") == "inactive"
    });
    assert!(inactive, "hello.service should become inactive after stop");

    // Orderly shutdown: ask the manager to stop and exit, then reap it.
    let _ = rystemd::client::request_json(&socket, &serde_json::json!({ "op": "shutdown" }));
    let status = daemon.wait_with_output().expect("failed to reap daemon");
    assert!(
        status.status.success(),
        "daemon should exit cleanly after shutdown"
    );
}

/// Fork the scratch manager as a child process (re-exec'd test binary) and
/// wait until its control socket exists. Returns the live child handle.
fn spawn_daemon() -> std::process::Child {
    let exe = std::env::current_exe().unwrap();
    let daemon = Command::new(&exe)
        .arg("daemon_subprocess")
        .env("RUSTEMCTL_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn daemon");
    let socket: std::path::PathBuf = std::env::var_os("RYSTEMD_SOCKET").unwrap().into();
    assert!(wait_for(Duration::from_secs(5), || {
        std::path::Path::new(&socket).exists()
    }));
    daemon
}

/// Ask the daemon to stop and exit, then reap it (asserting a clean exit).
fn shutdown_daemon(daemon: std::process::Child) {
    let socket: std::path::PathBuf = std::env::var_os("RYSTEMD_SOCKET").unwrap().into();
    let _ = rystemd::client::request_json(&socket, &serde_json::json!({ "op": "shutdown" }));
    let status = daemon.wait_with_output().expect("failed to reap daemon");
    assert!(
        status.status.success(),
        "daemon should exit cleanly after shutdown"
    );
}

#[test]
fn journalctl_reads_unit_marker() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "jctl.service",
        "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/echo journalctl-marker-42\n",
    );
    let daemon = spawn_daemon();

    rystemctl(&["start", "jctl.service"]);
    let active = wait_for(Duration::from_secs(3), || {
        is_active("jctl.service") == "active"
    });
    assert!(active, "jctl.service should become active");

    // `journalctl -u <unit> -n 5` reuses the same daemon journal op and shows
    // the service's captured stdout line.
    let found = wait_for(Duration::from_secs(5), || {
        let out = rystemctl_raw(&["journalctl", "-u", "jctl.service", "-n", "5"]);
        String::from_utf8_lossy(&out.stdout).contains("journalctl-marker-42")
    });
    assert!(
        found,
        "journalctl -u jctl.service should show the service's stdout marker"
    );

    shutdown_daemon(daemon);
}

#[test]
fn journalctl_priority_is_rejected() {
    let out = rystemctl_raw(&["journalctl", "-p", "err"]);
    assert!(!out.status.success(), "-p should exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--priority is not yet supported"),
        "-p should report the unsupported message, got: {stderr}"
    );
}

/// `try-restart` of an inactive unit must do NOTHING (no start, no ExecStop);
/// `restart-or-start` is the command that starts inactive units. On an active
/// unit both truly restart (ExecStop runs), proving the three ops are
/// distinct.
#[test]
fn try_restart_inactive_is_noop_restart_or_start_starts() {
    let scratch = Scratch::new();
    let marker = scratch.dir.path().join("re.stopped");
    scratch.write_unit(
        "re.service",
        &format!(
            "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\nExecStop=/bin/sh -c 'echo stopped > {}'\n",
            marker.display()
        ),
    );
    let daemon = spawn_daemon();

    // Inactive unit: try-restart must leave it inactive (no start, no stop).
    rystemctl(&["try-restart", "re.service"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        is_active("re.service") == "inactive",
        "try-restart must not start an inactive unit"
    );
    assert!(
        !marker.exists(),
        "ExecStop must not run on try-restart of an inactive unit"
    );

    // restart-or-start: starts the inactive unit (ExecStop must NOT run).
    rystemctl(&["restart-or-start", "re.service"]);
    let active = wait_for(Duration::from_secs(5), || {
        is_active("re.service") == "active"
    });
    assert!(active, "restart-or-start should start an inactive unit");
    assert!(
        !marker.exists(),
        "ExecStop must not run when restart-or-start starts an inactive unit"
    );

    // Active unit: try-restart restarts it (ExecStop runs).
    rystemctl(&["try-restart", "re.service"]);
    let stopped = wait_for(Duration::from_secs(5), || marker.exists());
    assert!(
        stopped,
        "ExecStop should run on try-restart of an active unit"
    );

    shutdown_daemon(daemon);
}

/// `mask` makes a unit unstartable and `is-enabled` reports `masked`; `unmask`
/// restores it (is-enabled state and startability both come back). Uses a
/// higher-precedence search dir so the mask symlink shadows the real unit file
/// without touching it — the same layout real systemd uses.
#[test]
fn mask_unmask_roundtrip() {
    let scratch = Scratch::new();
    let shadow = scratch.dir.path().join("shadow");
    std::fs::create_dir_all(&shadow).unwrap();
    let search = format!(
        "{}:{}",
        shadow.display(),
        scratch.dir.path().join("units").display()
    );
    // Prepend the shadow dir to the unit search path (overrides Scratch's
    // plain `units`), so a mask symlink there shadows the real unit file.
    unsafe {
        std::env::set_var("RYSTEMD_UNIT_PATH", &search);
    }
    scratch.write_unit(
        "halo.service",
        "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
    );
    let enabled_of = |u: &str| {
        String::from_utf8_lossy(&rystemctl_raw(&["is-enabled", u]).stdout)
            .trim()
            .to_string()
    };
    let before = enabled_of("halo.service");

    let daemon = spawn_daemon();

    rystemctl(&["start", "halo.service"]);
    let active = wait_for(Duration::from_secs(5), || {
        is_active("halo.service") == "active"
    });
    assert!(active, "halo.service should start before masking");

    // Mask: is-enabled now reports masked.
    rystemctl(&["mask", "halo.service"]);
    assert_eq!(enabled_of("halo.service"), "masked");

    // Stop it, reload so the daemon sees the mask, then start must fail.
    rystemctl(&["stop", "halo.service"]);
    let inactive = wait_for(Duration::from_secs(5), || {
        is_active("halo.service") != "active"
    });
    assert!(inactive, "halo.service should stop before masking assert");
    rystemctl(&["daemon-reload"]);
    let out = rystemctl_raw(&["start", "halo.service"]);
    assert!(
        !out.status.success(),
        "start must fail while the unit is masked, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Unmask restores is-enabled and startability.
    rystemctl(&["unmask", "halo.service"]);
    rystemctl(&["daemon-reload"]);
    rystemctl(&["start", "halo.service"]);
    let active2 = wait_for(Duration::from_secs(5), || {
        is_active("halo.service") == "active"
    });
    assert!(active2, "unmasked unit should start again");
    assert_eq!(enabled_of("halo.service"), before);

    shutdown_daemon(daemon);
}

/// `kill --signal` must fail on an unknown signal instead of silently
/// downgrading to SIGTERM, and pass through a valid signal verbatim.
#[test]
fn kill_rejects_unknown_signal() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "sleeper.service",
        "[Service]\nType=simple\nExecStart=/bin/sleep 30\n",
    );
    let daemon = spawn_daemon();
    rystemctl(&["start", "sleeper.service"]);
    let active = wait_for(Duration::from_secs(5), || {
        is_active("sleeper.service") == "active"
    });
    assert!(active, "sleeper.service should start");

    // Unknown signal: must be a hard error (non-zero exit, no kill issued) —
    // not silently converted to SIGTERM (regression for the unwrap_or fallback).
    let bad = rystemctl_raw(&["kill", "--signal=KILLLG", "sleeper.service"]);
    assert!(
        !bad.status.success(),
        "kill --signal=KILLLG must fail: {:?}",
        bad.status
    );
    let err = String::from_utf8_lossy(&bad.stderr);
    assert!(
        err.contains("KILLLG") || err.contains("signal"),
        "kill must report the bad signal, got: {err}"
    );
    // The service must still be alive (no bogus SIGTERM was sent).
    assert_eq!(is_active("sleeper.service"), "active");

    // Valid signal passes through and stops the service.
    rystemctl(&["kill", "--signal=SIGKILL", "sleeper.service"]);
    let stopped = wait_for(Duration::from_secs(5), || {
        matches!(is_active("sleeper.service").as_str(), "inactive" | "failed")
    });
    assert!(stopped, "SIGKILL should stop sleeper.service");

    shutdown_daemon(daemon);
}

/// Exercises the newly-added systemctl surface against the live daemon:
/// reset-failed, list-dependencies (forward + reverse), list-sockets, and clean.
#[test]
fn extended_systemctl_surface() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "boom.service",
        "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/false\n",
    );
    scratch.write_unit(
        "dep2.service",
        "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n",
    );
    scratch.write_unit(
        "dep1.service",
        "[Unit]\nWants=dep2.service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
    );
    let daemon = spawn_daemon();

    // reset-failed: a unit whose ExecStart fails goes failed; reset-failed
    // clears it back to inactive.
    rystemctl(&["start", "boom.service"]);
    let failed = wait_for(Duration::from_secs(5), || {
        is_active("boom.service") == "failed"
    });
    assert!(
        failed,
        "ExecStart=/bin/false should leave boom.service failed"
    );
    rystemctl(&["reset-failed", "boom.service"]);
    let inactive = wait_for(Duration::from_secs(5), || {
        is_active("boom.service") != "failed"
    });
    assert!(inactive, "reset-failed should clear the failed state");

    // list-dependencies: dep1 wants dep2 (forward), dep1 requires it (reverse).
    let deps = rystemctl(&["list-dependencies", "dep1.service"]);
    assert!(
        deps.lines().any(|l| l.trim() == "dep2.service"),
        "dep1.service should list dep2.service as a dependency: {deps}"
    );
    let rev = rystemctl(&["list-dependencies", "dep2.service", "--reverse"]);
    assert!(
        rev.lines().any(|l| l.trim() == "dep1.service"),
        "reverse deps of dep2 should include dep1.service: {rev}"
    );

    // list-sockets: no socket units in the scratch, but the op runs cleanly.
    let out = rystemctl_raw(&["list-sockets", "--no-legend"]);
    assert!(
        out.status.success(),
        "list-sockets should succeed, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // clean: prunes a unit's runtime state (no-op message when none).
    let cleaned = rystemctl(&["clean", "boom.service"]);
    assert!(
        cleaned.contains("Clean"),
        "clean should print a message: {cleaned}"
    );

    shutdown_daemon(daemon);
}
