# cavalry-driver

The daemon: conductor, sender, receiver and idle strategies. Data plane.

## What this does

- Runs as one driver per host. It owns the sockets and the shared-memory log buffers;
  clients attach to it through `cavalry-shm`, and it never sits in the message path as a
  broker.
- Runs dedicated threads for the sender and the receiver, pinned when the operating
  system allows, each with an idle strategy: what the thread does when it has no work,
  busy-spin, yield or backoff. Backoff is the default; `doctor` recommends busy-spin on
  isolated cores.
- Drains and fills sockets in batches. A receiver that detects a gap sends a NAK, and
  the sender retransmits from its own term buffer, so reliability costs no extra
  storage. Status messages drive flow control with the max, min and tagged policies.
- Degrades under cgroup CPU throttling, shared-memory pressure or packet loss. It does
  not fall over, and every state transition increments a counter.

In Aeron, this is the media driver ([glossary](../../docs/glossary.md)).

## What it must never do

- Allocate, lock, make a syscall, await or panic per message (invariant 2). A message
  never triggers its own `sendmsg`; clock reads go through a cached clock.
- Absorb back-pressure by growing. Every queue has a fixed bound and a documented
  behaviour when full, and the back-pressure result goes back to the publisher
  (invariant 3).
- Log on the data plane. Nothing here calls into `tracing` or `log`; it bumps a counter,
  and `doctor` explains the counter (invariant 4).
- Deliver a message twice or out of order, or lose one without reporting the gap to the
  subscriber (invariant 1).

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
