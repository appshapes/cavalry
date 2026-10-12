# 0014. Lay out the command ring and the broadcast buffer as Agrona's ring buffers behind a Cavalry header

- **Status:** Accepted
- **Date:** 2026-10-11
- **Invariants affected:** 1, 3, 4, 6
- **Supersedes:** none

## Context

The first slice (ADR 0007) needs two queues between the processes on a host besides the
log buffer: one for the commands every client sends the driver, add a publication, add a
subscription, remove one, keep alive, and one for the events the driver sends every
client, a publication is ready, an image is available or gone, a command failed, a client
timed out. Both are control plane: a command or event per publication or subscription,
never per message. Both are shared between processes, so a lock held by a process that
died would hang the host, and both are mapped by clients the driver does not trust, so
ADR 0010's table of writer, readers and bound applies to every field, and ADR 0011's
128-byte slot to every field one party writes and another reads in its hot loop. Card
78's verdict (`docs/explanation/should-zigbolt-inform-cavalry.md`, "SPSC, MPSC and
broadcast") already decided which designs to copy: Agrona's many-to-one ring buffer with
its unblock protocol and consumer heartbeat for the client-to-driver path, because
ZigBolt's MPSC "will be blocked forever" when a producer dies, and Agrona's broadcast
buffer for the driver-to-client path, control plane only, because a lapped receiver
cannot say what it lost.

Agrona, read on 2026-10-11 at commit 314c0c8: the many-to-one ring buffer is a
power-of-two ring followed by a 768-byte trailer of five longs 128 bytes apart, tail
position, head cache position, head position, correlation id counter and consumer
heartbeat (`RingBufferDescriptor`); a record is a length and a type in 8 bytes, then the
message, aligned to 8, with a negative length while claimed, a positive length once
committed and type −1 for padding (`RecordDescriptor`, `RingBuffer`). A producer claims
with a compare-and-swap loop on the tail that consults a cached head before the real one
and pads to the end of the ring when a record would cross it; the consumer reads to the
end of the ring or a non-positive length, zeroes what it read, and releases the head;
`unblock` turns a stale claim into padding and scans a zero gap for the next record;
`consumerHeartbeatTime` and `nextCorrelationId` are release stores and a fetch-and-add on
the trailer (`ManyToOneRingBuffer`). The broadcast buffer is a power-of-two ring followed
by a 128-byte trailer of three longs 8 bytes apart, tail intent, tail and latest
(`BroadcastBufferDescriptor`); the transmitter stores the tail intent with a release
store and a release fence before it writes, pads to the end when a record would cross
it, and releases the latest and then the tail (`BroadcastTransmitter`); a receiver joins
at the latest record, treats a cursor more than capacity behind the tail intent as lapped
and resynchronises to the latest, and validates after it copied a record out with an
acquire fence and the tail intent again (`BroadcastReceiver`, `CopyBroadcastReceiver`,
which throws "unable to keep up with broadcast"). Aeron at 32e531c uses them as the
to-driver and to-clients buffers of `cnc.dat` (`CncFileDescriptor`); `DriverProxy` takes
a correlation id, claims and commits a command per call and reads the heartbeat for the
client's driver timeout; `DriverConductor` stores the heartbeat on its timer, stores −1
when it closes, and unblocks the ring once the head has not moved for the client
liveness timeout, counting `UNBLOCKED_COMMANDS`; `ClientProxy` transmits each event;
`DriverEventsAdapter` dispatches them, and `ClientConductor` terminates the client when
the receiver reports a lap or the heartbeat is older than the driver timeout. The
defaults are a 1 MiB ring each, a 10 s client liveness timeout and a 10 s driver timeout
(`Configuration`).

Two of Agrona's behaviours have holes a Cavalry layout cannot leave. Its `unblock` scans
a zero gap only up to the first nonzero word, and returns without writing when it reaches
the limit with only zeros: a producer that dies between its compare-and-swap and its
first store, at the end of the ring, blocks the ring for good, while Aeron's own
`TermUnblocker` pads a term to its end in the same case. And its record header is two
plain 4-byte stores after the release store of the negative length, so a reader may see a
length whose type is not yet written.

## Decision

Two layouts, one file each, in the driver's directory: `spec/command-ring.md`, the file
`command-ring`, and `spec/broadcast-buffer.md`, the file `broadcast-buffer`, both at
version 0 as Agrona's, with the ring first and the trailer after it, as the log buffer
puts its metadata page (ADR 0013), so that the ring is page-aligned and a reader finds
the trailer from the file's size; the capacity a power of two from 4096 to 2^30, Agrona's
upper bound; the file length exactly capacity + trailer. Each trailer begins with the
128-byte header of ADR 0012, magic `CVLYCMDR` or `CVLYBCST`, version, capacity, creator
pid and start timestamp, written magic-last, then one 128-byte slot per trailer field
(ADR 0011): the command ring's five, so its trailer is 768 bytes, Agrona's figure with
the header in Agrona's leading pad; the broadcast buffer's three, so its trailer is 512
bytes. Records are Agrona's, length and type in 8 bytes, aligned to 8, padding type −1,
message types 1 or more, the longest message capacity ÷ 8, never crossing the end of the
ring; with one refinement: a record's length and type are written in one 8-byte store,
at claim, commit, abort, padding and unblock, so a reader never sees a length without
its type. The command ring's protocols are `ManyToOneRingBuffer`'s, claim by
compare-and-swap with the head cache, read to a non-positive length or the end of the
ring with the bytes read zeroed before the head is released, unblock after the client
liveness timeout, heartbeat and correlation ids, with two additions: each party checks the tail against the position it loaded
before it, the head cache for a producer and its own head for the consumer, and the
consumer checks every record against the bounds of ADR 0010, counting `command_ring.rejected_tail` and
`command_ring.rejected_record`, and a corrupt record or tail stops the ring, since the ring is anonymous and the driver
cannot evict its writer; and an unblock that reaches the end of the ring with only zeros
pads the gap to the end, as `TermUnblocker` pads a term, where Agrona's returns without
writing, while a gap that ends at the tail's index waits for the next claim to bound it,
as Agrona's does; every unblock counts in `command_ring.unblocked`. The driver stores −1 in the consumer
heartbeat when it closes, as Aeron's does, so a client tells `driver-closed` from
`driver-timeout`. The broadcast buffer's protocols are `BroadcastTransmitter`'s and
`BroadcastReceiver`'s: tail intent with a release store and a release fence before the
record, latest then tail released after it; a receiver joins at the latest record, is
`lapped` when its cursor is capacity or more behind the tail intent, copies a record out
and validates with an acquire fence and the tail intent again, and checks the three
trailer fields, loaded tail first, against the bounds that hold for values loaded in
that order, and every record against its bounds, where a failing record is `lapped`
if the tail intent has moved past the cursor and `record` otherwise. Record bytes in the
broadcast buffer are written and read with relaxed atomic operations a word at a time,
since a lapped receiver races the transmitter by design. The lapped client closes, as
Aeron's does; its transition and the command ring's `full` are a client's, and a client
owns no counter until the feature that registers clients with the driver, which gives it
its counters. The vectors in `spec/vectors/command-ring.json` and
`spec/vectors/broadcast-buffer.json` come from the two reference encoders and cover the
headers, the records, the claim, unblock and read arithmetic of the ring and the transmit
and receive arithmetic of the buffer.

## Alternatives considered

- **ZigBolt's SPSC, MPSC and broadcast rings.** Lost in card 78's verdict: a copy per
  message, a byte loop per write, no unblock, no header.
- **One CnC file holding both buffers and the counters, as Aeron.** Lost in ADR 0012:
  one file per layout, each with its own header, checks and version.
- **The header first, as the counters file.** Lost: the log buffer already put its
  metadata after its data so that the data region is page-aligned and the metadata is
  found from the kernel's file size, and Agrona's rings put the trailer after the ring;
  one shape for every layout whose data is a ring.
- **Agrona's trailers as they are: five longs 128 bytes apart with no header, and three
  longs 8 bytes apart.** Lost: every Cavalry layout carries a magic and a version, and
  ADR 0011 gives each peer-shared hot field its own 128 bytes; the command ring's trailer
  keeps Agrona's 768 bytes, the broadcast buffer's grows from 128 to 512.
- **One command ring per client, Agrona's `OneToOneRingBuffer`.** Lost: the driver
  would poll one ring per client on every duty cycle, a file and a mapping per client
  for what one ring carries, and Aeron runs many-to-one.
- **A per-client event ring instead of the broadcast buffer, so that no event is ever
  lost.** Lost: a slow client would block the driver's conductor or need a ring the size
  of its backlog, against invariant 3, and a file and a mapping per client; Aeron's
  broadcast loses to a slow client and tells it, which is the right trade on the control
  plane, where a lapped client re-registers.
- **Delivering broadcast records in place, without the copy.** Lost: a handler would act
  on bytes the transmitter is overwriting before the validation that follows; the copy is
  Agrona's `CopyBroadcastReceiver`, on the control plane, where a copy per event is
  nothing.
- **Agrona's two-store record header.** Lost: one 8-byte store costs the same and
  removes a state, a length with no type, from the layout.
- **Agrona's `unblock` as it is, returning without writing at the end of the ring.**
  Lost: a dead claim at the end of the ring would block every client on the host until
  a restart, with nothing a counter could say; Aeron's `TermUnblocker` already pads to
  the end of a term in that case. Padding a gap that ends at the tail's index as well
  was considered and lost: the next claim bounds that gap a moment later, and padding
  it at once would cover a live client's fresh claim that the later scan would have
  spared.
- **Skipping a corrupt command record as padding, or discarding the ring to the tail.**
  Lost: the corrupt field is the length, so the record's end is unknown, and zeroing
  claimed space under live producers leaves stale records that a later lap reads as
  commands; stopping the ring with a counter is the honest degradation, and a restart
  recreates the file.
- **Agrona's minimum capacity, 8 bytes.** Lost: a 4096-byte floor makes the longest
  message 512 bytes, enough for a command with a channel URI, and keeps the ring at least
  a page.
- **A lap counter for the broadcast receiver in this slice.** Lost: a client owns no
  counter before it is registered with the driver, and a counter only the client could
  write would need a record the driver has not allocated; the registration feature gives
  each client its counters, and this ADR's Revisit clause names it.
- **Milliseconds for the heartbeat, as Agrona.** Lost: one unit for every timestamp in
  `spec/`.
- **A checksum over the trailer.** Lost, as in ADR 0010, ADR 0012 and ADR 0013.

## Evidence

- Agrona at commit 314c0c8, read on 2026-10-11:
  `concurrent/ringbuffer/RingBufferDescriptor.java` (the trailer offsets,
  `checkCapacity`), `concurrent/ringbuffer/RecordDescriptor.java` (the record header,
  `ALIGNMENT`), `concurrent/ringbuffer/RingBuffer.java` (`PADDING_MSG_TYPE_ID`,
  `INSUFFICIENT_CAPACITY`), `concurrent/ringbuffer/ManyToOneRingBuffer.java` (`write`,
  `tryClaim`, `commit`, `abort`, `read`, `claimCapacity`, `unblock`,
  `scanBackToConfirmStillZeroed`, `consumerHeartbeatTime`, `nextCorrelationId`, `size`),
  `concurrent/broadcast/BroadcastBufferDescriptor.java`,
  `concurrent/broadcast/RecordDescriptor.java` (`calculateMaxMessageLength`),
  `concurrent/broadcast/BroadcastTransmitter.java` (`transmit`, `signalTailIntent`,
  `insertPaddingRecord`), `concurrent/broadcast/BroadcastReceiver.java` (`receiveNext`,
  `validate`, `lappedCount`) and `concurrent/broadcast/CopyBroadcastReceiver.java`
  (`receive`).
- Aeron at commit 32e531c, read on 2026-10-11: `CncFileDescriptor.java`
  (`createToDriverBuffer`, `createToClientsBuffer`), `DriverProxy.java`
  (`addPublication` and the other commands, `timeOfLastDriverKeepaliveMs`),
  `DriverEventsAdapter.java` (`onMessage`), `ClientConductor.java` (the driver timeout
  and the invalid adapter), `driver/ClientProxy.java` (`transmit`),
  `driver/DriverConductor.java` (`checkForBlockedToDriverCommands`,
  `consumerHeartbeatTime` on the timer and at close) and `driver/Configuration.java`
  (`CONDUCTOR_BUFFER_LENGTH_DEFAULT`, `TO_CLIENTS_BUFFER_LENGTH_DEFAULT`,
  `CLIENT_LIVENESS_TIMEOUT_DEFAULT_NS`).
- `docs/explanation/should-zigbolt-inform-cavalry.md`, "SPSC, MPSC and broadcast", and
  the rows for the queues and the broadcast buffer in its verdicts table.
- `spec/vectors/command-ring.py` produced 56 vectors and
  `spec/vectors/broadcast-buffer.py` 52 on 2026-10-11, the counts in each `.json`.
- Nothing measured: both layouts are control plane, a record per command or event, so
  `just bench` owes them no histogram; ADR 0011's slot size is re-measured on the log
  buffer, not here.

## Consequences

Easier: a client that dies mid-command cannot block the driver; a slow client cannot
block the driver's events; a stranger reads two pages and knows who writes every field;
Agrona's documentation and twelve years of production explain both rings; every client
language implements the same vectors.

Harder: a corrupt command record or tail stops the whole host's command ring until the
driver restarts, because the ring is anonymous, and `doctor` can only say to restart and
then stop clients one at a time; an unblock may cover the fresh claim of a live client in
the gap, whose command is lost and whose response timeout reports it; a lapped client is a closed client; the
broadcast receiver copies every event; two more files in the driver's directory; the
client's `full`, `lapped`, `driver-closed` and `driver-timeout` have no counter until
clients own counters.

Revisit: when the client registration feature lands, to give each client a lap counter
and a full counter; when a measurement shows the command path on a latency budget, which
would move its records onto the data plane rules; when a command needs more than
capacity ÷ 8 bytes; when the driver can name the writer of a record, which would make
eviction possible instead of stopping the ring.

## References

- `spec/command-ring.md`, `spec/vectors/command-ring.py`,
  `docs/reference/command-ring.md`; `spec/broadcast-buffer.md`,
  `spec/vectors/broadcast-buffer.py`, `docs/reference/broadcast-buffer.md`;
  `counters.toml`
- `CLAUDE.md`: invariants 1, 3, 4 and 6; Protocol; Unsafe. `.claude/rules/data-plane.md`
- ADR 0007, ADR 0010, ADR 0011, ADR 0012, ADR 0013
- Agrona: <https://github.com/aeron-io/agrona/blob/master/agrona/src/main/java/org/agrona/concurrent/ringbuffer/ManyToOneRingBuffer.java>,
  <https://github.com/aeron-io/agrona/blob/master/agrona/src/main/java/org/agrona/concurrent/broadcast/BroadcastTransmitter.java>,
  <https://github.com/aeron-io/agrona/blob/master/agrona/src/main/java/org/agrona/concurrent/broadcast/BroadcastReceiver.java>
- Aeron: <https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/CncFileDescriptor.java>,
  <https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/DriverProxy.java>,
  <https://github.com/aeron-io/aeron/blob/master/aeron-driver/src/main/java/io/aeron/driver/DriverConductor.java>
- AppShapes Trello card 83, "Build the log buffer, the ring buffers and the bench"
