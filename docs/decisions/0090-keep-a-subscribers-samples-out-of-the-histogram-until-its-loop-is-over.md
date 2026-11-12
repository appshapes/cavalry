# 0090. Keep a subscriber's samples out of the histogram until its loop is over

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

The subscriber of a message row does not record in a histogram inside its measured
loop. It keeps its samples so that taking one touches no memory that is cold for it.
A sample under 256 ns is counted in an array of 256 counts, one a nanosecond, a
kilobyte that the usual deliveries keep in the first-level cache. A sample of 256 ns
or more is written, eight bytes, after the last such one into a buffer that is made
at setup with room for every message of the loop and written through once, so that no
sample is the first touch of a page: a store to the next eight bytes waits for no
load, whatever the sample's value. The one-way histogram is made of the counts and
the buffer after the loop, once both threads are joined, with the bounds and the
precision the rows have always had, so a row reads to the nanosecond as before. This
holds for every message row: the log rows, the client, recorded and purging rows, the
ffi rows, the UDP rows and the link rows. The publisher's thread makes the buffer
before it starts the subscriber, so that the subscriber does not start the stream
behind. `--messages` is bounded at 100,000,000, since a subscriber has room for eight
bytes a message until its loop is over, and a link row's, which takes two samples a
message, for sixteen.

The publisher records each claim's time in the histogram, as it did. Its record falls
after one claim's time is taken and before the next claim begins, so a load that
misses there is in no sample and delays no delivery. The timer, slot and record rows
keep recording as they did too: their loops deliver nothing to another thread.

## Alternatives considered

- **The publisher's samples kept the same way.** This was built and pushed first, for
  both threads. On `grip` it read every row as well as the decision does. On `bench`
  its run failed the gate: `claim_commit_32_recorded` read 440 ns at p99.99 in every
  one of 21 runs against 300, and four other rows read one to three clock steps up
  (Evidence). On that host a recorded claim's tail is above 256 ns, so those samples
  went to the buffer, a few in a thousand: each was a store to a line that had gone
  cold, and the next claim began while the store was still on its way. A record that
  waits, between two samples, is what the publisher wants; one that does not wait is
  what the subscriber wants.
- **Every sample in the buffer, in order.** This was the first thing built, and it
  ended the slow runs. It lost because the stream of stores cost the fastest row: on
  `grip` `one_way_32_exclusive` read 413 ns at p99.99 against 399 (Evidence). Counting
  the usual samples in a kilobyte takes the stream away for them, and the row then
  read 390.
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
- **A buffer smaller than the loop.** On `grip` a one-way row sends the buffer a few
  samples in a thousand, and writing 40 MB through for each loop is setup the row
  does not use. It lost because the harness cannot know a row's samples before the row
  has run: on a slow host, or in a UDP row, every sample is 256 ns or more, and a loop
  that outgrew its buffer would have no row.
- **Counts centred on the row's own body**, found in the warm-up, in place of a fixed
  256 ns. It would keep a row whose body is above 256 ns out of the buffer too. It
  lost for now because it puts a second pass and a choice into every loop for rows
  that show no cost: the stream of stores that cost the exclusive row is one sample
  a message at 18 million messages a second, and a row whose deliveries are above
  256 ns writes a quarter of that or less. `bench`'s one-way rows are such rows, and
  what the subscriber's buffer alone does to them is in the run of the pull request
  that landed this. It is the first thing to try if such a row's tail
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

**Both threads' samples kept the new way, against the harness before it.** Three
builds as processes in turn, a row at a time, seven processes of each for each row,
21 loops a process: `master`'s harness at b4dab01, a build that kept every sample of
both threads in the buffer, and a build that kept both threads' samples as the
decision keeps the subscriber's. A process's level is the median of its loops'
p99.99. Below, the median of the seven levels in nanoseconds, then the slowest of the
147 loops.

| Row | harness before | every sample in the buffer | counts and buffer, both threads |
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
and one more on the client row, 603 on the recorded row); neither of the other two
has one. The ffi row is run alone in its process here, where CI runs it after the
others. Alone it has two ways to run, its claim at a p50 of 37 ns or of about 90, with
any of the builds, and which one a process took went with when the subscriber started
and not with how samples were kept; CI's full pass has always read the fast way, and
read it so for the build below. That was not looked into further.

**The same build in CI, on both hosts** (pull request 236's first run, 43158593739,
against the last run of the old harness, 43156541517; the full pass, 21 runs, checked
profile, p50 / p99 / p99.99 in nanoseconds).

| Host and row | harness before | counts and buffer, both threads |
| --- | --- | --- |
| `grip`, `one_way_32_client` | 110 / 181 / 491, runs 483 to 551 | 111 / 182 / 459, runs 452 to 467 |
| `grip`, `one_way_32_recorded` | 104 / 186 / 513, runs 494 to 848 | 103 / 179 / 486, runs 484 to 492 |
| `grip`, `one_way_32_ffi` | 124 / 225 / 487 | 123 / 216 / 463 |
| `grip`, `one_way_32_exclusive` | 89 / 136 / 404 | 86 / 136 / 389 |
| `grip`, `claim_commit_32_recorded` | 22 / 67 / 136 | 21 / 54 / 127 |
| `grip`, `claim_commit_32_ffi` | 36 / 82 / 136 | 37 / 85 / 144 |
| `bench`, `claim_commit_32_recorded` | 80 / 170 / 300, runs 290 to 310 | 80 / 170 / 440, runs 440 to 440 |
| `bench`, `claim_commit_32_client` | 80 / 150 / 160 | 80 / 110 / 180 |
| `bench`, `claim_commit_32_exclusive` | 70 / 90 / 150 | 90 / 100 / 170 |
| `bench`, `one_way_32_exclusive` | 140 / 160 / 220 | 170 / 200 / 250 |
| `bench`, `one_way_32_client` | 170 / 240 / 290 | 160 / 240 / 300 |
| `bench`, `one_way_32_recorded` | 170 / 280 / 580 | 150 / 270 / 570 |

So on `grip` the one-way rows read 15 to 32 ns lower and their runs lie within 15 ns
of each other, where the old harness's lay 68 and 354 apart. On `bench`, whose clock
steps by 10 ns, the publisher's rows paid: the recorded claim by 140 ns, which failed
the gate, and the unchecked profile read the same (409 against 290). That is why the
decision leaves the publisher alone. The decided form, the subscriber's samples
alone, is this change as it landed; its rows on both hosts are in the pull request
that landed it.

## Consequences

- The one-way rows' tail on `grip` is the path's again, and a run no longer reads two
  or three times its row's. The rows that ADR 0033's re-records loosened for those
  runs can be recorded tight.
- The subscriber's loop of every message row is compiled differently, so a row may
  move by the compiler alone, as `docs/how-to/run-the-bench.md` says a harness change
  can. Both bench hosts' profiles are to be re-recorded from processes at this
  change's tree, in a change of its own, since it takes the hosts for hours; until
  then the gate compares this harness against baselines of the old one, which admit
  more than they should, as they did before.
- A row costs memory in proportion to its messages: 40 MB for CI's 5,000,000, written
  through at setup, which is tens of milliseconds a loop that no sample sees, and 16
  bytes a message in the link rows. Making the histogram is one pass over the counts
  and the buffer after the loop.
- The two threads of a row now keep their samples in two ways, each for what its
  record must not delay. A reader of `bench/src/main.rs` finds the reason on
  `Samples`.
- The bound of 256 ns is a choice for today's rows: under it are `grip`'s one-way
  rows up to their p99. A row whose deliveries are all above it, as a UDP row's are,
  keeps every sample in the buffer, which is the first form. What that costs such a
  row was not measured apart from the publisher's part; `bench`'s one-way rows are
  mostly above the bound, and their reading with this change is in the pull request
  that landed it.
- A message row's late deliveries exist in order for the first time. A later change
  can keep what a histogram cannot: where in a loop its slowest samples were.
- Revisit if a row is added whose loop cannot know its sample count at setup, if a
  claim row's tail is ever found to be its own record, or if the buffer's own stores
  are ever measured in a one-way row's tail.

## References

- Card 198, where the runs above are recorded with their scripts.
- `bench/src/main.rs`, `Samples`.
- [Run the bench and compare it with the baseline](../how-to/run-the-bench.md).
- [ADR 0015](0015-measure-in-process-with-per-runner-baselines.md),
  [ADR 0016](0016-run-the-latency-gate-on-a-dedicated-host.md),
  [ADR 0033](0033-record-a-bench-baseline-from-every-process-at-one-tree.md).
