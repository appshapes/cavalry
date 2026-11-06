# 0053. Add multi-destination cast to `cavalry:udp` with add and remove destination

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 1, 2, 3, 4, 6
- **Supersedes:** none. Amends `spec/udp.md` to version 1, for its channel grammar and
  the sender's per-destination state, and `spec/client-protocol.md` to version 5, for
  two commands; no frame is new and no frame's field or version byte changes (ADR 0040;
  ADR 0052, feature 1)

## Context

ADR 0052 makes the fifth slice Raft on the durable log and decides its fan-out: the
leader's log is one publication whose sender sends each datagram to every follower's
endpoint, Aeron Cluster's shape, under the `min` flow control strategy so that the
slowest follower paces the leader and no follower loses its image. It leaves two
decisions to this ADR, with their evidence: the control address, and how a
destination's status messages reach the right publication. A `cavalry:udp` publication
of version 0 has one send endpoint per resolved channel, one socket connected to the
endpoint, and its sender limit is the maximum over every accepted status message of
the position plus the receiver window (`spec/udp.md`, "The sender"), which is Aeron's
`max` strategy under a name the page does not use. A publication cannot be given a
second destination, and a channel with a key version 0 does not name is refused with
error code 1 (`spec/udp.md`, "What a reader rejects"; ADR 0040), so a channel of this
version is never misread by a driver of version 0.

What Aeron does, read on 2026-11-06 at `master`, version 1.55.0-SNAPSHOT. A publication
on `aeron:udp?control=<host>:<port>|control-mode=manual` is multi-destination cast in
manual mode: `SendChannelEndpoint` keeps a `ManualSndMultiDestination` instead of a
connected socket, its socket bound to the control address (`UdpChannel`, the
explicit-control branch, whose canonical form keys the channel on the control and
endpoint values and not on `interface` or `fc`); `send` loops over the destinations and
returns the fewest bytes any of them took; a destination is added and removed by the
commands `ADD_DESTINATION` (`0x07`) and `REMOVE_DESTINATION` (`0x08`), each carrying
client id, correlation id, the publication's registration id and the destination
channel (`DestinationMessageFlyweight`), answered with `OPERATION_SUCCESS`;
`addDestination` does not refuse a duplicate address, `removeDestination` of an
address not present removes nothing and does not fail; a manual destination is never
removed on its own, only by the command, while a dynamic one times out after
`DESTINATION_TIMEOUT`, 5 s; a command on a publication whose channel is not in manual
mode is refused with `INVALID_CHANNEL`, "channel does not allow manual control"
(`validateAllowsManualControl`). A status message, NAK or RTT measurement arriving at
the endpoint finds its publication by session id and stream id
(`publicationBySessionAndStreamId`), whatever address it came from; the destination's
own record is matched by the datagram's source address, and by receiver id and port
once the receiver id is known, only to note activity and re-resolve a name. Flow
control is the publication's, from the channel's `fc` parameter, `max` by default for
multicast and the unicast strategy for unicast, both of which raise the limit to the
greatest position plus window seen; `min` (`AbstractMinMulticastFlowControl`) keeps a
table of receivers keyed by receiver id, each with its last position plus window and
the time of its last status message, raises the limit to the least of them on every
status message and never lowers it there, and on idle drops a receiver whose last
status message is older than the receiver timeout, `aeron.flow.control.receiver.timeout`,
5 s by default, or that flagged end of stream, taking the limit to the least of those
that remain; `fc=min` takes options, `t:<timeout>` and `g:<tag>/<min size>`, and
`tagged` keys the group. Retransmits and heartbeats go to every destination as data
does, and one retransmit is outstanding per publication, so a second destination's NAK
for a range on its way is dropped.

## Decision

A publication channel may name a control address instead of an endpoint,
`cavalry:udp?control=<host>:<port>|control-mode=manual`, and the two commands `add
destination` and `remove destination`, type ids 7 and 8 under Aeron's numbers, give the
publication's send endpoint the destinations its sender sends every datagram to; every
publication channel may name `fc=max` or `fc=min`, `max` being what version 0 does and
the default. `spec/udp.md` moves to version 1 and `spec/client-protocol.md` to
version 5; every frame's version byte stays 0, since a receiver of version 0 drops a
frame whose version is not 0 and a member at N would otherwise drop every datagram of
a member at N+1 (ADR 0052, feature 1). The pages say the rest; what follows is the
decisions they rest on.

**The control address is the sender's socket.** A control channel's send endpoint is
one socket bound to the control address and connected to nothing, where a unicast
channel's is one socket connected to the endpoint; the sender sends every datagram of
every publication on the endpoint, setup frames, data, heartbeats and retransmits, to
each of the endpoint's destinations in the order they were added, the datagrams per
burst bound of version 0 holding per destination, so a cycle sends at most sixteen
times the destinations. A destination's receiver answers to the source address of the
setup frame that created its image (`spec/udp.md`, "The receiver", step 1), which is
the control address, so its status messages, NAKs and RTT measurements arrive on the
sender's socket; that is what the control address is for, and why a channel names a
control address or an endpoint and not both. `interface` has no meaning beside a
control address, which is itself the local address, and is refused with it. Two
channels are the same channel when their resolved control addresses are the same, as
unicast channels are by resolved endpoint and interface, so publications on one
control address share the socket and the destinations, which belong to the endpoint
and go when it closes; `fc` is the publication's and not part of the identity, so an
`add publication` that would join a live publication under a different `fc` is
refused rather than silently take the first's. A control channel is a publication's: on
`add subscription`, `control`, `control-mode` and `fc` are refused, since this version
receives on no control address and reads no `fc` there, and refusing is cheaper than
ignoring and finding out later. `control-mode=dynamic`, where a receiver finds the
sender by its control address and the sender learns its destinations from their status
messages, is left out: the cluster's members are named in its configuration (ADR 0052,
Configuration), and a destination that announces itself is one the operator did not
name.

**A destination the socket can never send to is refused when it is added, and one the
socket refuses holds back no other.** A socket bound to the control address sends only
to addresses of that address's family, and when bound to a loopback address it sends
to no other host. A connected socket of version 0 meets either case at `connect`, and
its command gets code 4 at once. An unconnected socket would meet it at every send,
after the command had been answered with `operation success`. So `add destination`
resolves the destination's endpoint to the first address of the control address's
family that the resolver returns, not to the first address of any family. It refuses
with code 1 and the reason `destination` a destination with no such address, an
address of the other family written in the channel among them, and one that is not a
loopback address beside a loopback control address; the message names the
destination, the control address and what to change. A name that resolves to no
address at all is code 9, as on every channel. While the stream lives, the socket can
still refuse a destination for a reason of that destination's own: no route from the
control address to it, a firewall, an address that has gone. The sender does not wait
for such a destination. The datagrams of the burst from the refused one on are lost to
that destination alone, as datagrams lost between the hosts are, and its receiver NAKs
them as it NAKs any loss; when the socket refused every destination so, the sender
position holds, as on an endpoint. A refusal of the socket's own, no room in its send
buffer (`EAGAIN` or `ENOBUFS` on Linux) or an interrupted send (`EINTR`), holds the
sender position for every destination, as a short send does, and the destinations
that took the datagram get it again, so that back-pressure costs no destination a
frame. `sender.short_sends` and `sender.send_failed` count each destination's short or
refused send, as the counters of a send count each destination's send. Aeron's `send`
returns the fewest bytes any destination took (Context). Applied to a refusal as well,
that rule would let one destination the socket refuses stop the stream to every
destination until a client removes it, and `sender.send_failed`, which is not keyed by
destination, would not tell `doctor` which one.

**A status message reaches its publication by session id and stream id**, on the
endpoint it arrived at, as it does on a unicast endpoint and in Aeron; the datagram's
source address is not read for the matching, so a destination behind an address
translation is matched as any other. The per-destination state is the `min`
strategy's accounting, keyed by the status message's receiver id, which is the field
that tells a sender's receivers apart (`spec/udp.md`, "Status message"; ADR 0052):
under `min` the sender keeps per publication a table of at most 64 receivers, the
destinations bound, each with its last accepted status message's position plus window
and the time it arrived; an accepted status message, one that passes version 0's
window check, adds or updates its receiver's entry and the sender limit becomes the
greater of itself and the least position plus window in the table; on every duty cycle
an entry whose status message is older than the flow control receiver timeout, 5 s, or
whose last status message carried end of stream, is dropped and counted in
`sender.flow_control_receivers_timed_out`, and when entries remain the limit becomes
the greater of itself and the least of them; a 65th receiver's status messages are
accepted for the connection and not tracked, counted in
`sender.flow_control_receivers_full`; the entries now are the gauge
`sender.flow_control_receivers`, keyed as `sender.limit` is. The sender limit never
falls under either strategy, as version 0's vector `sender-limit-never-falls` pins:
Aeron's idle path can lower it when a receiver's window shrinks, and a Cavalry
receiver's window is fixed (`spec/udp.md`, "Values"). Under `max` the sender keeps no
table and version 0's rule stands. A destination itself never times out: the operator
named it and the command removes it, Aeron's manual mode. A dead destination's sends
either vanish on the way or are refused for a reason of its own (above), and in
neither case does the sender wait for it. What it costs the stream is nothing under
`max`. Under `min` it costs a sender limit held back for as long as its status
messages still arrive and for the flow control receiver timeout after they stop,
which is `min`'s promise that no receiver loses its image to a faster one. `cavalry
doctor` explains it from the three counters and `sender.send_failed`.

**The commands are Aeron's, under its numbers.** `add destination` and `remove
destination` carry client id, correlation id, the client's registration of the
publication and the destination channel, and are answered with `operation success`.
Only the destination channel's `endpoint` is read, resolved once as every channel's
host is, to an address of the control address's family (above); `cavalry:ipc` or a
control channel as a destination is code 1. An unknown or
another client's registration is code 3; a publication whose channel has no control
address is code 1, Aeron's `INVALID_CHANNEL`, with a message that says what to add to
the channel; a 65th destination on an endpoint is code 10 and counts
`driver.registry_full`, the destinations being a bounded table of the endpoint's,
listed under "Sizes". A destination is its endpoint as the add wrote it, the host
compared byte for byte and the port by value, and the endpoint keeps that beside the
address it resolved. A destination already present is added once more by nothing and
answered with success, so that a client that retries after a response timeout is not
refused; two writings of one address, a name and the address it resolves to, are two
destinations, as two adds are in Aeron. `remove destination` matches the endpoint as
written and resolves nothing. So a name whose address changed after the add removes
the address the add resolved, rather than missing it, answering success, and leaving
that address to receive every datagram. A destination not present is removed by
nothing and answered with success, as `stop recording` of a stopped recording is. A
replay's publication on a control
channel sends to the endpoint's destinations, which a client adds through a
publication of its own on the channel, since a replay has no client registration to
name. The reserved ids 7 to 10 become 9 and 10.

**Interim.** This change lands the pages, their vectors, the parser and the codecs in
`cavalry-proto`, and the driver's refusals: until the driver's change under card 225
lands, the driver answers a control channel, `fc=min` and the two commands with error
code 11 and a message naming the card, as ADR 0040's acceptance did for a channel
that parsed before cards 180 and 181 (`spec/client-protocol.md`, "Error codes", code
11); a well-formed channel of version 1 is not code 1's invalid channel. The driver's
sender, its counters and `doctor`, the client API's `add_destination` and
`remove_destination`, the ABI's functions and version bump with the five clients, the
conformance suite's two-destination round and the bench row follow in their own
commits on the card, each with its evidence.

## Alternatives considered

- **One publication per follower** (ADR 0052, Alternatives). Lost there: the leader's
  per-entry work grows with the cluster and the members' recordings differ in
  position.
- **Multicast.** Lost: the bench hosts' link is a cable between two ports and the
  cloud VMs the README promises do not route it; a multicast group is also a
  destination the operator cannot remove one member from.
- **The dynamic control mode.** Lost: a destination that announces itself with a
  status message is one the operator did not name, and the cluster's members are
  configuration (ADR 0052); it is also the mode where a destination must time out, and
  a timeout is a counter and a `doctor` check this slice does not need. Left to the
  Revisit with `tagged` and the group tag.
- **Destinations on the publication rather than the send endpoint.** Lost: the socket
  is the endpoint's and every datagram it sends goes to the destinations, so two
  publications on one control address could not send to different sets without two
  sockets on one address, which cannot bind; Aeron's are the endpoint's too.
- **`fc` as part of the channel's identity.** Lost: two publications on one control
  address with different strategies would be two endpoints bound to one address;
  refusing the join that differs keeps one endpoint and lies to nobody.
- **Match a destination's status message by its source address.** Lost: the
  publication is found by session id and stream id already, the source address
  changes under address translation, and the `min` accounting needs the receiver id
  in any case, since two receivers behind one address are two windows.
- **Let a destination time out in manual mode.** Lost: the operator named it, and a
  destination that goes quiet and is dropped would be sent nothing when it returns
  until the operator adds it again; Aeron keeps manual destinations until removed, and
  the stream's cost under `min` is bounded by the receiver timeout.
- **Lower the sender limit on idle, as Aeron's `min` does.** Lost: a Cavalry
  receiver's window is fixed, so the only way the least position plus window falls is
  a receiver that was dropped, after which the least rises; keeping "never falls"
  keeps version 0's vector and the sender's one comparison.
- **A new error code for a destination command on a unicast publication.** Lost:
  Aeron answers `INVALID_CHANNEL`, and code 1's message names what to add.
- **Refuse a duplicate `add destination`.** Lost: a client that retries after a
  response timeout would be refused for a destination it has; Aeron adds it twice,
  which sends every datagram twice to one address, and adding it once is the cheaper
  truth.
- **Ignore `control`, `control-mode` and `fc` on a subscription channel**, as Aeron
  ignores `fc` there. Lost: a key that does nothing is a key the reader cannot check
  (`CLAUDE.md`, Protocol); refusing now costs nothing and a later version can accept
  it with an ADR.
- **`fc=min` options, `t:` and `g:`.** Lost: the timeout is Aeron's default until
  `doctor` shows a host that needs otherwise (`CLAUDE.md`, Configuration and
  defaults), and the group is the Revisit's.
- **Hold the stream for a destination the socket refuses**, taking Aeron's fewest
  bytes for a refusal too. Lost: one destination the socket cannot reach stops the
  stream to every destination until a client removes it, which breaks this ADR's
  promise that a dead destination costs nothing under `max`. `sender.send_failed`,
  not keyed by destination, would not say which destination it is.
- **Leave a destination's family and loopback address to the send.** Lost: the
  command would answer `operation success` for a destination the socket can never
  send to, and every datagram to it would be refused for as long as it stays. Version
  0 refuses the same mistake at `connect` with code 4, and the add is where the
  operator is.
- **Code 9 for a destination with no address of the control address's family.** Lost:
  code 9 says a name does not resolve, and an address of the other family written in
  the channel needs no resolving. Code 1 with the reason `destination` covers both
  cases and names what to change.
- **Count a refused send in a keyed counter per destination.** Lost: 64 destinations
  on each of 64 endpoints is 4096 counters, and the counters file has 492 to spare
  (`spec/client-protocol.md`, "Sizes"). The refusals that last, the family and the
  loopback address, are refused when the destination is added. What is left, a route
  that comes and goes, is `sender.send_failed` on the sender's host beside
  `receiver.gaps_detected` on the destination's.
- **Match a remove by the address its endpoint resolves to now.** Lost: a name whose
  address changes between the add and the remove, such as a member replaced behind
  its name, would remove nothing and be answered with success. The address the add
  resolved, perhaps another host's by then, would go on receiving every datagram.

## Evidence

None measured: a channel grammar and two command layouts, with Aeron's rules. What
informed it: Aeron's sources named in Context, read on 2026-11-06, and its
Configuration's defaults, `aeron.flow.control.receiver.timeout` 5 s and
`aeron.flow.control.group.min.size` 0; ADR 0052's Decision and card 225's brief; the
driver's `resolve` in `crates/cavalry-driver/src/conductor.rs`, which takes the first
address of any family the system gives; the review of pull request 151 on 2026-11-06,
which found that a destination of the other family would be answered with success
and then refused at every send;
`spec/udp.md`'s sender and receiver as they stand, whose setup elicitation already
lets a destination added after the first status message get its setup frame from the
sender, and whose retransmit rules already drop a second receiver's NAK for a range on
its way. The change that accepts this ADR carries the two pages, 31 new channel
vectors and 13 new client protocol vectors with the reserved-id vector moved off 7,
the parser's `control`, `control-mode` and `fc` with the two new refusal
reasons and the order between every pair of checks pinned, the two commands' codecs
with their no-panic cases, and the driver's interim refusals with a test that each is
code 11 and registers nothing. Nothing on a message's path changes in it; the
sender's change carries its histograms.

## Consequences

Easier: the cluster's log is one publication to every follower (ADR 0052, feature 3);
Aeron's documentation of multi-destination cast and of `min` flow control explains
Cavalry's; a publication on a cable to two hosts needs no second publication; a
dropped receiver is three counters `doctor` reads.

Harder: the sender gains an unconnected socket with a per-datagram address, a
destinations table per endpoint and a receivers table per `min` publication, each
bounded and tested at its bound; the sender tells a refusal of the socket's own from
a destination's, and the destinations table keeps each destination's endpoint as
written beside its address; a cycle sends up to sixteen datagrams per
destination; the client API, the ABI and the five clients gain two functions, and the
ABI's version moves before its first release (ADR 0051); the counters file's sum rises
to 15,892 of 16384 ("Sizes"), leaving 492 for the slice's remaining features.

Revisit: when a cluster needs the `tagged` strategy, the group tag or the dynamic
control mode (ADR 0052); when `doctor` shows a host that needs a receiver timeout of
its own, for `t:` and its `why_not_default`; when a subscription needs to receive on a
control address, for multi-destination subscriptions; when a destination must be
re-resolved while a stream lives (ADR 0040's Revisit); when the counters file's sum
nears 16384, for a larger capacity under `spec/counters-file.md`.

## References

- `spec/udp.md`, "The channel URI", "The sender", "Counters", "Values", "What a reader
  rejects", "Vectors"; `spec/client-protocol.md`, "Commands", "Error codes", "Sizes",
  "What the driver rejects"; `spec/README.md`
- ADR 0040, ADR 0041, ADR 0043, ADR 0045, ADR 0051, ADR 0052
- `CLAUDE.md`: invariants 1 to 4 and 6; Protocol; Configuration and defaults; When in
  doubt
- Aeron, read 2026-11-06 at `master` (1.55.0-SNAPSHOT):
  `aeron-driver/src/main/java/io/aeron/driver/media/{SendChannelEndpoint,UdpChannel}.java`,
  the former holding `ManualSndMultiDestination`, `DynamicSndMultiDestination` and
  `Destination`;
  `aeron-driver/src/main/java/io/aeron/driver/{AbstractMinMulticastFlowControl,MinMulticastFlowControl,MaxMulticastFlowControl,UnicastFlowControl,DriverConductor,Configuration}.java`;
  `aeron-client/src/main/java/io/aeron/command/{ControlProtocolEvents,DestinationMessageFlyweight}.java`;
  `aeron-client/src/main/java/io/aeron/{ChannelUri,CommonContext,ErrorCode}.java`;
  the wiki pages Multiple Destinations and Flow and Congestion Control
- AppShapes Trello card 225; ADR 0052's card 224; the review of pull request 151,
  2026-11-06
