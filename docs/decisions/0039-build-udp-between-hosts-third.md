# 0039. Build UDP unicast between hosts third, in four features

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 1, 2, 3, 4, 5, 6
- **Supersedes:** none; ADR 0023's Revisit named UDP as the third slice, and this ADR
  is the one it asked for

## Context

The second slice is done. ADR 0023's Revisit clause fired on 2026-10-31: card 95 closed
with its three changes on `master` (1d880ea, e0c3b6e, 229d422), `just quickstart`
records and replays on both runners (`.github/workflows/ci.yml`; `master`'s run
41218568485 is green), and the second dedicated host passes `bench/host/check.sh`: it is
`grip` since ADR 0034, Accepted the same day at 30e29c7, with `bench (grip)` a required
check of `master`. ADR 0007 named UDP the second slice and ADR 0023 moved it to third
for two reasons, one gone and one honoured: no host existed to gate it on, and invariant
1's gate, `just replay-check`, came before everything else. Every recipe in the
`justfile` is real now; nothing prints `not built yet`.

The shape of UDP is already promised, so this ADR decides its order and scope, not its
design. `CLAUDE.md`: IPC and UDP share one API; the channel URI is
`cavalry:udp?endpoint=host:port`; Aeron's names are kept for image, term, position,
NAK, status message and flow control (max, min, tagged); the data plane includes the
driver's sender and receiver threads, dedicated and pinned when the OS allows. The
driver README: a receiver that detects a gap sends a NAK, the sender retransmits from
its own term buffer, and status messages drive flow control. The proto README: the
frames that carry fragments, NAKs and status messages, and version negotiation between
peers. The client README: `cavalry:udp?endpoint=host:port` and `cavalry:ipc` on one
API, back-pressure returned when the receiver window is full. The specifications left
the room: `spec/frame.md` specifies types 0 and 1 and says other types are specified
with their frames; `spec/log-buffer.md` keeps 56 reserved header bytes, and ADR 0013's
Revisit names the receiver window, the socket lengths and the image type as its version
1; `spec/client-protocol.md` says a channel is exactly `cavalry:ipc` "in this version",
leaves the channel status indicator id out of `publication ready` and `subscription
ready` "since IPC has no endpoint and the UDP version adds it" (ADR 0019), carries a
source identity because "an image names where it came from under both transports", and
reserves Aeron's error codes 4 and 9. The glossary's rows for NAK, status message, flow
control and fragment point at reference pages that do not exist yet.

What Aeron's transport does, read on 2026-10-31 from its wiki (the Transport Protocol
Specification, last edited 2025-03-12; Flow and Congestion Control; Channel
Configuration; Configuration Options): every frame begins with the data frame header's
first eight bytes, length, version, flags and type, with the types PAD 0, DATA 1, NAK 2,
SM 3, ERR 4, SETUP 5, RTTM 6 and RES 7. A sender sends a setup frame (term offset,
session id, stream id, initial term id, active term id, term length, MTU, TTL) until a
status message comes back, then data frames from its log buffer, packed whole into
datagrams of at most the MTU. A receiver answers with status messages (session id,
stream id, consumption term id and offset, receiver window, receiver id, group tag,
flags SETUP and EOS) on a new term, after a quarter of the window, or 200 ms after the
last one, and the sender sends no further than the consumption position plus the
window: "Aeron senders are dumb in that they only send as much as the receivers will
allow on each Stream at any one time". A receiver that sees a gap sends a NAK (session
id, stream id, term id, term offset, length) "immediately (in the case of unicast) or
after some delay (in the case of multicast)"; the sender retransmits from its own term
and ignores NAKs for the same frame for a time after. Heartbeats are data frames of
length 0 carrying the highest term id and offset sent, in the absence of data, with EOS
once the publication is closed, "particularly important for detecting tail loss". An
RTT measurement carries an echo timestamp, a reception delta and a receiver id, with a
reply flag. Over shared memory "there is no need for control messages": the log is the
protocol, which is what the first slice built. Its defaults: MTU 1408, receiver window
128 KiB, a receive socket buffer of 128 KiB and the operating system's own send buffer,
term 16 MiB over UDP against 64 MiB over IPC, status message timeout 200 ms, unicast
NAK delay 1 µs with a retry ratio of 100, connection timeout 5 s, image liveness 10 s,
linger 5 s. Its flow control: one receiver's window over unicast, and
max, min and tagged for a group of receivers over multicast or multi-destination cast;
congestion control is a static window unless Cubic is asked for.

The hosts. `bench` and `grip` are the gate's two hosts, tuned, each with four isolated
cores online and three of them pinned today, and each reaches the network over Wi-Fi
alone. The direct cable with hardware timestamps and the chrony link across it, which
bound two hosts' clocks to within a microsecond (`docs/how-to/set-up-a-bench-host.md`,
"Further hosts": 186 ns standard deviation and a last offset of 1 ns), still join
`bench` and `kip`, and `kip` is the soak host now; ADR 0034 says the cable moves to
`grip`'s wired port `eno2`, an Intel I219-LM, when the UDP rows come, and the link
steps are run again there. That is a hand on the machines, which is Rjae's. A one-way
number across two hosts carries the offset between their clocks; a round trip is
measured on one.

## Decision

The third slice is publish/subscribe over UDP unicast between two hosts, on the same
API as IPC: a publication on `cavalry:udp?endpoint=<host>:<port>` whose log buffer the
driver's sender thread puts on the wire, a subscription on the same channel whose image
the driver's receiver thread rebuilds from the wire, Aeron's control frames field for
field, Aeron's rules for setup, status messages, NAKs, heartbeats and flow control, and
Aeron's defaults, inside the one driver process (`CLAUDE.md`: one daemon per host). Four
features make the slice, one Trello card each, in this order:

1. **The transport, specified first (card 179).** `spec/udp.md` version 0: the datagram,
   the setup frame, the status message, the NAK, the heartbeat and the RTT measurement
   with their vectors from a Python reference encoder; the sender's and the receiver's
   rules and timeouts, Aeron's values; and the channel URI grammar,
   `cavalry:udp?endpoint=<host>:<port>` with `interface=<address>` as its one option,
   Aeron's name for the local address a host with two paths needs. `spec/log-buffer.md`
   version 1: the image type, type 2, which the receiver fills by term offset and
   subscribers read as any log, and the receiver window length and the socket buffer
   lengths in the reserved bytes, as ADR 0013's Revisit says. `spec/client-protocol.md`
   version 4: the `cavalry:udp` channel accepted by the five commands that carry a
   channel, the channel status indicator id in `publication ready`, `exclusive
   publication ready` and `subscription ready`, the source identity of an image over
   UDP as the sender's `<host>:<port>`, and error codes 4 (channel endpoint error) and 9
   (unknown host). Its ADR lands Proposed, and Rjae accepts it, because a wire format
   cannot be undone (`CLAUDE.md`, When in doubt). `cavalry-proto` gains the four control
   frames and the channel URI parser, each with the spec's checks in the spec's order, a
   no-panic test and a fuzz target; `cavalry-shm` reads and writes log buffer version
   1. Nothing sends a datagram yet. It builds the conformance vectors for three pages.
2. **The sender (card 180).** The driver's fourth dedicated thread, with the idle
   strategy and a name in the `start_with_threads` hook so that the bench pins it, and a
   send channel endpoint per endpoint address: setup frames until a status message,
   data frames from the log in bursts of whole frames up to the sender limit, the
   consumption position plus the receiver window of the last status message (the
   unicast flow control), heartbeats when there is nothing to send and with EOS after
   close, retransmits from the log's own term on a NAK, one `sendmmsg` per burst on
   Linux and one `sendmsg` per datagram elsewhere, so that a message never triggers its
   own syscall. The conductor keeps the publisher's limit at the sender position plus
   the publication window, keeps the publication connected while status messages
   arrive within the connection timeout, and lingers it after close so NAKs can still be
   answered. `std` has no `sendmmsg`, `recvmmsg` or socket buffer sizing and `unsafe` is
   denied in `cavalry-driver`, so the socket calls come through a dependency that
   carries their `unsafe`, as the bench's affinity crate does: named and justified in
   the feature's ADR and approved by Rjae before it exists. It builds the sender's
   counters and its connection state machine in `counters.toml`, the allocation
   assertion over the send path, and the `doctor` checks for a publication that never
   connected and for a send buffer the host capped. Its tests run against a
   hand-written receiver socket, as the driver's tests ran against a hand-written
   client before `cavalry-client` existed (ADR 0021).
3. **The receiver (card 181).** The fifth dedicated thread and a receive channel
   endpoint per endpoint, drained in batches: on a setup frame for a stream a
   subscription wants, the conductor creates an image, a log buffer of type 2 that the
   receiver fills by term offset with the completed position and the high-water mark as
   counters, and announces it with `available image` and the sender's address; the
   receiver sends status messages on the three triggers, a NAK for a gap after the
   unicast delay and not again within the retry delay, answers RTT measurements, and
   retires an image with no frame within the liveness timeout or ended by EOS with
   `unavailable image`; the subscriber's `subscription.position` drives the status
   message's consumption position, and a recording of a `cavalry:udp` stream on the
   receiving host records the image's log, as Aeron Archive records an image. It
   builds the fault-injection gate of `CLAUDE.md`'s gate 3 for the wire: a loopback shim
   between sender and receiver in the tests that drops, reorders and duplicates
   datagrams, with every message delivered once and in order through it on Linux and
   macOS, the receiver's counters and image state machine, the allocation assertion
   over the receive path, and the `doctor` checks for loss on an image and a receive
   buffer the host capped.
4. **The rows, the gate and the ten-minute path (card 182).** The harness's UDP rows
   over the loopback in one process, `claim_commit_32_udp` and `one_way_32_udp`, on
   each gate host with the publisher, the sender, the receiver and the subscriber on
   its four isolated cores and the conductor on the housekeeping CPU, as the recorder
   runs for the recorded rows, gated against each host's baselines; across the direct
   link, `udp_round_trip_32` measured on one clock with the harness on `bench` and a
   peer mode of the harness on `grip`, as two jobs of one workflow run that meet over
   the link after the single-host bench jobs, gated, and `udp_one_way_32` reported
   beside it with the offset `chronyc tracking` gives, never gated. The README's
   ten-minute path gains a UDP step over `127.0.0.1` on the same API, under `just
   quickstart` on both runners, which is the slice's acceptance test; a how-to for a
   publisher on one host and a subscriber on another; the `doctor` checks for an
   interface MTU below the stream's; and the README's status paragraph.

Out of the slice: multicast, multi-destination cast and the min and tagged flow control
strategies, which come together, since with one receiver per endpoint the three
strategies compute the same limit; congestion control beyond the static window; the
`mtu`, `term-length`, `ttl`, `fc`, `gtag`, `linger` and `session-id` channel parameters,
each an Aeron name that returns with the feature that needs it; error frames and name
resolution, types 4 and 7; spies on a local sender's log; kernel bypass; encryption;
the C ABI and the language clients; Raft. Three decisions are left to the feature ADRs
that must carry their evidence: the socket dependency and what it carries (card 180),
the image's rebuild and how its log is cleaned (card 181), and whether the cross-host
round trip is held to `bench`'s baseline alone or to both hosts' (card 182). Until a
feature's commit has a UDP row to quote, a commit under a data-plane crate says what
the existing rows show.

## Alternatives considered

- **Raft third.** Lost: replication between hosts needs a transport between hosts, and
  Raft sits on a stream and on the durable log, as ADR 0007 and ADR 0023 said; nothing
  of it can be gated before a frame has crossed a wire.
- **The C ABI and the language clients third.** Lost: the ABI cannot be undone
  (`CLAUDE.md`, When in doubt), and UDP changes the client API it would wrap, with the
  channel status and the source identity; fixing it before UDP fixes it twice.
- **Multicast in the slice.** Lost: the lab has one cable and no switch, cloud VPCs
  have no multicast (`docs/explanation/why-cavalry.md`), and the setup frame's TTL and
  the status message's group tag are carried as Aeron's from version 0, so multicast
  adds no wire change later. Multi-destination cast is lost with it.
- **All three flow control strategies in the slice.** Lost: `CLAUDE.md` names them and
  the glossary maps them, but with one receiver per endpoint max, min and tagged
  compute one limit, and a strategy nothing can exercise is a knob; they come with the
  feature that gives a publication several receivers, under their Aeron names.
- **TCP, or a stream protocol, instead of UDP.** Lost: `CLAUDE.md` names UDP, and a
  stream's head-of-line blocking and kernel buffering are what receiver-driven loss
  recovery exists to avoid; the frames are positions in a log on both ends.
- **A protocol of Cavalry's own: a sequence number per datagram, as ZigBolt's.** Lost
  in card 78's verdict and for ADR 0008's reason: a NAK that names a term and an offset
  asks for bytes the sender's log still holds, and the image a receiver rebuilds is the
  layout a subscriber already reads, so UDP adds no second path to the client.
- **Kernel bypass, `AF_XDP` or `io_uring`, in the slice.** Lost: the socket path is
  measured first, and the rows say where the tail is before anything bypasses it; a
  bypass is a later slice with its own ADR and its own host tuning.
- **Sender and receiver in one feature.** Lost: each is the size of the recorder
  (ADR 0025), and the sender is testable against a socket in the tests, as the driver
  was against a hand-written client.
- **The receiver before the sender.** Lost: a receiver has nothing to receive, while a
  sender's frames can be checked against the vectors by a test socket.
- **The frames specified with the sender.** Lost: the format is the irreversible
  decision and waits on Rjae, as ADR 0023 held the on-disk format apart from the
  recorder; three pages change versions at once and are vectored together.
- **A page per frame type.** Lost: Aeron's transport is one page, and the frame header
  page stays at version 0, since other types are "specified with their frames".
- **The one-way cross-host number as the gate.** Lost for the gate: it carries the
  offset between two clocks, bounded by the link but not zero, and a gate on it fails
  on the clock; the round trip is one clock's, and the one-way is reported beside it
  with the offset chrony measured.
- **Loopback rows only, no cross-host row.** Lost: the slice's promise is between
  hosts; the loopback rows catch a syscall per message, the cross-host row catches the
  wire, the interrupt path and the second host.
- **Configuration keys for the socket buffers, the MTU and the receiver window.** Lost:
  zero configuration is the primary supported configuration; Aeron's defaults and a
  `doctor` check that reads the host's limits and the interface's MTU; a key returns
  with a `why_not_default` when a host shows the need.
- **Encryption in the slice.** Lost: the plain path is measured first, and a cipher on
  the wire is a format decision of its own; `why-cavalry.md` promises it in the open
  core, and it has its own slice.
- **Deciding the slice in chat.** Lost: a decision that is not an ADR does not exist
  (ADR 0001), and this one orders four cards and a hand on two machines.

## Evidence

None measured: a scope decision. What informed it: ADR 0023's Revisit clause and the
state of `master` at dd16167 on 2026-10-31, every gate real; the promises in `CLAUDE.md`,
the crate READMEs and the glossary; the room the three specifications left and the
Revisit clauses of ADRs 0013, 0019 and 0021; Aeron's wiki pages named in Context, read
on 2026-10-31, with their inconsistencies noted for the feature ADR to resolve against
the source (the status message's drawn length against its stated one, the receiver id
of an RTT measurement); the hosts' state in ADR 0034 and the link's record on the
set-up page; and cards 179 to 183 on the Cavalry list with their briefs and "Done
when" sections.

Acceptance, 2026-10-31 *(acceptance)*: Rjae accepted the decision as proposed on
2026-10-31, answering the recorded decision d-20261031-d718fc through the team's Slack
gateway after the proposal merged as e545119 (pull request 86, reviewed and approved at
5c5a549). Nothing in the Decision changed; the four feature cards stand as written, and
the hand on the hosts the Consequences ask for stays Rjae's, before card 182. Card 179
is the next task.

## Consequences

Easier: the product's second transport on the API users already have, so a stranger
reads what is next from cards 179 to 182 and this page; every control frame and rule is
Aeron's, so its documentation explains Cavalry's wire; the frame header, the log buffer
and the client protocol gain their UDP versions in one feature, vectored together.

Harder: the driver gains two threads on the data plane, five in all, and the bench
hosts' four isolated cores are all taken by the loopback rows; three specification
pages move versions at once, and a version 3 client closes on the first `publication
ready` a version 4 driver sends in the longer encoding, so an upgrade restarts the
clients before the driver, as the client protocol page says of every version; the socket dependency is the first data-plane
dependency since `memmap2` and needs its ADR; the cross-host gate is a pair of jobs
that must meet, and a stall on either host holds `master`, as a stall on one does
today.

What the UDP rows need from the hosts, and when. Cards 179 to 181 need nothing: their
tests run over the loopback on the shared runners, and the two bench jobs keep running
the rows they have. Card 182's loopback rows need the fourth isolated core each host
already has online, `bench`'s CPU 14 and `grip`'s CPU 7. Card 182's cross-host rows
need, before the card starts and not before: the cable moved from `kip`'s `enp87s0` to
`grip`'s `eno2`; the netplan drop-in on `grip` with `10.0.0.2/30` on that port; the
chrony follower on `grip` with hardware timestamps on `eno2`, `bench` still serving
`10.0.0.2`; `grip-link` in both hosts' `/etc/hosts` in place of `kip-link`; a UDP port
open on the link in each host's firewall where one is enabled; and the link's record,
`ping -c 100` each way, the MTU, `ethtool -T` on both ports and `chronyc sourcestats`,
on card 182. These are the set-up page's link steps run again, and they are Rjae's
hand: this ADR asks for them rather than planning around them, and card 182's commit
updates the page with `grip`'s port and numbers. The required checks of `master` gain
the cross-host job when its baseline exists, by hand, as `bench (grip)` was added.

Revisit: when card 182 closes, with the cross-host row in the gate and `just
quickstart` publishing over UDP on both runners, with a new ADR for the fourth slice,
Raft or the C ABI and the language clients; when a user or the lab's switch needs
multicast or multi-destination cast, for the min and tagged strategies and the
`fc` and `gtag` parameters; when the cross-host rows' tail is the interrupt path, the
NIC's interrupt on a housekeeping CPU and the receiver on an isolated one, for
`SO_BUSY_POLL` or an interrupt bound to the receiver's core, which is a host change
under `bench/host/`; when a user needs jumbo frames or a term of another length, for
the `mtu` and `term-length` parameters; when a wide-area link appears, for congestion
control; when a bypass is measured against the socket path.

## References

- `CLAUDE.md`: invariants 1 to 6; Gates; Data plane; Protocol; When in doubt
- ADR 0007, ADR 0008, ADR 0013, ADR 0016, ADR 0019, ADR 0021, ADR 0023, ADR 0025,
  ADR 0034
- `spec/frame.md`, `spec/log-buffer.md`, `spec/client-protocol.md`; `crates/cavalry-proto/README.md`,
  `crates/cavalry-driver/README.md`, `crates/cavalry-client/README.md`;
  `docs/glossary.md`; `docs/explanation/why-cavalry.md`;
  `docs/how-to/set-up-a-bench-host.md`, "Further hosts"; `docs/how-to/implement-a-feature.md`
- Aeron wiki, read 2026-10-31: Transport Protocol Specification
  (<https://github.com/aeron-io/aeron/wiki/Transport-Protocol-Specification>), Flow
  and Congestion Control
  (<https://github.com/aeron-io/aeron/wiki/Flow-and-Congestion-Control>), Channel
  Configuration (<https://github.com/aeron-io/aeron/wiki/Channel-Configuration>),
  Configuration Options (<https://github.com/aeron-io/aeron/wiki/Configuration-Options>)
- AppShapes Trello cards 179, 180, 181, 182 and 183
