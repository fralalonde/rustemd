//! End-to-end coverage of `SystemCallFilter=` (seccomp) enforcement over the
//! real manager — the gap KNOWN_ISSUES.md flags ("no e2e coverage for sandbox
//! isolation"). Unlike the privileged sandbox suite this needs **no root**:
//! a `SECCOMP_MODE_FILTER` is installed in the forked child via
//! `no_new_privs`, which the manager now forces implicitly for such units
//! (matching systemd, where `SystemCallFilter=` implies `NoNewPrivileges=`).
//! A container that blocks `prctl(PR_SET_SECCOMP)` cannot run this at all, so
//! we self-skip when an always-allow filter cannot be installed — the same
//! principle the mount-op sandbox tests use.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Daemon, Scratch, wait_for};
use rystemd::control::{Control, SocketClient};

/// `PR_SET_SECCOMP` is not exported by libc for Linux (see `sandbox.rs`).
const PR_SET_SECCOMP: libc::c_int = 22;

/// Can this environment actually install a `SECCOMP_MODE_FILTER`? This is the
/// case on a normal (unprivileged) machine after `no_new_privs`, but denied
/// (`EPERM`) inside a container that restricts `prctl(PR_SET_SECCOMP)` in its
/// own seccomp profile. Forked so the probe's no_new_privs/filter state never
/// leaks back into the test process.
fn seccomp_installable() -> bool {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return false;
    }
    if pid == 0 {
        // Child: set no_new_privs, then try to install an always-ALLOW filter.
        // SAFETY: prctl with valid, constant args.
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
        let f = libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        };
        let mut fprog = libc::sock_fprog {
            len: 1,
            filter: (&f as *const libc::sock_filter).cast_mut(),
        };
        // SAFETY: fprog describes a valid one-instruction filter; prctl copies
        // it into the kernel before returning.
        let ok = unsafe {
            libc::prctl(PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &mut fprog, 0, 0) == 0
        };
        unsafe { libc::_exit(if ok { 0 } else { 1 }) };
    }
    let mut st: libc::c_int = 0;
    unsafe { libc::waitpid(pid, &mut st, 0) };
    libc::WIFEXITED(st) && libc::WEXITSTATUS(st) == 0
}

/// Start the manager and wait for its control socket, returning the client.
fn start_daemon() -> (Daemon, SocketClient) {
    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        Path::new(&daemon.socket).exists()
    }));
    let ctl = daemon.client();
    (daemon, ctl)
}

/// Start a oneshot service and wait for it to reach `inactive`.
fn run_oneshot(ctl: &mut SocketClient, name: &str) {
    ctl.start(&[name])
        .unwrap_or_else(|e| panic!("start {name}: {e}"));
    assert!(
        wait_for(Duration::from_secs(5), || ctl
            .status(&[name])
            .ok()
            .and_then(|v| v.first().map(|s| s.active == "inactive"))
            .unwrap_or(false)),
        "{name} should reach inactive; status: {:?}",
        ctl.status(&[name])
    );
}

/// `SystemCallFilter=~mkdir` (a deny-list) really blocks the `mkdir` syscall.
/// Each unit runs an identical shell command (fresh target + marker per unit)
/// that records `mkdir`'s exit code; the unfiltered control writes `0`, the
/// filtered unit writes a non-zero code because seccomp makes the `mkdir`
/// syscall return EPERM and the `mkdir` binary exits non-zero. The manager
/// applies the filter under a plain user-mode manager because
/// `SystemCallFilter=` now implies `NoNewPrivileges=` — without that fix the
/// unit would refuse to spawn (EACCES).
#[test]
fn systemcallfilter_denylist_blocks_syscall() {
    if !seccomp_installable() {
        eprintln!(
            "skipping systemcallfilter_denylist_blocks_syscall: \
             seccomp filter install is restricted in this environment"
        );
        return;
    }

    let scratch = Scratch::new();

    // Two units, one filtered and one not, writing to independent markers so
    // neither run can interfere with the other.
    let mut units = Vec::new();
    for (i, name) in ["mkdir-allow.service", "mkdir-deny.service"]
        .iter()
        .enumerate()
    {
        let marker = scratch.dir.path().join(format!("marker-{i}"));
        let target = scratch.dir.path().join(format!("probe-dir-{i}"));
        let cmd = format!(
            "rm -f {m}; mkdir {t}; echo $? > {m}",
            m = marker.display(),
            t = target.display()
        );
        let sandbox = if i == 0 {
            ""
        } else {
            "SystemCallFilter=~mkdir mkdirat\n"
        };
        scratch.write_unit(
            name,
            &format!("[Service]\nType=oneshot\n{sandbox}ExecStart=/bin/sh -c '{cmd}'\n"),
        );
        units.push((i, name.to_string(), marker));
    }

    let (_daemon, mut ctl) = start_daemon();

    for (i, name, marker) in units {
        run_oneshot(&mut ctl, &name);
        let code: i32 = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{name}: no marker written: {e}"))
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}: unparsable marker"));
        if i == 0 {
            assert_eq!(code, 0, "unfiltered mkdir should succeed (got {code})");
        } else {
            assert_ne!(
                code, 0,
                "SystemCallFilter=~mkdir should block mkdir (got {code})"
            );
        }
    }
}

/// `RestrictRealtime=yes` blocks the realtime-scheduler syscalls. We probe
/// with Python's `os.sched_setscheduler` — a *privilege-free* call to
/// `sched_setscheduler(0, SCHED_OTHER, prio 0)` succeeds for an unprivileged
/// process (verified), while under `RestrictRealtime=` the seccomp filter
/// makes it fail with `EPERM`, so the interpreted `os.sched_setscheduler` call
/// raises and `python3` exits non-zero. Like the `SystemCallFilter=` case,
/// this needs no root: the manager forces `NoNewPrivileges=` and installs the
/// filter in the forked child; it still self-skips if the container forbids
/// `prctl(PR_SET_SECCOMP)` altogether.
#[test]
fn restrict_realtime_denies_sched_setscheduler() {
    if !seccomp_installable() {
        eprintln!(
            "skipping restrict_realtime_denies_sched_setscheduler: \
             seccomp filter install is restricted in this environment"
        );
        return;
    }

    let scratch = Scratch::new();

    // Two unit runs: an unrestricted control and a `RestrictRealtime=yes`
    // unit, each recording the probe's exit code to its own marker.
    let mut units = Vec::new();
    for (i, name) in ["rr-allow.service", "rr-deny.service"].iter().enumerate() {
        let marker = scratch.dir.path().join(format!("marker-{i}"));
        let cmd = format!(
            "/usr/bin/python3 -c \"import os;os.sched_setscheduler(0,os.SCHED_OTHER,os.sched_param(0))\"; echo $? > {m}",
            m = marker.display()
        );
        let sandbox = if i == 0 { "" } else { "RestrictRealtime=yes\n" };
        scratch.write_unit(
            name,
            &format!("[Service]\nType=oneshot\n{sandbox}ExecStart=/bin/sh -c '{cmd}'\n"),
        );
        units.push((i, name.to_string(), marker));
    }

    let (_daemon, mut ctl) = start_daemon();

    for (i, name, marker) in units {
        run_oneshot(&mut ctl, &name);
        let code: i32 = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{name}: no marker written: {e}"))
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}: unparsable marker"));
        if i == 0 {
            assert_eq!(
                code, 0,
                "unrestricted sched_setscheduler should succeed (got {code})"
            );
        } else {
            assert_ne!(
                code, 0,
                "RestrictRealtime=yes should block sched_setscheduler (got {code})"
            );
        }
    }
}

/// `LockPersonality=yes` blocks the `personality(2)` syscall. We probe with
/// Python's `ctypes` to call libc's `personality(0)` directly — a
/// *privilege-free* no-change query that returns the current persona (`0` for
/// `PER_LINUX`) for an unprivileged process already on that persona, without
/// an argument-dependent error. (The higher-level `os.personality` is used
/// elsewhere, but was removed in Python 3.13, so `ctypes` keeps this probe
/// version-proof.) Under `LockPersonality=` the seccomp filter denies
/// `personality` with `EPERM`, so `ctypes` returns `-1`, the probe exits
/// non-zero, and `python3` records a non-zero marker. Needs no root (the
/// manager forces `NoNewPrivileges=` and installs the filter in the forked
/// child); self-skips where a container forbids `prctl(PR_SET_SECCOMP)`.
#[test]
fn lock_personality_blocks_personality() {
    if !seccomp_installable() {
        eprintln!(
            "skipping lock_personality_blocks_personality: \
             seccomp filter install is restricted in this environment"
        );
        return;
    }

    let scratch = Scratch::new();

    // Two unit runs: an unrestricted control and a `LockPersonality=yes` unit,
    // each recording the probe's exit code to its own marker.
    let mut units = Vec::new();
    for (i, name) in ["lp-allow.service", "lp-deny.service"].iter().enumerate() {
        let marker = scratch.dir.path().join(format!("marker-{i}"));
        let cmd = format!(
            "/usr/bin/python3 -c \"import ctypes,sys;sys.exit(0 if ctypes.CDLL(None).personality(0) >= 0 else 1)\"; echo $? > {m}",
            m = marker.display()
        );
        let sandbox = if i == 0 { "" } else { "LockPersonality=yes\n" };
        scratch.write_unit(
            name,
            &format!("[Service]\nType=oneshot\n{sandbox}ExecStart=/bin/sh -c '{cmd}'\n"),
        );
        units.push((i, name.to_string(), marker));
    }

    let (_daemon, mut ctl) = start_daemon();

    for (i, name, marker) in units {
        run_oneshot(&mut ctl, &name);
        let code: i32 = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{name}: no marker written: {e}"))
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}: unparsable marker"));
        if i == 0 {
            assert_eq!(
                code, 0,
                "unrestricted personality query should succeed (got {code})"
            );
        } else {
            assert_ne!(
                code, 0,
                "LockPersonality=yes should block personality(2) (got {code})"
            );
        }
    }
}

/// `RestrictSUIDSGID=yes` blocks the file-mode syscalls that could set an
/// SUID/SGID bit. We probe with the `chmod` binary trying to set SGID on a
/// scratch file (`chmod 6755`): without the restriction it succeeds (exit 0),
/// while under `RestrictSUIDSGID=yes` the seccomp filter makes `chmod(2)`
/// (and any `fchmodat`/`chmodat` fallback coreutils uses) return `EPERM`, so
/// `/usr/bin/chmod` exits non-zero and the marker records it. The coreutils
/// `chmod` is used in preference to a Python `os.chmod` probe because the
/// interpreter's own startup path is free of chmod — the probe is exactly the
/// syscall under test. Needs no root (the manager forces `NoNewPrivileges=`
/// and installs the filter in the forked child); self-skips where a container
/// forbids `prctl(PR_SET_SECCOMP)`.
#[test]
fn restrict_suidsgid_blocks_chmod() {
    if !seccomp_installable() {
        eprintln!(
            "skipping restrict_suidsgid_blocks_chmod: \
             seccomp filter install is restricted in this environment"
        );
        return;
    }

    let scratch = Scratch::new();

    // Two unit runs: an unrestricted control and a `RestrictSUIDSGID=yes`
    // unit, each recording the probe's exit code to its own marker.
    let mut units = Vec::new();
    for (i, name) in ["sg-allow.service", "sg-deny.service"].iter().enumerate() {
        let marker = scratch.dir.path().join(format!("marker-{i}"));
        let target = scratch.dir.path().join(format!("probe-file-{i}"));
        // `touch` the target (create is fine), then try to set SUID+SGID.
        let cmd = format!(
            "rm -f {t} {m}; : > {t}; /usr/bin/chmod 6755 {t} 2>/dev/null; echo $? > {m}",
            t = target.display(),
            m = marker.display()
        );
        let sandbox = if i == 0 { "" } else { "RestrictSUIDSGID=yes\n" };
        scratch.write_unit(
            name,
            &format!("[Service]\nType=oneshot\n{sandbox}ExecStart=/bin/sh -c '{cmd}'\n"),
        );
        units.push((i, name.to_string(), marker));
    }

    let (_daemon, mut ctl) = start_daemon();

    for (i, name, marker) in units {
        run_oneshot(&mut ctl, &name);
        let code: i32 = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{name}: no marker written: {e}"))
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}: unparsable marker"));
        if i == 0 {
            assert_eq!(
                code, 0,
                "unrestricted chmod 6755 should succeed (got {code})"
            );
        } else {
            assert_ne!(
                code, 0,
                "RestrictSUIDSGID=yes should block chmod (got {code})"
            );
        }
    }
}

/// `RestrictAddressFamilies=AF_UNIX` (an allow-list of a single family) blocks
/// creation of sockets in any other family. We probe with Python opening an
/// `AF_INET`/`SOCK_STREAM` socket — a *privilege-free* call that succeeds for
/// an unprivileged process — while under the directive the seccomp family gate
/// makes the `socket(2)` syscall return `EPERM`, the interpreter raises, and
/// `python3` exits non-zero. The probe needs no root (the manager forces
/// `NoNewPrivileges=` and installs the filter in the forked child); it
/// self-skips where a container forbids `prctl(PR_SET_SECCOMP)` altogether.
#[test]
fn restrict_address_families_allows_only_unix() {
    if !seccomp_installable() {
        eprintln!(
            "skipping restrict_address_families_allows_only_unix: \
             seccomp filter install is restricted in this environment"
        );
        return;
    }

    let scratch = Scratch::new();

    // Two unit runs: an unrestricted control and a `RestrictAddressFamilies=`
    // unit, each recording the probe's exit code to its own marker. The probe
    // opens an AF_INET stream socket; importing python's `socket` module
    // creates no sockets of its own.
    let mut units = Vec::new();
    for (i, name) in ["af-allow.service", "af-deny.service"].iter().enumerate() {
        let marker = scratch.dir.path().join(format!("marker-{i}"));
        let cmd = format!(
            "/usr/bin/python3 -c \"import socket;socket.socket(socket.AF_INET, \
             socket.SOCK_STREAM)\"; echo $? > {m}",
            m = marker.display()
        );
        let sandbox = if i == 0 {
            ""
        } else {
            "RestrictAddressFamilies=AF_UNIX\n"
        };
        scratch.write_unit(
            name,
            &format!("[Service]\nType=oneshot\n{sandbox}ExecStart=/bin/sh -c '{cmd}'\n"),
        );
        units.push((i, name.to_string(), marker));
    }

    let (_daemon, mut ctl) = start_daemon();

    for (i, name, marker) in units {
        run_oneshot(&mut ctl, &name);
        let code: i32 = std::fs::read_to_string(&marker)
            .unwrap_or_else(|e| panic!("{name}: no marker written: {e}"))
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}: unparsable marker"));
        if i == 0 {
            assert_eq!(
                code, 0,
                "unrestricted AF_INET socket should be created (got {code})"
            );
        } else {
            assert_ne!(
                code, 0,
                "RestrictAddressFamilies=AF_UNIX should block AF_INET sockets (got {code})"
            );
        }
    }
}
