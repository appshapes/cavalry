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
both hosts, which read about 500 and 540 loads a run at one millisecond and 249 to 252
and 271 to 272 at two (Evidence). Everything ADR 0022 said of the load's consumers
holds with the number doubled: the unblock test passes late by up to 2 ms against a
timeout measured in seconds, the `rejected_tail` check catches a corrupt tail up to 2 ms
later, closing and `add subscription` take the same loads they took.
`spec/log-buffer.md`, "The driver", says two milliseconds where it said one; the
driver's and the bench's READMEs and the `publication.position` counter's doc follow;
ADR 0022's text stays as the record of why the load left the spin loop.

This decision moves no baseline: the gate passes on both hosts at this cadence against
the baselines recorded at one millisecond, and the exclusive row's p99.99 is unchanged
within its spread (Evidence). A later re-record of a host's baseline is a person's
commit under `docs/how-to/run-the-bench.md`, step 4, with that run's histograms as its
evidence, and the second alternative below bounds it: not from a run whose slowest
exclusive run is above the baseline's, since that is the gate loosened to pass a
benchmark.

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

The first runs at two milliseconds: CI run 40958773217 on 2026-10-29, `bench (kip)` and
`bench (linux)` on the pull request's merge of 3532528 with master (git 64c8aec), the
same harness, five million messages, twenty-one runs per row, both profiles. Below,
`claim_commit_32_exclusive` p99.99 in ns is the median of the 21 runs, then their range,
then the baseline's slowest run, which the gate compares with; "before" is the last run
at one millisecond on the same host, master a4d7e5d on bench and 0e97c37's rerun on kip.

- Producer position loads per run, the exclusive row: 249 and 252 on kip (checked,
  unchecked) and 272 and 271 on bench, from 494 to 502 and 540 to 559 at one millisecond;
  the concurrent row, whose run lasts longer, 284 and 288 on kip and 280 and 279 on
  bench. Below the percentile's index of 500 on both hosts, as derived.
- kip, checked: 130 (119 to 152) against 132, -1.5%; before, 114 (105 to 150). The 21
  runs in order: 152 141 148 125 124 129 119 130 124 130 138 129 120 125 130 152 140 132
  125 135 126; thirteen of them at 119 to 130 and eight from 132 to 152, inside one
  process, where one millisecond had put a whole process on one side. The concurrent
  row 153 (150 to 157) against 154.
- kip, unchecked: 106 (103 to 150) against 165; before, 106 (102 to 132).
- bench, checked: 150 (140 to 169) against 160; before, 150 (120 to 169). The 21 runs
  alternate 140 and 160, with two at 150 and one at 169, where one millisecond alternated
  120 and 160 (run 40942523199, above).
- bench, unchecked: 120 (120 to 150) against 150; before, 130 (120 to 140).
- `bench-compare: ok` on both hosts, both profiles; every gated row within 5 percent of
  its baseline's slowest run.

So the loads halved and the gate passes on both hosts, and the exclusive row's p99.99 is
unchanged within its spread: what the cadence removed is the load-driven transfer at
the percentile, which had decided a whole process's reading. A second and smaller
source of slow claims, the runs from 132 to 152 inside one process on kip, remains, has
no cause on record, and is here for the next reader. Re-recording kip's baseline from
this run would set its slowest run at 152 where it is 132, which is the second
alternative; so no baseline moved with this decision.

## Consequences

Easier: the kip gate no longer reads one of two values by the process on a row no
commit touched, master's runs stay green on both hosts, and every publisher at this rate
pays the transfer half as often. Harder: the driver's view of a publisher's position is
up to 2 ms stale where it was 1 ms; the exclusive row's spread inside one run, 119 to
152 on kip and 140 to 169 on bench, is now the row's own and has no cause on record. We
now maintain one constant whose value has a derivation, which this page is.

Revisit: when a host runs the exclusive row at about 20 million claims a second, where
two milliseconds meets the percentile again; when a consumer of the producer position
needs it within a millisecond; when the gate's percentile changes.

## References

- ADR 0022 (the cadence and why it left the spin loop), ADR 0029 (the clock-step floor),
  card 161 (the rows and the `perf c2c` record), pull requests 63, 65 and 67.
- `crates/cavalry-driver/src/spec.rs`, `PRODUCER_POSITION_INTERVAL`; `bench/src/main.rs`,
  `CADENCE_SPINS` and `PRODUCER_POSITION_INTERVAL_NS`; `spec/log-buffer.md`, "The driver".
