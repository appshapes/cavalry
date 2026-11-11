#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, starts a driver on a
# directory of its own, runs conformance/python/quickstart.py through clients/python with
# the runner's Python 3, then clients/python/README.md's example through
# conformance/python/readme.py; then starts three cluster members on the loopback, each a
# `cavalry run` in a member's role, and runs conformance/python/committed_log.py beside
# member 1, the client's committed log against a cluster (spec/c-abi.md, version 2; ADR
# 0070); and cleans up. `just conformance` runs this after conformance/c
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

# The committed log: three members of one cluster, each with five free loopback ports,
# its own driver directory and its own log directory, started together as
# docs/how-to/run-a-cluster.md starts them. A member's output goes to a file beside its
# directories, printed if the cluster does not form.
echo "conformance/python: cavalry run, three cluster members, in the background, under $cluster"
mkdir -p "$cluster"
CAVALRY_CLUSTER_MEMBERS=$(python3 -c '
import socket
sockets = [socket.socket(socket.AF_INET, socket.SOCK_DGRAM) for _ in range(15)]
for s in sockets:
    s.bind(("127.0.0.1", 0))
ports = [s.getsockname()[1] for s in sockets]
print("|".join(str(m) + "," + ",".join("127.0.0.1:%d" % p for p in ports[m * 5:m * 5 + 5]) for m in range(3)))
')
export CAVALRY_CLUSTER_MEMBERS
for id in 0 1 2; do
    CAVALRY_CLUSTER_MEMBER_ID=$id target/release/cavalry run --dir "$cluster/member-$id" \
        --log-dir "$cluster/member-$id-log" >"$cluster/member-$id.out" 2>&1 &
    member_pids="$member_pids $!"
done
formed=""
for _ in $(seq 1 600); do
    if CAVALRY_CLUSTER_MEMBER_ID=1 target/release/cavalry doctor --dir "$cluster/member-1" \
        --log-dir "$cluster/member-1-log" 2>/dev/null | grep -q 'election closed'; then formed=yes; break; fi
    sleep 0.1
done
[ -n "$formed" ] || {
    echo "conformance/python: member 1 did not close its election in 60 s; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

echo "conformance/python: run committed_log.py beside member 1"
python3 conformance/python/committed_log.py "$cluster/member-1" 1
