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
instant; the same state from the same log, byte for byte; a version in every file
header; a bound on everything.

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

The durable log is a directory, the driver's `log.dir`, holding three kinds of file,
each versioned and specified in `spec/durable-log.md` version 0 with vectors:

1. **The catalog, `catalog`.** A 128-byte header: magic `CVLYCTLG`, version 0, the
   descriptor length (1024), the maximum number of descriptors (the catalog's bound,
   8192 by default), the data checksum kind (0 in version 0, meaning none; a reader
   rejects any other value), the creation timestamp, reserved to 128. Then fixed slots
   of 1024 bytes: descriptor i at 128 + i × 1024, and its recording id is i, so that
   an id is a slot and no header field changes after creation. A descriptor holds its
   state (0 empty, 1 valid, 2 invalid, 3 deleted), a CRC32 (the zlib polynomial, over
   bytes 8 to 1024 of the slot), the recording id, the start and stop timestamps
   (nanoseconds since the Unix epoch; stop is −1 while live), the start and stop
   positions (stop is −1 while live), the initial term id, the segment file length, the
   term length, the MTU length, the session id, the stream id, the channel as a length
   and up to 256 bytes, the source identity as a length and up to 128 bytes, and
   reserved bytes to 1024. The file grows by one slot at a time, written zero then
   filled with the state last; `start recording` with every slot used is refused with
   `catalog full`. A descriptor changes only in its stop fields and its state, each
   change followed by a new CRC32; a reader whose CRC32 does not match reports
   `checksum` for that descriptor and skips it.
2. **The segment files, `<recording id>-<segment base position>.rec`.** Aeron's: the
   log buffer's raw term bytes from the segment base position, frame headers and
   padding included, each file extended to the segment file length when opened, a power
   of two that is a multiple of the term length, 128 MiB by default for the 64 MiB IPC
   term. A segment base position is a multiple of the segment file length; a recording
   that starts inside a segment leaves the bytes before its start position zero. A
   recorded position is the stream's position, so the frame at a position is valid when
   its header's term offset, term id, session id and stream id are the position's and
   the descriptor's; a reader rejects anything else with `frame`.
3. **The time index, `<recording id>.idx`.** A 128-byte header: magic `CVLYTIDX`,
   version 0, the recording id, reserved; then entries of 16 bytes, a timestamp and a
   position, appended by the recorder at the start of the recording and then at most
   once per millisecond while the position moves, timestamps and positions both
   non-decreasing. `cavalry replay --from <instant>` starts at the position of the last
   entry whose timestamp is at or before the instant; the granularity of "between two
   instants" is the recorder's cadence, a millisecond, and the index's size is bounded
   by it, 16 KiB per second of recording at most.

Every file's header is written in full with the magic last; every reader checks, in this
order, `too-short`, `magic`, `version`, then the field bounds the page tables, and
reports the first failure by name, so that every implementation reports the same reason
for the same bytes. Version 0 carries no per-frame checksum: the data checksum kind is
reserved for a version that adds one beside the frames, never inside them.

## Alternatives considered

- **Aeron's variable-length descriptors, grown and shifted in place.** Lost: a fixed
  slot gives a recording id that is its index, a header that never changes, a bound
  stated in one field, and vectors a page can show in full; the strings it fixes
  (channel, source identity) are bounded by the client protocol already.
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
by `spec/vectors/durable-log.py` and read by `cavalry-log`'s tests, a no-panic test over
generated input for the catalog and index parsers, and Rjae's acceptance, since the
format cannot be undone. What the implementation proves goes in the commit messages of
cards 92 to 94.

## Consequences

Easier: a stranger reads every byte of a recording from one page; a recorded position
is the stream's position, so replay and `just replay-check` compare bytes; an instant
resolves to a position in one index lookup.

Harder: three files per recording to keep consistent, with the catalog's stop fields
and the index's last entry both written by the recorder; a bound of 8192 recordings
per catalog, which card 95's retention reuses by deleting.

Revisit: when a checksum beside the frames is needed, as version 1 with its own ADR;
when the UDP slice records a channel longer than 256 bytes or a source identity longer
than 128; when a recording must span more than 2^31 slots of 1024 bytes.

## References

- `spec/durable-log.md` (written at acceptance), `spec/log-buffer.md` ("Positions"),
  `spec/frame.md`, `spec/counters-file.md` ("Header")
- ADR 0012, ADR 0013, ADR 0014, ADR 0023; `crates/cavalry-log/README.md`
- Aeron, `aeron-archive/src/main/java/io/aeron/archive/{Archive,Catalog,
  RecordingWriter,ReplaySession}.java`, read 2026-10-13
- AppShapes Trello card 92
