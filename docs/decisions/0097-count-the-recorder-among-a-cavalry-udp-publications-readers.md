# 0097. Count the recorder among a cavalry:udp publication's readers

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 1, 3
- **Supersedes:** none. Amends ADR 0041's rule for the limit and the cleaning of a
  `cavalry:udp` publication, for one that is recorded

## Context

ADR 0025 gave a recorded publication one more reader, the driver's recorder: "The
publisher's limit for a recorded publication is the window past the lesser of the
subscribers' positions and `recording.position`, and cleaning uses the same lesser
position, so a slow disk back-pressures the publisher and no recorded byte is cleaned
before it is written; the recorder is never lossy." `spec/log-buffer.md`, "The
driver", says the same of every recorded publication.

ADR 0041 then gave a `cavalry:udp` publication a rule of its own, since its readers are
on other hosts: the sender position is the consumer position, the limit runs the
publication window ahead of it while a receiver answers, and the log is cleaned a term
length behind it, so that the resend window stays. `spec/udp.md`, "The sender", states
that rule and names no recording. The conductor computes the recorder's position for
every publication and uses it for `cavalry:ipc` alone.

So over `cavalry:udp` nothing holds the log for the recorder. The window is half a
term, 8 MiB, and the cleaning runs 16 MiB behind the sender. A recorder that is more
than a term behind the sender reads bytes the conductor has already zeroed, while the
receivers hold them. The recorder writes to a disk and syncs it, once a second and on
command; a sync of one small record took 8.5 ms at p50 and 54 ms at the longest on an
Apple M5 Pro laptop on 2026-11-12, and a sender at 1 Gb/s passes 16 MiB in 0.14 s.
Every recorded `cavalry:udp` publication is exposed: the cluster leader's log
(`spec/cluster.md`, "Members and streams"), and a feed that other hosts follow through
replays. Card 272 would let a channel name a shorter term, which shortens the margin
to that term's length.

## Decision

For a recorded `cavalry:udp` publication the consumer position is the lesser of the
sender position and the recording's position, `recording.position`, which stands at
the recording's start position until the recorder has attached the counter. The
publisher's limit is the publication window past that position while the publication
is connected, and the log is cleaned up to a term length behind the sender position
and no further than the recording's position. Two things stay the sender position
alone: where the limit rests while the publication is not connected, and where the
log is unblocked. A replay's publication that is itself recorded follows the same
rule. A publication that is not recorded keeps ADR 0041's rule unchanged. No frame,
channel key, command or counter changes, and `spec/udp.md` stays at version 2.

`cavalry doctor`'s `recording-stalled` is the finding for a publisher held by its
recorder. It compares the recorder's lag with the `cavalry:ipc` window, 32 MiB, which
a `cavalry:udp` publication held 8 MiB past its recorder never reaches. The card's
control-plane change gives the check the publication's own window; the driver's
change does not wait for it.

## Alternatives considered

- **Leave the rule, and stop a recording that the cleaning has reached, with an
  error.** The loss would be reported, at the position where the recording stops.
  Lost: ADR 0025 decided that a recorder back-pressures and is never lossy, and a
  recording that stops short while receivers hold the rest breaks what its users
  build on it. The cluster counts a leader's append position from its recording, and
  a feed's followers ask the recording for what they missed.
- **Hold the sender at the recorder's position too**, so that no receiver is sent a
  byte the publisher's disk does not hold. Lost: nothing asks for it. A replay of a
  live recording already plays no further than the synced position, and the cluster
  commits on the positions a quorum has recorded, not on the leader's disk. It would
  also put the disk between every message and the wire, which invariant 2 forbids
  without a measurement that says it is free.
- **Clean a term length behind the lesser position.** Lost: the term kept behind the
  sender position is the resend window, which a receiver's NAK reads. The recorder
  reads forward from its own position and needs nothing behind it, as over
  `cavalry:ipc`, where the log is cleaned up to the consumer position itself.
- **Unblock at the lesser position, and rest the limit there too.** Lost: a
  publisher that died inside a claim stops the sender at that frame wherever the
  recorder stands. Unblocking where the recorder stands finds a committed frame and
  does nothing, so the sender and every receiver would wait for the recorder to reach
  the frame and then for the unblock timeout again. And a publication that is not
  connected is refused by its flag, not by its limit, so lowering the limit below the
  sender position would change what `publication.limit` reads and nothing else.
- **A counter or a state for a publisher held by its recorder.** Lost: three counters
  already say it, `publication.limit`, `sender.position` and `recording.position`, and
  `recording-stalled` is the finding that reads a recorder's lag. What it lacks is the
  right window over `cavalry:udp`, which is a change to the check and not a new
  counter.

## Evidence

- **The fault, on `master` at 78625e9.** A test in the conductor's `recording` module
  adds a `cavalry:udp` publication, connects it with a status message, starts a
  recording of it, runs the recorder until its counter is attached and then no more,
  and lets a receiver acknowledge everything the sender sends. It failed with:
  "cleaned to 22528 with the recorder at 0, the sender at 16799744 and the
  publisher's limit at 24270848".
- **The same test after the change**, `a_recorded_udp_publication_waits_for_its_recorder`:
  with the recorder stalled the sender reaches 8,388,608, one window past the
  recorder, and stays there; the limit stays at 8,388,608 and nothing is cleaned.
  Once the recorder runs again the stream passes a term and a half, 25,165,824, and
  at every step the clean position is at most the recorder's position and at most a
  term length behind the sender position. The recording is then stopped, and its
  bytes fold to the same number as the messages offered. With the change taken out
  the test fails at "the limit is 9447424 with the recorder at 0".
- **Rotation stays safe.** A term enters a partition only when the partition is zero
  (`spec/log-buffer.md`, "Rotation"). With term length T, the producer enters term
  c + 1 only at a limit of at least (c + 1) × T, so the consumer position is at least
  (c + ½) × T, and so are the sender position and the recorder's. The conductor
  raises the limit only after it has cleaned up to the lesser of the sender position
  less T and the recorder's position, which is then at least (c − ½) × T, past the
  end of term c − 2, the term that held the partition.
- **Aeron does the same.** Its driver takes the publisher's limit from the least of
  the sender position and the spies' positions, and its archive records a publication
  through a spy.

## Consequences

- A disk that stalls now slows a publisher over `cavalry:udp`, as it does over
  `cavalry:ipc`: the publisher reads `back pressured` once it is a window past the
  recorder. Before, the publisher went on and the recording was at risk.
- The cluster leader's log publication is held by the leader's own recorder. The
  consensus module offers an entry again when an offer is refused, so it needs no
  change.
- `recording-stalled` in `cavalry doctor` does not yet fire for a `cavalry:udp`
  publication: until the card's control-plane change lands, a publisher held by its
  recorder over UDP is read from `publication.limit`, `sender.position` and
  `recording.position` by hand.
- Cards 271 and 272 build on this rule: a recording that connects a `cavalry:udp`
  publication takes its limit from the recorder alone, and a shorter term has a
  shorter margin.

Revisit: when a user needs a recording that may fall behind and lose bytes without
holding its publisher, which no page promises.

## References

- `spec/udp.md`, "The sender"; `spec/log-buffer.md`, "The driver".
- [ADR 0025](0025-record-a-stream-in-the-driver.md), the recorder as a reader.
- [ADR 0041](0041-send-from-a-sender-thread-through-nix.md), the sender position as
  the consumer position.
- `crates/cavalry-driver/src/conductor.rs`, `keep_publication`, and the test
  `a_recorded_udp_publication_waits_for_its_recorder`.
- AppShapes Trello card 270.
