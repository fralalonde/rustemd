# rystemd

A cross-platform, systemd-compatible service manager in Rust.

## Scope

- Linux PID 1 and standalone service-manager modes
- Windows service-manager mode
- `.service`, `.socket`, `.timer`, `.target`, `.mount`, `.path`, and runtime `.device` units
- Dependency ordering, process supervision, restart policy, timers, socket activation, mounts, and journals
- `systemctl`-compatible CLI, TUI, JSON-line IPC, and typed Rust control API

Compatibility is behavioral and selective. Unsupported surfaces are listed in
[KNOWN_ISSUES.md](KNOWN_ISSUES.md).

## Releases

Released binaries and package installation procedures are documented in the
[handbook](https://rystemd.github.io/) and [docs/install.md](docs/install.md).

Supported Linux release lanes:

- GNU Linux artifacts for Fedora, Debian, and other glibc systems
- Alpine APKs for `x86_64` and `aarch64` musl systems
- Static musl binaries for Alpine and live PID 1 images

Alpine installation:

```sh
apk add --no-cache --allow-untrusted --force-non-repository \
  https://github.com/rystemd/rystemd/releases/latest/download/rystemd-x86_64.apk
```

Use `rystemd-aarch64.apk` on `aarch64`. The current APK releases are unsigned.

## TUI

`rystemd-tui` provides a terminal interface for service state and control.

![rystemd-tui demo](docs/demo.gif)

## Demo

The interactive live VM boots rystemd as PID 1 with demo units for the supported
unit types. See [DEMO.md](DEMO.md).

## Development

Source layout, test organization, live environments, and release procedure are
documented in [DEV.md](DEV.md).

## License

MIT
