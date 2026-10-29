# 0022. Load a publication's producer position at most once per millisecond

- **Status:** Superseded by 0032
- **Date:** 2026-10-13
- **Invariants affected:** 1, 2, 4
- **Supersedes:** none; amends `spec/log-buffer.md`, "The driver", Positions

## Context

ADR 0016's Revisit asked for the cause of `claim_commit_32_exclusive`'s band on the bench
host: its p99.99 lands anywhere between 270 and 430 ns from one process to the next while
the other rows hold within 2 percent, so the gate compares every row with the baseline's
slowest run instead of its median. Card 86 made finding it the first measurement on the
host. The host's p50 of 130 to 140 ns on that row, against 30 to 50 ns on the shared
runners, was the other open number (ADR 0016, Evidence).

Measured on 2026-10-13 on the bench host (Ryzen 7 2700X, tuned by
`bench/host/setup-ubuntu.sh`, publisher, subscriber and conductor pinned to CPUs 8, 10
and 12, the log under `/dev/shm`, 5,000,000 messages per run, the harness of commit
6dc79c4 with experiment knobs that stayed in a scratch clone), p99.99 in nanoseconds:

- The limit moves are not the band. Splitting the publisher's samples by whether the
  limit it loaded had changed: 1,221 limit-moved samples per run at p99.99 170 to 240,
  and 0 or 1 of the 500 slowest samples among them, in five runs of each log type.
- Interrupts are not the band. Over a whole pass, CPUs 8, 10 and 12 took 15, 0 and 0
  local timer interrupts and under 10 function-call and TLB interrupts each.
- Address layout is not the band. Three processes with address randomization off
  (`setarch -R`) read medians of 400, 340 and 400 with runs from 270 to 420; three with
  it on read 390, 390 and 400 with runs from 330 to 430. The band is inside every
  process.
- The conductor's loads of the producer position are the band, and the host's p50. The
  harness's conductor, like the driver's, loads the producer position, the word the
  publisher stores on every commit, on every spin. Loading it every N spins instead,
  seven runs of each:

  | Load every | `claim_commit_32_exclusive` p50 / p99 / p99.99 | `claim_commit_32_concurrent` p50 / p99 / p99.99 | `one_way_32_exclusive` p99.99 |
  | --- | --- | --- | --- |
  | spin, as today | 130 / 150 / 290 to 430 | 140 / 210 / 430 to 450 | 650 to 660 |
  | 10 spins, about 1 µs | 130 / 141 / 410 to 440 | 140 / 210 / 410 to 460 | 660 to 670 |
  | 100 spins, about 10 µs | 90 / 140 / 230 to 270 | 90 / 160 / 260 to 290 | 580 to 620 |
  | 1,000 spins, about 100 µs | 90 / 140 / 220 to 240, one run 340 | 90 / 160 / 220 to 240 | 580 to 610 |
  | 10,000 spins, about 1 ms | 90 / 100 / 220 to 260 | 90 / 160 / 220 to 250 | 570 to 610 |

  Throughput rose from 4.7 to 6.0 million messages per second when the loads stopped.

Every commit stores to the line that holds the producer position. A core that loads
that line every 100 ns keeps it moving between the two caches, so each store first
requests the line back, which is the host's p50 against the shared runners', and a load
that lands while a store is in flight costs that store two or three transfers, which is
the band. The exclusive row shows a band because its stores are posted and otherwise
cheap; the concurrent row's compare-and-swap waits on the line every time, so its tail
sits at the collision cost steadily and hides the same cause.

The driver's conductor does what the harness's did: `keep_publication` loads every
publication's producer position on every duty cycle, stores it in
`publication.position` when it moved, and counts the move as work, so while a publisher
publishes the conductor never idles, whatever its idle strategy, and spins on the
publisher's line. `spec/log-buffer.md` bounds the traffic in the other direction, the
limit, which the driver raises only in steps of window ÷ 8 "so that the publisher's
cache line is not written on every cycle". It does not bound the load.

## Decision

The driver loads a publication's producer position at most once per millisecond per
publication, by its own clock, which it reads once per duty cycle; it stores
`publication.position` when the loaded value moved, and unblocking compares the
consumer position with the position so loaded. The loaded value is never above the
real producer position, so unblocking's test that the producer position is beyond the
consumer position can only pass late, by up to 1 ms against the unblock timeout, and
never wrongly. The `rejected_tail` check runs on the same load, so a corrupt tail is
caught, and its client evicted, up to 1 ms later than before. Closing loads the producer
position afresh, because the end-of-stream position is the last byte the publisher
committed and a stale one would drop up to a millisecond of committed frames (invariant
1). Joining loads it afresh too, when a subscription joins the publication at the
producer position (`spec/client-protocol.md`, "add subscription"): a stale value can be
below the clean position, because another subscriber has read to the real producer
position and cleaning has zeroed the log up to there. The new subscriber would read a
frame length of 0 in the cleaned range, never advance, hold the limit down as the
slowest subscriber, and back-pressure the publisher for good, while unblocking never
fires because the other subscriber keeps moving the consumer position (invariant 1). A move of
the producer position is not work for the conductor's idle strategy; the subscriber
positions, cleaning and the limit are loaded, done and raised on every cycle as the spec
already says, because a publisher waits on the limit and a subscriber's stores are
already one per poll. The cadence is 1 ms with no configuration key: it is the backoff
strategy's longest park and a thousandth of the heartbeat, and nothing reads
`publication.position` faster. `spec/log-buffer.md`, "The driver", Positions, gains the
sentence in this commit; the driver follows in its own commit with the bench host's
histograms before and after; the harness's conductor takes the same cadence in a third,
so that the raw rows measure the log and not the harness; and the host's baseline is
re-recorded from that run in a fourth, where `claim_commit_32_exclusive` joins the
other rows under the rule ADR 0016's Revisit named, the median against the median,
once the band has closed in the recorded runs.

## Alternatives considered

- **Leave it, and keep comparing every row with the baseline's slowest run.** Lost: the
  host's p50 and the band are the conductor interfering with the publisher, not the
  publisher's cost, and the gate would hold the data plane to the wrong number.
- **Load the producer position only on a duty cycle that found no other work.** Lost: a
  busy conductor is exactly when a publisher is busy, so the bound must hold then too.
- **A configuration key for the cadence.** Lost: no `why_not_default` survives. 1 ms is
  the backoff strategy's longest park, and a reader of `publication.position` that
  needs it fresher than that has a publication of its own to ask.
- **Have the publisher store its position into the counter itself.** Lost: another
  store per message on the publisher, into a line the conductor and `doctor` read
  (invariant 2), and a departure from Aeron, whose driver owns the counter.
- **Bound the subscriber position loads the same way.** Deferred: the measurement
  throttled only the producer load, and the subscriber's line sees the same traffic from
  the conductor, which may be why `one_way` fell less than `claim_commit`. Its own
  measurement comes with the sender thread, which reads the tail for the wire and needs
  its own answer; see Revisit.

## Evidence

The runs above, on the bench host on 2026-10-13, posted in full on card 86. The numbers
the commits after this one record on the host, through CI's `bench (linux)` job, are in
their messages: the driver commit shows the client rows, the harness commit shows the
raw rows, and the baseline commit records both profiles from the harness commit's run.

## Consequences

Easier: the publisher's p50 and p99.99 on the bench host fall by about a third; the
exclusive row's band closes, so the gate compares every row the same way; the conductor
idles while a publisher publishes and nothing else moves, which is what the backoff
strategy promises a laptop. Harder: one cadence to keep in both the driver and the
harness; a new baseline for the host; `publication.position` is up to 1 ms behind the
publisher, which `doctor` cannot tell from a publisher that paused, and the counter's
doc says so.

Revisit: when the sender thread arrives, which loads the tail for the wire and must be
measured the same way, together with the subscriber position loads; when a client needs
a producer position fresher than 1 ms from the counters file; when the harness no longer
needs a conductor of its own, which ADR 0015 chose for the raw rows.

## References

- `spec/log-buffer.md`, "The driver", Positions and Closing; `spec/client-protocol.md`,
  "add subscription", the join position; ADR 0013, the layout and
  the positions; ADR 0015, the harness; ADR 0016, the host, its Revisit and its Evidence
  with the p50 question; ADR 0017, which rows the gate reads
- `crates/cavalry-driver/src/conductor.rs`, `keep_publication`; `bench/src/main.rs`,
  the harness's conductor thread
- Card 86 on the AppShapes board, the closing comment of 2026-10-13 with the full tables
