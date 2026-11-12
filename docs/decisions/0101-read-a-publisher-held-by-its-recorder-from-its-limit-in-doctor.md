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
- **A whole window is not what a held publisher shows.** The conductor raises a limit
  in steps of an eighth of the window, so the limit stands up to an eighth short of
  the reader's position plus the window (`spec/log-buffer.md`, "The driver"). A claim
  is refused when the publisher's position is not below the limit, and a claim that
  starts below it is reserved whole, wherever it ends ("Claim, commit and abort",
  step 1). So a held publisher stands at its limit, or up to a message past it. That
  is a whole window past its slowest reader only when the limit was raised to the
  full window or the last message makes up the difference.
- **The recording was placed by session id and stream id alone.** A recording's label
  names those and not what it reads. A driver that receives its own stream has an
  image with them too, and two publications on different channels can share them.

## Decision

`recording-stalled` reads the room from the publication's own counters and holds no
window. For a recording it can place, the room is `publication.limit` less
`recording.position`, and the check warns when `publication.position` has reached
`publication.limit`: that is the condition a claim is refused on, so the finding names
a publisher that is held now, and no publisher that is not. It places a recording only when one publication
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
- **Warn from a fraction of the room, as "about to be held".** Two were built on the
  card's branch: a half, and five sevenths, the second from an arithmetic that had the
  claim rule backwards, which the review of pull request 244 found. Lost: a publisher
  below its limit is not refused, so a fraction names a recorder that holds nobody. A
  recorder that pauses for one sync at a high rate crosses any fraction. And one
  reading cannot say where a moving publisher will be at the next.
- **Name what a recording reads in its label, the publication's or the image's
  registration id.** It would place every recording. Lost for now: the label is
  `spec/client-protocol.md`'s, and the two cases the check now leaves alone are rare.
  The Revisit clause keeps it.

## Evidence

- **The rule.** `spec/log-buffer.md`, "Claim, commit and abort", step 1: "If the
  position is not below the publisher limit [...] the claim fails." `before_reserve`
  in `crates/cavalry-shm/src/log/layout.rs` returns the position when it is below
  the limit, before it looks at the length. With window W, the limit L is raised to a
  reader's position plus W whenever that is at least W ÷ 8 past the last limit. So
  with the recorder at R as the slowest reader, L − R is more than 7W ÷ 8 and at most
  W, and a held publisher's position P is at L or up to one message past it: P − R is
  at least L − R, every time. `publication.position` is written at most 2 ms behind
  the publisher (ADR 0032), and a held publisher does not move, so the gauge shows it.
- **Tests**, in `crates/cavalry-control/src/doctor.rs`:
  `recordings_are_listed_and_a_recorder_that_holds_its_publisher_warns`, a
  `cavalry:ipc` publication at its limit, a message past it, and a frame short of it,
  which is not held;
  `a_held_publisher_is_read_from_its_limit_and_its_slowest_reader`, a `cavalry:udp`
  publication left as the driver leaves a held one, a log with a 1 MiB term a message
  past its limit, nothing a frame short of the limit, and nothing when the sender or a
  subscriber is behind the recorder; `a_recording_that_cannot_be_placed_is_not_judged`,
  an image and a second publication with the recording's session id and stream id.
- **What the driver leaves**, from ADR 0097's test: with the recorder stalled at 0, a
  `cavalry:udp` publisher stops at 8,388,608 with its limit there.

## Consequences

- The back-pressure ADR 0097 added over `cavalry:udp` is named by `doctor`, and the
  same back-pressure over `cavalry:ipc` is named when it happens, not only when the
  limit stands a whole window past the recorder.
- The check needs no change when a channel names its term (card 272).
- A publisher that reaches its limit for a moment, while its recorder waits for one
  sync, is named by a reading that meets that moment. The message says to run `doctor`
  again.
- A `cavalry:udp` publication that loses its receivers has its limit lowered to the
  sender position, and its publisher reads `not connected`. While the recorder is
  still behind the sender, the check is met, and the finding says back-pressured
  where the publisher reads `not connected`. It ends when the recorder has written
  what the sender sent, which is at once unless the disk is slow. The counters do not
  tell the two apart for one publication.
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
