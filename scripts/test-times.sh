#!/usr/bin/env bash
# Run one `cargo test` command many times in a row, and stop at the first run that
# fails (docs/how-to/run-a-test-many-times.md). A test that passes once says little
# about a test that waits on a clock, a socket or another process: the tests of
# three drivers on the loopback are held to twenty runs in a row on both of CI's
# runners (ADR 0077, "Evidence"), and a test that failed once in CI is found again by
# running it until it fails.
#
#   scripts/test-times.sh 20 -p cavalry-raft --test cluster
#   scripts/test-times.sh 20 -p cavalry-raft --test cluster a_follower_lost -- --nocapture
#
# The first argument is how many runs, 1 to 1000. The rest are `cargo test`'s own
# arguments, with the test program's after `--` as `cargo test` takes them. The script
# builds once, so that a compile error is reported before any run and no run's time
# holds a build, then runs the command that many times, each run's output as it comes.
# After each run it prints one line, and one more at the end:
#
#   test-times: run 3 of 20 passed in 14 s
#   test-times: 20 of 20 runs passed
#
# It exits 0 when every run passed. At the first run that fails it prints which run it
# was and exits with that run's exit code, without the runs that were left. It exits 2
# for arguments it cannot use. `.github/workflows/test-times.yml` runs it on both
# runners when started by hand.
#
# Run it alone on a host. Two test runs at once on one machine slow each other's
# clocks, and a test that waits on one may then fail for the load and not for itself.
set -euo pipefail

usage() {
    echo "usage: scripts/test-times.sh <times, 1 to 1000> <cargo test arguments>" >&2
    echo "  for example: scripts/test-times.sh 20 -p cavalry-raft --test cluster" >&2
    exit 2
}

[ "$#" -ge 2 ] || usage
times="$1"
shift
case "$times" in
    '' | *[!0-9]* | 0*) usage ;;
esac
[ "$times" -le 1000 ] || usage

# `--no-run` belongs before `--`: what follows `--` is the test program's.
build=()
seen_separator=false
for argument in "$@"; do
    if [ "$argument" = "--" ] && [ "$seen_separator" = false ]; then
        seen_separator=true
        build+=(--no-run)
    fi
    build+=("$argument")
done
[ "$seen_separator" = true ] || build+=(--no-run)

cd "$(dirname "$0")/.."
cargo test "${build[@]}"

for run in $(seq 1 "$times"); do
    started=$SECONDS
    code=0
    cargo test "$@" || code=$?
    if [ "$code" -ne 0 ]; then
        echo "test-times: run $run of $times failed with exit code $code after $((SECONDS - started)) s; its output is above, and the $((times - run)) runs after it were not made"
        exit "$code"
    fi
    echo "test-times: run $run of $times passed in $((SECONDS - started)) s"
done
echo "test-times: $times of $times runs passed"
