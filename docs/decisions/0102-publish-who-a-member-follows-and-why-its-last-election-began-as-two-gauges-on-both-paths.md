# 0102. Publish who a member follows, and why its last election began, as two gauges on both paths

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 4, 6
- **Supersedes:** none. Adds two rows to `spec/cluster.md`, "Counters". No message's
  layout changes; the page stays at version 0. Builds the first change of ADR 0100
  and the gauges ADR 0077 decided.

## Context

ADR 0077 decided two gauges: `cluster.leader_member_id`, the id of the member this
member leads as or follows and −1 while it has none, and `cluster.election_cause`,
a code for why this member's last election began. No change registered them. ADR
0100 puts them first among the changes that move the consensus module to the
election, before the module moves, because the changes after it need a way to name
a leader that does not assume the lowest id: `cavalry doctor`, the tests over three
drivers, and the scripts that run a cluster.

**The page's "Counters" names what each counter counts, and has no row for these.**
A change to the page needs an Accepted ADR in the same change (`just lint-docs`),
and ADR 0100 changes no page.

**Three things ADR 0077 left open.**

- When a member has a leader. Where the leader is appointed the core knows its id
  from the member's start: `Consensus::leader_member_id` returns the lowest id while
  the member still canvasses and follows nobody.
- What "last election" is while one runs.
- Which causes a member whose leader is appointed can have. That path holds no
  election on a term above its own, and its leader stays the leader below a quorum.

## Decision

**Two gauges, registered by the module as its other gauges are.** Client counters,
one of each for a member, keyed by the member id, added at the module's start and
stored each duty cycle when the value has moved. Ids 140 and 141.

**`cluster.leader_member_id` is −1 in the five states in which a member has no
leader, on both paths:** init, canvass, nominate, candidate ballot and follower
ballot. From the state after them it holds the leader's id: the member's own from
leader log replication, and the sender's from the new leadership term a member
takes. It is −1 again from the init of the member's next election, and in the
canvass a member goes back to when a follower log replication has ended: that
canvass is no new election, the member has no leader until the next new leadership
term, and the cause stays as it was. So a member
whose leader is appointed reads −1 until it follows or leads, though its core has
known the id all along: the gauge says who the member follows, and a reader that
finds −1 knows the member has no leader, on either path.

**`cluster.election_cause` is set where an election begins at init, and holds until
the next one begins.** While an election runs it is that election's cause; once it
has closed it is still that election's. The codes are ADR 0077's, each set at one
place in the core:

| Code | The election began because | Where |
| --- | --- | --- |
| 0 | the member started | the core's construction |
| 1 | it followed, and its leader said nothing for the leader heartbeat timeout | a follower's duty cycle, election closed |
| 2 | it followed, and its image of the leader's log was no longer available | the same |
| 3 | it led, and its active members fell below a quorum | a leader's duty cycle, election closed |
| 4 | its election had closed, and a message carried a term above its own | reading that message |
| 5 | its election before failed | wherever an election fails |

A leader that loses its quorum before its election has closed fails that election,
so its next has cause 5, not 3: 3 is a leader that had closed.

**A member whose leader is appointed has causes 0, 1, 2 and 5.** Its leader below a
quorum stays the leader and begins no election, and it reads no term above its
own. The tests pin both: the cause stays 0 and the member still names its leader.

**The core answers both.** `Consensus::leader_gauge` and
`Consensus::election_cause`, with `ElectionCause` and its codes. The module stores
what they return and decides nothing.

## Alternatives considered

- **Store `leader_member_id()` as it is.** Lost. On the appointed path every member
  would name the lowest id from its first duty cycle, a leader that may not be
  running. `cavalry doctor` could not tell "follows member 0" from "has no leader".
- **Keep the first cause through an election that fails and begins again.** Lost.
  ADR 0077 gave "its election before failed" a code of its own, and the counters
  beside it, `cluster.leader_heartbeat_timeouts` and the rest, keep how often each
  cause happened. The gauge answers one question: why this election.
- **Leave the gauges to the change that moves the module**, as ADR 0081 listed
  them. Lost in ADR 0100.
- **A cause for a quorum lost before the election closed.** Lost. That is a failed
  election, which has its counter and its code.

## Evidence

- `crates/cavalry-raft/tests/election_gauges.rs`, seven tests on one core driven
  by hand.
  - A member that started reads no leader and cause 0, in init and in canvass, on
    both paths; the appointed core's `leader_member_id` is 0 while its gauge is −1.
  - A follower names its leader from the new leadership term it takes, in each of
    follower replay, follower log init, follower log await and follower ready, and
    closed, and none from the init of its next election. That
    election's cause is 1, and still 1 after it has closed under the leader's next
    term. Both paths.
  - A follower whose image is gone has cause 2, with `cluster.log_images_lost` at 1
    and no heartbeat timeout counted. Both paths.
  - A candidate names no leader in nominate or in its ballot, and itself from the
    cycle it leads in. A voter names none in follower ballot, and the candidate
    once it takes its new leadership term.
  - On the appointed path a follower behind its leader's log fails its election,
    and the next has cause 5.
  - A member that replicates a term it missed names its leader in follower log
    replication, and none in the canvass it goes back to when the replication has
    ended, with the cause and the count of its elections as they were.
- `crates/cavalry-raft/tests/step_down.rs`, assertions in the tests that make each
  election: a closed leader that reads a term above its own, and a follower that
  does, have cause 4 and name no leader from that call, the leader by a new
  leadership term, a request vote or a commit position; a leader whose election
  fails has cause 5; a closed leader that loses its quorum has cause 3, and one in
  leader log replication has 5. A member whose leader is appointed reads a higher
  term, and an appointed leader loses its quorum: the cause stays 0, and each still
  names the leader.
- `crates/cavalry-raft/tests/cluster.rs`, over three drivers, read from each
  driver's counters file. Before a duty cycle: −1 and 0 on every member. With
  every election closed: 0, the leader's id, on all three, the leader among them,
  and cause 0. With the leader lost: each follower reads −1, and cause 2 or 1 by
  which loss its duty cycle met first, the one its counters counted.
- Each rule taken out fails a test. With the gauge holding the core's leader id in
  every state, five of the seven tests on one core fail, the member that started
  among them. With the store of cause 3, 4 or 5 taken out, the four tests of
  `step_down.rs` that make those elections fail.
- Not shown: `cavalry doctor` reading them, which is the next change.

## Consequences

- A reader of one member's counters file knows who that member follows and why it
  last held an election, without reading another member's file.
- `counters.toml` has 141 entries. Id 139 is another change's and is not in this
  one.
- A duty cycle stores two more values when they have moved, which is at an
  election. Nothing is added once an entry.
- The appointed path's two rules, that it has no cause 3 or 4, leave with the path
  (ADR 0100, change 5); the tests that pin them go then.
- Revisit if `cavalry doctor` needs the cause of an election before the last one:
  the counters hold how often, not in which order.

## References

- `spec/cluster.md`, "Counters".
- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  "Two gauges tell an operator who leads and why the last election began".
- [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  change 1.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the two paths.
