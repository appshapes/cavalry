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
   `just bench --pin 8,10,12` binds the publisher, the subscriber and the driver's
   conductor to those CPUs, on Linux only, with the driver's recorder and replayer and
   the client's conductor on the lowest CPU outside them, so each pinned CPU runs one thread, and `just bench --repeat 21` runs every benchmark
   twenty-one times and reports each one's median run by p99.99, with every run's
   p99.99 in the last column and in the result file. The bench host runs it with all
   three: on an isolated core, one run's 0.01 percent tail of a few hundred nanoseconds
   lands anywhere in a band 50 percent wide, while the median of twenty-one runs holds
   within 3 percent, so the median is what the gate reads and the twenty-one are what
   a reader checks. `just bench --only claim_commit_32_exclusive` runs only the
   benchmarks with a row whose name contains that, here the exclusive log benchmark
   and both of its rows, for a profile of one row, and writes the result under the
   profile with `-only` appended, so `just bench-compare` then prints `no baseline` for
   it and never judges it as a full run; a name that no row contains is an error that
   lists the rows.

2. Read the table. Every row is one benchmark with its count and its p50, p99, p99.99
   and max in nanoseconds, then its rate. No row is a mean: the data plane is judged at
   p99.99 and max (`CLAUDE.md`, invariant 2). The rows are:

   | Benchmark | What one sample is |
   | --- | --- |
   | `timer` | Two readings of the harness's clock back to back, the time stamp counter calibrated once ([ADR 0028](../decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md)): the harness's own overhead, present in every other row, about 20 ns on the bench host and 0 or 42 ns on Apple silicon, whose counter ticks at 24 MHz. |
   | `claim_commit_<body>` | The publisher's `try_claim`, writing the body, and `commit`, for a body of that many bytes. |
   | `one_way_<body>` | From the publisher's timestamp before its claim to the subscriber's handler receiving the frame, across two threads. |
   | `claim_commit_<body>_client` | The same claim, write and commit through the public API: `Publication::try_claim` against a real driver in the harness's process, whose conductor loads the publisher's tail at most once per millisecond on its duty cycle (ADR 0022), as a user's does. |
   | `one_way_<body>_client` | From the publisher's timestamp to `Subscription::poll`'s handler, through a real driver and the client's conductor hand-off; the path a user's message takes (ADR 0021). |
   | `claim_commit_<body>_recorded` | The client row's claim, write and commit while the driver's recorder writes the same stream to the durable log under the bench directory (ADR 0025): what recording costs the publisher, whose limit respects the recorder's position. The run fails unless the recording reached the publisher's final position. |
   | `one_way_<body>_recorded` | The client row's publish to delivery with the recorder on: one more reader of the log, on the driver's recorder thread, which runs on a housekeeping CPU under `--pin` (the CPUs left to everything but the pinned threads, [the host's set](set-up-a-bench-host.md#steps)) (until card 136 it shared the conductor's CPU, where ADR 0025's cost of 80 ns at p99.99 was measured). |
   | `slot_padding_<spacing>_load` | A load of one word while another thread stores to a word that many bytes away: 0 shares the cache line, 64 shares the 128-byte pair, 128 shares nothing (ADR 0011). |
   | `slot_padding_<spacing>_store` | The other side of the same pair: one store while the other thread loads. |
   | `record_block_<body>` | The recorder's block path: one scan of a 1 MiB block of committed frames (`TermReader::block`) and its one positional write into a segment file under the bench directory (`RecordingWriter::write`), per block, under the counting allocator (ADR 0025). It runs last, so its writes reach no other row. Its change against the baseline is reported, not gated: it is not a row a message passes through (ADR 0017). The bench host's baseline for it was first recorded at 7f62673 (card 109). |

   The last column, present with `--repeat`, lists every run's p99.99 for the row. The
   `producer position loads` line under the table counts the loads of the producer
   position by the harness's own conductor in the run that loaded most, for the two
   log rows only, since the client rows run the driver's conductor: about one per
   millisecond of the run, the driver's cadence (ADR 0022), written to the result as
   `producer_position_loads`, so each host's cadence is on record (card 161). The
   last line says how many allocations the measured loops made per message. It is 0
   or the run has failed: the gate requires zero allocations per message. When no
   message row ran, under `--only`, the line says so in place of the ratio and the
   result's `allocations_per_message` is `null`; any allocation still fails the run.

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
   more than 5 percent above the baseline, and more than one step of the harness's
   clock above it, is a regression and the recipe exits 1; when the baseline was
   recorded with `--repeat`, the baseline's slowest run is the number compared, so a
   commit fails when its median tail is slower than anything the baseline measured, by
   more than 5 percent and by more than one clock step. The harness measures its
   clock's step at start, prints it in its first line in nanoseconds and in the
   counter's own ticks (`clock step 10 ns (37 ticks)` on the bench host, whose counter
   moves by 37 ticks every 10 ns) and writes it into the result as `clock_step_ns`; a
   row over 5 percent but within one step is printed `within a step` and passes
   ([ADR 0029](../decisions/0029-fail-a-gated-row-only-beyond-one-clock-step.md)). The
   baseline's entry carries the step its recording measured, and the floor is the
   smaller of the two steps, so a change to the harness that over-read its step cannot
   widen the gate by itself; when they differ, the first line says which it took. A
   result from before the field is printed `clock step unknown` and judged by the 5
   percent rule alone. On the bench host that is the rule on every row.
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
   `python3 scripts/bench-compare.py --update`, which writes the newest result's git,
   date, percentiles and clock step into `bench/baseline.json` for that profile. Commit
   it with the histograms in the message. The bench host's baseline is set from its own
   CI run: download the `bench-results-bench-linux` artifact of a green run and pass
   each of its two files, the `bench-linux` and the `bench-linux-unchecked` result, to
   `--update`. A new row joins a baseline that already exists by adding its entry from
   that run's result and leaving the other rows as they were recorded, in a commit that
   names the row and the run; `--update` rewrites the whole profile, which is for a host
   whose numbers moved, never for one new row.

## How to verify it worked

- `ls bench/results/` shows a new `.json` file whose `profile` field is the one you
  expected.
- The table's last line reads `allocations per message: 0`.
- `just bench-compare` ends with `bench-compare: ok`.
- On a Linux host, every Cavalry mapping is resident and no measured thread took a
  page fault in one. While a benchmark runs, every mapping under its directory,
  `/dev/shm/cavalry-bench-<pid>-<benchmark>`, has `Rss` equal to `Size` in the
  process's `smaps`, because every mapping is pre-touched when it is made
  ([ADR 0027](../decisions/0027-pre-touch-shared-mappings-at-map-time.md)); this needs
  no root:

  ```text
  pid=$(pgrep -x cavalry-bench)
  awk '/cavalry-bench-/ {m=$0} /^Size:/ {s=$2} /^Rss:/ && m {print s, $2, m; m=""}' /proc/$pid/smaps
  ```

  With the kernel's tracepoints enabled as root for one run, read-only apart from the
  trace buffer, no `exceptions:page_fault_user` event on a pinned CPU has an `address=`
  inside one of those mappings, whose ranges are the first column of the process's
  `maps` taken during the same benchmark; a fault there is a bug, not a cost. The faults
  that remain on a pinned CPU are the harness's own, and each is a burst at a row's
  setup, before its measured loop: the 54 pages of a histogram's counts, written once
  when the histogram is made, so that a rare sample never first-touches one inside the
  loop. A fault spread through a loop is a bug. `sched:sched_waking` to the pinned CPUs
  names no Cavalry thread as the wakee, apart from the driver's conductor waking from
  idle on its own CPU: the driver's recorder and replayer and the client's conductor
  run on the lowest CPU outside the pins, so nothing shares a pinned CPU with a measured thread
  (`ps -L -o tid,psr,comm` during a run shows one Cavalry thread per pinned CPU).

  ```text
  T=/sys/kernel/tracing
  echo "target_cpu == 8 || target_cpu == 10 || target_cpu == 12" > $T/events/sched/sched_waking/filter
  echo 1 > $T/events/sched/sched_waking/enable
  echo 1 > $T/events/exceptions/page_fault_user/enable
  echo 1 > $T/tracing_on
  # run the harness with --pin 8,10,12; while a benchmark runs, keep its mappings' ranges:
  cat /proc/$(pgrep -x cavalry-bench)/maps > maps.txt
  # when the harness has finished:
  echo 0 > $T/tracing_on
  grep page_fault_user $T/trace > faults.txt
  ```

## When it does not work

- `allocations per message` is not 0: a measured loop allocated. The gate has done its
  job; find the allocation with the counting allocator's report in the output and
  remove it. Nothing on the per-message path may allocate (`.claude/rules/data-plane.md`).
- `reported` beside a `timer` or `slot_padding` row that moved: expected; those rows
  flip between two adjacent clock steps from run to run and never fail the gate.
- `within a step` beside a gated row: the row is over 5 percent above the baseline's
  slowest run but by no more than one step of the clock, which on the bench host is
  10 ns of a 160 ns row; the gate cannot tell that from an unchanged commit and passes
  it ([ADR 0029](../decisions/0029-fail-a-gated-row-only-beyond-one-clock-step.md)).
  Two steps fail.
- `clock step unknown` in `bench-compare`'s first line: the result was written by a
  harness from before the step was measured, so the 5 percent rule alone applies,
  whatever step the baseline carries; the failure line says so in place of a step, and
  `--update` writes no step from it. Run `just bench` again; every result the harness
  writes now carries the step.
- `the smaller of the result's 20 and the baseline's 10` in that line: the harness read
  a different step from the one the baseline was recorded with, and the smaller one is
  the floor, so a harness that over-reads its step gains nothing. If the host's clock
  changed, re-record the baseline (step 4 above); if the harness's measurement changed,
  that change is the one to look at.
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
  whose message names the harness change and carries both histograms. A change to the
  harness's code can also move the raw rows through the compiler alone: 60 ns at p99.99
  and 20 ns at p50 on the `one_way` rows, blamed on a 16 KiB heap allocation at startup,
  was `TermReader::poll` compiled out of line in one binary and inlined into the
  subscriber's loop in the other, flipped by the size of the closure around it (card
  144). `poll` is `#[inline(always)]` since then. When the raw rows move with a change
  that touched no data-plane crate, compare the two binaries' symbols before the heap or
  the host: `nm -C target/release/cavalry-bench | grep 'TermReader>::poll'` prints the
  symbol only when it is out of line, and the publisher's `try_claim` and `commit` are
  out of line in every build.
- A fault in `faults.txt` whose address lies in a Cavalry mapping, or a mapping whose
  `Rss` is below its `Size`: a mapping was not pre-touched, which is a bug in
  `cavalry-shm`, where every `create` and `open` pre-touches before it hands the
  mapping out and `tests/pre_touch.rs` reads the residency back
  ([ADR 0027](../decisions/0027-pre-touch-shared-mappings-at-map-time.md)). A fault in
  the harness's own memory inside a measured loop, rather than in the burst at the
  row's setup: the histogram was not written through before the loop, which
  `histogram()` in `bench/src/main.rs` does. A wake-up of a Cavalry thread on a pinned
  CPU from another CPU: a thread shares that CPU with a measured one; `ps -L -o
  tid,psr,comm` during the run shows where each thread sits, and under `--pin` the
  driver's recorder and replayer and the client's conductor belong on the lowest CPU
  outside the pins. A p99.99 that a second host reads far above `bench`'s with the same p50 and
  none of the above is the host's own tail, and its profile's baseline is where it is
  held. The second bench host's 1.6 µs against `bench`'s 0.6 µs, with no fault and no
  wake-up in its loops, was the clock, the next bullet and
  [ADR 0028](../decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md).
- A p99.99 far above the p99 whose slow samples come one or two messages at a time,
  spaced exactly 1 ms apart: the host's timekeeper, not the log. The harness's clock is
  the time stamp counter ([ADR 0028](../decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md)),
  which reads no shared data; a host whose counter is not invariant falls back to the
  monotonic clock, and every `clock_gettime` after the housekeeping CPU's tick then pays
  for the timekeeper's write. `grep -o -E 'constant_tsc|nonstop_tsc' /proc/cpuinfo | sort -u`
  says which the host has.
- `no baseline for profile`: expected on a laptop and on a shared runner. On the bench
  host it means the baseline was not recorded yet; record one as step 4 says.
- `--pin: could not bind the thread to CPU N`, or `could not bind the driver's conductor`
  (or `recorder`) `to CPU N`: the CPU is offline or outside the process's allowed set;
  `cat /sys/devices/system/cpu/online` says which CPUs exist. The harness's own threads
  fail as they pin, before a row runs. A driver thread pins itself from the hook
  `Driver::start_with_threads` runs first on that thread, so the harness checks the
  conductor's pin once the driver has answered its first command, and both driver
  threads' pins again once the driver has closed at the end of the row, which is where
  the recorder's is first known. A failed pin fails the run either way, and the row's
  numbers are never written.
- `--pin: no CPU outside P,S,C in the process's allowed set`: the harness was started
  from a shell or a cgroup whose allowed set lies inside the three pins, and the
  driver's recorder and replayer and the client's conductor need one CPU outside them. `taskset -cp
  $$` prints the shell's allowed set. Run the harness from a shell that holds a
  housekeeping CPU too: `taskset -c 0-7 just bench --pin 8,10,12` runs it with CPUs 0 to
  7 in the set, and the pins themselves need not be in it, since a thread may bind
  itself to any online CPU its cgroup allows, as the bench host's runner does from a set
  of housekeeping CPUs alone.
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
