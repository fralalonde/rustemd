#![cfg(unix)]

//! End-to-end test: run the real manager daemon, then drive it with the
//! compiled `rustemctl` binary (the extracted `systemctl`-compatible CLI) over
//! the socket. This proves the extraction kept the CLI talking to the daemon
//! through `rustemd::client`/`rustemd::paths`.
//!
//! The daemon runs as a *separate process* (the test binary re-exec'd as a
//! daemon) rather than a thread: the manager installs a `signalfd` + `SIGCHLD`
//! handler and reaps children with `waitpid(-1)`, which would steal the
//! `rustemctl` child processes this test spawns and turn `Command::wait()`
//! into `ECHILD`. A process boundary keeps the reaper out of our way.

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Serializes `RUSTEMD_*` env mutation across tests in this binary (tests run
/// in parallel threads, and the env vars are process-global).
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct Scratch {
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Scratch {
        let lock = ENV_LOCK.lock().unwrap();
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
            std::env::set_var("RUSTEMD_UNIT_PATH", &units);
            std::env::set_var("RUSTEMD_CONFIG_DIR", &config);
            std::env::set_var("RUSTEMD_RUNTIME_DIR", &run);
            std::env::set_var("RUSTEMD_JOURNAL_DIR", &journal);
            std::env::set_var("RUSTEMD_SOCKET", run.join("control.sock"));
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
/// with `RUSTEMCTL_DAEMON=1`, it becomes the daemon (serving the `RUSTEMD_*`
/// scratch env it inherited) and blocks until the shutdown op arrives.
#[test]
fn daemon_subprocess() {
    if std::env::var_os("RUSTEMCTL_DAEMON").is_none() {
        return; // ordinary `cargo test` run: this test is a no-op.
    }
    let mut mgr =
        rustemd::manager::Manager::new(rustemd::manager::ManagerCfg::for_mode(false).unwrap())
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

/// Run the compiled `rustemctl` binary and return its raw output (no exit
/// code assertion — `is-active`/`is-failed`/`is-enabled` return non-zero for
/// their "negative" answers by design).
fn rustemctl_raw(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rustemctl"))
        .args(args)
        .output()
        .expect("failed to run rustemctl")
}

/// Run `rustemctl`, asserting a zero exit code, and return stdout.
fn rustemctl(args: &[&str]) -> String {
    let out = rustemctl_raw(args);
    assert!(
        out.status.success(),
        "rustemctl {args:?} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The state `rustemctl is-active <unit>` reports (ignores its exit code).
fn is_active(unit: &str) -> String {
    String::from_utf8_lossy(&rustemctl_raw(&["is-active", unit]).stdout)
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
    // not a thread). It inherits the scratch `RUSTEMD_*` env we just set.
    let exe = std::env::current_exe().unwrap();
    let daemon = Command::new(&exe)
        .arg("daemon_subprocess")
        .env("RUSTEMCTL_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn daemon");

    let socket: std::path::PathBuf = std::env::var_os("RUSTEMD_SOCKET").unwrap().into();
    assert!(wait_for(Duration::from_secs(5), || {
        std::path::Path::new(&socket).exists()
    }));

    // list-units works against the live daemon.
    let stdout = rustemctl(&["list-units"]);
    assert!(
        stdout.contains("UNIT"),
        "list-units should print the header: {stdout}"
    );

    // start → active round-trip.
    rustemctl(&["start", "hello.service"]);
    let active = wait_for(Duration::from_secs(3), || {
        is_active("hello.service") == "active"
    });
    assert!(active, "hello.service should become active");

    let stdout = rustemctl(&["status", "hello.service"]);
    assert!(
        stdout.contains("active"),
        "status should report active: {stdout}"
    );

    // stop → inactive round-trip.
    rustemctl(&["stop", "hello.service"]);
    let inactive = wait_for(Duration::from_secs(3), || {
        is_active("hello.service") == "inactive"
    });
    assert!(inactive, "hello.service should become inactive after stop");

    // Orderly shutdown: ask the manager to stop and exit, then reap it.
    let _ = rustemd::client::request_json(&socket, &serde_json::json!({ "op": "shutdown" }));
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
    let socket: std::path::PathBuf = std::env::var_os("RUSTEMD_SOCKET").unwrap().into();
    assert!(wait_for(Duration::from_secs(5), || {
        std::path::Path::new(&socket).exists()
    }));
    daemon
}

/// Ask the daemon to stop and exit, then reap it (asserting a clean exit).
fn shutdown_daemon(daemon: std::process::Child) {
    let socket: std::path::PathBuf = std::env::var_os("RUSTEMD_SOCKET").unwrap().into();
    let _ = rustemd::client::request_json(&socket, &serde_json::json!({ "op": "shutdown" }));
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

    rustemctl(&["start", "jctl.service"]);
    let active = wait_for(Duration::from_secs(3), || {
        is_active("jctl.service") == "active"
    });
    assert!(active, "jctl.service should become active");

    // `journalctl -u <unit> -n 5` reuses the same daemon journal op and shows
    // the service's captured stdout line.
    let found = wait_for(Duration::from_secs(5), || {
        let out = rustemctl_raw(&["journalctl", "-u", "jctl.service", "-n", "5"]);
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
    let out = rustemctl_raw(&["journalctl", "-p", "err"]);
    assert!(!out.status.success(), "-p should exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--priority is not yet supported"),
        "-p should report the unsupported message, got: {stderr}"
    );
}
