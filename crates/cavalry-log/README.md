# cavalry-log

The durable log: recording and replay. Data plane.

## What this does

- Records a stream to disk as an ordered, replayable log, written by the driver.
- Replays a log back into a subscription from a given position, at original or maximum
  speed. `cavalry replay` answers "what did this service see between two instants" in
  one command.
- Carries the headline promise: replaying the same log produces the same state, byte for
  byte. `just replay-check` replays a recorded log twice and diffs the state.

In Aeron, this is Archive, with its recordings and replay
([glossary](../../docs/glossary.md)).

## What it must never do

- Produce a different state from the same log (invariant 1). A bug here blocks a
  release.
- Change the on-disk format without an accepted ADR and a version in the file header. A
  recorded log outlives the binary that wrote it.
- Lose or reorder a recorded message, or report a position it has not made durable.
- Grow without a bound. Every buffer on the recording path has a fixed capacity and a
  documented behaviour when full (invariant 3); retention is explicit, never silent
  growth.
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
