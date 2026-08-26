## Architecture

This is a Cargo **workspace** with three crates: `rystemd` (the library and
the PID-1 `rystemd` daemon), `rystemctl` (the `systemctl`-compatible control
CLI, which depends on the library), and `rystemd-tui` (the terminal client,
which depends on the library's `Control` API).

| Module | Responsibility |
| --- | --- |
| `unit` | Unit-file parser + typed unit configuration |
| `calendar` / `timespan` | systemd calendar expressions & time spans |
| `manager` | The daemon: unit table, state machine, process supervision, timers, socket activation, mounts, udev devices |
| `manager::ops` | Typed operations shared by IPC and the `Control` API |
| `manager::deps` | Dependency graph (start/stop ordering) |
| `manager::timer` | Cancelable timer wheel |
| `platform` | OS-specific surface: process supervision, shutdown events, IPC, filesystem links, SCM hosting (Windows), mounts/udev (Linux) |
| `dbus` | zbus bridge for the `org.rystemd.Manager1.Manager` interface (Linux, opt-in `dbus` feature) |
| `ipc` / `client` | JSON wire protocol + client |
| `control` | The `Control` trait + in-process/remote implementations |
| `daemon` | The PID-1 manager entry point (`rystemd daemon`) |
| `enable` | `[Install]` symlink management |
| `paths` | System vs. user filesystem layout |

The `systemctl`-compatible CLI lives in the separate `rystemctl` crate (its
`cli` module), which consumes the library's `client`/`paths`/`enable`/
`cli_style`/`names` modules.

**Portability.** Raw OS operations live under `platform/`: Unix uses `nix`,
while Windows uses direct `windows-sys` bindings for named pipes, Job Objects,
console controls, Winsock polling, and SCM hosting. The manager retains one
state machine with platform-specific event-loop adapters.

---

## Programmatic control API

The library exposes a `Control` trait implemented two ways — an **in-process**
[`Manager`] and a **remote** [`SocketClient`] — so callers can hold a
`&mut dyn Control` without caring which one they have. This is the intended
alternative to shelling out to `systemctl` or speaking D-Bus.

```rust
use rystemd::control::{Control, SocketClient};

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
use rystemd::control::Control;
use rystemd::manager::{Manager, ManagerCfg};

let mut mgr = Manager::new(ManagerCfg::for_mode(false)?)?;
mgr.load_all();
mgr.start(&["myapp.service"])?;      // Control::start
```

Typed request/response structs — [`UnitStatus`], [`UnitSummary`],
[`TimerInfo`], [`UnitFileInfo`] — are `Serialize`/`Deserialize` and are the
same types the wire protocol carries.

---

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# From Windows, run the Linux suite too when WSL is available:
wsl.exe -e bash -lc 'cd /mnt/d/path/to/rystemd && cargo test --workspace'
```

Integration tests boot the real manager and drive it over the socket —
`rystemd/tests/e2e.rs` uses the programmatic `Control` API, and
`rystemctl/tests/cli.rs` runs the compiled `rystemctl` binary against the
daemon — both against a scratch filesystem via the `RYSTEMD_*` env hooks.

---

## Live environment

To drive rystemd interactively as a real PID-1 daemon in qemu (demo units for
every unit type, getty shell, etc.), see **[DEMO.md](DEMO.md)** and
`scripts/live-vm.sh`.

## Known issues

Categorized bugs / weaknesses / design holes live in
**[KNOWN_ISSUES.md](KNOWN_ISSUES.md)** — the input for the upcoming security
inspection.

## Releasing

```bash
./release.sh <major|minor|patch> [--push]
```

- performs basic sanity checks 
- bumps the semantic version
- tags the repo with the new version
- optionally pushes upstream to trigger a release build

Git tags are the source of truth for versioning; the build files (`Cargo.toml`,
`Cargo.lock`) are synced as a side effect of the release commit.

## CI

GitHub Actions (`.github/workflows/release.yml`) builds and packages release
artifacts on every `v*` tag push:

| Target                       | Runner             | Packages      |
|------------------------------|--------------------|---------------|
| `x86_64-unknown-linux-gnu`   | `ubuntu-22.04`     | tar.gz, deb, rpm |
| `aarch64-unknown-linux-gnu`  | `ubuntu-24.04-arm` | tar.gz, deb, rpm |
| `x86_64-pc-windows-msvc`     | `windows-2022`     | tests, zip, msi |
| `aarch64-apple-darwin`       | `macos-14`         | tar.gz, dmg   |

The package version comes from the git tag at build time (see
`rystemd/build.rs`); `Cargo.toml`/`Cargo.lock` are kept in sync by `release.sh`
at release time. Linux packaging lives in `scripts/package-linux.sh` +
`packaging/`.


## Windows development

The Windows manager uses direct `windows-sys` bindings rather than a POSIX
compatibility layer: Job Objects for process trees, named pipes for control
IPC, Winsock polling for TCP socket triggers, console control handlers, and
the Service Control Manager APIs.

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check

# Also protect the Unix implementation from regressions:
wsl.exe -e bash -lc 'cd /mnt/d/path/to/rystemd && cargo test --workspace'
```

Use a separate `CARGO_TARGET_DIR` if a crashed Windows integration test still
has an old test executable open; Windows will not relink over a running image.
