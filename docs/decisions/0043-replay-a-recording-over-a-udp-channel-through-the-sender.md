# 0043. Replay a recording over a `cavalry:udp` channel through the sender

- **Status:** Accepted
- **Date:** 2026-11-02
- **Invariants affected:** 1, 3, 4
- **Supersedes:** ADR 0042 on one point, `start replay` on a `cavalry:udp` channel
  refused with error code 11 until a card of its own; the rest of ADR 0042 stands. Amends
  `spec/client-protocol.md` within version 4 and `spec/udp.md` within version 0 on the
  points listed at the end of Decision, since no byte on the wire or in a file changes

## Context

ADR 0042 built the receiver (card 181) and left one command on a `cavalry:udp` channel
refused: `start replay`, with error code 11 and a message naming card 192, because a
replay's publication takes the sender's path, not the receiver's, and that change was
already the size of the sender's. The field table of `spec/client-protocol.md` has
accepted a `cavalry:udp` replay channel since ADR 0040, so the refusal was a gap the
spec named, not a freedom in the code. Card 192 closes it: a recording on one host is
played to a subscriber on another, which is what the UDP slice (ADR 0039) is for.

The constraints are these. A replay's publication is the driver's own: created by the
conductor when the replayer has opened the recording's segment file (`on_replay_ready`),
at the replay's start position, with the recording's term length, MTU and initial term id,
so that a position in the replay is the position in the recording (ADR 0030; invariant
1); paced by its `publication.limit`, which the conductor keeps a window past the
slowest subscriber over `cavalry:ipc`; closed at the position reached when the replay
ends and lingering for its subscribers (ADR 0036). A client's `cavalry:udp` publication
is put on the wire by the sender (ADR 0041): the conductor opens or shares the resolved
channel's send endpoint, allocates `sender.position` and `sender.limit` beside the
publication's two, hands the sender the socket when new and the publication with a
reader on its log, and keeps `publication.limit` at the sender position plus the
publication window while a receiver answers, so that the publisher goes no faster than
the receivers' window; the close goes to the sender, which lingers the publication with
heartbeats carrying EOS and answers NAKs until a status message with the end-of-stream
flag arrives or the linger passes (`spec/udp.md`, "The sender"). Every registry the
path touches has its bound and its code 10 (invariant 3), and every transition its
counter (invariant 4). One fact the sender fixed in ADR 0041 no longer holds once a
replay's publication sends: "every log of this version has the UDP term length", on
which it parsed every control frame it received against `spec::UDP_TERM_LENGTH`; a
replay's log keeps its recording's term, 64 MiB for a recording of a `cavalry:ipc`
publication, so a status message past 16 MiB into a term would be rejected as
`term-offset` and the replay would stall there. The receiver already parses against
`MAX_TERM_LENGTH` and holds the offset against the image's term once it has one (card
181). Aeron's Archive makes a replay a `NetworkPublication` like any other on a UDP
channel, with the recording's term length and MTU (`ReplaySession` and
`ArchiveConductor.newReplayPublication`, read 2026-11-02 at `master`).

## Decision

`start replay` takes a `cavalry:udp` replay channel, and the replay's publication is put
on the wire by the sender exactly as a client's `cavalry:udp` publication is. The
conductor resolves the channel's host once, as the command is handled, before the
command's other checks, code 9 when it does not resolve, and keeps the resolved endpoint
and interface on the replay's registry entry. When the replayer answers ready, the
conductor opens the resolved channel's send endpoint when no publication sends on it or
shares the open one, through the one path `add publication` uses; creates the replay's
log at the start position with the recording's term length, MTU and initial term id as
over `cavalry:ipc`; allocates `sender.position` and `sender.limit` beside
`publication.position`, `publication.limit` and `replay.position`, labelled as a
publication's with the endpoint's registration id last, so that `doctor` names the
endpoint from the counters file alone; hands the sender the socket when the endpoint is
new and the publication with a reader at the replay's start position; and keeps the
publication's limit from the sender position as it keeps a publication's, so that the
replayer, which paces against that limit as it does today, goes no faster than the
receivers' window and does not move until the publication is connected. No `available
image` goes to any subscription on this driver for the replay stream: the image is the
receiving driver's, created from the sender's setup frame, and a `cavalry:udp`
subscription on this driver on the same resolved channel gets its image from the
receiver as under `add publication`. For the replay connect timeout the replay has a
subscriber while its publication is connected as `spec/udp.md`, "The sender", says, from
the first status message without the send-setup flag until the connection timeout
passes without one; a status message with the flag answers a setup frame but does not
connect the publication. So a replay nobody receives ends with reason 2 after the replay
connect timeout from its start, as one nobody subscribes to does, and one whose receiver
went ends after the connection timeout and then the replay connect timeout. The replay's end
closes the publication at the position reached as over `cavalry:ipc`, and the close goes
to the sender, which lingers it; the conductor frees the replay's `replay.position` at
the stop as over `cavalry:ipc`, and the publication's four counters with its log, and
releases the endpoint, when the sender has replied, as for any publication. The refusals
are the path's own, each leaving nothing of the replay behind: the send endpoints
registry at capacity, a counters file with no free record, or the sender's request
queue with no room for the endpoint and the publication, or holding a request still
waiting for room, is code 10 adding one to `driver.registry_full`; a socket that cannot
be opened, bound or connected is code 4; a log that cannot be created is code 12 adding
one to `driver.log_create_failed`. The sender's bounds count a replay's publication
beside a client's, since a replay's is added and closed through the sender as a
client's is: the publications it holds, its pending replies, its queues and the
conductor's deferred requests for it are sized from `MAX_PUBLICATIONS` plus
`MAX_REPLAY_PUBLICATIONS` (`spec.rs`, `MAX_SENDER_PUBLICATIONS`), where ADR 0041 sized
them from the first alone. The sender parses every control frame it receives
against `MAX_TERM_LENGTH` and holds a status message's term offset, and a NAK's term
offset and range, against the publication's own term length, rejecting one past it as
it rejects a status message outside the window, counted in
`sender.status_messages_rejected` and `sender.naks_ignored`, and `counters.toml` and
the "Counters" table of `spec/udp.md` name the reason on both. `cavalry replay`
takes `--channel <uri>`, `cavalry:ipc` when absent, in the `cavalry` crate's own change
after the driver's: with a `cavalry:udp` channel the command subscribes to nothing,
since the subscriber is on the other host, starts the replay, waits for its end and
prints the summary line alone. The client's `start_replay` already takes the channel;
its contract says what a `cavalry:udp` one does.

The amendments, page by page, each within its page's version. `spec/client-protocol.md`:
"Commands", the `start replay` bullet, which said code 11 for a `cavalry:udp` channel,
now says what the driver does with one and the codes above, and that no `available
image` goes to any subscription on this driver for the replay stream; "Events", `replay
stopped` reason 2, the paragraph on the stops the driver makes by itself, and "Liveness
and timeouts", the replay connect timeout, count the replay as having a subscriber over
`cavalry:udp` while its publication is connected as `spec/udp.md`, "The sender", says;
the stops paragraph frees the publication's four counters with its log and releases its
endpoint once the sender has replied to the close; "Sizes" counts four counters per
replay publication, 512 for the registry, so that the counters file, 14,610 of 16384
with every registry full, still fills after the registries. `spec/udp.md`: "The
sender", a replay's publication keeps its recording's term length and MTU, which its
setup frame carries, and the sender holds a status message's term offset, and a NAK's
term offset and range, against the publication's own term length; "Counters", the term
reason beside the window reason on `sender.status_messages_rejected` and
`sender.naks_ignored`.

## Alternatives considered

- **Keep code 11 until a later version.** Lost: the field table has accepted the channel
  since ADR 0040, every other command takes one since ADR 0042, and the slice's purpose
  is a stream between hosts; a replay that cannot cross one leaves the recording where it
  was made.
- **The replayer sending the recording's frames itself, with a socket of its own.** Lost:
  a second sender, with the setup, heartbeat, retransmit and flow-control rules of
  `spec/udp.md`, "The sender" written twice; the sender exists so that any publication's
  log goes on the wire by one set of rules, and a replay's publication is a publication.
- **A replay's publication with the UDP term length and MTU, rewriting positions.** Lost:
  a position in the replay would no longer be the position in the recording, which ADR
  0030 fixed and `spec/client-protocol.md` promises, and every frame would be re-laid
  into a term of another length on the replayer's duty cycle; the receiver creates an
  image of any term length a setup frame allows (`spec/udp.md`, "What a reader rejects",
  `term-length`), so nothing on the wire needs the UDP term.
- **The sender parsing a control frame against the largest term among its live
  publications.** Lost: a bound that moves as publications come and go, and a rejection
  that depends on which other publications share the thread; `MAX_TERM_LENGTH` for the
  frame's shape and the publication's own term for its offset is one rule, the
  receiver's since card 181.
- **Checking the sender's queue at the command, as `add publication` does.** Lost on its
  own: the publication is created when the replayer answers, one or more cycles later,
  and the queue's room then is what matters; a check at the command would be a second
  check with nothing it settles. The one check runs where the requests are offered.
- **`cavalry replay --channel cavalry:udp` subscribing on the same channel and printing
  the messages.** Lost: the subscriber is on another host, which is the point; a
  loopback subscriber would print what this host received, not what the other did, and
  the client's test over the public API already proves the loopback case.

## Evidence

Nothing measured: a replay over UDP is the sender's path, whose rows are card 182's,
and no change here touches the sender's duty cycle but the term each control frame is
checked against, which is one comparison per frame either way. No byte on the wire, in
a log buffer or in a segment file changes: the replay's log is the one ADR 0030 creates,
and the sender puts it on the wire by ADR 0041's rules; no layout, no dependency and no
configuration key is added, so this ADR is accepted on the evidence that informed it, as
`docs/how-to/implement-a-feature.md`, step 2, says. What the implementation proves goes
in the commit messages: a replay driven by hand from the conductor through the sender to
a receiver by hand on the loopback, every frame in order at the recording's positions,
from the recording's start and from a position inside it, the replay ending and
lingering with heartbeats carrying EOS and the endpoint closing after it, and a status
message whose offset is past the replay's term rejected where the UDP term would have
passed it; the full cases of the send endpoints registry and the sender's queue, each
leaving nothing of the replay behind; a replay over the public API into a `cavalry:udp`
subscription on the same driver, the subscriber reading the recording in order at the
recording's positions and seeing the stream end; and `just replay-check` green.

## Consequences

Easier: a recording made on one host is played to a subscriber on another with the
command and the API a `cavalry:ipc` replay uses, the channel the one difference; a
replay's publication is a publication to the sender, the endpoints, the counters and
`doctor`, so nothing new is explained; the sender's term rule is the receiver's.

Harder: a replay over UDP holds its send endpoint open for the publication linger after
it ends, as any publication does, so a host that replays many short recordings to one
endpoint shares one socket among them and the endpoint's `channel.publications` counts
them; a `cavalry:udp` replay whose recording has a 64 MiB term creates a 64 MiB image
on the receiving host, three partitions made resident when the image is created (ADR
0027), where a `cavalry:udp` publication's image is 16 MiB; and `cavalry replay` over
UDP prints no message, so a person checking what arrived reads it on the receiving host.

Revisit: when card 182's rows measure the sender's path, for the term check's cost if any
shows; when a recording's MTU can differ from the UDP path's, which no key sets in this
version, so that every log carries the 1408 bytes of `spec/client-protocol.md`, "Sizes",
for whether a replay's publication keeps the recording's MTU, as decided here, or takes
the channel's, since a datagram longer than the path's MTU is dropped on the wire; when
multicast binds the interface (ADR 0039), for a replay to many receivers.

## References

- ADR 0030, ADR 0036, ADR 0039, ADR 0040, ADR 0041, ADR 0042
- `spec/client-protocol.md`, "Commands" (start replay), "Events" (replay stopped),
  "Liveness and timeouts"; `spec/udp.md`, "The channel URI", "The sender", "What a
  reader rejects"
- `CLAUDE.md`: invariants 1, 3 and 4; Protocol; `.claude/rules/data-plane.md`
- `crates/cavalry-driver/src/conductor.rs` (`start_replay`, `on_replay_ready`,
  `on_replay_stopped`, `keep_replays`, `open_send_endpoint`), `src/sender.rs`
  (`poll_endpoint`, `on_status_message`, `on_nak`); `crates/cavalry/src/main.rs`
  (`replay`); `docs/how-to/replay-a-recording.md`
- Aeron, read 2026-11-02 at `master`:
  `aeron-archive/src/main/java/io/aeron/archive/{ReplaySession,ArchiveConductor}.java`
- AppShapes Trello card 192, "Replay a recording over a cavalry:udp channel"; card 182
  for the rows
