#!/usr/bin/env bash
# Build an Alpine-based initramfs for systemd scenario compatibility tests.
# Alpine supplies a coherent musl userland; rystemd remains PID 1.
#
# Usage: scripts/build-alpine-initramfs.sh [rystemd] [rystemctl] [out.cpio.gz]
# Requires: curl, tar, cpio, gzip, sha256sum, unshare, and apk.static's
# rootless user-namespace support.
#
# Optional env hooks:
#   RYSTEMD_ALPINE_PACKAGES  space-separated packages added with apk
#   RYSTEMD_EXTRA_UNITS      directory of unit files plus optional enable list
#   RYSTEMD_SYSTEMCTL_ALIAS  install systemctl -> rystemctl
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/alpine-config.sh
. "$ROOT/scripts/alpine-config.sh"

BIN=${1:-./target/x86_64-unknown-linux-musl/release/rystemd}
CTL=${2:-./target/x86_64-unknown-linux-musl/release/rystemctl}
OUT=${3:-./target/alpine-compat-initramfs.cpio.gz}
PACKAGES=${RYSTEMD_ALPINE_PACKAGES:-"bash jq coreutils findutils grep sed gawk procps util-linux"}
CACHE="$ROOT/target/alpine"
BASE="$CACHE/alpine-minirootfs-$ALPINE_VERSION-$ALPINE_ARCH.tar.gz"
APK="$CACHE/apk.static-$APK_VERSION-$ALPINE_ARCH"
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$CACHE" "$(dirname "$OUT")"
[ -x "$BIN" ] || { echo "error: rystemd binary not found: $BIN" >&2; exit 2; }
[ -x "$CTL" ] || { echo "error: rystemctl binary not found: $CTL" >&2; exit 2; }

fetch_verified() {
  local url=$1 out=$2 expected=$3 actual
  if [ ! -f "$out" ]; then
    echo "fetching $(basename "$out")..." >&2
    curl -fsSL -o "$out" "$url"
  fi
  [ "$out" = "$APK" ] && chmod 0755 "$out"
  actual=$(sha256sum "$out" | cut -d' ' -f1)
  if [ "$actual" != "$expected" ]; then
    echo "error: sha256 mismatch for $out" >&2
    echo "  expected: $expected" >&2
    echo "  got:      $actual" >&2
    rm -f "$out"
    exit 1
  fi
}

fetch_verified "$ALPINE_ROOTFS_URL" "$BASE" "$ALPINE_ROOTFS_SHA256"
fetch_verified "$APK_URL" "$APK" "$APK_SHA256"

command -v unshare >/dev/null || { echo "error: unshare is required for rootless apk installation" >&2; exit 2; }
unshare -Ur true 2>/dev/null || {
  echo "error: unprivileged user namespaces are required to build the Alpine image" >&2
  exit 2
}

mkdir -p "$STAGE"
tar -xzf "$BASE" -C "$STAGE"
mkdir -p "$STAGE/etc/apk" "$STAGE/etc/systemd/system/default.target.wants"
printf '%s\n' "$ALPINE_REPOSITORY" > "$STAGE/etc/apk/repositories"

read -r -a APK_PACKAGES <<< "$PACKAGES"
# apk's package scripts and ownership changes assume root. A mapped root user
# namespace gives it both without requiring host root, while --no-scripts keeps
# package installation from trying to chroot into the not-yet-booted image.
unshare -Ur "$APK" \
  --root "$STAGE" \
  --initdb \
  --repositories-file "$STAGE/etc/apk/repositories" \
  --keys-dir "$STAGE/etc/apk/keys" \
  --no-cache --no-progress --no-chown --no-scripts \
  add "${APK_PACKAGES[@]}"

mkdir -p "$STAGE"/{dev,proc,sys,run,tmp,var/tmp,var/log,mnt/demo,etc/rystemd}
chmod 1777 "$STAGE/tmp" "$STAGE/var/tmp"

install_binary() {
  local src=$1 dst=$2
  install -m 0755 "$src" "$STAGE/usr/bin/$dst"
}
install_binary "$BIN" rystemd
install_binary "$CTL" rystemctl

cat > "$STAGE/init" <<'EOF'
#!/bin/sh
mount -t proc proc /proc 2>/dev/null || true
mount -t sysfs sysfs /sys 2>/dev/null || true
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true
mkdir -p /dev/pts /dev/shm /run /tmp /var/tmp /mnt/demo
mount -t devpts devpts /dev/pts 2>/dev/null || true
mount -t tmpfs tmpfs /dev/shm 2>/dev/null || true
mount -t tmpfs tmpfs /run 2>/dev/null || true
mkdir -p /run/systemd/system
mount -t tmpfs tmpfs /tmp 2>/dev/null || true
if [ ! -e /dev/ttyS0 ]; then
  mknod /dev/ttyS0 c 4 64 2>/dev/null || true
  mknod /dev/console c 5 1 2>/dev/null || true
  mknod /dev/null c 1 3 2>/dev/null || true
fi
stty rows 24 cols 80 < /dev/ttyS0 2>/dev/null || true
exec /usr/bin/rystemd daemon
EOF
chmod 0755 "$STAGE/init"

cat > "$STAGE/etc/systemd/system/getty@.service" <<'EOF'
[Unit]
Description=Getty on %i

[Service]
Type=idle
ExecStart=-/sbin/agetty --noclear 115200 %i vt100
Restart=always
TimeoutStopSec=3s
EOF
ln -sf ../getty@.service "$STAGE/etc/systemd/system/default.target.wants/getty@ttyS0.service"

cat > "$STAGE/etc/systemd/system/default.target" <<'EOF'
[Unit]
Description=Alpine compatibility target
Wants=getty@ttyS0.service
EOF

if [ -n "${RYSTEMD_EXTRA_UNITS:-}" ]; then
  [ -d "$RYSTEMD_EXTRA_UNITS" ] || {
    echo "error: RYSTEMD_EXTRA_UNITS=$RYSTEMD_EXTRA_UNITS is not a directory" >&2
    exit 2
  }
  for unit in "$RYSTEMD_EXTRA_UNITS"/*; do
    [ -f "$unit" ] || continue
    [ "$(basename "$unit")" = enable ] && continue
    cp "$unit" "$STAGE/etc/systemd/system/"
  done
  if [ -f "$RYSTEMD_EXTRA_UNITS/enable" ]; then
    while read -r name; do
      [ -n "$name" ] || continue
      case "$name" in \#*) continue ;; esac
      [ -f "$STAGE/etc/systemd/system/$name" ] || {
        echo "error: enable manifest names missing unit: $name" >&2
        exit 2
      }
      ln -sf "../$name" "$STAGE/etc/systemd/system/default.target.wants/$name"
    done < "$RYSTEMD_EXTRA_UNITS/enable"
  fi
fi

if [ -n "${RYSTEMD_SYSTEMCTL_ALIAS:-}" ]; then
  ln -sf /usr/bin/rystemctl "$STAGE/usr/bin/systemctl"
  ln -sf /usr/bin/rystemctl "$STAGE/usr/sbin/systemctl"
fi

echo rystemd > "$STAGE/etc/hostname"
( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$OUT"
echo "Alpine initramfs -> $OUT ($(du -h "$OUT" | cut -f1))"
