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

GitHub Actions (`.github/workflows/release.yml`) builds release artifacts for targets on
every `v*` tag push:

| Target                    | Runner      |
|---------------------------|-------------|
| `x86_64-pc-windows-msvc`  | `windows-2022` |
| `x86_64-unknown-linux-gnu`| `ubuntu-22.04`|
| `aarch64-apple-darwin`    | `macos-14` (native ARM) |
