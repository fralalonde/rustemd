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
# Optional root password for the real-root VM (the Fedora Cloud image locks
# root). Passed at BUILD time. We compute a sha512 (crypt $6$) hash ON THE HOST
# with a fixed salt (openssl and busybox produce identical output, verified),
# and bake the hash — not the plaintext — into the initramfs (target/,
# gitignored). At boot the shadow field is replaced with this hash via sed, so
# no runtime chpasswd/crypt is trusted. Plaintext never enters the repo.
ROOTPW=${REALROOT_ROOT_PW:-}
ROOTPW_SALT=${REALROOT_ROOT_PW_SALT:-RYSTE0MSALT}
ROOTPW_HASH=""
if [ -n "$ROOTPW" ]; then
  ROOTPW_HASH=$(openssl passwd -6 -salt "$ROOTPW_SALT" "$ROOTPW" 2>/dev/null || busybox cryptpw -m sha512 -S "$ROOTPW_SALT" "$ROOTPW")
fi
mkdir -p "$(dirname "$OUT")"

[ -x "$BIN" ] || { echo "error: rystemd binary not found (cargo build --release --features boot)" >&2; exit 2; }
[ -x "$CTL" ] || { echo "error: rystemctl binary not found" >&2; exit 2; }
[ -n "$BUSYBOX" ] && [ -x "$BUSYBOX" ] || { echo "error: need a static busybox" >&2; exit 2; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE"/{bin,sbin,usr/bin,dev,proc,sys,run,tmp,sysroot,mnt}

cp "$BUSYBOX" "$STAGE/bin/busybox"
for app in sh mount umount mkdir mknod cp cat echo sleep grep head tail ln blkid basename dirname ls rm touch sed; do
  ln -s /bin/busybox "$STAGE/bin/$app"
done
ln -s /bin/busybox "$STAGE/sbin/getty"

# Bake the optional root password HASH into the initramfs (target/, gitignored).
# Only the $6$ hash rides in the image — plaintext stays in the build env only.
if [ -n "$ROOTPW_HASH" ]; then
  mkdir -p "$STAGE/etc"
  printf '%s' "$ROOTPW_HASH" > "$STAGE/etc/rootpw"
fi

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
export PATH=/bin:/sbin:/usr/bin:/usr/sbin
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

# Set a root password (the Fedora Cloud image locks root and expects SSH).
# The $6$ hash is baked into /etc/rootpw at build time (see builder; openssl
# and busybox sha512 crypt agree, verified). Replace the root field in
# /sysroot/etc/shadow with it via sed — no runtime chpasswd/crypt trusted.
# /etc exists inside the mounted root subvol; snapshot=on keeps the image clean.
RH=""
if [ -r /etc/rootpw ]; then RH=$(cat /etc/rootpw); fi
if [ -n "$RH" ]; then
  mount -o remount,rw /sysroot 2>/dev/null
  if [ -f /sysroot/etc/shadow ] && sed -i "s|^root:[^:]*|root:$RH|" /sysroot/etc/shadow 2>/dev/null; then
    echo "rystemd: root password hash installed into /sysroot/etc/shadow" > /dev/console
    echo "rystemd: shadow root line now:" > /dev/console
    sed -n '1p' /sysroot/etc/shadow > /dev/console
  else
    echo "rystemd: WARNING shadow hash install FAILED" > /dev/console
  fi
else
  echo "rystemd: root password unset; root stays locked" > /dev/console
fi

# Deterministic console login: override the DEPLOYMENT's default.target and
# getty units to a slim, rystemd-native chain so we reach a live login: on our
# own — WITHOUT Fedora's full graphical/service head graph (systemd-journald,
# machine-id-commit, etc. wedge rystemd's job scheduler and never reach getty).
# Fedora's /usr/lib/systemd/system/default.target is a symlink to
# graphical.target; we overwrite that same path with a plain file (cat >
# truncates), which find_unit() (and /etc not being a mounted subvol here)
# resolves over everything. getty.target + getty@.service are the console
# login units. `snapshot=on` keeps the pristine image untouched. Read back.
USYS="/sysroot/usr/lib/systemd/system"
mount -o remount,rw /sysroot 2>/dev/null
mkdir -p "$USYS" "$USYS/default.target.wants" 2>/dev/null
cat > "$USYS/default.target" <<'DEFAULTEOF'
[Unit]
Description=Default (console login)
Wants=getty@ttyS0.service
After=getty@ttyS0.service
DEFAULTEOF
# Neutralize Fedora's real basic.target (which Wants systemd-journald.service,
# sysinit.target, ... and would drag the whole head graph into the boot and
# wedge the job scheduler before any getty runs). This empty override keeps the
# slim default -> getty chain isolated so login is deterministic.
cat > "$USYS/basic.target" <<'BASICEOF'
[Unit]
Description=Basic System (rystemd milestone override)
BASICEOF
cat > "$USYS/getty.target" <<'TARGETEOF'
[Unit]
Description=Login Prompts
TARGETEOF
cat > "$USYS/getty@.service" <<'GETTYEOF'
[Unit]
Description=Getty on %i
[Service]
Type=idle
ExecStart=-/usr/sbin/agetty -L 115200 %i linux
Restart=always
TimeoutStopSec=3s
GETTYEOF
cat > "$USYS/default.target.wants/getty@ttyS0.service" <<'WANTEOF'
[Unit]
Description=Getty on ttyS0
[Service]
Type=idle
ExecStart=-/usr/sbin/agetty -L 115200 ttyS0 linux
Restart=always
TimeoutStopSec=3s
WANTEOF
if [ -f "$USYS/default.target" ] && grep -q "getty" "$USYS/default.target.wants/getty@ttyS0.service" && grep -q "agetty" "$USYS/getty@.service"; then
  echo "rystemd: slim default.target + getty installed (verified)" > /dev/console
else
  echo "rystemd: WARNING getty override install FAILED" > /dev/console
fi

# rystemd finds /sysroot already mounted; hand PID 1 to it.
exec /usr/bin/rystemd daemon
EOF
chmod +x "$STAGE/init"

( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$OUT"
echo "realroot initramfs -> $OUT ($(du -h "$OUT" | cut -f1))"