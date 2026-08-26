# rustemd

A **systemd unit-manager reimplementation in Rust** — a `systemctl`-compatible
CLI with a built-in manager for unit files, user services, timers, socket
triggers, and dependency-driven lifecycle. Linux uses cgroups v2 with process
groups as a fallback; Windows uses native Win32 Job Objects.

> **Status:** functional core on Linux and Windows. Linux additionally supports
> PID-1 boot, D-Bus (opt-in), udev `.device` units, `.mount` units, an
> on-disk journal (`rustemctl journal`), and Phase-1 service sandboxing
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
./target/release/rustemd daemon --user
./target/release/rustemctl --user list-units

# Windows user manager (PowerShell):
.\target\release\rustemd.exe daemon --user
.\target\release\rustemctl.exe --user list-units
```

Two binaries split the work: `rustemd` is the PID-1 manager daemon, and
`rustemctl` is the `systemctl`-compatible CLI that talks to it. To make
existing `systemctl` scripts work unchanged, symlink the CLI under that name:

```sh
ln -s /path/to/rustemd /usr/local/bin/rustemd
ln -s /path/to/rustemctl /usr/local/bin/systemctl
```

`rustemctl` (optionally invoked via the symlink) is a drop-in `systemctl`.

### Windows service and user modes

Run an interactive manager for the signed-in user without elevation:

```powershell
.\target\release\rustemd.exe daemon --user
.\target\release\rustemctl.exe --user status myapp.service
```

User units are read from `%LOCALAPPDATA%\rustemd\config` and
`%LOCALAPPDATA%\rustemd\units`. The user control endpoint is a named pipe,
`\\.\pipe\rustemd-user-<identity-hash>`.

To host the system manager in the Windows Service Control Manager, use an
elevated PowerShell or Command Prompt:

```powershell
rustemd.exe service install                   # automatic start
rustemd.exe service install --manual          # demand start
sc.exe start rustemd
rustemctl.exe list-units
sc.exe stop rustemd
rustemd.exe service uninstall
```

`--name` and `--display-name` can customize registration. System units live
under `%ProgramData%\rustemd\config` and `%ProgramData%\rustemd\units`; the
control pipe is `\\.\pipe\rustemd-system`.

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
binary name, so `rustemctl completions …` and (via the symlink)
`systemctl completions …` both work:

```sh
rustemctl completions bash        > ~/.bash_completion.d/rustemctl
rustemctl completions fish        > ~/.config/fish/completions/rustemctl.fish
rustemctl completions zsh         > ~/.zsh/completions/_rustemctl
rustemctl completions powershell   # pipe into Register-ArgumentCompleter
rustemctl completions nushell
```

## Homebrew

On Linux (e.g. Bazzite) or macOS, install from a tap without layering via
`rpm-ostree`:

```sh
brew tap fralalonde/rustemd
brew install rustemd
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

The formula (in the `homebrew-rustemd` tap) installs `rustemd`, `rustemctl`, and
`rustemd-tui` plus shell completions. It deliberately does **not** create a
`systemctl` symlink — on a systemd host that would shadow the real `systemctl`
in your PATH. To exercise the drop-in CLI, symlink it into a directory you
keep ahead of `/usr/bin` only while testing:

```sh
ln -s "$(brew --prefix)/bin/rustemctl" ~/.local/bin/systemctl
```

Refresh the formula's pinned sha256 after each release with
`scripts/gen-brew-formula.sh <version>` in the `homebrew-rustemd` tap (run once
the release assets are published).

## TUI

`rustemd-tui` is a terminal client that talks to a running manager over the
same `Control` API. It **detects** the daemon's socket (it never spawns a
second instance — at most one rustemd manager runs) and shows live tabs:
**Units**, **Services**, **Timers**, and **Unit files**, each with a status
pane and single-key actions.

![rustemd-tui demo](docs/demo.gif)

```sh
./target/release/rustemd-tui --user   # or: cargo run --release -p rustemd-tui -- --user
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
- D-Bus — `org.rustemd.Manager1.Manager` (`ListUnits`/`GetUnit`/`StartUnit`/
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
