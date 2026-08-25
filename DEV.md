## Live environment

To drive rustemd interactively as a real PID-1 daemon in qemu (demo units for
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
`rustemd/build.rs`); `Cargo.toml`/`Cargo.lock` are kept in sync by `release.sh`
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
wsl.exe -e bash -lc 'cd /mnt/d/path/to/rustemd && cargo test --workspace'
```

Use a separate `CARGO_TARGET_DIR` if a crashed Windows integration test still
has an old test executable open; Windows will not relink over a running image.
