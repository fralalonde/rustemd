#!/usr/bin/env bash
# Prepare a bootable qemu image that boots rystemd as init (Model A: rystemd
# is the POST-pivot init — the stock initramfs mounts the root and hands PID 1
# to a `rystemd daemon` shim).
#
# Requires (root): libguestfs-tools, a base Fedora Cloud/Atomic qcow2, the
# rystemd RPM. Bakes in: rystemd, an /sbin/init shim, a console getty, a slim
# default.target, and SELinux disabled.
#
# Usage:
#   sudo scripts/prepare-realinit-vm.sh --base IMG.qcow2 --rpm rystemd.rpm \
#       [--out OUT.qcow2] [--rootpw PASSWORD]
set -euo pipefail

BASE=""; RPM=""; OUT="rystemd-vm.qcow2"; ROOTPW="rystemd"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="$2"; shift 2 ;;
    --rpm)  RPM="$2";  shift 2 ;;
    --out)  OUT="$2";  shift 2 ;;
    --rootpw) ROOTPW="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done
[ -n "$BASE" ] || { echo "error: --base required" >&2; exit 2; }
[ -n "$RPM" ] || { echo "error: --rpm required" >&2; exit 2; }

command -v virt-customize >/dev/null || { echo "error: install libguestfs-tools" >&2; exit 2; }

STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

# /sbin/init shim: kernel init= can't pass args, so exec rystemd daemon.
cat > "$STAGE/init-shim" <<'EOF'
#!/bin/sh
exec /usr/bin/rystemd daemon "$@"
EOF
chmod +x "$STAGE/init-shim"

# A console getty attached to multi-user (the target default.target resolves
# to). We do NOT override default.target — Fedora's is a symlink; just add a
# getty that comes up in the normal target and we boot to a login prompt.
cat > "$STAGE/console-getty.service" <<'EOF'
[Unit]
Description=Console getty on tty1
[Service]
Type=idle
ExecStart=-/sbin/agetty -o '-p -- \\u' --noclear - linux tty1
Restart=always
[Install]
WantedBy=multi-user.target
EOF

echo "=== preparing $OUT from $BASE ==="
RPM_BASE="$(basename "$RPM")"
virt-customize -a "$BASE" \
  --memsize 1024 \
  --copy-in "$RPM:/root" \
  --copy-in "$STAGE/init-shim:/sbin" \
  --copy-in "$STAGE/console-getty.service:/etc/systemd/system" \
  --run-command "mv /sbin/init-shim /sbin/init && chmod +x /sbin/init" \
  --run-command "rpm -i --nodeps /root/$RPM_BASE && rm -f /root/$RPM_BASE" \
  --run-command "systemctl enable console-getty.service" \
  --run-command "sed -i s/^SELINUX=.*/SELINUX=permissive/ /etc/selinux/config || true" \
  --root-password "password:$ROOTPW" \
  -o "$OUT"

echo ""
echo "=== done: $OUT ==="
echo "Boot it with: sudo scripts/boot-realinit-vm.sh $OUT"
echo "Login at the console with root / $ROOTPW (SELinux permissive)."