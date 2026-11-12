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
Revisit has this case: "when the loopback tests' reported election times are above
a second in more than one run in a hundred, for the split vote". With the store in
the window the rate was three in a hundred. Since ADR 0105 a candidate that reads a vote against its term
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
were taken before that ADR, on the scratch branch. The test that stops a leader,
on this change's own branch, prints its election's time and asserts no bound on
it; Evidence says how often it passed.

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

**A log directory that holds the log and no `terms` file is refused.** A member
records a term before it records an entry, so a directory whose catalog holds a
recording of the log's stream has been a member's, and one with no `terms` file
beside it has lost the member's votes. `TermsFile::open` creates a file that is
not there, which is right for a new directory alone: the module looks in the
catalog first and refuses with `RaftError::TermsLost`, `terms-lost`, and leaves
the directory as it was.

**Members of the version before are not members of this cluster.** A member whose
leader is appointed casts no vote and answers no request for one; when it has the
lowest id it leads term 0 once every member has reported, with no ballot, and
never gives the lead up; when it has not, it follows the member with the lowest id
and no other. No message's layout tells the two apart, so neither refuses the
other. Since no member starts again on its log, a cluster of that version comes to
this one as a new cluster, every member on this version and on a new log
directory: there is no rolling upgrade across this change.

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
- **Have a member whose record holds no term cast no vote for a candidate that
  holds a log, until it has joined a log.** This is the guard against a member
  started alone on an empty log directory. Lost, for this change. It is safe and
  it is not live: a member that was slow to join a new cluster's first term, and
  whose leader then stops, would never vote, and a cluster of three would have no
  leader until it was started again. Nor does it know what it had acknowledged
  before. The answer that has neither fault changes who the members are, a member
  whose identity goes with its log directory or a change of membership through
  the log, and it is a decision of its own: card 274.
- **Refuse a start on an empty log directory beside members that hold a log.**
  Lost. That is every member that starts late in a new cluster, which this change
  makes join.

## Evidence

- `crates/cavalry-raft/tests/cluster.rs`, three members over three drivers on the
  loopback, thirteen tests, run thirty times in a row on this change's branch on a
  laptop, macOS: thirty passed. Among them:
  - `a_lost_leader_is_replaced_by_the_two_that_remain_and_they_commit`: the leader
    and its driver stop with a hundred entries committed; the two that remain
    elect one; an entry offered to it is committed by both; and the first term's
    log is the same on both from the later of their recordings' starts.
  - The first test reads the term the cluster elected in, and takes the count of
    election steps as exact only in term 0: four on the leader, four or five on a
    follower.
  - The services' tests read each member's log from position 0, across its
    recordings where it has more than one (ADR 0103).
- Three `cavalry run` processes on the laptop, each a driver and a member. Member 2
  was elected within about a second: `cavalry doctor` said `the leader in term 0,
  election closed: append position 96, commit position 96; its last election
  began because the member started` on it, and `a follower in term 0 ...; it
  follows member 2` on the others. Member 2 was stopped. Thirteen seconds later
  `doctor` said `the leader in term 1, election closed: append position 192,
  commit position 192; its last election began because its leader said nothing
  for the leader heartbeat timeout` on member 0, and `a follower in term 1 ...; it
  follows member 0` on member 1. `docs/how-to/run-a-cluster.md` is written from
  that output.
- `crates/cavalry/tests/cli.rs`: three `cavalry run` members elect; exactly one
  has the role of leader and two follow, in term 0 or 1; `doctor` on a follower
  names it so; a member does not start again on its log directory, and the
  refusal says what to do.
- `just conformance`, the five clients' rounds beside a member of three that
  elect, and `just replay-check`, "three equal hashes of one recording, six equal
  hashes of three members and their services", both exit 0 on this branch.
- What left, counted on this branch against the change before it: 3,162 lines
  removed, 547 of them generated vectors. `tests/replication.rs`, eighteen tests on
  appointed clusters, and `tests/core.rs`, five, are gone. Three of their rules had
  no test on the election's path and were ported to
  `tests/elected_replication.rs`: a cluster's first entry is its leader's new
  leadership term event; two of three wait the startup canvass timeout and the
  member that starts late catches up; and a leader that lost its log directory
  leads no term and replicates what it lost. Five checks of leader ready went
  into `tests/step_down.rs`. Eight tests that said what an appointed member does
  not do are gone with it. `cargo test -p cavalry-proto -p cavalry-raft`: 212
  passed.
- `tests/transitions.rs` makes every registered row of `cluster_election` and
  `cluster_role` on the one path, and fails on a row no path makes: the rows
  canvass to leader log replication and follower replay to init left the
  registry with the appointed leader.

## Consequences

Easier. A cluster that loses its leader has another after the leader heartbeat
timeout and an election, with no person in the path. The core has one path, and
the page one set of rules.

Against invariant 1, and named so. A member started alone on an empty log
directory, where it had a log, now joins and votes with no record of the votes it
cast or of the entries it acknowledged. With a member whose log is behind it is a
quorum, and the two can elect a leader that lacks an entry the cluster committed.
Under the appointed leader that member joined nothing. A container whose log
directory is on no volume does this at every restart. `cavalry run` cannot tell
that directory from a new cluster's, so nothing refuses it, no counter counts it
and `cavalry doctor` cannot say it happened. It is left to a rule for the
operator, in the refusals' texts, in the page and in the how-to's prerequisites.
Card 274 is the decision that ends it, and a condition of the first release (ADR
0061); what Aeron does with a node whose cluster directory was lost is read there
first.

Harder. A cluster of three in which two members start while the third is down
has no leader for the startup canvass timeout, 60 s (ADR 0089); under an
appointed leader it led at once when the appointed member was one of the two. A
member still does not start on the log directory it kept, so a member that stops
is replaced by starting the cluster again, as before.

A truncation is now something a member over a driver does. One that faults part
way, a purge that succeeded and a later `truncate recording` that did not, leaves
the member's record of where its log ends, and its record of terms, as they were:
until the leader's next new leadership term asks again and the rest is cut, the
member reports an end its recordings no longer hold (ADR 0083, ADR 0095; the review
of pull request 246 named it). The fault-injection tests of ADR 0100's seventh
change are where it is shown or closed.

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
