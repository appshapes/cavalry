# 0098. Let a recording connect a cavalry:udp publication whose channel names ssc

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3, 4, 5, 6
- **Supersedes:** none. Amends ADR 0041's rule for when a `cavalry:udp` publication is
  connected, for a channel that names the new key

## Context

A `cavalry:udp` publication is connected by a receiver: from its first status message
until the connection timeout passes without one (ADR 0041; `spec/udp.md`, "The
sender"). Until then `offer` returns `not connected`. A recording of the publication
changes nothing: recorded or not, a publication with no receiver takes no message.
Over `cavalry:ipc` it is the other way. ADR 0025 decided that "a recording alone is a
legitimate consumer", and a recorded `cavalry:ipc` publication with no subscriber is
connected, with its limit a window past the recorder.

The first user of the durable log over UDP met the difference on 2026-11-11. It is a
server that records one feed and is followed by other hosts through replays of the
recording, so its feed has no receiver while the server is alone, and often none at
all, since its followers read the recording and not the live stream. A recorded
publication on a control channel with no destination refused an offer for 2 s, until
the test gave up. The way round was a subscription to its own feed, on a port of its
own, added as the publication's first destination. It works, and it costs one more
log, 3 × 16 MiB + 4 KiB resident in the driver directory, and a subscription the
program must keep reading, since a receiver that never reads holds the sender at its
window. Measured on Linux on 2026-11-12, five such servers that follow each other
hold ten logs each, 488 MiB, and one of the ten is that subscription's image.

Aeron has the same rule and a key for the exception. Its channel parameter `ssc`,
spies simulate connection, lets a publication be connected while only spies read it.
A spy is a subscription that reads a publication's log on the publisher's own driver,
and Aeron's archive records a publication on its own host through one.

The rule ADR 0097 added matters here: a recorded `cavalry:udp` publication's limit is
a window past the lesser of the sender position and the recorder's, so a recorder
already holds its publisher back.

## Decision

A `cavalry:udp` publication's channel may name `ssc=true` or `ssc=false`, Aeron's key
and Aeron's meaning, with the driver's recorder as the one spy. Under `ssc=true` a live
recording of the publication connects it whether or not a receiver answers. While it
has a live recording and no receiver answers, the publication is alone with its
recording: the sender sends no data frame of it, moves its sender position up to the
recording's position on every duty cycle, and counts the bytes it passed in
`sender.bytes_unsent`; the publisher's limit is then a window past the recorder, as
over `cavalry:ipc`. A receiver that joins is given a setup frame at the sender
position, and what was published before is in the recording, for a replay. `ssc=false`
is the default and is today's behaviour, byte for byte. The key is a publication's: a
subscription's, a destination's and a replay's channel refuse it, a recording's reads
past it, and a second `add publication` that would join a live publication under
another `ssc` is refused. `spec/udp.md` goes to version 3 and `spec/client-protocol.md`
to version 9; no frame changes.

## Alternatives considered

- **Connect every recorded `cavalry:udp` publication by its recording, with no key,
  as over `cavalry:ipc`.** One rule for both transports, and nothing to learn. Lost,
  for three reasons. A publisher over UDP reads `not connected` to learn that no host
  receives, and a recording on its own host would take that away from every publisher
  whose stream happens to be recorded. The cluster leader's log is a recorded
  `cavalry:udp` publication, and a leader that appended with no follower connected
  would change what the election cards build on while they are being built. And Aeron,
  whose names and semantics this project copies where they are good, makes it the
  publisher's choice.
- **Keep the subscription to its own feed as the answer, and document it.** No change
  to the wire page. Lost: it costs a log and a socket for a stream that crosses no
  wire, the program must read a subscription whose content it wrote, and a stranger
  finds the pattern only after the first refused offer.
- **Send the data to the destinations while alone, and count it.** The sender would
  keep its ordinary path. Lost: with no receiver's status message there is no window to
  send into and nobody to NAK a loss, so the bytes would go to addresses that may be
  dead, counted as sent. Nothing is counted as sent that no receiver took.
- **Move the sender position to the producer position, not the recorder's.** No
  counter to share with the sender. Lost: a receiver would then join at a position the
  recording does not hold yet, and the replay it asks for to fill what it missed could
  not reach its join position until the recorder had caught up. At the recorder's
  position, everything before a join is already in the recording.
- **A name of Cavalry's own for the key.** Lost: `ssc` is what an Aeron user searches
  for, and the concept matches. The glossary says what a spy is here.

## Evidence

- **The refusal, measured.** On 2026-11-11, at 0d28208 on macOS, a recorded
  publication on `cavalry:udp?control=127.0.0.1:41001|control-mode=manual` with no
  destination: `offer refused (not-connected) for 2000 ms at seq 0`. With the
  publication's own subscription as its first destination the same program appended,
  and two such programs exchanged every record once and in order across a restart, a
  kill and a suspension of each side.
- **What the subscription costs.** The same program on Linux 7.0, aarch64, on
  2026-11-12 at 54ad5e0: three instances that follow each other hold six logs each,
  296 MiB of `/dev/shm`, and five hold ten each, 488 MiB. One log of each instance is
  the image of its own feed.
- **A late receiver is not left stalled.** The receiver drops a data datagram, and a
  heartbeat, whose position is more than half a term past its last status message, as
  an over-run, and an over-run is not activity (`crates/cavalry-driver/src/receiver.rs`,
  `on_heartbeat`; `spec/udp.md`, "The receiver", steps 2 and 5). So an image whose
  sender moved on by more than half a term while it was away times out after the image
  liveness timeout and is retired with `unavailable image`, and its subscribers are
  told. One within half a term is accepted by the sender, since its status message
  lies in the window, and NAKs what it missed, which lies in the resend window: both
  are half a term.
- **The recorder's position is a safe place to stand.** The recorder takes whole
  committed frames, so `recording.position` is a frame boundary at most the producer
  position. A setup frame there names the start of a frame.
- **Aeron documents the same behaviour.** Its wiki's "Channel Configuration" page
  gives `ssc` as a publication's parameter by which the publication behaves as
  connected while a spy subscription is attached and no receiver is; its "Flow And
  Congestion Control" page says the publication is then connected whatever the size of
  the receiver group; and it describes the stream as advancing with no subscriber
  active when spies simulate connection.

## Consequences

- A program that records a `cavalry:udp` stream and names `ssc=true` appends with no
  receiver, and needs no subscription to its own stream: one log fewer per driver.
- A publisher on such a channel no longer learns from `not connected` that nobody
  receives. `sender.bytes_unsent` and a `cavalry doctor` note say when a publication is
  alone with its recording; `udp-publication-not-connected` names the key as what to do
  when a recorded stream should take messages with no receiver.
- The sender holds one more counter per such publication, the recording's position,
  given and taken away by the conductor. The conductor frees a stopped recording's
  position counter only once the sender has answered that it reads it no more, as it
  frees a publication's own counters only after the sender's close.
- The cluster's log channel names no `ssc` and is unchanged. A cluster of one member
  over a driver can name it when the election's cards need one.
- The change lands in three parts: this ADR with the spec pages, the vectors, the
  channel parser and the refusals; the driver; then `cavalry doctor` and the pages.
  Between the first and the second, `ssc=true` on a publication's channel is answered
  with code 11 and a message that says so, as the position keys were (ADR 0059).

Revisit: when a user needs a spy that is not the recorder, a subscription on the
publisher's own driver; when a `cavalry:ipc` channel takes parameters; when a
publisher should be told apart "recorded alone" from "received", beyond the counter.

## References

- `spec/udp.md`, version 3, "The channel URI", "The sender", "What a reader rejects";
  `spec/client-protocol.md`, version 9, "Commands".
- [ADR 0025](0025-record-a-stream-in-the-driver.md), a recording alone connects a
  `cavalry:ipc` publication.
- [ADR 0041](0041-send-from-a-sender-thread-through-nix.md), a receiver connects a
  `cavalry:udp` publication.
- [ADR 0053](0053-add-multi-destination-cast-to-cavalry-udp.md), `fc`, the key this one
  is read like.
- [ADR 0097](0097-count-the-recorder-among-a-cavalry-udp-publications-readers.md), the
  recorder as a reader of a `cavalry:udp` publication.
- Aeron's wiki, "Channel Configuration" and "Flow And Congestion Control"
  (`github.com/aeron-io/aeron/wiki`), read on 2026-11-12.
- AppShapes Trello card 271.
