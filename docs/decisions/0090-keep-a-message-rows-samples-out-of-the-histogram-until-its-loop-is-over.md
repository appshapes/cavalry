# 0090. Keep a message row's samples out of the histogram until its loop is over

- **Status:** Accepted
- **Date:** 2026-11-12
- **Invariants affected:** 2
- **Supersedes:** none

## Context

The latency gate reads each message row at p99.99
([ADR 0015](0015-measure-in-process-with-per-runner-baselines.md),
[ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md)). On the second bench
host, `grip`, about one run in two hundred of a one-way row read a p99.99 of 1.3 to
2.3 times its row's, with the p50 unmoved. Because the gate compares against the
slowest run of every process a baseline was recorded from
([ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md)), those runs set
what the gate admits: `one_way_32_client` failed only above about 1,172 ns against a
median of 485 ns, so a regression of 2.3 times passed.

The harness recorded every sample in an HdrHistogram as it was taken: the publisher
after each commit, and the subscriber inside its fragment handler. A histogram of
1 ns to 60 s at three significant digits is 54 pages of counts. A thread that streams
a log through its cache at 900 MB a second keeps warm only the lines of the latencies
it usually sees.

So a delivery that was late was recorded in a line that was cold, and the record was
a cache miss in the subscriber's handler, before the next delivery. The next delivery
was later by it and landed in another cold line. The subscriber fell behind one miss
at a time, with no pause anywhere, until the lines it had climbed through were warm.
Mostly that ended after a few hundred deliveries. Now and then it ran to a backlog of
a hundred messages or more, and that run was one of the slow ones. The harness was
measuring its own record.

## Decision

A message row's measured loop does not record in a histogram. Each of its two threads
keeps its samples so that taking one touches no memory that is cold for it. A sample
under 256 ns is counted in an array of 256 counts, one a nanosecond, a kilobyte that
the row's usual samples keep in the first-level cache. A sample of 256 ns or more is
written, eight bytes, after the last such one into a buffer that is made at setup
with room for every message of the loop and written through once, so that no sample
is the first touch of a page: a store to the next eight bytes waits for no load,
whatever the sample's value. The histogram is made of the counts and the buffer after
the loop, with the bounds and the precision the rows have always had, so a row reads
to the nanosecond as before. This holds for every message row: the log rows, the
client, recorded and purging rows, the ffi rows, the UDP rows and the link rows.
The publisher's thread makes both threads' buffers before the subscriber is started,
so that neither starts the stream behind the other, and the histograms are made once
both threads are joined. `--messages` is bounded at 100,000,000, since a row has room
for 16 bytes a message until its loop is over, and the link rows, whose subscriber
takes two samples a message, for 24.

The timer, slot and record rows keep recording as they did. Their loops deliver
nothing to another thread, so a slow record delays no later sample.

## Alternatives considered

- **Every sample in the buffer, in order.** This was built first, and it ended the
  slow runs. It lost because the stream of stores cost the fastest row: on `grip`
  `claim_commit_32_exclusive` read 79 ns at p99.99 against 74, and
  `one_way_32_exclusive` 413 against 399 (Evidence). Counting the usual samples in a
  kilobyte takes the stream away for them, and the same rows then read 73 and 390.
- **Keep the histogram and take a digit off its precision.** Two significant digits
  is a sixteenth of the counts, 3 pages. It lost because the lines of a late sample
  are still not the lines of a usual one, so the miss stays and only its count falls;
  and because every row would then read in steps of 1 percent, where the gate's rule
  is 5 percent and a clock step.
- **A small histogram on the thread's stack, eight buckets an octave.** The probe that
  found the cause used one, and it has no cold line. It lost because a row's p99.99
  would read in steps of 6 percent or more, coarser than the gate.
- **Record on a third thread.** The handler would hand each sample over a queue. It
  lost because the queue is the same store with a second cache line and a second
  thread's cost on a bench host whose four isolated cores are all taken
  ([ADR 0044](0044-gate-udp-on-the-two-bench-hosts.md)).
- **Leave the harness and gate on the median run alone.** The median of 21 runs
  already hides a slow run from the row's value; what the slow runs widen is the
  baseline's slowest run, which ADR 0033 compares against on purpose, so that a host's
  real spread is admitted. It lost because the spread was not the host's.
- **Four-byte samples in the buffer.** Half the memory, and a sample above 4.29 s
  would read as 4.29 s in the row's max. It lost because a row does not report a
  number it did not measure.
- **A buffer smaller than the loop.** The claim rows send almost nothing to it, and
  writing 40 MB through for each thread of each loop is setup the row does not use.
  It lost because the harness cannot know a row's samples before the row has run: on
  a slow host, or in a UDP row, every sample is 256 ns or more, and a loop that
  outgrew its buffer would have no row.
- **Counts centred on the row's own body**, found in the warm-up, in place of a fixed
  256 ns. It would keep a row whose body is above 256 ns out of the buffer too. It
  lost for now because it puts a second pass and a choice into every loop for rows
  that show no cost: the stream of stores that cost the exclusive row is one sample
  a message at 18 million messages a second, and a row whose body is above 256 ns
  writes a quarter of that or less. It is the first thing to try if such a row's tail
  is ever the buffer's.

## Evidence

All on `grip` (Core i7-10700, CPUs 4 to 7 isolated), by hand, with no CI job on either
bench host, on 2026-11-12; `--pin 4,5,6,7`, 5,000,000 messages a loop, the client row.

**No interrupt goes with a slow run.** With the kernel's counts read around each loop,
and then with its timer, interrupt-vector, softirq and scheduler events traced on the
four CPUs: of 504 loops of `master`'s harness, the two slowest read 873 and 628 ns at
`one_way_32_client` p99.99 against quartiles of 482 to 490, and the trace has no event
on the publisher's or the subscriber's CPU in either.

**Nothing stops in one.** A build that also kept each loop's three longest gaps
between two claims and between two deliveries: of 546 loops the two slowest read
1,695 and 1,406 ns, and in both the subscriber's longest gap was 754 ns and the
publisher's under 1 µs. The driver's conductor, whose backoff parks only after thirty
duty cycles in which the furthest subscriber's position did not move, slept inside
the stream in nine loops, the six slowest among them: the subscriber was in one long
poll, working through a backlog.

**The record makes the backlog.** Two builds as processes in turn, 546 loops each,
alike but for one call. Both count the deliveries 600 ns late or more and keep a
small histogram on the stack; A also records each delivery in the HdrHistogram in the
handler, as the harness did, and B does not.

| Of 546 loops each | A, records in the handler | B, does not |
| --- | --- | --- |
| deliveries 600 ns late or more, a loop, quartiles | 131, 187, 248 | 3, 4, 6 |
| the same, summed over the loops | 109,940 | 3,014 |
| latest delivery of a loop, quartiles | 1,033, 1,191, 1,394 ns | 708, 776, 899 ns |
| loops whose p99.99 is in the bucket of 576 ns or above | 10 | 0 |
| loops in which the conductor slept, of those the trace kept | 10 of 294 | 0 of 297 |
| `claim_commit_32_client` p99.99, quartiles | 125, 128, 129 ns | 115, 118, 119 ns |

Three of B's loops had a real stall, a timer interrupt on a measured CPU, one of
18 µs on both threads at once. They have 150 to 183 late deliveries and the p99.99 of
every other loop.

**This change against the harness before it.** Three builds as processes in turn, a
row at a time, seven processes of each for each row, 21 loops a process: `master`'s
harness at b4dab01, the first form of this change (every sample in the buffer), and
the form decided. A process's level is the median of its loops' p99.99. Below, the
median of the seven levels in nanoseconds, then the slowest of the 147 loops.

| Row | harness before | every sample in the buffer | this decision |
| --- | --- | --- | --- |
| `claim_commit_32_exclusive` | 74; 83 | 79; 83 | 73; 75 |
| `one_way_32_exclusive` | 399; 414 | 413; 425 | 390; 409 |
| `claim_commit_32_client` | 120; 128 | 118; 124 | 120; 123 |
| `one_way_32_client` | 482; 1,193 | 473; 486 | 459; 469 |
| `claim_commit_32_recorded` | 135; 141 | 131; 138 | 132; 151 |
| `one_way_32_recorded` | 500; 603 | 490; 500 | 485; 491 |
| `claim_commit_32_ffi` | 288; 461 | 137; 159 | 142; 168 |
| `one_way_32_ffi` | 514; 593 | 475; 491 | 462; 483 |

The harness before had three one-way loops of 1.2 times their row's or more (1,193
and one more on the client row, 603 on the recorded row); neither form of the change
has one. The ffi row is run alone in its process here, where CI runs it after the
others: alone, the harness before reads its claim at 288 ns with a p50 of 89, and CI's
runs of the same binary read 138 and 35. With either form it reads alone as it does in
CI. That was not looked into further. One process of the decided form read
`claim_commit_32_recorded` at 147, which is the 151 above; that row moves 5 ns from
one process to the next on unchanged code on this host. An earlier run of the first
form alone, nine processes each way, read the same as its column here.

**The decided form as it landed.** Its review moved three things outside the loops:
the publisher's thread makes both threads' buffers before the subscriber starts, the
histograms are made after the threads are joined, and a late sample is stored by
index. That build against the harness before, two or three processes of each for a
row before a CI job took the host: `one_way_32_client` 459 against 480.5 with one
loop of the old harness at 1,398, `one_way_32_recorded` 485.5 against 500.5,
`one_way_32_exclusive` 382 against 396, and no claim row of the three above the old
harness's. The ffi row, alone in its process, read as the old harness does alone:
its claim 304 ns with a p50 of 90, against 279.5 and 95. So that row has two ways to
run, the publisher's claim at 37 ns or at 90, and which one a process takes went with
when the subscriber started, not with how its samples were kept. CI runs the row
after the others and has read the fast way; the pull request's own run says which way
this build reads there, and it is not landed on a slow reading.

## Consequences

- The one-way rows' tail on `grip` is the path's again, and a run no longer reads two
  or three times its row's. The rows that ADR 0033's re-records loosened for those
  runs can be recorded tight.
- Every message row's loop is compiled differently, so a row may move by the compiler
  alone, as `docs/how-to/run-the-bench.md` says a harness change can. On `grip` no row
  read higher by hand than before; `bench` was not compared by hand, and the pull
  request's own run is its measurement. Both bench hosts' profiles are to be
  re-recorded from processes at this change's tree, in a change of its own, since it
  takes the hosts for hours; until then the gate compares this harness against
  baselines of the old one, which admit more than they should, as they did before.
- A row costs memory in proportion to its messages: 80 MB for CI's 5,000,000, written
  through at setup, which is tens of milliseconds a loop that no sample sees, and 24
  bytes a message in the link rows. Making the histogram is one pass over the counts
  and the buffer after the loop.
- The bound of 256 ns is a choice for today's rows: under it are the claim rows' bodies
  on both hosts and `grip`'s one-way rows up to their p99. A row whose usual samples
  are all above it, as a UDP row's are, keeps every sample in the buffer, which is
  the first form. What that costs such a row was not measured; its stream of stores
  is a quarter or less of the one that cost the exclusive row.
- A message row's late samples exist in order for the first time. A later change can
  keep what a histogram cannot: where in a loop its slowest samples were.
- Revisit if a row is added whose loop cannot know its sample count at setup, if a
  shared-memory row's body moves above 256 ns, or if the buffer's own stores are ever
  measured in a row's tail.

## References

- Card 198, where the runs above are recorded with their scripts.
- `bench/src/main.rs`, `Samples`.
- [Run the bench and compare it with the baseline](../how-to/run-the-bench.md).
- [ADR 0015](0015-measure-in-process-with-per-runner-baselines.md),
  [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md),
  [ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md).
