# The durable log, version 0

The on-disk record of a stream: what the driver writes when it records a publication,
and what `cavalry replay` and `just replay-check` read back. A directory, the driver's
`log.dir`, holds a catalog of recordings, the segment files of each recording, and a
time index per recording. The catalog and the time index each start with a header that
carries a magic and a version, and a reader rejects a version it does not know; a
segment file has no header, and the catalog's version governs it. In Aeron, this is
Archive's catalog and recording segments
([glossary](../docs/glossary.md)). Decided in ADR 0024; the slice is ADR 0023.

Integers are little-endian; offsets and lengths are in bytes; signed fields are two's
complement; a timestamp is nanoseconds since the Unix epoch as an i64. A position is
the stream's position as `spec/log-buffer.md`, "Positions", defines it: a byte count
from the start of the stream, a multiple of 32.

## Files

| File | What it holds |
| --- | --- |
| `catalog` | The header and one 1024-byte slot per recording; a recording's id is its slot's index. |
| `<recording id>-<segment base position>.rec` | A segment of the recording: the log buffer's raw term bytes from the segment base position, frame headers and padding included. |
| `<recording id>.idx` | The time index: one (timestamp, position) entry at the start of the recording and then at most once per millisecond while the position moves. |

A recording id and a segment base position are written in decimal with no sign and no
leading zeros. Nothing else lives in the directory; a reader ignores files it does not
name.

## Catalog header

The first 128 bytes of `catalog`. The creator writes every field, then the magic last;
a reader loads the magic first, and eight zero bytes mean `not-ready`. The header never
changes after the magic is written.

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the bytes `43 56 4C 59 43 54 4C 47` (`CVLYCTLG`) |
| 8 | 4 | version | i32 | 0 |
| 12 | 4 | descriptor length | i32 | 1024 |
| 16 | 4 | reserved | i32 | written as 0, never read |
| 20 | 4 | data checksum kind | i32 | 0: version 0 carries no checksum beside the frames |
| 24 | 8 | created | i64 | a timestamp, 0 or more |
| 32 | 96 | reserved | bytes | written as 0, never read |

The catalog holds n whole slots after the header, for an n from 0 to 1048576, the
catalog's bound, fixed by this version; a reader ignores bytes past the last whole
slot, which a crash while the file grew can leave, and the driver truncates them at
start-up. The catalog grows by one slot at a time, a slot is never reused, and ids are
given in order, so a recording id is unique for the life of the directory. At the
bound `start recording` is refused with `catalog full`, and the operator starts a new
directory; `cavalry doctor` warns from 90 percent of it.

## Descriptor

Slot i is at 128 + i × 1024 and describes recording i.

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 4 | state | i32 | 0, empty, the slot's other bytes are not read; 1, valid; 2, invalid, a recording the driver could not repair and `cavalry doctor` names; 3, deleted |
| 4 | 4 | stop checksum | u32 | the zlib CRC32 of bytes 8 to 24 |
| 8 | 8 | stop timestamp | i64 | −1 while the recording is live; otherwise at least the start timestamp |
| 16 | 8 | stop position | i64 | −1 while the recording is live; otherwise at least the start position, a multiple of 32 |
| 24 | 4 | creation checksum | u32 | the zlib CRC32 of bytes 28 to 1024 |
| 28 | 4 | reserved | i32 | written as 0, never read |
| 32 | 8 | recording id | i64 | i |
| 40 | 8 | start timestamp | i64 | 0 or more |
| 48 | 8 | start position | i64 | 0 or more, a multiple of 32 |
| 56 | 4 | initial term id | i32 | any; the log buffer's |
| 60 | 4 | segment file length | i32 | a power of two, at least the term length and at most 1073741824 |
| 64 | 4 | term length | i32 | a power of two from 65536 to 1073741824; the log buffer's |
| 68 | 4 | MTU length | i32 | a multiple of 32 from 64 to 65504; the log buffer's |
| 72 | 4 | session id | i32 | any; the publication's |
| 76 | 4 | stream id | i32 | any; the publication's |
| 80 | 4 | channel length | i32 | 0 to 256 |
| 84 | 256 | channel | bytes | the channel as UTF-8, zero padded |
| 340 | 4 | source identity length | i32 | 0 to 128 |
| 344 | 128 | source identity | bytes | the publisher's identity as UTF-8, zero padded |
| 472 | 552 | reserved | bytes | written as 0, never read |

The fields fall in two groups, each under its own checksum: the stop fields, bytes 8 to
24, under the stop checksum; and the creation fields, bytes 28 to 1024, written once
when the slot is created and never again, under the creation checksum. The state is
the slot's commit word and lies outside both checksums on purpose: it is
written last, in one four-byte store. While a recording is live only the stop fields
and the state change: a stop writes the stop timestamp and the stop position, then the
stop checksum; a delete writes the state, 3. A reader that finds a state of 1, 2 or 3
checks its state first: 2 is `invalid`, a recording the driver found damaged, and the
reader reads nothing else from the slot. For 1 and 3 it checks both checksums. A
creation checksum that does not match is damage, since nothing
writes those bytes after the state: the reader reports `checksum` at once. A stop
checksum that does not match may be a stop the driver is writing, so the reader retries
for up to 100 milliseconds and then reports `checksum`. Either way it reads nothing from
that slot, and only the driver repairs it, as the section "The driver" says. A deleted
recording keeps its slot and loses its files, so an old id never names a new recording.

## Segment files

A recording's bytes are the log buffer's own, copied term by term: from the start
position, every frame as the publisher committed it, header and body, padding frames
included, so that a position in the recording is the position in the stream, and the
frame at a position has the term id and term offset the position computes
(`spec/log-buffer.md`, "Positions") and the session id and stream id of the descriptor.
A reader that finds anything else at a position, or a position past the end of its
segment file, reports `frame`. A segment file has no header, no magic and no version
field: it is term bytes from byte 0, so that a recorded position maps to a file offset
with no adjustment; the per-frame check is its only check.

The recording is split into segments of the segment file length, each in a file named
`<recording id>-<segment base position>.rec`. Segment base positions follow Aeron's
rule: the first is the start position's term base, the start position minus its term
offset, and each next one is a segment file length further; byte p of the stream is at
offset p − base in the segment whose base is the largest at most p. A segment file is
extended to the segment file length when it is opened, and the bytes before a
recording's start position, at most one term of them, and after its stop position, are
zero. The segment file length is a power of two of at least the term length, fixed at
134217728 in this version for the 64 MiB IPC term and recorded in the descriptor, so a
term never crosses a segment.

## Time index

`<recording id>.idx` starts with a 128-byte header and continues with 16-byte entries.

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the bytes `43 56 4C 59 54 49 44 58` (`CVLYTIDX`) |
| 8 | 4 | version | i32 | 0 |
| 12 | 4 | reserved | i32 | written as 0, never read |
| 16 | 8 | recording id | i64 | 0 or more; the slot in the catalog |
| 24 | 104 | reserved | bytes | written as 0, never read |

Entry j is at 128 + j × 16: a timestamp (i64) and a position (i64, 0 or more, a
multiple of 32, at a frame boundary the recorder has written). The recorder appends the
first entry when the recording starts, at the start position, then an entry at most
once per millisecond while the position moves, and one when the recording stops;
timestamps and positions never decrease from one entry to the next, and the last
entry's position is at most the stop position. The file holds n whole entries after
the header; only the last entry can be part-written, so a reader ignores bytes past
the last whole entry and a last entry that breaks the order, and recovery truncates
the file to whole entries and drops such an entry.
`cavalry replay --from <instant>` starts at the position of the last entry whose
timestamp is at or before the instant, so an instant after the last entry resolves to
the last entry's position, or at the start position when the instant is before the
first entry, and the replay reports the position it starts at; `--to <instant>` ends at
the position of the first entry whose timestamp is after the instant, or at the last
entry's position when there is none; whether a replay of a live recording follows it is
the replay's rule, not the format's. The granularity of an instant is the recorder's
cadence, one millisecond. The index and the segments grow with the recording until
retention removes them.

## The driver

The driver is the directory's only writer; how it keeps a second driver out is its own
rule, not this page's. It creates a slot in this order: the file grows by one zeroed
slot, the creation fields are written and then the creation checksum, the stop fields
are written at −1 and then the stop checksum, and the state, 1, last; a slot whose state
is 0 is empty whatever its other bytes hold, so a crash during creation leaves an empty
last slot. The recording's id is reported, and its files created, only after the state is
1. With one writer only the last slot can be empty, and the next recording id is the
number of whole slots after recovery.

When the driver starts, and again at every `stop recording` for the slot it closes, it
repairs the catalog. At start-up it first truncates the file to whole slots, then drops
a last slot whose state is 0; an empty slot before the last cannot come from one writer,
so it is damage: it gets state 2 and `cavalry doctor` names it, and no recording takes
it. Then, for every slot that is not empty: a creation checksum that does not match is
damage the files cannot repair, since the segments confirm the session id, the stream
id, the initial term id and the start position but not the start timestamp, the term
length, the MTU length, the segment file length or the channel; the slot gets state 2
and `cavalry doctor` names it, and recovery never writes a creation field or the
creation checksum. A valid slot whose creation checksum holds but whose stop checksum
does not, or whose stop position is −1, was torn during a stop or live at a crash, and
its stop fields are derived wholly from the files, in this order. First the time index:
one that is missing, too short for its header, or whose magic, version or recording id
fails its check was torn while the recorder created it, and the driver writes it again,
the header with its magic last and one entry, the start timestamp at the start
position; an index with a header and no whole entry gets that entry; then the index is
truncated to whole entries and a last entry that breaks the order is dropped. Second,
an empty recording: when no segment file of the recording exists, or the segment
holding the start position exists and the frame length there is 0, the recorder
crashed before the first frame; the stop position is the start position, the stop
timestamp is the start timestamp, and the slot stays valid. Third, the scan's start:
the driver walks the index back from its last entry to the first entry at whose
position a frame checks, and starts there, or at the start position when none does;
after a power loss an entry can point past what its segment kept, since the two files
are synced apart, and walking back skips it. Fourth, the scan: from that position it
reads frame by frame, to the end of the last segment file at most, and stops at the
first frame that fails the `frame` check or whose length is 0; the stop position is the
end of the last frame that checks; index entries past it are dropped; the stop
timestamp is that of the last entry left; and it writes the stop fields and then the
stop checksum; segment files that lie wholly past the stop position, such as a next
segment pre-extended before a power loss, are removed. Only damage gets state 2 and a
`cavalry doctor` finding: segment files exist but the one holding the start position is
missing, a segment between the start position and the last segment file is missing, so
that the scan would cut off data that is on disk, or the frame at the start position has
a non-zero length and fails the check. Every action recovery takes, a truncation, a
dropped slot, a rewritten index, a rebuilt stop, a slot set to state 2, is a state
transition with a counter of its own, which the recorder's feature adds (ADR 0023,
card 93). A slot with state 3 whose files are
still present loses them again. After a process
crash every byte the recorder wrote is in the page cache and the scan finds it; after an
operating-system crash or a power loss, bytes past the last sync are not guaranteed, and
this version cannot tell a frame whose header survived but whose body did not from a
whole one. A `start recording` whose channel is longer than 256 bytes or whose source
identity is longer than 128 is refused with a named error before any slot is written.

## What a reader rejects

A reader checks each file in this order and reports the first failure by name, so that
every implementation reports the same reason for the same bytes.

| Check | Reported when |
| --- | --- |
| `too-short` | The catalog or the index is shorter than 128 bytes. |
| `not-ready` | The magic is eight zero bytes: the creator has not finished. |
| `magic` | The magic is any other value than the file's. |
| `version` | The version is not 0. |
| `descriptor-length` | The catalog's descriptor length is not 1024. |
| `checksum-kind` | The catalog's data checksum kind is not 0. |
| `timestamp` | A timestamp is below 0, or a stop timestamp that is not −1 is below its start timestamp. |
| `length` | The catalog holds more than 1048576 whole slots. |
| `state` | A descriptor's state is not 0 to 3. |
| `invalid` | A descriptor's state is 2: the driver found the recording damaged, and `cavalry doctor` names it. |
| `checksum` | A descriptor's creation checksum is not the zlib CRC32 of bytes 28 to 1024 of its slot, or its stop checksum is not that of bytes 8 to 24. |
| `recording-id` | A descriptor's recording id is not its slot's index, or an index header's recording id is below 0. |
| `position` | A position is below 0 or not a multiple of 32, or a stop position that is not −1 is below its start position. |
| `term-length` | The term length is not a power of two from 65536 to 1073741824. |
| `segment-length` | The segment file length is not a power of two, is below the term length, or is above 1073741824. |
| `mtu` | The MTU length is not a multiple of 32 from 64 to 65504. |
| `string` | The channel length is above 256, the source identity length is above 128, or the bytes are not UTF-8. |
| `order` | An index entry's timestamp or position is below the entry before it. |
| `frame` | The frame header at a position does not carry the position's term id and term offset and the descriptor's session id and stream id. |

## Vectors

`spec/vectors/durable-log.json`, written by `spec/vectors/durable-log.py`: catalog header
vectors with the file length the reader sees and the whole slots it holds; descriptor
vectors with the slot index;
index header and entry vectors, an entry vector with the entry before it; and position
vectors, arithmetic, from a descriptor's fields and a position to the segment file name,
the segment base position, the offset in the segment, the term id and the term offset.
