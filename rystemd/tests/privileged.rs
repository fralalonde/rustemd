//! Privilege-requiring integration tests: `User=`/`Group=` privilege dropping
//! and sandbox mount-namespace isolation. These need root (or an unprivileged
//! user+mount namespace via `unshare -m -U -r --map-root-user`), so each
//! self-skips when not run as root — the normal `cargo test` on an
//! unprivileged dev machine stays green, and CI runs this file under
//! `unshare`.
#![cfg(target_os = "linux")]

mod common;

use std::time::Duration;

use common::{Daemon, Scratch, wait_for};
use rystemd::control::Control;

/// The current effective uid (0 = root), read from `/proc/self/status`.
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

/// Can this process `setuid(65534)` (nobody)? Only if 65534 is mapped in the
/// current user namespace — true as real root (full `0 0 4294967295` map),
/// false under a single-uid `unshare -U -r` map (`0 <real-uid> 1`).
fn can_setuid_nobody() -> bool {
    std::fs::read_to_string("/proc/self/uid_map")
        .map(|s| {
            s.lines().any(|l| {
                let f: Vec<u64> = l
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                f.len() == 3 && 65534 >= f[0] && 65534 < f[0].saturating_add(f[2])
            })
        })
        .unwrap_or(false)
}

/// `User=nobody` must actually drop the service away from root: the spawned
/// process observes a non-zero uid. Exercises the `pre_exec` setgroups/setgid/
/// setuid path in `platform::process`, which only runs when the manager can
/// drop privileges (i.e. as real root).
#[test]
fn user_directive_drops_privileges() {
    if euid() != 0 {
        eprintln!("skipping user_directive_drops_privileges: needs root");
        return;
    }
    if !can_setuid_nobody() {
        eprintln!(
            "skipping user_directive_drops_privileges: nobody (65534) is not \
             mapped in this user namespace (run as real root, not unshare -U -r)"
        );
        return;
    }

    let scratch = Scratch::new();
    let marker = format!("/tmp/rystemd-uid-{}", std::process::id());
    scratch.write_unit(
        "drop.service",
        &format!("[Service]\nType=oneshot\nUser=nobody\nExecStart=/bin/sh -c 'id -u > {marker}'\n"),
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();
    ctl.start(&["drop.service"]).unwrap();

    assert!(
        wait_for(Duration::from_secs(5), || std::path::Path::new(&marker)
            .exists()),
        "the service should run and write its uid"
    );
    let uid: u32 = std::fs::read_to_string(&marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let _ = std::fs::remove_file(&marker);
    assert_ne!(uid, 0, "User=nobody should drop privileges away from root");
}

/// `PrivateTmp=yes` must give the service its own (empty) `/tmp` via a private
/// mount namespace: a file the service writes to `/tmp` is not visible in the
/// manager/test's real `/tmp`. Exercises the `unshare(CLONE_NEWNS)` +
/// `mount(tmpfs)` path in `platform::sandbox`, which needs `CAP_SYS_ADMIN`.
#[test]
fn private_tmp_is_isolated_from_real_tmp() {
    if euid() != 0 {
        eprintln!("skipping private_tmp_is_isolated_from_real_tmp: needs root");
        return;
    }

    let scratch = Scratch::new();
    // The sentinel is written into the service's *private* /tmp; PrivateTmp
    // must keep it out of the real /tmp the test observes.
    let sentinel = format!("rystemd-private-tmp-{}", std::process::id());
    scratch.write_unit(
        "sandboxed.service",
        &format!("[Service]\nType=oneshot\nPrivateTmp=yes\nExecStart=/bin/sh -c 'touch /tmp/{sentinel}'\n"),
    );

    let daemon = Daemon::start();
    assert!(wait_for(Duration::from_secs(3), || {
        std::path::Path::new(&daemon.socket).exists()
    }));
    let mut ctl = daemon.client();
    ctl.start(&["sandboxed.service"]).unwrap();

    // Wait for the oneshot to complete (active -> inactive). Completion is
    // observed over IPC because a marker in the scratch dir would itself sit
    // under /tmp and be shadowed by PrivateTmp.
    let completed = wait_for(Duration::from_secs(5), || {
        ctl.status(&["sandboxed.service"])
            .map(|v| v.first().map(|s| s.active == "inactive").unwrap_or(false))
            .unwrap_or(false)
    });
    assert!(completed, "the service should run to completion");

    assert!(
        !std::path::Path::new(&format!("/tmp/{sentinel}")).exists(),
        "PrivateTmp should isolate the service's /tmp from the real /tmp"
    );
}
