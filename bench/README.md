# cavalry-bench

The latency and throughput harness behind `just bench`: the gate for invariant 2
(`CLAUDE.md`, "Gates"). It is a binary, not a library, and lives outside `crates/` because
nothing depends on it.

## What this does

- Drives `cavalry-shm` directly, with no driver process yet: a publisher thread claims and
  commits into a real log buffer, a subscriber thread reads it through its own read-only
  mapping, and a conductor thread does the driver's duties, the publisher limit and the
  cleaning, loading the producer position at the driver's cadence, once every two
  milliseconds by the clock (ADR 0022, ADR 0032), through a real counters file, all under `/dev/shm` on Linux
  and the system's temporary directory elsewhere. The conductor reads the clock once
  every 256 spins, since a clock read on every spin costs the other threads. The loads
  per run are printed and written to the result, so each host's cadence is on record
  (card 161). A load of the producer position makes the publisher's next store to that
  line fetch it back from the conductor's cache, about 50 ns on kip. At ten million
  claims a second and a load every millisecond, one claim in about ten thousand paid it,
  which is the gate's own percentile, so the exclusive row read 107 or 156 ns at p99.99
  by the process (card 161); at two milliseconds it is one claim in about twenty
  thousand, below the percentile on both hosts (ADR 0032). The warm-up before recording is one rotation through
  all three terms, so no measured message first-touches a page, and every histogram's
  counts are written once before the loop, so no rare sample first-touches one either.
  `--pin P,S,C` binds the three threads to those CPUs on Linux, with the driver's
  recorder and replayer and the client's conductor on the lowest CPU outside them in
  the client rows, so each pinned CPU runs one thread, and `--repeat N` runs everything N times and
  reports each benchmark's median run by p99.99, every run's on record; the bench host
  runs it with both (ADR 0016). `--only NAME` runs only the benchmarks with a row whose
  name contains NAME, for a profile of one row, and writes the result under the
  profile with `-only` appended, so it is never compared as a full run. A NAME that no
  row's name contains is an error that lists the rows.
- Times every message with the time stamp counter, calibrated once against the
  monotonic clock through `quanta` ([ADR 0028](../docs/decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md)):
  the OS clock through the vDSO was the harness's own tail, 280 ns at p99.99 on the
  bench host and 1.4 µs on the second, once per millisecond when the timekeeper's tick
  rewrites what it reads. Records each sample in an HdrHistogram with fixed bounds,
  so recording never allocates, and reports p50, p99, p99.99 and max, never a mean. A
  `timer` row reports the harness's own overhead.
- Measures the publisher's claim and commit, the one-way latency from the publisher's
  timestamp to the subscriber's handler, for the exclusive and the concurrent log type,
  and a slot-padding microbench that times a load of a word and a store to a word 0, 64
  or 128 bytes away, from two threads (ADR 0011). `just bench-unchecked` runs it all
  again with the data-plane checks built out, under a profile of its own, to price
  ADR 0010's checks.
- Runs the measured loops under a counting allocator and fails the run when they
  allocate: the zero-allocations-per-message assertion of the gate.
- Writes `bench/results/<profile>-<unix time>.json`, the clock's step included;
  `scripts/bench-compare.py` fails a p99.99 regression over 5 percent and over one clock
  step, the smaller of the result's and the baseline's, against `bench/baseline.json`
  for the profile, on the `claim_commit` and `one_way` rows ([ADR 0029](../docs/decisions/0029-fail-a-gated-row-only-beyond-one-clock-step.md)); the `timer` and `slot_padding` rows are host
  probes and are reported, never failed (ADR 0017). The bench hosts' profiles have
  baselines, `bench-linux`, `kip-linux` and `grip-linux` with their `-unchecked` twins,
  grip's recorded on 2026-10-31 at 0782ec5 from its job's first four processes, and CI runs
  the gate on each host, as `bench (linux)`, `bench (grip)` and, until grip's job has
  been green over a handful of pushes, `bench (kip)`
  ([ADR 0034](../docs/decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md)): a
  shared runner's run is a report. The `_recorded` rows repeat the client
  rows with the driver's recorder writing the stream to the durable log under the bench
  directory, from a housekeeping CPU under `--pin`, and are gated against their baseline
  like the other message rows; `record_block` times the recorder's own block path and is
  reported against its own, not gated (ADR 0025, ADR 0017). Both got their first
  baselines at 7f62673 (card 109), and the `bench` host's two, `bench-linux` and
  `bench-linux-unchecked`, were re-recorded from master's run at 96fb91a, run on
  2026-10-28 and merged on 2026-10-29,
  when the unchecked `_recorded` claim row had drifted to within a step of its
  baseline's slowest run on master itself, and again on 2026-10-31 from every process
  the host had run at the tree of 4a09b6e, twelve, leaving out the one process a timer
  on the host disturbed (card 170). `kip`'s two were re-recorded on 2026-10-30
  from every process the host had run at the tree of 4a09b6e, eight and five, when
  master's own runs had failed the concurrent claim row and the unchecked `_recorded`
  row against the band of the one process recorded at 7f62673: a bench host's
  baseline holds every such process's runs, `processes: N` in its entry, a later
  process of the tree that reads above it joins it with `--join`, and the gate
  compares with the slowest run over all of them
  ([ADR 0033](../docs/decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
  On 2026-10-31 the host set-up changed twice under `bench/host/` (the stock timers
  masked, the driver-bound interrupts moved to the housekeeping CPUs), which ends a
  tree, so `bench`'s two were re-recorded from master's three processes at 0782ec5,
  and `grip`'s two, `grip-linux` and `grip-linux-unchecked`, were recorded for the first
  time from the same three runs, the host's first processes with its Wi-Fi interrupts
  off the isolated CPUs, and joined by a fourth process the same day when it read the
  checked client claim row above the three's band
  ([ADR 0034](../docs/decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md)).

## What it must never do

- Quote a mean, or hide a tail: every row is percentiles and a max.
- Add a knob to make a number pass. The options choose what to measure, not how well.
- Run on the data plane. It measures `cavalry-shm`; it is not part of it, so its
  dependencies are its own and it may allocate outside the measured loops.

## Example

```sh
just bench --messages 2000000
just bench-compare
```

The first prints the table below the run's header and writes the result file; the
second compares the newest result with the baseline for the profile and ends with
`bench-compare: ok`, or names the regression. The columns are nanoseconds.

```text
  benchmark                         count     p50 ns     p99 ns  p99.99 ns       max ns     per second
  claim_commit_32_exclusive       2000000         ...
```

## What to read next

- [`docs/how-to/run-the-bench.md`](../docs/how-to/run-the-bench.md): running it,
  reading the table, recording a baseline, what to do on a regression.
- [ADR 0015](../docs/decisions/0015-measure-in-process-with-per-runner-baselines.md):
  why the harness is in process, why HdrHistogram and one baseline per runner, and
  [ADR 0028](../docs/decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md),
  why the clock is the time stamp counter; [ADR 0016](../docs/decisions/0016-run-the-latency-gate-on-a-dedicated-host.md):
  why the gate fails only on the dedicated bench hosts, and
  [`docs/how-to/set-up-a-bench-host.md`](../docs/how-to/set-up-a-bench-host.md), how
  that host is built from `bench/host/`.
- [`scripts/bench-compare.py`](../scripts/bench-compare.py), the gate's second half, and
  [`bench/baseline.json`](baseline.json), its memory.
