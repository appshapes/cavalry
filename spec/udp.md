# UDP transport, version 0

The UDP transport carries one publication's stream from the driver on one host to the
driver on another: the sender puts the frames of the publication's log buffer into
datagrams, the receiver rebuilds them into an image, a log buffer its subscribers read
as they read any log, and four control frames keep the two in step: the setup frame, the
status message, the NAK and the RTT measurement. In Aeron, this is the transport
protocol specification's UDP media, its frames field for field (ADR 0040;
`docs/glossary.md`). The data frame header is `spec/frame.md`; the log buffer and the
image are `spec/log-buffer.md`; the channel a client names is `spec/client-protocol.md`.
This page is the bytes on the wire, the rules both sides follow, and the counters that
explain them.

## The datagram

A UDP datagram carries either one control frame or one or more data or padding frames
(`spec/frame.md`) taken whole and in order from one term of the sender's log, the first
at a term offset that is a multiple of 32, so that every frame in it begins at a multiple
of 32 from the datagram's start. No frame spans two datagrams. A datagram is at most the
publication's MTU length, 1408 bytes in this version (`spec/log-buffer.md`, "Header").
Integers are little-endian (`spec/README.md`). Every frame begins with the data frame
header's first eight bytes: frame length (i32), version (i8), flags (u8) and type (u16).

| Type | Frame | Page |
| --- | --- | --- |
| 0 | padding | `spec/frame.md` |
| 1 | data, and the heartbeat | `spec/frame.md`; "Heartbeat" below |
| 2 | NAK | below |
| 3 | status message | below |
| 5 | setup | below |
| 6 | RTT measurement | below |
| 4, 7, 8, 9, 10, 11 | Aeron's error, resolution, encrypted and response frames | reserved |
| 12 and above | | reserved |

A receiver of version 0 drops a frame whose version is not 0 or whose type it does not
handle, counting it in `receiver.dropped_frames`, and reads nothing from it: a peer of a
later version is ignored, never misread, and `doctor` names the count.

## Frames

Offsets are in bytes from the start of the frame. The first eight bytes of every frame
are the data frame header's: frame length, version 0, flags, type.

### Setup frame

The sender's announcement of a stream: 40 bytes, type 5.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | frame length | i32 | 40 |
| 4 | 1 | version | i8 | 0 |
| 5 | 1 | flags | u8 | bit 7 (`0x80`, send response setup) and bit 6 (`0x40`, group) are Aeron's and reserved: written 0, ignored by a reader of this version; the other bits reserved the same way |
| 6 | 2 | type | u16 | 5 |
| 8 | 4 | term offset | i32 | the term offset of the sender position; a multiple of 32 below the term length |
| 12 | 4 | session id | i32 | the publication's |
| 16 | 4 | stream id | i32 | the publication's |
| 20 | 4 | initial term id | i32 | the log's |
| 24 | 4 | active term id | i32 | the term id of the sender position |
| 28 | 4 | term length | i32 | the log's: a power of two from 65536 to 1073741824 |
| 32 | 4 | MTU length | i32 | the publication's: a multiple of 32 from 64 to 65504 |
| 36 | 4 | TTL | i32 | 0 in this version; Aeron's multicast time to live |

### Status message

The receiver's report of where its subscribers are and how far the sender may run ahead:
36 bytes, type 3, or 44 bytes with the group tag.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | frame length | i32 | 36, or 44 when the group tag is present |
| 4 | 1 | version | i8 | 0 |
| 5 | 1 | flags | u8 | bit 7 (`0x80`) send setup: the receiver asks for a setup frame; bit 6 (`0x40`) end of stream: the receiver has consumed the stream to its end; the other bits reserved, written 0, ignored |
| 6 | 2 | type | u16 | 3 |
| 8 | 4 | session id | i32 | the image's |
| 12 | 4 | stream id | i32 | the image's |
| 16 | 4 | consumption term id | i32 | the term id of the consumption position |
| 20 | 4 | consumption term offset | i32 | its term offset: a multiple of 32 below the term length |
| 24 | 4 | receiver window length | i32 | how far past the consumption position the sender may send: 0 or more |
| 28 | 8 | receiver id | i64 | the receiving driver's id, random at its start |
| 36 | 8 | group tag | i64 | present when the frame length is 44; Aeron's flow control group; a receiver of this version sends none and a sender ignores one |

### NAK

The receiver's request for a retransmission: 28 bytes, type 2.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | frame length | i32 | 28 |
| 4 | 1 | version | i8 | 0 |
| 5 | 1 | flags | u8 | reserved: written 0, ignored |
| 6 | 2 | type | u16 | 2 |
| 8 | 4 | session id | i32 | the image's |
| 12 | 4 | stream id | i32 | the image's |
| 16 | 4 | term id | i32 | the term of the gap |
| 20 | 4 | term offset | i32 | where the gap begins: a multiple of 32 below the term length |
| 24 | 4 | length | i32 | the gap's length: a positive multiple of 32, at most the term length − term offset |

### RTT measurement

A round-trip probe either side may answer: 40 bytes, type 6.

| Offset | Size | Field | Type | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 4 | frame length | i32 | 40 |
| 4 | 1 | version | i8 | 0 |
| 5 | 1 | flags | u8 | bit 7 (`0x80`) reply: set on the probe, which asks for an answer; clear on the answer; the other bits reserved, written 0, ignored |
| 6 | 2 | type | u16 | 6 |
| 8 | 4 | session id | i32 | the stream's |
| 12 | 4 | stream id | i32 | the stream's |
| 16 | 8 | echo timestamp | i64 | the prober's clock, in nanoseconds, echoed unchanged by the answer |
| 24 | 8 | reception delta | i64 | the nanoseconds the answering side held the probe; 0 in this version |
| 32 | 8 | receiver id | i64 | the receiver the probe is for, echoed by the answer; 0 for any |

### Heartbeat

A data frame header (`spec/frame.md`) of 32 bytes whose frame length is 0: the sender's
sign of life when it has no data, carrying the term id and term offset of the sender
position, the publication's session id and stream id, a reserved value of 0, the flags
BEGIN and END (`0xC0`), and EOS (`0x20`) as well once the publication is closed and the
sender position has reached the end of stream position. It is sent alone in its
datagram. A receiver moves its high-water mark to the heartbeat's position and delivers
nothing from it.

## The channel URI

`cavalry:udp?endpoint=<host>:<port>` names the transport, and
`cavalry:udp?endpoint=<host>:<port>|interface=<address>[:<port>]` names it with the
one option of this version: the local address a sender sends from, or a receiver binds
beside its endpoint, on a host with two paths. Parameters follow `?` separated by `|`,
each `key=value` with a key of `[a-z0-9-]+` and a non-empty value; the same key twice is
refused. `<host>` is a name, an IPv4 address, or an IPv6 address in brackets, and
`<port>` is 1 to 65535. The driver resolves `<host>` once, on its control plane, when
the command that carries the channel is handled. Two channels are the same channel when
their resolved endpoint and interface are the same: a second `add publication` on the
same channel and stream joins the first (`spec/client-protocol.md`, "Commands"), and
subscriptions on the same channel share one receive endpoint. A channel is refused as
"What a reader rejects" says: the grammar, an unknown key, a port of 0 or above 65535,
and a multicast address are error code 1 with a message naming what; a host that does
not resolve is code 9.

## The sender

The driver's sender is one dedicated thread per driver with the driver's idle strategy.
A `cavalry:udp` publication has a log buffer of type 0 or 1 (`spec/log-buffer.md`) with
the UDP term length and MTU of "Values", and a send endpoint per resolved channel: one
socket, shared by the publications on that channel, connected to the endpoint and bound
to the interface when one is named, with the operating system's send buffer. Per
publication, on every duty cycle, with p the sender position and L the sender limit:

1. **Setup.** Until the first status message arrives, and again after a status message
   with the send-setup flag, a setup frame every setup interval, carrying the sender
   position's active term id and term offset.
2. **Data.** A burst of whole committed frames from p, stopping at the first frame
   whose length is 0 or less, at most L − p bytes in all, in datagrams of at most the
   MTU, at most one burst's worth of datagrams ("Values"); the sender position
   advances by what was sent, a padding frame counted as sent whether or not it was
   put on the wire. A datagram the socket did not take whole leaves p at its first
   frame and counts `sender.short_sends`; a send the socket refused counts
   `sender.send_failed`. A cycle with a frame to send and L − p ≤ 0 counts
   `sender.flow_control_limited` once, on entry into that state.
3. **Heartbeat.** Once the first status message has arrived, after a heartbeat interval
   without a data frame sent, a heartbeat; before it, only setup frames go, so that a
   receiver's ask for a setup is answered by a setup frame and not by more heartbeats.
4. **Retransmit.** On a NAK for this publication whose term id and offset name a
   position in the resend window, from p − term length ÷ 2 − the longest message up to
   but not including p, the frames of the range it names, from the term, in datagrams
   of at most the MTU, at once. The retransmit stays outstanding for the retransmit
   linger after it, and one is outstanding at a time: a NAK whose offset lies inside the
   outstanding range is dropped silently, since the answer is on its way; one for
   another range while a retransmit is outstanding is dropped and counted in
   `sender.retransmit_overflow`, the recovery stall `doctor` explains from it; and a
   NAK outside the resend window is dropped and counted in `sender.naks_ignored`. A
   retransmit never moves the sender position.
5. **RTT.** An RTT measurement with the reply flag is answered: the same frame without
   the flag, the echo timestamp and receiver id unchanged, a reception delta of 0.

The sender limit is 0 when the publication is created and becomes
max(L, position + receiver window length) on every status message whose position,
consumption term id and offset as `spec/log-buffer.md` computes a position, lies within
[p − term length ÷ 2, p + term length + term length ÷ 2]; a status message outside that
window is counted in `sender.status_messages_rejected` and ignored. The publication is
connected from its first status message until the connection timeout passes without
one. The conductor keeps `publication.limit` at the sender position plus the publication
window while the publication is connected and at the sender position otherwise, so that
the publisher reads `not connected` (`spec/log-buffer.md`, "Claim, commit and abort"),
keeps `is connected` to match, and cleans and unblocks the log as "The driver" there
says with the sender position as the consumer position. After the publication is closed
it lingers for the linger timeout, or until a status message with the end-of-stream flag
arrives, sending heartbeats with EOS and answering NAKs; then its log is removed. Once
the publication is closed and the sender position has reached the end of stream
position, the condition under which its heartbeats carry EOS, a status message with the
send-setup flag is ignored and counted in `sender.status_messages_rejected`: a setup
then would create an image that ends at once, and the heartbeats with EOS already tell
a receiver with no image that the stream is over (Aeron's `triggerSendSetupFrame` is
guarded the same way). Before the sender position reaches
it, a closed publication answers one as step 1 says, since frames are still to be sent.

## The receiver

The driver's receiver is one dedicated thread per driver with the driver's idle
strategy. A `cavalry:udp` subscription has a receive endpoint per resolved channel: one
socket bound to the endpoint, and to the interface when one is named, shared by the
subscriptions on that channel, with the receive buffer of "Values" asked of the socket
and the length the socket holds stored in `channel.socket_rcvbuf`. The receiver drains
each socket in batches of at most one poll's worth of datagrams ("Values"). The driver's
receiver id is a random i64 chosen when it starts. On a datagram from a source address:

1. **Setup.** A setup frame for a stream id a subscription on the endpoint wants, with a
   session id that has no record, or one pending a setup (below): the receiver marks the
   session **in progress** and hands the setup to the conductor through the bounded
   queue between them; a setup the queue cannot take is dropped and counted in
   `receiver.dropped_in_progress`, the session stays as it was, and the sender's next
   setup tries again. The conductor gives the image a registration id of its own from the
   correlation id counter, as it gives a publication its (`spec/client-protocol.md`,
   "Commands"), creates a log buffer of type 2 (`spec/log-buffer.md`, version 1) at
   `images/<registration id>.logbuffer` with the setup's term length, MTU length and
   initial term id, at the position of its active term id and term offset, with the
   receiver window length of "Values", announces it to every subscription on the
   endpoint and stream with `available image` carrying that registration id as its
   correlation id and the source identity `<host>:<port>` of the datagram's source, and
   hands the image back to the receiver, which puts it in the session's record and sends
   the first status message at once. While the session is in progress, a setup, data
   datagram or heartbeat for it is dropped and counted in `receiver.dropped_in_progress`,
   no status message is sent and no second image is created: the sender sends a setup every
   setup interval until its first status message, and the conductor's creation can take
   longer than that under CPU throttling, so a second setup inside that time is the
   normal case and must not deliver every message twice. A conductor that cannot create
   the image, the images registry full (`driver.registry_full`) or the log not created
   (`driver.log_create_failed`), hands the refusal back instead, and the receiver removes
   the record, so the sender's next setup tries again. A setup for a session with an
   image is ignored, as a duplicate data frame is; one for a session on cool-down (step
   5) is dropped and counted in `receiver.dropped_cool_down`; one for a stream no
   subscription on the endpoint wants counts `receiver.setups_unwanted`. Every control
   frame the receiver sends for the image goes to the source address of the setup frame
   that created it. A data datagram or a heartbeat without EOS for a stream a
   subscription on the endpoint wants and a session with no record asks the sender for a
   setup: the receiver records the session as **pending a setup** for the pending setup
   timeout and sends a status message with the send-setup flag, consumption term id 0,
   consumption term offset 0 and receiver window length 0, to the datagram's source,
   counted in `receiver.setups_elicited`; every further data datagram or heartbeat for
   the session is dropped and counted in `receiver.dropped_pending`,
   `receiver.dropped_in_progress` or `receiver.dropped_cool_down` by the session's
   state, with no status message sent for any of them; when the pending record expires,
   the next datagram asks again. A data datagram or heartbeat for a stream no
   subscription on the endpoint wants is dropped and counted in
   `receiver.dropped_unwanted` and asks for nothing, as is a datagram with EOS for a
   session with no record: the stream is over. An RTT measurement for a session with no
   image is dropped and counted in `receiver.dropped_unwanted` too, so that a peer's
   measurement elicits nothing. That is how a receiving driver that restarts, or an endpoint whose
   subscription is removed and added again, gets its image back from a sender that has
   long stopped sending setups.
2. **Data.** A datagram of data frames for an image, with s the position of the last
   status message sent: one whose first position is below s is dropped as an under-run,
   `receiver.under_runs`; one whose end is above s + term length ÷ 2 is dropped as an
   over-run, `receiver.over_runs`; one that fails the datagram check is dropped as
   `receiver.rejected_datagrams`. Otherwise each frame is written at its term offset in
   the image's log, the bytes after the first eight with plain stores and then the first
   eight with a release store, and only where the frame length there is 0, so that a
   duplicate changes nothing; the high-water mark, `image.hwm_position`, becomes the
   greater of itself and the datagram's end position. A heartbeat moves the high-water
   mark the same way and, with EOS, records its position as the image's end of stream
   position once the rebuild position reaches it.
3. **Rebuild and NAK.** The rebuild position, `image.rebuild_position`, is the end of the
   contiguous frames from the greater of itself and the fastest subscriber's position,
   up to the high-water mark, scanned on every slow cycle. A gap between it and the next
   frame with a length, or the high-water mark, is a loss, counted in
   `receiver.gaps_detected` when it is first seen: after the NAK delay the receiver
   sends a NAK for its term id, term offset and length, and again every NAK retry while
   the same gap persists; a gap that changes restarts the delay. A subscriber reads the
   image's log as "Reading" in `spec/log-buffer.md` says and stops at the gap itself, so
   that nothing is delivered from beyond a gap before it is filled.
4. **Status messages.** One at creation, then whenever the slowest subscriber's position
   is at least the status message threshold past s, when the receiver window length
   changes, or when the status message interval has passed since the last: it carries the
   slowest subscriber's position as consumption term id and offset, the receiver window
   length, the receiver id, and the end-of-stream flag once the stream has ended. The
   slowest subscriber's position is the least `subscription.position` of the image's
   subscriptions (`spec/log-buffer.md`, "Counters"). Before s rises, the receiver cleans
   the image's log from its clean position to the slowest subscriber's position less the
   term length, as the driver cleans a publication's log, so that a term is zero before a
   frame lands in it.
5. **End.** A subscription removed while others remain on the endpoint and stream, by
   `remove subscription`, `client close` or `client timeout`, frees its
   `subscription.position` and the image goes on with the slowest of the rest; when the
   last one is removed, the image is retired as below, with no `unavailable image` for
   a subscription its client removed (`spec/client-protocol.md`, "Commands"), and the
   sender, hearing no status message for the connection timeout, disconnects its
   publication, so that the publisher reads `not connected` as it does over
   `cavalry:ipc` when the last subscriber goes. An image with no activity for the image
   liveness timeout is retired too: a data datagram that is accepted, and a heartbeat
   at or above the last status message's position less a term, count as activity; an
   under-run and an over-run do not; so a sender that pauses its data while
   heartbeating keeps its image. Retiring is `unavailable image` to its subscriptions
   that remain, its counters
   freed after the counter reuse timeout, its log removed once every subscription has
   released it, and its session record put **on cool-down** for the image cool-down, a
   bound of this page's own on every retire, during which a setup, data datagram or heartbeat for the
   session is dropped and counted in `receiver.dropped_cool_down` with no status message
   sent, so that a late retransmit or heartbeat for a retired session creates no second
   image at its position; when the cool-down ends the record is removed, and a setup for
   the session creates a new image with a registration id of its own. The cool-down
   costs a sender that resumes after a pause of the liveness timeout, and a subscription
   added back on an endpoint that stays open for another stream, up to one cool-down
   before the stream is received again, which `doctor` explains from
   `receiver.dropped_cool_down`. The image's states are a state machine in
   `counters.toml`: created (`image.created`), ended by its stream (`image.ended`),
   timed out (`image.timed_out`), left by its last subscription (`image.unsubscribed`),
   and removed once its log is gone (`image.removed`). At end of stream,
   once the rebuild position has reached the high-water mark, the receiver stores the end
   of stream position in the log, sends the status message with the end-of-stream flag
   five times at the status message interval, a bound of this page's own, and the image
   is retired the same way once every subscriber has reached that position or the
   liveness timeout passes.
6. **RTT.** An RTT measurement with the reply flag for the image's session and stream
   is answered as the sender answers one, when its receiver id is this driver's or 0.

The receiver keeps one table of session records for the driver, its size under
"Values", keyed by endpoint, stream id and session id, each record in one of four
states: pending a setup, in progress, with an image, or on cool-down. When the table is
full, a datagram for a session with no record is dropped and counted in
`receiver.sessions_full`, and no status message is sent, so that a sender that opens a
session per datagram fills the table and not the driver; a record is freed when a
pending setup expires, when the conductor refuses the image, and when a cool-down ends.
A record is keyed by its endpoint's registration id and dies with the endpoint, so a
cool-down never reaches the next endpoint on the same channel; an image the conductor
hands back for a session whose last subscription on the endpoint and stream was removed
while the image was being created is retired at once, as step 5 says, and its record
freed.

An endpoint has a life of its own, so the two registries of "Values" have a release. A
receive endpoint is bound by the first `add subscription` on its resolved channel and
closes when its last subscription is removed and its last image retired; a send
endpoint is opened by the first `add publication` on its resolved channel and closes
when its last publication's linger ends. Closing is `channel.status` 2: the socket is
closed, the endpoint's five counters are freed after the counter reuse timeout
(`spec/client-protocol.md`, "Liveness and timeouts"), and the registry slot is returned,
so a driver that has seen more than 64 distinct endpoints over its life refuses none
that are gone. The endpoint's states, initializing (0), active (1), closing (2) and
errored (−1), are a state machine in `counters.toml` with a transition counter each
that this version takes: `channel.opened` (initializing to active) and `channel.closed`
(active to closing, then freed). The socket is opened, bound and connected on the
control plane as the command is handled, so a port in use or an interface that is not
there is error code 4 to that command, with nothing registered and no counter kept;
initializing is the moment between the slot and the socket, which a client never
observes. No failure on a live socket moves an endpoint to errored in this version: a
send the operating system refuses is counted in `sender.send_failed` and the burst is
retried on the next cycle as a short send is; a port unreachable the connected socket
reports after an ICMP error, `ECONNREFUSED` on Linux, the normal case for a publisher
started before its subscriber or whose subscriber's driver restarts, is counted the
same way and ignored, and so are `EAGAIN`, `ENOBUFS` and `EINTR`; a receive the
operating system refuses is counted in `receiver.receive_failed` and the poll retried.
Errored, −1, is reserved for a later version that re-resolves names; a client never
reads −1 from the indicator in this version.

`start recording` on a `cavalry:udp` channel (`spec/client-protocol.md`, "Commands")
records every live log on this host whose channel and stream id match, publications and
images alike, one recording per log, each `recording ready` naming its session id; the
recorder reads an image's log as it reads a publication's and stops at a gap until it is
filled.

## Counters

Static unless a key is named (`counters.toml`):

| Counter | Moves when |
| --- | --- |
| `sender.bytes_sent`, `sender.frames_sent` | a data or padding frame is sent |
| `sender.setups_sent`, `sender.heartbeats_sent` | a setup frame or a heartbeat is sent |
| `sender.retransmits_sent`, `sender.retransmitted_bytes` | a NAK is answered |
| `sender.retransmit_overflow` | a NAK for another range arrives while a retransmit is outstanding |
| `sender.short_sends`, `sender.send_failed` | the socket took part of a datagram, or refused one |
| `sender.flow_control_limited` | a publication enters the state of having a frame to send and no window |
| `sender.naks_received`, `sender.naks_ignored` | a NAK arrives; one lies outside the resend window |
| `sender.status_messages_received`, `sender.status_messages_rejected` | a status message arrives; one lies outside the window, or asks a publication at its end of stream for a setup |
| `sender.rtt_replies_sent` | an RTT measurement is answered |
| `sender.position`, `sender.limit` | keyed as `publication.position` is: the sender position and the sender limit of one publication |
| `receiver.bytes_received`, `receiver.frames_received` | a data or padding frame is accepted |
| `receiver.setups_received`, `receiver.setups_unwanted`, `receiver.setups_elicited` | a setup frame arrives; one names a stream nobody wants; a status message asked a sender for one |
| `receiver.heartbeats_received` | a heartbeat is accepted |
| `receiver.status_messages_sent`, `receiver.naks_sent` | one is sent |
| `receiver.gaps_detected` | a gap is first seen |
| `receiver.under_runs`, `receiver.over_runs`, `receiver.rejected_datagrams` | a datagram is dropped by the receiver's bounds or the datagram check |
| `receiver.receive_failed` | a receive the operating system refused; the poll is retried |
| `receiver.dropped_frames` | a frame of a version or type this reader does not handle |
| `receiver.dropped_pending`, `receiver.dropped_in_progress`, `receiver.dropped_cool_down` | a setup, data datagram or heartbeat for a session with no image, by the session's state: the in-progress and cool-down counts are the ones an operator asks about |
| `receiver.dropped_unwanted` | a data datagram, heartbeat or RTT measurement for a stream no subscription on the endpoint wants, or an RTT measurement for a session with no image |
| `receiver.sessions_full` | a datagram for a new session found the session table full |
| `receiver.rtt_replies_sent` | an RTT measurement is answered |
| `image.hwm_position`, `image.rebuild_position` | keyed as `subscription.position` is, with the image's registration id: the high-water mark and the rebuild position of one image |
| `channel.status` | keyed by the endpoint's registration id, the correlation id of the command that created it, labelled with the resolved channel: 0 initializing, 1 active, 2 closing, −1 errored |
| `channel.socket_rcvbuf`, `channel.socket_sndbuf` | keyed the same: the buffer lengths the socket holds, read back after they were set |
| `channel.images`, `channel.publications` | keyed the same: how many the endpoint carries |

The publication's connection (`publication.connected`, `publication.disconnected`),
the image's life (`image.created`, `image.ended`, `image.timed_out`,
`image.unsubscribed`, `image.removed`) and the endpoint's (`channel.opened`,
`channel.closed`) are state machines in `counters.toml`: 44 static counters in all on
this page, the 15 under `sender`, the 20 under `receiver` and these nine.

## Values

Aeron's, and the driver's in this version; a configuration key for one needs its
`why_not_default` and `doctor` check (`CLAUDE.md`, Configuration and defaults).

| Value | This version |
| --- | --- |
| MTU length | 1408 bytes |
| UDP term length | 16777216 bytes |
| publication window | term length ÷ 2 |
| receiver window length | the lesser of 131072 bytes and term length ÷ 2 |
| socket receive buffer | 131072 bytes asked; the socket's answer in `channel.socket_rcvbuf` |
| socket send buffer | the operating system's |
| setup interval | 100 ms |
| pending setup timeout | 1 s |
| heartbeat interval | 100 ms |
| status message interval | 200 ms |
| status message threshold | receiver window length ÷ 4 |
| NAK delay, NAK retry | 1 µs, 100 µs |
| retransmit linger | 10 ms |
| connection timeout | 5 s |
| image liveness timeout | 10 s |
| image cool-down | 10 s, the liveness timeout; a bound of this page's own |
| session records | 4096 per driver, twice the images registry; a datagram for a 4097th session is dropped and counted in `receiver.sessions_full` |
| linger | 5 s, `spec/client-protocol.md`'s publication linger |
| datagrams per burst, per poll | 16 |
| send endpoints, receive endpoints | 64 each; a 65th is error code 10 |

Timeouts are measured on the clock the driver stores in the consumer heartbeat
(`spec/client-protocol.md`, "Liveness and timeouts").

## What a reader rejects

A reader is given a datagram and reads one control frame, or the data frames, from its
start. It runs these checks in this order and reports the first that fails, by name;
every rejection counts in `receiver.dropped_frames` or `receiver.rejected_datagrams`,
and nothing from a rejected datagram is delivered.

| Reason | Check |
| --- | --- |
| `too-short` | fewer than 8 bytes, or fewer than the type's length (40, 36, 28, 40) |
| `version` | the version byte is not 0 |
| `type` | the type is not 0, 1, 2, 3, 5 or 6 |
| `length` | the frame length is not the type's length, or for a status message neither 36 nor 44; for a data datagram, the datagram's length is not a multiple of 32, or a heartbeat is not exactly 32 bytes |
| `term-length` | a setup frame's term length is not a power of two from 65536 to 1073741824 |
| `mtu` | a setup frame's MTU length is not a multiple of 32 from 64 to 65504 |
| `term-offset` | a term offset is negative, not a multiple of 32, or not below the term length |
| `window` | a receiver window length is negative |
| `nak-length` | a NAK's length is not a positive multiple of 32, or runs past the end of the term |
| `datagram` | a data datagram's frames fail `spec/frame.md`'s checks against the bytes left, a frame's term offset is not the running offset from the first, or the frames run past the end of the term |

A channel is checked in this order: `grammar` (the scheme, the `?`, the `key=value`
form, a repeated key), `key` (a key this version does not name), `endpoint` (missing, a
port of 0 or above 65535, a malformed address), `multicast` (a multicast address), each
error code 1 with the reason in the message; then `host` (the name does not resolve),
error code 9. The socket that cannot be opened, bound or connected is error code 4 with
the operating system's reason.

## Vectors

`spec/vectors/udp.json`, written by `spec/vectors/udp.py`, the reference encoder. Each
vector has a `kind`:

- `frame`: the `bytes` of one control frame or heartbeat and the `fields` a conforming
  reader decodes, or the `error` it reports; every frame once valid, a status message
  with and without the group tag, a heartbeat with and without EOS, and one vector per
  rejection in the order the checks run.
- `datagram`: the `bytes` of a data datagram with the `term_length`, and the `frames` a
  reader walks (term offset and frame length of each) or the `error`.
- `channel`: a channel `string` and the `fields` it parses to (endpoint host and port,
  interface) or the `error` and its `code`.
- `arithmetic`: `inputs` and the `fields` that follow: the sender limit from a status
  message against a sender position, the retransmit window, the receiver's under-run and
  over-run bounds, the status message threshold, and an image's start position.

A conforming implementation decodes every valid vector to its fields, encodes those
fields to the same bytes, computes every arithmetic vector's fields from its inputs, and
reports the named reason for every invalid one.
