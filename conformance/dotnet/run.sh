#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, builds the .NET client with
# the runner's SDK (.NET 10, every warning an error: no package reference, no native shim),
# then conformance/dotnet's Quickstart, Readme and CommittedLog, starts a driver on a
# directory of its own, runs Quickstart through clients/dotnet with the runner's runtime,
# then clients/dotnet/README.md's example through Readme; then starts three cluster members
# on the loopback, each a `cavalry run` in a member's role, and runs CommittedLog beside
# member 1, the client's committed log against a cluster (spec/c-abi.md, version 2; ADR
# 0070); and cleans up. `just conformance` runs this after conformance/jvm
# (clients/README.md; ADR 0046, card 209).
set -euo pipefail
cd "$(dirname "$0")/../.."

export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-dotnet-$$"
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

echo "conformance/dotnet: build the driver and the library"
cargo build --release -p cavalry --bin cavalry
cargo build --release -p cavalry-ffi --lib

# The client as the README builds it, every warning an error (Cavalry.csproj), then the
# runner and the README's test, which reference the client's project.
echo "conformance/dotnet: build the client and the runner with .NET SDK $(dotnet --version)"
dotnet build -c Release clients/dotnet/Cavalry.csproj
dotnet build -c Release conformance/dotnet/Quickstart/Quickstart.csproj
dotnet build -c Release conformance/dotnet/Readme/Readme.csproj
dotnet build -c Release conformance/dotnet/CommittedLog/CommittedLog.csproj

echo "conformance/dotnet: cavalry run, in the background, on $CAVALRY_DIR"
target/release/cavalry run &
driver_pid=$!
for _ in $(seq 1 100); do
    if target/release/cavalry doctor >/dev/null 2>&1; then break; fi
    sleep 0.1
done
# Doctor's whole output first, as scripts/quickstart.sh reads it (conformance/c/run.sh).
running=$(target/release/cavalry doctor || true)
grep -q 'is running' <<<"$running" || {
    echo "conformance/dotnet: the driver did not report running" >&2; exit 1; }

echo "conformance/dotnet: run Quickstart"
dotnet conformance/dotnet/Quickstart/bin/Release/net10.0/Quickstart.dll

# The README's example against the same driver: it fails unless the program prints the
# line the README says the reader will see (conformance/dotnet/Readme).
echo "conformance/dotnet: run clients/dotnet/README.md's example"
dotnet conformance/dotnet/Readme/bin/Release/net10.0/Readme.dll

# The committed log: three members of one cluster, each with five free loopback ports,
# its own driver directory and its own log directory, started together as
# docs/how-to/run-a-cluster.md starts them (conformance/python/run.sh is the shape). A
# member's output goes to a file beside its directories, printed if the cluster does not
# form.
echo "conformance/dotnet: cavalry run, three cluster members, in the background, under $cluster"
mkdir -p "$cluster"
CAVALRY_CLUSTER_MEMBERS=$(dotnet conformance/dotnet/CommittedLog/bin/Release/net10.0/CommittedLog.dll members)
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
    echo "conformance/dotnet: member 1 did not close its election in 60 s; the members said:" >&2
    cat "$cluster"/member-*.out >&2 || true
    exit 1
}

echo "conformance/dotnet: run CommittedLog beside member 1"
dotnet conformance/dotnet/CommittedLog/bin/Release/net10.0/CommittedLog.dll "$cluster/member-1" 1
