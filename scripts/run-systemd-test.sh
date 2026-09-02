#!/usr/bin/env bash
# Run one of systemd's own integration-test *scenarios* against rystemd, on a
# STABLE, self-contained platform:
#
#   - a pinned, versioned kernel fetched from the net (scripts/fetch-kernel.sh)
#     — no fat distro image, no kernel already on the host; sha256-verified
#   - a pinned Alpine/musl initramfs (build-alpine-initramfs.sh) with rystemd
#     as PID 1 and `systemctl` aliased to the systemctl-compatible `rystemctl`
#   - the systemd test's `.units/` dropped in and its scenario .sh installed as
#     a boot one-shot that runs against the live rystemd manager and powers off
#
# The compatibility layer is the CLI itself: rystemctl presents a systemctl
# surface, so the scenario's `systemctl …` calls run as-is where the surrounding
# tooling exists. Assertions that reach past the CLI (journald internals, udev,
# cgroup enforcement, `systemd-run`, `busctl`, D-Bus Jobs) fail loudly — the
# honest gap surface, reported per-test rather than hidden.
#
# Scenario scripts often source systemd's test helpers / assume files absent
# The contract is: the .sh runs under Alpine's Bash and a PATH containing
# systemctl, rystemctl, jq, and the Alpine utilities; failures are reported,
# not papered over.
#
# Usage: scripts/run-systemd-test.sh <TEST-XX-KEY> [systemd-tests-dir]
# Requires: qemu-system-x86_64, bash, curl, tar, cpio, gzip, unshare. Run from
# the repo root.
set -euo pipefail

TESTKEY="${1:?TEST key required (e.g. TEST-03-JOBS)}"
TESTS_DIR="${2:-}"
KERNEL="$(bash scripts/fetch-kernel.sh)" || exit 2
INITRD=./target/compat-${TESTKEY}.cpio.gz
QEMU=${QEMU:-qemu-system-x86_64}
FEATURES=${RYSTEMD_LIVE_FEATURES:-boot,socket}
TARGET=${RYSTEMD_TARGET:-x86_64-unknown-linux-musl}
TEST_SHELL_GUEST=/bin/bash

# Locate (or borrow) a systemd checkout to source the test's .units + .sh.
if [ -z "$TESTS_DIR" ]; then
  SRC=$(mktemp -d)
  trap 'rm -rf "$SRC"' EXIT
  echo "=== cloning systemd (sparse, shallow) to borrow ${TESTKEY} ==="
  git clone --depth 1 --filter=blob:none --sparse \
    https://github.com/systemd/systemd.git "$SRC/systemd"
  git -C "$SRC/systemd" sparse-checkout set \
    "test/units" "test/integration-tests/${TESTKEY}"
  TESTS_DIR="$SRC/systemd/test/integration-tests"
fi

TESTDIR="$TESTS_DIR/$TESTKEY"
[ -d "$TESTDIR" ] || { echo "error: no such test dir: $TESTDIR" >&2; exit 2; }
# systemd keeps scenario bodies in test/units/TEST-XX.sh (integration-tests
# holds .units + build glue). Find the .sh either way.
SCRIPT=""
for cand in "$TESTS_DIR/../units/TEST-${TESTKEY#TEST-}.sh" "$TESTDIR/TEST-${TESTKEY#TEST-}.sh" \
           "$TESTDIR/test.sh" "$TESTDIR/$TESTKEY.sh"; do
  [ -f "$cand" ] && { SCRIPT="$cand"; break; }
done
[ -n "$SCRIPT" ] || { echo "error: no scenario .sh found for $TESTKEY (looked in a few known spots)" >&2; exit 2; }
UTIL=""
if [ -f "$TESTS_DIR/../units/util.sh" ]; then
  UTIL="$TESTS_DIR/../units/util.sh"
elif [ -f "$(dirname "$SCRIPT")/util.sh" ]; then
  UTIL="$(dirname "$SCRIPT")/util.sh"
fi

echo "preparing Alpine musl sysroot..."
SYSROOT=$(bash scripts/build-alpine-sysroot.sh)
rustup target add "$TARGET" >/dev/null
MUSL_RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=--target=x86_64-linux-musl -C link-arg=--sysroot=$SYSROOT"
echo "building rystemd/rystemctl for $TARGET..."
env \
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=clang \
  RUSTFLAGS="$MUSL_RUSTFLAGS" \
  cargo build --release --target "$TARGET" \
    -p rystemd --no-default-features --features "$FEATURES" \
    -p rystemctl

UNITS_DIR="$(find "$TESTDIR" -maxdepth 1 -type d -name '*.units' | head -1)"
if [ -n "$UNITS_DIR" ]; then
  export RYSTEMD_EXTRA_UNITS="$UNITS_DIR"
  echo "borrowing units from: $UNITS_DIR"
fi
export RYSTEMD_NO_BOOTTEST=1 RYSTEMD_SYSTEMCTL_ALIAS=1
BASE_CPIO=./target/compat-${TESTKEY}-base.cpio.gz
bash scripts/build-alpine-initramfs.sh \
  "target/$TARGET/release/rystemd" \
  "target/$TARGET/release/rystemctl" \
  "$BASE_CPIO"
unset RYSTEMD_EXTRA_UNITS 2>/dev/null || true

echo "=== ${TESTKEY} -> rystemd (kernel $(basename "$KERNEL")) ==="

# Install the scenario .sh as a boot one-shot that writes to /dev/console and
# powers off. Unpack the freshly built initramfs, inject the test script +
# service + poweroff footer, repack to the final INITRD. Use a temp copy so a
# partial run never destroys the base build output.
BASE_COPY=$(mktemp)
cp "$BASE_CPIO" "$BASE_COPY"
STAGE=$(mktemp -d)
trap '[ -n "${QEMU_PID:-}" ] && kill "$QEMU_PID" 2>/dev/null; rm -rf "$STAGE" "$BASE_COPY" /tmp/compat-inject' EXIT
( cd "$STAGE" && zcat < "$BASE_COPY" | cpio --null -idm ) 2>/dev/null   # unpack gzip'd newc cpio (NUL-terminated names)
mkdir -p "$STAGE/etc/rystemd" "$STAGE/etc/systemd/system/default.target.wants"
cp "$SCRIPT" "$STAGE/etc/rystemd/scenario.sh"
[ -z "$UTIL" ] || cp "$UTIL" "$STAGE/etc/rystemd/util.sh"
cat > "$STAGE/etc/systemd/system/default.target" <<'EOF'
[Unit]
Description=Systemd compatibility scenario target
Wants=ryn-test.service
EOF
# TEST-03-JOBS uses systemd-importd only as a convenient unit for the
# --show-transaction smoke check. Alpine does not ship that Fedora service;
# provide a harness-only placeholder rather than pretending importd exists.
cat > "$STAGE/etc/systemd/system/systemd-importd.service" <<'EOF'
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/true
EOF
cat > "$STAGE/etc/rystemd/run-test.sh" <<EOF
#!/bin/sh
echo RYNTEST_BEGIN > /dev/console
"$TEST_SHELL_GUEST" /etc/rystemd/scenario.sh > /dev/console 2>&1
rc=\$?
echo "RYNTEST_DONE rc=\$rc" > /dev/console
/usr/bin/rystemctl poweroff || /bin/poweroff -f 2>/dev/null
exit "\$rc"
EOF
chmod +x "$STAGE/etc/rystemd/run-test.sh"
cat > "$STAGE/etc/systemd/system/ryn-test.service" <<'EOF'
[Service]
Type=oneshot
ExecStart=/bin/sh /etc/rystemd/run-test.sh
EOF
ln -sf ../ryn-test.service "$STAGE/etc/systemd/system/default.target.wants/ryn-test.service"
# gzip'd newc cpio
( cd "$STAGE" && find . -print0 | cpio --null -o -H newc 2>/dev/null ) | gzip -9 > "$INITRD"

LOG=${RYSTEMD_TEST_LOG:-/tmp/ryn-compat-${TESTKEY}.log}
rm -f "$LOG"
ACCEL=()
if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=(-accel kvm); fi

"$QEMU" "${ACCEL[@]}" -m 4096 -nographic -no-reboot \
  -kernel "$KERNEL" -initrd "$INITRD" \
  -append "console=ttyS0 rdinit=/init panic=-1" > "$LOG" 2>&1 &
QEMU_PID=$!

for _ in $(seq 1 240); do
  kill -0 "$QEMU_PID" 2>/dev/null || break
  grep -q "RYNTEST_DONE\|powering off\|reboot: Power down" "$LOG" 2>/dev/null && { sleep 2; break; }
  sleep 1
done
kill "$QEMU_PID" 2>/dev/null || true
wait "$QEMU_PID" 2>/dev/null || true

if ! grep -q "RYNTEST_DONE rc=0" "$LOG"; then
  echo "FAIL: ${TESTKEY} did not complete successfully" >&2
  grep -E "RYNTEST_DONE|manager started|failed:|not found|syntax error" "$LOG" >&2 || true
  exit 1
fi

echo ""
echo "--- ${TESTKEY} result (condensed boot log) ---"
grep -vE "^\s*$|\[load\]|udev:" "$LOG" | tail -70
echo "--- end (rc markers: RYNTEST_DONE / rystemd result lines above) ---"