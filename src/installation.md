# Installation

## From source

```sh
cargo build --release
# binaries land in target/release/: rystemd, rystemctl, rystemd-tui
```

The workspace has three binaries:

- **`rystemd`** — the unit-manager daemon.
- **`rystemctl`** — the `systemctl`-compatible control CLI.
- **`rystemd-tui`** — a live terminal client for a running manager.

## Homebrew (Linux, Fedora/Bazzite)

`rystemd` is dogfooded on Fedora via a Homebrew tap. On an immutable host
(Bazzite/Fedora Atomic) install the CLI through brew — not `rpm-ostree`:

```sh
brew tap rystemd/rystemd
brew install rystemd
```

`rustemd` is Linux-native, so this searches only the Linux formula.

## Feature flags

`rystemd` is a library + binary crate. Its Cargo features are:

| Feature | Default | Purpose |
| --- | --- | --- |
| `socket` | yes | `.socket` units, `Listen*=` socket activation |
| `udev` | yes (Linux) | kernel device discovery (netlink + sysfs) |
| `dbus` | yes (Linux) | `org.rystemd.Manager1` + systemd1 D-Bus surface |
| `boot` | no | PID-1 boot (mount API fs, early boot, poweroff) |

Build a PID-1 image with `--features boot`.

## Verification

```sh
rystemd daemon --user        # run a per-user manager
rystemctl --user status      # ask it for unit status
```