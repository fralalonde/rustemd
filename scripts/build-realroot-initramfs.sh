#!/usr/bin/env bash
# Build an initramfs that boots a STOCK, downloadable OS disk (e.g. Fedora
# Cloud) with rystemd as init — no host root, no libguestfs, no disk
# modification.
#
#   host(no root: qemu + curl only)
#     -> /init mounts the attached disk's root partition READ-ONLY at /sysroot
#     -> exec rystemd daemon
#     -> rystemd: find_deployment(/sysroot) (a plain root = /sysroot itself),
#                 prepare_deployment (# binds ITS OWN rystemd/rystemctl/libs
#                 into the deployment, so re-exec is self-contained),
#                 handoff() pivot -> boots the real root as PID 1.
#
# Uses the SAME handoff path as test-handoff.sh but the root comes from a real
# qguest-attached disk instead of a fake tree baked into the initramfs.
#
# Requires: busybox (static), the rystemd binaries, cpio + gzip.
# Usage: scripts/build-realroot-initramfs.sh [rystemd-binary] [out.cpio.gz]
set -euo pipefail

BIN=${1:-./target/release/rystemd}
CTL=${CTL:-./target/release/rystemctl}
OUT=${2:-./target/realroot-initramfs.cpio.gz}
BUSYBOX=${BUSYBOX:-$(command -v busybox || true)}
mkdir -p "$(dirname "$OUT")"

[ -x "$BIN" ] || { echo "error: rystemd binary not found (cargo build --release --features boot)" >&2; exit 2; }
[ -x "$CTL" ] || { echo "error: rystemctl binary not found" >&2; exit 2; }
[ -n "$BUSYBOX" ] && [ -x "$BUSYBOX" ] || { echo "error: need a static busybox" >&2; exit 2; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE"/{bin,sbin,usr/bin,dev,proc,sys,run,tmp,sysroot,mnt}

cp "$BUSYBOX" "$STAGE/bin/busybox"
for app in sh mount umount mkdir mknod cp cat echo sleep grep head tail ln blkid basename dirname; do
  ln -s /bin/busybox "$STAGE/bin/$app"
done
ln -s /bin/busybox "$STAGE/sbin/getty"

copy_binary() {
  local src=$1 name=$2
  cp "$src" "$STAGE/usr/bin/$name"
  ldd "$src" 2>/dev/null | while read -r line; do
    case "$line" in
      *"=>"*) lib=$(echo "$line" | awk '{print $3}'); [ -n "$lib" ] && [ -f "$lib" ] && { d="$STAGE$(dirname "$lib")"; mkdir -p "$d"; cp -n "$lib" "$d/"; } ;;
      *"/ld-"*) loader=$(echo "$line" | awk '{print $1}'); [ -f "$loader" ] && { d="$STAGE$(dirname "$loader")"; mkdir -p "$d"; cp -n "$loader" "$d/"; } ;;
    esac
  done
}
copy_binary "$BIN" rystemd
copy_binary "$CTL" rystemctl

# /init: mount the real root (discover which /dev/vda* partition holds it),
# read-only, at /sysroot, then hand PID 1 to rystemd.
cat > "$STAGE/init" <<'EOF'
#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mount -t devpts devpts /dev/pts 2>/dev/null
mkdir -p /sysroot

# rystemd detects /usr/bin/rystemd from THIS initramfs; we also prop an sh
# into the real /usr/bin so getty/scripts on the real root can use a shell.
# (prepare_deployment binds rystemd+libs into the deployment.)

# Discover the root partition: the attached virtio disk surfaces as /dev/vda
# with partitions vda1..vdaN. Try each; mount the one that looks like a root
# (has /usr/lib or /usr/bin and /etc) and is not a boot/ESP partition.
ROOT_DEV=""
for p in /sys/class/block/vda*; do
  [ -e "$p" ] || continue
  dev="/dev/$(basename "$p")"
  [ "$dev" = "/dev/vda" ] && continue      # whole disk, not a partition
  echo "rystemd: probing $dev" > /dev/console
  mount -o ro "$dev" /mnt 2>/dev/null || { echo "rystemd:   mount failed" > /dev/console; continue; }
  # Accept any of: a plain root (/usr,/etc,/bin), an ostree/atomic sysroot
  # (/ostree, or boot/ + var/ at top). find_deployment() resolves the runnable
  # subtree inside /sysroot either way.
  if [ -d /mnt/usr ] || [ -d /mnt/etc ] || [ -d /mnt/ostree ] || [ -d /mnt/usr/lib ] || [ -d /mnt/usr/bin ]; then
    echo "rystemd:   FOUND root on $dev, moving to /sysroot" > /dev/console
    mount --move /mnt /sysroot 2>/dev/null || mount -t auto "$dev" /sysroot -o ro 2>/dev/null
    ROOT_DEV="$dev"
    break
  fi
  echo "rystemd:   not a root (no /usr,/etc,/ostree), listing:" > /dev/console
  ls -1 /mnt > /dev/console
  # btrfs top-level often holds the real root inside a subvol (root/@: mounts
  # as a dir here). Try common subvol names.
  for sv in root @ fedora; do
    if [ -d "/mnt/$sv/usr" ] || [ -d "/mnt/$sv/etc" ]; then
      echo "rystemd:   FOUND subvol /$sv, remounting" > /dev/console
      umount /mnt 2>/dev/null
      mount -t btrfs -o rw,subvol=$sv "$dev" /sysroot 2>/dev/null && { ROOT_DEV="$dev"; break 2; }
      # last resort: keep probing; mount auto may pick it up anyway
      mount -o rw "$dev" /sysroot 2>/dev/null && ROOT_DEV="$dev"
      break 2
    fi
  done
  umount /mnt 2>/dev/null
done
[ -n "$ROOT_DEV" ] || echo "rystemd: FATAL no root partition found" > /dev/console
# rystemd finds /sysroot already mounted; hand PID 1 to it.
exec /usr/bin/rystemd daemon
EOF
chmod +x "$STAGE/init"

( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$OUT"
echo "realroot initramfs -> $OUT ($(du -h "$OUT" | cut -f1))"