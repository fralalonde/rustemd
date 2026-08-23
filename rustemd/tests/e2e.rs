//! End-to-end test: run the real manager daemon in a thread and drive it over
//! the socket with the programmatic `Control` API (the library alternative to
//! `systemctl`/D-Bus). Exercises loading, the full process lifecycle, and
//! query operations end to end.

mod common;

use std::time::Duration;

use common::{Scratch, wait_for};
use rustemd::control::{Control, SocketClient};

/// Spawn the manager daemon in a background thread and return a handle that
/// shuts it down cleanly on drop.
struct Daemon {
    handle: Option<std::thread::JoinHandle<()>>,
    socket: std::path::PathBuf,
}

impl Daemon {
    fn start() -> Daemon {
        let socket = std::env::var_os("RUSTEMD_SOCKET").unwrap().into();
        let handle = std::thread::spawn(|| {
            let mut mgr = rustemd::manager::Manager::new(
                rustemd::manager::ManagerCfg::for_mode(false).unwrap(),
            )
            .unwrap();
            mgr.load_all();
            mgr.bind_ipc().unwrap();
            mgr.bind_notify().ok();
            mgr.setup_signals();
            mgr.run();
        });
        Daemon {
            handle: Some(handle),
            socket,
        }
    }

    fn client(&self) -> SocketClient {
        SocketClient::for_mode(false).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Ask the manager to stop and exit, then reap the thread.
        let _ =
            rustemd::client::request_json(&self.socket, &serde_json::json!({ "op": "shutdown" }));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn start_status_stop_lifecycle() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "hello.service",
        "[Unit]\nDescription=hello service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n[Install]\nWantedBy=multi-user.target\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let mut ctl = daemon.client();

    // Start: the oneshot /bin/true completes almost immediately and, with
    // RemainAfterExit=yes, parks in active(exited).
    ctl.start(&["hello.service"]).unwrap();
    let st = wait_for(Duration::from_secs(3), || {
        ctl.status(&["hello.service"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "active" && s.sub == "exited")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(st, "oneshot service should reach active(exited)");

    let status = ctl.status(&["hello.service"]).unwrap();
    let s = &status[0];
    assert_eq!(s.name, "hello.service");
    assert_eq!(s.description, "hello service");
    assert_eq!(s.enabled, "disabled");

    // is_active / is_enabled through the trait.
    assert_eq!(ctl.is_active(&["hello.service"]).unwrap(), vec!["active"]);
    assert_eq!(
        ctl.is_enabled(&["hello.service"]).unwrap(),
        vec!["disabled"]
    );

    // enable/disable round-trip (symlink management over the wire).
    ctl.enable(&["hello.service"]).unwrap();
    assert_eq!(ctl.is_enabled(&["hello.service"]).unwrap(), vec!["enabled"]);
    ctl.disable(&["hello.service"]).unwrap();
    assert_eq!(
        ctl.is_enabled(&["hello.service"]).unwrap(),
        vec!["disabled"]
    );

    // list_units sees it.
    let units = ctl.list_units(&[], None).unwrap();
    assert!(units.iter().any(|u| u.unit == "hello.service"));

    // Stop → inactive.
    ctl.stop(&["hello.service"]).unwrap();
    let stopped = wait_for(Duration::from_secs(3), || {
        ctl.status(&["hello.service"])
            .map(|v| v.first().map(|s| s.active == "inactive").unwrap_or(false))
            .unwrap_or(false)
    });
    assert!(stopped, "service should return to inactive after stop");
}

#[test]
fn long_running_service_and_kill() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "sleeper.service",
        "[Service]\nType=simple\nExecStart=/bin/sleep 30\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let mut ctl = daemon.client();

    // Type=simple goes active immediately on spawn (the process keeps running).
    ctl.start(&["sleeper.service"]).unwrap();
    let running = wait_for(Duration::from_secs(3), || {
        ctl.status(&["sleeper.service"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "active" && s.sub == "running")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(running, "simple service should be active(running)");

    let main_pid = ctl
        .status(&["sleeper.service"])
        .unwrap()
        .first()
        .and_then(|s| s.main_pid)
        .expect("running service should have a main pid");

    // kill the process group → the service is torn down.
    ctl.kill("sleeper.service", "SIGKILL").unwrap();
    let dead = wait_for(Duration::from_secs(3), || {
        ctl.status(&["sleeper.service"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "inactive" || s.active == "failed")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(dead, "service should be torn down after kill");
    assert!(main_pid > 0);
}

#[test]
fn stop_terminates_running_service() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "sleeper.service",
        "[Service]\nType=simple\nExecStart=/bin/sleep 30\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let mut ctl = daemon.client();
    ctl.start(&["sleeper.service"]).unwrap();
    let running = wait_for(Duration::from_secs(3), || {
        ctl.status(&["sleeper.service"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "active" && s.sub == "running")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(running, "sleeper should be active(running)");

    // stop() sends SIGTERM to the process group. Regression guard: the child
    // must start with a clean signal mask — if it inherits the manager's
    // blocked mask it ignores SIGTERM and this hangs for TimeoutStopSec.
    ctl.stop(&["sleeper.service"]).unwrap();
    let stopped = wait_for(Duration::from_secs(3), || {
        ctl.status(&["sleeper.service"])
            .map(|v| v.first().map(|s| s.active == "inactive").unwrap_or(false))
            .unwrap_or(false)
    });
    assert!(
        stopped,
        "stop() should SIGTERM and tear down the running service"
    );
}

/// A `Type=simple` service that self-daemonizes (forks a child, then the main
/// process exits) must have its orphaned process group swept and SIGKILLed so
/// nothing escapes process-group tracking.
#[test]
fn daemonizing_service_orphans_are_swept() {
    let scratch = Scratch::new();
    let pidfile = scratch.dir.path().join("orphan.pid");
    let pidfile_s = pidfile.to_string_lossy().to_string();
    scratch.write_unit(
        "daemonize.service",
        &format!(
            "[Service]\nType=simple\nExecStart=/bin/sh -c 'sleep 60 & echo $! > {pidfile_s}; exit 0'\n"
        ),
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let mut ctl = daemon.client();
    ctl.start(&["daemonize.service"]).unwrap();

    // The orphaned `sleep 60` records its pid; once the main sh exits, the
    // sweep SIGKILLs the orphan so its pid disappears from /proc.
    assert!(wait_for(Duration::from_secs(5), || pidfile.exists()));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid > 0);
    let swept = wait_for(Duration::from_secs(5), || {
        !std::path::Path::new(&format!("/proc/{pid}")).exists()
    });
    assert!(
        swept,
        "orphaned daemon process should be swept (SIGKILLed) on main-process exit"
    );
}

/// A `.socket` unit binds a unix socket; the first connection activates its
/// matching `.service` on demand (inetd-style socket activation).
#[cfg(feature = "socket")]
#[test]
fn socket_activates_service_on_connection() {
    use std::os::unix::net::UnixStream;

    let scratch = Scratch::new();
    let sock = scratch.dir.path().join("echo.sock");
    let sock_s = sock.to_string_lossy().to_string();
    scratch.write_unit("echo.socket", &format!("[Socket]\nListenStream={sock_s}\n"));
    scratch.write_unit(
        "echo.service",
        "[Service]\nType=simple\nExecStart=/bin/sleep 30\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let mut ctl = daemon.client();

    // Start the socket unit: it binds and listens, but the service stays down.
    ctl.start(&["echo.socket"]).unwrap();
    let listening = wait_for(Duration::from_secs(3), || {
        ctl.status(&["echo.socket"])
            .map(|v| v.first().map(|s| s.active == "active").unwrap_or(false))
            .unwrap_or(false)
    });
    assert!(listening, "socket unit should be active(listening)");
    assert_eq!(
        ctl.is_active(&["echo.service"]).unwrap(),
        vec!["inactive"],
        "service must not start before a connection arrives"
    );

    // Connect: the listener becomes readable and triggers on-demand activation.
    let _conn = UnixStream::connect(&sock).unwrap();

    let activated = wait_for(Duration::from_secs(3), || {
        ctl.status(&["echo.service"])
            .map(|v| v.first().map(|s| s.active == "active").unwrap_or(false))
            .unwrap_or(false)
    });
    assert!(
        activated,
        "connecting should activate the service via socket activation"
    );
}

/// A `.mount` unit mounts a filesystem on start and unmounts on stop, with no
/// process to supervise. `mount(2)` needs `CAP_SYS_ADMIN`, so this test
/// self-skips when not run as root (or in an unprivileged user+mount
/// namespace). The daemon runs as a *thread* in this process, so a `tmpfs`
/// mounted by the manager is visible to `/proc/self/mountinfo` here.
#[cfg(target_os = "linux")]
#[test]
fn mount_unit_lifecycle() {
    if euid() != 0 {
        eprintln!("skipping mount_unit_lifecycle: mount(2) requires root");
        return;
    }

    let scratch = Scratch::new();
    let mountpoint = scratch.dir.path().join("demo");
    std::fs::create_dir_all(&mountpoint).unwrap();
    let where_s = mountpoint.to_string_lossy().to_string();
    scratch.write_unit(
        "tmp-demo.mount",
        &format!("[Mount]\nWhat=tmpfs\nWhere={where_s}\nType=tmpfs\nOptions=mode=1777\n"),
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));

    let mut ctl = daemon.client();

    // Start → mount(2) succeeds → active(mounted).
    ctl.start(&["tmp-demo.mount"]).unwrap();
    let mounted = wait_for(Duration::from_secs(3), || {
        ctl.status(&["tmp-demo.mount"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "active" && s.sub == "mounted")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    });
    assert!(mounted, "mount unit should reach active(mounted)");
    assert!(
        is_mounted(&mountpoint),
        "tmpfs should be mounted at {where_s}"
    );

    // Stop → umount2(2) → inactive, and the filesystem is gone.
    ctl.stop(&["tmp-demo.mount"]).unwrap();
    let stopped = wait_for(Duration::from_secs(3), || {
        ctl.status(&["tmp-demo.mount"])
            .map(|v| v.first().map(|s| s.active == "inactive").unwrap_or(false))
            .unwrap_or(false)
    });
    assert!(stopped, "mount unit should return to inactive after stop");
    assert!(
        !is_mounted(&mountpoint),
        "tmpfs should be unmounted after stop"
    );
}

/// The current effective uid (0 = root), read from `/proc/self/status`.
#[cfg(target_os = "linux")]
fn euid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(u32::MAX)
}

/// Is `path` a mount point, per `/proc/self/mountinfo`?
#[cfg(target_os = "linux")]
fn is_mounted(path: &std::path::Path) -> bool {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    std::fs::read_to_string("/proc/self/mountinfo")
        .map(|s| {
            s.lines().any(|l| {
                // Field 4 (0-indexed) is the mount point; decode the
                // `\040`/`\011`/`\012`/`\134` escapes the kernel uses.
                l.split(' ').nth(4).is_some_and(|mp| {
                    let decoded = mp
                        .replace("\\040", " ")
                        .replace("\\011", "\t")
                        .replace("\\012", "\n")
                        .replace("\\134", "\\");
                    std::path::Path::new(&decoded) == canon
                })
            })
        })
        .unwrap_or(false)
}
