# Scoop manifests for rystemd

This directory holds the [Scoop](https://scoop.sh) (Windows package manager)
manifest for rystemd. It is not yet in the official
`ScoopInstaller/Main` bucket, so install it from this repo directly:

```powershell
# From this repository's root
scoop install scoop\rystemd.json

# Or add a bucket pointing at the rystemd repository, then:
#   scoop bucket add rystemd https://github.com/rystemd/rystemd
#   scoop install rystemd
```

This installs three commands onto your PATH:

- `rystemd` — the manager daemon (run it as a service/first).
- `rystemctl` — the `systemctl`-compatible control CLI.
- `rystemd-tui` — the terminal UI.

The manifest pulls the portable zip from the [latest GitHub
release](https://github.com/rystemd/rystemd/releases) and will auto-update to
the newest tagged version (`checkver`/`autoupdate`). The pinned hash in
`architecture.64bit` is regenerated automatically by `scoop update` on each
release.

For a Windows installer rather than a portable binary, see the
`rystemd-<ver>-x86_64.msi` asset on the same release page.