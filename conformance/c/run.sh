#!/usr/bin/env bash
# Builds the driver and cavalry-ffi in release, compiles conformance/c/quickstart.c
# against the committed header with the runner's C compiler, links the static library,
# starts a driver on a directory of its own, runs the program and cleans up. `just
# conformance` runs this after the Rust vector suites (spec/c-abi.md; ADR 0051).
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-c-$$"
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

echo "conformance/c: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

echo "conformance/c: compile quickstart.c with ${CC:-cc} against crates/cavalry-ffi/include/cavalry.h"
case "$(uname -s)" in
    Darwin) libs="-framework CoreFoundation -framework Security -lresolv" ;;
    *) libs="-lpthread -ldl -lm" ;;
esac
mkdir -p target/conformance
# shellcheck disable=SC2086
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -I crates/cavalry-ffi/include \
    conformance/c/quickstart.c target/release/libcavalry_ffi.a $libs \
    -o target/conformance/quickstart-c

echo "conformance/c: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it: doctor exits 1 on
# any failed finding, and a `grep -q` in a pipe exits at the first match, so doctor's
# next write would fail and pipefail would report a running driver as not running.
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || {
    echo "conformance/c: the driver did not report running" >&2; exit 1; }

echo "conformance/c: run the program"
target/conformance/quickstart-c
