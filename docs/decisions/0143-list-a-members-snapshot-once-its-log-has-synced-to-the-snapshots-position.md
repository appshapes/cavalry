# 0143. List a member's snapshot once its log has synced to the snapshot's position

- **Status:** Accepted
- **Date:** 2026-11-22
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Adds one condition to when a member lists a snapshot of its
  consensus module (ADR 0129) and one reason a snapshot fails. Changes no message, no
  layout and no format, so the cluster protocol stays at version 1.

## Context

A member takes a snapshot of its consensus module when its read of its own committed
log reaches a cluster action that asks for one (`spec/cluster.md`, "Snapshots"; ADR
0129). It writes the snapshot's messages to a recording, stops that recording, and
lists the snapshot in its snapshots file once the driver has announced the stop at
the end of what it wrote. A start takes its state from the newest snapshot it can
read and refuses a log that ends below that snapshot's position, since a truncation
never cuts below a commit position and so such a log has lost its end (`spec/cluster.md`,
"A member's start"; ADR 0130).

The snapshot's position is one the member read from its own log, as the recorder had
written it. It need not be one the recorder had synced. The commit position is the
quorum's synced positions, and this member's own can trail them. The member asks its
driver to sync its log whenever the recorder has written past what it synced
(`spec/cluster.md`, "The log"; ADR 0069), so its append position follows what it
holds by one sync; but nothing ties the listing to that sync, and a sync that is slow
or that fails leaves the member's log behind for longer. So a snapshot can be listed
at a position above what the member's log has synced.
Today that is harmless after a crash of the driver's process: recovery scans every
byte the recorder wrote, and the log reaches the snapshot. It is not harmless after a
power loss, which can take the bytes past the last sync. Work item 287 goes on to
make recovery cut a recording that a crash left live to the position it had synced,
on every kind of crash, since no start can tell a power loss from a crash of the
process; that is a change to the durable log's format with a decision of its own.
From then on a member whose snapshot was listed above its synced log would be
refused at its start after any crash: a liveness fault that the operator would meet
as a log that "has lost its end", where nothing was lost but bytes that had not been
synced. This decision lands first, so that the cut never meets such a snapshot.

## Decision

A member lists a snapshot of its consensus module only once its append position,
what its own log has synced, has reached the snapshot's log position. The wait comes
after the recording's stop has been read, and the condition is checked in each duty
cycle. The member has already asked the sync that ends it, since it asks one whenever
its log moves, so the wait is that sync's length, usually less than the snapshot's own
stop. Its bound is the snapshot log sync wait of `spec/cluster.md`, "Values",
`LOG_SYNC_WAIT` in the crate: the recording sync interval, 1 s, within which the
recorder syncs a recording by itself even when no sync is asked, and the snapshot
timeout, 1 s, beside it for the sync, 2 s in all. A log that has not synced to the
position by then gives the snapshot up: it is counted in `cluster.snapshots_failed`,
nothing is listed, and the next action asks again, as for every other way a snapshot
fails. The state machine `cluster_snapshot` keeps its
three states: the wait is part of `stopping`, whose two ways out are counted already.

A snapshot of the service beside the member needs no such condition. A service's
start that reads the log from a position above where the member's log ends is
answered "no log yet" until the member's live recording reaches it (ADR 0135), and
no start of the member reads from a service's snapshot.

## Alternatives considered

- **Start a member whose log ends below its snapshot, and let it catch up to the
  snapshot's position from the leader.** Its state would be the snapshot's while its
  log ended below it, and every rule of `spec/cluster.md` that reads a member's log
  and its state as of one position would need a case for the gap. The refusal would
  also lose its use: it is how a start finds a log that was purged or cut too far.
  The condition at the listing removes the case instead of handling it.
- **Count the wait for the log apart from the snapshot's other failures**, as a
  state of `cluster_snapshot` of its own or a counter of its own. Every way a
  snapshot fails is one count today, and `cluster.snapshots_failed`'s `doctor` text
  sends the operator to the driver's counters for the cause, `recording.write_failed`
  for a sync that failed among them. A log that will not sync is a fault of the
  recorder that those counters already show, so a fourth state would add a
  transition and no finding.
- **List the snapshot at once and fall back to an older one at a start whose log
  ends below the newest.** A start already passes over a snapshot it cannot read
  (ADR 0130). Passing over one it can read whole would make the newest snapshot in
  the file mean nothing to an operator who purges by `cluster.log_purge_position`:
  the start would need log below the position the counter names.

## Evidence

- `crates/cavalry-raft/src/snapshot.rs`, two tests over a driver:
  `a_snapshot_is_listed_only_once_the_members_log_has_synced_to_its_position` holds
  the member's append position 32 bytes short of the snapshot's position. Past the
  snapshot timeout the recording has stopped at the end of what was written, the
  snapshot is in hand and nothing is listed; once the position is reached the
  snapshot is listed and counted as taken.
  `a_log_that_does_not_sync_to_the_snapshots_position_within_the_wait_gives_it_up`
  holds it at 0: the snapshot is given up after `LOG_SYNC_WAIT`, counted as failed,
  and the next action's snapshot is taken once the log has synced.
- `crates/cavalry-raft/src/memory.rs`, `Cluster::restart`, read on 2026-11-22: the
  cluster in memory already plays a member's start after a power loss as its log cut
  to what it had synced.
- The rest of `just test -p cavalry-raft` passes unchanged, the cluster tests over
  three drivers included: in each, a member's log syncs one asked sync after the
  action, so no snapshot there waits long enough to fail.

## Consequences

- No crash of any kind leaves a member a snapshot of a position its log does not
  reach: recovery keeps what a recording synced, today and once work item 287 cuts a
  recording to it. The
  start's refusal of a log that ends below its snapshot then means what
  `spec/cluster.md` says it means: a log purged or cut too far, or a directory older
  than its snapshot.
- A snapshot counts at most one sync of the member's log later than before, the sync
  the member asked when its log moved. `cluster.snapshots_taken` and
  `cluster.snapshot_position` move then.
- A cluster action read while a snapshot waits for the log ends the later snapshot,
  as one read while the snapshot is written or stopped does. The wait is one sync in
  practice, and 2 s only on a recorder that cannot sync, so the window in which a
  second action is counted as failed grows by that sync.
- A member whose recorder cannot sync its log takes no snapshot, and each attempt is
  counted in `cluster.snapshots_failed`. Such a member reports no new append position
  either, so it is already the member the cluster leaves behind.
- Revisit if the wait shows up in a profile of a member's duty cycle, or if
  snapshots given up for the wait need telling apart from the others.

## References

- Work item 287 on the Brigade Board, which carries this decision and the change to
  the durable log after it; work item 363, feature 3 of the beta slice (ADR 0138).
- `spec/cluster.md`, "Snapshots" and "A member's start".
- [ADR 0129](0129-snapshot-a-members-sessions-when-its-read-of-its-log-reaches-the-leaders-action.md),
  the snapshot and its listing.
- [ADR 0130](0130-start-a-member-from-its-newest-snapshot-and-hold-its-log-to-that-position-alone.md),
  the start that refuses a log ending below its snapshot.
- [ADR 0135](0135-answer-no-log-yet-where-a-members-live-recording-has-not-reached-the-position.md),
  a service's read above where the member's log ends.
