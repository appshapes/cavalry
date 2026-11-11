# 0081. Build the election beside the appointed leader, and move the module to it once

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Amends ADR 0077 in one clause: the paragraph "The appointed
  leader" of `spec/cluster.md` stays on the page until the consensus module elects,
  where ADR 0077 had it shrink with every pull request. Adds two rows to
  `spec/cluster.md`, "Counters", and one clause to the row of state 2; the page's
  version stays 0, since no message changes and no member over a driver takes a vote
  yet.

## Context

ADR 0077 decided that card 228 builds the whole election of `spec/cluster.md`, and
that each pull request leaves the page's paragraph "The appointed leader" true of the
code on `master`. Writing the first step, nominate and the two ballots in
`cavalry_raft::Consensus`, found four things that decision did not have.

**A core that elects everywhere from its first step leaves clusters over drivers in
states nothing is built for.** The consensus module and the in-memory cluster run the
same core. With a vote and nothing else, a leader whose module stood still returns
as a second leader of an older term that never steps down; a follower that is behind
or ahead of the new leader can neither catch up nor truncate; and the module cannot
give a term's log publication up to lead another (pull request 210 made it refuse
that). The tests over drivers say what an appointed cluster does in those cases:
`crates/cavalry-raft/tests/cluster.rs`, 1,377 lines, and
`crates/cavalry-raft/tests/replication.rs`, 960, are about the appointed leader, as
are two test files of `cavalry-ffi` and the replay check. One pull request that
elected everywhere would rewrite all of them to behaviour that the next five pull
requests change again.

**A change to `spec/` needs an Accepted ADR in the same change.** `just lint-docs`
refuses one without. A paragraph that shrinks with every pull request is an ADR for
every pull request, each recording nothing but which line went.

**The page names no counter for a ballot begun or a ballot lost.** "Counters" has
`cluster.votes_for` and `cluster.votes_against`, a vote sent by its value. A member
that becomes a candidate, and a ballot that ends with no leader, move the election
and the role, and invariant 4 asks a counter of each move. Without one, a split
vote, which ADR 0077 names as the case that takes an election past a second, reads
in the counters as nothing at all.

**The row of state 2 does not say what a member in nominate does with a new
leadership term.** It names the delay and the candidacy. "Messages during an
election" gives request vote to canvass, nominate and the two ballots, and new
leadership term to canvass and the two ballots.

## Decision

The election is built on a path of its own in the core, one step at a time, and the
consensus module moves to it once, when its I/O can do what an elected member needs.

**Two constructors.** `Consensus::new(member_ids, member_id)` is unchanged: the
appointed leader's path, which `ConsensusModule` and every test over drivers use.
`Consensus::with_election(member_ids, member_id, seed)` is the election of
`spec/cluster.md`, with the seed ADR 0077 gave it. The in-memory cluster gets the
same choice in the change after this one, `memory::Cluster::with_election`. The
steps on that path, each a pull request with its tests: nominate, the two ballots and
the leader's identity, which is this change; the elections a leader holds, and
leader ready by a quorum; the truncation; state 9; states 11 to 13; a member's start
on a log it kept. Then the module's `Io` grows what those states need. Then one
change makes `ConsensusModule` build its core with `with_election`, rewrites the
tests over drivers to the election, adds the fault-injection tests of ADR 0077,
takes the appointed path out of the core, `cavalry-proto` and the page, drops the
module's refusal to start on a log, and gives `cavalry doctor` the two gauges.

**The paragraph stays until that change.** "The appointed leader" is true of every
cluster a person can run until the module elects, so it stays on the page as it is.
What the electing path does not do yet, and what a member on it does instead in each
case, is a section of `crates/cavalry-raft/README.md`, "What the election does not do
yet", which each step shortens.

**Two counters, and the rows of three states.** `cluster.candidacies` counts a
ballot begun: a member in nominate that stored candidate term + 1, voted for itself
and went to candidate ballot. `cluster.ballots_lost` counts a ballot that ended with
no leader for the member, which canvasses again: its own at the election timeout, the
one of the candidate it voted for when no new leadership term came within the
election timeout, or either on a new leadership term for a log term the member did
not report. `cluster.votes_for` and `cluster.votes_against` are registered as the
page has them. The machine `cluster_election` gains nominate, candidate ballot and
follower ballot with fourteen rows, and `cluster_role` gains follower to candidate,
candidate to leader and candidate to follower, the last with the counter of each of
its three causes: a ballot lost, a vote for another candidate, and an election that
fails on a leader's term the candidate cannot join, after which it is a follower in
init, never a candidate. Two members that each count a
candidacy and a ballot lost in one election are a split vote.

**Nominate reads what canvass reads.** A member in nominate is a canvassing member
with a delay running: it votes for a candidate as the page already says, and it
takes a new leadership term for a term at least its candidate term as canvass does.
A member that learns of a leader during its delay follows it, and opens no ballot
against it half a second later.

**The canvass deadline.** A member that elects takes the startup canvass timeout
after the state began as its deadline until its first election since it started has
closed, and the leader heartbeat timeout after the last word of a leader from then
on, as Aeron's election takes its extended canvass at a node's start and no later.
The page's row for state 1 says "when the member started with no term"; a member
that starts on a log it kept comes with a later step, which decides what its start
is.

**What this step leaves as it was.** Leader ready keeps the appointed leader's rule
on both paths, every member heard or a quorum at the leader heartbeat timeout and
none ahead, until the catch-up exists: a follower that misses a term's first entry
cannot join without it. A leader reads no new leadership term and holds no election.

## Alternatives considered

- **One path: the core elects everywhere from this change.** Lost, for the reasons of
  the first paragraph of Context: clusters over drivers in states with nothing built
  for them, and about 2,300 lines of tests rewritten to behaviour that lasts one pull
  request.
- **A branch that holds the whole election until it is done.** Lost. It is thousands
  of lines in one review, against `docs/how-to/implement-a-feature.md`'s limit of
  about a thousand, and it would be merged with a `master` that moves several times
  a day for as long as it lasted.
- **A cargo feature.** Lost. A feature is a second build that the gates do not run by
  default. A constructor is in the one build, and its tests run with every other.
- **Shrink the page's paragraph with every pull request, as ADR 0077 said.** Lost. It
  costs an ADR a pull request, and the paragraph would describe the electing path
  while every cluster a reader can run is still the appointed one.
- **Count a candidacy in `cluster.election_steps` and a lost ballot in
  `cluster.elections_failed`.** Lost. An election that failed in a state begins again
  at init and leaves the log; a lost ballot goes back to canvass with the reports it
  has. An operator who sees elections take two seconds needs to tell a split vote
  from a member that cannot join, and one counter for both cannot.
- **Leave a new leadership term unread in nominate.** Lost. The member would open a
  ballot for a higher term against a leader it had just heard of, and a leader with
  no step-down yet would never take it back.

## Evidence

- `crates/cavalry-raft/tests/transitions.rs` makes every row of `cluster_election`,
  the twenty of the appointed path and the fourteen new ones, and every row of
  `cluster_role`, and checks after every call into a core that each move it made, of
  the election and of the role, is a registered row and that a counter registered
  for it rose.
- `crates/cavalry-raft/tests/ballot.rs`, eight tests on one core: a term is stored
  before the request vote or the vote it allows; a member votes once in a term; a
  ballot closes at once when every member heard has voted for the candidate, waits
  the election timeout for a member heard that has not, and is lost on a vote
  against; a candidate whose term rose during its ballot leads no term.
- Every test that was on `master` passes unchanged: `replication` 18, `core` 5,
  `cluster` 10 over drivers, the two allocation tests.
- On the branch this was written on, the in-memory cluster elects with this core,
  in the tests of the change after this one. Three members that start together
  elect one leader at each of eleven seeds; of sixty seeds two split the first vote,
  12 and 27, and settle on term 1 after 1,129 ms and 1,185 ms; a leader that stops
  with its followers level is replaced within a second of the first heartbeat
  timeout at each of twenty seeds, the slowest in 473 ms.
- `scripts/lint-docs.py`, the `change` check: "spec/ changed with no ADR in the same
  change whose Status is Accepted".

## Consequences

- The core holds two paths for some pull requests: `election: Option<u64>` in
  `Consensus`, and a branch on it where the paths differ. A fix to what both share
  is made once; a fix to one path's own lines is not the other's.
- `Counts` has fifteen counts, and a member's module adds fifteen client counters;
  four of them stay 0 while the module's leader is appointed.
- `cluster.appointed_leader_behind` still counts on both paths, in leader ready,
  until leader ready closes by a quorum.
- Revisit: when the module moves to the election, at which the appointed path, its
  vectors, its counter and the page's paragraph leave together; sooner, if a fix has
  to be made twice.

## References

- `spec/cluster.md`, "Roles and the election", states 1 to 4, "Messages during an
  election", "The appointed leader" and "Counters".
- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the election, the ballot on the members heard and the seed.
- [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the core, the module and the appointed leader.
- [ADR 0067](0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  the election's state machine in `counters.toml`.
- [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the protocol
  and its departures from Aeron.
- Pull request 210, the module's refusal of a second term while it holds the first's
  log publication.
