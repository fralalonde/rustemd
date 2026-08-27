# Linux integration

Linux is the primary target and carries the platform-specific features that
make rystemd a real systemd work-alike.

## Process supervision

A service runs in its own **cgroup v2** subtree when available; the spawned
process adopts itself into the unit's cgroup before exec, so `cgroup.kill`
reaches every descendant. When cgroups aren't usable the fallback is a
**process group**: the child becomes a session leader so `kill(-pid, sig)`
reaches the tree, and it can acquire a controlling terminal (which
getty/login programs require). As PID 1 (or with the daemon running as root)
rystemd sets itself as a subreaper and sweeps orphaned / double-forked
children.

## Socket activation

`.socket` units bind listeners and activate a matching service on the first
connection. On Linux the listeners are passed to the child via `LISTEN_FDS`
/ `LISTEN_PID` (the standard systemd contract), so inetd-style handoff works.
Supported `Listen*` directives:

- `ListenStream=` — Unix-domain or TCP (systemd-*.socket uses this)
- `ListenDatagram=` — e.g. `systemd-journald.socket`
- `ListenNetlink=` — e.g. `systemd-udevd-kernel.socket`
- `ListenSequentialPacket=` — e.g. `systemd-udevd-control.socket`

`systemd-udevd/sockets`, journald, and coredump all depend on these; without
them udev receives no kernel events.

## Path activation

`.path` units watch filesystem paths and activate a matching service
(`Unit=`) on a trigger — `PathExists`, `PathExistsGlob`, `PathChanged`,
`DirectoryNotEmpty`, with `MakeDirectory=` support. The watch is stat-based
(polled in the manager's loop; no inotify yet).

## Mount + device

`.mount` units call `mount(2)`/`umount(2)`. `.device` units are discovered
from sysfs and tracked via netlink uevents (`udev` feature), so
`After=/Requires=` ordering on device paths resolves.

## D-Bus

With the `dbus` feature, rystemd owns `org.rystemd.Manager1` on the system
(or user) bus and — when the well-known name is free, i.e. no real systemd is
present — also serves an `org.freedesktop.systemd1`-compatible surface:
`Version`, `ListUnits`, `GetUnit`/`LoadUnit`, `ListJobs`,
`GetUnitProcesses`, per-unit `org.freedesktop.systemd1.Unit` objects, and
`UnitNew`/`UnitRemoved`. The systemd1 surface uses systemd's `bus_label_escape`
object-path encoding.

## PID-1 boot (`boot` feature)

With `--features boot`, `rystemd` can run as PID 1: it mounts the API/virtual
filesystems (`/proc`, `/sys`, `/dev`, `/run`, `/tmp`), runs early-boot
configuration, boots the default target, supervises everything, and powers the
machine off on shutdown. This is the VM-first / initramfs path toward a
drop-in init (see `scripts/`).

## Environment overrides for testing

`RYSTEMD_UNIT_PATH`, `RYSTEMD_CONFIG_DIR`, `RYSTEMD_RUNTIME_DIR`,
`RYSTEMD_SOCKET`, and `RYSTEMD_JOURNAL_DIR` repoint the manager at a scratch
tree so integration tests run the real daemon against an isolated layout.