# Known issues

A categorized inventory for checkpointing and as the input to the upcoming
**security inspection**. Three buckets:

- **Bugs** — incorrect behavior (a unit does the wrong thing, or crashes).
- **Weaknesses** — incomplete or partial features; the happy path works but
  coverage is thin.
- **Design holes** — structural risks that need an architectural decision, not
  a point fix.

Updated with the Windows code-review pass.
Nothing here blocks the current dogfood scope; it is triage material.

## Bugs

### ~~CLI panics on a broken stdout pipe~~ — fixed (rustemctl)
`rustemctl list-units | head` (any command whose stdout pipe closes early) used
to panic instead of exiting cleanly. Rust's runtime ignores `SIGPIPE` by
default, so `println!` surfaced `EPIPE` and the CLI unwrapped it into a panic;
`systemctl` dies silently (the default `SIGPIPE` action). Fixed by restoring
the `SIGPIPE` default disposition (`libc::signal(SIGPIPE, SIG_DFL)`) early in
`rustemctl/src/main.rs`; the CLI now dies via `SIGPIPE` (exit 141) like
systemd. See also the design hole below — the daemon/IPC write paths still
make the same assumption.

### Timer re-arm fires units that were never started
`OnUnitActiveSec` / `OnUnitInactiveSec` / `OnCalendar` re-arm and fire even
when the timer's target unit is not in the started state, whereas systemd only
arms these once the unit is activated. Causes spurious firings. The demo works
around it with `OnBootSec`. Location: `manager/timer.rs` re-arm path.

### ~~Required dependency failing synchronously treated as satisfied~~ — fixed
`Requires=` on a dependency whose job fails *synchronously* (a `.mount` hitting
`EPERM`/`ENOENT`, a `.socket` bind failure) used to be dropped by the job
engine's `waiting.retain`, so the parent proceeded as if the dependency were
met. Fixed this checkpoint; regression test
`required_dependency_that_fails_synchronously_fails_parent` in
`rustemd/src/manager/mod.rs`.

### resume_primary_thread can leave a service stuck in START_PENDING
Location: `rustemd/src/platform/windows/process.rs:299-334`. The thread-resume
path walks a system-wide `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)`, resumes
only the FIRST thread whose owner pid matches, and breaks on the first
`ResumeThread` that does not return `u32::MAX`. If that thread's prior suspend
count is 0 (it was not actually suspended), `found=true` fires without resuming
the real suspended primary thread, so a `CREATE_SUSPENDED` newborn hangs forever
and its service sits in `START_PENDING`.

### ~~Braced `${VAR}` expansion left a trailing `}`~~ — fixed
The shared `expand_env_token` had an off-by-one (consumed `end + 2` instead of
`end + 3`) so `${HOME}` expanded to `/home/me}`. Found and fixed while
deduplicating the two platform copies into `rustemd/src/expand.rs` (with
tests); the Unix and Windows impls previously diverged here (Windows consumed
the brace correctly).

## Weaknesses

### D-Bus is not a `systemd1` drop-in
Only a rustemd-specific `org.fralalonde.rustemd1.Manager` interface
(`ListUnits`/`GetUnit`/`StartUnit`/`StopUnit`/`Version`). No per-unit object
graph, no `org.freedesktop.DBus.Properties`, no job objects or signals — real
D-Bus clients (logind, desktop integration, `systemctl --user` over the bus)
won't work. D-Bus is also opt-in (the `dbus` cargo feature, off by default):
builds without it omit the zbus dependency tree entirely.

### No journald
Service stdout/stderr go to the manager's in-memory capture, not a queryable,
rotating journal. No `journalctl`-compatible surface. (`StandardOutput=journal`
is parsed but backs onto that capture.)

### No service sandboxing — highest priority for the security inspection
`ProtectSystem=` / `ProtectHome=` / `PrivateTmp=` / `PrivateDevices=` /
`DynamicUser=` / `NoNewPrivileges=` / `CapabilityBoundingSet=` /
`SystemCallFilter=` (seccomp) / `ReadOnlyPaths=` / `DeviceAllow=` are all
unimplemented and silently ignored. A unit that asks for hardening gets none.

### Partial cgroup resource controls
Only `MemoryMax` / `MemoryHigh` / `CPUWeight` / `TasksMax`. Missing
`CPUQuota=`, `IOWeight=` / `IODeviceWeight=`, and block-device/`DevicePolicy=`
access control.

### `Type=notify` is partial
`NOTIFY_SOCKET` is set and `READY=1` honored, but the sd_notify watchdog and
`NotifyAccess=` enforcement are not wired.

### Windows socket activation is trigger-only
Windows `.socket` units support TCP launch-on-connection, but do not transfer
the listening Winsock handle to the child. Services that require systemd-style
`LISTEN_FDS` inheritance are not portable to the Windows MVP. Unix-domain
listeners are rejected. `take_trigger()` (`rustemd/src/manager/socket.rs:36-48`)
does `accept()` then `drop(stream)`, and the Windows `process::spawn` never
reads `opts.listen_fds`, so the triggering connection is accepted and closed
before the service sees it (no `WSADuplicateSocket`/fd-passing equivalent). The
two Windows socket tests pass only because their services never touch the
socket.

### Windows directive subset
The Win32 manager supports foreground and oneshot services. `Type=forking`,
`notify`, and `dbus`; account switching (`User=`/`Group=`); `MemoryHigh=`; and
`CPUWeight=` fail explicitly. Job Objects implement tree lifetime,
`MemoryMax=`, and `TasksMax=`. Windows stop signals terminate the Job Object
because Win32 has no generic POSIX-signal delivery API. `kill_group`
(`windows/process.rs:357-372`) calls `TerminateJobObject` for both `SIGTERM` and
`SIGKILL` — the 137/143 exit codes are bookkeeping only — so there is no
graceful-stop phase (no `GenerateConsoleCtrlEvent`/`CTRL_BREAK`) and no
SIGTERM-then-timeout-then-SIGKILL like the Linux path.

### Windows output capture can drop the tail on shutdown and has no backpressure
Location: `rustemd/src/platform/windows/process.rs:336-355`. `forward_output`
spawns detached reader threads that are never joined, feeding an unbounded mpsc
channel; `drain_output` does a non-blocking `try_iter` drain. If the manager
exits before a reader thread drains its pipe the final lines are lost (the
Linux path polls fixed-size pipes with owned fds and backpressure), and a
service that writes faster than the manager drains grows memory without bound.

### Spawned diverges by platform, leaking the platform seam
Windows `Spawned { pid }` (`windows/process.rs:57-59`) vs Unix
`Spawned { pid, stdout, stderr }` (`platform/process.rs:39-43`); the manager
carries `#[cfg]` branches for fd-polling vs `drain_output`.

### Declared MSRV (1.85) is wrong; code needs 1.88+
`rustemd/Cargo.toml` says `rust-version = "1.85"` but
`rustemd/src/platform/windows/net.rs` uses let-chains (`... && let ...`,
stabilized in Rust 1.88). Bumping `rust-version` to 1.88 is correct but
unmasks ~29 `collapsible_if` clippy lints (clippy gates the let-chain collapse
suggestion on `rust-version`) that would then need collapsing; defer bump +
`cargo clippy --fix` + retest to a later pass. Note: `rustemd-repo` sets
`rust-version = "1.89"` (it uses `std::fs::File::lock`), which transitively
raises the workspace floor to 1.89 regardless.

### Two windows-sys versions in the tree
0.61.2 (direct) and 0.59.0 (via crossterm/anstyle). Harmless.

### macOS build path unverified
The release workflow builds a macOS artifact, but macOS does not yet have a
supported manager platform implementation.

### Live VM has no standard shutdown commands
Inside the live env, `shutdown` / `halt` / `poweroff` / `init 0` don't exist
(rustemd *is* PID 1) and `exit` at the getty just respawns the shell
(`Restart=always`). The only clean shutdown is `rustemctl poweroff`. The
`live-vm.sh` banner documents it, but it's an easy trap.

## Design holes

### Job-engine synchronous-completion model is fragile
`expand_start_job`'s `expanding` flag plus the `waiting.retain` post-filter
(and the new `sync_failed` check) compensate for the fact that a dependency's
job can complete *during* the parent's expansion — before the parent's
`waiting` list is committed, so `on_job_completed` can't see it. Every unit
type that resolves synchronously (`.target`, `.mount`, `.socket` bind) has to
be reasoned about against this. A completion queue (deferring synchronous
completion until after expansion commits) would be a more robust model and is
worth revisiting before more unit types are added.

### SIGPIPE is unhandled across the binary
The CLI bug above is one symptom; the daemon/IPC write paths make the same
assumption. A single early `signal(SIGPIPE, SIG_DFL)` (unix) would make every
`| head`-style pipeline behave like `systemctl`.

### No LICENSE file
`Cargo.toml` declares MIT but no `LICENSE` text ships in the repo or in the
deb/rpm/msi packages — a gap to close before any public release.
