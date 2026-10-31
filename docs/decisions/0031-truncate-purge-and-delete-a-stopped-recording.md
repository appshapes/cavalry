# 0031. Truncate, purge and delete a stopped recording with Aeron Archive's commands

- **Status:** Accepted
- **Date:** 2026-10-29; accepted 2026-10-31 with the four amendments the review of pull
  request 72 asked for, each marked *(acceptance)* where it stands
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
  id (i64), position (i64), Aeron's `truncateRecording`. The recording must be stopped;
  the position must be a multiple of 32 from the recording's retained position to its
  stop position, and a frame must begin at it or it must be the stop position, which
  the replayer checks by reading the frame header there before any catalog write, in
  the order the paragraph on threads below gives. The cut has three steps, in an
  order that leaves, at every point a crash can take, a slot and files that recovery's
  rule for a torn stop already accepts (`spec/durable-log.md`, "The driver"). First the
  driver writes the stop fields as −1 and −1 under the stop checksum: a stop of −1
  reads "live, or being cut", and either way the stop is in the files, not in the
  slot. Second, the file work, in this order: the segment files whose base is at or
  past the position lie wholly past it and are removed from the highest base down, one
  file at a time, so that the files left are contiguous at every point: no crash leaves
  a segment missing between the start position and the last segment file, which
  recovery reads as damage (`spec/durable-log.md`, "The driver"), and the stop recovery
  rebuilds is the end of the highest file left; this is what recovery does to a segment
  file wholly past the stop, so a truncate at a segment base removes the file at that
  base and cuts none; when the position is not a segment base, the segment file that
  holds it, the one whose base is below it, is cut to the position's offset with one
  truncation and extended again to the segment file length, so that the bytes past the
  position are zero and at every point the frames left are whole up to one boundary
  with nothing past it; and the time index is truncated to the entries at or before
  the position. Third it writes the stop position and the stop timestamp, which is
  what recovery's rule gives for the files as cut: the timestamp of the last index
  entry left, or the start timestamp when no segment file is left, a truncate to the
  retained position after a purge, which is recovery's empty recording, whose index
  entries past the stop are dropped as every rebuilt stop drops them, so the index
  holds its first entry alone and the stop timestamp is the start timestamp
  *(acceptance)*; then the stop checksum, and `recording.truncated` adds one. The third step thereby writes the stop
  recovery would rebuild, so a crash before it changes nothing but who writes it.
  A crash before the third step leaves a stop of −1, and recovery rebuilds it from the
  files as for a recording live at a crash. Before the file work, it rebuilds the old
  stop and the truncate is lost, which the client learns from its response timeout and
  sends again. During it, it rebuilds the stop at the boundary the cut reached: the old
  stop while no file is removed; the end of the highest segment file left while the
  files past the position are being removed from the top; the end of the segment that
  holds the position when they are gone and it is not yet cut; the position once it is
  cut; and it finishes the cut, since it drops the index entries and removes the segment
  files past the stop it rebuilds, and a segment file cut and not yet extended reads as
  zeros past its end, as a short start segment does today.
- **`purge segments`** (type id 262, `0x0106`): client id, correlation id, recording id,
  new start position (i64), Aeron's `purgeSegments`. The recording must be stopped; the
  position must be a segment base, a multiple of the segment file length from the
  first segment's base, above the retained position and at most the stop position. The
  driver writes the position as the descriptor's **retained position**, the new field
  below, then removes every segment file whose bytes lie wholly before it. The index
  entries before it stay, so an instant resolves to a position as today, and a replay
  from a position below it is answered as a position out of range, naming the retained
  position. `recording.segments_purged` adds one per segment file removed.
- **`purge recording`** (type id 263, `0x0107`): client id, correlation id, recording
  id, Aeron's `purgeRecording`: the delete ADR 0024 reserved. The recording must be
  stopped and have no live replay. The driver writes the state, 3, then removes the
  segment files and the time index; the slot stays, so the id never names another
  recording. `recording.deleted` adds one.

A recording that is live or stopping, that a replay starting or live is reading, or
that one of these three commands is working on, is refused with a new error code, 24,
`recording busy`, whose message says which and what to do: `cavalry record --stop <id>`
for a live recording; for a replay, `Cavalry::stop_replay` with its replay session id,
or ending the `cavalry replay` process that holds it; for a stop or a command in
progress, wait for its answer. `start replay` on a recording one of the three commands
is working on is code 24 the same way, since during a truncate the slot's stop reads
−1 and the replayer would take the recording for a live one with no synced position to
follow. An unknown or invalid recording is code 21 as today; a position outside the
rules above is code 23, position out of range, with the message naming the bounds, as
`start replay` answers. A file the driver could not remove, cut or extend is code 12,
storage space, after the catalog write; so is a segment file the replayer could not
open or read for a truncate's frame check, before any catalog write; and
`recording.retention_failed` adds one on either, so that it counts every refusal the
files made. After the catalog write the driver then runs recovery's rule for that slot
at once, as `stop recording` runs it for the slot it closes, so that the slot and the
files agree before the error is answered, and the message names the file and what the
recording holds now. Each command checks before it writes, writes the catalog before
its file work, and the rules of this page accept a crash at any point: a torn truncate
is a stop of −1 and is rebuilt from the files; a torn purge leaves segment files before
the retained position, which are removed, not read; a torn delete, state 3 with files
present, runs again (ADR 0024).

`start replay` after a purge: a position of −1 is the recording's retained position,
which is its start position until a purge moves it, the role Aeron's moved
`startPosition` plays; a position below the retained position, or above the extent, is
code 23, and the message names the retained position and the extent; `replay ready`
carries the position the replay starts at, the retained position when the command passed
−1. The time index keeps its entries before the retained position, so
`cavalry replay --from <instant>` resolves an instant as today and is refused with code
23 for an instant before the purge, naming the retained position, which the operator
passes instead. `just quickstart`'s `cavalry replay --recording 0 --from 0` replays from
position 0 of a recording nobody purged and is unchanged.

The catalog moves to version 1 for the retained position. Bytes 472 to 484 of the
descriptor become the **retained group**: the retained position, an i64 at 472, and its
zlib CRC32 at 480, a third group under its own checksum, which `purge segments`
rewrites; the reserved bytes are 484 to 1024. The retained position is at least the
start position, at most the stop position once there is one, and either the start
position or a segment base; the creator writes the start position there. In version 1
the creation checksum covers bytes 28 to 472 and 484 to 1024, so a rewrite of the group
leaves it whole.

A version 1 catalog holds slots in two forms, and a reader tells them apart by the
creation checksum, never by a flag. It checks the checksum over version 1's range first:
a slot that holds is in version 1's form, and its retained group is read under the
retained checksum. A slot that fails it is checked over version 0's range, bytes 28 to
1024: one that holds is in version 0's form, byte for byte what a version 0 driver wrote,
its retained position is its start position and its group is not read. One that holds
under neither is `checksum`, as today. Under a version 0 header every slot is in version
0's form. A healthy slot of either form holds under its own range, so no healthy slot
reports `checksum`, which is what the version bump is for (invariant 1); the price is a
second CRC32 over 996 bytes for a slot in version 0's form, off the message path.

A version 1 driver opens a version 0 catalog and reads it as it is, and under a version
0 header it creates slots in version 0's form, byte for byte what a version 0 driver
writes, so that a directory nobody has purged opens under a version 0 driver again, a
rollback *(acceptance)*. It writes 1 in the header's version, and syncs, before the
first slot whose group it writes, the directory's first `purge segments`, the operator's
own command and the one change that needs the group; from then on it creates slots in
version 1's form, with the group and version 1's range at creation. The version is
thereby the one header field that changes after the magic, which
`spec/durable-log.md`, "Catalog header", says never happens; version 1 says this field
does, once, from 0 to 1, and a version 1 reader's `version` check accepts 0 and 1. A
catalog a version 1 driver has only read, recorded into, stopped or truncated stays at
version 0, and a version 0 driver still opens it. A truncate under a version 0 header leaves, while it
cuts, a stop of −1 that version 0's text calls live; the bytes are the same bytes, every
reader's rule for them is the same, the stop is in the files, and a version 0 driver
started on such a catalog rebuilds the stop a version 1 driver would, at the boundary
the cut reached, so version 0's wording, "−1 while the recording is live", is amended
at acceptance to "live or being cut", and no reader changes. A version 0 driver refuses
a version 1 header with `version`, the check it runs today, since the catalog is read by
the driver alone and a downgrade is not an upgrade (invariant 3's mixed versions are a
client and a driver).

`purge segments` on a slot in version 0's form migrates it first, in the one order a
crash cannot hurt: the driver, which has checked the slot under version 0's range,
writes the creation checksum over version 1's range, then writes the group, the position
and then its checksum. This is the one rewrite of the creation checksum ADR 0024
forbade, and it signs no new byte: the bytes under the new checksum are the bytes the
old one vouched for, less the twelve of the group, which it vouched were zero. A crash
after the checksum and before the group leaves a slot in version 1's form whose retained
checksum fails, since the CRC32 of eight zero bytes is not zero, and the driver rebuilds
the group as it rebuilds a torn stop: the retained position is derived from the files,
the larger of the start position and the base of the lowest segment file present, or
the start position when there is none, and written again under its checksum. The same
rule repairs a purge torn between the position and its checksum, since the files are
removed only after the catalog write, so the lowest file present is at the old retained
position. A reader other than the driver retries a failing retained checksum for 100
milliseconds and then reports `checksum`, as for the stop checksum. Recovery's rules
that count from the start position, the empty recording, the missing segment that is
damage and the segment the scan may read, count from the retained position in version
1, and segment files before the retained position are removed, not read.

The replayer thread reads the slot and the files for a command's checks, as it reads
the slot for `start replay`, and does the file work: it already opens and reads segment
files and holds no replay most of the time (ADR 0030), the conductor never touches the
disk, and the catalog is written through the recorder, which owns the catalog writer,
one slot and a sync, as a stop is written today. The checks that need the disk run
before the first catalog write, so a refused command writes nothing and needs no way
back. A command runs in this order. The conductor checks what it holds in memory: a
recording id below 0 or at or past the next recording id, the slot count its last
reply refreshed, is code 21; a recording the recordings registry holds, live or
stopping, one a replay in the replays registry is reading, or one a command in
progress names, is code 24; the commands in progress are a registry of
`spec/client-protocol.md`, "Sizes", with the replays registry's capacity, 64, and that
registry at capacity, or the replayer's request queue full or holding requests still
waiting for room, is code 10, adding one to `driver.registry_full`, as `start replay`
answers. It enters the command in the registry and hands it to the replayer through
the request queue. The replayer reads the slot under the two-range rule above: a state
other than 1 is code 21, with the state in the message; a position outside the bounds,
the retained position, the stop position and, for a purge, the segment base rule from
the descriptor's start position, term length and segment file length, is code 23, with
the bounds in the message; for a truncate at a position below the stop it reads the
frame header at the position from its segment file, and a header that fails the
`frame` check there, or a frame length of 0, is code 23 with "no frame begins at the
position" in the message, while a segment file it cannot open or read is code 12. A
refusal goes back to the conductor on the reply queue, which answers the client and
frees the registry entry. A pass goes to the recorder for the first catalog write, the
stop of −1, the retained group or the state 3, a slot in version 0's form migrated
first as above; on the recorder's reply the conductor hands the file work to the
replayer; on the replayer's reply a truncate's third step is one more slot write
through the recorder; and the conductor answers the client when the last reply
arrives, as a replay is started. The replayer does a command's file work one file per
duty cycle, one removal, or the cut and extension of the segment that holds the
position, with the directory sync it needs, so that the replays it holds wait at most
one file's work per cycle, and it runs those cycles back to back while a command is in
progress, as it does while a replay is live, so a command over n files answers after n
cycles, a thousand segments within a second on the bench hosts' NVMe drives; the
client's 10 second response timeout thereby bounds one command to the order of ten
thousand segment files, over a terabyte at the fixed segment file length, and
`cavalry doctor`'s free-space check names the purge in segments, so an operator sends
a purge of that size as several *(acceptance)*. When the recorder's or the replayer's request queue
has no room for a later hop, the conductor keeps the command in its registry entry and
offers that hop again on its next duty cycle, in admission order, so the registry
bounds what waits and no hop is dropped or sent twice; the first hop alone is refused
at admission, with code 10, since nothing has been written then. The repair after a failed file work runs on the
replayer, which has the files open, before it replies. Between the replayer's checks
and the first catalog write nothing changes the slot: the recording is stopped, so the
recorder writes nothing to it on its own, and the registry entry refuses the other
three commands and `start replay`. Nothing runs on the recorder thread or the
conductor's duty cycle beyond a catalog write of one slot, which `start recording` and
`stop recording` already do. The catalog writer gains one transition, stopped to
cutting, −1 and −1, to stopped, for a truncate; every other transition stays refused.

Counters: `recording.truncated`, `recording.segments_purged`, `recording.deleted` and
`recording.retention_failed`, which counts a command the files refused, a frame header
the replayer could not read before the catalog write or a file work it could not finish
after it, the two code 12 paths above, as counters; and one more instance gauge
per live recording, `recording.bytes`, the bytes its segment files take on disk, kept
by the recorder at each roll, keyed and labelled as `recording.position` is and freed
with it after the stop. The three commands act on stopped recordings, whose gauges are
gone; `cavalry doctor` computes a stopped recording's bytes from its descriptor, the
segments from the retained position's to the stop position's times the segment file
length, with no file read. Recovery's two new outcomes count in the counters the
recorder's feature gave it: a retained group rebuilt from the files adds one to
`recording.recovered`, as a rebuilt stop does, and a segment file before the retained
position removed adds one to `recording.files_removed`, as a file past the stop does
*(acceptance)*. The counters-file arithmetic becomes 38 statics, the 34 `counters.toml`
holds with `scope = "static"` and these four, plus 2048 per publication pair, 2048 per
image, 3072 per recording with the gauge, 64 per replay and 256 per replay publication
(ADR 0036), 7526, under 8192; the "37 statics" that `spec/client-protocol.md`, "Sizes",
counted was corrected to 34 by ADR 0036's change, and this change writes 38. `cavalry doctor` gains a check that
reads the free space of the log directory's filesystem and, under 10 percent, names the
recording and the segment that would free the most, with the `purge segments` command
that removes it, and a check that names a recording whose bytes, from the gauge while
it is live and from its descriptor once stopped, are over half the filesystem.

No configuration key. The card allowed `log.retain` only if a default could not be
right, and no default can: a retention that deletes a user's recording on a schedule the
user did not set is a lie about delivery's record (invariant 1), and `crates/cavalry-log`
promises retention is explicit. The soak host (card 159) runs `purge segments` from a
script on its own schedule, which is what an operator does.

The `cavalry` binary: `record --truncate <id> --to <position or instant>`, `record
--purge <id> --before <position or instant>` and `record --delete <id>`, instants
resolved through the time index as `replay` resolves them. An instant for `--to`
resolves to an index entry's position, a frame boundary the recorder wrote, which the
frame check accepts. An instant for `--before` resolves to a position that is almost
never a segment base, so the CLI rounds it down to the segment base at or before that
position, from the descriptor's start position, term length and segment file length,
so that no byte recorded after the instant is purged, and prints the position the
instant resolved to and the base it sends; a base at or below the retained position is
nothing to purge, which the driver refuses with code 23 and the CLI prints with the
retained position, so the operator sees that an earlier purge already took it. The
client API gains
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
- **Version 1's creation checksum over bytes 28 to 1024, as version 0's, re-signed by
  `purge segments`.** Lost: a purge torn between the group and the checksum leaves a
  creation checksum that fails, which ADR 0024 reads as damage and sets a healthy
  recording to state 2, the harm the version bump exists to avoid.
- **The retained group hidden in version 0's reserved bytes, with no version bump.**
  Lost: a version 0 driver would read a purged recording's missing head as damage and
  invalidate a healthy recording, and the group lies under version 0's creation checksum
  anyway (above).
- **A slot version in the reserved i32 at offset 28, read to pick the checksum range.**
  Lost: that word sits inside version 0's creation range, so flipping it on a version 0
  slot changes the bytes under the old checksum before the new one is written, and no
  order of the two writes leaves a slot that holds at every crash point; checking the
  two ranges in turn needs no flag, and a flipped bit anywhere still fails both.
- **Migrating every slot at start-up when the header says 0.** Lost: two writes and a
  sync per slot, for up to 1048576 slots that may never be purged, at a start-up that
  must stay bounded; and the reader needs both ranges anyway, to read a catalog whose
  migration a crash cut short.
- **A truncate that writes its new stop under the stop checksum first and cuts the
  files after.** Lost, and it was this page's first draft: a crash between the two
  leaves a valid stopped slot with segment files past its stop, non-zero bytes past the
  stop and index entries past it, which recovery never looks at, since it rebuilds only
  a stop whose checksum fails or is −1, so the format's rules would be broken for the
  life of the recording.
- **A recovery pass over every valid stopped slot at start-up, to find a torn truncate.**
  Lost: a stat per segment file is cheap, but a stale tail past the stop can be found
  only by reading up to a segment per recording, at every start-up, for a crash that may
  never have happened; a stop of −1 during the cut uses the pass that already exists and
  costs a healthy slot nothing.
- **A stale stop checksum as the marker of a cut in progress.** Lost: a reader reads a
  failing stop checksum as damage after 100 milliseconds, so `cavalry doctor` would name
  a recording being cut as damaged; a stop of −1 is the format's word for "the stop is
  in the files", which is true during the cut.
- **Removing the segments past the position from the lowest base up.** Lost: a crash
  after the first removal leaves a segment missing between the start position and the
  last segment file, which recovery reads as damage and marks state 2, invalidating a
  healthy recording (invariant 1); from the highest base down the files left are
  contiguous at every point, and recovery's stop is the end of the highest file left.
- **Zeroing the tail from the position forwards.** Lost: a crash midway leaves whole
  frames past a run of zeros, and recovery's scan, which starts at the last index entry
  whose frame checks, would set the stop past the hole; a cut of the file to the
  position's offset and an extension back leaves whole frames up to one boundary at
  every point.
- **The file work on the conductor.** Lost: `unlink` of a 128 MiB file and `ftruncate`
  are syscalls of unbounded length on a filesystem under load, and the conductor's duty
  cycle sets the publishers' limits; the replayer thread is the driver's disk thread and
  idles between replays (ADR 0030).
- **The position checks on the conductor, from the slot.** Lost: the bounds are in the
  catalog slot and a truncate's frame header is in a segment file, and the conductor
  never touches the disk (ADR 0030); a copy of every stopped slot in the conductor's
  memory would be a second catalog, bounded at a gigabyte, kept in step with the first.
- **The position checks on the replayer after the first catalog write, on the file
  work's visit.** Lost: a truncate refused there has reopened its stop to −1, so the
  refusal needs the old stop written back under its checksum, a second slot write with
  a sync, and `cavalry doctor` reads a healthy recording as being cut in between; a
  check before the write refuses with nothing written, and costs one more pass through
  the replayer's queue, off every data-plane thread.
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
three commands and error code 24; the vectors of `spec/vectors/durable-log.py` for a
version 1 header, a slot in each form under it, a slot in version 0's form that a
version 1 reader reads with its start position as retained, and a retained group whose
checksum fails; the catalog's version 1 read of a version 0 file and the refusal the
other way, with `version`; recovery tests for a truncate cut at each of its steps
(before the file work, after one of several segments past the position is removed,
with the stop rebuilt at the end of the highest file left, after all of them are
removed, after the cut and before the extension, after the index is truncated) and the
stop each rebuilds to;
for a migration cut after the creation checksum and before the group, and a purge cut
between the position and its checksum, each rebuilding the group from the files; for a
purge cut after the catalog write, whose leading files are removed at start-up; and for
a torn delete; a conductor test for each refusal the conductor makes (live, replayed, in
progress, `start replay` during a command, the registry at capacity, an id past the
next) and a replayer test for each the replayer makes (a slot not valid, out of range,
no frame at the position, a segment file it cannot read, with code 12 and its counter),
each showing the slot unwritten; a test of each success with its counter, of a truncate
at a segment base, which removes the file at that base, and of a failed file work
answered with code 12 after the slot is repaired; `start replay` with −1 after a purge, starting
at the retained position, and with a position below it, refused with code 23 naming it;
the bench rows with the recorder idle, unchanged, and with a purge running on the
replayer thread during the recorded rows; `just quickstart` recording and replaying on
both runners, which this ADR's commit already has.

## Consequences

Easier: a log directory is bounded by the operator's commands, a recording can be cut
to what matters, and a soak host runs for days with a script that purges behind the
replays it has done. Harder: the catalog holds slots in two forms for the life of the
directory, read with up to two CRC32 passes each, and its header's version is the one
field that changes after the magic; the replayer thread has two more kinds of request,
a command's checks and its file work, and the conductor one more registry; recovery has
two more rules, the files before the retained position and the retained group rebuilt
from the files; and a recording being cut reads as live, stop −1, to `cavalry doctor`
for as long as the cut takes. A torn retained group is rebuilt from the files, so a
crash inside a purge's catalog write that coincides with the loss of the recording's
leading segment files would read that loss as the purge: a double fault, accepted as
ADR 0024 accepts the tail after a power loss. We now maintain three commands, an error
code, a registry, four counters, a gauge, two doctor checks and three CLI forms.

Revisit: when a user asks for retention by age or size, for the key and its default;
when the third slice moves segments between hosts, for detach and migrate; when a
version 0 driver must read a version 1 catalog, for a downgrade path; when the first
operator reads a `recording busy` refusal and asks for a force; when a per-frame
checksum lands (ADR 0024's data checksum kind), which would close the double fault
above.

## References

- `spec/client-protocol.md` version 3 and `spec/durable-log.md` catalog version 1,
  written with the acceptance; `spec/vectors/client-protocol.py` and
  `spec/vectors/durable-log.py`; ADR 0023 (the slice), ADR 0024 (the format, state 3),
  ADR 0025 (recording), ADR 0030 (replay, the replayer thread), ADR 0035 (an empty
  replay), ADR 0036 (the stop handed to the replayer, which a truncate's reopened stop
  travels by).
- Aeron Archive: `AeronArchive.truncateRecording`, `purgeRecording`, `purgeSegments`,
  `detachSegments`, `ArchiveTool`.
- Card 95, card 159 (the soak host that runs the purge).
