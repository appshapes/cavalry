# 0065. Give up a recording whose sync failed, without a stop of its own

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Amends ADR 0025 in one clause, where a recording stops whose
  write failed in a sync, and extends ADR 0064's rule for a stop that fails to the
  stop of a recording one of whose syncs has already failed. Amends
  `spec/client-protocol.md`, "Commands" and "Events", within version 7, and
  `spec/durable-log.md`, "The driver", within version 2. No encoding changes, and no
  vector.

## Context

ADR 0025 has the recorder stop a recording whose write fails "at the position
written", and the recorder did the same for a periodic sync that failed: it counted
`recording.write_failed` and ran the recording's stop. The stop is writes and syncs of
its own: the time index's stop entry, a sync of the segment file and of the time
index, and the catalog's stop fields (`spec/durable-log.md`). ADR 0064 decided what a
stop announces when one of those fails: the position last synced, with the catalog
slot left live.

That leaves the stop that succeeds after a sync has failed, and it can succeed for
the wrong reason. On Linux a writeback error is reported once to each open file
description, and ext4 and XFS do not mark the pages that failed dirty again. So when
the recorder's periodic `fdatasync` fails, the stop's `fdatasync` on the same
descriptor returns 0 and writes nothing. The stop then completes: the catalog takes
the position written as the stop, both counters are stored with it, and `recording
stopped` carries it to every client. The bytes from the last sync that succeeded up
to that position may never reach the disk. A write reaches the same place when it
fills a segment, since the writer syncs the segment it closes. This is the failure
PostgreSQL met in 2018 and answered by never trying a failed sync again.

A disk that returns EIO at writeback is enough, or a filesystem that reports ENOSPC
or EDQUOT then: XFS, NFS, a thin-provisioned volume.

Since card 227 the position matters to a client, as ADR 0064 says: the Rust client
returns the event's position as `Recording::synced_position()`, and a cluster member
reports that as its append position (ADR 0060).

A second path reaches the same place without a stop. A recording stops only when the
recorder has room for the stop's reply. When it had none, the recorder left the
recording live for the next cycle, and in that cycle it synced again. That sync
returned success, and `recording.synced_position` moved over the bytes that had
failed.

A write that fails with no sync failing is a different case. A full disk refuses a
block, or the next segment file, in the call itself. No sync has been told of an
error, so the stop's sync is the first to be asked about everything written before,
and its answer is true.

## Decision

A recording one of whose syncs has failed is given up, and nothing more is done to
its files. The sync is the periodic one, or the one a write makes of a segment it
closes; the writer says which writes failed so (`RecordingWriter::sync_failed`). The
recorder marks the recording at the failure. From then on it writes no block of it
and syncs nothing of it, in that cycle or a later one, and at the first cycle that
has room for the reply it stops the recording without running a stop: no stop entry
in the time index, no sync, no catalog write.

Such a recording is announced as ADR 0064 announces a stop that failed. The stop
position, in `recording stopped` and handed to every replay that followed the
recording, is the position the last sync that returned success covered, which a sync
that failed never moved: the start position when none succeeded. The catalog's slot
stays live, and the next open's recovery rebuilds the stop from the files, at or
above that position. `recording.position` keeps the position written and
`recording.synced_position` the position last synced until the conductor frees them.
A `stop recording` that arrives for a recording already marked is answered as ADR
0064 answers a stop that fails.

A recording whose write failed with no sync failing is stopped as ADR 0025 has it,
at the position written, by a stop that runs; if that stop's own writes or syncs
fail, ADR 0064 applies.

`recording.write_failed` counts a recording once, whichever of its writes, its syncs
and its stop failed.

The rule for both ADRs, in one sentence: the position a client reads from `recording
stopped` is one that a sync which returned success covered, and no sync is asked a
second time about bytes a sync has failed on.

## Alternatives considered

- **Run the stop after a failed sync, as before, and announce the position last
  synced whatever the stop returns.** Lost. The event would be right, and the catalog
  would hold a stop that no sync covers, which every later reader of the catalog, a
  replay after a restart among them, would take for the recording's end. The slot
  left live is recovered from what the files hold at the next open.
- **Give up after any failed write, a full disk's refusal included.** Lost. A write
  that is refused leaves nothing a sync has failed on, so its stop tells the truth,
  and the recording is stopped in the catalog at once and replayable to the position
  written. The driver's test of a roll that cannot create the next segment holds it
  to that: the recording stops at the segment's end, in the catalog, with every byte
  before it read back. Giving such a recording up would announce up to a second less
  than the disk holds and leave its slot live until a restart, on the commonest
  failure there is.
- **Make the writer refuse in `cavalry-log`: its `sync`, its `write` and
  `LogDir::stop` return an error after a sync that failed.** Lost for this change. It
  is the same rule one layer down, and the writer has one caller, the recorder, which
  now never calls it again after a failed sync. The writer says that a sync failed
  and leaves what follows to its caller. Revisit if the writer gets a second caller.
- **Open the files again and sync them through new descriptors.** Lost: whether a new
  description sees an error an older one was told of depends on the kernel version,
  and a sync that returns success on a new descriptor proves nothing about pages the
  filesystem no longer holds as dirty.

## Evidence

- `crates/cavalry-driver/src/recorder.rs` before this change: both failure branches of
  `Recorder::record` called `stop_at`, which called `LogDir::stop`, whose
  `sync_files` runs on the descriptors the failed sync ran on; and with no room for
  the reply the branches returned and the next cycle called `writer.sync` again.
- `conductor::replay::a_recording_whose_sync_failed_is_given_up_at_the_position_last_synced`
  in `cavalry-driver`: ten frames synced, 640 bytes; ten more written and not synced,
  1,280; the recording marked as the failure branches mark it when a sync fails.
  `recording stopped` carries 640; no command is answered; `recording.write_failed`
  and `recording.stopped` are 1; the counters keep 1,280 and 640; ten frames
  committed after the failure are not written; the catalog's slot reads live; and the
  next open's recovery rebuilds the stop at 1,280, at or above 640. With the stop run
  as before, the test fails with `left: [(0, 1280)] right: [(0, 640)]`.
- `conductor::recording::a_publisher_faster_than_the_recorder_is_back_pressured_and_nothing_is_lost`,
  unchanged: a roll that cannot create the next segment file stops the recording at
  the segment's end, in the catalog, counted once.
- What no test here covers: an `fdatasync` that fails. A test cannot make one fail on
  the recorder's descriptors without a failing disk, so the test sets the mark the
  branch sets, and the lines that set it, the writer's three and the recorder's two,
  are read and not run.
- The review of pull request 167 found the path.

## Consequences

- With ADR 0064, a `recording stopped` position is one a sync that returned success
  covered on every path the driver has: the catalog's stop when the stop ran with no
  sync having failed before it, and otherwise the position last synced.
- A recording one of whose syncs failed is not stopped in the catalog until the
  driver starts again. Until then its slot reads live, as after a stop that failed
  (ADR 0064, whose Revisit covers both).
- `recording.write_failed` counts one for each recording. Before, a failed write
  followed by a failed stop counted two for one recording.
- Recovery reads the files as the operating system serves them. In the same boot that
  can include bytes whose writeback failed, so the stop recovery writes is at or
  above the position announced, and not known to be on the disk past it. The
  position announced is what a client may rely on.

Revisit: with ADR 0064's, when a recording that failed should get its stop in the
catalog without a restart; and if recovery should sync what it scans before it writes
a stop.

## References

- ADR 0025 (the recorder and `recording.write_failed`), ADR 0064 (a stop whose own
  writes fail), ADR 0060 (a member's append position).
- `spec/client-protocol.md`, "Commands": the paragraph on the driver's own stops;
  "Events": `recording stopped`. `spec/durable-log.md`, "The driver".
- PostgreSQL's fsync failure of 2018: a sync that failed is not tried again.
- Pull request 167, card 227.
