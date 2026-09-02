#!/usr/bin/env bash
# Prepare a pinned Alpine sysroot containing musl headers and libraries for
# rootless cross-linking rystemd/rystemctl.
# Usage: scripts/build-alpine-sysroot.sh [out-dir]
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# shellcheck source=scripts/alpine-config.sh
. "$ROOT/scripts/alpine-config.sh"
CACHE="$ROOT/target/alpine"
OUT=${1:-$CACHE/musl-sysroot-$ALPINE_VERSION-$ALPINE_ARCH}
BASE="$CACHE/alpine-minirootfs-$ALPINE_VERSION-$ALPINE_ARCH.tar.gz"
APK="$CACHE/apk.static-$APK_VERSION-$ALPINE_ARCH"

mkdir -p "$CACHE" "$(dirname "$OUT")"

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
    rm -f "$out"
    exit 1
  fi
}

fetch_verified "$ALPINE_ROOTFS_URL" "$BASE" "$ALPINE_ROOTFS_SHA256"
fetch_verified "$APK_URL" "$APK" "$APK_SHA256"
command -v unshare >/dev/null || { echo "error: unshare is required" >&2; exit 2; }
unshare -Ur true 2>/dev/null || {
  echo "error: unprivileged user namespaces are required" >&2
  exit 2
}

if [ ! -f "$OUT/usr/lib/libc.a" ]; then
  rm -rf "$OUT"
  mkdir -p "$OUT"
  tar -xzf "$BASE" -C "$OUT"
  printf '%s\n' "$ALPINE_REPOSITORY" > "$OUT/etc/apk/repositories"
  unshare -Ur "$APK" \
    --root "$OUT" \
    --initdb \
    --repositories-file "$OUT/etc/apk/repositories" \
    --keys-dir "$OUT/etc/apk/keys" \
    --no-cache --no-progress --no-chown --no-scripts \
    add musl-dev >&2
fi

[ -f "$OUT/usr/lib/libc.a" ] || { echo "error: musl libc.a missing from sysroot" >&2; exit 1; }
printf '%s\n' "$OUT"
