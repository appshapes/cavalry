# 0106. Canvass with no reports after a replication

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 3, 6
- **Supersedes:** none. Changes one clause of `spec/cluster.md`, the end of the row
  of state 9 in "Roles and the election": what a member that goes back to canvass
  from a replication knows of the other members. No message's layout changes, and
  the page stays at version 0.

## Context

A member in canvass nominates itself at once when every member has reported and
no member's log is ahead of its own (`spec/cluster.md`, state 1). The reports are
the other members' canvass positions, read since the member's init. A member whose
log is behind its leader's term replicates the term it lacks, state 9, and goes
back to canvass (ADR 0086).

**The reports it held are then stale.** They were read against the log it had
before it replicated. A member that held no log read reports that were all ahead
of it. With the term replicated, the same reports are of logs no longer ahead, so
canvass makes it a unanimous candidate on its next cycle. It nominates itself
after its delay, unless its leader's answer to its canvass position reaches it
first, stores a term above its leader's, and asks for votes. The leader reads a
request vote for a term above its own and holds an election.

**Seen in memory.** Three members elect a leader and commit ten entries. The
leader starts again on an empty log directory. The two that kept their logs elect
one of them, in term 1; the third replicates what it lost. Over twenty seeds, the
member that replicated was a candidate in eight, the leader the two had elected
gave up the lead in each of those, and the three settled on term 3. No committed
entry was lost in any. The cluster had a leader, and lost it to a member that had
caught up with it.

**The page did not say what the member knows.** The row of state 9 said "then it
goes back to canvass", and the row of state 1 says "every member has reported".

## Decision

A member that goes back to canvass from a replication holds no member's report,
as a member that comes from init holds none. Its canvass position is answered by
its leader's new leadership term within an election status interval, which
carries it on to the next term or to the live log. With no leader to answer, it
canvasses as any member does: the other members' canvass positions are its
reports, read against the log it has now.

The reports a member holds when its ballot is lost stay as they are (ADR 0081,
ADR 0105): no member's log has grown since they were read, since no leader has
written.

## Alternatives considered

- **Forget every report on every entry to canvass, as Aeron does.** Lost for the
  entry from a lost ballot. The two candidates of a split vote canvass again at
  once since ADR 0105, and with their reports forgotten each would wait an
  election status interval for the other's before it could be a candidate again.
  Their logs have not changed.
- **Leave it, since the nomination delay lets the leader's answer arrive first in
  most cases.** Lost. Eight seeds in twenty is not rare, and each costs the
  cluster its leader for an election.
- **Have a leader ignore a request vote from a member whose log is not ahead.**
  Lost. A leader that holds an election on a higher term is what lets a cluster
  that elected without it replace it (ADR 0082).

## Evidence

- `crates/cavalry-raft/tests/elected_replication.rs`,
  `a_member_that_replicated_canvasses_with_no_reports_and_unseats_no_leader`, the
  case above at seeds 1 to 20, with the check that no committed entry is lost
  after every step. At every seed the leader the two elected still leads five
  seconds after the third holds the log, every member follows term 1, the leader
  counts no `cluster.leader_step_downs`, the member that replicated counts no
  `cluster.candidacies`, and its log is the leader's byte for byte.
- With the rule taken out, a probe of the same twenty seeds: the member that
  replicated counts one candidacy and the leader one step down at eight seeds, and
  the three end on term 3 there.
- Aeron's `Election.state`, read on 2026-11-12: entering `CANVASS` calls
  `resetMembers()`, which forgets every member's log position and vote.

## Consequences

Easier. A member that rejoins by replication does not cost a running cluster its
leader.

Harder. Nothing an operator sees: the member waits for its leader's answer, at
most an election status interval, where it could nominate itself before.

Revisit when a path other than a replication brings a member back to canvass with
a log that has grown since its reports were read.

## References

- [ADR 0086](0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md),
  the replication and its return to canvass;
  [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md)
  and [ADR 0105](0105-lose-a-ballot-at-its-first-vote-against.md), the reports a
  lost ballot keeps;
  [ADR 0082](0082-give-up-the-lead-on-a-higher-term-or-a-lost-quorum.md), a leader's
  election on a higher term.
- `spec/cluster.md`, "Roles and the election", states 1 and 9.
- Aeron's `Election`:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
- Card 228.
