#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, compiles the JVM client
# with the runner's javac (Java 25 or later, every lint a warning and every warning an
# error: no build tool, no dependency), then conformance/jvm's Quickstart, Readme and
# CommittedLogRound, starts a driver on a directory of its own, runs Quickstart through
# clients/jvm with the runner's java, then clients/jvm/README.md's example through Readme;
# then starts three cluster members on the loopback, each a `cavalry run` in a member's
# role, and runs CommittedLogRound beside member 1, the client's committed log against a
# cluster (spec/c-abi.md, version 2; ADR 0070); and cleans up.
# `just conformance` runs this after conformance/go (clients/README.md; ADR 0046, card
# 208).
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-jvm-$$"
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

echo "conformance/jvm: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

# The client as the README builds it, plus every lint as an error, which the generator's
# test also asks of the generated files; then the runner and the README's test, which
# use the client's classes.
echo "conformance/jvm: compile the client and the runner with $(javac -version 2>&1)"
rm -rf clients/jvm/build/classes conformance/jvm/build
javac --release 25 -Xlint:all -Werror -d clients/jvm/build/classes clients/jvm/src/com/appshapes/cavalry/*.java
javac --release 25 -Xlint:all -Werror -cp clients/jvm/build/classes -d conformance/jvm/build \
    conformance/jvm/Quickstart.java conformance/jvm/Readme.java conformance/jvm/CommittedLogRound.java

echo "conformance/jvm: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it (conformance/c/run.sh).
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || {
    echo "conformance/jvm: the driver did not report running" >&2; exit 1; }

echo "conformance/jvm: run Quickstart with $(java -version 2>&1 | head -1)"
java --enable-native-access=ALL-UNNAMED -cp clients/jvm/build/classes:conformance/jvm/build Quickstart

# The README's example against the same driver: it fails unless the program prints the
# line the README says the reader will see (conformance/jvm/Readme.java).
echo "conformance/jvm: run clients/jvm/README.md's example"
java -cp conformance/jvm/build Readme

# The committed log: three members of one cluster, each with five loopback ports, its own
# driver directory and its own log directory, started together as
# docs/how-to/run-a-cluster.md starts them (conformance/python/run.sh is the shape). A
# member's output goes to a file beside its directories, printed if the cluster does not
# form or the round fails.
#
# The fifteen ports are counted out of 10000 to 19999, never bound to be chosen, as
# `loopback_ports` in crates/cavalry/tests/cli.rs counts its own: a port found by binding
# to port 0 is one of those the kernel gives each driver's own send sockets, so another
# member could be given it before its owner binds it. The shell's process id picks where
# the count starts, through that function's hash, so each runner starts elsewhere.
echo "conformance/jvm: cavalry run, three cluster members, in the background, under $cluster"
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
    echo "conformance/jvm: member 1 did not close its election in 60 s; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

echo "conformance/jvm: run CommittedLogRound beside member 1"
java --enable-native-access=ALL-UNNAMED -cp clients/jvm/build/classes:conformance/jvm/build CommittedLogRound \
    "$cluster/member-1" 1 || {
    echo "conformance/jvm: CommittedLogRound failed beside member 1; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}
