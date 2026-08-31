#!/usr/bin/env bash
# Boot a prepared rystemd-as-init image to a serial console. Watch for the
# `login:` prompt — that proves rystemd booted a real distro root as init.
#
# Usage: sudo scripts/boot-realinit-vm.sh IMAGE.qcow2 [kernel-args...]
set -uo pipefail

IMG="${1:?usage: $0 IMAGE.qcow2 [args...]}"
shift || true
KERNEL_ARGS="${*:-console=ttyS0}"

command -v qemu-system-x86_64 >/dev/null || { echo "error: install qemu-kvm" >&2; exit 2; }

ACCEL=()
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=(-accel kvm); fi

echo "=== booting $IMG (kernel: ${KERNEL_ARGS}) ==="
echo "Watch for:  rystemd ... manager started   then   login:"
echo "To surprise-login at the prompt: type root and the password you set."
echo "(Ctrl-A x to force-exit qemu)"

exec qemu-system-x86_64 "${ACCEL[@]}" -m 1536 -nographic -no-reboot \
  -drive file="$IMG",if=virtio,format=raw \
  -append "$KERNEL_ARGS"