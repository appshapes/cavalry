# 0015. Measure the data plane in process, with a baseline per runner

- **Status:** Accepted
- **Date:** 2026-10-11
- **Invariants affected:** 2, 6
- **Supersedes:** none; moves `just bench` from the fifth feature of ADR 0007 to the third

## Context

Invariant 2 is a budget, and `CLAUDE.md`'s gate for it is `just bench`: HdrHistogram
p50, p99, p99.99 and max written to `bench/results/`, a diff against
`bench/baseline.json` that fails CI on a p99.99 regression over 5 percent, and an
allocation-counting test that asserts zero allocations per message. ADR 0007 placed the
harness with the fifth feature, the client API, because the ten-minute path would be
the thing to measure. ADRs 0010 and 0011 then named "the log buffer feature's `just
bench` run" as the evidence their acceptance waits on, and Rjae chose on 2026-10-11 to
build the harness with the log buffer, with no compromise on the evidence. There is no
driver process yet (feature 4) and no client (feature 5), so the harness has to drive
`cavalry-shm` itself and play the driver's part.

Every number a benchmark reports is read by the rule in `CLAUDE.md`: judged at p99.99
and max, never at a mean. `docs/explanation/should-zigbolt-inform-cavalry.md` recorded
what a harness must not do: time batches and report a percentile of batch means, run
publisher and subscriber on one thread and call it a round trip, or leave the hardware
unnamed. The repository's rule on `unsafe` is that only `cavalry-shm` and `cavalry-ffi`
carry it, and a counting global allocator needs it.

## Decision

`bench/` is a binary crate, `cavalry-bench`, off the data plane, that `just bench` runs
in release. It creates a real log buffer and a real counters file under the system's
temporary directory and runs three threads: a publisher that claims and commits through
a `TermAppender`, a subscriber that polls through a `TermReader` on a read-only mapping
of its own and stores its position with a release store, and a conductor that does the
driver's duty cycle, cleaning below the subscriber's position and raising the publisher
limit by the spec's rule. One sample is one message. The publisher's sample is
`Instant::now()` around claim, body and commit; the one-way sample is the subscriber's
`Instant` at delivery minus the timestamp the publisher wrote in the body before its
claim. Each goes into an HdrHistogram with fixed bounds, 1 ns to 60 s at three
significant figures, so recording never allocates, and the harness reports p50, p99,
p99.99 and max per benchmark, plus the rate; a `timer` row reports two `Instant::now()`
back to back, the overhead inside every other sample. Both log types run. A slot-padding
microbench loads one word while another thread stores to a word 0, 64 or 128 bytes
away, the access pattern of a publisher loading its limit while a subscriber stores its
position, for ADR 0011's question. The measured loops run inside a region of the
`stats_alloc` counting allocator, which carries the `unsafe` a global allocator needs so
this crate carries none, and the run fails when the region saw an allocation. The result
is one JSON file per run, `bench/results/<profile>-<unix time>.json`, where the profile
is `$CAVALRY_BENCH_PROFILE`, set to `linux` or `macos` by CI, or `local-<os>-<arch>`.
`scripts/bench-compare.py` reads the newest result and `bench/baseline.json`, which
holds one entry per profile, prints every benchmark's p99.99 against the baseline's, and
exits 1 when any is more than 5 percent above it or the allocation count is not zero;
a profile with no baseline is reported and passes, and `--update` writes a result into
the baseline, which is how a runner's baseline is set from its own run's artifact. CI's
gates job runs `just bench` and `just bench-compare` after the four gates and keeps the
result as an artifact.

## Alternatives considered

- **criterion.** Lost: it reports means with confidence intervals and tunes its sample
  count for them; the tail is what Cavalry is judged at, and its runner would be a
  second harness beside this one.
- **iai or cachegrind-style instruction counts.** Lost: deterministic, but not latency;
  a cache miss and a descheduling are what p99.99 is about.
- **Waiting for the driver and the client, as ADR 0007 planned.** Lost: two ADRs wait on
  this harness, and the log buffer is where the per-message cost is paid; the ten-minute
  path joins this harness as a benchmark when it exists.
- **A cycle counter instead of `Instant`.** Lost for now: reading it is an `unsafe`
  intrinsic, forbidden here, and `Instant` is the same clock on both runners. Its cost
  is reported in the `timer` row, and its granularity is the one known weakness: on
  Apple silicon `Instant` ticks every 41.67 ns, so a single load reads 0 at p50 there
  and only the Linux runner resolves the slot-padding rows below that. Revisit below.
- **Timing batches and dividing.** Lost: a percentile of batch means hides the slow
  message, the thing the invariant is about.
- **Pinning the threads.** Lost for now: macOS has no affinity call, and the runners
  are shared VMs either way; the harness names its thread count and the page says an
  idle machine is the prerequisite. Linux pinning joins when the noise demands it.
- **One baseline for every machine.** Lost: a laptop and a 4-vCPU runner differ by
  more than the 5 percent rule; a baseline per profile keeps the rule meaningful, and a
  laptop compares against nothing unless its owner records one.
- **Failing on max as well as p99.99.** Lost: one descheduling on a shared runner moves
  max; it is printed beside p99.99 and read by a person.
- **A counting allocator of our own.** Lost: it is an `unsafe` `GlobalAlloc` impl, and
  the rule keeps `unsafe` to two crates; `stats_alloc` is a 200-line crate that does
  exactly this.

## Evidence

- The first run, on 2026-10-11, Rjae's laptop, macOS aarch64, 18 cores, unpinned,
  release, 1,000,000 messages after 100,000 warm-up, 32-byte bodies, 4 MiB terms, commit
  02412a8 of `cavalry-shm`:

  | benchmark | p50 ns | p99 ns | p99.99 ns | max ns | per second |
  | --- | --- | --- | --- | --- | --- |
  | claim_commit_32_exclusive | 83 | 125 | 1,917 | 132,735 | 7,752,118 |
  | claim_commit_32_concurrent | 83 | 125 | 4,167 | 127,039 | 7,710,212 |
  | one_way_32_exclusive | 125 | 292 | 32,431 | 133,375 | 7,752,118 |
  | one_way_32_concurrent | 125 | 167 | 18,127 | 127,167 | 7,710,212 |
  | slot_padding_0 | 0 | 42 | 125 | 15,335 | 27,772,378 |
  | slot_padding_64 | 0 | 42 | 83 | 13,255 | 29,690,550 |
  | slot_padding_128 | 0 | 42 | 83 | 36,895 | 28,596,962 |
  | timer | 0 | 42 | 83 | 6,667 | 27,251,205 |

  Allocations per message: 0 in 2,000,000. Publisher retries: 16 per log type, the 16
  rotations of 64 MB of frames through 4 MiB terms. The p50 of 83 ns for claim and
  commit holds two `Instant` reads of up to 42 ns each; the publisher's own work is
  below the clock's tick.
- The macOS and Linux runners' first results are the artifacts of the CI run of this
  commit; their baselines are recorded in the next commit of card 83, with the ADR 0010
  and ADR 0011 measurements.

## Consequences

Easier: a change under a data-plane crate has a number before and after, on the same
machine, in the commit message; ADRs 0010 and 0011 have a harness to run on; the
ten-minute path, when it exists, is one more benchmark here.

Harder: CI runs about a minute longer per job; a baseline is per runner and must be
re-recorded when a runner changes; a laptop's numbers are not comparable to the
runner's and the page says so; macOS cannot resolve a single load.

Revisit: when the client API exists, to add the ten-minute path as a benchmark; when
CI noise fails the 5 percent rule on an unchanged data plane, for Linux pinning, longer
runs or a quieter runner; when a cycle counter is wanted, with an ADR for the `unsafe`
it needs or a crate that carries it; when the runners change.

## References

- `bench/src/main.rs`, `bench/README.md`, `bench/baseline.json`,
  `scripts/bench-compare.py`, `docs/how-to/run-the-bench.md`, the `bench` and
  `bench-compare` recipes in `justfile`, the gates job in `.github/workflows/ci.yml`
- `CLAUDE.md`: invariant 2; Gates; When in doubt. `.claude/rules/data-plane.md`
- ADR 0007, ADR 0010, ADR 0011, ADR 0013
- `docs/explanation/should-zigbolt-inform-cavalry.md`, "Benchmarks"
- HdrHistogram: <https://github.com/HdrHistogram/HdrHistogram_rust>; stats_alloc:
  <https://github.com/neoeinstein/stats_alloc>
- AppShapes Trello card 83, "Build the log buffer, the ring buffers and the bench"
