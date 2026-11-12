# 0106. Canvass with no reports after a replication

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 3, 6
- **Supersedes:** none. Amends ADR 0086 in one clause: what a member that goes
  back to canvass from a replication holds of the other members. Changes
  `spec/cluster.md`, "Roles and the election": the end of the row of state 9, which
  says that; and, in the rows of states 0, 1 and 3, what the page did not say and
  the code did: that init forgets every report, what "has reported" means in
  canvass, and that a member keeps its reports through a lost ballot, a departure
  from Aeron. No message's layout changes, and the page stays at version 0.

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
ADR 0105). This member's own log has not grown since they were read, which is what
went wrong after a replication. A ballot is lost in four ways. In the first the
reports are as good as new, or a leader's word follows within a round trip. In the
other three a report can stay behind the log it was made of:

- A candidate reads a vote against its candidate term, or holds no vote of its own
  for it. It has just heard from the member that outvoted it. If that member won,
  its new leadership term follows the vote within a round trip and is read in
  canvass or in the nomination delay, before another ballot. This is the split
  vote, and the reason the reports are kept.
- A candidate reaches the election timeout short of a quorum. No member it reaches
  has made a leader with it, but it may be cut off from members that have made one.
- A candidate or a voter reads a new leadership term for its candidate term that
  is not for its log term. A leader is in place and may have appended.
- A voter in follower ballot does not read its candidate's new leadership term
  within the election timeout. Its candidate may lead and may have appended.

In those three a report can be behind the log of a leader that is in place. A
member that then nominates itself from its reports asks for a vote in a higher
term, and a leader that reads it holds an election (ADR 0082): the cluster loses
its leader for
one election, and no committed entry, since a candidate whose log is behind is
voted against. What stands between is the same as after any lost ballot: the
member's canvass position goes out within an election status interval, a leader
answers it with a new leadership term, and a member reads that term in canvass and
in its nomination delay. These three are left as ADR 0081 has them, and Revisit
names what would change that.

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
a log that has grown since its reports were read. Revisit the reports kept through
a lost ballot when the fault-injection tests over drivers (ADR 0077), or a cluster
in use, show a leader that held an election on a request vote from a member that
had come back from a lost ballot with no vote against it: a member cut off in its
ballot, or one whose leader's new leadership term it did not read. Forgetting the
reports there, as Aeron does, makes such a member wait for the others' canvass
positions, which a leader answers with its term.

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
