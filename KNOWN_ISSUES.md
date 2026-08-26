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

### Timer re-arm fires units that were never started
`OnUnitActiveSec` / `OnUnitInactiveSec` / `OnCalendar` re-arm and fire even
when the timer's target unit is not in the started state, whereas systemd only
arms these once the unit is activated. Causes spurious firings. The demo works
around it with `OnBootSec`. Location: `manager/timer.rs` re-arm path.

### resume_primary_thread can leave a service stuck in START_PENDING
Location: `rustemd/src/platform/windows/process.rs:299-334`. The thread-resume
path walks a system-wide `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)`, resumes
only the FIRST thread whose owner pid matches, and breaks on the first
`ResumeThread` that does not return `u32::MAX`. If that thread's prior suspend
count is 0 (it was not actually suspended), `found=true` fires without resuming
the real suspended primary thread, so a `CREATE_SUSPENDED` newborn hangs forever
and its service sits in `START_PENDING`.

## Weaknesses

### D-Bus is not a `systemd1` drop-in
Only a rustemd-specific `org.fralalonde.rustemd1.Manager` interface
(`ListUnits`/`GetUnit`/`StartUnit`/`StopUnit`/`Version`). No per-unit object
graph, no `org.freedesktop.DBus.Properties`, no job objects or signals — real
D-Bus clients (logind, desktop integration, `systemctl --user` over the bus)
won't work. D-Bus is also opt-in (the `dbus` cargo feature, off by default):
builds without it omit the zbus dependency tree entirely.

### No journald
Service stdout/stderr are captured to a per-unit in-memory ring (`status`) and
persisted to a size-rotated on-disk journal under `/var/log/rustemd/`
(`~/.local/state/rustemd/journal` for user mode), readable via
`rustemctl journal [unit] [-n N] [-f] [--since SECS]` (the journal is a plain
append file, not the journald binary format). Not queryable beyond
unit/tail/since; no `journalctl -u`-style arbitrary field filters, and no
forwarding to an external journald/syslog sink yet. (`StandardOutput=journal`
backs onto that capture.)

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

### Spawned diverges by platform, leaking the platform seam
Windows `Spawned { pid }` (`windows/process.rs:57-59`) vs Unix
`Spawned { pid, stdout, stderr }` (`platform/process.rs:39-43`); the manager
carries `#[cfg]` branches for fd-polling vs `drain_output`.

### Two `windows-sys` versions in the tree — retained
`windows-sys` 0.61.2 is used directly by rustemd and transitively by current
terminal dependencies; 0.59.0 is pulled by `colored` 2.2.0. They do not cross
the rustemd API boundary and are binding-only declarations. Retain both until
an upstream `colored` upgrade naturally unifies them; forcing Cargo to do so
would require an incompatible dependency override.

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
