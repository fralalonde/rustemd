# rystemd

A **systemd unit-manager reimplementation in Rust** — a `systemctl`-compatible
CLI with a built-in manager for unit files, user services, timers, socket
triggers, and dependency-driven lifecycle. Linux uses cgroups v2 with process
groups as a fallback; Windows uses native Win32 Job Objects.

> **Status:** functional core on Linux and Windows. Linux additionally supports
> PID-1 boot, D-Bus (opt-in), udev `.device` units, `.mount` units, an
> on-disk journal (`rystemctl journal`), and Phase-1 service sandboxing
> (`PrivateTmp=`, `ProtectHome=`/`ProtectSystem=`, `ReadOnlyPaths=`,
> `NoNewPrivileges=`). Windows supports `.service`, `.socket` (TCP trigger
> activation), `.timer`, and `.target` units in SCM system mode or interactive
> `--user` mode. There is no journald/syslog forwarding or `journalctl`
> drop-in, no seccomp/`DynamicUser=`/`DevicePolicy=` hardening, and no
> `systemd1` D-Bus interface yet.

---

## Quick start

```sh
cargo build --release

# Linux/macOS user manager:
./target/release/rystemd daemon --user
./target/release/rystemctl --user list-units

# Windows user manager (PowerShell):
.\target\release\rystemd.exe daemon --user
.\target\release\rystemctl.exe --user list-units
```

Two binaries split the work: `rystemd` is the PID-1 manager daemon, and
`rystemctl` is the `systemctl`-compatible CLI that talks to it. To make
existing `systemctl` scripts work unchanged, symlink the CLI under that name:

```sh
ln -s /path/to/rystemd /usr/local/bin/rystemd
ln -s /path/to/rystemctl /usr/local/bin/systemctl
```

`rystemctl` (optionally invoked via the symlink) is a drop-in `systemctl`.

### Windows service and user modes

Run an interactive manager for the signed-in user without elevation:

```powershell
.\target\release\rystemd.exe daemon --user
.\target\release\rystemctl.exe --user status myapp.service
```

User units are read from `%LOCALAPPDATA%\rystemd\config` and
`%LOCALAPPDATA%\rystemd\units`. The user control endpoint is a named pipe,
`\\.\pipe\rystemd-user-<identity-hash>`.

To host the system manager in the Windows Service Control Manager, use an
elevated PowerShell or Command Prompt:

```powershell
rystemd.exe service install                   # automatic start
rystemd.exe service install --manual          # demand start
sc.exe start rystemd
rystemctl.exe list-units
sc.exe stop rystemd
rystemd.exe service uninstall
```

`--name` and `--display-name` can customize registration. System units live
under `%ProgramData%\rystemd\config` and `%ProgramData%\rystemd\units`; the
control pipe is `\\.\pipe\rystemd-system`.

Windows `.service` support covers `Type=simple`, `exec`, `idle`, and `oneshot`.
Each process tree is placed in a kill-on-close Job Object before its primary
thread is resumed. `MemoryMax=` and `TasksMax=` map to Job Object limits.
`Type=forking`, `notify`, and `dbus`, plus `User=`/`Group=`, fail explicitly.

Windows `.socket` units support TCP `ListenStream=host:port` and consume one pending connection and start the
matching service. The MVP does **not** pass
the listening socket into the child; it is a launch trigger rather than full
`LISTEN_FDS` handoff. Unix-domain `ListenStream=` values are rejected on
Windows.

### Shell completions

Generate completion scripts for your shell. Completions follow the invoked
binary name, so `rystemctl completions …` and (via the symlink)
`systemctl completions …` both work:

```sh
rystemctl completions bash        > ~/.bash_completion.d/rystemctl
rystemctl completions fish        > ~/.config/fish/completions/rystemctl.fish
rystemctl completions zsh         > ~/.zsh/completions/_rystemctl
rystemctl completions powershell   # pipe into Register-ArgumentCompleter
rystemctl completions nushell
```

## Homebrew

On Linux (e.g. Bazzite) or macOS, install from a tap without layering via
`rpm-ostree`:

```sh
brew tap fralalonde/rystemd
brew install rystemd
```

### ublue / Bazzite

On ublue images (Bazzite, Aurora, Bluefin) Homebrew is the supported way to
install CLI/TUI tools: it installs into its own prefix
(`/home/linuxbrew/.linuxbrew`) rather than layering into the immutable `/usr`,
so no `rpm-ostree` layer or reboot is needed. If `brew` isn't on PATH yet,
install it via the Bazzite Portal or `ujust` (see the
[Bazzite Homebrew docs](https://docs.bazzite.gg/Installing_and_Managing_Software/Homebrew/)),
then run the tap + install commands above. Binaries land in
`$(brew --prefix)/bin`, which the brew `shellenv` puts on PATH.

The formula (in the `homebrew-rystemd` tap) installs `rystemd`, `rystemctl`, and
`rystemd-tui` plus shell completions. It deliberately does **not** create a
`systemctl` symlink — on a systemd host that would shadow the real `systemctl`
in your PATH. To exercise the drop-in CLI, symlink it into a directory you
keep ahead of `/usr/bin` only while testing:

```sh
ln -s "$(brew --prefix)/bin/rystemctl" ~/.local/bin/systemctl
```

Refresh the formula's pinned sha256 after each release with
`scripts/gen-brew-formula.sh <version>` in the `homebrew-rystemd` tap (run once
the release assets are published).

## TUI

`rystemd-tui` is a terminal client that talks to a running manager over the
same `Control` API. It **detects** the daemon's socket (it never spawns a
second instance — at most one rystemd manager runs) and shows live tabs:
**Units**, **Services**, **Timers**, and **Unit files**, each with a status
pane and single-key actions.

![rystemd-tui demo](docs/demo.gif)

```sh
./target/release/rystemd-tui --user   # or: cargo run --release -p rystemd-tui -- --user
```

Keys: `Tab` switch tab · `↑`/`↓`/`j`/`k` move · `/` filter · `s` start · `x`
stop · `r` restart · `e` enable · `d` disable · `R` daemon-reload · `f`
refresh · `q`/`Esc`/`Ctrl+Q` quit.

Regenerate the GIF with `sh demo/generate.sh` (needs `vhs` + `ttyd`).

---

## What's implemented

**Unit files** (`systemd.syntax`-style)
- `[Unit]` / `[Service]` / `[Socket]` / `[Mount]` / `[Timer]` / `[Install]` sections
- `Description`, `After`, `Requires`, `Wants`, `Conflicts`
- `ExecStart` (multiple, with `-`/`@`/`+` prefixes), `ExecStartPre`,
  `ExecStartPost`, `ExecStop`, `ExecReload`, `ExecStart=-…`
- `Type=` — `simple`, `exec`, `forking`, `oneshot`, `notify`, `idle`
- `Restart=` (`no`/`on-success`/`on-failure`/`always`), `RestartSec`
- `User=`, `Group=`, `WorkingDirectory=`, `Environment=`, `EnvironmentFile=`
- `LimitNOFILE`/`LimitNPROC`/`LimitCORE`/`LimitAS` (rlimits), `Nice=`, `UMask=`
- `StandardOutput=`/`StandardError=` (`journal`, `inherit`, `null`, `file:…`)
- `RemainAfterExit=`, `PIDFile=` (forking), `KillSignal=`, `TimeoutStartSec=`,
  `TimeoutStopSec=`
- Quoting, `\` escapes, line continuations, drop-ins (`name.service.d/*.conf`),
  and specifiers (`%n`, `%p`, `%i`, `%u`, `%h`, …)

**Timers**
- `OnCalendar=` (full systemd calendar grammar: `*-*-* 09:00:00`,
  `Mon..Fri 09:00`, `*:0/15`, `daily`, `weekly`, ranges, steps, lists)
- `OnBootSec=`, `OnUnitActiveSec=`, `OnUnitInactiveSec=`, `OnStartupSec=`
- `Persistent=`, `AccuracySec=`

**Socket activation, mounts, & devices**
- `.socket` units — `ListenStream=`/`ListenDatagram=`, inetd-style activation
  via `LISTEN_FDS`, `Service=` target (the `socket` feature, default-on)
- `.mount` units — `[Mount]` `What=`/`Where=`/`Type=`/`Options=`, mount on
  start / unmount on stop (Linux only)
- `.device` units — runtime-generated from sysfs + netlink uevents (the
  `udev` feature, default-on); no unit file, matching systemd
- D-Bus — `org.rystemd.Manager1.Manager` (`ListUnits`/`GetUnit`/`StartUnit`/
  `StopUnit`/`Version`) plus `Type=dbus`/`BusName=` activation (Linux only;
  behind the opt-in `dbus` feature, not in `default`; not a `systemd1` drop-in)

**Lifecycle & supervision**
- Dependency graph (start order from `After`/`Requires`, stop order reversed)
- Linux: cgroups/process groups plus subreaper supervision
- Windows: Win32 Job Objects assigned before process resume; stop terminates
  the complete job tree
- `systemctl start/stop/restart/reload/kill/status/is-active/is-failed`
- `enable` / `disable` / `is-enabled` via `[Install]` symlinks
  (`WantedBy=`, `RequiredBy=`, `Alias=`, `Also=`)

**Control surfaces**
1. `systemctl`-compatible CLI
2. JSON-line IPC over Unix sockets (Linux) or named pipes (Windows)
3. A **programmatic `Control` API** (library alternative to the CLI/D-Bus) —
   see below

---

See [`docs/handbook.md`](docs/handbook.md) for worked examples.

## License

MIT
