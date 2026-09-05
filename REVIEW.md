# Engineering review

Review started: 2026-09-04.

## Decision under review

Suitability as the init system and service manager for a minimal modern desktop
Linux distribution.

Acceptance bar is not compilation or feature count. The manager must be as
trustworthy as established init systems within its claimed scope. Evidence must
cover failure handling, security boundaries, bounded resource use, portable OS
API use, recovery, and repeatable tests.

## Review rules

- Findings need a source location, test result, reproducer, or external
  reference.
- Severity: critical, high, medium, low, note.
- Small fixes may land during review after a failing regression test.
- Larger changes need stated acceptance criteria and stay open here.
- Linux PID 1 and Windows service-manager claims are evaluated separately.
- Unsupported behavior must fail explicitly or be documented.
- Release history remains in Git and `CHANGELOG.md`.

## Verdict

Not ready to be the PID 1 base of a general desktop distribution. The core
supervisor and event loop have become materially safer this session, but four
trust boundaries remain open. Suitable today as a controlled VM and
initramfs experiment base and as a user-level manager.

## Evidence

- `cargo test --workspace --all-features --locked` passes (201 tests).
- One full-feature run exposed a large-response truncation regression from an
  earlier one-shot write; fixed by poll-driven response delivery and re-run.
- clippy `-D warnings`, rustfmt, MSRV 1.89 build/test, Windows
  `x86_64-pc-windows-msvc` check: pass.
- macOS check fails (Linux-only cfg in process/boot/sandbox, setgroups,
  signalfd, netlink, prctl). macOS stays documented unsupported.
- `cargo audit`: no known vulnerabilities in the locked graph (190 crates).
- No git or license-missing dependencies in the lockfile.
- Release binaries: rystemd 2.2 MB, rystemctl 0.8 MB, rystemd-tui 0.8 MB.

## Applied fixes (each had a failing test on the pre-fix code)

### Control IPC no longer stalls the event loop (critical, verified)

`rystemd/src/manager/mod.rs` `handle_connection` did a blocking, unbounded
`read_line()` inline on the PID 1 loop thread. One client that connected and
sent nothing permanently stalled reaping, timers, shutdown, and all other
clients.

Fix: accepted sockets are non-blocking pending clients polled in the event
loop. One request is read per connection with a 16 KiB bound; the response is
held and flushed by `POLLOUT`, so a slow or non-reading peer cannot block the
loop. Concurrent clients are capped at 1024.

Regression: `incomplete_control_request_does_not_block_other_clients` (red
then green). A caught secondary regression: `list-units` responses exceed the
unix socket buffer; the poll-driven flush fixed truncated JSON
(`cli_drives_daemon_roundtrip`).

### Control socket is owner-only and peer-gated (critical, verified)

`bind_control` relied on the process umask for the socket mode (`022` gave a
`0755` socket), and accepts never checked who connected. Both are now closed.

Fix: `bind_control` sets the socket mode `0600` explicitly, independent of
umask, so an unprivileged connect attempt is denied at the socket layer. On
accept the manager reads `SO_PEERCRED` and drops any peer whose UID is not the
manager's own (`cfg.uid`); the mode is defense in depth behind that gate. A
system manager (root) accepts only root; a user manager accepts only its
owner.

Regression: `control_socket_mode_is_independent_of_umask` (red: `0777` under
umask 000, then `0600`), `peer_uid_reports_the_connecting_process_identity`.
Cross-UID rejection is by the `uid != cfg.uid` rule plus inspection; a
setuid-child test needs root and is not runnable in the unprivileged local
environment.

### Invalid `User=` / `Group=` fails the start before spawn (critical, verified)

`rystemd/src/manager/mod.rs:1782` resolved the user and group with
`Option`; lookup failure fell back to the manager identity and ran the unit as
root. Now an unresolved user (result `UnitResult::User`) or group
(`UnitResult::Group`) fails the start job before fork; `ExecStart` never runs.

Regression: `unresolved_service_identity_fails_before_exec`,
`unresolved_service_group_fails_before_exec` (red then green).

### Unit-name path traversal closed (high, verified)

`Paths::find_unit` and the journal read/append/exists joined raw unit names
into paths. `../` escaped the unit and journal directories.

Fix: shared `is_plain_unit_name` primitive in `rystemd/src/names.rs`; enforced
in `find_unit`, `Journal::{read,append,exists}`, and consolidated the
repository validation in `rystemd/src/repo/mod.rs`.

Regression: `unit_lookup_rejects_path_traversal`,
`journal::{read,append,exists}_rejects_unit_path_traversal`,
`plain_unit_names_cannot_escape_their_directory` (red then green).

## Open findings (blockers unless closed)

### Critical: D-Bus control methods carry no authorization

`rystemd/src/dbus.rs` `StartUnit`, `StopUnit`, identity methods forward to the
manager without sender-credential checks or policy. A process that can reach
the system bus can start root services, stop units, load units, and query
process data.

Requirement: authorize every mutating method from the sender credentials with a
documented policy; restrict read methods. Regression: unprivileged caller gets
an authorization error.

### High: boot and early setup are fail-open

`rystemd/src/daemon.rs` and `rystemd/src/platform/boot.rs` log-and-continue
when API filesystem mounts (`/proc`, `/dev`, `/run`, `/sys/fs/cgroup`) or
early boot steps fail. A real PID 1 can proceed without them.

Requirement: distinguish recovery-capable failures from hard ones. A missing
`/proc` or `/dev` must enter a bounded emergency/rescue state, not nominal
boot. Rootless user-manager boot stays best-effort.

### High: `setenv` in the fork-to-exec hook is not async-signal-safe

`rystemd/src/platform/process.rs` sets `LISTEN_FDS` / `LISTEN_PID` with
`libc::setenv` inside a `pre_exec` closure. After `fork` in a process with
other threads this can deadlock the child on libc internals.

Requirement: pass `LISTEN_FDS` through `Command::env`; carry the child's PID
another way (e.g. an exec wrapper). Add a multithreaded fork stress test.

### High: failed signal-source setup leaves signals blocked with no consumer

`rystemd/src/platform/signals.rs` blocks signals before the signalfd is
created; if creation fails, `setup_signals` stores `None`, so SIGTERM/SIGINT/
SIGHUP are blocked and never consumed.

Requirement: on signalfd creation failure, either restore the signal masks or
install fallback handlers; never leave the signals blocked.

### Medium: start/stop timeout entries keyed only by unit

A stale deadline for one activation can affect a later one. Key deadlines by
job/activation generation. Still open.

### Medium: Windows control pipe has no explicit security descriptor

`rystemd/src/platform/windows/net.rs` uses `lpSecurityAttributes = NULL`, so
the pipe inherits the default DACL and dispatches mutating operations without
caller checks or impersonation. Requirement: explicit SYSTEM/Administrators
ACL for the system pipe, owning-user ACL for the user pipe, and per-caller
authorization. Windows runtime tests still run zero tests on Linux.

### Note: bare dependency names are not a defect

A fork flagged `Requires=db` with a `db.service` present as misresolution.
`systemd-analyze verify` rejects a unit dependency without a type suffix, so
rystemd agreeing that untyped names do not resolve matches systemd. Not a
finding; a future release may mimic systemd's implicit-`.service` appending.

### Note: static musl and glibc NSS

Static musl binaries cannot load glibc NSS modules (SSSD, LDAP, NIS). Static
musl stays canonical for initramfs and PID 1; GNU artifacts remain for native
Fedora/Debian identity integration.

## Capability status

Implemented and tested: job transactions (generic and irreversible
replacement, cancellation results, nested ExecStop restarts), client-side
waits, invocation IDs, restart metadata, socket activation, seccomp and
capability bounding rules, timer and calendar basics, deterministic APK
packaging, GNU plus musl release lanes.

Untested or partial for a desktop: logind/session, user managers at scale,
desktop D-Bus activation, suspend/resume/hibernate, power buttons, udev-driven
device lifecycle beyond the monitor, emergency/recovery targets, fsck/crypt/
mounts/automount/swap, graphical display-manager boot, SELinux enforcement.

## Required gates for distribution use

1. Native control authorization with unprivileged-denial regression. Closed:
   socket owner-only `0600` and peer-UID gate. D-Bus authorization remains
   open.
2. Boot failure policy: missing or unmountable API filesystems enter a
   bounded recovery state, not nominal boot.
3. Signal-setup and pre-exec async-signal-safety fixes with stress tests.
4. Reboot/poweroff reliability and watchdog/readiness under unit stress.
5. Recovering from a hung or non-reading control client stays non-blocking;
   concurrent-control-client cap exercised.
6. Windows control pipe explicit ACL and per-caller authorization.
7. Fuzz or property coverage for unit parsing, timespans/calendars, JSON/IPC,
   and seccomp directives.
8. Real-root desktop boot evidence (display manager, session, suspend/resume,
   shutdown) in an enforcing SELinux environment.

## Next

- Close control-socket ownership and peer check with a regression.
- Add D-Bus authorization.
- Convert boot failures to a bounded emergency state.
- Fix signal setup and pre-exec safety.
- Windows control pipe explicit ACL and per-caller authorization.
- Fuzz/property coverage and boot recovery evidence.