# 0086. Record a term when its replication begins, and check each replay's image against the position last sent

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Changes row 9 of `spec/cluster.md`, "Roles and the election",
  in four places: a member at the next term base log position records that term
  before it asks, where the row had it recorded at the stop; what an image's join
  position is checked against; what a term with nothing in it asks for; and what a
  member carried past its stop does. Adds the steps into and out of follower log
  replication to the row of `cluster.election_steps` in "Counters". The page's
  version stays 0: no message changes, and no member over a driver takes a vote yet
  (ADR 0081).

## Context

ADR 0081 listed the steps of the electing path. The fourth is state 9, follower log
replication: a member whose append position is below its leader's term base
replicates one term it lacks from the leader's recordings, to that term's end, and
canvasses again. ADR 0085 built the leader that answers. This is the member that
asks.

Row 9 of the page says what the member does. Building it found six things the row
did not settle, or settled in a way that does not hold.

**A term recorded at the stop cannot be carried on after a failure.** The row had
the member record the term it replicates when it reaches the stop, as Aeron does
(`Election.followerLogReplication` calls `updateRecordingLogForReplication` when the
replication is done). A member at the base of a term it lacks then holds bytes of
that term while its record does not name it. If the replication fails midway, its
log term is still the term before, and its append position is inside the term it was
replicating. It reports that. The leader names the next term in its record above the
reported one, which is the term the member was replicating, at a base below the
member's append position, and the member must truncate to it. But the member was
told its leader's commit position while it replicated, and that is above the base.
ADR 0083 refuses a truncation below a commit position the member knows. The member
fails its election, and fails the next one the same way, for as long as that leader
leads. Aeron has no such guard: `Election.onTruncateLogEntry` truncates whatever the
commit position is, so there the member cuts what it replicated and fetches it
again.

**The join check races with the bytes it guards.** The row said an image's join
position must be the member's append position. A member's append position is what
its recordings have synced. A cycle that first sees a new image may already count
that image's first bytes, and then no image joins "at the append position".

**The row does not say what a term with nothing in it asks for.** A leader's record
can hold a term whose base is also its end: a leader that was elected and appended
nothing a later leader holds. A member at that base is told a stop position equal
to its append position.

**The row does not say what a member carried past its stop does.** The stop is the
end of the term the member's record holds its log in. Bytes above it are of a term
the record does not name.

**The registry has no state 9.** `counters.toml` registers the election's machine
with the core's states, and `just lint-counters` and `tests/transitions.rs` hold the
two together (ADR 0067). The moves into and out of state 9 need rows.

**The in-memory cluster can hold bytes a member never reported.** ADR 0083 left a
note: a member in memory whose sync is paused holds what it received above its
append position, and a member over a driver has no such bytes, since its recording's
stop is where its log ends. A replay that joins at the append position would be
recorded over them.

## Decision

A member records a term it holds none of when it begins to replicate it. Each
replay's image is checked against the position the member last sent its leader.
Three timers and checks bound the state, each with a counter.

**The term is recorded at its base, before anything is asked for.** A member whose
append position is at the next term base log position holds none of the term it is
to replicate. It records that term at that base first. A member whose append
position is below that base replicates the rest of its own log term, which its
record holds already. So a member in state 9 always holds its log in a term its
record names, and one that fails midway reports that term. The leader then names
the term after it, whose base is above the member's append position, and the member
carries on from where it stopped: nothing is truncated and nothing refused.

The record is safe on the ground a join at a leader's base rests on. A member that
joins a leader at the leader's term base records the leader's term there before it
holds a byte of it ("Messages during an election"), because it holds the leader's
log to that base. A member at the base of the next term in its leader's record
holds its leader's log to that base too: it replicated to it, or truncated to it,
under that leader's own new leadership term.

**At the stop.** The term replicated is in the record. When the stop is the leader's
term base log position the member records the leader's term at that base, before it
reports the stop, and then waits until the commit position it was told has reached
its append position, as the row says. A member that already knew such a commit
position waits for nothing. Then the subscription goes and the member canvasses.

**An image joins where the member's log ends, by what the member sent.** The first
image must join at the position the member asked from. Each image after must join
at the append position the member last sent its leader: that report is what the
leader starts the next recording's replay on (ADR 0085). The member checks an image
before it reports what the image brought, in one duty cycle. `Io::catchup_image`
reports an image no later than `Io::append_position` counts its bytes.

**A term with nothing in it is recorded, and asks for nothing.** The member is at
its stop at once: it subscribes to nothing, sends no catch-up position, and
canvasses.

**A member above its stop fails its election**, counted in
`cluster.elections_failed`. It records no term there.

**Counted.** `cluster.log_replications` rises on the member when its catch-up
position has left, once for a replication. `cluster.log_replications_unavailable`,
id 136, rises when the append position stayed below the stop without moving for the
leader heartbeat timeout. An image that joins elsewhere is counted in
`cluster.join_position_mismatches`, as in state 15. The registry has `follower log
replication` with eight rows: the four steps into it, from canvass, nominate,
candidate ballot and follower ballot, and the step from it to canvass, each in
`cluster.election_steps`; and its failure, to init, with the three counters that can
rise there.

**Four operations of `Io`.** `add_catchup` subscribes on the member's `catchup`
endpoint with a recording armed. `send_catchup_position(leader, term, position)`
sends the message with the member's own endpoint, which the core does not hold (ADR
0085). `catchup_image` is the join position of the image now on the subscription.
`remove_catchup` removes the subscription. The consensus module refuses them as it
refuses the leader's (ADR 0085): its member is appointed.

**A member told to wait waits.** A new leadership term that leaves a member at the
next term's base with no end carried changes nothing: the member stays where it is.
No leader of this version sends it.

**In memory.** A leader's replay brings a member that subscribed every entry of the
leader's own log from the position asked to its end, and ends a step later; with no
subscriber it reaches, it ends after the replay connect timeout, 5 s. When a member
subscribes on its catch-up endpoint, what it received above its append position and
never synced leaves its log: it never reported those bytes, and no rule of the page
speaks of bytes a member has not reported.

## Alternatives considered

- **Record the term at the stop, as the row had it and Aeron does.** Lost, for the
  first reason above. It needs a member to truncate what it replicated, which ADR
  0083 refuses under a commit position the member was told in the same state.
- **Record at the stop, and let the guard pass bytes a replication brought.** Lost.
  The guard is the only thing that stops a member from cutting a committed entry on
  the word of a leader whose log is not its cluster's. A member would have to keep,
  durably, which bytes came from which replay, and it would still fetch the term a
  second time.
- **Tell a member in state 9 no commit position.** Lost. The wait at the leader's
  base is for that position, and a member that knows less of what is committed
  protects less.
- **Check an image's join position against the append position of the cycle that
  sees it.** Lost. It fails a correct replay whenever the recording syncs before the
  core's next cycle.
- **Keep the in-memory member's unsynced bytes, and let a replay skip what the
  member holds.** Lost. Those bytes can differ from the leader's, and the member
  would hold them below a position it then reports. Making its append position jump
  to them when it leaves the log, as a recording's stop does over a driver, changes
  what every test with a paused sync sees, for a member no driver produces.
- **Fail a member told to wait, as before.** Lost. The page says it waits, and
  failing counts an election failed that did not fail.

## Evidence

- `crates/cavalry-raft/tests/follower_log_replication.rs`, twelve tests on one core
  through an `Io` that remembers what it was asked.
  - A member 32 bytes short of the next base subscribes, then asks from 32 once,
    reports with its own term as it goes, and at the stop records the leader's term
    before it reports, with that term. It canvasses, two steps, and the next new
    leadership term finds it level and it joins.
  - A member at the base of a term it lacks calls `record_term` before
    `add_catchup` and before the catch-up position. A record that fails is
    returned, nothing is asked, and the next cycle records again. At a stop below
    the leader's base it records nothing more and waits for nothing.
  - A member that began term 1 at 64, was told a commit position of 200, and
    stopped at 96 for the leader heartbeat timeout fails its election. Told the term
    after, it asks from 96: `truncate_log` is never called and
    `cluster.truncations_refused` stays 0.
  - An image that joins at 16 where the member asked from 32 fails the election,
    counted, and init removes the subscription. A second image that joins at the
    position last sent is taken even when the cycle that sees it already counts 16
    bytes of it; a third that joins elsewhere fails.
  - No move for the leader heartbeat timeout fails, counted; a move starts the time
    again; a catch-up position that never leaves is sent again each cycle, counts no
    replication, and fails by the same timer.
  - At the leader's base the member waits, reporting every leader heartbeat
    interval; a commit position below its own extends the wait; one at it ends it;
    with no word after the stop the election fails and counts no replication
    unavailable.
  - A term with nothing in it is recorded and asks for nothing. A member carried
    past its stop fails and records nothing. A member in the state follows its
    leader alone. A member told to wait stays and counts nothing. An appointed
    member fails where one that elects replicates.
- `crates/cavalry-raft/tests/election.rs`, in memory, with the check after every
  step that no committed entry is lost.
  - The five-member history of pull request 218: the follower cuts the entry no
    quorum recorded, records the second term at its base, replicates it, and records
    the third term at its end. Its log is the third leader's to that base, byte for
    byte, and the third leader's election closes, with the quorum that recorded its
    term.
  - Three members: a follower whose sync is paused replicates what it lacks; when
    it syncs it is at the leader's base and the commit position it knew, records the
    term, joins the log there, and the two commit.
  - A replay that never arrives: the leader's replay ends for want of a subscriber,
    the member's election fails after the leader heartbeat timeout, counted, and
    when the path is whole it asks again and is brought the leader's log.
- `crates/cavalry-raft/tests/transitions.rs` makes each of the eight new rows, from
  a member with a history a cluster can have: it followed term 0, lost its leader,
  and is told term 1 from 64.
- Every other test of the crate passes unchanged.

## Consequences

- A member that missed whole terms replicates them, one term for each new
  leadership term, and a leader that needs it for its quorum gets it.
- A member that replicated to its leader's base while the leader waited for it is
  counted by a report it sends from state 9, before it is on the live log. The
  leader closes its election and appends the term's first entry, and the member is
  behind by that entry until the catch-up within a term exists, states 11 to 13, the
  next step. In the five-member history nothing above the third term's base is
  committed until then. A member that already knew the commit position canvasses at
  once; its canvass position takes it out of the leader's count, and it joins at the
  base before the leader appends.
- A member in state 9 knows a commit position above its own append position. ADR
  0083's guard then refuses any truncation below it. One history is known, by
  reading and not yet by a test, in which a leader asks for such a truncation of
  bytes that are the cluster's: a leader whose record holds a term that fewer than a
  quorum recorded (card 267). It is a failure to join, not a wrong byte.
- `Io` has four more operations, twenty. `Counts` has twenty-one counts, and a
  member's module adds twenty-one client counters; ten of them stay 0 while the
  module's leader is appointed.
- `cavalry-proto`'s `replication_record` is unchanged, and its vectors with it: its
  `replicated` field is what the record holds by the stop, and only its `leader`
  field is written there.
- Revisit: when the module's I/O for the election is built, at which the four
  operations stop refusing and `catchup_image`'s order against `append_position` is
  the module's to keep; and with card 267.

## References

- `spec/cluster.md`, "Roles and the election", rows 0, 9 and 15, "Messages during an
  election", "Messages outside an election" and "Counters"; the `replication-stop`
  and `replication-record` vectors.
- [ADR 0085](0085-serve-a-replication-from-the-leaders-core-to-the-endpoint-the-members-string-names.md),
  the leader's half and the endpoint.
- [ADR 0083](0083-truncate-to-the-next-terms-base-and-never-below-the-commit-position.md),
  the guard on a truncation and its note on the in-memory member's unsynced bytes.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path and its steps.
- [ADR 0067](0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  the registry of the election's machine.
- Aeron's `Election.followerLogReplication`, `updateRecordingLogForReplication` and
  `onTruncateLogEntry`, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
