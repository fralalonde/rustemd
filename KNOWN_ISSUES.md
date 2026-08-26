## Limitations (intentional)

- **cgroup v2 (Linux)** — one cgroup per unit for reliable tree cleanup and
  `MemoryMax`/`MemoryHigh`/`CPUWeight`/`CPUQuota`/`IOWeight`/`IODeviceWeight`/
  `TasksMax`; falls back to process groups + a subreaper where cgroups aren't
  available.
- **PID-1 boot is opt-in** — the `boot` cargo feature adds mounting
  `/proc`/`/sys`/`/dev`/`/run`/`cgroup2`, early-boot config (hostname, sysctl,
  modules, fstab), template units (`getty@tty1`), and `reboot(2)` power-off.
  Off by default — a container runtime does this for you. Test it with
  `scripts/ns-boot-test.sh` (unprivileged namespaces, no qemu),
  `scripts/vm-test.sh` (qemu + initramfs, automated), or drive it
  interactively with `scripts/live-vm.sh` (see [DEMO.md](DEMO.md)).
- **D-Bus is opt-in** — the `dbus` cargo feature pulls in zbus for
  `Type=dbus`/`BusName=` activation and the `org.rustemd.Manager1.Manager`
  control interface. Off by default to keep the default build free of the
  zbus/zvariant/async-executor dependency tree (and ~48% smaller). Build with
  `--features dbus` to enable it.
- **Windows socket activation is trigger-only** — TCP listeners activate the
  service but are not inherited by it. Unix sockets and `LISTEN_FDS` handoff
  remain Unix-only.
- **Windows service-type subset** — `forking`, `notify`, `dbus`, `User=`,
  `Group=`, `MemoryHigh=`, `CPUWeight=`, and `KillMode=process` fail explicitly. Windows has no
  generic POSIX-signal equivalent; stop/kill terminate the unit Job Object.
- **macOS is not yet a supported manager target.**
- **Journaling is a plain store, not journald** — per-unit on-disk journal
  under `/var/log/rustemd/` with `rustemctl journal`; no `journalctl` binary,
  no journald wire format, no syslog/journald forwarding. **Service sandboxing
  is Phase-1 only** — no `CapabilityBoundingSet=`/`AmbientCapabilities=`,
  seccomp (`SystemCallFilter=`), `DynamicUser=`, or `DevicePolicy=`/`DeviceAllow=`
  (eBPF). No `systemd-analyze`-style tooling. See
  [`KNOWN_ISSUES.md`](KNOWN_ISSUES.md) for the full categorized list.
- `notify` types are supported (`NOTIFY_SOCKET`), but `sd_notify`'s watchdog is
  not yet wired.

## Bugs

### Timer re-arm fires units that were never started
`OnUnitActiveSec` / `OnUnitInactiveSec` / `OnCalendar` re-arm and fire even
when the timer's target unit is not in the started state, whereas systemd only
arms these once the unit is activated. Causes spurious firings. The demo works
around it with `OnBootSec`. Location: `manager/timer.rs` re-arm path.

### `try-restart` always restarts
`Command::RestartOrStart` (`rustemctl/src/cli.rs:215`) dispatches `restart`,
not a `try-restart` op, so `rustemctl try-restart` restarts a unit *even when
it is inactive* — the documented "restart if running, otherwise just start"
contract is broken, and it always stops an inactive unit's `ExecStop` (which
should not run). No `try_restart` IPC op exists; `restart_or_start`/`try-restart`
semantics are unimplemented.

### resume_primary_thread can leave a service stuck in START_PENDING
Location: `rustemd/src/platform/windows/process.rs:299-334`. The thread-resume
path walks a system-wide `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)`, resumes
only the FIRST thread whose owner pid matches, and breaks on the first
`ResumeThread` that does not return `u32::MAX`. If that thread's prior suspend
count is 0 (it was not actually suspended), `found=true` fires without resuming
the real suspended primary thread, so a `CREATE_SUSPENDED` newborn hangs forever
and its service sits in `START_PENDING`.

## Weaknesses

- The release URLs still use the `fralalonde` org name until the repository is relocated; the Homebrew formula now lives in a dedicated `homebrew-rustemd` tap (`brew tap fralalonde/rustemd` resolves to `github.com/fralalonde/homebrew-rustemd`).

### D-Bus is a partial `systemd1` drop-in
The `org.freedesktop.systemd1` surface (runtime-gated: only served when the
well-known name is free, i.e. no real systemd on the bus) now has a per-unit
object graph (`/org/freedesktop/systemd1/unit/<escaped>` with `Id`/`Description`/
`LoadState`/`ActiveState`/`SubState`/`Following` properties, via zbus's built-in
`org.freedesktop.DBus.Properties`), the read methods (`ListUnits`/`GetUnit`/
`LoadUnit`/`ListJobs`/`GetUnitProcesses`), and the `UnitNew`/`UnitRemoved`
signals. Still missing: the **control methods** (`StartUnit`/`StopUnit`/
`RestartUnit`/`ReloadUnit` returning job object paths), the **job object model**
and `JobNew`/`JobRemoved` signals, and `PropertiesChanged` emission. Until the
control surface lands, real D-Bus consumers — `logind`/seat management, desktop
portals, `systemctl --user` over the bus — cannot be driven drop-in. D-Bus is
opt-in (the `dbus` cargo feature, off by default): builds without it omit the
zbus dependency tree entirely.

### journald drop-in is incomplete
Service stdout/stderr are captured to a per-unit in-memory ring (`status`) and
persisted to a size-rotated on-disk journal under `/var/log/rustemd/`
(`~/.local/state/rustemd/journal` for user mode), readable via
`rustemctl journal [unit] [-n N] [-f] [--since SECS]`. But there is **no
`journalctl`-named binary** and no wire/format compatibility with journald:
the store is a plain append file, not the journald binary format, and there is
no forwarding to an external journald/syslog sink. A desktop user typing
`journalctl -u foo -f` gets nothing. Query surface is unit/tail/since only —
no field filters (`-u`, `-p`, `-S`), no boot/session disambiguation.
(`StandardOutput=journal` backs onto that capture.)

### `systemctl` CLI surface is incomplete
Implemented: start/stop/restart/reload/status/kill/is-active/is-failed/
is-enabled/enable/disable/daemon-reload/list-units/list-unit-files/list-timers/
cat/show/get-default/set-default/isolate/is-system-running/poweroff/journal.
Missing for drop-in use: `mask`/`unmask`, `edit`, `list-dependencies`,
`list-sockets`, `reset-failed`, `clean`, `preset`, `reenable`, `link`, `revert`,
`cancel`, `list-machines`, and a correct `try-restart` (see bug above).
`isolate` exists but its stop-others semantics are untested.

### No `Condition*`/`Assert*` directives
None of `ConditionPathExists=`, `ConditionFileNotEmpty=`, `ConditionUser=`,
`ConditionGroup=`, `ConditionHost=`, `AssertPathExists=` (or any other
condition/assert) are parsed. Desktop unit files lean heavily on these
(`ConditionPathExists=!/etc/foo` to gate a service), so many real units will
either load unconditionally or fail for reasons that are opaque.

### Missing unit types: `.slice`, `.scope`, `.automount`
Only `.service`, `.socket`, `.timer`, `.target`, `.mount`, `.path`, `.device`
exist. `.slice`/`.scope` (the cgroup hierarchy the desktop depends on —
`user.slice`, `app.slice`, `session.slice`) and `.automount` are absent.
`systemd-analyze`-style tooling is also absent.

### Missing runtime-directory directives
`RuntimeDirectory=`, `StateDirectory=`, `CacheDirectory=`, `LogsDirectory=`,
`ConfigurationDirectory=` are unimplemented (parsed? no — silently ignored).
Desktop apps and daemons commonly rely on systemd creating and owning these
(`%t`/`$XDG_RUNTIME_DIR` children), so their units will fail without them.

### Misc `[Service]` directives absent
`WatchdogSec=`/`WatchdogSignal=`, `SupplementaryGroups=`, `PAMName=`,
`SetLoginEnvironment=`, `TTYPath=`, `OOMScoreAdjust=`, `IgnoreSIGPIPE=`,
`StartLimitIntervalSec=`/`StartLimitBurst=` (start-limit rate limiting) are not
parsed. `Restart=` does support `no`/`on-success`/`on-failure`/`on-abnormal`/
`on-abort`/`on-watchdog`/`always`, but the start-limit machinery that systemd
pairs with it is absent.

### Service sandboxing is partial
Implemented (Linux, Phase-1): mount-namespace plumbing via `CLONE_NEWNS` (root)
or `CLONE_NEWUSER|CLONE_NEWNS` + uid_map (user), `PrivateTmp=`, `ProtectHome=`
(read-only or tmpfs), `ProtectSystem=` (yes/full/strict), `ReadOnlyPaths=`,
`NoNewPrivileges=`. A user-mode manager cannot read-only-relabel pre-existing
host mounts (needs `CAP_SYS_ADMIN` over them, which a userns doesn't confer) —
those ops degrade to a visible warning rather than failing the unit, matching
systemd's tolerance. Notably unimplemented: `CapabilityBoundingSet=` and
`AmbientCapabilities=` (dropped from Phase-1; the raw `capset` ABI wasn't in
`libc`), and the Phase-2/3 family — `SystemCallFilter=` (seccomp),
`SystemCallArchitectures=`, `MemoryDenyWriteExecute=`, `PrivateDevices=`,
`DynamicUser=`, `DeviceAllow=`/`DevicePolicy=` (eBPF), `ProtectKernel*`,
`Restrict*`, `IPAddress*`, `RemoveIPC=`, `LockPersonality=`. All of the latter
are **parsed and emit a per-unit compat warning at load** rather than being
silently ignored.

### Partial cgroup resource controls
`MemoryMax` / `MemoryHigh` / `CPUWeight` / `CPUQuota` / `IOWeight` /
`IODeviceWeight` / `TasksMax` are enforced (cgroup v2). Missing: block-device
access control (`DevicePolicy=`/`DeviceAllow=`, which on cgroup v2 requires an
eBPF `BPF_CGROUP_DEVICE` program), `MemoryMin`/`MemoryLow`/`MemorySwapMax=`,
`CPUAccounting`-adjacent knobs, and `IOMax=` bandwidth/IOPS caps.

### `Type=notify` is partial
`NOTIFY_SOCKET` is set and `READY=1` honored, but the sd_notify watchdog and
`NotifyAccess=` enforcement are not wired.

### Test harness maturity gaps
~115 tests pass on Linux (103 unit + 11 e2e + 2 CLI + 1 repo_dao + 5 repo +
1 typed_dao), plus ~9 `#[cfg(windows)]` tests. Coverage is solid for the happy
path — full lifecycle, kill/stop, daemonizing-orphan sweep, socket activation,
mount lifecycle, timer firing, target `Wants=` pull-in, live demo units, device
enumeration, journal persistence, and a real-CLI roundtrip — but thin where it
matters for the stated goals:

- **No e2e coverage for**: sandbox isolation (a `PrivateTmp`/`ProtectSystem`
  leak or a read-only write-denial is never asserted), cgroup limit enforcement,
  `enable`/`disable` symlink installation, template instantiation
  (`getty@tty1`→`getty@.service`), restart/`on-failure` policy, or
  timeout/`KillSignal` handling.
- **No CI on push/PR.** The only workflow (`release.yml`) runs `cargo test`
  (Linux only, no clippy/fmt) *on tag pushes*. The `boot` (PID-1) path, the
  `dbus` feature, and clippy/fmt are gated nowhere except the tag-triggered
  Windows job's clippy — a broken default build can land on `main` silently.
- **Boot/PID-1 scripts are manual** — `scripts/{vm-test,ns-boot-test,live-vm}.sh`
  are not in CI, so the `boot` feature (early-boot, template units, poweroff)
  has no automated regression net.
- **No fuzzing** of the unit-file parser (a security-sensitive surface, given
  the upcoming inspection) and **no property tests** for the calendar/timespan
  grammars.
- **Windows tests never run on Linux CI** and are only exercised by the
  tag-triggered release job at MSRV.

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

### journald wire format is a fork, not a bridge
The on-disk journal uses a plain tab-delimited append format rather than
journald's native binary format or its `AF_UNIX` forwarding protocol. To be a
drop-in for desktop tooling, rustemd must either (a) emit the journald native
protocol so `journalctl` reads its entries, or (b) ship a `journalctl`
replacement that reads the same store — the current `rustemctl journal` is
neither a wire-compatible producer nor a named `journalctl`. Decide before the
security inspection whether the goal is "reads like systemd" (own format + own
CLI) or "is systemd" (native protocol), as it changes the journal store's
design.
