# Real-host boot validation: rystemd as init in a VM

Two ways to boot a real distro root with rystemd as init, and how to run each.
They differ in **who performs the pivot** out of the initramfs:

- **Model A — rystemd is the *post-pivot* init.** The stock initramfs (dracut
  / systemd-in-initrd) mounts the root and pivots into it, then hands PID 1 to
  rystemd. Fast, de-risks "does rystemd boot a real root to a login prompt".
- **Model B — rystemd *is* the initramfs init** and pivots into `/sysroot`
  itself. Exercises rystemd's own `switch_root`; needs rystemd to mount the
  deployment.

The build below targets **Model A first** (cheap, proves the real-init boot),
then **Model B** once the ostree-mount tooling lands.

> Requires a **rootful Fedora host** with `libguestfs-tools`, `qemu-kvm`, and
> the network. The rystemd dev machine runs this project's tooling in an
> unprivileged toolbox (no loop mounts / no libguestfs), so the VM cannot be
> built *here* — run the scripts below on the rootful machine.

---

## What both models need in common

1. **The rystemd RPM** (the image's base): `rystemd-<ver>-1.x86_64.rpm` from the
   [release](https://github.com/rystemd/rystemd/releases).
2. **A base image.** Fedora *Cloud* (a normal root — simplest Model A) or an
   ostree/Atomic image (Silverblue / CoreOS — same post-pivot path, dracut's
   ostree module does the deployment staging either way).
3. **SELinux disabled** on that image (`enforcing=0` or a permissive kernel
   arg), because Fedora's stock policy assumes systemd's `init_t` and our SELinux
   module is not yet live-iterated. The goal is the *login prompt*, not an
   enforcing fight.
4. **An init wrapper.** The kernel `init=` cmdline names a single path with no
   args; rystemd needs the `daemon` subcommand. Install a tiny `/sbin/init`
   shim that `exec`s `rystemd daemon`:

   ```sh
   #!/bin/sh
   exec /usr/bin/rystemd daemon "$@"
   ```
   The stock initramfs (Model A) is told `init=/sbin/init` via its own boot
   config and will exec this shim after pivoting.

5. **A console getty + trimmed target.** Fedora's full `default.target` pulls a
   heavy graph (logind, dbus, journald). A console login needs agetty on `tty1`
   and little else. Ship a minimal `getty@.service` (as the initramfs builder
   does) and a slim `default.target` that just Wants it:

   ```ini
   # /etc/systemd/system/getty@.service
   [Unit]
   Description=Getty on %i
   [Service]
   Type=idle
   ExecStart=-/sbin/agetty -o '-p -- \\u' --noclear - linux %I
   [Install]
   WantedBy=getty.target
   ```

> `--noclear` and `agetty -o` flags: rystemd's getty unit in `build-initramfs.sh`
> uses a plain busybox getty; a Fedora root has real `agetty`, so use the real
> args above.

---

## Run it (Model A)

On the rootful Fedora host:

```sh
sudo dnf install -y libguestfs-tools qemu-kvm

# 1. layer rystemd + init shim + getty into a Fedora Cloud image
sudo bash scripts/prepare-realinit-vm.sh \
    --base Fedora-Cloud-Base-<rel>-<arch>.qcow2 \
    --rpm rystemd-0.1.5-1.x86_64.rpm \
    --out rystemd-vm.qcow2

# 2. boot to a serial console, watch for the login prompt
sudo bash scripts/boot-realinit-vm.sh rystemd-vm.qcow2
```

Expected: kernel boots → initramfs mounts the root and pivots → the init shim
execs `rystemd daemon` → `default.target` starts → `login:` appears on the
console (ttyS0). Log in as `root` (or the user the image configures) — no
graphics, just a console login, exactly the milestone.

### What "success" means

- `login:` prompt on the serial console → rystemd booted a **real distro root**
  as init to a usable console. That's the de-risk win.
- `rystemd: ... manager started` on the console = PID 1 took over.
- `rystemctl list-units` / `rystemd-tui` (if installed) from that console
  talk to the running PID-1 manager.

---

## Then: Model B (the real handoff)

Model B is the same VM but the **initramfs itself is rystemd's**, and
`prepare` swaps the image's dracut init for the deployment-mount + switch_root
that `test-handoff.sh` exercises. That path is gated on rystemd's own
**ostree-deployment mount** (block-device discovery, btrfs `root` subvol,
`/var`+`/etc` bind/overlay) — see the Model B work in
`rystemd/src/platform/boot.rs` and `scripts/test-handoff.sh`.

The runnable `prepare-realinit-vm.sh` / `boot-realinit-vm.sh` scripts below are
written for Model A; Model B is the follow-up once the mount code ships.