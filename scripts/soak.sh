#!/usr/bin/env bash
# The soak: a driver recording a steady stream for days, with doctor and the counters
# sampled on a schedule, so that what CI sees for seconds is seen for a week
# (docs/how-to/set-up-a-soak-host.md; card 159). `cavalry-soak.service` runs this; it
# runs by hand too. It starts the driver on its own directories, the soak program
# (crates/cavalry/examples/soak.rs) publishing and reading back the stream, and a
# recording of the stream; then, until it is told to stop:
#
#   - every SAMPLE_SECONDS it writes `cavalry doctor`'s report and a copy of the
#     counters file under $SOAK_DIR/samples, named by the time, and appends one line
#     for the driver and one for the program to the run's own file there,
#     processes-<the run's start>.txt, with the same time: the process's resident
#     memory, its parts, its virtual size, its threads and its open descriptors (card
#     240); nothing prunes them;
#   - every ROTATE_SECONDS it stops the recording and starts a new one, purges the
#     segments of the one it stopped before the middle of its hour, and deletes every
#     recording older than KEEP_RECORDINGS rotations, so retention runs on a host whose
#     disk would otherwise fill (ADR 0031).
#
# SIGTERM or SIGINT stops it cleanly: the stop file tells the program to finish reading
# what it sent and close, the recording is stopped, the driver gets SIGTERM, and the
# samples and recordings stay where they are. It exits 0 after a stop that was asked
# for, and 1 when the program or the driver ended the run on its own, so that the
# service shows `failed` for a week that ended early. Every line it prints goes to
# $SOAK_DIR/soak.log through the service, or to the terminal by hand.
#
# Settings, by environment variable, each with its default:
#   SOAK_DIR          /srv/cavalry-soak   the log directory, samples and state, on the big drive
#   CAVALRY_DIR       /dev/shm/cavalry-soak   the driver's directory, on a tmpfs
#   SOAK_BIN          target/release      where `cavalry` and `examples/soak` are
#   STREAM_ID         1001
#   RATE              10000               messages per second
#   BODY              32                  bytes per message
#   SAMPLE_SECONDS    60                  each sample copies the 6 MiB counters file, so
#                                         samples/ grows about 9 GB a day at 60 and is
#                                         never pruned: remove it when the week is read
#   ROTATE_SECONDS    3600
#   KEEP_RECORDINGS   24                  rotations kept before a recording is deleted
set -uo pipefail

SOAK_DIR=${SOAK_DIR:-/srv/cavalry-soak}
export CAVALRY_DIR=${CAVALRY_DIR:-/dev/shm/cavalry-soak}
SOAK_BIN=${SOAK_BIN:-target/release}
STREAM_ID=${STREAM_ID:-1001}
RATE=${RATE:-10000}
BODY=${BODY:-32}
SAMPLE_SECONDS=${SAMPLE_SECONDS:-60}
ROTATE_SECONDS=${ROTATE_SECONDS:-3600}
KEEP_RECORDINGS=${KEEP_RECORDINGS:-24}

cavalry="$SOAK_BIN/cavalry"
soak="$SOAK_BIN/examples/soak"
log_dir="$SOAK_DIR/log"
samples="$SOAK_DIR/samples"
state="$SOAK_DIR/recordings.txt"   # one line per recording started: <id> <start epoch> <state>
# One file of process samples for each run, named by the run's start, so that a run's
# first line and its last are of the same two processes.
processes="$samples/processes-$(date -u +%Y%m%dT%H%M%SZ).txt"
stop_file="$SOAK_DIR/stop"

say() { printf 'soak.sh: %s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
record() { "$cavalry" record --dir "$CAVALRY_DIR" --log-dir "$log_dir" "$@"; }

[ -x "$cavalry" ] && [ -x "$soak" ] || {
    say "no binaries at $SOAK_BIN: run cargo build --release -p cavalry --bin cavalry --example soak first"
    exit 1
}
mkdir -p "$log_dir" "$samples" || exit 1
rm -f "$stop_file"
touch "$state"
# A recording a previous run left `live` was stopped by the driver's recovery at this
# run's start (or by that run's stop), so the rotation's delete must reach it.
awk '$3 == "live" { $3 = "stopped" } { print }' "$state" > "$state.new" && mv "$state.new" "$state"

driver_pid=""
soak_pid=""
current=""
stopping=0
# Stops the program, the recording and the driver in that order and exits with the code
# given: 0, the default and the trap's, for a stop that was asked for; 1 for one the
# program or the driver forced by exiting on its own.
stop_all() {
    local code=${1:-0}
    [ "$stopping" = 1 ] && return
    stopping=1
    say "stopping"
    touch "$stop_file"
    if [ -n "$soak_pid" ] && kill -0 "$soak_pid" 2>/dev/null; then
        wait "$soak_pid"; say "soak program exited $?"
    fi
    if [ -n "$current" ]; then
        record --stop "$current" && mark "$current" stopped
    fi
    if [ -n "$driver_pid" ] && kill -0 "$driver_pid" 2>/dev/null; then
        kill -TERM "$driver_pid" 2>/dev/null
        wait "$driver_pid"; say "driver exited $?"
    fi
    say "stopped; the recordings are under $log_dir and the samples under $samples"
    exit "$code"
}
trap stop_all TERM INT

# The state file: one line per recording, `<id> <start epoch> live|stopped|deleted`.
mark() {
    local id=$1 what=$2
    awk -v id="$id" -v what="$what" '$1 == id { $3 = what } { print }' "$state" > "$state.new" && mv "$state.new" "$state"
}
start_recording() {
    local out id
    out=$(record "$STREAM_ID" 2>&1) || { say "record $STREAM_ID failed: $out"; return 1; }
    id=$(printf '%s\n' "$out" | sed -n -E 's/^recording ([0-9]+) started.*/\1/p')
    [ -n "$id" ] || { say "record printed no id: $out"; return 1; }
    printf '%s %s live\n' "$id" "$(date +%s)" >> "$state"
    current=$id
    say "recording $id started"
}
rotate() {
    local previous=$current now
    now=$(date +%s)
    if [ -n "$previous" ]; then
        if record --stop "$previous"; then
            mark "$previous" stopped; say "recording $previous stopped"
            # Nothing is current now: a start refused below must not make the next
            # rotation stop and purge this one again.
            current=""
        fi
    fi
    start_recording || say "no recording until the next rotation"
    if [ -n "$previous" ]; then
        # The purge of the hour's first half: an instant, resolved through the time
        # index and rounded down to a segment base by the command.
        local half=$(( (now - ROTATE_SECONDS / 2) * 1000000000 ))
        record --purge "$previous" --before "@$half" || say "purge of $previous refused (nothing to purge is error 23)"
    fi
    # Every recording older than KEEP_RECORDINGS rotations goes.
    local cutoff=$(( now - KEEP_RECORDINGS * ROTATE_SECONDS ))
    while read -r id started what; do
        if [ "$what" = stopped ] && [ "$started" -lt "$cutoff" ]; then
            record --delete "$id" && mark "$id" deleted && say "recording $id deleted"
        fi
    done < "$state"
}
# One line for one process, to standard output: what the kernel says of it at this
# sample, so that a week shows a course and not one reading (card 240):
#
#   <time> <name> pid=<pid> rss_kib=<n> anon_kib=<n> file_kib=<n> shmem_kib=<n> vsz_kib=<n> threads=<n> fds=<n>
#
# `rss_kib` is the resident memory and `vsz_kib` the virtual size, in KiB. On Linux,
# from /proc/<pid>/status, `anon_kib`, `file_kib` and `shmem_kib` are the resident
# memory's three parts: the heap and stacks, the files mapped from a disk, and the
# files mapped from a tmpfs, which is where the driver's directory is. A driver maps
# its rings, its counters file and every log buffer from that directory and touches
# each page once, so `shmem_kib` is the mappings and `anon_kib` is what could leak.
# `fds` counts /proc/<pid>/fd. Where there is no /proc, as on macOS, `ps` gives the
# resident memory, the virtual size and the threads, `lsof` the descriptors, and the
# three parts read `-`; whether there is one is decided once, when the script starts.
# A value that could not be read is `-`, never 0, and a process whose resident memory
# could not be read, because it ended before or while the script looked, has
# `<time> <name> pid=<pid> gone` for its line. On Linux the function starts no
# process of its own, so that a sample costs the host next to nothing.
#
# A soak of a cluster (ADR 0061, condition 5) samples each member with one more call.
if [ -r /proc/self/status ]; then have_proc=1; else have_proc=0; fi
sample_process() {
    local at=$1 name=$2 pid=$3
    local rss="" anon=- file=- shmem=- vsz=- threads=- fds=- key value _
    if [ -n "$pid" ] && [ "$have_proc" = 1 ]; then
        while read -r key value _; do
            case "$key" in
                VmRSS:) rss=$value ;;
                RssAnon:) anon=$value ;;
                RssFile:) file=$value ;;
                RssShmem:) shmem=$value ;;
                VmSize:) vsz=$value ;;
                Threads:) threads=$value ;;
            esac
        done 2>/dev/null < "/proc/$pid/status"
        set -- "/proc/$pid/fd"/*
        if [ -e "$1" ]; then fds=$#; fi
    elif [ -n "$pid" ]; then
        read -r rss vsz < <(ps -o rss= -o vsz= -p "$pid" 2>/dev/null)
        if [ -n "$rss" ]; then
            threads=$(ps -M -p "$pid" 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
            fds=$(lsof -n -P -a -p "$pid" -d 0-1048575 2>/dev/null | tail -n +2 | wc -l | tr -d ' ')
            if [ "$threads" = 0 ]; then threads=-; fi
            if [ "$fds" = 0 ]; then fds=-; fi
        fi
    fi
    if [ -z "$rss" ]; then
        printf '%s %s pid=%s gone\n' "$at" "$name" "${pid:--}"
        return
    fi
    printf '%s %s pid=%s rss_kib=%s anon_kib=%s file_kib=%s shmem_kib=%s vsz_kib=%s threads=%s fds=%s\n' \
        "$at" "$name" "$pid" "$rss" "$anon" "$file" "$shmem" "${vsz:--}" "$threads" "$fds"
}
sample() {
    local at
    at=$(date -u +%Y%m%dT%H%M%SZ)
    "$cavalry" doctor --dir "$CAVALRY_DIR" --log-dir "$log_dir" > "$samples/doctor-$at.txt" 2>&1
    printf 'exit %s\n' "$?" >> "$samples/doctor-$at.txt"
    cp "$CAVALRY_DIR/counters" "$samples/counters-$at" 2>/dev/null || say "no counters file to copy"
    {
        sample_process "$at" driver "$driver_pid"
        sample_process "$at" soak "$soak_pid"
    } >> "$processes"
}

say "starting: driver on $CAVALRY_DIR, log directory $log_dir, stream $STREAM_ID at $RATE messages/s"
"$cavalry" run --dir "$CAVALRY_DIR" --log-dir "$log_dir" &
driver_pid=$!
for _ in $(seq 1 100); do
    "$cavalry" doctor --dir "$CAVALRY_DIR" >/dev/null 2>&1 && break
    sleep 0.1
done
"$cavalry" doctor --dir "$CAVALRY_DIR" >/dev/null 2>&1 || { say "the driver did not come up"; stop_all 1; }
"$soak" --stream "$STREAM_ID" --rate "$RATE" --body "$BODY" --report "$SAMPLE_SECONDS" --stop-file "$stop_file" &
soak_pid=$!
sleep 1
start_recording || say "no recording until the first rotation"
sample

last_sample=$(date +%s)
last_rotate=$last_sample
while :; do
    sleep 1
    if ! kill -0 "$soak_pid" 2>/dev/null; then
        wait "$soak_pid"; say "the soak program exited $? on its own; stopping"
        soak_pid=""; stop_all 1
    fi
    if ! kill -0 "$driver_pid" 2>/dev/null; then
        wait "$driver_pid"; say "the driver exited $? on its own; stopping"
        driver_pid=""; stop_all 1
    fi
    now=$(date +%s)
    if [ $((now - last_sample)) -ge "$SAMPLE_SECONDS" ]; then sample; last_sample=$now; fi
    if [ $((now - last_rotate)) -ge "$ROTATE_SECONDS" ]; then rotate; last_rotate=$now; fi
done
