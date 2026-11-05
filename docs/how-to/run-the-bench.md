# Run the bench and compare it with the baseline

A how-to page: one task, measuring the data plane's latency and checking it against the
committed baseline, for a person at a laptop or for CI. The rules are in `CLAUDE.md`,
invariant 2 and the gates table; why the harness measures what it measures is
[ADR 0015](../decisions/0015-measure-in-process-with-per-runner-baselines.md), and why
the gate fails only on the dedicated bench hosts, `bench` and `grip`, and never on a
shared runner is
[ADR 0016](../decisions/0016-run-the-latency-gate-on-a-dedicated-host.md).

## Prerequisites

- A release build toolchain: `cargo` and `just`, as for every gate.
- An idle machine with at least three free cores, five for the UDP rows. The harness
  runs a publisher, a subscriber and a conductor thread, each spinning, and the UDP rows
  the driver's sender and receiver beside them; a browser or a build in the background
  moves p99.99 by more than the 5 percent the gate allows. The numbers that gate a
  commit come from the two bench hosts, `bench` and `grip`, each with four cores
  isolated for this ([Set up a bench host](set-up-a-bench-host.md)).
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
   `$CAVALRY_BENCH_PROFILE` when set, as CI sets `bench-linux`, `grip-linux`, `linux`
   or `macos`, and `local-<os>-<arch>` otherwise. Pass arguments after the recipe:
   `just bench --messages 5000000` runs longer and makes p99.99 steadier,
   `just bench --pin 8,10,12,14` binds the publisher, the subscriber and the driver's
   conductor to the first three CPUs, on Linux only, with the driver's recorder and
   replayer and the client's conductor on the lowest CPU outside them, so each pinned
   CPU runs one thread; the UDP rows put the sender on the third CPU and the receiver
   on the fourth, and the driver's conductor with the housekeeping threads, and three
   CPUs refuse them. `just bench --repeat 21` runs every benchmark
   twenty-one times and reports each one's median run by p99.99, with every run's
   p99.99 in the last column and in the result file. The p50, p99 and max beside it
   are the median run's; no other run's are kept, so the table and the result file
   cannot show whether a slow run's p50 or p99 moved. The bench hosts run it with all
   three: on an isolated core, one run's 0.01 percent tail of a few hundred nanoseconds
   lands anywhere in a band 50 percent wide, while the median of twenty-one runs holds
   within 3 percent, so the median is what the gate reads and the twenty-one are what
   a reader checks. `just bench --only claim_commit_32_exclusive` runs only the
   benchmarks with a row whose name contains that, here the exclusive log benchmark
   and both of its rows, for a profile of one row, and writes the result under the
   profile with `-only` appended, so `just bench-compare` then prints `no baseline` for
   it and never judges it as a full run; when no message row ran it judges the
   allocations by their count, which the result carries as `allocations` beside the
   ratio, and passes on 0. A name that no row contains is an error that lists the rows.

2. Read the table. Every row is one benchmark with its count and its p50, p99, p99.99
   and max in nanoseconds, then its rate. No row is a mean: the data plane is judged at
   p99.99 and max (`CLAUDE.md`, invariant 2). The rows are:

   | Benchmark | What one sample is |
   | --- | --- |
   | `timer` | Two readings of the harness's clock back to back, the time stamp counter calibrated once ([ADR 0028](../decisions/0028-stamp-bench-samples-with-the-time-stamp-counter.md)): the harness's own overhead, present in every other row, about 20 ns on the bench host and 0 or 42 ns on Apple silicon, whose counter ticks at 24 MHz. |
   | `claim_commit_<body>` | The publisher's `try_claim`, writing the body, and `commit`, for a body of that many bytes. |
   | `one_way_<body>` | From the publisher's timestamp before its claim to the subscriber's handler receiving the frame, across two threads. |
   | `claim_commit_<body>_client` | The same claim, write and commit through the public API: `Publication::try_claim` against a real driver in the harness's process, whose conductor loads the publisher's tail at most once every two milliseconds on its duty cycle (ADR 0022, ADR 0032), as a user's does. |
   | `one_way_<body>_client` | From the publisher's timestamp to `Subscription::poll`'s handler, through a real driver and the client's conductor hand-off; the path a user's message takes (ADR 0021). |
   | `claim_commit_<body>_recorded` | The client row's claim, write and commit while the driver's recorder writes the same stream to the durable log under the bench directory (ADR 0025): what recording costs the publisher, whose limit respects the recorder's position. The run fails unless the recording reached the publisher's final position. |
   | `one_way_<body>_recorded` | The client row's publish to delivery with the recorder on: one more reader of the log, on the driver's recorder thread, which runs on a housekeeping CPU under `--pin` (the CPUs left to everything but the pinned threads, [the host's set](set-up-a-bench-host.md#steps)) (until card 136 it shared the conductor's CPU, where ADR 0025's cost of 80 ns at p99.99 was measured). |
   | `claim_commit_<body>_udp` | The client row's claim, write and commit on a `cavalry:udp` publication whose endpoint is a port of `127.0.0.1`, with one message in flight: the publisher claims the next message once the subscriber's handler has had the last, so the window is never full and the row is the claim path, with the wait for a connection that dropped in it as in the other rows ([ADR 0044](../decisions/0044-gate-udp-on-the-two-bench-hosts.md)). Gated as the other message rows once the host's baseline holds it, `no baseline` until then. A tenth of `--messages` per run, after a warm-up of 10,000 messages rather than a rotation, since the driver pre-touches both logs when it makes them. |
   | `one_way_<body>_udp` | From the publisher's timestamp to `Subscription::poll`'s handler through the driver's sender, the loopback and the driver's receiver, one message in flight, so a sample is the path's latency and not the depth of the sender's window, which a publisher that runs ahead fills to 8 MiB. Under `--pin` the sender has the third CPU and the receiver the fourth, and the driver's threads busy-spin; unpinned they back off, as a driver on a laptop does. |
   | `udp_round_trip_<body>` | The link rows, run alone with `--link-here` and `--link-peer` (step 5): from the publisher's timestamp on `bench` to its subscriber's handler after the peer on `grip` has echoed the message across the direct cable, one message in flight, measured on one clock and gated against `bench-linux-link`. |
   | `udp_one_way_<body>` | Beside it: from the publisher's wall-clock stamp on `bench` to the peer's handler's on `grip`, which carries the offset between the two hosts' clocks: `bench` serves chrony's cooked time, its system clock plus the residual it has still to apply against its internet sources, and `grip` follows that, so the row is the path plus the "System time" line of `chronyc tracking` on `bench` when the line reads "slow of NTP time" and the path minus it when the line reads "fast of NTP time", hundreds of microseconds that move with the pools, which the `bench (udp)` job prints after the run, while the peer job's print on `grip` shows the link's own error, nanoseconds (the clock step of [Set up a bench host](set-up-a-bench-host.md#further-hosts-the-roles-the-direct-link-and-the-clock) says which way each wording goes); reported with that line and never gated; a sample below zero, which a "fast" line larger than the path gives, is recorded as 0 and counted under the table. |
   | `slot_padding_<spacing>_load` | A load of one word while another thread stores to a word that many bytes away: 0 shares the cache line, 64 shares the 128-byte pair, 128 shares nothing (ADR 0011). |
   | `slot_padding_<spacing>_store` | The other side of the same pair: one store while the other thread loads. |
   | `record_block_<body>` | The recorder's block path: one scan of a 1 MiB block of committed frames (`TermReader::block`) and its one positional write into a segment file under the bench directory (`RecordingWriter::write`), per block, under the counting allocator (ADR 0025). It runs last, so its writes reach no other row. Its change against the baseline is reported, not gated: it is not a row a message passes through (ADR 0017). The bench host's baseline for it was first recorded at 7f62673 (card 109). |

   The last column, present with `--repeat`, lists every run's p99.99 for the row. The
   `producer position loads` line under the table counts the loads of the producer
   position by the harness's own conductor in the run that loaded most, for the two
   log rows only, since the client rows run the driver's conductor: about one per
   two milliseconds of the run, the driver's cadence (ADR 0022, ADR 0032), written to the
   result as
   `producer_position_loads`, so each host's cadence is on record (card 161). The
   `allocations by benchmark` line gives each benchmark's count, summed over the
   runs, under the name the retries use and `record` for the recorder's block row,
   written to the result as `allocations_by_benchmark`, so a count above 0 names the
   loop to look at. The
   last line says how many allocations the measured loops made per message. It is 0
   or the run has failed: the gate requires zero allocations per message, and the
   failure line repeats the benchmarks that allocated. When no
   message row ran, under `--only`, the line says so in place of the ratio and the
   result's `allocations_per_message` is `null`, the count is in the result as
   `allocations`, and any allocation still fails the run.

   `just bench-unchecked` runs the same harness with the data-plane checks built out,
   under its own profile, `<profile>-unchecked`, and its own baseline: the difference
   between the two is what the checks cost (ADR 0010). Nothing else is ever built with
   that flag.

   The `publisher retries` line under the table counts, for each message row, the most
   times in any run the publisher's claim was refused and tried again: back pressure or
   a rotation on the log rows, and on the UDP rows a `not connected` before the first
   message as well.

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
   more than 5 percent and by more than one clock step. A bench host's baseline holds
   the runs of every process the host has run at the baseline's tree, three at least,
   so "anything the baseline measured" spans processes as well as runs: on kip a row's
   median moves by a tenth, and by half on the exclusive claim row, from one process
   of the same tree to the next, and the first line then says `baseline from N
   processes`
   ([ADR 0033](../decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
   The harness measures its
   clock's step at start, prints it in its first line in nanoseconds and in the
   counter's own ticks (`clock step 10 ns (37 ticks)` on `bench`, whose counter
   moves by 37 ticks every 10 ns, `clock step 1 ns (2 ticks)` on `kip`, where one
   step is below 5 percent of every gated row and the 5 percent rule decides alone,
   and `clock step 41 ns (41 ticks)` on an Apple silicon laptop, whose counter moves
   less often than the harness reads it and whose step is therefore the jump it makes,
   [ADR 0037](../decisions/0037-read-a-coarse-counters-step-from-its-jump.md))
   and writes it into the result as `clock_step_ns`; a
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
   two runs of the same commit; CI compares on the bench hosts only, `bench` in the
   `bench (linux)` job and `grip` in the `bench (grip)` job
   ([ADR 0034](../decisions/0034-run-the-gate-on-grip-and-the-soak-on-kip.md)), each
   against its own profile's baseline (ADR 0016).

4. To record a baseline for a machine, run `just bench` on it when it is idle and then
   `python3 scripts/bench-compare.py --update`, which writes the newest result's git,
   date, percentiles and clock step into `bench/baseline.json` for that profile, as one
   process. Commit it with the histograms in the message. A bench host's baselines are
   recorded from every process the host has run at one tree, three at least
   ([ADR 0033](../decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
   A tree is a commit and every later commit between which
   `git diff --stat <first> <later> -- crates/ bench/src bench/host bench/Cargo.toml
   Cargo.toml Cargo.lock justfile` prints nothing, so a dependency bump, a change to
   the recipes that build the harness or to the host check ends a tree as a code
   change does; a run of master's job at the tree is a process of it, and so is a run
   of a pull request whose head is at the tree. A push gives the first run, and
   `gh run rerun <run id> --job <job id>`, or the job's re-run button on the run's
   page, gives the next at the same commit whether the first passed or failed. Download
   each attempt's artifact before the next re-run, `bench-results-bench-linux` from
   `bench (linux)` for `bench` or `bench-results-grip-linux` from `bench (grip)` for
   `grip`: a re-run replaces
   the run's artifact of the same name, and the run's page and the artifacts endpoint
   keep only the newest attempt's, so an attempt whose artifact was not saved first is
   a lost process (attempts 1 and 2 of run 41196273792 went that way on 2026-10-31).
   Pass one profile's
   files to one `--update`, the `bench-linux` results in one call and the
   `bench-linux-unchecked` results in another, or the `grip-linux` and
   `grip-linux-unchecked` results for `grip`; it refuses files of two profiles or of two
   harness settings, and two processes. The entry then holds, for each percentile, the
   largest value over the processes, the processes' runs in order in `p99_99_ns_runs`,
   each `repeat` long, and `processes: N`. Take every run that reached the profile, never a choice
   among them: a run that failed the gate is one of them, and the artifact is uploaded
   whether or not the run passed, so a run that failed after a change to the host is
   one to record from ([Set up a bench host](set-up-a-bench-host.md#updating-the-host)).

   When a later run at the baseline's tree fails a row, it has measured a part of the
   host's band the baseline did not hold, since nothing the gate measures changed:
   download its artifact and add the process with
   `python3 scripts/bench-compare.py --join <that profile's result file>`, which
   appends its runs to the entry and raises the slowest run where it read higher, in a
   commit whose message names the run and the row. A run at another tree never joins:
   a pull request that changes the tree's paths is judged, and a red there is a
   change. Nor does a run whose row moved as a whole: when the row's p50 and p99 moved
   with its p99.99 and its rate fell against the other processes at the tree, the
   host was disturbed during the run (the troubleshooting list below); re-run the job
   and name the disturbance where the host's journal shows it. A baseline is re-recorded at a new tree when master's own run fails or
   brushes the gate there with no data-plane change behind it, from the tree's first
   three processes and every one after them; the commit lists every gated row's
   slowest run before and after and names the cause of every row that loosens by more
   than 5 percent, or the card that will find it. A new row joins a baseline that
   already exists by adding its entry from the tree's processes that have it and
   leaving the other rows as they were recorded, in a commit that names the row and
   the runs; `--update` rewrites the whole profile, which is for a host whose numbers
   moved, never for one new row.

5. Measure the link between the two bench hosts, which the `bench (udp)` job does on
   every push with its peer job `bench (udp peer)`
   ([ADR 0044](../decisions/0044-gate-udp-on-the-two-bench-hosts.md)). On `grip`, the
   peer, in one terminal:

   ```sh
   # ignore: run on grip, the host that echoes
   just bench --echo --link-here grip-link:40183 --link-peer bench-link:40182 --pin 4,5,6,7
   ```

   On `bench`, in another:

   ```sh
   # ignore: run on bench, the host that measures
   just bench --link-here bench-link:40182 --link-peer grip-link:40183 --messages 100000 --repeat 21 --pin 8,10,12,14
   just bench-compare
   ```

   `bench-link` and `grip-link` are the cable's two ends in both hosts' `/etc/hosts`
   ([Set up a bench host](set-up-a-bench-host.md#further-hosts-the-roles-the-direct-link-and-the-clock)).
   Each side subscribes on its own end and publishes to the other's, and waits up to
   ten minutes for the other to connect, so the order does not matter. In CI each job
   first waits for the run's other job to be in progress
   (`scripts/wait-for-partner-job.sh`, polling the run's jobs every 20 s for up to 45
   minutes), because the partner can sit queued behind another run's job on its host
   for longer than those ten minutes, and the end that started first died waiting;
   a job whose partner completed before it started, or did not start within the 45
   minutes, fails with a message that says so, and the pair is re-run when both hosts
   are free. A partner name that is still not one of the run's jobs after a minute, and
   an API that refuses the job's token or goes unanswered three times in a row, end the
   wait at once with the reason (card 212). The peer prints
   `the stream from <address> is connected, echoing`, then, when `bench`'s run ends the
   stream, `echoed N messages from <address> back to <host:port> in <s> s`, and exits 0;
   it exits 1 when the stream goes without its end. The measuring side prints the two
   link rows alone under the profile with `-link` appended, `bench-linux-link` in CI,
   which `just bench-compare` holds to its own baseline once one is recorded from three
   of the job's processes at one tree, as the host's are, and writes no other row.
   `--messages` is the count per run as given, since every message waits for its echo:
   100,000 at about 100 µs is ten seconds a run, after the UDP rows' warm-up of 10,000.
   The one-way row needs the offset
   between the two clocks to be read, and it is `bench`'s: chronyd serves its cooked
   time, the system clock plus the residual it has still to apply, and `grip` follows
   that, so the row is the path plus the "System time" line of `chronyc tracking` on
   `bench` when the line reads "slow of NTP time" and the path minus it when it reads
   "fast of NTP time", which the `bench (udp)` job prints after the run; `grip`'s
   `chronyc sourcestats -v`, which the peer job prints after the echo, gives the link's
   own error on the `bench-link` line, nanoseconds, and not the offset the row carries
   ([Set up a bench host](set-up-a-bench-host.md#further-hosts-the-roles-the-direct-link-and-the-clock),
   whose clock step says which way each wording goes).
   Under `--pin` the echo runs on the first CPU, the sender on the third and the
   receiver on the fourth.

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
  job; the `allocations by benchmark` line above it names the benchmark whose run
  allocated, with its count summed over the runs, and the failure line repeats it,
  `the measured loops allocated 1 times, by benchmark udp 1`; the result keeps it as
  `allocations_by_benchmark`. A reallocation counts as an allocation: a `Vec` or
  `String` grown past a capacity it already had counts once per grow, as one grown
  from empty does. Before
  [ADR 0047](../decisions/0047-keep-the-first-allocations-backtrace-in-the-bench.md)
  the count left reallocations out, so a loop that grows a vector which already has a
  capacity fails the gate now where it passed before; size the vector at setup. The
  harness's loops allocate on no path but their error returns, so the allocation is
  in the crates the row runs: `cavalry-shm` for a log row, `cavalry-client` and the
  driver's conductor for a client row, and the driver's sender and receiver as well
  for a UDP or link row. A count far below the run's messages is a rare event, not
  one per message. The frames under the failure line say where it is (the next
  entry). Find it and remove it: nothing on the per-message path may allocate
  (`.claude/rules/data-plane.md`).
- Frames under the failure line, after `the first allocation a measured loop saw`:
  the backtrace of the first allocation any measured loop counted, captured once per
  process inside the allocator's hook. It has the function names the release build
  keeps and no line numbers. This is the capture of a `format!` added for one run to
  the exclusive row's publisher loop, on the main thread, on Linux:

  ```text
     0: <cavalry_bench::CountingTracker as tracking_allocator::AllocationTracker>::allocated
     1: <tracking_allocator::allocator::Allocator<std::alloc::System> as core::alloc::global::GlobalAlloc>::alloc
     2: alloc::fmt::format::format_inner
     3: cavalry_bench::run_pass
     4: cavalry_bench::main
     5: std::sys::backtrace::__rust_begin_short_backtrace::<fn() -> std::process::ExitCode, std::process::ExitCode>
     6: std::rt::lang_start::<std::process::ExitCode>::{closure#0}
     7: std::rt::lang_start_internal
     8: main
     9: <unknown>
    10: __libc_start_main
    11: _start
  ```

  Read it from the top:

  - Frames 0 and 1 are the harness's hook and the allocator, in every capture.
  - Frame 2 is the allocation: `alloc::fmt::format::format_inner` for a `format!`,
    or `<alloc::raw_vec::RawVecInner>::finish_grow` over
    `<alloc::raw_vec::RawVec<u64>>::grow_one` for a push past a vector's capacity.
  - The first frame of a Cavalry crate under it is the function that allocated, or
    the function the release build inlined it into. The `format!` above was in
    `run_log`'s loop, which the build inlined into `cavalry_bench::run_pass`.
  - The frame `std::sys::backtrace::__rust_begin_short_backtrace` names the thread.
    On the main thread its type is `fn() -> std::process::ExitCode`, and the frame
    above it, `cavalry_bench::main`, is the thread. On a spawned thread its two type
    parameters are the closure the thread was spawned with and what the thread
    returns: `cavalry_bench::run_log::{closure#9}` and
    `Result<hdrhistogram::Histogram<u64>, String>` are the log rows' subscriber, and
    `<cavalry_driver::Driver>::start_with_threads<…>::{closure#7}` and `()` are the
    driver's conductor. `{closure#9}` is the tenth closure in `run_log`'s source,
    counted from 0, the `map_err` closures included. The frame above it is the
    thread's loop when the build did not inline it, such as
    `<cavalry_driver::conductor::Conductor>::run`.
  - The frames under it are the standard library's thread start and the C library's.
    They are the same on every thread of a system, `<unknown>` where the C library
    has no symbols, and may read differently on macOS.

  [ADR 0047](../decisions/0047-keep-the-first-allocations-backtrace-in-the-bench.md)
  has the captures from a spawned thread and from the driver's conductor.
- `no backtrace was captured for the first allocation` under the failure line, in
  place of the frames: the count and the benchmark in the failure line are right,
  but the first allocation's capture was not there when the harness read it, after
  the last row and for up to five seconds after. Every row joins the threads it
  starts, and a thread finishes the capture before its allocation returns, so this
  means a thread outlived its row or a capture stalled for five seconds. Run the same
  `just bench` command again: each process captures afresh. If the line prints
  again, look in the row the failure line names for a thread it starts and does not
  join.
- `reported` beside a `timer` or `slot_padding` row that moved: expected; those rows
  flip between two adjacent clock steps from run to run and never fail the gate.
- `within a step` beside a gated row: the row is over 5 percent above the baseline's
  slowest run but by no more than one step of the clock, which on `bench` is
  10 ns of a 160 ns row; the gate cannot tell that from an unchanged commit and passes
  it ([ADR 0029](../decisions/0029-fail-a-gated-row-only-beyond-one-clock-step.md)).
  Two steps fail. On `kip` the step is 1 ns, so the line never appears there.
- `clock step 41 ns (41 ticks)` or another step of tens of nanoseconds on a laptop:
  the machine's counter moves less often than the harness reads it, and the step is
  the jump it makes when it moves, not a divisor of its readings
  ([ADR 0037](../decisions/0037-read-a-coarse-counters-step-from-its-jump.md)). The
  percentiles there resolve to that step, and a row's move below it is not a move. Such
  a machine is a report, not a gate; it has no baseline unless you record one.
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
  says why. Both histograms go in the commit message either way. On a bench host a
  row's p99.99 moves between processes of one tree with no change behind it. On `kip`
  the concurrent and client claim rows' medians move by a tenth and the exclusive
  row's by half, 102 to 148 ns over the eight processes at the tree of 4a09b6e (card
  168). On `bench` a row's median moves by up to three 10 ns steps: over the twelve
  processes at the same tree the checked `one_way_32_exclusive` row read 210 to 230
  with its p50 at 140 in every one, and the unchecked `claim_commit_32_exclusive` row
  read 120 to 150. So a `REGRESSION` on a bench host is one of four things. On a
  changed tree it is a change. On the baseline's tree, with the row's p50 and its rate
  where the other processes read them, it is a part of the host's band the baseline
  did not hold, and the process joins the baseline with step 4's `--join` on either
  host. A row whose p50 and p99 moved with its p99.99 is the next
  bullet, and a red after a change to `bench/` alone is the bullet after it. Check the
  host first with `bench/host/check.sh`, which the job also runs.
- `REGRESSION` on a row whose p50 and p99 moved with its p99.99, and whose rate fell
  against the host's other processes at the tree: the host was disturbed during that
  row, not the band and not a change. A band leaves p50 and the rate where they were;
  on `bench` on 2026-10-30 the unchecked `claim_commit_32_concurrent` row read p50 90,
  p99 120 and p99.99 170 at 8.17 million claims a second in one process, where the
  six other processes of the same tree read 80, 90 to 100 and 150 at 9.35 to 9.45
  million, and the next run read 80, 100 and 150 again. Re-run the job with
  `gh run rerun <run id> --job <job id>`, read the host's journal for the bench's
  window (`journalctl --since <start> --until <end>` on the host) and name what it
  shows in the re-run's record; such a process joins no baseline
  ([ADR 0033](../decisions/0033-record-a-bench-baseline-from-every-process-at-one-tree.md)).
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
  outside the pins, with the driver's conductor there too during the UDP rows. A p99.99 that a second host reads far above `bench`'s with the same p50 and
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
- `--pin: no CPU outside P,S,C,R in the process's allowed set`: the harness was started
  from a shell or a cgroup whose allowed set lies inside the pins, and the driver's
  recorder and replayer and the client's conductor need one CPU outside them. `taskset -cp
  $$` prints the shell's allowed set. Run the harness from a shell that holds a
  housekeeping CPU too: `taskset -c 0-7 just bench --pin 8,10,12,14` runs it with CPUs 0
  to 7 in the set, and the pins themselves need not be in it, since a thread may bind
  itself to any online CPU its cgroup allows, as the bench host's runner does from a set
  of housekeeping CPUs alone.
- `--pin 8,10,12: the UDP rows need a fourth CPU for the receiver`: `--pin` named three
  CPUs and a UDP row is to run. Pass four, `--pin 8,10,12,14` on `bench` and
  `--pin 4,5,6,7` on `grip`, or `--only` a row without `udp` in its name; a receiver
  sharing a measured thread's CPU, or running with the housekeeping threads, gives a
  number the baseline cannot be compared with.
- `the udp benchmark failed: the stream did not connect in 10 s`, or the link rows' or
  the echo peer's `did not connect in 600 s`: the publication's setup frames were not
  answered, or no setup frame reached the subscription's endpoint, and the message says
  which. On the loopback that is a port another process took between the harness's
  check and the driver's bind, so run again; across the link it is the other host's
  harness not running, or running with the ends swapped, so that each host publishes
  to a port nobody receives on: `--link-here` is this host's end and `--link-peer` the
  other's, on both hosts, and `cavalry doctor` on either host says
  `udp-publication-not-connected` or `udp-subscription-no-image` while the harness
  waits ([Publish between two hosts](publish-between-two-hosts.md)).
- `message N of M did not reach the subscriber in 10 s`: with one message in flight the
  stream stopped. Across the link the peer stopped echoing, or the cable lost a datagram
  the sender no longer holds; on the loopback the driver's sender or receiver stalled.
  `cavalry doctor` on the host names the image's loss from its counters.
- The peer's `the stream from <address> went after N messages without its end`: the
  measuring side's harness stopped before it closed its publication, or the image timed
  out on the peer after 10 s without a datagram; the measuring side's own error says
  which, and the line's `image.ended` and `image.timed_out` counts say which the peer
  saw. A peer that ends this way exits 1, so the pair's run is red on both sides.
- The peer's `no message for 600 s after N echoed`: the measuring side stopped sending
  without ending the stream and its driver kept the image alive with heartbeats, a
  harness stalled on the other host; its own log says where.
- `a send endpoint still carried a publication 3 s past the publication linger`: the
  driver did not remove the closed publication once its linger ended, five seconds
  after the close at most; `cavalry doctor` on the host lists the endpoint with its
  count, and the driver's sender is the thread to look at.
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
