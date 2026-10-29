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

# Unit and integration tests, excluding bench and conformance, after the scripts' own
# tests; then the harness's own unit tests, which run no benchmark: the clock-step reading
# over the bench hosts' advance histograms (ADR 0029).
test *ARGS: test-scripts
    cargo test --workspace --exclude cavalry-bench --exclude cavalry-conformance {{ARGS}}
    cargo test -p cavalry-bench {{ARGS}}

# The tests of the Python scripts that have them (ADR 0026): the release-notes email's
# layout and its deterministic steps, against a made-up checkout and a fake GitHub API on
# loopback, and the latency gate's rule on made-up results (ADR 0029). Standard library
# only, no network.
test-scripts:
    python3 scripts/release-notes-email-test.py
    python3 scripts/release-notes-test.py
    python3 scripts/bench-compare-test.py

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
    cargo test -p cavalry-log --test conformance

# Deterministic replay test (CLAUDE.md, gate 1; ADR 0030): a driver on a directory of
# its own records 100,000 hash-chained messages while a live subscriber prints them,
# `cavalry replay` plays the recording twice from the start and once from an instant,
# and the script fails unless the live lines and both replays hash the same and the
# replay from the instant starts where the time index says. What CI runs on Linux and
# macOS. docs/how-to/replay-a-recording.md is the page.
replay-check:
    bash scripts/replay-check.sh

# The README's ten-minute path, timed, end to end (CLAUDE.md, gate 5): build the binary
# and the example, start a driver on a directory of its own, run the example against it
# through CAVALRY_DIR, read doctor, stop the driver, and fail on any step that does not
# do what the README says it does. What CI runs on Linux and macOS.
quickstart:
    bash scripts/quickstart.sh

# clippy with workspace lints, fmt, the workspace lint, the counters lint and the
# configuration lint below.
lint: fmt-check clippy lint-workspace lint-counters lint-config

# The CLAUDE.md rules rustc and clippy cannot see: data-plane flags, the dependency
# allowlist, no async on the data plane, unsafe only in two crates, the lock ban's
# scope, and `cavalry` as re-exports only. The script's header lists each check.
lint-workspace:
    python3 scripts/lint-workspace.py

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# counters.toml is the single source of every counter and of every data-plane state
# machine (ADR 0012). gen-counters checks it and writes docs/reference/counters.md and the
# constants in cavalry-shm from it; lint-counters runs the same checks and fails when
# either generated file is stale, when a [[transition]] names no counter, when the enum
# marked `// state machine: <name>` does not have the machine's states as variants, or
# when an enum named *State, *Status or *Phase has neither that mark nor
# `// not a state machine: <why>`.
gen-counters:
    python3 scripts/gen-counters.py

lint-counters:
    python3 scripts/gen-counters.py --check

# config.toml is the single source of every configuration key (ADR 0020). gen-config
# checks it and writes docs/reference/config.md and the key table in cavalry-control from
# it; lint-config runs the same checks, fails when either generated file is stale, and
# fails a key whose doctor check is not in doctor.rs with a test, whose flag the binary
# does not parse, or whose constant the config module does not read.
gen-config:
    python3 scripts/gen-config.py

lint-config:
    python3 scripts/gen-config.py --check

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

# Run cavalry doctor against the local driver: the one on the default directory
# (docs/how-to/run-the-driver.md). Exits 1 when a check failed.
doctor:
    cargo run --quiet -p cavalry -- doctor

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
    cargo +nightly fuzz run --fuzz-dir crates/cavalry-proto/fuzz client_protocol -- -max_total_time=60

# Retire branches whose work is on master, locally and on origin: every branch whose
# pull request merged, or the one named. GitHub deletes a merged pull request's branch
# on origin by itself; the local branch, its worktree and a working branch that never
# became a pull request linger until this runs. Never rebases, force-pushes or aborts a
# merge; a worktree with changes or a merge in progress is named and left alone.
tidy *BRANCH:
    python3 scripts/tidy.py {{BRANCH}}
