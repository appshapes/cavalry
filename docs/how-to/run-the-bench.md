# Run the bench and compare it with the baseline

A how-to page: one task, measuring the data plane's latency and checking it against the
committed baseline, for a person at a laptop or for CI. The rules are in `CLAUDE.md`,
invariant 2 and the gates table; why the harness measures what it measures is
[ADR 0015](../decisions/0015-measure-in-process-with-per-runner-baselines.md).

## Prerequisites

- A release build toolchain: `cargo` and `just`, as for every gate.
- An idle machine with at least three free cores. The harness runs a publisher, a
  subscriber and a conductor thread, each spinning; a browser or a build in the
  background moves p99.99 by more than the 5 percent the gate allows.
- Python 3 for `just bench-compare`.

## Steps

1. Run the harness:

   ```sh
   just bench
   ```

   It builds `cavalry-bench` in release, creates a log buffer and a counters file under
   the system's temporary directory, and runs the benchmarks for one million messages
   each after a warm-up; CI runs five million, which steadies p99.99 on a shared runner. It prints one table and writes one file,
   `bench/results/<profile>-<unix time>.json`. The profile is `$CAVALRY_BENCH_PROFILE`
   when set, as CI sets `linux` or `macos`, and `local-<os>-<arch>` otherwise. Pass
   arguments after the recipe: `just bench --messages 5000000` runs longer and makes
   p99.99 steadier.

2. Read the table. Every row is one benchmark with its count and its p50, p99, p99.99
   and max in nanoseconds, then its rate. No row is a mean: the data plane is judged at
   p99.99 and max (`CLAUDE.md`, invariant 2). The rows are:

   | Benchmark | What one sample is |
   | --- | --- |
   | `timer` | Two `Instant::now()` calls back to back: the harness's own overhead, present in every other row. |
   | `claim_commit_<body>` | The publisher's `try_claim`, writing the body, and `commit`, for a body of that many bytes. |
   | `one_way_<body>` | From the publisher's timestamp before its claim to the subscriber's handler receiving the frame, across two threads. |
   | `slot_padding_<spacing>_load` | A load of one word while another thread stores to a word that many bytes away: 0 shares the cache line, 64 shares the 128-byte pair, 128 shares nothing (ADR 0011). |
   | `slot_padding_<spacing>_store` | The other side of the same pair: one store while the other thread loads. |

   The last line says how many allocations the measured loops made per message. It is 0
   or the run has failed: the gate requires zero allocations per message.

   `just bench-unchecked` runs the same harness with the data-plane checks built out,
   under its own profile, `<profile>-unchecked`, and its own baseline: the difference
   between the two is what the checks cost (ADR 0010). Nothing else is ever built with
   that flag.

3. Compare with the baseline:

   ```sh
   just bench-compare
   ```

   It takes the newest file under `bench/results/`, finds the profile in
   `bench/baseline.json` and prints each benchmark's p99.99 against the baseline's. A
   p99.99 more than 5 percent above the baseline is a regression and the recipe exits 1.
   Max is shown but does not fail the gate. A profile with no baseline is reported and
   passes: a laptop has none unless you record one.

4. To record a baseline for a machine, run `just bench` on it when it is idle and then
   `python3 scripts/bench-compare.py --update`, which writes the newest result's
   percentiles into `bench/baseline.json` for that profile. Commit it with the
   histograms in the message. A CI runner's baseline is set from its own run: download
   the `bench-results-<profile>` artifact of a green run and pass the file to
   `--update`.

## How to verify it worked

- `ls bench/results/` shows a new `.json` file whose `profile` field is the one you
  expected.
- The table's last line reads `allocations per message: 0`.
- `just bench-compare` ends with `bench-compare: ok`.

## When it does not work

- `allocations per message` is not 0: a measured loop allocated. The gate has done its
  job; find the allocation with the counting allocator's report in the output and
  remove it. Nothing on the per-message path may allocate (`.claude/rules/data-plane.md`).
- `REGRESSION` on a p99.99: measure again on an idle machine before you believe either
  number; if it holds, the change under a data-plane crate is slower at the tail and
  does not merge until it is not, or the baseline is raised in a commit whose message
  says why. Both histograms go in the commit message either way.
- `no baseline for profile`: expected on a laptop and on a runner whose baseline has
  not been recorded yet. Record one as step 4 says.
- The harness refuses to start: it needs to create files under the system's temporary
  directory and at least two cores; the error names what it could not do.
