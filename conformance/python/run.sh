#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, starts a driver on a
# directory of its own, runs conformance/python/quickstart.py through clients/python with
# the runner's Python 3, then clients/python/README.md's example through
# conformance/python/readme.py, and cleans up. `just conformance` runs this after conformance/c
# (clients/README.md; ADR 0046, card 205). Nothing is installed: ctypes is in the standard
# library.
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-python-$$"
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

echo "conformance/python: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

echo "conformance/python: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it (conformance/c/run.sh).
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || {
    echo "conformance/python: the driver did not report running" >&2; exit 1; }

echo "conformance/python: run quickstart.py with $(python3 --version)"
PYTHONPATH=clients/python python3 conformance/python/quickstart.py

# The README's example against the same driver: it fails unless the program prints the
# line the README says the reader will see (conformance/python/readme.py).
echo "conformance/python: run clients/python/README.md's example"
python3 conformance/python/readme.py
