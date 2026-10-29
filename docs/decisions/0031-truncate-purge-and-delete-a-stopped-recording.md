# 0031. Truncate, purge and delete a stopped recording with Aeron Archive's commands

- **Status:** Proposed
- **Date:** 2026-10-29
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none; it takes the client protocol to version 3 (ADR 0019, ADR 0025,
  ADR 0030) and the catalog to version 1 (ADR 0024)

## Context

Card 95 is the fourth and last feature of the second slice (ADR 0023): the durable log
never grows without a bound, and a stranger records and replays in the ten-minute path.
The path records and replays since the commit that precedes this ADR; what is missing
is the way to make a recording smaller or to remove it. Today `spec/durable-log.md` says
the segments and the index "grow with the recording until retention removes them", and
nothing removes them: a stopped recording holds its segment files, 128 MiB each, until
the operator deletes files by hand under a directory the driver owns, which recovery
then reads as damage (`recording.invalidated`). `crates/cavalry-log/README.md` promises
that retention is "explicit, never silent growth", and the catalog's descriptor already
carries a state, 3, "deleted", that ADR 0024 reserved for this card: "a delete (card 95)
writes the state, 3, then removes the files".

Two facts of the format shape the decision. The descriptor's start position is a
creation field, written once under the creation checksum and never again (ADR 0024,
`spec/durable-log.md`, "Descriptor"), so Aeron Archive's way of purging the oldest
segments, which moves `startPosition` forward in the descriptor, is not available
without a new field. The stop fields, bytes 8 to 24, are rewritable under their own
checksum, so a truncate fits the format as it is. Segment files are named by their base
position and hold raw term bytes with no header, and recovery today reads a segment file
missing between the start and the last as damage and sets the slot to state 2.

Aeron Archive's commands are the reference: `truncateRecording(recordingId, position)`
on a stopped recording, `purgeRecording(recordingId)` which deletes it, and
`purgeSegments(recordingId, newStartPosition)` which removes whole segments before a
position; with `detachSegments`, `attachSegments`, `migrateSegments` and
`deleteDetachedSegments` for moving segments between archives. A wire change is a change
to `spec/`, a conformance test and a version bump before any code, and a decision
CLAUDE.md says to ask about first; so is a change to the on-disk format.

## Decision

The client protocol moves to version 3 with three commands and no new event, each
answered with `operation success` or an error, in the id range ADR 0025 gave Cavalry,
in order after `stop replay`:

- **`truncate recording`** (type id 261, `0x0105`): client id, correlation id, recording
  id (i64), position (i64). The recording must be stopped; the position must be a
  multiple of 32 from the recording's retained position to its stop position, and a
  frame must begin at it or it must be the stop position, which the driver checks by
  reading the frame header there. The driver writes the stop position and the stop
  timestamp again under the stop checksum, then removes the segment files that lie
  wholly past the new stop, zeroes the bytes past it in the segment that holds it, and
  truncates the time index to the entries at or before it. `recording.truncated` adds
  one.
- **`purge segments`** (type id 262, `0x0106`): client id, correlation id, recording id,
  new start position (i64), Aeron's `purgeSegments`. The recording must be stopped; the
  position must be a segment base, a multiple of the segment file length from the
  first segment's base, above the retained position and at most the stop position. The
  driver writes the position as the descriptor's **retained position**, a new field,
  then removes every segment file whose bytes lie wholly before it. The index entries
  before it stay; a replay from an instant that resolves before it is answered as a
  position out of range, naming the retained position. `recording.segments_purged`
  adds one per segment file removed.
- **`purge recording`** (type id 263, `0x0107`): client id, correlation id, recording
  id, Aeron's `purgeRecording`: the delete ADR 0024 reserved. The recording must be
  stopped and have no live replay. The driver writes the state, 3, then removes the
  segment files and the time index; the slot stays, so the id never names another
  recording. `recording.deleted` adds one.

A recording that is live, or that a live replay is reading, is refused with a new error
code, 24, `recording busy`, whose message says which and what to do: `cavalry record
--stop <id>` or `cavalry replay`'s stop. An unknown or invalid recording is code 21 as
today; a position outside the rules above is code 23, position out of range, with the
message naming the bounds, as `start replay` answers. A file the driver could not
remove, truncate or zero is code 12, storage space, after the catalog write: the catalog
is written first in every command, so a crash between the two leaves a recording whose
catalog says less than its files hold, which recovery already handles for a truncate
(segments wholly past the stop are removed) and for a delete (a state 3 with files
present runs the delete again, ADR 0024), and handles for a purge with one new rule:
segment files before the retained position are removed, not read.

The catalog moves to version 1 for the retained position: an i64 at offset 472 of the
descriptor and its CRC32 at offset 480, a third group under its own checksum, written
by `purge segments` and by the creator, who writes the start position there. A version 1
driver reads a version 0 catalog, taking the start position as the retained position of
every slot, and writes the header's version as 1 the first time it writes a slot; a
version 0 driver refuses a version 1 catalog with a message naming the version, since the
catalog is read by the driver alone and a downgrade is not an upgrade (invariant 3's
mixed versions are a client and a driver). Recovery's rule for a missing segment file
counts from the retained position's segment, not the start position's.

The file work runs on the replayer thread, which already opens and reads segment files
and holds no replay most of the time (ADR 0030): the conductor validates the command,
writes the catalog, hands the files to the replayer through its request queue and
answers the client when the replayer's reply arrives, as a replay is started. Nothing
runs on the recorder thread or the conductor's duty cycle beyond a catalog write of one
slot, which `start recording` and `stop recording` already do.

Counters: `recording.truncated`, `recording.segments_purged`, `recording.deleted` and
`recording.retention_failed` as counters; and one more instance gauge per recording,
`recording.bytes`, the bytes its segment files take on disk, kept by the recorder at
each roll and by the three commands, keyed and labelled as `recording.position` is. The
counters-file arithmetic becomes 41 statics plus 2048 plus 2048 plus 3072 plus 192, 7401,
under 8192. `cavalry doctor` gains a check that reads the free space of the log
directory's filesystem and, under 10 percent, names the recording and the segment that
would free the most, with the `purge segments` command that removes it, and a check that
names a recording whose `recording.bytes` is over half the filesystem.

No configuration key. The card allowed `log.retain` only if a default could not be
right, and no default can: a retention that deletes a user's recording on a schedule the
user did not set is a lie about delivery's record (invariant 1), and `crates/cavalry-log`
promises retention is explicit. The soak host (card 159) runs `purge segments` from a
script on its own schedule, which is what an operator does.

The `cavalry` binary: `record --truncate <id> --to <position or instant>`, `record
--purge <id> --before <position or instant>` and `record --delete <id>`, instants
resolved through the time index as `replay` resolves them; the client API gains
`Cavalry::truncate_recording`, `purge_segments` and `purge_recording`. The record how-to
gains the three as steps 4 to 6, and the glossary the rows retained position, truncate,
purge segments and delete.

## Alternatives considered

- **A `log.retain` key, by age or by bytes, with a doctor check.** Lost: no default can
  be right, so the key would be set on every host, and a retention the user did not ask
  for removes a record the user may need; explicit commands and a scheduled script on
  the host that wants them keep the decision with the operator. Revisited when a user
  asks for retention by age or size and says what the default should be.
- **Aeron's mutable start position for `purge segments`.** Lost: the start position is a
  creation field under the creation checksum (ADR 0024), and a field written once is
  what lets a reader trust the creation group with one check; a retained position in the
  reserved bytes keeps that and adds one group with one checksum.
- **The file work on the conductor.** Lost: `unlink` of a 128 MiB file and `ftruncate`
  are syscalls of unbounded length on a filesystem under load, and the conductor's duty
  cycle sets the publishers' limits; the replayer thread is the driver's disk thread and
  idles between replays (ADR 0030).
- **A delete that frees the slot for reuse.** Lost: ADR 0024's rule that an id never
  names another recording, which a client holding an old id depends on.
- **Events for each command, as Aeron's `RecordingSignal`.** Lost for this version: the
  commands are synchronous and the catalog is the record; a client that wants to know
  reads the catalog through `doctor` or the counters. A signal event comes with a need.
- **Detach, attach and migrate segments.** Lost for this slice: there is one archive per
  host and no UDP, so nothing moves segments between hosts; the third slice's ADR decides.
- **Truncate at any byte, as Aeron does.** Lost: a position inside a frame leaves a
  partial frame at the end, which the replayer's whole-frame rule (ADR 0030) and the
  `frame` check would report as damage; a frame boundary or the stop keeps every
  recorded byte readable.

## Evidence

To be filled at acceptance: the vectors of `spec/vectors/client-protocol.py` for the
three commands and error code 24; the catalog's version 1 read of a version 0 file and
the refusal the other way; recovery tests for a torn truncate, a torn purge and a torn
delete; a conductor test for each refusal (live, replayed, out of range, unknown) and
each success with its counter; the bench rows with the recorder idle, unchanged, and
with a purge running on the replayer thread during the recorded rows; `just quickstart`
recording and replaying on both runners, which this ADR's commit already has.

## Consequences

Easier: a log directory is bounded by the operator's commands, a recording can be cut
to what matters, and a soak host runs for days with a script that purges behind the
replays it has done. Harder: the catalog has two versions to read, the replayer thread
has one more kind of request, and recovery has one more rule. We now maintain three
commands, an error code, four counters, a gauge, two doctor checks and three CLI forms.

Revisit: when a user asks for retention by age or size, for the key and its default;
when the third slice moves segments between hosts, for detach and migrate; when a
version 0 driver must read a version 1 catalog, for a downgrade path; when the first
operator reads a `recording busy` refusal and asks for a force.

## References

- `spec/client-protocol.md` version 3 and `spec/durable-log.md` catalog version 1 (to be
  written on acceptance), ADR 0023 (the slice), ADR 0024 (the format, state 3), ADR 0025
  (recording), ADR 0030 (replay, the replayer thread).
- Aeron Archive: `AeronArchive.truncateRecording`, `purgeRecording`, `purgeSegments`,
  `detachSegments`, `ArchiveTool`.
- Card 95, card 159 (the soak host that runs the purge).
