# cavalry

The one crate users touch (ADR 0002). Not on the data plane: it holds re-exports and the
binary's `main`.

## What this does

- `cargo add cavalry` gives the client API, re-exported from `cavalry-client`, and an
  embeddable driver, so a program can run with no separate daemon. Embedding is how the
  ten-minute path works on a laptop; one shared driver per host is the production shape.
- `cargo install cavalry` gives the `cavalry` binary: `run`, `doctor`, `replay` and
  `console`.
- Control-plane and command-line dependencies sit behind the `cli` feature, on by
  default so that `cargo install cavalry` needs no flag. `cargo add cavalry --no-
  default-features` is the lean library build; it skips the binary.
- Today the binary prints `cavalry 0.1.0: not built yet` to stderr and exits 1, so that
  no check can pass against it by accident.

In Aeron, this is the client library and the media driver, which are separate artifacts
there ([glossary](../../docs/glossary.md)).

## What it must never do

- Grow logic of its own. `src/lib.rs` holds only `pub use` re-exports, and `just lint`
  fails on anything else (ADR 0002). Code belongs in the crate it is about and is re-
  exported here.
- Need configuration to work. Zero configuration is the primary supported configuration:
  `cavalry` with no arguments works on a laptop and on a cloud VM (invariant 5). A new
  configuration key needs a written reason why the default cannot be right, and a
  `doctor` check that fires when it is set wrong.
- Carry a knob that exists to make a benchmark pass.
- Exit 0 while it does nothing. A stub says that it is not built and exits non-zero.

## Example

Not yet. This crate has no public item to show, and a crate README's example is also
its doctest, so an example written now would invent an API.
[ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) records why the example
rule is deferred for scaffold crates. The example arrives in the same commit as the
first public item.

## What to read next

Not yet: the pages this crate will point at, its section of `spec/` and its reference
page, do not exist. Until they do, read [`CLAUDE.md`](../../CLAUDE.md) for the
invariants and the gates, and
[ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace is
laid out.
