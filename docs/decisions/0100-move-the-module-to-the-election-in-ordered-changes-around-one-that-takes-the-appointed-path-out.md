# 0100. Move the module to the election in ordered changes, around one that takes the appointed path out

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 6
- **Supersedes:** none. Amends ADR 0081 in one clause, the last sentence of "Two
  constructors": "Then one change makes `ConsensusModule` build its core with
  `with_election`, rewrites the tests over drivers to the election, adds the
  fault-injection tests of ADR 0077, takes the appointed path out of the core,
  `cavalry-proto` and the page, drops the module's refusal to start on a log, and
  gives `cavalry doctor` the two gauges." Changes no page of `spec/`.

## Context

ADR 0081 built the election on a path of its own in the core and left one change for
the end: the consensus module moves to that path, and everything of the appointed
leader leaves with it. The steps before that change have landed or are in review:
the core's election, and the module's `Io` for what an elected member asks of a
driver. The last change is next.

**It is too large to be one pull request.** A pull request here is about a thousand
hand-written lines, so that a review's blockers fit one fix run
(`docs/how-to/implement-a-feature.md`, step 3.8). Counted on `master` at dffa899:

- "appointed" is in 39 files outside `docs/decisions/` and the changelog.
- `crates/cavalry-raft/src/consensus.rs` reads `self.election` in 26 places.
- `crates/cavalry-raft/tests/cluster.rs`, the tests over three drivers, is 1,511
  lines and 11 tests, and names `members[0]` as the leader 14 times.
  `tests/module_allocations.rs`, two tests of `cavalry-ffi`, the replay check's
  example and five conformance runners make the same assumption.
- `crates/cavalry-raft/tests/replication.rs` has 18 tests on 19 in-memory clusters
  made with `memory::Cluster::new`, the appointed path. They test rules both paths
  share: what a commit needs, and what a follower's log holds.
- `crates/cavalry-control/src/doctor.rs` says "appointed" 54 times, and computes the
  leader as the member with the lowest id.
- `spec/cluster.md`, "The appointed leader", is 67 lines, 716 to 782, and 25 of the
  page's 212 vectors pin its rules: 4 `appointed-leader`, 11 `appointed-canvass`
  and 10 `leader-ready`.

**Some of it must leave together, and some need not.** The page says what a member
does. While `cavalry run` starts a member whose leader is appointed, the paragraph
is true and its vectors, the arithmetic in `cavalry-proto` and the core's path are
its implementation. In the commit where the module elects, the paragraph is false,
and so is every one of those. They leave in that commit or the page is wrong for a
while. A test that offers through `members[0]` has no such tie: it can ask which
member leads today, while the answer is still member 0.

**No counter says which member leads.** A member's counters say its own role. A
test, a script or `cavalry doctor` that must find the leader of an elected cluster
reads every member, or reads `cluster.leader_member_id`, the gauge ADR 0077 decided
and no change has registered.

**The module holds no session.** ADR 0089 and the crate's README say the module
starts on a log directory it kept "with the replay of the log it kept into its own
state, the open sessions". The module has no session state yet: `ConsensusModule`
holds its core, its `Io`, its consensus subscription and its counters. What a start
on a kept directory needs over a driver is the module's table of the recordings it
keeps, and where its log ends, read back from the driver's catalog and the `terms`
file.

## Decision

The module moves in the changes below, in this order. Each is a pull request or
more, each leaves `master` green, and after each one every sentence of
`spec/cluster.md` is true of a member started with `cavalry run`.

**Before the module moves. No cluster does anything it did not do.**

1. **The two gauges.** `cluster.leader_member_id` and `cluster.election_cause`, as
   ADR 0077 has them, registered by the module and moved by the core on both paths.
   Under an appointed leader the first holds the appointed leader's id once a member
   follows or leads, and the second holds the causes that path has.
2. **`cavalry doctor` reads the leader.** It names the leader from
   `cluster.leader_member_id` and says why the last election began, where it
   computed the member with the lowest id.
3. **Whatever runs a cluster over drivers finds its leader.** The tests over three
   drivers, the allocation tests, the two tests of `cavalry-ffi`, the replay check's
   example and the conformance runners ask which member leads and offer through it.
   While the leader is appointed the answer is the lowest id, and they pass as they
   did.
4. **The core's tests of shared rules run on the election's path.** The in-memory
   tests of replication and commit, and the others that reach a shared rule through
   `Consensus::new` or `memory::Cluster::new`, are made with `with_election`. A test
   of a rule the appointed path alone has stays where it is until the next change.

**The move. One change, as ADR 0081 decided, for what leaves together.**

5. `ConsensusModule::start` builds its core with `Consensus::with_election` and the
   seed of ADR 0077. In the same change the appointed path leaves the core, its
   arithmetic leaves `cavalry-proto`, its 25 vectors and "The appointed leader"
   leave the page, `cluster.appointed_leader_behind` is retired, and the tests of
   that path's own rules go. Two sentences of the paragraph stay true and stay on
   the page: a member does not start on a log directory that holds a term, and the
   leader's own process appends a session message of no session. With changes 3 and
   4 landed, this change adds little and removes much.

**After the module moves.**

6. **A member starts on the log directory it kept** (ADR 0089, over a driver). The
   module reads back the recordings it keeps and where its log ends, and
   `RaftError::Restart` goes. Until this change a member that elects still refuses
   such a directory, as every member does today: a cluster that lost a member goes
   on with the members that remain.
7. **The fault-injection tests of ADR 0077**, over three drivers on the loopback: a
   lost leader and a partitioned member, with what ADR 0077 has them assert and
   report.

**What the amended clause keeps.** The module's constructor, the appointed path, its
vectors, its counter and the page's paragraph leave in one commit, which is ADR
0081's Revisit. The gauges and the tests' way to find a leader go before it. The
start on a kept directory and the fault-injection tests come after it.

**A change that cannot stand alone joins its neighbour.** If one of the changes
above cannot leave `master` green and the page true by itself, it lands with the
one after it, and its pull request says so.

## Alternatives considered

- **One change, as ADR 0081 wrote it.** Lost. It is several thousand lines by the
  counts above. A review of that size finds more than one fix run holds, and a
  conflict with any other landing costs the whole of it.
- **Move the module first and take the appointed path out after.** Lost
  (invariant 6). Between the two, the page's paragraph, its vectors and
  `cavalry doctor`'s texts describe a leader no cluster has.
- **Take the appointed path's tests out instead of moving them.** Lost
  (invariant 1). The 18 replication tests are the in-memory evidence for the commit
  rules. Deleted with the path, those rules would rest on the tests over drivers
  alone, where the clock is the wall's.
- **Leave the gauges to the move, as ADR 0081 listed them.** Lost. Changes 2 and 3
  need a way to name the leader that does not assume the lowest id. Without the
  gauge each of them reads every member's role, and reads it differently.
- **Start on a kept directory before the move.** Not possible. A member whose
  leader is appointed does not start again on its log ("The appointed leader"), so
  the module could not run what it built.

## Evidence

- The counts in Context, each a `grep` or a line count on `master` at dffa899.
- The module's fields: `ConsensusModule` in `crates/cavalry-raft/src/module.rs`
  holds `core`, `io`, `consensus`, `counters`, `messages_rejected`, `started` and
  `clock`. `MAX_SESSIONS` and `MAX_PENDING_ANSWERS` of `cavalry-proto` are used by
  no crate.
- The five runners, `conformance/{python,node,go,jvm,dotnet}/run.sh`, wait for
  "election closed" from `cavalry doctor` on member 1 and run beside member 1. A
  member's election closes whether it leads or follows, so that wait holds on both
  paths; what they expect of the log's first entry is change 3's to check.
- Not shown here: that each change stands alone. Each pull request shows it with
  its own gates, and the last paragraph of Decision says what happens when one does
  not.

## Consequences

- Seven changes where ADR 0081 wrote one, and more pull requests than changes.
  Change 3 and change 4 are tests and scripts alone.
- `crates/cavalry-raft/README.md`, "What the election does not do yet", shrinks
  with changes 5 and 6 and goes when nothing is left in it.
- A counter leaves `counters.toml` for the first time. Its id, 113, is never used
  again. How the generator and `just lint-counters` carry an id that is retired is
  change 5's to settle.
- A partitioned member over drivers needs a way to cut a member off that the driver
  does not have: no loss or partition can be asked of it. Change 7 decides between
  a relay in the test and a facility of the driver, in an ADR of its own.
- Between changes 5 and 6 a cluster elects and a member that stopped cannot come
  back on its log. That is one more thing than today's cluster can do, and one
  fewer than the card asks.
- Revisit if two changes in a row have to be joined: the order is then wrong, and
  this decision is written again.

## References

- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  "Two constructors" and Consequences, the Revisit.
- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the seed, the two gauges and the tests over three drivers.
- [ADR 0089](0089-start-a-member-that-elects-on-the-log-directory-it-kept.md), "Over
  a driver" and its Revisit.
- `spec/cluster.md`, "Roles and the election", "The appointed leader".
- `docs/how-to/implement-a-feature.md`, step 3.8, the size of a pull request.
