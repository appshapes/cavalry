#!/usr/bin/env bash
# The deterministic replay gate (CLAUDE.md, gate 1; ADR 0030). `just replay-check` runs
# it; CI runs it on Linux and macOS. A driver on a directory of its own records a stream
# of 100,000 messages whose bodies carry a hash chain while a live subscriber prints
# every message; `cavalry replay` plays the recording twice from its start and once from
# an instant resolved through the time index; the live lines and the two replays' lines
# must hash the same, and the replay from the instant must start at the position the
# index gives. A byte out of place or a message missing anywhere fails it.
set -euo pipefail

cd "$(dirname "$0")/.."
started=$(date +%s)

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-replay-check-$$"
# The log directory follows XDG_DATA_HOME (ADR 0025): the run's recording lands beside
# its driver directory and never under the user's home.
export XDG_DATA_HOME="${CAVALRY_DIR}-data"
work="${CAVALRY_DIR}-out"
rm -rf "$CAVALRY_DIR" "$XDG_DATA_HOME" "$work"
mkdir -p "$work"
cleanup() {
    if [ -n "${driver_pid:-}" ] && kill -0 "$driver_pid" 2>/dev/null; then
        kill -TERM "$driver_pid" 2>/dev/null || true
        wait "$driver_pid" 2>/dev/null || true
    fi
    rm -rf "$CAVALRY_DIR" "$XDG_DATA_HOME" "$work"
}
trap cleanup EXIT

step() { printf '\nreplay-check: %s\n' "$1"; }
digest() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

step "1. build the binary and the program"
cargo build --release -p cavalry --bin cavalry --example replay-check

cavalry=target/release/cavalry
step "2. cavalry run, in the background, on $CAVALRY_DIR"
"$cavalry" run &
driver_pid=$!
for _ in $(seq 1 100); do
    if "$cavalry" doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
"$cavalry" doctor | grep -q 'is running' || { echo "replay-check: the driver did not report running" >&2; exit 1; }

step "3. record 100,000 messages while a live subscriber prints them"
target/release/examples/replay-check --messages 100000 > "$work/live.txt" 2> "$work/info.txt"
cat "$work/info.txt"
recording=$(sed -n 's/.*recording=\([0-9]*\).*/\1/p' "$work/info.txt")
end=$(sed -n 's/.*end=\([0-9]*\).*/\1/p' "$work/info.txt")
instant=$(sed -n 's/.*instant=\(@[0-9]*\).*/\1/p' "$work/info.txt")
instant_position=$(sed -n 's/.*instant_position=\([0-9]*\).*/\1/p' "$work/info.txt")
lines=$(wc -l < "$work/live.txt" | tr -d ' ')
[ "$lines" = 100000 ] || { echo "replay-check: the live subscriber printed $lines lines, not 100000" >&2; exit 1; }

step "4. replay recording $recording twice from its start, and hash the three"
for n in 1 2; do
    "$cavalry" replay --recording "$recording" --from 0 > "$work/replay$n.txt"
    tail -1 "$work/replay$n.txt"
    grep -q "ended at position $end: 100000 messages from position 0" "$work/replay$n.txt" || {
        echo "replay-check: replay $n did not end at $end with 100000 messages" >&2; exit 1; }
    sed '$d' "$work/replay$n.txt" > "$work/replay$n.body"
done
live=$(digest "$work/live.txt"); one=$(digest "$work/replay1.body"); two=$(digest "$work/replay2.body")
printf 'live    %s\nreplay1 %s\nreplay2 %s\n' "$live" "$one" "$two"
[ "$live" = "$one" ] && [ "$one" = "$two" ] || { echo "replay-check: the three hashes differ" >&2; exit 1; }

step "5. replay from the instant $instant, which the index resolves to $instant_position"
"$cavalry" replay --recording "$recording" --from "$instant" > "$work/replay3.txt"
tail -1 "$work/replay3.txt"
grep -q "ended at position $end: .* messages from position $instant_position\$" "$work/replay3.txt" || {
    echo "replay-check: the replay from the instant did not start at $instant_position" >&2; exit 1; }
first=$(head -1 "$work/replay3.txt" | cut -d' ' -f1)
[ "$first" = "$instant_position" ] || { echo "replay-check: the first line is at $first, not $instant_position" >&2; exit 1; }
grep -F "$(head -1 "$work/replay3.txt")" "$work/live.txt" >/dev/null || {
    echo "replay-check: the first line of the replay from the instant is not a line the live subscriber saw" >&2; exit 1; }

step "6. stop the driver"
kill -TERM "$driver_pid"
wait "$driver_pid"
driver_pid=""

printf '\nreplay-check: three equal hashes, %s s\n' "$(( $(date +%s) - started ))"
