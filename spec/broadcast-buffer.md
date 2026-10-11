# Broadcast buffer, version 0

The broadcast buffer is the shared-memory file through which the driver tells every
client on a host what happened: a publication is ready, an image is available or gone, a
command failed, a client timed out. One transmitter, the driver's conductor, writes
records; any number of receivers, the clients' conductors, read them, each at its own
pace, and a receiver that falls a whole ring behind is lapped: it loses records, and is
told so. That loss is why this layout is control plane only: a lapped receiver cannot say
what it missed, which invariant 1 forbids on a stream, while a lapped client learns that
it was lapped and acts on it (ADR 0014). Every field below names its writer, its readers
and the bound a reader checks (ADR 0010), and every field the transmitter writes and
receivers read has a 128-byte slot of its own (ADR 0011). In Aeron, this is the
to-clients buffer of the CnC file, Agrona's `BroadcastTransmitter` and
`BroadcastReceiver` (ADR 0014; `docs/glossary.md`). The events themselves, their type
ids and their encodings, belong to the client protocol's page; this page carries any
record whose type is 1 or more.

## File

The file is named `broadcast-buffer` in the driver's directory. It has two regions, in
this order:

| Region | Offset | Length | Holds |
| --- | --- | --- | --- |
| ring | 0 | capacity | records, back to back, wrapping at the end |
| trailer | capacity | 512 | the header and the three slots below |

The file length is exactly capacity + 512 bytes, and the capacity is a power of two from
4096 to 1073741824, so a reader finds the trailer from the file's size. Integers are
little-endian (`spec/README.md`). Offsets in the trailer tables are from the start of the
trailer; an index is an offset from the start of the ring. Clients map the file
read-only: only the driver writes it.

## Header

The first 128 bytes of the trailer. The creator writes every field, then the magic last
with a release store; a reader loads the magic with an acquire load before it reads
anything else, and eight zero bytes mean `not-ready`. The header never changes after the
magic is written, and a reader re-reads it through its final mapping and treats a
difference as `changed` (ADR 0010).

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the creator, last, with a release store | everyone, first, with an acquire load | the bytes `43 56 4C 59 42 43 53 54` (`CVLYBCST`) |
| 8 | 4 | version | i32 | the creator | everyone | 0 |
| 12 | 4 | capacity | i32 | the creator | everyone | a power of two from 4096 to 1073741824; the file length is capacity + 512 |
| 16 | 8 | creator pid | i64 | the creator | `doctor` | greater than 0 |
| 24 | 8 | start timestamp | i64 | the creator | `doctor` | nanoseconds since the Unix epoch, 0 or more |
| 32 | 96 | reserved | bytes | the creator | nobody | written as 0, never read |

## Trailer

The rest of the trailer: one 128-byte slot per field (ADR 0011). The bytes of a slot
after its field are reserved: written as 0 by the creator, never read. A position is a
byte count from the creation of the buffer, starting at 0 and only increasing; the index
of a position is position modulo capacity. The transmitter keeps three: the tail
position, where the next record begins; the tail intent, where the tail will be once the
record it is writing now is complete; and the latest record, where the last complete
record begins. A receiver keeps its own cursor and compares it with the tail intent: a
cursor more than capacity behind the tail intent points at bytes the transmitter is
overwriting or has overwritten.

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 128 | header | bytes | the creator | everyone | the table above |
| 128 | 8 | tail intent | i64 | the transmitter, with a release store followed by a release fence, before it writes a record | receivers, with acquire loads, and after an acquire fence once they have copied a record | a multiple of 8, at least the tail position loaded before it |
| 256 | 8 | tail position | i64 | the transmitter, with a release store, after it has written a record | receivers, with acquire loads; `doctor` | a multiple of 8, 0 or more |
| 384 | 8 | latest record | i64 | the transmitter, with a release store, after it has written a record and before the tail | receivers, with acquire loads, when they are lapped and when they join | a multiple of 8, 0 or more, at least the tail position loaded before it − capacity |

A receiver loads the tail position first, then the tail intent, then the latest record,
each with an acquire load, and the bounds above are the ones that hold for any three
values loaded in that order: the transmitter stores the tail intent before a record and
the latest record before the tail, so the tail intent and the latest record a receiver
loads are at least the ones stored before the tail it loaded, and may be later ones. A
latest record at or past the tail, or a tail intent far past it, is a transmit in
progress, not a fault; a tail intent below the tail, or a latest record more than a
capacity behind it, is one.

To create the buffer the driver sizes the file to its full length, writes zero
everywhere, fills the header fields after the magic, and writes the magic last with a
release store. Every position starts at 0.

## Record

A record begins at an index that is a multiple of 8 and is 8 + message length bytes,
rounded up to a multiple of 8 (its aligned length). It never crosses the end of the ring:
a record that would reach the end is preceded by a padding record that reaches it, and
begins at index 0. The longest message is capacity ÷ 8 bytes. The transmitter writes a
record's bytes, and a receiver reads them, with atomic stores and loads of relaxed
ordering, a word at a time: a receiver that has fallen behind reads bytes the transmitter
is overwriting, by design, and learns it afterwards from the tail intent.

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 4 | length | i32 | the transmitter | receivers | 8 + the message length: 8 or more, and its aligned length at most capacity − index; when the type is not −1, at most 8 + capacity ÷ 8; when it is −1, exactly capacity − index |
| 4 | 4 | type | i32 | the transmitter | receivers | −1, a padding record, which delivers nothing; or 1 or more, the message type id |
| 8 | length − 8 | message | bytes | the transmitter | receivers | opaque to this page |

## Transmitting

The transmitter writes a record in place; it never waits for a receiver and never fails:

1. It loads the tail position, which it alone writes, with a plain load. The record would
   begin at the tail's index, and the tail after it is tail + aligned length.
2. If the aligned length is more than the bytes from that index to the end of the ring,
   it stores tail + aligned length + the bytes to the end in the tail intent with a
   release store followed by a release fence, writes a padding record over the bytes to
   the end, with their count as its length and −1 as its type, and the record begins at
   index 0, with the tail moved past the padding. Otherwise it stores tail + aligned
   length in the tail intent, with the same release store and fence.
3. It writes the record's length, type and message.
4. It stores the record's position in the latest record with a release store, then tail +
   aligned length in the tail position with a release store.

A message longer than capacity ÷ 8 is `message`, and a type below 1 is `type`; the
transmitter's API refuses both before step 1.

## Receiving

A receiver joins by loading the latest record with an acquire load, after the checks of
"What a reader rejects", and setting its cursor and its next record to it, so the first
record it receives is the latest. Then, each time it is asked for a record:

1. It loads the tail position, the tail intent and the latest record, in that order,
   with acquire loads, and checks them against their bounds: a failure is `tail`. A tail
   that is not above the next record means nothing is available; a tail below it is a
   stale load after a lap, which the next receive catches up on.
2. Its cursor becomes the next record. If cursor + capacity is not above the tail intent,
   the receiver has been lapped: the result is `lapped`, nothing is delivered, and the
   cursor and the next record become the latest record, so that the next call continues
   from there.
3. It reads the length and type at the cursor's index and checks them against the bounds
   of "Record". The next record becomes cursor + aligned length. If the type is −1, a
   padding record, the cursor becomes that next record, whose index is 0, and the length
   and type there are read and checked the same way, and the next record moves past that
   record too.
4. It copies the message out, runs an acquire fence, and loads the tail intent again. If
   cursor + capacity is not above it, the record was overwritten while it was being read:
   the result is `lapped`, as in step 2. Otherwise it delivers the type and the copy.

A check of step 3 that fails is `lapped` when the tail intent loaded afterwards says the
receiver was lapped, since the bytes it read were being overwritten, and `record`
otherwise: the writer is the driver, so the file is corrupt or not the driver's, and the
client reports it and closes.

## When a receiver is lapped

Loss is not a fault of the buffer: the transmitter runs at its own pace, and a receiver
that cannot keep up is told `lapped` and positioned at the latest record. What it does
with that is its conductor's decision; Cavalry's client conductor closes the client with
an error that names this page, because an event it missed may be the one that told it an
image was available or a command failed (ADR 0014). The transition is a client's, and a
client owns no counter in this slice (ADR 0014); `doctor` reads the three positions from
the file. Capacity is fixed when the file is created; a larger buffer means a restart.

## Counters

None. The transmitter has no transition of its own, since every transmit succeeds, and
the receiver's transitions, `lapped`, `tail` and `record`, happen in a client, which owns
no counter in this slice (ADR 0014).

## What a reader rejects

A reader runs these checks in this order when it attaches and reports the first that
fails, by the name in the first column.

| Reason | Check |
| --- | --- |
| `too-short` | The file is shorter than 512 bytes, or fewer than 128 header bytes were given. |
| `not-ready` | Magic is eight zero bytes: the creator has not finished. Wait and read again. |
| `magic` | Magic is any other value than `CVLYBCST`. |
| `version` | Version is not 0. |
| `capacity` | Capacity is not a power of two from 4096 to 1073741824. |
| `length` | The file length is not capacity + 512. |
| `changed` | The header read through the final mapping differs from the first read. |

In use: `tail` when the tail position, the tail intent or the latest record is outside
its bound in "Trailer"; `lapped` when a
receiver's cursor + capacity is not above the tail intent, before or after it read a
record; `record` when a record's length or type is outside the bounds of "Record" and the
receiver was not lapped; `message` when the transmitter is given a message longer than
capacity ÷ 8, and `type` when it is given a type below 1, both refused before anything is
written.

## Vectors

`spec/vectors/broadcast-buffer.json`, written by `spec/vectors/broadcast-buffer.py`, the
reference encoder. Each vector has a `region`:

- `header`: the 128 bytes at the start of the trailer as `bytes`, the `file_length` the
  reader sees, and the `fields` a conforming reader decodes or the `error` it reports.
- `record`: a record's `bytes` with the `capacity` and `index` it sits at, and the
  `fields` a conforming reader decodes, length, type, state (`message` or `padding`),
  aligned length and message length, or the `error` it reports.
- `transmit`: an arithmetic vector with `inputs` and no bytes: the capacity, the tail and
  the message length, and the `fields` the transmit produces: the record and aligned
  lengths, the tail intent, the padding record written if any, the record's index, the
  latest record and the tail after; or the `error`.
- `receive`: an arithmetic vector: the capacity, the receiver's next record, the tail,
  tail intent and latest record it loads, in that order, the length and type at the
  cursor's index and at index 0, and the tail intent it loads after copying when that
  moved; and the `fields`: the result (`none`, `message` or `lapped`), the cursor and
  the next record after, and for a message its index, type and message length; or the
  `error`.

A conforming implementation decodes every valid vector to its fields, encodes those
fields to the same bytes, computes every arithmetic vector's fields from its inputs, and
reports the named reason for every invalid one.
