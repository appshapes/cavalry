# Cavalry task runner. This file is the single entry point named in CLAUDE.md.
#
# Every recipe below is a gate from the "Gates" table in CLAUDE.md. A recipe that
# prints "not built yet" is a to-do, not a pass: building it is part of the first
# feature that needs it. Do not make a recipe exit 0 until it does what its line in
# CLAUDE.md says.

set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# Workspace build, release profile for driver and bench.
build:
    cargo build --workspace --release

# Unit and integration tests, excluding bench and conformance.
test *ARGS:
    cargo test --workspace --exclude cavalry-bench --exclude cavalry-conformance {{ARGS}}

# Latency and throughput harness; prints histograms; writes bench/results/.
bench:
    @echo "not built yet: bench harness (CLAUDE.md gate 2). Must emit HdrHistogram p50/p99/p99.99/max to bench/results/." && exit 1

# Diff latest bench results against bench/baseline.json; fail on >5% p99.99 regression.
bench-compare:
    @echo "not built yet: bench-compare (CLAUDE.md gate 2)." && exit 1

# Wire-spec conformance suite against every client in clients/.
conformance:
    @echo "not built yet: conformance suite (CLAUDE.md gate 1)." && exit 1

# Deterministic replay test: replay a recorded log twice and diff the state.
replay-check:
    @echo "not built yet: replay-check (CLAUDE.md gate 1)." && exit 1

# The README's ten-minute path, timed, end to end.
quickstart:
    @echo "not built yet: quickstart (CLAUDE.md gate 5). The README path does not exist yet." && exit 1

# clippy with workspace lints, fmt, and the workspace lint below. lint-counters and
# lint-config rejoin this list when they are built; until then they would fail it.
lint: fmt-check clippy lint-workspace

# The CLAUDE.md rules rustc and clippy cannot see: data-plane flags, the dependency
# allowlist, no async on the data plane, unsafe only in two crates, the lock ban's
# scope, and `cavalry` as re-exports only. The script's header lists each check.
lint-workspace:
    python3 scripts/lint-workspace.py

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Fail on a state transition with no counter in counters.toml.
lint-counters:
    @echo "not built yet: lint-counters (CLAUDE.md gate 4)." && exit 1

# Fail on a config key without why_not_default, a doctor check and a reference line.
lint-config:
    @echo "not built yet: lint-config (CLAUDE.md gate 5)." && exit 1

# rustdoc + mdBook, warnings denied, code blocks run, links checked.
docs:
    @echo "not built yet: docs build (CLAUDE.md gate 6). Needs mdbook, mdbook test, lychee or equivalent, and RUSTDOCFLAGS=-Dwarnings." && exit 1

# Fail if a public item, counter, config key or spec/ changed without docs.
lint-docs:
    @echo "not built yet: lint-docs (CLAUDE.md gate 6)." && exit 1

# Run cavalry doctor against the local driver.
doctor:
    @echo "not built yet: cavalry doctor." && exit 1

# Miri on the unsafe crate.
miri:
    cargo +nightly miri test -p cavalry-shm
