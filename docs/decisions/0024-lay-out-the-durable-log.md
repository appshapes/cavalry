# 0024. Lay out the durable log as a fixed-slot catalog, Aeron's segment files and a time index

- **Status:** Proposed
- **Date:** 2026-10-13
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none

## Context

Card 92, the first feature of the second slice (ADR 0023), is the on-disk format of the
durable log: the bytes every implementation of Cavalry must agree on, decided before any
code, because a recorded log outlives the binary that wrote it and `CLAUDE.md` says a
decision that cannot be undone is written up and stopped at, not chosen. What the format
must carry comes from the crate's README and ADR 0023: a stream recorded to disk by the
driver as an ordered, replayable log; replay from a position; `cavalry replay` from an
instant; the same state from the same log, byte for byte; a version that governs every
file, carried in the file's header where it has one; a bound on everything.

Aeron Archive, read from its source on 2026-10-13: a recording's data is the log
buffer's raw term bytes, frame headers and padding included, written in blocks into
segment files named `<recordingId>-<segmentBasePosition>.rec`, each a power of two of
at least the term length (128 MiB by default) and extended to its full length when
opened, so that a recorded position is the stream's position and a replay copies frames
as they were, rewriting only the session and stream ids. Its catalog, `archive.catalog`,
is a header (version, length, next recording id, alignment) followed by variable-length
descriptors packed back to back, each with a length, a state (valid, invalid, deleted)
and a checksum, then the body (recording id, start and stop timestamps, start and stop
positions, initial term id, segment file length, term length, MTU length, session id,
stream id, the stripped and original channels and the source identity as
length-prefixed strings), grown by 1.5× when full and rewritten in place when a
descriptor changes size. Its optional per-frame checksum is stored in each frame's
session-id field. It has no time index: `startTimestamp` and `stopTimestamp` are the
only instants a recording carries, so "between two instants" is not a question it
answers.

Cavalry's other layouts (ADR 0012, 0013, 0014) each start with a 128-byte header: an
eight-byte magic written last, a version, then the fields, and the checks a reader runs
in a fixed order with a fixed name for each failure, with vectors a Python script
writes. The log buffer's positions (`spec/log-buffer.md`, "Positions") are multiples of
32 bytes within a term of a power-of-two length.

## Decision

The durable log is a directory, the driver's `log.dir`, holding three kinds of file, all
specified in `spec/durable-log.md` version 0 with vectors. The catalog and the time
index each start with a header that carries a magic and a version. The segment files
have no header (item 2); the catalog's version governs them.

1. **The catalog, `catalog`.** A 128-byte header: magic `CVLYCTLG`, version 0, the
   descriptor length (1024), the data checksum kind (0 in version 0, meaning none; a
   reader rejects any other value), the creation timestamp, reserved to 128. No header
   field changes after creation. Then fixed slots of 1024 bytes, slot i at 128 + i ×
   1024, and recording i's descriptor is slot i: an id is a slot, ids are given in
   order, and a slot is never reused, as Aeron's `nextRecordingId` never goes back. The
   catalog's bound is the format's, 1048576 slots (1 GiB of slots), not a header field:
   at the bound `start recording` is refused with `catalog full`, whose message tells
   the operator to start a new `log.dir`, and `cavalry doctor` warns from 90 percent of
   it. A deleted recording loses its files and keeps its slot, so an old id never names
   a new recording and a leftover file never collides with a new one's name. The
   catalog has one writer, the driver that owns `log.dir`, which holds an advisory lock
   on the file while it runs so that a second driver refuses to start; readers in other
   processes read it with positional reads, never a mapping.

   **A descriptor** holds its state (0 empty, 1 valid, 2 invalid, 3 deleted), two
   CRC32s (the zlib polynomial), the recording id, the start and stop timestamps
   (nanoseconds since the Unix epoch; stop is −1 while live), the start and stop
   positions (stop is −1 while live), the initial term id, the segment file length, the
   term length, the MTU length, the session id, the stream id, the channel as a length
   and up to 256 bytes, the source identity as a length and up to 128 bytes, and
   reserved bytes to 1024. The fields fall in two groups, each under its own CRC32:
   the *stop fields*, the stop timestamp and the stop position, are under the *stop
   CRC32*; every other byte of the slot except the state and the two CRC32s, reserved
   bytes included, is a *creation field*, written once when the slot is created and
   never again, and is under the *creation CRC32*. The state is the slot's commit word
   and lies outside both CRC32s on purpose: it is written last, in one four-byte store.

   **Creating a slot.** The writer is the only writer, so it creates one slot at a time
   and only the last slot can be empty. The next recording id is the number of whole
   slots after recovery (below). The writer grows the file by one zeroed slot, writes
   the creation fields and the creation CRC32, then the stop fields at −1 and the stop
   CRC32, and writes the state, 1, last. A slot whose state is 0 is empty whatever its
   body holds, so a crash during creation leaves an empty last slot, which recovery
   removes and the next recording takes again. The recorder reports the id to the
   client, and creates the recording's files, only after the state is 1, so an id is
   never seen before its slot is valid.

   **Changing a slot.** While a recording lives only its stop fields and its state
   change: a stop writes the stop position and the stop timestamp, then the stop CRC32;
   a delete (card 95) writes the state, 3, then removes the files. A reader that finds
   a state of 1, 2 or 3 checks both CRC32s. A creation CRC32 that does not match is
   damage, since nothing writes those bytes after the state: the reader reports
   `checksum` at once. A stop CRC32 that does not match may be a stop the driver is
   writing, so the reader retries for up to 100 milliseconds and then reports
   `checksum`. Either way it reads nothing from that slot, and only the driver repairs
   it. Nothing a reader does mistakes a racing stop for damage, and nothing it does
   hides damage.

   **Recovery.** When the driver starts, and again at `stop recording` for the slot it
   closes, it repairs the catalog. At start-up it first truncates the file to whole
   slots, 128 + n × 1024 bytes, which drops a slot part-grown at a crash, and then
   drops a last slot whose state is 0; the next recording id is then n. A reader ignores
   bytes past the last whole slot. An empty slot before the last cannot come from one
   writer, so it is damage: recovery sets it to state 2, `cavalry doctor` names it,
   and no recording takes it. Then, for every slot that is not empty:
   - A creation CRC32 that does not match is damage that the files cannot repair: the
     segment files can confirm the session id, the stream id, the initial term id and
     the start position, but not the start timestamp, the term length, the MTU length,
     the segment file length or the channel. The slot gets state 2 and `cavalry doctor`
     names it. Recovery never writes a creation field or the creation CRC32.
   - A valid slot whose creation CRC32 checks but whose stop CRC32 does not, or whose
     stop position is −1, was torn during a stop or live at a crash. The stop fields
     are wholly derived from the files, so the driver rebuilds them as Aeron recovers a
     stop position, in this order:
     1. *The time index first.* An index file that is missing, too short for its
        header, or whose magic, version or recording id does not check was torn while
        the recorder created it, which the creation order above allows. Recovery
        writes it again: the header, magic last, and one entry, the start timestamp
        and the start position. An index with a header and no whole entry gets that
        entry appended. Then it truncates the index to whole entries and drops a last
        entry that breaks the order (item 3).
     2. *An empty recording.* If no segment file of the recording exists, or the
        segment holding the start position exists and the frame length there is 0,
        the recorder crashed before the first frame. The stop position is the start
        position, the stop timestamp is the start timestamp, and the slot stays
        valid: it is an empty recording, as Aeron stops a recording with no data at
        its start position.
     3. *The scan's start.* Recovery walks the index back from its last entry to the
        first entry at whose position a frame checks (below), and starts there; when
        none does, it starts at the start position. After a power loss the index can
        hold an entry past what its segment kept, since the two files are synced
        apart; walking back skips such an entry, so the stop position is always at a
        frame boundary the recorder wrote. An entry at the end of the data, where the
        frame length is 0, is skipped the same way, and the earlier entry's scan
        reaches the same end.
     4. *The scan.* From that position it reads frame by frame, to the end of the last
        segment file at most, and stops at the first frame that does not check or
        whose length is 0. The stop position is the end of the last frame that
        checks. It drops index entries whose position is past the stop position. The
        stop timestamp is the timestamp of the last entry left, at or before the stop
        position, which the first entry guarantees exists. It writes the stop fields
        and then the stop CRC32.

     Only damage gets state 2, and `cavalry doctor` names it: segment files exist but
     the one holding the start position is missing, or the frame at the start position
     has a non-zero length and does not check.
   - A torn delete, state 3 with files still present, runs again.

   After a process crash every byte the recorder wrote is in the page cache and the
   scan finds it; after an operating-system crash or a power loss, bytes past the last
   sync are not guaranteed (card 93 decides the sync cadence), and version 0 cannot
   tell a frame whose header survived but whose body did not from a whole one. The
   data checksum kind is the place for that fix.
2. **The segment files, `<recording id>-<segment base position>.rec`.** Aeron's: the
   log buffer's raw term bytes from the segment base position, frame headers and
   padding included, each file extended to the segment file length when opened. The
   segment file length is a power of two that is a multiple of the term length. The
   version 0 recorder always writes 128 MiB for the 64 MiB IPC term, a fixed value and
   not a configuration key; the descriptor's field exists so that a reader never
   assumes it. The segment base position is Aeron's (`segmentFileBasePosition`):
   aligned to the start term's base position, not to a multiple of the segment length.
   With start term base = start position − (start position mod term length), the
   segment holding position p starts at start term base + ⌊(p − start term base) ÷
   segment file length⌋ × segment file length. Only the bytes of the start term before
   the start position are zero. A recorded position is the stream's position, so the
   frame at a position *checks* when its header's term offset, term id, session id
   and stream id are the position's and the descriptor's; a reader rejects anything
   else with `frame`, and a position past the end of its segment file does not check.
   A segment file has no header, no magic and no version field: like Aeron's, it is
   term bytes from byte 0, so that a recorded position maps to a file offset with no
   adjustment. Its version is the catalog's version, and the per-frame check is its
   only check; a reader does not run `too-short`, `magic` or `version` on it.
3. **The time index, `<recording id>.idx`.** A 128-byte header: magic `CVLYTIDX`,
   version 0, the recording id, reserved; then entries of 16 bytes, a timestamp and a
   position at a frame boundary the recorder has written. The recorder appends an
   entry when the recording starts, at most once per millisecond while the position
   moves, and once when it stops. Timestamps and positions are both non-decreasing.
   Only the last entry can be part-written, so a reader of a live recording ignores
   the last entry, and recovery truncates the file to whole entries and drops a last
   entry that breaks the order. `cavalry replay --from <instant>` starts at the
   position of the last entry whose timestamp is at or before the instant; an instant
   after the last entry resolves to the last entry's position by the same rule; an
   instant before the first entry resolves to the start position, and the replay
   reports the position it starts at. Whether a replay of a live recording follows it
   is card 94's decision and does not touch the format. The granularity of "between
   two instants" is the recorder's cadence, a millisecond.

The two files that have a header, the catalog and the time index, write it in full with
the magic last; a reader of either checks, in this order, `too-short`, `magic`,
`version`, then the field bounds the page tables, and reports the first failure by
name, so that every implementation reports the same reason for the same bytes. Version
0 carries no per-frame checksum: the data checksum kind is reserved for a version that
adds one beside the frames, never inside them.

`spec/durable-log.md` states the byte offsets of every field above. It decides nothing
this ADR leaves open, except the names of the failures it tables.

**String bounds.** The client protocol allows a channel of up to 4096 bytes
(`spec/client-protocol.md`, "Encoding"), so the 256-byte and 128-byte fields are not
bounds the client protocol already gives. They suffice for a different reason. Version 0
records IPC only, and its one channel, `cavalry:ipc`, is 11 bytes. A UDP channel URI
with an endpoint, an interface and a few parameters fits in 256. A source identity is
an address and a port, at most 47 bytes for a bracketed IPv6 address. `start recording`
for a channel longer than 256 bytes, or from a source identity longer than 128, is
refused with a named error that card 93 adds to the client protocol. Nothing is
truncated, and no recording starts.

## Alternatives considered

- **Aeron's variable-length descriptors, grown and shifted in place.** Lost: a fixed
  slot gives a recording id that names its slot, a header that never changes, a bound
  stated in one field, and vectors a page can show in full. The price is the string
  bounds above and the refusal beyond them.
- **Slots reused under a generation number: id = generation × N + slot, a sequence
  lock on a mutable part of the descriptor with a CRC32 of its own, and readers that
  map the catalog and retry on an odd sequence.** Lost: it brings shared-memory
  ordering into a disk file every reader must map, and a `busy` failure for a stalled
  writer; it fixes N for the life of the directory; and Aeron's ids are monotonic and
  never reused. The bound it buys, recordings kept at once rather than ever made, is
  card 95's to buy by purging files, not slots. Its recovery rules are kept above.
- **The slot index as the recording id, slots reused without a generation.** Lost: an
  id would name a different recording after a purge, so a client holding an old id
  would replay another stream, and old files would collide with new ones by name.
- **One CRC32 over the whole body, re-signed by recovery after it rebuilds the stop
  fields.** Lost: a mismatch cannot tell a torn stop from damage anywhere else in the
  body, so recovery would sign a flipped bit in the term length, the segment file
  length or the start timestamp as valid, and replay would then compute wrong segment
  names, wrong positions or a wrong `--from` position from it.
- **One CRC32 over the whole body, with recovery re-signing it only after validating
  the body against the segment files.** Lost: the files confirm the session id, the
  stream id, the initial term id and the start position, but not the start timestamp,
  the term length, the MTU length, the segment file length or the channel, so the
  validation would leave most of the body unchecked. Two CRC32s cost four bytes.
- **A CRC32 that covers the state.** Lost: the state is the commit word, written last
  and alone, so that a crash during creation leaves an empty slot and a reader never
  sees a half-written body under a valid state; a CRC32 over the state would need a
  second write after the commit word, which is the torn update the design avoids.
- **A valid slot with no files, or no frame at its start, set to state 2.** Lost: the
  creation order makes that state legitimate after a crash, so `cavalry doctor` would
  send the operator after damage that does not exist, and a recording that started
  cleanly could never be listed as valid. It is an empty recording, as in Aeron.
- **The stop position taken from the last time index entry, or the scan always
  started there.** Lost: the index and the segments are synced apart, so after a power
  loss the last entry can point past the data the segment kept, and the stop position
  would not be at a frame the recorder wrote. Walking back to an entry whose frame
  checks costs one header read per skipped entry.
- **Segments aligned to multiples of the segment file length.** Lost: a recording that
  starts in the second term of a segment would carry a zeroed leading term, 64 MiB of
  disk with nothing in it. Aeron's alignment to the start term costs nothing.
- **A database for the catalog.** Lost in ADR 0023: a dependency on the data plane and
  a format no page can show.
- **A per-frame checksum in the session-id field, as Aeron's.** Lost: a recorded frame
  is the frame as written, so a replay and a conformance test compare bytes without
  rewriting them; the checksum kind field reserves the place for a checksum beside the
  frames in a later version, with its own ADR and evidence.
- **CRC32C for the descriptor checksum.** Lost narrowly: the zlib CRC32 is in every
  language's standard library, the vectors script included, and a 1024-byte checksum off
  the message path does not need the hardware instruction.
- **Timestamps per frame.** Lost: a frame's reserved value is the application's
  (`spec/frame.md`), and changing the frame changes the log buffer and the wire.
- **The time index inside the catalog.** Lost: it grows with the recording, and the
  catalog's bound is per recording, not per millisecond.
- **A segment equal to one term.** Lost: Aeron's multiple of the term length halves the
  files to open and roll, and the default of two 64 MiB terms matches Aeron's 128 MiB.

## Evidence

A layout decision: what informed it is above. What will accept it: the vectors written
by `spec/vectors/durable-log.py` and read by `cavalry-log`'s tests, among them: an
empty last slot (state 0) with a non-zero body, which recovery removes; an empty slot
before the last, which recovery sets to state 2; a catalog with a trailing partial
slot, truncated to whole slots; a valid slot whose stop CRC32 fails, read before and
after the 100 ms retry, and the stop fields it recovers to; a valid slot whose creation
CRC32 fails, reported `checksum` at once and set to state 2, its creation fields
untouched; a slot live at a crash (stop −1) and the stop position it recovers to; a
valid slot with stop −1 and no files, which stays valid with its stop at its start; a
valid slot with stop −1 whose first segment holds only zeros, recovered the same way;
a slot that cannot be repaired (segment files present, the one holding the start
position missing), set to state 2; a deleted slot (state 3) whose files remain; a time
index with a torn header, written again with one entry; a time index whose last entry
is part-written, one whose last entry is out of order, and one whose last entry points
past what its segment holds, which the scan walks back from. Also a no-panic test over
generated input for the catalog and index parsers, and Rjae's acceptance, since the
format cannot be undone. What the implementation proves goes in the commit messages of
cards 92 to 94.

## Consequences

Easier: a stranger reads every byte of a recording from one page; a recorded position
is the stream's position, so replay and `just replay-check` compare bytes; an instant
resolves to a position in one index lookup.

Harder: three files per recording to keep consistent, with the catalog's stop fields
and the index's last entry both written by the recorder, and a recovery pass at every
start-up that rebuilds them after a crash; a lifetime bound of 1048576 recordings per
directory, after which the operator starts a new one. The catalog is bounded by the
format. The segment files and the time index are not bounded by this format: each
grows with the length of its recording, the index by at most 16 KiB per second, until
card 95's retention truncates or purges it. A channel or source identity past its field is
refused, not recorded. After a power loss, the tail past the last sync is trusted on
its frame headers alone.

Revisit: when a checksum beside the frames is needed, as version 1 with its own ADR,
and first if a power loss ever yields a recovered frame whose body is wrong; when the
UDP slice records a channel longer than 256 bytes or a source identity longer than 128;
when a directory reaches 1048576 recordings in its life, which only a new version with
a wider bound or with reuse can change.

## References

- `spec/durable-log.md` (written at acceptance), `spec/log-buffer.md` ("Positions"),
  `spec/frame.md`, `spec/counters-file.md` ("Header"), `spec/client-protocol.md`
  ("Encoding")
- ADR 0012, ADR 0013, ADR 0014, ADR 0023; `crates/cavalry-log/README.md`
- Aeron, `aeron-archive/src/main/java/io/aeron/archive/{Archive,Catalog,
  RecordingWriter,ReplaySession}.java`, read 2026-10-13
- AppShapes Trello card 92
