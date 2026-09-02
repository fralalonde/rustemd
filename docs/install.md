# Installing released binaries

Release assets are published on the [GitHub releases page](https://github.com/rystemd/rystemd/releases). Select the package matching both the distribution and libc implementation.

## Alpine Linux

The APK lane targets Alpine `x86_64` and `aarch64` with musl.

### x86_64

```sh
apk add --no-cache --allow-untrusted --force-non-repository \
  https://github.com/rystemd/rystemd/releases/latest/download/rystemd-x86_64.apk
```

### aarch64

```sh
apk add --no-cache --allow-untrusted --force-non-repository \
  https://github.com/rystemd/rystemd/releases/latest/download/rystemd-aarch64.apk
```

The package contains `rystemd`, `rystemctl`, `rystemd-tui`, a `systemctl`
compatibility symlink, and shell completions. It does not disable OpenRC, alter
the bootloader, or configure rystemd as PID 1.

The current APK lane is unsigned. `--allow-untrusted` is required. Pin a
versioned release URL for reproducible deployment.

Run a user manager explicitly:

```sh
rystemd daemon --user
rystemctl --user list-units
```

Machine-init experiments belong in the VM procedures. Keep the stock init
recoverable from the bootloader.

## Fedora, Debian, and other GNU Linux systems

Use the GNU release artifacts for glibc-based distributions. RPM and DEB assets
are published for the supported `x86_64` and `aarch64` Linux lanes. Package
commands and release-specific URLs are maintained in the handbook.

## Source builds

Source builds, test organization, live VMs, and release procedure are documented
in [DEV.md](../DEV.md).
