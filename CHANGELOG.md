# Changelog

All notable changes to rystemd are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions are
derived from Git tags via `build.rs` (never hardcoded).

## [Unreleased]

### Fixed

- **`SystemCallFilter=` (seccomp) now actually blocks.** Before, a deny-list
  built its BPF program with `SystemCallErrorNumber=` defaulting to `0`, so a
  blocked syscall returned `-0` — indistinguishable from success — and the
  filter silently did nothing (e.g. `~mkdir` let GNU `mkdir` succeed via
  `mkdirat`). A deny-list now defaults to `EPERM` (matching systemd), both in
  the unit parser and as a guard in the BPF builder.
- **`SystemCallFilter=` implies `NoNewPrivileges=`.** Installing a
  `SECCOMP_MODE_FILTER` requires `CAP_SYS_ADMIN` or `no_new_privs`; a
  user-mode (unprivileged) manager has neither, so seccomp units previously
  refused to spawn with `EACCES`. The manager now forces `NoNewPrivileges`
  before the filter (systemd.exec semantics), so seccomp works unprivileged.

### Added

- **`SystemCallFilter=` e2e test** (`rystemd/tests/seccomp.rs`): runs the real
  manager unprivileged and asserts a `~mkdir mkdirat` deny-list blocks the
  syscall while an unfiltered control succeeds — the sandbox-isolation e2e gap.
  Unit tests cover the `NoNewPrivileges` implication, the `EPERM` default, and
  the errno-0 builder guard.

## [0.1.0] — Unreleased

Initial release of rystemd: a systemd / `systemctl` reimplementation in Rust.

### Added

- **Unit manager daemon (`rystemd`)** with an internal job engine, dependency
  ordering (`Wants=`/`Requires=`/`After=`/`Before=`/`Conflicts=`), and a
  synchronous poll loop (no tokio).
- **Unit types:** `.service` (`Type=` simple / oneshot / forking / notify /
  dbus), `.socket` (socket activation over `ListenStream=`, `ListenDatagram=`,
  `ListenNetlink=`, `ListenSequentialPacket=`), `.timer` (calendar + monotonic),
  `.target`, `.mount`, `.device` (udev), and `.path` (path-based activation).
- **`rystemctl`** — a `systemctl`-compatible CLI: `start`/`stop`/`restart`/
  `reload`/`try-restart`/`status`/`is-active`/`is-failed`/`is-enabled`/`enable`/
  `disable`/`reenable`/`mask`/`unmask`/`reset-failed`/`clean`/`list-units`/
  `list-unit-files`/`list-dependencies`/`list-sockets`/`list-timers`/`cat`/
  `show`/`journal`/`daemon-reload`/`set-default`/`get-default`/`isolate`, plus
  shell completions.
- **`rystemd-tui`** — a terminal UI client for a running manager.
- **Process supervision:** cgroup v2 isolation (process-group fallback),
  `User=`/`Group=` privilege dropping, journal capture, `Restart=` policies,
  and daemonization/orphan sweeping.
- **D-Bus (default-on for Linux `dbus` feature):** a native `org.rystemd.Manager1`
  interface plus an `org.freedesktop.systemd1`-compatible read surface, served
  only when that well-known name is free (so it drops in where systemd is absent).
- **PID-1 boot (opt-in `boot` feature):** mount API filesystems, early-boot
  configuration, and shutdown.
- **Packaging:** Homebrew tap, `.deb` / `.rpm`, portable `.tar.gz`, and Windows
  `.zip` / `.msi`. (Linux + Windows targets; macOS is not supported.)

### Known limitations

See [KNOWN_ISSUES.md](KNOWN_ISSUES.md) for the full compatibility matrix —
notably `.slice` / `.scope` / `.automount` unit types and several sandbox
directives are not yet supported.
