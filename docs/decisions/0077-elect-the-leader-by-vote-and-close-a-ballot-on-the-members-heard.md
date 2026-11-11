# 0077. Elect the leader by vote, close a ballot on the members heard, and build in the election's card what a lost leader needs

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Amends ADR 0058 in one clause, when a candidate's ballot
  closes; ADR 0052 in which of its features builds the replication of an older term
  and a member's start on a log it already holds, the fourth and no longer the sixth;
  and the last consequence of ADR 0067, which card adds state 9. It answers the first
  Revisit of ADR 0060 and the first of ADR 0063. `spec/cluster.md`'s version stays 0:
  no message changes, and no member of any build before this card casts or counts a
  vote, so two rules for a ballot never meet in one cluster.

## Context

Card 228 is the fifth slice's fourth feature (ADR 0052): a leader elected in under a
second after the heartbeat timeout, by vote, and fault-injection tests in which the
leader is killed and a member is partitioned, each asserting that nothing committed
is lost and that the cluster is available while a quorum remains. `spec/cluster.md`,
"Roles and the election", already specifies the whole election, Aeron's eighteen
states with the departures ADR 0058 records, and `spec/vectors/cluster.json` pins its
arithmetic. Card 227 built the path an appointed leader takes through those states
(ADR 0063), and the page's paragraph "The appointed leader" says what that leaves
out. Reading the card against the page and the code found five things that the page
does not settle, or settles in a way the card cannot meet.

**A ballot cannot close in under a second when the leader is dead.** State 3 says a
candidate leads when every member has voted for it, or when the election timeout,
1 s, has passed and a quorum has, no member having voted against. That is Aeron's
`Election.candidateBallot`: `ClusterMember.isUnanimousLeader`, else
`isQuorumLeader` once `electionTimeoutNs` has elapsed. A leader that was killed casts
no vote, so the vote is never unanimous and the ballot lasts the whole second. With
the canvass before it, up to one election status interval of 100 ms, and the random
delay of nominate, up to 500 ms, the new leader leads 1.0 s to 1.6 s after the
followers' heartbeat timeout. Aeron's election takes that long too. Aeron leaves one
member out of the unanimous count, the leader that closed gracefully
(`gracefulClosedLeaderId` in `isUnanimousCandidate` and `isUnanimousLeader`); a
leader that is killed says nothing as it goes.

**The member that loses the vote is behind the new term's base.** The commit
position is the quorum-th highest append position. With three members, a leader at
position 1,000 and followers at 900 and 500, it is 900. When the leader is killed
the follower at 900 wins, since the other's vote goes to the log that is at least
its own, and its term's base is 900. The follower at 500 can join only by
replicating the span from 500 to 900 from the new leader's recording, state 9. Until
it does, one member has recorded the new term, which is no quorum of three, so the
leader stays in state 5 and the cluster commits nothing. ADR 0067 and ADR 0052 left
state 9 to card 230. A test that kills the leader at rest, every follower at the
same position, would pass without it and would not be the test the card asks for.

**A partitioned member rejoins a log that has moved.** A follower cut off for longer
than the leader heartbeat timeout canvasses. When the partition heals, the leader's
log position is past the follower's append position, and state 15 lets a follower
join only at its append position. States 11 to 13, the catch-up within the term, are
how it rejoins while the leader appends. When the member cut off was the leader, it
returns with a tail that no quorum recorded, and the new leadership term it reads
asks it to truncate that tail first.

**The core has nothing to draw a delay from, and keeps no record of a vote.**
`cavalry_raft::Consensus` takes a clock and an `Io` and has no source of randomness;
nominate waits a random delay. The `terms` file holds a candidate term in two
checksummed slots (ADR 0060) and no member id a vote went to.

**`cavalry doctor` cannot name the leader or say why an election was held.** It
computes the leader from the members string, the lowest id, and no counter holds
either fact. The card's "Done when" asks for both from counters alone.

## Decision

The election is built as `spec/cluster.md` gives it, with one rule changed and four
things added.

**A ballot closes on the members heard.** A candidate leads its candidate term at
once when the votes for that term, its own among them, make a quorum and every
member it has heard in this election has cast one of them. It leads at the election
timeout as before: a quorum of votes for the term and none against. A member is
heard once the candidate has read any consensus message from it since the candidate's
election began at init. A member that has said nothing since then is not waited for:
a leader that was killed, or one that every member of the quorum has lost. A member
that is heard keeps Aeron's hold on the ballot: the candidate waits for its vote,
and a vote against sends the candidate back to canvass at the election timeout.
Leading still takes a quorum of votes for the term, each cast by a member whose log
the candidate's is at least, so the rule that keeps a committed entry is unchanged.
`cavalry_proto::ballot` takes the members heard, the `ballot` vectors carry them, and
state 3 and the election timeout's row of "Values" say so. The page gains a
paragraph, "How long an election takes": after the heartbeat timeout of the members
that remain, the canvass positions cross within one election status interval,
100 ms, nominate waits at most 500 ms, and the ballot is the candidate's stored
term, one request vote, the voter's stored term and one vote. So a member leads the
next term less than a second after the last of those timeouts wherever the two
synced writes and the round trip take under 400 ms together. Two things make it
longer, each by the election timeout: two candidates whose delays end within a round
trip of each other each vote for themselves, and a member that is heard and does not
vote is waited for.

**Card 228 builds every state the page names.** States 2 to 4, nominate and the two
ballots; state 9, the replication of an older term's span; states 11 to 13, the
catch-up within the term; the truncation a new leadership term asks for; the
elections a leader holds when it loses its quorum or reads a higher term; and a
member's start on a log directory that holds terms. With the last of them the
paragraph "The appointed leader" leaves the page and the module's refusal to start
on a log leaves the module (ADR 0063's Revisit). Each pull request leaves the
paragraph true of the code on `master`: it shrinks by what that pull request built.
Card 230 keeps the snapshot, the standby and the fault-injection tests of a restart
from a snapshot. The driver needs nothing new: `start replay` over `cavalry:udp`,
`stop replay`, `truncate recording` and `purge recording` are client protocol
version 3's and a publication at a position is ADR 0059's.

**The core takes a seed.** `Consensus::new` takes a 64-bit seed and draws nominate's
delay from a xorshift generator it holds, with no allocation and no call out of the
core. The module makes the seed once at its start from the standard library's
`RandomState`, which the operating system seeds, mixed with the member id, so that
two members never share a sequence; the in-memory cluster gives each member a seed
derived from its own, so that a test's elections are the same at every run.

**A member keeps its candidate term, and no record of whom it voted for.** It stores
the candidate term, one synced write, before it sends the vote or the request vote
that raises it, and it answers a request for a term at or below its candidate term
against. So it votes once in a term across a restart, with the file as ADR 0060 laid
it out. This is ADR 0060's first Revisit: the write ordering stays. The core reads
one consensus message at a time on one thread, so two requests that arrive in one
duty cycle are two stores and two answers, in order; and a store that is torn by a
crash reads back as the term before it, for which no vote had been sent.

**Two gauges tell an operator who leads and why the last election began.**
`cluster.leader_member_id` holds the id of the member this member leads as or
follows, −1 while it has none. `cluster.election_cause` holds a code for why this
member's last election began: 0 the member started, 1 the leader heartbeat timeout,
2 the image of the leader's log was lost, 3 the leader lost its quorum, 4 a message
carried a term above its own, 5 its election before failed. `cavalry doctor` reads
both from one member's counters file. The counters `spec/cluster.md` names for the
states this card builds, `cluster.votes_for`, `cluster.votes_against`,
`cluster.leader_step_downs`, `cluster.log_replications`,
`cluster.log_replications_unavailable` and `cluster.catchups`, are registered with
the transitions that move them, each in the change that builds its state (ADR 0067).

**The bound is asserted where the clock is the test's.** The in-memory cluster kills
its leader under load at each of a fixed set of seeds and asserts, on its own clock,
that a member leads the next term within one second of the first heartbeat timeout,
and that every entry committed before the kill is in every remaining member's log.
The tests over three drivers on the loopback assert what a wall clock can: no
committed entry lost, every committed entry on every member's recording, and an
entry committed by the two members that remain within five seconds of the heartbeat
timeout. They report the election's time and do not assert the second, since a
split vote, one election in some hundreds, takes longer by design.

## Alternatives considered

- **Keep Aeron's ballot and promise a second and a half.** Lost. ADR 0052 carried
  "elected in under a second" from the README, and the wait buys nothing against a
  member that will never vote. What decides safety is a quorum of votes for the term,
  which the rule here still requires.
- **Lead as soon as a quorum has voted for, Raft's rule.** Lost. It drops the hold a
  member that is present has on the ballot. With members at 900, 500 and 500 and the
  canvass deadline passed, a member at 500 is a quorum candidate; under this rule it
  leads on the other's vote and truncates the tail at 900, which the member holding
  it was there to defend. By the rule chosen the candidate waits for that member,
  reads its vote against, and goes back to canvass, and the longer log leads. The
  tail is uncommitted either way; the rule chosen loses less of it.
- **Shorten the election timeout to 400 ms.** Lost. It is a constant chosen to make a
  sentence true. It halves the time a slow network has to carry a vote, for every
  ballot and not only the one with a dead member, and it leaves Aeron's default for no
  reason an operator could be told.
- **Leave out only a leader that closed gracefully, as Aeron does.** Lost. It needs a
  message a closing leader sends, and the card's case is a leader that is killed and
  sends nothing.
- **Leave state 9 and the restart with card 230, as ADR 0052 and ADR 0067 had it.**
  Lost. The card's first test fails without state 9 whenever the two followers are
  at different positions when the leader dies, which under load is always. The
  catch-up within the term, the replication of an older term and the truncation are
  one mechanism seen three ways: a replay of the leader's recording to the follower's
  `catchup` endpoint, recorded at the follower's append position. A member that starts
  on its log needs the same three and its record of terms read back. Splitting them
  across two cards would land a leader election that survives a lost leader only at
  rest.
- **Kill the leader only when the followers are level.** Lost. A test written to
  avoid the case that fails is not a test of invariant 1.
- **Record the member a vote went to.** Lost. The candidate term stored before the
  vote already keeps a member from voting twice in a term, and a member that restarts
  after voting for itself canvasses again and proposes the next term. Aeron's node
  state file holds the candidate term alone.
- **Draw the delay through `Io`.** Lost. The trait is the member's I/O, each method a
  thing the module does over its driver. A delay is arithmetic on a seed. A seed at
  construction also makes every election of a test repeatable without a second fake.
- **Let `doctor` find the leader by reading every member's counters.** Lost. `doctor`
  reads the counters file of one driver directory. The member it reads knows whom it
  follows, and a gauge says so.

## Evidence

- Aeron, `aeron-cluster/src/main/java/io/aeron/cluster/Election.java` at its `master`
  on 2026-11-11: `canvass` moves to nominate on `isUnanimousCandidate`, or on
  `isQuorumCandidate` once `timeOfLastLeaderUpdateNs + leaderHeartbeatTimeoutNs` has
  passed, with `delayNs = random.nextDouble() * (electionTimeoutNs >> 1)`;
  `candidateBallot` leads on `isUnanimousLeader`, and otherwise only once
  `electionTimeoutNs` has elapsed since the state began, on `isQuorumLeader`.
  `ClusterMember.isUnanimousLeader` returns false for any member, other than
  `gracefulClosedLeaderId`, whose vote for the candidate term is not true.
- The arithmetic, from `spec/cluster.md`, "Values": election status interval 100 ms,
  election timeout 1 s, a nomination delay of up to half of it. By the page's rule
  with a dead leader: up to 100 ms, up to 500 ms, then 1 s. By this rule: up to
  100 ms, up to 500 ms, then one round trip.
- The commit position's arithmetic, the `commit-position` vectors: with a leader at
  1,000 and followers at 900 and 500 the quorum position is 900, so the follower at
  500 is behind the base of the term the follower at 900 leads next.
- `crates/cavalry-log/src/terms.rs`, `set_candidate_term`: one write of the slot the
  stored term is not read from, then `sync_all`, before it returns.
- `crates/cavalry-client/src/client.rs`: `truncate_recording`, `purge_recording`,
  `start_replay` and `stop_replay` exist and are tested against the driver.
- What the implementation proves goes in its commit messages: the in-memory test's
  election times at each seed, and the loopback tests' twenty runs on both runners.

## Consequences

- The election is faster than Aeron's by the ballot's second whenever a member is
  dead, and identical to it whenever every member answers.
- A member that is alive and silent through a candidate's canvass, one whose
  messages a lossy network dropped, is not waited for, and may find a leader of a
  term it did not vote in. It follows that leader as any member does, and truncates
  a tail the quorum did not hold.
- `memory::Cluster` gives every member a log of its own, so that two members can
  hold different bytes at one position and the safety check of the crate's tests can
  fail; it gains partitions between chosen members, replays and a seeded delay.
- `Consensus::new` gains a parameter, and `ElectionState` gains seven states with
  their rows in `counters.toml`. The appointed leader's functions and vectors,
  `appointed_leader`, `appointed_canvass` and `leader_ready`, leave `cavalry-proto`
  and the page with the paragraph.
- Card 230 is the snapshot and the standby. ADR 0052's sixth feature reads as that
  from here on.
- Revisit: when the loopback tests' reported election times are above a second in
  more than one run in a hundred, for the split vote; when a cluster of five or seven
  is first run, for how many candidates a canvass makes; when card 229 gives sessions
  their state, for what states 6 and 10 replay; when the first release freezes the
  cluster protocol's version.

## References

- `spec/cluster.md`, "Roles and the election", state 3 and "Values"; `spec/vectors/cluster.json`, the `ballot` vectors.
- [ADR 0052](0052-build-raft-on-the-durable-log-fifth.md), the slice's features.
- [ADR 0058](0058-specify-the-cluster-protocol-from-aerons-codecs.md), the election
  and its departures from Aeron.
- [ADR 0059](0059-create-a-publication-at-a-position-and-arm-a-recording.md), a
  publication at a position.
- [ADR 0060](0060-replicate-the-log-with-an-appointed-leader.md), the `terms` file.
- [ADR 0063](0063-run-the-consensus-module-as-a-client-and-lead-by-appointment.md),
  the core, the module and the appointed leader.
- [ADR 0067](0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  the election's state machine in `counters.toml`.
- Aeron: `Election.java` and `ClusterMember.java` under
  `aeron-cluster/src/main/java/io/aeron/cluster/` at github.com/aeron-io/aeron.
- Card 228, and the note on it from card 227 about the in-memory safety check.
