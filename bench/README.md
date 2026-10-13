# cavalry-bench

The latency and throughput harness behind `just bench`: the gate for invariant 2
(`CLAUDE.md`, "Gates"). It is a binary, not a library, and lives outside `crates/` because
nothing depends on it.

## What this does

- Drives `cavalry-shm` directly, with no driver process yet: a publisher thread claims and
  commits into a real log buffer, a subscriber thread reads it through its own read-only
  mapping, and a conductor thread does the driver's duties, the publisher limit and the
  cleaning, loading the producer position at most once per millisecond as the driver
  does (ADR 0022), through a real counters file, all under `/dev/shm` on Linux and the system's
  temporary directory elsewhere. The warm-up before recording is one rotation through
  all three terms, so no measured message first-touches a page. `--pin P,S,C` binds the
  three threads to those CPUs on Linux, and `--repeat N` runs everything N times and
  reports each benchmark's median run by p99.99, every run's on record; the bench host
  runs it with both (ADR 0016).
- Times every message with `Instant` and records it in an HdrHistogram with fixed bounds,
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
- Writes `bench/results/<profile>-<unix time>.json`; `scripts/bench-compare.py` fails a
  p99.99 regression over 5 percent against `bench/baseline.json` for the profile, on
  the `claim_commit` and `one_way` rows; the `timer` and `slot_padding` rows are host
  probes and are reported, never failed (ADR 0017). Only the bench host's profiles have
  a baseline: a shared runner's run is a report.

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
  why the harness is in process, why `Instant` and HdrHistogram, why one baseline per
  runner; [ADR 0016](../docs/decisions/0016-run-the-latency-gate-on-a-dedicated-host.md):
  why the gate fails only on the dedicated bench host, and
  [`docs/how-to/set-up-a-bench-host.md`](../docs/how-to/set-up-a-bench-host.md), how
  that host is built from `bench/host/`.
- [`scripts/bench-compare.py`](../scripts/bench-compare.py), the gate's second half, and
  [`bench/baseline.json`](baseline.json), its memory.
