# 0099. Let a cavalry:udp publication's channel name its term length

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 3, 4, 5, 6
- **Supersedes:** none. Amends ADR 0059, which left `term-length` out, and ADR 0098,
  whose refused join gets a counter of its own

## Context

Every `cavalry:udp` publication's log has one term length, 16 MiB. The log is three
terms and a metadata page, 48 MiB, and it is resident from its creation (ADR 0027). An
image's log is as long as its sender's. A driver that holds many streams of a low rate
pays 48 MiB for each: on 2026-11-12, five instances of a server that record one feed
each and follow the four others held ten logs each, 488 MiB of `/dev/shm`, on Linux
7.0, where a container's default `/dev/shm` is 64 MiB. Their feeds carry a few
kilobytes a second.

ADR 0059 gave a publication's channel the position keys `init-term-id`, `term-id` and
`term-offset` and left `term-length` out, because "this version's one term length would
make a key that does nothing". Everything but the conductor already takes a term
length as a value: a receiver takes its image's from the setup frame, a replay's
publication takes its recording's (ADR 0043), and the sender, the recorder and the
replayer read the log's header. The driver's tests have run `cavalry:udp` publications
on a 64 KiB term since ADR 0041, through the driver's own term length.

ADR 0098 left one thing to this decision. A receiver cannot join a publication that is
alone with its recording when the publisher writes more than a term between a setup
frame and the receiver's answer. A short term is reached sooner.

## Decision

A `cavalry:udp` publication's channel may name `term-length=<length>`. The value is
decimal digits, optionally followed by one of `k`, `m` or `g` in either case, for
× 1024, × 1048576 and × 1073741824. The length is a power of two from 65536 to
67108864. The lower bound is the shortest term a log has (`spec/log-buffer.md`). The
upper bound is the `cavalry:ipc` term, the longest term this version creates. A channel
that names none has the 16 MiB term, as before, so the default is unchanged
(invariant 5). The publication's log, its publication window, its longest message and
the receiver window follow from the term as `spec/log-buffer.md` and `spec/udp.md`
already say, and the position keys count in it. The term length is part of what a
publication is: an `add publication` that would join a live publication under another
term length is refused with code 1 and the reason `term-length`, as under another `fc`.
A subscription's, a destination's and a replay's channel that names the key is refused
with code 1, since an image takes its sender's term length and a replay's publication
its recording's. On a `start recording` channel the key takes no part. `cavalry:ipc`
takes no key: its grammar has no parameters in this version, and its one term length
is the driver's.

A status message that would join a publication alone with its recording and names a
position more than a term behind is counted in `sender.joins_refused`, a counter of the
publication's own, id 142, beside `sender.status_messages_rejected`. The refusal of a
message that is too long names the key. `cavalry doctor` reads both from counters, in
the change to `cavalry-control` that follows the driver's: it warns with
`udp-publication-join-refused` while that counter is above 0 and the publication is
still alone, naming `term-length`, and `recording-stalled` says when the room it reads
is a short window.

The rule for choosing a term: it holds what the publisher writes in 16 milliseconds at
its fastest. That is the peak rate in bytes a second ÷ 64, rounded up to a power of
two: 64 KiB for 4 MiB a second, 1 MiB for 64 MiB a second, the default for 1 GiB a
second.

## Alternatives considered

- **A driver configuration key for the UDP term length.** One value for every stream
  of a driver, where the need is per stream: a feed of a few kilobytes a second beside
  a stream that needs the default. A configuration key also needs a reason why the
  default cannot be right (invariant 5), and the default is right for a stream that
  names nothing. In Aeron, this is the `term-length` channel parameter, and the name
  and the place are copied.
- **A shorter default.** It would change the back-pressure and the join of every
  deployment that names nothing, and the cluster's log channel counts its positions in
  the 16 MiB term (`log_channel_at` in `cavalry-raft`). The default has to carry the
  fastest stream with no configuration.
- **A log that is not resident until it is written.** That is ADR 0027's decision,
  made for the tail (invariant 2): a page fault on the first write of each page is a
  latency the publisher cannot bound.
- **Bytes only, no suffix.** `term-length=1048576` is read wrong more easily than
  `term-length=1m`, and Aeron's parameter takes the suffixes.
- **An upper bound of 1 GiB**, which is the longest term a reader of
  `spec/log-buffer.md` accepts. A 1 GiB term is a 3 GiB resident log, and nothing
  measured needs one. Raising a bound later is compatible; lowering one is not.
- **`term-length` on a subscription's channel.** An image's log has its sender's term
  length, from the setup frame. A key there would be a second source for one fact.
- **Hold the sender position after each setup frame, so that every join succeeds.**
  The sender would stop following the recorder for some milliseconds after a setup
  frame, ten times a second, and a publisher with no receiver would read
  `back-pressured` in each. The cost falls on every stream that is recorded alone, to
  serve the one whose term is too short. The counter and the `doctor` finding say
  which stream that is, and the remedy is one key.
- **Count the refused join in `sender.status_messages_rejected` alone.** That counter
  is the driver's, not the publication's, and counts three other refusals. `doctor`
  could not say which publication a receiver failed to join, or why (invariant 4).

## Evidence

All of it on 2026-11-12, on an Apple M5 Pro laptop, macOS, loopback, unpinned, with a
scratch program over the client API of this change's branch.

- **What a short term saves.** Three instances that each record one feed and follow
  the two others, feeds on `ssc=true`, five logs each. With the default term: 248 MiB
  of driver directory and 404 MiB resident for each instance. With
  `term-length=256k`: 11.8 MiB of driver directory and from 21.7 to 22.9 MiB resident.
  The same twelve checks pass both ways: every record read once and in order on every
  feed, across a roll of the recording, a purge and a restart of one instance.
- **The join, by term and rate.** One driver publishes 1,024-byte messages at a fixed
  rate on a channel that names `ssc=true`, recorded and alone. After one second a
  subscriber on a second driver subscribes. "Joins" is a first message within four
  seconds; it came from 66 to 89 milliseconds after the subscription, which is the
  wait for a setup frame. "No" is an image that received nothing.

  | Term | 0.25 MiB/s | 4 MiB/s | 16 MiB/s | 32 MiB/s | 128 MiB/s |
  | --- | --- | --- | --- | --- | --- |
  | 64 KiB | joins | joins | joins | no | no |
  | 256 KiB | joins | joins | joins | joins | no |
  | 1 MiB | joins | joins | joins | joins | joins |
  | 4 MiB | not run | not run | joins | not run | joins |
  | 16 MiB, the default | joins | joins | joins | joins | joins |

  A join failed when a term was written in about 2 milliseconds and succeeded when it
  took 4 or more. The rule's 16 milliseconds leaves room for a network's round trip.
- **Back-pressure, by term and rate.** In the same runs the publisher retried at once
  when an offer was refused. It reached the asked rate in every run but one: the 64 KiB
  term at 32 MiB a second sent 28.1 MiB a second. It read `back-pressured` at least
  once in every run of the 64 KiB term from 4 MiB a second, of the 256 KiB term from
  16 MiB a second, and of the 1 MiB and 4 MiB terms at 128 MiB a second, and in no run
  of the default term. A short term gives a short window, half the term, and a
  publisher that passes it waits for the recorder and the conductor.
- **Tests.** `a_udp_publication_takes_its_term_length_from_its_channel`,
  `a_udp_publications_term_length_is_held_to_its_bounds` and
  `a_short_term_stream_is_recorded_alone_across_terms_and_joined_over_a_terms_end` in
  `cavalry-driver`: the last records nine 64 KiB terms, joins a receiver over the end
  of a term, and replays the recording. The refusals are in
  `destination_commands_and_a_control_channel_are_refused_with_the_spec_codes`. Each
  vector of `spec/vectors/udp.json`, version 4, is read by
  `crates/cavalry-proto/tests/udp_conformance.rs`.

## Consequences

- A stream's memory is its caller's to choose within stated bounds (invariant 3). A
  server of low-rate feeds fits a container's default `/dev/shm`.
- A caller can now choose a term that is too short. Three things say so. A message
  over the term ÷ 8 is refused at the call, and the refusal names the key. A window
  that is short shows as `back-pressured` at the call and in `recording-stalled`. A
  receiver that cannot join shows in `sender.joins_refused` and in `doctor`.
- The refused join of ADR 0098 stays: a receiver does not join a publication alone
  with its recording while the publisher writes more than a term in the time a join
  takes. The default term does not meet it below gigabytes a second.
- A publication under `ssc=true` holds seven counters, one more than before.
- The cluster's log channel is built by the consensus module and names no
  `term-length`, so `log_channel_at` and the live-add window, which count in the
  16 MiB term, are unchanged. A cluster log on another term is a later decision.
- `spec/udp.md` is at version 4 and `spec/client-protocol.md` at version 10. A reader
  of the earlier version refuses a channel that names the key, as a key it does not
  name. No frame changes.
- Revisit the bounds if a stream needs a term over 64 MiB, and the held sender
  position if `udp-publication-join-refused` fires for streams whose term already
  follows the rule.

## References

- `spec/udp.md`, version 4: "The channel URI", "The sender", "What a reader rejects",
  "Values". `spec/client-protocol.md`, version 10: "Commands", "Sizes".
- `spec/vectors/udp.py` and `spec/vectors/udp.json`.
- `counters.toml`, `sender.joins_refused`.
- ADR 0027 (resident logs), ADR 0041 (the sender), ADR 0043 (a replay's publication),
  ADR 0059 (the position keys), ADR 0097 (the recorder among the readers), ADR 0098
  (`ssc`), ADR 0101 (`recording-stalled` reads the limit).
- Card 272.
