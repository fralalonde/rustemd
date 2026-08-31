#!/usr/bin/env bash
# Boot a STOCK OS disk (qcow2) with rystemd as init, entirely rootlessly:
# uses only the host kernel + this repo's realroot initramfs + the attached
# disk. No host root, no libguestfs, no disk modification.
#
# Usage: scripts/boot-realroot-vm.sh [image.qcow2] [kernel]
set -uo pipefail

IMG=${1:-/tmp/rystemd-vm/Fedora-Cloud.qcow2}
[ -f "$IMG" ] || { echo "error: image not found: $IMG" >&2; exit 2; }

KERNEL=${2:-}
if [ -z "$KERNEL" ]; then
  KERNEL=$(ls -1 /lib/modules/*/vmlinuz 2>/dev/null | sort -V | tail -1)
fi
[ -r "$KERNEL" ] || { echo "error: no kernel found" >&2; exit 2; }

INITRD=./target/realroot-initramfs.cpio.gz
command -v qemu-system-x86_64 >/dev/null || { echo "error: no qemu" >&2; exit 2; }

# Always (re)build rystemd with boot + the realroot initramfs.
cargo build --release --features boot
bash scripts/build-realroot-initramfs.sh

ACCEL=()
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=(-accel kvm); fi

echo "=== booting rystemd-as-init against a stock Fedora Cloud disk ==="
echo "kernel: $(basename "$KERNEL")   disk: $(basename "$IMG")"
echo "(Ctrl-A x to exit qemu)"

exec qemu-system-x86_64 "${ACCEL[@]}" -m 2048 -nographic -no-reboot \
  -kernel "$KERNEL" \
  -initrd "$INITRD" \
  -append "console=ttyS0 rdinit=/init panic=-1" \
  -drive file="$IMG",if=virtio,format=qcow2,snapshot=on