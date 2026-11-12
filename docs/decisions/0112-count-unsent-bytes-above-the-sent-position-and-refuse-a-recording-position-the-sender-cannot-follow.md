# 0112. Count unsent bytes above the sent position, and refuse a recording position the sender cannot follow

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 4, 6
- **Supersedes:** none. Amends ADR 0098's rule for what a join takes off
  `sender.bytes_unsent`, and states what the sender does with a recording position it
  cannot follow, which ADR 0098 left unspecified

## Context

ADR 0098 lets a live recording connect a `cavalry:udp` publication whose channel names
`ssc=true`. While no receiver answers, the publication is *alone with its recording*.
The sender sends no data frame. It moves its sender position up to the recording's
position, `recording.position`, and counts the bytes it passes in
`sender.bytes_unsent`. When a status message names a position at most a term behind,
the sender goes back to that position and "takes the bytes it goes back over off"
the gauge (`spec/udp.md`, "The sender"). ADR 0098 gives the reason as "nothing in it
was sent".

That reason holds for a receiver's first join. It does not hold for a receiver that
comes back. A status message names the slowest reader's position (`spec/udp.md`, "The
receiver", step 4). That position trails what the receiver's image took by up to the
receiver window. A receiver whose status messages stop for the connection timeout
leaves the publication alone: a throttled or paused host, or loss on the return path,
is enough. Its next status message names a position below what was already sent to
it. The review of pull request 255 traced the case. The sender had sent to 640 KiB.
The receiver had read to 600 KiB. Then it was quiet for 5 s, and came back at 600
KiB. The sender went back 40 KiB and took 40 KiB off a gauge that held 0. The
driver's test of that case reads −2560 bytes against the sender of ccd7168: 320 bytes
passed while alone, less 2,880 bytes gone back over. A gauge below zero, beside a
`doctor` text that says nothing was lost, cannot be explained from counters
(invariant 4). The bytes gone back over are also sent again, and counted in
`sender.bytes_sent` again, which no text said.

ADR 0098 also left one case unspecified: a recording position that the sender cannot
follow. The sender of pull request 255 refuses a position that is not a multiple of
the frame alignment, or that lies more than two terms past the sender position. It
counts the refusal in `log_buffer.rejected_position`. That counter's text says a
client wrote an impossible position and was evicted, and its `doctor` text tells the
operator to rebuild or remove the client. Here no client is evicted and nothing is
logged, so the text sends the operator after a client that did nothing. Unspecified
behaviour is a bug in the spec (CLAUDE.md, Protocol).

## Decision

The sender keeps one more position per publication, its *sent position*: the highest
position a data burst has taken the sender position to. It is the publication's start
position until the sender has sent. Nothing at or above it was sent. While the
publication is alone, `sender.bytes_unsent` counts what the sender position passes
above the sent position. When the sender goes back for a receiver, it takes off the
gauge what it goes back over above the sent position: those bytes were passed while
alone and are now to be sent. What it goes back over below the sent position went to a
receiver before, the one that comes back among them. Those bytes were never counted,
and they are not taken off. The frames sent again from there are counted in
`sender.bytes_sent` and `sender.frames_sent`, as any data frame is. So the gauge is
never below zero.

A recording position that is not a multiple of the frame alignment, or that lies more
than two term lengths past the sender position, is not a position the recorder of this
publication's log can have written. The sender does not move to it. It counts it once
in `log_buffer.rejected_position`, evicts no client, and follows the recording again
once its position is one it can follow. The counter's text and its `doctor` text name
this case beside the client's. No frame, layout or version changes: `spec/udp.md` stays
at version 3.

## Alternatives considered

- **Go back no further than the sent position.** A receiver that comes back below what
  it was sent already has those frames in its image; its subscriber has only not read
  them. Sending from the sent position would send nothing twice, and
  `sender.bytes_sent` would count each byte once. Lost: a receiver whose image began
  below the sent position, at an earlier setup frame, before another receiver joined
  above it, does not have those frames. It would NAK them one NAK at a time, the cost
  ADR 0098 rejected in its fifth alternative. Going back to the receiver's own position
  keeps "the receiver has nothing to NAK" true for every receiver the sender goes back
  for.
- **Keep which bytes below the sent position were passed and not sent.** The gauge would
  be exact for any number of receivers: a byte passed while alone would come off
  whenever any receiver is sent it later, by a burst or by a retransmit. Lost: those
  bytes are a set of ranges, one for each time a receiver joined above where the sender
  had sent, bounded only by the term. A bounded sender would have to drop ranges when
  its table is full, and the gauge would be wrong again, with nothing to say so. The
  sent position is one `i64` and two comparisons, one per follow and one per join.
- **Clamp the gauge at zero.** Take off what the sender goes back over, as before, and
  never go below zero. Lost: the bytes passed while alone and the bytes sent before
  would still be mixed. A gauge that held bytes from an earlier time alone, bytes no
  receiver was ever sent, would lose them to bytes that were sent.
- **Follow any position the recording's counter holds.** No refusal and no counter.
  Lost: a position that is not a frame boundary puts the sender inside a frame. A setup
  frame or a heartbeat there names no frame's start, and a receiver that joined there
  would build its image from the middle of a frame (invariant 1). A position more than
  two terms ahead lies past what the log can hold.
- **A static counter of its own for the refused recording position.** Lost: the
  refusal is the check ADR 0010 makes of a subscriber's position, on the same kind of
  value, a position in the counters file that a process other than its writer can
  corrupt. One counter whose text names both cases keeps `doctor`'s finding in one
  place. A new static counter would also be the first with an id of 128 or more, which
  the tests and the bench that create a counters file of 128 records do not hold (ADR
  0098, Consequences).

## Evidence

- **The gauge below zero, and the fix.** The driver's test
  `a_receiver_that_comes_back_below_what_it_was_sent_takes_off_only_what_was_passed`
  builds the review's case on the loopback. The sender sends 100 messages of 64 bytes
  to a receiver. The receiver's status message says it read 60 of them. The receiver is
  quiet for the connection timeout, the publication is alone, and the sender position
  passes 5 more messages. The receiver comes back at 3,840 bytes. Against the sender of
  ccd7168 the gauge reads −2560. With the sent position it reads 320 before the join
  and 0 after it. The receiver is then sent every frame from 3,840 bytes on, in order,
  with no NAK, and `sender.bytes_sent` reads 6,400 + 2,880 bytes.
- **Why the gauge is never below zero.** At any time the gauge equals the bytes passed
  while alone below the sent position and never sent, plus the bytes from the sent
  position to the sender position when the sender position is above it. A follow adds
  the bytes it passes above both. A join takes off the bytes it goes back over above
  the sent position. A burst moves the sent position up past bytes that are then sent,
  and leaves the gauge as it was. No step takes off more than the second part holds.
- **Where the gauge may read more than went to no receiver.** The sender keeps the one
  sent position, not which bytes below it were passed. A byte passed while alone stays
  counted when a receiver whose image began below the sent position is sent it later,
  by a retransmit or after the sender went back for it. That takes two receivers, or two
  images of one, joined at different positions within a term. While one receiver at a
  time is joined, through any number of times it goes quiet and comes back, the gauge
  is exact. It never counts a byte that was sent before it was passed.
- **Why two terms bound a recording position.** While the publication is alone, the
  consumer position is the sender position, and the publisher's limit is a window past
  it. A window is at most half a term. After the sender goes back up to a term for a
  receiver, the producer position is at most a term and a half past the sender
  position. The recording's position is at most the producer position. A position more
  than two terms past the sender position is none the recorder wrote.

## Consequences

- `sender.bytes_unsent` never reads below zero, and `doctor` can read it as the bytes
  that went to the recording and to no receiver. With receivers joined at different
  positions it may read more than that, never less.
- The sender holds one more `i64` per publication, set on every burst; nothing more on
  the per-message path.
- `log_buffer.rejected_position` has two sources. `doctor` tells them apart by whether a
  client was evicted: an eviction is in the driver's log and in a `client timeout`
  event; the sender's refusal leaves a `ssc=true` publication's `sender.position` still
  while its `recording.position` moves.
- `spec/udp.md` states both rules in "The sender", and the gauge's in its Counters
  table too.

Revisit: when a stream with several receivers needs the gauge exact, or when the
driver's static counters may take ids of 128 or more and the sender's refusal can have
a counter of its own.

## References

- `spec/udp.md`, version 3, "The sender" and "Counters".
- [ADR 0010](0010-validate-peer-written-shared-memory.md), the bound checks that
  `log_buffer.rejected_position` counts.
- [ADR 0098](0098-let-a-recording-connect-a-cavalry-udp-publication-whose-channel-names-ssc.md),
  the decision this one amends.
- Pull request 255's review, blockers 2 and 3.
- AppShapes Trello card 271.
