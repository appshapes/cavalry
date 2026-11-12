# 0105. Lose a ballot at its first vote against

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 3, 4, 6
- **Supersedes:** none. Amends ADR 0077 in one rule: when a candidate that cannot
  win goes back to canvass. Changes `spec/cluster.md`: the row of state 3 in "Roles
  and the election", "How long an election takes", one sentence of "Messages during
  an election", the rows of `cluster.ballots_lost` and of the election timeout, and
  the `ballot` vectors, which say whether the ballot is lost: twelve gain the
  field, four are new. No message's layout changes, and the page stays at version
  0.

## Context

ADR 0077 took Aeron's ballot. A candidate leads its term at once when a quorum
has voted for it and every member it has heard is among them, or at the election
timeout, 1 s, with a quorum for it and no vote against. With no such ballot it
goes back to canvass at the election timeout.

**A vote against decides the ballot when it is read.** The rule counts no leader
with a vote against the candidate's term, at once or at the timeout. So a
candidate that has read one waits out the rest of the second for nothing, and so
does the member that voted for it. The same is true of a candidate whose term rose
during its ballot, on a request vote from a log behind its own: it holds no vote
of its own for its term and can lead nothing in it.

**The wait is what a split vote costs, and a split vote is not rare.** Two
members whose nomination delays end close together each store a term, vote for
themselves and ask for votes, and each answers the other against. ADR 0077
reckoned the window as a round trip and wrote "one election in some hundreds".
Over drivers the window holds the candidate's synced store of its term before it
asks. On a laptop, macOS, with the consensus module made to elect in a scratch
branch, three drivers on the loopback:

- 64 runs of a test that stops the leader. A second ballot was taken in 2 of the
  64 elections after the loss, 3%, and in 3 of the 64 first elections at a start,
  5%. The store took 3.8 ms at the median, and two delays drawn up to 500 ms end
  within 6 ms of each other in 2.4% of elections.
- The two elections after a loss that were split closed 1.66 s and 1.68 s after
  the leader heartbeat timeout. The 30 of the last batch that were not closed in
  142 ms at the least, 327 ms at the median and 548 ms at the most.

ADR 0077's Revisit has this case: "when the loopback tests' reported election times
are above a second in more than one run in a hundred, for the split vote". It has
fired, before any cluster elects on `master`.

## Decision

**A candidate that can no longer win goes back to canvass on its next duty
cycle.** That is a candidate that has recorded a vote against its current
candidate term, and a candidate that holds no vote of its own for its candidate
term. It counts the ballot in `cluster.ballots_lost` and canvasses as a follower,
with the reports and the members heard that it has, as it did at the election
timeout. `cavalry_proto::cluster::ballot` says the first of the two in a third
answer, `lost`: it is given the votes and not which member is the candidate, so the
core checks the candidate's own vote itself, and the `ballot` vectors hold no such
case.

**The election timeout stays for what it still decides.** A candidate with a
quorum and no vote against waits it for a member it has heard that has not voted.
A candidate short of a quorum with no vote against waits it and then canvasses. A
member in follower ballot waits it for its candidate's new leadership term.

**Nothing else of the ballot changes.** Who can win, the one vote a member casts
in a term, and the terms stored before a vote or a request are as ADR 0077 has
them. The step is the one a candidate took at the timeout, taken earlier: it
sends nothing and stores nothing. The next candidacy stores candidate term + 1
before it asks, as every candidacy does. The member that voted for a candidate
that lost stays in follower ballot, and votes for a request of a higher term from
there, as it did.

## Alternatives considered

- **Leave the ballot as Aeron has it.** Lost. Three elections in a hundred take a
  second longer than they need, and the card's promise is a leader in under a
  second after the heartbeat timeout.
- **Let a quorum lead over a vote against.** Lost. It changes who can lead: a
  vote against can come from a member whose log is ahead, and ADR 0077's rule and
  its argument for safety are Aeron's, with a vote against as a veto.
- **A shorter election timeout.** Lost. The same value bounds the wait for a slow
  voter and a voter's wait for its candidate, where a shorter one fails ballots
  that would have closed.
- **Ask for the vote before the term is stored.** Lost. The store comes first so
  that a member never votes twice in a term across a restart (ADR 0077).
- **Longer nomination delays.** Lost. Fewer delays would collide, and every
  election would wait longer for its first candidate.
- **Tell the member in follower ballot that its candidate lost.** Lost. It needs a
  message the page does not have, and that member already votes again on the next
  request for a higher term.

## Evidence

- `spec/vectors/cluster.json`, the `ballot` vectors: each of the twelve says
  `lost`, true for the two with a vote against the candidate's term; four are
  new, a ballot lost at the first vote against before the others have voted, one
  lost with a quorum for it, a vote against an older term of the candidate's that
  loses nothing, and a candidate short of a quorum with no vote against that
  waits. `cluster.py` now checks every `ballot` vector's fields against its
  reference rule. `crates/cavalry-proto/tests/cluster_conformance.rs` computes
  each.
- `crates/cavalry-raft/tests/ballot.rs`, one core through an `Io` that remembers
  what it was asked. A candidate with one vote for it stays in its ballot; the
  cycle after a vote against, it is in canvass, a follower, with one ballot lost
  and no term recorded, 4 ms into a ballot it used to leave at 1,000 ms. A
  candidate whose term rose is in canvass on its next cycle and has sent no
  request vote for the raised term; in a cluster of five, votes for the raised
  term from a quorum of the others do not make it the leader.
- `crates/cavalry-raft/tests/election.rs`, three members in memory at seed 12,
  where two delays end within a round trip: the election closed 132 ms after the
  cluster's start, and 1,129 ms before this change. The two candidates count one
  ballot lost each and the third member none; no member records term 0, and all
  three follow term 1. The check that no committed entry is lost runs after every
  step.
- `cargo test -p cavalry-raft` at the commit that made the change: 193 passed, the
  tests of every other seed and path unchanged.
- Not shown here: the rate over drivers after this change. No cluster elects over
  drivers on `master` until the consensus module moves (ADR 0100); the test that
  stops a leader comes with that change and prints each election's time.

## Consequences

Easier. A split vote costs the shorter of two new delays, at most 500 ms, and no
longer a second. A candidate that a member with a longer log answers against
makes way for that member at once.

Harder. A cluster in which no member can win, each answered against by a member
it cannot follow, now holds a ballot every delay and not every second: each is a
synced store of a term, and `cluster.ballots_lost` rises faster. `cavalry doctor`
reads that count as before.

To maintain. `ballot`'s third answer and its vectors; the two departures from
Aeron in the row of state 3, the members heard (ADR 0077) and this one.

Revisit when the test that stops a leader over drivers shows an election over a
second in more than one run in a hundred after this change.

## References

- [ADR 0077](0077-elect-the-leader-by-vote-and-close-a-ballot-on-the-members-heard.md),
  the ballot and its Revisit;
  [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  `cluster.ballots_lost`;
  [ADR 0100](0100-move-the-module-to-the-election-in-ordered-changes-around-one-that-takes-the-appointed-path-out.md),
  the module's move.
- `spec/cluster.md`, "Roles and the election", state 3, "How long an election
  takes" and "Messages during an election"; "Counters"; "Values"; "Vectors".
- Aeron's `Election.candidateBallot` and `ClusterMember.isQuorumLeader`:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
- Card 228.
