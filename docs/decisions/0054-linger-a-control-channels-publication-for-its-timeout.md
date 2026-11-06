# 0054. Linger a control channel's publication for its timeout, and drop an ended receiver uncounted

- **Status:** Accepted
- **Date:** 2026-11-06
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Amends `spec/udp.md`, "The sender" and "Counters", within
  version 1, in two rules that version 0 wrote for one receiver and ADR 0053 did not
  restate for many; nothing on the wire changes

## Context

`spec/udp.md` version 0 ends a closed publication's linger "for the linger timeout, or
until a status message with the end-of-stream flag arrives". A channel with an endpoint
has one receiver, and a receiver sets that flag only once its image has ended, which is
after the sender's heartbeat with EOS and after the rebuild position reached the end of
stream (`spec/udp.md`, "The receiver", step 5). So the flag says the one receiver holds
the whole stream. ADR 0053 gave a control channel many destinations and kept this rule.
The first destination's flag then ends the linger for all of them. The sender removes
the publication, finds nothing for its session after that, and drops every other
destination's NAK unanswered. That includes the datagrams that ADR 0053 leaves to NAK
recovery when the socket refused a destination. A slower destination that lost a
datagram near the end never reaches its end of stream; its image times out after the
image liveness timeout, 10 s, with the tail missing. The review of pull request 152 on
2026-11-06 found this, blocker 1.

ADR 0053's `min` table drops a receiver "whose status message is older than the flow
control receiver timeout, or whose last status message carried end of stream", and
counts both in `sender.flow_control_receivers_timed_out`. Every receiver sets the flag
after every clean close, so every clean close of a `min` publication raised the
counter, and `cavalry doctor`'s `udp-flow-control-receiver-timed-out` reported each one
as a receiver that held the stream back for the timeout. The table also added an entry
for a status message with the flag from a receiver it did not hold, so a receiver's
five status messages with the flag (step 5) added, dropped and counted it five times
once the publication lingers. The same review found this, blocker 3.

What Aeron does, read on 2026-11-06 at `master`, version 1.55.0-SNAPSHOT.
`NetworkPublication.onStatusMessage` sets `hasReceivedUnicastEos`, which ends the
linger early, only when the channel endpoint is neither multicast nor multi-destination
cast; a multi-destination publication lingers for the full linger timeout and answers
NAKs throughout. `AbstractMinMulticastFlowControl.processStatusMessage` adds a receiver
only when the status message does not carry end of stream (`!isEos`), and its idle path
removes a receiver whose last status message carried end of stream as it removes one
that timed out. Aeron keeps no counter of either removal.

## Decision

On a control channel a status message with the end-of-stream flag does not end the
linger: the closed publication lingers for the linger timeout, 5 s, sending heartbeats
with EOS and answering every destination's NAKs, and then its log is removed. On a
channel with an endpoint the flag ends the linger as version 0 says, since its one
receiver has the stream to its end. Under `min`, a status message with the flag from a
receiver the table does not hold adds no entry. An entry whose last status message
carried the flag is dropped on the next duty cycle and is not counted: its image ended,
as every image does after a close. The receiving driver counts that end in
`image.ended`, and the gauge `sender.flow_control_receivers` shows the drop on this
host. `sender.flow_control_receivers_timed_out` counts only a receiver dropped for the
flow control receiver timeout, which is the case `doctor` reports. `spec/udp.md` says
so in "The sender" and "Counters". The page stays at version 1: nothing on the wire
changes, and no driver of version 1 has been released.

## Alternatives considered

- **End the linger once every destination's receiver has sent the flag.** Lost: the
  sender does not know how many receivers a destination has, or whether a destination
  has any. Under `max` it keeps no table of receivers at all. A destination with no
  subscription would hold the linger for its full timeout anyway, so the rule costs
  the sender a table and gains nothing over the timeout. Aeron does not do it either.
- **Keep the rule, and leave the tail to the image liveness timeout.** Lost: the image
  ends with frames missing that the sender still held, which breaks invariant 1: a
  message is delivered once, or its absence is reported.
- **Count an ended receiver in a counter of its own**, such as
  `sender.flow_control_receivers_ended`. Lost: the end of an image is already counted,
  on the host where it happens, in `image.ended`. The gauge falls when the receiver
  leaves the table, and no `doctor` check needs the count on the sender's host. A
  static counter for the normal end of every image explains nothing a reader asks
  about.
- **Keep one counter for both drops, and have `doctor` warn only when it rises while
  a publication is live.** Lost: a counter that mixes the normal case with the fault
  cannot show the fault. `doctor` reads one sample of the counters file and cannot
  tell when the counter rose.
- **Add an entry for a status message with the flag, as the table did.** Lost: the
  entry is dropped on the next cycle in any case. While the publication lingers, each
  of a receiver's five status messages with the flag would add it again, which is
  churn with no use; Aeron adds none.

## Evidence

None measured: two rules for a case version 0 did not have, with Aeron's sources
named in Context. The change that accepts this ADR carries the page and the sender.
`a_control_channel_lingers_past_one_destinations_end_of_stream` in
`crates/cavalry-driver/src/conductor.rs`: two destinations, a status message with the
flag from one after the close, the publication still held and answering the other
destination's NAK, and closed only once the linger timeout has passed. The test that
already covered a channel with an endpoint, where the flag ends the linger, still
passes unchanged. `min_flow_control_follows_the_slowest_receiver_and_drops_one_that_times_out`
drops a receiver whose status message carried the flag with
`sender.flow_control_receivers_timed_out` unmoved, and adds no entry for the flag from
a receiver the table does not hold. `doctor`'s
`a_clean_close_of_a_min_publication_leaves_doctor_with_no_warning` reads the counters
file a clean close leaves and finds no warning.

## Consequences

Easier: every destination of a control channel can recover the tail of a closed
stream for the linger timeout; `sender.flow_control_receivers_timed_out` and the
`doctor` check on it mean one thing, a receiver that stopped answering.

Harder: a control channel's endpoint closes no earlier than 5 s after its last
publication's close, so its socket, its six counters and its registry slot are held
that long, which the send endpoints registry of 64 already bounds; a client that
removes and adds a publication on the channel within that time gets the endpoint that
is still open.

Revisit: when a cluster needs a control channel's publication to close in less than
the linger timeout, for a linger of its own with a `why_not_default`; when Aeron
changes the rule for a multi-destination publication.

## References

- `spec/udp.md`, "The sender", "The receiver" step 5, "Counters"; ADR 0040, ADR 0041,
  ADR 0053
- `crates/cavalry-driver/src/sender.rs`, `on_status_message` and `serve`;
  `crates/cavalry-control/src/doctor.rs`, `udp_checks`
- Aeron, read 2026-11-06 at `master` (1.55.0-SNAPSHOT):
  `aeron-driver/src/main/java/io/aeron/driver/{NetworkPublication,AbstractMinMulticastFlowControl}.java`
- The review of pull request 152, 2026-11-06, blockers 1 and 3; AppShapes Trello card
  225
