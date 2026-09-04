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

## Experimental: rystemd as PID 1 on an rpm-ostree system

This is a boot experiment, not a supported desktop configuration. The current
implementation is suitable for a reduced multi-user or console target. SELinux
labeling, graphical sessions, the complete Fedora unit graph, and independent
ostree deployment mounting remain incomplete. Test in a VM or on a disposable
deployment.

The safe shape is:

1. Install rystemd into a new ostree deployment.
2. Add an init wrapper without changing the existing `/sbin/init` link.
3. Add a separate boot entry or one-time kernel edit containing `init=`.
4. Keep the original deployment and boot entry unchanged.

Do not overwrite `/sbin/init`, `/usr/lib/systemd/systemd`, or
`/usr/bin/systemctl`. A global replacement makes recovery depend on repairing
the filesystem instead of selecting the previous boot configuration. Do not
replace `/usr/bin/systemctl` with a symlink. The released RPM intentionally
ships no systemd-name symlink for this reason.

### Prerequisites

- A working rpm-ostree deployment with a visible previous boot entry.
- Console access through the bootloader. A graphical-only recovery path is not
  sufficient.
- A local console or out-of-band method for selecting the previous deployment.
- A rystemd release RPM matching the architecture and glibc ABI.
- A reduced unit graph, including a console getty. The stock Fedora
  `default.target` graph is not a compatibility target.
- SELinux disabled for the experimental entry, for example with `enforcing=0`.
  This does not alter the policy or relabel the deployment.

Record the current deployment before changing anything:

```sh
rpm-ostree status
ostree admin status
```

### Option A: layer the release RPM

Layering creates a new deployment. It does not modify the currently booted
deployment:

```sh
rpm-ostree install ./rystemd-<version>-1.x86_64.rpm
rpm-ostree status
```

Use the exact published RPM path and architecture. Reboot only after the new
deployment is visible in `rpm-ostree status`.

### Option B: install with Homebrew

Homebrew is acceptable for a disposable or development deployment when the
binary is available before stage 2 starts:

```sh
brew install rystemd
command -v rystemd
rystemd --version
```

The init wrapper below must reference the resolved absolute path. Homebrew
prefixes are architecture- and installation-specific. A brew install alone
does not modify the ostree boot entry and does not make rystemd PID 1.

### Create an init wrapper

Create this wrapper in the mutable `/etc` tree of the deployment. The wrapper
is separate from the stock init path and supplies the subcommand required by
rystemd:

```sh
sudo install -d -m 0755 /etc/rystemd
sudo tee /etc/rystemd/init >/dev/null <<'EOF'
#!/bin/sh
exec /usr/bin/rystemd daemon "$@"
EOF
sudo chmod 0755 /etc/rystemd/init
```

For Homebrew, replace `/usr/bin/rystemd` with the absolute path printed by
`command -v rystemd`. Do not use a path under a transient build directory.

The wrapper must be present in the deployment selected by the experimental
boot entry. Verify it before rebooting:

```sh
test -x /etc/rystemd/init
test -x /usr/bin/rystemd || command -v rystemd
```

### Compatibility links and command paths

The portable tarball and Alpine APK provide `systemctl -> rystemctl`. The RPM
does not install that link globally. If units in the experimental graph invoke
`systemctl` by command lookup, use a private compatibility directory:

```sh
sudo install -d -m 0755 /etc/rystemd/bin
sudo ln -sfn /usr/bin/rystemctl /etc/rystemd/bin/systemctl
```

Add `/etc/rystemd/bin` before the normal system paths in the environment of
the experimental units or wrapper. Do not replace `/usr/bin/systemctl`; stock
systemd recovery and package scripts must retain their original command.
Units containing an absolute `/usr/bin/systemctl` require conversion or a
separate test fixture. A symlink cannot safely override an absolute path.

### Add the experimental kernel argument

The stock initramfs must continue staging the ostree deployment. Override the
stage-2 init only. The important argument is:

```text
init=/etc/rystemd/init enforcing=0 systemd.unit=multi-user.target
```

Do not replace `rdinit=/init` in the stock initramfs unless a separately built
rystemd initramfs is being tested. `rdinit` selects the initramfs program;
`init` selects the post-pivot PID 1.

Prefer a one-time bootloader edit for the first test. In the boot menu, edit
the selected entry, append the arguments above to its existing kernel command
line, and boot it. Do not edit the existing deployment's `/sbin/init` link.

For a persistent experiment, create a new boot entry or a new ostree
deployment-specific kernel-argument configuration. Preserve the original BLS
entry and its original kernel command line. Confirm the resulting entry with:

```sh
rpm-ostree status
cat /proc/cmdline
```

`cat /proc/cmdline` is checked after boot. It must show the intended `init=`
and `enforcing=0` arguments.

### First boot checks

Expected early output includes `manager started` and a console `login:` prompt.
At the console, verify:

```sh
test "$(ps -o pid= -p 1 | tr -d ' ')" = 1
rystemctl list-units
rystemctl status default.target
```

Start with a console-only target. Do not enable graphical services, Network
Manager integration, or broad service activation until each dependency has
been tested under rystemd.

### Reversal

The intended reversal requires no filesystem repair:

1. Reboot from the console or hardware reset path.
2. Select the previous boot entry or previous ostree deployment.
3. Confirm that `/proc/1/comm` reports the stock systemd process.

The previous entry must retain its original `init=` setting, normally
`/usr/lib/systemd/systemd`. If the experimental entry loops or fails before a
console appears, interrupt the bootloader and select that previous entry. Do
not delete the new deployment until the stock entry has been booted and
verified. After recovery, remove the experimental deployment with the normal
`rpm-ostree cleanup` workflow if desired.

Selecting the previous entry is the rollback mechanism. A global init symlink,
an overwritten `/usr/bin/systemctl`, or an in-place initramfs replacement is
outside this reversible procedure.

## Source builds

Source builds, test organization, live VMs, and release procedure are documented
in [DEV.md](../DEV.md).
