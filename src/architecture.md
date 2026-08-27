# Architecture

rystemd is built as three crates in one workspace, plus a small internal `repo`
module:

- **`rystemd`** — the library + daemon binary. Everything else builds on it.
- **`rystemctl`** — the `systemctl`-compatible CLI.
- **`rystemd-tui`** — the terminal client.

## The manager

`Manager` drives a synchronous, single-threaded poll loop (no tokio). The loop
iterates on a bounded timeout, draining:

- control requests over the IPC channel,
- D-Bus requests, events, and unit snapshots (Linux, `dbus` feature),
- timer-wheel elapses (`fire_timer`),
- path-watch polls (`.path` units),
- SIGCHLD reaps and job resolution (`process_jobs`).

Jobs (start/stop/restart) encode the lifecycle transition and the `Wants=`,
`Requires=`, `After=`, `Before=`, and `Conflicts=` ordering graph. A
per-unit-type VTable (`unit_type` → `UnitType` trait) handles the concrete
`start`/`stop` for each kind.

## Unit types

| Kind | Dir/file | Notes |
| --- | --- | --- |
| `.service` | `(suffix)` | `Type=` simple/exec/idle/oneshot/forking/notify/dbus |
| `.socket` | `[Socket]` | `ListenStream=/Datagram=/Netlink=/SequentialPacket=`, inetd-style activation |
| `.timer` | `[Timer]` | calendar + monotonic triggers |
| `.target` | — | grouping only |
| `.mount` | `[Mount]` | `mount(2)` in `[Mount]` |
| `.path` | `[Path]` | `PathExists=`, `DirectoryNotEmpty=`, `PathChanged=`, `PathExistsGlob=` |
| `.device` | udev | runtime device units (Linux) |

The parser lives in `unit/` with a `parse::RawUnitFile` layer; directives are
pulled via `unit_scalar`/`list_of` helpers so unsupported directives fail
loudly or are parsed-and-warned, never silently lost.

## platform

`platform/` abstracts the OS. Linux uses `nix`; Windows uses direct Win32
bindings. The manager never touches raw syscalls directly — everything is
behind this layer (process spawn + cgroups/Job Objects, sandboxing mounts,
signal handling, udev/netlink, D-Bus, boot).

## repo

A typed DAO (`UnitDefinition`, not unit-file text) over a directory backend.
Mutations are atomic (temp + rename); reads are never locked. Single process
by design. `rystemctl repo` and the manager share it so clients can inspect or
edit unit files in place.

## Control

`Control` is a single interface with two implementations: the in-process
`Manager` and the remote `SocketClient`. The manager also exposes the same
surface over JSON IPC and (Linux, `dbus`) over D-Bus.
This is what `rystemctl`, `rystemd-tui`, and D-Bus clients all speak.