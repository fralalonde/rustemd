#![cfg(unix)]

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
            // Mirror the real daemon (cli::run_daemon): enumerate kernel
            // devices into runtime `.device` units before serving requests.
            #[cfg(all(target_os = "linux", feature = "udev"))]
            mgr.udev_init();
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
#[cfg(all(unix, feature = "socket"))]
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

/// A `.timer` unit arms a monotonic schedule and fires its target. This uses
/// the standard systemd idiom for running a one-shot job periodically:
/// `OnUnitInactiveSec` re-fires the (now-inactive) target, so the target's
/// side effect is observable on every elapse.
#[test]
fn timer_activates_target_on_schedule() {
    let scratch = Scratch::new();
    let tick = scratch.dir.path().join("ticks");
    let tick_s = tick.to_string_lossy().to_string();
    scratch.write_unit(
        "tick.service",
        &format!("[Unit]\nDescription=tick\n[Service]\nType=oneshot\nExecStart=/bin/sh -c 'echo tick >> {tick_s}'\n"),
    );
    scratch.write_unit(
        "tick.timer",
        "[Unit]\nDescription=tick timer\n[Timer]\nOnBootSec=1s\nUnit=tick.service\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();

    ctl.start(&["tick.timer"]).unwrap();
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.is_active(&["tick.timer"])
            .map(|v| v == vec!["active"])
            .unwrap_or(false)
    }));

    // The timer fires tick.service, which appends to the marker file.
    assert!(
        wait_for(Duration::from_secs(5), || tick.exists()),
        "timer should fire tick.service, which writes the marker"
    );

    // list_timers records the last elapse — direct proof the *timer* fired.
    let last_set = wait_for(Duration::from_secs(3), || {
        ctl.list_timers()
            .map(|v| v.iter().any(|t| t.unit == "tick.timer" && t.last.is_some()))
            .unwrap_or(false)
    });
    assert!(
        last_set,
        "list_timers should record the timer's last elapse"
    );
}

/// A `.target` is a pure grouping unit: starting it pulls in (and orders) its
/// `Wants=`, each of which reaches its own active state.
#[test]
fn target_start_pulls_in_wants() {
    let scratch = Scratch::new();
    scratch.write_unit(
        "demo.service",
        "[Unit]\nDescription=demo svc\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
    );
    scratch.write_unit(
        "demo.target",
        "[Unit]\nDescription=demo target\nWants=demo.service\nAfter=demo.service\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();

    ctl.start(&["demo.target"]).unwrap();

    assert!(wait_for(Duration::from_secs(3), || {
        ctl.is_active(&["demo.target"])
            .map(|v| v == vec!["active"])
            .unwrap_or(false)
    }));
    assert_eq!(ctl.is_active(&["demo.service"]).unwrap(), vec!["active"]);
}

/// The `examples/live/` demo units, exercised end to end through the CLI
/// client: one of every unit type rustemd supports, pulled in together by a
/// `.target` exactly as the interactive initramfs wires them. Covers .service,
/// .timer, .socket, .mount, and .target. The .mount portion self-skips when
/// not run as root (mount(2) needs CAP_SYS_ADMIN) — run under
/// `unshare -m -U -r --map-root-user` to exercise it, as `mount_unit_lifecycle`
/// does.
#[cfg(all(target_os = "linux", feature = "socket"))]
#[test]
fn live_demo_units_lifecycle() {
    use std::os::unix::net::UnixStream;

    let scratch = Scratch::new();
    let root = scratch.dir.path().to_path_buf();
    let tick = root.join("demo.ticks");
    let tick_s = tick.to_string_lossy().to_string();
    let sock = root.join("demo.sock");
    let sock_s = sock.to_string_lossy().to_string();
    let mnt = root.join("mnt-demo");
    std::fs::create_dir_all(&mnt).unwrap();
    let mnt_s = mnt.to_string_lossy().to_string();

    scratch.write_unit(
        "demo.service",
        "[Unit]\nDescription=Demo service\n[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/true\n",
    );
    scratch.write_unit(
        "demo-tick.service",
        &format!("[Unit]\nDescription=Demo tick\n[Service]\nType=oneshot\nExecStart=/bin/sh -c 'echo tick >> {tick_s}'\n"),
    );
    scratch.write_unit(
        "demo.timer",
        "[Unit]\nDescription=Demo timer\n[Timer]\nOnBootSec=2s\nUnit=demo-tick.service\n",
    );
    scratch.write_unit(
        "demo.socket",
        &format!("[Unit]\nDescription=Demo socket\n[Socket]\nListenStream={sock_s}\nService=demo-echo.service\n"),
    );
    scratch.write_unit(
        "demo-echo.service",
        "[Unit]\nDescription=Demo echo service\n[Service]\nType=simple\nExecStart=/bin/sleep 30\n",
    );
    scratch.write_unit(
        "demo.mount",
        &format!("[Unit]\nDescription=Demo mount\n[Mount]\nWhat=tmpfs\nWhere={mnt_s}\nType=tmpfs\nOptions=mode=1777\n"),
    );
    scratch.write_unit(
        "demo.target",
        "[Unit]\nDescription=Demo target\nWants=demo.service demo.timer demo.socket demo.mount\nAfter=demo.service demo.timer demo.socket demo.mount\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();

    // One `start demo.target` pulls in every demo unit via Wants=.
    ctl.start(&["demo.target"]).unwrap();
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.is_active(&["demo.target"])
            .map(|v| v == vec!["active"])
            .unwrap_or(false)
    }));

    // .service: oneshot + RemainAfterExit=yes parks in active(exited).
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.status(&["demo.service"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "active" && s.sub == "exited")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }));

    // .timer: armed and active.
    assert_eq!(ctl.is_active(&["demo.timer"]).unwrap(), vec!["active"]);

    // .socket: active(listening), and its service is NOT yet started.
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.status(&["demo.socket"])
            .map(|v| v.first().map(|s| s.active == "active").unwrap_or(false))
            .unwrap_or(false)
    }));
    assert_eq!(
        ctl.is_active(&["demo-echo.service"]).unwrap(),
        vec!["inactive"]
    );

    // .timer fires demo-tick.service (OnUnitInactiveSec=1s): the tick marker
    // proves the target actually ran on a timer elapse.
    assert!(
        wait_for(Duration::from_secs(5), || tick.exists()),
        "timer should fire demo-tick.service, which writes the marker"
    );
    let last_set = wait_for(Duration::from_secs(3), || {
        ctl.list_timers()
            .map(|v| v.iter().any(|t| t.unit == "demo.timer" && t.last.is_some()))
            .unwrap_or(false)
    });
    assert!(
        last_set,
        "list_timers should record demo.timer's last elapse"
    );

    // .socket: a connection activates demo-echo.service on demand.
    let _conn = UnixStream::connect(&sock).unwrap();
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.status(&["demo-echo.service"])
            .map(|v| v.first().map(|s| s.active == "active").unwrap_or(false))
            .unwrap_or(false)
    }));

    // .mount: mount(2) needs CAP_SYS_ADMIN, so self-skip when not root.
    if euid() != 0 {
        eprintln!("skipping live_demo_units_lifecycle .mount: mount(2) requires root");
        return;
    }
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.status(&["demo.mount"])
            .map(|v| {
                v.first()
                    .map(|s| s.active == "active" && s.sub == "mounted")
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }));
    assert!(is_mounted(&mnt), "tmpfs should be mounted at {mnt_s}");
    ctl.stop(&["demo.mount"]).unwrap();
    assert!(wait_for(Duration::from_secs(3), || {
        ctl.is_active(&["demo.mount"])
            .map(|v| v == vec!["inactive"])
            .unwrap_or(false)
    }));
    assert!(!is_mounted(&mnt), "tmpfs should be unmounted after stop");
}

/// `.device` units are runtime-generated by udev enumeration — there is no
/// unit file. The test daemon calls `udev_init()` (like the real one), so
/// `list-units` should surface `.device` entries after enumeration. Skips
/// quietly in sandboxes without a mounted sysfs.
#[cfg(all(target_os = "linux", feature = "udev"))]
#[test]
fn device_units_appear_after_enumeration() {
    if !std::path::Path::new("/sys/devices").is_dir() {
        eprintln!("skipping device_units_appear_after_enumeration: /sys/devices not present");
        return;
    }
    // Scratch sets the RUSTEMD_* env vars (and holds the env lock) that the
    // daemon thread reads; we don't need to write any unit files.
    let _scratch = Scratch::new();
    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let ctl = daemon.client();
    let found = wait_for(Duration::from_secs(3), || {
        ctl.list_units(&[], None)
            .map(|v| v.iter().any(|u| u.unit.ends_with(".device")))
            .unwrap_or(false)
    });
    assert!(
        found,
        "udev enumeration should register .device units in list-units"
    );
}

#[test]
fn journal_persists_service_output_and_reads_over_ipc() {
    let s = Scratch::new();
    s.write_unit(
        "j.service",
        "[Service]\nType=oneshot\nRemainAfterExit=yes\nExecStart=/bin/echo journal-marker\n",
    );
    let d = Daemon::start();
    let mut c = d.client();
    assert!(wait_for(Duration::from_secs(5), || c
        .list_units(&[], None)
        .is_ok()));
    c.start(&["j.service"]).unwrap();
    assert!(wait_for(Duration::from_secs(5), || c
        .status(&["j.service"])
        .map(|v| v.first().is_some_and(|x| x.active == "active"))
        .unwrap_or(false)));

    // The durable store is on disk under the isolated journal dir.
    assert!(
        s.journal().join("j.service").exists(),
        "journal file should exist on disk"
    );

    // And it's readable over the IPC journal op.
    let (records, dir) = c.journal(Some("j.service"), None, None).unwrap();
    assert_eq!(dir, s.journal().display().to_string());
    assert!(
        records.iter().any(|r| r.text.contains("journal-marker")),
        "journal should contain the service's stdout line"
    );
}

#[test]
fn failing_condition_skips_unit_but_not_dependents() {
    let scratch = Scratch::new();
    // The path genuinely does not exist, so `ConditionPathExists` is
    // unsatisfied and cond-svc.service must be *skipped* (left inactive and
    // not failed, per systemd's skip-vs-fail semantics).
    scratch.write_unit(
        "cond-svc.service",
        "[Unit]\n\
         Description=conditionally skipped service\n\
         ConditionPathExists=/nonexistent/rustemd-e2e-cond-skip\n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart=/bin/true\n",
    );
    // The target `Wants=`+`After=` the skipped service. Even though the
    // dependency is skipped, its start job is treated as satisfied, so the
    // target must still activate (the condition must not block dependents).
    scratch.write_unit(
        "cond-parent.target",
        "[Unit]\n\
         Description=condition dependent target\n\
         Wants=cond-svc.service\n\
         After=cond-svc.service\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();

    ctl.start(&["cond-parent.target"]).unwrap();

    // The dependent target activates...
    let target_active = wait_for(Duration::from_secs(3), || {
        ctl.status(&["cond-parent.target"])
            .map(|v| v.first().is_some_and(|s| s.active == "active"))
            .unwrap_or(false)
    });
    assert!(
        target_active,
        "target should still activate despite the skipped Wants= dependency"
    );

    // ...while the skipped service stays inactive and is NOT marked failed.
    let st = wait_for(Duration::from_secs(3), || {
        ctl.status(&["cond-svc.service"])
            .map(|v| v.first().is_some_and(|s| s.active == "inactive"))
            .unwrap_or(false)
    });
    assert!(
        st,
        "condition-skipped service should remain inactive (skipped, not failed)"
    );
    let s = &ctl.status(&["cond-svc.service"]).unwrap()[0];
    assert_ne!(
        s.active, "failed",
        "a skipped condition must not fail the unit"
    );
}

#[test]
fn failing_assert_fails_unit() {
    let scratch = Scratch::new();
    // `Assert*` is a hard gate: when it is unsatisfied the unit's start job
    // fails and the unit is marked `failed` (unlike a plain condition, which
    // skips).
    scratch.write_unit(
        "assert-fail.service",
        "[Unit]\n\
         Description=assert gated service\n\
         AssertPathExists=/nonexistent/rustemd-e2e-assert-fail\n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart=/bin/true\n",
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();

    ctl.start(&["assert-fail.service"]).unwrap();
    let failed = wait_for(Duration::from_secs(3), || {
        ctl.status(&["assert-fail.service"])
            .map(|v| v.first().is_some_and(|s| s.active == "failed"))
            .unwrap_or(false)
    });
    assert!(
        failed,
        "an unsatisfied Assert* must fail the unit's start job (state failed)"
    );
}
