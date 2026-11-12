# 0101. Read a publisher held by its recorder from its limit, in doctor

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 4, 6
- **Supersedes:** none. Amends what ADR 0025 named as the `recording-stalled` check,
  and does what ADR 0097 left to card 270's control-plane change

## Context

`cavalry doctor` has one finding for a recorder that holds its publisher back,
`recording-stalled` (ADR 0025). It compared `publication.position` less
`recording.position` with the `cavalry:ipc` publication window, 32 MiB, and warned when
the recorder was a whole window behind. Three things were wrong with that.

- **The window was a constant.** A `cavalry:udp` publication's window is 8 MiB, and
  since ADR 0097 the driver holds a recorded one that far past its recorder, which
  32 MiB never sees. A replay's publication takes its window from its recording's
  term, whichever channel carries it. Card 272 lets a channel name its term.
- **A whole window is rarely reached.** The conductor raises a limit in steps of an
  eighth of the window, so the limit stands up to an eighth short of the reader's
  position plus the window. A publisher is refused when its next message does not
  fit under the limit, so it stops up to a message short of it, and a message is at
  most a quarter of the window (`spec/log-buffer.md`, "The driver", "Claim, commit
  and abort"). A held publisher is a whole window ahead only when both line up.
- **The recording was placed by session id and stream id alone.** A recording's label
  names those and not what it reads. A driver that receives its own stream has an
  image with them too, and two publications on different channels can share them.

## Decision

`recording-stalled` reads the room from the publication's own counters and holds no
window. For a recording it can place, the room is `publication.limit` less
`recording.position`, and the check warns when `publication.position` less
`recording.position` is at least five sevenths of the room: every publisher held by
its recorder has used more than that. It places a recording only when one publication
on the driver has its session id and stream id and no image has them. It leaves a
recording alone when another reader under the publication's key, its `sender.position`
or a `subscription.position`, is behind the recorder, since that reader holds the
publisher then. The message gives both numbers and says to run `doctor` again, since
one reading cannot tell a recorder that stays behind from one that waited for a sync.

## Alternatives considered

- **A window for each transport, 8 MiB where the publication has a `sender.limit`.**
  It was built first, on the card's branch. Lost in review before it was pushed: a
  replay's publication over `cavalry:ipc` of a `cavalry:udp` recording has the 8 MiB
  window and no `sender.limit`, and card 272 would break it for every channel.
- **Read the window from the log's header.** It is the number the conductor uses.
  Lost: `doctor` explains from the counters file alone (invariant 4), which is what a
  copy of a driver directory taken after a fault still has.
- **A gauge of the window beside `publication.limit`.** Lost: one more record for
  every publication, for a number the limit and the slowest reader already give.
- **Warn from half the room.** It was the first branch's threshold. Lost: it is
  below what any held publisher has used, so it warns earlier for no gain, and a
  recorder that pauses for one sync at a high rate crosses it without holding
  anyone.
- **Name what a recording reads in its label, the publication's or the image's
  registration id.** It would place every recording. Lost for now: the label is
  `spec/client-protocol.md`'s, and the two cases the check now leaves alone are rare.
  The Revisit clause keeps it.

## Evidence

- **The arithmetic.** With window W, the limit L is raised to a reader's position
  plus W whenever that is at least W ÷ 8 past the last limit. So with the recorder
  at R as the slowest reader, L − R is more than 7W ÷ 8 and at most W, and W is
  less than 8 ÷ 7 of L − R. A held publisher is less than one message short of L,
  and a message is at most W ÷ 4, less than 2 ÷ 7 of L − R. So it has used more than
  five sevenths of L − R.
- **Tests**, in `crates/cavalry-control/src/doctor.rs`:
  `recordings_are_listed_and_a_recorder_that_holds_its_publisher_warns`, a
  `cavalry:ipc` publication a frame short of its limit;
  `a_held_publisher_is_read_from_its_limit_and_its_slowest_reader`, a `cavalry:udp`
  publication left as the driver leaves a held one, a log with a 1 MiB term read the
  same way, nothing just under five sevenths, and nothing when the sender or a
  subscriber is behind the recorder; `a_recording_that_cannot_be_placed_is_not_judged`,
  an image and a second publication with the recording's session id and stream id.
- **What the driver leaves**, from ADR 0097's test: with the recorder stalled at 0, a
  `cavalry:udp` publisher stops at 8,388,608 with its limit there.

## Consequences

- The back-pressure ADR 0097 added over `cavalry:udp` is named by `doctor`, and the
  same back-pressure over `cavalry:ipc` is named when it happens, not only when the
  limit and the publisher's last frame line up.
- The check needs no change when a channel names its term (card 272).
- A recorder that pauses for a sync while its publisher runs near its limit can be
  named once. The message says to run `doctor` again.
- A recording of a stream this driver also receives, or one of two publications that
  share a session id and a stream id, is listed and not judged.

Revisit: when a recording's label names what it reads; when a user reports a held
publisher that `doctor` did not name.

## References

- `crates/cavalry-control/src/doctor.rs`, `recording_checks`.
- [ADR 0025](0025-record-a-stream-in-the-driver.md), the check's first form.
- [ADR 0097](0097-count-the-recorder-among-a-cavalry-udp-publications-readers.md), the
  recorder as a reader of a `cavalry:udp` publication.
- `spec/log-buffer.md`, "The driver"; `spec/udp.md`, "The sender".
- AppShapes Trello card 270.
