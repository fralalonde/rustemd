# Running rystemd as the boot init on Fedora Silverblue

**Short version:** the blocking piece — pivoting out of the initramfs into a
real ostree deployment — is now **implemented** behind the `boot` feature.
Something is still missing before rystemd can *fully* replace systemd for daily
use on a real Silverblue desktop: **SELinux policy labeling** and **launching a
graphical session / display manager**. Until those land and are validated on a
real boot, treat real-Silverblue replacement as **experimental** — test on a
throwaway VM first, and keep systemd recoverable.

Use this document to (a) understand exactly what is and isn't supported, and
(b) run rystemd as PID 1 the *supported* way — initramfs/VM or throwaway host —
so you can exercise every boot behavior without risking a machine you use.

---

## 1. The real-root handoff (implemented, `boot` feature)

When rystemd is PID 1 inside an initramfs and a real deployment is staged at
`/sysroot` (an ostree/dracut initramfs mounts it before exec'ing stage-2),
rystemd now performs the classic `switch_root(8)` sequence **before** reading
any config:

```
chdir("/sysroot"); mount(".", "/", MS_MOVE); chroot("."); chdir("/");
re-exec `rystemd daemon`   → boots the real default.target from the real /etc
```

If `/sysroot` is absent (a `--user` run, a container, or the self-contained
initramfs harness), the handoff is skipped and rystemd boots in place. So the
existing `build-initramfs.sh` / `live-vm.sh` / `vm-test.sh` flow is unchanged —
the handoff only fires when there really is a deployment to take over.

### Verified

- Detection logic (`in_initramfs`, `sysroot_mounted`) is unit-tested.
- The existing namespace PID-1 boot (`scripts/ns-boot-test.sh`) still passes —
  the handoff correctly skips when there is no `/sysroot`.
- **Not yet boot-verified end-to-end.** A real handoff needs an actual
  initramfs→deployment boot (qemu/KVM via `vm-test.sh`). This is the
  recommended next step before trusting it on hardware.

## 2. What `boot` does and doesn't do (after this change)

| Capability | Status |
|---|---|
| Mount API/virtual filesystems (`/proc /sys /dev /dev/pts /dev/shm /run /tmp /sys/fs/cgroup`) | ✅ idempotent, best-effort |
| Early-boot config: hostname, machine-id, sysctl, module load, random-seed, tmpfiles runtime dirs, `/etc/fstab` | ✅ best-effort per step |
| **Real-root handoff**: pivot out of initramfs into a real `/sysroot` deployment + re-exec | ✅ **new** — `switch_root(8)` sequence |
| Boot `default.target`, supervise units, socket/timer activation | ✅ |
| `reboot(2)` / `poweroff(2)` as PID 1 on shutdown | ✅ |
| ostree deployment *mounting* (block device + btrfs subvol discovery own) | ❌ relies on the upstream initramfs staging `/sysroot` |
| SELinux policy labels | ❌ none |
| Launch GNOME / display manager / `graphical.target` | ❌ boots to getty/multi-user only |
| `dbus-daemon`, `journald`, NetworkManager, FirewallD supervision as units | ⚠️ only as unit files you provide |

The code is explicit that this is the *host-init / initramfs* surface, framed in
the handbook as "the VM-first / initramfs path **toward** a drop-in init." The
new handoff removes the biggest blocker; the remaining gaps are a coherent unit
graph and SELinux, not a missing mechanism.

---

## 3. Why a real ostree boot is handed to rystemd today — and what's still open

A Silverblue boot is: **UEFI firmware → GRUB/systemd-boot → kernel + dracut
initramfs → dracut mounts the ostree *deployment* at `/sysroot`, sets up
`/var`/`/etc` overlays → `exec`s the stage-2 init as PID 1 in that pivoted
root.**

rystemd can now take the stage-2 init seat when `/sysroot` is already staged.
What it still **cannot** do on a real host, honestly:

1. **Mount the deployment itself.** A dracut/ostree initramfs mounts the block
   device, decrypts LUKS, and sets up btrfs subvols / `/var`/`/etc` overlays.
   rystemd expects that staging done upstream (`/sysroot` prepared). It does
   not yet parse `root=`/`rd.*` and mount the deployment on its own.
2. **SELinux enforcing.** Silverblue enforces SELinux; rystemd creates no policy
   labels, so enforcement would deny much of the boot. No label calls exist.
3. **Start the graphical stack** (`gdm`, `gnome-shell`, PipeWire…) the way the
   GNOME session expects. Only a getty is provided by the repo units.

Consequence if you try a real desktop today: rystemd *will* switch root into
your ostree deployment and boot it as PID 1 — but multi-user/getty is the
terminus; no GNOME, and SELinux enforcement will likely deny critical steps.
That is **not yet safe for a primary daily-driver desktop**.

**Recovery if you experiment:** from the bootloader, edit the GRUB entry to
`init=/usr/lib/systemd/systemd` (or `rdinit=/lib/systemd/systemd`) — but being
stuck in a boot loop is not a fun way to learn that, so test in a VM first.

---

## 4. Supported: rystemd as PID 1 in an initramfs (qemu / throwaway host)

This is the real, tested way to run rystemd as the boot init. It boots its own
self-contained initramfs as PID 1, drops you into a getty on `/dev/ttyS0`, and
you drive everything with `rystemctl`.

### Prerequisites (on your Silverblue host, in a toolbox/container or via brew)

```sh
# Rust toolchain (a toolbox or Fedora container is most convenient):
rpm-ostree install rust cargo       # or use a `toolbox enter` with dnf install rust cargo

# Build the PID-1 binary WITH the boot feature (release CI also builds this way):
cd ~/Code/rustemd
cargo build --release --features boot
#   → target/release/rystemd (manager)
#   → target/release/rystemctl (control CLI, used from the getty shell)

# Tooling the harness needs on the host:
#   qemu-system-x86_64, a static busybox, cpio, gzip, and a kernel image.
sudo dnf install -y qemu-system-x86 busybox cpio gzip   # in the toolbox/container
# Kernel: the harness auto-discovers /boot/vmlinuz-* or /usr/lib/modules/*/vmlinuz.
```

### Interactive boot (qemu, serial console to your terminal)

```sh
scripts/live-vm.sh                    # interactive: getty shell on /dev/ttyS0
```

At the getty you can run real commands against the PID-1 manager:

```sh
rystemctl list-units
rystemctl status demo.service demo.mount demo.socket demo.timer demo.target
rystemctl start demo.mount && ls /mnt/demo
printf 'hi\n' | nc 127.0.0.1 8080      # socket-activates demo-echo.service
rystemd-tui                            # TUI over the serial console
```

Quit with `rystemctl poweroff` (or `Ctrl-A x` to force-exit qemu).

### One-shot automated boot (for scripts / assertions)

```sh
scripts/vm-test.sh                     # boots, runs boottest, asserts, powers off
```

### Under the hood

`scripts/build-initramfs.sh` packs busybox (sh/getty/mount/ip…), the rystemd
binaries plus their dynamic libs, a set of unit files, and a `/init` that mounts
the API filesystems then `exec /usr/bin/rystemd daemon`. The kernel is launched
with `rdinit=/init`. All mounts are idempotent/best-effort (same semantics as
the `boot` feature's own `mount_api_filesystems`).

For a **throwaway real host/VM** (not Silverblue, e.g. a plain Fedora/ARCH VM you
don't care about), the same initramfs is bootable: put `initramfs.cpio.gz` +
`vmlinuz` on the bootloader/KVM cmdline with `rdinit=/init`. Treat it strictly
as disposable — it has a minimal root, not a distro.

---

## 5. What remains for a real "replace systemd on Silverblue" goal

The real-root handoff closes the single biggest blocker. What remains for a
genuine drop-in on a real ostree host is shorter and mostly not about the boot
mechanism:

1. **✅ Real-root handoff** — pivoting out of the initramfs into a staged
   `/sysroot` deployment is implemented (this change; `boot` feature). Not yet
   boot-verified end-to-end on hardware — do that in a VM.
2. **ostree mount tooling (optional)**: parse `root=`/`rd.*` and mount the
   deployment + btrfs subvols / `/var`/`/etc` overlays ourselves, rather than
   relying on the upstream initramfs staging `/sysroot`. Possibly `libostree`
   bindings; honor `.ostree` xattrs/fsverity.
3. **SELinux**: ship a policy module and set exec/printf contexts so enforcing
   mode permits boot. (missing)
4. **Unit ecosystem**: provide units for `dbus`, `journald` (or rystemd's
   journal), `systemd-logind`, `gdm` → `graphical.target`, `NM`, `FirewallD`,
   `flatpak`, `ostree-finalization`, etc., matching Silverblue's dependency
   graph. (partial — getty/demo units only)
5. **Bootloader integration**: a post-ostree hook to install/deploy the
   initramfs and override the BLS `init=` while keeping each deployment's own
   systemd recoverable (a `default`-deployment-independent entry, plus failsafe
   fallback to `systemd`).

Until #2–#5 land, keep rystemd as PID 1 in the VM harness — that's exactly what
it's built and tested for, and it's a genuinely useful system.