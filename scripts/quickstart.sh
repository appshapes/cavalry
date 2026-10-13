#!/usr/bin/env bash
# The README's ten-minute path, end to end and timed (CLAUDE.md, gate 5). `just
# quickstart` runs it; CI runs it on Linux and macOS. Every step is one the README tells
# a reader to type, with `cargo run` where the reader has `cavalry` installed, and the
# script fails on the first step that does not do what the README says it does.
#
# The driver gets a directory of its own under the temporary directory (or /dev/shm when
# it exists), through CAVALRY_DIR, so that a driver already running for the user on this
# host is left alone, and the directory is removed at the end.
set -euo pipefail

cd "$(dirname "$0")/.."
started=$(date +%s)

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-quickstart-$$"
rm -rf "$CAVALRY_DIR"
cleanup() {
    if [ -n "${driver_pid:-}" ] && kill -0 "$driver_pid" 2>/dev/null; then
        kill -TERM "$driver_pid" 2>/dev/null || true
        wait "$driver_pid" 2>/dev/null || true
    fi
    rm -rf "$CAVALRY_DIR"
}
trap cleanup EXIT

step() { printf '\nquickstart: %s\n' "$1"; }

step "1. build the binary and the example (the README's cargo install)"
cargo build --release -p cavalry --bin cavalry --example quickstart

cavalry=target/release/cavalry
step "2. cavalry run, in the background, on $CAVALRY_DIR"
"$cavalry" run &
driver_pid=$!
for _ in $(seq 1 100); do
    if "$cavalry" doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
"$cavalry" doctor | grep -q 'is running' || { echo "quickstart: the driver did not report running" >&2; exit 1; }

step "3. the program: publish 1,000 messages and read them back"
program=$(target/release/examples/quickstart)
printf '%s\n' "$program"
grep -q 'received once, in order' <<<"$program" || {
    echo "quickstart: the program did not report every message once, in order" >&2; exit 1; }

step "4. cavalry doctor"
report=$("$cavalry" doctor)
printf '%s\n' "$report"
grep -q '0 failed' <<<"$report" || { echo "quickstart: doctor reported a failure" >&2; exit 1; }

step "5. stop the driver with SIGTERM"
kill -TERM "$driver_pid"
wait "$driver_pid"
driver_pid=""
if "$cavalry" doctor >/dev/null 2>&1; then
    echo "quickstart: doctor should exit 1 on a closed driver" >&2; exit 1
fi

printf '\nquickstart: the ten-minute path took %s s\n' "$(( $(date +%s) - started ))"
