# Log buffer, version 0

The log buffer is the shared-memory file that carries one publication's stream between
processes on one host: three term buffers that publishers fill with frames and
subscribers read in place, and a metadata page that says where the stream is. The
driver creates it and keeps it healthy; publishers write it; subscribers read it. In
Aeron, this is the log buffer, the `.logbuffer` file that `LogBufferDescriptor`
describes (ADR 0013; `docs/glossary.md`). Every field below names its writer, its readers
and the bound a reader checks (ADR 0010), and every field that one party writes and
another reads in its hot loop has a 128-byte slot of its own (ADR 0011). The frames
inside a term are `spec/frame.md`.

## File

The file is named `publications/<correlation id>.logbuffer` in the driver's directory,
where the correlation id is the decimal value of the header field below. It has four
regions, in this order:

| Region | Offset | Length | Holds |
| --- | --- | --- | --- |
| term 0 | 0 | term length | the terms whose term count is 0 modulo 3 |
| term 1 | term length | term length | the terms whose term count is 1 modulo 3 |
| term 2 | 2 × term length | term length | the terms whose term count is 2 modulo 3 |
| metadata | 3 × term length | 4096 | the tables below |

The file length is exactly 3 × term length + 4096 bytes, and the term length is a
power of two from 65536 to 1073741824, so each term begins at a multiple of its own
length and a reader finds the metadata page from the file's size. Integers are
little-endian (`spec/README.md`). Offsets in the metadata tables are from the start of
the metadata page.

## Header

The first 128 bytes of the metadata page. The creator writes every field, then the
magic last with a release store; a reader loads the magic with an acquire load before it
reads anything else, and eight zero bytes mean `not-ready`. The header never changes
after the magic is written, and a reader re-reads it through its final mapping and
treats a difference as `changed` (ADR 0010).

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the creator, last, with a release store | everyone, first, with an acquire load | the bytes `43 56 4C 59 4C 4F 47 42` (`CVLYLOGB`) |
| 8 | 4 | version | i32 | the creator | everyone | 0 |
| 12 | 4 | type | i32 | the creator | everyone, `doctor` | 0, a concurrent publication, whose tail any number of publishers advance with fetch-and-add; 1, an exclusive publication, whose one publisher advances the tail with release stores |
| 16 | 8 | correlation id | i64 | the creator | everyone | any; the publication's registration with the driver, and the file's name |
| 24 | 4 | initial term id | i32 | the creator | everyone | any; term count 0 is this term id |
| 28 | 4 | term length | i32 | the creator | everyone | a power of two from 65536 to 1073741824 |
| 32 | 4 | MTU length | i32 | the creator | publishers | a multiple of 32 from 64 to 65504; the longest frame a message is split into |
| 36 | 4 | publication window length | i32 | the creator | the driver | 1 to term length ÷ 2; how far the publisher may run ahead of the slowest subscriber |
| 40 | 24 | reserved | bytes | the creator | nobody | written as 0, never read |
| 64 | 32 | default frame header | bytes | the creator | publishers, the driver | a data frame header (`spec/frame.md`) with frame length 0, version 0, flags `C0`, type 1, term offset 0, the publication's session id and stream id, term id equal to the initial term id, and reserved value 0 |
| 96 | 32 | reserved | bytes | the creator | nobody | written as 0, never read |

## Metadata page

The rest of the page. A field written after creation has a 128-byte slot of its own
(ADR 0011). Reserved bytes are written as 0 by the creator and never read.

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 128 | header | bytes | the creator | everyone | the table above |
| 128 | 8 | tail counter 0 | i64 | publishers, when they claim; a publisher or the driver, when they rotate | publishers, the driver, with acquire loads | term id in the high 32 bits, term offset in the low 32 bits, as "Tail counters" says |
| 256 | 8 | tail counter 1 | i64 | as tail counter 0 | as tail counter 0 | as tail counter 0 |
| 384 | 8 | tail counter 2 | i64 | as tail counter 0 | as tail counter 0 | as tail counter 0 |
| 512 | 4 | active term count | i32 | a publisher or the driver, at rotation, with a compare-and-swap | publishers, the driver, with acquire loads | 0 to 2^31 − 1; the tail counter of partition (count modulo 3) holds term id initial term id + count |
| 640 | 8 | end of stream position | i64 | the driver, with a release store | subscribers, with acquire loads | 2^63 − 1 while the publication is open; its final position once it is closed |
| 704 | 4 | is connected | i32 | the driver, with a release store | publishers | 0 or 1: whether at least one subscriber has a position on this log |
| 708 | 3388 | reserved | bytes | the creator | nobody | written as 0, never read |

To create a log the driver sizes the file to its full length, writes zero everywhere,
sets tail counter 0 to (initial term id, 0) and tail counter i, for i of 1 and 2, to
(initial term id + i − 3, 0), sets the active term count to 0, the end of stream
position to 2^63 − 1, is connected to 0, fills the header fields after the magic, and
writes the magic last with a release store. Nothing else in the file is written until a
publisher claims.

## Positions

A position is a byte count from the start of the stream. With position bits the base-2
logarithm of the term length: the term count of a term id is term id − initial term id,
computed in 32-bit two's complement so that a term id that wrapped past 2^31 − 1 still
counts upward; a position is term count × term length + term offset; the partition of a
position is (position ≫ position bits) modulo 3, its term offset is position modulo term
length, and its term id is initial term id + (position ≫ position bits). The largest
position is term length × 2^31; a claim that would reach it fails with `max position
exceeded`, and the publication is closed and recreated. A position given to a reader
is a multiple of 32, at least the reader's current position and at most the end of the
current term; otherwise the reader rejects it with `position`.

## Tail counters

A tail counter packs a term id into its high 32 bits and a term offset into its low
32 bits. The term offset read from a tail is the low 32 bits, or the term length when
they exceed it: a claim that ran past the end leaves a larger value behind, and that
means "the term is full". The active partition is the active term count modulo 3; the
term id in its tail must be the initial term id + active term count, and a publisher
that finds otherwise has raced a rotation and retries (`admin action`), while the
driver, which finds otherwise on its duty cycle, reports `tail`. The producer position
is the position of the active tail.

## Claim, commit and abort

A publisher writes a frame in place:

1. It loads the active term count with an acquire load, the tail of the active
   partition with an acquire load, checks the tail's term id as above, and computes the
   producer position. If the position is not below the publisher limit, the counter
   the driver maintains (`publication.limit` in `counters.toml`), the claim fails: with
   `max position exceeded` when position + aligned length would reach the largest
   position, else with `back pressured` when is connected is 1, else with `not
   connected`. Nothing is written.
2. It reserves the frame's aligned length, the frame length rounded up to a multiple of
   32. In a concurrent publication (type 0) it adds the aligned length to the tail with
   a fetch-and-add, and the value before the add is its term id and term offset. In an
   exclusive publication (type 1) it keeps the term id and offset itself, having read
   them once when it attached or rotated, and after the claim stores the new tail with a
   release store.
3. If term offset + aligned length is greater than the term length, the frame does not
   fit. If the term offset is below the term length, the publisher writes a padding
   frame from the term offset to the end of the term, by the protocol of step 4 with
   type 0 and the padding's length. If the resulting position would reach the largest
   position, the result is `max position exceeded`. Otherwise it rotates (below) and the
   result is `admin action`: the caller retries, and the retry lands in the next term.
4. Otherwise it writes the claim header: the frame's first eight bytes, frame length
   as the negative of the frame length together with the version, flags and type bytes
   of the default frame header, in one 8-byte release store; then term offset and
   session id as one 8-byte store, stream id and term id as one 8-byte store. The
   reserved value keeps the 0 the driver cleaned it to unless the publisher sets it.
   The body follows the header. A reader that loads a negative frame length knows a
   claim is in progress and stops; the driver knows the same when it unblocks.
5. Commit: the publisher stores the positive frame length with a release store. Abort:
   it stores type 0 (padding) and then the positive frame length with a release store,
   and a reader skips the frame.

A publisher that copies a message in (offer) runs the same steps with the body written
before the release store of step 5. A message longer than the maximum payload, MTU
length − 32, is fragmented: one reservation of the framed length, the sum of the
aligned lengths of every fragment, then each fragment in order, with the BEGIN flag on
the first, the END flag on the last, and each fragment's length released as it is
written; the fragments of one message are consecutive within one term. The longest
message is the smaller of term length ÷ 8 and 16777216 bytes; a longer one is refused
by the caller's API before anything is reserved.

## Rotation

With term count c and term id t the active term, rotation moves the stream to partition
(c + 1) modulo 3: load that partition's tail with an acquire load; if its term id is not
t + 1 − 3, another party has already rotated it, and this step is done; otherwise
compare-and-swap the tail from the value loaded to (t + 1, 0), and repeat on failure;
then compare-and-swap the active term count from c to c + 1. Any publisher that finds
the term full may rotate, and so may the driver when it unblocks; the compare-and-swaps
make the rotation happen once.

Rotation is safe only because the partition rotated into has been cleaned. The
publisher never runs more than the publication window length ahead of the slowest
subscriber, the window is at most half a term, and the driver cleans everything the
slowest subscriber has consumed before it raises the limit (below), so when the stream
enters term c + 1 every byte of term c − 2, which shared the partition, is zero.

## Reading

A subscriber reads from its position: the partition and term offset as "Positions"
says. At the offset it loads the frame length with an acquire load. A length of 0 or
less means nothing more is committed, and the read stops. Otherwise the frame is checked
as `spec/frame.md` says, with the bytes from the offset to the end of the term as the
bytes given; a frame that fails is reported to the subscriber's conductor with the
reason, and nothing from it on is delivered. A data frame is delivered as its body, the
bytes after the 32-byte header, with its header; a padding frame delivers nothing. The
read advances by the aligned length and stops at the end of the term. After a batch
the subscriber stores its new position in its position counter (`subscription.position`
in `counters.toml`) with a release store, so that the driver's acquire load of that
position orders the subscriber's reads before any cleaning. A subscriber whose position
is at or past the end of stream position has seen everything the publication wrote.

## The driver

The driver creates the log, and on its duty cycle:

- **Positions.** It stores the producer position in `publication.position`, and the
  publisher limit, the minimum subscriber position + the publication window length, in
  `publication.limit`, each with a release store. It loads the producer position at
  most once per millisecond per publication, by its own clock, and unblocking compares
  against the position so loaded, so that the publisher's cache line is not read on
  every cycle; `publication.position` is at most a millisecond behind the publisher
  (ADR 0022). It raises the limit only when the new value is at least window ÷ 8
  beyond the last, so that the publisher's cache line is not written on every cycle,
  and only after cleaning. With no subscriber the limit is
  the consumer position, the largest position any subscriber reached, so the publisher
  is told `not connected`. It sets is connected to 1 while a subscriber has a position
  and to 0 otherwise.
- **Cleaning.** Before it raises the limit to L, it zeroes every byte from the clean
  position, the end of the last cleaning, up to L − window, the minimum subscriber
  position, one term at a time: the bytes after the first eight of the range with plain
  stores, then the first eight bytes with a release store, so a reader that loads a
  frame length of 0 never sees a half-cleaned frame. The clean position becomes the end
  of the range.
- **Unblocking.** A publisher that claimed and died, or never committed, leaves a frame
  length that is negative or 0 at the consumer position while the producer position is
  beyond it or the active term count is beyond the consumer position's term count. When
  the consumer position has not moved for the unblock timeout, the driver unblocks at
  that offset: a negative length becomes a padding frame of its magnitude, written as
  the default frame header with type 0, the term offset and term id, and the length
  with a release store; a length of 0 is scanned forward in 32-byte steps to the tail's
  offset, and if a nonzero length is found with only zeros before it, a padding frame
  covers the gap, while if the end of the term is reached with only zeros, a padding
  frame covers the rest of the term and the driver rotates. If the consumer position is
  the first byte of the term after the active one, the driver only rotates. Each unblock
  adds one to `publication.unblocked`.
- **Closing.** When the publication is closed it loads the producer position afresh
  and stores it in the end of stream position with a release store, and removes the
  file once every subscriber has reached that position or the publication's linger has
  passed.
- **Checks.** Before it uses a value a client wrote, a subscriber position or a tail, it
  checks the bound the tables name; a value outside it adds one to
  `log_buffer.rejected_position` or `log_buffer.rejected_tail`, and the client is
  evicted (ADR 0010). A frame a subscriber reported adds one to
  `log_buffer.rejected_frame`.

The unblock timeout and the linger are the driver's to specify; Aeron's are 15 s and
5 s.

## Counters

The log buffer's positions live in the counters file (`spec/counters-file.md`), as
instance counters of `counters.toml`: `publication.position` and `publication.limit`,
written by the driver, and `subscription.position`, written by the subscriber that owns
it. The key of each is the publication's correlation id (i64), session id (i32) and
stream id (i32), little-endian, 16 bytes; the label is the counter's name, a space, and
the three values in decimal separated by spaces. A position is stored with a release
store and loaded with an acquire load by whoever branches on it, the driver on a
subscriber's position and the publisher on its limit, the one exception to the counters
file's relaxed rule (ADR 0013). `publication.unblocked` and the three
`log_buffer.rejected_*` counters are static.

## What a reader rejects

A reader runs these checks in this order when it attaches and reports the first that
fails, by the name in the first column.

| Reason | Check |
| --- | --- |
| `too-short` | The file is shorter than 4096 bytes, or fewer than 128 header bytes were given. |
| `not-ready` | Magic is eight zero bytes: the creator has not finished. Wait and read again. |
| `magic` | Magic is any other value than `CVLYLOGB`. |
| `version` | Version is not 0. |
| `type` | Type is not 0 or 1. |
| `term-length` | Term length is not a power of two from 65536 to 1073741824. |
| `mtu` | MTU length is not a multiple of 32 from 64 to 65504. |
| `window` | Publication window length is not 1 to term length ÷ 2. |
| `frame-header` | The default frame header is not a version 0 data frame header with frame length 0, flags `C0`, term offset 0, term id equal to the initial term id and reserved value 0. |
| `length` | The file length is not 3 × term length + 4096. |
| `changed` | The header read through the final mapping differs from the first read. |

In use: `position` when a position given to a reader is not a multiple of 32, is below
the reader's position or is beyond the end of its term; `tail` when the active tail's
term id is not initial term id + active term count, which a publisher retries and the
driver reports; and a frame's reasons are `spec/frame.md`'s.

## Vectors

`spec/vectors/log-buffer.json`, written by `spec/vectors/log-buffer.py`, the reference
encoder. Each vector has a `region`:

- `header`: the first 128 bytes of the metadata page as `bytes`, the `file_length` the
  reader sees, and the `fields` a conforming reader decodes or the `error` it reports.
- `tail`: eight `bytes`, a tail counter, with the `term_length`, and the `fields` term id
  and term offset.
- `position`: an arithmetic vector with `inputs` and no bytes: initial term id and term
  length with either a term id and term offset or a position, and the `fields` term
  count, term id, term offset, position and partition that follow.
- `claim`: an arithmetic vector: the tail and active term count before a claim of a
  frame length against a limit, and the `fields` the claim produces: the result, the
  tail after, the padding frame written if any, and the tails and active term count
  after a rotation if one happened.
- `claim-header`: the 32 `bytes` a publisher writes at step 4 of the claim for the
  given `inputs`, and the `fields` `committed` and `aborted`, the same 32 bytes after
  step 5 either way.

A conforming implementation decodes every valid vector to its fields, encodes those
fields to the same bytes, computes every arithmetic vector's fields from its inputs, and
reports the named reason for every invalid one.
