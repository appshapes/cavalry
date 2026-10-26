# 0028. Stamp bench samples with the time stamp counter, calibrated once

- **Status:** Accepted
- **Date:** 2026-10-26
- **Invariants affected:** 2
- **Supersedes:** none

## Context

Card 138. With every mapping pre-touched and one thread per pinned CPU (card 136), the
second bench host, `kip`, still read `one_way` at 1.6 µs at p99.99 against `bench`'s
0.6 µs, with p50 and p99 below `bench`'s, no page fault inside a measured loop and no
Cavalry thread woken from another CPU. An experiment branch recorded every `one_way`
sample over 1 µs with its message index and stamp. On kip the slow samples came in
clusters of one or two messages spaced 1.000 ms apart, 172 clusters in a 172 ms loop,
with no relation to cleaning, rotation or poll batches, and unchanged when the raw
rows' conductor read the producer position every 20,000 or 100,000 spins instead of
10,000, or not at all. The pinned CPUs took almost no timer interrupts during a run
(53, 0 and 127 on CPUs 6, 8 and 10). Both hosts run `HZ=1000` on the `tsc`
clocksource.

What runs at exactly 1 kHz and touches both hosts' pinned cores is the timekeeper: on
every tick of the housekeeping CPU the kernel rewrites the vDSO data that `clock_gettime`
reads, and the harness read that clock through `Instant::now` twice per sample, for the
publisher's stamp and the subscriber's arrival, plus twice more for each `claim_commit`
span. The next reader on a pinned core paid a cross-core miss and a seqlock retry.
Stamping with the time stamp counter instead removed the clusters on both hosts:

| row, one_way p99.99 | kip, vDSO | kip, TSC | bench, vDSO | bench, TSC |
| --- | --- | --- | --- | --- |
| `exclusive` | 1,655 ns | 344 and 358 ns | 610 and 620 ns | 330 ns |
| `concurrent` | 1,639 ns | 373 and 370 ns | 620 and 630 ns | 340 ns |
| `client` | 5,895 ns | 386 ns | 560 and 590 ns | 320 and 330 ns |
| `recorded` | 1,015 ns | 421 and 427 ns | 790 and 780 ns | 750 and 760 ns |

ADR 0015 chose `Instant` and set a cycle counter aside "for now", because reading one is
an `unsafe` intrinsic and the repository keeps `unsafe` to two crates; `quanta` reads it
behind a safe call, which removes that reason.

Two runs of each, `--messages 1000000`, pinned, 2026-10-26. On kip p50 fell by 40 ns too,
the cost of the two vDSO reads; every max halved. The `recorded` row's remaining tail is
the recorder's back-pressure on the publisher, not the clock. Cavalry's data plane reads
no clock per message (`.claude/rules/data-plane.md`); the harness did, and measured its own
clock's tail as the log's.

## Decision

The bench harness takes every sample from the time stamp counter: `rdtsc` on x86_64,
`cntvct_el0` on aarch64, through the `quanta` crate, which reads the counter without
`unsafe` in this repository, calibrates it once against the monotonic clock for at most
200 ms before the first row, and falls back to the monotonic clock where the counter is
not invariant. A stamp and a span are readings of that clock, and `Instant` keeps the
control plane's work: elapsed time for the rate, and deadlines. The `timer` row is two
readings of that clock back to back. Nothing in a Cavalry crate changes.

## Alternatives considered

- **Keep `clock_gettime` and call the tail the host's.** Lost: the gate would hold the
  timekeeper's tick, not the log's, and `bench`'s 600 ns baseline carried 280 ns of it.
- **`rdtsc` in the harness directly.** Lost: it is `unsafe` in Rust, and the bench would
  be a third crate carrying `unsafe` where `CLAUDE.md` permits two; `quanta` carries it
  behind a safe call and the detection of an invariant counter.
- **A lower `HZ` or `nohz_full` on the timekeeping CPU.** Lost: the timekeeper must tick
  somewhere, and a host setting would hide a cost every harness on every host pays.
- **A cached clock, read by one thread and published.** Lost: that is what the driver does
  on the data plane (ADR 0022), and a published clock has the publisher's granularity,
  not a sample's.

## Evidence

The table above; the trace and the experiment are on card 138 and in the commit that
implements this, with the bench host's rows before and after in the pull request.

## Consequences

- Every bench baseline moves down, by about 280 ns at p99.99 on `bench`, and is
  re-recorded in a person's commit (`docs/how-to/run-the-bench.md`, step 4); kip's can
  be recorded for the first time against the same clock.
- On Apple silicon the counter ticks at 24 MHz, so a laptop's samples are quantized to
  about 42 ns, as `Instant` already was there; the laptop is not a gate.
- A host whose counter is not invariant measures with the monotonic clock and its tick,
  and reads the clock's tail at exactly 1 ms spacing, which the how-to names.
- `quanta` is a dependency of the bench, not of a Cavalry crate.
- ADR 0017's Revisit clause, "when the harness has a cycle counter and the probes resolve
  below a clock step", half fires: the harness has the counter, and `timer` reads 20 ns
  on both hosts, two readings of about 10 ns, which still sits on a step of the
  reported percentiles. Whether the probe rows become gates is a later decision; this
  one leaves them reported.

## References

- Card 138; card 136; [ADR 0015](0015-measure-in-process-with-per-runner-baselines.md),
  [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md),
  [ADR 0022](0022-load-the-producer-position-once-per-millisecond.md).
- `bench/src/main.rs`, `clock`, `tick`, `nanos` and `span_ns`.
