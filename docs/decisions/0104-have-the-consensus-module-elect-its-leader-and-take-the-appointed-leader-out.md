# 0104. Have the consensus module elect its leader, and take the appointed leader out

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Builds the fifth change of ADR 0100, which is the last
  change of ADR 0081. Changes `spec/cluster.md`: "The appointed leader" leaves
  "Roles and the election", and two paragraphs take its place for the two things
  that stay true, a member's start and the leader's own entries; the rows of
  "Counters" and "Values" and the two lines of "The log" that named the appointed
  leader say what a member that elects does; 25 vectors leave, the kinds
  `appointed-leader`, `appointed-canvass` and `leader-ready`. No message's layout
  changes, and the page stays at version 0. Amends ADR 0100 in one clause:
  `cluster.appointed_leader_behind` stays registered, added and counted by no
  member, until the change to `cavalry doctor` that follows this one, which also
  takes "appointed" out of `doctor`'s texts and of the `cluster.member.id` key's.

## Context

ADR 0081 built the election on a path of its own and left the consensus module on
the appointed leader's path until its I/O could follow. ADR 0100 ordered the
changes around the move. The four before it have landed: the two gauges, `cavalry
doctor`'s read of the leader, the tests' own way to find a leader, and the core's
tests of shared rules on the election's path. One change more came before the
move, a service's read of a log that is in several recordings (ADR 0103), because
the move turned out to need it.

**The move was tried first, in a scratch branch.** `ConsensusModule::start` built
its core with `Consensus::with_election` and the seed of ADR 0077, and nothing
else changed. On a laptop, macOS, three drivers and three modules in one process
on the loopback:

- The tests over drivers, the linter, `just conformance` with three `cavalry run`
  members, and `just replay-check` passed.
- A leader and its driver were stopped with a hundred entries committed. In 64
  runs the two members that remained elected one, committed an entry, and held
  the same log for the first term. In the last 30 runs the new leader's election
  closed 142 ms at the least, 327 ms at the median and 548 ms at the most after
  the leader heartbeat timeout, 10 s.
- A second ballot was taken in 2 of the 64 elections after a loss, 3%, and those
  two closed 1.66 s and 1.68 s after the timeout; and in 3 of 64 first elections
  at a start, 5%. In each, two members had nominated themselves for one term
  within about 6 ms and each had answered the other against. The window is the
  candidate's durable store of its term before it asks for a vote, 3.8 ms at the
  median here, and the request's way to the other member. Two delays drawn up to
  500 ms collide in such a window in 2.4% of elections, and three at a start in
  about twice that.
- A test over drivers that assumed term 0, one ballot or an exact count of
  election steps failed in 5 of 20 runs. An elected leader's first term is 1 when
  its first ballot was split; a follower takes four steps or five.
- A follower that was slow at a cold start joined after its leader's first entry,
  in 2 of 30 runs of twelve tests at once and in none of 40 starts of one
  cluster to a process. It caught up through a replay over real drivers, and its
  service was refused below its live recording, which ADR 0103 ends.

**ADR 0077's Revisit fired, and ADR 0105 answered it before this change.** The
Revisit reads: when an election takes more than the election timeout, 1 s, in
more than one run in a hundred. With the store in the window the rate was three
in a hundred. Since ADR 0105 a candidate that reads a vote against its term
canvasses again at once, so a split vote costs a second delay of at most 500 ms
and not the second those two elections took.

**What the page says while a member is appointed.** "The appointed leader" is 66
lines of "Roles and the election", with 25 vectors, three functions of
`cavalry-proto`, a path of 27 branches in the core, a counter, and the tests of
its own rules. ADR 0100 has them leave in the commit where the module elects, so
that the page is never untrue of a member started with `cavalry run`.

## Decision

**The consensus module elects.** `ConsensusModule::start` builds its core with
`Consensus::with_election` and a seed taken from the operating system through the
standard library's `RandomState`, mixed with the member id, so that no two
members draw one sequence of nomination delays. A cluster started with `cavalry
run` canvasses, nominates, votes and leads as "Roles and the election" says.

**The appointed leader leaves.** `Consensus::new` and `memory::Cluster::new`, the
path's branches in the core, `appointed_leader`, `appointed_canvass` and
`leader_ready` in `cavalry-proto`, the 25 vectors, the paragraph, and the tests of
the path's own rules go in this change. The one constructor keeps its name,
`with_election`, in the core and in memory.

**Two paragraphs stay on the page**, since both are still true. A member starts
on a log directory that holds no term and no stored candidate term, and refuses
one that holds either, until ADR 0100's sixth change. And the leader's own
process may append a session message of no session.

**The ballot is ADR 0105's.** This change does not touch it. The figures above
were taken before that ADR, on the scratch branch; the figures of the test that
stops a leader, on this change's own branch, are in Evidence.

**No test over drivers assumes a term, one ballot or a step count.** Each reads
the leader's term, and takes a count of steps as exact only where the term says
the first ballot was won. A new test stops the leader and its driver: the two
that remain elect one, an entry is committed by both, the first term's log is
the same on both from the later of their recordings' starts, and the election's
time and each member's ballot counters are printed.

**`cluster.appointed_leader_behind` stays registered for one change more.**
`cavalry doctor` reads its constant, and `cavalry-control` is the control plane,
which a pull request under a data-plane crate does not touch. No member adds or
counts it from this change on, which is how `doctor` already tells a member that
elects. The change that follows takes it out with `doctor`'s check
`cluster-leader-behind`, the texts that name an appointed leader, and the sentence
of the `cluster.member.id` key that does, which `just gen-config` writes into
`cavalry-control`.

**One member is never started alone on a new log directory.** Under an appointed
leader such a member joined nothing. A member that elects joins: it replicates
the log and votes. It has lost the record of the votes it cast and the log it
held, so with a member whose log is behind it is a quorum that can elect a leader
without an entry the cluster committed. The page's paragraph on a member's start
and the module's refusal of a kept directory say so, and say what to do: the
cluster goes on with the members that remain, and is started again as a new
cluster to have every member again, until ADR 0100's sixth change.

## Alternatives considered

- **Move the module and take the path out in two changes.** Lost, as ADR 0100
  decided: between the two the page describes a leader no cluster has.
- **Change the ballot in this change.** Lost. It is a rule of the page with
  vectors of its own, and it went first, as ADR 0105.
- **Rename `with_election` to `new`.** Lost. The name says what the seed is for,
  and some hundreds of test lines call it.
- **Keep the appointed path in memory as a test double.** Lost. The tests of
  shared rules run on the election's path since ADR 0100's fourth change, and a
  path no cluster runs is a second set of rules to keep true.
- **Retire the counter here.** Lost for the plane's rule above, at the cost of
  one registered counter that reads 0 for one change.

## Evidence

@@EVIDENCE@@

## Consequences

Easier. A cluster that loses its leader has another after the leader heartbeat
timeout and an election, with no person in the path. The core has one path, and
the page one set of rules.

Harder. A cluster of three in which two members start while the third is down
has no leader for the startup canvass timeout, 60 s (ADR 0089); under an
appointed leader it led at once when the appointed member was one of the two. A
member still does not start on the log directory it kept, so a member that stops
is replaced by starting the cluster again, as before.

To maintain. The tests over drivers must not assume which member leads, which
term is first, or how many ballots an election takes. `cavalry doctor` still has
texts for an appointed leader until the change that follows.

Revisit when the lost-leader test's election times are known on Linux: `just
test` shows a passing test's output nowhere, so this record has the laptop's
alone.

## References

- [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  the order, and its fifth change;
  [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the two paths;
  [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the ballot, the seed and the Revisit;
  [ADR 0105](0105-lose-a-ballot-at-its-first-vote-against.md), which answered it;
  [ADR 0089](0089-start-a-member-that-elects-on-the-log-directory-it-kept.md),
  the canvass at a start;
  [ADR 0103](0103-list-the-recordings-of-a-members-log-in-its-driver-directory-for-a-service-to-read-in-order.md),
  the service's read this change needed first;
  [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the appointed leader.
- `spec/cluster.md`, "Roles and the election", "Counters", "Values", "Vectors".
- Card 228.
