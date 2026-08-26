#!/usr/bin/env bash
# Smoke test: run rystemd as real PID 1 in a user+mount+pid namespace and
# verify it boots a service and shuts down cleanly. No qemu/kernel needed —
# uses `unshare`, so it runs anywhere user namespaces are enabled (including
# a toolbox/container).
set -uo pipefail

BIN=${1:-./target/release/rystemd}

# Always (re)build the PID-1 binary with the `boot` feature (incremental, so
# a fast no-op when current). This guarantees a stale default build is never
# reused when $BIN is the default path; a caller-supplied $1 is still honored
# as-is below.
cargo build --release --features boot

if [ ! -x "$BIN" ]; then
  echo "error: need a rystemd binary (build with: cargo build --release --features boot)" >&2
  exit 2
fi

# Temp dir under target/, NOT /tmp (rystemd mounts a fresh /tmp and would hide
# it) and not the repo root (keep generated artifacts out of the tree).
mkdir -p target
D=$(mktemp -d "./target/.ns-boot-XXXXXX")
trap 'rm -rf "$D"' EXIT
mkdir -p "$D/units/default.target.wants"

# A oneshot service that proves a unit actually ran under PID-1 rystemd by
# writing a marker to a host-visible path.
cat > "$D/units/hello.service" <<EOF
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'echo booted > $D/marker'
EOF
ln -s ../hello.service "$D/units/default.target.wants/hello.service"

export RYSTEMD_UNIT_PATH="$D/units"
unset RYSTEMD_RUNTIME_DIR RYSTEMD_SOCKET RYSTEMD_CONFIG_DIR

LOG="$D/daemon.log"
# --user must come first (creates the userns); --pid implies --fork, so the
# child becomes PID 1 and `unshare` (the parent) waits for it.
unshare --user --map-root-user --mount --pid --fork \
  "$BIN" daemon >"$LOG" 2>&1 &
NS=$!

# Wait (bounded) for the marker = proof a unit booted under PID 1.
ok=""
for _ in $(seq 1 150); do
  [ -f "$D/marker" ] && { ok=1; break; }
  sleep 0.1
done
if [ -z "$ok" ]; then
  echo "FAIL: service never booted under PID 1" >&2
  echo "--- daemon.log ---"; cat "$LOG" >&2
  kill -9 "$NS" 2>/dev/null || true
  exit 1
fi
echo "PASS: unit booted (marker written)"

# Clean shutdown: SIGTERM to PID 1 (the child of `unshare`) → orderly stop.
CHILD=$(ps --ppid "$NS" -o pid= 2>/dev/null | head -1 | tr -d ' ')
[ -n "$CHILD" ] && kill -TERM "$CHILD" 2>/dev/null || true
for _ in $(seq 1 50); do
  kill -0 "$NS" 2>/dev/null || { echo "PASS: clean shutdown"; exit 0; }
  sleep 0.1
done
echo "FAIL: did not exit after SIGTERM" >&2
kill -9 "$NS" 2>/dev/null || true
exit 1
