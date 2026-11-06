#!/usr/bin/env bash
# Builds the driver and cavalry-ffi's shared library in release, compiles the JVM client
# with the runner's javac (Java 25 or later, every lint a warning and every warning an
# error: no build tool, no dependency), then conformance/jvm's Quickstart and Readme,
# starts a driver on a directory of its own, runs Quickstart through clients/jvm with the
# runner's java, then clients/jvm/README.md's example through Readme, and cleans up.
# `just conformance` runs this after conformance/go (clients/README.md; ADR 0046, card
# 208).
set -euo pipefail
cd "$(dirname "$0")/../.."

if [ -d /dev/shm ]; then base=/dev/shm; else base="${TMPDIR:-/tmp}"; fi
export CAVALRY_DIR="${base%/}/cavalry-conformance-jvm-$$"
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
    conformance/jvm/Quickstart.java conformance/jvm/Readme.java

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
