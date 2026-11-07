# 0059. Create a publication at a position and arm a recording on a subscription

- **Status:** Accepted
- **Date:** 2026-11-07
- **Invariants affected:** 1, 3, 6
- **Supersedes:** none. Takes `spec/udp.md` to version 2 and `spec/client-protocol.md`
  to version 6; changes no other page.

## Context

`spec/cluster.md` (ADR 0058) names four transport dependencies that card 227, the
replicated log, must meet before any of it is built, and leaves the first two to a
version of `spec/udp.md` and `spec/client-protocol.md` under an ADR of their own. The
first: the leader's log publication is created at the term base log position for every
term after the first, so that every member's log holds every entry at the same
position. No command of `spec/client-protocol.md` version 5 and no key of
`spec/udp.md` version 1 gives a publication a starting position: `add publication`
creates the log at position 0 with an initial term id the driver takes from its clock
(`crates/cavalry-driver/src/conductor.rs`, the add publication path), and only a
replay's log is created at a position, `spec/log-buffer.md`'s "created at a position"
rule for `start replay` (ADR 0030; `LogBuffers::create_at`). The second: a follower's
recording of the live log, and of a replay during follower log replication, must begin
at its image's join position, the position the setup frame carries and the image's log
is created at (`spec/udp.md`, "The receiver"). `start recording` of version 5 records an
image from its rebuild position when the command is handled, and a stream with a
subscription but no image yet is error code 20, so the data that arrived before the
command lands in no recording: a hole at the start of the recording that the member's
append position, the end of the contiguous span its recordings hold, never passes. The
third dependency, a replay with the log's session id, is met by version 0 of the
cluster page, which gives each replay a session id of its own; the fourth, the format
of the record of recorded terms, is written with the replicated log that reads it
(ADR 0058, Consequences), not here.

Aeron gives a publication its position through the channel URI: `init-term-id`,
`term-id` and `term-offset`, with `term-length`, make a publication start at that
position, which Archive's extend recording and Cluster's consensus module use for the
log of a term at its base, and `AeronArchive.startRecording(channel, streamId,
sourceLocation)` records every image that arrives on a subscription to that channel
from the image's join position, each as a recording of its own. Cavalry's UDP term
length is fixed by `spec/udp.md`, "Values", and a key that does nothing is a key the
reader cannot check (ADR 0053), so `term-length` has no place; a recording belongs to
the driver and is started by one command (ADR 0025), so the arming is the command's.

## Decision

`spec/udp.md` version 2: a publication's channel may name `init-term-id`, `term-id`
and `term-offset`, Aeron's names, all three or none, each a 32-bit integer in decimal,
and the driver creates the publication's log at the position they name, (`term-id` −
`init-term-id`, a 32-bit two's complement count of 0 or more) × the UDP term length +
`term-offset`, with `init-term-id` as the initial term id, under `spec/log-buffer.md`'s
"created at a position" rule, so that the first frame the publication sends is at that
position and its setup frames carry it. The keys are a publication's, as `control`,
`control-mode` and `fc` are: an `add subscription` or a destination channel that names
one is refused with code 1, and an `add publication` that would join a live publication
whose keys differ from its own, or that names them where the live one named none or the
reverse, is refused with code 1 and the reason `position`, since a shared log has one
position. A channel is checked for `position` after `fc` and before `multicast`: one or
two of the three keys without the rest, a value that is not a 32-bit integer, a
`term-id` below `init-term-id` by that count, or a `term-offset` that is negative, not a
multiple of 32 or not below the term length. The channel vectors carry the three fields
and the position they make, null when the string names none.

`spec/client-protocol.md` version 6: `start recording` on a `cavalry:udp` channel and
stream with no live publication and no image, but a live subscription on the resolved
channel and stream, arms the recording: the driver answers `operation success` at
once, counted in `recording.armed`, and holds the armed recording, at most one per
receive endpoint and stream that has a live subscription, so that the subscriptions
registry of `spec/client-protocol.md`, "Sizes", bounds the armed recordings and a table
of its capacity is never full; when the first image of that stream arrives on the
endpoint it starts the recording at the image's initial position, its join position,
before the image is announced, so that every frame the image holds is in the
recording, and sends `recording ready` to the arming client with the arming command's
correlation id, counted in `recording.armed_started`. A second `start recording` while
one is armed is code 22 with a message that says the recording is armed. An armed
recording is dropped when the last subscription on its endpoint and stream goes
before an image arrives, by `remove subscription`, `client close` or `client timeout`,
a receive endpoint that closes being one case of it, counted in
`recording.armed_dropped`: the receiver creates an image only for a stream a
subscription on the endpoint wants (`spec/udp.md`, "The receiver", step 1), so an
armed recording that outlived its stream's last subscription could never start, and
while the endpoint stayed open for another stream it would never be dropped either.
Code 20 is now a stream and channel with no live
publication, image or subscription. The three counters are static counters of
`counters.toml`. Version 6 encodes every message as version 5 does, and a version 5
driver answers the arming case with code 20.

This change carries the two pages, their vectors and `cavalry-proto`'s channel parser
and protocol version; the driver's own change is card 227's next pull request, and
until it lands the driver answers an `add publication` that names the keys, and a
`start recording` that would arm, with code 11 and a message naming this ADR and the
card, as ADR 0053's first change did for card 225.

## Alternatives considered

- **A position field in `add publication`.** Lost. Aeron carries the position in the
  channel, the channel string crosses the C ABI and the five generated clients unchanged
  while a command field changes every client's encoder, and a publication the cluster
  alone creates at a position should cost the client API nothing.
- **`term-length` as Aeron's fourth key.** Lost: the UDP term length is "Values"' and
  one for every publication, so the key could only repeat it or disagree, and a key
  that does nothing is a key the reader cannot check (ADR 0053).
- **Recording from the rebuild position, the follower replaying the gap afterwards.**
  Lost: invariant 1. A hole at the start of a recording is a span the member's append
  position never covers (`spec/cluster.md`, "The log"), and a replay of the gap from
  the leader is a second recording where one is wanted.
- **Arming through the subscription's channel, a `record` key.** Lost: a recording is
  the driver's and starts with one command (ADR 0025); a key would put a verb into a
  channel's identity, which two channels compare by.
- **Answering `recording ready` alone, when the image arrives.** Lost: invariant 3's
  bound on waiting. The client's response timeout, 10 s, would report the command lost
  whenever no image came in time; `operation success` answers the command within the
  timeout, and the later `recording ready` carries its correlation id, as `recording
  stopped` reaches every client unasked.
- **Recording every image that arrives, as Aeron's Archive does.** Lost for this
  version: bounded at one armed recording per endpoint and stream, since the cluster's
  follower subscribes to one leader's stream at a time; the Revisit names it.
- **Dropping an armed recording only when its receive endpoint closes.** Lost:
  invariant 3. An endpoint is shared by every stream on its channel and stays open
  while any of them has a subscription, and the receiver creates no image for a stream
  with none, so an armed recording whose stream's last subscription went could neither
  start nor be dropped, and a client that armed and unsubscribed stream after stream
  would grow the table with no bound. Tying the armed recording to its stream's
  subscriptions bounds it by the subscriptions registry.

## Evidence

None measured: a channel's keys and a command's semantics. What informed it: Aeron's
`CommonContext` parameter names and `ChannelUri`, Archive's `startRecording` and the
recording signals, read 2026-11-07; ADR 0058's Consequences, which name the two
dependencies and their cost; `spec/log-buffer.md`'s "created at a position" rule and
`LogBuffers::create_at`, which the replayer already uses; the conductor's add
publication path, which takes the initial term id from the clock and the position as
0; and the card's brief. What the implementation proves goes in the commit messages of
the driver's change: a publication created at a position whose image joins there, and
a recording armed before its image that holds the image's first frame.

## Consequences

Easier: card 227's leader creates each term's log publication at the term base log
position and every member's log holds every entry at the same position; a follower's
recording of the live log, or of a replay, holds the image from its first frame, so
the member's append position passes it and `just replay-check` compares whole
recordings across members; the five clients gain both through the channel string and
the command they have.

Harder: the sender's first setup frame carries a position that is not 0, which the
receiver already reads; the driver keeps an armed-recording slot per receive endpoint
and stream with a live subscription, at most the subscriptions registry's capacity,
drops it when the stream's last subscription on the endpoint goes, and starts a
recording from the
receiver's thread's announcement rather than a command; two commands of the client
protocol answer differently from version 5, which this version's paragraph says.

Revisit: when a client needs a term length of its own on a `cavalry:udp` publication,
for Aeron's `term-length`; when a subscription needs every image recorded, for
Archive's rule; when the cluster's catch-up needs a replay with the log's session id
(card 230), for Aeron's `session-id` key.

## References

- `spec/udp.md`, version 2, "The channel URI", "The sender" and "What a reader rejects".
- `spec/client-protocol.md`, version 6, "Commands", start recording, and "Error codes".
- ADR 0058, Consequences, the four dependencies; ADR 0053, the keys a reader can check;
  ADR 0030, a log created at a position; ADR 0025, a recording is the driver's; ADR
  0042, an image's recording from the rebuild position.
- Aeron's `io.aeron.CommonContext` (`INITIAL_TERM_ID_PARAM_NAME`, `TERM_ID_PARAM_NAME`,
  `TERM_OFFSET_PARAM_NAME`) and `io.aeron.archive.client.AeronArchive.startRecording`.
