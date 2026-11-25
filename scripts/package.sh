#!/usr/bin/env bash
# The clients' packages, each built with the native library inside and installed as a
# user installs it (work item 469, the first part of work item 369; ADR 0165). `just
# package` runs it; CI runs it on Linux for every pull request and on Linux and macOS
# for every commit on `master`, as it runs the other gates (ADR 0073).
#
# Today there is one package, the Python client's (clients/python/README.md, "The
# package"). The script builds the driver and cavalry-ffi's shared library in release,
# starts a driver on a directory of its own, and runs scripts/package-python.py, which
# builds the wheel, installs it into a new environment outside the checkout, runs the
# client README's example there against this driver, and shows that the same wheel
# without its library fails. A client's package joins here with a script of its own.
#
# Nothing is published and nothing comes from a package index.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-package-$$"
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

echo "package: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

echo "package: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it (conformance/c/run.sh).
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || { echo "package: the driver did not report running" >&2; exit 1; }

echo "package: the Python client's package, with $(python3 --version)"
python3 scripts/package-python.py
