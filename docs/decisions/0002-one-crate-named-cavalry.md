# 0002. Make `cavalry` the one crate users touch, and reserve the crate family

- **Status:** Accepted
- **Date:** 2026-10-10
- **Invariants affected:** 5, 6
- **Supersedes:** none

## Context

crates.io names are first-come, first-served, with no reservation mechanism and no
policy for reclaiming a name from whoever published first. The name `cavalry` was
unclaimed on 2026-10-10, and so were the component names in the `CLAUDE.md`
repository map. A name is held only by publishing something under it.

The first draft of the repository map put the binary in a crate called
`cavalry-cli` and left `cavalry` itself undefined. That gives a new user two names to
learn before the ten-minute path starts, and it leaves `cargo install cavalry` and
`cargo add cavalry` pointing at nothing.

## Decision

`cavalry` is the one crate a user touches. It is both a library and a binary:

- `cargo add cavalry` gives the client API (re-exported from `cavalry-client`) and an
  embeddable driver, so a program can run with no separate daemon. Embedding is how
  the ten-minute path works on a laptop; a shared per-host driver is the production
  shape.
- `cargo install cavalry` gives the `cavalry` binary: `run`, `doctor`, `replay`,
  `console`.

`cavalry-cli` is removed from the map. The implementation crates keep their names and
are published, but documentation points users at `cavalry` only.

Control-plane and CLI dependencies sit behind a default-on `cli` feature. A library
user who wants the lean build uses `cargo add cavalry --no-default-features`; that is
documented in the crate README, and the default stays on so that `cargo install
cavalry` works with no flags.

The whole family is reserved on crates.io now with `0.0.0` placeholder releases
whose description and README point at this repository: `cavalry`, `cavalry-proto`,
`cavalry-shm`, `cavalry-driver`, `cavalry-client`, `cavalry-log`, `cavalry-raft`,
`cavalry-control`, `cavalry-ffi`. Ownership is attached to the `appshapes` GitHub
organization team, not to an individual. crates.io treats `-` and `_` as the same
name, so the underscore spellings are covered.

## Alternatives considered

- **Keep `cavalry-cli` and leave `cavalry` as an empty umbrella.** Lost: two names
  to learn, and the most obvious command, `cargo install cavalry`, installs nothing.
- **Make `cavalry` library-only and ship the binary under another name.** Lost: the
  same two-name problem from the other side.
- **Do not reserve; publish when the first release is ready.** Lost: the name is the
  product's name and is free today; the cost of a placeholder is minutes, the cost
  of losing the name is a rename across the brand, the repository and the docs.
- **Keep the binary's dependencies off the default feature set.** Lost: `cargo
  install cavalry` would need a flag, which breaks the ten-minute path for the
  common case. Library users who care about compile time have a documented flag.

## Evidence

crates.io search for `cavalry` returned no crates on 2026-10-10. The policy text
("first-come, first-served"; no squatting policy under which ownership is handed
over) is quoted in `docs/explanation/why-cavalry.md` sources and in the crates.io
policies page.

## Consequences

Easier: one name in every tutorial, README and error message; `cavalry` is the
product version number. Harder: library users compile control-plane dependencies
unless they opt out; the `cavalry` crate must never grow logic of its own beyond
re-exports and the binary's `main`, or the data-plane rules stop applying cleanly.
We revisit this if the `cli` feature's dependency weight becomes a measured
complaint from library users.

## References

- `CLAUDE.md`, repository map
- crates.io package policies: <https://crates.io/policies>
