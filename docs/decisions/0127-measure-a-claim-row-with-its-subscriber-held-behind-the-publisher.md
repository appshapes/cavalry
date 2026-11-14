# 0127. Measure a claim row with its subscriber held behind the publisher

- **Status:** Accepted
- **Date:** 2026-11-14
- **Invariants affected:** 2, 6
- **Supersedes:** none. Changes how the bench harness measures the shared-memory
  `claim_commit` rows, which [ADR 0015](0015-measure-in-process-with-per-runner-baselines.md)
  introduced. Asks for the re-record that
  [ADR 0125](0125-re-record-grips-checked-profile-at-pull-request-280s-tree-on-the-owners-ruling-and-again-when-work-item-316-reports.md)
  made conditional on work item 316's answer, as the third case of
  [ADR 0110](0110-let-a-persons-session-re-record-a-bench-baseline-after-telling-the-person.md).

## Context

The latency gate reads each message row at p99.99 and fails a row that is more than
5 percent and more than one clock step above the slowest run its baseline holds
([ADR 0015](0015-measure-in-process-with-per-runner-baselines.md),
[ADR 0029](0029-fail-a-gated-row-only-beyond-one-clock-step.md),
[ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md)). A
shared-memory message benchmark of the harness, `bench/src/main.rs`, ran one loop: a
publisher that claims as fast as it can, and a subscriber on another core that polls
again as soon as a poll returns. One loop gave two rows, `claim_commit` and
`one_way`. So the subscriber sat at the publisher's tail, the position the publisher
writes next, about 115 ns behind it on the host `grip`.

Between 2026-11-13 and 2026-11-14 the client claim rows failed the gate three times
with no instruction changed on the claim path. Pull request 280 added a module that
no row runs, and `claim_commit_32_client` on `grip`'s checked profile rose from 117
to 120 ns at p99.99 to 126 to 132 ns at each of its heads. The owner ruled a
re-record, ADR 0125, on the condition that work item 316 find the cause. Pull
request 283, the next change to `cavalry-client`, then read the same row red on
`grip`'s unchecked profile and on the host `bench`'s checked profile in its first
process, and passed both in its second, with the same bench code. Every function a
message passes through had the same instructions in all of these trees.

Work item 316 found the cause on `grip` on 2026-11-14 (Evidence). A subscriber at
the publisher's tail reads the cache line the publisher writes next. The publisher
then waits to have that line back before it can write it. The wait is real. How many
claims pay it depends on how the two threads happen to be timed against each other,
and anything moves that: where a build's linker put the code, where the kernel put a
process's memory, a step inside one process. The share of claims that paid the wait
sat across the gate's percentiles. So the rows' p99 and p99.99, and at times their
p50, measured the harness's own timing as much as the claim.

Until that changes, any process can read a claim row red on either host, and a
baseline wide enough to pass them all passes a real regression of a tenth.

## Decision

Every shared-memory message benchmark runs its loop in two parts, `--messages` each,
after one warm-up: the two log types, `client`, `recorded`, `recorded_purging` and
`ffi`. In the first part the subscriber polls at the publisher's tail, as before. The
`one_way` row is read there, and the claim is read there too, as a new row,
`claim_commit_<body>_<name>_at_tail`, reported and never gated. In the second part
the subscriber is held behind the publisher, and the gated `claim_commit` row is read
there. A held subscriber polls up to 1,024 fragments. After a poll that returned
fewer, which has reached the publisher's tail or the end of a term, it stays away
from the log for as long as the publisher has been taking to claim 64 messages, and
then polls again. It takes the publisher's rate from its own count of what it
drained in this part and its own clock, the time stamp counter, so that it reads and
writes no line another thread writes while it waits. A stay is 100 ns at the least
and 1 ms at the most, 4 µs until the first 64 messages are drained, and never more
messages than a quarter of the publication window. Each run's count of such polls is
printed under the table and written to the result as `held_polls`. The UDP rows keep
one loop.

A held subscriber comes to the tail once for each stay, the same way each time: it
drains what the stay left and reads the tail once. So about one claim in 80 meets
the reader, on any host and at any rate, since the publisher also claims while the
subscriber drains; at the tail the subscriber polled several times for each claim
and the share that paid moved with the timing. One in 80 is over a hundred times the
gate's one in ten thousand. So a claim row's p99.99 is a claim that met the reader,
in every run, and both bench hosts read each row at one level (Evidence).

Both hosts' four profiles are re-recorded at this change's tree, in a change of its
own, from every process each host has run there (Consequences).

## Alternatives considered

- **Leave the harness and widen the baselines**, as ADR 0125 did once. Lost: the band
  that holds every draw is a tenth wide and more, so a regression of a tenth passes;
  and the band moves with each change to `cavalry-client`, so each such change asks
  for another re-record that no rule gives a session.
- **Gate another statistic of the same rows.** Lost: the share moved p50 on `grip`
  (21 or 22 ns against 27 or 28 ns), p99 on `bench` (90 ns against 150 to 160 ns) and
  p99.99 on both. No percentile of a row measured that way is the claim's own.
- **A longer stay, 1,000 messages.** This was built first, on the reasoning that one
  claim in a thousand sits between the percentiles, so that p50 and p99 would be the
  claim alone. Lost on what the hosts read (Evidence). A claim row has other slow
  claims that are rare. `bench` has about one in ten thousand with no reader on the
  log at all. A log row's conductor raises the publisher's limit once in 4,096
  frames. With the meetings as rare as those, a row's p99.99 fell among them or
  under them by the run, the build and the process: `bench` read its client row from
  90 to 160 ns, and `grip` read its exclusive row from 40 to 67 ns, wider than at
  the tail. Stays of 250, 4,000 and 16,000 messages did the same on one host or the
  other.
- **No reader while a claim is timed.** The publisher claimed a batch while the
  subscriber read nothing, then waited, untimed, while the subscriber read exactly
  that batch. On `grip` this read the tightest rows of all: the client row at 71 to
  73 ns in twelve processes. Lost on `bench`: with no reader, its rows fell on that
  host's own slow claims, and its client row read from 110 to 210 ns by the process.
  Its runs went up to 230 ns, so a baseline of every process's slowest run would
  have failed that row only above 240 ns, against a usual 140.
- **Hold the subscriber for a fixed time**, 64 µs, work item 316's first proposal.
  Lost: a fixed time leaves the publisher a number of messages that follows the
  host. It was about 1,500 messages on `grip`, 850 on `bench` and 5,400 on a laptop,
  and a count in that range is the longer stay above.
- **Hold it for a count of pause instructions.** This is what the experiment that
  found the cause did. Lost: a pause is about 16 ns on `grip`'s Core i7-10700 and far
  less on `bench`'s Ryzen 7 2700X, so one count is two distances.
- **Hold it by the publisher's position.** Lost: the position is a line the
  publisher writes on every claim, and a load of it from another core is the wait
  this decision steadies
  ([ADR 0032](0032-load-the-producer-position-every-two-milliseconds.md)).
- **Two runs for each benchmark, one at the tail and one held.** Lost: a second
  driver, client and warm-up for each of five benchmarks, 21 times a profile, and the
  claim row and the one-way row would come from two drivers. One loop keeps the
  set-up and the warm-up once.
- **One loop of `--messages`, half at the tail and half held.** It adds no time.
  Lost: each row would have half its samples, 250 above its p99.99 where it has 500,
  and the result's `messages` would no longer be a row's count.
- **Read the one-way row while the subscriber is held.** Lost: a delivery would wait
  for the stay, and the row would measure the harness.
- **Turn address-space randomisation off for the bench** (work item 198). Lost: it
  removes a process's draw. A build's layout and a step inside a process stay.
- **Spread a row's runs over several processes.** Lost: it averages the draws and
  leaves the cause, and a build's layout is the same in every process.
- **Build the data-plane crates with one codegen unit, or align their functions.**
  Lost: the front end's counters differ between builds and do not follow the tail
  (Evidence).
- **Gate the `_at_tail` rows as well.** Lost: they are the rows that moved.
- **Drop the claim at the tail from the output.** Lost: a publisher with a caught-up
  subscriber is what a user runs, and its claim costs more. The harness must not
  hide a tail (`bench/README.md`). The row stays in the table and in the result.
- **Hold the driver's recorder too**, for the `recorded` rows. Lost: the recorder is
  the product's thread and follows the publisher as the product makes it.
- **Change the product so that a caught-up subscriber costs the publisher less.**
  Not this decision: it is a change to the log buffer or to how a reader finds the
  tail, with a spec change and an ADR of its own. The `_at_tail` rows are where it
  would show.

## Evidence

**The cause** (work item 316's comment of 2026-11-14 17:00 UTC has every number).
On `grip`, with a scratch copy of the old harness whose subscriber spun a number of
pause instructions after an empty poll: `claim_commit_32_client` read 117 to 125 ns
at p99.99 with one pause, the harness as it was; 96 to 99 ns with the subscriber
about 950 ns behind; 79 to 81 ns in ten processes of two trees with it 4.2 µs
behind. Loads that hit a line another core had modified fell from 2.3 a message to
0.43, and 71 to 76 percent of them were the subscriber's read of a frame's first
word. The share of uops from the decoded-uop cache differed between two builds (77
and 79 percent) whose claim rows read the same, so the front end does not carry the
tail. The stock builds of two trees read the reverse of what CI had read for them.

**How the rest was measured.** By hand on both hosts on 2026-11-14, with no CI job on
either, in a scratch directory: `grip` is a Core i7-10700 pinned to CPUs 4 to 7 with
a 1 ns clock step, `bench` a Ryzen 7 2700X pinned to CPUs 8, 10, 12 and 14 with a
10 ns step. Each row ran alone in a process (`--only`), 5,000,000 messages a part,
5 runs a process. A process's level is its median run by p99.99. The binaries were
built there with the login user's toolchain, rustc 1.99.1 on `grip` and 1.99.0 on
`bench`, where CI installs 1.100.0, so they are not CI's binaries; CI's own rows are
in the pull request that landed this. "At the tail" is the old harness at 9266fd9f.
The stay's length and the batches came from the environment in a scratch copy only.

**The stay's length.** p99.99 in ns, the range of the processes' levels, two to
eight processes each. "None" is a subscriber that reads only while the publisher
waits.

| Host, row | at the tail | 32 | 64 | 128 | 250 | 1,000 | 16,000 | none |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `grip`, client | 116 to 132 | 83 to 86 | 78 to 81 | 74 to 78 | 75 to 81 | 72 to 78 | 72 to 74 | 71 to 73 |
| `grip`, exclusive | 73 to 81 | 57 to 62 | 55 to 60 | 54 to 58 | 41 to 68 | 40 to 67 | 40 to 45 | 39 to 46 |
| `grip`, concurrent | 75 to 79 | 71 to 74 | 65 to 70 | 59 to 64 | 47 to 65 | 45 to 71 | 45 | 45 |
| `bench`, client | 160 to 170 | 160 to 170 | 160 | 150 to 160 | 150 to 160 | 90 to 160 | 100 to 160 | 110 to 210 |
| `bench`, exclusive | 150 | 150 to 160 | 150 | 149 to 150 | 150 | 129 to 140 | 110 to 120 | 99 to 120 |
| `bench`, concurrent | 150 to 160 | 149 to 150 | 119 to 140 | 100 to 120 | 99 to 109 | 90 to 110 | 120 | 100 to 120 |

At 64 `bench`'s exclusive row read 149 or 150 ns in every run of eight processes,
and its client row 160 ns in ten processes with runs of 150 to 160 ns. On `bench` the client
row held 490 to 590 claims of 130 ns or more in a run with no reader at all, 1,110
to 1,630 with a stay of 1,000 messages and about 11,000 with a stay of 64: at 64
the meetings are twenty times that host's other slow claims. A scratch build with
the driver's producer position load every 20 ms, where it is every 2 ms, held as
many, so those are not that load; their cause was not found (work item 198). With a fixed stay of 64 µs `grip` counted 3,259 to 3,326 short polls a run and
`bench` 5,847 to 6,005. On an Apple silicon laptop, whose held publisher claims 68
million messages a second, the same 64 µs left 909 to 947 short polls and its client
row read 58 to 83 ns run to run.

**The decision as built, against the old harness.** Eight processes of each on each
host, the checked profile's build. p50 / p99 / p99.99 are the medians over the
processes, with the range of the levels.

| Host, row | old harness, at the tail | this harness, held 64 messages |
| --- | --- | --- |
| `grip`, `claim_commit_32_client` | 27 / 71 / 122 (116 to 124) | 20 / 41 / 78 (77 to 79) |
| `grip`, `claim_commit_32_recorded` | 22 / 68 / 127 (118 to 142) | 20 / 41 / 90 (89 to 91) |
| `grip`, `claim_commit_32_ffi` | 34 / 80 / 134 (133 to 140) | 29 / 51 / 92 (91 to 98) |
| `grip`, `claim_commit_32_exclusive` | 17 / 36 / 75 (74 to 80) | 16 / 29 / 56 (55 to 59) |
| `grip`, `claim_commit_32_concurrent` | 19 / 45 / 77 (77 to 83) | 18 / 40 / 66 (66 to 67) |
| `bench`, `claim_commit_32_client` | 80 / 90 / 160 (159 to 160) | 60 / 90 / 160 (150 to 160) |
| `bench`, `claim_commit_32_recorded` | 80 / 170 / 280 (270 to 280) | 60 / 160 / 290 (280 to 300) |
| `bench`, `claim_commit_32_ffi` | 100 / 140 / 200 (200) | 70 / 100 / 170 (170 to 180) |
| `bench`, `claim_commit_32_exclusive` | 70 / 140 / 150 (150 to 160) | 50 / 80 / 150 (140 to 150) |
| `bench`, `claim_commit_32_concurrent` | 70 / 140 / 160 (150 to 160) | 50 / 80 / 120 (120 to 140) |

The one-way rows did not move: on `grip` `one_way_32_client` read 471 ns (467 to 478)
before and 471 ns (469 to 473) after, `one_way_32_recorded` 490 and 492 ns,
`one_way_32_ffi` 466 and 471 ns, `one_way_32_exclusive` 387 ns in both. On
`bench` `one_way_32_client` read 300 ns and `one_way_32_ffi` 340 ns in both,
`one_way_32_recorded` 500 and 510 ns, `one_way_32_exclusive` 230 and 240 ns and
`one_way_32_concurrent` 240 ns in both, each within one 10 ns step.
The held subscriber's short polls were 59,440 to 62,821 a run on `grip` and 65,766
to 70,057 on `bench`, with 53,252 in one recorded run: one in 71 to 94 messages,
since the publisher claims while the subscriber drains.

The unchecked build on `grip`, ten processes: client 79 to 84 ns where the old
harness read 118 to 122, recorded 87 to 93 (127 to 129), ffi 93 to 107 (139 to 145),
exclusive 53 to 59 (71 to 75), concurrent 62 to 66 (77 to 86), in nine of the ten.
For about twenty seconds, four processes in a row read every row a third up: the
held rows at 119, 133, 75 and 77 ns with their rate down 13 percent, and the same
processes' `_at_tail` rows up as well (144 ns against 119). The host's journal holds
nothing for those seconds and no CI job ran. Six processes right after read as the
first three. That is a disturbance of the host, of the kind work item 198 holds, and
not the harness's.

The unchecked build on `bench`, four processes of each: client 160 ns before and
after (p50 80 and 60 ns), recorded 260 to 269 and 260 to 270, ffi 189 to 190 and 170,
exclusive 150 to 160 and 150, concurrent 160 to 170 and 120 to 140.

**What still moves.** The log rows follow the build by a few nanoseconds in every
form measured: two builds with the same publisher instructions read `grip`'s
concurrent row at 51 to 59 ns and at 65 to 70 ns with a stay of 64 messages, and at
45 and at 52 ns with a stay of 1,000. The recorded row follows the driver's
recorder: one process in six to eight read it at 102 to 117 ns on `grip` where the
others read 84 to 96.

## Consequences

- A shared-memory `claim_commit` row is a claim under a subscriber that polls once
  in 64 messages' time, and its p99.99 is a claim that met that reader. A build's
  layout or a process's draw no longer moves the client, recorded and ffi rows
  across the gate on `grip`, and `bench`'s rows read where they did (Evidence).
- The `one_way` rows are read as they were, in the first part.
- Each such benchmark has a third row, `_at_tail`, and the result has `held_polls`.
  A baseline entry holds the new rows once it is re-recorded; until then
  `bench-compare` prints `no baseline` for them, and `--join` refuses a process of
  this harness against an entry of the old one, since their rows differ.
- **Both hosts' four profiles, `bench-linux`, `grip-linux` and their `-unchecked`
  twins, are to be re-recorded** from every process each host has run at this
  change's tree, three at least, in a change of its own, as ADR 0110's third case
  and `docs/how-to/run-the-bench.md`, step 4, say. Until then the gate compares the
  held claim rows against entries of the old harness, which read them at the tail
  and so admit more than they should. That re-record is also the one ADR 0125's
  Decision asks for when work item 316's answer is on `master`: `grip-linux` then
  gates `claim_commit_32_client` and `claim_commit_32_recorded` at what this harness
  reads, and no longer at 145 ns and 168 ns.
- The log rows still move by the build, and the `recorded` rows by the driver's
  recorder (Evidence). Neither is the harness's subscriber. The first is work item
  198's ground; the second is the product's.
- `bench` has about one slow claim in ten thousand with no reader on the log, from a
  cause not found. The held rows sit above it. A row that is ever measured with no
  reader on that host meets it.
- A full pass takes longer: each of the five benchmarks claims `--messages` more, at
  20 to 23 million a second on `grip` and 10 to 13 million on `bench`. That is about
  25 s more for a profile's 21 passes on `grip` and about 50 s on `bench`, where the
  bench step took 2 min 35 s and 5 min 33 s. The job times are in the pull request
  that landed this.
- A stay follows the publisher's rate, so the harness has a small control loop of
  its own. `held_polls` is its record: a count far from `--messages` ÷ 80 says the
  share moved, and `docs/how-to/run-the-bench.md` says what to do.
- The loops of the subscriber and of the publisher are compiled differently, so a
  row may move by the compiler alone, as the how-to says a harness change can. The
  re-record takes that in.
- **Revisit** if a gated row's `held_polls` leave the range of `--messages` ÷ 128 to
  `--messages` ÷ 48; if a `claim_commit` row's level again moves by more than the
  gate's 5 percent between builds with the same instructions on the claim path, on
  the client, recorded or ffi rows; or if the product changes what a caught-up
  subscriber costs the publisher, which would make the two claim rows one.

## References

- Work item 316: the finding, with every number, and this change. Work item 317:
  the ruling behind ADR 0125. Work item 198: the host's process modes and single-run
  spikes.
- `bench/src/main.rs`: `Held`, `HOLD_MESSAGES`, `run_log`, `run_client`, `run_ffi`.
- `scripts/bench-compare.py`: `REPORTED_SUFFIXES`.
- [Run the bench and compare it with the baseline](../how-to/run-the-bench.md), step
  2 and "When it does not work".
- [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md),
  [ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md),
  [ADR 0090](0090-keep-a-subscribers-samples-out-of-the-histogram-until-its-loop-is-over.md),
  [ADR 0110](0110-let-a-persons-session-re-record-a-bench-baseline-after-telling-the-person.md),
  [ADR 0125](0125-re-record-grips-checked-profile-at-pull-request-280s-tree-on-the-owners-ruling-and-again-when-work-item-316-reports.md).
