# 0003. Lay out the workspace: one version, no publishing yet, lints in three places

- **Status:** Accepted
- **Date:** 2026-10-10
- **Invariants affected:** 2, 5, 6
- **Supersedes:** none

## Context

The first code in the repository is a Cargo workspace with the nine crates from the
`CLAUDE.md` repository map, every one of them empty. crates.io already holds `0.0.0`
placeholders for the whole family (ADR 0002). `CLAUDE.md` names gates that enforce
each invariant, and the first of them, `just lint` and `just test`, have to be real
before any code lands, because a gate that prints "not built yet" is a to-do, not a
pass.

Laying the workspace out forced decisions that the map does not make. How crate
versions relate to each other. Whether anything is publishable. Where each rule lives
when rustc, clippy and Cargo can each see only part of it: `unsafe` in two crates
only, no lock and no `async` on the data plane, a dependency allowlist for data-plane
crates. Which crates are on the data plane, since the map's labels and the rules file
disagreed on `cavalry-raft` and `cavalry-ffi`. Whether the crate README rule, a
twenty-line example that is also a doctest, can be met by crates with no public item.
And, once CI exists, how Dependabot and agent workflows fit a repository where
developers commit straight to `master`.

The toolchain is Rust 1.99 on the laptop that made these decisions and the current
stable on CI. Edition 2024 needs 1.85 or later.

## Decision

The workspace has one version, `0.1.0` in `[workspace.package]`, and every crate
inherits it: the family moves together and `cavalry` is the product version. Every
crate is `publish = false` until the first release; the placeholders stay `0.0.0`.
The rules live where the tool that can see them lives. `[workspace.lints.rust]`
denies `unsafe_code`; seven crates inherit it and `cavalry-shm` and `cavalry-ffi`
carry their own `[lints.rust]` table. The root `clippy.toml` disallows `std::sync` and
`parking_lot` locks and the `block_on` entry points; the two non-data-plane crates
override it with an empty `clippy.toml`. `scripts/lint-workspace.py`, run by
`just lint-workspace` as part of `just lint`, checks what neither can: every crate
declares `data_plane` in `[package.metadata.cavalry]`; a data-plane crate's
dependencies are workspace members or on its `allowed-dependencies` list; a data-plane
crate never depends on a non-data-plane crate; no `async` or `.await` under a
data-plane crate's `src/`; the lint inheritance above; the clippy overrides are exactly
the non-data-plane crates; and `crates/cavalry/src/lib.rs` holds only re-exports. The
docs lints are crate attributes, `#![deny(missing_docs,
rustdoc::broken_intra_doc_links)]`, in every `lib.rs`. Seven crates are data plane:
`proto`, `shm`, `driver`, `client`, `log`, `raft` and `ffi`; `cavalry` and
`cavalry-control` are not, and `.claude/rules/data-plane.md` lists the same seven.
Scaffold crates defer the README example rule: the "Example" and "What to read next"
sections say "not yet" and link here, and the example lands in the same commit as the
crate's first public item. The repository is trunk-based by default; bots and agent
workflows use branches and pull requests, CI runs on those pull requests, Dependabot
opens one grouped pull request a week to move the SHA-pinned actions, and a human
merges it with a merge commit.

## Alternatives considered

- **A version per crate.** Lost: the clients are released together and pass one
  conformance suite, and mixed-version testing is about the wire protocol, not crate
  semver. One number is one less thing a stranger has to reconcile.
- **Publish `0.1.0` now.** Lost: nothing works. A crate on crates.io whose binary
  prints "not built yet" lies to `cargo install`, and the placeholders already hold
  the names.
- **`#![forbid(unsafe_code)]` in each `lib.rs` instead of the Cargo table.** Lost:
  enforcement spread over nine files that anyone can edit, checked only by reading.
  The Cargo table is one place, and the script checks that every crate inherits it.
- **`missing_docs` and the rustdoc lint in the Cargo table too.** Lost: the two
  unsafe crates cannot inherit the workspace table, so the table would not reach all
  nine without duplication, and a reader opening `lib.rs` should see the lint there.
- **A `clippy.toml` in each data-plane crate.** Lost: seven copies that drift. The
  root file plus two overrides, with a check that the override set equals the
  non-data-plane set, is less to maintain and cannot silently disagree with the flag.
- **A Rust `xtask` crate instead of a Python script.** Lost for now: it compiles on
  every `just lint`, it is a tenth crate with its own README and lints, and Python 3.9
  with the standard library is on every laptop and both CI runners. Revisited when a
  check needs Rust's own parser.
- **`cavalry` on the data plane.** Lost: by ADR 0002 it is re-exports plus the
  binary's `main`, and its default-on `cli` feature pulls control-plane dependencies
  in, which the allowlist would have to bless and the plane check would have to
  exempt. The re-export check protects what the data-plane rules protected there.
- **`cavalry-raft` or `cavalry-ffi` off the data plane.** Lost: the append and commit
  path and the C ABI are both per message by the `CLAUDE.md` definition. The strict
  default holds, election and membership included, until a decision splits them out.
- **A placeholder API so the README example rule could be met now.** Lost: an API is a
  spec or ABI decision, and an example of an invented API teaches the wrong thing.
- **Actions pinned by version tag.** Lost: tags are mutable. The tj-actions compromise
  of March 2025 (CVE-2025-30066) was delivered by moving tags. Every action is pinned
  to a full commit SHA with the version in a trailing comment.
- **Move SHA pins by hand, no Dependabot.** Lost: pinned SHAs that nobody moves are
  worse than tags. Dependabot can only work through pull requests, which is why
  bot pull requests are the exception to committing straight to `master`.

## Evidence

- Verified in a throwaway workspace on 2026-10-10 with cargo 1.99.0: the workspace
  `unsafe_code` lint denies in inheriting crates and permits in a crate with its own
  table; clippy reads the nearest `clippy.toml` per crate and does not merge; `cargo
  metadata` exposes `package.metadata`; `--exclude` of a crate not yet in the
  workspace is a warning, not an error.
- Commit 9bbe35a planted one violation of each kind and reverted it:
  `scripts/lint-workspace.py` reported six findings and exit 1; clippy with
  `-D warnings` rejected a `Mutex` in `cavalry-log` and an `unsafe` block in
  `cavalry-driver` and accepted a `Mutex` in `cavalry-control`.
- CI run 38048302932 on commit 60776c8 passed `just lint` and `just test` on
  `ubuntu-latest` and `macos-latest`; the run on commit 2b67873 did the same.
- `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` is clean with every
  crate README as its rustdoc front page (commit 2b67873).

## Consequences

Easier: one version to bump; one place to look for each rule, named in this ADR; the
`data_plane` flag drives every data-plane check, so adding a crate means setting one
field; a stranger can clone, build and run the gates on a laptop or a runner.

Harder: a new dependency in a data-plane crate needs an allowlist entry, a commit
justification and an ADR (ADR 0001), by design. The lint script is Python in a Rust
repository. `cavalry-raft` carries the strict rules for its election and membership
code until it is split. A Dependabot pull request needs a human merge every week.
`just lint` runs without `lint-counters` and `lint-config` until those are built, and
they rejoin it then.

Revisit: at the first release, when `publish` flips (new ADR); when `cavalry-raft`
needs a lock for membership (split the crate, new ADR); when a workspace check needs
Rust's parser (`xtask`); when `cargo` dependencies exist (add the `cargo` ecosystem to
`.github/dependabot.yml`).

## References

- `CLAUDE.md`: repository map, "Data plane", "Unsafe", "Working rules for agents"
- ADR 0001, ADR 0002
- `Cargo.toml`, `clippy.toml`, `scripts/lint-workspace.py`, `.github/workflows/ci.yml`,
  `.github/dependabot.yml`, `.claude/rules/data-plane.md`
- Commits 583373b (workspace), 9bbe35a (lints), 60776c8 (CI), 2b67873 (READMEs)
- AppShapes Trello card 75, "Setup repo and SDLC"
