# 0032. Load a publication's producer position at most once every two milliseconds

- **Status:** Accepted
- **Date:** 2026-10-29
- **Invariants affected:** 2, 4
- **Supersedes:** 0022, whose cadence this doubles; its reasoning stands

## Context

ADR 0022 made the driver's conductor load a publication's producer position, the
word the publisher stores on every commit, at most once per millisecond instead of on
every spin, and the exclusive claim row's band on the bench host closed from 270 to 430
ns down to 220 to 260. Each load still moves the line to the conductor's cache, and the
publisher's next store to it fetches the line back: one claim per load pays a cross-core
transfer, about 50 ns on both bench hosts.

Card 161 found where that transfer lands. The gate reads p99.99, the 500th slowest claim
of a five-million-message run; at kip's rate of about 10.4 million claims a second, a
run takes about 0.48 s and holds about 495 loads, so whether the 500th slowest claim is
a transfer is decided by whether the run lasted past half a second. The arithmetic is of
the rate alone, one transfer per 10,000 claims at 10 million claims a second, so no
message count moves it. Once the harness timed its cadence by the clock as the driver
does (772f988), the row on kip read one of two values per process, each process wholly
on one side: 107, 134, 156, 115 and 156 ns at p99.99 in five runs of `bench (kip)` on
2026-10-29, on a live path no commit of the day changed, against a baseline whose slowest
run is 132. A run that lands high is red; a rerun is a coin flip; master's own run at
0e97c37 landed high. The bench host, whose rate is about 9.7 million claims a second,
sits just past the index, and its checked exclusive row alternated 120 and 160 within one
run of 21 (pull request 65's review); its 10 ns clock step and 160 ns baseline had
hidden that. A `perf c2c` record on kip shows no other line in play (card 161).

The load has to happen: the driver's unblock check, the `publication.position` gauge
and the `rejected_tail` check read the producer position (ADR 0022). Loosening the gate,
re-recording the baseline from a high run or exempting the row, would let a regression
of 50 ns on a low-mode process pass, and CLAUDE.md forbids a knob that makes a benchmark
pass. What remains is the cadence.

## Decision

The driver loads a publication's producer position at most once every two milliseconds
(`cavalry_driver::spec::PRODUCER_POSITION_INTERVAL`, 2 ms), and the harness's conductor,
which derives its cadence from the same constant, follows. At 10 million claims a second
one claim in about 20,000 pays the transfer, below the gate's percentile with room on
both hosts, which read about 500 and 540 loads a run today and will read about 250 and
270. Everything ADR 0022 said of the load's consumers holds with the number doubled: the
unblock test passes late by up to 2 ms against a timeout measured in seconds, the
`rejected_tail` check catches a corrupt tail up to 2 ms later, closing and `add
subscription` take the same loads they took. `spec/log-buffer.md`, "The driver", says
two milliseconds where it said one; the driver's and the bench's READMEs and the
`publication.position` counter's doc follow; ADR 0022's text stays as the record of why
the load left the spin loop.

Both hosts' baselines are re-recorded from the first green run of master with this
cadence, in a commit of their own with the histograms, since the exclusive row falls to
its ordinary tail on both: about 107 ns on kip, and on bench whatever its first run
reads, which today is 120 in the runs that miss the transfer.

## Alternatives considered

- **Keep one millisecond and rerun a red kip job.** Lost: a coin flip on every push to
  master, and a red master that is nobody's regression; four reruns a day is a cost with
  no end.
- **Re-record kip's baseline from a high-mode run.** Lost: a baseline of 160 passes a
  low-mode process that regressed by 50 ns, which is the gate loosened to make a
  benchmark pass (CLAUDE.md, Configuration and defaults).
- **Load every 0.8 ms, so the transfer always lands at the percentile.** Lost: the row
  reads a stable 155 everywhere, honest and stable, but the publisher pays a quarter more
  transfers and the gate's number gets worse so that it can be stable.
- **The publisher stores its own position to a line of its own every N commits, and the
  driver reads that.** Lost: the store that publishes the position pays the same
  transfer on the publisher's thread once per N commits, inside a commit, so it moves the
  cost rather than removing it, and a position that advances only with commits leaves
  the unblock check blind to a publisher that commits fewer than N frames and stalls.
- **A longer or shorter run in the harness.** Lost: the ratio of transfers to the
  percentile's index is the rate's alone, as the Context derives.

## Evidence

Measured on 2026-10-29, the harness pinned, the checked build, five million messages,
twenty-one runs where CI ran it and seven where a hand did:

- `bench (kip)`, `claim_commit_32_exclusive` p99.99 on an unchanged live path, against
  the baseline's slowest run of 132: 107 (master a4d7e5d), 134 (pull request 63), 156
  (pull request 67), 115 (its rerun), 156 (master 0e97c37, red); in that red run the concurrent claim row crossed too, 168 (166 to 169) against its baseline's slowest run of 154, +9.1%, so a high-mode process lifts both rows that pay the transfer. Producer
  position loads per run, printed by the harness since 772f988: 494 to 502.
- The cadence halved by hand on kip (5,000 spins of the spin-counted harness, about
  0.5 ms): every run at 152 to 164 in three processes; the cadence as committed: runs
  from 131 to 161; a longer count did not restore the low mode (card 161's comment of
  18:07Z), so the high value is the transfer's cost and the low mode is its absence.
- `perf c2c` on kip, one process of three runs: the publisher's rare modified-line hits
  are on the log cells at 136 to 190 cycles, the transfer; the dominant hits are the
  conductor's own loads of the subscriber's position cell, which cost the conductor and
  not the publisher.
- The bench host, run 40942523199: the checked exclusive row's 21 values alternating 120
  and 160, median 120, loads 540 to 559 a run.

To be added at acceptance: the first `bench (linux)` and `bench (kip)` runs at two
milliseconds, with the loads per run near 250 and 270 and the exclusive rows at their
ordinary tails, and the re-recorded baselines.

## Consequences

Easier: the kip gate stops flipping per process on a row no commit touched, master's
runs stay green on both hosts, and every publisher at this rate pays the transfer half
as often. Harder: the driver's view of a publisher's position is up to 2 ms stale where
it was 1 ms; both baselines move once. We now maintain one constant whose value has a
derivation, which this page is.

Revisit: when a host runs the exclusive row at about 20 million claims a second, where
two milliseconds meets the percentile again; when a consumer of the producer position
needs it within a millisecond; when the gate's percentile changes.

## References

- ADR 0022 (the cadence and why it left the spin loop), ADR 0029 (the clock-step floor),
  card 161 (the rows and the `perf c2c` record), pull requests 63, 65 and 67.
- `crates/cavalry-driver/src/spec.rs`, `PRODUCER_POSITION_INTERVAL`; `bench/src/main.rs`,
  `CADENCE_SPINS` and `PRODUCER_POSITION_INTERVAL_NS`; `spec/log-buffer.md`, "The driver".
