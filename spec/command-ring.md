# Command ring, version 1

The command ring is the shared-memory file through which every client on a host sends
the driver its commands: add a publication, add a subscription, remove one, keep alive.
Many producers, the clients, write records into one ring; one consumer, the driver's
conductor, reads them in order and clears them. It carries a command per publication or
subscription, not a message per frame, so it is control plane, but it is shared between
processes and lock-free, so every field below names its writer, its readers and the bound
a reader checks (ADR 0010), and every field one party writes and another reads in its hot
loop has a 128-byte slot of its own (ADR 0011). In Aeron, this is the to-driver command
buffer of the CnC file, Agrona's `ManyToOneRingBuffer` (ADR 0014; `docs/glossary.md`).
The commands themselves, their type ids and their encodings, belong to the client
protocol's page; this page carries any record whose type is 1 or more.

## File

The file is named `command-ring` in the driver's directory. It has two regions, in this
order:

| Region | Offset | Length | Holds |
| --- | --- | --- | --- |
| ring | 0 | capacity | records, back to back, wrapping at the end |
| trailer | capacity | 768 | the header and the five slots below |

The file length is exactly capacity + 768 bytes, and the capacity is a power of two from
4096 to 1073741824, so a reader finds the trailer from the file's size. Integers are
little-endian (`spec/README.md`). Offsets in the trailer tables are from the start of the
trailer; an index is an offset from the start of the ring.

## Header

The first 128 bytes of the trailer. The creator writes every field, then the magic last
with a release store; a reader loads the magic with an acquire load before it reads
anything else, and eight zero bytes mean `not-ready`. The header never changes after the
magic is written, and a reader re-reads it through its final mapping and treats a
difference as `changed` (ADR 0010).

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the creator, last, with a release store | everyone, first, with an acquire load | the bytes `43 56 4C 59 43 4D 44 52` (`CVLYCMDR`) |
| 8 | 4 | version | i32 | the creator | everyone | 1 |
| 12 | 4 | capacity | i32 | the creator | everyone | a power of two from 4096 to 1073741824; the file length is capacity + 768 |
| 16 | 8 | creator pid | i64 | the creator | `doctor` | greater than 0 |
| 24 | 8 | start timestamp | i64 | the creator | `doctor` | nanoseconds since the Unix epoch, 0 or more |
| 32 | 96 | reserved | bytes | the creator | nobody | written as 0, never read |

## Trailer

The rest of the trailer: one 128-byte slot per field (ADR 0011). The bytes of a slot
after its field are reserved: written as 0 by the creator, never read. A position is a
byte count from the creation of the ring: the tail position is where the next record is
claimed, the head position where the next record is read; both start at 0 and only
increase; the index of a position is position modulo capacity. The ring holds tail − head
bytes, at most capacity.

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 128 | header | bytes | the creator | everyone | the table above |
| 128 | 8 | tail position | i64 | producers, with a compare-and-swap | producers, with acquire loads; the consumer, with acquire loads, when it unblocks or measures; `doctor` | a multiple of 8, from the head position to the head position + capacity |
| 256 | 8 | head cache position | i64 | producers, with release stores | producers, with acquire loads | a multiple of 8, at most the tail position; a producer derives no index from it, only whether to load the head position |
| 384 | 8 | head position | i64 | the consumer, with release stores | producers, with acquire loads; the consumer, which owns it; `doctor` | a multiple of 8, at most the tail position |
| 512 | 8 | correlation id counter | i64 | producers, with fetch-and-add | producers | 0 or more: the next correlation id |
| 640 | 8 | consumer heartbeat | i64 | the consumer, with release stores | producers, `doctor`, with acquire loads | nanoseconds since the Unix epoch, 0 or more, while the driver runs; −1 once it has closed |

To create the ring the driver sizes the file to its full length, writes zero everywhere,
stores its clock in the consumer heartbeat, fills the header fields after the magic, and
writes the magic last with a release store. Every position starts at 0.

## Record

A record begins at an index that is a multiple of 8 and is 8 + message length bytes,
rounded up to a multiple of 8 (its aligned length). It never crosses the end of the ring:
a claim that would reach the end writes a padding record to the end and begins at index 0.
The longest message is capacity ÷ 8 bytes.

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 4 | length | i32 | the producer that claimed the record: negative with a compare-and-swap from 0 when it claims, positive with a compare-and-swap from that word when it commits or aborts; the consumer, with a compare-and-swap from the word it loaded when it unblocks, and with plain stores when it clears | the consumer, with acquire loads | 0: unclaimed. Otherwise its magnitude is 8 + the message length: 8 or more, and its aligned length at most capacity − index; when the type is not −1, at most 8 + capacity ÷ 8 |
| 4 | 4 | type | i32 | as the length, in the same 8-byte store | the consumer | −1, a padding record, which delivers nothing; or 1 or more, the message type id |
| 8 | length − 8 | message | bytes | the producer, before it commits | the consumer | opaque to this page |

Length and type are written together, in one 8-byte word, so that a reader that sees a
length sees the type that goes with it. A negative length is a claim in progress: a
reader stops at it, and the driver knows it when it unblocks. A positive length is a
committed record. Every change to the word that another party may be racing is a
compare-and-swap from the value its writer last loaded, so that of a commit and an
unblock exactly one takes effect and the other learns it; only the clearing of records
the consumer has read is a plain store, since no producer may claim there before the
head is released (ADR 0018).

## Claim, commit and abort

A producer writes a record in place:

1. It loads the consumer heartbeat with an acquire load: −1 is `driver-closed`, and a
   value older than the driver timeout is `driver-timeout`; nothing is written either
   way. It loads the head cache position with an acquire load.
2. It loads the tail position with an acquire load. A tail that is not a multiple of 8 or
   is below the head cache is `tail`, and nothing is written.
3. If the aligned length is more than capacity − (tail − head cache), it loads the head
   position with an acquire load; if the aligned length is still more than capacity −
   (tail − head), the result is `full` and nothing is written; otherwise it stores the
   head it loaded in the head cache with a release store. The head it loads may be
   beyond the tail it loaded, when the tail is stale: the compare-and-swap of step 5
   then fails and the producer goes back to step 2. Only the consumer, which owns the
   head, checks the tail against it ("Unblocking").
4. The record would begin at the tail's index. If the aligned length is more than the
   bytes from there to the end of the ring, the record begins at index 0 instead, and the
   bytes to the end become a padding record. The bytes free at the start of the ring are
   the head's index: if the aligned length is more than the head cache's index, the
   producer loads the head position with an acquire load and stores it in the head
   cache; if the aligned length is more than the head's index too, the result is `full`,
   and the producer claims only the padding.
5. It compare-and-swaps the tail position from the value it loaded to tail + aligned
   length + the padding's length, or to tail + the padding's length when the result is
   `full`; on failure it goes back to step 2.
6. If there is a padding record, it writes it: a compare-and-swap of the word at the
   tail's index from 0 to the padding's length and type −1. A failed swap means the
   driver padded that gap meanwhile ("Unblocking", step 3), which covers the same bytes,
   and the producer goes on.
7. If the result is `full`, it is done. Otherwise the record is claimed at its index: it
   compare-and-swaps the word there from 0 to the claim word, the negative record length
   and the type, with release ordering on success and acquire on failure. A failed swap
   is `unblocked`: the driver padded a gap over the index before the claim word landed,
   the tail has moved past the record, and the producer reports `unblocked` to its
   caller, whose command was not delivered and may be claimed again.
8. Before it writes the message, it loads the record's 8-byte word with an acquire load.
   A word that is not its claim word is `unblocked`: the driver padded the claim after
   the unblock timeout ("Unblocking"), and the consumer may have read and zeroed it or
   another producer claimed the index since. The producer writes nothing, and reports
   `unblocked` to its caller, whose command was not delivered and may be claimed again.
   Otherwise it writes the message.
9. Commit: it compare-and-swaps the record's word from the claim word to the positive
   record length and the type, with release ordering on success and acquire ordering on
   failure. Abort: the same swap to the positive record length and type −1, a padding
   record the consumer skips. A failed swap is `unblocked`, as in step 8: nothing more is
   stored, and the caller's command was not delivered. A stall between the load of step
   8 and the message's stores that outlasts the unblock timeout can still put bytes into
   a record another producer has claimed at that index since; the unblock timeout is the
   bound, since a producer stalled that long has been timed out as a client, and the
   driver validates every command it decodes (ADR 0018).

A producer that copies a message in (write) runs the same steps, with the message
written in step 8. A message longer than capacity ÷ 8 is `message`, and a type below 1
is `type`; the producer's API refuses both before step 1.

## Reading

The consumer reads from the head position, which it alone writes, so it loads it with a
plain load. With the head's index as the start, while the bytes read are fewer than
capacity − head index and fewer messages than its limit have been delivered:

1. It loads the next record's length and type with one acquire load. A length of 0,
   unclaimed, or less, claimed and not committed, means nothing more is committed: the
   read stops.
2. It checks the record against the bounds of "Record". A record outside them is
   `record`: the consumer adds one to `command_ring.rejected_record`, stops, and clears
   nothing from that record on. The ring is corrupt from that record: every later read
   reports `record` without reading it, the driver reports it, and restarting the driver
   recreates the file.
3. The bytes read grow by the aligned length. A padding record delivers nothing; any
   other record delivers its type and message to the handler, in place.

After the loop, if any bytes were read, the consumer writes zero over them, from the
head's index, with plain stores, and stores head + bytes read in the head position with a
release store, so that a producer whose acquire load sees the new head finds zero where it
may next claim. The read stops at the end of the ring; the next read begins at index 0.

## Unblocking

A producer that claimed and died, or claimed and never committed, leaves a length that
is negative or 0 at the head's index while the tail is beyond the head, and the consumer
reads nothing more. When the head position has not moved for the unblock timeout while
the tail is beyond it, the driver unblocks:

1. It loads the head and the tail with acquire loads. A tail that is not a multiple of 8,
   is below the head or is more than capacity above it is `tail`: one is added to
   `command_ring.rejected_tail`, nothing is written, and the ring is corrupt from here as
   it is after `record`: every later read and unblock reports `tail` without reading the
   ring, the driver reports it, and restarting the driver recreates the file. A tail
   equal to the head means nothing is blocked.
2. It loads the length and type at the head's index, the consumer index, with an acquire
   load. A positive length is a committed record the next read will take: nothing is
   blocked. A negative length is the stale claim: after the checks of "Record", whose
   failure is `record` and adds one to `command_ring.rejected_record`, the driver
   compare-and-swaps the word from the one it loaded to its magnitude as the length and
   −1 as the type, a padding record the next read skips. A failed swap means the
   producer committed or aborted meanwhile, and the driver writes nothing: the next read
   takes or skips the record.
3. A length of 0 means the claimant died before its first store. The driver scans forward
   in 8-byte steps from the consumer index to a limit: the tail's index when it is above
   the consumer index, else the end of the ring. It stops at the first index whose length
   word is not 0; then it scans back to the consumer index to confirm every word between
   is still 0, since a claim may have landed meanwhile, and if so compare-and-swaps the
   word at the consumer index from 0 to the gap's length, that index − consumer index,
   and type −1; a failed swap means a claim landed there after the scan, and the driver
   writes nothing. If it reaches the limit with every word 0, what it does depends on
   the limit:
   at the tail's index it writes nothing, since the next claim will bound the gap, as in
   the design this page copies; at the end of the ring it scans back the same way and
   covers the gap to the end, because no later claim can bound it and the ring would
   otherwise stay blocked for good (ADR 0014). A claim in the gap that landed just before
   the driver loaded the tail belongs to a live client, and its command is lost under the
   padding; only the record at the consumer index is known to be older than the unblock
   timeout, and the client's own response timeout reports the loss.

A producer whose claim was padded finds the word is not its claim word before it writes
its message, or its claim's, commit's or abort's compare-and-swap fails, and reports
`unblocked` (steps 7 to 9 of "Claim, commit and abort"); it stores nothing more, so a
late commit never puts a positive header behind the head, which in version 0 delivered
a command nobody sent on the next lap and left the head past the tail, and a padding
written by the driver never covers a record a producer committed in the same instant,
which in version 0 lost that command while its producer was told it was delivered
(ADR 0018).

Each record an unblock writes adds one to `command_ring.unblocked`. The unblock timeout
is the driver's to specify; it is the client liveness timeout, 10 s in Aeron, so that a
producer slower than it has already been timed out as a client.

## Heartbeat and correlation ids

The consumer stores its clock in the consumer heartbeat with a release store at least
once per heartbeat interval, and stores −1 when it closes. A producer loads it before it
claims (step 1). The intervals are the driver's to specify. A client that finds
`driver-closed` or `driver-timeout` reports it to its caller and closes.

A producer takes a correlation id with a fetch-and-add of 1 on the correlation id counter
and puts it in the command it then claims; the driver's reply on the broadcast buffer
(`spec/broadcast-buffer.md`) names the same id. The counter starts at 0.

## When the ring is full

A claim that finds fewer free bytes than it needs fails with `full`. From step 3, when
the whole ring has fewer free bytes than the record needs, nothing is written. From step
4, when the record fits neither before the end of the ring nor at its start, the producer
claims and writes only the padding record to the end, so that the next claim begins at
index 0; the design this page copies does the same (ADR 0014). Either way the record is
not written, and the producer returns `full` to its caller with an error that names this
page; the caller retries after the consumer has read, or gives up. The driver
reads the ring on every duty cycle, so a ring that stays full means the driver is stalled,
which the consumer heartbeat shows, or that clients issue commands faster than one
conductor reads them. `doctor` reads the tail and head positions from the file and reports
the fill. The transition is a client's, and a client owns no counter in this slice (ADR
0014). Capacity is fixed when the file is created; a larger ring means a restart.

## Counters

Three static counters in `counters.toml` (`spec/counters-file.md`), written by the
driver: `command_ring.unblocked`, `command_ring.rejected_tail` and
`command_ring.rejected_record`.

## What a reader rejects

A reader runs these checks in this order when it attaches and reports the first that
fails, by the name in the first column.

| Reason | Check |
| --- | --- |
| `too-short` | The file is shorter than 768 bytes, or fewer than 128 header bytes were given. |
| `not-ready` | Magic is eight zero bytes: the creator has not finished. Wait and read again. |
| `magic` | Magic is any other value than `CVLYCMDR`. |
| `version` | Version is not 1. |
| `capacity` | Capacity is not a power of two from 4096 to 1073741824. |
| `length` | The file length is not capacity + 768. |
| `changed` | The header read through the final mapping differs from the first read. |

In use: `tail` when a tail position is not a multiple of 8 or, as a producer loads it
after the head cache, is below the head cache or, as the consumer loads it after its own
head, is below the head or more than capacity above it; `record` when a
record's length or type is outside the bounds of "Record"; `full` when a claim finds
fewer free bytes than it needs, with nothing written from step 3 and only the padding to
the end of the ring written from step 4; `message` when a producer is given a message longer than
capacity ÷ 8, and `type` when it is given a type below 1, both refused before anything
is written; `unblocked` when a producer's claim swap finds the index padded, or it finds
its claim word gone before it writes its message, or its commit's or abort's
compare-and-swap fails, with nothing more written;
`driver-closed` when the consumer heartbeat is −1, and `driver-timeout` when it is older
than the driver timeout.

## Vectors

`spec/vectors/command-ring.json`, written by `spec/vectors/command-ring.py`, the
reference encoder. Each vector has a `region`:

- `header`: the 128 bytes at the start of the trailer as `bytes`, the `file_length` the
  reader sees, and the `fields` a conforming reader decodes or the `error` it reports.
- `record`: a record's `bytes` with the `capacity` and `index` it sits at, and the
  `fields` a conforming reader decodes, length, type, state (`unclaimed`, `claimed`,
  `committed` or `padding`), aligned length and message length, or the `error` it
  reports.
- `claim`: an arithmetic vector with `inputs` and no bytes: the capacity, the tail, head
  and head cache a producer loads, and the message length, and the `fields` the claim
  produces: the result, the record and aligned lengths, the tail after, the head cache
  after, the padding record written if any, and the index of the claim; or the `error`.
- `unblock`: an arithmetic vector: the capacity, head and tail the driver loads and the
  length words at 8-byte steps from the consumer index, with words not listed 0, and the
  `fields`: whether it unblocked and the padding record it wrote; or the `error`.
- `read`: an arithmetic vector: the capacity, the head, the message limit and the ring's
  bytes as hex, and the `fields` a read produces: why it stopped, the messages delivered
  in order with their types, the bytes read, the head after, and the bytes it zeroed.
- `commit`: an arithmetic vector: the claim word a producer stores or stored, as its
  length and type, the word found at the record when the producer acts, and the
  operation, `claim` for the swap of step 7 from 0, `write` for the load of step 8,
  `commit` or `abort` for the swap of step 9; and the `fields`: the result, `claimed`,
  `writable`, `committed`, `aborted` or `unblocked`, and the word written, as length and
  type, or none.

A conforming implementation decodes every valid vector to its fields, encodes those
fields to the same bytes, computes every arithmetic vector's fields from its inputs, and
reports the named reason for every invalid one.
