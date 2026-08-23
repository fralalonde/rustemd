#!/usr/bin/env bash
# Build a minimal initramfs that boots rustemd as PID 1 (`rdinit=/init`).
# The initramfs is self-contained: busybox (sh/agetty/etc.), rustemd + its
# dynamic libs, unit files, and /init which execs `rustemd daemon`.
#
# Requires: busybox (static), the rustemd binary, cpio + gzip.
# Usage: scripts/build-initramfs.sh [rustemd-binary] [out.cpio.gz]
set -euo pipefail

BIN=${1:-./target/release/rustemd}
OUT=${2:-./initramfs.cpio.gz}
BUSYBOX=${BUSYBOX:-$(command -v busybox || true)}

[ -x "$BIN" ] || { echo "error: rustemd binary not found (build with: cargo build --release --features boot)" >&2; exit 2; }
[ -n "$BUSYBOX" ] && [ -x "$BUSYBOX" ] || { echo "error: need a static busybox (set BUSYBOX=/path/to/busybox)" >&2; exit 2; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE"/{bin,sbin,usr/bin,dev,proc,sys,run,tmp,etc/rustemd,etc/systemd/system/default.target.wants}

# --- busybox + applet symlinks ---
cp "$BUSYBOX" "$STAGE/bin/busybox"
for app in sh mount modprobe ln cp cat echo sleep ls; do
  ln -s /bin/busybox "$STAGE/bin/$app"
done
# getty@.service references /sbin/getty (busybox's applet; no agetty there).
ln -s /bin/busybox "$STAGE/sbin/getty"

# --- rustemd + its dynamic loader and shared libs ---
cp "$BIN" "$STAGE/usr/bin/rustemd"
ldd "$BIN" 2>/dev/null | while read -r line; do
  case "$line" in
    *"=>"*)
      lib=$(echo "$line" | awk '{print $3}')
      if [ -n "$lib" ] && [ -f "$lib" ]; then
        dest="$STAGE$(dirname "$lib")"; mkdir -p "$dest"; cp -n "$lib" "$dest/"
      fi
      ;;
    *"/ld-"*)
      loader=$(echo "$line" | awk '{print $1}')
      if [ -f "$loader" ]; then
        dest="$STAGE$(dirname "$loader")"; mkdir -p "$dest"; cp -n "$loader" "$dest/"
      fi
      ;;
  esac
done

# --- /init ---
cat > "$STAGE/init" <<'EOF'
#!/bin/sh
exec /usr/bin/rustemd daemon
EOF
chmod +x "$STAGE/init"

# --- unit files ---
cat > "$STAGE/etc/systemd/system/getty@.service" <<'EOF'
[Unit]
Description=Getty on %i
[Service]
Type=idle
ExecStart=-/sbin/getty -L -n -l /bin/sh 115200 %i linux
Restart=always
# busybox's login shell (sh) ignores SIGTERM when interactive; TimeoutStopSec
# escalates to SIGKILL so shutdown doesn't hang on the getty.
TimeoutStopSec=3s
EOF
ln -s ../getty@.service "$STAGE/etc/systemd/system/default.target.wants/getty@ttyS0.service"

# boottest: a short "show" for the serial console — greet, report the getty
# template instance, then power off. Output goes to /dev/console (serial), not
# the unit's captured stdout, so vm-test.sh can both stream it live and grep
# it. The `BOOTTEST getty=…` line is the assertion the host-side test checks.
cat > "$STAGE/etc/rustemd/boottest.sh" <<'SCRIPT_EOF'
#!/bin/sh
sleep 1
state=$(/usr/bin/rustemd is-active getty@ttyS0.service)
cat > /dev/console <<EOF

==========================================
  rustemd -- hello from PID 1
==========================================

  getty@ttyS0.service is $state
  console login on /dev/ttyS0

BOOTTEST getty=$state

  shutting down -- goodbye!

EOF
/usr/bin/rustemd poweroff
SCRIPT_EOF
chmod +x "$STAGE/etc/rustemd/boottest.sh"

cat > "$STAGE/etc/systemd/system/boottest.service" <<'EOF'
[Service]
Type=oneshot
ExecStart=/bin/sh /etc/rustemd/boottest.sh
EOF
ln -s ../boottest.service "$STAGE/etc/systemd/system/default.target.wants/boottest.service"

# --- etc ---
echo "rustemd" > "$STAGE/etc/hostname"

# --- pack into a gzip'd newc cpio ---
( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$OUT"
echo "initramfs -> $OUT ($(du -h "$OUT" | cut -f1))"
