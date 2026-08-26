#!/usr/bin/env bash
# Interactive live environment: boot rystemd as PID 1 in qemu and hand the
# serial console straight to your terminal, so you can drive the daemon by
# hand. A busybox getty on /dev/ttyS0 gives you a shell where you can type
# `rystemctl list-units`, `rystemctl start demo.service`, `rystemctl status demo.mount`,
# `rystemd-tui`, and so on — against the real PID-1 manager.
#
# Unlike vm-test.sh (which redirects the console to a log and auto-powers-off
# for CI assertions), this script passes qemu's stdin/stdout through to your
# terminal (qemu -nographic, no redirection) and boots WITHOUT auto-poweroff.
# You quit cleanly by typing `rystemctl poweroff` at the getty shell, or force
# qemu to exit with Ctrl-A x.
#
# The initramfs installs the friendly demo units from examples/live/ (one of
# every unit type rystemd supports) and enables demo.target at boot. The demo
# .mount unit needs /mnt/demo to exist; /init creates it.
#
# Requires: qemu-system-x86_64, a kernel image, busybox, cpio, gzip.
# Usage: scripts/live-vm.sh [kernel]   (auto-discovers the kernel if omitted)
set -uo pipefail

# Kernel discovery: prefer an explicit $1, then the conventional distro
# locations — /boot/vmlinuz-* (Debian/Fedora with /boot mounted), then the
# /(usr)/lib/modules/*/vmlinuz trees (Fedora toolbox/container, ostree hosts).
KERNEL=${1:-}
if [ -z "$KERNEL" ]; then
  KERNEL=$(ls -1 /boot/vmlinuz-* 2>/dev/null | sort -V | tail -1)
fi
if [ -z "$KERNEL" ]; then
  KERNEL=$(ls -1 /lib/modules/*/vmlinuz /usr/lib/modules/*/vmlinuz 2>/dev/null | sort -V | tail -1)
fi
QEMU=${QEMU:-qemu-system-x86_64}
BUSYBOX=${BUSYBOX:-$(command -v busybox || true)}
BIN=${BIN:-./target/release/rystemd}
INITRD=./target/initramfs-live.cpio.gz
EXTRA_UNITS=${RYSTEMD_EXTRA_UNITS:-examples/live}

if [ -z "$KERNEL" ] || [ ! -r "$KERNEL" ]; then
  echo "error: no kernel found (pass one as \$1)" >&2; exit 2
fi
command -v "$QEMU" >/dev/null || { echo "error: $QEMU not found" >&2; exit 2; }
[ -n "$BUSYBOX" ] && [ -x "$BUSYBOX" ] || { echo "error: need busybox (set BUSYBOX=/path/to/busybox)" >&2; exit 2; }
[ -d "$EXTRA_UNITS" ] || { echo "error: demo units dir $EXTRA_UNITS not found" >&2; exit 2; }

# Always (re)build the PID-1 binary with the `boot` feature. Cargo is
# incremental, so this is a fast no-op when up to date — but it guarantees a
# stale default build (no `boot`) is never silently reused. The `boot`
# feature mounts the API filesystems itself; our /init also does it, so the
# initramfs is explicit. The `--features boot` at the workspace root applies
# `boot` to rystemd and is ignored by rystemctl/rystemd-tui.
echo "building $BIN (cargo build --release --features boot)..."
cargo build --release --features boot

# Build the live initramfs: demo units enabled at boot, no auto-poweroff.
export BUSYBOX RYSTEMD_EXTRA_UNITS="$EXTRA_UNITS" RYSTEMD_NO_BOOTTEST=1
scripts/build-initramfs.sh "$BIN" "$INITRD"

# KVM when /dev/kvm is usable, else TCG (software).
ACCEL=()
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  ACCEL=(-accel kvm)
fi

cat <<EOF
=== rystemd live env (kernel: $(basename "$KERNEL")) ===
  Boots rystemd as PID 1 and drops you into a getty shell on /dev/ttyS0.

  Try:
    rystemctl list-units
    rystemctl status demo.service demo.mount demo.socket demo.timer demo.target
    rystemctl start demo.mount && ls /mnt/demo
    rystemctl is-active demo.mount
    rystemctl list-timers
    printf 'hi\n' | nc 127.0.0.1 8080     # socket-activates demo-echo.service
    rystemctl status demo-echo.service
    rystemd-tui                            # renders over serial (see DEMO.md)

  Quit:  rystemctl poweroff   (or Ctrl-A x to force qemu to exit)
============================================================
EOF

# -nographic multiplexes the serial console onto stdio. We do NOT redirect it
# to a log: stdin/stdout pass straight through, which is what makes the session
# interactive. exec() so signals (Ctrl-C, Ctrl-A x) reach qemu directly.
exec "$QEMU" "${ACCEL[@]}" -m 512 -nographic -no-reboot \
  -kernel "$KERNEL" \
  -initrd "$INITRD" \
  -append "console=ttyS0 rdinit=/init panic=-1"
