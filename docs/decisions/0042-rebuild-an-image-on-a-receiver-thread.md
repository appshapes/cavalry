# 0042. Rebuild an image on a dedicated receiver thread with a bounded session table

- **Status:** Accepted
- **Date:** 2026-11-02
- **Invariants affected:** 1, 2, 3, 4
- **Supersedes:** none; amends `spec/udp.md` on five points named under Decision, each
  within version 0 of the transport, since none changes a byte on the wire

## Context

ADR 0039 orders the UDP slice's third feature, the receiver (card 181): the driver's
fifth dedicated thread rebuilds a `cavalry:udp` stream from the wire into an image, a
log buffer of type 2 that subscribers read as any log, by the rules of `spec/udp.md`,
"The receiver", which ADR 0040 accepted: a setup frame creates the image through the
conductor, data frames are inserted by term offset within the bounds the last status
message set, a gap is NAKed after the NAK delay and again every NAK retry, status
messages go on three triggers, an image with no activity for the liveness timeout or
ended by EOS is retired, and a bounded table of session records in four states keeps a
second setup or a late retransmit from creating a second image. ADR 0039 decided the
thread itself and left one decision to this ADR with its evidence: the image's rebuild
and how its log is cleaned. ADR 0041 decided the socket dependency for both threads, so
the receiver takes `recvmmsg`, `recvmsg`, `sendmsg`, the buffer options and the
`SockaddrIn` and `SockaddrIn6` readers from `nix` with no second ADR, and its allocation
audit of those calls stands for this thread.

The constraints are these. The receiver is on the message path (`CLAUDE.md`, invariant
2): every frame a subscriber reads over UDP passes through it, so its duty cycle
allocates nothing, locks nothing and makes no syscall per message, and the one syscall
per poll takes up to 16 datagrams. Every table it keeps has a bound with a documented
behaviour when full (invariant 3): 4096 session records, 64 receive endpoints, 2048
images and the queues to the conductor. A message is delivered once, in order, or its
absence is reported (invariant 1): a duplicate changes nothing in the log, a frame beyond
a gap is not delivered before the gap is filled, and a second image for a session is
never created. Every transition has its counter (invariant 4): the 20 `receiver.*`
counters and the image's five of `spec/udp.md`, "Counters". The driver already runs
four dedicated threads linked to the conductor by bounded SPSC queues (ADR 0025, ADR
0030, ADR 0041), and the sender's shape, a thread built on its own thread because the
headers its batched calls fill are not `Send`, requests and replies with a bounded
list of replies held back when the reply queue is full, and a park while it holds
nothing, is the shape this thread copies. Aeron's receiver side, read on 2026-10-31
(ADR 0040, Context) and again on 2026-11-02 for this ADR: `Receiver`,
`ReceiveChannelEndpoint`, `DataPacketDispatcher` with its `SessionInterest` map,
`PublicationImage`, `LossDetector`, `TermGapScanner` and `TermRebuilder`.

## Decision

The receiver is the driver's fifth dedicated thread, `DriverThread::Receiver`, with
the driver's idle strategy and a name in the `start_with_threads` hook so that the bench
pins it; it is built on its own thread, as the sender is, since the headers `recvmmsg`
fills with each datagram's source address are not `Send`, and it parks while it holds
no receive endpoint, unparked by the conductor's next request. It is linked to the
conductor by two bounded SPSC queues with their capacities in `spec.rs`: requests from
the conductor, an endpoint added or removed, a subscription's interest in a stream on an
endpoint added or removed, an image created or refused in answer to a setup, and a
reader added, attached or removed; replies from the receiver, a setup frame for a
session the conductor must create an image for, an image retired with the state it
ended in, a reader removed, a reader whose position failed its bound, and an endpoint
closed. An `add subscription` whose two requests find no room in the queue, or find a
request still waiting for room, is error code 10 and registers nothing, as an `add
publication` is (ADR 0041); every other request the queue has no room for waits in a
bounded list in the conductor, one per subscription, endpoint, image and session record
in progress, offered again each cycle in order; and a reply the reply queue has no room
for is held by the receiver in a list bounded by what can be owed at once, one setup
per session record, a retire and a removed reader per image and recording, a closed
endpoint, and the requests of one cycle, at which bound the receiver handles no request
and marks no session in progress, and keeps receiving.

The conductor owns the control plane of it: `add subscription` on a `cavalry:udp`
channel resolves the host once, binds the channel's receive endpoint when it is the
first subscription on that resolved channel, one `std::net::UdpSocket` bound to the
endpoint and set non-blocking with the receive buffer asked at the 131072 bytes of
`spec/udp.md`, "Values", and its lengths read back into `channel.socket_rcvbuf` and
`channel.socket_sndbuf`, allocates the endpoint's five counters, hands the socket to the
receiver, and answers `subscription ready` with the endpoint's `channel.status` counter;
the interface a channel names keys the channel and binds nothing on a receive endpoint
in this version, since a unicast receive endpoint's address is the endpoint's, which
amends the sentence of "The receiver" that bound both. On a setup reply the conductor
gives the image a registration id from the command ring's correlation id counter,
creates `images/<registration id>.logbuffer` with `LogBuffers::create_at` at the
position the setup names, allocates `image.hwm_position` and `image.rebuild_position`
keyed as `subscription.position` is, allocates a `subscription.position` per
subscription on the endpoint and stream at that position, sends `available image` to
each with the sender's `<host>:<port>` as the source identity, and hands the receiver
the log, its writer, the two counters and the readers; a registry or counters file
with no room, or a log that cannot be created, is a refusal back to the receiver, which
frees the record so that the sender's next setup tries again. The conductor keeps of
each image only its ids and the counter ids it will free: the receiver owns the image's
log and its writer, and the file is unlinked by the conductor when the receiver reports
the image retired, after `unavailable image` has gone to the subscriptions that remain,
as a closed publication's log is unlinked while its subscribers may still map it.

The rebuild and the cleaning, the decision ADR 0039 left here. The receiver inserts each
frame of an accepted datagram through `cavalry-shm`'s image writer, by term offset, the
first eight bytes last with a release store and only where the frame length is 0 (ADR
0040). On every duty cycle on which the high-water mark is past the rebuild position, it
scans the image through two readers of the frame length word that `cavalry-shm` adds to
the image writer: the end of the contiguous committed frames from the greater of the
rebuild position and the fastest reader's position, and, from there, the next frame
with a length up to the high-water mark; the scan is bounded at 1 MiB of term per image
per cycle, so a gap longer than the bound is reported as the bound and NAKed for that
length, and the next cycle scans on from where the last stopped. No frame is checked on
the scan: the writer inserted checked frames, and a subscriber checks what it reads
(`spec/log-buffer.md`, "Reading"). The gap, the rebuild position and the next frame, is
the NAK's range; a gap first seen counts `receiver.gaps_detected`, is NAKed after the
NAK delay and again every NAK retry while it is the same, and a gap that changes restarts
the delay. A reader of an image is a subscription's `subscription.position` or a
recording's `recording.position`, the latter standing at the recording's start position
until the recorder's reply attaches the counter, as the conductor's limit stands for a
publication (ADR 0025); the readers live with the receiver, their counter handles handed
over by the conductor and handed back in the reply that says a reader was removed, after
which the conductor frees the record, so that the receiver never loads a record the
conductor has freed. The slowest reader's position is the status message's consumption
position, the fastest's is where the rebuild scan starts, and before a status message
raises that position the receiver cleans the image's log from its clean position to the
slowest reader's position less the term length, whole terms at a time, so that a term is
zero before a frame lands in it; this amends "The receiver" and "Counters" of
`spec/udp.md`, where the slowest subscriber's position stood alone, to count a
recording of the image among the readers, which is what keeps a recorded byte from being
cleaned before the recorder has written it (invariant 1). A reader's position outside
its bound, below the image's start, above the largest position or not a multiple of 32,
is not used, counts `log_buffer.rejected_position` and is reported once to the
conductor, which evicts the client as it evicts one on a publication (ADR 0010).

The session table is one open-addressing hash table of 4096 records keyed by the
endpoint's registration id, the stream id and the session id, with 8192 slots and
backward-shift deletion, so that a datagram's session is found in a bounded number of
probes with no allocation and no list walk; a datagram for a session with no record
when the table is full is dropped and counted in `receiver.sessions_full`. Pending
records and cool-downs expire on a sweep the receiver runs once every 100 ms, a bound of
its own, so that the table is not walked on every cycle. The receiver's id is an i64
drawn from the clock and the process id when it starts. An RTT measurement for an image's
session whose receiver id is neither this driver's nor 0 is dropped and counted in
`receiver.dropped_unwanted`, which amends step 6 of "The receiver", where it had no
counter. The receive socket's datagram buffers, 16 of 65536 bytes so that a sender of
any MTU the setup frame allows is read whole, are preallocated once by the receiver as
it is built and lent to every endpoint's poll, as the sender lends its headers; a
control frame the receiver sends goes to the source address of the setup frame that
created the image, through one `sendmsg` with that address and no control message,
which allocates nothing (ADR 0041, Evidence).

`start recording` on a `cavalry:udp` channel records a log whose channel and stream id
match, a publication sending on the resolved channel or an image received on it, one
recording per command and the lowest registration id not yet recorded first, as the
command's rule is over `cavalry:ipc`; the recorder reads an image's log through the same
reader it reads a publication's with, which stops at a frame length of 0, a gap the wire
has not filled, until it is filled, and the recording's start position is the image's
rebuild position when the command is handled. `start replay` on a `cavalry:udp` channel,
a replay's publication put on the wire by the sender, is not this card's: it stays error
code 11 with a message naming the card that brings it, since a replay's publication
takes the sender's path, not the receiver's, and this change is already the size of the
sender's. Two notes the reviews of pull requests 94 and 95 left for this card are
taken: a status message whose frame length is 44 in a datagram of 40 to 43 bytes is
reported as `length`, which "What a reader rejects" now says with a vector for it; and
step 2 of "The sender" names a committed frame the window does not cover whole as an
entry into the flow-control-limited state, which the sender counts.

## Alternatives considered

- **The slowest reader's position computed by the conductor and passed to the
  receiver.** Lost: a request per cycle per image through the queue, or a shared slot
  per image that is not a counter of `spec/udp.md`; the receiver loads the counters
  itself, as the conductor loads them for a publication, one pass per cycle.
- **A list of session records searched in order.** Lost: 4096 compares per datagram on
  the message path, where a hash finds the record in a handful; Aeron's
  `DataPacketDispatcher` keeps a map per stream for the same reason.
- **Scanning the rebuild with `TermReader::block_from`, which checks every frame.**
  Lost: it would check every frame again on every cycle that scans it, and a frame the
  writer inserted was checked once as the datagram was; the scan reads the length word
  alone, as Aeron's `TermGapScanner` does.
- **A NAK for the whole gap, scanned to its end on one cycle.** Lost: a gap may be half
  a term, eight million bytes of zero words to read on one cycle while the other images
  on the thread wait; the bound keeps every cycle bounded (invariant 3), and the sender
  answers one burst of a NAK per cycle for the same reason (ADR 0041).
- **An image per subscription, each with its own log, as IPC images are pairs.** Lost:
  `spec/udp.md` and ADR 0040 keep one image per session on an endpoint and stream,
  shared by its subscriptions, and a log per subscription would insert every frame as
  many times.
- **Binding a receive endpoint's socket to the interface's address at the endpoint's
  port.** Lost: the endpoint is the address a unicast receiver binds, which is what
  Aeron's `ReceiveChannelEndpoint` binds for unicast; the interface is multicast's and
  returns with it (ADR 0039, Revisit).
- **The receiver inside the conductor's duty cycle, no fifth thread.** Lost: ADR 0039
  decided the thread, and a poll of 16 datagrams inserted into an image is work the
  conductor's clients would wait through on every command.
- **`start replay` over UDP in this change.** Lost: it is the sender's path, a replay's
  publication opened on a send endpoint with its reader, and this change is already the
  size of the sender's; it is a card of its own, refused with code 11 until then.

## Evidence

None measured: the receiver's rows are card 182's, and this change receives the first
datagram. Until then a commit under `cavalry-driver` says what the existing rows show,
as ADR 0039 says. The allocation audit of ADR 0041, read at `nix` v0.31.3 on
2026-10-31, covers every `nix` call on the receiver's duty cycle: `recvmmsg` and
`recvmsg` perform no allocation, and `sendmsg` with no control message allocates
nothing; `MultiHeaders::preallocate` allocates once, on the receiver's start. The
`net` feature those address types need has been on since ADR 0041. What the change
that implements this ADR proves goes in its commit messages: the fault-injection gate
of `CLAUDE.md`'s gate 3 for the wire, a loopback shim in the client's tests that
drops, reorders and duplicates datagrams with every message delivered once and in order
on Linux and macOS, the allocation assertion over the receive path alone in its process,
the conductor's and the receiver's tests driven by hand against a sender by hand, and
a recording of an image replayed byte for byte.

## Consequences

Easier: the subscriber's poll does not change, since an image is a log; the sender's
shape is the receiver's, so one review has read both; `doctor` explains loss on an
image from `image.hwm_position`, `image.rebuild_position` and `receiver.gaps_detected`
alone, and a subscription whose image never arrived from `channel.images` on a receive
endpoint; a recording of an image is a recording.

Harder: a fifth thread wants a core of its own on a gate host for the UDP rows, which
card 182 settles with the sender's; the driver's `DriverThread` enum grows again, which
the bench's `DriverPins` and the host set-up page follow; the conductor's `add
subscription` gains the endpoint path `add publication` has, and `remove subscription`,
`client close` and `client timeout` reach the receiver through the queue, so a
subscription's counters are freed one reply later than an IPC subscription's; a
recording of an image is held back by the image's gaps, as its subscribers are.

Revisit: when multicast binds the interface (ADR 0039); when a gate host shows the 1 MiB
scan bound on a cycle's tail; when `start replay` over UDP lands, for the sender's side
of a replay; when a session table of 4096 is not enough for a host that receives from
many senders, which a rising `receiver.sessions_full` shows.

## References

- ADR 0010, ADR 0013, ADR 0025, ADR 0039, ADR 0040, ADR 0041
- `spec/udp.md`, "The receiver", "Counters", "Values", "What a reader rejects";
  `spec/log-buffer.md`, "The receiver"; `spec/client-protocol.md`, "Commands" (add
  subscription, remove subscription, start recording on a `cavalry:udp` channel)
- `CLAUDE.md`: invariants 1 to 4; Data plane; `.claude/rules/data-plane.md`
- `crates/cavalry-driver/src/lib.rs` (`DriverThread`, `start_with_threads`),
  `src/spec.rs`, `src/conductor.rs` (`SenderLink`, `add_udp_publication`),
  `src/sender.rs` (the thread's shape), `src/socket.rs`;
  `crates/cavalry-shm/src/log/file.rs` (`ImageWriter`); `bench/src/main.rs`
  (`DriverPins`); `crates/cavalry-control/src/doctor.rs`
- Aeron, read 2026-11-02 at `master`:
  `aeron-driver/src/main/java/io/aeron/driver/{Receiver,ReceiverProxy,DataPacketDispatcher,PublicationImage,LossDetector,DriverConductor}.java`;
  `aeron-driver/src/main/java/io/aeron/driver/media/{ReceiveChannelEndpoint,UdpChannelTransport}.java`;
  `aeron-client/src/main/java/io/aeron/logbuffer/{TermGapScanner,TermRebuilder}.java`
- AppShapes Trello card 181, "Receive a UDP stream into an image on the driver's
  receiver thread"; card 182 for the rows
