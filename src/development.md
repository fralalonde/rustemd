# Development

## Build & test

```sh
cargo build                 # debug
cargo test --workspace      # unit + integration
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The release profile is tuned for small, audit-friendly binaries:
`opt-level = "z"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`,
`strip = true`.

## Features

Build the full Linux surface (`socket` + `udev` + `dbus`) with `--all-features`,
or add `boot` for the PID-1 path. The D-Bus integration test
(`rystemd/tests/dbus.rs`) spins up a private `dbus-daemon` and exercises the
`org.freedesktop.systemd1` surface end-to-end. Privileged tests
(`rystemd/tests/privileged.rs`) cover `User=` privilege dropping and
`PrivateTmp=` mount namespaces and need real root (CI runs them via `sudo`).

## Releases

Versioning follows the Git tag via `build.rs` (the tag is the source of truth;
no version string is hardcoded). Tag and push with:

```sh
./release.sh patch          # prompts before pushing
./release.sh patch -y       # tag + push in one step
```

The GitHub Actions release workflow builds Linux (`x86_64`, `aarch64`) and
Windows packages, runs the full test gate (including the D-Bus and
privileged suites), and attaches the artifacts to a GitHub Release. Windows
packages: portable zip + MSI. Linux: tar.gz, .deb, .rpm.