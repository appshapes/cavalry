# Should ZigBolt's design inform Cavalry's?

This page answers one question: should any part of ZigBolt's design inform
Cavalry's, and if so which part, in which crate, which `spec/` section and which
ADR. It was written for AppShapes Trello card 78 on 2026-10-11, against ZigBolt at
commit b7eeb6b (version 0.2.1, 2026-06-13) and Aeron at commit 32e531c on `master`
(release 1.53.3). Like [`why-cavalry.md`](why-cavalry.md), it names Aeron
throughout: the question is whether ZigBolt adds anything to the design Cavalry
already copies from Aeron, and that comparison cannot be made in one mention. That
is the one exception this page takes to the Aeron rule in `.claude/rules/docs.md`.

## Answer

No, with two exceptions. Both are on the shared-memory layout, and both are
adaptations rather than copies:

1. **Validate every field of a shared-memory layout that another process can
   write, before using it in any index or pointer, and fail with a counter and an
   error.** ZigBolt does the validation and carries the tests that show why it
   matters; it then reports a corrupt header as "no data", which Cavalry must not.
   Proposed as [ADR 0008](../decisions/0008-validate-peer-written-shared-memory.md),
   for `cavalry-shm` and the log buffer and counters file sections of `spec/`.
2. **Pad a field that one party writes and another party spins on to 128 bytes,
   not 64.** ZigBolt pads to 128 because Zig's standard library does; Aeron and
   Agrona pad their counters and ring-buffer positions to two cache lines;
   Cavalry's data-plane rule says 64. Proposed as
   [ADR 0009](../decisions/0009-pad-peer-shared-fields-to-128-bytes.md), for every
   shared layout in `cavalry-shm`.

Everything else is rejected, with the reason recorded below so that nobody asks
twice. Where ZigBolt is right, it is Aeron's design with fewer fields. Where it
differs from Aeron, the difference loses to an invariant. Nothing in this verdict
changes the data frame header, so card 80 proceeds as
[ADR 0007](../decisions/0007-build-ipc-pub-sub-first.md) planned it, and that ADR's
revisit clause does not fire. The comparison also produced requirements for
Cavalry's own benchmark harness and on-disk log format. They adopt nothing from
ZigBolt, so they need no ADR; they are listed under
[What this page changes](#what-this-page-changes).

### Verdicts

| Area | Verdict | Where in Cavalry | Why, in one line |
| --- | --- | --- | --- |
| Log buffer layout and header | Reject | `spec/` log buffer section, `cavalry-shm` | A subset of Aeron's layout: no term id, no session or stream id, two positions and nothing else in the metadata page. |
| SPSC and MPSC queues | Reject | `cavalry-shm` | A copy per message on read, a byte loop on write, and an MPSC that wedges forever when a producer dies; Agrona's ring buffers unblock. |
| Broadcast buffer | Reject | `cavalry-driver`, control plane only | A port of Agrona's lossy broadcast buffer. Loss with no per-subscriber position breaks invariant 1 on the data plane; Aeron uses it only for driver-to-client events, and so will Cavalry. |
| Claim and commit protocol | Reject | `spec/` log buffer section | A CAS loop on an absolute tail, zero meaning uncommitted, no abort. Aeron's fetch-and-add on a packed tail, negative length while claimed, and `abort` do more. |
| Frame padding and alignment | Reject | `spec/` data frame section | 8-byte alignment and a sign-flagged padding frame; Aeron aligns to 32 bytes and types its padding so the same frame goes on the wire. |
| Counter padding | **Adapt** | [ADR 0009](../decisions/0009-pad-peer-shared-fields-to-128-bytes.md), every shared layout | 128-byte slots for peer-shared fields. Aeron, Agrona, Zig and crossbeam agree; Cavalry's rule says 64. |
| Batching | Reject | `cavalry-client` | Nothing beyond Aeron's one position store per poll batch. |
| Behaviour when full | Reject | `spec/` log buffer section | A two-term window computed on the publisher's hot path from the reader's position. Aeron's publisher reads a limit the driver maintains. |
| Slow or dead reader | Reject | `cavalry-driver` | No liveness, no timeout, no eviction, no counter. Aeron's untethered subscriptions and liveness timeouts are the model. |
| Cross-process safety of the shared header | **Adapt** | [ADR 0008](../decisions/0008-validate-peer-written-shared-memory.md), `spec/`, `cavalry-shm` | Bounds-checked, re-checked after mapping, tested with forged positions; but a corrupt header reads as "no data" with no counter. |
| Wire framing and flyweights | Reject | `cavalry-proto` | The Aeron-layout flyweights are unused. The UDP path has its own 32-byte header with sequence numbers, a version equality check and no negotiation. |
| Codec choice | Reject | `cavalry-proto` | "Comptime SBE" has no schema compiler and no user in the tree. The packed-struct codec is what `zerocopy` derives give Rust. |
| Archive and replay | Reject | `cavalry-log` | Headerless, unversioned segment files holding a record that is not the wire frame; index, catalog and compression unwired; no determinism test. |
| Raft | Reject | `cavalry-raft` | An in-memory library with no timers, no transport, no snapshot install and no membership change, by its own README. |
| Flow control and congestion control | Reject | `cavalry-driver` | Min, max, tagged and AIMD exist as files nothing imports. The live path is a sender-side credit counter and heartbeats. |
| Client API across the C ABI | Reject | `cavalry-client`, `cavalry-ffi` | Offer copies twice, there is no claim and commit, IPC and UDP have different APIs, and back-pressure is one of three error codes at the C boundary. |
| Operator experience | Reject | `cavalry` | No CLI, no counters file, no doctor, no metrics; 31 counter names with zero call sites. |
| Benchmarks | Reject | `bench/` | Same-thread publish and poll reported as round trip, a wall clock, batch means, a zero-nanosecond codec result, no hardware named. The lessons are recorded below. |

## What ZigBolt is

Three things, kept apart.

**Claimed.** The site calls ZigBolt "A pure-Zig, zero-allocation, lock-free
messaging system for high-frequency trading" and lists "< 200ns IPC Latency" and
"100M+ msg/sec", with the line "Latency and throughput figures are design targets,
not measured marketing numbers". The README's feature list has shared-memory IPC,
SPSC and MPSC ring buffers, a broadcast buffer, "Aeron-compatible" flyweights, an
SBE codec, NAK reliability, AIMD congestion control, min, max and tagged flow
control, fragmentation, idle strategies, shared counters, an experimental Raft
cluster, a write-ahead log, a segment archive with per-record CRC32, and five
language bindings. Its own caveat, verbatim: "Performance numbers are design
targets, not measured results." (`README.md:72-73`).

**Implemented.** About 25,800 lines of Zig, including benchmarks, in 51 commits.
The first three commits, on 2026-03-14 at 01:49, 01:55 and 02:00, added 3,319,
1,506 and 2,025 lines: transport core, network layer, and archive plus Raft plus
sequencer, eleven minutes apart. Forty-three of the 51 commits carry a Claude
`Co-Authored-By` trailer. A second burst on 2026-06-10 and 2026-06-11 fixed what
the first had left; its commit messages say the FFI and bindings "were dead on
arrival because no library was ever produced" (commit 5616689), that the typed
subscriber "was dead on arrival" (commit 36731d5), and that 87 of the tests had
been "silently skipped" (commit 5616689). A pull request merged on 2026-06-13 took
line coverage to 100% of measurable lines with 203 inline exclusions. There has
been no commit since. The README counts 503 unit tests; CI runs them on Linux in
Debug and ReleaseFast builds, and not on macOS, where the README says they also
pass. Several listed features are files that nothing imports: the Aeron-layout
flyweights, the min, max and tagged flow control, the AIMD congestion control,
the sparse index, the catalog and the compressor (sources 5 and 6).

**Measured.** One `bench/results.json`, committed on 2026-03-14 and never updated,
with no hardware, operating system or date recorded. Its figures are
single-process, mostly single-thread, and measured with a wall clock that the code
itself documents as "NOT monotonic" (`src/platform/config.zig:33-39`):

| Benchmark | Message | p50 | p99 | p99.9 | max | Throughput |
| --- | --- | --- | --- | --- | --- | --- |
| SPSC ring, same thread | 64 B | 50 ns | 60 ns | 320 ns | 156,350 ns | 17.6 M/s |
| IPC channel, same thread | 64 B | 30 ns | 40 ns | 40 ns | 540 ns | 35.8 M/s |
| IPC channel, same thread | 1,024 B | 90 ns | 260 ns | 900 ns | 115,580 ns | 10.0 M/s |
| Log buffer, same thread | 32 B | 30 ns | 40 ns | 320 ns | 6,300 ns | 33.7 M/s |
| Codec encode | 32 B | 0 ns | 0 ns | 0 ns | 0 ns | 10,000,000 M/s |

The "ping-pong" benchmark publishes to and polls from one channel in one thread
(`bench/ping_pong.zig:62-63`), so no round trip between two processes has been
measured. Every latency row is a percentile of per-operation means over batches of
100 operations (`bench/run_all.zig:86-95`), so no single slow operation survives
into the tail. The codec row is a loop the compiler removed. The
histogram is a custom bucket array that reports a bucket's lower bound as the
percentile (`bench/hdr_histogram.zig:39`), not HdrHistogram. The README's 50 M/s
throughput target is not met by the one committed figure.

**Licence.** MIT, copyright 2026 suenot (`LICENSE`; `zigbolt-spec.md`, appendix
C). The site states no licence. Ideas are free to use; copying code into an
Apache-2.0 project would carry the MIT notice with it. This page copies no code and
proposes none.

## Method

- Read the site, the README, the specification, every source file in the areas
  the card names, the tests, the benchmark harness, the CI workflow and the git
  history. Four read-only sweeps of the source were cross-checked against the
  files read directly for the ring buffer.
- For every feature, separate what is claimed, what is implemented and what is
  measured. A file nothing imports is implemented but not shipped.
- Judge each idea against `CLAUDE.md` in invariant order: delivery, tail latency,
  boundedness, counters, defaults, understandable. An idea that trades a higher
  invariant for a lower one is out whatever it claims.
- Keep Aeron's names. ZigBolt's ring buffers call the write position `head` and
  the read position `tail`, the reverse of Agrona's, and its `msg_type_id` is
  Agrona's record type id; this page uses Aeron's terms, which are the
  [glossary's](../glossary.md).
- Cite everything. ZigBolt citations are `file:line` at commit b7eeb6b. Aeron
  citations are file names at commit 32e531c or wiki page names. Sources are
  numbered at the end.

## Ring buffer

The card's first scope. ZigBolt has two shared-memory-shaped layouts. The
in-process `LogBuffer` (`src/core/log_buffer.zig`) is a struct of three
[terms](../glossary.md) and two [positions](../glossary.md). The IPC channel
(`src/channel/ipc.zig`) is a separate implementation over a mapped file, and it is
the one a second process can attach to.

### Layout and header versioning

The IPC layout is a 4,096-byte metadata page followed by three terms
(`src/channel/ipc.zig:19-25`). The page holds a magic number, a version fixed at
1, the term length, the publisher's tail position and the subscriber's head
position, each position on its own cache line (`ipc.zig:28-43`). That is all. The
in-process `LogBuffer` has no header at all (`log_buffer.zig:21-27`).

Aeron's log metadata page is also 4,096 bytes, and it carries one packed tail
counter per partition (term id in the high 32 bits, term offset in the low 32),
the active term count, the end-of-stream position, the connected flag, the
transport count, the correlation id, the initial term id, the MTU, the term
length, the page size, the default frame header and the timeouts a late joiner
needs (`LogBufferDescriptor.java`, source 10). ZigBolt has no term id anywhere: a
position is a byte count since creation and the term index is that count divided
by the term length, modulo three (`ipc.zig:299-302`). Four things follow. A
subscriber cannot join late, because an [image](../glossary.md) is addressed by
initial term id and position. A [NAK](../glossary.md) cannot name what it is
missing by term id and term offset, which is how Aeron's receiver and sender agree
on a gap. There is no end-of-stream marker and no connected flag. There is no MTU
in the layout, so one message must fit one term and the IPC path has no
fragmentation (`ipc.zig:182-184`). The term length is bounded to a power of two
between 4 KiB and 1 GiB (`ipc.zig:50-58`); Aeron's bounds are 64 KiB to 1 GiB
(`LogBufferDescriptor.java`, `TERM_MIN_LENGTH`, `TERM_MAX_LENGTH`).

Verdict: reject. The log buffer section of `spec/` writes Aeron's layout, with
the versioned header that `.claude/rules/data-plane.md` already requires.

### SPSC, MPSC and broadcast

The SPSC ring (`src/core/spsc.zig`) writes its 8-byte header and payload one byte
at a time through a mask (`spsc.zig:83-99`), and its reader copies every payload
into a scratch buffer the size of the whole ring before it releases the slot
(`spsc.zig:41-44`, `131-150`), so the ring costs twice its capacity and a copy per
message. The MPSC ring (`src/core/mpsc.zig`) claims with a CAS loop, copies, then
release-stores the length; its reader copies out and zeroes the consumed region
(`mpsc.zig:70-152`, `212-225`). Its own header comment records the defect: a
producer that claims and dies "will be blocked forever at that slot", and adds
"Aeron has the same limitation" (`mpsc.zig:16-20`). It does not. Agrona's
`ManyToOneRingBuffer` has `unblock()`, which rewrites an abandoned claim as
padding, and a consumer heartbeat (source 12); Aeron's driver runs the equivalent
on a publication's log after `aeron.publication.unblock.timeout`, 15 s by default
(`IpcPublication.checkForBlockedPublisher`, `TermUnblocker`, sources 13 and 14).
Neither ring is cross-process: both are plain structs with no header.

The broadcast buffer (`src/core/broadcast.zig`) is a port of Agrona's
`BroadcastTransmitter` and `BroadcastReceiver`: a tail-intent counter, a tail
counter and a latest counter in a trailer, a receiver that detects it was lapped
and resynchronises to the latest record (`broadcast.zig:38-47`, `238-349`). Agrona
states the contract: "If a Receiver cannot keep up with the transmission stream
then loss will be experienced. Loss is not an error condition." (source 15). On
Cavalry's data plane that is a violation of invariant 1, because a lapped receiver
cannot say how many messages it lost and the stream cannot report their absence.
Aeron uses the broadcast buffer for driver-to-client events, where a lapped client
re-syncs by asking the driver; Cavalry's conductor path copies that use and nothing
more.

Verdict: reject all three. `cavalry-shm` ports Agrona's one-to-one and
many-to-one ring buffers with their unblock protocol and consumer heartbeat for
the client-to-driver command path, and Agrona's broadcast buffer for the
driver-to-client event path; the fan-out of market data is a
[subscription](../glossary.md) with one image and one position per publisher.

### Claim and commit

The in-process log buffer claims with a CAS loop on an absolute tail
(`log_buffer.zig:105-113`); the IPC channel has no claim at all, it copies the
caller's bytes into the term (`ipc.zig:232-234`) and advances the tail with a plain
release store (`ipc.zig:243`), which is why it is single-publisher
(`ipc.zig:77-79`). A frame length of zero means uncommitted, a negative length
means padding, and a positive length is a committed frame (`src/core/frame.zig:3-16`).
There is no abort. On the first claim in a fresh term the publisher zeroes the
whole term, 1 MiB by default, on its own thread (`log_buffer.zig:92-101`).

Aeron claims with one fetch-and-add on the packed tail counter
(`ConcurrentPublication.claim`, source 11), which is wait-free and serves any
number of publishers in any number of processes. The header is written before the
length, the length is published last with a release store, and `abort` turns the
claim into a padding frame by writing the PAD type and then the length
(`BufferClaim.java`, source 16); the reader stops at a length that is zero or
negative (`TermReader.java`, source 17). The driver, not the publisher, zeroes a
term once every subscriber has consumed it, and writes the header word of each
cleaned block last (`IpcPublication.cleanBufferTo`, source 13). ZigBolt's encoding
cannot tell "never written" from "claimed and abandoned", so the unblock protocol
above has nothing to act on; Aeron's negative length while claimed is what makes
`TermUnblocker` possible.

Verdict: reject. The log buffer section of `spec/` writes Aeron's protocol: a
packed tail per term, fetch-and-add on claim, negative length in flight, release
store to commit, PAD type to abort, cleaning by the driver.

### Padding and alignment

Frames in ZigBolt are 8-byte aligned (`src/platform/config.zig:22`,
`frame.zig:30`) and a padding frame is a negative length (`frame.zig:14`). Aeron
aligns every frame to 32 bytes (`FrameDescriptor.FRAME_ALIGNMENT`, source 18) and
types its padding frame `HDR_TYPE_PAD` with a positive length, so the frame in the
log is the frame on the wire and the unblocker can write one. Verdict: reject;
card 80's frame header keeps Aeron's alignment and type.

The positions are a different matter. ZigBolt aligns and pads each one to
`std.atomic.cache_line`, which Zig 0.15.1 defines as 128 bytes on x86-64 and
aarch64 (`ipc.zig:30-40`, `config.zig:4-6`, source 19). Agrona spaces the tail,
head, correlation and heartbeat counters of its ring buffers two cache lines apart
(`RingBufferDescriptor.java`, source 20) and gives every counter in the counters
file a slot of `CACHE_LINE_LENGTH * 2` "to avoid false sharing"
(`CountersReader.COUNTER_LENGTH`, source 21). Aeron's log metadata puts its hot
fields at offsets 0, 128 and 256 (source 10). Rust's `crossbeam-utils` pads to 128
bytes on x86-64, aarch64 and powerpc64 because "spatial prefetcher is pulling
pairs of 64-byte cache lines at a time" (source 22). Cavalry's data-plane rule says
64 bytes. Verdict: adapt, as [ADR 0009](../decisions/0009-pad-peer-shared-fields-to-128-bytes.md).
The idea is not ZigBolt's, but the comparison is what surfaced the gap.

### Batching

The IPC reader delivers up to `limit` frames and stores its position once after the
batch (`ipc.zig:299-346`); the in-process reader stores it per frame
(`log_buffer.zig:207-209`). Publishing is one frame per call with no batch form.
Aeron's `TermReader` stores the subscriber position once per batch in a `finally`
block (source 17), and its sender and receiver drain sockets in batches, which
ZigBolt's UDP layer does not (one `sendto` and one `recvfrom` per datagram,
`src/channel/udp.zig:138-174`). Verdict: reject; nothing to take.

### Behaviour when full

The IPC publisher refuses a frame that would put the tail more than two terms
ahead of the head, with `error.BackPressure` (`ipc.zig:200-205`); the in-process
buffer returns null on the same rule (`log_buffer.zig:63-68`, `85-90`). To decide,
every publish acquire-loads the subscriber's head position (`ipc.zig:187`), which
is the subscriber's hot cache line. Aeron's publisher never reads a subscriber
position. It reads one limit counter the driver maintains as the minimum
subscriber position plus the term window, half a term by default, and the driver
moves that limit in steps of one eighth of the window
(`IpcPublication.updatePublisherPositionAndLimit`, `Configuration.producerWindowLength`,
sources 13 and 23). [Back-pressure](../glossary.md) is the same result code in both
designs; the cost of deciding it is not. Verdict: reject; the spec writes Aeron's
publisher limit.

### Slow or dead reader

ZigBolt has no reader liveness. A reader that stops leaves the publisher
back-pressured forever; a reader that dies does the same; nothing counts either
(`ipc.zig` increments no counter, although `src/core/counters.zig:6-11` names
`ipc_backpressure_count`). Aeron times a client out after 10 s without a
heartbeat (`aeron.client.liveness.timeout`), an image after 10 s
(`aeron.image.liveness.timeout`), and moves an untethered subscription through
window-limit, linger and resting states with their own timeouts so that a slow
reader is dropped and later rejoined without stalling the publisher
(`IpcPublication.checkUntetheredSubscriptions`, sources 13 and 24). Verdict:
reject. `cavalry-driver` copies Aeron's mechanism and gives every transition a
counter, which `why-cavalry.md` already promises as a documented slow-consumer
eviction policy.

### Cross-process safety of the shared header

This is the one place ZigBolt does more than Aeron. When a subscriber opens a
channel it maps only the metadata page, checks the magic and the version, reads
the term length, bounds it to a power of two in range, checks with `fstat` that
the object is large enough before mapping the rest, then re-reads magic, version
and term length through the final mapping and fails if any changed
(`ipc.zig:124-166`, `src/platform/memory.zig:165-173`). The creator writes the
magic last with a release store (`ipc.zig:97-112`). On every publish and every
poll it validates the two positions before deriving a pointer from them: reader
not ahead of writer, span at most three terms, both frame-aligned
(`ipc.zig:61-75`, `186-192`, `290-296`); and it bounds a frame's length to the
bytes left in the term before handing out a slice (`ipc.zig:323-332`). The tests
forge a head near the end of a term with a huge length, flip the term length from
another thread during open, and set every combination of corrupt positions
(`ipc.zig:544-588`, `643-684`, `686-725`). The comment on the length check records
the bug the test caught: a subtraction that wrapped to about 4 GiB and let "a
forged frame_length hand the consumer a slice reaching ~2 GiB past the mapping"
(`ipc.zig:326-330`).

Aeron validates the term length and page size when it maps a log
(`LogBufferDescriptor.checkTermLength`, `checkPageSize`), caps an unpacked term
offset at the term length, and stops reading at a non-positive frame length
(sources 10 and 17). Beyond that it trusts its peers, which is reasonable for a
system whose driver and clients are deployed together by one team.

ZigBolt's flaw is what it does with a failure. A corrupt header makes `poll`
return zero frames, the same answer as an empty channel, with no error and no
counter (`ipc.zig:293-296`); only `publish` reports `error.CorruptChannel`
(`ipc.zig:191`). A reader that spins on "no data" forever, with nothing in the
[counters file](../glossary.md) to say why, breaks invariants 1 and 4 at once.

Verdict: adapt, as [ADR 0008](../decisions/0008-validate-peer-written-shared-memory.md).
Cavalry's driver outlives every client, maps files that any attached client can
write, and sells memory safety as a compliance feature (`why-cavalry.md`, Build).
Every peer-written field gets a bound in the spec, a check before use, a counter
on failure and an error the conductor acts on. The cost is a handful of integer
compares per claim and per poll, which `just bench` measures when the log buffer
lands.

## Wire framing and flyweights

ZigBolt ships flyweights whose layouts match Aeron's data, status message, NAK,
setup, RTT measurement and error frames, with the same type codes 0x00 to 0x07
(`src/protocol/flyweight.zig:15-24`, `155-253`). Nothing uses them: the only
importer is a re-export in `src/root.zig:148`. What goes on the wire is an 8-byte
record header over IPC (`frame.zig:11-27`) and, over UDP, a 32-byte
`NetworkHeader` of version, type, session id, stream id, a 64-bit sequence
number, payload length and reserved bytes (`src/channel/reliability.zig:11-35`),
plus a separate 16-byte fragment header (`src/channel/fragment.zig:32-66`). The
receiver drops any datagram whose version is not exactly 1
(`src/channel/network.zig:239-240`), the setup and teardown types are handled by
an empty arm (`network.zig:316`), and a repository-wide search for negotiation
finds comments only. A sequence number per datagram replaces term id and term
offset, so the UDP path and the IPC path address a message differently and
cannot share a log buffer, which is the property Aeron's design and ADR 0007
rest on.

Verdict: reject. `cavalry-proto` writes Aeron's data frame header, 32 bytes with
version, flags, type, term offset, session id, stream id, term id and reserved
value, 32-byte aligned (source 25), as card 80 specifies. Version negotiation is
Cavalry's own work, since neither ZigBolt nor the Aeron wiki page describes one
beyond "Current version is 0".

## Codec choice

`WireCodec(T)` checks at compile time that `T` is a packed struct with no pointers
and a size that is a multiple of 8, then casts a buffer to it
(`src/codec/wire.zig:19-117`). Its length guard is `unreachable`, which is
compiled out in release builds (`wire.zig:60`, `75`, `91`); a checked variant
tests the length and nothing else. In Rust that is what the `zerocopy` derives
provide, with the bounds check kept in release, and `cavalry-proto` already
requires them (`.claude/rules/data-plane.md`, Memory and layout). The "SBE codec"
has SBE's message and group headers and a streaming encoder and decoder, but no
schema, no XML, no code generation, a variable-length field whose length type is
fixed at `u32` where the standard makes it schema-defined, and no caller anywhere
in the tree (`src/codec/sbe.zig:26`, `39-62`, `113-159`, `567-584`). The FIX
messages are hand-written and do not check template id, schema id or version on
decode (`src/codec/fix_messages.zig`). The codec benchmark reports zero
nanoseconds per encode because the compiler removed the loop
(`bench/results.json`, rows `Codec-Enc` and `Codec-Dec`).

Verdict: reject. Cavalry's plan stands: real SBE from a schema for compatibility,
`zerocopy` for the native path, fuzzed parsers that reject with a counter.

## Archive and replay

A ZigBolt segment file has no file header and no version; records start at byte
zero (`src/archive/segment.zig:49-59`). A record is a 24-byte header of length,
CRC32, timestamp, stream id and message type id, then the payload
(`segment.zig:18-28`), which is not the frame that crossed the wire, so a replay
delivers a different object from what a subscriber saw. The CRC covers the whole
record. `fsync` is never per record; the default is once a second
(`src/archive/archive.zig:16-26`). A torn tail is detected on read and never
truncated, and a restart always opens a new segment (`segment.zig:118-121`,
`227-275`). Replay takes a segment id and a byte offset, has no timestamp form and
no rate control (`archive.zig:125-143`), and reports a corrupt record only by
leaving it out of the count. The sparse index, the catalog and the compressor are
complete data structures that the archive never calls (`archive.zig:2` imports
only the segment module; `archive.zig:9-12`). No test replays a log twice and
compares the result.

Aeron Archive records the frames of the log buffer as they are, in segment files
of 128 MiB by default made of whole terms, so a recording is addressed by position
like the stream it came from, and a replay is a publication of the same frames
(source 26). Its checksum is optional and chosen by name, CRC-32 or CRC-32C, for
recording and for replay separately (`Archive.java`, `Checksums.java`, sources 26
and 27). Cavalry's `cavalry-log` README already requires a versioned file header,
an accepted ADR for the format, and a `just replay-check` that replays twice and
diffs state.

Verdict: reject. The on-disk log format ADR, when it is written, takes Aeron's
shape: the record is the frame, the file has a versioned header, the checksum is
a named option. Two requirements from this comparison are listed under
[What this page changes](#what-this-page-changes).

## Raft

The README says it: "no built-in election timers or transport loop (liveness is
the embedder's responsibility), no log compaction / InstallSnapshot / membership
changes, and it has not been validated with real multi-process fault injection"
(`README.md:64-71`). The code agrees. The four Raft messages are in-process union
values that carry slices into the leader's memory, with no serialisation
(`src/cluster/raft.zig:48-53`, `72`); the timeout fields in the configuration are
copied and never read (`raft.zig:42-44`, `src/cluster/cluster.zig:46-48`). The
persistence discipline is sound, term and vote and log entries are durable before
a reply (`raft.zig:8-14`), and the write-ahead log is in native byte order
(`src/cluster/wal.zig:140-149`), so a log written on one architecture does not
read on another. Verdict: reject; `cavalry-raft` copies Aeron Cluster's shape, a
replicated log fed to a deterministic state machine, as `why-cavalry.md` says.

## Flow control and congestion control

Two things carry each name. The min, max and tagged strategies in
`src/channel/flow_control.zig:41-45` model Aeron's [status message](../glossary.md)
in an in-memory struct that nothing sends or parses; `network.zig` does not import
the file (`network.zig:4-8`). The AIMD controller with its RTT estimator in
`src/channel/congestion.zig` has no caller outside its tests. What the reliable
UDP channel runs is a sender-side credit counter of 4 MiB (`network.zig:22`,
`reliability.zig:367-411`) replenished by receiver heartbeats that carry the
receiver's contiguous position (`network.zig:515-532`). Retransmission is from a
ring of heap copies, one allocation per datagram sent (`reliability.zig:119`); a
gap is asked for at most ten times and then forgotten without an error or a
counter (`congestion.zig:281`); a receive window overrun rejects every later
datagram silently (`reliability.zig:225-233`); and delivery is in arrival order,
so a retransmitted gap arrives after the frames that followed it
(`network.zig:279-281`), which the file's own heading calls "ordered"
(`network.zig:53`). The six reliability counters are declared and never
incremented (`counters.zig:22-27`).

Aeron's [flow control](../glossary.md) is receiver-driven: status messages carry
term id, consumption term offset and receiver window; max is the default, min
and tagged are the alternatives, with a group minimum; the congestion control is
`StaticWindowCongestionControl` by default and `CubicCongestionControl` as the
option; NAKs retransmit from the sender's own term buffer with a 1 µs unicast
delay and a 200 ms status message timeout (sources 24 and 28). Verdict: reject.
`cavalry-driver` copies Aeron's three strategies, names included, and its two
congestion controllers.

## Client API across the C ABI

The typed publisher encodes into a stack buffer and the channel copies it again
into shared memory (`src/api/publisher.zig:29-33`, `ipc.zig:234`); there is no
claim and commit on the IPC channel, although the in-process log buffer has one.
The typed subscriber hands the handler a pointer into shared memory that is valid
for the duration of the call (`src/api/subscriber.zig:28-31`), with no context
argument, so handlers in the tests use file-scope statics. The transport knows
only IPC channels (`src/api/transport.zig:21-24`); the UDP channel has a different
publish and poll signature (`network.zig:162`, `206`), so IPC and UDP do not share
one API. The C ABI has ten exports over tagged, poisoned opaque handles
(`src/ffi/exports.zig:14-25`, `67-192`); back-pressure, a message too large and a
corrupt channel share the one return code `-3` (`exports.zig:136-138`), a poll on
a bad handle returns 0 like an empty channel (`exports.zig:154-155`), and there is
no error string and no generated header in the repository. The handle poisoning on
destroy is unconditional (`exports.zig:17-20`).

Verdict: reject. Cavalry's rule is claim and commit as the primary publish API,
in-place views on receive, IPC and UDP as one API, a C ABI generated by cbindgen,
and debug builds that poison released buffers; every error says what to do next.

## Operator experience

None. There is no command-line tool other than the benchmarks (`build.zig`), no
counters file, no doctor, no metrics export and no console. The counter set is an
in-process array of 64 cache-line-padded slots per subsystem whose comment says it
"Mirrors Aeron, which pads each counter to a cache line in its counters file"
(`counters.zig:96-115`); no code outside the file increments one, and nothing
outside the process can read one. The idle strategies exist and no channel uses
them; there is no thread pinning (`zigbolt-spec.md:638-640`). Configuration is
Zig struct literals with defaults and no file, environment or validation beyond
the term length. Verdict: reject; nothing to take.

## Benchmarks

Described under [What ZigBolt is](#what-zigbolt-is). The verdict is reject, and the
lessons are requirements for Cavalry's harness rather than adoptions:

- Two processes on two cores for a round trip, never one thread.
- `CLOCK_MONOTONIC` or the TSC, never the wall clock.
- HdrHistogram, with p50, p99, p99.9, p99.99 and max from the same run.
- Every measured call wrapped in `std::hint::black_box`, and a run whose p50 is
  zero fails rather than reports.
- One sample per operation, never a mean over a batch.
- The hardware, operating system, kernel, commit and date in every results file.
- Results published per release and never committed per run, which
  `.gitignore` already enforces for `bench/results/`.

## What this page changes

- `why-cavalry.md`, Build: the "isolate the unsafe" and "no locks on the hot
  path" rules now point at ADR 0008 and ADR 0009 and link this page.
- ADR 0007 is untouched: the frame header, card 80, is unchanged.
- The log buffer feature that follows card 78 writes Aeron's layout into `spec/`
  and adds, from this page: the validation table of ADR 0008, the 128-byte slots
  of ADR 0009 if accepted, an unblock protocol and consumer heartbeat for every
  many-to-one ring, and a counter for every slow-reader transition.
- The on-disk log format ADR, when written, requires: the record is the frame; a
  versioned file header; a named checksum option as Aeron offers; truncation of a
  torn tail on recovery; and a time-to-position index, because `cavalry replay`
  promises an answer for a time range and position alone cannot give one.
- The benchmark feature takes the list under [Benchmarks](#benchmarks) as
  requirements.

## Sources

1. [ZigBolt site](https://zigbolt.marketmaker.cc/): headline, figures, the
   "design targets" caveat, feature list, no licence statement. Read 2026-10-11.
2. [suenot/zigbolt on GitHub](https://github.com/suenot/zigbolt): commit b7eeb6b
   of 2026-06-13, `LICENSE` (MIT), `README.md`, `zigbolt-spec.md`, `CLAUDE.md`,
   `src/`, `bench/`, `.github/workflows/ci.yml`, git history. Cloned 2026-10-11.
3. ZigBolt `README.md:72-73`, `321-323`, `368-370`: the caveats, verbatim.
4. ZigBolt `src/core/log_buffer.zig`, `spsc.zig`, `mpsc.zig`, `broadcast.zig`,
   `frame.zig`; `src/channel/ipc.zig`; `src/platform/memory.zig`, `config.zig`:
   read in full.
5. ZigBolt `src/protocol/flyweight.zig`, `src/codec/wire.zig`, `sbe.zig`,
   `fix_messages.zig`, `src/channel/reliability.zig`, `network.zig`, `udp.zig`,
   `fragment.zig`, `flow_control.zig`, `congestion.zig`.
6. ZigBolt `src/archive/`, `src/cluster/`, `src/sequencer/`.
7. ZigBolt `src/api/`, `src/ffi/exports.zig`, `src/core/counters.zig`,
   `idle_strategy.zig`, `agent.zig`.
8. ZigBolt `bench/results.json` (commit bf7da49, 2026-03-14), `bench/ping_pong.zig`,
   `spsc_latency.zig`, `hdr_histogram.zig`, `run_all.zig`.
9. ZigBolt commits 1ca10fd, 1a82b3b, 75a40db (2026-03-14), 5616689 and 36731d5
   (2026-06-10), b7eeb6b (2026-06-13): dates, sizes, trailers and messages.
10. Aeron `aeron-client/src/main/java/io/aeron/logbuffer/LogBufferDescriptor.java`
    at 32e531c: partition count, metadata offsets, packed tail, term bounds,
    `checkTermLength`, `checkPageSize`, `termOffset`.
11. Aeron `aeron-client/src/main/java/io/aeron/ConcurrentPublication.java`:
    `claim` by `getAndAddLong`, `handleEndOfLog`, the position limit.
12. Agrona `agrona/src/main/java/org/agrona/concurrent/ringbuffer/ManyToOneRingBuffer.java`:
    `tryClaim`, `commit`, `abort`, `unblock`, consumer heartbeat.
13. Aeron `aeron-driver/src/main/java/io/aeron/driver/IpcPublication.java`:
    `cleanBufferTo`, `checkForBlockedPublisher`, `updatePublisherPositionAndLimit`,
    `checkUntetheredSubscriptions`.
14. Aeron `aeron-client/src/main/java/io/aeron/logbuffer/TermUnblocker.java`.
15. Agrona `agrona/src/main/java/org/agrona/concurrent/broadcast/BroadcastTransmitter.java`
    and `BroadcastReceiver.java`: the class comments and `validate`.
16. Aeron `aeron-client/src/main/java/io/aeron/logbuffer/BufferClaim.java`:
    `commit`, `abort`.
17. Aeron `aeron-client/src/main/java/io/aeron/logbuffer/TermReader.java`.
18. Aeron `aeron-client/src/main/java/io/aeron/logbuffer/FrameDescriptor.java`:
    `FRAME_ALIGNMENT`, `PADDING_FRAME_TYPE`, `computeMaxMessageLength`.
19. Zig 0.15.1 `lib/std/atomic.zig`: `cache_line` and `cacheLineForCpu`.
20. Agrona `agrona/src/main/java/org/agrona/concurrent/ringbuffer/RingBufferDescriptor.java`.
21. Agrona `agrona/src/main/java/org/agrona/concurrent/status/CountersReader.java`:
    `COUNTER_LENGTH`.
22. [`crossbeam_utils::CachePadded`](https://docs.rs/crossbeam-utils/latest/crossbeam_utils/struct.CachePadded.html),
    crate version 0.8.23.
23. Aeron `aeron-driver/src/main/java/io/aeron/driver/Configuration.java`:
    `producerWindowLength`, `PUBLICATION_UNBLOCK_TIMEOUT_DEFAULT_NS`,
    `CLIENT_LIVENESS_TIMEOUT_DEFAULT_NS`, term buffer defaults.
24. [Aeron wiki, Configuration Options](https://github.com/aeron-io/aeron/wiki/Configuration-Options):
    liveness and untethered timeouts, NAK and status message settings, congestion
    control supplier.
25. [Aeron wiki, Transport Protocol Specification](https://github.com/aeron-io/aeron/wiki/Transport-Protocol-Specification):
    data frame header, frame types, 32-byte alignment, padding frame, term rules.
26. Aeron `aeron-archive/src/main/java/io/aeron/archive/Archive.java`:
    `aeron.archive.record.checksum`, `aeron.archive.replay.checksum`,
    `aeron.archive.segment.file.length`, `aeron.archive.file.sync.level`.
27. Aeron `aeron-archive/src/main/java/io/aeron/archive/checksum/Checksums.java`:
    CRC-32 and CRC-32C.
28. [Aeron wiki, Flow and Congestion Control](https://github.com/aeron-io/aeron/wiki/Flow-and-Congestion-Control).

Aeron files were read from `master` at commit 32e531c on 2026-10-11; the release
current that day was 1.53.3. Figures quoted from ZigBolt are its own, from one
uncontrolled run, and have not been reproduced.
