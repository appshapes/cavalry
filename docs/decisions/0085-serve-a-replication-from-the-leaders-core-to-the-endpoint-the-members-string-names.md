# 0085. Serve a replication from the leader's core, one replay a member, to the endpoint the members string names

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Adds one clause to `spec/cluster.md`, "Catch-up position,
  template 56", and one to "Messages outside an election": the leader's replay goes
  to the `catchup` endpoint its members string gives the follower, and the leader
  does not read the endpoint the message carries. The page's version stays 0: no
  message changes, and no member over a driver takes a vote yet (ADR 0081).

## Context

ADR 0081 put the election on a path of its own in the core and listed its steps.
The fourth is state 9, follower log replication: a member that missed a whole term
replicates it from the leader's recordings before it joins. It has two halves. The
member asks, with a catch-up position, and records what arrives. The leader answers
with replays of its own recordings. This decision is the leader's half, which is
built first: nothing depends on a member asking, and a member that asks needs a
leader that answers.

`spec/cluster.md`, "Messages outside an election", already says what the leader
does. A catch-up position below its term base log position is a replication. Its
replays end at the first term base log position above the position asked. One
replay runs to a follower at a time. Where the span lies in several recordings, the
next recording's replay begins where the last ended, once that one ended with
reason 0 and the follower's reported append position has reached the position. A
catch-up position that arrives while a replay runs stops it, and is started when
the `replay stopped` event arrives. A canvass position stops the replay and drops
the next. Row 0 of the election's states has init stop any replay.

Building it found two things the page did not settle.

**The page does not say which layer keeps this.** A member is two layers (ADR
0063): the core, `Consensus`, which does no I/O, and the I/O under it, the module
over a driver or the in-memory cluster. The rule above needs two facts that only
the core has, the follower's last reported append position and whether the
follower canvassed, and one that only the I/O has, the driver's `replay stopped`
event with its position and its reason.

**The follower's `catchup` endpoint is in two places.** The members string gives
every member's (`spec/cluster.md`, "Members and streams"). The catch-up position
carries the sender's too. Aeron's leader reads the message's:
`ConsensusModuleAgent.onCatchupPosition` puts the message's endpoint into the
replay channel. The core holds no endpoint. It names a member by its id, and
`Io::send` takes that id. A request that is kept until an older replay has stopped
would have to keep the endpoint's bytes with it, up to eight of them in the core,
or in a table of the I/O's that the core fills.

## Decision

The core keeps what a leader serves each member, and reaches the driver's replays
through four operations of `Io`. The replay goes to the `catchup` endpoint the
leader's members string gives the member.

**One `Serving` a member, in the core.** A leader that elects holds, for each other
member, one of four things: nothing; a replay that runs, with the replication's
end; a replay it asked to stop, with the newest request the member has sent since;
or the next recording's replay that it owes, with the position the last ended at.
The table is an array of `MAX_MEMBERS`, eight. A catch-up position, a canvass
position and the leader's duty cycle move a member from one to another as the page
says. A start that faults keeps the request as a replay owed, and the duty cycle
starts it once the member has reported the position: how a stopped replay ended is
handed over once, so a request must not depend on reading it twice. Only a leader
made with `Consensus::with_election` serves: no member beside an appointed leader
replicates.

**Four operations of `Io`.** `term_base_above(position)` is the lowest term base in
the member's record of terms above a position, where a replication's replays end.
`start_replay(member, position, end)` starts a replay of the recording that holds
the position, to that recording's end or to `end`, whichever is lower, and says
`false` when no recording holds the position. `stop_replay(member)` asks the
running replay to stop. `replay_stopped(member)` says how the last replay ended,
once: its position, and whether it ended by itself, the event's reason 0. The core
asks about a member only while it holds a replay for it.

**The endpoint is the members string's.** `start_replay` takes the member's id,
and the I/O replays to the `catchup` endpoint its members string gives that member.
The leader does not read the endpoint a catch-up position carries. The live log
already goes where the leader's string says: every other member's `log` endpoint is
a destination of the leader's log publication. A host whose members string differs
from the leader's is reached by neither.

**Counted once a replay.** `cluster.log_replications`, id 135, rises by one for
each replay a leader starts for a replication, as the page's row says. A position
the leader's recordings do not hold starts nothing and counts nothing: the member's
own timer counts it, in the member's half.

**Init stops every replay.** A leader that holds an election stops each replay it
runs, in init, where it gives its log up (ADR 0082). It asks no more about one it
had already asked to stop, and drops what it owed. A stop that fails is a fault:
the member stays in init and the next cycle stops the replay again. Init does not
wait for a stopped replay's `replay stopped`. A member that leads again and is
asked by the same follower before that event has come calls `start_replay` for it,
and the I/O starts the new replay once the old one has stopped.

**A replay's fault costs the leader nothing else.** The leader looks after its
replays after its state's own work in a duty cycle. A `start_replay` or a
`replay_stopped` that faults is returned from the cycle once the heartbeat, the
commit position and the quorum's watch have run.

**What does not serve yet.** The in-memory cluster's `start_replay` says `false`,
as a leader whose recordings were purged does. Its replays come with the member's
half, where a test can show them. The consensus module's three replay operations
return `unsupported`, as its `remove_log` and `truncate_log` do: its leader is
appointed and its core never calls them.

## Alternatives considered

- **The I/O keeps the chain: the core calls one `replicate_to(member, position,
  end)`.** Lost. The rule needs the follower's reported append position and its
  canvass position, which the I/O would have to be told. The module and the
  in-memory cluster would each implement the page's rule, and no test on one core
  could pin it.
- **Read the endpoint from the message, as Aeron does.** Lost. The core would hold
  the bytes of up to eight endpoints for requests it starts later, or the I/O a
  table the core fills. It would also give a follower two addresses, one for the
  live log, from the leader's string, and one for a replay, from its own. A
  follower whose own string names another `catchup` endpoint is misconfigured, and
  with one source its replication fails where its join would.
- **Leave a replay running when the leader holds an election.** Lost. It would end
  by itself, at its end. But row 0 of the page says init stops any replay, and
  Aeron's election stops every catch-up replay when it begins again (`Election.init`
  calls `ConsensusModuleAgent.stopAllCatchups`). A member that replicates under a
  leader that no longer leads waits for a commit position that never comes.
- **Count a request, not a replay.** Lost. The page's row says once a replay, and a
  replication that needs three recordings is three starts an operator can see.
- **Serve from the appointed leader too.** Lost. No member beside it asks, and the
  path is taken out of the core when the module elects (ADR 0081).

## Evidence

- `crates/cavalry-raft/tests/replays.rs`, thirteen tests on one core through an `Io`
  that remembers what it was asked. The leader is member 0 of three, leading term 2
  from 96 over a record of terms (0, 0), (1, 64), (2, 96).
  - A position inside a term is replayed to that term's end, and one at a term's
    base to the end of the term that begins there; two members are served at once,
    and each replay is counted.
  - Where a replay ends is `cavalry-proto`'s `replay_end` for the same record, at
    seven positions.
  - A position at or above the leader's base, a position below 0, a message of
    another term and one from an id that is no other member's start nothing. An
    appointed leader starts nothing.
  - A position the recordings do not hold starts nothing, counts nothing, and
    leaves the leader free to serve the member's next request.
  - A replay that ended by itself at 32, below the term's end at 64, is followed
    by the next from 32 only once the member has reported 32: not before any
    report, and not at 16.
  - A replay that stopped for another reason is followed by nothing.
  - A member that asks twice while a replay runs has that replay stopped once, and
    its newest request started when the replay has stopped.
  - A canvass position stops the replay before the leader answers it, and drops
    the replay that was owed.
  - A stop that fails is returned from the call and asked again.
  - A start that fails is returned, and the request is started on a later cycle
    once the member has reported its position, after a stop too.
  - A start that fails in every cycle is returned from every cycle, after the
    leader's heartbeat, its new leadership term and its commit position, has gone
    to both members; and a leader whose members fall silent meanwhile still holds
    its election.
  - A leader whose active members fall below a quorum stops its replay in init,
    again after a stop that failed, and serves nobody as a follower.
- `crates/cavalry-raft/tests/next_term.rs` passes on the leader the new tests use,
  which moved to `tests/common/mod.rs`.
- Every other test of the crate passes unchanged. No member in the in-memory
  cluster sends a catch-up position yet, so no cluster test can move.

## Consequences

- `Io` has four more operations, sixteen. The module's I/O for the election
  implements the three of a replay over `start replay`, `stop replay` and the
  `replay stopped` event, and keeps the event until the core has asked for it once.
  A fault that lasts, a `start replay` the driver refuses for a full registry or a
  `catchup` host that does not resolve, is returned from every duty cycle of the
  leader while the replay is owed. The module's `do_work` returns a fault before it
  stores its counters, which is the module's to change when it serves replays.
- `Counts` has twenty counts, and a member's module adds twenty client counters;
  nine of them stay 0 while the module's leader is appointed.
- A replication whose follower's members string names another `catchup` endpoint
  than the leader's never arrives. The leader's replay ends with reason 2 after the
  replay connect timeout, 5 s, and the follower's timer fails its election. The
  member's half gives that timer its counter, and `cavalry doctor` the check.
- The leader serves a replication from the term its record holds at the position.
  It does not check that the member's own record names the same term there. The
  member's half decides what a member records when it begins a term and when it
  ends one.
- Revisit: if a member's endpoints ever differ from the members string by design,
  a host behind an address translation; and when the catch-up within a leader's
  term is built (states 11 to 13), whose replay follows the live recording and has
  no end.

## References

- `spec/cluster.md`, "Catch-up position, template 56", "Messages outside an
  election", row 0 and row 9 of the election's states, and "Counters".
- `spec/client-protocol.md`, "Commands", start replay and stop replay, and
  "Events", replay stopped and its reasons.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path and its steps.
- [ADR 0082](0082-give-up-the-lead-on-a-higher-term-or-a-lost-quorum.md), what a
  leader that holds an election gives up in init.
- [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the two layers of a member.
- Aeron's `ConsensusModuleAgent.onCatchupPosition` and `stopAllCatchups`, and
  `Election.init`, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/ConsensusModuleAgent.java>
  and
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
