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
from the log for as long as the publisher has been taking to claim 1,000 messages,
and then polls again. It takes the publisher's rate from its own count of what it
drained in this part and its own clock, the time stamp counter, so that it reads and
writes no line another thread writes while it waits. A stay is 1 µs at the least and
1 ms at the most, 16 µs until the first 1,000 messages are drained, and never more
messages than a quarter of the publication window. Each run's count of such polls is
printed under the table and written to the result as `held_polls`. The UDP rows keep
one loop.

A held subscriber meets the tail once for each stay. So about one claim in a
thousand pays the wait, on any host and at any rate. That share is a tenth of one in
a hundred and ten times one in ten thousand. So a claim row's p50 and p99 are the
claim alone, its p99.99 is a claim that met a reader, and the share sits on neither
percentile.

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
- **Hold the subscriber for a fixed time**, 64 µs, work item 316's first proposal.
  Lost: a fixed time leaves the publisher a number of messages that follows the
  host's rate, and the share with it (Evidence). @@FIXED@@
- **Hold it for a count of pause instructions.** This is what the experiment that
  found the cause did. Lost: a pause is about 16 ns on `grip`'s Core i7-10700 and far
  less on `bench`'s Ryzen 7 2700X, so one count is two distances.
- **Hold it until the publisher is a thousand frames ahead, read from the
  publisher's position.** Lost: the position is a line the publisher writes on every
  claim, and a load of it from another core is the wait this decision takes out of
  the row ([ADR 0032](0032-load-the-producer-position-every-two-milliseconds.md)).
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
  removes a process's draw. A build's layout and a step inside a process stay, and
  the one layout left is whatever the build gave.
- **Spread a row's runs over several processes.** Lost: it averages the draws and
  leaves the cause, and a build's layout is the same in every process.
- **Build the data-plane crates with one codegen unit, or align their functions.**
  Lost: the front end's counters differ between builds and do not follow the tail
  (Evidence).
- **Gate the `_at_tail` rows as well.** Lost: they are the rows that moved.
- **Drop the claim at the tail from the output.** Lost: a publisher with a caught-up
  subscriber is what a user runs, and its claim costs more at p99 and above. The
  harness must not hide a tail (`bench/README.md`). The row stays in the table and in
  the result, where a change to it is read by a person.
- **Hold the driver's recorder too**, for the `recorded` rows. Lost: the recorder is
  the product's thread and follows the publisher as the product makes it. The
  harness holds only its own subscriber (Consequences).
- **Change the product so that a caught-up subscriber costs the publisher less.**
  Not this decision: it is a change to the log buffer's layout or to how a reader
  finds the tail, with a spec change and an ADR of its own. The `_at_tail` rows are
  where it would show.

## Evidence

@@EVIDENCE@@

## Consequences

- A shared-memory `claim_commit` row gates the claim's own cost at p50 and p99 and a
  claim that met a reader at p99.99. A build's layout or a process's draw no longer
  moves it across the gate (Evidence).
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
- The `recorded` and `recorded_purging` claim rows stay wider than the others: the
  driver's recorder reads the log at the publisher's tail as a subscriber does, and
  the harness does not hold it (Evidence).
- A full pass takes longer: each of the five benchmarks claims `--messages` more.
  @@TIME@@
- A stay follows the publisher's rate, so the harness has a small control loop of
  its own. `held_polls` is its record: a count far from `--messages` ÷ 1,000 says
  the share moved, and `docs/how-to/run-the-bench.md` says what to do.
- The loops of the subscriber and of the publisher are compiled differently, so a
  row may move by the compiler alone, as the how-to says a harness change can. The
  re-record takes that in.
- **Revisit** if a host's `held_polls` leave the range of `--messages` ÷ 2,000 to
  `--messages` ÷ 500 on a gated row; if a `claim_commit` row's level again moves by
  more than the gate's 5 percent between processes of one tree; or if the product
  changes what a caught-up subscriber costs the publisher, which would make the two
  claim rows one.

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
