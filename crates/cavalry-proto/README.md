# cavalry-proto

Wire types, zero-copy frames and version negotiation. Data plane.

## What this does

- Defines every frame on the wire as a `#[repr(C)]` struct with a versioned header,
  using `zerocopy` derives (ADR 0009), so a frame is read and written in place with no
  copy and no allocation.
- Parses and builds the frames that carry data fragments (the wire-sized pieces of a
  message larger than the MTU), `spec/frame.md`, and the UDP transport's control frames,
  `spec/udp.md`: the setup frame, the status message (a receiver's position and window,
  which drive flow control), the NAK (a receiver's report of a gap), the RTT measurement
  and the heartbeat, in `udp`, with the spec's checks in the spec's order, a datagram
  walker that checks a packet of data frames whole before any is read, and the
  arithmetic of the sender's and the receiver's rules. Every frame carries a version
  byte, and a reader drops a frame of a version or type it does not know, so that mixed
  versions run during an upgrade.
- Parses a channel, `cavalry:ipc` or `cavalry:udp?endpoint=<host>:<port>` with its
  `interface` option, in `channel`, borrowing from the string, with each refusal named
  and every refusal error code 1; resolving the host is the driver's.
- Is the one place bytes are shuffled by hand. Every other crate uses its types. A
  reader that holds only a frame's header, as the log buffer's does, passes the bytes
  left in its term to `parse_within`, and the same four checks run against them.
- Encodes and decodes the client protocol, `spec/client-protocol.md`: the commands a
  client sends the driver through the command ring and the events the driver answers
  with through the broadcast buffer, in `client_protocol`, with the spec's decoding
  checks in the spec's order and strings borrowed from the message, never copied.

In Aeron, this is the transport protocol specification and its frame layouts
([glossary](../../docs/glossary.md)).

## What it must never do

- Implement behaviour that `spec/` does not specify. Unspecified behaviour is a bug in
  the spec, not a freedom in this crate. A wire change is a change to `spec/`, a
  conformance test and a version bump, in that order, before any code.
- Change a layout without changing its version. A layout change is a wire-protocol
  change.
- Trust an incoming frame. Frame parsers are fuzzed; a malformed frame is rejected with
  a counter, never a panic. The one decoder without checks, `decode_unchecked`, exists
  only in a build made with `--cfg cavalry_unchecked`, which `just bench-unchecked` makes
  to measure what the checks cost (ADR 0010); no shipped build has it.
- Allocate, lock, make a syscall, await or panic per message (invariant 2).
- Invent a name. The vocabulary is the glossary's: publication, subscription, image,
  term, position, NAK, status message, stream id, session id.

## Example

A publisher writes one message as one frame, a header then a body, into a slot of a
term. A subscriber reads the header back in place and checks it before it trusts the
body. This is the crate's doctest.

```rust
use cavalry_proto::frame::{DataFrameHeader, Flags, FrameError, FrameType, HEADER_LENGTH};

fn main() -> Result<(), FrameError> {
    // The publisher: an 8-byte message that fits in one frame.
    let body = b"cavalry!";
    let header = DataFrameHeader::new(FrameType::Data, Flags::UNFRAGMENTED, HEADER_LENGTH + body.len())?
        .with_session_id(1)
        .with_stream_id(1001);
    let mut frame = [0u8; 64]; // a slot in a term, 32-byte aligned
    header.write(&mut frame)?;
    frame[HEADER_LENGTH..HEADER_LENGTH + body.len()].copy_from_slice(body);

    // The subscriber: read the header in place and check it before touching the body.
    let read = DataFrameHeader::parse(&frame)?;
    assert_eq!(read.frame_type(), FrameType::Data);
    assert_eq!(read.stream_id(), 1001);
    assert_eq!(read.body_length(), body.len());
    assert_eq!(&frame[HEADER_LENGTH..][..read.body_length()], body);
    assert_eq!(read.aligned_length(), 64); // where the next frame begins

    // A frame from a version this reader does not know is rejected, never trusted.
    frame[4] = 1;
    let rejected = DataFrameHeader::parse(&frame).unwrap_err();
    assert_eq!(rejected.reason(), "version");
    println!("{rejected}");
    Ok(())
}
```

Running it prints one line, the rejection with its reason, what to do and the docs
anchor:

```text
frame rejected (version): version 1, this reader knows 0; the writer runs another Cavalry version, upgrade the older side; see docs/reference/frame.md#what-a-reader-rejects
```

## What to read next

- [`spec/frame.md`](../../spec/frame.md): the layout, the flags, the padding frame and
  the four checks a reader runs, in order. The book shows the same page as
  [`docs/reference/frame.md`](../../docs/reference/frame.md).
- [ADR 0008](../../docs/decisions/0008-adopt-aeron-data-frame-header.md), why the
  header is Aeron's, and [ADR 0009](../../docs/decisions/0009-read-wire-structs-with-zerocopy.md),
  why `zerocopy`.
- [`spec/vectors/frame.json`](../../spec/vectors/frame.json), which
  `tests/conformance.rs` runs on every `just test`; `tests/no_panic.rs` and `fuzz/`,
  which feed the parser bytes it must never panic on.
- [`spec/client-protocol.md`](../../spec/client-protocol.md), the commands and events
  `client_protocol` reads and writes, and [ADR 0019](../../docs/decisions/0019-adopt-aeron-control-protocol-as-the-client-protocol.md),
  why they are Aeron's; [`spec/vectors/client-protocol.json`](../../spec/vectors/client-protocol.json),
  which `tests/client_protocol_conformance.rs` runs; `tests/client_protocol_no_panic.rs`
  and `fuzz/fuzz_targets/client_protocol.rs`, which feed the decoder bytes it must
  never panic on, on every commit and under `just fuzz`.
- [`spec/udp.md`](../../spec/udp.md), the UDP transport's frames, datagram, channel URI
  and rules `udp` and `channel` implement, and [ADR 0040](../../docs/decisions/0040-adopt-aerons-transport-frames-as-the-udp-transport.md),
  why they are Aeron's; [`spec/vectors/udp.json`](../../spec/vectors/udp.json), which
  `tests/udp_conformance.rs` runs; `tests/udp_no_panic.rs`, `tests/channel_no_panic.rs`
  and the `udp` and `channel` fuzz targets.
