#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's static library in release, vets and builds the Go
# client (cgo against the header and the library; nothing from a module proxy), checks that
# its generated files are gofmt's, starts a driver on a directory of its own, runs
# conformance/go/quickstart through clients/go with the runner's Go, then
# clients/go/README.md's example through conformance/go/readme, and cleans up.
# `just conformance` runs this after conformance/node (clients/README.md; ADR 0046, card
# 207).
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-go-$$"
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

echo "conformance/go: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

echo "conformance/go: vet and build the client with $(go version)"
unformatted=$(gofmt -l clients/go conformance/go)
if [ -n "$unformatted" ]; then
    echo "conformance/go: not gofmt's: $unformatted; the generator writes gofmt's own layout, so fix clients/generate_go.py" >&2
    exit 1
fi
(cd clients/go && go vet ./... && go build ./...)

echo "conformance/go: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it (conformance/c/run.sh).
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || {
    echo "conformance/go: the driver did not report running" >&2; exit 1; }

echo "conformance/go: run quickstart"
(cd conformance/go && go run ./quickstart)

# The README's example against the same driver: it fails unless the program prints the
# line the README says the reader will see (conformance/go/readme).
echo "conformance/go: run clients/go/README.md's example"
(cd conformance/go && go run ./readme)
