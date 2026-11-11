# 0088. Catch up within a leader's term, and join the live log from the replay

- **Status:** Accepted
- **Date:** 2026-11-11
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none. Changes rows 11 and 13 of `spec/cluster.md`, "Roles and the
  election": row 11 counts the catch-up position in `cluster.catchups` when it has
  left; row 13 names the leader's position the live-add window is measured from, and
  says that "no progress" is an append position that has not moved. Adds the steps
  of the catch-up to the row of `cluster.election_steps` in "Counters". The page's
  version stays 0: no message changes, and no member over a driver takes a vote yet
  (ADR 0081).

## Context

ADR 0081 listed the steps of the electing path. The fifth is the catch-up within a
leader's term, states 11 to 13. ADR 0087 built the leader that answers. This is the
member that asks.

Until this change a member that had joined its leader's term, and was behind the
leader's log, failed its election in follower replay, every election status
interval, for as long as the leader led. That was the end of five of the cluster's
tests: a leader that gave up the lead and came back, a member that replicated a term
it missed, a leader whose module had stood still. In two of them, with a member
stopped, the cluster could commit nothing until the member behind could join.

Rows 10 to 14 of the page say what the member does. Building it found three things
the rows left open.

**Which position of the leader's the live-add window is measured from.** Row 13 adds
the live log "once its append position is within the live-add window of the
leader's". The member knows two positions of its leader's: the log position of the
new leadership term that sent it to follower replay, and the commit position, which
the leader goes on sending. A leader whose election has closed sends no more new
leadership terms to a member that follows it, so the first goes stale as the leader
appends.

**What "no progress" is.** Row 13 fails the election "when the catch-up makes no
progress for the leader heartbeat timeout". A member that holds all of its leader's
log, and waits for stop catch-up, receives nothing.

**The registry has none of the three states**, nor the step from follower log init
to follower ready that a member already subscribed makes (ADR 0067).

## Decision

A member that elects catches up as rows 10 to 14 say, through the operations of
`Io` that a replication uses (ADR 0086) and the live log's.

**Follower replay.** A member whose append position is behind the leader's log
position goes to follower catch-up init. One whose leader is appointed has no
catch-up and fails there, as before.

**Follower catch-up init.** The member subscribes on its `catchup` endpoint and
sends catch-up position with its append position, tried each duty cycle until its
publication takes it. `cluster.catchups` counts it when it has left, and the member
goes to follower catch-up await. One that has not left within the leader heartbeat
timeout fails the election.

**Follower catch-up await.** The image of the leader's replay must join at the
position the member asked from. One that joins elsewhere fails the election,
counted in `cluster.join_position_mismatches`; so does none within the leader
heartbeat timeout.

**Follower catch-up.** The member reports its append position with the catch-up
flag, when it moves and every leader heartbeat interval. It adds the live log once
its append position is within the live-add window of the greater of the log
position and the commit position its leader told it: the commit position is the
one the leader goes on sending, and it never passes the leader's log. When it reads
stop catch-up for its term and its own id, it removes its catch-up subscription.
It goes on to follower log init when three things hold: stop catch-up was read; its
append position has reached the position the live image joined at; and its append
position has reached the commit position it was told. An append position that has
not moved for the leader heartbeat timeout fails the election.

**Follower log init, for a member already subscribed.** It goes to follower ready
with no second subscription, and its report there, without the catch-up flag,
closes its election. It does not pass follower log await: that state checks that a
new image joins at the member's append position, and this member's live image
joined at or past it by design, with the replay bringing the log up to it.

**Counted, and registered.** The three states are in the machine `cluster_election`
with nine rows: the steps from follower replay through the three states to follower
log init, the step from follower log init to follower ready, and each state's
failure, follower catch-up await's with `cluster.join_position_mismatches` beside
`cluster.elections_failed`.

**In memory.** A leader's replay with no end brings a member that subscribed every
entry the leader's log holds from the position asked, each step, as the log grows,
for as long as the leader holds its log publication. A live image that a member
adds while it is behind joins above the member's log and brings it nothing until
the replay has brought the log to where it joined.

## Alternatives considered

- **Measure the live-add window from the log position of the new leadership term
  alone.** Lost. It is the leader's position when the member was told the term. A
  member far behind a leader that keeps appending would add the live log when it
  reached that old position, which can be more than the window behind the leader's
  position by then.
- **Fail a catch-up that receives nothing, whatever its append position does.**
  Lost. It is the same test for a member below its leader's log. For one that holds
  all of it, the leader's stop catch-up comes within a leader heartbeat interval of
  the member's report, and when it does not, the leader is gone and the election
  should fail.
- **Check the live image's join position again in follower log await.** Lost, for
  the reason in the decision: the image joined past the append position on purpose.
  The three conditions of follower catch-up are what make the join safe.
- **Keep the catch-up subscription until the member leaves follower catch-up.**
  Lost. The page has stop catch-up remove it, and the leader has stopped the replay
  by then.

## Evidence

- `crates/cavalry-raft/tests/follower_catchup.rs`, seven tests on one core through
  an `Io` that remembers what it was asked.
  - A member at its leader's term base, told the leader's log is 96 bytes on,
    subscribes, then asks from its append position, counted. The image joins there.
    It reports with the catch-up flag and the leader's term, adds the live log,
    reads stop catch-up, removes the catch-up subscription once, and goes through
    follower log init to follower ready with one live subscription in all. Its last
    report carries no flag and closes its election: four steps.
  - A member 2 MiB behind adds the live log when it is 1,048,576 bytes from the
    leader's position, and not 32 bytes sooner.
  - A catch-up position that does not leave for the leader heartbeat timeout fails
    the election and counts no catch-up.
  - An image that joins elsewhere fails, counted; none within the timeout fails.
  - A stop catch-up for another term or another member is not read. A member that
    was told to stop stays while the live image is not there, while its append
    position is below where the live image joined, and while it is below the commit
    position it was told.
  - An append position that does not move for the leader heartbeat timeout fails
    the election; init then leaves the live log and removes the subscription.
  - An appointed member behind its leader's log fails in follower replay.
- `crates/cavalry-raft/tests/election.rs`, in memory, with the check after every
  step that no committed entry is lost. Five tests that ended with a member behind
  its leader's log now end with it joined, one catch-up counted on it, its log the
  leader's, and the next entry committed by every running member: the five-member
  history of pull request 218; the leader that was cut off; the leader that stood
  still; the leader that three of five could not hear; and the leader whose module
  stood still.
- `crates/cavalry-raft/tests/transitions.rs` makes each of the nine new rows.
- Every other test of the crate passes unchanged.

## Consequences

- A member that comes back behind its leader, by any of the ways the electing path
  has, ends on the leader's log: it truncates what no quorum recorded, replicates
  the terms it missed, and catches up within the leader's term.
- Leader ready still waits for every member the leader has heard to count, and for
  a quorum alone only once the leader heartbeat timeout has passed in that state.
  The page's row 8 closes it on a quorum at once. The wait was there because a
  member that missed the term's first entry could not come back; it can now, so the
  wait can go, in the next change to the core.
- No operation is added to `Io`. Over a driver the catch-up's recording and the
  live one overlap from where the live image joined, and `Io::append_position` is
  the end of what the two hold together, which is the module's to keep when its I/O
  for the election is built.
- Revisit: with ADR 0087, whether a leader can end a catch-up before the member
  holds the log to where its live image joined; a member in that state fails after
  the leader heartbeat timeout and begins again.

## References

- `spec/cluster.md`, "Roles and the election", rows 10 to 16, "Messages outside an
  election", "Values" (live-add window) and "Counters".
- [ADR 0087](0087-serve-a-catch-up-from-the-leaders-core-and-stop-it-at-the-leaders-own-append-position.md),
  the leader's half.
- [ADR 0086](0086-record-a-term-when-its-replication-begins-and-check-each-replays-image-against-the-position-last-sent.md),
  the operations of `Io` a member replicates and catches up through.
- [ADR 0081](0081-build-the-election-beside-the-appointed-leader-and-move-the-module-to-it-once.md),
  the electing path and its steps.
- [ADR 0067](0067-register-the-elections-state-machine-with-its-core-and-count-each-step.md),
  the registry of the election's machine.
- Aeron's `Election.followerCatchup`, which adds the live destination when the
  member is near the greater of the catch-up join position and the notified commit
  position, read on 2026-11-11:
  <https://github.com/aeron-io/aeron/blob/master/aeron-cluster/src/main/java/io/aeron/cluster/Election.java>
