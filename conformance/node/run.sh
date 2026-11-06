#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's static library in release, builds the Node client's
# addon against them with npm's own node-gyp (`npm install` in clients/node, nothing from
# the registry), starts a driver on a directory of its own, runs
# conformance/node/quickstart.js through clients/node with the runner's Node, then
# clients/node/README.md's example through conformance/node/readme.js, and cleans up.
# `just conformance` runs this after conformance/python (clients/README.md; ADR 0046, card
# 206). The runner takes `--expose-gc` so that its allocation round can read the heap after
# a full collection.
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-node-$$"
export CAVALRY_LOG_DIR="${CAVALRY_DIR}.log"
rm -rf "$CAVALRY_DIR" "$CAVALRY_LOG_DIR"
cleanup() {
    if [ -n "${driver_pid:-}" ] && kill -0 "$driver_pid" 2>/dev/null; then
        kill -TERM "$driver_pid" 2>/dev/null || true
        wait "$driver_pid" 2>/dev/null || true
    fi
    rm -rf "$CAVALRY_DIR" "$CAVALRY_LOG_DIR"
}
trap cleanup EXIT

echo "conformance/node: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

echo "conformance/node: build the addon with $(node --version) and npm $(npm --version)"
(cd clients/node && npm install --no-audit --no-fund --loglevel=warn)

echo "conformance/node: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it (conformance/c/run.sh).
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || {
    echo "conformance/node: the driver did not report running" >&2; exit 1; }

echo "conformance/node: run quickstart.js with $(node --version)"
node --expose-gc conformance/node/quickstart.js

# The README's example against the same driver: it fails unless the program prints the
# line the README says the reader will see (conformance/node/readme.js).
echo "conformance/node: run clients/node/README.md's example"
node conformance/node/readme.js
