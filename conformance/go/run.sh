#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's static library in release, vets, builds and tests the
# Go client (cgo against the header and the library; nothing from a module proxy), its
# tests being the subscription table at its bound and the trampoline's refusal of a slot
# the table does not hold, which need no driver; checks that
# its generated files are gofmt's, starts a driver on a directory of its own, runs
# conformance/go/quickstart through clients/go with the runner's Go, then
# clients/go/README.md's example through conformance/go/readme; then starts three cluster
# members on the loopback, each a `cavalry run` in a member's role, and runs
# conformance/go/committed_log beside member 1, the client's committed log against a
# cluster (spec/c-abi.md, version 2; ADR 0070); and cleans up.
# `just conformance` runs this after conformance/node (clients/README.md; ADR 0046, card
# 207).
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-go-$$"
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

echo "conformance/go: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

echo "conformance/go: vet, build and test the client with $(go version)"
unformatted=$(gofmt -l clients/go conformance/go)
if [ -n "$unformatted" ]; then
    echo "conformance/go: not gofmt's: $unformatted; the generator writes gofmt's own layout, so fix clients/generate_go.py" >&2
    exit 1
fi
(cd clients/go && go vet ./... && go build ./... && go test -count=1 ./...)

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

# The committed log: three members of one cluster, each with five free loopback ports,
# its own driver directory and its own log directory, started together as
# docs/how-to/run-a-cluster.md starts them (conformance/python/run.sh is the shape). A
# member's output goes to a file beside its directories, printed if the cluster does not
# form.
echo "conformance/go: cavalry run, three cluster members, in the background, under $cluster"
mkdir -p "$cluster"
CAVALRY_CLUSTER_MEMBERS=$(cd conformance/go && go run ./members)
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
    echo "conformance/go: member 1 did not close its election in 60 s; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

echo "conformance/go: run committed_log beside member 1"
(cd conformance/go && go run ./committed_log "$cluster/member-1" 1)
