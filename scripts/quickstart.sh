#!/usr/bin/env bash
# The README's ten-minute path, end to end and timed (CLAUDE.md, gate 5). `just
# quickstart` runs it; CI runs it on Linux and macOS. Every step is one the README tells
# a reader to type, running the binaries `cargo build --release` puts under
# `target/release/` where a release would put `cavalry` on the PATH, and the script fails on the first step that does not do what the README says it does.
#
# The driver gets a directory of its own under the temporary directory (or /dev/shm when
# it exists), through CAVALRY_DIR, so that a driver already running for the user on this
# host is left alone, and the directory is removed at the end.
set -euo pipefail

cd "$(dirname "$0")/.."
started=$(date +%s)

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-quickstart-$$"
# The driver's log directory follows XDG_DATA_HOME (ADR 0025), so the run's recording
# lands beside its driver directory and never under the user's home.
export XDG_DATA_HOME="${CAVALRY_DIR}-data"
rm -rf "$CAVALRY_DIR" "$XDG_DATA_HOME"
cleanup() {
    if [ -n "${driver_pid:-}" ] && kill -0 "$driver_pid" 2>/dev/null; then
        kill -TERM "$driver_pid" 2>/dev/null || true
        wait "$driver_pid" 2>/dev/null || true
    fi
    rm -rf "$CAVALRY_DIR" "$XDG_DATA_HOME"
}
trap cleanup EXIT

step() { printf '\nquickstart: %s\n' "$1"; }

step "1. build the binary and the example (the README's cargo build --release)"
cargo build --release -p cavalry --bin cavalry --example quickstart

cavalry=target/release/cavalry
step "2. cavalry run, in the background, on $CAVALRY_DIR"
"$cavalry" run &
driver_pid=$!
for _ in $(seq 1 100); do
    if "$cavalry" doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
running=$("$cavalry" doctor || true)
grep -q 'is running' <<<"$running" || { echo "quickstart: the driver did not report running" >&2; exit 1; }

step "3. the program: record the stream, publish 1,000 messages and read them back"
program=$(target/release/examples/quickstart)
printf '%s\n' "$program"
grep -q 'received once, in order' <<<"$program" || {
    echo "quickstart: the program did not report every message once, in order" >&2; exit 1; }
recording=$(sed -n 's/^quickstart: recording \([0-9]*\) of stream .* stopped;.*/\1/p' <<<"$program")
[ -n "$recording" ] || { echo "quickstart: the program did not report its recording" >&2; exit 1; }

step "4. cavalry replay: the recording played back from the durable log"
replayed=$("$cavalry" replay --recording "$recording" --from 0)
tail -2 <<<"$replayed"
grep -q "ended at position 64000: 1000 messages from position 0" <<<"$replayed" || {
    echo "quickstart: the replay did not end at 64000 with 1000 messages" >&2; exit 1; }
grep -q '^63936 e703000000000000$' <<<"$replayed" || {
    echo "quickstart: the thousandth message is not at position 63936 with the body 999" >&2; exit 1; }
[ "$(grep -c '^[0-9]* [0-9a-f]*$' <<<"$replayed")" = 1000 ] || {
    echo "quickstart: the replay did not print 1000 message lines" >&2; exit 1; }

step "5. cavalry doctor"
report=$("$cavalry" doctor)
printf '%s\n' "$report"
grep -q '0 failed' <<<"$report" || { echo "quickstart: doctor reported a failure" >&2; exit 1; }

step "6. stop the driver with SIGTERM"
kill -TERM "$driver_pid"
wait "$driver_pid"
driver_pid=""
if "$cavalry" doctor >/dev/null 2>&1; then
    echo "quickstart: doctor should exit 1 on a closed driver" >&2; exit 1
fi

printf '\nquickstart: the ten-minute path took %s s\n' "$(( $(date +%s) - started ))"
