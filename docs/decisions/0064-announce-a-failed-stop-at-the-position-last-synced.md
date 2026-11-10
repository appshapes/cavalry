# 0064. Announce a stop whose own writes failed at the position last synced

- **Status:** Accepted
- **Date:** 2026-11-10
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Amends `spec/client-protocol.md`, "Commands" and "Events", within
  version 7, with a rule for a case the page did not specify: a stop whose own writes or
  syncs fail. No encoding changes, and no vector

## Context

A recording's stop is disk work of its own. `LogDir::stop` in `cavalry-log` takes four
steps (`spec/durable-log.md`, "The driver"):

1. It appends the time index's stop entry.
2. It syncs the segment file and the time index.
3. When the writer had just rolled into a new segment file and wrote nothing into it,
   it removes that file and syncs the directory.
4. It writes the catalog's stop fields and their checksum, and syncs the catalog.

Any step can fail. A full disk that stopped the recording at a block write can refuse
the stop entry too. When a step fails, the driver's recorder (`Recorder::stop_at` in
`crates/cavalry-driver/src/recorder.rs`) does three things. It adds one to
`recording.write_failed`. It leaves the catalog slot live for the next open's recovery.
It announces the stop with the position written, the end of the last block it handed
to the operating system. The `recording.synced_position` counter still holds the last
sync, which can be up to a second behind that position (ADR 0025).

The page did not say what happens then. "Commands" says the driver answers `stop
recording` "once the catalog's stop fields are written and synced". "Events" says the
driver sends `recording stopped` "after the catalog's stop fields are synced, so the
position a client reads from the event is what the catalog holds". Neither can hold for
a stop whose own writes failed, and unspecified behaviour is a bug in the spec
(CLAUDE.md, "Protocol").

The position now matters to a client. Card 227 makes the Rust client follow `recording
stopped`: after a stop, `Recording::synced_position()` returns the stop position. A
cluster member's append position is its log recording's synced position (ADR 0060). A
member that reports an append position past its last sync acknowledges entries that a
power loss can take from it. That breaks Raft's safety (invariant 1). After a power
loss, recovery rebuilds the stop from the files (`spec/durable-log.md`, "The driver").
Its scan ends at the last frame that checks. That frame's end is at or above the last
sync, and it can be below the position written.

## Decision

A stop whose own writes or syncs fail still stops the recording. The recorder writes no
more of it, the failure adds one to `recording.write_failed`, and the catalog slot stays
live until the next open's recovery rebuilds its stop from the files. The stop position
the driver announces is the position the recorder had synced before the stop began. The
driver reads it from the writer before `LogDir::stop` takes the writer, and it is the
value `recording.synced_position` holds. `recording stopped` carries that position to
every client, and the conductor hands the same position to the replayer for a replay
that follows the recording (ADR 0036). Every frame below that position survives a power
loss, so the stop that recovery rebuilds is at or above it. `recording.position` keeps
the position written, which is what the operating system holds; the driver does not
lower it. The `stop recording` command on this path is answered with `operation
success`, as on every other path. Here the answer means two things: the recording is
stopped, and the `recording stopped` that follows carries a durable position. It does
not mean that the catalog holds the stop.

## Alternatives considered

- **Keep announcing the position written.** This was the behaviour before this
  decision. A client that read `recording stopped` would take up to a second of bytes
  that no sync covered as the recording's durable end, and a member would acknowledge
  entries it does not durably hold. It lost on invariant 1.
- **Announce the writer's synced position after the failed stop.** `LogDir::stop` would
  return the writer, or its synced position, inside its error. This is more exact when
  the stop fails after its own syncs, at the segment removal or the catalog write: the
  position written is durable then, and this decision announces a lower position. It
  lost because it changes `cavalry-log`'s public API for that one case. The cost of the
  simpler rule in that case is a stop announced below what the files hold, and recovery
  raises it at the next open. The position read before the stop is never above the
  durable position, whichever step fails.
- **Answer the `stop recording` with `error`, code 12.** The command did its work: the
  recording is stopped, and a second `stop recording` is answered with `operation
  success` and nothing written, since no live recording holds the id. An error would
  tell the client that the recording is still live when it is not. A recording that
  stops by itself, at end of stream or after a failed write, has no command to answer,
  so the event has to carry the outcome on every path anyway. The operator learns of the
  failure from `recording.write_failed`, which `cavalry doctor` reports as its
  `recording-write-failed` finding. It lost.
- **Repair the slot from the files at once.** A retention command's failed file work
  gets this repair (ADR 0031), and the catalog would then hold a stop without waiting
  for the next open. It lost for this change for two reasons. The disk that refused the
  stop's writes is the disk the repair would write to. And the repair's file side runs
  on the replayer, a round trip that a stop does not make today. It is the Revisit
  below.

## Evidence

- Before this decision, `Recorder::stop_at` took the position written when
  `LogDir::stop` returned an error. `Conductor::on_stopped` sent that position in
  `recording stopped` and handed it to the replayer.
- `RecordingWriter::sync_files` moves the synced position only after the segment's and
  the time index's `sync_data` have both returned (`crates/cavalry-log/src/recorder.rs`).
  So the position read before the stop is one that a sync covered.
- `conductor::replay::a_stop_whose_own_write_fails_announces_the_position_last_synced`
  in `cavalry-driver` drives the case. Ten frames are synced, 640 bytes, and ten more
  are written and not synced, 1,280 bytes. The slot's stop checksum is then damaged, so
  that the stop's catalog step, the last, is refused. The stop's index entry and syncs go through
  files the recorder holds open, and a test cannot make them fail without a full disk.
  A failure at any step takes the same branch of `stop_at`. The test checks that the
  command is answered with `operation success`, and that `recording.write_failed` and
  `recording.stopped` each move by one. It checks that `recording stopped` carries 640.
  It checks that the position counter keeps 1,280 and the synced counter keeps 640.
  Finally, it opens the directory again: recovery counts one rebuilt stop, at 1,280,
  which is at or above 640. On the code before this decision, the test fails with
  `left: [(0, 1280)] right: [(0, 640)]`.
- The review of pull request 167 found the gap.

## Consequences

- A `recording stopped` position is durable on every path. It is either the catalog's
  stop, written and synced, or the position last synced before a stop that failed. A
  client may treat it as the recording's durable end.
- After a failed stop, the announced position can be below the stop that the catalog
  holds after the next open. A reader of the catalog after a restart may see more of
  the recording than the event said, but never less.
- On that path the Rust client's `Recording::position()` falls to the stop position.
  That can be below a value it returned before the stop. The handle's documentation
  says so.
- Until the next open, the catalog reads the slot of a recording whose stop failed as
  live. That was already true before this decision.

Revisit: when a stop that failed should put its stop in the catalog without a restart,
through the repair that a retention command's failed file work gets (ADR 0031).

## References

- `spec/client-protocol.md`, "Commands": `stop recording`, and the paragraph on the
  stops the driver makes by itself. "Events": `recording stopped`.
- `spec/durable-log.md`, "The driver": the stop's writes and recovery's scan.
- ADR 0025: the recorder, its sync cadence and `recording.write_failed`.
- ADR 0036: the conductor hands the replayer the stop position.
- ADR 0060: a member's append position is its log recording's synced position.
- Pull request 167, card 227.
