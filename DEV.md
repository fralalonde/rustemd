## Live environment

To drive rustemd interactively as a real PID-1 daemon in qemu (demo units for
every unit type, getty shell, etc.), see **[DEMO.md](DEMO.md)** and
`scripts/live-vm.sh`.

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
| `x86_64-pc-windows-msvc`     | `windows-2022`     | zip, msi      |
| `aarch64-apple-darwin`       | `macos-14`         | tar.gz, dmg   |

The package version comes from the git tag at build time (see
`rustemd/build.rs`); `Cargo.toml`/`Cargo.lock` are kept in sync by `release.sh`
at release time. Linux packaging lives in `scripts/package-linux.sh` +
`packaging/`.
