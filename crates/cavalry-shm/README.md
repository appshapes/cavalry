# cavalry-shm

Memory-mapped log buffers, ring buffers and the counters file. Data plane, and one of
the two crates where `unsafe` is permitted.

## What this does

- Maps the log buffers that clients and the driver share: a stream's log is a ring of
  fixed-size terms, and a position is a byte offset into it that increases across terms.
- Provides the lock-free SPSC and MPSC queues every data-plane thread uses to talk to
  another, with 64-byte alignment on shared state so that two threads never share a
  cache line by accident.
- Owns the counters file, the shared-memory file where the driver publishes every
  counter. It is the primary instrument: cheaper than a log line, and it survives a
  crash.
- Keeps every shared layout `#[repr(C)]` with a version in its header. Atomics use
  `Acquire` for loads that gate reads of other memory and `Release` for stores that
  publish it.

In Aeron, this is the log buffer layout and the CnC file, which Agrona provides
([glossary](../../docs/glossary.md)).

## What it must never do

- Carry an `unsafe` block without a `// SAFETY:` comment naming the invariant and where
  it is established. New `unsafe` needs a `loom` test in the same commit, and the crate
  passes `loom` and Miri as a release gate.
- Change a shared layout without bumping its version. Another process may be mapping the
  old one.
- Grow. Every buffer, queue and map has a capacity fixed at startup and a documented
  behaviour when full (invariant 3). Back-pressure is returned to the caller, never
  absorbed.
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
