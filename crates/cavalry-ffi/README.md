# cavalry-ffi

The `extern "C"` ABI and its cbindgen header. Data plane, and the other crate where
`unsafe` is permitted.

## What this does

- Exposes the client API as a C ABI with a generated header, so one Rust core serves
  every language. The thin clients under `clients/` for Python, Node, .NET, JVM and Go
  are generated over it and released together.
- Carries every non-Rust client's messages: claim, commit and the fragment handler all
  cross this boundary, which is why it is on the data plane.
- Is the one surface frozen between releases. Rust-to-Rust integration goes through
  crates with semver; everything else goes through this ABI.

In Aeron, this is the C client, which re-implements the Java one; here the Rust core is
the only implementation and the C ABI is a thin layer over it
([glossary](../../docs/glossary.md)).

## What it must never do

- Change the ABI without an accepted ADR. It is one of the three things that cannot be
  undone, with the wire format and the on-disk log format.
- Carry an `unsafe` block without a `// SAFETY:` comment naming the invariant and where
  it is established.
- Let a released buffer be reused silently. Buffer lifetimes across the ABI are by
  convention, not the borrow checker, so debug builds poison released buffers and misuse
  is loud.
- Behave differently from the Rust client. Every client passes the conformance suite in
  CI.
- Allocate, lock, make a syscall, await or panic per message (invariant 2).

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
