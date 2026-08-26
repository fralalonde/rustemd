#!/usr/bin/env bash
# Boot the initramfs in qemu with rystemd as PID 1 and assert: the manager
# starts, the getty template instance is active, and the machine powers off
# cleanly (via rystemd's reboot(2) poweroff).
#
# The serial console is streamed live to your terminal so you can watch the
# machine boot, greet, and shut down; it is also captured to a temp log for
# the assertions below.
#
# Requires: qemu-system-x86_64, a kernel image, busybox, cpio, gzip.
# Usage: scripts/vm-test.sh [kernel]   (auto-discovers the kernel if omitted)
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
INITRD=./target/initramfs.cpio.gz

if [ -z "$KERNEL" ] || [ ! -r "$KERNEL" ]; then
  echo "error: no kernel found (pass one as \$1)" >&2; exit 2
fi
command -v "$QEMU" >/dev/null || { echo "error: $QEMU not found" >&2; exit 2; }

# Always (re)build the PID-1 binary with the `boot` feature (incremental, so
# a fast no-op when current). Guarantees a stale default build is never
# reused — without `boot` the getty template doesn't resolve and no prompt
# appears. The `boot` feature is ignored by rystemctl/rystemd-tui.
cargo build --release --features boot

[ -f "$INITRD" ] || { echo "building initramfs..."; scripts/build-initramfs.sh; }

LOG=$(mktemp)
QEMU_PID=""
TAIL_PID=""
cleanup() {
  [ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null
  [ -n "$QEMU_PID" ] && kill "$QEMU_PID" 2>/dev/null
  rm -f "$LOG"
}
trap cleanup EXIT

# -nographic routes the serial console (console=ttyS0) to stdout. -no-reboot
# makes power-off the only way the VM exits. Use KVM when /dev/kvm is usable,
# else fall back to TCG (software). stdbuf line-buffers qemu's output so the
# live console streams smoothly instead of arriving in block-buffered bursts.
ACCEL=()
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  ACCEL=(-accel kvm)
fi
STDBUF=()
if command -v stdbuf >/dev/null 2>&1; then
  STDBUF=(stdbuf -oL -eL)
fi

echo "=== rystemd VM test: booting (kernel: $(basename "$KERNEL")) ==="
"${STDBUF[@]}" "$QEMU" "${ACCEL[@]}" -m 512 -nographic -no-reboot \
  -kernel "$KERNEL" \
  -initrd "$INITRD" \
  -append "console=ttyS0 rdinit=/init panic=-1" \
  > "$LOG" 2>&1 &
QEMU_PID=$!

# Stream the serial console live from the first line.
tail -n +1 -f "$LOG" &
TAIL_PID=$!

# Wait for the manager to start and the boot test to run (bounded).
ok=""
for _ in $(seq 1 600); do
  if grep -q "manager started" "$LOG" && grep -q "BOOTTEST getty=active" "$LOG"; then
    ok=1; break
  fi
  # qemu exited early = boot failure.
  kill -0 "$QEMU_PID" 2>/dev/null || break
  sleep 0.5
done

# Wait for clean power-off (qemu exits).
for _ in $(seq 1 100); do
  kill -0 "$QEMU_PID" 2>/dev/null || break
  sleep 0.5
done

# Stop the live tail now that the VM is done, and let it flush.
kill "$TAIL_PID" 2>/dev/null || true
wait "$TAIL_PID" 2>/dev/null || true
TAIL_PID=""

if [ -z "$ok" ]; then
  echo "FAIL: boot/getty assertion not met" >&2
  echo "--- serial tail ---"; tail -40 "$LOG" >&2
  exit 1
fi
if kill -0 "$QEMU_PID" 2>/dev/null; then
  echo "FAIL: VM did not power off" >&2
  echo "--- serial tail ---"; tail -40 "$LOG" >&2
  exit 1
fi

echo ""
echo "PASS: booted as PID 1, getty active, clean power-off"
