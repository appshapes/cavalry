#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, starts a driver on a
# directory of its own, runs conformance/python/quickstart.py through clients/python with
# the runner's Python 3, then clients/python/README.md's example through
# conformance/python/readme.py; then starts three cluster members on the loopback, each a
# `cavalry run` in a member's role, and runs conformance/python/committed_log.py beside
# one of them, member 1 unless it joined late (conformance/log-member.sh), the client's
# committed log against a cluster (spec/c-abi.md, version 2; ADR 0070), and then
# conformance/python/cluster.py, the cluster round: the client's service beside each
# member and its session on the first driver (spec/c-abi.md, versions 4 and 5; ADR 0124);
# and cleans up. `just conformance` runs this after conformance/c
# (clients/README.md; ADR 0046, card 205). Nothing is installed: ctypes is in the standard
# library.
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-python-$$"
export CAVALRY_LOG_DIR="${CAVALRY_DIR}.log"
cluster="${CAVALRY_DIR}.cluster"
rm -rf "$CAVALRY_DIR" "$CAVALRY_LOG_DIR" "$cluster"
member_pids=""
cleanup() {
    for pid in ${driver_pid:-} $member_pids; do
        if kill -0 "$pid" 2>/dev/null; then
            kill -TERM "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    rm -rf "$CAVALRY_DIR" "$CAVALRY_LOG_DIR" "$cluster"
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

# The committed log: three members of one cluster, each with five loopback ports, its own
# driver directory and its own log directory, started together as
# docs/how-to/run-a-cluster.md starts them. A member's output goes to a file beside its
# directories, printed if the cluster does not form or the round fails.
#
# The sixteen ports, fifteen for the members and one for the session's response channel,
# are counted out of 10000 to 19999, never bound to be chosen, as
# `loopback_ports` in crates/cavalry/tests/cli.rs counts its own: a port found by binding
# to port 0 is one of those the kernel gives each driver's own send sockets, so another
# member could be given it before its owner binds it. The shell's process id picks where
# the count starts, through that function's hash, so each runner starts elsewhere.
echo "conformance/python: cavalry run, three cluster members, in the background, under $cluster"
mkdir -p "$cluster"
start=$(( ($$ * 2654435761) % 10000 ))
CAVALRY_CLUSTER_MEMBERS=""
for id in 0 1 2; do
    member="$id"
    for i in 0 1 2 3 4; do
        member="$member,127.0.0.1:$(( 10000 + (start + id * 5 + i) % 10000 ))"
    done
    CAVALRY_CLUSTER_MEMBERS="${CAVALRY_CLUSTER_MEMBERS:+$CAVALRY_CLUSTER_MEMBERS|}$member"
done
export CAVALRY_CLUSTER_MEMBERS
response="cavalry:udp?endpoint=127.0.0.1:$(( 10000 + (start + 15) % 10000 ))"
for id in 0 1 2; do
    CAVALRY_CLUSTER_MEMBER_ID=$id target/release/cavalry run --dir "$cluster/member-$id" \
        --log-dir "$cluster/member-$id-log" >"$cluster/member-$id.out" 2>&1 &
    member_pids="$member_pids $!"
done
# Doctor's whole output first, then the match, as above: doctor exits 1 on any failed
# finding, and a `grep -q` that ends at its match can fail doctor's write, so under
# pipefail a pipe of the two could fail for a cluster whose election had closed.
formed=""
for _ in $(seq 1 600); do
    election=$(CAVALRY_CLUSTER_MEMBER_ID=1 target/release/cavalry doctor --dir "$cluster/member-1" \
        --log-dir "$cluster/member-1-log" 2>/dev/null || true)
    if grep -q 'election closed' <<<"$election"; then formed=yes; break; fi
    sleep 0.1
done
[ -n "$formed" ] || {
    echo "conformance/python: member 1 did not close its election in 60 s; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

# The member the round reads beside, and the recording that holds its log: member 1,
# unless it joined its leader's log late and so holds its log in two recordings, and
# then another (conformance/log-member.sh; ADR 0103).
chosen=$(conformance/log-member.sh "$cluster") || {
    echo "conformance/python: no member holds its log in one recording; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}
read -r member recording <<<"$chosen"

echo "conformance/python: run committed_log.py beside member $member"
python3 conformance/python/committed_log.py "$cluster/member-$member" "$member" "$recording" || {
    echo "conformance/python: committed_log.py failed beside member $member; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

# The cluster round, after the committed log's: a service beside each member and a
# session on the runner's first driver. The committed log round left the cluster as it
# found it, one entry, the term's first.
echo "conformance/python: run cluster.py, a service beside each member and a session on $CAVALRY_DIR"
python3 conformance/python/cluster.py "$cluster" "$CAVALRY_CLUSTER_MEMBERS" "$response" || {
    echo "conformance/python: cluster.py failed; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}
