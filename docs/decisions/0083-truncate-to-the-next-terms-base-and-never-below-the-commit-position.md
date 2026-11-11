# 0083. Truncate to the next term's base, never below the commit position, and count both

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Adds two rows to `spec/cluster.md`, "Counters",
  `cluster.log_truncations` and `cluster.truncations_refused`; two sentences to
  "Messages during an election", the count of a truncation and what a reader does
  when the position is below the commit position it knows; and one clause to "The
  commit position", that a canvass position keeps a member active as an append
  position does. The page's version stays 0: no message changes, and no member over
  a driver takes a vote yet (ADR 0081).

## Context

ADR 0081 put the election on a path of its own in the core and listed its steps.
This is the third: a member's truncation. `spec/cluster.md`, "Messages during an
election", has a reader of new leadership term whose append position is above the
next term base log position truncate its log to that position first, with every
term it recorded above it, and then read the message with what it holds. The
leader's half, naming the next term from its record of terms, is pull request 218.
Until this change a member that elects failed its election wherever the message
asked for a truncation. A leader that had been cut off, and had appended an entry
that no quorum recorded, came back as a member that could never join again.

Building it found five things that the page and the earlier decisions did not
settle.

**The page names no counter for a truncation.** A member that cuts entries it held
is the first thing an operator asks about after a leader was lost: a session message
in a truncated entry is lost to its client ("The term's first entry"). Invariant 4
asks that the counters explain it.

**The page says a committed entry is never truncated, and not what a member does
when it is told to.** It is a property of the protocol, argued in "The term's first
entry". A member that holds a log from another cluster, or a restored copy of an
older one, can be told to cut below a position it knows to be committed. Doing it
would lose a committed entry on that member, against invariant 1.

**The core cannot cut a log.** `Io` has no operation for it. The core's append
position only rises: every duty cycle takes the greater of what it has and what
`Io` reports, so that a report that dips between two recordings moves nothing. And
the core keeps its log term, the highest term in its record, in memory, which a
truncation can lower.

**The page and the core differ on what keeps a member active.** "The commit
position" says a member is active while its last append position is younger than
the leader heartbeat timeout. The core has always taken a member's last report of
either kind, a canvass position among them, as "Messages during an election" has
canvass position update the sender's position in the reader's table. The review of
pull request 221 asked which is meant, now that the quorum's watch starts an
election.

**The appointed path's `lead()` cleared a log it could not hold.** The same review
noted a dead line there.

## Decision

A member that elects truncates as the page says, never below the commit position it
knows, with a counter for each outcome, through one new method of `Io`.

**The truncation.** A member that reads a new leadership term for its own log term,
whose next term base log position is not −1 and is below the member's append
position, truncates to that position before it does anything else with the
message. Then its append position is the position, its log term is the highest term
its record still holds, and it reads the message as a member with that log: it
follows the leader, recording the leader's term when the position is the leader's
base, or it is below the leader's base and would replicate (state 9), which is a
later step, so its election fails there as before. `cluster.log_truncations` counts
it.

**Never below the commit position.** A member told to truncate to a position below
the commit position it knows does not: it keeps its log, counts
`cluster.truncations_refused`, and fails its election, counted in
`cluster.elections_failed` as any failed election is. The counter should never rise.
When it does, one of the two members holds a log that is not this cluster's, and the
counter's `doctor` text says to stop both and compare.

**Why truncating to the next term's base is safe.** A leader appends a term's first
entry only after a quorum has recorded the term (states 5 and 8). So a member holds
entries of a term X only if a quorum recorded X, and then every later leader's
record holds X, by the vote's rule on logs. Such a member is told X's own end, the
base of the term after X, and cuts what it holds above that: entries of X that X's
leader appended and no later leader's log has, which no quorum recorded. A member
whose term the leader's record lacks holds none of that term's entries. Its log up
to its own base of that term is of older terms, a prefix of the leader's, and it
truncates only when the leader's base is below its own, which takes that term out
of its record with the bytes.

**`Io::truncate_log(position)`.** It cuts the member's recordings to the position
and takes every term recorded above it out of the record, durably, and returns the
highest term the record then holds. It returns nothing while a step is under way:
the core then does nothing with the message, and the leader's next new leadership
term, a leader heartbeat interval later or in answer to the member's next canvass
position, asks again. The core calls it only for a member that is out of the log,
which every member in canvass, nominate or a ballot is. The in-memory cluster cuts
the member's log, its positions and its record in one step. The consensus module
refuses, as it refuses `remove_log` (ADR 0082): its member is appointed, and only a
member that elects truncates. `truncate recording`, `purge recording` and the
`terms` file's `truncate_above` come with the rest of the module's I/O for the
election.

**The core sets its append position down in that one place.** Everywhere else it
still takes the greater of what it has and what `Io` reports.

**A canvass position keeps a member active.** The page now says what the core does
and what Aeron's member table does: the position a member last reported, in an
append position or in a canvass position, is what the leader keeps, and either
keeps the member active. A leader in leader log replication has only canvass
positions from the members that voted for it. If those did not count, it would have
no active member but itself, and would fail its election on its first cycle.

**The dead line goes.** `lead()` no longer clears a log: an appointed leader leads
once and holds none before.

**What this step leaves as it was.** A member that truncated is at the next term's
base. When that is the leader's base it is behind the leader's log position by what
the leader appended since, and joins by catching up, states 11 to 13. When it is
below the leader's base it lacks a whole term, and joins by replicating it, state 9.
Both are later steps, and until them the member fails its election every election
status interval, counted. The section "What the election does not do yet" of
`crates/cavalry-raft/README.md` says so.

## Alternatives considered

- **Truncate to the commit position the member knows, and replicate from there.**
  Lost. It cuts entries the leader's log holds, which the member then fetches
  again, and the commit position a member knows is as old as its last word from a
  leader. The page's rule cuts no more than the leader's log lacks.
- **Do what the message says, below the commit position too.** Lost. A member that
  cuts a committed entry on the word of one message has no way back, and the only
  histories that send that message are a log from another cluster or an older
  copy. Refusing costs an election that fails, and a counter that says why.
- **Count a refusal in `cluster.elections_failed` alone.** Lost. That counter rises
  every election status interval on any member that cannot join yet. A refused
  truncation among them would not show.
- **Count a truncation in no counter of its own.** Lost. It is the one place the
  cluster drops what a member held, and "which entries were lost when the leader
  changed" would have no counter to start from.
- **Let the core's append position follow `Io` down.** Lost. The rise-only rule
  protects every other cycle from a report that dips, and a truncation is one call
  at one known position.
- **Count a member as active by its append positions alone, as the page read.**
  Lost, for the reason in the decision: a new leader has none until its followers
  have joined its log.
- **Build the module's truncation in this change.** Lost. ADR 0081 puts the
  module's I/O after the core's steps, and nothing calls it while the module's
  leader is appointed.

## Evidence

- `crates/cavalry-raft/tests/truncation.rs`, six tests on one core through an `Io`
  that remembers what it was asked. A member that holds 128 bytes of term 0 and is
  told that term 1 begins at 64 cuts to 64 before it records term 1 there, once,
  and its append position stays at 64 on the next cycle. One at the base cuts
  nothing. While the truncation is under way the member records nothing, takes no
  leader and stays in canvass, and the next message asks again. A member that was
  told a commit position of 128 refuses to cut to 64, counts the refusal, fails its
  election and calls nothing. A truncation below the base of a term the member
  recorded takes that term out of its record, and the member then records the
  leader's term as one whose log is of the term before. A member whose leader is
  appointed fails where the election's member truncates, and calls nothing.
- The guard caught a history that this change's author wrote wrong: a test in which
  a member had been told a commit position of 96, and was then told to cut to 64,
  failed in the guard until its history was made a legitimate one.
- `crates/cavalry-raft/tests/election.rs`, on three and on five members in memory,
  with the check after every step that no committed entry is lost. The leader that
  was cut off and appended an entry no quorum recorded cuts it when the cut ends,
  once, at each of four seeds: its log is then byte for byte what it was before the
  entry, and it records the new term at that position. The follower of the
  five-member history of pull request 218 cuts its entry too, and what it holds is
  the third leader's log up to the second term's base.
- `crates/cavalry-raft/tests/transitions.rs`: a truncation moves neither the
  election nor the role, so no row is added; the three paths that failed where a
  member would truncate now fail where it would replicate, and one path truncates
  and follows.
- Every other test that was on the branch passes unchanged.

## Consequences

- A member that led without a quorum, or followed such a leader, drops the entries
  no quorum recorded when it next hears a leader, and `cluster.log_truncations`
  says that it did. It still does not join until the catch-up or the replication
  exists.
- `Counts` has nineteen counts, and a member's module adds nineteen client counters;
  eight of them stay 0 while the module's leader is appointed.
- The in-memory cluster's `pause_sync` leaves a member holding bytes it received
  above its append position while it is out of the log. A member over a driver has
  no such bytes: its recording's stop is where its log ends. The step that makes a
  member behind join decides what the in-memory member does with them.
- Revisit: when the module's I/O for the election is built, at which
  `ClientIo::truncate_log` stops refusing; and if `cluster.truncations_refused`
  ever rises in a cluster whose logs are its own, which would be a fault in the
  argument above.

## References

- `spec/cluster.md`, "The commit position", "Messages during an election", "The
  term's first entry" and "Counters".
- [ADR 0082](0082-give-up-the-lead-on-a-higher-term-or-a-lost-quorum.md), the
  elections a leader holds and `Io::remove_log`.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path, its steps and the module's move to it.
- [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the protocol
  and the next fields of new leadership term.
- Pull request 218, the next term from the leader's record; pull request 221 and
  its review, the quorum's watch and what keeps a member active.
- Aeron's `Election.onCanvassPosition` and
  `ConsensusModuleAgent.updateMemberLogPosition`, which sets a member's
  `timeOfLastAppendPositionNs` from a canvass position, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/tree/master/aeron-cluster/src/main/java/io/aeron/cluster>.
