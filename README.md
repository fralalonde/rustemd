# rustemd

A **systemd init reimplementation in Rust** — a drop-in `systemctl`
replacement with a built-in unit manager: unit files, user services, timers,
and dependency-driven lifecycle. Per-service **cgroups** (Linux cgroup v2) with
**process groups** + a **subreaper** as the fallback where cgroups aren't
available.

> **Status:** functional core, Linux-focused. Unit parsing, service
> supervision, timers, `enable`/`disable`, a JSON IPC daemon, and a
> `systemctl`-compatible CLI all work and are tested. No journald,
> no socket/dbus activation yet.

---

## Quick start

```sh
cargo build --release

# Run the manager (user mode — no root needed):
./target/release/rustemd daemon --user

# In another terminal, control it (the same binary, non-daemon mode):
./target/release/rustemd --user list-units
./target/release/rustemd --user start myapp.service
./target/release/rustemd --user status myapp.service
```

One binary does both jobs. To make existing `systemctl` scripts work
unchanged, symlink it under that name:

```sh
ln -s /path/to/rustemd /usr/local/bin/rustemd
ln -s /path/to/rustemd /usr/local/bin/systemctl
```

`rustemd` (invoked without `daemon`) is the `systemctl`-compatible CLI, so
running it via the symlink gives you a drop-in `systemctl`.

### Shell completions

Generate completion scripts for your shell. Completions follow the invoked
binary name, so `rustemd completions …` and (via the symlink)
`systemctl completions …` both work:

```sh
rustemd completions bash        > ~/.bash_completion.d/rustemd
rustemd completions fish        > ~/.config/fish/completions/rustemd.fish
rustemd completions zsh         > ~/.zsh/completions/_rustemd
rustemd completions powershell   # pipe into Register-ArgumentCompleter
rustemd completions nushell
```

## Homebrew

On Linux (e.g. Bazzite) or macOS, install from a tap without layering via
`rpm-ostree`:

```sh
brew tap fralalonde/rustemd https://github.com/fralalonde/rustemd
brew install rustemd
```

The formula (`Formula/rustemd.rb`) installs `rustemd` and `rustemd-tui` plus
shell completions. It deliberately does **not** create a `systemctl` symlink —
on a systemd host that would shadow the real `systemctl` in your PATH. To
exercise the drop-in CLI, symlink it into a directory you keep ahead of
`/usr/bin` only while testing:

```sh
ln -s "$(brew --prefix)/bin/rustemd" ~/.local/bin/systemctl
```

Refresh the formula's pinned sha256 after each release with
`scripts/gen-brew-formula.sh <version>` (run once the release assets are
published).

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
- `[Unit]` / `[Service]` / `[Install]` / `[Timer]` sections
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

**Lifecycle & supervision**
- Dependency graph (start order from `After`/`Requires`, stop order reversed)
- Process groups as the supervision boundary; `kill(-pgid, …)` reaches the
  whole tree, SIGTERM then SIGKILL
- Subreaper for daemonizing (`forking`) services
- `systemctl start/stop/restart/reload/kill/status/is-active/is-failed`
- `enable` / `disable` / `is-enabled` via `[Install]` symlinks
  (`WantedBy=`, `RequiredBy=`, `Alias=`, `Also=`)

**Control surfaces**
1. `systemctl`-compatible CLI
2. JSON-over-unix-socket IPC (the `rustemd daemon` listens here)
3. A **programmatic `Control` API** (library alternative to the CLI/D-Bus) —
   see below

---

## Programmatic control API

The library exposes a `Control` trait implemented two ways — an **in-process**
[`Manager`] and a **remote** [`SocketClient`] — so callers can hold a
`&mut dyn Control` without caring which one they have. This is the intended
alternative to shelling out to `systemctl` or speaking D-Bus.

```rust
use rustemd::control::{Control, SocketClient};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Talk to a running daemon over the socket (like `systemctl` does).
    let mut ctl = SocketClient::for_mode(false /* system */)?;

    ctl.start(&["myapp.service"])?;
    for status in ctl.status(&["myapp.service"])? {
        println!("{}: {} ({})", status.name, status.active, status.sub);
    }
    ctl.stop(&["myapp.service"])?;
    Ok(())
}
```

An in-process manager implements the same trait:

```rust
use rustemd::control::Control;
use rustemd::manager::{Manager, ManagerCfg};

let mut mgr = Manager::new(ManagerCfg::for_mode(false)?)?;
mgr.load_all();
mgr.start(&["myapp.service"])?;      // Control::start
```

Typed request/response structs — [`UnitStatus`], [`UnitSummary`],
[`TimerInfo`], [`UnitFileInfo`] — are `Serialize`/`Deserialize` and are the
same types the wire protocol carries.

---

## Architecture

This is a Cargo **workspace** with two crates: `rustemd` (the library,
the `rustemd` daemon, and the `systemctl`-compatible CLI) and
`rustemd-tui` (the terminal client, which depends on the library's
`Control` API).

| Module | Responsibility |
| --- | --- |
| `unit` | Unit-file parser + typed unit configuration |
| `calendar` / `timespan` | systemd calendar expressions & time spans |
| `manager` | The daemon: unit table, state machine, process supervision, timers |
| `manager::ops` | Typed operations shared by IPC and the `Control` API |
| `manager::deps` | Dependency graph (start/stop ordering) |
| `manager::timer` | Cancelable timer wheel |
| `platform` | OS-specific surface: `process` (spawn/kill/reap), `signals` (signalfd), `net` (unix sockets) |
| `ipc` / `client` | JSON wire protocol + client |
| `control` | The `Control` trait + in-process/remote implementations |
| `cli` | `systemctl`-compatible command surface |
| `enable` | `[Install]` symlink management |
| `paths` | System vs. user filesystem layout |

**Portability.** All raw Linux/unix syscalls live in `platform/` behind small,
documented functions. Porting to a new OS means reimplementing those three
submodules, not auditing the manager. `unsafe` is confined to two justified
sites: the `pre_exec` closure in `platform::process` (the API requires it) and
a one-line `prctl(PR_SET_CHILD_SUBREAPER)`.

---

## Testing

```sh
cargo test            # 50 unit tests + 2 end-to-end daemon tests
cargo clippy -- -D warnings
cargo fmt --check
```

Integration tests (`tests/e2e.rs`) boot the real manager in a thread and drive
it over the socket with the `Control` API — starting/stopping real processes,
checking lifecycle states, and round-tripping `enable`/`disable` — against a
scratch filesystem via the `RUSTEMD_*` env hooks.

---

## Limitations (intentional, for now)

- **cgroup v2 (Linux)** — one cgroup per unit for reliable tree cleanup and
  `MemoryMax`/`MemoryHigh`/`CPUWeight`/`TasksMax`; falls back to process groups
  + a subreaper where cgroups aren't available.
- **PID-1 boot is opt-in** — the `boot` cargo feature adds mounting
  `/proc`/`/sys`/`/dev`/`/run`/`cgroup2`, early-boot config (hostname, sysctl,
  modules, fstab), template units (`getty@tty1`), and `reboot(2)` power-off.
  Off by default — a container runtime does this for you. Test it with
  `scripts/ns-boot-test.sh` (unprivileged namespaces, no qemu) or
  `scripts/vm-test.sh` (qemu + initramfs).
- **Linux/unix only** — `platform/` is `#[cfg(unix)]`; Windows/Mac are planned.
- No socket activation, D-Bus, journald, or `systemd-analyze`-style tooling.
- `notify` types are supported (`NOTIFY_SOCKET`), but `sd_notify`'s watchdog is
  not yet wired.

See [`docs/handbook.md`](docs/handbook.md) for worked examples.

## License

MIT
