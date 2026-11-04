# 0045. Carry a padding frame on the wire as its 32-byte header alone

- **Status:** Accepted
- **Date:** 2026-11-04
- **Invariants affected:** 1, 3
- **Supersedes:** none. Amends `spec/udp.md` within version 0 with a rule the page did
  not state: how a padding frame is carried and read

## Context

A term of a `cavalry:udp` publication's log is 16 MiB (`spec/udp.md`, "Values"), and a
message whose aligned frame length does not divide it leaves a tail at the end of every
term that the next frame does not fit: the publisher writes a padding frame over the tail
and rotates (`spec/log-buffer.md`, "Claim, commit and abort", step 3). The driver's sender
puts that padding frame on the wire as its 32-byte header alone, carrying the padding's
full length in the header, as Aeron's sender does (`TermReader::block` in
`crates/cavalry-shm/src/log/file.rs`; the burst in `crates/cavalry-driver/src/sender.rs`;
ADR 0041). The receiver checks a datagram's frames against the bytes present
(`cavalry_proto::udp::Datagram::check`, `spec/udp.md`, "What a reader rejects",
`datagram`), so a header whose length runs past the datagram's end is rejected: the
datagram is counted in `receiver.rejected_datagrams` and dropped, the rebuild position
stops below the term end, the receiver NAKs the gap, the sender resends the same header,
and the stream stays there for ever. Had the datagram been accepted, `ImageWriter::insert`
would copy the frame's 32 bytes into a body slice of the frame's length and panic.

Card 182's UDP rows met it on 2026-11-04 with a 100-byte body: 160-byte frames, and
16 MiB over 160 bytes is 104,857.6, so `just bench --only udp --body 100 --messages
1200000` on a laptop stopped with "message 104857 of 151457 did not reach the subscriber
in 10 s", the first term end. Every test before it, the bench's rows, the ten-minute
path's 8-byte bodies and card 181's lossy-shim test among them, used 32-byte or 8-byte
bodies, whose 64-byte frames divide the term exactly, so no padding frame had crossed the
wire. `spec/udp.md` said that a datagram carries data or padding frames and that the
sender counts a padding frame as sent whether or not it was put on the wire, and nothing
of how one is carried or read: unspecified behaviour, which is a bug in the spec before
it is one in the code (`CLAUDE.md`, Protocol).

## Decision

A padding frame crosses the wire as its 32-byte header alone and is the last frame of
its datagram. Its frame length is the padding's, checked against the term, so that the
frame ends at or before the term's end, and not against the bytes present; it counts its
aligned length in the term, so the datagram's end position, the receiver's high-water
mark and the sender position advance by it. The receiver writes the header at its term
offset, by the same protocol as a data frame's, and nothing after it: the body of a
padding frame is zeros a reader never reads, and the image's term is zero before a frame
lands in it, so the image's readers skip the padding by its aligned length as a log's
reader does (`spec/log-buffer.md`, "Reading"), and a recording of the image writes it as
a publication's recording does. A padding frame carried with bytes after its header, or
followed by another frame in the datagram, fails the `datagram` check. The page stays at
version 0: no field changes, a datagram the page already specified reads as it did, and
a reader of today rejects a padding datagram as it does now. `sender.bytes_sent` and
`receiver.bytes_received` count the bytes on the wire, 32 for a padding frame, and the
frame counters count it as one frame. The vectors carry a padding frame alone, after a
data frame, in the middle of a term as an aborted claim leaves it, and the three
rejections.

## Alternatives considered

- **Carry the padding frame whole, its zeros included.** Lost: a tail is up to
  16 MiB − 32 bytes of zeros, twelve thousand datagrams of nothing per term at the MTU,
  and Aeron's sender carries the header alone.
- **Accept both shapes, the header alone and the whole frame.** Lost: one frame, one
  shape; a reader that accepts bytes after a padding header has no check for them, and
  the sender has produced one shape since ADR 0041.
- **A version 1 of the page.** Lost: no field changes, and nothing the page specified
  reads differently; a version is for a change a reader of the old version would misread
  (`spec/README.md`), and a reader of version 0 rejects the padding datagram today as it
  rejects it tomorrow.
- **Choose a body length whose frame divides the term.** Lost: a message has the length
  the user gives it, and a stream carries any.

## Evidence

Before, on a laptop on 2026-11-04 at master (715861d) with the test below added: the
stream stopped at message 104,857 of 210,715, the first term end, and stayed there for
the three minutes the test allows, the publisher back-pressured behind it; card 182's
bench measurement above is the same stall. After: the six padding vectors among the 102
in `spec/vectors/udp.json` pass in `crates/cavalry-proto/tests/udp_conformance.rs`; the
test `a_stream_whose_frames_do_not_divide_the_term_crosses_two_term_ends_through_the_shim`
in `crates/cavalry-client/tests/udp_receive.rs` drives a 100-byte body, 160-byte frames,
across two term ends through the lossy shim, which drops the first padding datagram it
sees so that the padding is NAKed and resent, and every one of 210,714 messages arrives
once and in order in under a second, `receiver.frames_received` reading the messages plus
the two padding frames and the image's position the publication's, two terms and a
thousand frames; `just replay-check` passes. The receiver's first parse of a datagram
was a second site of the same check, found by that test: the fix is in the receiver and
the datagram check both, each holding a padding header against the term.

## Consequences

The bench's `--body` runs the UDP rows at any length, and the ten-minute path's UDP step
is no longer the only shape that works. A padding frame lost on the wire is a gap like any
other: the receiver reports it as far as the scan bound, 1 MiB, and NAKs that length, and
the sender's answer is the padding block whole, since a block scan always takes the first
frame at the position, so one NAK fills the whole tail. Nothing new is bounded or
unbounded: the datagram is 32 bytes. Revisit: when a sender of a later version carries a
padding frame otherwise, for a version of the page.

## References

- `spec/udp.md`, "The datagram", "The receiver" step 2, "What a reader rejects",
  "Vectors"; `spec/frame.md`, "Padding frame"; `spec/log-buffer.md`, "Reading".
- Card 196 (the bug and its evidence); card 182 (the rows that found it); ADR 0041 (the
  sender's padding block); ADR 0042 (the receiver's rebuild).
