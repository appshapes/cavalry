#!/usr/bin/env bash
# Builds the driver and cavalry-ffi in release, compiles conformance/c/quickstart.c
# against the committed header with the runner's C compiler, links the static library,
# starts a driver on a directory of its own and runs the program; then starts three
# cluster members on the loopback, each a `cavalry run` in a member's role, compiles
# conformance/c/cluster.c the same way and runs it: a service beside each member and a
# session with the cluster on the first driver, the cluster round (spec/c-abi.md,
# versions 4 and 5; ADR 0122; ADR 0123); and cleans up. `just conformance` runs this
# after the Rust vector suites (spec/c-abi.md; ADR 0051).
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-c-$$"
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

# The cluster round: three members of one cluster, each with five loopback ports, its own
# driver directory and its own log directory, as conformance/python/run.sh and
# docs/how-to/run-a-cluster.md start them. A member's output goes to a file beside its
# directories, printed if the cluster does not form or the round fails.
#
# The sixteen ports, fifteen for the members and one for the session's response channel,
# are counted out of 10000 to 19999, never bound to be chosen, as `loopback_ports` in
# crates/cavalry/tests/cli.rs counts its own: a port found by binding to port 0 is one of
# those the kernel gives each driver's own send sockets, so another member could be given
# it before its owner binds it. The shell's process id picks where the count starts,
# through that function's hash, so each runner starts elsewhere.
echo "conformance/c: compile cluster.c with ${CC:-cc} against crates/cavalry-ffi/include/cavalry.h"
# shellcheck disable=SC2086
"${CC:-cc}" -std=c11 -Wall -Wextra -Werror -I crates/cavalry-ffi/include \
    conformance/c/cluster.c target/release/libcavalry_ffi.a $libs \
    -o target/conformance/cluster-c

echo "conformance/c: cavalry run, three cluster members, in the background, under $cluster"
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
    echo "conformance/c: member 1 did not close its election in 60 s; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

echo "conformance/c: run the cluster round, a service beside each member and a session on $CAVALRY_DIR"
target/conformance/cluster-c "$cluster" "$CAVALRY_CLUSTER_MEMBERS" "$response" || {
    echo "conformance/c: the cluster round failed; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}
