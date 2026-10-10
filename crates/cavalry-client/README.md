# cavalry-client

The client API implementation, re-exported by the `cavalry` crate. Data plane.

## What this does

- Attaches a process to the driver through shared memory and creates publications (a
  handle for writing one stream) and subscriptions (a handle for reading one stream,
  possibly from several publishers) on channels such as `cavalry:udp?endpoint=host:port`
  and `cavalry:ipc`. IPC and UDP are the same API.
- Publishes by claim and commit: claim a slice of the log buffer, write into it in
  place, commit it. Nothing is allocated per message, and a codec encodes straight into
  the claimed slice.
- Delivers to the subscriber's fragment handler as a view into mapped memory, one image
  per connected publisher.
- Returns back-pressure as a result code when the receiver window or the log buffer is
  full. The caller decides what to do; the driver never buffers more.

In Aeron, this is the client, with `tryClaim` and `commit`
([glossary](../../docs/glossary.md)).

## What it must never do

- Lie about delivery (invariant 1). A stream is ordered; a message is delivered once, or
  its absence is reported to the subscriber.
- Allocate, lock, make a syscall, await or panic per message (invariant 2). The path
  from claim to the fragment handler is judged at p99.99 and max, never at the mean.
- Absorb back-pressure. It is explicit and returned to the caller, never hidden behind a
  growing buffer (invariant 3).
- Rename a concept. Publication, subscription, image, session, position and back-
  pressure keep the names in the glossary.

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
