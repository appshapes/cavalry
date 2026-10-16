# Run the bench and compare it with the baseline

A how-to page: one task, measuring the data plane's latency and checking it against the
committed baseline, for a person at a laptop or for CI. The rules are in `CLAUDE.md`,
invariant 2 and the gates table; why the harness measures what it measures is
[ADR 0015](../decisions/0015-measure-in-process-with-per-runner-baselines.md), and why
the gate fails only on the dedicated bench host is
[ADR 0016](../decisions/0016-run-the-latency-gate-on-a-dedicated-host.md).

## Prerequisites

- A release build toolchain: `cargo` and `just`, as for every gate.
- An idle machine with at least three free cores. The harness runs a publisher, a
  subscriber and a conductor thread, each spinning; a browser or a build in the
  background moves p99.99 by more than the 5 percent the gate allows. The number that
  gates a commit comes from the bench host, whose three cores are isolated for this
  ([Set up a bench host](set-up-a-bench-host.md)).
- Python 3 for `just bench-compare`.

## Steps

1. Run the harness:

   ```sh
   just bench
   ```

   It builds `cavalry-bench` in release, creates a log buffer and a counters file under
   `/dev/shm` on Linux and the system's temporary directory elsewhere, and runs the
   benchmarks for one million messages each after a warm-up; CI runs five million. The
   warm-up is one rotation through all three terms, 196,608 messages for the default
   body and term, so that every page of the log is mapped before a sample is taken: a
   first touch of a page costs microseconds, and 1,500 of them in a 5,000,000-message
   run were the whole p99.99 on the bench host. It prints one table and writes one
   file, `bench/results/<profile>-<unix time>.json`. The profile is
   `$CAVALRY_BENCH_PROFILE` when set, as CI sets `bench-linux`, `linux` or `macos`,
   and `local-<os>-<arch>` otherwise. Pass arguments after the recipe:
   `just bench --messages 5000000` runs longer and makes p99.99 steadier,
   `just bench --pin 8,10,12` binds the publisher, the subscriber and the conductor to
   those CPUs, on Linux only, and `just bench --repeat 21` runs every benchmark
   twenty-one times and reports each one's median run by p99.99, with every run's
   p99.99 in the last column and in the result file. The bench host runs it with all
   three: on an isolated core, one run's 0.01 percent tail of a few hundred nanoseconds
   lands anywhere in a band 50 percent wide, while the median of twenty-one runs holds
   within 3 percent, so the median is what the gate reads and the twenty-one are what
   a reader checks.

2. Read the table. Every row is one benchmark with its count and its p50, p99, p99.99
   and max in nanoseconds, then its rate. No row is a mean: the data plane is judged at
   p99.99 and max (`CLAUDE.md`, invariant 2). The rows are:

   | Benchmark | What one sample is |
   | --- | --- |
   | `timer` | Two `Instant::now()` calls back to back: the harness's own overhead, present in every other row. |
   | `claim_commit_<body>` | The publisher's `try_claim`, writing the body, and `commit`, for a body of that many bytes. |
   | `one_way_<body>` | From the publisher's timestamp before its claim to the subscriber's handler receiving the frame, across two threads. |
   | `claim_commit_<body>_client` | The same claim, write and commit through the public API: `Publication::try_claim` against a real driver in the harness's process, whose conductor loads the publisher's tail at most once per millisecond on its duty cycle (ADR 0022), as a user's does. |
   | `one_way_<body>_client` | From the publisher's timestamp to `Subscription::poll`'s handler, through a real driver and the client's conductor hand-off; the path a user's message takes (ADR 0021). |
   | `slot_padding_<spacing>_load` | A load of one word while another thread stores to a word that many bytes away: 0 shares the cache line, 64 shares the 128-byte pair, 128 shares nothing (ADR 0011). |
   | `slot_padding_<spacing>_store` | The other side of the same pair: one store while the other thread loads. |
   | `record_block_<body>` | The recorder's block path: one scan of a 1 MiB block of committed frames (`TermReader::block`) and its one positional write into a segment file under the bench directory (`RecordingWriter::write`), per block, under the counting allocator (ADR 0025). It runs last, so its writes reach no other row, and has no baseline until the bench host records one. |

   The last column, present with `--repeat`, lists every run's p99.99 for the row. The
   last line says how many allocations the measured loops made per message. It is 0
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
   `bench/baseline.json` and prints each benchmark's p99.99 against the baseline's,
   with the lowest and highest run beside it when the result has several. A p99.99
   more than 5 percent above the baseline is a regression and the recipe exits 1; when
   the baseline was recorded with `--repeat`, the baseline's slowest run is the number
   compared, so a commit fails when its median tail is slower than anything the baseline
   measured, by more than 5 percent. On the bench host that is the rule on every row.
   Until ADR 0022 one row escaped it: `claim_commit_32_exclusive` landed anywhere
   between 270 and 430 ns at p99.99 from one process to the next, which was the
   harness's own conductor loading the publisher's tail on every spin; with that load at
   the driver's cadence
   ([ADR 0022](../decisions/0022-load-the-producer-position-once-per-millisecond.md))
   the row's 21 runs sit between 210 and 250 ns inside one process and its median holds
   within 2.5 percent across processes, and the baseline is recorded from that harness.
   The rule stays with the slowest run rather than the median because at these numbers
   every row's 21 runs spread 10 to 20 percent inside one process, which a 5 percent
   rule on medians would fail on an unchanged commit; the slowest run leaves about 20
   percent between a row's median and its failing number. Only the rows a message passes
   through, `claim_commit_*` and `one_way_*`, can fail; the `timer` and
   `slot_padding_*` rows run no Cavalry code and sit where one 10 ns step of the clock
   is 15 to 33 percent, so their change is printed and marked `reported`
   ([ADR 0017](../decisions/0017-gate-the-message-rows-and-report-the-host-probes.md)).
   Max is shown but does not fail the gate. A profile with no baseline is reported and
   passes: a laptop has none unless you record one, and the shared CI runners have
   none on purpose, because their own noise moved p99.99 by 40 to 100 percent between
   two runs of the same commit; CI compares on the bench host only (ADR 0016).

4. To record a baseline for a machine, run `just bench` on it when it is idle and then
   `python3 scripts/bench-compare.py --update`, which writes the newest result's
   percentiles into `bench/baseline.json` for that profile. Commit it with the
   histograms in the message. The bench host's baseline is set from its own CI run:
   download the `bench-results-bench-linux` artifact of a green run and pass each of
   its two files, the `bench-linux` and the `bench-linux-unchecked` result, to
   `--update`. A new row joins a baseline that already exists by adding its entry from
   that run's result and leaving the other rows as they were recorded, in a commit that
   names the row and the run; `--update` rewrites the whole profile, which is for a host
   whose numbers moved, never for one new row.

## How to verify it worked

- `ls bench/results/` shows a new `.json` file whose `profile` field is the one you
  expected.
- The table's last line reads `allocations per message: 0`.
- `just bench-compare` ends with `bench-compare: ok`.

## When it does not work

- `allocations per message` is not 0: a measured loop allocated. The gate has done its
  job; find the allocation with the counting allocator's report in the output and
  remove it. Nothing on the per-message path may allocate (`.claude/rules/data-plane.md`).
- `reported` beside a `timer` or `slot_padding` row that moved: expected; those rows
  flip between two adjacent clock steps from run to run and never fail the gate.
- `REGRESSION` on a p99.99: measure again on an idle machine before you believe either
  number; if it holds, the change under a data-plane crate is slower at the tail and
  does not merge until it is not, or the baseline is raised in a commit whose message
  says why. Both histograms go in the commit message either way. On the bench host,
  every row of the same commit repeated within 1 percent at p99.99 over three runs, so
  a regression there is not noise; check the host first with `bench/host/check.sh`,
  which the job also runs.
- `REGRESSION` after a change to `bench/` only, with no data-plane crate changed: the
  tail moved with the harness, not with the log. What a spinning harness thread does on
  every spin reaches the other two threads: a clock read on every spin of the conductor
  cost the raw rows about 100 ns at p99.99 on the bench host, in each of the processes
  measured, while the `_client` rows, whose driver reads its clock once per duty cycle,
  did not move. Before believing one run, run the same binary in several
  processes on the host and compare their medians, since one process lands in one mode
  ([ADR 0016](../decisions/0016-run-the-latency-gate-on-a-dedicated-host.md)). A person
  decides whether to re-record the baseline from that run, as step 4 says, in a commit
  whose message names the harness change and carries both histograms.
- `no baseline for profile`: expected on a laptop and on a shared runner. On the bench
  host it means the baseline was not recorded yet; record one as step 4 says.
- `--pin: could not bind the thread to CPU N`: the CPU is offline or outside the
  process's allowed set; `cat /sys/devices/system/cpu/online` says which CPUs exist.
- `the client benchmark failed: the client subscriber failed: ...`: the subscriber of the
  client rows got an error from `Subscription::poll`, and the rest of the line is that
  error, which says what to do. The publisher stops as soon as its subscriber has
  returned, instead of retrying a full log for ever.
- `the client benchmark failed: the publisher was back-pressured for over 10 s`: the
  client rows' subscriber is alive but has not read for 10 seconds. Look for a stopped
  or starved subscriber CPU under `--pin`, then run the harness again.
- The harness refuses to start: it needs to create files under `/dev/shm` or the
  system's temporary directory and at least two cores; the error names what it could
  not do.
