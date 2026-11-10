# cavalry-raft

The replicated state machine on top of the durable log. Data plane: the append and
commit path runs per message.

## What this does

- Runs one cluster member's part of `spec/cluster.md`: `Consensus` is the state
  machine of a member, its role, its election, its terms and the positions it knows.
  The leader records its term, announces it, appends the term's first entry and commits
  what a quorum has recorded; a follower canvasses, joins the leader's log at its
  append position and reports that position as it rises.
- Leads by appointment, until the election is built: the member with the lowest id is
  the leader, and every other member follows it (`spec/cluster.md`, "The appointed
  leader"; ADR 0063). A member that could join only with a catch-up or a truncation,
  one that starts late or holds a log, is refused and counted, never joined wrong.
- Does no I/O of its own: the core reaches the consensus stream, the log, its
  recording and its `terms` file through one trait, `Io`. `memory::Cluster` gives it
  that in memory, three members in one process on a clock the caller steps, with
  loss, delay, cuts and paused syncs to set: what the example below and the crate's
  tests run. The module that gives it the same over a driver, as a client of it, is
  this crate's next change.
- To come, each with its card: the module over the client and `cavalry run` (card
  227), the election and mixed versions in a rolling upgrade (card 228), the cluster
  client (card 229), snapshots and catch-up (card 230), and standby members, which
  follow the log without voting, in the open core.

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

Three members in one process, with no driver: the leader is appointed, the others join
its log, and an entry is committed once a quorum has recorded it.

```rust
use cavalry_raft::memory::Cluster;
use cavalry_raft::{ElectionState, Role};

let mut cluster = Cluster::new(&[0, 1, 2])?;
// One second of the cluster's clock: the member with the lowest id leads, the others
// canvass, join its log and report, and every election closes.
cluster.run_ms(1_000);
assert_eq!(cluster.role(0), Some(Role::Leader));
assert_eq!(cluster.role(1), Some(Role::Follower));
assert_eq!(cluster.election_state(2), Some(ElectionState::Closed));

// The leader's own process appends an entry. It is committed when a quorum has
// recorded it, and every member learns the commit position from the leader.
let position = cluster.offer(b"hello")?;
cluster.run_ms(1_000);
assert_eq!(cluster.commit_position(0), Some(position));
assert_eq!(cluster.commit_position(2), Some(position));
// Every member's recording holds the same bytes.
assert_eq!(cluster.log(1), cluster.log(0));
assert_eq!(cluster.log(2), cluster.log(0));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What to read next

- [`spec/cluster.md`](../../spec/cluster.md), the cluster protocol this crate implements:
  the members string and the streams, the log's entries and their positions, the
  consensus messages, the session protocol, the election's states, the timeouts and
  what a reader rejects; its vectors are `spec/vectors/cluster.json`.
- [ADR 0058](../../docs/decisions/0058-specify-the-cluster-protocol-from-aerons-codecs.md),
  why the protocol is Aeron Cluster's field for field under a header of Cavalry's own,
  and [ADR 0052](../../docs/decisions/0052-build-raft-on-the-durable-log-fifth.md), the
  slice's eight features in order, of which this crate's are the third to the seventh.
- [ADR 0063](../../docs/decisions/0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  how this crate's first feature is built: the consensus module is a client of its
  driver, in two layers, a core that does no I/O and the module that acts for it, and
  its leader is appointed, on the path `spec/cluster.md`, "The appointed leader",
  gives through the election's states.
- [`spec/udp.md`](../../spec/udp.md), "The sender", for the multi-destination cast the
  log travels over, and [`spec/durable-log.md`](../../spec/durable-log.md) for the
  recording every member keeps of it.
- [`CLAUDE.md`](../../CLAUDE.md) for the invariants and the gates, and
  [ADR 0003](../../docs/decisions/0003-lay-out-the-workspace.md) for how the workspace
  is laid out.
