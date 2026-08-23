#!/usr/bin/env bash
# Generate the animated TUI demo GIF (docs/demo.gif) with vhs.
# Deterministic: boots a real daemon against scratch fixtures, pre-arms a
# timer + enablement, records the tape, then tears everything down.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --workspace

SCRATCH=$(mktemp -d /tmp/rustemd-demo.XXXXXX)
mkdir -p "$SCRATCH/units" "$SCRATCH/config" "$SCRATCH/run"
cp demo/fixtures/* "$SCRATCH/units/"

export RUSTEMD_UNIT_PATH="$SCRATCH/units"
export RUSTEMD_CONFIG_DIR="$SCRATCH/config"
export RUSTEMD_RUNTIME_DIR="$SCRATCH/run"
export RUSTEMD_SOCKET="$SCRATCH/run/control.sock"

# The tape types `rustemd-tui --user` bare, so put the release binaries on
# PATH for the shell vhs spawns.
export PATH="$PWD/target/release:$PATH"

./target/release/rustemd daemon --user &
DAEMON_PID=$!
trap 'kill "$DAEMON_PID" 2>/dev/null || true; rm -rf "$SCRATCH"' EXIT
sleep 1

# Pre-arm state so the recording shows color and a live timer countdown.
./target/release/rustemd --user enable app.service backup.timer >/dev/null 2>&1 || true
./target/release/rustemd --user start backup.timer >/dev/null 2>&1 || true

vhs demo/demo.tape
echo "wrote docs/demo.gif"
