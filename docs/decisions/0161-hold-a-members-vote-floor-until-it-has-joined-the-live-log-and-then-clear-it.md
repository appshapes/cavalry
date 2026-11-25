# 0161. Hold a member's vote floor until it has joined the live log, and then clear it

- **Status:** Accepted
- **Date:** 2026-11-25
- **Invariants affected:** 1, 3
- **Supersedes:** ADR 0145 on three points:
  - What ends a vote floor: "a member whose append position has reached its floor ...
    votes as any voter does from then on", with "The floor is never lowered". The
    member now also has to join the live log, and then clears the floor.
  - The slot "written once and synced": it is now written once with each floor and
    once with zeros after it.
  - "A message whose log position is 0, a new cluster's first leader's, gives no
    floor": it now gives a floor of 0.

  The rest of ADR 0145 stands: the floor's store before the first term, its slot's
  place and checksum, and the comparison of the member's log with the floor, which
  stays as one of two conditions.

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

**The floor is a mark, held until the member has caught up and joined the live log.**
A member whose log is empty that takes a leader stores a vote floor, as ADR 0145 says,
once and before it records a term, and now whatever the message's log position, 0
included. It withholds for as long as it holds a floor. It clears the floor once two
things hold, since it stored the floor and in this run of its module:

- it has joined a leader's live log, going to follower ready (state 16);
- its append position is at or past the floor, as ADR 0145 asked.

It clears it by writing the slot's eight bytes and its checksum as zeros and syncing the
file, counted in `cluster.vote_floors_cleared` (counter 200), and `cluster.vote_floor`
reads −1 once it has. It votes as any voter does from then on.

**Which condition holds which entries.** The join holds what the leader's live sender
had sent the member. The floor from a new leadership term written after the member's
start holds what the leader had appended. Each holds a case the other does not, and
follower catch-up's commit target, which ADR 0145 did not need to name, holds what the
leader had committed and told the member.

- The leader the member joins holds every entry the cluster committed. The member took
  it by the rules of "Taking a leader" (ADR 0145), whose second case keeps out a leader
  that a later term deposed, or a later election gave it a leader by the vote rule.
- **The join.** That leader's live log reaches a member started on a new driver in a
  new image, made at the sender's position (`spec/udp.md`, "The receiver", steps 1 and
  2), which ADR 0145's second fact already rests on. A follower goes to follower ready
  only with its append position at or past its live image's join position: state 15
  checks it, and state 13 goes on until the member's log reaches the live recording's
  start. A sender's position never goes back, so the join holds every entry the sender
  sent the member before it came back. A later leader's term begins past every such entry the
  cluster committed, since it holds them by the vote rule. This is what holds the case
  of work item 411: a new leadership term the sender held back from the member's image
  before its start gives a floor below an entry the sender had since sent it.
- **The floor.** It is the leader's append position when it wrote the new leadership
  term the member took. For a message written after the member's start, that is past
  every entry of the leader's log the member acknowledged before it came back, one it
  had from a replay of the leader's recording among them. That entry is the one the
  join does not hold. The recorder is a spy, which reads the publication's log on the
  leader's own driver (`spec/udp.md`, "The channel URI"), so it may hold an entry the
  sender has not yet sent, and the member's live image may then join below it. If the
  member's acknowledgement from its earlier run reaches the leader only after the
  member came back, which work item 463 is to establish or rule out, the leader commits
  the entry with it: the leader keeps a member's latest report with no check of which
  run sent it. A member cleared by the join alone would then vote without the entry.
  The floor keeps it withholding until its log holds the entry.
- **Catch-up's commit target.** A member had a replay in its earlier run only from a
  leader whose log was above position 0 when it wrote the message the member followed,
  and a held-back message the member reads now was written after that one, so it is
  above 0 too. A member that comes back on it with its log empty is below the leader's
  log and catches up again, and state 13 is not left before its append position
  reaches the commit position it was told when it read the leader's stop catch-up.
  That holds an entry from a replay before the sender sent it, on a held-back
  message's floor, when the leader committed it and told the member so before then.
- **None of the three.** A held-back message's floor, an entry the member had from a
  replay before the sender sent it, and its acknowledgement counted so late that the
  entry was not yet committed in the commit position the member was told when it read
  stop catch-up: in practice, counted after the member came back. Work item 463 holds
  that case, by having the leader count a report from the member's present run only.

**Which held-back frames reach a member at all.** A member whose log is empty reads a
new leadership term only when its log leadership term id is −1: the leader wrote it in
answer to a report of an empty log. Any other is for a log term not the member's, and
the member canvasses on it. So the frame of work item 411 is one the leader wrote while
the member had lost its log once before, and held back while the member followed on an
earlier frame and acknowledged entries; the member then lost its log again.

**A floor of 0.** ADR 0145 stored no floor for a message at position 0. A held-back
message at position 0 would then leave the member with no floor at all, though it
acknowledged entries after the leader wrote the message. So a floor of 0 is stored:
the mark is the slot, and the join does the rest.

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
once with each floor and once with zeros after it. A slot of zeros is a state every
reader of version 6 already has, as a creation leaves it, and reads as no floor, so a
directory either build wrote opens under the other.

**A restart before the clear.** The member reads its floor at its start, as before, and
withholds after it until it joins the live log again in that run, even with its
append position at or past the floor. `cavalry doctor` names such a member, saying
that it waits for the live log too and not for the position alone, and
`cluster.votes_withheld`'s text says so too.

**Two pure functions and their vectors.** `withholds(own, vote_floor, log_heard)` in
`cavalry-proto` withholds for any floor held, and `clears_vote_floor(vote_floor,
joined, append_position)` says when a floor is cleared. The `withholds` vectors and the
new `clears-vote-floor` vectors of `spec/vectors/cluster.json`, made by
`spec/vectors/cluster.py`, pin them.

## Alternatives considered

- **Clear at the join alone, with no comparison.** The first version of this decision,
  which a reading of the design before it landed asked for: it would drop ADR 0145's
  cost of a floor that a later leader's cut left above the cluster's log, which holds
  the member out until the log grows past it. A second reading found the entry the
  member had from a replay before the sender sent it, which the join does not hold and
  the floor does (Decision). The cost stays.
- **The echo of ADR 0145's Alternatives.** It closes the case on no property of the
  transport. But it adds a field to two layouts, so `spec/cluster.md` would go to
  version 3, a member on a new log directory would take no leader until every member
  ran it, and the upgrade gate would have to hold a cluster across the two majors. The
  live join closes the case of work item 411 within version 2, on a property ADR 0145
  already relies on. The echo stays the way if a later major is made for another
  reason.
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
- **Let a member that withholds stand in ADR 0145's one voting case**, every voter
  heard and its own log at least each. A cluster in which every member withholds, as
  one started all at once on this build with floors of the build before, would then
  elect with no operator's step, by the argument of that case. It changes ADR 0145's
  "it never seeks leadership", and so the nominate state, the vectors and the
  searches, and it would not help a new cluster whose first leader is lost for good,
  since that leader never reports. It is left for a later decision.

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
    the entries. It passed with the join alone too, the first version of this
    decision: the entries reached the member from the leader's sender.
- **The floor's own case has no test yet.** An entry the member had from a replay
  before the sender sent it, with its acknowledgement counted after the member came
  back, needs a recorder ahead of the sender and an acknowledgement read after its
  run ended; work item 463's "Done when" asks for that test.
- **What the test found on the way.** A member whose log is empty reads a held-back new
  leadership term only when it answered a report of an empty log (Decision). And the
  in-memory cluster brought a replay's whole span in one step, so a member's catch-up
  never rested between a floor and the entries above it. The test sets the new replay
  rate to one entry a step, as a replay over a driver brings its frames over time.
- **The rule's other holders.**
  - The `withholds` and `clears-vote-floor` vectors, the latter with the member below
    its floor once joined: 343 vectors in all, which the generator and the JSON agree
    on.
  - `a_vote_floor_is_cleared_to_zeros_and_a_torn_clear_reads_as_none` in the terms file.
  - `doctor`'s floor test, for a floor held below its position, at it, and cleared,
    and a member of the build before at its floor, which it does not name.
  - Five tests of `lost_log_directory.rs` that held the floor's position after a member
    caught up now hold the clear: the floor −1, and `vote_floors_cleared` 1.
- **The searches' regression seeds, proven again**, since a new cluster's followers now
  hold a floor of 0 until they join, which moves every search's paths. Each was run
  from a release binary built with its rule taken out, the source restored at once,
  under the rule as it lands and under the join alone before it, with the same
  results.
  - `lost_log_search`, with the term condition of ADR 0145's one voting case taken out
    (`votes_in_term` always true): seeds 779, 1573 and 2983 find two members leading one
    term, `[4, 4]`, `[2, 2]` and `[4, 4]`; seed 1 passes.
  - `faulted_truncation_search`, with ADR 0148's rule taken out: seeds 13 and 96 fail at
    40,437 ms and 53,671 ms, as in ADR 0148's Evidence.
- **Wide runs on this decision's rule**, release builds, as it lands:
  - `lost_log_search` seeds 1 to 500 pass, in 304 s;
  - `faulted_truncation_search` seeds 1 to 300 pass, in 174 s;
  - `timer_search` seeds 1 to 200 pass, in 706 s, with one seed of 200 that ends with no
    settled quorum, seed 23. On `master` too, seed 23 has none, and seeds 44 and 107
    settle, so the floor's change alters nothing there (work item 420).
  - Under the join alone the three gave the same results, the timer search's totals
    the same to the event.
- **Gates** at the head merged with `master` (commit 770648f6): `just lint`, `just
  docs`, `just lint-docs` over `origin/master...HEAD`, the Linux check, `just test` in
  618 s, `just conformance` in 120 s and `just replay-check` in 21 s, each exit 0.
- **Twenty runs on both CI runners** at commit e3852142, the rule as it lands:
  `cavalry-raft --test cluster` and `cavalry-raft --lib module::tests`, 20 of 20 on
  Linux and on macOS each.

## Consequences

- The case ADR 0145 left open is held for every entry the leader's sender had sent the
  member: a held-back new leadership term can still give a floor below such an entry,
  but the member votes only after it has joined the live log, which reaches it at or
  past that entry.
- One case is left, the Decision's last point, and work item 463 holds it.
- A member that lost its log withholds until it is on the live log and its log is at
  or past its floor. While its leader runs, that is the same catch-up, since a follower
  joins the live log once it has caught up. A member that caught up and was stopped
  before it joined withholds after its next start until it joins. ADR 0145's cost
  stays: a floor that a later leader's cut left above the cluster's log holds the
  member out until the log grows past it.
- A new cluster's followers take their first leader with their logs empty, so each
  stores a floor of 0 and withholds until it joins that leader's live log, which it
  does in the same election. If that leader is lost for good before they join, no
  follower can stand, since a member that withholds never stands, and the cluster
  elects no leader. That is the price of the floor of 0, not an oversight. No start can
  tell those followers from a member that lost its log beside a member that never had
  one, and there electing would lose what the lost leader holds. The way out is the
  operator's, in `docs/how-to/run-a-cluster.md` and `cluster.vote_floor`'s doctor
  text. First every stopped voter is started on the log directory it kept. Then, when
  every voter but the lost leader runs at position 0, none holds an entry, and the
  cluster is started again as a new cluster, which gives up whatever the lost leader
  held.
- A floor that a build of ADR 0145 stored, which that build never clears, counts again
  under this one. A member that lost its log while it ran such a build, long since
  caught up, withholds after its first start under this build until it joins a live
  log, which it does in its first cycles while a leader runs, and which clears the
  floor for good. An upgrade one member at a time
  (`docs/how-to/upgrade-a-cluster.md`) waits for each member's election to close, and
  so for that join. A cluster stopped whole and started on this build at once, with
  such a floor on every member a quorum could elect, elects no leader, since a member
  that withholds never stands. No build has been released, so the way out is written
  down and not built. With every voter running, start on the build before the member
  whose log is at least every other's. It votes and stands once its log is at or past
  its floor, and the others vote for it by ADR 0145's one case. Upgrade it again once
  they have joined its live log. Two other ways were weighed:
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
- The terms file's slot is written once with each floor the member stores and once with
  zeros after it, and the module writes nothing else there. A member can store a floor
  again after it cleared one, when a later leader's term empties its log.
- The in-memory cluster gains a frame held back, `Cluster::hold_back_new_leadership_term`
  and `Cluster::release_held_back`, bounded at eight, and a replay rate,
  `Cluster::set_replay_rate`, unlimited unless a test lowers it, so that a test can stop
  a leader while a member's catch-up is part way through a span, as a replay over a
  driver brings its frames over time. Neither changes what the searches draw.
- Revisit when work item 463 lands. Once the leader counts a report from the member's
  present run only, an entry the member had from a replay was committed before the
  member came back, if at all. State 13 then keeps the member until its log reaches
  the commit position it was told, and the floor's comparison may be weighed again
  against its cost. Revisit too when the consensus stream or the live log moves to
  another transport, or a subscription can join an image below its rebuild position,
  since the clear rests on where the live image joins (`spec/udp.md`, "The receiver");
  and when a later major of `spec/cluster.md` carries an echo of the member's run,
  which would let the floor rest on no property of the transport.

## References

- Work item 411, which carried this decision; work item 286 and ADR 0145, the vote
  floor; work item 458, the in-memory cluster's reading of a member in init; work item
  463, a report from a member's present run only, the case this decision leaves.
- `crates/cavalry-raft/tests/lost_log_directory.rs`, the held-back frame's test;
  `crates/cavalry-log/src/terms.rs`, the clear's test; `crates/cavalry-control/src/doctor.rs`,
  the finding's test.
- `spec/cluster.md`, "A member that lost its log" and "Roles and the election", states
  13 and 15; `spec/durable-log.md`, "Terms"; `spec/udp.md`, "The receiver" and "The
  channel URI".
