#!/usr/bin/env bash
# Fetch a *pinned, versioned* kernel for the rootless rystemd-as-PID-1 VMs.
#
# Why pinned: the compat/live VMs must boot reproducibly from the net alone —
# no fat distro image, no kernel already sitting on the host. We pin one exact
# immutable artifact URLs (+ sha256) so a fresh checkout can always re-download
# the same kernel and get byte-identical code.
#
# Fetch: GET the exact Fedora kernel-core RPM, verify its sha256, extract the
# kernel image, verify that too, and cache it at target/kernel/.
# Usage: scripts/fetch-kernel.sh [out]     (prints the path)
set -euo pipefail

# Fedora's immutable Koji artifact, rather than a moving release/pxeboot URL.
# When changing kernels, bump all three version/checksum values together.
KERNEL_VERSION="6.14.0-63.fc42.x86_64"
KERNEL_RPM_URL="https://kojipkgs.fedoraproject.org/packages/kernel/6.14.0/63.fc42/x86_64/kernel-core-6.14.0-63.fc42.x86_64.rpm"
KERNEL_RPM_SHA256="b970aaa67d3d02cbc6669bd8362c58cc6a7b7a2a754f3cac4a58dbf031e85d94"
KERNEL_SHA256="507b2265becc1125b372233c43b044ca68d8cdeba9ed7da2544e1c98529ec289"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CACHE="${ROOT}/target/kernel"
OUT="${1:-$CACHE/vmlinuz-$KERNEL_VERSION}"
mkdir -p "$CACHE" "$(dirname "$OUT")"

# Reuse a previous good download (same version and sha256).
if [ -f "$OUT" ] && [ "$(sha256sum "$OUT" | cut -d' ' -f1)" = "$KERNEL_SHA256" ]; then
  echo "$OUT"
  exit 0
fi

command -v curl >/dev/null || { echo "error: curl is required" >&2; exit 2; }
command -v rpm2cpio >/dev/null || { echo "error: rpm2cpio is required" >&2; exit 2; }
command -v cpio >/dev/null || { echo "error: cpio is required" >&2; exit 2; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
RPM="$TMP/kernel-core.rpm"
echo "fetching pinned kernel $KERNEL_VERSION..." >&2
curl -fsSL -o "$RPM" "$KERNEL_RPM_URL"
sum=$(sha256sum "$RPM" | cut -d' ' -f1)
if [ "$sum" != "$KERNEL_RPM_SHA256" ]; then
  echo "error: kernel RPM sha256 mismatch" >&2
  echo "  expected: $KERNEL_RPM_SHA256" >&2
  echo "  got:      $sum" >&2
  exit 1
fi
mkdir "$TMP/root"
( cd "$TMP/root" && rpm2cpio "$RPM" | cpio -idm --quiet )
EXTRACTED="$TMP/root/lib/modules/$KERNEL_VERSION/vmlinuz"
[ -f "$EXTRACTED" ] || { echo "error: RPM did not contain $EXTRACTED" >&2; exit 1; }
sum=$(sha256sum "$EXTRACTED" | cut -d' ' -f1)
if [ "$sum" != "$KERNEL_SHA256" ]; then
  echo "error: extracted kernel sha256 mismatch" >&2
  echo "  expected: $KERNEL_SHA256" >&2
  echo "  got:      $sum" >&2
  exit 1
fi
cp "$EXTRACTED" "$OUT"
echo "kernel verified: $KERNEL_VERSION ($KERNEL_SHA256)" >&2
echo "$OUT"