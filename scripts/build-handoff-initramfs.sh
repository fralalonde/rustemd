#!/usr/bin/env bash
# Build an initramfs that exercises rystemd's real-root handoff end-to-end.
#
#   stage-1 initramfs  ->  /sysroot = a fake deployment (its own tmpfs)
#                            -> rystemd switch_roots into it, boots a service
#                               that only exists in the deployment, then powers off.
#
# The core idea: /init mounts a throwaway rootfs at /sysroot and populates it
# with a *real-deployment layout* (own /usr/bin/rystemd, own /etc, own
# /etc/hostname, own unit files). Because /sysroot is a different filesystem
# than the initramfs root, rystemd sees `in_initramfs && sysroot_mounted`,
# pivots out via switch_root(8), re-execs against the real /etc, and boots a
# handoff-marker.service that EXISTS ONLY in the deployment. If it prints
# HANDOFF_OK, the pivot genuinely happened — rystemd is managing the deployment,
# not the initramfs.
#
# Requires: busybox (static), the rystemd binaries, cpio + gzip.
# Usage: scripts/build-handoff-initramfs.sh [rystemd-binary] [out.cpio.gz]
set -euo pipefail

BIN=${1:-./target/release/rystemd}
CTL=${CTL:-./target/release/rystemctl}
OUT=${2:-./target/handoff-initramfs.cpio.gz}
BUSYBOX=${BUSYBOX:-$(command -v busybox || true)}
mkdir -p "$(dirname "$OUT")"

[ -x "$BIN" ] || { echo "error: rystemd binary not found (build with: cargo build --release --features boot)" >&2; exit 2; }
[ -x "$CTL" ] || { echo "error: rystemctl binary not found (build with: cargo build --release --features boot)" >&2; exit 2; }
[ -n "$BUSYBOX" ] && [ -x "$BUSYBOX" ] || { echo "error: need a static busybox (set BUSYBOX=/path/to/busybox)" >&2; exit 2; }

STAGE=$(mktemp -d)
DEPLOY=$(mktemp -d)     # the fake "real deployment", baked into the initramfs
trap 'rm -rf "$STAGE" "$DEPLOY"' EXIT

copy_binary() {   # copy a binary + its dynamic libs + loader into stage-1 root
  local src=$1 name=$2
  cp "$src" "$STAGE/usr/bin/$name"
  ldd "$src" 2>/dev/null | while read -r line; do
    case "$line" in
      *"=>"*) lib=$(echo "$line" | awk '{print $3}'); [ -n "$lib" ] && [ -f "$lib" ] && { d="$STAGE$(dirname "$lib")"; mkdir -p "$d"; cp -n "$lib" "$d/"; } ;;
      *"/ld-"*) loader=$(echo "$line" | awk '{print $1}'); [ -f "$loader" ] && { d="$STAGE$(dirname "$loader")"; mkdir -p "$d"; cp -n "$loader" "$d/"; } ;;
    esac
  done
}

# --- stage-1 initramfs root -------------------------------------------------
mkdir -p "$STAGE"/{bin,sbin,usr/bin,dev,proc,sys,run,tmp,sysroot}
cp "$BUSYBOX" "$STAGE/bin/busybox"
for app in sh mount umount mkdir mknod cp cat echo sleep grep head tail ln; do
  ln -s /bin/busybox "$STAGE/bin/$app"
done
ln -s /bin/busybox "$STAGE/sbin/getty"
copy_binary "$BIN" rystemd
copy_binary "$CTL" rystemctl

# --- the fake real deployment (extracted into /sysroot by /init) ------------
mkdir -p "$DEPLOY"/{bin,usr/bin,etc/systemd/system/default.target.wants,proc,sys,dev,run,tmp}
# The deployment must be able to re-exec the manager after the pivot; give it a
# copy of the same binary (cp puts it at the deployment's own /usr/bin).
cp "$STAGE/usr/bin/rystemd" "$DEPLOY/usr/bin/rystemd"
cp "$STAGE/usr/bin/rystemctl" "$DEPLOY/usr/bin/rystemctl"   # marker service powers off via this
cp "$STAGE/bin/busybox" "$DEPLOY/bin/busybox"               # /bin/sh for the marker service
# Busybox applet symlinks the marker's sh needs (cat, echo, sh).
for app in cat echo sh; do
  ln -sf busybox "$DEPLOY/bin/$app"
done
# Shared libs + dynamic loader must exist in the deployment too, or the
# post-pivot re-exec (which resolves /usr/bin/rystemd in the NEW root) fails.
for f in lib lib64 usr/lib usr/lib64; do
  [ -d "$STAGE/$f" ] && { mkdir -p "$DEPLOY/$f"; cp -a "$STAGE/$f"/. "$DEPLOY/$f/"; }
done
# A distinct hostname proves we're managing the deployment, not the initramfs.
echo "handoff-deployment" > "$DEPLOY/etc/hostname"

# The assertion unit: exists ONLY in the deployment. booting it means rystemd
# pivoted into /sysroot and loaded real /etc/systemd/system units.
cat > "$DEPLOY/etc/systemd/system/handoff-marker.service" <<'EOF'
[Unit]
Description=Prove rystemd is running from the /sysroot deployment

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'cat /etc/hostname > /dev/console; echo HANDOFF_OK > /dev/console; /usr/bin/rystemctl poweroff'

[Install]
WantedBy=default.target
EOF
ln -s ../handoff-marker.service "$DEPLOY/etc/systemd/system/default.target.wants/handoff-marker.service"

mkdir -p "$STAGE/deploy"
cp -a "$DEPLOY"/. "$STAGE/deploy/"

cat > "$STAGE/init" <<'EOF'
#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /sysroot
# The deployment is its own tmpfs at /sysroot — a DIFFERENT filesystem than the
# ramfs/rootfs we booted in, mirroring an ostree/dracut initramfs staging the
# real disk deployment. /init then execs rystemd daemon, which detects the
# handoff condition and switch_roots into /sysroot.
mount -t tmpfs tmpfs /sysroot 2>/dev/null
# Copy the baked deployment into it, preserving the layout.
cp -a /deploy/. /sysroot/ 2>/dev/null
exec /usr/bin/rystemd daemon
EOF
chmod +x "$STAGE/init"

# --- pack into a gzip'd newc cpio ---
( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$OUT"
echo "handoff initramfs -> $OUT ($(du -h "$OUT" | cut -f1))"