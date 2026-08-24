#!/usr/bin/env bash
# Build a minimal initramfs that boots rustemd as PID 1 (`rdinit=/init`).
# The initramfs is self-contained: busybox (sh/getty/etc.), rustemd + its
# dynamic libs, unit files, and /init which mounts the API filesystems then
# execs `rustemd daemon`.
#
# Requires: busybox (static), the rustemd binary, cpio + gzip.
# Usage: scripts/build-initramfs.sh [rustemd-binary] [out.cpio.gz]
#
# Optional env hooks:
#   RUSTEMD_EXTRA_UNITS=<dir>   install every unit file in <dir> into
#                               /etc/systemd/system, and symlink the units
#                               listed in <dir>/enable into default.target.wants
#   RUSTEMD_NO_BOOTTEST=1       omit the auto-poweroff boottest.service (used
#                               by the interactive live env; see live-vm.sh)
set -euo pipefail

BIN=${1:-./target/release/rustemd}
OUT=${2:-./initramfs.cpio.gz}
BUSYBOX=${BUSYBOX:-$(command -v busybox || true)}

[ -x "$BIN" ] || { echo "error: rustemd binary not found (build with: cargo build --release --features boot)" >&2; exit 2; }
[ -n "$BUSYBOX" ] && [ -x "$BUSYBOX" ] || { echo "error: need a static busybox (set BUSYBOX=/path/to/busybox)" >&2; exit 2; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE"/{bin,sbin,usr/bin,dev,proc,sys,run,tmp,etc/rustemd,etc/systemd/system/default.target.wants,mnt/demo}

# --- busybox + applet symlinks ---
cp "$BUSYBOX" "$STAGE/bin/busybox"
for app in sh mount umount mkdir mknod modprobe ln cp cat echo sleep ls date nc grep head tail ps timeout stty ip; do
  ln -s /bin/busybox "$STAGE/bin/$app"
done
# getty@.service references /sbin/getty (busybox's applet; no agetty there).
ln -s /bin/busybox "$STAGE/sbin/getty"

# --- rustemd (+ optional rustemd-tui) and their dynamic libs ---
# copy_binary <src> <dest-name>: copy a binary and every shared lib + the
# dynamic loader it links against, recreating the lib dirs under $STAGE.
copy_binary() {
  local src=$1 name=$2
  cp "$src" "$STAGE/usr/bin/$name"
  ldd "$src" 2>/dev/null | while read -r line; do
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
}
copy_binary "$BIN" rustemd
# The TUI is optional (the live env is CLI-first); ship it when present so a
# human can run `rustemd-tui` from the getty shell.
TUI=${TUI:-./target/release/rustemd-tui}
if [ -x "$TUI" ]; then
  copy_binary "$TUI" rustemd-tui
else
  echo "note: $TUI not found — skipping rustemd-tui (build with: cargo build --release --features boot)" >&2
fi

# --- /init ---
# Mount the API filesystems before exec'ing rustemd so the udev feature sees
# the real sysfs tree (and thus registers real .device units), and so the
# getty has a /dev/ttyS0. rustemd's `boot` feature would mount these itself,
# but doing it here keeps the initramfs explicit and self-contained. Every
# mount is best-effort and idempotent (a mount that already exists is a no-op).
cat > "$STAGE/init" <<'EOF'
#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
# devtmpfs gives us /dev/ttyS0 etc.; fall back to a static node if unavailable.
mount -t devtmpfs devtmpfs /dev 2>/dev/null
mkdir -p /dev/pts /dev/shm
mount -t devpts devpts /dev/pts 2>/dev/null
mount -t tmpfs tmpfs /tmp 2>/dev/null
if [ ! -e /dev/ttyS0 ]; then
  mknod /dev/ttyS0 c 4 64 2>/dev/null
  mknod /dev/console c 5 1 2>/dev/null
  mknod /dev/null c 1 3 2>/dev/null
fi
mkdir -p /mnt/demo
# Bring up loopback so TCP `.socket` demo units (ListenStream=127.0.0.1:…) are
# reachable from the getty shell; the kernel leaves lo DOWN on a fresh boot.
ip link set lo up 2>/dev/null
# Give the serial console a window size so `stty size` reports non-zero and
# the TUI needs no 0×0 fallback. A bare serial line has no TIOCGWINSZ, so
# crossterm would otherwise see a 0×0 terminal. Resize it from the getty shell
# with `stty rows N cols N < /dev/ttyS0` to match the host terminal.
stty rows 24 cols 80 < /dev/ttyS0 2>/dev/null || true
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
# Skipped for the interactive live env (RUSTEMD_NO_BOOTTEST=1).
if [ -z "${RUSTEMD_NO_BOOTTEST:-}" ]; then
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
fi

# --- extra unit files (the live demo env) ---
# When RUSTEMD_EXTRA_UNITS points at a directory, copy every unit file in it
# into /etc/systemd/system, and symlink the units named in its `enable`
# manifest (one name per line) into default.target.wants so they start at boot.
if [ -n "${RUSTEMD_EXTRA_UNITS:-}" ]; then
  [ -d "$RUSTEMD_EXTRA_UNITS" ] || { echo "error: RUSTEMD_EXTRA_UNITS=$RUSTEMD_EXTRA_UNITS is not a directory" >&2; exit 2; }
  for f in "$RUSTEMD_EXTRA_UNITS"/*; do
    [ -f "$f" ] || continue
    case "$(basename "$f")" in
      enable) continue ;;  # not a unit file; handled below
    esac
    cp "$f" "$STAGE/etc/systemd/system/"
  done
  if [ -f "$RUSTEMD_EXTRA_UNITS/enable" ]; then
    while read -r name; do
      [ -n "$name" ] || continue
      case "$name" in \#*) continue ;; esac
      [ -f "$STAGE/etc/systemd/system/$name" ] || { echo "error: enable manifest names missing unit: $name" >&2; exit 2; }
      ln -s "../$name" "$STAGE/etc/systemd/system/default.target.wants/$name"
    done < "$RUSTEMD_EXTRA_UNITS/enable"
  fi
fi

# --- etc ---
echo "rustemd" > "$STAGE/etc/hostname"

# --- pack into a gzip'd newc cpio ---
( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$OUT"
echo "initramfs -> $OUT ($(du -h "$OUT" | cut -f1))"
