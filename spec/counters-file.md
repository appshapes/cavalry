# Counters file, version 0

The counters file is the shared-memory file where the driver publishes every counter:
the primary instrument, cheaper than a log line, and it survives a crash (invariant 4).
The driver creates it and is the only writer. Clients, `cavalry doctor` and the control
plane map it read-only. In Aeron, this is the counters metadata and values buffers of
the CnC file, `cnc.dat` (ADR 0012; `docs/glossary.md`). It is the first layout written
under ADR 0010 and ADR 0011: every field below names its writer, its readers and the
bound a reader checks, and every counter value has a 128-byte slot of its own.

## File

The file is named `counters` in the driver's directory; the directory is specified with
the driver. It has three regions, in this order, and no gaps:

| Region | Offset | Length | Holds |
| --- | --- | --- | --- |
| header | 0 | 128 | the table below |
| metadata | 128 | capacity × 256 | one record per counter: what it is |
| values | 128 + capacity × 256 | capacity × 128 | one slot per counter: its value |

The file length is 128 + capacity × 384 bytes. Record i and slot i belong to counter i,
for i from 0 to capacity − 1. Integers are little-endian (`spec/README.md`).

## Header

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 8 | magic | bytes | the creator, last, with a release store | everyone, first, with an acquire load | the bytes `43 56 4C 59 43 4E 54 52` (`CVLYCNTR`) |
| 8 | 4 | version | i32 | the creator | everyone | 0 |
| 12 | 4 | capacity | i32 | the creator | everyone | a power of two from 64 to 65536 |
| 16 | 8 | creator pid | i64 | the creator | `doctor` | greater than 0 |
| 24 | 8 | start timestamp | i64 | the creator | `doctor` | nanoseconds since the Unix epoch, 0 or more |
| 32 | 96 | reserved | bytes | the creator | nobody | written as 0, never read |

The creator sizes the file to its full length, writes zero everywhere, fills the
metadata and values regions, fills the header fields after magic, and writes magic last
with a release store. A reader loads magic with an acquire load before it reads anything
else: a magic of eight zero bytes means the creator has not finished, which is
`not-ready`, not a fault. The header never changes after magic is written; a reader
re-reads it through its final mapping and treats a difference from its first read as
`changed` (ADR 0010).

## Metadata record

Record i is at offset 128 + i × 256 and is 256 bytes, Aeron's counter metadata record
(ADR 0012).

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 4 | state | i32 | the driver, with a release store | everyone, with an acquire load | 0 unused, 1 allocated, −1 reclaimed |
| 4 | 4 | type id | i32 | the driver | everyone | the `id` of a counter in `counters.toml` |
| 8 | 8 | reclaim deadline | i64 | the driver | the driver | nanoseconds since the Unix epoch, or 0 |
| 16 | 112 | key | bytes | the driver | everyone | opaque; the counter's `doc` in `counters.toml` says its layout, and a static counter's key is all zero |
| 128 | 4 | label length | i32 | the driver | everyone | 0 to 124 |
| 132 | 124 | label | bytes | the driver | everyone | UTF-8; the first label-length bytes are the label, the rest are 0 |

To allocate counter i the driver writes type id, key, label length and label, sets slot
i's value to 0, then sets state to 1 with a release store. A reader loads state with an
acquire load before it reads the rest of the record or the slot, and a record whose
state is not 1 is not a counter. To free counter i the driver sets state to −1 and
reclaim deadline to the time after which the record may be reused; it reuses the record
by allocating it again, after the deadline, which rewrites every field. The label of a
static counter is its name in `counters.toml`; the label of an instance counter is that
name, a space, and the key's text as the counter's `doc` defines it.

## Value slot

Slot i is at offset 128 + capacity × 256 + i × 128 and is 128 bytes, so that two
counters never share the 128 bytes a core moves at once (ADR 0011).

| Offset | Size | Field | Type | Writer | Readers | Bound |
| --- | --- | --- | --- | --- | --- | --- |
| 0 | 8 | value | i64 | the one thread that owns the counter, with relaxed stores or adds | everyone, with relaxed loads | any value; a counter of kind `counter` never decreases while allocated, a `gauge` moves both ways |
| 8 | 120 | reserved | bytes | the driver | nobody | written as 0 at allocation, never read |

Nobody branches on a value on the data plane, so relaxed ordering is enough
(`.claude/rules/data-plane.md`). A reader that needs a consistent pair of counters
reads each twice and retries when they moved.

## `counters.toml`

`counters.toml` at the repository root is the single source of every counter. Each
`[[counter]]` entry has an `id` that is never reused, a `name`, a `kind` (`counter` or
`gauge`), a `scope` (`static`, one counter allocated when the driver starts, with the
record whose type id is the entry's id; or `instance`, one counter per key, allocated
when the instance is created), a `doc` sentence and a `doctor` sentence. The reference
page `docs/reference/counters.md` and the Rust constants in `cavalry-shm` are generated
from it, and `just lint-counters` fails when either is stale or an entry lacks a field.

## When the file is full

An allocation that finds no record in state 0 and none in state −1 past its deadline
fails with `full`: the driver increments `counters.allocation_failed`, refuses whatever
needed the counter with an error that names this page, and allocates nothing. Capacity
is fixed when the file is created; a larger file means a restart.

## What a reader rejects

A reader runs these checks in this order and reports the first that fails, by the name
in the first column. A reader of this file has no counter to increment when it rejects
the file, so it returns the error, and `cavalry doctor` reports it.

| Reason | Check |
| --- | --- |
| `too-short` | The file is shorter than 128 bytes. |
| `not-ready` | Magic is eight zero bytes: the creator has not finished. Wait and read again. |
| `magic` | Magic is any other value than `CVLYCNTR`. |
| `version` | Version is not 0. |
| `capacity` | Capacity is not a power of two from 64 to 65536. |
| `length` | The file is shorter than 128 + capacity × 384 bytes. |
| `changed` | The header read through the final mapping differs from the first read. |

For a record: `id` when a counter id is not below capacity; `state` when state is not
−1, 0 or 1; `label` when label length is not 0 to 124. The checks are integer compares;
the file length comes from the file's size, read once before the mapping is made.

## Vectors

`spec/vectors/counters-file.json`, written by `spec/vectors/counters-file.py`, the
reference encoder. Each vector has a `region`, `header` or `record`, its `bytes` as hex,
and either the `fields` a conforming reader decodes or the `error` it reports; a header
vector also gives the `file_length` the reader sees. A conforming implementation decodes
every valid vector to its fields, encodes those fields to the same bytes, and reports
the named reason for every invalid one.
