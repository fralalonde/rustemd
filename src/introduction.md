# Introduction

**rystemd** is a systemd reimplementation in Rust: a unit manager daemon, a
`systemctl`-compatible CLI (`rystemctl`), and a terminal client (`rystemd-tui`).

It speaks the same unit-file language as systemd and drives the same lifecycle
events, so existing `.service`, `.timer`, `.socket`, `.target`, `.mount`,
`.path`, and `.device` units are understood without translation. `rystemctl`
mirrors the `systemctl` surface (`start`, `stop`, `status`, `enable`, timers,
sockets, journal, and more).

The design priorities are **binary size**, **attack surface**, and **no hidden
machinery**:

- Synchronous, single-threaded poll loop — no async runtime (no tokio).
- Optional features compiled in only where they're wanted: `socket`, `udev`,
  `dbus`, and `boot` (PID-1).
- `LTO = "fat"`, `codegen-units = 1`, full strip — a small, auditable binary.
- cgroup v2 process supervision with a plain process-group fallback.

Primary support is **Linux** (cgroups, udev/netlink, D-Bus system bus, and the
PID-1 boot path are all Linux-native). Windows is supported for the
manager/CLI/daemon compatibility port (Job Objects, named pipes, the Service
Control Manager). See [Compatibility & known issues](compatibility.md).

The project is named after its intent: **ry** (Rust) + **stemd** (systemd).