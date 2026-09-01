# Changelog

All notable changes to rystemd are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions are
derived from Git tags via `build.rs` (never hardcoded).

## [Unreleased]

### Added

- **Release automation for package managers.** `release.sh` now calls
  `scripts/bump-package-managers.sh --push` after tagging, which regenerates
  and pushes the Homebrew `homebrew-rystemd` formula and the Scoop
  `scoop-rystemd` bucket so `brew install` / `scoop install` see the new
  version (previously a forgotten manual step that left package managers at an
  older release). Asset hashes are fetched with bounded retry to wait out CI's
  asynchronous asset upload.

## [0.2.1] — 2026-08-31

### Fixed

- **Boot aggregation targets no longer leak into every manager.** `322b046`
  made `sysinit.target`/`graphical.target`/`getty.target` unconditionally
  preseeded builtins, which polluted plain user managers and the Windows build
  and broke `cargo test --workspace` — and with it the v0.2.0 release pipeline.
  They are now gated behind the `boot` feature (preseed, `is_builtin`,
  `builtin_target` description), and the affected test's assert is
  feature-conditional. (This was the fix carried in as v0.2.1.)

## [0.2.0] — 2026-08-31

### Added

- **Real-root boot against a stock, downloadable OS disk.** rystemd running as
  PID 1 in an initramfs can now discover, mount, and pivot into an actual
  Fedora Cloud disk attached in qemu — no host root, no libguestfs, no disk
  modification.
  - `switch_root` handoff (`prepare_deployment` + `handoff`): bind/copy rystemd
    into the deployment so the post-pivot re-exec is self-contained, resolve
    the real ostree deployment under `/sysroot` (`find_deployment`), bind the
    sysroot `/var` in for state.
  - `--features boot` aggregation targets (`sysinit`/`graphical`/`getty`) so
    Fedora's real target graph resolves instead of failing on
    "unit not found".
  - Deterministic console login by overriding the deployment's `default.target`
    (slim `default → getty@ttyS0`), neutralizing Fedora's real `basic.target`.
  - Optional root password injected at `/init` time (host-computed `$6$` hash,
    verified against glibc `crypt`, `sed`-into the live shadow) so a stock
    cloud image can be logged into.
  - `scripts/{build-realroot-initramfs,boot-realroot-vm}.sh` + the rootless
    recipe in the handbook.
- **SELinux policy module** (`pol/rystemd.te`, `f.c`, `README.md`) — a starter
  `rystemd_t` domain, compile/package-verified offline.
- **Handbook published to its own repo** (`rystemd/rystemd.github.io`) and
  `main` made code-only; scoop install switched to the dedicated
  `scoop-rystemd` bucket; package-manager docs now rely on release artifacts.

## [0.1.5] — 2026-08-27

### Fixed

- **`OnUnitInactiveSec=`/`OnUnitActiveSec=` no longer fire units that were
  never activated.** `rearm_timer` took the inactive branch for any non-active
  target, so a timer whose only elapse source is `OnUnitInactiveSec=` spuriously
  started a service that had never run this boot. The inactive branch is now
  gated on the target having passed through `Active` (systemd semantics: these
  directives only arm once the unit has actually been activated/deactivated).
  Regression coverage: `timer_onunitinactive_requires_prior_activation` (e2e).
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

- **`RestrictRealtime=`** (Linux/x86_64) now enforces: it denies the realtime
  scheduler syscalls (`sched_setscheduler`/`sched_setattr`/`sched_setparam`)
  via the seccomp BPF machinery. It implies `NoNewPrivileges=` (needs a filter
  installed) and composes with `SystemCallFilter=` (standalone it is a
  deny-list; combined, the filter's allow/deny mode wins and the RT calls are
  still blocked). Enforced by an e2e test that probes with a privilege-free
  `os.sched_setscheduler` and a unit test for the deny-list folding.
- **`SystemCallFilter=` e2e test** (`rystemd/tests/seccomp.rs`): runs the real
  manager unprivileged and asserts a `~mkdir mkdirat` deny-list blocks the
  syscall while an unfiltered control succeeds — the sandbox-isolation e2e gap.
  Unit tests cover the `NoNewPrivileges` implication, the `EPERM` default, and
  the errno-0 builder guard.
- **`LockPersonality=`** (Linux/x86_64) now enforces: it denies the
  `personality(2)` syscall outright via the seccomp BPF machinery, so a managed
  service cannot switch execution domains or drop ASLR hardening. It implies
  `NoNewPrivileges=` and composes with `SystemCallFilter=` (merged into a
  deny-list as an extra deny entry; harmless alone under an allow-list).
  Enforced by an e2e test (`lock_personality_blocks_personality`) that probes
  the syscall through Python `ctypes` — version-proof across Python releases —
  plus unit tests for parsing, the `NoNewPrivileges` implication, and deny-list
  folding.
- **`PrivateDevices=`** (Linux) now enforces: it shadows `/dev` with a fresh
  tmpfs holding a minimal core device tree (null/zero/full/random/urandom/tty,
  `devpts`, `/dev/shm`, `/dev/fd` symlinks), hiding host devices. A user-mode
  manager degrades best-effort (warns, runs with `/dev` unsandboxed) because
  `mknod` is refused in an unprivileged user namespace; the privileged e2e test
  runs only under real root.
- **`RestrictSUIDSGID=`** (Linux/x86_64) now enforces: it denies the file-mode
  syscalls that could set an SUID/SGID bit or relabel ownership
  (`chmod`/`fchmod`/`fchmodat`/`chown`/`fchown`/`lchown`/`fchownat`), with unit
  tests and an e2e test (`restrict_suidsgid_blocks_chmod`).
- **`RestrictAddressFamilies=`** (Linux/x86_64) now enforces: a seccomp gate on
  `socket(2)`/`socketpair(2)` comparing the family argument against the
  directive's list (`~`-prefixed lists deny the named families, an unprefixed
  list allows only those), with unit tests on the BPF layout and an e2e test
  (`restrict_address_families_allows_only_unix`).
- **`MemoryDenyWriteExecute=`** (Linux/x86_64) now enforces: a seccomp arg-gate
  on `mmap`/`mprotect`/`pkey_mprotect` (implies `NoNewPrivileges=` like the
  other filter installs) that denies any request setting both `PROT_WRITE` and
  `PROT_EXEC` — creating a mapping, or `mprotect`ing RW→RWX — with `EPERM`,
  with unit tests on the BPF layout and an e2e test probing an RW→RWX
  `mprotect` via `ctypes`.
- **`CapabilityBoundingSet=` and `AmbientCapabilities=`** (Linux/x86_64,
  phase-2c) now enforce: `drop-from` or `~`-inverted bounding-set reduction via
  `prctl(PR_CAPBSET_DROP)`, and best-effort `PR_CAP_AMBIENT_RAISE` for the
  ambient set.
- **Windows install options** — Scoop bucket (`scoop-rystemd`), an MSI, and a
  NuGet native-tools package published per release.

## [0.1.0] — Initial release

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