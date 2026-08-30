#!/usr/bin/env bash
# End-to-end test of rystemd's real-root handoff (switch_root into /sysroot):
# boot the handoff initramfs in qemu; assert that rystemd, as PID 1 inside the
# initramfs, pivots out and boots a unit that exists ONLY in the fake
# deployment at /sysroot. The deployment's handoff-marker.service prints
# "HANDOFF_OK" to the serial console — if we see it, the handoff genuinely
# happened (rystemd is managing the deployment's real /etc, not the initramfs).
# The unit then powers the VM off cleanly.
#
# Requires: qemu-system-x86_64, a kernel image, busybox, cpio, gzip.
# Usage: scripts/test-handoff.sh [kernel]   (auto-discovers the kernel)
set -uo pipefail

KERNEL=${1:-}
if [ -z "$KERNEL" ]; then
  KERNEL=$(ls -1 /boot/vmlinuz-* 2>/dev/null | sort -V | tail -1)
fi
if [ -z "$KERNEL" ]; then
  KERNEL=$(ls -1 /lib/modules/*/vmlinuz /usr/lib/modules/*/vmlinuz 2>/dev/null | sort -V | tail -1)
fi
QEMU=${QEMU:-qemu-system-x86_64}
INITRD=./target/handoff-initramfs.cpio.gz

if [ -z "$KERNEL" ] || [ ! -r "$KERNEL" ]; then
  echo "error: no kernel found (pass one as \$1)" >&2; exit 2
fi
command -v "$QEMU" >/dev/null || { echo "error: $QEMU not found" >&2; exit 2; }

# Always (re)build the PID-1 binary with `boot` (the handoff is boot-feature
# code) and the handoff initramfs.
cargo build --release --features boot
bash scripts/build-handoff-initramfs.sh

LOG=$(mktemp)
QEMU_PID=""
TAIL_PID=""
cleanup() {
  [ -n "$TAIL_PID" ] && kill "$TAIL_PID" 2>/dev/null
  [ -n "$QEMU_PID" ] && kill "$QEMU_PID" 2>/dev/null
  rm -f "$LOG"
}
trap cleanup EXIT

ACCEL=()
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  ACCEL=(-accel kvm)
fi
STDBUF=()
if command -v stdbuf >/dev/null 2>&1; then
  STDBUF=(stdbuf -oL -eL)
fi

echo "=== rystemd handoff test: booting (kernel: $(basename "$KERNEL")) ==="
"${STDBUF[@]}" "$QEMU" "${ACCEL[@]}" -m 512 -nographic -no-reboot \
  -kernel "$KERNEL" \
  -initrd "$INITRD" \
  -append "console=ttyS0 rdinit=/init panic=-1" \
  > "$LOG" 2>&1 &
QEMU_PID=$!

tail -n +1 -f "$LOG" &
TAIL_PID=$!

# Wait for the handoff to complete and the deployment-only marker to print.
ok=""
for _ in $(seq 1 600); do
  if grep -q "switching root" "$LOG" && grep -q "handoff-deployment" "$LOG" && grep -q "HANDOFF_OK" "$LOG"; then
    ok=1; break
  fi
  kill -0 "$QEMU_PID" 2>/dev/null || break
  sleep 0.5
done

# Wait for clean power-off (qemu exits).
for _ in $(seq 1 100); do
  kill -0 "$QEMU_PID" 2>/dev/null || break
  sleep 0.5
done

kill "$TAIL_PID" 2>/dev/null || true
wait "$TAIL_PID" 2>/dev/null || true
TAIL_PID=""

if [ -z "$ok" ]; then
  echo "FAIL: handoff assertion not met" >&2
  echo "--- serial tail ---"; tail -50 "$LOG" >&2
  exit 1
fi
if kill -0 "$QEMU_PID" 2>/dev/null; then
  echo "FAIL: VM did not power off" >&2
  echo "--- serial tail ---"; tail -50 "$LOG" >&2
  exit 1
fi

echo ""
echo "PASS: rystemd switch_root'ed into /sysroot deployment and powered off"