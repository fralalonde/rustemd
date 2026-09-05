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
- Only global objectives (verdict, blockers, gates) carry between review runs;
  per-run detail stays in the run's own history and is not re-derived.
- A weakness that recurs across runs gets a permanent entry under
  `Recurring weaknesses`, with the surfaces it has hit and their status.

## Verdict

Not yet the PID 1 base of a general desktop distribution. This run closed the
Linux authorization surfaces (native socket, D-Bus), boot fail-open for real
PID 1, and pre-exec/signal async-signal-safety. Remaining trust work:
recovery-mode polish, resource/performance stress, fuzz coverage, and the
Windows control pipe (needs a Windows runner). Suitable today as a controlled
VM and initramfs experiment base and as a user-level manager.

## Evidence

- `cargo test --workspace --all-features --locked` passes (205+ tests; exact
  count depends on feature set — `dbus` live test self-skips when the host has
  no `dbus-daemon`).
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

### D-Bus mutating methods require the caller's bus identity (critical, verified)

`StartUnit` and `StopUnit` forwarded to the manager with no sender check; any
process able to reach the system bus could start a root unit. The systemd1
surface here is read/load-only (no start/stop) and carries no start/stop jacks,
so the native `org.rystemd.Manager1` surface was the escalation point.

Fix: both mutators now read the caller's `UnixUserID` from the bus
(`GetConnectionCredentials`) and require it to be the manager's own UID, or
root. Identity comes from the bus, never from the request body. `manager_uid`
is threaded from `ManagerCfg` into the D-Bus interface.

Regression: `mutating_calls_are_limited_to_the_manager_owner_or_root`
(pure policy). A live same-UID `StartUnit` over a private bus is asserted in
`tests/dbus.rs`; it self-skips when `dbus-daemon` is absent, so it runs in CI
but not on this host. Cross-UID denial is by the `uid_allowed` rule plus
inspection (setuid needs root).

### Real PID 1 refuses to boot without /proc and /dev (high, verified)

`daemon.rs` logged mount failures and carried on. `mount_api_filesystems` stays
best-effort for unprivileged namespaces, but a real PID 1 now verifies the two
mounts supervision cannot do without — `/proc` (reaping, per-process
inspection) and `/dev` (every service's `/dev/null` stdin) — and aborts with a
loud message instead of starting units against a hollow system. `/run`, `/sys`,
`/tmp` stay tolerant.

Regression: `missing_pid1_api_mounts_are_high_signal`. A full interactive
emergency/recovery target remains an open gate.

### pre-exec env and signal setup are now async-signal-safe (high, verified)

`setenv()` is not async-signal-safe, yet `pre_exec` used it for `LISTEN_FDS`
and `LISTEN_PID`; a multithreaded fork could deadlock the child on libc locks.
`LISTEN_FDS` is now set via `Command::env` in the parent. `LISTEN_PID` (the
child's pid, known only between fork and exec) is written into a pre-seeded
fixed-width `environ` slot in place — async-signal-safe memory writes with no
allocation.

`SignalSource::new` now installs `SIGPIPE` first, and on signalfd-creation
failure unblocks the managed set before returning `None`, so SIGTERM/SIGINT/
SIGHUP are never left blocked with no consumer.

## Recurring weaknesses

### Fail-open authorization on every control surface

Mutating control is reachable by callers the manager has never authorized:

- Native unix socket: fixed. Owner-only `0600` plus a `SO_PEERCRED` UID gate
  in `accept_connections`.
- D-Bus `org.rystemd.Manager1`: closed. `StartUnit`/`StopUnit` check the
  caller's `UnixUserID` against the manager UID (or root).
- Windows control pipe: open. Created with a `NULL` security descriptor and no
  per-caller check; blocked on Windows runtime verification (contract below).

Check every future control surface against this pattern before adding it.

## Open findings (blockers unless closed)

### Medium: start/stop timeout entries keyed only by unit

A stale deadline for one activation can affect a later one. Key deadlines by
job/activation generation. Still open.

### Medium: Windows control pipe has no explicit security descriptor (blocked)

`rystemd/src/platform/windows/net.rs` uses `lpSecurityAttributes = NULL`, so
the pipe inherits the default DACL and dispatches mutating operations without
caller checks or impersonation.

Blocked on a Windows runtime/CI runner. This host cannot execute Windows
code, and a correct fix cannot be safely shipped untested: the policy is
SYSTEM + Administrators for the system pipe, the owning user for the user
pipe (a naive same-SID-as-server check would break the elevated admin CLI).
Contract once a runner exists: explicit per-mode DACL (or per-caller
`CheckTokenMembership` against the owning user / BUILTIN\\Administrators),
mutating ops authorized per caller, and Windows tests that connect as allowed
and denied SIDs and assert every mutating op. Not half-built here by design.

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

1. Native control and D-Bus authorization with unprivileged-denial. Closed:
   socket owner-only `0600`, peer-UID gate, and D-Bus `StartUnit`/`StopUnit`
   bus-identity check.
2. Boot failure policy: real PID 1 aborts without `/proc`/`/dev`. Open: a full
   interactive emergency/recovery target.
3. Signal-setup and pre-exec async-signal-safety. Closed in code; a
   multithreaded fork stress test is still worth adding.
4. Reboot/poweroff reliability and watchdog/readiness under unit stress.
5. Recovering from a hung or non-reading control client stays non-blocking;
   concurrent-control-client cap exercised.
6. Windows control pipe explicit ACL and per-caller authorization. Blocked on
   a Windows runner; contract specified.
7. Fuzz or property coverage for unit parsing, timespans/calendars, JSON/IPC,
   and seccomp directives.
8. Real-root desktop boot evidence (display manager, session, suspend/resume,
   shutdown) in an enforcing SELinux environment.

## Next

- Add a multithreaded fork stress test for the socket-activation env path.
- Implement and verify the Windows control-pipe ACL once a Windows runner
  exists (contract above).
- Fuzz/property coverage for unit parsing, timespans/calendars, JSON/IPC, and
  seccomp directives.
- Reboot/poweroff and watchdog/readiness under unit stress; real-root desktop
  boot evidence (display manager, session, suspend/resume, shutdown).