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

# Latency and throughput harness (CLAUDE.md, gate 2; ADR 0015): HdrHistogram p50, p99,
# p99.99 and max of claim and commit, of publish to delivery, and of the slot-padding
# microbench, with zero allocations per message asserted; writes
# bench/results/<profile>-<unix time>.json. Arguments pass through:
# `just bench --messages 5000000`. docs/how-to/run-the-bench.md is the page.
bench *ARGS:
    cargo run --release -p cavalry-bench -- {{ARGS}}

# The same harness with the data-plane checks built out, `cfg(cavalry_unchecked)`, for
# one purpose: what ADR 0010's checks cost at p99.99. The profile gets the suffix
# -unchecked, so it compares against its own baseline. Its own target directory keeps
# the flag from rebuilding the default one. Never run anything else with this cfg.
bench-unchecked *ARGS:
    RUSTFLAGS="--cfg cavalry_unchecked" cargo run --release -p cavalry-bench --target-dir target/unchecked -- --profile-suffix -unchecked {{ARGS}}

# Diff the newest result in bench/results/ against bench/baseline.json for its profile;
# fail on a p99.99 regression over 5 percent on any benchmark. What CI runs after bench.
bench-compare:
    python3 scripts/bench-compare.py

# Wire-spec conformance: every implementation against every vector in spec/vectors/
# (spec/README.md). Today the implementations are cavalry-proto's (the frame header
# and the client protocol) and cavalry-shm's (the counters file, the log buffer, the command ring and the
# broadcast buffer); every vector test also runs in `just test`. A client in clients/
# joins here with its own runner.
conformance:
    cargo test -p cavalry-proto --test conformance
    cargo test -p cavalry-proto --test client_protocol_conformance
    cargo test -p cavalry-shm --test conformance
    cargo test -p cavalry-shm --test log_buffer_conformance
    cargo test -p cavalry-shm --test command_ring_conformance
    cargo test -p cavalry-shm --test broadcast_buffer_conformance

# Deterministic replay test: replay a recorded log twice and diff the state.
replay-check:
    @echo "not built yet: replay-check (CLAUDE.md gate 1)." && exit 1

# The README's ten-minute path, timed, end to end.
quickstart:
    @echo "not built yet: quickstart (CLAUDE.md gate 5). The README path does not exist yet." && exit 1

# clippy with workspace lints, fmt, the workspace lint and the counters lint below.
# lint-config rejoins this list when it is built; until then it would fail it.
lint: fmt-check clippy lint-workspace lint-counters

# The CLAUDE.md rules rustc and clippy cannot see: data-plane flags, the dependency
# allowlist, no async on the data plane, unsafe only in two crates, the lock ban's
# scope, and `cavalry` as re-exports only. The script's header lists each check.
lint-workspace:
    python3 scripts/lint-workspace.py

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# counters.toml is the single source of every counter (ADR 0012). gen-counters checks
# it and writes docs/reference/counters.md and the constants in cavalry-shm from it;
# lint-counters runs the same checks and fails when either generated file is stale.
# The state-transition check CLAUDE.md names arrives with the first state machine.
gen-counters:
    python3 scripts/gen-counters.py

lint-counters:
    python3 scripts/gen-counters.py --check

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

# Miri on the unsafe crate: the layout, cell and conformance tests, which run on plain
# memory. Miri has no file-backed mapping, so the file tests carry
# `#[cfg_attr(miri, ignore)]` and the README's doctest, which creates a file, is left
# out by naming the lib and test targets. Needs `rustup +nightly component add miri`.
miri:
    cargo +nightly miri test -p cavalry-shm --lib --tests

# loom on the publish protocols of every shared layout in cavalry-shm (the counters file,
# the log buffer, the command ring and the broadcast buffer, each in its cells.rs):
# `cfg(loom)` swaps the atomics for loom's, which explores every interleaving of the
# parties. Run it after any change to a protocol in cavalry-shm.
loom:
    RUSTFLAGS="--cfg loom" cargo test -p cavalry-shm --lib --release

# cargo-fuzz over every parser in cavalry-proto, a minute each, on nightly. Not in CI
# (ADR 0008): the no-panic tests in `just test` cover the same property on every
# commit; run this after a parser change and before a release. Needs
# `rustup toolchain install nightly` and `cargo install cargo-fuzz`.
fuzz:
    cargo +nightly fuzz run --fuzz-dir crates/cavalry-proto/fuzz frame_header -- -max_total_time=60

# Retire branches whose work is on master, locally and on origin: every branch whose
# pull request merged, or the one named. GitHub deletes a merged pull request's branch
# on origin by itself; the local branch, its worktree and a working branch that never
# became a pull request linger until this runs. Never rebases, force-pushes or aborts a
# merge; a worktree with changes or a merge in progress is named and left alone.
tidy *BRANCH:
    python3 scripts/tidy.py {{BRANCH}}
