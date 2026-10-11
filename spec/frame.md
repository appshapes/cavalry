# Data frame header, version 0

The data frame header is the first 32 bytes of every data frame and of every padding
frame, on the wire and in a log buffer. A message that fits in one frame is one data
frame. A longer message is a sequence of fragments, each a data frame, reassembled by
the subscriber; how a message is split is specified with the transport that splits
it, not here. In Aeron, this is the data frame header of the transport protocol
specification, field for field (ADR 0008; `docs/glossary.md`).

## Layout

Offsets are in bytes. Integers are little-endian. The header is 32 bytes, and a frame
begins at an offset that is a multiple of 32 within its term or packet.

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         Frame length                          |
+---------------+---------------+-------------------------------+
|    Version    |     Flags     |             Type              |
+---------------+---------------+-------------------------------+
|                         Term offset                           |
+---------------------------------------------------------------+
|                          Session id                           |
+---------------------------------------------------------------+
|                          Stream id                            |
+---------------------------------------------------------------+
|                           Term id                             |
+---------------------------------------------------------------+
|                                                               |
|                        Reserved value                         |
|                                                               |
+---------------------------------------------------------------+
|                             Body                             ...
```

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | frame length | i32 | Length of the frame in bytes, header included, before alignment padding. At least 32. Never negative on the wire: the log buffer spec uses the sign bit for its claim protocol. |
| 4 | 1 | version | i8 | 0 for this layout. |
| 5 | 1 | flags | u8 | The bits in the table below. |
| 6 | 2 | type | u16 | 0 for a padding frame, 1 for a data frame. Other types are specified with their frames. |
| 8 | 4 | term offset | i32 | Byte offset of the frame from the start of its term. At least 0 and a multiple of 32. |
| 12 | 4 | session id | i32 | The publication's session. |
| 16 | 4 | stream id | i32 | The stream the frame belongs to. |
| 20 | 4 | term id | i32 | The term the frame is in. |
| 24 | 8 | reserved value | i64 | Carried from publisher to subscriber unchanged. The application's to use; 0 when unused. |
| 32 | frame length − 32 | body | bytes | The fragment's bytes in a data frame. Not meaningful in a padding frame. |

The next frame begins at the frame's offset plus its frame length rounded up to a
multiple of 32. The bytes between the end of the body and that boundary are
alignment padding; a reader does not read them, and they need not be present after
the last frame in a buffer.

## Flags

| Bit | Mask | Name | Meaning |
| --- | --- | --- | --- |
| 7 | `0x80` | BEGIN | The first fragment of a message. |
| 6 | `0x40` | END | The last fragment of a message. BEGIN and END together, `0xC0`, mean the message is this one frame. |
| 5 | `0x20` | EOS | End of stream: the publication closed, and nothing follows on this session. |
| 4 to 0 | `0x1F` | reserved | A writer sets them to 0. A reader of version 0 ignores them, so that a later version can set them. |

## Padding frame

A padding frame has type 0 and the header above. Its frame length is the length of
the padding, at least 32. It fills the end of a term when the next frame would not
fit, or the space of a claim that was abandoned. Its body is not meaningful, a
subscriber skips it and delivers nothing, and its flags are set to 0 by a writer and
ignored by a reader. Term offset, session id, stream id and term id are set as they
would be for a data frame at that place; the reserved value is 0.

## What a reader rejects

A reader is given a byte range and reads one frame from its start. It runs these
checks in this order and reports the first that fails, by the name in the first
column. A rejected frame is never delivered, and each rejection increments a counter
once the counters file exists (ADR 0007).

| Reason | Check |
| --- | --- |
| `too-short` | Fewer than 32 bytes were given. |
| `version` | The version byte is not 0. |
| `type` | The type is one this reader does not handle. This page specifies 0 and 1. |
| `length` | The frame length is below 32, or greater than the number of bytes given. A negative value is below 32. |

## Vectors

`spec/vectors/frame.json`, written by `spec/vectors/frame.py`, which is this page's
reference encoder. Each vector has a `name`, its `bytes` as hex, and either a `header`
with the nine fields a conforming reader decodes or an `error` with the reason it
reports. A conforming implementation decodes every valid vector to its fields, encodes
those fields to the first frame-length bytes of the vector, and reports the named
reason for every invalid one.
