# cavalry-raft

The replicated state machine on top of the durable log. Data plane: the append and
commit path runs per message.

## What this does

- Replicates a durable log across a cluster of drivers with Raft and feeds a
  deterministic state machine from the committed log, so any member can be rebuilt by
  replaying its log and a failover loses nothing that was committed.
- Elects a leader in under a second and runs mixed versions during a rolling upgrade;
  both are tested in CI with fault injection.
- Includes standby members, which follow the log without voting and are ready to be
  promoted, in the open core.
- Lives under the data-plane rules as a whole, leader election and membership included,
  until a recorded decision splits the control parts out.

In Aeron, this is Cluster, with Cluster Standby as a paid component
([glossary](../../docs/glossary.md)).

## What it must never do

- Report an entry as committed before a majority holds it durably, or apply entries in
  any order but log order (invariant 1).
- Lock, await or allocate on the append and commit path (invariant 2). Election and
  membership code follow the same rules while they live in this crate.
- Queue without a bound. A slow follower is handled by an explicit, documented policy,
  not by a growing buffer (invariant 3).
- Change the format of the log it replicates without an ADR; that is the on-disk format
  of `cavalry-log`.

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
