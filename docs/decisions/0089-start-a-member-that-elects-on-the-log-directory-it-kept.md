# 0089. Start a member that elects on the log directory it kept

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Changes `spec/cluster.md`: "The log" says what a member that
  elects reads when it starts and what it does not keep; row 0 of "Roles and the
  election" says where init's two terms come from; row 1 and "Values" give the
  startup canvass timeout to every start, where they said "a member that started
  with no term"; the `is startup` field of new leadership term says whose start it
  is. The page's version stays 0: no message's layout changes, and no member over a
  driver takes a vote yet (ADR 0081).

## Context

ADR 0081 listed the steps of the electing path. The last before the module's I/O is
a member's start on a log it kept, and that ADR left it to this step to decide what
such a start is.

Until this change a member made with `Consensus::with_election` started with no
term, whatever its log directory held: a candidate term of −1 and a log leadership
term id of −1, beside the append position its recordings gave it. Two things were
wrong with that on a directory the member had kept.

**It could vote twice in a term.** A member stores its candidate term before the
vote or the request vote that raises it (ADR 0077), so that it votes once in a term
across a restart. The store was written and never read: a member that voted in
term 3, stopped and started answered a second candidate's request for term 3 for.

**It canvassed with a log of no term.** Its canvass position named −1 as its log
leadership term id and its real append position. A leader answers a member for the
term the member reports (ADR 0083): to −1 it names its record's first term, from
position 0, and the member, whose log ends above 0, is asked to truncate to 0 and
replicate everything it already holds.

The page had three clauses that the start touches. Row 0 takes "the higher of its
stored candidate term and its leadership term" as the candidate term, and says
nothing of where a member that has just started gets either. Row 1 gives the
startup canvass timeout to a member that "started with no term". The `is startup`
field of new leadership term is "1 when the leader started with the cluster rather
than taking over a running one", which a leader cannot know, and the core wrote 1
in every one.

## Decision

**What a member reads when it starts.** A member that elects reads two things from
its log directory, once, in its first init: the candidate term it stored, and the
highest term in its record of terms, which is its log leadership term id. Its
candidate term is the higher of the two, as init makes it at every election. They
come through two operations of `Io`, `candidate_term` and
`log_leadership_term_id`. Both are read from memory and cannot fail: the module
holds its `terms` file open and parsed (ADR 0060), and a directory that cannot be
read fails the member's start before it has a core. Init is the place because a
member in init reads no message: no vote is cast and no leader taken before the
read.

**What it does not keep.** The term it followed and its leader: it follows none
until it takes a leader's new leadership term or leads. And the commit position it
knew: it knows 0 until a leader tells it one. So a service reads nothing of the log
after a start until then, and the rule that a member never truncates below the
commit position it knows (ADR 0083) guards nothing for that long. What keeps a
committed entry then is what keeps it in any election: a leader holds every
committed entry, so what it asks a member to truncate was never committed.

**Its append position is what its recordings have synced.** The member reads it
each duty cycle through `Io::append_position`, as it always has. What had reached
a recording and was not synced is not in the member's log, since it never reported
it.

**The canvass at a start.** Every start takes the startup canvass timeout, 60 s,
as its canvass deadline until the member's first election since it started has
closed, on an empty directory or on one it kept. The core has done so since ADR
0081; the page says so now. Members that start together hear one another and are
candidates at once. A member that starts into a running cluster is answered by its
leader within an election status interval. Two members of three that start while
the third is down wait the 60 s before one of them, a quorum candidate, nominates
itself.

**`is startup` is the leader's own.** A leader that elects writes 1 in the new
leadership terms of a term it leads from its first election since it started, and
0 in those of any later term, in its election and after it has closed. In Aeron,
this is the leader's node startup, which its followers take from the message. The
appointed leader leads once, from its start, and writes 1 as before.

**In memory.** `Cluster::restart(member)` starts a member that elects again on the
log directory it kept, as a host that lost power and came back: a new core over its
stored candidate term, its record of terms and what its recordings had synced.
What had reached it and was not synced is gone, with its subscriptions, its image
of the log and its replays, and when it led, its log publication and every
follower's image of it. In a cluster whose leader is appointed it does nothing.

**Over a driver.** The module's `Io` answers both reads from its `terms` file. The
module still refuses to start on a log directory that holds a term or a stored
candidate term (ADR 0063) until it moves to the electing path (ADR 0081). Leader
replay and follower replay pass in the core; the replay of the kept log into the
module's own state, the open sessions, is the module's and comes with its I/O.

## Alternatives considered

- **Give the constructor what the directory holds.** Lost. The core reaches its
  `terms` file through `Io` and no other way, and a constructor argument would be
  a second one that the module and the in-memory cluster each assemble.
- **Read the base of the highest term too.** Lost. The core uses the base of the
  term it leads or follows, which a member that has just started does neither.
- **Keep the commit position across a restart.** Lost. It is a synced write for
  each commit, on the path a commit takes, to keep a guard that the election
  already gives.
- **A shorter canvass for a member that starts on a log it kept.** Lost. A member
  that has just started cannot tell a cluster that is starting from one whose
  leader it has not heard yet, and its last word from a leader is from before it
  stopped. Aeron takes its extended canvass at every node start, and the cost here
  is the same as there: 60 s with no leader when a quorum starts and the rest does
  not.
- **Keep in memory what a member had received and not synced.** Lost. A member
  reports only what it has synced, so the start that loses the rest is the one a
  test of invariant 1 must pass.
- **Leave `is startup` at 1.** Lost. Unspecified behaviour is a bug in the spec:
  the field had a meaning no leader could compute, and the core wrote a constant.

## Evidence

- `crates/cavalry-raft/tests/start.rs`, five tests on one core through an `Io` that
  remembers what it was asked.
  - A member on a directory that holds candidate term 7, a record whose highest
    term is 5 and 96 bytes reads each once in its first init, and canvasses with
    log leadership term id 5, position 96 and candidate term 7. It follows no
    term and knows a commit position of 0. After it has followed term 8, lost its
    leader and begun again at init, it has read nothing a second time.
  - With a stored candidate term of 2 and a record that ends at term 4, its
    candidate term is 4.
  - A member votes for a candidate in term 3 and stops. Started again on the
    directory, it answers another candidate's request for term 3 against, with
    nothing stored, and a request for term 4 for, the term stored before the vote.
    A member on an empty directory answers the request for term 3 for.
  - A leader's new leadership terms say `is startup` 1 in the term of its first
    election, in the election and after, and 0 in the term it leads after it lost
    its quorum.
  - A member whose leader is appointed reads neither and writes 1.
- `crates/cavalry-raft/tests/election.rs`, three tests of three members in memory,
  at seeds 1, 2, 3 and 12, with the check after every step that no committed entry
  is lost.
  - A follower starts again: it joins its leader again 12 ms later, under the same
    leader and term, with the record it had, no truncation and no candidacy, and
    holds the entry the leader appended meanwhile.
  - A leader starts again: its followers lose their images of the log and hold an
    election, a member leads a later term 28 to 72 ms later, the member that
    started again itself at two of the seeds, and all three end on its log with
    the entry the first leader committed.
  - All three start again, the leader with an entry appended that no recording had
    synced: each is a candidate at once, the cluster has one leader and three
    closed elections 37 to 1,132 ms later, the longest a split vote, every
    member's log holds what was committed byte for byte, and the unsynced entry is
    in none.
- Aeron's `Election`, read on 2026-11-12: `init` takes the greater of the node
  state file's candidate term and the leadership term; `isExtendedCanvass` begins
  as the node startup and gives canvass the startup canvass timeout; the leader
  sets `isLeaderStartup` from its own node startup, and a follower takes it from
  new leadership term.

## Consequences

- A member that elects can stop and start on its log directory. "What the election
  does not do yet" in the crate's README loses that entry, and keeps the module's
  refusal.
- A cluster of three in which two members start while the third is down has no
  leader for the startup canvass timeout, 60 s.
- A start has no counter of its own. A member's counters begin at 0 with its
  process, and `cluster.elections` counts 1 for the election its start begins.
- Revisit with the module's I/O for the election: what `Io::append_position` gives
  at a start over a driver for a recording that was not closed, and the replay of
  the kept log into the module's state. If the first can be above what a power
  loss left synced, the read at a start moves behind a check of the recordings.

## References

- `spec/cluster.md`, "The log", "Roles and the election", rows 0 and 1, "New
  leadership term, template 53" and "Values".
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path, its steps and the canvass deadline of a start.
- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the stored candidate term.
- [ADR 0083](0083-truncate-to-the-next-terms-base-and-never-below-the-commit-position.md),
  what a leader answers a member's log leadership term id with, and the commit
  position's guard.
- [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the module's refusal to start on a log.
- [ADR 0060](0060-replicate-the-log-with-an-appointed-leader.md), the `terms` file
  and its stored candidate term.
- Aeron's `Election`:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
