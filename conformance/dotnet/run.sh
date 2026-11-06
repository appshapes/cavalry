#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, builds the .NET client with
# the runner's SDK (.NET 10, every warning an error: no package reference, no native shim),
# then conformance/dotnet's Quickstart and Readme, starts a driver on a directory of its
# own, runs Quickstart through clients/dotnet with the runner's runtime, then
# clients/dotnet/README.md's example through Readme, and cleans up. `just conformance` runs
# this after conformance/jvm (clients/README.md; ADR 0046, card 209).
set -euo pipefail
cd "$(dirname "$0")/../.."

export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-dotnet-$$"
export CAVALRY_LOG_DIR="${CAVALRY_DIR}.log"
rm -rf "$CAVALRY_DIR" "$CAVALRY_LOG_DIR"
cleanup() {
    if [ -n "${driver_pid:-}" ] && kill -0 "$driver_pid" 2>/dev/null; then
        kill -TERM "$driver_pid" 2>/dev/null || true
        wait "$driver_pid" 2>/dev/null || true
    fi
    rm -rf "$CAVALRY_DIR" "$CAVALRY_LOG_DIR"
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
