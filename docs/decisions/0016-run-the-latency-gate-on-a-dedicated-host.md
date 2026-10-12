# 0016. Run the latency gate on a dedicated host

- **Status:** Accepted
- **Date:** 2026-10-12
- **Invariants affected:** 2, 5, 6
- **Supersedes:** the CI half of ADR 0015's decision, the comparison on the shared
  runners with `linux` and `macos` baselines, and its "pinning the threads: lost for
  now"; the harness, the histograms, the baseline per profile and the rest of ADR 0015
  stand. Amends ADR 0004's "Blacksmith runners for every job": the latency gate's job
  is the exception.

## Context

Invariant 2 is enforced by `just bench-compare`: a p99.99 more than 5 percent above the
baseline fails CI. The first comparison with a recorded baseline, CI run 38171206638 of
commit 467cc49 on 2026-10-11, failed on both Blacksmith runners with no change under a
data-plane crate. The `timer` row, two `Instant::now()` calls back to back, moved 43
percent on Linux and 100 percent on macOS at p99.99; `one_way_32_exclusive` read
348 µs against a baseline of 11 µs on Linux, one descheduling of the subscriber; the
macOS unchecked run saw 77,758 back-pressure retries against 77. Rjae's decision was to
halt feature work rather than loosen the rule: "halt until I provide you with the level
of resource(s) that a real capital markets or sports betting organization would utilize
to process the level of volume we are targeting".

A shared runner is a virtual machine with neighbours, a scheduler tick, interrupts and
no core isolation. Nothing in the harness can tell its noise from a regression, and a
rule wide enough to pass it gates nothing (`CLAUDE.md`, Configuration and defaults:
"Never add a knob to make a benchmark pass"). What a trading or betting shop does is
own the machine: bare metal, a tuned Linux, isolated cores, nothing else running. On
2026-10-12 Rjae provided one, the host `bench`: an AMD Ryzen 7 2700X, 8 cores in two
core complexes of 4, 60 GiB, Ubuntu 26.04.1 LTS, kernel 7.0, registered as a GitHub
Actions runner of the organization with the label `bench-linux`, in a runner group that
holds this repository alone.

Measured on that host before any tuning, the same commit's three runs of 5,000,000
messages repeated within 2 percent at p99.99 on every log row but one, a 170 µs
preemption of the subscriber that put 500 messages over 100 µs in one run. Tuning,
pinning and the harness's warm-up then took the log rows from 5.6 and 9.1 µs to under
0.7 µs at p99.99, as the Evidence section shows; the 5.6 µs was 1,500 first touches of
a page of the 12 MiB log, which the 100,000-message warm-up covered only half of.

## Decision

The latency gate runs on the dedicated bench host and nowhere else. CI's `bench (linux)`
job runs on the self-hosted runner labelled `bench-linux`, on pushes to `master` and on
pull requests from this repository only, never on a fork's code; it runs
`bench/host/check.sh` first, which prints the host's state and fails unless the CPUs the
harness will pin to are isolated and tickless, SMT is off, the governor is performance,
boost is off, transparent huge pages are off and swap is off; then `just bench` and
`just bench-unchecked` with `--pin 8,10,12`, each followed by `just bench-compare`
against the profiles `bench-linux` and `bench-linux-unchecked`, whose baselines are
recorded from the host's own run; and it keeps the results as an artifact. Branch
protection requires it beside the two gates jobs. The gates jobs on the shared runners
still run `just bench` and keep the table and the artifact, as a report of what a cloud
VM and a laptop see, with no comparison and no baseline. The host is built from the
repository: `bench/host/setup-ubuntu.sh` isolates one core complex with `isolcpus`,
`nohz_full`, `rcu_nocbs` and `irqaffinity`, takes SMT, deep C-states, the lockup
watchdog and transparent huge pages off, fixes the clock with the performance governor
and no boost, turns swap and unattended upgrades off, and keeps the runner on the
housekeeping cores; `docs/how-to/set-up-a-bench-host.md` is the page. The harness binds
its publisher, subscriber and conductor to the three CPUs of `--pin`, each thread
binding itself, puts its files under `/dev/shm` on Linux, and warms up for one rotation
through all three terms by default, so that no measured message first-touches a page.
CI runs 5,000,000 messages per benchmark and repeats every benchmark 21 times in one
process; each benchmark's reported run is the median by p99.99, the result lists all 21,
and `bench-compare` fails when the median is more than 5 percent above the slowest of
the baseline's 21 runs. On every row this host can measure, the band of 21 runs is 2
percent wide and that is the 5 percent rule; on the one row it cannot yet,
`claim_commit_32_exclusive`, whose p99.99 lands anywhere between 270 and 430 ns from
process to process, the rule is as sharp as the band and the ADR says so, instead of
failing an unchanged commit.

## Alternatives considered

- **A wider tolerance on the shared runners.** Lost: the `timer` row alone moved 43 and
  100 percent between two runs of one commit; a rule wide enough to pass that passes
  everything, and a knob added to make a benchmark pass is forbidden by `CLAUDE.md`.
- **Gating only the rows that measure Cavalry, still on the shared runners.** Lost:
  the Cavalry rows are where the 348 µs against 11 µs happened. Which rows a gate reads
  is not the problem; where it reads them is.
- **Longer runs on the shared runners.** Lost: a 348 µs descheduling is a scheduling
  event with a count of its own, not sampling noise; more samples put more of them in
  the histogram, and the neighbours on a shared machine are not ours to quiet.
- **Pinning on the shared runners.** Lost: a virtual CPU pinned is still a virtual CPU;
  the hypervisor schedules it. Pinning is part of this decision because the host is real.
- **A bare-metal cloud instance.** Lost for now: an instance costs by the hour for a
  job that runs for seconds and would need the same tuning; it joins when the README
  claims cloud-VM numbers, as a second profile, not as the gate.
- **A MacBook as the host.** Lost: macOS has no core isolation and no affinity call,
  and its `Instant` ticks every 41.67 ns, so a single load reads 0 at p50 (ADR 0015).
  The macOS runner keeps the correctness gates and the laptop half of invariant 5.
- **The whole CI on the bench host.** Lost: the correctness gates are fine on the
  shared runners and must run for a fork's pull request too, which the bench host must
  never do; the host does one thing.
- **Binding the process with `taskset` instead of each thread binding itself.** Lost:
  with `isolcpus` the scheduler never balances the isolated set, so two threads placed
  on one core stay there and starve each other; the harness refuses `--pin` with a
  repeated CPU for the same reason.
- **Pre-touching the log in `cavalry-shm`.** Deferred: touching every page at creation
  is the driver's duty, as Aeron's pre-touch option does, and the driver does not exist
  yet (ADR 0007, feature 4). Until it does, the harness's warm-up covers one rotation,
  and the gate measures what a driver that pre-touches would see.
- **Keeping the shared runners' baselines, and comparing.** Lost: a comparison that
  fails on noise trains everyone to ignore the gate, which is worse for invariant 2 than
  no comparison; the shared runners' tables stay in the logs as reports.

## Evidence

All on the host `bench`, AMD Ryzen 7 2700X, 2026-10-12, release, 32-byte bodies, 4 MiB
terms, commit 467cc49 of `cavalry-shm` with the harness of this commit; p99.99 in
nanoseconds unless marked.

- **Untuned, unpinned, files under `/tmp` on ext4, warm-up 100,000, 5,000,000
  messages, three runs.** `claim_commit_32_exclusive` 5,531 / 5,511 / 5,523;
  `claim_commit_32_concurrent` 5,543 / 5,591 / 5,663; `one_way_32_exclusive`
  8,911 / 8,863 / 8,871; `one_way_32_concurrent` 9,015 / 9,007 / 106,495, the last a
  170 µs preemption of the subscriber (max 167,551); `slot_padding_0_store` 2,601 to
  2,631, the two threads on different core complexes; `timer` 30 to 50; max of the log
  rows 28 to 191 µs. The host's p50 of 170 to 180 ns for claim and commit, against 30
  to 50 ns on the Blacksmith Linux runner, is the same cross-complex traffic: the
  conductor reads the tail the publisher writes on every spin.
- **Tuned, pinned to CPUs 8, 10 and 12, warm-up 100,000, 5,000,000 messages, three
  runs.** `claim_commit_32_exclusive` 5,611 / 5,611 / 5,603; `claim_commit_32_concurrent`
  5,671 / 5,683 / 5,683; `one_way_32_exclusive` 9,095 / 9,103 / 9,047;
  `one_way_32_concurrent` 9,167 / 9,167 / 9,143; every `slot_padding` row 31 or 61 in
  every run; `timer` 31; max of the log rows 13 to 16 µs. Unpinned on the same tuned
  host, one run: `one_way_32_exclusive` 95,615, max 160,255. Isolation and pinning
  make the host repeatable; they do not move the 5.6 and 9.1 µs.
- **The warm-up.** Pinned, 5,000,000 messages, one run each: files under `/tmp` with a
  250,000-message warm-up, `claim_commit_32_exclusive` 240 and `one_way_32_exclusive`
  480; under `/dev/shm` with 100,000, 5,611 and 9,143; under `/dev/shm` with 250,000,
  260 and 550. The 5.6 µs was the first touch of each of the 12 MiB log's 3,072 pages,
  about 1,500 of them after a warm-up of 6.4 MB, in a run whose p99.99 is its 500
  slowest messages. The default warm-up is now one rotation, 196,608 messages for
  these sizes.
- **Tuned, pinned, `/dev/shm`, the default warm-up, 5,000,000 messages, three runs.**

  | benchmark | p50 | p99 | p99.99 (3 runs) | max (3 runs) |
  | --- | --- | --- | --- | --- |
  | claim_commit_32_exclusive | 140 | 200 | 240 / 260 / 260 | 1,160 / 1,030 / 7,891 |
  | claim_commit_32_concurrent | 140 | 210 | 390 / 430 / 390 | 960 / 8,255 / 1,110 |
  | one_way_32_exclusive | 230 | 300 | 480 / 560 / 450 | 7,631 / 1,390 / 7,943 |
  | one_way_32_concurrent | 240 | 310 | 660 / 669 / 670 | 1,400 / 8,311 / 1,330 |
  | slot_padding_0_load | 40 | 60 | 60 / 60 / 60 | 390 / 230 / 380 |
  | slot_padding_64_load | 30 | 30 | 30 / 30 / 30 | 360 / 280 / 530 |
  | slot_padding_128_load | 30 | 30 | 30 / 30 / 30 | 500 / 270 / 370 |
  | timer | 20 | 30 | 30 / 30 / 30 | 600 / 390 / 180 |

  Allocations per message: 0 in 10,000,000, every run. Retries: 79 per log type, the
  rotations. The unchecked build, one run: `claim_commit_32_exclusive` 300,
  `claim_commit_32_concurrent` 430, `one_way_32_exclusive` 660, `one_way_32_concurrent`
  670: ADR 0010's checks cost nothing this host can see, as the shared runners found.
  On this part a field 64 bytes from a hot store costs the same as one 128 bytes away;
  ADR 0011's pair effect is the Intel and Apple prefetchers', and the padding costs
  nothing here.
- **What does not help, and what the clock resolves.** Pinned, `/dev/shm`, the default
  warm-up. Ten times the messages, 50,000,000 per benchmark, three runs:
  `claim_commit_32_exclusive` 250 / 260 / 240, `claim_commit_32_concurrent`
  420 / 390 / 370, `one_way_32_exclusive` 470 / 550 / 470, `one_way_32_concurrent`
  670 / 660 / 660: the same band as 5,000,000, so the band is not sampling. `/dev/shm`
  remounted with `huge=always`, three runs: p99 fell, 210 to 150 on
  `claim_commit_32_concurrent`, and p99.99 read 280 / 300 / 380, the same band, so it is
  not page placement either. The host's clock, TSC through the vDSO, steps in 10 ns:
  2,000,000 back-to-back reads from Python differ by 410, 420 or 430 ns with a handful
  of 411 and 421, so a 5 percent rule at 300 ns is one and a half steps of the clock.
- **The harness rebuilt.** The harness restructured for `--repeat`, with no change to a
  measured loop, read `one_way_32_exclusive` at 650 to 660 where the previous binary
  read 450 to 560, and the previous unchecked binary read 660 all along; after a reboot
  the new binary read the same 650 to 660. A tail of 0.01 percent at this scale moves
  with the binary's layout, so a change to the harness itself moves the baseline, and
  the page says to re-record it in a commit that says why.
- **Twenty-one runs in one process, pinned, `/dev/shm`, the default warm-up, 5,000,000
  messages, two processes.** Each row's 21 p99.99 values sorted, with the median the
  gate reads:

  | benchmark | process 1: band, median | process 2: band, median | wall |
  | --- | --- | --- | --- |
  | claim_commit_32_exclusive | 270 to 430, 310 | 290 to 420, 370 | |
  | claim_commit_32_concurrent | 360 to 440, 430 | 410 to 450, 430 | |
  | one_way_32_exclusive | 630 to 660, 650 | 650 to 660, 660 | |
  | one_way_32_concurrent | 660 to 670, 670 | 660 to 700, 670 | |
  | slot_padding_0_load | 60, 60 | 60, 60 | |
  | timer | 31, 31 | 31, 31 | 77.5 s and 77.2 s |

  Three rows' medians agree within 2 percent and `claim_commit_32_exclusive`'s differ
  by 19 percent, with a band 50 percent wide inside each process and a cluster that
  moves between processes. Why that row and not the concurrent one, whose claim is a
  compare-and-swap on the same tail line, is not known; it is the first thing to find
  when the driver's own claim path is measured.
- **The shared runners.** CI run 38171206638 of commit 467cc49, 2026-10-11, no change
  under a data-plane crate: 7 rows over 5 percent on Linux, 6 on macOS; `timer` +43
  percent and +100 percent; `one_way_32_exclusive` 348 µs against 11,215 on Linux; the
  macOS unchecked run 77,758 retries against 77. The baselines came from run
  38171048394, two runs earlier.

## Consequences

Easier: a regression at p99.99 on the bench host is a regression, and the commit that
caused it is the one that fails; the host's state is in every run's log; a stranger can
build a second host from `bench/host/` and the page; the harness has numbers under a
microsecond to hold the data plane to.

Harder: one physical host is a single point: when it is off, `bench (linux)` waits and
`master` cannot merge until someone turns it on, which is the right failure, since a
merge without the gate is what the halt forbade. The host changes by hand only, and any
change to it, a kernel, a package, a BIOS setting, is followed by a re-recorded baseline
in a commit that names the change. The runner runs as the host's administrative user;
a dedicated unprivileged runner user is the hardening to do next. The numbers are a
Ryzen 7 2700X's; the shared runners' tables show what a cloud VM and a laptop see, and
nobody compares them with the host's.

Revisit: when the cause of `claim_commit_32_exclusive`'s process-to-process band is
found, to compare that row with the baseline's median like the others; when the driver
exists and pre-touches its log at creation, to drop the harness's rotation-long
warm-up; when the UDP slice needs a sender and a receiver on
two hosts, to add the second host and a second profile; when the README claims
cloud-VM numbers, for a bare-metal cloud profile; when a fork's contribution needs the
gate, which today a maintainer gives it by pushing the branch to this repository.

## References

- `.github/workflows/ci.yml`, the `bench` job; `bench/host/setup-ubuntu.sh`,
  `bench/host/check.sh`; `bench/src/main.rs`, `--pin`, `Options::warmup`, `bench_dir`;
  `bench/baseline.json`
- `docs/how-to/set-up-a-bench-host.md`, `docs/how-to/run-the-bench.md`
- `CLAUDE.md`: invariant 2; Gates; Configuration and defaults
- ADR 0004, ADR 0010, ADR 0011, ADR 0015
- GitHub, self-hosted runner security:
  <https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#hardening-for-self-hosted-runners>
- AppShapes Trello card 83, "Build the log buffer, the ring buffers and the bench"
