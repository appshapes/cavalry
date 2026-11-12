# The durable log, version 3

The on-disk record of a stream: what the driver writes when it records a publication,
and what `cavalry replay` and `just replay-check` read back. A directory, the driver's
`log.dir`, holds a catalog of recordings, the segment files of each recording, and a
time index per recording. The catalog and the time index each start with a header that
carries a magic and a version, and a reader rejects a version it does not know; a
segment file has no header, and the catalog's version governs it. In Aeron, this is
Archive's catalog and recording segments
([glossary](../docs/glossary.md)). Decided in ADR 0024; the slice is ADR 0023. Version
1 added the retained group a slot may hold (ADR 0031). Version 2 adds the terms file,
a cluster member's record of the terms it has recorded and the candidate term it has
stored, which `spec/cluster.md`, "The log", names and which the consensus module of
`cavalry-raft` writes and reads (ADR 0060); a directory with no cluster member has no
terms file, and a reader of version 1 ignores one, as it ignores every file it does
not name. Version 3 adds the log recordings file, a cluster member's record of which
recordings in the directory hold its log, which the consensus module writes before
the member reports a position a recording holds and reads when the member starts on
the directory again (ADR 0108); a directory with no cluster member has none, and a
reader of version 2 ignores one.

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
| `terms` | A cluster member's record of recorded terms and its candidate term ("Terms"); absent on a driver that is no member. |
| `log-recordings` | A cluster member's record of the recordings that hold its log ("Log recordings"); absent on a driver that is no member. |

A recording id and a segment base position are written in decimal with no sign and no
leading zeros. Nothing else lives in the directory; a reader ignores files it does not
name.

## Catalog header

The first 128 bytes of `catalog`. The creator writes every field, then the magic last;
a reader loads the magic first, and eight zero bytes mean `not-ready`. The header never
changes after the magic is written, but for the version, which the driver moves from 0
to 1 once, before the first slot it writes in version 1's form (ADR 0031; "The driver").

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the bytes `43 56 4C 59 43 54 4C 47` (`CVLYCTLG`) |
| 8 | 4 | version | i32 | 0 or 1: under 1 a slot may hold a retained group, under 0 none does |
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
| 16 | 8 | stop position | i64 | −1 while the recording is live; otherwise at least the start position, a multiple of 32; −1 exactly when the stop timestamp is −1 |
| 24 | 4 | creation checksum | u32 | the zlib CRC32 of bytes 28 to 1024 for a slot in version 0's form, of bytes 28 to 472 and 484 to 1024 for one in version 1's |
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
| 472 | 8 | retained position | i64 | version 1's form: at least the start position, at most the stop position once there is one, and the start position or a segment base; version 0's form: 0, never read |
| 480 | 4 | retained checksum | u32 | version 1's form: the zlib CRC32 of bytes 472 to 480; version 0's form: 0, never read |
| 484 | 540 | reserved | bytes | written as 0, never read |

A slot is in one of two forms, and the catalog's version bounds which. In version 0's
form the fields fall in two groups, each under its own checksum: the stop fields, bytes
8 to 24, under the stop checksum; and the creation fields, bytes 28 to 1024, written
once when the slot is created and never again, under the creation checksum. In version
1's form there are three: the stop fields as before; the creation fields, bytes 28 to
472 and 484 to 1024, under a creation checksum over those two ranges; and the *retained
group*, bytes 472 to 484, the retained position under the retained checksum, which
`purge segments` rewrites (ADR 0031). The **retained position** is where the
recording's bytes begin on disk: the start position until a purge moves it to a
segment base, and then the lowest segment base whose file is kept; it is what a replay
of the recording may start from, and a slot in version 0's form has it at its start
position without a group. Under a version 0 header every slot is in version 0's form;
under a version 1 header a slot is in either, and a reader tells them apart by the
creation checksum, never by a flag: it checks the checksum over version 1's ranges
first, and a slot that holds is in version 1's form, its group read under the retained
checksum; a slot that fails it is checked over version 0's range, bytes 28 to 1024, and
one that holds is in version 0's form, byte for byte what a version 0 driver wrote; one
that holds under neither is `checksum`. A healthy slot of either form holds under its
own range, so no healthy slot reports `checksum`; the price is a second CRC32 over 996
bytes for a slot in version 0's form, off the message path. The state is the slot's
commit word and lies outside every checksum on purpose: it is written last, in one
four-byte store. While a recording is live only the stop fields and the state change: a
stop writes the stop timestamp and the stop position, then the stop checksum; a delete
writes the state, 3; a truncate writes the stop fields as −1 and −1 under their
checksum while it cuts, then the new stop ("The driver"); a purge writes the retained
group. A reader that finds a state of 1, 2 or 3 checks its state first: 2 is
`invalid`, a recording the driver found damaged, and the reader reads nothing else from
the slot. For 1 and 3 it checks the creation checksum as above, then the stop checksum,
then, in version 1's form, the retained checksum. A creation checksum that holds under
neither range is damage, since nothing writes those bytes after the state: the reader
reports `checksum` at once. A stop or retained checksum that does not match may be a
write the driver is making, so the reader retries for up to 100 milliseconds and then
reports `checksum`. Either way it reads nothing from that slot, and only the driver
repairs it, as the section "The driver" says. A deleted recording keeps its slot and
loses its files, so an old id never names a new recording. The stop fields at −1 and −1
read "live, or being cut": a reader other than the driver treats them as live, and
the stop is in the files either way.

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

## Terms

A cluster member's record of the terms it has recorded, each with its term base log
position and the recording of the term in this directory, and the candidate term it
has stored, the highest term it has voted in or proposed (`spec/cluster.md`, "The
log"). In Aeron, this is the recording log and the node state file, one file here
(ADR 0060). The file is `terms` in the log directory: a 64-byte header, then 32-byte
entries in the order they were written.

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the bytes `43 56 4C 59 54 45 52 4D` (`CVLYTERM`), written last when the file is created; eight zero bytes until then |
| 8 | 4 | version | i32 | 0 |
| 12 | 4 | entry length | i32 | 32 |
| 16 | 8 | candidate term, slot 0 | i64 | −1 for none, else 0 or more |
| 24 | 4 | candidate checksum, slot 0 | u32 | the zlib CRC32 of bytes 16 to 24 |
| 28 | 4 | reserved | bytes | written as 0, never read |
| 32 | 8 | candidate term, slot 1 | i64 | −1 for none, else 0 or more |
| 40 | 4 | candidate checksum, slot 1 | u32 | the zlib CRC32 of bytes 32 to 40 |
| 44 | 20 | reserved | bytes | written as 0, never read |

The candidate term is stored in two slots, so that a raise a crash tears reads as the
candidate term stored before it. The stored candidate term is the higher of the slots
whose checksum matches. A raise writes only the other slot, the one the stored
candidate term is not read from, slot 1 when both slots hold it. A slot whose checksum
does not match is the raise a crash tore, and it is not read. A crash tears at most one
slot, so a file in which neither checksum matches is damaged, as is one in which a slot
whose checksum matches holds a value below −1, and the reader refuses both with
`candidate`.

Entry i is at 64 + i × 32. A reader ignores bytes past the last whole entry, which a
write cut short can leave; the next append writes over them.

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 4 | state | i32 | 0, empty, the entry's other bytes are not read: the file's end when it is the last whole entry, and damage before it; 1, valid; 3, deleted, a term truncation dropped |
| 4 | 4 | checksum | u32 | the zlib CRC32 of bytes 8 to 32 |
| 8 | 8 | leadership term id | i64 | 0 or more, above every valid entry before it |
| 16 | 8 | term base log position | i64 | 0 or more, a multiple of 32, at least every valid entry's before it |
| 24 | 8 | recording id | i64 | the recording of this term in the catalog, or −1 for a term that has none when its entry is appended; written once, by the append |

The file holds at most 1048576 entries, the catalog's bound on recordings; a member
that would record a term past it refuses with `terms full`, and the operator starts a
new directory, as for a full catalog; `cavalry doctor` warns from 90 percent of it. A
valid entry's term is above every valid entry's before it and its base at least
theirs, so the entries a reader takes are the member's terms in order; the term base
log position of the first term a member records is the position of its log at the
time, 0 for a cluster's first term. The reader takes entries in file order, skips an
entry whose state is 3, and stops at an entry whose state is 0 when it is the file's
last whole entry: the torn tail of a crash, which the next write overwrites. With one
writer only the last entry can be empty, since an append writes entry i only after
entry i − 1's state is synced. An empty entry before the last is damage, as an empty
catalog slot before the last is, and the reader refuses the file with `state` and
drops no term. An entry whose state is 1
or 3 and whose checksum fails is damage wherever it is, the last entry included: the
writer syncs an entry's fields and checksum before it writes the state, so no crash
leaves it, and the reader refuses the file with `checksum` and drops no term. The
state is an entry's commit word and lies outside its checksum on purpose, as a
catalog slot's does, so that an append and a truncation each end in one write of four
bytes. The cost is damage to a state that no check detects. A state damaged from 1 to
3, or from 3 to 1, makes the reader take a term for truncated that was not, or the
reverse. A state damaged from 1 to 0 in the file's last whole entry reads as the torn
tail of a crash, since a torn append leaves the same bytes: the reader drops that
term, the member's log leadership term id falls to the term before it, and the next
append writes over the entry. A state damaged to 0 before the last entry is refused
with `state`. The member's candidate term
at its start is the higher of the stored candidate term and its log leadership term
id, as "Roles and the election" of `spec/cluster.md` has init take it.

## Log recordings

A cluster member's record of which recordings in this directory hold its log, in the
order they carry it on (`spec/cluster.md`, "The log"). A member's log is in more than
one recording: one for each time the member joined a leader's log, one for each
replay it replicated or caught up by, and one for each term it led. The catalog holds
those. It also holds recordings the member made and never took into its log, and
their bytes can be of a history the cluster cut. This file tells the two apart when
the member starts on the directory again (ADR 0108). In Aeron, a member's log is one
recording that the member extends, so no such list exists
([glossary](../docs/glossary.md)). The file is `log-recordings` in the log directory:
a 64-byte header and two slots of 16440 bytes, 32944 bytes in all, a length that
never changes. It is not the file `log-recordings-<member id>` in the driver
directory, which a member writes for its service and which is gone when the driver
starts (`spec/cluster.md`, "The log's recordings"; ADR 0103).

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the bytes `43 56 4C 59 4C 4F 47 52` (`CVLYLOGR`), written last when the file is created; eight zero bytes until then |
| 8 | 4 | version | i32 | 0 |
| 12 | 4 | slot length | i32 | 16440 |
| 16 | 4 | member id | i32 | 0 or more: the member whose log the file lists |
| 20 | 44 | reserved | bytes | written as 0, never read |

Slot s, 0 or 1, is at 64 + s × 16440. A slot holds one whole list:

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 4 | checksum | u32 | the zlib CRC32 of the slot's bytes 4 to 24 + 16 × entry count |
| 4 | 4 | entry count | i32 | 0 to 1026 |
| 8 | 8 | sequence | i64 | 0 or more: 0 in the list a creation writes, and in every list after it one above the sequence of the list before |
| 16 | 8 | floor position | i64 | 0 or more, a multiple of 32: where the first entry's recording begins; with no entry, where the member's log ends |
| 24 | 16 × entry count | entries | | the recordings, in the order they carry the log on |

The bytes of a slot after its last entry are not read. Entry i is at 24 + i × 16 of
its slot:

| Offset | Size | Field | Type | Bound |
| --- | --- | --- | --- | --- |
| 0 | 8 | recording id | i64 | 0 or more: a recording of the cluster's log stream in this directory's catalog |
| 8 | 8 | start position | i64 | 0 or more, a multiple of 32, at least the start position of the entry before it; the first entry's is the floor position |

The list is kept in two slots, as the candidate term of the terms file is, so that a
write a crash tears reads as the list before it. The list is the one in the slot
whose checksum matches, and of two such slots the one with the higher sequence. A
write puts the whole list into the other slot, the one the list is not read from,
under a sequence one above the list it replaces, and syncs once. A slot whose
checksum does not match is the write a crash tore, or the slot no write has reached
yet, whose bytes are zero, and it is not read. A crash tears at most one slot, so a
file in which neither checksum matches is damaged, and the reader refuses it with
`slots`. A slot whose checksum matches holds what a writer wrote, so one that fails
another check is damage too, as are two slots of one sequence, and the reader
refuses the file with that check's name.

A recording is part of the member's log from the write that names it, once that
write is synced, and never before ("The driver"). The recordings a list names carry
the log on from the floor position: each begins at or below where the ones before it
end, and two can overlap, a replay's recording and the recording of the live log a
catch-up joined (`spec/cluster.md`, "The log"). A recording's end is not in the
file: it is the stop position the catalog holds for the recording, which the driver
writes at a stop and rebuilds from the files at its start. A list names at most 1026
recordings, the newest: the 1024 a member keeps once it has stopped them and the two
it can be making, its recording of a replay and its recording of the live log or of
the log it leads (`spec/cluster.md`, "Roles and the election", state 0). A member
whose log is in more recordings than it keeps forgets the oldest, which stay in the
catalog, and the floor position is where the oldest one it still names begins. A
list with no entry is a log that holds nothing, with a floor position of 0, or a log
wholly in recordings the member has forgotten, which a truncation below every
recording it kept leaves: the floor position is then where the log ends, the
position the member truncated to.

## The driver

The driver is the directory's only writer; how it keeps a second driver out is its own
rule, not this page's. It creates the catalog's header in the page's order, every field
and a sync, then the magic and a sync; a catalog shorter than its header, or whose magic
is zero while it holds no whole slot, was torn while the driver created it, and the
driver writes the header again; a zero magic over whole slots is damage, and the driver
refuses to start with `not-ready`. It creates a slot in this order: the file grows by
one zeroed slot, the creation fields are written and then the creation checksum, the
stop fields are written at −1 and then the stop checksum, and the state, 1, last; a slot
whose state is 0 is empty whatever its other bytes hold, so a crash during creation
leaves an empty last slot. The recording's id is reported, and its files created, only
after the state is 1. With one writer only the last slot can be empty, and the next
recording id is the number of whole slots after recovery. Under a version 0 header the
driver creates slots in version 0's form, so that a directory nobody has purged opens
under a version 0 driver again; before the first slot whose retained group it writes,
the directory's first `purge segments`, it writes 1 in the header's version and syncs,
and from then on it creates slots in version 1's form, the retained position at the
start position. A version 0 driver refuses a version 1 header with `version`. A slot in
version 0's form that a `purge segments` names is migrated first, in the one order a
crash cannot hurt: the creation checksum is written over version 1's ranges, which
signs the bytes the old one vouched for less the twelve of the group, which it vouched
were zero; then the retained position; then the retained checksum. A crash between the
new checksum and the group leaves a slot in version 1's form whose retained checksum
fails, which recovery rebuilds from the files as below.

The retention commands (`spec/client-protocol.md`, "Commands"; ADR 0031) write the
catalog before they touch the files, so that the rules below accept a crash at any
point. A truncate to a position writes the stop fields as −1 and −1 under the stop
checksum, which reads "live, or being cut" and puts the stop in the files; then removes
the segment files whose base is at or past the position from the highest base down,
one file per duty cycle, so that the files left are contiguous at every point and the
stop recovery would rebuild is the end of the highest file left; then, when the
position is not a segment base, cuts the segment file that holds it to the position's
offset with one truncation and extends it to the segment file length again, so that
the bytes past the position are zero; then truncates the time index to the entries at
or before the position; and then writes the stop position and the stop timestamp,
which is what recovery's rule gives for the files as cut, the timestamp of the last
index entry left, or the start timestamp when no segment file is left, and the stop
checksum. A purge writes the retained position under its checksum, then removes the
segment files whose bytes lie wholly before it, one per duty cycle; the time index
keeps its entries. A delete writes the state, 3, then removes the segment files and the
time index, one per duty cycle. A file the driver could not remove, cut or extend is
refused to the client, and the driver runs the rules below for that slot at once, so
that the slot and the files agree before the refusal is answered; a catalog write of
the command's that fails is refused and repaired the same way, since a truncate's last
write failing would otherwise leave the slot reading live, or being cut, over files
already cut.

When the driver starts, and again at every `stop recording` for the slot it closes, it
repairs the catalog; a stop whose own writes or syncs fail leaves its slot to the next
start's repair (`spec/client-protocol.md`, "Commands"; ADR 0064), and so does a
recording given up after a sync of its own failed, for which no stop is run (ADR
0065). At start-up it first
truncates the file to whole slots, then drops
a last slot whose state is 0; an empty slot before the last cannot come from one writer,
so it is damage: it gets state 2 and `cavalry doctor` names it, and no recording takes
it. Then, for every slot that is not empty: a creation checksum that does not match is
damage the files cannot repair, since the segments confirm the session id, the stream
id, the initial term id and the start position but not the start timestamp, the term
length, the MTU length, the segment file length or the channel; the slot gets state 2
and `cavalry doctor` names it, and recovery never writes a creation field or the
creation checksum, but for the migration above. A slot in version 1's form whose
retained checksum does not match was torn between the group and its checksum, by a
migration or a purge, and its retained position is derived from the files: the larger
of the start position and the base of the lowest segment file present, or the start
position when none is, written again under its checksum, which adds one to
`recording.recovered`; a purge's files are removed only after its catalog write, so the
lowest file present is at the retained position the purge replaced or the one it
wrote. The rules below that count from the start position, the empty recording, the
segment that is damage when missing and the segment the scan may read, count from the
retained position, and segment files whose bytes lie wholly before the retained
position are removed, not read, each adding one to `recording.files_removed`. A valid
slot whose creation checksum holds but whose stop checksum
does not, or whose stop position is −1, was torn during a stop, live at a crash, left
live by a stop whose own writes or syncs failed (ADR 0064) or by a recording given up
after a sync of its own failed (ADR 0065), or being cut by a truncate
the crash took, and
its stop fields are derived wholly from the files, in this order. First the time index:
one that is missing, too short for its header, or whose magic, version or recording id
fails its check was torn while the recorder created it, and the driver writes it again,
the header with its magic last and one entry, the start timestamp at the start
position; an index with a header and no whole entry gets that entry; then the index is
truncated to whole entries and a last entry that breaks the order is dropped. Second,
an empty recording: when no segment file of the recording exists, or the segment
holding the retained position exists and the frame length there is 0, the recorder
crashed before the first frame, or a truncate to the retained position removed every
file; the stop position is the retained position, the stop timestamp is the start
timestamp, the index entries past the stop are dropped so the index holds its first
entry alone, and the slot stays valid; a start segment file shorter
than the start position's offset in it, which a pre-extension torn by a crash can leave,
reads as a frame length of 0 there, an empty recording and not damage, and the scan below
stops the same way at a position past the end of its segment file. Third, the scan's start:
the driver walks the index back from its last entry to the first entry at whose
position a frame checks, and starts there, or at the start position when none does;
after a power loss an entry can point past what its segment kept, since the two files
are synced apart, and walking back skips it; the walk costs one header read per entry
skipped, and the entries that can point past the data are those appended since the last
sync, at most a second's worth, since the index and the segment are synced on one cadence. Fourth, the scan: from that position it
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

The consensus module is the terms file's only writer, from its own thread (ADR
0060). It creates the file as the driver creates the catalog's header, before the
member's first message: every field after the magic, both candidate slots at −1 under
their checksums, and a sync, then the magic and a sync. A terms file shorter than its
header, or whose magic is zero while it holds no whole entry and no slot whose
checksum matches holds a candidate term of 0 or more, was torn while the module
created it, and the module writes the header again; a zero magic over a whole entry
or over such a slot is damage, since creation writes neither, and the member refuses
to start with `not-ready`. The module appends an entry in two synced writes, before
the member reports an append position for the term: the checksum and the fields,
bytes 4 to 32, and a sync, then the state, 1, and a sync, so that a crash between the
two leaves an empty entry. An entry's fields are written once, by its append, and
only its state changes after it. The module truncates by writing state 3 over each
valid entry whose term base log position is above the truncation position, from the
last entry backwards, each write synced, before the member reports its log leadership
term id again. It drops the terms a leader's record lacks the same way, by writing
state 3 over each valid entry whose leadership term id is above the term the leader
named, whatever its base (`spec/cluster.md`, "Messages during an election"; ADR
0096). It raises the candidate term in one synced write, the other slot's
value and checksum, before the vote or request vote that raises it is sent. A crash
leaves at most one write torn, which the reader reads as the state before it: an
empty entry, a valid entry not yet deleted, or the candidate term stored before the
raise. At a member's start the module checks the file's length against the bound
before it reads a byte of it, so that what it reads is bounded: a file over the bound
is `length` whatever its header holds, and every other file gets the checks of "What
a reader rejects" in their order. The recovery pass of this section leaves the file
alone: it names files by recording id, and `terms` is none.

The consensus module is the log recordings file's only writer too, from the same
thread (ADR 0108). At the start of a member whose terms file holds no term, the
module creates the file when there is none, as it creates the terms file: every
field after the magic, slot 0 as the list of no entry with sequence 0 and floor
position 0 under its checksum, the file extended to its length, and a sync, then the
magic and a sync. A file shorter than its header, or whose magic is zero while no
slot whose checksum matches holds an entry, a sequence above 0 or a floor position
above 0, was torn while the module created it, and the module writes it again; a
zero magic over such a slot is damage, since creation writes none, and the member
refuses to start with `not-ready`. At the start of a member whose terms file holds a
term, the module creates nothing and writes nothing before it has read the file: a
member that has recorded a term holds a log, and a list created for it would say
that it holds none. Such a member with no file refuses to start with `absent`,
which is the state of a directory a version before this one wrote, and one whose
file was torn while it was created refuses with `too-short` or `not-ready`, since
creation ends before a member records its first term.

The module writes the list, whole and synced, at four moments, and a recording
counts toward the member's append position only once a synced list names it
(`spec/cluster.md`, "The log"). First, at the end of the duty cycle in which the
member's recording of a replay or of the live log has come to carry its log on: the
election was told the recording's image, and the recording begins where the log
ends, or, for the live recording in a catch-up, at or below where the replay's
recording has synced to. Second, before a leader appends to the log of a term: the
list names its recording of that log, which begins at the term's base. Third, in a
truncation, after the last `purge recording` and `truncate recording` has been
answered and before the terms file's entries are deleted: the list names the
recordings that are left, and when none is left its floor position is the position
truncated to. Fourth, at a member's start, when the list it read names a recording
that is gone or that holds nothing: the list then names the recordings the start
took ("A member's start" of `spec/cluster.md` says which those are). A list that
cannot be written or synced leaves the recording out of the member's log, the duty
cycle reports the error with what to do, and the next cycle writes again; a sync
that failed is not tried again on the same bytes, since the next write is the whole
list into the same slot. A member that stops a recording, at a step back to an
election or at its close, writes nothing: the list already names the recording, and
its stop position is the catalog's. The recovery pass of this section leaves the
file alone, as it leaves the terms file.

What the order of a truncation leaves after a crash: before the list is written,
recordings the list names are gone from the catalog, the last ones it names, since
the module purges from the last recording back; after the list and before the terms
file, a term whose base is above the end of the log. A member's start reads both as
that truncation and finishes it.

## What a reader rejects

A reader checks each file in the order its table gives and reports the first failure by
name, so that every implementation reports the same reason for the same bytes.

The catalog header, with the file's length:

| Check | Reported when |
| --- | --- |
| `too-short` | The file is shorter than 128 bytes. |
| `not-ready` | The magic is eight zero bytes: the creator has not finished. |
| `magic` | The magic is any other value than `CVLYCTLG`. |
| `version` | The version is not 0 or 1. |
| `descriptor-length` | The descriptor length is not 1024. |
| `checksum-kind` | The data checksum kind is not 0. |
| `timestamp` | The creation timestamp is below 0. |
| `length` | The file holds more than 1048576 whole slots. |

A descriptor, with its slot's index:

| Check | Reported when |
| --- | --- |
| `too-short` | Fewer than 1024 bytes were given. |
| `state` | The state is not 0 to 3. A state of 0 is an empty slot, not a failure: nothing else is read. |
| `invalid` | The state is 2: the driver found the recording damaged, and `cavalry doctor` names it. Nothing else is read. |
| `checksum` | The creation checksum is neither the zlib CRC32 of bytes 28 to 472 and 484 to 1024 (version 1's form) nor that of bytes 28 to 1024 (version 0's form), tried in that order; or the stop checksum is not that of bytes 8 to 24; or, in version 1's form, the retained checksum is not that of bytes 472 to 480. A stop or retained checksum is retried for 100 milliseconds first. |
| `timestamp` | The start timestamp is below 0, or a stop timestamp that is not −1 is below the start timestamp. |
| `recording-id` | The recording id is not the slot's index. |
| `position` | The start position is below 0 or not a multiple of 32; a stop position that is not −1 is below the start position or not a multiple of 32; one stop field is −1 and the other is not; or, in version 1's form, the retained position is below the start position, not a multiple of 32, above a stop position that is not −1, or neither the start position nor a segment base. |
| `term-length` | The term length is not a power of two from 65536 to 1073741824. |
| `segment-length` | The segment file length is not a power of two, is below the term length, or is above 1073741824. |
| `mtu` | The MTU length is not a multiple of 32 from 64 to 65504. |
| `string` | The channel length is below 0 or above 256, the source identity length is below 0 or above 128, or either's bytes are not UTF-8. |

The time index header, with the file's length:

| Check | Reported when |
| --- | --- |
| `too-short` | The file is shorter than 128 bytes. |
| `not-ready` | The magic is eight zero bytes. |
| `magic` | The magic is any other value than `CVLYTIDX`. |
| `version` | The version is not 0. |
| `recording-id` | The recording id is below 0. |

A time index entry, with the entry before it when there is one:

| Check | Reported when |
| --- | --- |
| `too-short` | Fewer than 16 bytes were given. |
| `timestamp` | The timestamp is below 0. |
| `position` | The position is below 0 or not a multiple of 32. |
| `order` | The timestamp or the position is below the entry before it. |

A segment file has no header and one check, `frame`: the frame header at a position
does not carry the position's term id and term offset and the descriptor's session id
and stream id, or the position is past the end of its segment file.

The terms file, with the file's length:

| Check | Reported when |
| --- | --- |
| `too-short` | The file is shorter than 64 bytes. |
| `not-ready` | The magic is eight zero bytes: the creator has not finished. |
| `magic` | The magic is any other value than `CVLYTERM`. |
| `version` | The version is not 0. |
| `entry-length` | The entry length is not 32. |
| `length` | The file holds more than 1048576 whole entries. |
| `candidate` | Neither candidate checksum matches its slot, or a slot whose checksum matches holds a candidate term below −1: the file is damaged. One checksum that does not match is not a failure: that slot is the raise a crash tore, and the stored candidate term is the other slot's. |

An entry, with its index and the entry before it:

| Check | Reported when |
| --- | --- |
| `state` | The state is not 0, 1 or 3; or it is 0 and a whole entry follows it in the file: the file is damaged, since only the last whole entry can be empty. An entry vector holds one entry and so pins the first half alone. |
| `checksum` | The state is 1 or 3 and the checksum does not match bytes 8 to 32: the file is damaged, since no crash leaves it, and the reader refuses it. |
| `term` | The term is below 0, or not above the valid entry before it. |
| `position` | The term base log position is below 0, not a multiple of 32, or below the valid entry's before it. |
| `recording` | The recording id is below −1. |

The log recordings file, with the file's length:

| Check | Reported when |
| --- | --- |
| `too-short` | The file is shorter than 32944 bytes. |
| `length` | The file is longer than 32944 bytes. |
| `not-ready` | The magic is eight zero bytes: the creator has not finished. |
| `magic` | The magic is any other value than `CVLYLOGR`. |
| `version` | The version is not 0. |
| `slot-length` | The slot length is not 16440. |
| `member` | The member id is below 0; or, for the member that opens the file, it is not that member's id: the directory is another member's. A header vector pins the first half alone. |

A slot of it, its fields and then its entries in order:

| Check | Reported when |
| --- | --- |
| `checksum` | The entry count is not 0 to 1026, fewer bytes were given than the count needs, or the checksum is not the zlib CRC32 of bytes 4 to 24 + 16 × entry count. For one slot of the two this is not a failure of the file's: that slot is the write a crash tore or the slot never written, and the list is the other slot's. |
| `sequence` | The sequence is below 0. |
| `position` | The floor position is below 0 or not a multiple of 32. |
| `recording` | An entry's recording id is below 0. |
| `position` | An entry's start position is below 0, not a multiple of 32, or below the start position of the entry before it; or the first entry's start position is not the floor position. |

The two slots together, slot 0's checks and then slot 1's:

| Check | Reported when |
| --- | --- |
| `slots` | Neither slot passes `checksum`: the file is damaged, since a crash tears one slot at most. |
| a slot's check | A slot passes `checksum` and fails `sequence`, `position` or `recording`: the file is damaged, since a writer writes no such slot. Slot 0's failure is reported before slot 1's. |
| `sequence` | Both slots pass every check and hold the same sequence. |

A member that opens its file reports two more, which no vector pins since neither
is a property of the file's bytes: `absent`, when the terms file holds a term and
there is no log recordings file ("The driver"), and `recordings-full`, when a list
of more than 1026 recordings is given to a write, which writes nothing.

## Vectors

`spec/vectors/durable-log.json`, written by `spec/vectors/durable-log.py`: catalog header
vectors with the file length the reader sees and the whole slots it holds, under version
0 and version 1; descriptor vectors with the slot index, in both forms, a valid vector's
`fields` naming its `form`, 0 or 1, and its retained position;
index header and entry vectors, an entry vector with the entry before it; terms
header vectors with the file length, a valid vector's `fields` naming the candidate
term each slot holds, `null` for a slot whose checksum does not match, and the stored
candidate term, and terms entry vectors with the entry's index and
the valid entry before it, under version 2; log recordings header vectors with the
file length, slot vectors that carry a slot's fields and its entries and no byte
after them, a valid vector's `fields` naming the sequence, the floor position and
each entry as its recording id and its start position, and file vectors that carry
both slots as `slots` and name in `fields` the slot that holds the list, 0 or 1,
beside that list, under version 3; and position
vectors, arithmetic, from a descriptor's fields and a position to the segment file name,
the segment base position, the offset in the segment, the term id and the term offset.
