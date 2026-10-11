# 0013. Lay out the log buffer as Aeron's three terms and a metadata page

- **Status:** Accepted
- **Date:** 2026-10-11
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none; amends ADR 0012's one-writer rule for the counters file

## Context

The log buffer is the third feature of the first slice (ADR 0007) and the layout every
message passes through: a publisher claims a frame in it, a subscriber reads the frame
in place, and the driver keeps the publisher from overrunning the slowest subscriber.
Invariant 1 wants a stream ordered and delivered once or reported absent; invariant 2
wants claim, commit and read to allocate nothing, lock nothing, make no syscall and
have no panic path; invariant 3 wants a fixed-size log with back-pressure returned to
the caller. The layout is shared between processes, so ADR 0010's table of writer,
readers and bound applies to every field, and ADR 0011's 128-byte slot applies to
every field one party writes and another reads in its hot loop. Card 78's verdict
(`docs/explanation/should-zigbolt-inform-cavalry.md`) rejected the only other design
in view, ZigBolt's, point by point and in each case named Aeron's as the one to copy.

Aeron's log buffer, read on 2026-10-11 at commit 32e531c: a file of three term
partitions followed by a 4096-byte metadata section (`LogBufferDescriptor`,
`LogBuffers`); terms a power of two from 64 KiB to 1 GiB; a tail counter per
partition that packs the term id into the high 32 bits and the term offset into the
low 32; an active term count; an end-of-stream position; a connected flag; the
initial term id, MTU, term length, page size, windows and a default frame header; then
UDP, cluster and driver parameters. A publisher claims with one fetch-and-add on the
active tail, writes the header with the frame length negated in a release store, the
body, then the positive length in a release store; it aborts by writing the padding
type first; it rotates by compare-and-swapping the next tail and then the active term
count (`ConcurrentPublication`, `HeaderWriter`, `BufferClaim`). A subscriber reads
frames until a length of 0 or less and publishes its position with a release store
(`TermReader`, `Image`). The driver keeps the publisher limit at the minimum subscriber
position plus a window of at most half a term, zeroes consumed bytes with the first
eight bytes last, and unblocks a stale claim after a timeout (`IpcPublication`,
`TermUnblocker`, `LogBufferUnblocker`). Positions are counters in the CnC file,
written by their owners, the client included (`Image`, `SubscriberPos`).

Cavalry's counters file (ADR 0012) said the driver is its only writer, and anticipated
in its Revisit clause the day a client needs to own a counter. A subscriber must
publish its position per poll batch, and the driver must read it to raise the limit and
to clean, so that day is this feature.

## Decision

`spec/log-buffer.md` specifies the log buffer at version 0 as Aeron's: three term
buffers of one power-of-two length from 64 KiB to 1 GiB, then a 4096-byte metadata
page, in a file of exactly 3 × term length + 4096 bytes named by the publication's
correlation id; positions as term count × term length + term offset with the term count
in wrapping 32-bit arithmetic; a packed tail counter per partition advanced by
fetch-and-add (type 0, concurrent) or by release stores from one publisher (type 1,
exclusive); Aeron's claim protocol, negative length first, positive length last, padding
type to abort; Aeron's rotation by two compare-and-swaps, a padding frame to the end of
the term, and `admin action` to the caller; Aeron's fragmentation at MTU − 32 with the
longest message at the smaller of term length ÷ 8 and 16 MiB; Aeron's reader, which
stops at a length of 0 or less and skips padding; and Aeron's driver duties, the limit
at minimum subscriber position + window with the window at most half a term, cleaning
before the limit rises with the first eight bytes written last, unblocking of a stale
claim, and the end-of-stream position at close. Three things are Cavalry's. The
metadata page begins with the header convention of ADR 0012, magic `CVLYLOGB`, version
and type, written magic-last, with a reader that runs eleven named checks in order and a
table of writer, readers and bound for every field (ADR 0010). Every field written after
creation, the three tails, the active term count, the end-of-stream position and the
connected flag, has a 128-byte slot (ADR 0011), and the fields Aeron keeps for UDP,
cluster and driver parameters are reserved zero bytes until the slice that needs them.
And the publisher limit, publisher position and subscriber positions are instance
counters in the counters file, `publication.limit`, `publication.position` and
`subscription.position`, written by their owners: this ADR amends ADR 0012's one-writer
rule so that the driver remains the only writer of the counters file's header and
records while a value is written by the counter's owner, the driver or a client, and
amends its relaxed rule so that a position is stored with release and loaded with
acquire, which is what orders a subscriber's reads before the driver's cleaning. The
driver checks every client-written position and tail against its bound and counts a
failure in `log_buffer.rejected_position` or `log_buffer.rejected_tail`; a subscriber
reports a bad frame to its conductor, counted in `log_buffer.rejected_frame`; an unblock
counts in `publication.unblocked`. The vectors in `spec/vectors/log-buffer.json` come
from `spec/vectors/log-buffer.py` and cover the header, the tail packing, the position
arithmetic, the claim including rotation and the claim header's bytes before and after
commit and abort.

## Alternatives considered

- **ZigBolt's log buffer: no term id, two positions, a CAS loop on an absolute tail,
  zero meaning uncommitted, no abort.** Lost in card 78's verdict, for the reasons on
  its page: a subscriber cannot join late, a NAK cannot name a gap, and the driver
  cannot unblock a dead publisher.
- **The metadata page first, as the counters file.** Lost: with the metadata at the end
  every term begins at a multiple of its own length, so terms stay aligned to any page
  size, 16 KiB on Apple silicon included, and the file is laid out as Aeron's
  `.logbuffer`; a reader finds the page from the file's size, which comes from the
  kernel, not from a peer.
- **Aeron's page size field and a file length rounded up to it.** Lost: nothing in
  version 0 reads it; the metadata at the end makes the file length exact, which is
  one more check a reader can make; the process asks the OS for its page size when it
  pre-touches.
- **Aeron's three tail counters eight bytes apart.** Lost to ADR 0011: the active tail
  is written on every claim, and the fields beside it are read on every claim by every
  publisher; 128-byte slots cost nothing in a 4096-byte page.
- **Two partitions.** Lost: with the window at most half a term, three partitions
  guarantee that the partition rotated into has been consumed and cleaned; two would
  need the window below a quarter term or a cleaning race.
- **Subscriber positions in the metadata page.** Lost: subscribers would need write
  access to the publisher's file and could corrupt its frames; Aeron keeps positions
  out of the log for the same reason.
- **One position file per client.** Lost: a layout, a file and a mapping per client
  for what the counters file already is, and `doctor` would read every one to show a
  stream's positions. Bounds and eviction (ADR 0010) limit what a bad client can do with
  its counter.
- **Positions relayed to the driver through the command ring.** Lost: a message per
  poll batch on the data plane, and the driver's view would lag.
- **Keeping ADR 0012's one-writer rule and letting the driver write subscriber
  positions on the client's behalf.** Lost: the driver cannot know where a subscriber
  is without being told, which is the alternative above.
- **A larger MTU bound for IPC, since no datagram carries the frame.** Lost: IPC and
  UDP share one API and one frame, and a frame that fits a datagram today needs no
  change when the stream crosses a network.
- **Only the concurrent type, or only the exclusive type.** Lost: the layout is the
  same, the one-publisher case is the common fast case and Aeron ships both; the type
  field tells a reader and `doctor` which protocol advances the tail.
- **Specifying unblocking later, with the driver.** Lost: a negative frame length left
  by a dead publisher is a state of this layout, and a layout with an unspecified state
  is a bug in the spec.
- **Keeping Aeron's UDP, cluster and driver fields as named fields written 0.** Lost: a
  named field with no reader is a promise about a slice that is not designed; reserved
  bytes become fields with the version that gives them meaning.
- **A checksum over the metadata page.** Lost, as in ADR 0010 and ADR 0012.

## Evidence

- Aeron at commit 32e531c, read on 2026-10-11: `LogBufferDescriptor.java` (the
  constants, the static initializer that lays out the metadata section, `rotateLog`,
  `computePosition`, `termOffset`, `packTail`), `aeron_logbuffer_descriptor.h` (the
  same layout as a C struct with its padding), `ConcurrentPublication.java` (`claim`,
  `appendUnfragmentedMessage`, `appendFragmentedMessage`, `handleEndOfLog`),
  `ExclusivePublication.java` (the release-store tail), `HeaderWriter.java`,
  `BufferClaim.java` (`commit`, `abort`), `FrameDescriptor.java`
  (`computeMaxMessageLength`, `frameLengthVolatile`, `frameLengthOrdered`),
  `TermReader.java`, `Image.java` (`poll`, `validatePosition`), `Publication.java`
  (`backPressureStatus`, the five result codes), `LogBuffers.java` (the checks at
  mapping), `IpcPublication.java` (`updatePublisherPositionAndLimit`, `cleanBufferTo`,
  `checkForBlockedPublisher`, `joinPosition`), `TermUnblocker.java`,
  `LogBufferUnblocker.java`, `DriverConductor.java` (`initialiseLogBufferPosition`,
  the end-of-stream position written last at creation) and `Configuration.java`
  (`producerWindowLength`, the term, MTU and timeout defaults).
- `docs/explanation/should-zigbolt-inform-cavalry.md`, the rows for the log buffer
  layout, the claim and commit protocol and the behaviour when full.
- `spec/vectors/log-buffer.py` produced its vectors on 2026-10-11; the count is in
  `spec/vectors/log-buffer.json`.
- Nothing measured: `just bench` is the third commit of card 83, and ADRs 0010 and
  0011 take their evidence from it on this layout.

## Consequences

Easier: Aeron's documentation, tooling and twelve years of production explain Cavalry's
log; a publisher is one fetch-and-add and two release stores per message; the driver
reads one page to know where a stream is; every client language implements the same
vectors; positions appear in `doctor` and the counters page like every other counter.

Harder: the driver owns five duties per log on its duty cycle, limit, cleaning,
unblocking, connection and close, which feature 4 implements; a client can corrupt its
own position counter and must be checked and evicted; the longest message is a function
of the term length; a term length is fixed when the publication is created.

Revisit: when UDP arrives, with the receiver window, socket lengths and the image type
in a new version; when a client needs a counter the driver did not allocate for it;
when the exclusive publication needs a different unblocking rule; when a measurement
shows the 128-byte tail slots or the per-frame checks cost at p99.99 (ADRs 0010 and
0011's own Revisit clauses).

## References

- `spec/log-buffer.md`, `spec/vectors/log-buffer.py`, `docs/reference/log-buffer.md`,
  `spec/frame.md`, `spec/counters-file.md` (the amended sentences), `counters.toml`
- `CLAUDE.md`: invariants 1 to 4; Protocol; Data plane. `.claude/rules/data-plane.md`
- ADR 0007, ADR 0008, ADR 0010, ADR 0011, ADR 0012
- Aeron: <https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/logbuffer/LogBufferDescriptor.java>,
  <https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/ConcurrentPublication.java>,
  <https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/java/io/aeron/logbuffer/TermReader.java>,
  <https://github.com/aeron-io/aeron/blob/master/aeron-driver/src/main/java/io/aeron/driver/IpcPublication.java>,
  <https://github.com/aeron-io/aeron/blob/master/aeron-client/src/main/c/concurrent/aeron_logbuffer_descriptor.h>
- AppShapes Trello card 83, "Build the log buffer, the ring buffers and the bench"
