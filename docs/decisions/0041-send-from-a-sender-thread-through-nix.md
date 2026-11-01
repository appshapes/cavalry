# 0041. Send a UDP publication from a dedicated sender thread through nix's socket calls

- **Status:** Accepted
- **Date:** 2026-10-31
- **Invariants affected:** 2, 3, 4
- **Supersedes:** none

## Context

ADR 0039 orders the UDP slice's second feature, the sender (card 180): the driver's
fourth dedicated thread puts a `cavalry:udp` publication's log on the wire by the rules
of `spec/udp.md`, "The sender", which ADR 0040 accepted: setup frames until the first
status message without the send-setup flag, data in bursts of whole frames up to the
sender limit, heartbeats, retransmits on a NAK, an RTT reply, and the status messages,
NAKs and RTT measurements that arrive on the send endpoint's own socket. ADR 0039
decided the thread itself, the driver's fourth dedicated thread with the idle strategy
and the bench's pin hook, and left one decision to this ADR with its evidence: the
socket dependency and what it carries. What this ADR adds about the thread is how it
is linked to the conductor and what each side owns.

The constraints are these. `std::net::UdpSocket` opens, binds, connects and sets a
socket non-blocking, and sends one datagram per call; it has no `sendmmsg` or
`recvmmsg`, no vectored send on a connected socket, and no way to set or read the socket
buffer lengths, which `spec/udp.md`, "Values" asks for (131072 bytes asked for the
receive buffer, the socket's answer in `channel.socket_rcvbuf`; the send buffer left
the operating system's, what it holds in `channel.socket_sndbuf`). ADR 0039 fixed one
`sendmmsg` per burst on Linux and one `sendmsg` per datagram elsewhere, so that a
message never triggers its own syscall (`CLAUDE.md`, invariant 2: no syscall per
message on the data plane; a burst of up to 16 datagrams is one call). `unsafe` is
denied outside `cavalry-shm` and `cavalry-ffi` (`CLAUDE.md`, Unsafe), so the driver
cannot call `libc` itself, and a dependency of a data-plane crate needs this ADR, an
`allowed-dependencies` entry and a justification in the commit message (ADR 0001, ADR
0003), and Rjae's approval before it exists (`CLAUDE.md`, Working rules for agents).
Aeron's C driver makes the same split
(`aeron_udp_channel_transport.c`): `sendmmsg` under `HAVE_SENDMMSG`, `sendmsg_x` under
`HAVE_SENDMSG_X`, a per-datagram loop otherwise; `recvmmsg` under `HAVE_RECVMMSG` when
more than one datagram is asked for; `SO_SNDBUF` and `SO_RCVBUF` set in
`aeron_udp_channel_transport_init` and `SO_RCVBUF` read back; and `EAGAIN`,
`EWOULDBLOCK`, `ECONNREFUSED` and `EINTR` on a batched send or receive returned as
nothing sent, not as an error, which is the rule `spec/udp.md` states for a live
socket.

The driver already runs three dedicated threads, the conductor, the recorder and the
replayer (`crates/cavalry-driver/src/lib.rs`, `DriverThread`), each started by
`Driver::start_with_threads` with the driver's idle strategy and a hook the bench uses
to pin it, and each linked to the conductor by a pair of bounded SPSC queues with their
capacities in `spec.rs` (ADR 0025, ADR 0030). The sender is the first thread on the
live message path besides the conductor: a message a publisher commits is read by the
sender from the log and put in a datagram, so the per-message checklist of
`.claude/rules/data-plane.md` applies to its duty cycle in full.

## Decision

`cavalry-driver` depends on `nix` 0.31 with `default-features = false` and the
features `socket`, `uio` and `net`, listed in its `allowed-dependencies`, each named
for the calls it gates in `nix`'s source (Evidence has the lines). `socket` gates the
`sys::socket` module itself: `setsockopt` and `getsockopt` with `sockopt::RcvBuf` and
`sockopt::SndBuf`, `MsgFlags`, and the `SockaddrLike` trait with its implementation
for `()`, the address a connected socket passes. `uio` gates the vectored and batched
calls, `sendmsg`, `recvmsg`, `sendmmsg`, `recvmmsg` and `MultiHeaders`: every call on
the sender's duty cycle. `net` gates `SockaddrIn` and `SockaddrIn6`, the readers of an
IPv4 or IPv6 address out of a `SockaddrStorage`, and the conversions to and from
`std::net`: the sender needs none of it, since its socket is connected and every send
and receive passes `()` for the address, and the receiver (card 181) needs it for the
source address `recvmmsg` fills in for every datagram, which is the image's source
identity and where the receiver's status messages and NAKs go (`spec/udp.md`, "The
receiver"); it is on from this ADR so that the receiver takes the dependency as
Consequences says, with no second ADR. A send endpoint's socket is a
`std::net::UdpSocket`, opened, bound to the interface when one is named, connected to
the endpoint and set non-blocking by `std` on the control plane as the command is
handled; `nix` is called on its descriptor for what `std` lacks:
`sendmmsg` for a burst on Linux and `sendmsg` per datagram elsewhere, with no control
messages, so that neither allocates, through one `MultiHeaders` the endpoint
preallocates for the burst's 16 datagrams when it opens; `recvmmsg` on Linux and
`recvmsg` elsewhere for the status messages, NAKs and RTT measurements the endpoint
receives, into buffers the endpoint owns; `setsockopt` for `RcvBuf` alone, set to the
131072 bytes of `spec/udp.md`, "Values", when the socket is opened; and `getsockopt`
for `RcvBuf` and `SndBuf`, read once after that into `channel.socket_rcvbuf` and
`channel.socket_sndbuf`. The send buffer stays the operating system's, as "Values"
and "The sender" say, so nothing sets it: `channel.socket_sndbuf` records what the
host gave, which the `doctor` check for a send buffer the host capped (ADR 0039)
reads. The crate stays `unsafe_code = "deny"`: the `unsafe` that makes these calls
sound is inside `nix`, whose bindings are reviewed under its own policy, as
`zerocopy`'s and `memmap2`'s are (ADR 0009, ADR 0013). The sender is the
driver's fourth dedicated thread, `DriverThread::Sender`, with the driver's idle
strategy and a name in the `start_with_threads` hook so that the bench pins it; it is
linked to the conductor as the recorder is, by two bounded SPSC queues with capacities
in `spec.rs` (a request for every publication and endpoint the conductor adds or
removes, a reply for every state the sender alone can see: connected, disconnected,
the linger's end), each with the recorder's rule when it is full: an `add
publication` whose request finds the request queue full, or finds a remove still
waiting for room, is error code 10 to the client and registers nothing, as a `start
recording` is (`spec/client-protocol.md`, "Commands"); a remove the queue has no room
for waits in a bounded list in the conductor, one per registered publication and
endpoint, offered again each cycle in order; and a reply the reply queue has no room
for is held by the sender in a list with a bound of its own in `spec.rs`, derived as
the recorder's `PENDING_REPLIES` is from how many replies can be owed at once, and at
that bound the sender handles no request until the conductor takes replies again, and
keeps sending; it owns the send endpoints' sockets and each publication's send
state (the sender position, the sender limit, the setup and heartbeat clocks, the
outstanding retransmit), reads the log through the `TermReader` the conductor hands it
in the request, as the recorder's `Start` carries one, and moves `sender.position` and
`sender.limit`, instance counters the conductor allocates and hands over cloned, as it
hands the replayer its two; the conductor reads `sender.position` on its duty cycle to
keep `publication.limit` at the sender position plus the publication window while the
publication is connected (`spec/udp.md`, "The sender"). The conductor keeps
what it keeps today: the registries, the name resolution, the socket's opening and the
error codes 4 and 9, the counters' allocation and release, the events to clients.

## Alternatives considered

- **`socket2`.** Lost: it sets and reads the buffer lengths and sends vectored, but it
  has no `sendmmsg` or `recvmmsg` (0.6.5, 2026-07-13, MIT OR Apache-2.0, MSRV
  1.70), so a burst of 16 datagrams would be 16 syscalls on Linux, which ADR 0039 ruled
  out; it would be the second dependency beside one that has the batched calls.
- **`libc` directly, in a crate of Cavalry's own that carries the `unsafe`.** Lost: it
  is a third `unsafe` crate, which `CLAUDE.md` permits only after asking, and it
  re-writes the `msghdr` and `mmsghdr` marshalling that `nix` already has under review
  and `loom` cannot model; `nix` is that crate, maintained by others.
- **`std` alone, one `send` per datagram and the operating system's buffer lengths.**
  Lost: the receive buffer of 131072 bytes that `spec/udp.md` asks for cannot be set,
  `channel.socket_rcvbuf` and `channel.socket_sndbuf` cannot be read, and a burst is a
  syscall per datagram on every platform.
- **`sendmsg_x` on macOS.** Lost for now: Apple's batched send is a private symbol
  that `libc` does not bind on stable; macOS is a development platform, not a gate
  host, and one `sendmsg` per datagram there keeps the rule that no message triggers its
  own syscall. Revisited if a macOS gate host appears.
- **The sender inside the conductor's duty cycle, no fourth thread.** Lost: ADR 0039
  decided the thread, and Aeron separates them for the reason that holds here: a
  `sendmmsg` of 16 datagrams of 1408 bytes is tens of microseconds the conductor's
  clients would wait through on every command and every limit update.
- **Shared state between conductor and sender behind a lock.** Lost: no lock on the
  data plane (invariant 2); the recorder's pattern, bounded queues and counters, already
  carries every fact the two threads share.

## Evidence

- `nix` 0.31.3 was published on 2026-05-11, MIT, MSRV 1.69 against the workspace's
  1.89; its dependencies are `libc`, `bitflags`, `cfg-if` and, behind the `socket`
  feature, `memoffset` 0.9. `nix` 0.31.3, `libc` 0.2.190, `bitflags` 2.13.2 and
  `cfg-if` 1.0.5 are already in `Cargo.lock` through `ctrlc` 3.5.2, the `cavalry`
  crate's control-plane dependency; `memoffset` is the one crate this adds to the
  lock. `default = []`, so none of the 34 named features in its `[features]` table is
  on; `net` enables `socket`, `socket` enables `memoffset`, its optional dependency,
  and `uio` enables nothing, so the surface is the three features asked for and
  `memoffset`.
- Which feature gates which call, read 2026-10-31 at tag v0.31.3. `src/sys/mod.rs`
  lines 128 to 131 put the `sys::socket` module behind `socket`, and with it
  `setsockopt` and `getsockopt` (`src/sys/socket/mod.rs` lines 2507 and 2487),
  `MsgFlags` (line 318), `RcvBuf` and `SndBuf` (`src/sys/socket/sockopt.rs` lines 813
  to 828, under no further gate), the `SockaddrLike` trait and its implementation for
  `()` (`src/sys/socket/addr.rs` lines 651 and 784) and `SockaddrStorage` (line 1126).
  The `feature!` block behind `uio` runs from `src/sys/socket/mod.rs` line 646 to line
  2214 and holds `sendmsg` (line 1729), `sendmmsg` (line 1765), `MultiHeaders` and
  `preallocate` (lines 1838 and 1853), `recvmmsg` (line 1917) and `recvmsg` (line
  2193). `net` gates `SockaddrIn` and `SockaddrIn6` (`addr.rs` lines 812 and 959),
  `SockaddrStorage`'s `as_sockaddr_in` and `as_sockaddr_in6` (lines 1364 to 1372),
  `LinkAddr`, the conversions from `std::net::SocketAddr` (lines 1428 to 1458) and
  the multicast and IP-level options in `sockopt.rs`. `sendmmsg` takes its addresses
  as `AsRef<[Option<S>]>` with `S: SockaddrLike` (line 1777), `sendmsg` its as
  `Option<&S>` (line 1730), and `MultiHeaders<S>` asks `S: Copy + SockaddrLike` (line
  1855), so a connected socket passes `()` throughout and calls nothing `net` gates.
- The allocation audit, read 2026-10-31 at tag v0.31.3, `src/sys/socket/mod.rs`, over
  every `nix` call on the duty cycle of either path: `sendmmsg` (lines 1765 to 1832)
  and `recvmmsg` (lines 1917 to 1962), both `cfg(any(linux_android, target_os =
  "freebsd", target_os = "netbsd"))`, write the slices, addresses and control messages
  into the `MultiHeaders` passed to them and call `libc::sendmmsg` or `libc::recvmmsg`;
  neither body allocates (no `Vec`, `Box`, `collect` or `String`); `recvmmsg`'s doc
  says "This method performs no allocations" and `sendmmsg`'s doc line "Allocations
  are performed for cmsgs and to build `msghdr` buffer" is older than its body.
  `sendmsg` (lines 1729 to 1744) "Allocates if cmsgs is nonempty": its one `Vec` is
  sized to the control messages' space, 0 with none, and a `Vec` of capacity 0 does not
  allocate; the sender passes none. `recvmsg` (lines 2193 to 2213) keeps the address in
  a `MaybeUninit` on the stack and passes a null control buffer when `cmsg_buffer` is
  `None`, which the sender passes, so the macOS path allocates nothing either.
  `MultiHeaders::preallocate(num_slices, cmsg_buffer)` (lines 1853 to 1890) allocates
  the headers, the addresses and the optional control buffer once, at the endpoint's
  opening on the control plane. The allocation assertion over the send path (ADR 0039;
  this card's test) is the gate on this audit.
- Nothing measured: the UDP rows are card 182's, and this change puts the first
  datagram on the wire. Until then a commit under `cavalry-driver` says what the
  existing rows show, as ADR 0039 says.

Acceptance, 2026-11-01 *(acceptance)*: Rjae accepted the decision as proposed on
2026-11-01, answering the recorded decision d-20261031-b93489 through the team's Slack
gateway. The review of pull request 92 left two notes for the acceptance, taken here:
`spec/client-protocol.md`, "Commands", gains the rule that `add publication` on a
`cavalry:udp` channel is error code 10 when the sender's request queue has no room for
the endpoint and the publication or holds a remove still waiting for room; and the "tens
of microseconds" of the alternative that kept the sender inside the conductor's cycle
has no source and is withdrawn, that alternative losing on ADR 0039 alone. Two points of
the Decision were implemented differently, with the reason: the log is cleaned only up
to a term length behind the sender position, Aeron's rule in
`NetworkPublication.cleanBufferTo`, and not up to the sender position as `spec/udp.md`,
"The sender" said, since the resend window the same page keeps for a NAK lies in that
term, so the page's sentence is amended; and `sendmsg` and `recvmsg` run per datagram on
every platform but Linux, with `std`'s `send` and `recv` used nowhere, so that one audit
covers every call. One point the Decision did not name: the sender parks while it holds
no endpoint and no publication, unparked by the conductor's next request, as the
replayer parks with no replay (ADR 0030), because a sender idling with the backoff
strategy woke every millisecond and the laptop's client claim row read its p99.99 at 292
ns against master's 125 ns in two runs, and 125 ns again once the sender parked. The
change that accepts this ADR carries the sender thread with its socket layer, the
conductor's `cavalry:udp` publication path with the send endpoint registry, the counters
of `spec/udp.md` the sender and the endpoints move (the 15 `sender.*` rows,
`sender.position` and `sender.limit`, the publication's connection and the endpoint's
state machines with their five counters, and `receiver.receive_failed`, which the send
endpoint's receives count too), `TermReader::block_from` in `cavalry-shm` for a burst's
blocks and a retransmit's, the bench's pin of the fourth thread beside the recorder and
the replayer until card 182, and the tests: the conductor's against a receiver by hand
on the loopback, the client's over the public API for the stream in order, the NAK, the
window, the heartbeat and the close, and the allocation gate over the send path, alone
in its process. `add subscription`, `start recording` and `start replay` on a
`cavalry:udp` channel stay code 11 until the receiver lands (card 181), and `doctor`'s
two checks of ADR 0039, an unconnected publication and a capped send buffer, follow on
the control plane in their own change.

## Consequences

Easier: the receiver (card 181) takes `recvmmsg`, `RcvBuf`, `net`'s `SockaddrIn` and
`SockaddrIn6` and the same endpoint shape from this dependency with no second ADR for
it; the bench pins the sender as it pins the conductor, so the UDP rows measure a
pinned thread; every fact the two threads share is a counter or a bounded queue entry,
so `doctor` explains a stalled publication from `publication.limit`, `sender
position`, `sender limit` and the connection state machine alone.

Harder: a 0.x crate on the data plane, moved by Dependabot one pull request at a time
and reviewed by the agent; two platforms with two send paths, so the macOS gate runs
the `sendmsg` path and the Linux gates the `sendmmsg` path, and a test must hold both;
a fourth thread wants a core of its own on a gate host for the UDP rows, and ADR
0039 already notes that the loopback rows take all four isolated cores, which card 182
settles; the driver's `DriverThread` enum grows, which the bench's `DriverPins`, its
thread-hook test and the host set-up page must follow.

Revisit: at `nix` 1.0; if a `sendmmsg` of a burst reads slower at p99.99 than the
per-datagram path on a gate host, measured as ADR 0033 measures; if a macOS gate host
makes `sendmsg_x` worth binding; if kernel bypass (ADR 0039, out of the slice) replaces
the socket.

## References

- ADR 0001, ADR 0003, ADR 0009, ADR 0013, ADR 0025, ADR 0030, ADR 0039, ADR 0040
- `spec/udp.md`, "The channel URI", "The sender", "Counters", "Values";
  `spec/client-protocol.md`, "Commands" (add publication on a `cavalry:udp` channel),
  "Sizes"
- `CLAUDE.md`: invariants 2, 3 and 4; Data plane; Unsafe; Working rules for agents;
  `.claude/rules/data-plane.md`
- `crates/cavalry-driver/src/lib.rs` (`DriverThread`, `start_with_threads`),
  `src/spec.rs`, `src/conductor.rs` (`RecorderLink`, `keep_publication`'s limit pass),
  `src/recorder.rs` (`Request::Start` with its `TermReader`); `bench/src/main.rs`
  (`DriverPins`); `crates/cavalry-control/src/doctor.rs`
- nix: <https://crates.io/crates/nix>, <https://github.com/nix-rust/nix>
  (`src/sys/socket/mod.rs` at v0.31.3); socket2: <https://crates.io/crates/socket2>
- Aeron, read 2026-10-31 at `master`:
  `aeron-driver/src/main/c/media/aeron_udp_channel_transport.c`;
  `aeron-driver/src/main/java/io/aeron/driver/{Sender,SenderProxy,NetworkPublication}.java`;
  `aeron-driver/src/main/java/io/aeron/driver/media/SendChannelEndpoint.java`
- AppShapes Trello card 180, "Send a UDP publication's stream from the driver's sender
  thread"; card 182 for the rows
