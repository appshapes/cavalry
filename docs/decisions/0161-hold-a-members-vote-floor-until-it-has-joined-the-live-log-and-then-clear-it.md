# 0161. Hold a member's vote floor until it has joined the live log, and then clear it

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 3
- **Supersedes:** ADR 0145 on two points: what ends a vote floor, "once the member's
  append position reaches its floor ... it votes as any voter does", with "a floor is
  never lowered"; and "a message whose log position is 0 ... gives no floor". The rest
  of ADR 0145 stands, the floor's store before the first term and its slot among it.

## Context

ADR 0145 lets a member that lost its log directory come back on a new one. With its log
empty, it takes a leader only in two cases, and it stores that leader's log position,
from the new leadership term it took the leader on, as its vote floor. It withholds its
vote and its candidacy while its append position is below the floor. The floor is
past every entry the cluster committed, so once the member's log reaches it, the log
holds every entry the member could have acknowledged before it came back.

That rests on the new leadership term being written after the member's start. ADR
0145's Consequences name the one frame that is not. A peer writes consensus messages
to the member on a publication of its own. A frame the leader wrote before the
member's start, and that its sender had not yet sent because the member's image before
was a receiver window behind, is sent to the member's new image. If that frame is a new
leadership term written before the leader appended an entry the member acknowledged,
the member's floor lies below that entry. Once its log reaches the floor, it votes. If
the leader then stops before the member holds the entry, a member that lags behind the
entry can be voted in without it, and a committed entry is lost (work item 411).

ADR 0145 weighed three ways to close it: an echo of a value of the member's run in two
messages, a floor that rises when the member's live image joins, and a sender that
drops what it held. It left the case open, with the echo first. The echo adds a field to
canvass position and to new leadership term, so it moves `spec/cluster.md` to a new
major, which version 2 of the page, only days old, would then have to cross with the
upgrade gate.

## Decision

**The floor is a mark, held until the member has joined the live log.** A member whose
log is empty that takes a leader stores a vote floor, as ADR 0145 says, once and before
it records a term, and now whatever the message's log position, 0 included. It
withholds for as long as it holds a floor. It clears the floor once, in one run of its
module, it has joined a leader's live log, going to follower ready (state 16), and
votes as any voter does from then on. It clears it by writing the slot's eight bytes
and its checksum as zeros and syncing the file, counted in `cluster.vote_floors_cleared`
(counter 200), and `cluster.vote_floor` reads −1 once it has. The floor's position no
longer decides anything: it says where the leader stood when the member took it.

**Why the join holds every committed entry the member could have acknowledged.** Three
facts, each one a page already states:

- The leader the member joins holds every entry the cluster committed. The member took
  it by the rules of "Taking a leader" (ADR 0145), whose second case keeps out a leader
  that a later term deposed, or a later election gave it a leader by the vote rule. A
  deposed leader's live log would give the member nothing, and those rules are what
  keep it out.
- That leader's live log reaches a member started on a new driver in a new image, made
  at the sender's position (`spec/udp.md`, "The receiver", steps 1 and 2), which ADR
  0145's second fact already rests on. The member acknowledged only what it was sent,
  and a sender's position never goes back, so the image joins past every entry the
  leader sent the member before it came back. A later leader's term begins past every
  such entry the cluster committed, since it holds them by the vote rule.
- A follower goes to follower ready only with its append position at its live image's
  join position: state 15 checks it, and state 13 goes on until the member's log reaches
  the live recording's start (`spec/cluster.md`, "Roles and the election").

So when the member clears its floor, its log holds every committed entry it could have
acknowledged, whatever message gave it the floor, a held-back one among them, and
whatever position the floor holds. The comparison with the floor's position that ADR
0145 made is then needed by nothing, and it goes: a floor that a later leader's cut
left above the cluster's log no longer holds the member out until the log grows past
it, and a held-back message at position 0, which gave no floor under ADR 0145, now gives
one.

**Which held-back frames reach a member at all.** A member whose log is empty reads a
new leadership term only when its log leadership term id is −1: the leader wrote it in
answer to a report of an empty log. Any other is for a log term not the member's, and
the member canvasses on it. So the frame of work item 411 is one the leader wrote while
the member had lost its log once before, and held back while the member followed on an
earlier frame and acknowledged entries; the member then lost its log again.

**Why a second write is safe here.** ADR 0145 wrote the floor once because a torn write
reads as no floor, and a member with a log and no floor would vote. A torn clear also
reads as no floor, which is exactly what the clear was to leave, at a moment when the
member's log already holds what the floor guarded. The store is unchanged: torn, it
leaves a member that recorded no term yet, which finds its log empty at its next start.

**Why neither page's version moves.** Two pages change in their text and in no layout.
`spec/cluster.md` changes rules of version 2: what ends a floor, a floor at position 0,
and a counter; no message, entry, template or enumeration changes, so a member of this
build and one of the build before run together in one cluster, each by its own rule.
`spec/durable-log.md` changes the writes of version 6's terms file: the slot is written
once with the floor and once with zeros. A slot of zeros is a state every reader of
version 6 already has, as a creation leaves it, and reads as no floor, so a directory
either build wrote opens under the other.

**A restart before the clear.** The member reads its floor at its start, as before, and
withholds after it until it joins the live log again in that run, whatever its append
position. `cavalry doctor` names such a member at or past its floor, saying that it
waits for the live log and not for the position, and `cluster.votes_withheld`'s text
says so too.

**Two pure functions and their vectors.** `withholds(own, vote_floor, log_heard)` in
`cavalry-proto` withholds for any floor held, and `clears_vote_floor(vote_floor, joined)`
says when a floor is cleared. The `withholds` vectors and the new `clears-vote-floor`
vectors of `spec/vectors/cluster.json`, made by `spec/vectors/cluster.py`, pin them.

## Alternatives considered

- **The echo of ADR 0145's Alternatives.** It closes the case on no property of the
  transport. But it adds a field to two layouts, so `spec/cluster.md` would go to
  version 3, a member on a new log directory would take no leader until every member
  ran it, and the upgrade gate would have to hold a cluster across the two majors. The
  live join closes the same case within version 2, on a property ADR 0145 already
  relies on. The echo stays the way if a later major is made for another reason.
- **Clear at the join, and keep the comparison with the floor's position too.** The
  join already holds every committed entry the member could have acknowledged, so the
  comparison would add nothing but its cost: a member held out while a floor that a
  later leader's cut left above the cluster's log waits for the log to grow, and no
  floor at all from a message at position 0, the held-back case with the floor at
  nothing, which a reading of this design before it landed found.
- **A floor that rises to the live image's join position.** The floor would have to
  be written twice with a value, and a torn raise reads as no floor, so the member would
  vote with a log that may lack the entry. A second slot to keep the value before does
  not fit the 20 bytes the header has left, ADR 0145 found. Clearing writes the one
  value whose torn form is harmless.
- **Take the floor at the live join, and store nothing before it.** The member catches
  up from the leader's recordings before it joins the live log (state 13), so its log
  grows before the join, and a member that crashed then would have a log and no floor.
- **A sender that drops what it held when its receiver's image goes.** It changes the
  UDP transport for every stream, and a stream that is not the cluster's may need what
  was held.
- **Withhold until a live join in every run, floor or not.** A member that holds its
  whole log, as after any stop, would then vote in no election that follows its
  leader's loss until another leader let it join. A cluster that lost its leader and
  had a member start in the meantime could not elect.

## Evidence

All runs on an Apple M-series laptop, macOS 26, in the in-memory cluster of
`cavalry-raft` unless named.

- **The held-back frame, in memory**,
  `a_held_back_new_leadership_term_gives_a_member_that_lost_its_log_no_vote_until_it_joins_the_live_log`
  in `crates/cavalry-raft/tests/lost_log_directory.rs`, under the check of every step
  that what a leader committed is never lost and never replaced.
  - A member loses its log, and the leader's answer to its empty log is held back. It
    follows on a later answer and acknowledges entries the leader commits with it,
    while the third member has stopped hearing the leader.
  - It loses its log again. The held frame reaches its new run, and it takes the
    leader with a floor below those entries. The leader stops once the member's
    catch-up has passed the floor.
  - Under `master`'s rule, with `withholds` below the floor, no clear and no floor at
    position 0, the check fails: "member 2 holds, below the commit position it knows,
    bytes that were not committed". The member voted, and a leader without the entries
    was elected.
  - Under this decision's rule the test passes. No leader is elected until the leader
    comes back; then the member joins, clears its floor once, and every member holds
    the entries.
- **What the test found on the way.** A member whose log is empty reads a held-back new
  leadership term only when it answered a report of an empty log (Decision). And the
  in-memory cluster brought a replay's whole span in one step, so a member's catch-up
  never rested between a floor and the entries above it. The test sets the new replay
  rate to one entry a step, as a replay over a driver brings its frames over time.
- **The rule's other holders.**
  - The `withholds` and `clears-vote-floor` vectors: 341 vectors in all, which the
    generator and the JSON agree on.
  - `a_vote_floor_is_cleared_to_zeros_and_a_torn_clear_reads_as_none` in the terms file.
  - `doctor`'s floor test, for a floor held below its position, at it, and cleared.
  - Five tests of `lost_log_directory.rs` that held the floor's position after a member
    caught up now hold the clear: the floor −1, and `vote_floors_cleared` 1.
- **The searches' regression seeds, proven again**, since a new cluster's followers now
  hold a floor of 0 until they join, which moves every search's paths. Each was run
  from a release binary built with its rule taken out, the source restored at once.
  - `lost_log_search`, with the term condition of ADR 0145's one voting case taken out
    (`votes_in_term` always true): seeds 779, 1573 and 2983 find two members leading one
    term, `[4, 4]`, `[2, 2]` and `[4, 4]`; seed 1 passes.
  - `faulted_truncation_search`, with ADR 0148's rule taken out: seeds 13 and 96 fail at
    40,437 ms and 53,671 ms, as in ADR 0148's Evidence.
- **Wide runs on this decision's rule**, release builds:
  - `lost_log_search` seeds 1 to 500 pass, in 305 s;
  - `faulted_truncation_search` seeds 1 to 300 pass, in 174 s;
  - `timer_search` seeds 1 to 200 pass, in 707 s, with one seed of 200 that ends with no
    settled quorum, seed 23. On `master` too, seed 23 has none, and seeds 44 and 107
    settle, so the floor's change alters nothing there (work item 420).
- **Gates:** GATES_PLACEHOLDER

## Consequences

- The case ADR 0145 left open is held: a held-back new leadership term can still give
  a floor below an entry the member acknowledged, but the member votes only after it
  has joined the live log, which reaches it at or past that entry.
- A member that lost its log withholds until it is on the live log, not until its log
  reaches a position. While its leader runs, that is the same catch-up, since a
  follower joins the live log once it has caught up. A member that caught up and was
  stopped before it joined withholds after its next start until it joins. A floor that
  a later leader's cut left above the cluster's log no longer holds a member out.
- A new cluster's followers take their first leader with their logs empty, so each
  stores a floor of 0 and withholds until it joins that leader's live log, which it
  does in the same election. If the first leader stops before a follower has joined,
  that follower withholds as a member that never caught up does, which ADR 0145's
  Consequences already count as a new cluster's cost.
- A floor that a build of ADR 0145 stored, which that build never clears, counts again
  under this one. A member that lost its log while it ran such a build, long since
  caught up, withholds after its first start under this build until it joins a live
  log, which it does in its first cycles while a leader runs, and which clears the
  floor for good. If the cluster has no leader then, and the members that could elect
  one hold such floors, they withhold until a member without a floor runs, or until
  ADR 0145's one case lets them vote. That costs liveness, never a committed entry,
  and it ends at each such member's first join. Two other ways were weighed:
  - Clearing at a member's start a floor its log is already past, as ADR 0145's rule
    would have let it vote, reopens the case this decision closes, for a member started
    again between its floor and its join.
  - Marking which build wrote a floor needs a new field in the terms file, and so a
    new version of `spec/durable-log.md`.

  `cavalry doctor` tells the two builds apart. A member of the build before registers
  no `cluster.vote_floors_cleared`, and doctor names it only while its log is below
  its floor, as that build's rule has it. It names a member of this build for as long
  as it holds a floor.
- ADR 0145's one case in which a withholding member votes, every voter heard and the
  candidate at least each, is unchanged, so a cluster in which two members lost their
  logs and the third holds the whole log still elects.
- The terms file's slot is written at most twice now, once with the floor and once with
  zeros, and the module writes nothing else there.
- The in-memory cluster gains a frame held back, `Cluster::hold_back_new_leadership_term`
  and `Cluster::release_held_back`, bounded at eight, and a replay rate,
  `Cluster::set_replay_rate`, unlimited unless a test lowers it, so that a test can stop
  a leader while a member's catch-up is part way through a span, as a replay over a
  driver brings its frames over time. Neither changes what the searches draw.
- Revisit when the consensus stream or the live log moves to another transport, or a
  subscription can join an image below its rebuild position, since the clear rests on
  where the live image joins (`spec/udp.md`, "The receiver"); and when a later major of
  `spec/cluster.md` carries an echo of the member's run, which would let the floor rest
  on no property of the transport.

## References

- Work item 411, which carried this decision; work item 286 and ADR 0145, the vote
  floor; work item 458, the in-memory cluster's reading of a member in init.
- `crates/cavalry-raft/tests/lost_log_directory.rs`, the held-back frame's test;
  `crates/cavalry-log/src/terms.rs`, the clear's test; `crates/cavalry-control/src/doctor.rs`,
  the finding's test.
- `spec/cluster.md`, "A member that lost its log" and "Roles and the election", states
  13 and 15; `spec/durable-log.md`, "Terms"; `spec/udp.md`, "The receiver".
