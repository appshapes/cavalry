# 0040. Adopt Aeron's control frames and rules as the UDP transport, version 0, with log buffer version 1 and client protocol version 4

- **Status:** Proposed
- **Date:** 2026-10-31
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** none; amends `spec/log-buffer.md` to version 1 and
  `spec/client-protocol.md` to version 4, as ADR 0013's and ADR 0019's Revisit
  clauses said UDP would

## Context

Card 179 is the first feature of the third slice (ADR 0039): the UDP transport
specified before any code sends a datagram. A wire format cannot be undone
(`CLAUDE.md`, When in doubt), so this ADR lands Proposed and Rjae accepts it; the
three pages it decides, their vectors and the two crates that read and write them land
with the acceptance, as ADR 0024's format did with its (card 92) and ADR 0031's with
its (card 95). What the slice needs from the pages is fixed by ADR 0039: Aeron's
control frames field for field; Aeron's rules for setup, status messages, NAKs,
heartbeats and flow control, with Aeron's defaults; a log buffer type for the image the
receiver rebuilds; a `cavalry:udp` channel on the five commands that carry one, the
channel status indicator id ADR 0019 left out "since IPC has no endpoint and the UDP
version adds it", the source identity as an address, and Aeron's error codes 4 and 9.
What is already specified and does not change: the data frame header (`spec/frame.md`,
version 0, whose "other types are specified with their frames"), the log's three
terms, its claim protocol and its reader (`spec/log-buffer.md`), and the command ring,
the broadcast buffer and every command and event of versions 0 to 3
(`spec/client-protocol.md`).

Aeron's transport, read from its source on 2026-10-31 (`aeron_udp_protocol.h`; the
`Setup`, `StatusMessage`, `Nak`, `RttMeasurement`, `Error` and `DataHeader`
flyweights; `NetworkPublication`, `Sender`, `SendChannelEndpoint`,
`UnicastFlowControl`, `RetransmitHandler`; `PublicationImage`, `Receiver`,
`ReceiveChannelEndpoint`, `DataPacketDispatcher`, `LossDetector`, `TermRebuilder`,
`TermGapScanner`, `StaticDelayGenerator`; `LogBufferDescriptor`, `ChannelUri`, `CommonContext`, the
command flyweights, `ErrorCode`, `UdpChannel`, `DriverConductor`, `PublicationParams`
and `Configuration`). Every frame begins with the data frame header's first eight bytes,
and the types are PAD 0, DATA 1, NAK 2, SM 3, ERR 4, SETUP 5, RTTM 6, RES 7, with 8 to 11
taken by Aeron's encrypted and response variants. A setup frame is 40 bytes: the common
eight, then term offset, session id, stream id, initial term id, active term id, term
length, MTU and TTL, each an `int32`, with the flags `0x80` (send response setup) and
`0x40` (group). A status message is 36 bytes, or 44 with an optional group tag: session
id, stream id, consumption term id, consumption term offset, receiver window and a
64-bit receiver id, with the flags `0x80` (send setup) and `0x40` (end of stream). A NAK
is 28 bytes: session id, stream id, term id, term offset, length. An RTT measurement is
40 bytes: session id, stream id, a 64-bit echo timestamp, a 64-bit reception delta and a
64-bit receiver id, with the flag `0x80` (reply). A heartbeat is a 32-byte data frame
header whose frame length is 0, carrying the sender's term id and term offset, with
BEGIN and END set and EOS once the publication is closed. The sender sends a setup
frame every 100 ms until the first status message, and one more when a status message
with the send-setup flag asks for it; data frames in a datagram of at most the MTU and
the window; a heartbeat after 100 ms without data, once a status message has arrived;
and retransmits from its own term on a NAK, one outstanding retransmit at a time over
unicast, lingering 10 ms, ignoring a NAK whose offset lies inside it and counting one
for another range as a retransmit overflow; its sender limit is the maximum of itself
and the status message's position plus receiver window, taken only from a status
message whose position lies within half a term below the sender position and a term and
a half above it; its publisher limit is the sender position plus the publication window
while a status message has arrived within 5 s, and the sender position alone otherwise;
it lingers 5 s after close, or until a status message with the end-of-stream flag, so
that NAKs are still answered. The receiver creates an image from a setup frame at the
position the setup names; asks for one, when a data frame or heartbeat without EOS
arrives for a stream it wants and a session it has no image for, with a status message
carrying the send-setup flag, term id 0, term offset 0 and window 0, at most once a
second while no setup arrives (`DataPacketDispatcher`); sends a status message at once
and then on three triggers,
the slowest subscriber's position a quarter of the window past the last one, a change
of window, or 200 ms without one, carrying the slowest subscriber's position and the
window; inserts a data frame at its term offset only if its position is at or past the
last status message's position and its end at most half a term beyond it, writing the
frame's first eight bytes last; keeps the highest position received and a rebuild
position that stops at the first gap; sends a NAK for the gap after a delay of 1 µs,
again every 100 µs while it persists; drops a duplicate silently, a frame before the
last status message's position as an under-run and one beyond the threshold as an
over-run, each counted; retires an image with no accepted frame in 10 s; and at end of
stream, once the rebuild position has reached the highest, stores the end-of-stream
position and sends the status message with the end-of-stream flag. Its defaults: MTU
1408, UDP term length 16 MiB, receiver window 128 KiB, socket receive buffer 128 KiB,
socket send buffer the operating system's, a random session id and initial term id
per publication. Its channel URI is `aeron:udp?endpoint=host:port` with parameters
separated by `|`, an unresolvable host `UNKNOWN_HOST` (9), a socket that cannot be
opened `CHANNEL_ENDPOINT_ERROR` (4), a multicast endpoint detected from the address.
Its log buffer metadata carries, for the UDP client, the receiver window length, the
socket buffer lengths asked for and the operating system's defaults and maxima, and a
type byte whose value 2 is a publication image. Its `publication ready` carries a
channel status indicator id after the publication limit counter id, and its
`subscription ready` one after the correlation id; an image's source identity is
`host:port`.

## Decision

Three pages change in one accepted change, vectored together, and `cavalry-proto`
and `cavalry-shm` read and write them.

### `spec/udp.md`, version 0: the UDP transport

**The datagram.** A UDP datagram carries either one control frame or one or more data
or padding frames taken whole and in order from one term, the first at a term offset
that is a multiple of 32, so that every frame in it begins at a multiple of 32. No frame
spans two datagrams, and a datagram is at most the MTU, 1408 bytes by default, a
multiple of 32 from 64 to 65504 (`spec/log-buffer.md`'s bound). Integers are
little-endian. Every frame begins with the data frame header's first eight bytes
(`spec/frame.md`): frame length, version, flags, type. The types of this page are 2
(NAK), 3 (status message), 5 (setup) and 6 (RTT measurement); 0 and 1 are
`spec/frame.md`'s; 4, 7, 8, 9, 10 and 11 are Aeron's and reserved; 12 and above are
reserved. A receiver of version 0 drops a frame whose version is not 0 or whose type it
does not handle and counts it, so that a peer of a later version is ignored and never
misread (invariant 3): nothing is delivered from it, and `doctor` reads the count.

**The frames**, Aeron's field for field, offsets from the start of the frame:

| Frame | Type | Length | Fields after the common eight | Flags |
| --- | --- | --- | --- | --- |
| setup | 5 | 40 | term offset i32 at 8, session id i32 at 12, stream id i32 at 16, initial term id i32 at 20, active term id i32 at 24, term length i32 at 28, MTU length i32 at 32, TTL i32 at 36 | bit 7 `0x80` send response setup and bit 6 `0x40` group, both reserved: written 0, ignored by a reader of this version |
| status message | 3 | 36, or 44 with the group tag | session id i32 at 8, stream id i32 at 12, consumption term id i32 at 16, consumption term offset i32 at 20, receiver window length i32 at 24, receiver id i64 at 28, group tag i64 at 36 when the frame length is 44 | bit 7 `0x80` send setup, bit 6 `0x40` end of stream |
| NAK | 2 | 28 | session id i32 at 8, stream id i32 at 12, term id i32 at 16, term offset i32 at 20, length i32 at 24 | none; written 0 |
| RTT measurement | 6 | 40 | session id i32 at 8, stream id i32 at 12, echo timestamp i64 at 16, reception delta i64 at 24, receiver id i64 at 32 | bit 7 `0x80` reply |
| heartbeat | 1 | 32 | a data frame header (`spec/frame.md`) whose frame length is 0: term offset and term id are the sender position's, session id and stream id the publication's, reserved value 0 | BEGIN and END; EOS once the publication is closed |

A reader runs these checks in this order and reports the first that fails by name:
`too-short` (fewer bytes than the frame's length), `version` (not 0), `type` (not one
this page or `spec/frame.md` specifies), `length` (the frame length is not the frame's
length, or, for a status message, neither 36 nor 44), then the frame's bounds: a term
length that is not a power of two from 65536 to 1073741824 (`term-length`), an MTU that
is not a multiple of 32 from 64 to 65504 (`mtu`), a term offset that is negative, not a
multiple of 32 or not below the term length (`term-offset`), a receiver window below 0
(`window`), a NAK length that is not a positive multiple of 32 (`nak-length`). A
datagram of data frames is checked as a whole: its length is a multiple of 32, each
frame passes `spec/frame.md`'s checks against the bytes left, each frame's term offset
is the running offset from the first, and the frames fit in the term (`datagram`). A
heartbeat is exactly 32 bytes with frame length 0.

**The channel URI.** `cavalry:udp?endpoint=<host>:<port>` names the transport, with
one option, `|interface=<address>[:<port>]`, the local address a sender sends from or
a receiver binds beside the endpoint, for a host with two paths. `<host>` is a name, an
IPv4 address or an IPv6 address in brackets, and `<port>` 1 to 65535; a `<port>` of 0
is refused. Parameters are separated by `|`, a key is `[a-z0-9-]+` and a value
non-empty, and a parameter this version does not name is refused with error code 1
and a message naming it, so that a parameter a later version adds is never silently
ignored by an older driver. A multicast address is refused with code 1 in this version.
The driver resolves `<host>` once, when the command is handled, on its control plane;
a host that does not resolve is code 9 (unknown host). Two channels are the same channel
when their resolved endpoint and interface are the same, which is what a joined
publication and a shared endpoint are keyed by. `cavalry:ipc` stays the IPC channel.

**The sender**, one dedicated thread per driver. A `cavalry:udp` publication has a log
buffer of type 0 or 1 as any publication, with the UDP term length, 16 MiB by default,
and a send endpoint per resolved channel, one socket, shared by the publications on
that channel, connected to the endpoint, bound to the interface when one is named.
Per publication, on every duty cycle, in this order:

1. **Setup.** Until the first status message without the send-setup flag, a setup
   frame every 100 ms carrying the active term id and term offset of the sender
   position, the session and stream ids, the initial term id, the term length, the MTU
   and TTL 0. A status message with the send-setup flag is a receiver asking for a
   setup ("The receiver", step 2) and does nothing else: it does not set the sender
   limit and does not connect the publication; it is answered with one setup frame,
   sent once 100 ms have passed since the last one.
2. **Data.** With the sender position p and the sender limit L, a burst of whole frames
   from p, committed ones only, stopping at the first frame not committed, in datagrams
   of at most the MTU and at most L − p bytes in all, at most 16 datagrams per cycle,
   one `sendmmsg` on Linux and one `sendmsg` per datagram elsewhere; the sender
   position advances by what was sent, padding frames counted as sent whether or not
   they were. A burst that could not be sent whole leaves the position at the first
   unsent frame and counts `sender.short_sends`. A cycle that finds L − p ≤ 0 while
   there is a frame to send counts one `sender.flow_control_limited` per entry into that
   state.
3. **Heartbeat.** Once the first status message without the send-setup flag has
   arrived, after 100 ms without a data frame or heartbeat sent, a heartbeat carrying
   the sender position's term id and term offset; with the EOS flag once the
   publication is closed and the sender position has reached the end of stream
   position. Before that status message only setup frames go, so that a receiver's
   request for a setup is answered by a setup and not by more heartbeats.
4. **Retransmit.** A NAK for this publication whose term id and offset name a position
   in the resend window, from sender position − term ÷ 2 − longest message up to but
   not including the sender position: the range it names, split at the MTU, in datagrams
   from the term, at once. The retransmit stays outstanding for 10 ms after it, and over
   unicast one is outstanding at a time: a NAK whose offset lies inside the outstanding
   range is dropped silently, since the answer is on its way, and one for another range
   while a retransmit is outstanding is dropped and counted as
   `sender.retransmit_overflow`, the 10 ms stall in recovery that `doctor` explains from
   it. A NAK outside the resend window is dropped and counted as `sender.naks_ignored`.
   The retransmit never moves the sender position.
5. **RTT.** An RTT measurement with the reply flag is answered without the flag,
   echoing its timestamp and receiver id with a reception delta of 0. Nothing initiates
   one in this version.

The sender limit starts at 0 and becomes max(L, position + receiver window) on every
status message without the send-setup flag whose position lies within [p − term ÷ 2,
p + term + term ÷ 2]; one outside is counted and ignored. The publication is connected
from the first such status message until 5 s pass without one; the conductor keeps `publication.limit` at the
sender position plus the publication window (term ÷ 2) while connected, and at the
sender position otherwise, so that the publisher reads `not connected`, and keeps
`is connected` in the log accordingly; the conductor cleans the log as for an IPC
publication, with the sender position as the consumer position (`spec/log-buffer.md`,
"The driver"). After close, the publication lingers 5 s, or until a status message with
the end-of-stream flag arrives, sending heartbeats with EOS and answering NAKs, then its
log is removed. The socket send buffer is the operating system's.

**The receiver**, one dedicated thread per driver. A `cavalry:udp` subscription has a
receive endpoint per resolved channel, one socket bound to the endpoint, shared by the
subscriptions on that channel, drained in batches of at most 16 datagrams per poll,
`recvmmsg` on Linux and `recvmsg` elsewhere, with a receive buffer of 128 KiB asked of
the socket. The driver's receiver id is a random i64 chosen at start, and it sends no
group tag (36-byte status messages). On a datagram from a source address:

1. **Setup.** A setup frame for a stream id a subscription on the endpoint wants and a
   session id with no image: the conductor creates an image with a registration id of
   its own, taken from the command ring's correlation id counter as the driver takes its
   client id (`spec/client-protocol.md`), since the image has no local publication whose
   id could name it; its log is a log buffer of type 2 (`spec/log-buffer.md`, version
   1), `images/<registration id>.logbuffer`, with the setup's term length, MTU and
   initial term id, at the position of its active term id and term offset, with a
   receiver window of min(128 KiB, term ÷ 2); the conductor announces it to every
   subscription on the endpoint and stream with `available image` carrying that id as
   correlation id and the source identity `<host>:<port>` of the datagram's source, and
   sends the first status message at once. A setup for a session with an
   image is ignored; one for a stream nobody wants is counted and dropped. Every
   control frame to that image's sender goes to the source address of its setup frame.
2. **Asking for a setup.** A data datagram or heartbeat without the EOS flag for a
   stream id a subscription on the endpoint wants and a session id with no image and no
   pending setup: the receiver records the session as pending a setup for 1 s and sends
   the datagram's source a status message with the send-setup flag, consumption term
   id 0, consumption term offset 0 and receiver window 0, and the receiver id as in
   every status message, counting `receiver.setups_elicited`; the datagram itself is
   dropped and counted in `receiver.dropped_frames`, as is every one for the session
   while it is pending. A setup frame for the pending session creates the image as
   step 1 says and ends the wait; after 1 s without one the record expires and the next
   datagram asks again, so a sender that stopped sending setups at its first status
   message, before this receiver started, restarted or added its subscription back, is
   asked at most once a second until it answers. A datagram with EOS for a session with
   no image is dropped and counted the same way and asks for nothing: the stream is
   over. A data datagram or heartbeat for a stream nobody on the endpoint wants is
   dropped and counted the same way.
3. **Data.** A data datagram for an image whose first position is below the last status
   message's position is dropped as `receiver.under_runs`; one whose end is beyond that
   position plus half a term is dropped as `receiver.over_runs`; otherwise each frame is
   written at its term offset, the bytes after the first eight first and the first eight
   last with a release store, and only where the frame length there is 0, so that a
   duplicate changes nothing; the high-water mark becomes the greater of itself and the
   datagram's end position. A heartbeat moves the high-water mark the same way, and one
   with EOS records the end-of-stream position as its position.
4. **Rebuild and NAK.** The rebuild position is the end of the contiguous frames from
   the greater of itself and the fastest subscriber's position, up to the high-water
   mark. A gap between it and the next frame, or the high-water mark, is a loss: after
   1 µs a NAK for the gap's term id, term offset and length, and again every 100 µs
   while the gap is the same; a gap that changes restarts the delay. A subscriber reads
   the image's log as any log (`spec/log-buffer.md`, "Reading") and stops at the gap
   itself; nothing is delivered from beyond a gap before it is filled (invariant 1).
5. **Status messages.** One at once at creation, then whenever the slowest subscriber's
   position is at least a quarter of the window past the last status message's
   position, when the window changes, or 200 ms after the last one; it carries the
   slowest subscriber's position as consumption term id and offset and the window, and
   the end-of-stream flag once the stream has ended. Before the position in it rises,
   the receiver cleans the image's log from the clean position to the slowest
   subscriber's position less the term length, as the driver cleans a publication's log,
   so that a term is zero before a frame lands in it.
6. **End.** An image with no accepted frame for 10 s is retired: `unavailable image` to
   its subscriptions, its counters freed after the reuse timeout, its log removed once
   every subscription has unmapped it. At end of stream, once the rebuild position
   reaches the high-water mark, the end-of-stream position is stored in the log, the
   status message carries the end-of-stream flag, five times at the 200 ms cadence (this
   page's bound; Aeron's receiver sends them for as long as the image lingers), and the
   image is retired the same way once every subscriber has reached that position or the
   liveness timeout passes.
7. **RTT.** A measurement with the reply flag is answered as the sender answers one.

A subscription's `subscription.position` on an image is the counter the status message's
position is read from, as it is the counter the driver's cleaning reads (ADR 0013).
`start recording` on a `cavalry:udp` channel records every live log on this host whose
channel and stream id match, publications and images alike, one recording per log,
each `recording ready` naming its session id; the recorder reads an image's log as it
reads a publication's, stopping at a gap until it is filled.

**Counters**, static unless keyed: `sender.bytes_sent`, `sender.frames_sent`,
`sender.setups_sent`, `sender.heartbeats_sent`, `sender.retransmits_sent`,
`sender.retransmitted_bytes`, `sender.retransmit_overflow`, `sender.short_sends`,
`sender.flow_control_limited`,
`sender.naks_received`, `sender.naks_ignored`, `sender.status_messages_received`,
`sender.status_messages_rejected`, `sender.rtt_replies_sent`, `sender.send_failed`;
`receiver.bytes_received`, `receiver.frames_received`, `receiver.setups_received`,
`receiver.setups_unwanted`, `receiver.setups_elicited` (the status messages with the
send-setup flag sent for a session with no image), `receiver.heartbeats_received`,
`receiver.status_messages_sent`, `receiver.naks_sent`, `receiver.gaps_detected`,
`receiver.under_runs`, `receiver.over_runs`, `receiver.rtt_replies_sent`,
`receiver.dropped_frames` (the version and type drops, and the data datagrams and
heartbeats for a session with no image, by reason in `doctor`),
`receiver.rejected_datagrams` (the datagram checks); per publication, keyed as
`publication.position` is, `sender.position` and `sender.limit`; per image, keyed as
`subscription.position` is, with the image's registration id as the correlation id and
the image's session and stream ids, `image.hwm_position` and `image.rebuild_position`;
per endpoint, keyed by the endpoint's registration id, the
correlation id of the command that created it, and labelled with the resolved channel,
`channel.status` (0 initializing, 1 active, 2 closing, −1 errored), `channel.socket_rcvbuf` and `channel.socket_sndbuf` (what the socket holds,
read back after it was set, which is what `doctor` compares with `net.core.rmem_max`
and `wmem_max`), `channel.images` and `channel.publications`. The publication's
connection and the image's life are state machines in `counters.toml`
(`publication.connected`, `publication.disconnected`, `image.created`,
`image.ended`, `image.timed_out`, `image.removed`).

**Values**, Aeron's, the driver's in this version, so that a configuration key is
justified later by a `why_not_default` and not here: setup interval 100 ms, pending
setup 1 s, heartbeat interval 100 ms, status message interval 200 ms, status message threshold window ÷ 4,
NAK delay 1 µs, NAK retry 100 µs, retransmit linger 10 ms, connection timeout 5 s, image
liveness 10 s, linger 5 s, MTU 1408, UDP term length 16 MiB, publication window term ÷ 2,
receiver window min(128 KiB, term ÷ 2), socket receive buffer 128 KiB, socket send
buffer the operating system's, 16 datagrams per burst and per poll, 64 receive endpoints
and 64 send endpoints per driver (a 65th is code 10), 2048 images (ADR 0019's registry,
unchanged), and a counters file of 16384 counters, since the sum the client protocol
page sizes it by becomes 14,337: 65 statics, four per publication, three per image,
three per recording, one per replay, two per replay publication and five per endpoint.
Session ids and initial term ids are the driver's as for IPC.

**Vectors**, `spec/vectors/udp.json` from `spec/vectors/udp.py`: every frame once
valid and once per rejection in the order the checks run, a status message with and
without the group tag and the one that asks for a setup (send-setup flag, consumption
term id 0, consumption term offset 0, receiver window 0), a heartbeat with and without
EOS, a datagram of two data frames
and one of a padding frame, the datagram rejections, URI vectors (the shortest, an IPv6
host, an interface, each refusal with its code), and arithmetic vectors for the sender
limit's window, the retransmit window, the receiver's under-run and over-run bounds, the
status message threshold and the image's start position.

### `spec/log-buffer.md`, version 1: the image type and the receiver window

Version 1 keeps version 0's layout and adds: type 2, an image, whose terms the driver's
receiver writes by term offset as "The receiver" above says, the first eight bytes of a
frame last with a release store, and whose tail counters and active term count keep
their creation values and are read by nobody, since an image's readers read by
position; a receiver window length, i32 at header offset 40, written by the creator,
read by the receiver and `doctor`, 0 in a publication's log and 1 to term length ÷ 2 in
an image's; and the image's end-of-stream position written by the receiver when the
stream ends. The 20 bytes from 44 and the 32 from 96 stay reserved. Unblocking does not
apply to an image; cleaning is the receiver's; `is connected` is 1 while the image is
live. A reader of version 1 reads version 0 as version 1 with a receiver window of 0.
Image logs live in `images/<registration id>.logbuffer` under the driver directory,
named by the registration id the driver gives the image when the setup frame creates it
(`spec/client-protocol.md`, version 4), beside `publications/`, so that a directory
listing says what each log is. The vectors
gain a version 1 header for each type and the `window` rejection for an image.

### `spec/client-protocol.md`, version 4: the `cavalry:udp` channel

Every version 3 encoding is unchanged but two events. The channel string of add
publication, add exclusive publication, add subscription, start recording and start
replay is `cavalry:ipc` or a `cavalry:udp` URI as `spec/udp.md` says; `publication
ready` and `exclusive publication ready` gain the channel status indicator id, i32 at
offset 28 before the log file name, Aeron's offset, and `subscription ready` gains it at
offset 8 after the correlation id: the id of the endpoint's `channel.status` counter,
or −1 for `cavalry:ipc`, so that a client can tell an endpoint that failed from one
that is waiting. An image over UDP has no local publication whose registration id
could name it, as version 3's `available image` says a publication's does: the driver
gives each such image a registration id of its own from the command ring's correlation
id counter when the setup frame creates it, as Aeron's conductor does; `available
image` and `unavailable image` carry it as correlation id, the log file name is
`images/<that id>.logbuffer`, and the image's `image.hwm_position` and
`image.rebuild_position` counters and its subscriptions' `subscription.position`
counters are keyed by it with the image's session and stream ids. `available image`'s
source identity is `<host>:<port>` for an image over UDP; `unavailable image`'s channel
is the subscription's. Error codes 4 (channel
endpoint error: the socket could not be opened, bound or connected, the message carrying
the operating system's reason and the endpoint) and 9 (unknown host) are taken from
Aeron's table; a `cavalry:udp` channel whose grammar fails, a multicast address and an
unknown parameter stay code 1. `add subscription` on an endpoint binds it, and the
first `add publication` on an endpoint connects it; the registries gain 64 send and 64
receive endpoints, and the counters file is created with 16384 counters, 6 MiB, so that
the registries still fill first. The driver directory gains `images/`. A version 3 client closes on
the first `publication ready` of a version 4 driver, since it is longer than its fields,
so an upgrade restarts the clients before the driver, as every version has said. The
vectors gain the two events, an `available image` for an image over UDP with its own
registration id and an `images/` log file name, a `cavalry:udp` channel in each of the
five commands, and error events with codes 4 and 9.

### The crates

`cavalry-proto` gains `udp`: the four control frames and the heartbeat as `zerocopy`
views (ADR 0009) with `parse` in the spec's order, `write`, and the datagram walker;
and `channel`: the URI parser, borrowing from the string, with each refusal named.
Each parser has a no-panic test over generated bytes in `just test` and a `cargo fuzz`
target under `just fuzz`. `cavalry-shm` reads and writes log buffer version 1, refuses
version 2 as a reader must, and gains the image's writer, the one `unsafe` block of the
feature with its `loom` model: a release store of the first eight bytes after plain
stores of the rest, which is the claim header's protocol in reverse. Nothing in this
change opens a socket: the driver refuses a `cavalry:udp` channel with code 1 and a
message naming card 180 until the sender lands.

## Alternatives considered

- **A page per frame type, or the frames in `spec/frame.md` at version 1.** Lost:
  Aeron's transport is one page, and the data frame header does not change; a reader of
  version 0 of `spec/frame.md` reads every datagram this page sends.
- **Leaving out the TTL, the group tag, the receiver id and the setup flags, which
  unicast does not use.** Lost: Aeron's layout field for field is what lets multicast
  and multi-destination cast (ADR 0039's Revisit) add no wire change; the fields cost 4
  to 8 bytes on frames sent a few times a second.
- **Rejecting a frame of an unknown type or version, as `spec/frame.md`'s reader
  does.** Lost for the wire: a log's reader rejects because a bad frame in a log is
  corruption, while a datagram from a later version is a peer to ignore, and a driver
  that closed a stream on it would fail the mixed-version rule (invariant 3). Dropped and
  counted is what `doctor` can explain.
- **Ignoring unknown channel parameters, as Aeron does.** Lost: a parameter a later
  version adds, `mtu=` say, silently ignored by an older driver is a stream with the
  wrong MTU and no counter; code 1 with the parameter's name costs nothing.
- **Status messages per poll batch, as IPC's limit moves.** Lost: a datagram per batch
  on the receiver's thread; Aeron's three triggers bound them to five a second plus one
  per quarter window, which is what the sender needs.
- **A NAK at once with no retry, or Aeron's multicast backoff.** Lost: a NAK lost on
  the wire needs the retry, and the backoff is for a group of receivers that would NAK
  the same gap; unicast has one.
- **The rebuild and high-water positions in the image's metadata page, as its tails.**
  Lost: every position is a counter in the counters file (ADR 0013), where `doctor`
  reads it without mapping a log.
- **The socket buffer lengths in the log buffer's header, as ADR 0013's Revisit and
  Aeron's metadata have them.** Lost: they belong to the endpoint, not the stream, a
  counter is the primary instrument (invariant 4), and `doctor` compares them with the
  host's limits from the counters file alone. The receiver window goes in the header,
  since it is the image's as the publication window is the publication's.
- **Gap detection and NAKs in the client, from the subscriber's poll.** Lost: a client
  never holds a socket (`CLAUDE.md`: one daemon per host owns the sockets), and the
  subscriber's poll must stay free of syscalls (invariant 2).
- **Frames split across datagrams to fill the MTU.** Lost: Aeron sends whole frames,
  the receiver inserts by term offset, and a frame that fits a datagram today is what
  the MTU bound in `spec/log-buffer.md` was for.
- **A send burst of one datagram per cycle, as Aeron's sender.** Lost: Aeron's cycle is
  a tight loop; with `sendmmsg` a burst of up to 16 datagrams is one syscall, and the
  bound keeps one publication from starving another on the thread.
- **Recording over UDP by source location, as Aeron Archive's `sourceLocation`.** Lost
  for this version: a channel and a stream id name the logs on this host, and a host
  that both sends to an endpoint and receives on it is the case the Revisit names.
- **An image from the first data frame or heartbeat, with no setup asked for.** Lost:
  a data frame carries no term length, MTU or initial term id, so a receiver that
  starts, restarts or adds its subscription back after the sender's first status
  message could not size the image's log; Aeron's receiver asks for the setup with a
  status message carrying the send-setup flag and the sender answers with one setup, so
  a late receiver costs one round trip and no new frame type.
- **An error frame (type 4) to tell a sender its image was rejected.** Lost: nothing
  rejects an image in this version; the type stays reserved.
- **Resolving names on the receiver thread, or re-resolving them.** Lost: resolution is
  a syscall on the control plane, once, when the command is handled; a name that moves
  is a Revisit.
- **Configuration keys for the MTU, the window, the buffers and the intervals.** Lost:
  zero configuration, Aeron's values and a `doctor` check; a key returns with its
  `why_not_default` when a host shows a need (ADR 0039).

## Evidence

None measured: a format decision. What informed it: Aeron's sources named in Context,
read on 2026-10-31 on its `master` branch, with the two inconsistencies found noted for
the record (the wiki's status message length against the source's 36 and 44; the
Java and C headers' numbering of the two encrypted types, which this page reserves
both of); ADR 0039's Decision and the cards' briefs; the three pages' room; the
direct link between `bench` and `grip` measured on 2026-10-31 (card 182: 0.12 to 0.14 ms
round trip, no loss in 200 pings, MTU 1500, hardware timestamps on both ports), which is
what the MTU default and the cross-host rows will run over. The vectors' counts and the
no-panic and `loom` runs are the accepting change's to record.

## Consequences

Easier: Aeron's wire documentation and tools explain Cavalry's datagrams; a subscriber
reads an image as it reads a log, so the client's poll path does not change; every drop
and every state has a counter; the sender and the receiver (cards 180 and 181) implement
a page each.

Harder: three pages move versions at once and a version 3 client restarts before a
version 4 driver; the driver gains two registries and five endpoint counters; the
`loom` model for the image's writer is new `unsafe` in `cavalry-shm`; `doctor` gains the
checks the counters above imply, which cards 180 and 181 build.

Revisit: with multicast and multi-destination cast, for the TTL, the group tag, the
`control` parameter and the min and tagged strategies (ADR 0039); when a user needs
`mtu`, `term-length`, `so-rcvbuf` or `rcv-wnd` on a channel, each an Aeron parameter
name this page refused; when a recording over UDP needs a source location; when a name
must be re-resolved while a stream lives; when congestion control initiates RTT
measurements; when encryption takes Aeron's types 8 to 10 or Cavalry's own.

## References

- `spec/frame.md`, `spec/log-buffer.md`, `spec/client-protocol.md`, `spec/README.md`
- ADR 0008, ADR 0009, ADR 0010, ADR 0013, ADR 0019, ADR 0021, ADR 0025, ADR 0039
- `CLAUDE.md`: invariants 1 to 6; Protocol; Data plane; When in doubt;
  `.claude/rules/data-plane.md`
- Aeron, read 2026-10-31 at `master`:
  `aeron-client/src/main/c/protocol/aeron_udp_protocol.h`;
  `aeron-client/src/main/java/io/aeron/protocol/{HeaderFlyweight,DataHeaderFlyweight,SetupFlyweight,StatusMessageFlyweight,NakFlyweight,RttMeasurementFlyweight,ErrorFlyweight}.java`;
  `aeron-driver/src/main/java/io/aeron/driver/{NetworkPublication,Sender,UnicastFlowControl,RetransmitHandler,PublicationImage,Receiver,DataPacketDispatcher,LossDetector,StaticDelayGenerator,DriverConductor,PublicationParams,Configuration}.java`;
  `aeron-driver/src/main/java/io/aeron/driver/media/{SendChannelEndpoint,ReceiveChannelEndpoint,UdpChannel,UdpChannelTransport}.java`;
  `aeron-client/src/main/java/io/aeron/logbuffer/{LogBufferDescriptor,TermRebuilder,TermGapScanner}.java`;
  `aeron-client/src/main/java/io/aeron/{ChannelUri,CommonContext,ErrorCode}.java`;
  `aeron-client/src/main/java/io/aeron/command/{PublicationBuffersReadyFlyweight,SubscriptionReadyFlyweight,ImageBuffersReadyFlyweight,ImageMessageFlyweight}.java`
- AppShapes Trello cards 179, 180, 181, 182 and 183
