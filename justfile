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

# rustdoc with warnings denied; the mdBook under target/book with any warning denied;
# the book's Rust code blocks; and every relative link and anchor in docs/ and the
# READMEs (scripts/lint-docs.py links). External links are `just docs-links`, not a
# gate: a site going down is not a bug in this repository (ADR 0006). The mdbook
# version named below is the one CI pins in .github/workflows/ci.yml; bump both.
docs:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v mdbook >/dev/null || { echo "docs: mdbook is not installed. brew install mdbook, or cargo install --locked mdbook@0.5.4" >&2; exit 1; }
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    out=$(mdbook build 2>&1) || { printf '%s\n' "$out" >&2; exit 1; }
    printf '%s\n' "$out"
    if grep -qE '(^ *|\[)(WARN|ERROR)(\]| )' <<<"$out"; then echo "docs: mdbook warned; warnings are denied" >&2; exit 1; fi
    mdbook test
    python3 scripts/lint-docs.py links

# Fail if a public item, counter, config key or spec/ changed without docs, a page is
# missing from docs/SUMMARY.md, an ADR is malformed, or an accepted ADR was edited. The
# range it checks is described in the script's header. What CI runs.
lint-docs:
    python3 scripts/lint-docs.py

# External links in docs/ and the READMEs, checked online. Not a gate: run it before a
# release, and when a source in docs/explanation/why-cavalry.md looks stale.
docs-links:
    command -v lychee >/dev/null || { echo "docs-links: lychee is not installed. brew install lychee, or cargo install --locked lychee" >&2; exit 1; }
    lychee --no-progress docs README.md crates/*/README.md

# Run cavalry doctor against the local driver.
doctor:
    @echo "not built yet: cavalry doctor." && exit 1

# Miri on the unsafe crate.
miri:
    cargo +nightly miri test -p cavalry-shm
